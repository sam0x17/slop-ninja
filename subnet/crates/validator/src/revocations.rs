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
//! Revocations arrive from the operator (`slop_ninja_validator revoke`) or
//! from the fetcher, when the authorization pointer names one. Read-modify-
//! write holds `<journal>.revocations.json.lock`, so both writers can run at
//! once. The file holds only signed records, which only the authorization key
//! can produce; it grows by one entry per revocation and has no other bound.

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

/// Every journaled revocation, each checked again under `keys` and `chain`.
/// A missing store is empty. An entry that fails its check is an error: the
/// store is written only with checked records, so a failure means the trust
/// roots changed or the file was altered, and the operator must look.
pub fn load(store: &Path, keys: &TrustedKeys, chain: &ChainBinding) -> Result<Vec<Signed<RewardIntentRevocation>>> {
    let file = read_file(store)?;
    file.entries
        .iter()
        .map(|e| {
            let bytes = hex::decode(&e.signed).context("revocation entry is not hex")?;
            check(&bytes, keys, chain)
        })
        .collect()
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
