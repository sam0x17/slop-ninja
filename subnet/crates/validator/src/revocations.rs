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
//! treated as a revocation still in force: it is reported
//! ([`Loaded::problems`], alerted once per distinct problem) and applied
//! from what it still records. That deliberately withdraws authorizations
//! with no verifiable signature behind it: the file is this operator's own
//! journal, an entry in it was verified when it was written, and the safe
//! direction for an entry that can no longer be read is not committing
//! rather than resuming commitments a revocation may have stopped.
//!
//! - When its recorded target parses, it withdraws that target's
//!   authorizations issued at or before its recorded epoch, as a readable
//!   entry would. Its recorded authorization is an audit binding only
//!   (withdrawal never consults it), so it need not parse.
//! - When its recorded target does not parse, the target it withdrew cannot
//!   be recovered, so it could have withdrawn any target: it withdraws every
//!   authorization of this chain issued at or before its recorded epoch,
//!   whatever its target ([`Loaded::unrecovered`], applied to each installed
//!   or staged authorization by
//!   [`crate::package_v2::Authorized::set_revocations`]). Only an
//!   authorization issued after that epoch commits.
//! - Signed bytes that still decode, but not to the recorded digest, are
//!   applied as well, so whichever of the two was damaged, both withdraw.
//!
//! Either way it only withdraws; queued work that never signed is retired,
//! and commitments already signed, and their reveals, stay with recovery.
//! The operator clears it by restoring the entry or removing it by hand.
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
use slop_ninja_protocol::{ChainBinding, Digest, Record, RewardIntentRevocation, Signed, TrustedKeys};

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
    /// Every journaled revocation, as checked when it was journaled, and
    /// each unreadable entry's recorded or decoded revocation (module docs).
    pub records: Vec<RewardIntentRevocation>,
    /// Unreadable entries whose withdrawn target cannot be recovered: each
    /// withdraws every authorization issued at or before its epoch, whatever
    /// its target (module docs).
    pub unrecovered: Vec<Unrecovered>,
    /// Entries whose signed bytes no longer decode or match their digest,
    /// each still in force (module docs). For an alert.
    pub problems: Vec<String>,
    /// Set when the store could not be read as a whole and `records` and
    /// `unrecovered` are the ones loaded before (never set by [`load`]
    /// itself).
    pub store_error: Option<String>,
}

/// An unreadable revocation entry whose recorded target does not parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unrecovered {
    /// The entry's recorded digest, as the store holds it (it may not be hex).
    pub entry: String,
    /// Withdraws every authorization issued at or before this epoch: the
    /// entry's recorded epoch, or its decoded record's when that is later.
    pub issued_epoch: u64,
}

impl Unrecovered {
    /// Its withdrawal of an authorization of `target` on `chain`, whose
    /// record names `authorization`: the same chain and target, so the
    /// authorization is withdrawn exactly when it was issued at or before
    /// [`Self::issued_epoch`].
    pub fn against(&self, chain: &ChainBinding, target: Digest, authorization: Digest) -> RewardIntentRevocation {
        RewardIntentRevocation {
            chain: chain.clone(),
            target,
            authorization,
            checkpoint_generation: 0,
            issued_epoch: self.issued_epoch,
        }
    }
}

/// Every journaled revocation (module docs). A missing store is empty; a
/// store that is not readable as a whole is an error.
pub fn load(store: &Path, chain: &ChainBinding) -> Result<Loaded> {
    let file = read_file(store)?;
    let mut loaded = Loaded::default();
    let digest = |h: &str| -> Option<Digest> { hex::decode(h).ok()?.try_into().ok().map(Digest) };
    for e in &file.entries {
        let decoded =
            hex::decode(&e.signed).ok().and_then(|bytes| Signed::<RewardIntentRevocation>::from_bytes(&bytes).ok());
        let decoded = match decoded {
            Some(signed) if signed.record.digest().is_ok_and(|d| d.to_string() == e.digest) => {
                loaded.records.push(signed.record);
                continue;
            }
            other => other.map(|s| s.record),
        };
        // Unreadable: still in force, from what it records.
        let issued_epoch = decoded.as_ref().map_or(e.issued_epoch, |r| r.issued_epoch.max(e.issued_epoch));
        match digest(&e.target) {
            Some(target) => {
                loaded.problems.push(format!(
                    "revocation entry {} does not decode to its digest; in force from its recorded target {} at or before epoch {} until the entry is restored or removed",
                    e.digest, e.target, e.issued_epoch
                ));
                loaded.records.push(RewardIntentRevocation {
                    chain: chain.clone(),
                    target,
                    authorization: digest(&e.authorization).unwrap_or(Digest([0; 32])),
                    checkpoint_generation: 0,
                    issued_epoch: e.issued_epoch,
                });
            }
            None => {
                loaded.problems.push(format!(
                    "revocation entry {} does not decode to its digest and its recorded target does not parse; \
                     every authorization issued at or before epoch {issued_epoch} is withdrawn, whatever its target, \
                     until the entry is restored or removed",
                    e.digest
                ));
                loaded.unrecovered.push(Unrecovered { entry: e.digest.clone(), issued_epoch });
            }
        }
        loaded.records.extend(decoded);
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
