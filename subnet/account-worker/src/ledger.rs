//! The billing ledger: holds and how they end, grants, plan periods,
//! `billing-policy/v1`, and the internal billing routes the detect Worker
//! calls (`/internal/v1/billing/{peek,hold,commit,release}`).
//!
//! **Credit is held in tokens.** Its sources are `payments` rows (grants are
//! non-cash) and the active plan period's allowance. A check draws on the
//! plan first, then on payments first paid, first used; a hold that spans
//! sources is split into tagged parts (`account_wire::credit::draw` fixes
//! the totals for the quote and the hold alike).
//!
//! **A hold** (`/billing/hold {user_id, tokens, channel, idem}`) reads, plans,
//! then writes in one batch. A D1 batch is a fixed list of statements run as
//! one transaction: a read inside it cannot shape the later statements, and
//! an `UPDATE` that matches no row is not an error. So:
//! 1. **read** (outside any batch): a hold with this `idem` (answered as it
//!    is while it has not ended; an ended one answers `quote_invalid`, so
//!    one `idem` never backs a second check), then the active plan period
//!    and the active payments in FIFO order;
//! 2. **plan** the parts here ([`plan`]); a shortfall answers 402
//!    `not_enough_credit` and writes nothing;
//! 3. **one batch**: a guard that each source still belongs to the account
//!    and is active, the *unguarded* decrements, then the hold row. If a
//!    concurrent hold, a void or a period end took the headroom since the
//!    read, a decrement breaks one of the store's CHECKs (no payment below
//!    none left, no period past what it may draw, nothing drawn from a
//!    closed source), and D1 rolls back the whole batch, the hold row
//!    included; a duplicate `idem` fails it on `holds.idem UNIQUE`;
//! 4. **on a rollback** it reads and plans once more and runs the batch
//!    again; a second failure, or a shortfall, answers 402 with nothing
//!    changed, and a duplicate `idem` answers the existing hold under the
//!    same rule as the first read.
//!
//! So no hold exists whose parts were not all deducted, and concurrent holds
//! never draw more than the balance.
//!
//! **A hold ends once.** Commit and release each begin their batch with
//! `INSERT INTO hold_ends`, whose primary key is the hold id: a second
//! commit, a second release, or a release after a commit fails that insert
//! and rolls back its batch, and the route then answers the end already
//! recorded (`by_this_call: false`). Commit writes one `debits` row per
//! part and changes no balance; release returns each part to its source
//! only while that source is still active (nothing flows back into a voided
//! grant or a closed period).
//!
//! **The `BILLING` switch.** `off`: no peek and no new hold (`paid_off`).
//! `grants`: grants and plans granted by the operator back holds end to end.
//! `live` (the processor, later) answers `not_configured` until it is built.
//! Commit and release work in every state, so a hold placed before a flip
//! still ends.
//!
//! Nothing here stores a slug, a text or an address: a hold names its user,
//! its tokens, its channel, its parts and the quote's nonce.

use serde::{Deserialize, Serialize};
use slop_ninja_account_wire::credit::{self, MAX_HOLD_TOKENS};
use slop_ninja_account_wire::{
    Bucket, BucketKind, CommitRequest, HoldAnswer, HoldEnd, HoldEndAnswer, HoldPart, HoldRequest, PartSource,
    PeekAnswer, PeekRequest, ReleaseRequest, UserState,
};
use slop_ninja_auth_core::policy::{TokenBucket, Valued};
use slop_ninja_detect_page::{BillingState, Channel, RefusalCode, UserId};

use crate::config::Config;
use crate::host::{DbError, Host, Row, Stmt, Val, guard};
use crate::http::{self, Req, Resp};
use crate::store::{Switches, USER_ACTIVE};

/// The policy's version string.
pub const VERSION: &str = slop_ninja_account_wire::BILLING_POLICY;

/// The FIFO payments one hold plans over at most. A hold that spans more
/// than this many payments would also need more decrements than one batch
/// should carry; credit is bought in reloads of at least a few thousand
/// tokens, so a real balance never comes near it.
pub const FIFO_PAYMENTS: u64 = 90;

