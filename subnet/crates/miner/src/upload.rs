//! Resumable direct upload of one artifact object (admission contract v2).
//!
//! The session id is the envelope's digest, so a restarted upload finds its
//! session without any local state. The flow (`docs/testnet/admission-api.md`
//! sections 5 and 6):
//!
//! 1. open (or find) the session; while its header is not recorded, the
//!    answer carries a fresh header URL;
//! 2. PUT the header bytes `H` straight to R2;
//! 3. ask `urls`: the coordinator checks the header (R2's stored checksum,
//!    the spec's file table, the committed content) and answers one URL per
//!    file it has not recorded;
//! 4. PUT each missing file straight to R2, largest first;
//! 5. `complete`: the coordinator heads every object and queues the
//!    admission run, or lists what is still missing;
//! 6. follow the session while it verifies.
//!
//! **Retries are per object** (a single PUT cannot resume): a transport
//! error, a 5xx, 429 or an expired URL gets a fresh URL and resends that
//! object from byte 0 (doubling backoff, except after expiry); 412 means R2
//! already holds it; `BadDigest`, `SignatureDoesNotMatch` and 501 stop the
//! run. R2 checks expiry when a PUT starts, so a transfer in flight is never
//! cut by it, at any rate. Every URL request mints new URLs: expiry never
//! ends a session.
//!
//! **Resume is per object.** The coordinator's records are authoritative;
//! the local state file under `uploads/` also remembers which objects R2
//! accepted in an earlier run, so a restarted upload goes straight to
//! `complete` for those instead of sending them again (`complete` heads
//! them; one it does not find is sent again).
//!
//! Nothing here hashes a model byte: the digests are the header's, computed
//! once at `pack`, and R2 verifies each object against them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use slop_ninja_admission_wire::json::{Session, Upload};
use slop_ninja_admission_wire::{ErrorCode, SessionState};
use slop_ninja_protocol::Record;
use slop_ninja_protocol::submission::SubmissionUpload;
use slop_ninja_r2_presign::layout::{ObjectId, ObjectKind};

use crate::client::{AdmissionClient, ApiError, check_session_matches};
use crate::direct::{Progress, PutError, PutOk, PutTarget, Silent, object_path_ok, put_object};
use crate::object::ObjectHeader;
use crate::opening::{OpeningFile, Store, UPLOAD_STATE_SCHEMA, upload_envelope, write_private};

/// The key of the header object in local records and session lists.
pub const HEADER: &str = "header";

/// Local record of an upload's progress.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadState {
    pub schema: String,
    pub session: String,
    pub cohort: u64,
    pub hotkey: String,
    pub commitment: String,
    pub artifact: PathBuf,
    pub header_sha256: String,
    pub header_len: u32,
    pub object_len: u64,
    /// The coordinator's records: `header` or a file digest, and whether it
    /// is recorded present.
    #[serde(default)]
    pub recorded: BTreeMap<String, bool>,
    /// Objects R2 accepted from this client (stored, or 412 already
    /// present), which the coordinator may not have recorded yet.
    #[serde(default)]
    pub accepted: BTreeSet<String>,
    pub completed: bool,
    pub state: String,
    #[serde(default)]
    pub refusal: Option<String>,
    #[serde(default)]
    pub repair_needed: Vec<String>,
}

impl UploadState {
    pub fn load(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        crate::opening::check_private(path)?;
        let state: Self = match serde_json::from_slice(&std::fs::read(path)?) {
            Ok(state) => state,
            // A record this build cannot read (an older schema) only cost a
            // resend: the coordinator's records are authoritative.
            Err(_) => return Ok(None),
        };
        if state.schema != UPLOAD_STATE_SCHEMA {
            return Ok(None);
        }
        Ok(Some(state))
    }

    fn save(&self, path: &Path) -> Result<()> {
        write_private(path, &serde_json::to_vec_pretty(self)?, true)
    }

