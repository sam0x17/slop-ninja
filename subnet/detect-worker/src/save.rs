//! Every check's page (`POST /internal/v1/save`, the website's `POST
//! /detect/result`): a sealed free check becomes its text's secret page, or
//! lands on the page the same text already has (`permalink-policy/v1`).
//!
//! **The address.** A pasted text's page is found by `texts.text_h`, a keyed
//! hash of `canon_v1(text)` under a slug pepper ([`crate::keys`]), which
//! holds the page's generation, its slug's length, its pepper version and
//! its store id, never the slug. The slug is the first `slug_len` characters
//! the text's generation derives ([`crate::keys::Pepper::text_slug_chars`]),
//! so the same text always leads to the same page, and a later save rebuilds
//! the slug from the row without probing.
//!
//! **The save**, in order:
//! 1. one batch reads the switch rows, whether the seal already made a page,
//!    and the text's row under every live pepper version, joined to its
//!    page; `DETECT_STORE` off answers `store_off`, a forged, expired or
//!    mismatched seal `seal_invalid`;
//! 2. a text whose page is `disabled` or `removed`, or whose row is held (a
//!    disabled page was deleted), answers `text_on_hold`;
//! 3. a text whose page is `live` or `pending` lands on it
//!    ([`existing`]): a seal under another package than the page's newest
//!    run adds its run (the page shows the newest and lists the others,
//!    [`append_run`]); with "Save to my detections" the account owns it if
//!    its owner hash is the page's, and keeps a bookmark otherwise. No
//!    manage token is drawn and nothing changes for anyone else;
//! 4. otherwise (no row, or the page was deleted) a new page at generation
//!    0 or `gen + 1`: a seal that already made a page answers `seal_used`.
//!    The slug is probed from the policy length ([`policy_len`]) up to
//!    [`slug::TEXT_MAX_LEN`], one character at a time, past any slug a row
//!    holds under any live pepper version (a deleted page's slug is never
//!    used again) and past the reserved segments. The slug is claimed in D1
//!    before the document is written ([`claim_batch`]): a `pending` row and
//!    the text's address in one batch, so only the save whose address write
//!    stands puts a document under that id. Then the document goes to R2 and
//!    the row goes `live`. A failed put tombstones the claim, and the next
//!    save of the text takes the next generation;
//! 5. with "Save to my detections", the account's `owned/add` records the
//!    page; if that fails the ownership just set is cleared and the answer
//!    says `account_unavailable`. The page exists either way.
//!
//! No model runs, so nothing is counted. The text is in request memory only;
//! D1 gets its keyed address and nothing else derived from it.

use slop_ninja_account_wire::OwnedAddRequest;
use slop_ninja_detect_page::api::{SaveReason, SavedAs};
use slop_ninja_detect_page::codes::{DetState, Kind};
use slop_ninja_detect_page::ids::{IdH, UserId};
use slop_ninja_detect_page::internal::{SaveAnswer, SaveRequest, Summary};
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_page::slug;
use slop_ninja_detect_page::sn::Principal;

use crate::db::{Done, Io, Row, Stmt, Val};
use crate::doc::{SecretDoc, StoredResult};
use crate::keys::{Bytes32, NONCE_BYTES, Pepper, Peppers, StoreKeys, ct_eq, manage_h, seal_nonce_h};
use crate::lookup::{SELECT_SWITCHES, SwitchRows};
use crate::store::secret_doc_key;

/// What an opened result seal holds (opened by the seal module).
#[derive(Clone, Debug, PartialEq)]
pub struct Sealed {
    /// `SHA-256(canon_v1(text))` of the scored text.
    pub text_sha256: [u8; 32],
    /// The seal's nonce, whose hash makes the seal make one page.
    pub nonce: Vec<u8>,
    /// The result, its passage offsets in UTF-16 units of `canon_v1(text)`.
    pub result: StoredResult,
}

/// The keys a store flow needs.
#[derive(Clone, Copy, Debug)]
pub struct Keys<'a> {
    pub store: &'a StoreKeys,
    pub peppers: &'a Peppers,
}

/// The var of `permalink-policy/v1`'s slug length.
pub const SLUG_LEN_VAR: &str = "DETECT_TEXT_SLUG_LEN";

