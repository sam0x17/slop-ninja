//! Resumable upload of one artifact object.
//!
//! The session id is the envelope's digest, so a restarted upload finds its
//! session without any local state. The coordinator's recorded parts are
//! authoritative: a part is skipped only when the coordinator holds it with
//! the digest of the local bytes. The local state file under `uploads/`
//! records progress for `status` and is rewritten after every part.
//!
//! Memory: one part at a time, shared (not copied) across retries.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use slop_ninja_admission_wire::json::Session;
use slop_ninja_admission_wire::{ErrorCode, PartPlan, SessionState};
use slop_ninja_protocol::submission::SubmissionUpload;
use slop_ninja_protocol::{Record, VerifiedArtifact, object_sha256};

use crate::client::{AdmissionClient, ApiError, check_session_matches};
use crate::object::read_range;
use crate::opening::{OpeningFile, Store, UPLOAD_STATE_SCHEMA, upload_envelope, write_private};

/// Local record of an upload's progress.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadState {
    pub schema: String,
    pub session: String,
    pub cohort: u64,
    pub hotkey: String,
    pub commitment: String,
    pub artifact: PathBuf,
    pub artifact_sha256: String,
    pub artifact_len: u64,
    pub part_size: u64,
    pub part_count: u32,
    #[serde(default)]
    pub attempt: u32,
    /// Part number to the digest the coordinator acknowledged.
    pub parts: BTreeMap<u32, String>,
    pub completed: bool,
    pub state: String,
    #[serde(default)]
    pub refusal: Option<String>,
}

impl UploadState {
    pub fn load(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        crate::opening::check_private(path)?;
        let state: Self = serde_json::from_slice(&std::fs::read(path)?)
            .with_context(|| format!("{} is not an upload state file", path.display()))?;
        ensure!(
            state.schema == UPLOAD_STATE_SCHEMA,
            "unknown upload state schema"
        );
        Ok(Some(state))
    }

    fn save(&self, path: &Path) -> Result<()> {
        write_private(path, &serde_json::to_vec_pretty(self)?, true)
    }

    fn observe(&mut self, session: &Session) {
        if session.attempt != self.attempt {
            // A new attempt starts empty.
            self.attempt = session.attempt;
            self.parts.clear();
            self.completed = false;
        }
        self.state = session.state.clone();
        self.refusal = session.refusal.as_ref().map(|r| r.code.clone());
    }
}

/// Upload behaviour.
#[derive(Clone, Debug)]
pub struct UploadOptions {
    /// Attempts per request for retryable errors.
    pub attempts: u32,
    /// First backoff; it doubles per attempt. A `Retry-After` from the
    /// coordinator replaces it for that wait.
    pub backoff: Duration,
    /// How long to poll after completion for verification to finish.
    pub wait: Duration,
    pub poll: Duration,
    /// Stop after this many part uploads in this run (an interruption, for
    /// tests and for bandwidth-limited miners). The next run resumes.
    pub stop_after_parts: Option<u32>,
    /// When the session is refused (for example `object_mismatch`), abort its
    /// attempt and upload every part again in a new attempt.
    pub restart: bool,
}

impl Default for UploadOptions {
    fn default() -> Self {
        Self {
            attempts: 6,
            backoff: Duration::from_secs(2),
            wait: Duration::from_secs(600),
            poll: Duration::from_secs(10),
            stop_after_parts: None,
            restart: false,
        }
    }
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadOutcome {
    /// The session reached a state that needs no further upload.
    Finished(Box<Session>),
    /// The run stopped before every part was sent (`stop_after_parts`).
    Interrupted { uploaded: u32, remaining: u32 },
}

/// Runs `attempt` until it succeeds, fails without a retry, or uses
/// `options.attempts`. Waits double from `options.backoff`, except that a
/// coordinator's `Retry-After` sets that wait.
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
                    && error.code() != Some(ErrorCode::PartsMissing)
                    && tries < options.attempts =>
            {
                tokio::time::sleep(error.retry_after().unwrap_or(wait)).await;
                wait = wait.saturating_mul(2);
            }
            other => return other,
        }
    }
}

