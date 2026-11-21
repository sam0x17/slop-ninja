//! Manage a secret page by its token, act for its owner, and report saved
//! pages' states.
//!
//! - **Manage** (the secret host's `/api/detect/v1/manage`): `delete` tombstones
//!   the row (`deleted`; document key, manage hash and owner cleared;
//!   `state_v + 1`), deletes `p/<id_h>` and asks for a purge of the page and
//!   card URLs (the slug is in the request). Deleting a family root made
//!   before `slop_ninja_detect_store_v2` also ends its re-run copies, in the
//!   same batch. `claim_check` checks the token. No page ends by time, so
//!   there is no expiry to change.
//! - **Deleting a disabled page holds its text.** Every delete batch (by
//!   token, by owner, an owner purge, the operator's `delete_private`)
//!   first sets `texts.hold` on the text whose current page it is, when the
//!   page is `disabled`, under the delete's own guard: the same text then
//!   gets no new page while the notice is reviewed (a reject or restore of
//!   the case clears the hold, as `release_text` does). A deleted page's
//!   text row is otherwise left as it is, so the next check of the text
//!   gets a new generation and a new slug, and the old link stays 410.
//! - **Owner** (`/internal/v1/owner`, with the principal): the Worker
//!   derives `owner_h` from the principal's user id; the body never carries
//!   one. Every statement that changes a page for an account runs `… WHERE
//!   id_h = ? AND owner_h = ?`, so a bookmark can never delete or release
//!   another account's page; a miss answers `not_owner`, changes nothing and
//!   writes an `owner {skipped}` point. Owners exist only for secret pages,
//!   so every action refuses a slug of any other form (a public page's
//!   included) with `not_found`, before any read and writing nothing.
//! - **States** (`/internal/v1/states`): at most 90 ids per call.
//!
//! Deleting a pending paid page cancels its running job in the same batch;
//! the job's hold goes back through a `commit_hold` message.
//!
//! The D1 tombstone comes first and the R2 delete after it: a delete that
//! fails between the two leaves an orphan document, which the hourly
//! backstop removes ([`crate::cron`]).

use slop_ninja_detect_page::api::{ManageAnswer, ManageRequest};
use slop_ninja_detect_page::codes::{DetState, HostRole, Kind, ManageAction, OwnerAction, OwnerRelation};
use slop_ninja_detect_page::ids::IdH;
use slop_ninja_detect_page::internal::{
    ClaimAnswer, MAX_STATES_PER_CALL, OwnerDone, OwnerRequest, PeekAnswer, StateEntry, StatesAnswer, StatesRequest,
    Summary,
};
use slop_ninja_detect_page::origins::Origins;
use slop_ninja_detect_page::queue::{Job, QueueMessage};
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_page::slug::{self, SlugRoute};
use slop_ninja_detect_page::sn::Principal;

use crate::db::{Done, Io, Stmt, Val, delete_docs};
use crate::doc::SecretDoc;
use crate::keys::Bytes32;
use crate::lookup::{Found, SlugForm, secret_lookup, slug_form};
use crate::save::{Keys, summary};

/// The hold a delete by manage token sets first: on the text whose current
/// page is `?1`, when the page is disabled and the token is its own.
pub const HOLD_BY_TOKEN: &str = "UPDATE texts SET hold = 'notice', updated_at = ?3 WHERE det_id_h = ?1 \
AND EXISTS (SELECT 1 FROM detections WHERE id_h = ?1 AND state = 'disabled' AND manage_h = ?2)";

/// The hold a delete by the owner sets first.
pub const HOLD_BY_OWNER: &str = "UPDATE texts SET hold = 'notice', updated_at = ?3 WHERE det_id_h = ?1 \
AND EXISTS (SELECT 1 FROM detections WHERE id_h = ?1 AND state = 'disabled' AND owner_h = ?2)";

/// The hold the operator's `delete_private` sets first.
pub const HOLD_ANY: &str = "UPDATE texts SET hold = 'notice', updated_at = ?2 WHERE det_id_h = ?1 \
AND EXISTS (SELECT 1 FROM detections WHERE id_h = ?1 AND state = 'disabled')";
use crate::store::{self, DetRow, secret_doc_key};

/// The tombstone of a delete by manage token.
pub const DELETE_BY_TOKEN: &str = "UPDATE detections SET state = 'deleted', doc_key = NULL, doc_bytes = NULL, \
manage_h = NULL, owner_h = NULL, owner_via = NULL, state_v = state_v + 1, updated_at = ?3 \
WHERE id_h = ?1 AND manage_h = ?2 AND state IN ('live','pending','failed','disabled')";

