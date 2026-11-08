//! Each hotkey's admission state in one cohort, for the status
//! (`admission.cohorts[].hotkeys`, rule [`HOTKEYS_RULE`]): what the
//! publisher's per-cohort roster documents read (`dashboard/v1/m/a/
//! rosters/<label>.json`, status API `miners`).
//!
//! Content-free. A row names a hotkey (64 lowercase hex), its UID, the
//! block of its logged commitment inside the cohort's admission window
//! (chain data), its state and, for a refused session, a closed refusal
//! class ([`refusal_class`]: never the code, message or detail). Once the
//! roster froze, an admitted row also carries its 1-based roster position
//! (the cohort's `(commitment block, extrinsic)` order, which the chain
//! already shows); the publisher shows positions only after the cohort
//! closes. No session id, digest, object key, size, mint count, timestamp,
//! verdict try or request refusal leaves this function.
//!
//! - **Rows.** Every hotkey of the UID map at the stored tempo boundary
//!   (`commitments::uid_map`, at most [`UID_MAP_ROWS`]) and every hotkey
//!   with a session of the cohort (its latest session that was not
//!   superseded), each once: at most [`HOTKEY_ROWS`], refused whole
//!   (`over_bound`) rather than truncated, counted before any session row
//!   is read. A session hotkey outside the map takes the UID of its counted
//!   commitment when no map row holds that UID, else none.
//! - **`committed_block`.** The hotkey's counted commitment (its latest
//!   while the window admits, its last before the cutoff once closed: the
//!   one admission checks) when it was made at or after the window's
//!   opening block; `null` otherwise. The commitment value is a digest, so
//!   whether it opens this cohort is known only from a session.
//! - **`state`.** The latest session's `uploading`, `verifying`,
//!   `verified`, `refused`, `admitted` or `overflow`; without a session
//!   `committed` (a `committed_block`) or `none`.
//! - **Freshness.** The map and `epoch`/`block` are the stored boundary's;
//!   commitment blocks and session states are read with the status.

use crate::eval::commitments::{self, UID_MAP_ROWS};
use crate::eval::engine::{R, i, opt_int, opt_text, s};
use crate::sql::{Sql, bind_u64, text};
use serde_json::{Value, json};
use slop_ninja_admission_wire::ErrorCode;
use std::collections::{BTreeMap, BTreeSet};

/// The rule of a cohort's `hotkeys` in the admission status.
pub const HOTKEYS_RULE: &str = "slop_ninja.eval.admission_hotkeys/v1";

/// The most rows: every UID of the map ([`UID_MAP_ROWS`]) plus as many
/// hotkeys outside it (a session hotkey whose UID changed hands, or that
/// registered after the boundary). Equal to the dashboard contract's
/// `roster::ROWS`.
pub const HOTKEY_ROWS: usize = 2 * UID_MAP_ROWS;

/// The closed refusal classes, in the publisher's order.
pub const REFUSAL_CLASSES: &[&str] = &["commitment", "artifact", "cutoff", "registration"];

/// The public class of a session refusal code; `None` for a code that only
/// ever answers a request (it never ends a session).
pub fn refusal_class(code: ErrorCode) -> Option<&'static str> {
    use ErrorCode::*;
    Some(match code {
        CommitmentOverwritten | CommitmentMismatch | CommitmentNotFound | CommitmentUnresolvable
        | CommitmentBeforeOpen | WrongChain | WrongCohort => "commitment",
        ObjectMismatch | ArtifactInvalid | ContractMismatch | ContentMismatch | CheckFailed | ObjectReplaced
        | HeaderTooLarge | SecondArtifact | HeaderMissing | ObjectsMissing => "artifact",
        IncompleteAtCutoff | AfterCutoff | NotAdmitting => "cutoff",
        NotRegistered | RegistrationMismatch | HotkeyMismatch => "registration",
        BadRequest | UnsupportedMediaType | BadSignature | RequestExpired | RequestValidityTooLong
        | ContentDigestMismatch | Forbidden | NotFound | SessionNotUploading | NotNeeded | RateLimited | Paused
        | Unavailable | Internal => return None,
    })
}

/// The admission cohort a `hotkeys` report describes.
pub(super) struct CohortRef {
    pub seq: i64,
    pub cohort: u64,
    pub open_block: u64,
    pub cutoff_block: Option<u64>,
    pub frozen: bool,
}

/// The stored tempo boundary `(epoch, block)` and the UID map there, read
/// once for every report of one status.
pub(super) struct Boundary {
    pub epoch: u64,
    pub block: u64,
    pub map: Value,
}

/// The stored boundary, `None` before one is observed.
pub(super) fn boundary(sql: &impl Sql) -> R<Option<Boundary>> {
    let obs = sql.exec("SELECT epoch_index, last_epoch_block FROM eval_obs WHERE id = 1", &[])?;
    let at = obs.first().and_then(|r| {
        let e = u64::try_from(opt_int(r, 0)?).ok()?;
        let b = u64::try_from(opt_int(r, 1)?).ok()?;
        Some((e, b))
    });
    let Some((epoch, block)) = at else {
        return Ok(None);
    };
    Ok(Some(Boundary { epoch, block, map: commitments::uid_map(sql, epoch, block)? }))
}

/// One row before its commitment is read.
#[derive(Default)]
struct Entry {
    uid: Option<u16>,
    /// The latest session's state and, when refused, its stored code.
    session: Option<(String, Option<String>)>,
    /// The admitted session's commitment position.
    position: Option<(i64, i64)>,
}