/// `billing-policy/v1`: the sizes of the billing quota buckets. The
/// minimums and the plan's API part are the version's own figures
/// (`account_wire::credit`), shared with the detect Worker's quote, so the
/// var cannot change them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BillingPolicy {
    /// Always [`VERSION`].
    pub version: String,
    /// Paid quotes per account (`quote:<user_id>`).
    pub quote_quota: Valued<TokenBucket>,
    /// Checkouts per account (`checkout:<user_id>`), once payments open.
    pub checkout_quota: Valued<TokenBucket>,
    /// Paid API calls per key prefix (`api:<prefix>`), once the API opens.
    pub api_quota: Valued<TokenBucket>,
}

fn valued(value: TokenBucket, reason: &str) -> Valued<TokenBucket> {
    Valued { value, reason: reason.to_owned() }
}

impl BillingPolicy {
    /// The default values.
    pub fn v1() -> BillingPolicy {
        BillingPolicy {
            version: VERSION.to_owned(),
            quote_quota: valued(
                TokenBucket { capacity: 30, refill_every_s: 120 },
                "a reader quotes a few documents at a time; 30 quotes an hour per account bounds the whole-document token counts one account can ask for, and reset_quota refills it",
            ),
            checkout_quota: valued(
                TokenBucket { capacity: 5, refill_every_s: 24 * 3600 / 5 },
                "five checkouts a rolling day per account, the processor's card-testing control on top of its own screening",
            ),
            api_quota: valued(
                TokenBucket { capacity: 60, refill_every_s: 1 },
                "bursts of 60 paid API calls, one a second sustained, per key; the gate's per-key in-flight limit bounds the scorer itself",
            ),
        }
    }

    /// The `BILLING_POLICY` var: empty or the version string is the default;
    /// otherwise the policy's JSON, validated.
    pub fn from_var(var: &str) -> Result<BillingPolicy, &'static str> {
        let var = var.trim();
        if var.is_empty() || var == VERSION {
            return Ok(BillingPolicy::v1());
        }
        let p: BillingPolicy = serde_json::from_str(var).map_err(|_| "schema")?;
        p.validate()?;
        Ok(p)
    }

    /// Every bucket refills and holds at least one take.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != VERSION {
            return Err("version");
        }
        for (name, b) in [
            ("quote_quota", self.quote_quota.value),
            ("checkout_quota", self.checkout_quota.value),
            ("api_quota", self.api_quota.value),
        ] {
            if b.capacity == 0 || b.refill_every_s == 0 {
                return Err(name);
            }
        }
        Ok(())
    }

    /// The size of a bucket this policy owns (`quote`, `checkout`, `api`).
    pub fn bucket(&self, kind: BucketKind) -> Option<TokenBucket> {
        match kind {
            BucketKind::Quote => Some(self.quote_quota.value),
            BucketKind::Checkout => Some(self.checkout_quota.value),
            BucketKind::Api => Some(self.api_quota.value),
            BucketKind::Email | BucketKind::EmailLink | BucketKind::LinkMail => None,
        }
    }
}

// ------------------------------------------------------------------ sources

/// The active plan period a check draws on first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Period {
    pub id: u64,
    pub left: u64,
    pub api_left: u64,
    pub cash: bool,
}

/// One active payment with tokens left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Payment {
    pub id: u64,
    pub left: u64,
    pub cash: bool,
}

/// An account's drawable sources, as read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sources {
    pub period: Option<Period>,
    /// First paid, first used.
    pub payments: Vec<Payment>,
}

impl Sources {
    /// The credit the read payments hold.
    pub fn credit_left(&self) -> u64 {
        self.payments.iter().fold(0u64, |a, p| a.saturating_add(p.left))
    }
}

/// The active plan period: the one ending first, if several overlap.
pub const SELECT_PERIOD: &str = "SELECT period_id, allowance - used AS plan_left, api_cap - api_used AS api_left, \
cash FROM plan_periods WHERE user_id = ? AND state = 'active' AND starts_at <= ? AND ends_at > ? \
ORDER BY ends_at, period_id LIMIT 1";

