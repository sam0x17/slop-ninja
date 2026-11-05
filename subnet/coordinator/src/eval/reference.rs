//! Reference staging (`slop_ninja_reference_staging_v1`): the operator's
//! reference packages (`R_k`) staged the way miners' artifacts are, so R2
//! verifies every reference file's SHA-256 at ingest.
//!
//! The operator's pack (`slop_ninja_a_served_profile pack`) writes each file
//! once under `files/<sha256>` and an unpinned files manifest v2 naming the
//! admission bucket and `reference/v2/files/<sha256>` with no entity tags.
//! Two operator routes of the eval Worker (operator token) then stage it:
//!
//! - **`POST /eval/reference/v2/urls`**: `{"files": [{"sha256", "bytes"}]}`,
//!   at most the spec's file table. The eval Worker asks the internal signer
//!   (`R2_SIGNER_TESTNET`, entry kind `Reference`) for one presigned `PUT`
//!   per file. Each URL binds the key (derived from the digest),
//!   `content-length`, `if-none-match: *` and `x-amz-checksum-sha256`, so R2
//!   stores exactly the named bytes or nothing (qualification G1/G4). The
//!   validity follows the upload URL policy over the package's total size.
//!   The operator's client (`slop_ninja_reference_stage`) checks each URL
//!   and PUTs the file straight from disk; no file byte reaches a Worker.
//! - **`POST /eval/reference/v2/commit?spec=<spec id>`**: the unpinned
//!   manifest's exact bytes. The eval Worker checks its shape and file table
//!   under the spec, requires every file in the admission bucket at its
//!   reference key with no tag, heads each one (size, stored SHA-256, and
//!   the stored-attribute rule admitted objects meet), pins each file's
//!   entity tag, and writes the pinned manifest to the eval bucket at
//!   `artifacts/packages/<package>.json` with R2 checking its SHA-256. The
//!   package (the pinned manifest's SHA-256, a small record) is what a
//!   template's `reference.package_sha256` names.
//!
//! The routes' paths and request and answer shapes are
//! `slop_ninja_admission_wire::reference`, which the operator's client uses
//! too; the manifest rule (profile id, locations, bound) is
//! `slop_ninja_artifact_check::files_manifest`, which the client checks
//! before any upload.
//!
//! Nothing here hashes a file byte: the operator's pack hashes each file
//! once, R2 verifies it at ingest, and loads read it by entity-tag-pinned
//! ranges, as for admitted artifacts. Every bound is derived: the request
//! sizes from the spec's file table and the manifest bound, each file's
//! length from the largest object of any compiled admission spec (the
//! signer's own bound), the URL life from the upload URL policy.

use serde_json::{Value, json};
use slop_ninja_admission_wire::json::Upload;
pub use slop_ninja_admission_wire::reference::{COMMIT_PATH, PREFIX, STAGING_RULE, UPLOAD_KIND, URLS_PATH};
use slop_ninja_admission_wire::reference::{CommitReply, PinnedFile, UrlsReply, UrlsRequest, urls_body_max};
use slop_ninja_artifact_check::admission::largest_admission_object;
use slop_ninja_artifact_check::files_manifest::{
    FilesManifest, MAX_ETAG_BYTES, REFERENCE_MANIFEST_MAX_BYTES, check_unpinned_reference, reference_label_ok,
};
use slop_ninja_artifact_check::{Version, spec};
use slop_ninja_r2_presign::buf::parse_hex32;
use slop_ninja_r2_presign::check::put_headers;
use slop_ninja_r2_presign::presign::{IF_NONE_MATCH, MAX_URL_LEN};
use slop_ninja_r2_presign::{policy, wire as mint};

use crate::eval::admission::{Head, Minted, manifest_key};

/// The most files one request names: the spec's file table.
pub const MAX_FILES: usize = spec::ROLES.len();
/// The exact bound of a compact `urls` body naming the whole file table
/// (`reference::urls_body_max`).
pub const URLS_BODY_BYTES: usize = urls_body_max(MAX_FILES);

