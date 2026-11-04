//! Automatic target handoff: the coordinator installs each closed cohort's
//! target as its signed calculation target and signs that cohort's
//! reward-intent authorization itself, with no operator step.
//!
//! The eval Worker (testnet shortcut T13) writes every cohort's signed
//! records to the receipts bucket under `cohorts/<label>/`. The ChainJournal
//! follows them there in one bounded pass per cron tick ([`run_pass`], the
//! same code natively and in the Worker through [`PassIo`]), in small steps,
//! each one transaction ([`plan_step`]):
//!
//! 1. **List** the cohort labels (`cohorts/` with delimiter `/`, one page a
//!    step) once per finalized epoch, and on every pass while the epoch is
//!    unknown (no signing preflight yet). A label not seen before is
//!    `unresolved`. A failed page does not hold the rest of the pass: the
//!    labels already known are still resolved and checked, and a failed
//!    continuation restarts the listing from its first page.
//! 2. **Resolve** one unresolved label: its manifest is opened under the
//!    trusted package key and checked against this chain and the main
//!    record's label prefixes ([`Record`]: the eval authorities the
//!    deployment names in `AUTO_TARGET_AUTHORITIES`). A manifest that opens,
//!    names this chain and its own label, and carries a main-record label is
//!    `in_record` with its cohort number and opening epoch. A manifest not
//!    signed by the trusted package key, or naming another chain or label,
//!    is `foreign` (load cohorts signed under the load keys); one with a
//!    label outside the main record is `outside` (a scratch authority's).
//!    Both are classified under the fingerprint of the trust roots, chain
//!    and record prefixes they were read with, and are read again when any
//!    of those change ([`requeue_reclassified`]), so a deploy that briefly
//!    carried a wrong key loses no cohort. A manifest whose signature is
//!    valid but whose record this build cannot decode is `unreadable`: a
//!    failure on `/status` (`manifest_undecodable`), read again at every
//!    listing. One with no manifest yet is `unsealed` until the next listing
//!    (a probe build never has one).
//! 3. **Check** the in-record cohorts after the last one handled or pending,
//!    in cohort order, and hand each closed one (`closure.signed`) to
//!    [`handle`] with its records. The first one not closed stops the step:
//!    a later cohort is never handed in past an earlier sealed one. Cohort
//!    numbers continue without gaps: cohort N is handed in only after N - 1
//!    was handled, is pending, or was passed by the operator
//!    (`auto_abandoned` for an in-record cohort, [`mark_abandoned`];
//!    `auto_gap` for a number with no in-record label, [`mark_gap`]); the
//!    first cohort of a chain record (none handled since the restart floor)
//!    is the lowest in-record one. Anything else stalls on `/status`
//!    (`cohort_gap`, or `earlier_cohort_unclosed` while a later cohort has
//!    closed behind an unclosed one).
//!
//! [`handle`] verifies the cohort's signed records under the trust roots
//! (manifest, package, closure and, when the closure names one, the target;
//! the published index bound by the manifest; the outcome recomputed), then:
//!
//! - **No target** (`NoTarget`): nothing is signed and the held target is
//!   unchanged. Its authorization stays current (no authorization expires),
//!   so the pointer keeps naming the target's own cohort, whose receipts the
//!   eval Worker retains. Whether to sign carried authorizations naming a
//!   later no-target cohort (whose receipts go after N+2) waits on the
//!   user's ruling on the carrier hold.
//! - **A target**: the authorization (`RewardIntentAuthorization` for the
//!   closed cohort and its target, issued at the closing epoch, when the
//!   target activates, with the manifest's renew-by distance) is signed with
//!   the authorization key ([`AuthorizationKeySource`]: a Worker secret
//!   distinct from the hotkey and from the eval Worker's package and receipt
//!   keys), wrapped in the coordinator's own reward-v1 bundle form (records
//!   only) and installed through the operator path's own checks
//!   ([`super::install_target_v2_from`]: verification against the held
//!   target, the restart floor, the held record moved forward). The Worker's
//!   pointer pass then writes the new authorization to the pointer
//!   validators fetch.
//!
//! **Retained receipts only.** The signed calculation target changes only
//! to a cohort whose detailed receipts are retained: its signed receipt
//! index authenticates under the receipt key (streamed, [`Receipts`]) to the
//! receipt root and count its package binds, its chunk map is present, and
//! the eval Worker has not started retiring them. A target cohort whose
//! receipts are retired (only after a coordinator outage longer than two
//! cohorts) is checked from the held target and journaled as a pending
//! intermediate (`sign_auto_pending`, [`Pending`]): it is never installed or
//! authorized on its own and does not count as handled. Each pass resumes
//! the chain after the last pending cohort instead of verifying it again, so
//! a chain of any length completes over as many passes as it needs. The
//! pending cohorts are recorded only in the transaction that installs a
//! later closed cohort's target with retained receipts, whose predecessor
//! the last of them leaves held (the held record moves straight to it, as a
//! validator's walk through intermediates does); that transaction first
//! checks that the held target is still the one the first pending cohort was
//! checked from, and any install clears them. Until such a cohort closes
//! the intermediates wait (`receipts_retired`), and the held target keeps
//! committing. A target cohort with neither its receipts nor the eval
//! Worker's retirement marker waits the same way but is reported as
//! `receipts_missing`: a retention or write-order defect, not N+2 retention.
//!
//! **Once per cohort, never skipped.** Each cohort is handled once
//! (`sign_auto_cohorts`); a duplicate or stale wakeup, or a second call for
//! the same cohort, answers the recorded outcome and signs nothing. The
//! authorization's bytes are a function of the cohort's records, the
//! installed generation and the key (Ed25519 signatures are deterministic),
//! so a transaction that never committed (an eviction mid-sign) signs the
//! same bytes when it runs again. A cohort that cannot be handled yet
//! (records missing, a refusal, the key absent) is retried at every pass
//! with its failure on `/status` (`signing.auto_target.error`), and the
//! cohorts after it wait: a later target's predecessor is the earlier one.
//! A failure clears itself when the step it came from next succeeds.
//!
//! **Revocation stays manual.** The handler signs only the closed cohort's
//! own authorization, at its closing epoch, once. It never re-authorizes a
//! revoked target: a revocation at or after that epoch withdraws it. When
//! the revocation is journaled before the handoff runs, the target is still
//! installed (its closure activated it), as revoked: nothing commits for it,
//! exactly as when the revocation comes after the install, and the next
//! cohort's target installs as its successor. A re-authorization is an
//! operator bundle.
//!
//! **Never gated on other validators.** Nothing here reads another
//! validator's state, version or acknowledgment. Signing being paused does
//! not stop it either (the operator path installs while paused too); only
//! the operator's own `auto_pause` holds it, and `auto_resume` continues from
//! the same cohort. Each pass also checks that the authorization key's
//! secret is present and derives the trusted root (`key_ok` on `/status`),
//! without signing, without reading the hotkey's secret (compared only when
//! an authorization is signed) and without holding cohorts that need no
//! signature. A pass that runs out of its deadline before any progress
//! reports `pass_budget` on the cohort it was on.

use super::{held_target, install_target_v2_from, log, opt_t, opt_u, restart_floor, signer_target_bundle};
use crate::sql::{Sql, SqlValue, StoreError, bind_u64, text, uint};
use crate::target;
use serde::Serialize;
use slop_ninja_protocol::submission::CohortClosure;
use slop_ninja_protocol::{
    ActiveTarget, CalculationTargetV2, ChainBinding, CohortManifestV2, Digest, FingerprintIndex, KeyRole, LaunchAuthorization,
    PublishedContents, PublishedIndex, ReceiptIndexBinding, ReceiptIndexStream, Record as Record_, RewardIntentAuthorization,
    ScorePackageV2, Signed, SigningKey, TrustedKeys, sign,
};
use zeroize::Zeroizing;

/// The receipts-bucket prefix of every cohort's objects
/// (`eval::engine::receipt_prefix`).
pub const COHORT_PREFIX: &str = "cohorts/";

/// The object names a step reads under a cohort's prefix.
pub mod object {
    pub const MANIFEST: &str = "manifest.signed";
    pub const PACKAGE: &str = "package.signed";
    pub const TARGET: &str = "target.signed";
    pub const CLOSURE: &str = "closure.signed";
    pub const PUBLISHED_CONTENTS: &str = "published-contents.bin";
    pub const FINGERPRINT_INDEX: &str = "fingerprint-index.bin";
    pub const RECEIPT_INDEX: &str = "receipt-index.signed";
    pub const CHUNK_MAP: &str = "chunk-map.json";
    /// The eval Worker's marker, written before a cohort's first detailed
    /// receipt is deleted under the N+2 rule.
    pub const RECEIPTS_RETIRED: &str = "receipts-retired.json";
}

/// The key of `name` under cohort `label`'s prefix.
pub fn key(label: &str, name: &str) -> String {
    format!("{COHORT_PREFIX}{label}/{name}")
}

/// The JSON a bundle adds around its records: the field names, the
/// authorization's hex (a signed reward-intent authorization is 309 bytes)
/// and the kind. Checked against the encoded bundle by a test.
pub const BUNDLE_FRAMING_BYTES: usize = 2 * 1024;

/// What one cohort's records may occupy, raw, in total: the coordinator's
/// bundle holds them hex-encoded within the largest stored bundle every
/// later signing step re-verifies within its own footprint
/// ([`SIGNED_BUNDLE_BYTES`]), so half of that less the framing. A step reads
/// each object within what is left of it; a cohort above it is refused
/// (`cohort_too_large`) before anything is signed or installed. The
/// `/sign-auto` route's footprint is derived from it
/// (`arena_routes::JOURNAL_AUTO_BYTES`).
pub const SOURCE_BYTES: usize = (SIGNED_BUNDLE_BYTES - BUNDLE_FRAMING_BYTES) / 2;