/// The active payments with tokens left, first paid first (`payments_fifo`).
pub const SELECT_PAYMENTS: &str = "SELECT payment_id, tokens_left, cash FROM payments \
WHERE user_id = ? AND state = 'active' AND tokens_left > 0 ORDER BY created_at, payment_id LIMIT ?";

/// All the credit an account holds, for its balance.
pub const SELECT_CREDIT: &str =
    "SELECT COALESCE(SUM(tokens_left), 0) AS n FROM payments WHERE user_id = ? AND state = 'active'";

/// Reads an account's sources at `now`.
pub async fn sources<H: Host>(host: &H, user: &UserId, now: u64) -> Result<Sources, DbError> {
    let period = host
        .first(crate::stmt!(SELECT_PERIOD, user.0, now, now))
        .await?
        .and_then(|r| {
            Some(Period {
                id: r.u64("period_id")?,
                left: r.u64("plan_left")?,
                api_left: r.u64("api_left")?,
                cash: r.int("cash")? != 0,
            })
        })
        .filter(|p| p.left > 0);
    let payments = host
        .all(crate::stmt!(SELECT_PAYMENTS, user.0, FIFO_PAYMENTS))
        .await?
        .iter()
        .filter_map(|r| {
            Some(Payment { id: r.u64("payment_id")?, left: r.u64("tokens_left")?, cash: r.int("cash")? != 0 })
        })
        .collect();
    Ok(Sources { period, payments })
}

/// The parts of a hold of `tokens` on `channel`, or `None` when the sources
/// fall short (pure). The plan's part comes first, then payments in FIFO
/// order, each taking what it holds until the billed tokens are covered.
pub fn plan(tokens: u64, channel: Channel, s: &Sources) -> Option<Vec<HoldPart>> {
    let (plan_left, api_left) = s.period.map_or((0, 0), |p| (p.left, p.api_left));
    let d = credit::draw(tokens, channel, plan_left, api_left, s.credit_left())?;
    let mut parts = Vec::new();
    if d.plan > 0 {
        let p = s.period?;
        parts.push(HoldPart { src: PartSource::Period, id: p.id, tokens: d.plan, cash: p.cash });
    }
    let mut rest = d.credit;
    for p in &s.payments {
        if rest == 0 {
            break;
        }
        let t = rest.min(p.left);
        if t > 0 {
            parts.push(HoldPart { src: PartSource::Payment, id: p.id, tokens: t, cash: p.cash });
            rest -= t;
        }
    }
    (rest == 0).then_some(parts)
}

/// Every part's tokens.
pub fn total(parts: &[HoldPart]) -> u64 {
    parts.iter().fold(0u64, |a, p| a.saturating_add(p.tokens))
}

// -------------------------------------------------------------- statements

/// The decrement of one part: unguarded, so an overdraw breaks a CHECK.
fn take_stmt(part: &HoldPart, channel: Channel) -> Stmt {
    match part.src {
        PartSource::Period => {
            let api = if channel == Channel::Api { part.tokens } else { 0 };
            crate::stmt!(
                "UPDATE plan_periods SET used = used + ?, api_used = api_used + ? WHERE period_id = ?",
                part.tokens,
                api,
                part.id
            )
        }
        PartSource::Payment => {
            crate::stmt!("UPDATE payments SET tokens_left = tokens_left - ? WHERE payment_id = ?", part.tokens, part.id)
        }
    }
}

/// The source still belongs to `user` and is active: a merge or a void
/// since the read fails the batch instead of leaving a part undeducted.
fn source_guard(part: &HoldPart, user: &UserId) -> Stmt {
    let cond = match part.src {
        PartSource::Period => {
            "EXISTS (SELECT 1 FROM plan_periods WHERE period_id = ? AND user_id = ? AND state = 'active')"
        }
        PartSource::Payment => {
            "EXISTS (SELECT 1 FROM payments WHERE payment_id = ? AND user_id = ? AND state = 'active')"
        }
    };
    guard(cond, vec![Val::from(part.id), Val::from(user.0)])
}

