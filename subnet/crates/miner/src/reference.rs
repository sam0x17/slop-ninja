//! Reference staging (`slop_ninja_reference_staging_v1`), the operator's
//! side: stage a packed reference package (`R_k`) the way
//! miners upload artifacts, so R2 verifies every file's SHA-256 at ingest.
//!
//! Input: the output directory of `slop_ninja_a_served_profile pack`
//! (`files/<sha256>` and the unpinned files manifest
//! `artifact-files.json`, naming the admission bucket and
//! `reference/v2/files/<sha256>` with no entity tags). Steps:
//!
//! 1. Check the manifest under the cohort's spec, under the staging rule
//!    the eval Worker's commit applies (its bound, the profile id label and
//!    every file at its reference location: `files_manifest::
//!    check_unpinned_reference`), and each local file's length (no hash: the
//!    pack hashed each file once, and R2 verifies it), all before any PUT.
//! 2. `POST /eval/reference/v2/urls` (operator token): one presigned `PUT`
//!    per file. Each URL is checked like a miner's ([`check_reference_upload`]:
//!    `https`, the pinned R2 host and bucket, exactly the key derived from the
//!    digest, the seven SigV4 parameters, and the three headers computed
//!    here), then the file is streamed from disk with those three headers.
//!    `412` (already present under its key) counts as staged; the commit
//!    confirms it.
//! 3. `POST /eval/reference/v2/commit?spec=<spec id>` with the manifest's
//!    exact bytes: the eval Worker heads each file, pins its entity tag and
//!    writes the pinned manifest; its answer names the package digest a
//!    template's `reference.package_sha256` takes.
//!
//! The routes' paths and shapes are `slop_ninja_admission_wire::reference`,
//! shared with the eval Worker.
//!
//! The operator token is read from a file and sent only in the
//! `Authorization` header; presigned URLs are never printed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use slop_ninja_admission_wire::json::Upload;
pub use slop_ninja_admission_wire::reference::{COMMIT_PATH, STAGING_RULE, UPLOAD_KIND, URLS_PATH};
use slop_ninja_admission_wire::reference::{CommitReply, FileRequest, UrlsReply, UrlsRequest, urls_body_max};
use slop_ninja_artifact_check::files_manifest::{
    FilesManifest, MAX_MANIFEST_BYTES, REFERENCE_MANIFEST_MAX_BYTES, check_unpinned_reference,
};
use slop_ninja_artifact_check::{Version, spec};
use slop_ninja_r2_presign::check::{check_put_url, put_headers};
use slop_ninja_r2_presign::layout;
use url::Url;

use crate::direct::{Body, Progress, PutError, PutOk, PutPolicy, send_put};

/// The manifest `pack` writes.
pub const MANIFEST_FILE: &str = "artifact-files.json";
/// The longest answer read from either route: the commit answer lists at
/// most the manifest's files with their keys and tags, and the URLs answer
/// one presigned URL per file; both are far below the manifest bound.
pub const REPLY_LIMIT: usize = MAX_MANIFEST_BYTES;

/// One reference file to stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefFile {
    pub name: String,
    pub sha256: [u8; 32],
    pub len: u64,
    pub path: PathBuf,
}

/// A checked pack output: the manifest's exact bytes, the spec it was
/// checked under, and its distinct files (one per digest).
#[derive(Debug)]
pub struct Staging {
    pub manifest_bytes: Vec<u8>,
    pub version: Version,
    pub files: Vec<RefFile>,
}

