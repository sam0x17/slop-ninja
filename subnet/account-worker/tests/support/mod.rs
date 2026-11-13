//! The native test host: the account store's own migration in an
//! in-memory SQLite, the quota objects as a map running the object's own
//! logic, a capturing mailer, the queue and Analytics Engine as lists, a
//! settable clock and a deterministic random stream, and every outbound
//! call answered by the dev auth stub's provider routes (pure functions of
//! a request). Browsers keep their own cookie jars, so the tests can sign
//! in, switch browsers and replay requests as real clients do.
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use slop_ninja_account_worker::host::{
    DbError, Host, NetError, OutReply, OutRequest, QueueUnavailable, QuotaTake, QuotaUnavailable, Row, Stmt, Val,
    classify,
};
use slop_ninja_account_worker::quota::{self, QuotaCall};
use slop_ninja_account_worker::{Config, ConfigError, Req, Resp, handle};
use slop_ninja_auth_core::mail::{Message, SendFailure};
use slop_ninja_auth_core::policy::{BucketState, TokenBucket};
use slop_ninja_detect_page::ae::Point;
use slop_ninja_detect_page::queue::QueueMessage;
use slop_ninja_dev_auth_stub::routes::{self as stub, Outcome, Stub};
use slop_ninja_dev_http::Call;

pub const SITE: &str = "https://slop.localhost:8787";
pub const SECRET: &str = "https://s.slop.localhost:8788";
pub const T0: u64 = 1_794_547_800_000;
const MIGRATION: &str = include_str!("../../migrations/0001_account_store.sql");

/// One captured mail.
#[derive(Clone, Debug)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub text: String,
}

impl Mail {
    /// The token in the mail's confirm link (`#t=`).
    pub fn token(&self) -> String {
        let at = self.text.find("#t=").expect("a confirm link");
        self.text[at + 3..].split_whitespace().next().unwrap().to_owned()
    }
}

pub struct TestHost {
    pub db: RefCell<rusqlite::Connection>,
    pub quota: RefCell<BTreeMap<String, BucketState>>,
    pub quota_down: Cell<bool>,
    pub mail: RefCell<Vec<Mail>>,
    pub fail_mail: RefCell<Option<SendFailure>>,
    pub stub: RefCell<Stub>,
    pub stub_cfg: stub::Config,
    pub queue: RefCell<Vec<QueueMessage>>,
    pub queue_down: Cell<bool>,
    pub points: RefCell<Vec<(String, Vec<String>)>>,
    pub http_log: RefCell<Vec<String>>,
    pub now: Cell<u64>,
    seq: Cell<u64>,
}

fn to_sql(v: &Val) -> SqlValue {
    match v {
        Val::Null => SqlValue::Null,
        Val::Int(i) => SqlValue::Integer(*i),
        Val::Text(t) => SqlValue::Text(t.clone()),
        Val::Blob(b) => SqlValue::Blob(b.clone()),
    }
}

fn from_sql(v: SqlValue) -> Val {
    match v {
        SqlValue::Null => Val::Null,
        SqlValue::Integer(i) => Val::Int(i),
        SqlValue::Real(f) => Val::Int(f as i64),
        SqlValue::Text(t) => Val::Text(t),
        SqlValue::Blob(b) => Val::Blob(b),
    }
}

fn run(conn: &rusqlite::Connection, s: &Stmt) -> Result<Vec<Row>, String> {
    let mut st = conn.prepare(&s.sql).map_err(|e| e.to_string())?;
    let names: Vec<String> = st.column_names().iter().map(|c| (*c).to_owned()).collect();
    let params: Vec<SqlValue> = s.args.iter().map(to_sql).collect();
    let mut rows = st.query(rusqlite::params_from_iter(params)).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().map_err(|e| e.to_string())? {
        let mut cols = Vec::new();
        for (i, n) in names.iter().enumerate() {
            cols.push((n.clone(), from_sql(r.get::<_, SqlValue>(i).map_err(|e| e.to_string())?)));
        }
        out.push(Row { cols });
    }
    Ok(out)
}