/// The largest reward-v1 bundle the coordinator installs, by either path:
/// the smaller of one stored value ([`target::MAX_BUNDLE_V2_BYTES`]) and
/// what a signing step (`journal/fetch`, `arena_routes::JOURNAL_CONTROL_BYTES`)
/// can parse and verify again at every use
/// (`arena_routes::SIGNING_BUNDLE_BYTES`, witnessed natively).
pub const SIGNED_BUNDLE_BYTES: usize = if crate::arena_routes::SIGNING_BUNDLE_BYTES < target::MAX_BUNDLE_V2_BYTES {
    crate::arena_routes::SIGNING_BUNDLE_BYTES
} else {
    target::MAX_BUNDLE_V2_BYTES
};

/// The main chain record's labels: the eval authorities the deployment names
/// (`AUTO_TARGET_AUTHORITIES`, comma separated, matching the eval Worker's
/// `EVAL_MAIN_AUTHORITY` and the `EVAL_MAIN_PREVIOUS` it still follows).
/// Only labels under their prefixes (`testnet-<name>-`) are handed in, so a
/// scratch authority's cohorts in the same bucket under the same package key
/// can never move the main record's numbering or stall it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    prefixes: Vec<String>,
}

impl Record {
    /// Reads `AUTO_TARGET_AUTHORITIES`: one or more named authorities (the
    /// original `main` has no label prefix and cannot be bound). Unset,
    /// empty or malformed is refused, so the handoff runs bound or not at all.
    pub fn parse(authorities: Option<&str>) -> Result<Self, &'static str> {
        let mut prefixes = Vec::new();
        for name in authorities.unwrap_or_default().split(',').map(str::trim).filter(|n| !n.is_empty()) {
            if !crate::config::authority_name_ok(name) {
                return Err("record_authorities_malformed");
            }
            let p = format!("{}-{name}-", crate::config::NETWORK);
            if !prefixes.contains(&p) {
                prefixes.push(p);
            }
        }
        if prefixes.is_empty() {
            return Err("record_authorities_unset");
        }
        prefixes.sort();
        Ok(Self { prefixes })
    }

    /// A record of raw label prefixes (tests and tools).
    pub fn prefixes(prefixes: &[&str]) -> Self {
        let mut prefixes: Vec<String> = prefixes.iter().map(|p| (*p).to_owned()).collect();
        prefixes.sort();
        Self { prefixes }
    }

    /// Whether `label` is one of the main record's.
    pub fn admits(&self, label: &str) -> bool {
        self.prefixes.iter().any(|p| label.len() > p.len() && label.starts_with(p.as_str()))
    }

    /// What a label's classification depends on: the trusted package key,
    /// the chain and these prefixes ([`requeue_reclassified`]).
    pub fn fingerprint(&self, cfg: &super::SignConfig, keys: &TrustedKeys) -> String {
        use sha2::{Digest as _, Sha256};
        let mut h = Sha256::new();
        h.update(b"slop_ninja.sign_auto.classification/v1\0");
        h.update(keys.key(KeyRole::Package));
        h.update(cfg.genesis.0);
        h.update(cfg.netuid.to_le_bytes());
        for p in &self.prefixes {
            h.update((p.len() as u64).to_le_bytes());
            h.update(p.as_bytes());
        }
        let digest = h.finalize();
        hex::encode(digest.get(..16).unwrap_or_default())
    }
}

/// The label of a delimited listing prefix (`cohorts/<label>/`), if it names
/// one this module can read: a non-empty label of the characters a plan's
/// label may use, so every key built from it is a plain path.
pub fn label_of(prefix: &str) -> Option<&str> {
    let label = prefix.strip_prefix(COHORT_PREFIX)?.strip_suffix('/')?;
    (!label.is_empty() && label.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && label != "."
        && label != "..")
        .then_some(label)
}

/// The authorization signing key, read while signing an authorization, and
/// checked once per pass ([`run_pass`]).
pub trait AuthorizationKeySource {
    /// The checked key for signing, or a content-free refusal code: it
    /// derives the trusted authorization root and is not the hotkey's seed.
    fn authorization_key(&self, keys: &TrustedKeys) -> Result<SigningKey, &'static str>;
    /// The per-pass check: the key is present and derives the trusted root.
    /// A source holding other secrets (the Worker's hotkey) leaves them
    /// unread here; the hotkey comparison runs only while signing.
    fn check_key(&self, keys: &TrustedKeys) -> Result<(), &'static str> {
        self.authorization_key(keys).map(|_| ())
    }
}

/// The authorization key from its secret (32-byte hex, an optional `0x`):
/// it must derive the trusted authorization root, and it must differ from
/// the coordinator hotkey's seed when that is readable. The other trust
/// roots (package, receipt) are distinct from the authorization root by
/// construction ([`TrustedKeys`]), so a key deriving it is none of them.
pub fn authorization_key_from(
    seed_hex: &str,
    hotkey_seed_hex: Option<&str>,
    keys: &TrustedKeys,
) -> Result<SigningKey, &'static str> {
    let decode = |s: &str| -> Option<Zeroizing<[u8; 32]>> {
        let bytes = Zeroizing::new(hex::decode(s.trim().trim_start_matches("0x")).ok()?);
        let arr: [u8; 32] = bytes.as_slice().try_into().ok()?;
        Some(Zeroizing::new(arr))
    };
    let seed = decode(seed_hex).ok_or("authorization_key_malformed")?;
    if let Some(hotkey) = hotkey_seed_hex.and_then(decode)
        && *hotkey == *seed
    {
        return Err("authorization_key_is_hotkey");
    }
    let key = SigningKey::from_bytes(&seed);
    if key.verifying_key().to_bytes() != *keys.key(KeyRole::Authorization) {
        return Err("authorization_key_mismatch");
    }
    Ok(key)
}

fn chain_of(cfg: &super::SignConfig) -> ChainBinding {
    ChainBinding {
        genesis: Digest(cfg.genesis.0),
        netuid: cfg.netuid,
        mechanism: 0,
    }
}

// ------------------------------------------------------------------ state

struct State {
    paused: bool,
    listing: bool,
    cursor: Option<String>,
    listed_epoch: Option<u64>,
    listed_at_ms: Option<u64>,
}

fn state(sql: &impl Sql) -> Result<State, StoreError> {
    let rows = sql.exec(
        "SELECT paused, listing, list_cursor, listed_epoch, listed_at_ms FROM sign_auto_state WHERE id = 1",
        &[],
    )?;
    Ok(match rows.first() {
        None => State { paused: false, listing: false, cursor: None, listed_epoch: None, listed_at_ms: None },
        Some(r) => State {
            paused: uint(r, 0)? != 0,
            listing: uint(r, 1)? != 0,
            cursor: opt_t(r, 2),
            listed_epoch: opt_u(r, 3),
            listed_at_ms: opt_u(r, 4),
        },
    })
}

fn ensure_state(sql: &impl Sql, now_ms: u64) -> Result<(), StoreError> {
    sql.exec(
        "INSERT INTO sign_auto_state (id, paused, listing, updated_at_ms) VALUES (1, 0, 0, ?) ON CONFLICT(id) DO NOTHING",
        &[bind_u64(now_ms)?],
    )?;
    Ok(())
}

/// The operator's hold on automatic handoff (`auto_pause` / `auto_resume`).
/// Holding it changes nothing else: the held target keeps committing and a
/// resume continues from the same cohort. Returns whether the flag changed.
pub fn set_paused(sql: &impl Sql, paused: bool, now_ms: u64) -> Result<bool, StoreError> {
    ensure_state(sql, now_ms)?;
    let was = state(sql)?.paused;
    if was == paused {
        return Ok(false);
    }
    sql.exec(
        "UPDATE sign_auto_state SET paused = ?, updated_at_ms = ? WHERE id = 1",
        &[SqlValue::Integer(i64::from(paused)), bind_u64(now_ms)?],
    )?;
    log(sql, now_ms, None, None, None, if paused { "auto_target_paused" } else { "auto_target_resumed" }, None)?;
    Ok(true)
}

/// The operator's record that an in-record cohort was abandoned and will
/// never close (`auto_abandoned`): the positive evidence that lets the
/// cohorts after it be handed in. Only a cohort not handled or pending yet,
/// and in record, is passed; a refusal changes nothing. The Worker refuses
/// it first for a cohort whose closure is in the bucket (`cohort_closed`);
/// [`unmark_abandoned`] takes it back while nothing after it was handled.
pub fn mark_abandoned(sql: &impl Sql, label: &str, now_ms: u64) -> Result<Result<u64, &'static str>, StoreError> {
    let rows = sql.exec("SELECT state, cohort FROM sign_auto_labels WHERE label = ?", &[label.into()])?;
    let Some(r) = rows.first() else {
        return Ok(Err("label_unknown"));
    };
    if text(r, 0)? != "in_record" {
        return Ok(Err("label_not_in_record"));
    }
    if !sql.exec("SELECT 1 FROM sign_auto_pending WHERE label = ?", &[label.into()])?.is_empty() {
        return Ok(Err("label_pending"));
    }
    let cohort = uint(r, 1)?;
    sql.exec(
        "UPDATE sign_auto_labels SET state = 'passed', detail = 'operator_abandoned' WHERE label = ?",
        &[label.into()],
    )?;
    log(sql, now_ms, None, None, None, "auto_target_abandoned", Some(&format!("{cohort} {label}")))?;
    clear_failure_of(sql, &[label], now_ms)?;
    Ok(Ok(cohort))
}