/// The page a seal made, if any, and its state.
pub const SEAL_USED: &str = "SELECT hex(id_h) AS id_h, state FROM detections WHERE seal_nonce_h = ?1";

/// A text's address under one pepper version, joined to its current page.
pub const SELECT_TEXT: &str = "SELECT t.pepper_v AS pepper_v, t.gen AS gen, t.slug_len AS slug_len, \
t.hold AS hold, hex(t.det_id_h) AS det_id_h, d.state AS state, d.kind AS kind, d.package_load AS package_load, \
hex(d.owner_h) AS owner_h, d.created_at AS created_at FROM texts t JOIN detections d ON d.id_h = t.det_id_h \
WHERE t.text_h = ?1";

/// The claim of a new page's slug: a `pending` row holding this save's seal.
/// `ON CONFLICT DO NOTHING`: a slug another row holds (any state, so a
/// deleted page's slug stays dead) or a seal another page holds inserts
/// nothing.
pub const INSERT_PENDING: &str = "INSERT INTO detections (id_h, pepper_v, kind, platform, state, state_v, \
created_at, updated_at, package_load, manage_h, owner_h, owner_via, seal_nonce_h, doc_key) \
VALUES (?1, ?2, 'private_text', 'paste', 'pending', 1, ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT DO NOTHING";

/// A text's first address (generation 0), only beside the row this save
/// claimed.
pub const INSERT_TEXT: &str = "INSERT INTO texts (text_h, pepper_v, gen, slug_len, det_id_h, hold, created_at, \
updated_at) SELECT ?1, ?2, 0, ?3, ?4, NULL, ?5, ?5 \
WHERE EXISTS (SELECT 1 FROM detections WHERE id_h = ?4 AND seal_nonce_h = ?6) ON CONFLICT (text_h) DO NOTHING";

/// A text's next generation: only from the generation this save read
/// (`?6`), while no hold is set, beside the row this save claimed, and while
/// the current page is deleted (or a legacy expired or failed page).
pub const NEXT_GEN: &str = "UPDATE texts SET gen = ?6 + 1, slug_len = ?2, det_id_h = ?3, updated_at = ?4 \
WHERE text_h = ?1 AND gen = ?6 AND hold IS NULL \
AND EXISTS (SELECT 1 FROM detections WHERE id_h = ?3 AND seal_nonce_h = ?5) \
AND (SELECT d.state FROM detections d WHERE d.id_h = texts.det_id_h) IN ('deleted','expired','failed')";

/// A claim whose address write lost goes again.
pub const DROP_CLAIM: &str = "DELETE FROM detections WHERE id_h = ?1 AND seal_nonce_h = ?2 AND state = 'pending' \
AND NOT EXISTS (SELECT 1 FROM texts WHERE det_id_h = ?1)";

/// Who holds a candidate slug's id: the seal hash of its row, if any.
pub const SELECT_HOLDER: &str = "SELECT hex(seal_nonce_h) AS nonce FROM detections WHERE id_h = ?1";

/// The claimed page goes live once its document is in R2.
pub const FLIP_LIVE: &str = "UPDATE detections SET state = 'live', state_v = state_v + 1, doc_bytes = ?3, \
updated_at = ?4 WHERE id_h = ?1 AND state = 'pending' AND seal_nonce_h = ?2";

/// A claim whose document could not be written ends; its seal is let go,
/// so the same check can be saved again (as the text's next generation).
pub const DROP_FAILED_CLAIM: &str = "UPDATE detections SET state = 'deleted', doc_key = NULL, doc_bytes = NULL, \
manage_h = NULL, owner_h = NULL, owner_via = NULL, seal_nonce_h = NULL, state_v = state_v + 1, updated_at = ?3 \
WHERE id_h = ?1 AND state = 'pending' AND seal_nonce_h = ?2";

/// The fence of a run added to a live text page: the row's package moves
/// from the value read, so one writer adds a package's run.
pub const APPEND_FENCE: &str = "UPDATE detections SET package_load = ?2, doc_bytes = ?3, state_v = state_v + 1, \
updated_at = ?4 WHERE id_h = ?1 AND state = 'live' AND kind = 'private_text' AND package_load IS ?5 RETURNING 1 AS ok";