async fn send_part(
    client: &mut AdmissionClient,
    options: &UploadOptions,
    artifact_path: &Path,
    plan: &PartPlan,
    session_id: &str,
    part: u32,
) -> Result<(u32, String)> {
    let (offset, len) = plan
        .range(part)
        .with_context(|| format!("the coordinator named part {part}, outside the plan"))?;
    let bytes = Bytes::from(read_range(artifact_path, offset, len)?);
    let ack = retry(options, async || {
        client.put_part(session_id, part, bytes.clone()).await
    })
    .await?;
    Ok((ack.part, ack.sha256))
}

/// Opens or resumes the session for `opening_file` and `artifact`, uploads
/// every part the coordinator lacks, completes it and waits for the
/// verification result.
pub async fn upload(
    client: &mut AdmissionClient,
    store: &Store,
    opening_file: &OpeningFile,
    artifact_path: &Path,
    artifact: &VerifiedArtifact,
    options: &UploadOptions,
) -> Result<UploadOutcome> {
    let opening = opening_file.opening()?;
    ensure!(
        artifact.sha256.hex() == opening_file.artifact_sha256
            && artifact.len == opening_file.artifact_len,
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
    let upload: SubmissionUpload = upload_envelope(opening, artifact)?;
    let session_id = upload
        .digest()
        .map_err(|error| anyhow::anyhow!("envelope: {error:?}"))?
        .hex();
    let state_path = store.upload_state_path(&session_id);

    let info = retry(options, async || client.refresh().await).await?;
    let admitting = match &info.admitting {
        Some(admitting) if admitting.cohort == upload.opening.cohort => admitting.clone(),
        Some(admitting) => bail!(
            "the coordinator admits cohort {}, but this opening names cohort {}; an opening cannot \
             carry over: commit again for cohort {} and upload that",
            admitting.cohort,
            upload.opening.cohort,
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
        artifact.len <= admitting.max_object_len,
        "the object is {} bytes; cohort {} allows {}",
        artifact.len,
        admitting.cohort,
        admitting.max_object_len
    );

    let (mut session, _) = retry(options, async || client.create_session(&upload).await).await?;
    check_session_matches(&session, &upload)?;
    if options.restart && session.state() == Some(SessionState::Refused) {
        let attempt = session.attempt;
        session = retry(options, async || client.abort(&session_id, attempt).await).await?;
    }
    if session.state() == Some(SessionState::Aborted) {
        let next = session.attempt.saturating_add(1);
        session = retry(options, async || client.reopen(&session_id, next).await).await?;
    }
    let plan = PartPlan::new(artifact.len, session.part_size, admitting.max_parts)?;
    ensure!(
        plan.count() == session.part_count,
        "the coordinator plans {} parts; the contract gives {}",
        session.part_count,
        plan.count()
    );
    let mut state = match UploadState::load(&state_path)? {
        Some(state) if state.session == session_id => state,
        _ => UploadState {
            schema: UPLOAD_STATE_SCHEMA.into(),
            session: session_id.clone(),
            cohort: upload.opening.cohort,
            hotkey: session.hotkey.clone(),
            commitment: upload.commitment.hex(),
            artifact: artifact_path.to_path_buf(),
            artifact_sha256: artifact.sha256.hex(),
            artifact_len: artifact.len,
            part_size: plan.part_size(),
            part_count: plan.count(),
            attempt: session.attempt,
            parts: BTreeMap::new(),
            completed: false,
            state: session.state.clone(),
            refusal: None,
        },
    };
    state.observe(&session);
    state.save(&state_path)?;

    if session.state() == Some(SessionState::Uploading) {
        let recorded: BTreeMap<u32, String> = session
            .parts
            .iter()
            .map(|p| (p.part, p.sha256.clone()))
            .collect();
        let mut uploaded = 0u32;
        for part in 1..=plan.count() {
            let (offset, len) = plan.range(part).expect("in range");
            if let Some(digest) = recorded.get(&part) {
                let bytes = read_range(artifact_path, offset, len)?;
                if *digest == object_sha256(&bytes).hex() {
                    state.parts.insert(part, digest.clone());
                    continue;
                }
            }
            if let Some(limit) = options.stop_after_parts
                && uploaded >= limit
            {
                state.save(&state_path)?;
                let remaining = plan.missing(|n| state.parts.contains_key(&n)).count() as u32;
                return Ok(UploadOutcome::Interrupted {
                    uploaded,
                    remaining,
                });
            }
            let (part, digest) =
                send_part(client, options, artifact_path, &plan, &session_id, part).await?;
            state.parts.insert(part, digest);
            state.save(&state_path)?;
            uploaded += 1;
        }
        session = complete(
            client,
            options,
            artifact_path,
            &plan,
            &session_id,
            &mut state,
            &state_path,
        )
        .await?;
        state.completed = true;
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

/// Completes the session. When the coordinator reports `parts_missing`, the
/// listed parts are uploaded and completion is asked again, up to
/// `options.attempts` times.
async fn complete(
    client: &mut AdmissionClient,
    options: &UploadOptions,
    artifact_path: &Path,
    plan: &PartPlan,
    session_id: &str,
    state: &mut UploadState,
    state_path: &Path,
) -> Result<Session> {
    let mut rounds = 0;
    loop {
        rounds += 1;
        match retry(options, async || client.complete(session_id).await).await {
            Ok(session) => return Ok(session),
            Err(error) if error.code() == Some(ErrorCode::PartsMissing) => {
                let missing = error
                    .body()
                    .and_then(|b| b.error.detail.missing.clone())
                    .unwrap_or_default();
                if missing.is_empty() || rounds >= options.attempts {
                    bail!("{error}; run upload again");
                }
                for part in missing {
                    let (part, digest) =
                        send_part(client, options, artifact_path, plan, session_id, part).await?;
                    state.parts.insert(part, digest);
                    state.save(state_path)?;
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// A human explanation of a session for `status` and `upload`.
pub fn describe(session: &Session) -> String {
    let mut out = format!(
        "session {} (attempt {}) in cohort {}: {}",
        session.session, session.attempt, session.cohort, session.state
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
    match session.state() {
        Some(SessionState::Overflow) => out.push_str(&format!(
            "; the roster was full. This opening cannot carry over: commit again (a new nonce) for cohort {} \
             once it opens, then upload",
            session
                .next_cohort
                .map_or_else(|| "the next cohort".to_owned(), |c| c.to_string())
        )),
        Some(SessionState::Uploading) => {
            let have = session.parts.len();
            out.push_str(&format!(
                "; {have} of {} parts recorded; run upload to resume",
                session.part_count
            ));
        }
        Some(SessionState::Superseded) => out.push_str("; a newer commitment replaced it"),
        Some(SessionState::Refused) => {
            let code = session.refusal.as_ref().and_then(|r| r.code());
            match code {
                Some(ErrorCode::ObjectMismatch) => out.push_str(
                    "; the assembled object did not match the envelope (a part changed in transit). \
                     Run upload --restart to abort this attempt and send every part again",
                ),
                Some(
                    ErrorCode::IncompleteAtCutoff
                    | ErrorCode::CommitmentOverwritten
                    | ErrorCode::NotRegistered
                    | ErrorCode::RegistrationMismatch,
                ) => {}
                _ => out.push_str(&format!(
                    "; this envelope stays refused until its attempt is aborted \
                     (slop_ninja_miner abort --cohort {}); fix the artifact, or upload another \
                     object for this commitment after aborting",
                    session.cohort
                )),
            }
        }
        _ => {}
    }
    out
}
