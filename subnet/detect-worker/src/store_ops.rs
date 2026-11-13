//! The store's operator op handlers: `set_switch`, `index_force`,
//! `delete_private`, `purge`, `set_examples` and `status`.
//!
//! The op route verifies the signed envelope (target, env, expiry, nonce,
//! signature) and inserts the journal row first in the op's batch; each
//! handler here adds its own statements to that batch, so the op and its
//! journal row apply together or not at all. A statement that records the
//! op's journal row reads it as `(SELECT max(seq) FROM op_journal)`, the row
//! inserted earlier in the same batch. Side effects that are not D1 (R2
//! deletes, purges) run after the batch and are repeated by a replay.

use serde::{Deserialize, Serialize};
use slop_ninja_detect_page::codes::{DetState, IndexForce, Kind};
use slop_ninja_detect_page::ids::IdH;
use slop_ninja_detect_page::origins::Origins;
use slop_ninja_detect_page::queue::{Job, QueueMessage};
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_page::switches::{self, Owner};

use crate::db::{Io, Stmt, Val};
use crate::keys::public_id_h;
use crate::lookup::{SlugForm, slug_form};
use crate::sitemap::SHARD_URLS;

/// The journal row the batch inserted first.
pub const JOURNAL_SEQ: &str = "(SELECT max(seq) FROM op_journal)";

/// `set_switch {name, state, reason}` on a detect switch.
pub fn set_switch(name: &str, state: &str, now: u64) -> Result<Vec<Stmt>, RefusalCode> {
    let sw = switches::switch(name).filter(|s| s.owner == Owner::Detect).ok_or(RefusalCode::OpMalformed)?;
    if !sw.states.contains(&state) {
        return Err(RefusalCode::OpMalformed);
    }
    Ok(vec![Stmt::new(
        "INSERT INTO switches (name, state, at, journal_seq) VALUES (?1, ?2, ?3, (SELECT max(seq) FROM op_journal)) \
ON CONFLICT (name) DO UPDATE SET state = excluded.state, at = excluded.at, journal_seq = excluded.journal_seq",
        vec![Val::text(name), Val::text(state), Val::int(now)],
    )])
}

/// `index_force {slug, value}` on a public page (`None` clears it); its
/// sitemap shard is marked for the next rebuild.
pub fn index_force(slug: &str, value: Option<IndexForce>) -> Result<Vec<Stmt>, RefusalCode> {
    if slug_form(slug) != Some(SlugForm::Public) {
        return Err(RefusalCode::OpMalformed);
    }
    let id = public_id_h(slug);
    Ok(vec![
        Stmt::new(
            "UPDATE detections SET index_force = ?2, state_v = state_v + 1 WHERE id_h = ?1 AND kind = 'public_url'",
            vec![Val::blob(&id), Val::opt_text(value.map(|v| v.as_str()))],
        ),
        Stmt::new(
            "INSERT INTO sitemap_shards (shard, dirty, urls, built_at) SELECT sitemap_seq / ?2, 1, 0, NULL \
FROM detections WHERE id_h = ?1 AND sitemap_seq IS NOT NULL ON CONFLICT (shard) DO UPDATE SET dirty = 1",
            vec![Val::blob(&id), Val::int(SHARD_URLS)],
        ),
    ])
}

/// `delete_private {id_h}`: tombstones a secret page that is not already
/// ended, with its copies when it is a family root. The caller deletes the
/// documents after the batch ([`delete_private_docs`]).
pub fn delete_private(id_h: &IdH, now: u64) -> Vec<Stmt> {
    vec![
        Stmt::new(
            "UPDATE detections SET state = 'deleted', doc_key = NULL, doc_bytes = NULL, manage_h = NULL, \
owner_h = NULL, owner_via = NULL, state_v = state_v + 1, updated_at = ?2 WHERE id_h = ?1 \
AND kind IN ('private_text','private_paid') AND state NOT IN ('deleted','expired')",
            vec![Val::blob(&id_h.0), Val::int(now)],
        ),
        Stmt::new(crate::manage::DELETE_COPIES, vec![Val::blob(&id_h.0), Val::int(now)]),
    ]
}

/// The documents `delete_private` leaves to delete: the page's, and those
/// of the copies its second statement returned.
pub fn delete_private_docs(id_h: &IdH, copies: &crate::db::Done) -> Vec<String> {
    let mut keys = vec![crate::store::secret_doc_key(&id_h.0)];
    keys.extend(copies.rows.iter().filter_map(|r| r.hex32("id_h")).map(|c| crate::store::secret_doc_key(&c)));
    keys
}

