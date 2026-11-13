//! Get a link (`POST /api/detect/v1/share`): a sealed free check becomes a
//! secret page.
//!
//! In order:
//! 1. one batch reads the switch rows and whether the seal already made a
//!    link (`seal_nonce_h`, one share per scan);
//! 2. `DETECT_STORE` off answers `store_off`; a forged, expired or
//!    mismatched seal (the text's `canon_v1` hash differs) `seal_invalid`; a
//!    used one `seal_used`;
//! 3. the optional source is reduced to its host ([`source_host`]); only
//!    `{host, checked: false}` goes into the encrypted document, and D1 gets
//!    nothing of it;
//! 4. the Worker draws the 20-character slug and the 26-character manage
//!    token, encrypts `{text, result_full, source_check}` under the page's
//!    key and puts `p/<hex id_h>`;
//! 5. only then it inserts the detection (`private_text`, `paste`, `live`),
//!    with `owner_h` and `owner_via = created` when saved: nothing is
//!    written to D1 before the R2 put succeeds;
//! 6. when saved, the account Worker's `owned/add` records the link; if that
//!    fails, the ownership just set is cleared (`… WHERE id_h = ? AND
//!    owner_h = ?`) and the answer says `account_unavailable`. The link
//!    exists either way.
//!
//! No model runs, so nothing is counted. The text is in request memory only;
//! nothing derived from it reaches D1.

use slop_ninja_account_wire::OwnedAddRequest;
use slop_ninja_detect_page::api::{SaveReason, ShareAnswer, ShareRequest};
use slop_ninja_detect_page::codes::Kind;
use slop_ninja_detect_page::ids::{IdH, UserId};
use slop_ninja_detect_page::internal::Summary;
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_page::slug;
use slop_ninja_detect_page::sn::Principal;

use crate::db::{Io, Stmt, Val};
use crate::doc::{DOC_V, SecretDoc, SourceCheck, StoredResult};
use crate::keys::{Bytes32, NONCE_BYTES, Peppers, StoreKeys, ct_eq, manage_h, seal_nonce_h};
use crate::lookup::{SELECT_SWITCHES, SwitchRows};
use crate::store::secret_doc_key;

/// What an opened result seal holds (opened by the seal module).
#[derive(Clone, Debug, PartialEq)]
pub struct Sealed {
    /// `SHA-256(canon_v1(text))` of the scored text.
    pub text_sha256: [u8; 32],
    /// The seal's nonce, whose hash makes the seal single-use for links.
    pub nonce: Vec<u8>,
    pub result: StoredResult,
}

/// The keys a store flow needs.
#[derive(Clone, Copy, Debug)]
pub struct Keys<'a> {
    pub store: &'a StoreKeys,
    pub peppers: &'a Peppers,
}

/// `SELECT 1 … WHERE seal_nonce_h = ?1`.
pub const SEAL_USED: &str = "SELECT 1 AS used FROM detections WHERE seal_nonce_h = ?1";

/// The share insert. `ON CONFLICT DO NOTHING`: a second share of one seal
/// (or, never in practice, an id collision) inserts nothing.
pub const INSERT_SHARE: &str = "INSERT INTO detections (id_h, pepper_v, kind, platform, state, state_v, \
created_at, updated_at, expires_at, package_load, manage_h, owner_h, owner_via, seal_nonce_h, doc_key, doc_bytes) \
VALUES (?1, ?2, 'private_text', 'paste', 'live', 1, ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) ON CONFLICT DO NOTHING";

/// Clears an ownership this request set, when the account save failed.
pub const CLEAR_OWNER: &str = "UPDATE detections SET owner_h = NULL, owner_via = NULL WHERE id_h = ?1 AND owner_h = ?2";

/// The longest source a submitter may name.
pub const MAX_SOURCE_BYTES: usize = 2048;