impl TestHost {
    pub fn new() -> TestHost {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(MIGRATION).unwrap();
        let stub_cfg = stub::Config::from_vars(|k| match k {
            "SITE_ORIGIN" => Some(SITE.to_owned()),
            _ => None,
        })
        .unwrap();
        TestHost {
            db: RefCell::new(conn),
            quota: RefCell::default(),
            quota_down: Cell::new(false),
            mail: RefCell::default(),
            fail_mail: RefCell::default(),
            stub: RefCell::default(),
            stub_cfg,
            queue: RefCell::default(),
            queue_down: Cell::new(false),
            points: RefCell::default(),
            http_log: RefCell::default(),
            now: Cell::new(T0),
            seq: Cell::new(0),
        }
    }

    pub fn advance(&self, ms: u64) {
        self.now.set(self.now.get() + ms);
    }

    /// A query for the tests themselves.
    pub fn q(&self, sql: &str) -> Vec<Row> {
        run(&self.db.borrow(), &Stmt::new(sql, vec![])).unwrap()
    }

    pub fn count(&self, sql: &str) -> i64 {
        self.q(sql)[0].int("n").unwrap()
    }

    pub fn mails_to(&self, to: &str) -> Vec<Mail> {
        self.mail.borrow().iter().filter(|m| m.to == to).cloned().collect()
    }

    pub fn points_named(&self, name: &str) -> Vec<Vec<String>> {
        self.points.borrow().iter().filter(|(n, _)| n == name).map(|(_, b)| b.clone()).collect()
    }

    pub fn alerts(&self, rule: &str) -> usize {
        self.points_named("alert").iter().filter(|b| b[0] == rule).count()
    }

    /// Every byte of the store, for leak scans: a SQL dump of every table.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        let tables: Vec<String> = self
            .q("SELECT name FROM sqlite_master WHERE type = 'table'")
            .iter()
            .map(|r| r.text("name").unwrap().to_owned())
            .collect();
        for t in tables {
            for r in self.q(&format!("SELECT * FROM {t}")) {
                for (c, v) in r.cols {
                    let s = match v {
                        Val::Null => String::new(),
                        Val::Int(i) => i.to_string(),
                        Val::Text(s) => s.escape_debug().to_string(),
                        Val::Blob(b) => format!("{}|{:?}", hex::encode(&b), String::from_utf8_lossy(&b)),
                    };
                    out.push_str(&format!("{t}.{c}={s}\n"));
                }
            }
        }
        out
    }

    /// The Worker's answer to `req`.
    pub fn call(&self, cfg: &Result<Config, ConfigError>, req: &Req) -> Resp {
        block_on(handle(cfg, self, req))
    }

    fn stub_call(&self, out: &OutRequest) -> Result<OutReply, NetError> {
        let url = url::Url::parse(&out.url).map_err(|_| NetError::Failed)?;
        let host = url.host_str().ok_or(NetError::Failed)?.to_owned();
        let mut path = format!("/{host}{}", url.path());
        if let Some(q) = url.query() {
            path.push('?');
            path.push_str(q);
        }
        self.http_log.borrow_mut().push(path.clone());
        let mut call = Call::new(out.method, &path).host("auth.slop.localhost");
        for (k, v) in &out.headers {
            call = call.header(k, v);
        }
        call = call.body(out.body.clone().into_bytes());
        let outcome = self.stub.borrow_mut().handle(&self.stub_cfg, &call, self.now.get());
        let (status, body, delay) = match outcome {
            Outcome::Reply(r) => (r.status, r.body.to_vec(), r.delay_ms),
            Outcome::IdToken { claims, mut body, delay_ms } => {
                let header = json!({"alg": "RS256", "kid": "stub", "typ": "JWT"});
                let jwt = format!("{}.c3R1Yg", stub::jwt_signing_input(&header, &claims));
                body["id_token"] = Value::String(jwt);
                (200, serde_json::to_vec(&body).unwrap(), delay_ms)
            }
            Outcome::Jwks => (200, b"{\"keys\":[]}".to_vec(), 0),
        };
        if delay >= out.timeout_ms {
            self.advance(out.timeout_ms);
            return Err(NetError::Timeout);
        }
        self.advance(delay);
        Ok(OutReply { status, body: (body.len() <= out.max_body).then_some(body) })
    }

    /// The browser's visit to a provider's authorize URL: the `Location`
    /// the stub answers (our callback with `code` and `state`, or `error`).
    pub fn authorize_location(&self, location: &str) -> String {
        let url = url::Url::parse(location).unwrap();
        let mut path = format!("/{}{}", url.host_str().unwrap(), url.path());
        if let Some(q) = url.query() {
            path.push('?');
            path.push_str(q);
        }
        let call = Call::new("GET", &path).host("auth.slop.localhost");
        match self.stub.borrow_mut().handle(&self.stub_cfg, &call, self.now.get()) {
            Outcome::Reply(r) => {
                assert!(r.status == 302 || r.status == 303, "authorize answered {}", r.status);
                r.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("location")).unwrap().1.clone()
            }
            _ => panic!("authorize must redirect"),
        }
    }
}