/// Takes back an operator's `auto_abandoned` record (`auto_unabandoned`)
/// while no cohort after it has been handled or is pending, so a cohort
/// passed by mistake that later closes is handed in after all.
pub fn unmark_abandoned(sql: &impl Sql, label: &str, now_ms: u64) -> Result<Result<u64, &'static str>, StoreError> {
    let rows = sql.exec("SELECT state, cohort FROM sign_auto_labels WHERE label = ?", &[label.into()])?;
    let Some(r) = rows.first() else {
        return Ok(Err("label_unknown"));
    };
    if text(r, 0)? != "passed" {
        return Ok(Err("label_not_passed"));
    }
    let cohort = uint(r, 1)?;
    let floor = restart_floor(sql)?.unwrap_or(0);
    if chain_max(sql, floor)?.is_some_and(|m| m > cohort) {
        return Ok(Err("later_cohort_handled"));
    }
    sql.exec("UPDATE sign_auto_labels SET state = 'in_record', detail = NULL WHERE label = ?", &[label.into()])?;
    log(sql, now_ms, None, None, None, "auto_target_unabandoned", Some(&format!("{cohort} {label}")))?;
    Ok(Ok(cohort))
}

/// The operator's record that cohort number `cohort` has no in-record label
/// and never will (`auto_gap`: a number the eval Worker consumed without a
/// main-record manifest), so the numbering continues past it. Only a number
/// after the last handled or pending one, and with no in-record, handled or
/// passed label, is recorded; a label resolving to it later is reported
/// (`cohort_marked_absent`).
pub fn mark_gap(sql: &impl Sql, cohort: u64, now_ms: u64) -> Result<Result<u64, &'static str>, StoreError> {
    let floor = restart_floor(sql)?.unwrap_or(0);
    if chain_max(sql, floor)?.is_some_and(|m| cohort <= m) {
        return Ok(Err("cohort_already_handled"));
    }
    let known = sql.exec(
        "SELECT 1 FROM sign_auto_labels WHERE cohort = ? AND state IN ('in_record', 'handled', 'passed')",
        &[bind_u64(cohort)?],
    )?;
    if !known.is_empty() {
        return Ok(Err("cohort_has_a_label"));
    }
    sql.exec(
        "INSERT INTO sign_auto_absent (cohort, at_ms) VALUES (?, ?) ON CONFLICT(cohort) DO NOTHING",
        &[bind_u64(cohort)?, bind_u64(now_ms)?],
    )?;
    log(sql, now_ms, None, None, None, "auto_target_gap", Some(&cohort.to_string()))?;
    Ok(Ok(cohort))
}

/// Records a failure of the pass on `label` (empty: the pass itself), once
/// per distinct failure in the sign log; its first time stays on `/status`
/// until the step it came from succeeds ([`clear_failure_of`]).
pub fn note_failure(sql: &impl Sql, label: &str, code: &str, now_ms: u64) -> Result<(), StoreError> {
    ensure_state(sql, now_ms)?;
    let rows = sql.exec("SELECT error, error_label FROM sign_auto_state WHERE id = 1", &[])?;
    let same = rows.first().is_some_and(|r| opt_t(r, 0).as_deref() == Some(code) && opt_t(r, 1).as_deref() == Some(label));
    if same {
        return Ok(());
    }
    sql.exec(
        "UPDATE sign_auto_state SET error = ?, error_label = ?, error_since_ms = ?, updated_at_ms = ? WHERE id = 1",
        &[code.into(), label.into(), bind_u64(now_ms)?, bind_u64(now_ms)?],
    )?;
    let detail = if label.is_empty() { code.to_owned() } else { format!("{label}: {code}") };
    log(sql, now_ms, None, None, None, "auto_target_failed", Some(&detail))?;
    Ok(())
}

/// Clears the recorded failure when it belongs to one of `labels` (the
/// empty label: a failure of the pass itself, such as a listing).
pub fn clear_failure_of(sql: &impl Sql, labels: &[&str], now_ms: u64) -> Result<(), StoreError> {
    let rows = sql.exec("SELECT error, error_label FROM sign_auto_state WHERE id = 1", &[])?;
    let Some(r) = rows.first() else {
        return Ok(());
    };
    if opt_t(r, 0).is_none() || !labels.contains(&opt_t(r, 1).unwrap_or_default().as_str()) {
        return Ok(());
    }
    sql.exec(
        "UPDATE sign_auto_state SET error = NULL, error_label = NULL, error_since_ms = NULL, updated_at_ms = ? WHERE id = 1",
        &[bind_u64(now_ms)?],
    )?;
    log(sql, now_ms, None, None, None, "auto_target_recovered", None)?;
    Ok(())
}

/// Records this pass's check of the authorization key's secret (`None`:
/// present and deriving the trusted root), written only when it changes.
pub fn note_key(sql: &impl Sql, error: Option<&str>, now_ms: u64) -> Result<(), StoreError> {
    ensure_state(sql, now_ms)?;
    let rows = sql.exec("SELECT key_error, key_checked_at_ms FROM sign_auto_state WHERE id = 1", &[])?;
    let (was, checked) = rows.first().map(|r| (opt_t(r, 0), opt_u(r, 1))).unwrap_or((None, None));
    if checked.is_some() && was.as_deref() == error {
        return Ok(());
    }
    sql.exec(
        "UPDATE sign_auto_state SET key_error = ?, key_checked_at_ms = ?, updated_at_ms = ? WHERE id = 1",
        &[error.map_or(SqlValue::Null, |e| e.into()), bind_u64(now_ms)?, bind_u64(now_ms)?],
    )?;
    log(sql, now_ms, None, None, None, if error.is_some() { "auto_target_key_refused" } else { "auto_target_key_ok" }, error)?;
    Ok(())
}

/// The highest cohort handled in the current chain record (labels opened at
/// or after the restart floor), if any.
fn handled_max(sql: &impl Sql, floor: u64) -> Result<Option<u64>, StoreError> {
    Ok(sql
        .exec("SELECT MAX(cohort) FROM sign_auto_cohorts WHERE opened_epoch >= ?", &[bind_u64(floor)?])?
        .first()
        .and_then(|r| opt_u(r, 0)))
}

/// The highest cohort handled or pending in the current chain record: the
/// chain resumes after it.
fn chain_max(sql: &impl Sql, floor: u64) -> Result<Option<u64>, StoreError> {
    let pending = sql
        .exec("SELECT MAX(cohort) FROM sign_auto_pending WHERE opened_epoch >= ?", &[bind_u64(floor)?])?
        .first()
        .and_then(|r| opt_u(r, 0));
    Ok(handled_max(sql, floor)?.max(pending))
}

/// How many numbers strictly between `after` and `before` the operator
/// passed (an abandoned in-record cohort, or a recorded gap).
fn passed_between(sql: &impl Sql, floor: u64, after: u64, before: u64) -> Result<u64, StoreError> {
    let (a, b) = (bind_u64(after)?, bind_u64(before)?);
    Ok(sql
        .exec(
            "SELECT COUNT(*) FROM (
                SELECT cohort FROM sign_auto_labels WHERE state = 'passed' AND opened_epoch >= ? AND cohort > ? AND cohort < ?
                UNION SELECT cohort FROM sign_auto_absent WHERE cohort > ? AND cohort < ?)",
            &[bind_u64(floor)?, a.clone(), b.clone(), a, b],
        )?
        .first()
        .map(|r| uint(r, 0))
        .transpose()?
        .unwrap_or(0))
}

// ------------------------------------------------------------------ steps

/// The next step of a pass ([`plan_step`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing to do now: `paused` or `waiting` (no cohort to check).
    Idle(&'static str),
    /// List one page of cohort labels, continuing from `cursor`.
    List { cursor: Option<String> },
    /// Read one label's manifest.
    Resolve { label: String },
    /// Check these labels, in cohort order, handing in each closed one up to
    /// the first that is not.
    Check { labels: Vec<String> },
}

/// The pass's next step. `listed` says whether this pass has already
/// completed a listing (while the epoch is unknown every pass lists once);
/// `list_failed` that a listing step of this pass failed, so the labels
/// already known are resolved and checked without it.
pub fn plan_step(sql: &impl Sql, listed: bool, list_failed: bool, now_ms: u64) -> Result<Step, StoreError> {
    ensure_state(sql, now_ms)?;
    let st = state(sql)?;
    if st.paused {
        return Ok(Step::Idle("paused"));
    }
    let epoch = super::authority::finalized_epoch(sql)?;
    let due = match epoch {
        Some(e) => st.listed_epoch != Some(e),
        None => !listed,
    };
    if !list_failed && (st.listing || st.listed_at_ms.is_none() || (due && !listed)) {
        return Ok(Step::List { cursor: st.cursor });
    }
    if let Some(r) = sql
        .exec("SELECT label FROM sign_auto_labels WHERE state = 'unresolved' ORDER BY label LIMIT 1", &[])?
        .first()
    {
        return Ok(Step::Resolve { label: text(r, 0)?.to_owned() });
    }
    let rows = waiting(sql)?;
    if rows.is_empty() {
        return Ok(Step::Idle("waiting"));
    }
    Ok(Step::Check { labels: rows.into_iter().map(|(_, l)| l).collect() })
}

/// The in-record cohorts after the last one handled or pending in this
/// chain record, in cohort order.
fn waiting(sql: &impl Sql) -> Result<Vec<(u64, String)>, StoreError> {
    let floor = restart_floor(sql)?.unwrap_or(0);
    let after = chain_max(sql, floor)?;
    sql.exec(
        "SELECT cohort, label FROM sign_auto_labels WHERE state = 'in_record' AND opened_epoch >= ? AND cohort > ?
         ORDER BY cohort, label",
        &[bind_u64(floor)?, after.map_or(SqlValue::Integer(-1), |a| SqlValue::Integer(a as i64))],
    )?
    .iter()
    .map(|r| Ok((uint(r, 0)?, text(r, 1)?.to_owned())))
    .collect()
}