/// The return of one part on release, only into a source still active.
fn return_stmt(part: &HoldPart, channel: Channel) -> Stmt {
    match part.src {
        PartSource::Period => {
            let api = if channel == Channel::Api { part.tokens } else { 0 };
            crate::stmt!(
                "UPDATE plan_periods SET used = used - ?, api_used = api_used - ? WHERE period_id = ? AND state = 'active'",
                part.tokens,
                api,
                part.id
            )
        }
        PartSource::Payment => crate::stmt!(
            "UPDATE payments SET tokens_left = tokens_left + ? WHERE payment_id = ? AND state = 'active'",
            part.tokens,
            part.id
        ),
    }
}

/// A hold row as read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoldRow {
    pub hold_id: String,
    pub user_id: UserId,
    pub channel: Channel,
    pub parts: Vec<HoldPart>,
    pub ended: Option<HoldEnd>,
}

/// A hold and how it ended, by id.
pub const SELECT_HOLD: &str = "SELECT h.hold_id, h.user_id, h.channel, h.parts, e.kind AS ended FROM holds h \
LEFT JOIN hold_ends e ON e.hold_id = h.hold_id WHERE h.hold_id = ?";

/// A hold and how it ended, by its idempotency key.
pub const SELECT_HOLD_BY_IDEM: &str = "SELECT h.hold_id, h.user_id, h.channel, h.parts, e.kind AS ended FROM holds h \
LEFT JOIN hold_ends e ON e.hold_id = h.hold_id WHERE h.idem = ?";

fn hold_row(r: &Row) -> Option<HoldRow> {
    Some(HoldRow {
        hold_id: r.text("hold_id")?.to_owned(),
        user_id: UserId(r.b16("user_id")?),
        channel: Channel::parse(r.text("channel")?)?,
        parts: serde_json::from_str(r.text("parts")?).ok()?,
        ended: match r.get("ended") {
            Some(Val::Text(k)) => Some(HoldEnd::parse(k)?),
            _ => None,
        },
    })
}

async fn read_hold<H: Host>(host: &H, sql: &str, key: &str) -> Result<Option<HoldRow>, RefusalCode> {
    let r = host.first(Stmt::new(sql, vec![Val::from(key)])).await.map_err(|_| RefusalCode::Internal)?;
    match r {
        None => Ok(None),
        Some(r) => hold_row(&r).map(Some).ok_or(RefusalCode::Internal),
    }
}

