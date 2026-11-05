//! Submission openings and the local state directory.
//!
//! An opening holds the secret nonce, so it is written only to files of mode
//! 0600 in directories of mode 0700, created exclusively and renamed into
//! place after `fsync`. Loading refuses a file that group or others can read.
//! Each commitment gets its own file, named by its commitment digest; an
//! opening is never rewritten with another nonce.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use slop_ninja_protocol::submission::{SubmissionOpening, SubmissionUpload};
use slop_ninja_protocol::{AccountId, Canonical, ChainBinding, Decoder, Digest, Encoder, Nonce};

use crate::object::ObjectHeader;

/// The opening file schema this client writes: it names the object the
/// opening was made for by its header's SHA-256 and length.
pub const OPENING_SCHEMA: &str = "slop_ninja.miner.opening/v2";
/// The earlier schema, which named the object by a whole-object SHA-256 (a
/// second hash over the same bytes, no longer computed). Such a file still
/// loads: it matches an object by the content its opening commits to, and
/// `upload` rewrites it as v2 once it has read that object's header.
pub const OPENING_SCHEMA_V1: &str = "slop_ninja.miner.opening/v1";
pub const UPLOAD_STATE_SCHEMA: &str = "slop_ninja.miner.upload/v2";

/// A source of secret nonces.
pub trait NonceSource {
    fn fill(&mut self, out: &mut [u8; 32]) -> Result<()>;
}

/// The operating system's random source.
pub struct OsNonce;

impl NonceSource for OsNonce {
    fn fill(&mut self, out: &mut [u8; 32]) -> Result<()> {
        getrandom::getrandom(out).map_err(|error| anyhow::anyhow!("random source: {error}"))
    }
}

/// A fresh 256-bit nonce. All-zero output means a broken source.
pub fn fresh_nonce(source: &mut dyn NonceSource) -> Result<Nonce> {
    let mut bytes = [0u8; 32];
    source.fill(&mut bytes)?;
    ensure!(bytes != [0u8; 32], "the random source returned zeros");
    Ok(Nonce(bytes))
}

/// Builds the opening for an object: it commits to the content the
/// object's header derives.
pub fn make_opening(
    chain: &ChainBinding,
    cohort: u64,
    hotkey: AccountId,
    registration_block: u64,
    object: &ObjectHeader,
    source: &mut dyn NonceSource,
) -> Result<SubmissionOpening> {
    Ok(SubmissionOpening {
        chain: chain.clone(),
        cohort,
        hotkey,
        registration_block,
        content: object.content,
        nonce: fresh_nonce(source)?,
    })
}

pub fn encode_opening(opening: &SubmissionOpening) -> Vec<u8> {
    let mut e = Encoder::new();
    opening.encode_to(&mut e);
    e.into_bytes()
}

pub fn decode_opening(bytes: &[u8]) -> Result<SubmissionOpening> {
    let mut d = Decoder::new(bytes);
    let opening = SubmissionOpening::decode_from(&mut d)
        .map_err(|error| anyhow::anyhow!("opening does not decode: {error:?}"))?;
    d.finish()
        .map_err(|error| anyhow::anyhow!("opening has trailing bytes: {error:?}"))?;
    Ok(opening)
}

/// The upload envelope for an opening and its object: the opening, its
/// commitment, and the header's SHA-256 and length (the header digest was
/// taken when the header was read; nothing is hashed here).
pub fn upload_envelope(
    opening: SubmissionOpening,
    object: &ObjectHeader,
) -> Result<SubmissionUpload> {
    ensure!(
        object.content == opening.content,
        "the artifact's inference content is not the committed content"
    );
    let upload = SubmissionUpload {
        commitment: opening.commitment(),
        opening,
        header_sha256: object.sha256,
        header_len: object.len,
    };
    slop_ninja_protocol::Record::validate(&upload)
        .map_err(|error| anyhow::anyhow!("upload envelope: {error:?}"))?;
    Ok(upload)
}