/// One listing page: every readable label is recorded (a new one as
/// `unresolved`; an `unsealed` or `unreadable` one is resolved again); `next` continues the
/// listing at the next step, `None` completes it at the finalized epoch. A
/// page clears a failure of the pass itself. Returns the labels recorded for
/// the first time.
pub fn note_page(sql: &impl Sql, prefixes: &[String], next: Option<&str>, now_ms: u64) -> Result<u64, StoreError> {
    ensure_state(sql, now_ms)?;
    let mut new = 0u64;
    for p in prefixes {
        let Some(label) = label_of(p) else { continue };
        let before = sql.exec("SELECT state FROM sign_auto_labels WHERE label = ?", &[label.into()])?;
        match before.first() {
            None => {
                sql.exec(
                    "INSERT INTO sign_auto_labels (label, state, seen_at_ms) VALUES (?, 'unresolved', ?)",
                    &[label.into(), bind_u64(now_ms)?],
                )?;
                new += 1;
            }
            Some(r) if matches!(text(r, 0)?, "unsealed" | "unreadable") => {
                sql.exec("UPDATE sign_auto_labels SET state = 'unresolved' WHERE label = ?", &[label.into()])?;
            }
            Some(_) => {}
        }
    }
    match next {
        Some(cursor) => {
            sql.exec(
                "UPDATE sign_auto_state SET listing = 1, list_cursor = ?, updated_at_ms = ? WHERE id = 1",
                &[cursor.into(), bind_u64(now_ms)?],
            )?;
        }
        None => {
            let epoch = super::authority::finalized_epoch(sql)?;
            sql.exec(
                "UPDATE sign_auto_state SET listing = 0, list_cursor = NULL, listed_epoch = ?, listed_at_ms = ?,
                    updated_at_ms = ? WHERE id = 1",
                &[epoch.map_or(Ok(SqlValue::Null), bind_u64)?, bind_u64(now_ms)?, bind_u64(now_ms)?],
            )?;
        }
    }
    clear_failure_of(sql, &[""], now_ms)?;
    Ok(new)
}

/// A listing page that failed: the failure is recorded, and a failed
/// continuation is dropped so the next listing starts from the first page
/// (a cursor the bucket keeps refusing never holds the handoff).
pub fn note_list_failed(sql: &impl Sql, continued: bool, now_ms: u64) -> Result<(), StoreError> {
    note_failure(sql, "", "list_unavailable", now_ms)?;
    if continued {
        sql.exec(
            "UPDATE sign_auto_state SET listing = 0, list_cursor = NULL, updated_at_ms = ? WHERE id = 1",
            &[bind_u64(now_ms)?],
        )?;
    }
    Ok(())
}

/// Labels classified `foreign` or `outside` under another fingerprint
/// ([`Record::fingerprint`]: the trust roots, chain or record prefixes
/// changed since) are read again. Returns how many.
pub fn requeue_reclassified(sql: &impl Sql, fingerprint: &str) -> Result<u64, StoreError> {
    let rows = sql.exec(
        "SELECT COUNT(*) FROM sign_auto_labels WHERE state IN ('foreign', 'outside') AND classified IS NOT ?",
        &[fingerprint.into()],
    )?;
    let n = rows.first().map(|r| uint(r, 0)).transpose()?.unwrap_or(0);
    if n > 0 {
        sql.exec(
            "UPDATE sign_auto_labels SET state = 'unresolved', classified = NULL
             WHERE state IN ('foreign', 'outside') AND classified IS NOT ?",
            &[fingerprint.into()],
        )?;
    }
    Ok(n)
}

/// Resolves `label` from its manifest's bytes (`None`: not sealed yet)
/// under `record`, whose [`Record::fingerprint`] is `fingerprint`. Returns
/// the label's new state.
#[allow(clippy::too_many_arguments)]
pub fn note_manifest(
    sql: &impl Sql,
    cfg: &super::SignConfig,
    keys: &TrustedKeys,
    record: &Record,
    fingerprint: &str,
    label: &str,
    manifest: Option<&[u8]>,
    now_ms: u64,
) -> Result<&'static str, StoreError> {
    let (state, cohort, opened, detail): (&'static str, Option<u64>, Option<u64>, Option<&str>) = match manifest {
        None => ("unsealed", None, None, None),
        // The signature first: a record the trusted key signed but this
        // build cannot decode (a schema newer than the build) is not foreign.
        Some(bytes) => match Signed::<CohortManifestV2>::open_raw(bytes, keys) {
            Err(_) => ("foreign", None, None, Some("manifest_unverified")),
            Ok(raw) => match <CohortManifestV2 as Record_>::from_bytes(raw) {
                Err(_) => ("unreadable", None, None, Some("manifest_undecodable")),
                Ok(m) if m.chain != chain_of(cfg) => ("foreign", None, None, Some("chain")),
                Ok(m) if m.cohort_label != label => ("foreign", None, None, Some("label")),
                Ok(_) if !record.admits(label) => ("outside", None, None, Some("authority")),
                Ok(m) => ("in_record", Some(m.cohort), Some(m.opened_epoch), None),
            },
        },
    };
    let classified = matches!(state, "foreign" | "outside").then_some(fingerprint);
    sql.exec(
        "INSERT INTO sign_auto_labels (label, state, cohort, opened_epoch, detail, classified, seen_at_ms)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(label) DO UPDATE SET state = excluded.state, cohort = excluded.cohort,
            opened_epoch = excluded.opened_epoch, detail = excluded.detail, classified = excluded.classified",
        &[
            label.into(),
            state.into(),
            cohort.map_or(Ok(SqlValue::Null), bind_u64)?,
            opened.map_or(Ok(SqlValue::Null), bind_u64)?,
            detail.map_or(SqlValue::Null, |d| d.into()),
            classified.map_or(SqlValue::Null, |f| f.into()),
            bind_u64(now_ms)?,
        ],
    )?;
    // An in-record cohort the record has already moved past (it resolved
    // only after later cohorts were handled, for example once a wrong trust
    // root was corrected) is never handed in: it is reported, not skipped
    // silently.
    let floor = restart_floor(sql)?.unwrap_or(0);
    let behind = match (state, cohort, opened) {
        ("in_record", Some(c), Some(o)) => o >= floor && chain_max(sql, floor)?.is_some_and(|m| c < m),
        _ => false,
    };
    match (state, cohort) {
        ("unreadable", _) => note_failure(sql, label, "manifest_undecodable", now_ms)?,
        ("in_record", Some(c))
            if !sql.exec("SELECT 1 FROM sign_auto_absent WHERE cohort = ?", &[bind_u64(c)?])?.is_empty() =>
        {
            note_failure(sql, label, "cohort_marked_absent", now_ms)?
        }
        ("in_record", Some(_)) if behind => note_failure(sql, label, "cohort_behind_record", now_ms)?,
        _ => clear_failure_of(sql, &[label], now_ms)?,
    }
    Ok(state)
}

// ---------------------------------------------------------------- handling

/// A target cohort's detailed receipts as a step found them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Receipts {
    /// Not read (the cohort has no target object).
    #[default]
    Unread,
    /// The eval Worker has started retiring them (its marker is present).
    Retired,
    /// The signed receipt index or the chunk map is missing.
    Missing,
    /// The signed index authenticated under the receipt key to this binding,
    /// with the chunk map present.
    Index(ReceiptIndexBinding),
    /// The signed index is present and does not authenticate.
    Invalid,
}

/// One closed cohort's objects as a step read them (raw bytes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observed {
    pub label: String,
    pub manifest: Option<Vec<u8>>,
    pub package: Option<Vec<u8>>,
    pub closure: Vec<u8>,
    /// `target.signed`, when present.
    pub target: Option<Vec<u8>>,
    pub published_contents: Option<Vec<u8>>,
    pub fingerprint_index: Option<Vec<u8>>,
    pub receipts: Receipts,
    /// An object did not fit what is left of [`SOURCE_BYTES`].
    pub too_large: bool,
}

/// What [`handle`] did with a cohort.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Handled {
    pub label: String,
    pub cohort: Option<u64>,
    /// `no_target`, `installed`, `intermediate` (checked on the way to a
    /// later installed target, never installed itself), `held` (its target
    /// was already held), `superseded` (a later target is held), `revoked`
    /// (installed, withdrawn by a journaled revocation), or `None` when the
    /// cohort was not handled (`error` says why; it is retried).
    pub outcome: Option<String>,
    pub target: Option<String>,
    pub authorization: Option<String>,
    /// First time this cohort was handled (`false`: the recorded outcome).
    pub new: bool,
    pub error: Option<String>,
}

impl Handled {
    fn failed(label: &str, cohort: Option<u64>, code: &str) -> Self {
        Self {
            label: label.to_owned(),
            cohort,
            outcome: None,
            target: None,
            authorization: None,
            new: false,
            error: Some(code.to_owned()),
        }
    }
}

/// A closed cohort verified but not recorded yet, journaled in
/// `sign_auto_pending`: it is recorded only in the transaction that installs
/// a later cohort's target (module docs). `from` is the held target it was
/// checked from, `to` the one it leaves held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub label: String,
    pub cohort: u64,
    pub opened_epoch: u64,
    pub closed_epoch: u64,
    /// `intermediate`, `no_target`, `held` or `superseded`.
    pub outcome: String,
    /// Why an intermediate waits: `receipts_retired` or `receipts_missing`.
    pub detail: Option<String>,
    pub target: Option<String>,
    pub from: ActiveTarget,
    pub to: ActiveTarget,
}

/// What [`handle`] did in its transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handle {
    /// Recorded (the pending cohorts first, then this one): the step goes on
    /// to the next label.
    Done(Vec<Handled>),
    /// Verified and journaled as pending (`true`; `false`: already pending),
    /// recorded only with a later cohort's install: the step goes on to the
    /// next label.
    Defer(Pending, bool),
    /// Not handled; retried at the next pass.
    Failed(Handled),
}

fn digest_of(hex_text: &str) -> Result<Digest, StoreError> {
    let bytes: [u8; 32] =
        hex::decode(hex_text).ok().and_then(|b| b.try_into().ok()).ok_or(StoreError("pending target digest"))?;
    Ok(Digest(bytes))
}