/// A hold id or an idempotency key: 1 to 64 characters of `[0-9A-Za-z_-]`.
pub fn key_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A published package digest: 64 lowercase hex digits.
fn digest_ok(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The `BILLING` state that lets a peek or a new hold through.
pub fn billing_open(state: BillingState) -> Result<(), RefusalCode> {
    match state {
        BillingState::Grants => Ok(()),
        BillingState::Off => Err(RefusalCode::PaidOff),
        // The processor is not built: nothing backs `live` yet.
        BillingState::Live => Err(RefusalCode::NotConfigured),
    }
}

async fn require_active<H: Host>(host: &H, user: &UserId) -> Result<(), RefusalCode> {
    match crate::store::user_state(host, user).await {
        Ok(Some(UserState::Active)) => Ok(()),
        Ok(_) => Err(RefusalCode::AccountUnavailable),
        Err(_) => Err(RefusalCode::Internal),
    }
}

// ------------------------------------------------------------------- routes

/// `POST /internal/v1/billing/peek`: what the account can draw. Each peek is
/// a quote's, so it takes one from the account's `quote` bucket.
pub async fn peek<H: Host>(cfg: &Config, host: &H, req: &Req, sw: &Switches) -> Resp {
    let body: PeekRequest = match http::json(&req.body) {
        Ok(b) => b,
        Err(c) => return Resp::refusal(c),
    };
    match peek_answer(cfg, host, sw, &body.user_id).await {
        Ok(a) => Resp::json(200, &a),
        Err(c) => Resp::refusal(c),
    }
}

async fn peek_answer<H: Host>(cfg: &Config, host: &H, sw: &Switches, user: &UserId) -> Result<PeekAnswer, RefusalCode> {
    billing_open(sw.billing())?;
    let policy = cfg.billing.as_ref().ok_or(RefusalCode::NotConfigured)?;
    require_active(host, user).await?;
    let bucket = Bucket { kind: BucketKind::Quote, key: user.hex() };
    let size = policy.bucket(BucketKind::Quote).ok_or(RefusalCode::NotConfigured)?;
    match host.quota_take(&cfg.keys.quota_name(&bucket.name()), size).await {
        Ok(t) if t.taken => {}
        Ok(_) => return Err(RefusalCode::Quota),
        Err(_) => return Err(RefusalCode::Unavailable),
    }
    let now = host.now_ms();
    let read = |_: DbError| {
        crate::metrics::error(host, "d1");
        RefusalCode::Internal
    };
    let period = host
        .first(crate::stmt!(SELECT_PERIOD, user.0, now, now))
        .await
        .map_err(read)?
        .and_then(|r| Some((r.u64("plan_left")?, r.u64("api_left")?)));
    let credit = host.first(crate::stmt!(SELECT_CREDIT, user.0)).await.map_err(read)?.and_then(|r| r.u64("n"));
    let flags = host
        .first(crate::stmt!("SELECT flags FROM users WHERE user_id = ?", user.0))
        .await
        .map_err(read)?
        .and_then(|r| r.u64("flags"))
        .unwrap_or(0);
    let (plan_left, api_left) = period.unwrap_or((0, 0));
    Ok(PeekAnswer {
        plan_left,
        api_left,
        credit_left: credit.unwrap_or(0),
        paid_preview: flags & u64::from(slop_ninja_detect_page::sn::flags::PAID_PREVIEW) != 0,
    })
}

/// `POST /internal/v1/billing/hold`: idempotent by `idem`.
pub async fn hold<H: Host>(host: &H, req: &Req, sw: &Switches) -> Resp {
    let body: HoldRequest = match http::json(&req.body) {
        Ok(b) => b,
        Err(c) => return Resp::refusal(c),
    };
    match place(host, sw, &body).await {
        Ok(a) => Resp::json(200, &a),
        Err(c) => Resp::refusal(c),
    }
}

/// The hold of `req` (the route's logic, for the tests too).
///
/// An `idem` backs one check. Its hold is answered again only while it has
/// not ended (a confirm sent twice); once it has ended, the `idem` answers
/// `quote_invalid` and places nothing, so the reader quotes again. A
/// released hold answered again would back a new job whose tokens are
/// already back in the balance, and a committed one a second job on one
/// debit.
pub async fn place<H: Host>(host: &H, sw: &Switches, req: &HoldRequest) -> Result<HoldAnswer, RefusalCode> {
    if !key_ok(&req.idem) || !credit::holds_on(req.channel) || req.tokens == 0 || req.tokens > MAX_HOLD_TOKENS {
        return Err(RefusalCode::MalformedRequest);
    }
    billing_open(sw.billing())?;
    let existing = |h: HoldRow| {
        if h.user_id != req.user_id {
            Err(RefusalCode::MalformedRequest)
        } else if h.ended.is_some() {
            Err(RefusalCode::QuoteInvalid)
        } else {
            Ok(HoldAnswer { hold_id: h.hold_id, parts: h.parts })
        }
    };
    if let Some(h) = read_hold(host, SELECT_HOLD_BY_IDEM, &req.idem).await? {
        return existing(h);
    }
    require_active(host, &req.user_id).await?;
    let hold_id = format!("h{}", hex::encode(host.random::<16>()));
    for _attempt in 0..2 {
        let now = host.now_ms();
        let src = sources(host, &req.user_id, now).await.map_err(|_| RefusalCode::Internal)?;
        let Some(parts) = plan(req.tokens, req.channel, &src) else { return Err(RefusalCode::NotEnoughCredit) };
        let mut stmts = vec![guard(USER_ACTIVE, vec![Val::from(req.user_id.0)])];
        stmts.extend(parts.iter().map(|p| source_guard(p, &req.user_id)));
        stmts.extend(parts.iter().map(|p| take_stmt(p, req.channel)));
        let parts_json = serde_json::to_string(&parts).map_err(|_| RefusalCode::Internal)?;
        stmts.push(crate::stmt!(
            "INSERT INTO holds (hold_id, user_id, channel, tokens, parts, idem, state, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, 'held', ?, ?)",
            hold_id.as_str(),
            req.user_id.0,
            req.channel.as_str(),
            total(&parts),
            parts_json,
            req.idem.as_str(),
            now,
            now
        ));
        match host.batch(stmts).await {
            Ok(_) => {
                crate::metrics::ledger(host, "hold");
                return Ok(HoldAnswer { hold_id, parts });
            }
            Err(DbError::Unavailable) => return Err(RefusalCode::Internal),
            // A rollback: another hold took the headroom, a source closed or
            // moved, or another request placed this `idem`'s hold.
            Err(DbError::Constraint | DbError::Guard) => {
                if let Some(h) = read_hold(host, SELECT_HOLD_BY_IDEM, &req.idem).await? {
                    return existing(h);
                }
                require_active(host, &req.user_id).await?;
            }
        }
    }
    Err(RefusalCode::NotEnoughCredit)
}

/// `POST /internal/v1/billing/commit`: idempotent by `hold_id`.
pub async fn commit<H: Host>(host: &H, req: &Req) -> Resp {
    let body: CommitRequest = match http::json(&req.body) {
        Ok(b) => b,
        Err(c) => return Resp::refusal(c),
    };
    answer(
        end_hold(host, &body.hold_id, End::Commit { receipt_id: &body.receipt_id, package_pub: &body.package_pub })
            .await,
    )
}

/// `POST /internal/v1/billing/release`: idempotent by `hold_id`.
pub async fn release<H: Host>(host: &H, req: &Req) -> Resp {
    let body: ReleaseRequest = match http::json(&req.body) {
        Ok(b) => b,
        Err(c) => return Resp::refusal(c),
    };
    answer(end_hold(host, &body.hold_id, End::Release).await)
}

fn answer(r: Result<HoldEndAnswer, RefusalCode>) -> Resp {
    match r {
        Ok(a) => Resp::json(200, &a),
        Err(c) => Resp::refusal(c),
    }
}

/// How a hold is to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End<'a> {
    Commit { receipt_id: &'a str, package_pub: &'a str },
    Release,
}

