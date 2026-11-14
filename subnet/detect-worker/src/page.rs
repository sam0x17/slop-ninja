//! Page JSON (`slop_ninja_detect_page_v2`) from stored rows and documents
//! (pure).
//!
//! Each builder takes what the store read and returns the page the website
//! or the secret host renders, with the display policy applied
//! ([`crate::display`]) and the ninja state computed here, so a page and its
//! card agree. Every page it returns passes [`Page::check`]. A live page
//! whose document is missing answers `report_unavailable` (503, never
//! cached) instead of a page.

use slop_ninja_detect_page::codes::{CardVariant, DetState, HostRole, Kind, Platform, RemovalScope, Robots, SpansNote};
use slop_ninja_detect_page::page::{CardRef, Ended, Failure, JobProgress, Page, PageSchema, Pending, Removal, XPost};
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_page::slug::{self, SlugRoute};
use slop_ninja_detect_page::x::{PostUrl, parse_x_canonical};
use slop_ninja_detect_page::{NinjaInput, ninja_state};

use crate::display::{self, PageSwitches, SourceRow};
use crate::doc::{RunDoc, SecretDoc, Serving, StoredResult};
use crate::store::DetRow;

/// The card layout version (`?v=<CARD_V>.<state_v>`).
pub const CARD_V: u32 = 1;

/// What a page read needs besides the rows.
#[derive(Clone, Debug)]
pub struct PageCtx<'a> {
    pub serving: &'a Serving,
    pub switches: PageSwitches,
    pub now: u64,
    /// The feedback receipt for a live page (issued by the feedback object).
    pub receipt: Option<String>,
}

/// The refusal a page read answers instead of page JSON.
pub type PageResult = Result<Page, RefusalCode>;

fn empty(slug: Option<String>, host: HostRole, kind: Kind, platform: Platform, serving: &Serving) -> Page {
    Page {
        schema: PageSchema,
        slug,
        host,
        kind,
        platform,
        state: DetState::Live,
        state_v: 1,
        created_at: None,
        expires_at: None,
        robots: if host == HostRole::Secret { Robots::NoindexAll } else { Robots::Noindex },
        canonical: None,
        superseded_by: None,
        supersedes: None,
        example_slot: None,
        serving: serving.line(None),
        run: None,
        sections: None,
        text: None,
        x: None,
        source: None,
        pending: None,
        failure: None,
        ended: None,
        removal: None,
        card: None,
        ninja: None,
        receipt: None,
        paid_open: None,
    }
}

fn card_ref(page: &Page, generic: bool) -> Option<CardRef> {
    let run = page.run.as_ref()?;
    let slug = page.slug.as_deref()?;
    // As the card renderer's own choice (`Card::from_page`).
    let variant = if page.is_paid_document() {
        CardVariant::PaidDocument
    } else if run.short.is_some() {
        CardVariant::Short
    } else if generic || page.state == DetState::ExcerptWithheld {
        CardVariant::Generic
    } else {
        CardVariant::Band
    };
    let image = format!("{}?v={CARD_V}.{}", slug::path(page.host, SlugRoute::Card, slug), page.state_v);
    Some(CardRef { variant, image })
}

fn ninja_of(page: &Page, generic: bool) -> Option<slop_ninja_detect_page::codes::NinjaState> {
    if page.state == DetState::ExcerptWithheld {
        return None;
    }
    let input = page.ninja_input()?;
    if matches!(input, NinjaInput::PaidDocument) {
        return None;
    }
    ninja_state(input, generic)
}

/// Fills a shown page's card, ninja and receipt.
fn finish_shown(page: &mut Page, ctx: &PageCtx<'_>) {
    let generic = ctx.switches.cards_generic;
    page.card = card_ref(page, generic);
    page.ninja = ninja_of(page, generic);
    page.receipt = if page.state == DetState::Live { ctx.receipt.clone() } else { None };
}