/// The cohort's `hotkeys` report at boundary `b`.
pub(super) fn report(sql: &impl Sql, c: &CohortRef, b: &Boundary) -> R<Value> {
    let refused = |why: &str| {
        json!({"rule": HOTKEYS_RULE, "cohort": c.cohort, "epoch": b.epoch, "block": b.block, "rows": null, "refused": why})
    };
    let Some(map) = b.map.get("rows").and_then(Value::as_array) else {
        let why = b.map.get("refused").and_then(Value::as_str).unwrap_or("uid_map");
        return Ok(refused(&format!("uid_map_{why}")));
    };
    let sessions = super::count(sql, "SELECT COUNT(DISTINCT hotkey) FROM adm_sessions WHERE seq = ?", &[i(c.seq)])?;
    if usize::try_from(sessions).ok().is_none_or(|n| n > HOTKEY_ROWS) {
        return Ok(refused("over_bound"));
    }
    let mut rows: BTreeMap<String, Entry> = BTreeMap::new();
    let mut uids = BTreeSet::new();
    for r in map {
        let (Some(hotkey), Some(uid)) =
            (r.get("hotkey").and_then(Value::as_str), r.get("uid").and_then(Value::as_u64).and_then(|u| u16::try_from(u).ok()))
        else {
            return Ok(refused("uid_map_invalid_log"));
        };
        uids.insert(uid);
        rows.insert(hotkey.to_owned(), Entry { uid: Some(uid), ..Entry::default() });
    }
    // The latest session of each hotkey that a later one did not replace.
    let latest = sql.exec(
        "SELECT hotkey, state, code, pos_block, pos_extrinsic FROM (
             SELECT hotkey, state, json_extract(refusal, '$.code') AS code, pos_block, pos_extrinsic,
                 ROW_NUMBER() OVER (PARTITION BY hotkey ORDER BY created_ms DESC, session DESC) AS n
             FROM adm_sessions WHERE seq = ? AND state <> 'superseded')
         WHERE n = 1",
        &[i(c.seq)],
    )?;
    for r in &latest {
        let e = rows.entry(text(r, 0)?.to_owned()).or_default();
        e.session = Some((text(r, 1)?.to_owned(), opt_text(r, 2)));
        e.position = opt_int(r, 3).zip(opt_int(r, 4));
    }
    if rows.len() > HOTKEY_ROWS {
        return Ok(refused("over_bound"));
    }
    // Roster positions: the admitted rows in commitment order.
    let mut admitted: Vec<((i64, i64), &str)> = rows
        .iter()
        .filter(|(_, e)| c.frozen && e.session.as_ref().is_some_and(|(st, _)| st == "admitted"))
        .filter_map(|(k, e)| Some((e.position?, k.as_str())))
        .collect();
    admitted.sort();
    let position: BTreeMap<&str, usize> = admitted.iter().enumerate().map(|(n, (_, k))| (*k, n + 1)).collect();
    // Below the cutoff once closed; any later block while admitting.
    let upper = match c.cutoff_block {
        Some(b) => bind_u64(b)?,
        None => bind_u64(crate::wire::MAX_SAFE_INTEGER)?,
    };
    let mut out: Vec<(Option<u16>, String, Value)> = Vec::with_capacity(rows.len());
    for (hotkey, e) in &rows {
        let counted = sql.exec(
            "SELECT block, uid FROM commit_log WHERE hotkey = ? AND block >= ? AND block < ? ORDER BY block DESC LIMIT 1",
            &[s(hotkey), bind_u64(c.open_block)?, upper.clone()],
        )?;
        let committed_block = counted.first().and_then(|r| opt_int(r, 0)).and_then(|b| u64::try_from(b).ok());
        let uid = e.uid.or_else(|| {
            let u = counted.first().and_then(|r| opt_int(r, 1)).and_then(|u| u16::try_from(u).ok())?;
            uids.insert(u).then_some(u)
        });
        let (state, class) = match &e.session {
            Some((st, code)) if st == "refused" => {
                let class = code.as_deref().and_then(ErrorCode::parse).and_then(refusal_class);
                let Some(class) = class else {
                    return Ok(refused("unclassified_refusal"));
                };
                ("refused", Some(class))
            }
            Some((st, _)) => match st.as_str() {
                "uploading" => ("uploading", None),
                "verifying" => ("verifying", None),
                "verified" => ("verified", None),
                "admitted" => ("admitted", None),
                "overflow" => ("overflow", None),
                _ => return Ok(refused("unknown_session_state")),
            },
            None if committed_block.is_some() => ("committed", None),
            None => ("none", None),
        };
        let row = json!({"hotkey": hotkey, "uid": uid, "committed_block": committed_block, "state": state,
            "refusal_class": class, "roster_position": position.get(hotkey.as_str())});
        out.push((uid, hotkey.clone(), row));
    }
    // UID order (rows without a UID last), then hotkey: never roster order.
    out.sort_by(|a, b| (a.0.is_none(), a.0, &a.1).cmp(&(b.0.is_none(), b.0, &b.1)));
    let rows: Vec<Value> = out.into_iter().map(|(_, _, v)| v).collect();
    Ok(json!({"rule": HOTKEYS_RULE, "cohort": c.cohort, "epoch": b.epoch, "block": b.block, "rows": rows}))
}