/// A page's state, read after a run's document was written.
pub const SELECT_STATE: &str = "SELECT state FROM detections WHERE id_h = ?1";

/// Clears an ownership this request set, when the account save failed.
pub const CLEAR_OWNER: &str = "UPDATE detections SET owner_h = NULL, owner_via = NULL WHERE id_h = ?1 AND owner_h = ?2";

/// The account list's summary of a secret page.
pub fn summary(kind: Kind, r: &StoredResult, created_at: u64) -> Summary {
    Summary {
        kind,
        band: r.band,
        strong_sections: None,
        total_sections: None,
        words: r.total_words,
        created_at,
    }
}

/// Draws a paid page's 20-character slug and its manage token.
pub fn draw_secret(io: &impl Io) -> (String, String) {
    let mut fill = |b: &mut [u8]| io.fill(b);
    (slug::draw_secret(&mut fill), slug::draw_token(&mut fill))
}

/// Draws a manage token.
pub fn draw_token(io: &impl Io) -> String {
    let mut fill = |b: &mut [u8]| io.fill(b);
    slug::draw_token(&mut fill)
}

/// Encrypts a document under the page's key (its own pepper version), with
/// a fresh nonce.
pub fn seal_doc(io: &impl Io, pepper: &Pepper, slug: &str, doc: &SecretDoc) -> Vec<u8> {
    let mut nonce = [0u8; NONCE_BYTES];
    io.fill(&mut nonce);
    pepper.doc_key(slug).seal(&nonce, &doc.encode())
}

/// `permalink-policy/v1`'s length: `DETECT_TEXT_SLUG_LEN`, by default
/// [`slug::TEXT_DEFAULT_LEN`]; `None` for a value outside
/// [`slug::TEXT_MIN_LEN`] to [`slug::TEXT_MAX_LEN`], which makes every save
/// refuse (a bad var never shortens links).
pub fn policy_len(io: &impl Io) -> Option<usize> {
    match io.var(SLUG_LEN_VAR) {
        None => Some(slug::TEXT_DEFAULT_LEN),
        Some(v) => v.parse::<usize>().ok().filter(|n| (slug::TEXT_MIN_LEN..=slug::TEXT_MAX_LEN).contains(n)),
    }
}

/// A text's address row, read under its own pepper version.
#[derive(Clone, Debug)]
pub struct TextRow<'p> {
    pub pepper: &'p Pepper,
    pub text_h: Bytes32,
    pub generation: u32,
    pub slug_len: usize,
    pub held: bool,
    pub det_id_h: Bytes32,
    pub state: DetState,
    pub kind: Kind,
    pub package_load: Option<String>,
    pub owner_h: Option<Bytes32>,
    pub created_at: u64,
}

impl TextRow<'_> {
    /// The current page's slug, rebuilt from the row (no probing).
    pub fn slug(&self, text: &str) -> Option<String> {
        let chars = self.pepper.text_slug_chars(text, self.generation);
        chars.get(..self.slug_len).map(str::to_owned)
    }
}

fn text_row<'p>(r: &Row, pepper: &'p Pepper, text_h: Bytes32) -> Option<TextRow<'p>> {
    let opt32 = |name: &str| r.text(name).filter(|s| !s.is_empty()).and_then(|_| r.hex32(name));
    Some(TextRow {
        pepper,
        text_h,
        generation: u32::try_from(r.u64("gen")?).ok()?,
        slug_len: usize::try_from(r.u64("slug_len")?).ok()?,
        held: r.text("hold").is_some(),
        det_id_h: r.hex32("det_id_h")?,
        state: DetState::parse(r.text("state")?)?,
        kind: Kind::parse(r.text("kind")?)?,
        package_load: r.text("package_load").map(str::to_owned),
        owner_h: opt32("owner_h"),
        created_at: r.u64("created_at")?,
    })
}