/// The longest pinned reference manifest
/// (`files_manifest::REFERENCE_MANIFEST_MAX_BYTES`).
pub const MANIFEST_MAX_BYTES: usize = REFERENCE_MANIFEST_MAX_BYTES;

/// A `commit` body: one unpinned reference manifest (shorter than its
/// pinned form).
pub const COMMIT_BODY_BYTES: usize = MANIFEST_MAX_BYTES;

const URLS_REPLY_FRAME: &str = r#"{"rule":"","uploads":[]}"#;
const URLS_REPLY_UPLOAD: &str = r#"{"kind":"","sha256":"","len":,"url":"","content_length":"","if_none_match":"","checksum_sha256":"","signed_at":,"valid_secs":},"#;
/// The longest compact `urls` answer ([`UrlsReply`] as `serde_json`
/// writes it), counted from its layout: the frame and rule, and per file of
/// the spec's table the field names, the kind, the 64-digit digest, the
/// length in at most 20 digits, a presigned URL of at most `MAX_URL_LEN`
/// (no byte of which JSON escapes), the length header in at most 20 digits,
/// `if-none-match`, the 44-character base64 checksum, the signing time in
/// at most 20 digits and the life in at most 10.
pub const URLS_REPLY_BYTES: usize = URLS_REPLY_FRAME.len()
    + STAGING_RULE.len()
    + MAX_FILES
        * (URLS_REPLY_UPLOAD.len() + UPLOAD_KIND.len() + 64 + 20 + MAX_URL_LEN + 20 + IF_NONE_MATCH.len() + 44 + 20 + 10);

/// The longest reference file the signer binds (its own bound): the
/// largest object of any compiled admission spec, never above R2's single
/// PUT.
pub fn max_file_len() -> u64 {
    largest_admission_object().min(policy::R2_SINGLE_PUT_MAX)
}