/// Reads and checks `dir` (a `pack` output) under `version`.
pub fn load(dir: &Path, version: Version) -> Result<Staging> {
    let path = dir.join(MANIFEST_FILE);
    let len = std::fs::metadata(&path).with_context(|| format!("{}", path.display()))?.len();
    ensure!(
        len <= REFERENCE_MANIFEST_MAX_BYTES as u64,
        "{} is larger than a staged reference manifest may be ({REFERENCE_MANIFEST_MAX_BYTES} bytes)",
        path.display()
    );
    let manifest_bytes = std::fs::read(&path).with_context(|| format!("{}", path.display()))?;
    ensure!(manifest_bytes.len() <= REFERENCE_MANIFEST_MAX_BYTES, "{} grew while it was read", path.display());
    let m = FilesManifest::parse_unbound(&manifest_bytes, version)
        .map_err(|e| anyhow::anyhow!("{}: refused under {}: {e}", path.display(), version.id()))?;
    check_unpinned_reference(&m).map_err(|r| {
        anyhow::anyhow!(
            "{}: outside the staging rule {STAGING_RULE} ({}); pack with this release",
            path.display(),
            r.as_str()
        )
    })?;
    let mut files: Vec<RefFile> = Vec::new();
    for f in &m.files {
        let sha = slop_ninja_r2_presign::buf::parse_hex32(&f.sha256).context("file digest")?;
        if files.iter().any(|g| g.sha256 == sha) {
            continue;
        }
        let local = dir.join("files").join(&f.sha256);
        let on_disk = std::fs::metadata(&local).with_context(|| format!("{}", local.display()))?.len();
        ensure!(on_disk == f.bytes, "{} is {on_disk} bytes; the manifest names {}", local.display(), f.bytes);
        files.push(RefFile { name: f.name.clone(), sha256: sha, len: f.bytes, path: local });
    }
    Ok(Staging { manifest_bytes, version, files })
}

/// The `urls` request body for `files`, refused above the eval Worker's
/// bound (the spec's file table).
pub fn urls_body(files: &[RefFile]) -> Result<Vec<u8>> {
    let req = UrlsRequest {
        files: files.iter().map(|f| FileRequest { sha256: hex::encode(f.sha256), bytes: f.len }).collect(),
    };
    let body = serde_json::to_vec(&req).context("urls body")?;
    ensure!(body.len() <= urls_body_max(spec::ROLES.len()), "{} files are more than one urls request names", files.len());
    Ok(body)
}

/// Parses the `urls` answer for `files`: the staging rule and one upload
/// per file, in order.
pub fn parse_urls_reply(body: &[u8], files: &[RefFile]) -> Result<Vec<Upload>> {
    let reply: UrlsReply = serde_json::from_slice(body).context("urls answer")?;
    ensure!(reply.rule == STAGING_RULE, "the eval Worker speaks {}, not {STAGING_RULE}", reply.rule);
    ensure!(reply.uploads.len() == files.len(), "the eval Worker minted {} URLs for {} files", reply.uploads.len(), files.len());
    Ok(reply.uploads)
}

/// Parses the `commit` answer: the staging rule and a package digest of 64
/// lowercase hex digits.
pub fn parse_commit_reply(body: &[u8]) -> Result<CommitReply> {
    let reply: CommitReply = serde_json::from_slice(body).context("commit answer")?;
    ensure!(reply.rule == STAGING_RULE, "the commit answer names another staging rule");
    ensure!(
        slop_ninja_r2_presign::buf::parse_hex32(&reply.package_sha256).is_some(),
        "the commit answer names no package digest"
    );
    Ok(reply)
}

/// Checks one minted reference upload against the file: kind, digest and
/// length, the URL (the pinned host and bucket, exactly
/// `reference/v2/files/<sha256>`), and the three headers.
pub fn check_reference_upload(upload: &Upload, file: &RefFile, policy: &PutPolicy) -> Result<(), String> {
    if upload.kind != UPLOAD_KIND || upload.sha256 != hex::encode(file.sha256) || upload.len != file.len {
        return Err(format!("the eval Worker minted a URL for {} {}, not for {}", upload.kind, upload.sha256, file.name));
    }
    let key = layout::reference_key(&file.sha256);
    let checked = check_put_url(&upload.url, policy.pinned_host.as_deref(), policy.bucket.as_deref(), key.as_str())
        .map_err(|e| format!("refusing the presigned URL for {}: {e:?}", file.name))?;
    if let Some(pin) = &policy.pinned_host_sha256
        && !crate::direct::pinned_host_digest_ok(checked.host, pin)
    {
        return Err(format!("refusing the presigned URL for {}: its host is not the pinned upload host", file.name));
    }
    let (length, if_none_match, checksum) = put_headers(file.len, &file.sha256);
    if upload.content_length != length.as_str()
        || upload.if_none_match != if_none_match
        || upload.checksum_sha256 != checksum.as_str()
    {
        return Err(format!("the presigned URL for {} names other headers than its length and digest", file.name));
    }
    Ok(())
}

/// The operator token, held only for the `Authorization` header; its
/// `Debug` never shows it.
pub struct Token(String);

impl Token {
    pub fn new(token: String) -> Self {
        Self(token)
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(..)")
    }
}