/// The text's row among one batch's per-pepper reads (`done[i]` read
/// `texts` under `peppers[i]`): a row counts only under its own version.
fn find_text<'p>(peppers: &[&'p Pepper], text: &str, done: &[Done]) -> Result<Option<TextRow<'p>>, RefusalCode> {
    for (p, d) in peppers.iter().zip(done) {
        let Some(r) = d.first() else { continue };
        if r.u64("pepper_v") != Some(u64::from(p.v)) {
            continue;
        }
        return text_row(r, p, p.text_h(text)).map(Some).ok_or(RefusalCode::Internal);
    }
    Ok(None)
}

/// The account side of a save that asked for it.
struct Account {
    user_id: UserId,
    owner_h: Bytes32,
}

/// The save.
pub async fn save(
    io: &impl Io,
    keys: Keys<'_>,
    req: &SaveRequest,
    sealed: Option<Sealed>,
    principal: Option<Principal>,
) -> Result<SaveAnswer, RefusalCode> {
    let nonce_h = sealed.as_ref().map(|s| seal_nonce_h(&s.nonce));
    let text = crate::canon::canon_v1(&req.text);
    let peppers: Vec<&Pepper> = keys.peppers.all().collect();
    let mut batch = vec![Stmt::new(SELECT_SWITCHES, Vec::new())];
    batch.push(Stmt::new(SEAL_USED, vec![Val::opt_blob(nonce_h.as_ref().map(|n| n.as_slice()))]));
    for p in &peppers {
        batch.push(Stmt::new(SELECT_TEXT, vec![Val::blob(&p.text_h(&text))]));
    }
    let done = io.batch(batch).await.map_err(|_| RefusalCode::SaveUnavailable)?;
    if !SwitchRows::from_done(done.first()).on(io, "DETECT_STORE") {
        return Err(RefusalCode::StoreOff);
    }
    let (Some(sealed), Some(nonce_h)) = (sealed, nonce_h) else {
        return Err(RefusalCode::SealInvalid);
    };
    if !ct_eq(&crate::canon::canonical_sha256(&text), &sealed.text_sha256) {
        return Err(RefusalCode::SealInvalid);
    }
    let account = principal
        .filter(|_| req.save)
        .map(|p| Account { user_id: p.user_id, owner_h: keys.store.owner_h(&p.user_id.0) });
    let mut reason = (req.save && account.is_none()).then_some(SaveReason::SignedOut);
    let seal_used = done.get(1).is_some_and(|d| !d.rows.is_empty());
    let found = find_text(&peppers, &text, done.get(2..).unwrap_or_default())?;
    let mut answer = match found {
        Some(row) => match row.state {
            _ if row.held => return held(io),
            DetState::Disabled | DetState::Removed => return held(io),
            DetState::Live | DetState::Pending => existing(io, &text, &row, &sealed, account.as_ref()).await?,
            _ if seal_used => return Err(RefusalCode::SealUsed),
            _ => make(io, keys, &text, Some(row), &sealed, &nonce_h, account.as_ref()).await?,
        },
        None if seal_used => return Err(RefusalCode::SealUsed),
        None => make(io, keys, &text, None, &sealed, &nonce_h, account.as_ref()).await?,
    };
    if answer.reason.is_none() {
        answer.reason = reason.take();
    }
    Ok(answer)
}

fn held(io: &impl Io) -> Result<SaveAnswer, RefusalCode> {
    io.metric("store", &["on_hold"]);
    Err(RefusalCode::TextOnHold)
}

/// A save that lands on the text's page: a newer package's run is added,
/// and the account's save is a bookmark unless the account owns the page.
async fn existing(
    io: &impl Io,
    text: &str,
    row: &TextRow<'_>,
    sealed: &Sealed,
    account: Option<&Account>,
) -> Result<SaveAnswer, RefusalCode> {
    let slug = row.slug(text).ok_or(RefusalCode::Internal)?;
    let mut updated = false;
    if row.state == DetState::Live
        && row.kind == Kind::PrivateText
        && row.package_load.as_deref() != Some(sealed.result.package_load.as_str())
    {
        updated = append_run(io, &slug, row.pepper, &row.det_id_h, row.package_load.as_deref(), &sealed.result)
            .await
            .unwrap_or(false);
    }
    io.metric("store", &[if updated { "updated" } else { "existing" }]);
    let mut answer = SaveAnswer { slug: slug.clone(), manage: None, made: false, updated, saved: None, reason: None };
    if let Some(a) = account {
        let owns = row.owner_h.as_ref().is_some_and(|o| ct_eq(o, &a.owner_h));
        let add = OwnedAddRequest {
            user_id: a.user_id,
            slug,
            id_h: IdH(row.det_id_h),
            summary: summary(Kind::PrivateText, &sealed.result, row.created_at),
            is_owner: owns,
        };
        if io.owned_add(&add).await.is_ok() {
            answer.saved = Some(if owns { SavedAs::Owner } else { SavedAs::Bookmark });
        } else {
            answer.reason = Some(SaveReason::AccountUnavailable);
        }
    }
    Ok(answer)
}

