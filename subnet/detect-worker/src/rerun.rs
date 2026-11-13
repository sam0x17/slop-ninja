//! The secret re-run (`POST /api/detect/v1/rerun {slug}` with a secret
//! slug): a live secret text checked again under the serving package makes
//! a separate secret page, a copy of its family.
//!
//! 1. The row is read and gated before anything is read or decrypted
//!    ([`crate::store::rerun_gate`]): only a live `private_text` page whose
//!    run is older than the serving package goes on; every other case answers
//!    its own refusal and touches no R2 object.
//! 2. The document is read and decrypted, and the text scored under the
//!    serving package (as a paste, `free_text`).
//! 3. A new slug and manage token are drawn and the copy's document put.
//! 4. One statement inserts the copy and re-reads the original: its expiry
//!    and cap are the original's, its `copy_of` the family root. Zero rows
//!    mean the original was deleted, disabled, removed or expired while the
//!    scan ran: the copy's document is deleted and the original's current
//!    refusal answered, so a removal or a delete that lands during the scan
//!    still wins.
//! 5. The answer is the copy's slug and manage token; the copy has no owner
//!    and the original never links to it.

use slop_ninja_detect_page::api::RerunAnswer;
use slop_ninja_detect_page::refusal::RefusalCode;

use crate::db::{Io, Stmt, Val};
use crate::doc::{DOC_V, SecretDoc, Serving, StoredResult};
use crate::keys::manage_h;
use crate::lookup::{Found, SlugForm, secret_lookup, slug_form};
use crate::share::{Keys, draw_secret, seal_doc};
use crate::store::{self, secret_doc_key};

/// Scores a text under the serving package.
#[allow(async_fn_in_trait)]
pub trait Scorer {
    async fn score(&self, text: &str) -> Result<StoredResult, RefusalCode>;
}

/// The copy insert, re-reading the original in the same statement.
pub const INSERT_COPY: &str = "INSERT INTO detections (id_h, pepper_v, kind, platform, state, state_v, \
created_at, updated_at, expires_at, package_load, manage_h, doc_key, doc_bytes, copy_of, expiry_cap) \
SELECT ?1, ?2, 'private_text', 'paste', 'live', 1, ?3, ?3, o.expires_at, ?4, ?5, ?6, ?7, \
coalesce(o.copy_of, o.id_h), o.expires_at FROM detections o \
WHERE o.id_h = ?8 AND o.kind = 'private_text' AND o.state = 'live' AND (o.expires_at IS NULL OR o.expires_at > ?3)";

async fn gate<'p>(io: &impl Io, keys: Keys<'p>, slug: &str, serving: &Serving) -> Result<Found<'p>, RefusalCode> {
    let (found, _) = secret_lookup(io, keys.peppers, slug).await.map_err(|_| RefusalCode::Unavailable)?;
    // A slug nobody holds reads as a deleted one: the page cannot be told
    // apart from a link that never existed.
    let found = found.ok_or(RefusalCode::RerunGone)?;
    let differs = found.row.package_load.as_deref().is_some_and(|l| l != serving.load);
    store::rerun_gate(found.row.kind, found.row.state_at(io.now_ms()), differs)?;
    Ok(found)
}

/// The secret re-run.
pub async fn rerun_secret(
    io: &impl Io,
    keys: Keys<'_>,
    slug: &str,
    serving: &Serving,
    scorer: &impl Scorer,
) -> Result<RerunAnswer, RefusalCode> {
    if slug_form(slug) != Some(SlugForm::Secret) {
        return Err(RefusalCode::NotFound);
    }
    let found = gate(io, keys, slug, serving).await?;
    let bytes = io.doc_get(&secret_doc_key(&found.row.id_h)).await.map_err(|_| RefusalCode::Unavailable)?;
    let bytes = bytes.ok_or(RefusalCode::RerunUnavailable)?;
    let plain = found.pepper.doc_key(slug).open(&bytes).map_err(|_| RefusalCode::RerunUnavailable)?;
    let original = SecretDoc::decode(&plain).map_err(|_| RefusalCode::RerunUnavailable)?;
    let result = scorer.score(&original.text).await?;
    let (copy_slug, manage) = draw_secret(io);
    let pepper = keys.peppers.current();
    let copy_id = pepper.id_h(&copy_slug);
    let doc = SecretDoc {
        v: DOC_V,
        text: original.text,
        result_full: result,
        source_check: original.source_check,
        sections: None,
    };
    let sealed = seal_doc(io, keys, &copy_slug, &doc);
    let doc_bytes = sealed.len() as u64;
    let key = secret_doc_key(&copy_id);
    io.doc_put(&key, sealed).await.map_err(|_| RefusalCode::SaveUnavailable)?;
    let now = io.now_ms();
    let insert = Stmt::new(
        INSERT_COPY,
        vec![
            Val::blob(&copy_id),
            Val::int(u64::from(pepper.v)),
            Val::int(now),
            Val::text(doc.result_full.package_load.clone()),
            Val::blob(&manage_h(&manage)),
            Val::text(key.clone()),
            Val::int(doc_bytes),
            Val::blob(&found.row.id_h),
        ],
    );
    let inserted = io.run(insert).await.map(|d| d.changes == 1);
    if inserted != Ok(true) {
        let _ = io.doc_delete(std::slice::from_ref(&key)).await;
        if inserted.is_err() {
            return Err(RefusalCode::SaveUnavailable);
        }
        // The original changed while the scan ran: answer what it is now.
        return match gate(io, keys, slug, serving).await {
            Err(code) => Err(code),
            Ok(_) => Err(RefusalCode::RerunUnavailable),
        };
    }
    io.metric("store", &["rerun_copy"]);
    Ok(RerunAnswer { slug: copy_slug, manage: Some(manage) })
}