/// The host of a submitter's source: an `http`/`https` URL or a bare host,
/// lowercased, without a trailing dot. `None` for anything else: userinfo,
/// a port that is not a number, an IP literal, a single label, a label out
/// of `[a-z0-9-]` (an internationalized name must be given in its A-label
/// form) or a name over 253 characters.
pub fn source_host(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || s.len() > MAX_SOURCE_BYTES {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    let rest = if let Some(r) = lower.strip_prefix("https://") {
        r
    } else if let Some(r) = lower.strip_prefix("http://") {
        r
    } else if lower.contains("://") {
        return None;
    } else {
        lower.as_str()
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.contains('@') || authority.starts_with('[') {
        return None;
    }
    let host = match authority.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
        Some(_) => return None,
        None => authority,
    };
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || host.len() > 253 {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let label_ok = |l: &&str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    if labels.len() < 2 || !labels.iter().all(label_ok) {
        return None;
    }
    // An all-digit last label is an IP literal or not a name at all.
    if labels.last().is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    Some(host.to_owned())
}

/// The account list's summary of a secret page.
pub fn summary(kind: Kind, r: &StoredResult, created_at: u64, expires_at: Option<u64>) -> Summary {
    Summary {
        kind,
        band: r.band,
        short: r.short,
        strong_sections: None,
        total_sections: None,
        words: r.total_words,
        created_at,
        expires_at,
    }
}

/// Draws a secret page's slug and manage token.
pub fn draw_secret(io: &impl Io) -> (String, String) {
    let mut fill = |b: &mut [u8]| io.fill(b);
    (slug::draw_secret(&mut fill), slug::draw_token(&mut fill))
}

/// Encrypts a document under the page's key, with a fresh nonce.
pub fn seal_doc(io: &impl Io, keys: Keys<'_>, slug: &str, doc: &SecretDoc) -> Vec<u8> {
    let mut nonce = [0u8; NONCE_BYTES];
    io.fill(&mut nonce);
    keys.peppers.current().doc_key(slug).seal(&nonce, &doc.encode())
}

/// Get a link. `sealed` is the opened seal (`None`: forged or expired);
/// `principal` the account the website resolved for a save.
pub async fn share(
    io: &impl Io,
    keys: Keys<'_>,
    req: &ShareRequest,
    sealed: Option<Sealed>,
    principal: Option<Principal>,
) -> Result<ShareAnswer, RefusalCode> {
    let nonce_h = sealed.as_ref().map(|s| seal_nonce_h(&s.nonce));
    let mut batch = vec![Stmt::new(SELECT_SWITCHES, Vec::new())];
    if let Some(n) = &nonce_h {
        batch.push(Stmt::new(SEAL_USED, vec![Val::blob(n)]));
    }
    let done = io.batch(batch).await.map_err(|_| RefusalCode::SaveUnavailable)?;
    if !SwitchRows::from_done(done.first()).on(io, "DETECT_STORE") {
        return Err(RefusalCode::StoreOff);
    }
    let (Some(sealed), Some(nonce_h)) = (sealed, nonce_h) else { return Err(RefusalCode::SealInvalid) };
    if !ct_eq(&crate::canon::text_sha256(&req.text), &sealed.text_sha256) {
        return Err(RefusalCode::SealInvalid);
    }
    if done.get(1).is_some_and(|d| !d.rows.is_empty()) {
        return Err(RefusalCode::SealUsed);
    }
    let source_check = match req.source.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => Some(SourceCheck { host: source_host(s).ok_or(RefusalCode::BadUrl)?, checked: false }),
    };
    let now = io.now_ms();
    let expires_at = req.expiry.duration_ms().map(|d| now.saturating_add(d));
    let owner: Option<(UserId, Bytes32)> =
        principal.filter(|_| req.save).map(|p| (p.user_id, keys.store.owner_h(&p.user_id.0)));
    let (slug, manage) = draw_secret(io);
    let pepper = keys.peppers.current();
    let id_h = pepper.id_h(&slug);
    let doc = SecretDoc {
        v: DOC_V,
        text: req.text.clone(),
        result_full: sealed.result.clone(),
        source_check,
        sections: None,
    };
    let bytes = seal_doc(io, keys, &slug, &doc);
    let doc_bytes = bytes.len() as u64;
    let key = secret_doc_key(&id_h);
    io.doc_put(&key, bytes).await.map_err(|_| RefusalCode::SaveUnavailable)?;
    let insert = Stmt::new(
        INSERT_SHARE,
        vec![
            Val::blob(&id_h),
            Val::int(u64::from(pepper.v)),
            Val::int(now),
            Val::opt_int(expires_at),
            Val::text(sealed.result.package_load.clone()),
            Val::blob(&manage_h(&manage)),
            Val::opt_blob(owner.as_ref().map(|(_, o)| o.as_slice())),
            if owner.is_some() { Val::text("created") } else { Val::Null },
            Val::blob(&nonce_h),
            Val::text(key.clone()),
            Val::int(doc_bytes),
        ],
    );
    let inserted = match io.run(insert).await {
        Ok(d) => d.changes == 1,
        Err(_) => {
            let _ = io.doc_delete(std::slice::from_ref(&key)).await;
            return Err(RefusalCode::SaveUnavailable);
        }
    };
    if !inserted {
        // Another share of this seal won (or the id collided): this request's
        // document is the only copy of nothing.
        let used =
            io.run(Stmt::new(SEAL_USED, vec![Val::blob(&nonce_h)])).await.map_err(|_| RefusalCode::SaveUnavailable)?;
        if !used.rows.is_empty() {
            let _ = io.doc_delete(std::slice::from_ref(&key)).await;
            return Err(RefusalCode::SealUsed);
        }
        return Err(RefusalCode::Internal);
    }
    io.metric("store", &["share"]);
    let mut answer = ShareAnswer { slug: slug.clone(), manage, saved: false, reason: None };
    if req.save && owner.is_none() {
        answer.reason = Some(SaveReason::SignedOut);
    }
    if let Some((user_id, o)) = owner {
        let add = OwnedAddRequest {
            user_id,
            slug,
            id_h: IdH(id_h),
            summary: summary(Kind::PrivateText, &sealed.result, now, expires_at),
            is_owner: true,
        };
        if io.owned_add(&add).await.is_ok() {
            answer.saved = true;
        } else {
            let _ = io.run(Stmt::new(CLEAR_OWNER, vec![Val::blob(&id_h), Val::blob(&o)])).await;
            answer.reason = Some(SaveReason::AccountUnavailable);
        }
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_hosts() {
        assert_eq!(source_host("https://x.com/a/status/1").as_deref(), Some("x.com"));
        assert_eq!(source_host("  Garden-Diary.Example/2026/11 ").as_deref(), Some("garden-diary.example"));
        assert_eq!(source_host("http://news.example:8080/a?b#c").as_deref(), Some("news.example"));
        assert_eq!(source_host("news.example.").as_deref(), Some("news.example"));
        assert_eq!(source_host(&format!("https://user{}news.example/", '@')), None);
        assert_eq!(source_host("https://203.0.113.9/"), None);
        assert_eq!(source_host("https://[2001:db8::1]/"), None);
        assert_eq!(source_host("localhost"), None);
        assert_eq!(source_host("ftp://news.example"), None);
        assert_eq!(source_host("news.example:http"), None);
        assert_eq!(source_host("bücher.example"), None);
        assert_eq!(source_host("xn--bcher-kva.example").as_deref(), Some("xn--bcher-kva.example"));
        assert_eq!(source_host("-bad.example"), None);
        assert_eq!(source_host(""), None);
        assert_eq!(source_host(&"a".repeat(3000)), None);
    }
}