/// Adds `run` to the live text page `id_h` (slug `slug`) when its newest
/// run is under another package: the fence moves the row's package from
/// the value read (`fence`), so one writer adds a package's run; the winner
/// writes the document with the run appended, then reads the row again and
/// deletes its document if the page ended or entered a legal state
/// meanwhile (a disabled or removed page's copy lives in its quarantine).
/// `Ok(false)`: nothing was added (the document's newest run already is
/// under that package, another writer won, or the page ended).
pub async fn append_run(
    io: &impl Io,
    slug: &str,
    pepper: &Pepper,
    id_h: &Bytes32,
    fence: Option<&str>,
    run: &StoredResult,
) -> Result<bool, RefusalCode> {
    let key = secret_doc_key(id_h);
    let bytes = io.doc_get(&key).await.map_err(|_| RefusalCode::SaveUnavailable)?;
    let Some(bytes) = bytes else { return Ok(false) };
    let plain = pepper.doc_key(slug).open(&bytes).map_err(|_| RefusalCode::ReportUnavailable)?;
    let mut doc = SecretDoc::decode(&plain).map_err(|_| RefusalCode::ReportUnavailable)?;
    if doc.latest().is_some_and(|r| r.package_load == run.package_load) {
        return Ok(false);
    }
    doc.v = crate::doc::DOC_V2;
    doc.runs.push(run.clone());
    let sealed = seal_doc(io, pepper, slug, &doc);
    let now = io.now_ms();
    let fenced = io
        .run(Stmt::new(
            APPEND_FENCE,
            vec![
                Val::blob(id_h),
                Val::text(run.package_load.clone()),
                Val::int(sealed.len() as u64),
                Val::int(now),
                Val::opt_text(fence),
            ],
        ))
        .await
        .map_err(|_| RefusalCode::SaveUnavailable)?;
    if fenced.rows.is_empty() {
        return Ok(false);
    }
    if io.doc_put(&key, sealed).await.is_err() {
        // The fence moved and the older document stays: the page shows its
        // older run with the re-run offer, and the next re-run writes again.
        return Err(RefusalCode::SaveUnavailable);
    }
    let now_state = io.run(Stmt::new(SELECT_STATE, vec![Val::blob(id_h)])).await.ok();
    let still_live = now_state
        .as_ref()
        .and_then(Done::first)
        .and_then(|r| r.text("state"))
        .is_some_and(|s| s == DetState::Live.as_str());
    if !still_live {
        let _ = io.doc_delete(std::slice::from_ref(&key)).await;
        return Ok(false);
    }
    crate::purge::purge_pages(io, std::slice::from_ref(id_h)).await;
    Ok(true)
}

/// What a claim's batch found.
enum Claimed {
    /// This save's row holds the text's address.
    Won,
    /// Another save of the same text made its page first.
    SameText(Box<RowOwned>),
    /// Another text holds the candidate: probe on.
    Taken,
    /// The seal made another page.
    SealUsed,
    /// The text came to be held meanwhile.
    Held,
}

/// [`TextRow`] without its pepper borrow, for [`Claimed`].
struct RowOwned {
    generation: u32,
    slug_len: usize,
    held: bool,
    det_id_h: Bytes32,
    state: DetState,
    kind: Kind,
    package_load: Option<String>,
    owner_h: Option<Bytes32>,
    created_at: u64,
}