/// The eval Worker's operator client.
pub struct EvalClient {
    pub http: reqwest::Client,
    pub base: Url,
    pub token: Token,
}

impl EvalClient {
    async fn post(&self, path_and_query: &str, body: Vec<u8>) -> Result<(u16, Vec<u8>)> {
        let url = self.base.join(path_and_query).context("eval URL")?;
        let mut r = self
            .http
            .post(url)
            .header(reqwest::header::AUTHORIZATION, self.token.bearer())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("{path_and_query}: {}", e.without_url()))?;
        let status = r.status().as_u16();
        let mut out = Vec::new();
        while let Some(chunk) = r.chunk().await.map_err(|e| anyhow::anyhow!("{path_and_query}: {}", e.without_url()))? {
            ensure!(out.len() + chunk.len() <= REPLY_LIMIT, "{path_and_query}: answer above {REPLY_LIMIT} bytes");
            out.extend_from_slice(&chunk);
        }
        Ok((status, out))
    }

    /// Presigned URLs for `files`, in their order.
    pub async fn urls(&self, files: &[RefFile]) -> Result<Vec<Upload>> {
        let (status, body) = self.post(URLS_PATH, urls_body(files)?).await?;
        ensure!(status == 200, "{URLS_PATH} answered {status}: {}", String::from_utf8_lossy(&body));
        parse_urls_reply(&body, files)
    }

    /// The commit: the eval Worker's answer (package digest, manifest key,
    /// pinned files).
    pub async fn commit(&self, staging: &Staging) -> Result<CommitReply> {
        let path = format!("{COMMIT_PATH}?spec={}", staging.version.id());
        let (status, body) = self.post(&path, staging.manifest_bytes.clone()).await?;
        ensure!(status == 200, "{COMMIT_PATH} answered {status}: {}", String::from_utf8_lossy(&body));
        parse_commit_reply(&body)
    }
}

/// How one file's PUT ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Staged {
    Stored,
    AlreadyPresent,
}

