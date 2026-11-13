//! The store's read routes (internal, binding-only): `page`, `preview`,
//! `card`, `examples`, `example`, the sitemap shards and the removal record.
//!
//! - **page** (`/internal/v1/page {slug}`): a slug that is not canonical is
//!   not found with no store read. One batch reads the switch rows with the
//!   detection (a secret slug under every pepper version; a public slug with
//!   its source, its removal, the page it supersedes and its claim's lease).
//!   A live page's document is then read from R2 (and a secret one
//!   decrypted with its own key). `DETECT_PAGES` off answers `pages_off` for
//!   every stored page.
//! - **preview** (`/internal/v1/preview {text, seal}`): the unstored result;
//!   it writes nothing and does not consume the seal.
//! - **card** (`/internal/v1/card {slug}`): the card JSON of a shown page.
//! - **state** (`/api/detect/v1/state {slug}`): a pending public page's
//!   poll: its state, whether its claim's lease ended, and its failure.

use slop_ninja_detect_page::api::StateAnswer;
use slop_ninja_detect_page::card::Card;
use slop_ninja_detect_page::codes::{DetState, ExampleSource, Kind, RemovalScope};
use slop_ninja_detect_page::internal::{ExampleEntry, ExamplesAnswer, MonthlyCount, RemovalEntry, RemovalsAnswer};
use slop_ninja_detect_page::page::{JobProgress, Page, Removal};
use slop_ninja_detect_page::refusal::RefusalCode;

use crate::db::{Done, Io, Stmt, Val};
use crate::display::{SOURCE_COLUMNS, SourceRow};
use crate::doc::{RunDoc, SecretDoc, Serving};
use crate::keys::public_id_h;
use crate::lookup::{SELECT_SWITCHES, SlugForm, SwitchRows, secret_lookup, slug_form};
use crate::page::{PageCtx, PublicExtras, public_page, secret_page, unstored_page};
use crate::share::{Keys, Sealed};
use crate::store::{DetRow, SELECT_DET, run_doc_key, secret_doc_key};

/// A public page's source.
pub fn select_source() -> String {
    format!("SELECT {SOURCE_COLUMNS} FROM detections d JOIN sources s ON s.source_id = d.source_id WHERE d.id_h = ?1")
}

/// The page a public page supersedes.
pub const SELECT_SUPERSEDES: &str =
    "SELECT public_slug FROM detections WHERE superseded_by = ?1 ORDER BY created_at DESC LIMIT 1";

/// A public page's latest removal.
pub const SELECT_REMOVAL: &str =
    "SELECT at, scope, basis, jurisdiction FROM removals WHERE public_slug = ?1 ORDER BY seq DESC LIMIT 1";

/// A pending page's claim lease.
pub const SELECT_LEASE: &str = "SELECT max(lease_until) AS lease_until FROM claims WHERE det_id_h = ?1";

/// A pending paid page's progress.
pub const SELECT_JOB: &str = "SELECT sections_done, sections_total FROM jobs WHERE det_id_h = ?1";

fn removal_of(d: Option<&Done>) -> Option<Removal> {
    let r = d?.first()?;
    Some(Removal {
        scope: RemovalScope::parse(r.text("scope")?)?,
        basis: r.text("basis")?.to_owned(),
        jurisdiction: r.text("jurisdiction")?.to_owned(),
        at: r.u64("at")?,
    })
}

async fn receipt_for(io: &impl Io, state: DetState, result: Option<&crate::doc::StoredResult>) -> Option<String> {
    match (state, result) {
        (DetState::Live, Some(r)) => io.receipt(r).await,
        _ => None,
    }
}

/// The page JSON of a slug.
pub async fn page(io: &impl Io, keys: Keys<'_>, serving: &Serving, slug: &str) -> Result<Page, RefusalCode> {
    match slug_form(slug).ok_or(RefusalCode::NotFound)? {
        SlugForm::Secret => secret(io, keys, serving, slug).await,
        SlugForm::Public => public(io, serving, slug).await,
    }
}