/// Ends a hold once (the routes' logic, for the tests too).
pub async fn end_hold<H: Host>(host: &H, hold_id: &str, end: End<'_>) -> Result<HoldEndAnswer, RefusalCode> {
    if !key_ok(hold_id) {
        return Err(RefusalCode::MalformedRequest);
    }
    if let End::Commit { receipt_id, package_pub } = end
        && (!key_ok(receipt_id) || !digest_ok(package_pub))
    {
        return Err(RefusalCode::MalformedRequest);
    }
    let h = read_hold(host, SELECT_HOLD, hold_id).await?.ok_or(RefusalCode::NotFound)?;
    if let Some(ended) = h.ended {
        return Ok(HoldEndAnswer { hold_id: h.hold_id, ended, by_this_call: false });
    }
    let now = host.now_ms();
    let (kind, state) = match end {
        End::Commit { .. } => (HoldEnd::Committed, "committed"),
        End::Release => (HoldEnd::Released, "released"),
    };
    let mut stmts =
        vec![crate::stmt!("INSERT INTO hold_ends (hold_id, kind, at) VALUES (?, ?, ?)", hold_id, kind.as_str(), now)];
    for p in &h.parts {
        stmts.push(match end {
            End::Commit { receipt_id, package_pub } => crate::stmt!(
                "INSERT INTO debits (hold_id, user_id, src, src_id, tokens, cash, package_pub, receipt_id, channel, at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                hold_id,
                h.user_id.0,
                p.src.as_str(),
                p.id,
                p.tokens,
                p.cash,
                package_pub,
                receipt_id,
                h.channel.as_str(),
                now
            ),
            End::Release => return_stmt(p, h.channel),
        });
    }
    stmts.push(crate::stmt!(
        "UPDATE holds SET state = ?, updated_at = ? WHERE hold_id = ? AND state = 'held'",
        state,
        now,
        hold_id
    ));
    match host.batch(stmts).await {
        Ok(_) => {
            crate::metrics::ledger(host, state);
            Ok(HoldEndAnswer { hold_id: h.hold_id, ended: kind, by_this_call: true })
        }
        Err(DbError::Constraint) => {
            // Another call ended it first: answer that end.
            let again = read_hold(host, SELECT_HOLD, hold_id).await?.ok_or(RefusalCode::NotFound)?;
            let ended = again.ended.ok_or(RefusalCode::Internal)?;
            Ok(HoldEndAnswer { hold_id: again.hold_id, ended, by_this_call: false })
        }
        Err(_) => Err(RefusalCode::Internal),
    }
}