/// The claim's batch for candidate `id_h` of generation `gen` (`prev`: the
/// generation this save read, for a text whose page was deleted).
#[allow(clippy::too_many_arguments)]
pub fn claim_batch(
    id_h: &Bytes32,
    pepper_v: u32,
    text_h: &Bytes32,
    slug_len: usize,
    prev: Option<u32>,
    nonce_h: &Bytes32,
    manage: &Bytes32,
    owner: Option<&Bytes32>,
    package_load: &str,
    now: u64,
) -> Vec<Stmt> {
    let len = Val::int(slug_len as u64);
    let address = match prev {
        None => Stmt::new(
            INSERT_TEXT,
            vec![
                Val::blob(text_h),
                Val::int(u64::from(pepper_v)),
                len,
                Val::blob(id_h),
                Val::int(now),
                Val::blob(nonce_h),
            ],
        ),
        Some(g) => Stmt::new(
            NEXT_GEN,
            vec![Val::blob(text_h), len, Val::blob(id_h), Val::int(now), Val::blob(nonce_h), Val::int(u64::from(g))],
        ),
    };
    vec![
        Stmt::new(
            INSERT_PENDING,
            vec![
                Val::blob(id_h),
                Val::int(u64::from(pepper_v)),
                Val::int(now),
                Val::text(package_load),
                Val::blob(manage),
                Val::opt_blob(owner.map(|o| o.as_slice())),
                if owner.is_some() { Val::text("created") } else { Val::Null },
                Val::blob(nonce_h),
                Val::text(secret_doc_key(id_h)),
            ],
        ),
        address,
        Stmt::new(DROP_CLAIM, vec![Val::blob(id_h), Val::blob(nonce_h)]),
        Stmt::new(SELECT_TEXT, vec![Val::blob(text_h)]),
        Stmt::new(SELECT_HOLDER, vec![Val::blob(id_h)]),
        Stmt::new(SEAL_USED, vec![Val::blob(nonce_h)]),
    ]
}

fn claimed(done: &[Done], id_h: &Bytes32, nonce_h: &Bytes32, pepper_v: u32) -> Result<Claimed, RefusalCode> {
    let text = done.get(3).and_then(Done::first);
    let holder = done.get(4).and_then(Done::first);
    // The candidate's row is this save's claim: another save of the same
    // text derives the same id, so the id alone does not say whose it is.
    let held_by_me = holder.is_some_and(|r| r.hex32("nonce") == Some(*nonce_h));
    let mine = |r: &Row| held_by_me && r.hex32("det_id_h").is_some_and(|d| d == *id_h);
    if let Some(r) = text {
        let row = RowOwned {
            generation: r.u64("gen").and_then(|g| u32::try_from(g).ok()).ok_or(RefusalCode::Internal)?,
            slug_len: r.u64("slug_len").and_then(|g| usize::try_from(g).ok()).ok_or(RefusalCode::Internal)?,
            held: r.text("hold").is_some(),
            det_id_h: r.hex32("det_id_h").ok_or(RefusalCode::Internal)?,
            state: r.text("state").and_then(DetState::parse).ok_or(RefusalCode::Internal)?,
            kind: r.text("kind").and_then(Kind::parse).ok_or(RefusalCode::Internal)?,
            package_load: r.text("package_load").map(str::to_owned),
            owner_h: r.text("owner_h").filter(|s| !s.is_empty()).and_then(|_| r.hex32("owner_h")),
            created_at: r.u64("created_at").ok_or(RefusalCode::Internal)?,
        };
        if mine(r) && r.u64("pepper_v") == Some(u64::from(pepper_v)) {
            return Ok(Claimed::Won);
        }
        if row.held || matches!(row.state, DetState::Disabled | DetState::Removed) {
            return Ok(Claimed::Held);
        }
        if matches!(row.state, DetState::Live | DetState::Pending) {
            return Ok(Claimed::SameText(Box::new(row)));
        }
    }
    if holder.is_some() && !held_by_me {
        return Ok(Claimed::Taken);
    }
    if done.get(5).and_then(Done::first).is_some_and(|r| r.hex32("id_h") != Some(*id_h)) {
        return Ok(Claimed::SealUsed);
    }
    // The address write lost for another reason (a generation moved on to a
    // page that ended meanwhile): this save can make nothing now.
    Err(RefusalCode::SaveUnavailable)
}