async fn secret(io: &impl Io, keys: Keys<'_>, serving: &Serving, slug: &str) -> Result<Page, RefusalCode> {
    let (found, sw) = secret_lookup(io, keys.peppers, slug).await.map_err(|_| RefusalCode::Unavailable)?;
    let sw = sw.page(io);
    if !sw.pages {
        return Err(RefusalCode::PagesOff);
    }
    let found = found.ok_or(RefusalCode::NotFound)?;
    let now = io.now_ms();
    let state = found.row.state_at(now);
    let mut doc = None;
    let mut job = None;
    if state == DetState::Live {
        let bytes = io.doc_get(&secret_doc_key(&found.row.id_h)).await.map_err(|_| RefusalCode::Unavailable)?;
        if let Some(b) = bytes {
            let plain = found.pepper.doc_key(slug).open(&b).map_err(|_| RefusalCode::ReportUnavailable)?;
            doc = Some(SecretDoc::decode(&plain).map_err(|_| RefusalCode::ReportUnavailable)?);
        }
    } else if state == DetState::Pending && found.row.kind == Kind::PrivatePaid {
        let d = io
            .run(Stmt::new(SELECT_JOB, vec![Val::blob(&found.row.id_h)]))
            .await
            .map_err(|_| RefusalCode::Unavailable)?;
        job = d.first().and_then(|r| {
            Some(JobProgress {
                sections_done: u32::try_from(r.u64("sections_done")?).ok()?,
                sections_total: u32::try_from(r.u64("sections_total")?).ok()?,
            })
        });
    }
    let receipt = receipt_for(io, state, doc.as_ref().map(|d| &d.result_full)).await;
    let ctx = PageCtx { serving, switches: sw, now, receipt };
    secret_page(slug, &found.row, doc.as_ref(), job, &ctx)
}

async fn public(io: &impl Io, serving: &Serving, slug: &str) -> Result<Page, RefusalCode> {
    let id = public_id_h(slug);
    let batch = vec![
        Stmt::new(SELECT_SWITCHES, Vec::new()),
        Stmt::new(SELECT_DET, vec![Val::blob(&id)]),
        Stmt::owned(select_source(), vec![Val::blob(&id)]),
        Stmt::new(SELECT_SUPERSEDES, vec![Val::blob(&id)]),
        Stmt::new(SELECT_REMOVAL, vec![Val::text(slug)]),
        Stmt::new(SELECT_LEASE, vec![Val::blob(&id)]),
    ];
    let done = io.batch(batch).await.map_err(|_| RefusalCode::Unavailable)?;
    let sw = SwitchRows::from_done(done.first()).page(io);
    if !sw.pages {
        return Err(RefusalCode::PagesOff);
    }
    let row = match done.get(1).and_then(Done::first) {
        Some(r) => DetRow::from_row(r).map_err(|_| RefusalCode::Internal)?,
        None => return Err(RefusalCode::NotFound),
    };
    if row.kind != Kind::PublicUrl || row.public_slug.as_deref() != Some(slug) {
        return Err(RefusalCode::NotFound);
    }
    let now = io.now_ms();
    let source = match done.get(2).and_then(Done::first) {
        Some(r) => Some(SourceRow::from_row(r).map_err(|_| RefusalCode::Internal)?),
        None => None,
    };
    let ex = PublicExtras {
        source,
        supersedes: done.get(3).and_then(Done::first).and_then(|r| r.text("public_slug")).map(str::to_owned),
        removal: removal_of(done.get(4)),
        lease_ended: done.get(5).and_then(Done::first).and_then(|r| r.u64("lease_until")).is_none_or(|t| t <= now),
    };
    let state = row.state_at(now);
    let mut doc = None;
    let shown = matches!(state, DetState::Live | DetState::ExcerptWithheld);
    let purged = sw.x_purged && row.platform == slop_ninja_detect_page::codes::Platform::X;
    if shown
        && !purged
        && let Some(run_id) = row.run_id
        && let Some(b) = io.doc_get(&run_doc_key(run_id)).await.map_err(|_| RefusalCode::Unavailable)?
    {
        doc = Some(RunDoc::decode(&b).map_err(|_| RefusalCode::ReportUnavailable)?);
    }
    let receipt = receipt_for(io, state, doc.as_ref().map(|d| &d.result_full)).await;
    let ctx = PageCtx { serving, switches: sw, now, receipt };
    public_page(slug, &row, &ex, doc.as_ref(), &ctx)
}