/// A secret page (`private_text` or `private_paid`) from its row and, when
/// live, its decrypted document. `job` is a pending paid page's progress.
pub fn secret_page(
    slug: &str,
    row: &DetRow,
    doc: Option<&SecretDoc>,
    job: Option<JobProgress>,
    ctx: &PageCtx<'_>,
) -> PageResult {
    let mut p = empty(Some(slug.to_owned()), HostRole::Secret, row.kind, row.platform, ctx.serving);
    let state = row.state_at(ctx.now);
    p.state = state;
    p.state_v = row.state_v;
    p.created_at = Some(row.created_at);
    p.expires_at = row.expires_at;
    p.serving = ctx.serving.line(row.package_load.as_deref());
    match state {
        DetState::Live => {
            let doc = doc.ok_or(RefusalCode::ReportUnavailable)?;
            p.serving = ctx.serving.line(Some(&doc.result_full.package_load));
            p.run = Some(doc.result_full.to_run(ctx.serving));
            p.text = Some(display::secret_text(&doc.text));
            p.source = doc.source_check.as_ref().map(display::submitter_source);
            if row.kind == Kind::PrivatePaid {
                p.sections = Some(doc.sections.clone().ok_or(RefusalCode::ReportUnavailable)?);
            }
            finish_shown(&mut p, ctx);
        }
        DetState::Pending => {
            p.pending =
                Some(Pending { lease_ended: false, job: (row.kind == Kind::PrivatePaid).then_some(job).flatten() });
            if row.kind == Kind::PrivatePaid && p.pending.as_ref().is_some_and(|x| x.job.is_none()) {
                return Err(RefusalCode::ReportUnavailable);
            }
        }
        DetState::Failed => p.failure = Some(Failure { code: row.refusal.unwrap_or(RefusalCode::Internal) }),
        _ => {}
    }
    check(p)
}

fn check(p: Page) -> PageResult {
    p.check().map(|()| p).map_err(|_| RefusalCode::Internal)
}

/// What a public page read joined to its detection.
#[derive(Clone, Debug, Default)]
pub struct PublicExtras {
    /// The source row (absent on a removal tombstone whose source went).
    pub source: Option<SourceRow>,
    /// The page this one supersedes, by public slug.
    pub supersedes: Option<String>,
    /// The removal behind a removed or withheld page.
    pub removal: Option<Removal>,
    /// Whether the claim on a pending page has run past its lease.
    pub lease_ended: bool,
    /// Whether the page's host has an opt-out row (index gate 3).
    pub host_opted_out: bool,
}

