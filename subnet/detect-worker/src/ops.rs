//! Signed detect ops (`POST /api/detect/v1/op`, DESIGN 14.2).
//!
//! The body is the bare signed-op envelope (`slop-ninja-signed-op`'s `Wire`):
//! nothing travels beside it, so a body carrying any other field, an X link
//! included, is refused as malformed. The Worker verifies it under its own
//! environment's operator key (`DETECT_OP_KEY`, `OP_ENV`; staging and
//! production never share a key), for the `detect` target (and `edge` for
//! the coverage probe's op), inside the op's validity window, then applies
//! the op and its journal row in one D1 batch. The journal's `nonce_h
//! UNIQUE` makes a reused nonce fail the whole batch; the route also reads
//! it first, so a replay is refused before anything else runs.
//!
//! **The journal.** Each row holds the signed op exactly: `op` is the
//! canonical JSON the signature covers (after its fixed prefix),
//! `signature` the signature and `actor` `operator:<key fingerprint>`, so a
//! row verifies on its own and `detect-op replay` can sign its arguments
//! again after a D1 point-in-time restore. Two ops keep a redacted row
//! instead (signature NULL, nonce kept): `case_note`, whose note is kept
//! encrypted beside the case and never in D1, and `owner_match`, whose
//! cursor is an account id and which changes nothing. A `purge` that names
//! a secret page keeps a redacted row too (its site URLs and the count of
//! the others), so no slug is journaled. A row names a post only by the
//! hash its tombstone keeps (`x_remove`'s `canon_key`), never by its link
//! or id.
//!
//! | Op | Args (each also takes `reason`, required but on `status`) |
//! |---|---|
//! | `withhold_excerpt` | `case_id`, `basis`?, `jurisdiction`? |
//! | `remove` | `case_id` or `slug` (a public page), `basis`?, `jurisdiction`? |
//! | `restore` | `case_id` or `slug` |
//! | `reject`, `close_case`, `case_show` | `case_id` |
//! | `case_note` | `case_id`, `note` |
//! | `review_strike` | `case_id`, `counts` |
//! | `owner_match` | `case_id`, `after`? (an account id: where the last answer's `next` left off) |
//! | `x_remove` | `reason` (`author`, `x` or `other`), `canon_key` (64 lowercase hex digits) |
//! | `set_switch` | `name` (a detect switch), `state` |
//! | `index_force` | `slug`, `value` (`on`, `off` or null) |
//! | `delete_private` | `id_h` (64 lowercase hex digits) |
//! | `purge` | `urls` |
//! | `set_examples` | `slots` (`[{slot, public_slug}]` or `[{slot, doc_key}]`) |
//! | `status` | none |
//!
//! `exclude_host`, `rerun`, `release_claim` and `x_purge` belong to the
//! URL pipeline's build, and `edge_coverage` to the edge tooling's; until
//! those land they answer `not_configured` without spending the nonce.

use serde_json::{Map, Value, json};
use slop_ninja_detect_page::ids::{IdH, UserId};
use slop_ninja_detect_page::origins::Origins;
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_page::slug;
use slop_ninja_signed_op::{Env, OperatorKey, SIGNED_PREFIX, Target, Verified, Verifier};

use crate::cases::{self, Accounts, Basis, CaseRow, XCase, XReason};
use crate::db::{Io, Stmt, Val};
use crate::keys::{Bytes32, parse_key_hex};
use crate::lookup::{SlugForm, slug_form};
use crate::share::Keys;
use crate::store_ops::{self, ExampleSlot};

/// The longest op body read: an envelope with a note of at most
/// [`cases::NOTE_BYTES`], escaped, and room for its other fields.
pub const OP_BODY_MAX: usize = 4 * cases::NOTE_BYTES;
/// The longest `reason`.
pub const REASON_MAX: usize = 1000;
/// The var of the operator's verifying key (64 lowercase hex digits).
pub const OP_KEY_VAR: &str = "DETECT_OP_KEY";
/// The var naming this deployment's environment (`production` or
/// `staging`).
pub const OP_ENV_VAR: &str = "OP_ENV";