/// The unstored result (`kind = unstored`) of an opened seal.
pub async fn preview(
    io: &impl Io,
    serving: &Serving,
    text: &str,
    sealed: Option<&Sealed>,
) -> Result<Page, RefusalCode> {
    let sealed = sealed.ok_or(RefusalCode::SealInvalid)?;
    if !crate::keys::ct_eq(&crate::canon::text_sha256(text), &sealed.text_sha256) {
        return Err(RefusalCode::SealInvalid);
    }
    let sw = crate::lookup::read_switches(io).await;
    let paid_open = sw.on(io, "DETECT_PAID");
    let receipt = io.receipt(&sealed.result).await;
    let ctx = PageCtx { serving, switches: sw.page(io), now: io.now_ms(), receipt };
    unstored_page(text, &sealed.result, paid_open, &ctx)
}

/// A public page's poll: any other slug (a secret one's included) is not
/// found, with no store read. Writes nothing.
pub async fn state(io: &impl Io, slug: &str) -> Result<StateAnswer, RefusalCode> {
    if slug_form(slug) != Some(SlugForm::Public) {
        return Err(RefusalCode::NotFound);
    }
    let id = public_id_h(slug);
    let batch = vec![Stmt::new(SELECT_DET, vec![Val::blob(&id)]), Stmt::new(SELECT_LEASE, vec![Val::blob(&id)])];
    let done = io.batch(batch).await.map_err(|_| RefusalCode::Unavailable)?;
    let row = match done.first().and_then(Done::first) {
        Some(r) => DetRow::from_row(r).map_err(|_| RefusalCode::Internal)?,
        None => return Err(RefusalCode::NotFound),
    };
    if row.kind != Kind::PublicUrl || row.public_slug.as_deref() != Some(slug) {
        return Err(RefusalCode::NotFound);
    }
    let now = io.now_ms();
    let state = row.state_at(now);
    Ok(StateAnswer {
        state,
        lease_ended: done.get(1).and_then(Done::first).and_then(|r| r.u64("lease_until")).is_none_or(|t| t <= now),
        code: if state == DetState::Failed { row.refusal } else { None },
    })
}

/// The card JSON of a slug: shown pages only; an ended page answers its
/// own refusal.
pub async fn card(io: &impl Io, keys: Keys<'_>, serving: &Serving, slug: &str) -> Result<Card, RefusalCode> {
    let p = page(io, keys, serving, slug).await?;
    if let Some(r) = crate::store::ended_refusal(p.state) {
        return Err(r);
    }
    let generic = p.card.as_ref().is_some_and(|c| c.variant == slop_ninja_detect_page::codes::CardVariant::Generic);
    let card = Card::from_page(&p, generic).map_err(|_| RefusalCode::NotFound)?;
    card.check().map_err(|_| RefusalCode::Internal)?;
    Ok(card)
}

/// The examples table.
pub const SELECT_EXAMPLES: &str = "SELECT slot, source, public_slug, doc_key FROM examples ORDER BY slot";

