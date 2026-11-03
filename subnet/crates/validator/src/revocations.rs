//! This validator's journal of signed revocations.
//!
//! An authorization never expires: it yields commitments for as long as its
//! target is the held active target, until a newer authorization supersedes
//! it or a signed [`RewardIntentRevocation`] withdraws it. Each revocation
//! this validator accepts is checked under the pinned authorization key and
//! this chain, then journaled in `<journal>.revocations.json` (mode 600)
//! before it takes effect, so a restart keeps it. Entries are only ever
//! added. Every per-tempo commitment is checked against all of them
//! ([`crate::package_v2::Authorized::check_epoch`]), and the dispatch loop
//! retires queued work of a withdrawn target that never signed and withdraws
//! its signing grants, so no new attempt is signed for it.
//!
//! The signature is checked once, when an entry is journaled; [`load`] does
//! not check it again ([`RewardIntentRevocation::journaled`]). A revocation
//! can only withdraw, and only authorizations of its own chain and target, so
//! a trust root rotated since an entry was journaled never stops commitments
//! of other targets.
//!
//! An entry whose signed bytes no longer decode, or no longer match its
//! recorded digest (a hand edit or disk damage: writes are atomic), is
//! reported ([`Loaded::problems`], alerted once per distinct problem) and
//! still applied from its recorded target and epoch. That deliberately
//! withdraws a target with no verifiable signature behind it: the file is
//! this operator's own journal, an entry in it was verified when it was
//! written, and the safe direction for an entry that can no longer be read
//! is not committing its target rather than resuming commitments a
//! revocation may have stopped. It can only withdraw (its own target, at or
//! before its epoch); it never stops the others, and the operator clears it
//! by restoring the entry or removing it by hand.
//!
//! A store that cannot be read as a whole is an error here; the validator
//! keeps the revocations it last loaded and alerts once per distinct error
//! while running, and refuses to start (alerted) until the store is fixed
//! when it has none loaded yet (`live.rs`, `app.rs`).
//!
//! Revocations arrive from the operator (`slop_ninja_validator revoke`) or
//! from the fetcher, which journals every new entry of the authorization
//! pointer's revocation set in one locked write per session ([`record_set`]).
//! Read-modify-write holds `<journal>.revocations.json.lock`, so both writers
//! can run at once. The file holds only signed records, which only the
//! authorization key can produce; it grows by one entry per revoked target
//! (the set carries each target's latest) and has no other bound.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use slop_ninja_protocol::{ChainBinding, Record, RewardIntentRevocation, Signed, TrustedKeys};

pub const REVOCATIONS_SCHEMA: &str = "slop_ninja.validator_revocations/v1";

/// One journaled revocation. `signed` is the authoritative record; the other
/// fields are its decoded summary for operators.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Digest of the revocation record (hex).
    pub digest: String,
    /// The withdrawn target (hex).
    pub target: String,
    /// The authorization it names (hex).
    pub authorization: String,
    /// Withdraws authorizations of `target` issued at or before this epoch.
    pub issued_epoch: u64,
    /// The signed record (hex).
    pub signed: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    schema: String,
    entries: Vec<Entry>,
}

/// `<journal>.revocations.json`.
pub fn path(journal: &Path) -> PathBuf {
    let mut name = journal.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".revocations.json");
    journal.with_file_name(name)
}

fn lock_path(store: &Path) -> PathBuf {
    let mut name = store.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".lock");
    store.with_file_name(name)
}

/// Holds the store's lock; released on drop.
struct Lock(std::fs::File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn lock(store: &Path) -> Result<Lock> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock_path(store))
        .context("revocation store lock file")?;
    file.lock().context("revocation store lock")?;
    Ok(Lock(file))
}

fn read_file(store: &Path) -> Result<File> {
    match std::fs::read(store) {
        Ok(bytes) => {
            let file: File = serde_json::from_slice(&bytes).context("revocation store is not readable")?;
            if file.schema != REVOCATIONS_SCHEMA {
                bail!("revocation store schema {:?} is not {REVOCATIONS_SCHEMA}", file.schema);
            }
            Ok(file)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(File { schema: REVOCATIONS_SCHEMA.into(), entries: Vec::new() })
        }
        Err(e) => Err(e).context("revocation store read"),
    }
}

/// Decodes and checks one signed revocation for `chain` under `keys`.
pub fn check(bytes: &[u8], keys: &TrustedKeys, chain: &ChainBinding) -> Result<Signed<RewardIntentRevocation>> {
    let signed = Signed::<RewardIntentRevocation>::from_bytes(bytes).map_err(|e| anyhow::anyhow!("revocation: {e}"))?;
    RewardIntentRevocation::check(&signed, keys, chain).map_err(|e| anyhow::anyhow!("revocation refused: {e}"))?;
    Ok(signed)
}

/// What [`load`] read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Loaded {
    /// Every journaled revocation, as checked when it was journaled.
    pub records: Vec<RewardIntentRevocation>,
    /// Entries whose signed bytes no longer decode or match their digest
    /// (each applied from its recorded target and epoch when those parse).
    /// For an alert; never a reason to stop.
    pub problems: Vec<String>,
    /// Set when the store could not be read as a whole and `records` are
    /// the ones loaded before (never set by [`load`] itself).
    pub store_error: Option<String>,
}