/// `purge {urls}`: a purge of pages on the two public origins only.
pub fn purge(origins: &Origins, urls: &[String]) -> Result<QueueMessage, RefusalCode> {
    let ours = |u: &str| {
        [origins.site.as_str(), origins.secret.as_str()]
            .iter()
            .any(|o| u.strip_prefix(o).is_some_and(|rest| rest.starts_with('/')))
    };
    if urls.is_empty() || urls.len() > 30 || !urls.iter().all(|u| ours(u)) {
        return Err(RefusalCode::OpMalformed);
    }
    Ok(QueueMessage::new(Job::Purge { urls: urls.to_vec() }))
}

/// One example slot of `set_examples`.
#[derive(Clone, Debug, PartialEq)]
pub enum ExampleSlot {
    /// A public page.
    Public { slot: u8, public_slug: String },
    /// A synthetic example's page JSON, scored by the op from the compiled
    /// fixture set and put at `doc_key` before the batch.
    Synthetic { slot: u8, doc_key: String },
}

/// `set_examples`: replaces the panel's slots (an empty list clears it).
pub fn set_examples(slots: &[ExampleSlot]) -> Result<Vec<Stmt>, RefusalCode> {
    let mut out = vec![Stmt::new("DELETE FROM examples", Vec::new())];
    for s in slots {
        let (slot, source, public_slug, doc_key) = match s {
            ExampleSlot::Public { slot, public_slug } => {
                if slug_form(public_slug) != Some(SlugForm::Public) {
                    return Err(RefusalCode::OpMalformed);
                }
                (*slot, "public", Some(public_slug.clone()), None)
            }
            ExampleSlot::Synthetic { slot, doc_key } => {
                if !doc_key.starts_with("x/examples/") {
                    return Err(RefusalCode::OpMalformed);
                }
                (*slot, "synthetic", None, Some(doc_key.clone()))
            }
        };
        if !(1..=4).contains(&slot) {
            return Err(RefusalCode::OpMalformed);
        }
        out.push(Stmt::new(
            "INSERT INTO examples (slot, source, public_slug, doc_key, journal_seq) \
VALUES (?1, ?2, ?3, ?4, (SELECT max(seq) FROM op_journal))",
            vec![Val::int(u64::from(slot)), Val::text(source), Val::opt_text(public_slug), Val::opt_text(doc_key)],
        ));
    }
    Ok(out)
}

/// The `status` op's store part (content-free counts).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreStatus {
    /// Detections by kind and state.
    pub detections: Vec<(Kind, DetState, u64)>,
    /// Claims whose lease has not ended.
    pub live_claims: u64,
    /// Sitemap shards waiting for the rebuild.
    pub dirty_shards: u64,
    /// The switch rows.
    pub switches: Vec<(String, String)>,
    /// Live secret pages past their expiry (the sweep's backlog).
    pub expiry_backlog: u64,
}

/// Reads [`StoreStatus`].
pub async fn status(io: &impl Io) -> Result<StoreStatus, RefusalCode> {
    let now = io.now_ms();
    let done = io
        .batch(vec![
            Stmt::new(
                "SELECT kind, state, count(*) AS n FROM detections GROUP BY kind, state ORDER BY kind, state",
                Vec::new(),
            ),
            Stmt::new("SELECT count(*) AS n FROM claims WHERE lease_until >= ?1", vec![Val::int(now)]),
            Stmt::new("SELECT count(*) AS n FROM sitemap_shards WHERE dirty = 1", Vec::new()),
            Stmt::new(crate::lookup::SELECT_SWITCHES, Vec::new()),
            Stmt::new(
                "SELECT count(*) AS n FROM detections WHERE expires_at IS NOT NULL AND expires_at <= ?1 \
AND state IN ('live','pending','failed')",
                vec![Val::int(now)],
            ),
        ])
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    let n = |i: usize| done.get(i).and_then(|d| d.first()).and_then(|r| r.u64("n")).unwrap_or(0);
    let detections = done
        .first()
        .map(|d| {
            d.rows
                .iter()
                .filter_map(|r| Some((Kind::parse(r.text("kind")?)?, DetState::parse(r.text("state")?)?, r.u64("n")?)))
                .collect()
        })
        .unwrap_or_default();
    let switches = done
        .get(3)
        .map(|d| {
            d.rows.iter().filter_map(|r| Some((r.text("name")?.to_owned(), r.text("state")?.to_owned()))).collect()
        })
        .unwrap_or_default();
    Ok(StoreStatus { detections, live_claims: n(1), dirty_shards: n(2), switches, expiry_backlog: n(4) })
}