/// The "What a report looks like" panel: empty until the operator sets
/// examples. A slot whose report cannot be read is left out.
pub async fn examples(io: &impl Io, keys: Keys<'_>, serving: &Serving) -> Result<ExamplesAnswer, RefusalCode> {
    let d = io.run(Stmt::new(SELECT_EXAMPLES, Vec::new())).await.map_err(|_| RefusalCode::Unavailable)?;
    let mut examples = Vec::new();
    for r in &d.rows {
        let (Some(slot), Some(source)) = (r.u64("slot"), r.text("source").and_then(ExampleSource::parse)) else {
            continue;
        };
        let Ok(slot) = u8::try_from(slot) else { continue };
        if let Ok(page) = example_of(io, keys, serving, slot, source, r.text("public_slug"), r.text("doc_key")).await {
            examples.push(ExampleEntry { slot, source, page });
        }
    }
    Ok(ExamplesAnswer { examples })
}

async fn example_of(
    io: &impl Io,
    keys: Keys<'_>,
    serving: &Serving,
    slot: u8,
    source: ExampleSource,
    public_slug: Option<&str>,
    doc_key: Option<&str>,
) -> Result<Page, RefusalCode> {
    match source {
        ExampleSource::Public => {
            let p = page(io, keys, serving, public_slug.ok_or(RefusalCode::NotFound)?).await?;
            if p.kind == Kind::PublicUrl && p.state == DetState::Live && !p.is_x_post() {
                Ok(p)
            } else {
                Err(RefusalCode::NotFound)
            }
        }
        ExampleSource::Synthetic => {
            let key = doc_key.ok_or(RefusalCode::NotFound)?;
            let b = io.doc_get(key).await.map_err(|_| RefusalCode::Unavailable)?.ok_or(RefusalCode::NotFound)?;
            let p: Page = serde_json::from_slice(&b).map_err(|_| RefusalCode::Internal)?;
            crate::page::example_page(p, slot)
        }
    }
}

/// One example's page JSON.
pub async fn example(io: &impl Io, keys: Keys<'_>, serving: &Serving, slot: u8) -> Result<Page, RefusalCode> {
    let all = examples(io, keys, serving).await?;
    all.examples.into_iter().find(|e| e.slot == slot).map(|e| e.page).ok_or(RefusalCode::NotFound)
}

/// Public removals, newest first.
pub const SELECT_REMOVALS: &str = "SELECT at, public_slug, scope, basis, jurisdiction FROM removals \
WHERE public_slug IS NOT NULL ORDER BY seq DESC LIMIT 1000";

/// Secret-page removals by month.
pub const SELECT_SECRET_REMOVALS: &str = "SELECT strftime('%Y-%m', at / 1000, 'unixepoch') AS month, count(*) AS n \
FROM removals WHERE public_slug IS NULL GROUP BY month ORDER BY month DESC";

/// The public removal record: public pages by slug, secret pages only as
/// monthly counts.
pub async fn removals(io: &impl Io) -> Result<RemovalsAnswer, RefusalCode> {
    let done = io
        .batch(vec![Stmt::new(SELECT_REMOVALS, Vec::new()), Stmt::new(SELECT_SECRET_REMOVALS, Vec::new())])
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    let entries = done
        .first()
        .map(|d| {
            d.rows
                .iter()
                .filter_map(|r| {
                    Some(RemovalEntry {
                        at: r.u64("at")?,
                        public_slug: r.text("public_slug")?.to_owned(),
                        scope: RemovalScope::parse(r.text("scope")?)?,
                        basis: r.text("basis")?.to_owned(),
                        jurisdiction: r.text("jurisdiction")?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let secret_monthly = done
        .get(1)
        .map(|d| {
            d.rows
                .iter()
                .filter_map(|r| {
                    Some(MonthlyCount { month: r.text("month")?.to_owned(), count: u32::try_from(r.u64("n")?).ok()? })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(RemovalsAnswer { entries, secret_monthly })
}

/// A sitemap shard's XML (`sitemap/detect-<n>.xml`), or an empty set when
/// the shard was never built.
pub async fn sitemap_shard(io: &impl Io, shard: u32) -> Result<Vec<u8>, RefusalCode> {
    let b = io.doc_get(&crate::sitemap::shard_key(shard)).await.map_err(|_| RefusalCode::Unavailable)?;
    Ok(b.unwrap_or_else(|| crate::sitemap::xml(&[]).into_bytes()))
}