    fn observe(&mut self, session: &Session) {
        self.recorded = session
            .objects
            .iter()
            .map(|o| (object_key(&o.kind, &o.sha256), o.present))
            .collect();
        for (key, present) in &self.recorded {
            if *present {
                self.accepted.remove(key);
            }
        }
        self.state = session.state.clone();
        self.refusal = session.refusal.as_ref().map(|r| r.code.clone());
        self.repair_needed = session.repair_needed.clone();
    }
}

/// `header` for the header object, else the file digest.
fn object_key(kind: &str, sha256: &str) -> String {
    if kind == HEADER {
        HEADER.to_owned()
    } else {
        sha256.to_owned()
    }
}

/// Upload behaviour.
#[derive(Clone)]
pub struct UploadOptions {
    /// Attempts per request for retryable errors, and per object for PUTs.
    pub attempts: u32,
    /// First backoff; it doubles per attempt. A `Retry-After` from the
    /// coordinator replaces it for that wait.
    pub backoff: Duration,
    /// How long to poll after completion for verification to finish.
    pub wait: Duration,
    pub poll: Duration,
    /// Stop after this many object PUTs in this run (an interruption, for
    /// tests and for bandwidth-limited miners). The next run resumes.
    pub stop_after_objects: Option<u32>,
    /// Body progress of each PUT.
    pub progress: Arc<dyn Progress>,
}

impl std::fmt::Debug for UploadOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadOptions")
            .field("attempts", &self.attempts)
            .field("backoff", &self.backoff)
            .field("wait", &self.wait)
            .field("poll", &self.poll)
            .field("stop_after_objects", &self.stop_after_objects)
            .finish_non_exhaustive()
    }
}

impl Default for UploadOptions {
    fn default() -> Self {
        Self {
            attempts: 6,
            backoff: Duration::from_secs(2),
            wait: Duration::from_secs(600),
            poll: Duration::from_secs(10),
            stop_after_objects: None,
            progress: Arc::new(Silent),
        }
    }
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadOutcome {
    /// The session reached a state that needs no further upload.
    Finished(Box<Session>),
    /// The run stopped before every object was sent (`stop_after_objects`).
    Interrupted { uploaded: u32, remaining: u32 },
}

/// Runs `attempt` until it succeeds, fails without a retry, or uses
/// `options.attempts`. Waits double from `options.backoff`, except that a
/// coordinator's `Retry-After` sets that wait. `header_missing` and
/// `objects_missing` are returned at once: they ask the client to PUT.
pub async fn retry<T, F>(options: &UploadOptions, mut attempt: F) -> Result<T, ApiError>
where
    F: AsyncFnMut() -> Result<T, ApiError>,
{
    let mut wait = options.backoff;
    let mut tries = 0;
    loop {
        tries += 1;
        match attempt().await {
            Err(error)
                if error.retryable()
                    && !matches!(
                        error.code(),
                        Some(ErrorCode::HeaderMissing | ErrorCode::ObjectsMissing)
                    )
                    && tries < options.attempts =>
            {
                tokio::time::sleep(error.retry_after().unwrap_or(wait)).await;
                wait = wait.saturating_mul(2);
            }
            other => return other,
        }
    }
}

/// Where fresh URLs come from for one run.
enum Minter<'a> {
    /// An upload session: the header URL from session creation (idempotent),
    /// file URLs from `urls`.
    Session { envelope: &'a SubmissionUpload },
    /// A repair: every URL from `repair`.
    Repair,
}

/// What a fresh-URL request found for one object.
enum Fresh {
    Url(Box<Upload>),
    /// The coordinator records it present: nothing to send.
    Present,
    /// The session left `uploading` (or, for a repair, needs nothing more).
    Ended(Box<Session>),
}

struct Run<'a> {
    client: &'a mut AdmissionClient,
    options: &'a UploadOptions,
    session_id: String,
    minter: Minter<'a>,
    session: Session,
}