/// The tombstone of a delete by the owner.
pub const DELETE_BY_OWNER: &str = "UPDATE detections SET state = 'deleted', doc_key = NULL, doc_bytes = NULL, \
manage_h = NULL, owner_h = NULL, owner_via = NULL, state_v = state_v + 1, updated_at = ?3 \
WHERE id_h = ?1 AND owner_h = ?2 AND state IN ('live','pending','failed','disabled') \
AND (kind <> 'public_url' OR state <> 'disabled')";

/// A deleted root's copies end with it; guarded on the root being deleted
/// now, so a wrong token or owner ends nothing.
pub const DELETE_COPIES: &str = "UPDATE detections SET state = 'deleted', doc_key = NULL, doc_bytes = NULL, \
manage_h = NULL, owner_h = NULL, owner_via = NULL, state_v = state_v + 1, updated_at = ?2 \
WHERE copy_of = ?1 AND state IN ('live','pending') \
AND (SELECT r.state FROM detections r WHERE r.id_h = ?1) = 'deleted' RETURNING hex(id_h) AS id_h";

/// `claim`: the first account to save a page with its manage token owns it.
pub const CLAIM: &str = "UPDATE detections SET owner_h = ?3, owner_via = 'claimed', state_v = state_v + 1, \
updated_at = ?4 WHERE id_h = ?1 AND manage_h = ?2 AND state IN ('live','pending') AND owner_h IS NULL";

/// `release`: the page stays, owned by nobody.
pub const RELEASE: &str = "UPDATE detections SET owner_h = NULL, owner_via = NULL WHERE id_h = ?1 AND owner_h = ?2";

/// The purge of a secret page's URLs (the slug is in the request).
pub fn purge_urls(origins: &Origins, host: HostRole, slug: &str) -> QueueMessage {
    let o = origins.of(host);
    QueueMessage::new(Job::Purge {
        urls: vec![o.url(&slug::path(host, SlugRoute::Page, slug)), o.url(&slug::path(host, SlugRoute::Card, slug))],
    })
}

/// The batch that holds the text of a disabled page (`hold`, under the
/// delete's own guard), tombstones `id_h` under `first`, ends its copies and
/// cancels a paid page's running job (the owner preempts any driver; the
/// hold goes back through `commit_hold`, `paid::after_cancel`).
pub fn delete_batch(hold: Stmt, first: Stmt, id_h: &Bytes32, now: u64) -> Vec<Stmt> {
    vec![
        hold,
        first,
        // SQLite's changes() here is the row count of the guarded tombstone
        // immediately above in the same D1 batch. A retry or an ownership
        // change must never add an instruction to replay a deletion.
        Stmt::new(
            "INSERT INTO op_journal (at, actor, op) \
             SELECT ?2, 'auto:private_delete', json_object('op', 'delete_private', \
             'args', json_object('id_h', lower(hex(id_h)))) FROM detections \
             WHERE changes() = 1 AND id_h = ?1 AND kind IN ('private_text','private_paid') AND state = 'deleted'",
            vec![Val::blob(id_h), Val::int(now)],
        ),
        Stmt::new(DELETE_COPIES, vec![Val::blob(id_h), Val::int(now)]),
        Stmt::new(crate::paid::CANCEL_DELETED, vec![Val::blob(id_h), Val::int(now)]),
    ]
}

/// The statements per page of [`delete_batch`]; the tombstone is the
/// second, the recovery journal third, the copies fourth and the cancel fifth.
pub const DELETE_BATCH_LEN: usize = 5;

/// After a delete batch: whether the page was deleted, and the pages it
/// ended (the page and its copies).
fn deleted_docs(done: &[Done], id_h: &Bytes32) -> (bool, Vec<Bytes32>) {
    let hit = done.get(1).is_some_and(|d| d.changes == 1);
    let mut ids = Vec::new();
    if hit {
        ids.push(*id_h);
        if let Some(copies) = done.get(3) {
            ids.extend(copies.rows.iter().filter_map(|r| r.hex32("id_h")));
        }
    }
    (hit, ids)
}

/// Finishes a delete: the documents, then the purges of the ended pages'
/// cached copies (all best effort; the backstop and the cache bound cover
/// a failure).
async fn after_delete(io: &impl Io, origins: &Origins, slug: &str, ids: Vec<Bytes32>) {
    let keys: Vec<String> = ids.iter().map(secret_doc_key).collect();
    let _ = delete_docs(io, &keys).await;
    crate::purge::purge_pages(io, &ids).await;
    let _ = io.enqueue(vec![purge_urls(origins, HostRole::Secret, slug)]).await;
}

