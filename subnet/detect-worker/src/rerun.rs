//! A pasted text's re-run (`POST /api/detect/v1/rerun {slug}` with a secret
//! slug): "Check it again with the current model" scores the page's stored
//! text under the serving package and adds the run to the same page, which
//! then shows the newest run and lists the earlier ones.
//!
//! 1. The row is read and gated before anything is read or decrypted
//!    ([`crate::store::rerun_gate`]): only a live `private_text` page goes
//!    on; every other case answers its own refusal and touches no R2 object.
//! 2. The document is read and decrypted. Its newest run decides: a run
//!    already under the serving package answers `rerun_not_needed`.
//! 3. The text is scored under the serving package (as a paste,
//!    `free_text`).
//! 4. The run is added behind the row's fence ([`crate::save::append_run`]):
//!    one writer per package, the page's cache tag purged, and the document
//!    deleted again if the page ended or entered a legal state while the
//!    scan ran, so a removal or a delete that lands during the scan still
//!    wins.
//! 5. The answer is the page's slug and whether this call added the run;
//!    nothing changes the text, the owner or the manage token.

use slop_ninja_detect_page::api::RerunAnswer;
use slop_ninja_detect_page::refusal::RefusalCode;

use crate::db::Io;
use crate::doc::{SecretDoc, Serving, StoredResult};
use crate::lookup::{Found, SlugForm, secret_lookup, slug_form};
use crate::save::{Keys, append_run};
use crate::store::{self, secret_doc_key};

/// Scores a text under the serving package.
#[allow(async_fn_in_trait)]
pub trait Scorer {
    async fn score(&self, text: &str) -> Result<StoredResult, RefusalCode>;
}

async fn gate<'p>(io: &impl Io, keys: Keys<'p>, slug: &str) -> Result<Found<'p>, RefusalCode> {
    let (found, _) = secret_lookup(io, keys.peppers, slug).await.map_err(|_| RefusalCode::Unavailable)?;
    // A slug nobody holds reads as a deleted one: the page cannot be told
    // apart from a link that never existed.
    let found = found.ok_or(RefusalCode::RerunGone)?;
    // The state and kind decide first; the document's newest run decides
    // whether a run is needed.
    store::rerun_gate(found.row.kind, found.row.state, true)?;
    Ok(found)
}

/// The re-run of a pasted text's page.
pub async fn rerun_secret(
    io: &impl Io,
    keys: Keys<'_>,
    slug: &str,
    serving: &Serving,
    scorer: &impl Scorer,
) -> Result<RerunAnswer, RefusalCode> {
    if !slug_form(slug).is_some_and(SlugForm::is_secret) {
        return Err(RefusalCode::NotFound);
    }
    let found = gate(io, keys, slug).await?;
    let bytes = io.doc_get(&secret_doc_key(&found.row.id_h)).await.map_err(|_| RefusalCode::Unavailable)?;
    let bytes = bytes.ok_or(RefusalCode::RerunUnavailable)?;
    let plain = found.pepper.doc_key(slug).open(&bytes).map_err(|_| RefusalCode::RerunUnavailable)?;
    let doc = SecretDoc::decode(&plain).map_err(|_| RefusalCode::RerunUnavailable)?;
    let newest = doc.latest().ok_or(RefusalCode::RerunUnavailable)?;
    store::rerun_gate(found.row.kind, found.row.state, newest.package_load != serving.load)?;
    let result = scorer.score(&doc.text).await?;
    let fence = found.row.package_load.as_deref();
    match append_run(io, slug, found.pepper, &found.row.id_h, fence, &result).await {
        Ok(true) => {
            io.metric("store", &["rerun_added"]);
            Ok(RerunAnswer { slug: slug.to_owned(), updated: true })
        }
        // Another save or re-run added the serving package's run first:
        // the page shows it.
        Ok(false) => match gate(io, keys, slug).await {
            Err(code) => Err(code),
            Ok(_) => Ok(RerunAnswer { slug: slug.to_owned(), updated: false }),
        },
        Err(code) => Err(code),
    }
}