/// The ops whose handlers come with other builds: refused before the nonce
/// is spent.
pub const NOT_BUILT: &[&str] = &["exclude_host", "rerun", "release_claim", "x_purge", "edge_coverage"];

/// The op route's key and environment, from the vars; `None` (the route
/// answers `not_configured`) when either is missing or malformed.
pub fn op_config(io: &impl Io) -> Option<(OperatorKey, Env)> {
    let key = OperatorKey::parse(&io.var(OP_KEY_VAR)?)?;
    let env = Env::parse(&io.var(OP_ENV_VAR)?)?;
    Some((key, env))
}

/// What the op handlers need besides the store.
pub struct OpCtx<'a, A> {
    pub keys: Keys<'a>,
    pub origins: &'a Origins,
    pub accounts: &'a A,
}

/// The canonical JSON a signature covers, as the journal keeps it.
pub fn signed_text(v: &Verified) -> Result<String, RefusalCode> {
    let b = v.op.signed_bytes().map_err(|_| RefusalCode::OpMalformed)?;
    let body = b.strip_prefix(SIGNED_PREFIX).ok_or(RefusalCode::OpMalformed)?;
    String::from_utf8(body.to_vec()).map_err(|_| RefusalCode::OpMalformed)
}

/// The journal row of a verified op: the op exactly, signed.
pub fn journal_signed(v: &Verified, now: u64) -> Result<Stmt, RefusalCode> {
    Ok(Stmt::new(
        "INSERT INTO op_journal (at, actor, op, signature, nonce_h) VALUES (?1, ?2, ?3, ?4, ?5)",
        vec![
            Val::int(now),
            Val::text(v.actor()),
            Val::text(signed_text(v)?),
            Val::blob(&v.sig),
            Val::blob(&v.nonce_h()),
        ],
    ))
}

/// A redacted journal row: the op's name and the arguments given, unsigned,
/// with the nonce kept so the op works once.
pub fn journal_redacted(v: &Verified, args: Value, now: u64) -> Stmt {
    let op = cases::canonical(&json!({"op": v.op.op, "args": args, "redacted": true}));
    Stmt::new(
        "INSERT INTO op_journal (at, actor, op, signature, nonce_h) VALUES (?1, ?2, ?3, NULL, ?4)",
        vec![Val::int(now), Val::text(v.actor()), Val::text(op), Val::blob(&v.nonce_h())],
    )
}

const NONCE_USED: &str = "SELECT seq FROM op_journal WHERE nonce_h = ?1";

/// One verified op being applied.
struct Op<'a> {
    v: &'a Verified,
    now: u64,
}

impl Op<'_> {
    fn args(&self) -> &Map<String, Value> {
        &self.v.op.args
    }

    fn str(&self, k: &str) -> Result<&str, RefusalCode> {
        self.args().get(k).and_then(Value::as_str).ok_or(RefusalCode::OpMalformed)
    }

    fn opt_str(&self, k: &str) -> Result<Option<&str>, RefusalCode> {
        match self.args().get(k) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(RefusalCode::OpMalformed),
        }
    }

    fn bool(&self, k: &str) -> Result<bool, RefusalCode> {
        self.args().get(k).and_then(Value::as_bool).ok_or(RefusalCode::OpMalformed)
    }

    /// Only these keys (and `reason`) may appear.
    fn only(&self, keys: &[&str]) -> Result<(), RefusalCode> {
        if self.args().keys().all(|k| k == "reason" || keys.contains(&k.as_str())) {
            Ok(())
        } else {
            Err(RefusalCode::OpMalformed)
        }
    }

    fn journal(&self) -> Result<Stmt, RefusalCode> {
        journal_signed(self.v, self.now)
    }
}

fn hex32(s: &str) -> Option<Bytes32> {
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .then(|| parse_key_hex(s))
        .flatten()
}

