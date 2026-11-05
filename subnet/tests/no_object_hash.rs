//! Lint: nothing on our side hashes model bytes after the miner's commit.
//!
//! The user's rule (2026-11-04): every artifact is hashed once. The miner
//! hashes each file once at `pack` to build the header its commitment binds;
//! R2 verifies each uploaded object against that committed SHA-256 at
//! ingest. After that, admission compares R2's stored checksum with the
//! header, and every reader of the files takes the header's digests: the
//! spec's hash-free files check, the role binding and load guards, the
//! upload Worker's header check and its routes, the signer, the scorer
//! Container's placement and load path, and the publisher's publication at
//! scoring start (its copies are R2's: a binding stream R2 verifies, or a
//! server-side `CopyObject`, checked by R2's stored checksum or etag).
//!
//! This scan fails when a hash call appears in any of those paths, the
//! cohort authority's admission and its scorer loads included. Small
//! records may be hashed and are named here one by one: the files manifest
//! (the package digest), the canary body, a session-creation body, the
//! control document, the chain's runtime-upgrade marker value, the profile
//! roster's serialized list, a header
//! record's content digest (domain hashes over records, not SHA-256 calls
//! on bytes), the seal's digest of a small record of its own (a header
//! record that no upload verified, a reference's or an operator-roster
//! entrant's, or the batch index; an admitted entrant's artifact digest is
//! the one admission recorded), the miner's upload-host pin (a host
//! name's digest, decision D1). The one remaining hash over model bytes is the graph allow-list's
//! structure digest (`onnx.rs`). Decision D5 (approved 2026-11-05) replaces
//! it with a byte comparison against each class's pinned reference
//! structure stream in the next artifact spec version; until that spec
//! ships, the scan pins that it is exactly one hasher so no second one
//! joins it.
//!
//! Test code (after a file's first `#[cfg(test)]`) and comments are skipped.
//! Run with `cargo test --test no_object_hash`.

use std::path::{Path, PathBuf};