/// The pending cohorts of the current chain record, in cohort order.
fn pending_rows(sql: &impl Sql, cfg: &super::SignConfig, floor: u64) -> Result<Vec<Pending>, StoreError> {
    let chain = chain_of(cfg);
    let restore = |d: &str, a: Option<u64>| {
        ActiveTarget::restore(&chain, digest_of(d)?, a).map_err(|_| StoreError("pending target"))
    };
    sql.exec(
        "SELECT label, cohort, opened_epoch, closed_epoch, outcome, detail, target, from_target, from_activated,
            to_target, to_activated FROM sign_auto_pending WHERE opened_epoch >= ? ORDER BY cohort",
        &[bind_u64(floor)?],
    )?
    .iter()
    .map(|r| {
        Ok(Pending {
            label: text(r, 0)?.to_owned(),
            cohort: uint(r, 1)?,
            opened_epoch: uint(r, 2)?,
            closed_epoch: uint(r, 3)?,
            outcome: text(r, 4)?.to_owned(),
            detail: opt_t(r, 5),
            target: opt_t(r, 6),
            from: restore(text(r, 7)?, opt_u(r, 8))?,
            to: restore(text(r, 9)?, opt_u(r, 10))?,
        })
    })
    .collect()
}

/// Journals `p` as pending.
fn defer(sql: &impl Sql, p: &Pending, now_ms: u64) -> Result<(), StoreError> {
    let epoch = |t: &ActiveTarget| t.activated_epoch().map_or(Ok(SqlValue::Null), bind_u64);
    sql.exec(
        "INSERT INTO sign_auto_pending (cohort, label, opened_epoch, closed_epoch, outcome, detail, target,
            from_target, from_activated, to_target, to_activated, at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            bind_u64(p.cohort)?,
            p.label.as_str().into(),
            bind_u64(p.opened_epoch)?,
            bind_u64(p.closed_epoch)?,
            p.outcome.as_str().into(),
            p.detail.as_deref().map_or(SqlValue::Null, |d| d.into()),
            p.target.as_deref().map_or(SqlValue::Null, |t| t.into()),
            hex::encode(p.from.digest().0).into(),
            epoch(&p.from)?,
            hex::encode(p.to.digest().0).into(),
            epoch(&p.to)?,
            bind_u64(now_ms)?,
        ],
    )?;
    let detail = format!("{} {} {}{}", p.cohort, p.label, p.outcome, p.detail.as_deref().map(|d| format!(" {d}")).unwrap_or_default());
    log(sql, now_ms, None, None, None, "auto_target_pending", Some(&detail))?;
    clear_failure_of(sql, &[&p.label], now_ms)
}

/// Drops every pending cohort: any install moves the held target they were
/// checked from (`signing::record_held` calls it too).
pub(crate) fn clear_pending(sql: &impl Sql) -> Result<(), StoreError> {
    sql.exec("DELETE FROM sign_auto_pending", &[])?;
    Ok(())
}

/// The failure a waiting chain of pending cohorts shows: a missing-receipts
/// intermediate first (a defect), otherwise the first pending cohort's
/// retired receipts.
fn pending_wait(sql: &impl Sql, cfg: &super::SignConfig) -> Result<Option<(String, &'static str)>, StoreError> {
    let floor = restart_floor(sql)?.unwrap_or(0);
    let pending = pending_rows(sql, cfg, floor)?;
    if let Some(p) = pending.iter().find(|p| p.detail.as_deref() == Some("receipts_missing")) {
        return Ok(Some((p.label.clone(), "receipts_missing")));
    }
    Ok(pending.first().map(|p| (p.label.clone(), "receipts_retired")))
}

/// The checkpoint generation of the installed reward-v1 authorization (1
/// when none is installed): an automatic authorization keeps it. An
/// installed bundle that does not read refuses the handoff rather than
/// guessing a generation.
fn installed_generation(sql: &impl Sql) -> Result<Result<u64, &'static str>, StoreError> {
    let Some((_, bundle)) = signer_target_bundle(sql)? else {
        return Ok(Ok(1));
    };
    const UNREADABLE: &str = "installed_generation_unreadable";
    let Ok(b) = target::parse_v2(bundle.as_bytes()) else {
        return Ok(Err(UNREADABLE));
    };
    let Ok(bytes) = hex::decode(&b.authorization.authorization) else {
        return Ok(Err(UNREADABLE));
    };
    let generation = match b.authorization.kind.as_str() {
        "launch" => Signed::<LaunchAuthorization>::from_bytes(&bytes).ok().map(|s| s.record.checkpoint_generation),
        "cohort" => Signed::<RewardIntentAuthorization>::from_bytes(&bytes).ok().map(|s| s.record.checkpoint_generation),
        _ => None,
    };
    Ok(generation.ok_or(UNREADABLE))
}

/// The recorded outcome of `cohort`'s handling, if any.
fn recorded(sql: &impl Sql, label: &str) -> Result<Option<Handled>, StoreError> {
    let rows = sql.exec(
        "SELECT cohort, outcome, target, authorization FROM sign_auto_cohorts WHERE label = ?",
        &[label.into()],
    )?;
    rows.first()
        .map(|r| {
            Ok(Handled {
                label: label.to_owned(),
                cohort: Some(uint(r, 0)?),
                outcome: Some(text(r, 1)?.to_owned()),
                target: opt_t(r, 2),
                authorization: opt_t(r, 3),
                new: false,
                error: None,
            })
        })
        .transpose()
}

/// The verified records of a closed cohort.
struct Closed {
    manifest: CohortManifestV2,
    package: ScorePackageV2,
    closure: CohortClosure,
    target: Option<CalculationTargetV2>,
    /// The target the checked closure activated.
    activation: Option<ActiveTarget>,
}

fn verify_closed(obs: &Observed, keys: &TrustedKeys, chain: &ChainBinding) -> Result<Closed, &'static str> {
    fn need(o: &Option<Vec<u8>>) -> Result<&[u8], &'static str> {
        o.as_deref().ok_or("cohort_object_missing")
    }
    let manifest = Signed::<CohortManifestV2>::open(need(&obs.manifest)?, keys).map_err(|_| "manifest_unverified")?;
    let package = Signed::<ScorePackageV2>::open(need(&obs.package)?, keys).map_err(|_| "package_unverified")?;
    let closure = Signed::<CohortClosure>::open(&obs.closure, keys).map_err(|_| "closure_unverified")?;
    if manifest.chain != *chain || package.chain != *chain || closure.chain != *chain {
        return Err("cohort_chain");
    }
    if manifest.cohort_label != obs.label {
        return Err("cohort_label");
    }
    package.check_manifest(&manifest).map_err(|_| "package_manifest")?;
    let index = PublishedIndex {
        contents: PublishedContents::from_bytes(need(&obs.published_contents)?).map_err(|_| "published_index")?,
        fingerprints: FingerprintIndex::from_bytes(need(&obs.fingerprint_index)?).map_err(|_| "published_index")?,
    };
    index.validate().map_err(|_| "published_index")?;
    if index.binding().ok() != Some(manifest.published) {
        return Err("published_index");
    }
    slop_ninja_protocol::reward_v1::check_outcome(&package, &index).map_err(|_| "outcome_differs")?;
    drop(index);
    let target = match closure.target {
        None => None,
        Some(_) => {
            let t = obs.target.as_deref().ok_or("cohort_object_missing")?;
            let t = Signed::<CalculationTargetV2>::open(t, keys).map_err(|_| "target_unverified")?;
            t.check_package(&package).map_err(|_| "target_package")?;
            Some(t)
        }
    };
    let activation = {
        let checked = closure.check(&manifest, &package, target.as_ref()).map_err(|_| "closure_check")?;
        target.as_ref().map(|_| ActiveTarget::activated(&checked)).transpose().map_err(|_| "closure_check")?
    };
    Ok(Closed { manifest, package, closure, target, activation })
}

#[allow(clippy::too_many_arguments)]
fn record(
    sql: &impl Sql,
    label: &str,
    cohort: u64,
    opened_epoch: u64,
    closed_epoch: u64,
    outcome: &str,
    target: Option<&str>,
    authorization: Option<&str>,
    now_ms: u64,
) -> Result<(), StoreError> {
    sql.exec(
        "INSERT INTO sign_auto_cohorts (label, cohort, opened_epoch, closed_epoch, outcome, target, authorization, at_ms)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            label.into(),
            bind_u64(cohort)?,
            bind_u64(opened_epoch)?,
            bind_u64(closed_epoch)?,
            outcome.into(),
            target.map_or(SqlValue::Null, |t| t.into()),
            authorization.map_or(SqlValue::Null, |a| a.into()),
            bind_u64(now_ms)?,
        ],
    )?;
    sql.exec("UPDATE sign_auto_labels SET state = 'handled' WHERE label = ?", &[label.into()])?;
    let detail = format!("{cohort} {label} {outcome}{}", target.map(|t| format!(" {t}")).unwrap_or_default());
    log(sql, now_ms, None, None, None, "auto_target_cohort", Some(&detail))?;
    clear_failure_of(sql, &[label], now_ms)
}

/// Records the pending cohorts, in order.
fn record_pending(sql: &impl Sql, pending: &[Pending], now_ms: u64) -> Result<Vec<Handled>, StoreError> {
    let mut out = Vec::with_capacity(pending.len());
    for p in pending {
        record(sql, &p.label, p.cohort, p.opened_epoch, p.closed_epoch, &p.outcome, p.target.as_deref(), None, now_ms)?;
        out.push(Handled {
            label: p.label.clone(),
            cohort: Some(p.cohort),
            outcome: Some(p.outcome.clone()),
            target: p.target.clone(),
            authorization: None,
            new: true,
            error: None,
        });
    }
    Ok(out)
}