/// A public page (`public_url`: web, Bluesky or X) from its rows and, when
/// shown, its run document.
pub fn public_page(slug: &str, row: &DetRow, ex: &PublicExtras, doc: Option<&RunDoc>, ctx: &PageCtx<'_>) -> PageResult {
    let mut p = empty(Some(slug.to_owned()), HostRole::Site, Kind::PublicUrl, row.platform, ctx.serving);
    let is_x = row.platform == Platform::X;
    let mut state = row.state_at(ctx.now);
    let mut ended = row.refusal.filter(|c| Ended::state_of(*c) == Some(state)).map(|code| Ended { code });
    if is_x && ctx.switches.x_purged && !matches!(state, DetState::Removed) {
        state = DetState::Deleted;
        ended = Some(Ended { code: RefusalCode::XPurged });
    }
    p.state = state;
    p.state_v = row.state_v;
    p.created_at = Some(row.created_at);
    p.expires_at = row.expires_at;
    p.serving = ctx.serving.line(row.package_load.as_deref());
    let mut full_analysis = false;
    if is_x {
        p.ended = if state.is_ended() { ended } else { None };
        if matches!(state, DetState::Deleted | DetState::Removed) && p.ended.is_none() {
            // An X page that ended without a recorded reason (an operator
            // purge of a single page): the purge reason.
            p.ended = Some(Ended {
                code: if state == DetState::Removed { RefusalCode::XRemoved } else { RefusalCode::XPurged },
            });
        }
    }
    match state {
        DetState::Removed => {
            p.removal =
                Some(ex.removal.clone().filter(|r| r.scope == RemovalScope::Page).ok_or(RefusalCode::Internal)?);
        }
        DetState::Pending | DetState::Failed => {
            // The domain only: a web or Bluesky page's source row (its
            // domain-only row from the claim, or a failure's, or its live
            // source's), an X page's host. A web or Bluesky page with no
            // source row has no domain to name.
            let (host, via) = match ex.source.as_ref() {
                Some(s) => (s.host.as_str(), s.via()),
                None if is_x => (slop_ninja_detect_page::x::X_HOST, slop_ninja_detect_page::codes::SourceVia::X),
                None => return Err(RefusalCode::ReportUnavailable),
            };
            p.source = Some(display::domain_only(host, via, None));
            if state == DetState::Pending {
                p.pending = Some(Pending { lease_ended: ex.lease_ended, job: None });
                if is_x {
                    p.canonical = Some(slug.to_owned());
                }
            } else {
                p.failure = Some(Failure { code: row.refusal.unwrap_or(RefusalCode::FetchFailed) });
            }
        }
        DetState::Live | DetState::ExcerptWithheld => {
            let doc = doc.ok_or(RefusalCode::ReportUnavailable)?;
            let src = ex.source.as_ref().ok_or(RefusalCode::ReportUnavailable)?;
            let mut run = doc.result_full.to_run(ctx.serving);
            p.serving = ctx.serving.line(Some(&doc.result_full.package_load));
            p.canonical = Some(row.superseded_slug.clone().unwrap_or_else(|| slug.to_owned()));
            p.superseded_by = row.superseded_slug.clone();
            p.supersedes = ex.supersedes.clone();
            p.source = Some(display::public_source(state, src, doc.page_hash.as_deref(), ctx.now));
            if is_x {
                run.spans.clear();
                run.spans_note = Some(SpansNote::XPost);
                let id = src.canon_url.as_deref().and_then(parse_x_canonical).ok_or(RefusalCode::ReportUnavailable)?;
                p.x = (state == DetState::Live).then(|| PostUrl::new(id).map(|post_url| XPost { post_url })).flatten();
                if state == DetState::Live && p.x.is_none() {
                    return Err(RefusalCode::ReportUnavailable);
                }
                p.canonical = Some(slug.to_owned());
                p.superseded_by = None;
            } else {
                let (text, keep_spans) = display::public_text(state, src, doc, ctx.switches);
                if !keep_spans {
                    run.spans.clear();
                }
                full_analysis = keep_spans;
                p.text = Some(text);
            }
            if state == DetState::ExcerptWithheld {
                p.removal =
                    Some(ex.removal.clone().filter(|r| r.scope == RemovalScope::Excerpt).ok_or(RefusalCode::Internal)?);
            }
            p.run = Some(run);
            finish_shown(&mut p, ctx);
        }
        _ => {}
    }
    // The five structural gates (`index-gates/v1`), with the overrides.
    p.robots = crate::index_gates::robots(&crate::index_gates::GateInput {
        state,
        platform: row.platform,
        words: p.run.as_ref().map(|r| r.words),
        short: p.run.as_ref().is_some_and(|r| r.short.is_some()),
        source: ex.source.as_ref().map(|s| (s.state, s.reputation)),
        host_opted_out: ex.host_opted_out,
        full_analysis,
        index_force: row.index_force,
        index_on: ctx.switches.index,
    });
    check(p)
}

/// The unstored result of a sealed free check (`kind = unstored`): the
/// whole text plain, with no slug, card or created time. `paid_open` is
/// `DETECT_PAID = on`; it rides only on a cut result.
pub fn unstored_page(text: &str, result: &StoredResult, paid_open: bool, ctx: &PageCtx<'_>) -> PageResult {
    let mut p = empty(None, HostRole::Site, Kind::Unstored, Platform::Paste, ctx.serving);
    p.serving = ctx.serving.line(Some(&result.package_load));
    p.run = Some(result.to_run(ctx.serving));
    p.text = Some(display::secret_text(text));
    p.paid_open = (result.cut != slop_ninja_detect_page::codes::CutKind::None).then_some(paid_open);
    p.ninja = ninja_of(&p, ctx.switches.cards_generic);
    p.receipt = ctx.receipt.clone();
    check(p)
}

/// Checks an example report's stored page JSON (`x/examples/<v>/<slot>`)
/// before it is served.
pub fn example_page(page: Page, slot: u8) -> PageResult {
    if page.kind != Kind::Example || page.example_slot != Some(slot) {
        return Err(RefusalCode::Internal);
    }
    check(page)
}