/// Every journaled revocation (module docs). A missing store is empty; a
/// store that is not readable as a whole is an error.
pub fn load(store: &Path, chain: &ChainBinding) -> Result<Loaded> {
    let file = read_file(store)?;
    let mut loaded = Loaded::default();
    for e in &file.entries {
        let decoded = hex::decode(&e.signed)
            .ok()
            .and_then(|bytes| Signed::<RewardIntentRevocation>::from_bytes(&bytes).ok())
            .filter(|s| s.record.digest().is_ok_and(|d| d.to_string() == e.digest));
        match decoded {
            Some(signed) => loaded.records.push(signed.record),
            None => {
                loaded.problems.push(format!("revocation entry {} does not decode to its digest", e.digest));
                // Applied from its recorded summary: it can only withdraw.
                let digest = |h: &str| -> Option<slop_ninja_protocol::Digest> {
                    hex::decode(h).ok()?.try_into().ok().map(slop_ninja_protocol::Digest)
                };
                if let (Some(target), Some(authorization)) = (digest(&e.target), digest(&e.authorization)) {
                    loaded.records.push(RewardIntentRevocation {
                        chain: chain.clone(),
                        target,
                        authorization,
                        checkpoint_generation: 0,
                        issued_epoch: e.issued_epoch,
                    });
                }
            }
        }
    }
    Ok(loaded)
}

/// The journaled entries, unchecked (for reports).
pub fn entries(store: &Path) -> Result<Vec<Entry>> {
    Ok(read_file(store)?.entries)
}

/// What [`record`] did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Recorded {
    pub entry: Entry,
    /// `false` when the store already held it.
    pub new: bool,
}

/// Checks `bytes` as a signed revocation for `chain` under `keys` and
/// journals it (durably, before returning) unless the store already holds it.
pub fn record(store: &Path, keys: &TrustedKeys, chain: &ChainBinding, bytes: &[u8]) -> Result<Recorded> {
    let signed = check(bytes, keys, chain)?;
    let r = &signed.record;
    let entry = Entry {
        digest: r.digest().map_err(|e| anyhow::anyhow!("revocation digest: {e}"))?.to_string(),
        target: hex::encode(r.target.0),
        authorization: hex::encode(r.authorization.0),
        issued_epoch: r.issued_epoch,
        signed: hex::encode(signed.to_bytes().map_err(|e| anyhow::anyhow!("revocation bytes: {e}"))?),
    };
    let _lock = lock(store)?;
    let mut file = read_file(store)?;
    if file.entries.iter().any(|e| e.digest == entry.digest) {
        return Ok(Recorded { entry, new: false });
    }
    file.entries.push(entry.clone());
    crate::supersede::write_private_atomic(store, &serde_json::to_vec_pretty(&file)?)?;
    Ok(Recorded { entry, new: true })
}

/// What [`record_set`] did with a revocation set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordedSet {
    /// Entries journaled by this call.
    pub new: Vec<Entry>,
    /// Entries the store already held (not verified again).
    pub known: usize,
    /// Entries that do not decode or verify under `keys` for `chain`.
    pub skipped: usize,
}

/// Journals every entry of a revocation set (each `len` bytes) that the
/// store does not hold yet, after checking it under `keys` for `chain`, in
/// one locked read and at most one durable write; entries already
/// journaled (by digest) are not verified again, and entries that do not
/// verify are counted and skipped. With `store` `None`, checks the new
/// entries without keeping them.
pub fn record_set(
    store: Option<&Path>,
    keys: &TrustedKeys,
    chain: &ChainBinding,
    set: &[u8],
    len: usize,
) -> Result<RecordedSet> {
    if len == 0 || !set.len().is_multiple_of(len) {
        bail!("the revocation set is not whole revocations");
    }
    let _lock = store.map(lock).transpose()?;
    let mut file = match store {
        Some(store) => read_file(store)?,
        None => File { schema: REVOCATIONS_SCHEMA.into(), entries: Vec::new() },
    };
    let mut known: std::collections::BTreeSet<String> = file.entries.iter().map(|e| e.digest.clone()).collect();
    let mut out = RecordedSet::default();
    for bytes in set.chunks_exact(len) {
        let Ok(signed) = Signed::<RewardIntentRevocation>::from_bytes(bytes) else {
            out.skipped += 1;
            continue;
        };
        let Ok(digest) = signed.record.digest().map(|d| d.to_string()) else {
            out.skipped += 1;
            continue;
        };
        if known.contains(&digest) {
            out.known += 1;
            continue;
        }
        if RewardIntentRevocation::check(&signed, keys, chain).is_err() {
            out.skipped += 1;
            continue;
        }
        let r = &signed.record;
        let entry = Entry {
            digest: digest.clone(),
            target: hex::encode(r.target.0),
            authorization: hex::encode(r.authorization.0),
            issued_epoch: r.issued_epoch,
            signed: hex::encode(bytes),
        };
        known.insert(digest);
        file.entries.push(entry.clone());
        out.new.push(entry);
    }
    if let Some(store) = store
        && !out.new.is_empty()
    {
        crate::supersede::write_private_atomic(store, &serde_json::to_vec_pretty(&file)?)?;
    }
    Ok(out)
}
