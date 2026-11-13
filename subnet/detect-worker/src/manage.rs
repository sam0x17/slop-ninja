//! Manage a secret page by its token, act for its owner, and report saved
//! pages' states.
//!
//! - **Manage** (the secret host's `/api/detect/v1/manage`): `delete` tombstones
//!   the row (`deleted`; document key, manage hash and owner cleared;
//!   `state_v + 1`), deletes `p/<id_h>` and asks for a purge of the page and
//!   card URLs (the slug is in the request). Deleting a family root also
//!   ends its re-run copies, in the same batch. `set_expiry` recomputes
//!   `expires_at`, never past a re-run copy's latest allowed expiry (set once,
//!   when the copy is made).
//!   `claim_check` checks the token.
//! - **Owner** (`/internal/v1/owner`, with the principal): the Worker
//!   derives `owner_h` from the principal's user id; the body never carries
//!   one. Every statement that changes a page for an account runs `… WHERE
//!   id_h = ? AND owner_h = ?`, so a bookmark can never delete or release
//!   another account's page; a miss answers `not_owner`, changes nothing and
//!   writes an `owner {skipped}` point. Owners exist only for secret pages,
//!   so every action refuses a slug of any other form (a public page's
//!   included) with `not_found`, before any read and writing nothing.
//! - **States** (`/internal/v1/states`): at most 90 ids per call, expiry
//!   enforced on read.
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
use crate::share::{Keys, summary};
use crate::store::{self, DetRow, secret_doc_key};

/// The tombstone of a delete by manage token.
pub const DELETE_BY_TOKEN: &str = "UPDATE detections SET state = 'deleted', doc_key = NULL, doc_bytes = NULL, \
manage_h = NULL, owner_h = NULL, owner_via = NULL, state_v = state_v + 1, updated_at = ?3 \
WHERE id_h = ?1 AND manage_h = ?2 AND state IN ('live','pending','failed','disabled')";

/// The tombstone of a delete by the owner.
pub const DELETE_BY_OWNER: &str = "UPDATE detections SET state = 'deleted', doc_key = NULL, doc_bytes = NULL, \
manage_h = NULL, owner_h = NULL, owner_via = NULL, state_v = state_v + 1, updated_at = ?3 \
WHERE id_h = ?1 AND owner_h = ?2 AND state IN ('live','pending','failed','disabled')";

/// A deleted root's copies end with it; guarded on the root being deleted
/// now, so a wrong token or owner ends nothing.
pub const DELETE_COPIES: &str = "UPDATE detections SET state = 'deleted', doc_key = NULL, doc_bytes = NULL, \
manage_h = NULL, owner_h = NULL, owner_via = NULL, state_v = state_v + 1, updated_at = ?2 \
WHERE copy_of = ?1 AND state IN ('live','pending') \
AND (SELECT r.state FROM detections r WHERE r.id_h = ?1) = 'deleted' RETURNING hex(id_h) AS id_h";

/// `set_expiry`: the new expiry (NULL: until deleted), already limited to a
/// copy's latest allowed expiry, which never changes after the copy is made.
pub const SET_EXPIRY: &str = "UPDATE detections SET expires_at = ?3, state_v = state_v + 1, updated_at = ?4 \
WHERE id_h = ?1 AND manage_h = ?2 AND state IN ('live','pending')";

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

/// The batch that tombstones `id_h` under `first` and ends its copies.
fn delete_batch(first: Stmt, id_h: &Bytes32, now: u64) -> Vec<Stmt> {
    vec![first, Stmt::new(DELETE_COPIES, vec![Val::blob(id_h), Val::int(now)])]
}

/// After a delete batch: whether the page was deleted, and the documents
/// to remove (the page's and its copies').
fn deleted_docs(done: &[Done], id_h: &Bytes32) -> (bool, Vec<String>) {
    let hit = done.first().is_some_and(|d| d.changes == 1);
    let mut keys = Vec::new();
    if hit {
        keys.push(secret_doc_key(id_h));
        if let Some(copies) = done.get(1) {
            keys.extend(copies.rows.iter().filter_map(|r| r.hex32("id_h")).map(|c| secret_doc_key(&c)));
        }
    }
    (hit, keys)
}

/// Finishes a delete: the documents, then the purge (both best effort; the
/// backstop and the cache bound cover a failure).
async fn after_delete(io: &impl Io, origins: &Origins, slug: &str, keys: Vec<String>) {
    let _ = delete_docs(io, &keys).await;
    let _ = io.enqueue(vec![purge_urls(origins, HostRole::Secret, slug)]).await;
}