/// Whether a purge URL may name a secret page: any URL not on the site
/// origin, and one on it whose path holds a run of a secret slug's length
/// in the slug alphabet (as a page path folds it).
pub fn names_secret(origins: &Origins, url: &str) -> bool {
    let on = |o: &str| url.strip_prefix(o).filter(|rest| rest.starts_with('/'));
    let (Some(path), None) = (on(origins.site.as_str()), on(origins.secret.as_str())) else { return true };
    let mut run = 0;
    for b in path.bytes() {
        run = if slug::value(cases::fold_byte(b)).is_some() { run + 1 } else { 0 };
        if run >= slug::SECRET_LEN {
            return true;
        }
    }
    false
}

/// `POST /api/detect/v1/op`: verifies, journals and applies one op.
pub async fn op<A: Accounts>(io: &impl Io, ctx: &OpCtx<'_, A>, body: &[u8]) -> Result<Value, RefusalCode> {
    let (key, env) = op_config(io).ok_or(RefusalCode::NotConfigured)?;
    if body.len() > OP_BODY_MAX {
        return Err(RefusalCode::TooLarge);
    }
    let now = io.now_ms();
    let v = Verifier::new(key, env, &[Target::Detect, Target::Edge])
        .verify(body, now)
        .map_err(|r| RefusalCode::parse(r.code()).unwrap_or(RefusalCode::OpMalformed))?;
    let name = v.op.op.clone();
    if NOT_BUILT.contains(&name.as_str()) {
        return Err(RefusalCode::NotConfigured);
    }
    let used =
        io.run(Stmt::new(NONCE_USED, vec![Val::blob(&v.nonce_h())])).await.map_err(|_| RefusalCode::Unavailable)?;
    if used.first().is_some() {
        return Err(RefusalCode::OpReusedNonce);
    }
    if name != "status" {
        match v.op.args.get("reason").and_then(Value::as_str) {
            Some(r) if !r.trim().is_empty() && r.len() <= REASON_MAX => {}
            _ => return Err(RefusalCode::OpMalformed),
        }
    }
    let o = Op { v: &v, now };
    let r = dispatch(io, ctx, &o, &name).await;
    let mut extra = match r {
        Ok(x) => x,
        Err(code) => {
            // A batch that failed on the nonce's UNIQUE (two posts of one
            // op at once) answers as a reused nonce.
            if code == RefusalCode::Unavailable
                && let Ok(d) = io.run(Stmt::new(NONCE_USED, vec![Val::blob(&v.nonce_h())])).await
                && d.first().is_some()
            {
                return Err(RefusalCode::OpReusedNonce);
            }
            return Err(code);
        }
    };
    let seq = io
        .run(Stmt::new(NONCE_USED, vec![Val::blob(&v.nonce_h())]))
        .await
        .ok()
        .and_then(|d| d.first().and_then(|r| r.u64("seq")));
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("op".into(), json!(name));
    out.insert("seq".into(), json!(seq));
    if let Value::Object(m) = &mut extra {
        out.append(m);
    }
    Ok(Value::Object(out))
}

async fn case_of(io: &impl Io, o: &Op<'_>) -> Result<CaseRow, RefusalCode> {
    cases::read_case(io, o.str("case_id")?).await
}