/// Chain submission record of an opening's commitment.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submitted {
    pub extrinsic_hash: Option<String>,
    pub block_hash: Option<String>,
    pub extrinsic_id: Option<String>,
    /// The block of the commitment read back from finalized state.
    pub finalized_commitment_block: Option<u64>,
}

/// One stored opening. `opening` is the canonical hex; the other fields
/// repeat it for people and are checked against it on load.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpeningFile {
    pub schema: String,
    pub opening: String,
    pub commitment: String,
    pub chain_payload: String,
    pub cohort: u64,
    pub netuid: u16,
    pub hotkey: String,
    pub registration_block: u64,
    pub content: String,
    /// SHA-256 of the header bytes of the object the opening was made for
    /// (absent in a v1 file until `upload` migrates it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_sha256: Option<String>,
    /// Its header length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_len: Option<u32>,
    /// The object's whole length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_len: Option<u64>,
    #[serde(default)]
    pub submitted: Option<Submitted>,
}

impl OpeningFile {
    pub fn new(opening: &SubmissionOpening, object: &ObjectHeader, hotkey_ss58: &str) -> Self {
        Self {
            schema: OPENING_SCHEMA.into(),
            opening: hex::encode(encode_opening(opening)),
            commitment: opening.commitment().hex(),
            chain_payload: hex::encode(opening.chain_payload()),
            cohort: opening.cohort,
            netuid: opening.chain.netuid,
            hotkey: hotkey_ss58.into(),
            registration_block: opening.registration_block,
            content: opening.content.hex(),
            header_sha256: Some(object.sha256.hex()),
            header_len: Some(object.len),
            object_len: Some(object.object_len),
            submitted: None,
        }
    }

    /// Whether this opening was made for `object`: by its header digest and
    /// length (v2), or, for a v1 file that names none, by the content its
    /// opening commits to (which binds every inference file).
    pub fn fits(&self, object: &ObjectHeader) -> bool {
        match (&self.header_sha256, self.header_len) {
            (Some(sha), Some(len)) => *sha == object.sha256.hex() && len == object.len,
            _ => self.content == object.content.hex(),
        }
    }

    /// The v2 record of a v1 file whose opening commits to `object`'s
    /// content: the header fields filled in; `None` when there is nothing to
    /// migrate.
    pub fn migrated(&self, object: &ObjectHeader) -> Option<Self> {
        if self.schema == OPENING_SCHEMA || self.content != object.content.hex() {
            return None;
        }
        Some(Self {
            schema: OPENING_SCHEMA.into(),
            header_sha256: Some(object.sha256.hex()),
            header_len: Some(object.len),
            object_len: Some(object.object_len),
            ..self.clone()
        })
    }

    /// The opening, checked against every repeated field.
    pub fn opening(&self) -> Result<SubmissionOpening> {
        ensure!(
            self.schema == OPENING_SCHEMA || self.schema == OPENING_SCHEMA_V1,
            "unknown opening schema {:?}",
            self.schema
        );
        let opening = decode_opening(&hex::decode(&self.opening).context("opening is not hex")?)?;
        ensure!(
            opening.commitment().hex() == self.commitment
                && hex::encode(opening.chain_payload()) == self.chain_payload
                && opening.cohort == self.cohort
                && opening.chain.netuid == self.netuid
                && opening.registration_block == self.registration_block
                && opening.content.hex() == self.content,
            "opening file fields do not match its opening"
        );
        Ok(opening)
    }

    pub fn commitment(&self) -> Result<Digest> {
        Ok(self.opening()?.commitment())
    }
}