/// Stages every file (fresh URLs for each attempt after a retryable
/// failure, at most `attempts` attempts per file: an operator choice), then
/// commits. Returns each file's outcome and the commit answer.
pub async fn stage(
    client: &EvalClient,
    policy: &PutPolicy,
    staging: &Staging,
    attempts: u32,
    timeout_for: impl Fn(u64) -> Duration,
    progress: Arc<dyn Progress>,
) -> Result<(Vec<Staged>, CommitReply)> {
    ensure!(attempts >= 1, "at least one attempt");
    let mut outcomes: Vec<Option<Staged>> = vec![None; staging.files.len()];
    let mut attempt = 0;
    while outcomes.iter().any(Option::is_none) {
        attempt += 1;
        let pending: Vec<usize> = (0..staging.files.len()).filter(|&i| outcomes[i].is_none()).collect();
        let files: Vec<RefFile> = pending.iter().map(|&i| staging.files[i].clone()).collect();
        let uploads = client.urls(&files).await?;
        let mut last_error = None;
        for (&i, upload) in pending.iter().zip(&uploads) {
            let f = &staging.files[i];
            check_reference_upload(upload, f, policy).map_err(anyhow::Error::msg)?;
            let body = Body { path: &f.path, offset: 0, len: f.len, sha256: &f.sha256, name: &f.name };
            progress.start(&f.name, f.len, attempt);
            match send_put(&client.http, policy, &upload.url, &body, timeout_for(f.len), progress.clone()).await {
                Ok(PutOk::Stored) => {
                    progress.finish(&f.name, true, "stored");
                    outcomes[i] = Some(Staged::Stored);
                }
                Ok(PutOk::AlreadyPresent) => {
                    progress.finish(&f.name, true, "already present");
                    outcomes[i] = Some(Staged::AlreadyPresent);
                }
                Err(e @ (PutError::Retry(_) | PutError::Expired { .. })) => {
                    progress.finish(&f.name, false, &e.to_string());
                    last_error = Some(e.to_string());
                }
                Err(e) => bail!("{}: {e}", f.name),
            }
        }
        if let Some(e) = last_error
            && attempt >= attempts
        {
            bail!("a file is still not staged after {attempts} attempts: {e}");
        }
    }
    let commit = client.commit(staging).await?;
    Ok((outcomes.into_iter().map(|o| o.unwrap_or(Staged::Stored)).collect(), commit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_r2_presign::presign::{Binding, Credentials, Target, presign_put};

    const HOST: &str = "0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com";
    const BUCKET: &str = "slop-ninja-admission-testnet";

    fn file() -> RefFile {
        RefFile { name: "model.onnx".into(), sha256: [8; 32], len: 681_508_964, path: PathBuf::from("/nonexistent") }
    }

    fn upload_for(f: &RefFile, key: &str, kind: &str) -> Upload {
        let cred = Credentials { access_key_id: "CFEXAMPLEKEY12345", secret_access_key: b"not-a-real-secret" };
        let p = presign_put(&cred, &Target { host: HOST, bucket: BUCKET, key }, &Binding { len: f.len, sha256: f.sha256 }, 1_764_612_312, 900)
            .unwrap();
        let (length, inm, checksum) = put_headers(f.len, &f.sha256);
        Upload {
            kind: kind.into(),
            sha256: hex::encode(f.sha256),
            len: f.len,
            url: p.url.as_str().into(),
            content_length: length.as_str().into(),
            if_none_match: inm.into(),
            checksum_sha256: checksum.as_str().into(),
            signed_at: p.signed_at,
            valid_secs: p.valid_secs,
        }
    }

    fn policy() -> PutPolicy {
        PutPolicy { pinned_host: Some(HOST.into()), bucket: Some(BUCKET.into()), ..Default::default() }
    }

    #[test]
    fn a_reference_url_must_name_exactly_its_digest_key() {
        let f = file();
        let good = upload_for(&f, layout::reference_key(&f.sha256).as_str(), "reference");
        assert_eq!(check_reference_upload(&good, &f, &policy()), Ok(()));
        let miner_key = layout::ObjectId { cohort: 0, commitment: [0; 32], kind: layout::ObjectKind::File(f.sha256) }.key();
        let miner = upload_for(&f, miner_key.as_str(), "reference");
        assert!(check_reference_upload(&miner, &f, &policy()).is_err());
        let other = upload_for(&f, layout::reference_key(&[9; 32]).as_str(), "reference");
        assert!(check_reference_upload(&other, &f, &policy()).is_err());
        let wrong_kind = upload_for(&f, layout::reference_key(&f.sha256).as_str(), "file");
        assert!(check_reference_upload(&wrong_kind, &f, &policy()).is_err());
        let other_bucket = PutPolicy { bucket: Some("eval-bucket-testnet".into()), ..policy() };
        assert!(check_reference_upload(&good, &f, &other_bucket).is_err());
        let mut lying = good.clone();
        lying.checksum_sha256 = put_headers(f.len, &[1; 32]).2.as_str().into();
        assert!(check_reference_upload(&lying, &f, &policy()).is_err());
    }

    #[test]
    fn the_urls_body_fits_the_eval_bound() {
        let n = spec::ROLES.len();
        let files: Vec<RefFile> =
            (0..=n).map(|i| RefFile { name: format!("f{i}"), sha256: [i as u8; 32], len: u64::MAX, path: PathBuf::new() }).collect();
        let body = urls_body(&files[..n]).unwrap();
        assert!(body.len() < urls_body_max(n));
        let parsed: UrlsRequest = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed.files.len(), n);
        assert!(urls_body(&files).is_err(), "more files than the spec's table");
    }

    /// The eval Worker's answers, as its shared shapes serialize them, pass
    /// the client's parse and URL check (the Worker builds each upload from
    /// `put_headers` and the derived key, as `upload_for` does).
    #[test]
    fn the_eval_answers_parse_and_check() {
        let f = file();
        let upload = upload_for(&f, layout::reference_key(&f.sha256).as_str(), UPLOAD_KIND);
        let urls = serde_json::to_vec(&UrlsReply { rule: STAGING_RULE.into(), uploads: vec![upload] }).unwrap();
        let got = parse_urls_reply(&urls, std::slice::from_ref(&f)).unwrap();
        assert_eq!(check_reference_upload(&got[0], &f, &policy()), Ok(()));
        assert!(parse_urls_reply(&urls, &[f.clone(), f.clone()]).is_err(), "one upload per file");
        let other_rule = serde_json::to_vec(&UrlsReply { rule: "other".into(), uploads: got.clone() }).unwrap();
        assert!(parse_urls_reply(&other_rule, std::slice::from_ref(&f)).is_err());

        let package = "ab".repeat(32);
        let commit = CommitReply {
            rule: STAGING_RULE.into(),
            manifest_key: format!("artifacts/packages/{package}.json"),
            package_sha256: package,
            manifest_bytes: 1_234,
            files: vec![slop_ninja_admission_wire::reference::PinnedFile {
                name: f.name.clone(),
                sha256: hex::encode(f.sha256),
                bytes: f.len,
                key: layout::reference_key(&f.sha256).as_str().into(),
                etag: "0123456789abcdef0123456789abcdef".into(),
            }],
        };
        let bytes = serde_json::to_vec(&commit).unwrap();
        assert_eq!(parse_commit_reply(&bytes).unwrap(), commit);
        let upper = CommitReply { package_sha256: "AB".repeat(32), ..commit.clone() };
        assert!(parse_commit_reply(&serde_json::to_vec(&upper).unwrap()).is_err());
        let mut extra: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        extra["unexpected"] = serde_json::json!(1);
        assert!(parse_commit_reply(&serde_json::to_vec(&extra).unwrap()).is_err());
    }

    /// A `pack` output for the spec's file table: sparse files of the
    /// table's lengths and the unpinned manifest.
    fn packed(dir: &Path, version: Version, edit: impl Fn(&mut FilesManifest)) {
        use slop_ninja_artifact_check::files_manifest::{FILES_SCHEMA_V2, FileEntry, REFERENCE_BUCKET, reference_file_key};
        let class = version.classes().iter().find(|c| version.serves(c)).unwrap();
        std::fs::create_dir_all(dir.join("files")).unwrap();
        let files = spec::ROLES
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let (bytes, sha) = if r.name == spec::GRAPH_FILE {
                    (class.bytes, format!("{:064x}", i + 1))
                } else if r.scorer_role == "tokenizer" {
                    (spec::TOKENIZER_BYTES, spec::TOKENIZER_SHA256.to_owned())
                } else {
                    (1_000 + i as u64, format!("{:064x}", i + 1))
                };
                std::fs::File::create(dir.join("files").join(&sha)).unwrap().set_len(bytes).unwrap();
                FileEntry {
                    name: r.name.into(),
                    role: r.scorer_role.into(),
                    bytes,
                    key: reference_file_key(&sha),
                    sha256: sha,
                    bucket: REFERENCE_BUCKET.into(),
                    etag: None,
                }
            })
            .collect();
        let mut m = FilesManifest {
            schema: FILES_SCHEMA_V2.into(),
            profile_id: format!("{}/{}", version.id(), class.id),
            contract: spec::CONTRACT_ID.into(),
            graph: spec::GRAPH_FILE.into(),
            files,
        };
        edit(&mut m);
        std::fs::write(dir.join(MANIFEST_FILE), m.to_bytes()).unwrap();
    }

    #[test]
    fn load_refuses_what_the_commit_would_before_any_upload() {
        let v = Version::V2;
        let ok = tempfile::tempdir().unwrap();
        packed(ok.path(), v, |_| {});
        let staging = load(ok.path(), v).unwrap();
        assert_eq!(staging.files.len(), spec::ROLES.len());
        assert!(staging.manifest_bytes.len() <= REFERENCE_MANIFEST_MAX_BYTES);

        let quoted = tempfile::tempdir().unwrap();
        packed(quoted.path(), v, |m| m.profile_id = "a\"b".into());
        assert!(load(quoted.path(), v).unwrap_err().to_string().contains("profile_id"));
        let long = tempfile::tempdir().unwrap();
        packed(long.path(), v, |m| m.profile_id = "x".repeat(slop_ninja_protocol::MAX_LABEL_LEN + 1));
        assert!(load(long.path(), v).unwrap_err().to_string().contains("profile_id"));
        let eval = tempfile::tempdir().unwrap();
        packed(eval.path(), v, |m| m.files[0].bucket = "eval".into());
        assert!(load(eval.path(), v).unwrap_err().to_string().contains("location"));
        let big = tempfile::tempdir().unwrap();
        packed(big.path(), v, |_| {});
        let mut bytes = std::fs::read(big.path().join(MANIFEST_FILE)).unwrap();
        bytes.resize(REFERENCE_MANIFEST_MAX_BYTES + 1, b' ');
        std::fs::write(big.path().join(MANIFEST_FILE), bytes).unwrap();
        assert!(load(big.path(), v).is_err());
    }
}
