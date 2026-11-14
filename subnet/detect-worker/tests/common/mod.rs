//! The store tests' harness: the store's [`Io`] over an in-memory SQLite
//! (the detect store's own migration, foreign keys on, as D1 runs it), a
//! map for R2, and recorders for the account Worker, the queue, the relay
//! and the metric points. Every future the store returns is ready at once,
//! so a no-op waker drives it.

#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use rusqlite::types::{Value, ValueRef};
use slop_ninja_account_wire::OwnedAddRequest;
use slop_ninja_detect_page::queue::QueueMessage;
use slop_ninja_detect_worker::db::{DbError, DocPage, Done, Io, IoError, Row, Sql, Stmt, Val};
use slop_ninja_detect_worker::doc::StoredResult;

pub const MIGRATION: &str = include_str!("../../migrations/0001_detect_store.sql");

pub fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// A hook run before a statement whose text contains its needle.
pub type Hook = Box<dyn FnMut(&rusqlite::Connection)>;

pub struct Harness {
    pub db: rusqlite::Connection,
    pub docs: RefCell<BTreeMap<String, (Vec<u8>, u64)>>,
    pub now: Cell<u64>,
    rng: Cell<u64>,
    pub vars: RefCell<BTreeMap<String, String>>,
    pub owned: RefCell<Vec<OwnedAddRequest>>,
    pub owned_fails: Cell<bool>,
    pub queue: RefCell<Vec<QueueMessage>>,
    pub metrics: RefCell<Vec<(String, Vec<String>)>>,
    pub relay: RefCell<Vec<Vec<u8>>>,
    pub purged: RefCell<Vec<String>>,
    pub purge_token: Cell<bool>,
    pub statements: Cell<u64>,
    pub doc_reads: Cell<u64>,
    pub fail_doc_put: Cell<bool>,
    pub fail_doc_delete: Cell<bool>,
    pub hooks: RefCell<Vec<(String, Hook)>>,
    pub list_page: Cell<usize>,
}

impl Harness {
    pub fn new() -> Harness {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        db.execute_batch(MIGRATION).unwrap();
        Harness {
            db,
            docs: RefCell::new(BTreeMap::new()),
            now: Cell::new(1_795_169_040_000),
            rng: Cell::new(0x9e37_79b9_7f4a_7c15),
            vars: RefCell::new(BTreeMap::new()),
            owned: RefCell::new(Vec::new()),
            owned_fails: Cell::new(false),
            queue: RefCell::new(Vec::new()),
            metrics: RefCell::new(Vec::new()),
            relay: RefCell::new(Vec::new()),
            purged: RefCell::new(Vec::new()),
            purge_token: Cell::new(false),
            statements: Cell::new(0),
            doc_reads: Cell::new(0),
            fail_doc_put: Cell::new(false),
            fail_doc_delete: Cell::new(false),
            hooks: RefCell::new(Vec::new()),
            list_page: Cell::new(1000),
        }
    }

    pub fn advance(&self, ms: u64) {
        self.now.set(self.now.get() + ms);
    }

    /// Runs `hook` once, before the first statement containing `needle`.
    pub fn before(&self, needle: &str, hook: impl FnMut(&rusqlite::Connection) + 'static) {
        self.hooks.borrow_mut().push((needle.to_owned(), Box::new(hook)));
    }

    fn run_hooks(&self, sql: &str) {
        let mut hooks = self.hooks.borrow_mut();
        if let Some(i) = hooks.iter().position(|(n, _)| sql.contains(n.as_str())) {
            let (_, mut h) = hooks.remove(i);
            drop(hooks);
            h(&self.db);
        }
    }

    pub fn exec(&self, sql: &str) {
        self.db.execute_batch(sql).unwrap();
    }

    pub fn count(&self, sql: &str) -> i64 {
        self.db.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// Every cell of every table as text (blobs as hex), for dump checks.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        let tables: Vec<String> = self
            .db
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for t in tables {
            let mut st = self.db.prepare(&format!("SELECT * FROM {t}")).unwrap();
            let n = st.column_count();
            let mut rows = st.query([]).unwrap();
            while let Some(r) = rows.next().unwrap() {
                for i in 0..n {
                    match r.get_ref(i).unwrap() {
                        ValueRef::Text(s) => out.push_str(std::str::from_utf8(s).unwrap()),
                        ValueRef::Blob(b) => out.push_str(&hex::encode(b)),
                        ValueRef::Integer(v) => out.push_str(&v.to_string()),
                        _ => {}
                    }
                    out.push('|');
                }
                out.push('\n');
            }
        }
        out
    }