/// The miner's state directory: `openings/` and `uploads/`.
#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Opens (creating, mode 0700) the state directory.
    pub fn open(root: &Path) -> Result<Self> {
        for dir in [
            root.to_path_buf(),
            root.join("openings"),
            root.join("uploads"),
        ] {
            make_private_dir(&dir)?;
        }
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn opening_path(
        &self,
        netuid: u16,
        cohort: u64,
        hotkey_ss58: &str,
        commitment: &Digest,
    ) -> PathBuf {
        self.root.join("openings").join(format!(
            "{netuid}-{cohort}-{hotkey_ss58}-{}.json",
            commitment.hex()
        ))
    }

    pub fn upload_state_path(&self, session: &str) -> PathBuf {
        self.root.join("uploads").join(format!("{session}.json"))
    }

    /// Stores a new opening. Refuses to replace an existing file.
    pub fn save_new_opening(&self, file: &OpeningFile) -> Result<PathBuf> {
        let opening = file.opening()?;
        let path = self.opening_path(
            opening.chain.netuid,
            opening.cohort,
            &file.hotkey,
            &opening.commitment(),
        );
        ensure!(!path.exists(), "{} already exists", path.display());
        write_private(&path, &serde_json::to_vec_pretty(file)?, false)?;
        Ok(path)
    }

    /// Rewrites an opening file in place (for its submission record, or its
    /// migration to the v2 schema). The opening itself must be unchanged.
    pub fn update_opening(&self, path: &Path, file: &OpeningFile) -> Result<()> {
        let stored = load_opening(path)?;
        ensure!(
            stored.opening == file.opening,
            "an opening file's opening never changes"
        );
        file.opening()?;
        write_private(path, &serde_json::to_vec_pretty(file)?, true)
    }

    /// Every stored opening for `(netuid, cohort, hotkey)`.
    pub fn openings_for(
        &self,
        netuid: u16,
        cohort: u64,
        hotkey_ss58: &str,
    ) -> Result<Vec<(PathBuf, OpeningFile)>> {
        let prefix = format!("{netuid}-{cohort}-{hotkey_ss58}-");
        let mut out = Vec::new();
        for entry in fs::read_dir(self.root.join("openings"))? {
            let path = entry?.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if name.starts_with(&prefix) && name.ends_with(".json") {
                out.push((path.clone(), load_opening(&path)?));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Every stored opening.
    pub fn all_openings(&self) -> Result<Vec<(PathBuf, OpeningFile)>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(self.root.join("openings"))? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                out.push((path.clone(), load_opening(&path)?));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }
}

/// Loads an opening file, refusing one that group or others can access.
pub fn load_opening(path: &Path) -> Result<OpeningFile> {
    check_private(path)?;
    let file: OpeningFile = serde_json::from_slice(&fs::read(path)?)
        .with_context(|| format!("{} is not an opening file", path.display()))?;
    file.opening()?;
    Ok(file)
}

fn make_private_dir(dir: &Path) -> Result<()> {
    if !dir.exists() {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    let mode = fs::metadata(dir)?.permissions().mode();
    if mode & 0o077 != 0 {
        bail!(
            "{} is accessible to group or others (mode {:o}); run chmod 700 on it",
            dir.display(),
            mode & 0o777
        );
    }
    Ok(())
}

pub(crate) fn check_private(path: &Path) -> Result<()> {
    let mode = fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        bail!(
            "{} is accessible to group or others (mode {:o}); run chmod 600 on it",
            path.display(),
            mode & 0o777
        );
    }
    Ok(())
}

/// Writes `bytes` to `path` with mode 0600: a fresh temporary file, `fsync`,
/// then rename. Without `replace`, an existing `path` is an error.
pub(crate) fn write_private(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    if !replace && path.exists() {
        bail!("{} already exists", path.display());
    }
    let mut name = path
        .file_name()
        .context("path has no file name")?
        .to_os_string();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    let _ = fs::remove_file(&tmp);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("create {}", tmp.display()))?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(error) = written {
        let _ = fs::remove_file(&tmp);
        return Err(error.into());
    }
    if !replace && path.exists() {
        let _ = fs::remove_file(&tmp);
        bail!("{} already exists", path.display());
    }
    fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    if let Some(parent) = path.parent()
        && let Ok(dir) = fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// The default state directory: `$SLOP_NINJA_MINER_HOME`, else the platform's
/// data directory (`$XDG_DATA_HOME` or `~/.local/share` on Linux,
/// `~/Library/Application Support` on macOS) with `slop_ninja_miner`.
pub fn default_state_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("SLOP_NINJA_MINER_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .context("HOME is not set; pass --state-dir")?;
    let base = if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("share"))
    };
    Ok(base.join("slop_ninja_miner"))
}
