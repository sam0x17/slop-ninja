//! The dev session's package (`session/dev-package/`): what the detect
//! Worker's dev build loads from the local package bucket into the stub
//! scorer. Its manifest, files and the detect Worker's `[env.dev]` vars must
//! agree, or the lane refuses the load: the manifest's SHA-256 is
//! `PACKAGE_SHA256`, every file names the bucket label `PACKAGE_BUCKET_LABEL`
//! and the entity tag local R2 gives its bytes, the spec is one the scorer
//! serves, and the build identity is the stub's. Then the package loads and
//! scores through the scorer client, step by step as the lane drives it.

use md5::{Digest as _, Md5};
use serde_json::Value;
use slop_ninja_artifact_check::files_manifest::FilesManifest;
use slop_ninja_artifact_check::spec::{PART_BYTES, ROLES, Version};
use slop_ninja_dev_http::Call;
use slop_ninja_dev_stub_scorer::instance::{Instance, Switches};
use slop_ninja_dev_stub_scorer::result::sha;
use slop_ninja_scorer_client::{self as sc, Ours, Slot};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn devtools() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("devtools").to_path_buf()
}

/// The detect Worker's `[env.dev.vars]`.
fn detect_dev_vars() -> BTreeMap<String, String> {
    let w = fs::read_to_string(devtools().join("../detect-worker/wrangler.toml")).expect("detect wrangler.toml");
    let mut table = String::new();
    let mut out = BTreeMap::new();
    for l in w.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if l.starts_with('[') {
            table = l.trim_matches(|c| c == '[' || c == ']').to_owned();
            continue;
        }
        if table != "env.dev.vars" {
            continue;
        }
        let (k, v) = l.split_once('=').expect("key = value");
        let v = v.trim().strip_prefix('"').and_then(|v| v.strip_suffix('"')).expect("a string value");
        out.insert(k.trim().to_owned(), v.to_owned());
    }
    out
}

#[test]
fn the_dev_package_agrees_with_the_detect_dev_env_and_loads() {
    let dir = devtools().join("session/dev-package");
    let vars = detect_dev_vars();
    let var = |k: &str| vars.get(k).unwrap_or_else(|| panic!("[env.dev.vars] {k}")).as_str();

    let key = var("PACKAGE_MANIFEST_KEY");
    let name = key.strip_prefix("dev-package/").expect("the manifest sits under dev-package/");
    let manifest = fs::read(dir.join(name)).expect("the manifest");
    let pkg = sha(&manifest);
    assert_eq!(var("PACKAGE_SHA256"), pkg, "PACKAGE_SHA256 is the manifest's SHA-256");
    let spec_digest = Version::V4.spec_sha256();
    assert_eq!(var("PACKAGE_SPEC_SHA256"), spec_digest, "PACKAGE_SPEC_SHA256 is spec v4's digest");
    let stub = Switches::default();
    assert_eq!(var("SCORER_BINARY_SHA256"), stub.binary_sha256, "the stub's build identity");
    assert_eq!(var("SCORER_ORT_SHA256"), stub.ort_sha256, "the stub's build identity");
    assert_ne!(var("PUBLISHED_PACKAGE_DIGEST"), pkg, "the published digest is its own value");

    // The file table of the spec, in order, each file in the dev bucket at
    // its key with the MD5 entity tag local R2 gives it.
    let m: FilesManifest = serde_json::from_slice(&manifest).expect("a files manifest v2");
    assert_eq!(m.files.len(), ROLES.len());
    let mut on_disk: Vec<String> = fs::read_dir(&dir)
        .expect("dev-package/")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != name)
        .collect();
    on_disk.sort();
    let mut named: Vec<String> = m.files.iter().map(|f| f.name.clone()).collect();
    named.sort();
    assert_eq!(on_disk, named, "every file on disk is in the manifest, and no other");
    for (f, role) in m.files.iter().zip(ROLES.iter()) {
        assert_eq!((f.name.as_str(), f.role.as_str()), (role.name, role.scorer_role));
        let bytes = fs::read(dir.join(&f.name)).expect("a dev package file");
        assert_eq!(f.bytes, bytes.len() as u64, "{}", f.name);
        assert!(f.bytes > 0 && f.bytes < PART_BYTES, "{}: one chunk", f.name);
        assert_eq!(f.sha256, sha(&bytes), "{}", f.name);
        assert_eq!(f.bucket, var("PACKAGE_BUCKET_LABEL"), "{}", f.name);
        assert_eq!(f.key, format!("dev-package/{}", f.name), "{}", f.name);
        assert_eq!(f.etag.as_deref(), Some(hex::encode(Md5::digest(&bytes)).as_str()), "{}", f.name);
        assert!(!String::from_utf8_lossy(&bytes).contains("sn-stub:"), "no marker in a package file");
    }

    // The load, as the lane drives it.
    let mut i = Instance::new(0);
    let h = i.handle(&Call::new("GET", "/healthz"), 1, false).body_json();
    let spec = sc::spec_sent(&h, Some(&spec_digest)).expect("spec served");
    let r = i.handle(&Call::new("POST", &sc::prepare_path(&pkg, None, spec)).body(manifest.clone()), 2, false);
    let missing = sc::prepare_reply(r.status, &r.body_json()).expect("prepared");
    assert_eq!(missing.len(), m.files.len(), "one chunk per file");
    for (index, offset) in missing {
        let c = i.chunk(&sc::chunk_path(index, offset), m.files[index].bytes - offset, 3);
        assert!(matches!(c.status, 200 | 201), "{}", c.status);
    }
    let l = i.handle(&Call::new("POST", &sc::load_path(&pkg, None, spec)), 4, false);
    assert_eq!(sc::load_reply(l.status, &l.body_json()), None);
    let h = i.handle(&Call::new("GET", "/healthz"), 5, false).body_json();
    assert_eq!(sc::slot(&h, &pkg, None, spec), Slot::Ours(Ours::Ready));
    let docs = vec![(1u32, format!("sn-stub:band=strong {}", "word ".repeat(200)), String::new())];
    let s = i.handle(&Call::new("POST", "/v1/score").body(sc::score_body(&pkg, None, &docs, true)), 6, false);
    assert_eq!(s.status, 200);
    assert_eq!(s.body_json()["package"], Value::String(pkg));
}