fn need_secret(slug: &str) -> Result<(), RefusalCode> {
    if slug_form(slug) == Some(SlugForm::Secret) { Ok(()) } else { Err(RefusalCode::NotFound) }
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
    let state = row.state_at(now);
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
            let first = Stmt::new(DELETE_BY_TOKEN, vec![Val::blob(&row.id_h), Val::blob(&token_h), Val::int(now)]);
            let done = io.batch(delete_batch(first, &row.id_h, now)).await.map_err(|_| RefusalCode::SaveUnavailable)?;
            let (hit, docs) = deleted_docs(&done, &row.id_h);
            if !hit {
                return Err(RefusalCode::ManageInvalid);
            }
            after_delete(io, origins, &req.slug, docs).await;
            io.metric("manage", &["delete"]);
            Ok(ManageAnswer {
                ok: true,
                state: DetState::Deleted,
                expires_at: None,
                latest_expiry: None,
                choices: Vec::new(),
            })
        }
        ManageAction::SetExpiry => {
            let choice = req.expiry.ok_or(RefusalCode::MalformedRequest)?;
            if !store::MANAGEABLE.contains(&state) {
                return Err(store::ended_refusal(state).unwrap_or(RefusalCode::ManageInvalid));
            }
            let latest = row.expiry_cap;
            let expires_at = store::new_expiry(choice, now, latest);
            let s = Stmt::new(
                SET_EXPIRY,
                vec![Val::blob(&row.id_h), Val::blob(&token_h), Val::opt_int(expires_at), Val::int(now)],
            );
            let done = io.run(s).await.map_err(|_| RefusalCode::SaveUnavailable)?;
            if done.changes != 1 {
                return Err(RefusalCode::ManageInvalid);
            }
            io.metric("manage", &["set_expiry"]);
            Ok(ManageAnswer {
                ok: true,
                state,
                expires_at,
                latest_expiry: latest,
                choices: store::expiry_choices(now, latest),
            })
        }
        ManageAction::ClaimCheck => {
            if !store::MANAGEABLE.contains(&state) {
                return Err(store::ended_refusal(state).unwrap_or(RefusalCode::ManageInvalid));
            }
            Ok(ManageAnswer {
                ok: true,
                state,
                expires_at: row.expires_at,
                latest_expiry: row.expiry_cap,
                choices: store::expiry_choices(now, row.expiry_cap),
            })
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
        short: None,
        strong_sections: None,
        total_sections: None,
        words: 0,
        created_at: row.created_at,
        expires_at: row.expires_at,
    };
    if row.state != DetState::Live {
        return Ok(empty);
    }
    let bytes = io.doc_get(&secret_doc_key(&row.id_h)).await.map_err(|_| RefusalCode::SaveUnavailable)?;
    let Some(bytes) = bytes else { return Err(RefusalCode::ReportUnavailable) };
    let plain = f.pepper.doc_key(slug).open(&bytes).map_err(|_| RefusalCode::ReportUnavailable)?;
    let doc = SecretDoc::decode(&plain).map_err(|_| RefusalCode::ReportUnavailable)?;
    let mut s = summary(row.kind, &doc.result_full, row.created_at, row.expires_at);
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
    let state = row.state_at(now);
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
            let first = Stmt::new(DELETE_BY_OWNER, vec![Val::blob(&row.id_h), Val::blob(&o), Val::int(now)]);
            let done = io.batch(delete_batch(first, &row.id_h, now)).await.map_err(|_| RefusalCode::SaveUnavailable)?;
            let (hit, docs) = deleted_docs(&done, &row.id_h);
            if !hit {
                return Ok(skipped(io));
            }
            after_delete(io, origins, &req.slug, docs).await;
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
    format!("SELECT hex(id_h) AS id_h, state, expires_at, kind FROM detections WHERE id_h IN ({})", marks.join(", "))
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
    let now = io.now_ms();
    let states = req
        .id_h
        .iter()
        .map(|id| {
            let r = done.rows.iter().find(|r| r.hex32("id_h") == Some(id.0));
            let read = r.and_then(|r| {
                let kind = Kind::parse(r.text("kind")?)?;
                let st = DetState::parse(r.text("state")?)?;
                kind.is_secret().then(|| (store::read_state(st, r.u64("expires_at"), now), r.u64("expires_at")))
            });
            StateEntry { id_h: *id, state: read.map(|x| x.0), expires_at: read.and_then(|x| x.1) }
        })
        .collect();
    Ok(StatesAnswer { states })
}