fn need_secret(slug: &str) -> Result<(), RefusalCode> {
    if slug_form(slug).is_some_and(SlugForm::is_secret) { Ok(()) } else { Err(RefusalCode::NotFound) }
}

/// Reads a secret page or answers `not_found`.
async fn find<'p>(io: &impl Io, keys: Keys<'p>, slug: &str) -> Result<Found<'p>, RefusalCode> {
    let (found, _) = secret_lookup(io, keys.peppers, slug).await.map_err(|_| RefusalCode::SaveUnavailable)?;
    found.ok_or(RefusalCode::NotFound)
}

/// The manage route.
pub async fn manage(
    io: &impl Io,
    keys: Keys<'_>,
    origins: &Origins,
    req: &ManageRequest,
) -> Result<ManageAnswer, RefusalCode> {
    need_secret(&req.slug)?;
    let Found { row, .. } = find(io, keys, &req.slug).await?;
    let now = io.now_ms();
    let state = row.state;
    if !row.manage_ok(Some(&req.manage)) {
        // A tombstone has no manage hash: an ended page answers its state.
        return Err(store::ended_refusal(state).unwrap_or(RefusalCode::ManageInvalid));
    }
    let token_h = crate::keys::manage_h(&req.manage);
    match req.action {
        ManageAction::Delete => {
            if !store::DELETABLE.contains(&state) {
                return Err(store::ended_refusal(state).unwrap_or(RefusalCode::ManageInvalid));
            }
            let guard = vec![Val::blob(&row.id_h), Val::blob(&token_h), Val::int(now)];
            let hold = Stmt::new(HOLD_BY_TOKEN, guard.clone());
            let first = Stmt::new(DELETE_BY_TOKEN, guard);
            let done =
                io.batch(delete_batch(hold, first, &row.id_h, now)).await.map_err(|_| RefusalCode::SaveUnavailable)?;
            let (hit, docs) = deleted_docs(&done, &row.id_h);
            if !hit {
                return Err(RefusalCode::ManageInvalid);
            }
            after_delete(io, origins, &req.slug, docs).await;
            crate::paid::after_cancel(io, done.get(4)).await;
            io.metric("manage", &["delete"]);
            Ok(ManageAnswer { ok: true, state: DetState::Deleted })
        }
        ManageAction::ClaimCheck => {
            if !store::MANAGEABLE.contains(&state) {
                return Err(store::ended_refusal(state).unwrap_or(RefusalCode::ManageInvalid));
            }
            Ok(ManageAnswer { ok: true, state })
        }
    }
}

/// The answer of the owner route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnerAnswer {
    Peek(PeekAnswer),
    Claim(ClaimAnswer),
    Done(OwnerDone),
}

/// A saved page's summary: from its document when live, from the row
/// otherwise (a pending paid page has no result yet).
async fn page_summary(io: &impl Io, slug: &str, f: &Found<'_>) -> Result<Summary, RefusalCode> {
    let row = &f.row;
    let empty = Summary {
        kind: row.kind,
        band: None,
        strong_sections: None,
        total_sections: None,
        words: 0,
        created_at: row.created_at,
    };
    if row.state != DetState::Live {
        return Ok(empty);
    }
    let bytes = io.doc_get(&secret_doc_key(&row.id_h)).await.map_err(|_| RefusalCode::SaveUnavailable)?;
    let Some(bytes) = bytes else { return Err(RefusalCode::ReportUnavailable) };
    let plain = f.pepper.doc_key(slug).open(&bytes).map_err(|_| RefusalCode::ReportUnavailable)?;
    let doc = SecretDoc::decode(&plain).map_err(|_| RefusalCode::ReportUnavailable)?;
    let latest = doc.latest().ok_or(RefusalCode::ReportUnavailable)?;
    let mut s = summary(row.kind, latest, row.created_at);
    if let Some(sec) = &doc.sections {
        s.strong_sections = Some(sec.document.strong_sections);
        s.total_sections = Some(sec.document.total_sections);
    }
    Ok(s)
}