/// The bundle JSON of a cohort's records and its signed authorization,
/// written once with each object hex-encoded in place (no intermediate
/// copies of the records): byte for byte what `serde_json` writes for the
/// same `target::BundleV2` (checked by a test).
fn bundle_json(obs: &Observed, authorization: &[u8]) -> String {
    let parts: [(&str, &[u8]); 6] = [
        ("manifest", obs.manifest.as_deref().unwrap_or_default()),
        ("package", obs.package.as_deref().unwrap_or_default()),
        ("target", obs.target.as_deref().unwrap_or_default()),
        ("closure", &obs.closure),
        ("published_contents", obs.published_contents.as_deref().unwrap_or_default()),
        ("fingerprint_index", obs.fingerprint_index.as_deref().unwrap_or_default()),
    ];
    let len = parts.iter().map(|(_, b)| 2 * b.len()).sum::<usize>() + 2 * authorization.len() + BUNDLE_FRAMING_BYTES;
    let mut out = String::with_capacity(len);
    let hex_into = |out: &mut String, b: &[u8]| {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let digit = |n: u8| char::from(DIGITS.get(usize::from(n)).copied().unwrap_or(b'0'));
        for x in b {
            out.push(digit(x >> 4));
            out.push(digit(x & 15));
        }
    };
    // The schema is a plain ASCII constant: no JSON escaping.
    out.push_str("{\"schema\":\"");
    out.push_str(target::BUNDLE_SCHEMA_V2);
    out.push_str("\",\"shortcut\":\"T12\",\"source\":{");
    for (i, (name, bytes)) in parts.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(name);
        out.push_str("\":\"");
        hex_into(&mut out, bytes);
        out.push('"');
    }
    out.push_str("},\"authorization\":{\"kind\":\"cohort\",\"authorization\":\"");
    hex_into(&mut out, authorization);
    out.push_str("\"}}");
    out
}

/// Handles one closed cohort (module docs) after the pending ones journaled
/// before it. One transaction: either everything it records (the pending
/// cohorts, this one, the authorization signed and the bundle installed),
/// this cohort journaled as pending, or nothing but a failure note.
/// Refusals are on `/status` and retried at the next pass.
#[allow(clippy::too_many_arguments)]
pub fn handle(
    sql: &impl Sql,
    cfg: &super::SignConfig,
    keys: &TrustedKeys,
    main_record: &Record,
    key_source: &impl AuthorizationKeySource,
    obs: &Observed,
    now_ms: u64,
) -> Result<Handle, StoreError> {
    let fail = |code: &'static str, cohort: Option<u64>| -> Result<Handle, StoreError> {
        note_failure(sql, &obs.label, code, now_ms)?;
        Ok(Handle::Failed(Handled::failed(&obs.label, cohort, code)))
    };
    if let Some(done) = recorded(sql, &obs.label)? {
        return Ok(Handle::Done(vec![done]));
    }
    let floor = restart_floor(sql)?.unwrap_or(0);
    let pending = pending_rows(sql, cfg, floor)?;
    // A second pass (an overlapping wakeup) reaching a cohort another one
    // journaled: it goes on past it.
    if let Some(p) = pending.iter().find(|p| p.label == obs.label) {
        return Ok(Handle::Defer(p.clone(), false));
    }
    ensure_state(sql, now_ms)?;
    if state(sql)?.paused {
        return Ok(Handle::Failed(Handled::failed(&obs.label, None, "paused")));
    }
    let row = sql.exec("SELECT state, cohort FROM sign_auto_labels WHERE label = ?", &[obs.label.as_str().into()])?;
    let Some(r) = row.first() else {
        return Ok(Handle::Failed(Handled::failed(&obs.label, None, "label_unknown")));
    };
    if text(r, 0)? != "in_record" || !main_record.admits(&obs.label) {
        return Ok(Handle::Failed(Handled::failed(&obs.label, opt_u(r, 1), "label_not_in_record")));
    }
    let listed_cohort = opt_u(r, 1);
    if obs.too_large {
        return fail("cohort_too_large", listed_cohort);
    }
    let chain = chain_of(cfg);
    let c = match verify_closed(obs, keys, &chain) {
        Ok(c) => c,
        Err(code) => return fail(code, listed_cohort),
    };
    let cohort = c.manifest.cohort;
    if listed_cohort != Some(cohort) {
        return fail("cohort_number_changed", listed_cohort);
    }
    if c.manifest.opened_epoch < floor {
        return Ok(Handle::Failed(Handled::failed(&obs.label, Some(cohort), "before_restarted_record")));
    }
    let max = handled_max(sql, floor)?.max(pending.last().map(|p| p.cohort));
    if let Some(max) = max {
        // A reordered wakeup after a later cohort was handled is harmless;
        // a second label with a handled or pending cohort's number is not.
        if cohort < max {
            return Ok(Handle::Failed(Handled::failed(&obs.label, Some(cohort), "stale")));
        }
        if cohort == max {
            return fail("cohort_number_duplicate", Some(cohort));
        }
        // Numbers continue: every one between was passed by the operator.
        if cohort - max - 1 > passed_between(sql, floor, max, cohort)? {
            return fail("cohort_gap", Some(cohort));
        }
    }
    // The pending cohorts were checked from the target held then; another
    // install since drops them and the chain starts again from the new one.
    let real_held = held_target(sql, cfg)?;
    if pending.first().is_some_and(|p| p.from != real_held) {
        clear_pending(sql)?;
        log(sql, now_ms, None, None, None, "auto_target_pending_dropped", Some(&obs.label))?;
        return fail("held_target_changed", Some(cohort));
    }
    let held = pending.last().map_or(real_held, |p| p.to.clone());
    let opened = c.manifest.opened_epoch;
    let closed = c.closure.closed_epoch;
    let pend = |outcome: &str, detail: Option<&str>, target: Option<String>, to: ActiveTarget| -> Result<Handle, StoreError> {
        let p = Pending {
            label: obs.label.clone(),
            cohort,
            opened_epoch: opened,
            closed_epoch: closed,
            outcome: outcome.to_owned(),
            detail: detail.map(str::to_owned),
            target,
            from: held.clone(),
            to,
        };
        defer(sql, &p, now_ms)?;
        Ok(Handle::Defer(p, true))
    };
    // A cohort that needs no install is recorded now, or journaled behind
    // the pending ones it follows.
    let settle = |outcome: &'static str, target: Option<String>| -> Result<Handle, StoreError> {
        if !pending.is_empty() {
            return pend(outcome, None, target, held.clone());
        }
        record(sql, &obs.label, cohort, opened, closed, outcome, target.as_deref(), None, now_ms)?;
        Ok(Handle::Done(vec![Handled {
            label: obs.label.clone(),
            cohort: Some(cohort),
            outcome: Some(outcome.into()),
            target,
            authorization: None,
            new: true,
            error: None,
        }]))
    };
    let (Some(t), Some(activation)) = (&c.target, &c.activation) else {
        return settle("no_target", None);
    };
    let digest = t.digest().map_err(|_| StoreError("target digest"))?;
    let digest_hex = digest.to_string();
    if held.digest() == digest {
        return settle("held", Some(digest_hex));
    }
    if held.activated_epoch().is_some_and(|a| closed <= a) {
        return settle("superseded", Some(digest_hex));
    }
    // From here the target replaces the held one: its predecessor must be
    // the held target and it must activate after it.
    if t.predecessor != Some(held.digest()) {
        return fail("predecessor_not_held", Some(cohort));
    }
    if held.activated_epoch().is_some_and(|a| activation.activated_epoch().is_none_or(|b| b <= a)) {
        return fail("held_target_regressed", Some(cohort));
    }
    match &obs.receipts {
        Receipts::Index(b) if b.check(&c.package).is_ok() => {}
        Receipts::Index(_) => return fail("receipt_index_mismatch", Some(cohort)),
        Receipts::Invalid => return fail("receipt_index_unverified", Some(cohort)),
        Receipts::Unread => return fail("receipts_unread", Some(cohort)),
        // Never installed on its own: journaled for a later cohort's install.
        Receipts::Retired => return pend("intermediate", Some("receipts_retired"), Some(digest_hex), activation.clone()),
        // No marker and no receipts: a retention or write-order defect,
        // reported as such (it waits the same way).
        Receipts::Missing => return pend("intermediate", Some("receipts_missing"), Some(digest_hex), activation.clone()),
    }
    let generation = match installed_generation(sql)? {
        Ok(g) => g,
        Err(code) => return fail(code, Some(cohort)),
    };
    let key = match key_source.authorization_key(keys) {
        Ok(k) => k,
        Err(code) => return fail(code, Some(cohort)),
    };
    let Some(renew_by) = closed.checked_add(u64::from(c.manifest.schedule.lease_tempos)) else {
        return fail("authorization_epoch_overflow", Some(cohort));
    };
    let authorization = RewardIntentAuthorization {
        chain: chain.clone(),
        cohort,
        cohort_manifest: c.manifest.digest().map_err(|_| StoreError("manifest digest"))?,
        score_package: c.package.digest().map_err(|_| StoreError("package digest"))?,
        target: digest,
        checkpoint_generation: generation,
        issued_epoch: closed,
        renew_by_epoch: renew_by,
    };
    let auth_digest = authorization.digest().map_err(|_| StoreError("authorization digest"))?.to_string();
    let signed = sign(authorization, &key).and_then(|s| s.to_bytes()).map_err(|_| StoreError("authorization sign"))?;
    drop(key);
    drop(c);
    let json = bundle_json(obs, &signed);
    drop(signed);
    let (installed, revoked) = match install_target_v2_from(sql, cfg, keys, &json, now_ms, &held, true)? {
        Ok(r) => r,
        Err(r) => return fail(r.code, Some(cohort)),
    };
    drop(json);
    if installed != digest_hex {
        return Err(StoreError("installed digest"));
    }
    clear_pending(sql)?;
    let mut out = record_pending(sql, &pending, now_ms)?;
    // Withdrawn by a journaled revocation: installed (its closure activated
    // it) and never re-authorized here.
    let outcome = if revoked { "revoked" } else { "installed" };
    record(sql, &obs.label, cohort, opened, closed, outcome, Some(&digest_hex), Some(&auth_digest), now_ms)?;
    if !pending.is_empty() {
        let via: Vec<String> = pending.iter().map(|p| p.cohort.to_string()).collect();
        log(sql, now_ms, None, None, None, "auto_target_via", Some(&format!("{cohort} after {}", via.join(","))))?;
    }
    out.push(Handled {
        label: obs.label.clone(),
        cohort: Some(cohort),
        outcome: Some(outcome.into()),
        target: Some(digest_hex),
        authorization: Some(auth_digest),
        new: true,
        error: None,
    });
    Ok(Handle::Done(out))
}