impl Host for TestHost {
    async fn all(&self, stmt: Stmt) -> Result<Vec<Row>, DbError> {
        run(&self.db.borrow(), &stmt).map_err(|e| classify(&e))
    }

    async fn batch(&self, stmts: Vec<Stmt>) -> Result<Vec<Vec<Row>>, DbError> {
        let mut conn = self.db.borrow_mut();
        let tx = conn.transaction().map_err(|_| DbError::Unavailable)?;
        let mut out = Vec::new();
        for s in &stmts {
            match run(&tx, s) {
                Ok(r) => out.push(r),
                Err(e) => return Err(classify(&e)),
            }
        }
        tx.commit().map_err(|_| DbError::Unavailable)?;
        Ok(out)
    }

    async fn quota_take(&self, name: &str, size: TokenBucket) -> Result<QuotaTake, QuotaUnavailable> {
        if self.quota_down.get() {
            return Err(QuotaUnavailable);
        }
        let mut q = self.quota.borrow_mut();
        let call = QuotaCall::Take { capacity: size.capacity, refill_every_s: size.refill_every_s };
        let next = quota::apply(q.get(name).copied(), call, self.now.get());
        match next.state {
            Some(s) => q.insert(name.to_owned(), s),
            None => q.remove(name),
        };
        Ok(next.answer.into())
    }

    async fn quota_reset(&self, name: &str) -> Result<(), QuotaUnavailable> {
        self.quota.borrow_mut().remove(name);
        Ok(())
    }

    async fn send_mail(&self, m: &Message) -> Result<(), SendFailure> {
        if let Some(f) = self.fail_mail.borrow_mut().take() {
            return Err(f);
        }
        self.mail.borrow_mut().push(Mail {
            to: m.to.as_str().to_owned(),
            subject: m.subject.clone(),
            text: m.text.clone(),
        });
        Ok(())
    }

    async fn http(&self, req: &OutRequest) -> Result<OutReply, NetError> {
        self.stub_call(req)
    }

    async fn enqueue(&self, msg: &QueueMessage) -> Result<(), QueueUnavailable> {
        if self.queue_down.get() {
            return Err(QueueUnavailable);
        }
        self.queue.borrow_mut().push(msg.clone());
        Ok(())
    }

    fn point(&self, point: &Point, blobs: &[&str]) {
        assert_eq!(blobs.len(), point.blobs.len(), "point {} takes {:?}", point.name, point.blobs);
        self.points.borrow_mut().push((point.name.to_owned(), blobs.iter().map(|b| (*b).to_owned()).collect()));
    }

    fn fill(&self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(32) {
            let n = self.seq.get() + 1;
            self.seq.set(n);
            let h = Sha256::digest(format!("slop_ninja_account_test_rng/{n}"));
            chunk.copy_from_slice(&h[..chunk.len()]);
        }
    }

    fn now_ms(&self) -> u64 {
        self.now.get()
    }
}

/// Runs a future that never pends (every fake answers at once).
pub fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// The operator's test signing key and the config's vars.
pub fn op_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[42; 32])
}