// ------------------------------------------------------------- operator ops

/// The longest plan period an operator grants, in days: a plan's period is
/// a month, so a year is the bound past which a figure is a typo.
pub const MAX_PLAN_DAYS: u64 = 366;
const DAY_MS: u64 = 86_400_000;

/// `grant_credit`: a non-cash payment of `tokens`, journaled first so the
/// row names its journal entry.
pub fn grant_credit_stmt(user: &UserId, tokens: u64, now: u64) -> Stmt {
    crate::stmt!(
        "INSERT INTO payments (user_id, kind, cash, tokens_total, tokens_left, state, created_at, journal_seq) \
         VALUES (?, 'grant', 0, ?, ?, 'active', ?, (SELECT MAX(seq) FROM journal)) RETURNING payment_id",
        user.0,
        tokens,
        tokens,
        now
    )
}

/// `grant_plan`: a non-cash payment holding no credit, which backs a plan
/// period of `days` with `allowance` tokens and `api_cap` of them usable on
/// the API. The period names that payment through the journal entry written
/// first in the same batch.
pub fn grant_plan_stmts(user: &UserId, days: u64, allowance: u64, api_cap: u64, now: u64) -> Vec<Stmt> {
    let ends = now.saturating_add(days.saturating_mul(DAY_MS));
    vec![
        crate::stmt!(
            "INSERT INTO payments (user_id, kind, cash, tokens_total, tokens_left, state, created_at, journal_seq) \
             VALUES (?, 'grant', 0, 0, 0, 'active', ?, (SELECT MAX(seq) FROM journal)) RETURNING payment_id",
            user.0,
            now
        ),
        crate::stmt!(
            "INSERT INTO plan_periods (user_id, payment_id, starts_at, ends_at, allowance, api_cap, allowance_granted, \
             api_cap_granted, used, api_used, cash, state) VALUES (?, (SELECT payment_id FROM payments WHERE user_id = ? \
             AND journal_seq = (SELECT MAX(seq) FROM journal)), ?, ?, ?, ?, ?, ?, 0, 0, 0, 'active') \
             RETURNING period_id, payment_id",
            user.0,
            user.0,
            now,
            ends,
            allowance,
            api_cap,
            allowance,
            api_cap
        ),
    ]
}

/// `void_grant`: the grant payment closes (`void`, no tokens left) and any
/// plan period it backs is reversed (no headroom left), in one statement
/// each, as the closed-source CHECKs require. Holds already placed on it
/// still commit; a release returns nothing into it.
pub fn void_grant_stmts(payment_id: u64) -> Vec<Stmt> {
    vec![
        guard(
            "EXISTS (SELECT 1 FROM payments WHERE payment_id = ? AND kind = 'grant' AND state = 'active')",
            vec![Val::from(payment_id)],
        ),
        crate::stmt!(
            "UPDATE payments SET state = 'void', tokens_left = 0 WHERE payment_id = ? AND kind = 'grant' AND state = 'active'",
            payment_id
        ),
        crate::stmt!(
            "UPDATE plan_periods SET state = 'reversed', allowance = used, api_cap = api_used \
             WHERE payment_id = ? AND state = 'active' RETURNING period_id",
            payment_id
        ),
    ]
}

/// The hourly end of plan periods past `ends_at`: no rollover, so the period
/// keeps only what it used.
pub fn end_periods_stmt(now: u64, limit: u32) -> Stmt {
    crate::stmt!(
        "UPDATE plan_periods SET state = 'ended', allowance = used, api_cap = api_used \
         WHERE rowid IN (SELECT rowid FROM plan_periods WHERE state = 'active' AND ends_at <= ? LIMIT ?) \
         RETURNING period_id",
        now,
        limit
    )
}