async fn dispatch<A: Accounts>(io: &impl Io, ctx: &OpCtx<'_, A>, o: &Op<'_>, name: &str) -> Result<Value, RefusalCode> {
    let store = ctx.keys.store;
    match name {
        "withhold_excerpt" => {
            o.only(&["case_id", "basis", "jurisdiction"])?;
            let case = case_of(io, o).await?;
            let basis = Basis::of(o.args(), Some(case.kind))?;
            let e = cases::withhold_excerpt(io, ctx.origins, &case, &basis, o.journal()?).await?;
            Ok(json!(e))
        }
        "remove" => {
            o.only(&["case_id", "slug", "basis", "jurisdiction"])?;
            match (o.opt_str("case_id")?, o.opt_str("slug")?) {
                (Some(_), None) => {
                    let case = case_of(io, o).await?;
                    let basis = Basis::of(o.args(), Some(case.kind))?;
                    Ok(json!(cases::remove_case(io, store, ctx.origins, &case, &basis, o.journal()?).await?))
                }
                (None, Some(slug)) if slug_form(slug) == Some(SlugForm::Public) => {
                    let basis = Basis::of(o.args(), None)?;
                    Ok(json!(cases::remove_slug(io, ctx.origins, slug, &basis, o.journal()?).await?))
                }
                _ => Err(RefusalCode::OpMalformed),
            }
        }
        "restore" => {
            o.only(&["case_id", "slug"])?;
            match (o.opt_str("case_id")?, o.opt_str("slug")?) {
                (Some(_), None) => {
                    let case = case_of(io, o).await?;
                    Ok(json!(cases::restore_case(io, store, ctx.origins, &case, o.journal()?).await?))
                }
                (None, Some(slug)) if slug_form(slug) == Some(SlugForm::Public) => {
                    Ok(json!(cases::restore_slug(io, ctx.origins, slug, o.journal()?).await?))
                }
                _ => Err(RefusalCode::OpMalformed),
            }
        }
        "reject" => {
            o.only(&["case_id"])?;
            let case = case_of(io, o).await?;
            Ok(json!(cases::reject_case(io, ctx.origins, &case, o.journal()?).await?))
        }
        "close_case" => {
            o.only(&["case_id"])?;
            let case = case_of(io, o).await?;
            cases::close_case(io, &case, o.journal()?).await
        }
        "case_note" => {
            o.only(&["case_id", "note"])?;
            let case = case_of(io, o).await?;
            let note = o.str("note")?;
            let journal = journal_redacted(o.v, json!({"case_id": case.case_id}), o.now);
            cases::case_note(io, store, &case, note, &o.v.op.nonce.hex(), journal).await
        }
        "case_show" => {
            o.only(&["case_id"])?;
            let case = case_of(io, o).await?;
            io.batch(vec![o.journal()?]).await.map_err(|_| RefusalCode::Unavailable)?;
            cases::case_show(io, store, &case).await
        }
        "review_strike" => {
            o.only(&["case_id", "counts"])?;
            let case = case_of(io, o).await?;
            cases::review_strike(io, &case, o.bool("counts")?, o.journal()?).await
        }
        "owner_match" => {
            o.only(&["case_id", "after"])?;
            let case = case_of(io, o).await?;
            let after = match o.opt_str("after")? {
                None => None,
                Some(s) => Some(UserId::parse(s).ok_or(RefusalCode::OpMalformed)?),
            };
            let journal = journal_redacted(o.v, json!({"case_id": case.case_id}), o.now);
            io.batch(vec![journal]).await.map_err(|_| RefusalCode::Unavailable)?;
            cases::owner_match(io, ctx.accounts, store, &case, after).await
        }
        "x_remove" => {
            o.only(&["canon_key"])?;
            let reason = XReason::parse(o.str("reason")?).ok_or(RefusalCode::OpMalformed)?;
            let canon_key = hex32(o.str("canon_key")?).ok_or(RefusalCode::OpMalformed)?;
            let r = cases::x_removal(io, ctx.origins, &canon_key, reason, XCase::New, vec![o.journal()?]).await?;
            io.metric("removal", &["x_request", "x_removed"]);
            Ok(json!({"case_id": r.case_id, "removed": r.removed}))
        }
        "set_switch" => {
            o.only(&["name", "state"])?;
            let stmts = store_ops::set_switch(o.str("name")?, o.str("state")?, o.now)?;
            apply(io, o, stmts).await.map(|_| json!({}))
        }
        "index_force" => {
            o.only(&["slug", "value"])?;
            let value = match o.args().get("value") {
                Some(Value::Null) => None,
                Some(Value::String(s)) => {
                    Some(slop_ninja_detect_page::codes::IndexForce::parse(s).ok_or(RefusalCode::OpMalformed)?)
                }
                _ => return Err(RefusalCode::OpMalformed),
            };
            let stmts = store_ops::index_force(o.str("slug")?, value)?;
            apply(io, o, stmts).await.map(|_| json!({}))
        }
        "delete_private" => {
            o.only(&["id_h"])?;
            let id = IdH(hex32(o.str("id_h")?).ok_or(RefusalCode::OpMalformed)?);
            let done = apply(io, o, store_ops::delete_private(&id, o.now)).await?;
            let deleted = done.get(1).map_or(0, |d| d.changes);
            let empty = crate::db::Done::default();
            let keys = store_ops::delete_private_docs(&id, done.get(2).unwrap_or(&empty));
            let docs_pending = u64::from(deleted > 0 && crate::db::delete_docs(io, &keys).await.is_err());
            Ok(json!({"deleted": deleted, "docs_pending": docs_pending}))
        }
        "purge" => {
            o.only(&["urls"])?;
            let urls: Vec<String> = o
                .args()
                .get("urls")
                .and_then(Value::as_array)
                .ok_or(RefusalCode::OpMalformed)?
                .iter()
                .map(|u| u.as_str().map(str::to_owned).ok_or(RefusalCode::OpMalformed))
                .collect::<Result<_, _>>()?;
            let msg = store_ops::purge(ctx.origins, &urls)?;
            // A secret page's link opens it, and the journal is kept for
            // good: an op naming one keeps a redacted row that counts its
            // secret links (the purge message alone carries them, DESIGN 7.13).
            let public: Vec<&String> = urls.iter().filter(|u| !names_secret(ctx.origins, u)).collect();
            let journal = if public.len() == urls.len() {
                o.journal()?
            } else {
                journal_redacted(o.v, json!({"urls": public, "secret_urls": urls.len() - public.len()}), o.now)
            };
            io.batch(vec![journal]).await.map_err(|_| RefusalCode::Unavailable)?;
            io.enqueue(vec![msg]).await.map_err(|_| RefusalCode::Unavailable)?;
            Ok(json!({}))
        }
        "set_examples" => {
            o.only(&["slots"])?;
            let slots = o.args().get("slots").and_then(Value::as_array).ok_or(RefusalCode::OpMalformed)?;
            let mut parsed = Vec::with_capacity(slots.len());
            for s in slots {
                let slot = s.get("slot").and_then(Value::as_u64).and_then(|n| u8::try_from(n).ok());
                let slot = slot.ok_or(RefusalCode::OpMalformed)?;
                match (s.get("public_slug").and_then(Value::as_str), s.get("doc_key").and_then(Value::as_str)) {
                    (Some(p), None) => parsed.push(ExampleSlot::Public { slot, public_slug: p.to_owned() }),
                    (None, Some(k)) => parsed.push(ExampleSlot::Synthetic { slot, doc_key: k.to_owned() }),
                    _ => return Err(RefusalCode::OpMalformed),
                }
            }
            apply(io, o, store_ops::set_examples(&parsed)?).await.map(|_| json!({}))
        }
        "status" => {
            o.only(&[])?;
            io.batch(vec![o.journal()?]).await.map_err(|_| RefusalCode::Unavailable)?;
            let store_status = store_ops::status(io).await?;
            let case_status = cases::status(io).await?;
            Ok(json!({"store": store_status, "cases": case_status}))
        }
        _ => Err(RefusalCode::OpUnknown),
    }
}

/// Runs the journal row and `stmts` in one batch.
async fn apply(io: &impl Io, o: &Op<'_>, stmts: Vec<Stmt>) -> Result<Vec<crate::db::Done>, RefusalCode> {
    let mut batch = vec![o.journal()?];
    batch.extend(stmts);
    io.batch(batch).await.map_err(|_| RefusalCode::Unavailable)
}