// -------------------------------------------------------------------- pass

/// Why one bounded object read of a step failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// The object is larger than the step's bound (it is not read).
    TooLarge,
    /// The bucket failed or the pass's deadline passed.
    Unavailable,
}

/// Everything a pass does outside this module: the journal's transactions,
/// the receipts bucket within the pass's deadline, and the clock. The
/// Worker's ChainJournal (`cf.rs`) and the native tests implement it, so
/// both run [`run_pass`] unchanged.
#[allow(async_fn_in_trait)]
pub trait PassIo {
    type Sql: Sql;
    /// Runs `f` in one transaction, rolled back when it fails.
    fn tx<T: 'static>(&self, f: impl FnOnce(&Self::Sql) -> Result<T, StoreError> + 'static) -> Result<T, StoreError>;
    /// One page of delimited prefixes under `prefix`, and the cursor that
    /// continues it.
    async fn list(&mut self, prefix: &str, cursor: Option<&str>) -> Result<(Vec<String>, Option<String>), ()>;
    /// `key`'s bytes, refused unread above `bound`.
    async fn read(&mut self, key: &str, bound: usize) -> Result<Option<Vec<u8>>, ReadError>;
    /// Whether `key` exists (a head, no body).
    async fn exists(&mut self, key: &str) -> Result<bool, ()>;
    /// Feeds `key`'s body to `f` piece by piece, holding a bounded buffer;
    /// `false`: no object.
    async fn fold(&mut self, key: &str, f: &mut dyn FnMut(&[u8])) -> Result<bool, ()>;
    fn now_ms(&self) -> u64;
    /// Whether the pass's deadline has passed.
    fn past_deadline(&self) -> bool;
}

/// What one pass did (content-free; `/sign-auto`'s reply).
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct PassReport {
    pub steps: u32,
    pub pages: u32,
    pub new_labels: u64,
    pub resolved: u32,
    pub checks: u32,
    /// Cohorts journaled as pending in this pass.
    pub deferred: u32,
    /// Labels read again because what classified them changed.
    pub requeued: u64,
    /// `paused`, `waiting` (no cohort to hand in), or `budget`.
    pub idle: Option<String>,
    /// The cohorts this pass handled or failed to.
    pub handled: Vec<Handled>,
    /// A bucket or journal failure in the pass.
    pub error: Option<String>,
    /// The authorization key's check (`None`: present and checked).
    pub key_error: Option<String>,
}

/// The step's reads of one closed cohort's records and, when it names a
/// target, its receipts.
async fn observe<I: PassIo>(io: &mut I, keys: &TrustedKeys, label: &str, closure: Vec<u8>) -> Result<Observed, ()> {
    let mut obs = Observed { label: label.to_owned(), closure, ..Default::default() };
    let mut left = SOURCE_BYTES.saturating_sub(obs.closure.len());
    let mut too_large = false;
    for (name, slot) in [
        (object::MANIFEST, &mut obs.manifest),
        (object::PACKAGE, &mut obs.package),
        (object::TARGET, &mut obs.target),
        (object::PUBLISHED_CONTENTS, &mut obs.published_contents),
        (object::FINGERPRINT_INDEX, &mut obs.fingerprint_index),
    ] {
        match io.read(&key(label, name), left).await {
            Ok(b) => {
                left = left.saturating_sub(b.as_ref().map_or(0, Vec::len));
                *slot = b;
            }
            Err(ReadError::TooLarge) => {
                too_large = true;
                break;
            }
            Err(ReadError::Unavailable) => return Err(()),
        }
    }
    if too_large {
        obs.too_large = true;
        return Ok(obs);
    }
    if obs.target.is_some() {
        obs.receipts = receipts(io, keys, label).await?;
    }
    Ok(obs)
}

/// A target cohort's receipts: the eval Worker's retirement marker first
/// (written before any receipt goes), then the chunk map, then the signed
/// index, streamed through its signature check ([`ReceiptIndexStream`]) so
/// an index of any length is checked in a fixed buffer.
async fn receipts<I: PassIo>(io: &mut I, keys: &TrustedKeys, label: &str) -> Result<Receipts, ()> {
    if io.exists(&key(label, object::RECEIPTS_RETIRED)).await? {
        return Ok(Receipts::Retired);
    }
    if !io.exists(&key(label, object::CHUNK_MAP)).await? {
        return Ok(Receipts::Missing);
    }
    let mut stream = ReceiptIndexStream::new();
    if !io.fold(&key(label, object::RECEIPT_INDEX), &mut |piece| stream.update(piece)).await? {
        return Ok(Receipts::Missing);
    }
    Ok(match stream.finish(keys) {
        Ok(b) => Receipts::Index(b),
        Err(_) => Receipts::Invalid,
    })
}

/// How a check step ended.
enum CheckEnd {
    /// At least one cohort was recorded: plan the next step.
    Progress,
    /// Nothing more to hand in this pass.
    Stop,
}

/// One bounded pass of automatic target handoff (module docs): each step is
/// planned in a transaction, its bucket operations run within the pass's
/// deadline, and its outcome is recorded in another. A closed cohort is
/// handled, signed and installed (or journaled as pending) in one
/// transaction, so a pass cut short anywhere leaves either nothing or that
/// whole step, and the next pass resumes after it. `record` is `Err` when
/// the deployment names no main record: nothing is read or handed in.
pub async fn run_pass<I: PassIo, K: AuthorizationKeySource + Clone + 'static>(
    io: &mut I,
    cfg: &super::SignConfig,
    keys: &TrustedKeys,
    record: Result<&Record, &'static str>,
    key_source: &K,
) -> PassReport {
    let mut rep = PassReport::default();
    let key_error = key_source.check_key(keys).err();
    rep.key_error = key_error.map(str::to_owned);
    let now = io.now_ms();
    if io.tx(move |sql| note_key(sql, key_error, now)).is_err() {
        rep.error = Some("store_unavailable".into());
        return rep;
    }
    let record = match record {
        Ok(r) => r,
        Err(code) => {
            rep.error = Some(code.into());
            let _ = io.tx(move |sql| note_failure(sql, "", code, now));
            return rep;
        }
    };
    let fingerprint = record.fingerprint(cfg, keys);
    let fp = fingerprint.clone();
    match io.tx(move |sql| requeue_reclassified(sql, &fp)) {
        Ok(n) => rep.requeued = n,
        Err(_) => {
            rep.error = Some("store_unavailable".into());
            return rep;
        }
    }
    let mut listed = false;
    let mut list_failed = false;
    let mut failed = false;
    loop {
        if io.past_deadline() {
            rep.idle = Some("budget".into());
            break;
        }
        let now = io.now_ms();
        let step = match io.tx(move |sql| plan_step(sql, listed, list_failed, now)) {
            Ok(s) => s,
            Err(_) => {
                rep.error = Some("store_unavailable".into());
                break;
            }
        };
        rep.steps += 1;
        let ok = match step {
            Step::Idle(reason) => {
                rep.idle = Some(reason.into());
                break;
            }
            Step::List { cursor } => match io.list(COHORT_PREFIX, cursor.as_deref()).await {
                Ok((prefixes, next)) => {
                    listed = next.is_none();
                    let now = io.now_ms();
                    io.tx(move |sql| note_page(sql, &prefixes, next.as_deref(), now)).map(|n| {
                        rep.pages += 1;
                        rep.new_labels += n;
                    })
                }
                Err(()) => {
                    list_failed = true;
                    failed = true;
                    rep.error = Some("list_unavailable".into());
                    let now = io.now_ms();
                    let continued = cursor.is_some();
                    io.tx(move |sql| note_list_failed(sql, continued, now))
                }
            },
            Step::Resolve { label } => match io.read(&key(&label, object::MANIFEST), SOURCE_BYTES).await {
                Ok(manifest) => {
                    let (c, t, r, f, now) = (cfg.clone(), keys.clone(), record.clone(), fingerprint.clone(), io.now_ms());
                    rep.resolved += 1;
                    io.tx(move |sql| note_manifest(sql, &c, &t, &r, &f, &label, manifest.as_deref(), now).map(|_| ()))
                }
                // A manifest no bundle could hold: left unsealed (read
                // again at the next listing), with the failure shown.
                Err(ReadError::TooLarge) => {
                    let (c, t, r, f, now) = (cfg.clone(), keys.clone(), record.clone(), fingerprint.clone(), io.now_ms());
                    io.tx(move |sql| {
                        note_manifest(sql, &c, &t, &r, &f, &label, None, now)?;
                        note_failure(sql, &label, "manifest_too_large", now)
                    })
                }
                Err(ReadError::Unavailable) => {
                    let budget = io.past_deadline();
                    let code = if budget { "pass_budget" } else { "object_unavailable" };
                    if budget {
                        rep.idle = Some("budget".into());
                    } else {
                        rep.error = Some(code.into());
                    }
                    let now = io.now_ms();
                    let _ = io.tx(move |sql| note_failure(sql, &label, code, now));
                    break;
                }
            },
            Step::Check { labels } => {
                rep.checks += 1;
                match check(io, cfg, keys, record, key_source, &labels, &mut rep).await {
                    Ok(CheckEnd::Progress) => Ok(()),
                    Ok(CheckEnd::Stop) => {
                        if rep.idle.is_none() && rep.error.is_none() {
                            rep.idle = Some("waiting".into());
                        }
                        break;
                    }
                    Err(e) => Err(e),
                }
            }
        };
        if ok.is_err() {
            rep.error = Some("store_unavailable".into());
            break;
        }
    }
    // A pass whose own steps all succeeded clears a failure of the pass
    // itself (a bucket binding, a listing), whatever its cohorts wait on.
    if !failed && rep.error.is_none() {
        let now = io.now_ms();
        let _ = io.tx(move |sql| clear_failure_of(sql, &[""], now));
    }
    rep
}