/// A new page for `text`: generation 0, or the next one after `row`'s
/// deleted page.
async fn make(
    io: &impl Io,
    keys: Keys<'_>,
    text: &str,
    row: Option<TextRow<'_>>,
    sealed: &Sealed,
    nonce_h: &Bytes32,
    account: Option<&Account>,
) -> Result<SaveAnswer, RefusalCode> {
    let Some(floor) = policy_len(io) else {
        io.metric("error", &["policy"]);
        return Err(RefusalCode::SaveUnavailable);
    };
    // A new text takes the current pepper version; a text's later
    // generation keeps its row's, under which its address was made.
    let pepper = row.as_ref().map_or(keys.peppers.current(), |r| r.pepper);
    let text_h = pepper.text_h(text);
    let (generation, prev) = match &row {
        None => (0, None),
        Some(r) => (r.generation.checked_add(1).ok_or(RefusalCode::Internal)?, Some(r.generation)),
    };
    let chars = pepper.text_slug_chars(text, generation);
    let all: Vec<&Pepper> = keys.peppers.all().collect();
    let manage = draw_token(io);
    let manage_hash = manage_h(&manage);
    let owner = account.map(|a| a.owner_h);
    let now = io.now_ms();
    let mut len = floor;
    while len <= slug::TEXT_MAX_LEN {
        let Some(cand) = chars.get(..len) else { break };
        if slug::is_reserved(cand) || held_anywhere(io, &all, cand).await? {
            len += 1;
            continue;
        }
        let id_h = pepper.text_id_h(cand);
        let batch = claim_batch(
            &id_h,
            pepper.v,
            &text_h,
            len,
            prev,
            nonce_h,
            &manage_hash,
            owner.as_ref(),
            &sealed.result.package_load,
            now,
        );
        let done = io.batch(batch).await.map_err(|_| RefusalCode::SaveUnavailable)?;
        match claimed(&done, &id_h, nonce_h, pepper.v)? {
            Claimed::Won => {
                return finish(io, cand, pepper, &id_h, nonce_h, text, sealed, manage, account).await;
            }
            Claimed::SameText(r) => {
                let row = TextRow {
                    pepper,
                    text_h,
                    generation: r.generation,
                    slug_len: r.slug_len,
                    held: r.held,
                    det_id_h: r.det_id_h,
                    state: r.state,
                    kind: r.kind,
                    package_load: r.package_load,
                    owner_h: r.owner_h,
                    created_at: r.created_at,
                };
                return existing(io, text, &row, sealed, account).await;
            }
            Claimed::Taken => len += 1,
            Claimed::SealUsed => return Err(RefusalCode::SealUsed),
            Claimed::Held => return held(io),
        }
    }
    io.metric("error", &["slug_exhausted"]);
    Err(RefusalCode::SlugExhausted)
}

/// Whether any row holds `cand` under any live pepper version (whatever its
/// state: a deleted page's slug is never used again).
async fn held_anywhere(io: &impl Io, peppers: &[&Pepper], cand: &str) -> Result<bool, RefusalCode> {
    let marks: Vec<String> = (1..=peppers.len()).map(|i| format!("?{i}")).collect();
    let sql = format!("SELECT 1 AS hit FROM detections WHERE id_h IN ({}) LIMIT 1", marks.join(", "));
    let params = peppers.iter().map(|p| Val::blob(&p.text_id_h(cand))).collect();
    let d = io.run(Stmt::owned(sql, params)).await.map_err(|_| RefusalCode::SaveUnavailable)?;
    Ok(!d.rows.is_empty())
}