/// The grant's own size: what a grant op is given, checked.
pub fn grant_tokens_ok(tokens: u64) -> bool {
    (1..=MAX_HOLD_TOKENS).contains(&tokens)
}

/// A plan grant's figures, checked: `days` within [`MAX_PLAN_DAYS`], an
/// allowance a hold can draw, and an API part within it (by default the
/// smaller of the allowance and the policy's API part).
pub fn plan_figures(days: u64, allowance: u64, api_cap: Option<u64>) -> Option<(u64, u64, u64)> {
    if !(1..=MAX_PLAN_DAYS).contains(&days) || !grant_tokens_ok(allowance) {
        return None;
    }
    let api = api_cap.unwrap_or(allowance.min(credit::PLAN_API_TOKENS));
    (api <= allowance).then_some((days, allowance, api))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(period: Option<(u64, u64)>, payments: &[u64]) -> Sources {
        Sources {
            period: period.map(|(left, api_left)| Period { id: 7, left, api_left, cash: false }),
            payments: payments
                .iter()
                .enumerate()
                .map(|(i, &left)| Payment { id: i as u64 + 1, left, cash: false })
                .collect(),
        }
    }

    #[test]
    fn plan_draws_the_period_then_payments_first_paid_first() {
        let s = src(Some((1_000, 40_960)), &[500, 2_000, 9_000]);
        let parts = plan(3_000, Channel::PaidPage, &s).unwrap();
        let got: Vec<(PartSource, u64, u64)> = parts.iter().map(|p| (p.src, p.id, p.tokens)).collect();
        assert_eq!(
            got,
            vec![(PartSource::Period, 7, 1_000), (PartSource::Payment, 1, 500), (PartSource::Payment, 2, 1_500)]
        );
        assert_eq!(total(&parts), 3_000);
        assert!(plan(12_501, Channel::PaidPage, &s).is_none());
        assert_eq!(total(&plan(12_500, Channel::PaidPage, &s).unwrap()), 12_500);
    }

    #[test]
    fn a_short_check_takes_the_plan_minimum_and_no_credit() {
        let s = src(Some((10_000, 40_960)), &[500]);
        let parts = plan(200, Channel::PaidPage, &s).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!((parts[0].src, parts[0].tokens), (PartSource::Period, credit::PLAN_MIN_TOKENS));
        let credit_only = plan(50, Channel::PaidPage, &src(None, &[500])).unwrap();
        assert_eq!(total(&credit_only), credit::PAYG_MIN_TOKENS);
    }

    #[test]
    fn the_policy_parses_and_refuses_empty_buckets() {
        assert_eq!(BillingPolicy::from_var("").unwrap(), BillingPolicy::v1());
        assert_eq!(BillingPolicy::from_var(VERSION).unwrap(), BillingPolicy::v1());
        let json = serde_json::to_string(&BillingPolicy::v1()).unwrap();
        assert_eq!(BillingPolicy::from_var(&json).unwrap(), BillingPolicy::v1());
        let mut p = BillingPolicy::v1();
        p.quote_quota.value.capacity = 0;
        assert_eq!(BillingPolicy::from_var(&serde_json::to_string(&p).unwrap()), Err("quote_quota"));
        assert!(BillingPolicy::from_var("{\"version\":\"billing-policy/v2\"}").is_err());
        assert_eq!(BillingPolicy::v1().bucket(BucketKind::Email), None);
    }

    #[test]
    fn keys_and_plan_figures() {
        assert!(key_ok("h0123abc") && key_ok("q_1-x") && !key_ok("") && !key_ok("a b") && !key_ok(&"a".repeat(65)));
        assert_eq!(plan_figures(30, 409_600, None), Some((30, 409_600, credit::PLAN_API_TOKENS)));
        assert_eq!(plan_figures(30, 1_000, None), Some((30, 1_000, 1_000)));
        assert_eq!(plan_figures(30, 1_000, Some(2_000)), None);
        assert_eq!(plan_figures(0, 1_000, None), None);
        assert_eq!(plan_figures(367, 1_000, None), None);
        assert_eq!(plan_figures(30, 0, None), None);
    }
}