    fn one(&self, s: &Stmt) -> Result<Done, DbError> {
        self.statements.set(self.statements.get() + 1);
        self.run_hooks(&s.sql);
        let mut st = self.db.prepare(&s.sql).map_err(|e| panic!("prepare {}: {e}", s.sql)).unwrap();
        let params: Vec<Value> = s
            .params
            .iter()
            .map(|v| match v {
                Val::Null => Value::Null,
                Val::Int(i) => Value::Integer(*i),
                Val::Text(t) => Value::Text(t.clone()),
                Val::Blob(b) => Value::Blob(b.clone()),
            })
            .collect();
        let names: Vec<String> = st.column_names().into_iter().map(str::to_owned).collect();
        let mut rows = st.query(rusqlite::params_from_iter(params.iter())).map_err(|_| DbError::Failed)?;
        let mut out = Vec::new();
        loop {
            match rows.next() {
                Ok(Some(r)) => {
                    let mut row = Vec::new();
                    for (i, n) in names.iter().enumerate() {
                        let v = match r.get_ref(i).unwrap() {
                            ValueRef::Null => Val::Null,
                            ValueRef::Integer(x) => Val::Int(x),
                            ValueRef::Text(t) => Val::Text(String::from_utf8(t.to_vec()).unwrap()),
                            ValueRef::Blob(b) => Val::Blob(b.to_vec()),
                            ValueRef::Real(_) => panic!("a real column"),
                        };
                        row.push((n.clone(), v));
                    }
                    out.push(Row(row));
                }
                Ok(None) => break,
                Err(_) => return Err(DbError::Failed),
            }
        }
        drop(rows);
        let changes = if names.is_empty() || s.sql.contains("RETURNING") || !s.sql.trim_start().starts_with("SELECT") {
            self.db.changes()
        } else {
            0
        };
        Ok(Done { rows: out, changes })
    }

    pub fn doc(&self, key: &str) -> Option<Vec<u8>> {
        self.docs.borrow().get(key).map(|(b, _)| b.clone())
    }

    pub fn metric_count(&self, point: &str, blob: &str) -> usize {
        self.metrics.borrow().iter().filter(|(p, b)| p == point && b.first().map(String::as_str) == Some(blob)).count()
    }
}

impl Sql for Harness {
    async fn run(&self, s: Stmt) -> Result<Done, DbError> {
        self.one(&s)
    }

    async fn batch(&self, v: Vec<Stmt>) -> Result<Vec<Done>, DbError> {
        self.db.execute_batch("BEGIN").unwrap();
        let mut out = Vec::new();
        for s in &v {
            match self.one(s) {
                Ok(d) => out.push(d),
                Err(e) => {
                    self.db.execute_batch("ROLLBACK").unwrap();
                    return Err(e);
                }
            }
        }
        self.db.execute_batch("COMMIT").unwrap();
        Ok(out)
    }
}

impl Io for Harness {
    fn now_ms(&self) -> u64 {
        self.now.get()
    }

    fn var(&self, name: &str) -> Option<String> {
        self.vars.borrow().get(name).cloned()
    }

    fn fill(&self, buf: &mut [u8]) {
        for b in buf {
            let mut x = self.rng.get();
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.rng.set(x);
            *b = (x >> 24) as u8;
        }
    }

    async fn doc_put(&self, key: &str, bytes: Vec<u8>) -> Result<(), IoError> {
        if self.fail_doc_put.get() {
            return Err(IoError);
        }
        self.docs.borrow_mut().insert(key.to_owned(), (bytes, self.now.get()));
        Ok(())
    }

    async fn doc_get(&self, key: &str) -> Result<Option<Vec<u8>>, IoError> {
        self.doc_reads.set(self.doc_reads.get() + 1);
        Ok(self.doc(key))
    }

    async fn doc_delete(&self, keys: &[String]) -> Result<(), IoError> {
        assert!(keys.len() <= 1000);
        if self.fail_doc_delete.get() {
            return Err(IoError);
        }
        let mut d = self.docs.borrow_mut();
        for k in keys {
            d.remove(k);
        }
        Ok(())
    }

    async fn doc_list(&self, prefix: &str, cursor: Option<&str>) -> Result<DocPage, IoError> {
        let d = self.docs.borrow();
        let objects: Vec<(String, u64)> = d
            .iter()
            .filter(|(k, _)| k.starts_with(prefix) && cursor.is_none_or(|c| k.as_str() > c))
            .take(self.list_page.get())
            .map(|(k, (_, at))| (k.clone(), *at))
            .collect();
        let more = d.keys().filter(|k| k.starts_with(prefix)).any(|k| objects.last().is_some_and(|(l, _)| k > l));
        let cursor = if more { objects.last().map(|(k, _)| k.clone()) } else { None };
        Ok(DocPage { objects, cursor })
    }

    async fn owned_add(&self, req: &OwnedAddRequest) -> Result<(), IoError> {
        if self.owned_fails.get() {
            return Err(IoError);
        }
        self.owned.borrow_mut().push(req.clone());
        Ok(())
    }

    async fn enqueue(&self, msgs: Vec<QueueMessage>) -> Result<(), IoError> {
        self.queue.borrow_mut().extend(msgs);
        Ok(())
    }

    async fn cache_purge(&self, urls: &[String]) -> Result<bool, IoError> {
        if !self.purge_token.get() {
            return Ok(false);
        }
        self.purged.borrow_mut().extend(urls.iter().cloned());
        Ok(true)
    }

    async fn relay_legal(&self, body: Vec<u8>) -> Result<(), IoError> {
        self.relay.borrow_mut().push(body);
        Ok(())
    }

    async fn receipt(&self, _result: &StoredResult) -> Option<String> {
        Some("receipt".to_owned())
    }

    fn metric(&self, point: &'static str, blobs: &[&str]) {
        self.metrics.borrow_mut().push((point.to_owned(), blobs.iter().map(|b| (*b).to_owned()).collect()));
    }
}