/// The won claim: its document, then the row live, then the account.
#[allow(clippy::too_many_arguments)]
async fn finish(
    io: &impl Io,
    slug_: &str,
    pepper: &Pepper,
    id_h: &Bytes32,
    nonce_h: &Bytes32,
    text: &str,
    sealed: &Sealed,
    manage: String,
    account: Option<&Account>,
) -> Result<SaveAnswer, RefusalCode> {
    let doc = SecretDoc::text_page(text.to_owned(), sealed.result.clone());
    let bytes = seal_doc(io, pepper, slug_, &doc);
    let doc_bytes = bytes.len() as u64;
    let key = secret_doc_key(id_h);
    let drop = |now: u64| Stmt::new(DROP_FAILED_CLAIM, vec![Val::blob(id_h), Val::blob(nonce_h), Val::int(now)]);
    if io.doc_put(&key, bytes).await.is_err() {
        let _ = io.run(drop(io.now_ms())).await;
        return Err(RefusalCode::SaveUnavailable);
    }
    let now = io.now_ms();
    let flip = Stmt::new(FLIP_LIVE, vec![Val::blob(id_h), Val::blob(nonce_h), Val::int(doc_bytes), Val::int(now)]);
    let live = io.run(flip).await.map(|d| d.changes == 1);
    if live != Ok(true) {
        // The claim ended meanwhile (the operator, or the pending sweep), or
        // the flip's answer was lost: the claim ends unless the row is live,
        // and its document goes only once the row reads as not live. No link
        // was shown for the slug.
        let _ = io.run(drop(now)).await;
        let state = io.run(Stmt::new(SELECT_STATE, vec![Val::blob(id_h)])).await;
        let state = state.as_ref().ok().map(|d| d.first().and_then(|r| r.text("state")).map(str::to_owned));
        match state {
            Some(Some(s)) if s == DetState::Live.as_str() => {}
            Some(_) => {
                let _ = io.doc_delete(std::slice::from_ref(&key)).await;
                return Err(RefusalCode::SaveUnavailable);
            }
            None => return Err(RefusalCode::SaveUnavailable),
        }
    }
    io.metric("store", &["made"]);
    let mut answer = SaveAnswer {
        slug: slug_.to_owned(),
        manage: Some(manage),
        made: true,
        updated: false,
        saved: None,
        reason: None,
    };
    if let Some(a) = account {
        let add = OwnedAddRequest {
            user_id: a.user_id,
            slug: slug_.to_owned(),
            id_h: IdH(*id_h),
            summary: summary(Kind::PrivateText, &sealed.result, now),
            is_owner: true,
        };
        if io.owned_add(&add).await.is_ok() {
            answer.saved = Some(SavedAs::Owner);
        } else {
            let _ = io.run(Stmt::new(CLEAR_OWNER, vec![Val::blob(id_h), Val::blob(&a.owner_h)])).await;
            answer.reason = Some(SaveReason::AccountUnavailable);
        }
    }
    Ok(answer)
}

/// The pending text pages older than this the hourly cron tombstones: a save
/// that stopped between its claim and its flip.
pub const SELECT_STALE_PENDING: &str = "SELECT hex(id_h) AS id_h FROM detections \
WHERE kind = 'private_text' AND state = 'pending' AND created_at <= ?1 LIMIT ?2";

/// The stale claim's tombstone (its seal let go, as a failed put's).
pub const DROP_STALE_PENDING: &str = "UPDATE detections SET state = 'deleted', doc_key = NULL, doc_bytes = NULL, \
manage_h = NULL, owner_h = NULL, owner_via = NULL, seal_nonce_h = NULL, state_v = state_v + 1, updated_at = ?3 \
WHERE id_h = ?1 AND kind = 'private_text' AND state = 'pending' AND created_at <= ?2";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_claim_batch_reads_back_in_its_order() {
        let b = claim_batch(&[1; 32], 1, &[2; 32], 8, None, &[3; 32], &[4; 32], None, "load", 5);
        assert_eq!(b.len(), 6);
        assert_eq!(b[0].sql, INSERT_PENDING);
        assert_eq!(b[1].sql, INSERT_TEXT);
        assert_eq!(b[3].sql, SELECT_TEXT);
        assert_eq!(b[4].sql, SELECT_HOLDER);
        assert_eq!(b[5].sql, SEAL_USED);
        let b = claim_batch(&[1; 32], 1, &[2; 32], 9, Some(0), &[3; 32], &[4; 32], Some(&[5; 32]), "load", 5);
        assert_eq!(b[1].sql, NEXT_GEN);
        assert_eq!(b[0].params[6], Val::text("created"));
    }
}