/// Calls that hash bytes, or run a check that does.
const HASHES: &[&str] = &[
    "Sha256",
    "sha2::",
    "sha256_hex(",
    "sha(",
    "object_sha256(",
    "ArtifactReader",
    "VerifiedArtifact::from_bytes",
    "header_for_dir(",
    "check_dir_for(",
    "check_dir_with_header(",
    "sha256_file(",
    "file_sha256(",
    "hash_file(",
    "verify_object_for(",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn production(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let end = text.find("#[cfg(test)]").unwrap_or(text.len());
    text[..end]
        .lines()
        .map(|l| match l.find("//") {
            Some(at) => &l[..at],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The body of the function whose signature starts with `sig`, through its
/// matching closing brace.
fn body<'a>(text: &'a str, sig: &str) -> &'a str {
    let at = text.find(sig).unwrap_or_else(|| panic!("no function {sig:?}"));
    let open = at + text[at..].find('{').expect("a body");
    let mut depth = 0usize;
    for (i, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &text[at..open + i + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced body of {sig:?}")
}

/// `text` with the bodies of the named allowed functions removed.
fn without(text: &str, allowed: &[&str]) -> String {
    let mut out = text.to_owned();
    for sig in allowed {
        let b = body(&out, sig).to_owned();
        out = out.replacen(&b, "", 1);
    }
    out
}

fn violations(what: &str, text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (n, line) in text.lines().enumerate() {
        for h in HASHES {
            if line.contains(h) {
                found.push(format!("{what}:{}: {h} in `{}`", n + 1, line.trim()));
            }
        }
    }
    found
}

#[test]
fn nothing_after_the_commit_hashes_model_bytes() {
    let r = root();
    let inference = r.join("../inference/a-container-serve/src");
    let mut report = Vec::new();
    let mut scan = |what: &str, text: String| report.extend(violations(what, &text));

    // The spec's hash-free files check, header check and guards: whole files.
    scan("artifact-check/files.rs", production(&r.join("crates/artifact-check/src/files.rs")));
    scan("artifact-check/guard.rs", production(&r.join("crates/artifact-check/src/guard.rs")));
    scan("artifact-check/admission.rs", production(&r.join("crates/artifact-check/src/admission.rs")));
    // The files manifest: only its package digest (a small record).
    let fm = production(&r.join("crates/artifact-check/src/files_manifest.rs"));
    scan("artifact-check/files_manifest.rs", without(&fm, &["fn package_digest(b: &[u8])"]));
    // The role binding compares the header's digests; the profile roster's
    // serialized list is the one small record it hashes.
    let roles = production(&r.join("crates/artifact-check/src/roles.rs"));
    let binding = body(&roles, "pub fn check_roles_with(");
    scan("artifact-check/roles.rs check_roles_with", binding.to_owned());
    // The graph allow-list's structure digest: decision D5, exactly one hasher.
    let onnx = production(&r.join("crates/artifact-check/src/onnx.rs"));
    assert_eq!(onnx.matches("Sha256::new()").count(), 1, "onnx.rs must hold exactly the one structure hasher (D5)");

    // The signer: the canary body (64 bytes of our own) is the one record it hashes.
    let mint = production(&r.join("r2-signer/src/mint.rs"));
    let canary = mint.matches("Sha256::digest(layout::canary_body(").count();
    assert_eq!(canary, 1, "the signer hashes only the canary body");
    let mint_rest = mint.replace("Sha256::digest(layout::canary_body(&e.commitment))", "").replace("use sha2::{Digest as _, Sha256};", "");
    scan("r2-signer/mint.rs", mint_rest);
    scan("r2-signer/cf.rs", production(&r.join("r2-signer/src/cf.rs")));

    // The cohort authority (the eval Worker): it records objects from R2's
    // stored size, checksum and entity tag, recomputes a header's content
    // from the returned table (a domain hash over a small record), and loads
    // a scorer by ranged reads pinned to the recorded tag. The small records
    // it hashes are named: a session-creation body (one `SubmissionUpload`
    // record, `create_inner`), the control document (`sha_hex_of`) and the
    // canary body (`canary`, 64 bytes of its own); the files manifest's
    // package digest is the artifact-check one above.
    let sha2_use = "use sha2::{Digest as _, Sha256};";
    let adm = production(&r.join("coordinator/src/eval/admission.rs")).replace(sha2_use, "");
    scan("coordinator/eval/admission.rs", without(&adm, &["fn create_inner(", "fn sha_hex_of("]));
    let cfa = production(&r.join("coordinator/src/cf_admission.rs")).replace(sha2_use, "");
    scan("coordinator/cf_admission.rs", without(&cfa, &["async fn canary("]));
    // The seal (review round 2): an admitted entrant's artifact digest is
    // the header digest admission recorded; the one hash in the module is a
    // header record no upload verified (a reference's or an operator-roster
    // entrant's, re-encoded from its files manifest), in `record_digest`.
    let v3 = production(&r.join("coordinator/src/eval/v3.rs"));
    assert!(body(&v3, "pub fn artifact_digests(").contains("Some(d) => d,"), "the recorded digest is taken as is");
    scan("coordinator/eval/v3.rs", without(&v3, &["fn record_digest("]));
    let lanes = production(&r.join("coordinator/src/cf_lanes.rs"));
    scan("coordinator/cf_lanes.rs load", body(&lanes, "    async fn load(").to_owned());
    scan("coordinator/eval/scorer_client.rs", production(&r.join("coordinator/src/eval/scorer_client.rs")));

    // The upload Worker: its header route decodes the header record and
    // derives the content digest through `check_header_bytes` (scanned
    // above). Its one hash is the digest of the chain's runtime-upgrade
    // marker value (a small storage value, never an uploaded byte).
    for f in ["cf.rs", "front.rs", "routes.rs", "lib.rs"] {
        scan(&format!("admission-worker/{f}"), production(&r.join("admission-worker/src").join(f)));
    }
    let registry = production(&r.join("admission-worker/src/registry.rs"));
    assert_eq!(registry.matches("Sha256::digest(").count(), 1, "the registry hashes only the marker value");
    scan("admission-worker/registry.rs", without(&registry, &["pub fn marker_digest("]));

    // The signer's copy route: no hash at all.
    scan("r2-signer/mint.rs copy", body(&mint, "pub fn copy(").to_owned());
    // `X-Amz-Content-Sha256` is a SigV4 query parameter's name (its value is
    // `UNSIGNED-PAYLOAD`), not a hash.
    let copy = production(&r.join("crates/r2-presign/src/copy.rs")).replace("X-Amz-Content-Sha256", "X-Amz-Content-Payload");
    scan("r2-presign/copy.rs", copy);

    // The publisher's publication at scoring start: the module, the
    // closed-cohort settlement, the stream copy and the server-side copy.
    scan("publisher/artifacts.rs", production(&r.join("publisher/src/artifacts.rs")));
    let publish = production(&r.join("publisher/src/publish.rs"));
    for sig in ["    fn settle(", "    async fn scoring_start_record(", "    async fn retry_artifacts("] {
        scan(&format!("publisher/publish.rs {sig}"), body(&publish, sig).to_owned());
    }
    let cf = production(&r.join("publisher/src/cf.rs"));
    for sig in ["    async fn copy_from(", "impl crate::artifacts::ServerCopy for SignerCopy"] {
        scan(&format!("publisher/cf.rs {sig}"), body(&cf, sig).to_owned());
    }

    // The scorer: placement and the load path.
    let artifact = production(&inference.join("artifact.rs"));
    scan("a-container-serve/artifact.rs", without(&artifact, &["pub fn pack("]));
    let engine = production(&inference.join("engine.rs"));
    for sig in [
        "pub fn guard_verified(",
        "pub fn guard_any(",
        "pub fn admit(",
        "    pub fn load(",
        "    pub fn load_without_admission(",
    ] {
        scan(&format!("a-container-serve/engine.rs {sig}"), body(&engine, sig).to_owned());
    }
    assert!(body(&engine, "pub fn guard_any(").contains("guard_verified(dir, contract_sha256)"), "the load guard takes the header digest");
    let scorer = production(&inference.join("bin/a_scorer.rs"));
    scan("a_scorer.rs load", body(&scorer, "fn load(cfg: &Config").to_owned());
    // Its routes: the binary's own build identity (the scorer binary and the
    // ONNX Runtime library, not model bytes) is the one hash it computes.
    let routes = without(&scorer, &["fn file_sha256("]).replace("file_sha256(", "build_identity(");
    let routes = routes.replace("use sha2::{Digest, Sha256};", "");
    scan("a_scorer.rs routes", routes);

    assert!(report.is_empty(), "hashing after the commit (the miner's pack hash is the only one):\n{}", report.join("\n"));
}

/// The miner's side of the rule: `pack` hashes each file exactly once,
/// in the pass that writes it (`artifact-check` `write_files_once`), and no
/// other miner command hashes a model byte: `commit`, `upload`, `repair`
/// and `check` read the object's header (a range read) and take its
/// digests; R2 verifies each uploaded file against them. The SHA-256s the
/// miner itself computes are the signed body digest of the fixed-size
/// session-creation envelope (`create_session`) and the digest of a
/// presigned URL's host name for the trust file's upload-host pin
/// (`pinned_host_digest_ok`, decision D1); the header digest
/// (`VerifiedHeader::from_bytes`, a file table of at most 1 MiB) is a
/// record, not a model file.
#[test]
fn the_miner_hashes_each_file_once_at_pack() {
    let r = root();
    let fs = production(&r.join("crates/artifact-check/src/fs.rs"));
    let once = body(&fs, "pub fn write_files_once(");
    assert_eq!(once.matches("Sha256::new()").count(), 1, "write_files_once holds exactly one hasher");
    // The hasher is per file, inside the loop that writes each file once,
    // and nothing reads the written object back.
    let per_file = body(once, "for (name, role, path) in &sorted");
    assert!(per_file.contains("Sha256::new()") && per_file.contains("h.update(") && per_file.contains("w.write_all("));
    assert_eq!(once.matches("h.update(").count(), 1, "each chunk is hashed once, as it is written");
    let mut report = violations("write_files_once", &once.replace("Sha256::new()", "").replace("h.update(", ""));
    report.extend(violations("write_object_once", body(&fs, "pub fn write_object_once(")));
    report.extend(violations("spec_files_in", body(&fs, "pub fn spec_files_in(")));

    // The miner crate: no hash at all outside the envelope digest.
    let miner = r.join("crates/miner/src");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&miner)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    files.sort();
    assert!(files.len() >= 10, "the miner's sources: {files:?}");
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut text = production(path);
        if name == "direct.rs" {
            // The upload-host pin (decision D1): a host name's digest.
            let pin = body(&text, "fn pinned_host_digest_ok(").to_owned();
            assert_eq!(pin.matches("object_sha256(host.as_bytes())").count(), 1, "the host pin");
            text = text.replacen(&pin, "", 1);
        }
        if name == "client.rs" {
            let create = body(&text, "pub async fn create_session(").to_owned();
            assert_eq!(create.matches("object_sha256(&bytes)").count(), 1, "the envelope digest");
            text = text.replacen(&create, &create.replacen("object_sha256(&bytes)", "", 1), 1);
            text = text.replace("Digest, Record, object_sha256}", "Digest, Record}");
        }
        report.extend(violations(&format!("miner/{name}"), &text));
        for reader in ["verify_file", "verify_object", "ArtifactReader", "VerifiedArtifact"] {
            if text.contains(reader) {
                report.push(format!("miner/{name}: {reader} re-reads the object to hash it"));
            }
        }
    }
    // pack goes through the hash-once writer; check through the hash-free check.
    let object = production(&miner.join("object.rs"));
    assert!(body(&object, "pub fn pack(").contains("write_files_once("));
    assert!(body(&object, "pub fn pack_dir(").contains("write_object_once("));
    assert!(production(&miner.join("check.rs")).contains("check_object_for("));
    let manifest = std::fs::read_to_string(r.join("crates/miner/Cargo.toml")).unwrap();
    let deps = &manifest[manifest.find("[dependencies]").unwrap()..manifest.find("[dev-dependencies]").unwrap()];
    assert!(!deps.contains("sha2"), "the miner has no hash dependency of its own");

    assert!(report.is_empty(), "the miner hashes model bytes after pack:\n{}", report.join("\n"));
}

#[test]
fn the_lint_sees_a_hash_and_ignores_comments_and_tests() {
    assert!(!violations("x", "let d = Sha256::digest(&bytes);").is_empty());
    assert!(!violations("x", "let h = sha256_hex(&file);").is_empty());
    assert!(!violations("x", "let r = fs::check_dir_for(dir, v)?;").is_empty());
    assert!(violations("x", "let n = bytes.len();").is_empty());
    let dir = std::env::temp_dir().join(format!("no-object-hash-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("a.rs");
    std::fs::write(&f, "fn a() {} // Sha256 in a comment\n#[cfg(test)]\nfn t() { Sha256::new(); }\n").unwrap();
    assert!(violations("a", &production(&f)).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
    let src = "fn keep() { x(); }\nfn drop_me() { Sha256::new(); }\n";
    assert!(violations("s", &without(src, &["fn drop_me("])).is_empty());
}