/// Whether this pass has recorded or journaled anything yet.
fn progressed(rep: &PassReport) -> bool {
    rep.deferred > 0 || rep.handled.iter().any(|h| h.new)
}

/// One check step (module docs): the labels in cohort order, each closed one
/// handed in, until one is not closed or cannot be handled.
async fn check<I: PassIo, K: AuthorizationKeySource + Clone + 'static>(
    io: &mut I,
    cfg: &super::SignConfig,
    keys: &TrustedKeys,
    record: &Record,
    key_source: &K,
    labels: &[String],
    rep: &mut PassReport,
) -> Result<CheckEnd, StoreError> {
    let mut step_progressed = false;
    let note = |io: &I, label: &str, code: &'static str| {
        let (label, now) = (label.to_owned(), io.now_ms());
        io.tx(move |sql| note_failure(sql, &label, code, now))
    };
    // The deadline passed (between reads, or a read raced against it): the
    // next pass resumes from the last recorded or pending cohort. A pass
    // that got nowhere says so on the cohort it was on, so a chain that
    // cannot advance within one deadline is never silent.
    let out_of_budget = |io: &I, rep: &mut PassReport, label: &str| -> Result<CheckEnd, StoreError> {
        rep.idle = Some("budget".into());
        if !progressed(rep) {
            note(io, label, "pass_budget")?;
        }
        Ok(CheckEnd::Stop)
    };
    let unavailable = |io: &I, rep: &mut PassReport, label: &str| -> Result<CheckEnd, StoreError> {
        rep.error = Some("object_unavailable".into());
        note(io, label, "object_unavailable")?;
        Ok(CheckEnd::Stop)
    };
    for (i, label) in labels.iter().enumerate() {
        if io.past_deadline() {
            return out_of_budget(io, rep, label);
        }
        let closure = match io.read(&key(label, object::CLOSURE), SOURCE_BYTES).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                let c = cfg.clone();
                if let Some((first, code)) = io.tx(move |sql| pending_wait(sql, &c))? {
                    // Pending intermediates wait for a later cohort with
                    // retained receipts to close.
                    note(io, &first, code)?;
                    return Ok(CheckEnd::Stop);
                }
                // Never past an earlier sealed cohort: show the stall when a
                // later one has closed behind it.
                let mut later_closed = false;
                for later in labels.iter().skip(i + 1) {
                    match io.exists(&key(later, object::CLOSURE)).await {
                        Ok(true) => {
                            later_closed = true;
                            break;
                        }
                        Ok(false) => {}
                        Err(()) if io.past_deadline() => return out_of_budget(io, rep, label),
                        Err(()) => return unavailable(io, rep, label),
                    }
                }
                if later_closed {
                    note(io, label, "earlier_cohort_unclosed")?;
                } else {
                    let (l, now) = (label.clone(), io.now_ms());
                    io.tx(move |sql| clear_failure_of(sql, &[&l], now))?;
                }
                return Ok(CheckEnd::Stop);
            }
            Err(ReadError::TooLarge) => {
                note(io, label, "cohort_too_large")?;
                return Ok(CheckEnd::Stop);
            }
            Err(ReadError::Unavailable) if io.past_deadline() => return out_of_budget(io, rep, label),
            Err(ReadError::Unavailable) => return unavailable(io, rep, label),
        };
        let obs = match observe(io, keys, label, closure).await {
            Ok(o) => o,
            Err(()) if io.past_deadline() => return out_of_budget(io, rep, label),
            Err(()) => return unavailable(io, rep, label),
        };
        let (c, t, r, ks, now) = (cfg.clone(), keys.clone(), record.clone(), key_source.clone(), io.now_ms());
        match io.tx(move |sql| handle(sql, &c, &t, &r, &ks, &obs, now))? {
            Handle::Done(h) => {
                rep.handled.extend(h);
                step_progressed = true;
            }
            Handle::Defer(_, new) => {
                rep.deferred += u32::from(new);
                step_progressed = true;
            }
            Handle::Failed(h) => {
                rep.handled.push(h);
                return Ok(CheckEnd::Stop);
            }
        }
    }
    let c = cfg.clone();
    if let Some((first, code)) = io.tx(move |sql| pending_wait(sql, &c))? {
        note(io, &first, code)?;
    }
    Ok(if step_progressed { CheckEnd::Progress } else { CheckEnd::Stop })
}

// ------------------------------------------------------------------ status

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct LastCohort {
    pub cohort: u64,
    pub label: String,
    pub outcome: String,
    pub target: Option<String>,
    pub authorization: Option<String>,
    pub age_ms: u64,
}

/// `/status` `signing.auto_target`: content-free.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AutoView {
    pub paused: bool,
    pub listing: bool,
    pub listed_epoch: Option<u64>,
    pub listed_age_ms: Option<u64>,
    /// Labels per state (`unresolved`, `unsealed`, `unreadable`,
    /// `in_record`, `foreign`, `outside`, `handled`, `passed`).
    pub labels: Vec<(String, u64)>,
    /// `foreign` and `outside` labels per reason (`manifest_unverified`: not
    /// signed by the trusted package key; `chain`; `label`; `authority`:
    /// outside the main record).
    pub unclassified: Vec<(String, u64)>,
    /// Pending cohorts (verified, recorded with the next install) and the
    /// first of them.
    pub pending: u64,
    pub pending_first: Option<(u64, String)>,
    /// Numbers the operator recorded as gaps (`auto_gap`).
    pub gaps: u64,
    /// The next in-record cohort waiting for its closure, and how many wait.
    pub next: Option<(u64, String)>,
    pub waiting: u64,
    pub last: Option<LastCohort>,
    pub handled: u64,
    pub error: Option<String>,
    pub error_label: Option<String>,
    pub error_age_ms: Option<u64>,
    /// The last pass's check of the authorization key's secret (`None`
    /// before the first pass): it does not hold cohorts with no target.
    pub key_ok: Option<bool>,
    pub key_error: Option<String>,
    pub key_age_ms: Option<u64>,
}

pub fn view(sql: &impl Sql, now_ms: u64) -> Result<AutoView, StoreError> {
    let st = state(sql)?;
    let labels = sql
        .exec("SELECT state, COUNT(*) FROM sign_auto_labels GROUP BY state ORDER BY state", &[])?
        .iter()
        .map(|r| Ok((text(r, 0)?.to_owned(), uint(r, 1)?)))
        .collect::<Result<Vec<_>, StoreError>>()?;
    let unclassified = sql
        .exec(
            "SELECT state || ':' || COALESCE(detail, ''), COUNT(*) FROM sign_auto_labels
             WHERE state IN ('foreign', 'outside') GROUP BY state, detail ORDER BY state, detail",
            &[],
        )?
        .iter()
        .map(|r| Ok((text(r, 0)?.to_owned(), uint(r, 1)?)))
        .collect::<Result<Vec<_>, StoreError>>()?;
    let pending_list = sql.exec("SELECT cohort, label FROM sign_auto_pending ORDER BY cohort", &[])?;
    let pending_first = pending_list.first().map(|r| Ok::<_, StoreError>((uint(r, 0)?, text(r, 1)?.to_owned()))).transpose()?;
    let gaps = sql.exec("SELECT COUNT(*) FROM sign_auto_absent", &[])?.first().map(|r| uint(r, 0)).transpose()?.unwrap_or(0);
    let waiting_rows = waiting(sql)?;
    let last = sql
        .exec(
            "SELECT cohort, label, outcome, target, authorization, at_ms FROM sign_auto_cohorts ORDER BY seq DESC LIMIT 1",
            &[],
        )?
        .first()
        .map(|r| {
            Ok::<_, StoreError>(LastCohort {
                cohort: uint(r, 0)?,
                label: text(r, 1)?.to_owned(),
                outcome: text(r, 2)?.to_owned(),
                target: opt_t(r, 3),
                authorization: opt_t(r, 4),
                age_ms: now_ms.saturating_sub(uint(r, 5)?),
            })
        })
        .transpose()?;
    let handled = sql.exec("SELECT COUNT(*) FROM sign_auto_cohorts", &[])?.first().map(|r| uint(r, 0)).transpose()?.unwrap_or(0);
    let err = sql.exec(
        "SELECT error, error_label, error_since_ms, key_error, key_checked_at_ms FROM sign_auto_state WHERE id = 1",
        &[],
    )?;
    let (error, error_label, error_age_ms, key_error, key_checked) = match err.first() {
        Some(r) => (
            opt_t(r, 0),
            opt_t(r, 1).filter(|l| !l.is_empty()),
            opt_u(r, 2).map(|t| now_ms.saturating_sub(t)),
            opt_t(r, 3),
            opt_u(r, 4),
        ),
        None => (None, None, None, None, None),
    };
    Ok(AutoView {
        paused: st.paused,
        listing: st.listing,
        listed_epoch: st.listed_epoch,
        listed_age_ms: st.listed_at_ms.map(|t| now_ms.saturating_sub(t)),
        labels,
        unclassified,
        pending: pending_list.len() as u64,
        pending_first,
        gaps,
        next: waiting_rows.first().cloned(),
        waiting: waiting_rows.len() as u64,
        last,
        handled,
        error,
        error_label,
        error_age_ms,
        key_ok: key_checked.map(|_| key_error.is_none()),
        key_error,
        key_age_ms: key_checked.map(|t| now_ms.saturating_sub(t)),
    })
}

#[cfg(test)]
#[path = "auto_tests.rs"]
mod tests;