/// The owner route.
pub async fn owner(
    io: &impl Io,
    keys: Keys<'_>,
    origins: &Origins,
    principal: &Principal,
    req: &OwnerRequest,
) -> Result<OwnerAnswer, RefusalCode> {
    // Saved lists hold secret pages only: a public slug (an X page's
    // included) is refused by its form, before any read.
    need_secret(&req.slug)?;
    let o = keys.store.owner_h(&principal.user_id.0);
    let found = find(io, keys, &req.slug).await?;
    let now = io.now_ms();
    let row: &DetRow = &found.row;
    let state = row.state;
    let skipped = |io: &_| {
        Io::metric(io, "owner", &["skipped"]);
        OwnerAnswer::Done(OwnerDone { ok: false, code: Some(RefusalCode::NotOwner) })
    };
    match req.action {
        OwnerAction::Peek => {
            if let Some(r) = store::ended_refusal(state) {
                return Err(r);
            }
            let summary = page_summary(io, &req.slug, &found).await?;
            Ok(OwnerAnswer::Peek(PeekAnswer {
                id_h: IdH(row.id_h),
                summary,
                manage_ok: row.manage_ok(req.manage.as_deref()),
                owner: row.owner_relation(&o),
            }))
        }
        OwnerAction::Claim => {
            if let Some(r) = store::ended_refusal(state) {
                return Err(r);
            }
            let rel = row.owner_relation(&o);
            let token_ok = row.manage_ok(req.manage.as_deref());
            if rel != OwnerRelation::You && !token_ok {
                return Err(RefusalCode::ManageInvalid);
            }
            let mut newly = false;
            if rel == OwnerRelation::None && store::MANAGEABLE.contains(&state) {
                let token_h = crate::keys::manage_h(req.manage.as_deref().unwrap_or_default());
                let s = Stmt::new(CLAIM, vec![Val::blob(&row.id_h), Val::blob(&token_h), Val::blob(&o), Val::int(now)]);
                newly = io.run(s).await.map_err(|_| RefusalCode::SaveUnavailable)?.changes == 1;
            }
            let is_owner = newly || {
                // Re-read: another claim may have won between the read and
                // the update.
                let again = find(io, keys, &req.slug).await?;
                again.row.owner_relation(&o) == OwnerRelation::You
            };
            let summary = page_summary(io, &req.slug, &found).await?;
            Ok(OwnerAnswer::Claim(ClaimAnswer { id_h: IdH(row.id_h), summary, is_owner, newly_owned: newly }))
        }
        OwnerAction::Delete => {
            if row.owner_relation(&o) != OwnerRelation::You || !store::DELETABLE.contains(&state) {
                return Ok(skipped(io));
            }
            let guard = vec![Val::blob(&row.id_h), Val::blob(&o), Val::int(now)];
            let hold = Stmt::new(HOLD_BY_OWNER, guard.clone());
            let first = Stmt::new(DELETE_BY_OWNER, guard);
            let done =
                io.batch(delete_batch(hold, first, &row.id_h, now)).await.map_err(|_| RefusalCode::SaveUnavailable)?;
            let (hit, docs) = deleted_docs(&done, &row.id_h);
            if !hit {
                return Ok(skipped(io));
            }
            after_delete(io, origins, &req.slug, docs).await;
            crate::paid::after_cancel(io, done.get(4)).await;
            io.metric("owner", &["deleted"]);
            Ok(OwnerAnswer::Done(OwnerDone { ok: true, code: None }))
        }
        OwnerAction::Release => {
            let done = io
                .run(Stmt::new(RELEASE, vec![Val::blob(&row.id_h), Val::blob(&o)]))
                .await
                .map_err(|_| RefusalCode::SaveUnavailable)?;
            if done.changes != 1 {
                return Ok(skipped(io));
            }
            io.metric("owner", &["released"]);
            Ok(OwnerAnswer::Done(OwnerDone { ok: true, code: None }))
        }
    }
}

/// The `states` statement for `n` ids.
pub fn states_sql(n: usize) -> String {
    let marks: Vec<String> = (1..=n).map(|i| format!("?{i}")).collect();
    format!("SELECT hex(id_h) AS id_h, state, kind FROM detections WHERE id_h IN ({})", marks.join(", "))
}

/// The states of saved pages, in request order; a missing id reads `null`.
pub async fn states(io: &impl Io, req: &StatesRequest) -> Result<StatesAnswer, RefusalCode> {
    if req.id_h.len() > MAX_STATES_PER_CALL {
        return Err(RefusalCode::TooLarge);
    }
    if req.id_h.is_empty() {
        return Ok(StatesAnswer { states: Vec::new() });
    }
    let params = req.id_h.iter().map(|i| Val::blob(&i.0)).collect();
    let done =
        io.run(Stmt::owned(states_sql(req.id_h.len()), params)).await.map_err(|_| RefusalCode::SaveUnavailable)?;
    let states = req
        .id_h
        .iter()
        .map(|id| {
            let r = done.rows.iter().find(|r| r.hex32("id_h") == Some(id.0));
            let state = r.and_then(|r| {
                Kind::parse(r.text("kind")?)?;
                let st = DetState::parse(r.text("state")?)?;
                Some(st)
            });
            StateEntry { id_h: *id, state }
        })
        .collect();
    Ok(StatesAnswer { states })
}