/// Why a staging request was not done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefError {
    /// The request is outside the rule (400): the body, the spec, the
    /// manifest's shape or a file's location.
    Request(&'static str),
    /// A file is not in the bucket at its key (409): PUT it first.
    Missing(String),
    /// A file's key holds other bytes than the manifest names (409): an
    /// object written outside this path; delete it and stage again.
    Mismatch(String),
    /// A file carries stored attributes admitted objects may not (409).
    Attributes(String),
    /// A file's stored entity tag is outside the label rule the pinned
    /// manifest needs (409): deterministic for that object, so a retry
    /// cannot succeed.
    EntityTag(String),
    /// R2 or the signer failed (503); retry.
    Platform(&'static str),
}

impl RefError {
    pub fn status(&self) -> u16 {
        match self {
            Self::Request(_) => 400,
            Self::Missing(_) | Self::Mismatch(_) | Self::Attributes(_) | Self::EntityTag(_) => 409,
            Self::Platform(_) => 503,
        }
    }

    /// The content-free answer body.
    pub fn body(&self) -> Value {
        match self {
            Self::Request(what) => json!({"error": "reference_request", "detail": what, "rule": STAGING_RULE}),
            Self::Missing(sha) => json!({"error": "reference_missing", "sha256": sha, "rule": STAGING_RULE}),
            Self::Mismatch(sha) => json!({"error": "reference_mismatch", "sha256": sha, "rule": STAGING_RULE}),
            Self::Attributes(sha) => json!({"error": "reference_attributes", "sha256": sha, "rule": STAGING_RULE}),
            Self::EntityTag(sha) => json!({"error": "reference_entity_tag", "sha256": sha, "rule": STAGING_RULE}),
            Self::Platform(what) => json!({"error": "unavailable", "detail": what, "rule": STAGING_RULE}),
        }
    }
}

/// The signer entries of a `urls` body: one `Reference` entry per file,
/// each valid for the URL policy's life of the whole package.
pub fn mint_entries(body: &[u8]) -> Result<Vec<mint::Entry>, RefError> {
    if body.len() > URLS_BODY_BYTES {
        return Err(RefError::Request("body"));
    }
    let req: UrlsRequest = serde_json::from_slice(body).map_err(|_| RefError::Request("body"))?;
    if req.files.is_empty() || req.files.len() > MAX_FILES {
        return Err(RefError::Request("file count"));
    }
    let max = max_file_len();
    let mut entries: Vec<mint::Entry> = Vec::with_capacity(req.files.len());
    let mut total = 0u64;
    for f in &req.files {
        let sha = parse_hex32(&f.sha256).ok_or(RefError::Request("sha256"))?;
        if !(1..=max).contains(&f.bytes) {
            return Err(RefError::Request("bytes"));
        }
        if entries.iter().any(|e| e.sha256 == sha) {
            return Err(RefError::Request("duplicate file"));
        }
        total = total.checked_add(f.bytes).ok_or(RefError::Request("bytes"))?;
        entries.push(mint::Entry { op: mint::Op::Reference, cohort: 0, commitment: [0; 32], sha256: sha, len: f.bytes, valid_secs: 0 });
    }
    let valid = policy::valid_secs(total).map_err(|_| RefError::Request("bytes"))?;
    for e in &mut entries {
        e.valid_secs = valid;
    }
    Ok(entries)
}

/// The `urls` answer: each file's URL and the three headers its PUT sends
/// (computed here from the digest and length, as the client does).
pub fn urls_reply(entries: &[mint::Entry], minted: &[Minted]) -> Result<UrlsReply, RefError> {
    if entries.len() != minted.len() {
        return Err(RefError::Platform("signer reply"));
    }
    let mut uploads = Vec::with_capacity(entries.len());
    for (e, m) in entries.iter().zip(minted) {
        let (length, if_none_match, checksum) = put_headers(e.len, &e.sha256);
        if checksum.as_str() != m.checksum {
            return Err(RefError::Platform("signer checksum"));
        }
        uploads.push(Upload {
            kind: UPLOAD_KIND.into(),
            sha256: hex::encode(e.sha256),
            len: e.len,
            url: m.url.clone(),
            content_length: length.as_str().into(),
            if_none_match: if_none_match.into(),
            checksum_sha256: checksum.as_str().into(),
            signed_at: m.signed_at,
            valid_secs: m.valid_secs,
        });
    }
    Ok(UrlsReply { rule: STAGING_RULE.into(), uploads })
}

/// A checked `commit` request: the unpinned manifest, the spec it was
/// checked under, and the key of each file in file-table order.
#[derive(Debug)]
pub struct Commit {
    pub manifest: FilesManifest,
    pub version: Version,
    pub keys: Vec<String>,
}

/// Checks a `commit` body under the spec `spec_id` names: a files manifest
/// v2 whose every file is in the admission bucket at its reference key
/// with no entity tag.
pub fn commit_request(body: &[u8], spec_id: &str) -> Result<Commit, RefError> {
    if body.len() > COMMIT_BODY_BYTES {
        return Err(RefError::Request("body"));
    }
    let version = Version::from_id(spec_id).ok_or(RefError::Request("spec"))?;
    let manifest = FilesManifest::parse_unbound(body, version).map_err(|_| RefError::Request("manifest"))?;
    check_unpinned_reference(&manifest).map_err(|r| RefError::Request(r.as_str()))?;
    let keys = manifest.files.iter().map(|f| f.key.clone()).collect();
    Ok(Commit { manifest, version, keys })
}

/// A pinned reference manifest: its exact bytes, its package digest (the
/// SHA-256 of a small record) and its parse.
#[derive(Debug)]
pub struct Pinned {
    pub bytes: Vec<u8>,
    pub package: String,
    pub manifest: FilesManifest,
}

/// Pins each file's entity tag from its head (in [`Commit::keys`] order).
/// Each file must be present with the length and stored SHA-256 the
/// manifest names and clean stored attributes.
pub fn pin(commit: Commit, heads: &[Head]) -> Result<Pinned, RefError> {
    let Commit { mut manifest, version, keys } = commit;
    if heads.len() != keys.len() {
        return Err(RefError::Platform("heads"));
    }
    for (f, h) in manifest.files.iter_mut().zip(heads) {
        let st = match h {
            Head::Absent => return Err(RefError::Missing(f.sha256.clone())),
            Head::Error => return Err(RefError::Platform("head")),
            Head::Present(st) => st,
        };
        if st.size != f.bytes || st.sha256.map(hex::encode).as_deref() != Some(f.sha256.as_str()) {
            return Err(RefError::Mismatch(f.sha256.clone()));
        }
        if !st.clean {
            return Err(RefError::Attributes(f.sha256.clone()));
        }
        if !reference_label_ok(&st.etag, MAX_ETAG_BYTES) {
            return Err(RefError::EntityTag(f.sha256.clone()));
        }
        f.etag = Some(st.etag.clone());
    }
    let bytes = manifest.to_bytes();
    // The pinned manifest is still a valid one, within the staging bound.
    if bytes.len() > MANIFEST_MAX_BYTES {
        return Err(RefError::Request("pinned manifest"));
    }
    FilesManifest::parse_unbound(&bytes, version).map_err(|_| RefError::Request("pinned manifest"))?;
    let package = FilesManifest::package_of(&bytes);
    Ok(Pinned { bytes, package, manifest })
}

/// The `commit` answer: the package, its manifest key and each pinned file.
pub fn commit_reply(p: &Pinned) -> CommitReply {
    CommitReply {
        rule: STAGING_RULE.into(),
        package_sha256: p.package.clone(),
        manifest_key: manifest_key(&p.package),
        manifest_bytes: p.bytes.len() as u64,
        files: p
            .manifest
            .files
            .iter()
            .map(|f| PinnedFile {
                name: f.name.clone(),
                sha256: f.sha256.clone(),
                bytes: f.bytes,
                key: f.key.clone(),
                etag: f.etag.clone().unwrap_or_default(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::admission::{Stored, minted_from_reply};
    use slop_ninja_artifact_check::files_manifest::{
        FileEntry, REFERENCE_BUCKET, REFERENCE_PROFILE_ID_BYTES, reference_file_key,
    };
    use slop_ninja_r2_presign::layout;

    fn digest(i: usize) -> String {
        format!("{:064x}", i + 1)
    }

    /// An unpinned manifest of the spec's file table at the given lengths.
    fn manifest(version: Version) -> FilesManifest {
        let class = version.classes().iter().find(|c| version.serves(c)).unwrap();
        let files = spec::ROLES
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let (bytes, sha) = if r.name == spec::GRAPH_FILE {
                    (class.bytes, digest(i))
                } else if r.scorer_role == "tokenizer" {
                    (spec::TOKENIZER_BYTES, spec::TOKENIZER_SHA256.to_owned())
                } else {
                    (1_000 + i as u64, digest(i))
                };
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
        FilesManifest {
            schema: slop_ninja_artifact_check::files_manifest::FILES_SCHEMA_V2.into(),
            profile_id: format!("{}/{}", version.id(), class.id),
            contract: spec::CONTRACT_ID.into(),
            graph: spec::GRAPH_FILE.into(),
            files,
        }
    }

    fn present(f: &FileEntry, tag: &str) -> Head {
        Head::Present(Stored { size: f.bytes, sha256: parse_hex32(&f.sha256), etag: tag.into(), clean: true })
    }

    #[test]
    fn the_layouts_agree() {
        assert_eq!(slop_ninja_artifact_check::files_manifest::REFERENCE_FILES_PREFIX, layout::REFERENCE_PREFIX);
        let d = digest(3);
        assert_eq!(reference_file_key(&d), layout::reference_key(&parse_hex32(&d).unwrap()).as_str());
        assert_eq!(REFERENCE_BUCKET, "admission");
        assert!(!layout::REFERENCE_PREFIX.starts_with(layout::ADMISSION_PREFIX));
    }

    #[test]
    fn the_manifest_bound_is_the_longest_pinned_reference_manifest() {
        let mut m = manifest(Version::V2);
        m.profile_id = "x".repeat(REFERENCE_PROFILE_ID_BYTES);
        for f in &mut m.files {
            f.bytes = u64::MAX;
            f.etag = Some("e".repeat(MAX_ETAG_BYTES));
        }
        let n = m.to_bytes().len();
        assert!(n <= MANIFEST_MAX_BYTES, "{n} > {MANIFEST_MAX_BYTES}");
        // Tight: the count only adds a comma after the last file.
        assert_eq!(n + 1, MANIFEST_MAX_BYTES);
        let mut long = manifest(Version::V2);
        long.profile_id = "x".repeat(REFERENCE_PROFILE_ID_BYTES + 1);
        assert_eq!(commit_request(&long.to_bytes(), Version::V2.id()).unwrap_err(), RefError::Request("profile_id"));
        let mut quoted = manifest(Version::V2);
        quoted.profile_id = "a\"b".into();
        assert_eq!(commit_request(&quoted.to_bytes(), Version::V2.id()).unwrap_err(), RefError::Request("profile_id"));
    }

    #[test]
    fn urls_mint_reference_entries_for_the_whole_package() {
        let files: Vec<Value> = (0..MAX_FILES).map(|i| json!({"sha256": digest(i), "bytes": 1_000 + i as u64})).collect();
        let body = serde_json::to_vec(&json!({"files": files})).unwrap();
        let entries = mint_entries(&body).unwrap();
        assert_eq!(entries.len(), MAX_FILES);
        let total: u64 = entries.iter().map(|e| e.len).sum();
        let valid = policy::valid_secs(total).unwrap();
        assert!(entries.iter().all(|e| e.op == mint::Op::Reference && e.cohort == 0 && e.commitment == [0; 32] && e.valid_secs == valid));
        let minted: Vec<Minted> = entries
            .iter()
            .map(|e| Minted { url: "https://x".into(), checksum: put_headers(e.len, &e.sha256).2.as_str().into(), signed_at: 1, valid_secs: valid })
            .collect();
        let reply = urls_reply(&entries, &minted).unwrap();
        assert_eq!(reply.uploads[2].sha256, digest(2));
        assert_eq!(reply.uploads[2].kind, UPLOAD_KIND);
        assert_eq!(reply.uploads[2].if_none_match, "*");
        // The answer's bytes parse as the client's shape.
        let wire: UrlsReply = serde_json::from_slice(&serde_json::to_vec(&reply).unwrap()).unwrap();
        assert_eq!(wire, reply);
        let bad = Minted { checksum: "AAAA".into(), ..minted[0].clone() };
        assert_eq!(urls_reply(&entries[..1], &[bad]), Err(RefError::Platform("signer checksum")));
    }

    #[test]
    fn the_urls_body_bound_is_the_largest_compact_body() {
        let files: Vec<Value> = (0..MAX_FILES).map(|i| json!({"sha256": digest(i), "bytes": u64::MAX})).collect();
        let body = serde_json::to_vec(&json!({"files": files})).unwrap();
        // Tight: the bound counts a comma after the last entry too.
        assert_eq!(body.len() + 1, URLS_BODY_BYTES);
        let mut long = body.clone();
        long.resize(URLS_BODY_BYTES + 1, b' ');
        assert_eq!(mint_entries(&long), Err(RefError::Request("body")));
    }

    #[test]
    fn urls_refuse_what_the_signer_would_and_more() {
        let one = |sha: &str, bytes: u64| serde_json::to_vec(&json!({"files": [{"sha256": sha, "bytes": bytes}]})).unwrap();
        assert_eq!(mint_entries(&one(&digest(0), 0)), Err(RefError::Request("bytes")));
        assert_eq!(mint_entries(&one(&digest(0), max_file_len() + 1)), Err(RefError::Request("bytes")));
        assert!(mint_entries(&one(&digest(0), max_file_len())).is_ok());
        assert_eq!(mint_entries(&one(&"ab".repeat(32).to_uppercase(), 5)), Err(RefError::Request("sha256")));
        assert_eq!(mint_entries(&one("ab", 5)), Err(RefError::Request("sha256")));
        let dup = serde_json::to_vec(&json!({"files": [{"sha256": digest(0), "bytes": 5}, {"sha256": digest(0), "bytes": 5}]})).unwrap();
        assert_eq!(mint_entries(&dup), Err(RefError::Request("duplicate file")));
        let none = serde_json::to_vec(&json!({"files": []})).unwrap();
        assert_eq!(mint_entries(&none), Err(RefError::Request("file count")));
        let many: Vec<Value> = (0..=MAX_FILES).map(|i| json!({"sha256": digest(i), "bytes": 5})).collect();
        let many = serde_json::to_vec(&json!({"files": many})).unwrap();
        assert!(mint_entries(&many).is_err());
        let extra = serde_json::to_vec(&json!({"files": [{"sha256": digest(0), "bytes": 5, "key": "admission/v2/x"}]})).unwrap();
        assert_eq!(mint_entries(&extra), Err(RefError::Request("body")));
    }

    #[test]
    fn commit_pins_every_tag_and_names_the_pinned_package() {
        let v = Version::V2;
        let m = manifest(v);
        let body = m.to_bytes();
        let c = commit_request(&body, v.id()).unwrap();
        assert_eq!(c.keys, m.files.iter().map(|f| f.key.clone()).collect::<Vec<_>>());
        let heads: Vec<Head> = m.files.iter().enumerate().map(|(i, f)| present(f, &format!("{:032x}", i))).collect();
        let pinned = pin(c, &heads).unwrap();
        let (bytes, package) = (pinned.bytes.clone(), pinned.package.clone());
        let bad: Vec<Head> = m.files.iter().map(|f| present(f, "a\"b")).collect();
        assert_eq!(pin(commit_request(&body, v.id()).unwrap(), &bad).unwrap_err(), RefError::EntityTag(m.files[0].sha256.clone()));
        assert_ne!(package, FilesManifest::package_of(&body), "the package is the pinned manifest's");
        let parsed = FilesManifest::parse(&bytes, &package, v).unwrap();
        assert_eq!(parsed, pinned.manifest);
        assert!(parsed.files.iter().enumerate().all(|(i, f)| f.etag.as_deref() == Some(format!("{:032x}", i).as_str())));
        let reply = commit_reply(&pinned);
        assert_eq!(reply.manifest_key, format!("artifacts/packages/{package}.json"));
        assert_eq!(reply.manifest_bytes, bytes.len() as u64);
        assert_eq!(reply.files.len(), MAX_FILES);
        assert_eq!(reply.files[1].etag, format!("{:032x}", 1));
        let wire: CommitReply = serde_json::from_slice(&serde_json::to_vec(&reply).unwrap()).unwrap();
        assert_eq!(wire, reply);
    }

    #[test]
    fn commit_refuses_other_locations_tags_and_specs() {
        let v = Version::V2;
        let base = manifest(v);
        let with = |f: &dyn Fn(&mut FilesManifest)| {
            let mut m = base.clone();
            f(&mut m);
            m.to_bytes()
        };
        let eval = with(&|m| m.files[0].bucket = "eval".into());
        assert_eq!(commit_request(&eval, v.id()).unwrap_err(), RefError::Request("location"));
        let miner_key = with(&|m| m.files[1].key = format!("admission/v2/c1/{}/f/{}", "00".repeat(32), m.files[1].sha256));
        assert_eq!(commit_request(&miner_key, v.id()).unwrap_err(), RefError::Request("location"));
        let tagged = with(&|m| m.files[2].etag = Some("abc".into()));
        assert_eq!(commit_request(&tagged, v.id()).unwrap_err(), RefError::Request("location"));
        assert_eq!(commit_request(&base.to_bytes(), "v2").unwrap_err(), RefError::Request("spec"));
        assert_eq!(commit_request(b"{}", v.id()).unwrap_err(), RefError::Request("manifest"));
        let mut huge = base.to_bytes();
        huge.resize(COMMIT_BODY_BYTES + 1, b' ');
        assert_eq!(commit_request(&huge, v.id()).unwrap_err(), RefError::Request("body"));
    }

    /// The heaviest `commit` at its bounds (a body whose pinned manifest is
    /// exactly the manifest bound) under `Mode::Strict` in exactly the
    /// route's footprint (`--features arena-witness`, one test thread).
    #[cfg(feature = "arena-witness")]
    #[test]
    fn arena_witness_reference_commit_fits_its_footprint() {
        use crate::arena_routes::{R_REFERENCE, REFERENCE_CONTROL_BYTES};
        use crate::arena_witness::witness;
        let v = Version::V2;
        let mut m = manifest(v);
        m.profile_id = "x".repeat(REFERENCE_PROFILE_ID_BYTES);
        let body = m.to_bytes();
        let tag = "e".repeat(MAX_ETAG_BYTES);
        let heads: Vec<Head> = m.files.iter().map(|f| present(f, &tag)).collect();
        let run = || {
            let c = commit_request(&body, v.id()).unwrap();
            let pinned = pin(c, &heads).unwrap();
            assert!(pinned.bytes.len() <= MANIFEST_MAX_BYTES);
            pinned.bytes.len() + serde_json::to_vec(&commit_reply(&pinned)).unwrap().len()
        };
        run();
        witness(R_REFERENCE.route(), REFERENCE_CONTROL_BYTES, || {
            let copy = body.to_vec();
            let c = commit_request(&copy, v.id()).unwrap();
            drop(copy);
            let pinned = pin(c, &heads).unwrap();
            pinned.bytes.len() + serde_json::to_vec(&commit_reply(&pinned)).unwrap().len()
        });
    }

    /// The signer's reply to `entries` with every URL at `MAX_URL_LEN` and
    /// the longest signing time and life.
    fn longest_signer_reply(entries: &[mint::Entry]) -> Vec<u8> {
        let url = format!("https://{}", "a".repeat(MAX_URL_LEN - "https://".len()));
        let mut out = vec![0u8; mint::reply_max(entries.len())];
        let mut at = mint::encode_reply_head(&mut out, entries.len() as u8).unwrap();
        for e in entries {
            let checksum = put_headers(e.len, &e.sha256).2;
            let entry = mint::ReplyEntry {
                status: mint::Status::Ok,
                signed_at: u64::MAX,
                valid_secs: u32::MAX,
                content_length: e.len,
                checksum: checksum.as_str(),
                url: &url,
            };
            at = mint::encode_reply_entry(&mut out, at, &entry).unwrap();
        }
        out.truncate(at);
        out
    }

    /// The `urls` answer bound is the longest compact answer: every field at
    /// its format bound, URLs at `MAX_URL_LEN` (no byte escaped).
    #[test]
    fn the_urls_reply_bound_is_the_longest_compact_answer() {
        let upload = Upload {
            kind: UPLOAD_KIND.into(),
            sha256: digest(0),
            len: u64::MAX,
            url: format!("https://{}", "a".repeat(MAX_URL_LEN - "https://".len())),
            content_length: u64::MAX.to_string(),
            if_none_match: IF_NONE_MATCH.into(),
            checksum_sha256: "A".repeat(44),
            signed_at: u64::MAX,
            valid_secs: u32::MAX,
        };
        let reply = UrlsReply { rule: STAGING_RULE.into(), uploads: vec![upload; MAX_FILES] };
        let n = serde_json::to_vec(&reply).unwrap().len();
        // Tight: the count only adds a comma after the last upload.
        assert_eq!(n + 1, URLS_REPLY_BYTES);
        // A real presigned URL's characters need no JSON escape either.
        let entries = mint_entries(&serde_json::to_vec(&json!({"files": [{"sha256": digest(0), "bytes": 5}]})).unwrap()).unwrap();
        let minted = minted_from_reply(&longest_signer_reply(&entries), &entries).unwrap();
        assert_eq!(serde_json::to_string(&minted[0].url).unwrap().len(), minted[0].url.len() + 2);
    }

    /// The heaviest `urls` at its bounds (the whole file table, each file
    /// at the signer's length bound, every URL at `MAX_URL_LEN`) under
    /// `Mode::Strict` in exactly the route's footprint: the body, its
    /// entries, the signer's reply and its decode, the answer and its
    /// serialization.
    #[cfg(feature = "arena-witness")]
    #[test]
    fn arena_witness_reference_urls_fits_its_footprint() {
        use crate::arena_routes::{R_REFERENCE, REFERENCE_CONTROL_BYTES};
        use crate::arena_witness::witness;
        let files: Vec<Value> = (0..MAX_FILES).map(|i| json!({"sha256": digest(i), "bytes": max_file_len()})).collect();
        let body = serde_json::to_vec(&json!({"files": files})).unwrap();
        assert!(body.len() <= URLS_BODY_BYTES);
        let entries = mint_entries(&body).unwrap();
        let reply = longest_signer_reply(&entries);
        assert!(reply.len() <= mint::reply_max(MAX_FILES));
        let run = |body: &[u8], reply: &[u8]| {
            let entries = mint_entries(body).unwrap();
            let minted = minted_from_reply(reply, &entries).unwrap();
            let answer = urls_reply(&entries, &minted).unwrap();
            let bytes = serde_json::to_vec(&answer).unwrap();
            assert!(bytes.len() < URLS_REPLY_BYTES);
            bytes.len()
        };
        run(&body, &reply);
        witness(R_REFERENCE.route(), REFERENCE_CONTROL_BYTES, || {
            let body = body.to_vec();
            let reply = reply.to_vec();
            run(&body, &reply)
        });
    }

    #[test]
    fn pin_refuses_absent_changed_and_dirty_files() {
        let v = Version::V2;
        let m = manifest(v);
        let body = m.to_bytes();
        let ok: Vec<Head> = m.files.iter().map(|f| present(f, "t")).collect();
        let run = |i: usize, h: Head| {
            let mut heads = ok.clone();
            heads[i] = h;
            pin(commit_request(&body, v.id()).unwrap(), &heads).unwrap_err()
        };
        let f = &m.files[1];
        assert_eq!(run(1, Head::Absent), RefError::Missing(f.sha256.clone()));
        assert_eq!(run(1, Head::Error), RefError::Platform("head"));
        let short = Head::Present(Stored { size: f.bytes - 1, sha256: parse_hex32(&f.sha256), etag: "t".into(), clean: true });
        assert_eq!(run(1, short), RefError::Mismatch(f.sha256.clone()));
        let other = Head::Present(Stored { size: f.bytes, sha256: Some([9; 32]), etag: "t".into(), clean: true });
        assert_eq!(run(1, other), RefError::Mismatch(f.sha256.clone()));
        let unverified = Head::Present(Stored { size: f.bytes, sha256: None, etag: "t".into(), clean: true });
        assert_eq!(run(1, unverified), RefError::Mismatch(f.sha256.clone()));
        let dirty = Head::Present(Stored { size: f.bytes, sha256: parse_hex32(&f.sha256), etag: "t".into(), clean: false });
        assert_eq!(run(1, dirty), RefError::Attributes(f.sha256.clone()));
        let bad_tag = Head::Present(Stored { size: f.bytes, sha256: parse_hex32(&f.sha256), etag: "a\"b".into(), clean: true });
        let e = run(1, bad_tag);
        assert_eq!(e, RefError::EntityTag(f.sha256.clone()));
        assert_eq!(e.status(), 409);
        assert_eq!(pin(commit_request(&body, v.id()).unwrap(), &ok[1..]).unwrap_err(), RefError::Platform("heads"));
    }
}