pub fn vars(extra: &[(&str, &str)]) -> BTreeMap<String, String> {
    let key = slop_ninja_signed_op::OperatorKey::from_verifying_key(op_key().verifying_key()).hex();
    let mut v: BTreeMap<String, String> = [
        ("SITE_ORIGIN", SITE),
        ("SECRET_ORIGIN", SECRET),
        ("MAIL_SUBJECT_PREFIX", "[dev] "),
        ("MAIL_FROM", "sender@mail-domain.invalid"),
        ("OP_ENV", "staging"),
        ("AUTH_POLICY", "auth-policy/v1"),
        ("ACCOUNT_STORE_SECRET", "test-store-secret-0123456789abcdef-0123456789abcdef"),
        ("GOOGLE_CLIENT_ID", "dev-google-client-id"),
        ("GOOGLE_CLIENT_SECRET", "dev-google-client-secret-not-real"),
        ("GITHUB_CLIENT_ID", "dev-github-client-id"),
        ("GITHUB_CLIENT_SECRET", "dev-github-client-secret-not-real"),
        ("X_CLIENT_ID", "dev-x-client-id"),
        ("X_CLIENT_SECRET", "dev-x-client-secret-not-real"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
    v.insert("DETECT_OP_KEY".into(), key);
    for (k, val) in extra {
        v.insert((*k).to_owned(), (*val).to_owned());
    }
    v
}

pub fn config(extra: &[(&str, &str)]) -> Result<Config, ConfigError> {
    let v = vars(extra);
    Config::from_vars(|k| v.get(k).cloned())
}

/// A browser: its cookie jar and the forwarder's headers.
#[derive(Clone, Debug, Default)]
pub struct Browser {
    pub jar: BTreeMap<String, String>,
    pub ua: Option<String>,
}

pub const SESSION: &str = "__Host-sn_session";
pub const FLOW: &str = "__Host-sn_flow";

impl Browser {
    pub fn new() -> Browser {
        Browser {
            jar: BTreeMap::new(),
            ua: Some("Mozilla/5.0 (X11; Linux x86_64) Gecko/20100101 Firefox/130.0".into()),
        }
    }

    fn cookie_header(&self) -> Option<String> {
        (!self.jar.is_empty()).then(|| self.jar.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; "))
    }

    /// Applies an answer's `Set-Cookie` headers.
    pub fn take(&mut self, r: &Resp) {
        for c in r.set_cookies() {
            let (pair, attrs) = c.split_once(';').unwrap_or((c, ""));
            let (k, v) = pair.split_once('=').unwrap();
            if attrs.contains("Max-Age=0") {
                self.jar.remove(k);
            } else {
                self.jar.insert(k.to_owned(), v.to_owned());
            }
        }
    }

    pub fn req(&self, route: &str, method: &str, path_query: &str, ct: Option<&str>, body: Vec<u8>) -> Req {
        let (path, query) = path_query.split_once('?').unwrap_or((path_query, ""));
        let mut headers = vec![
            ("x-sn-route".to_owned(), route.to_owned()),
            ("x-sn-host".to_owned(), "slop.localhost:8787".to_owned()),
            ("sec-fetch-site".to_owned(), "same-origin".to_owned()),
            ("origin".to_owned(), SITE.to_owned()),
        ];
        if let Some(c) = self.cookie_header() {
            headers.push(("cookie".into(), c));
        }
        if let Some(ct) = ct {
            headers.push(("content-type".into(), ct.into()));
        }
        if let Some(ua) = &self.ua {
            headers.push(("user-agent".into(), ua.clone()));
        }
        Req { method: method.into(), path: path.into(), query: query.into(), headers, body }
    }

    pub fn send(&mut self, h: &TestHost, cfg: &Result<Config, ConfigError>, req: Req) -> Resp {
        let r = h.call(cfg, &req);
        self.take(&r);
        r
    }

    pub fn post_json(
        &mut self,
        h: &TestHost,
        cfg: &Result<Config, ConfigError>,
        route: &str,
        path: &str,
        body: Value,
    ) -> Resp {
        let req = self.req(route, "POST", path, Some("application/json"), serde_json::to_vec(&body).unwrap());
        self.send(h, cfg, req)
    }

    pub fn post_form(
        &mut self,
        h: &TestHost,
        cfg: &Result<Config, ConfigError>,
        route: &str,
        path: &str,
        fields: &[(&str, &str)],
    ) -> Resp {
        let body = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(fields).finish();
        let req = self.req(route, "POST", path, Some("application/x-www-form-urlencoded"), body.into_bytes());
        self.send(h, cfg, req)
    }

    pub fn get(&mut self, h: &TestHost, cfg: &Result<Config, ConfigError>, route: &str, path_query: &str) -> Resp {
        let req = self.req(route, "GET", path_query, None, vec![]);
        self.send(h, cfg, req)
    }

    /// A binding call from the website, with this browser's cookie.
    pub fn internal(&mut self, h: &TestHost, cfg: &Result<Config, ConfigError>, path: &str, body: Value) -> Resp {
        let mut headers = vec![("content-type".to_owned(), "application/json".to_owned())];
        if let Some(c) = self.cookie_header() {
            headers.push(("cookie".into(), c));
        }
        let req = Req {
            method: "POST".into(),
            path: path.into(),
            query: String::new(),
            headers,
            body: serde_json::to_vec(&body).unwrap(),
        };
        let r = h.call(cfg, &req);
        self.take(&r);
        r
    }

    pub fn signed_in(&self) -> bool {
        self.jar.contains_key(SESSION)
    }

    // ------------------------------------------------------------ flows

    pub fn email_start(&mut self, h: &TestHost, cfg: &Result<Config, ConfigError>, body: Value) -> Resp {
        self.post_json(h, cfg, "email_start", "/api/account/v1/email/start", body)
    }

    pub fn email_confirm(&mut self, h: &TestHost, cfg: &Result<Config, ConfigError>, token: &str) -> Resp {
        self.post_json(h, cfg, "email_confirm", "/api/account/v1/email/confirm", json!({"token": token}))
    }

    /// Requests a sign-in link for `addr` and confirms it in this browser.
    pub fn email_sign_in(&mut self, h: &TestHost, cfg: &Result<Config, ConfigError>, addr: &str) -> Resp {
        let before = h.mails_to(addr).len();
        assert_eq!(self.email_start(h, cfg, json!({"email": addr})).status, 202);
        let mails = h.mails_to(addr);
        assert_eq!(mails.len(), before + 1, "one mail to {addr}");
        let r = self.email_confirm(h, cfg, &mails.last().unwrap().token());
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        r
    }

    /// An OAuth round trip: start, the stub's consent, the callback.
    pub fn oauth(&mut self, h: &TestHost, cfg: &Result<Config, ConfigError>, p: &str, query: &str) -> Resp {
        let start = self.get(h, cfg, "auth_start", &format!("/auth/{p}/start{query}"));
        if start.status != 302 {
            return start;
        }
        let back = h.authorize_location(start.header("location").unwrap());
        let url = url::Url::parse(&back).unwrap();
        let pq = format!("{}?{}", url.path(), url.query().unwrap_or(""));
        self.get(h, cfg, "auth_callback", &pq)
    }

    pub fn me(&mut self, h: &TestHost, cfg: &Result<Config, ConfigError>) -> Value {
        let r = self.get(h, cfg, "me", "/api/account/v1/me");
        serde_json::from_slice(&r.body).unwrap()
    }

    pub fn session(&mut self, h: &TestHost, cfg: &Result<Config, ConfigError>) -> Option<Value> {
        let r = self.internal(h, cfg, "/internal/v1/session", json!({}));
        (r.status == 200).then(|| serde_json::from_slice(&r.body).unwrap())
    }

    pub fn view(&mut self, h: &TestHost, cfg: &Result<Config, ConfigError>, page: &str) -> Value {
        let r = self.internal(h, cfg, "/internal/v1/view", json!({"page": page}));
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        serde_json::from_slice(&r.body).unwrap()
    }
}

/// A signed account op's request body.
pub fn op_body(h: &TestHost, op: &str, args: Value) -> Vec<u8> {
    use slop_ninja_signed_op::{Env, Nonce, Op, Target};
    let n: [u8; 16] = h.random();
    let op = Op {
        target: Target::Account,
        env: Env::Staging,
        issued_ms: h.now.get(),
        expires_ms: h.now.get() + 60_000,
        nonce: Nonce(n),
        op: op.into(),
        args: args.as_object().unwrap().clone(),
    };
    serde_json::to_vec(&slop_ninja_signed_op::sign(&op, &op_key()).unwrap()).unwrap()
}

/// Sends a signed op as the operator's CLI does (through the website's
/// forwarder, with no cookie and no Fetch Metadata).
pub fn op(h: &TestHost, cfg: &Result<Config, ConfigError>, name: &str, args: Value) -> (u16, Value) {
    let body = op_body(h, name, args);
    let req = Req {
        method: "POST".into(),
        path: "/api/account/v1/op".into(),
        query: String::new(),
        headers: vec![
            ("x-sn-route".into(), "account_op".into()),
            ("x-sn-host".into(), "slop.localhost:8787".into()),
            ("content-type".into(), "application/json".into()),
        ],
        body,
    };
    let r = h.call(cfg, &req);
    (r.status, serde_json::from_slice(&r.body).unwrap_or(Value::Null))
}

/// The user id (hex) of the browser's session.
pub fn user_of(b: &mut Browser, h: &TestHost, cfg: &Result<Config, ConfigError>) -> String {
    b.session(h, cfg).expect("signed in")["user_id"].as_str().unwrap().to_owned()
}