impl Run<'_> {
    /// Asks for fresh URLs and returns the one for `target`.
    async fn fresh(&mut self, target: &PutTarget) -> Result<Fresh> {
        let id = self.session_id.clone();
        let session = match self.minter {
            Minter::Session { envelope } if target.is_header() => {
                let client = &mut *self.client;
                retry(self.options, async || client.create_session(envelope).await)
                    .await?
                    .0
            }
            Minter::Session { .. } => {
                let client = &mut *self.client;
                match retry(self.options, async || client.urls(&id).await).await {
                    Ok(session) => session,
                    // The header is not recorded after all: send it first.
                    Err(error) if error.code() == Some(ErrorCode::HeaderMissing) => {
                        return Err(
                            anyhow::Error::new(error).context("the coordinator has no header yet")
                        );
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Minter::Repair => {
                let client = &mut *self.client;
                match retry(self.options, async || client.repair(&id).await).await {
                    Ok(session) => session,
                    Err(error) if error.code() == Some(ErrorCode::NotNeeded) => {
                        let client = &mut *self.client;
                        let session =
                            retry(self.options, async || client.session(&id).await).await?;
                        return Ok(Fresh::Ended(Box::new(session)));
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        };
        self.session = session.clone();
        let key = if target.is_header() {
            HEADER.to_owned()
        } else {
            hex::encode(target.sha256)
        };
        let repairing = matches!(self.minter, Minter::Repair);
        if !repairing && session.state() != Some(SessionState::Uploading) {
            return Ok(Fresh::Ended(Box::new(session)));
        }
        let found = session
            .uploads
            .iter()
            .find(|u| object_key(&u.kind, &u.sha256) == key);
        if let Some(upload) = found {
            return Ok(Fresh::Url(Box::new(upload.clone())));
        }
        if repairing {
            // No URL: the object is back (nothing to send), or the
            // coordinator will not mint one now (stop and report).
            let digest = hex::encode(target.sha256);
            let still_needed = session
                .repair_needed
                .iter()
                .any(|k| *k == key || *k == digest);
            return Ok(if still_needed {
                Fresh::Ended(Box::new(session))
            } else {
                Fresh::Present
            });
        }
        let present = session
            .objects
            .iter()
            .any(|o| object_key(&o.kind, &o.sha256) == key && o.present);
        if present {
            return Ok(Fresh::Present);
        }
        bail!(
            "the coordinator answered no URL for {} and does not record it present",
            target.name
        )
    }

    /// PUTs one object, retrying per the contract. `upload` is a URL already
    /// in hand, if any. Returns `None` when R2 holds it, or the session that
    /// ended meanwhile.
    async fn put(
        &mut self,
        target: &PutTarget,
        mut upload: Option<Upload>,
    ) -> Result<Option<Session>> {
        let transfer = self.client.transfer();
        let mut wait = self.options.backoff;
        let mut tries = 0u32;
        loop {
            let current = match upload.take() {
                Some(upload) => upload,
                None => match self.fresh(target).await? {
                    Fresh::Url(upload) => *upload,
                    Fresh::Present => return Ok(None),
                    Fresh::Ended(session) => return Ok(Some(*session)),
                },
            };
            tries += 1;
            self.options.progress.start(&target.name, target.len, tries);
            let outcome = put_object(
                self.client.http(),
                self.client.put_policy(),
                &current,
                target,
                transfer.put_timeout(target.len),
                self.options.progress.clone(),
            )
            .await;
            match outcome {
                Ok(ok) => {
                    let why = match ok {
                        PutOk::Stored => "stored",
                        PutOk::AlreadyPresent => "already present",
                    };
                    self.options.progress.finish(&target.name, true, why);
                    return Ok(None);
                }
                Err(error) => {
                    self.options
                        .progress
                        .finish(&target.name, false, &error.to_string());
                    match error {
                        // The URL expired before the PUT started: a fresh
                        // one at once.
                        PutError::Expired { .. } if tries < self.options.attempts => {}
                        PutError::Retry(_) if tries < self.options.attempts => {
                            tokio::time::sleep(wait).await;
                            wait = wait.saturating_mul(2);
                        }
                        PutError::Expired { .. } | PutError::Retry(_) => {
                            bail!(
                                "{}: {error}; gave up after {tries} attempts. Run the command again to \
                                 resume: objects already in R2 are not sent again",
                                target.name
                            )
                        }
                        PutError::BadDigest(_) | PutError::Signature(_) | PutError::Fatal(_) => {
                            bail!("{}: {error}", target.name)
                        }
                    }
                }
            }
        }
    }
}

fn target_for(
    object: &ObjectHeader,
    path: &Path,
    cohort: u64,
    commitment: [u8; 32],
    key: &str,
) -> Result<PutTarget> {
    if key == HEADER {
        return Ok(PutTarget {
            id: ObjectId {
                cohort,
                commitment,
                kind: ObjectKind::Header,
            },
            sha256: object.sha256.0,
            len: u64::from(object.len),
            path: path.to_path_buf(),
            offset: 4,
            name: HEADER.into(),
        });
    }
    let sha = slop_ninja_admission_wire::parse_hex32(key)
        .with_context(|| format!("the coordinator named object {key:?}, not a digest"))?;
    let digest = slop_ninja_protocol::Digest(sha);
    let (offset, len) = object.file_range(&digest).with_context(|| {
        format!("the coordinator named file {key}, which the header does not list")
    })?;
    let name = object
        .header
        .files
        .iter()
        .find(|f| f.sha256 == digest)
        .map(|f| f.name.clone())
        .unwrap_or_else(|| key.to_owned());
    Ok(PutTarget {
        id: ObjectId {
            cohort,
            commitment,
            kind: ObjectKind::File(sha),
        },
        sha256: sha,
        len,
        path: path.to_path_buf(),
        offset,
        name,
    })
}

/// Opens or resumes the session for `opening_file` and the object at
/// `artifact_path`, PUTs every object R2 lacks straight to R2, completes the
/// session and waits for the verification result.
pub async fn upload(
    client: &mut AdmissionClient,
    store: &Store,
    opening_file: &OpeningFile,
    artifact_path: &Path,
    object: &ObjectHeader,
    options: &UploadOptions,
) -> Result<UploadOutcome> {
    let opening = opening_file.opening()?;
    ensure!(
        opening_file.fits(object),
        "{} is not the artifact this opening was made for",
        artifact_path.display()
    );
    ensure!(
        opening.chain == *client.chain(),
        "the coordinator serves another chain or subnet than the opening names"
    );
    ensure!(
        opening.hotkey == client.hotkey(),
        "the opening names another hotkey than the signing wallet"
    );
    object_path_ok(artifact_path, object.object_len).map_err(anyhow::Error::msg)?;
    let envelope = upload_envelope(opening, object)?;
    let session_id = envelope
        .digest()
        .map_err(|error| anyhow::anyhow!("envelope: {error:?}"))?
        .hex();
    let state_path = store.upload_state_path(&session_id);

    let info = retry(options, async || client.refresh().await).await?;
    let admitting = match &info.admitting {
        Some(admitting) if admitting.cohort == envelope.opening.cohort => admitting.clone(),
        Some(admitting) => bail!(
            "the coordinator admits cohort {}, but this opening names cohort {}; an opening cannot \
             carry over: commit again for cohort {} and upload that",
            admitting.cohort,
            envelope.opening.cohort,
            admitting.cohort
        ),
        None => bail!(
            "no cohort is admitting{}",
            info.next
                .as_ref()
                .map(|n| format!("; the next is cohort {}", n.cohort))
                .unwrap_or_default()
        ),
    };
    ensure!(
        object.object_len <= admitting.max_object_len,
        "the object is {} bytes; cohort {} allows {}",
        object.object_len,
        admitting.cohort,
        admitting.max_object_len
    );

    // The confirmed block of this opening's commitment, when this wallet
    // read it back: a coordinator that has not logged that commitment yet
    // answers the retryable `commitment_not_found` until its reader has
    // polled at or after that block, instead of a definitive refusal based
    // on an older logged commitment.
    client.set_commitment_block(
        opening_file
            .submitted
            .as_ref()
            .and_then(|s| s.finalized_commitment_block),
    );
    let (session, _) = retry(options, async || client.create_session(&envelope).await).await?;
    check_session_matches(&session, &envelope)?;
    let mut state = match UploadState::load(&state_path)? {
        Some(state)
            if state.session == session_id && state.header_sha256 == object.sha256.hex() =>
        {
            state
        }
        _ => UploadState {
            schema: UPLOAD_STATE_SCHEMA.into(),
            session: session_id.clone(),
            cohort: envelope.opening.cohort,
            hotkey: session.hotkey.clone(),
            commitment: envelope.commitment.hex(),
            artifact: artifact_path.to_path_buf(),
            header_sha256: object.sha256.hex(),
            header_len: object.len,
            object_len: object.object_len,
            recorded: BTreeMap::new(),
            accepted: BTreeSet::new(),
            completed: false,
            state: session.state.clone(),
            refusal: None,
            repair_needed: Vec::new(),
        },
    };
    state.observe(&session);
    state.save(&state_path)?;

    let mut session = session;
    if session.state() == Some(SessionState::Uploading) {
        let mut run = Run {
            client: &mut *client,
            options,
            session_id: session_id.clone(),
            minter: Minter::Session {
                envelope: &envelope,
            },
            session: session.clone(),
        };
        match send_all(&mut run, object, artifact_path, &mut state, &state_path).await? {
            Sent::Interrupted {
                uploaded,
                remaining,
            } => {
                state.save(&state_path)?;
                return Ok(UploadOutcome::Interrupted {
                    uploaded,
                    remaining,
                });
            }
            Sent::Session(done) => session = *done,
        }
        state.completed = session.state() != Some(SessionState::Uploading);
    }
    state.observe(&session);
    state.save(&state_path)?;

    let deadline = tokio::time::Instant::now() + options.wait;
    while session.state() == Some(SessionState::Verifying) && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(options.poll).await;
        session = retry(options, async || client.session(&session_id).await).await?;
        state.observe(&session);
        state.save(&state_path)?;
    }
    Ok(UploadOutcome::Finished(Box::new(session)))
}

enum Sent {
    Session(Box<Session>),
    Interrupted { uploaded: u32, remaining: u32 },
}

fn present(session: &Session, key: &str) -> bool {
    session
        .objects
        .iter()
        .any(|o| object_key(&o.kind, &o.sha256) == key && o.present)
}

/// The header, then every file, then `complete`, until the coordinator
/// records every object or the session ends. A `complete` that still lists
/// missing objects (or a `urls` that finds no header) starts another round:
/// at most `options.attempts` such answers in one run, after which the run
/// stops and a rerun resumes.
async fn send_all(
    run: &mut Run<'_>,
    object: &ObjectHeader,
    path: &Path,
    state: &mut UploadState,
    state_path: &Path,
) -> Result<Sent> {
    let cohort = state.cohort;
    let commitment =
        slop_ninja_admission_wire::parse_hex32(&state.commitment).context("commitment")?;
    let mut uploaded = 0u32;
    let mut rounds = 0u32;
    // Every distinct file, largest first.
    let mut files: Vec<(String, u64)> = object
        .distinct_files()
        .iter()
        .map(|f| (hex::encode(f.sha256.0), f.len))
        .collect();
    files.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    loop {
        // The header.
        if !present(&run.session, HEADER) && !state.accepted.contains(HEADER) {
            if let Some(limit) = run.options.stop_after_objects
                && uploaded >= limit
            {
                return Ok(Sent::Interrupted {
                    uploaded,
                    remaining: 1 + files.len() as u32,
                });
            }
            let target = target_for(object, path, cohort, commitment, HEADER)?;
            let in_hand = run
                .session
                .uploads
                .iter()
                .find(|u| u.kind == HEADER)
                .cloned();
            run.options.progress.plan(1, target.len);
            if let Some(ended) = run.put(&target, in_hand).await? {
                return Ok(Sent::Session(Box::new(ended)));
            }
            state.accepted.insert(HEADER.into());
            state.save(state_path)?;
            uploaded += 1;
        }
        // The file URLs: the coordinator settles the header first.
        let id = run.session_id.clone();
        let options = run.options;
        let client = &mut *run.client;
        match retry(options, async || client.urls(&id).await).await {
            Ok(session) => run.session = session,
            Err(error) if error.code() == Some(ErrorCode::HeaderMissing) => {
                // R2 has no header after all (a PUT this client believed
                // stored, or a deleted object): send it again.
                state.accepted.remove(HEADER);
                state.save(state_path)?;
                rounds += 1;
                ensure!(rounds < run.options.attempts, "{error}; run upload again");
                let client = &mut *run.client;
                let envelope = match run.minter {
                    Minter::Session { envelope } => envelope,
                    Minter::Repair => bail!("{error}: a repair sends no header this way"),
                };
                run.session = retry(options, async || client.create_session(envelope).await)
                    .await?
                    .0;
                continue;
            }
            Err(error) => return Err(error.into()),
        }
        state.observe(&run.session);
        state.save(state_path)?;
        if run.session.state() != Some(SessionState::Uploading) {
            return Ok(Sent::Session(Box::new(run.session.clone())));
        }
        let missing: Vec<(String, u64)> = files
            .iter()
            .filter(|(sha, _)| !present(&run.session, sha) && !state.accepted.contains(sha))
            .cloned()
            .collect();
        run.options
            .progress
            .plan(missing.len(), missing.iter().map(|(_, len)| *len).sum());
        for (i, (sha, _)) in missing.iter().enumerate() {
            if let Some(limit) = run.options.stop_after_objects
                && uploaded >= limit
            {
                return Ok(Sent::Interrupted {
                    uploaded,
                    remaining: (missing.len() - i) as u32,
                });
            }
            let target = target_for(object, path, cohort, commitment, sha)?;
            let in_hand = run
                .session
                .uploads
                .iter()
                .find(|u| u.kind != HEADER && u.sha256 == *sha)
                .cloned();
            if let Some(ended) = run.put(&target, in_hand).await? {
                return Ok(Sent::Session(Box::new(ended)));
            }
            state.accepted.insert(sha.clone());
            state.save(state_path)?;
            uploaded += 1;
        }
        // Completion: the coordinator heads every object.
        let client = &mut *run.client;
        match retry(options, async || client.complete(&id).await).await {
            Ok(session) => return Ok(Sent::Session(Box::new(session))),
            Err(error) if error.code() == Some(ErrorCode::ObjectsMissing) => {
                let listed = error
                    .body()
                    .and_then(|b| b.error.detail.missing.clone())
                    .unwrap_or_default();
                for sha in &listed {
                    state.accepted.remove(sha);
                    if *sha == object.sha256.hex() {
                        state.accepted.remove(HEADER);
                    }
                }
                if listed.is_empty() {
                    state.accepted.clear();
                }
                state.save(state_path)?;
                rounds += 1;
                ensure!(rounds < run.options.attempts, "{error}; run upload again");
                // Fresh records and URLs for the next round.
                let client = &mut *run.client;
                run.session = retry(options, async || client.session(&id).await).await?;
                run.session.uploads.clear();
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Re-PUTs every recorded object of `session_id` that went missing
/// (`repair_needed`) from the local object at `artifact_path`, whose header
/// must be the session's. Returns the session afterwards.
pub async fn repair(
    client: &mut AdmissionClient,
    session_id: &str,
    artifact_path: &Path,
    object: &ObjectHeader,
    options: &UploadOptions,
) -> Result<Session> {
    let session = retry(options, async || client.session(session_id).await).await?;
    ensure!(
        session.header_sha256 == object.sha256.hex() && session.header_len == object.len,
        "{} is not the artifact of session {session_id} (its header is {})",
        artifact_path.display(),
        session.header_sha256
    );
    object_path_ok(artifact_path, object.object_len).map_err(anyhow::Error::msg)?;
    let session = if session.repair_needed.is_empty() {
        // Nothing listed yet: the repair route heads every recorded object
        // first, so a loss the coordinator has not noticed (no scoring
        // lane or publication met it yet) is found and listed here. Its
        // `not_needed` means every recorded object is present.
        match retry(options, async || client.repair(session_id).await).await {
            Ok(s) if s.repair_needed.is_empty() => return Ok(s),
            Ok(s) => s,
            Err(error) if error.code() == Some(ErrorCode::NotNeeded) => return Ok(session),
            Err(error) => return Err(error.into()),
        }
    } else {
        session
    };
    let commitment =
        slop_ninja_admission_wire::parse_hex32(&session.commitment).context("commitment")?;
    let cohort = session.cohort;
    let needed = session.repair_needed.clone();
    let mut run = Run {
        client: &mut *client,
        options,
        session_id: session_id.to_owned(),
        minter: Minter::Repair,
        session,
    };
    let targets = needed
        .iter()
        .map(|key| {
            // A repair names the header by its digest or as `header`.
            let key = if *key == object.sha256.hex() {
                HEADER
            } else {
                key.as_str()
            };
            target_for(object, artifact_path, cohort, commitment, key)
        })
        .collect::<Result<Vec<_>>>()?;
    run.options
        .progress
        .plan(targets.len(), targets.iter().map(|t| t.len).sum());
    for target in &targets {
        if let Some(ended) = run.put(target, None).await? {
            run.session = ended;
        }
    }
    let client = &mut *run.client;
    Ok(retry(options, async || client.session(session_id).await).await?)
}

/// A human explanation of a session for `status` and `upload`.
pub fn describe(session: &Session) -> String {
    let mut out = format!(
        "session {} in cohort {}: {}",
        session.session, session.cohort, session.state
    );
    if let Some(refusal) = &session.refusal {
        out.push_str(&format!(" ({}: {})", refusal.code, refusal.message));
        if let Some(check) = &refusal.detail.check {
            out.push_str(&format!(" [check {check}]"));
        }
    }
    if let Some(position) = session.position {
        out.push_str(&format!(
            "; position block {} extrinsic {}",
            position.block, position.extrinsic
        ));
    }
    if !session.repair_needed.is_empty() {
        out.push_str(&format!(
            "; {} recorded objects went missing on the coordinator's side (not the miner's fault): \
             run slop_ninja_miner repair --artifact <the object> to send them again",
            session.repair_needed.len()
        ));
    }
    match session.state() {
        Some(SessionState::Overflow) => out.push_str(&format!(
            "; the roster was full. This opening cannot carry over: commit again (a new nonce) for cohort {} \
             once it opens, then upload",
            session
                .next_cohort
                .map_or_else(|| "the next cohort".to_owned(), |c| c.to_string())
        )),
        Some(SessionState::Uploading) => {
            let have = session.objects.iter().filter(|o| o.present).count();
            if session.objects.len() > 1 {
                out.push_str(&format!(
                    "; {have} of {} objects recorded; run upload to resume",
                    session.objects.len()
                ));
            } else {
                out.push_str("; the header is not recorded yet; run upload to resume");
            }
        }
        Some(SessionState::Superseded) => out.push_str("; a newer commitment replaced it"),
        Some(SessionState::Refused) => {
            let code = session.refusal.as_ref().and_then(|r| r.code());
            match code {
                Some(ErrorCode::ObjectReplaced) => out.push_str(
                    "; a recorded object was replaced in R2 (another entity tag or stored attribute). \
                     This envelope stays refused: commit again (a new nonce) and upload with the \
                     official client, which sends only the signed headers",
                ),
                Some(
                    ErrorCode::IncompleteAtCutoff
                    | ErrorCode::CommitmentOverwritten
                    | ErrorCode::NotRegistered
                    | ErrorCode::RegistrationMismatch,
                ) => {}
                _ => out.push_str(
                    "; this envelope stays refused. Fix the artifact, then commit again (a new nonce) \
                     and upload the new object; the newer commitment replaces this one",
                ),
            }
        }
        _ => {}
    }
    out
}
