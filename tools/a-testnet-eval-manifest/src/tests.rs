//! Offline tests on synthetic bundles. Every refusal test gives the refused split a
//! correct digest and rows labelled `dev_iter`, so only the guard under test can stop it.
use super::*;

const DEV_ITER: &str = "data/bundles/b1/dev-iter";

struct Repo {
    root: PathBuf,
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn row(id: &str, split: &str, domain: &str, origin: &str, group: &str, population: &str) -> Value {
    let text = format!("Synthetic {origin} document {id} for the plumbing test.");
    let (share, generator) = match origin {
        "human_only" => (0.0, Value::Null),
        "model_only" => (1.0, json!("gen-a")),
        _ => (0.5, json!("gen-b")),
    };
    json!({"schema": ROW_SCHEMA, "id": id, "split": split, "text_sha256": sha256_hex(text.as_bytes()),
        "text": text, "origin": origin, "share": share, "domain": domain, "source_group": group,
        "generator_label": generator, "population": population, "writer_key": "synthetic"})
}

fn default_rows() -> Vec<Value> {
    let mut rows = Vec::new();
    for d in ["alpha", "beta"] {
        for (o, origin) in ORIGINS.iter().enumerate() {
            for i in 0..6 {
                let id = format!("{d}-{o}-{i}");
                rows.push(row(&id, ALLOWED_SPLIT, d, origin, &format!("{d}-g{i}"), "primary"));
            }
        }
    }
    rows.push(row("extra-train-only", ALLOWED_SPLIT, "alpha", "mixed", "alpha-x", "supplementary_train_only"));
    rows
}

impl Repo {
    /// A temporary repository; `tag` must not itself contain a refused token.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("a-testnet-eval-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("data")).unwrap();
        Repo { root }
    }

    /// Write a bundle split directory and return its allow-list source.
    fn bundle(&self, dir: &str, rows: &[Value], manifest_split: &str) -> Source {
        let d = self.root.join(dir);
        fs::create_dir_all(&d).unwrap();
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        fs::write(d.join("rows.jsonl"), &body).unwrap();
        let sha = sha256_hex(body.as_bytes());
        let m = json!({"schema": BUNDLE_MANIFEST_SCHEMA, "split": manifest_split, "sealed": false,
            "test_opened": false, "rows": rows.len(), "files": {"rows.jsonl": sha}});
        let mb = serde_json::to_vec(&m).unwrap();
        fs::write(d.join("MANIFEST.json"), &mb).unwrap();
        Source {
            path: format!("{dir}/rows.jsonl"),
            sha256: sha,
            rows: rows.len(),
            split: ALLOWED_SPLIT.into(),
            populations: vec!["primary".into()],
            bundle_manifest_path: format!("{dir}/MANIFEST.json"),
            bundle_manifest_sha256: sha256_hex(&mb),
        }
    }
}

fn allowlist(sources: Vec<Source>) -> Vec<u8> {
    serde_json::to_vec(&Allowlist {
        schema: ALLOWLIST_SCHEMA.into(),
        allowlist_id: "synthetic".into(),
        purpose: PURPOSE.into(),
        removed_by: "mainnet hidden evaluation".into(),
        sources,
    })
    .unwrap()
}

/// An allow-list entry written as raw JSON, bypassing nothing but the struct.
fn raw_allowlist(path: &str, manifest: &str, split: &str) -> Vec<u8> {
    let d = "a".repeat(64);
    serde_json::to_vec(&json!({"schema": ALLOWLIST_SCHEMA, "allowlist_id": "x", "purpose": PURPOSE,
        "removed_by": "mainnet", "sources": [{"path": path, "sha256": d, "rows": 1, "split": split,
        "populations": ["primary"], "bundle_manifest_path": manifest, "bundle_manifest_sha256": d}]}))
    .unwrap()
}

fn sel(cohort: u64) -> Selection {
    Selection { seed: [7; 32], cohort, per_stratum: 2, max_per_source_group: 1 }
}

fn refused(r: Result<Value>) -> String {
    let e = format!("{:#}", r.expect_err("must be refused"));
    assert!(e.contains("refused"), "not a refusal: {e}");
    e
}

#[test]
fn builds_a_deterministic_rotating_dev_iter_manifest() {
    let repo = Repo::new("ok");
    let a = allowlist(vec![repo.bundle(DEV_ITER, &default_rows(), ALLOWED_SPLIT)]);
    let m = build(&repo.root, &a, &sel(1)).unwrap();
    assert_eq!(m["purpose"], PURPOSE);
    assert_eq!(m["counts"]["selected"], 12);
    assert_eq!(m["counts"]["by_origin"]["human_only"], 4);
    assert_eq!(m["sources"][0]["eligible_rows"], 36);
    assert!(m["strata"].as_array().unwrap().iter().all(|s| s["selected"] == 2 && s["shortfall"] == 0));
    let ids: Vec<&str> = m["rows"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert!(!ids.contains(&"extra-train-only"), "population filter");
    assert_eq!(build(&repo.root, &a, &sel(1)).unwrap(), m, "same cohort, same manifest");
    let other = build(&repo.root, &a, &sel(2)).unwrap();
    assert_ne!(other["rows_sha256"], m["rows_sha256"], "another cohort rotates the sample");
    let text = serde_json::to_string(&m).unwrap();
    assert!(!text.contains("Synthetic") && !text.contains("\"text\""), "no text in the manifest");
}

#[test]
fn source_groups_are_limited_per_cohort() {
    let repo = Repo::new("groups");
    let rows: Vec<Value> =
        (0..6).map(|i| row(&format!("h{i}"), ALLOWED_SPLIT, "alpha", "human_only", "one-group", "primary")).collect();
    let a = allowlist(vec![repo.bundle(DEV_ITER, &rows, ALLOWED_SPLIT)]);
    let m = build(&repo.root, &a, &Selection { per_stratum: 3, ..sel(1) }).unwrap();
    assert_eq!(m["counts"]["selected"], 1);
    assert_eq!(m["strata"][0]["shortfall"], 2);
}

#[test]
fn refuses_dev_select_even_with_matching_digests_and_dev_iter_labels() {
    let repo = Repo::new("ds");
    for dir in ["data/bundles/b1/dev-select", "data/bundles/b1/dev_select", "data/bundles/b1/DevSelect"] {
        let a = allowlist(vec![repo.bundle(dir, &default_rows(), ALLOWED_SPLIT)]);
        let e = refused(build(&repo.root, &a, &sel(1)));
        assert!(e.contains("select") || e.contains("devselect"), "{e}");
    }
}

#[test]
fn refuses_cal_even_with_matching_digests_and_dev_iter_labels() {
    let repo = Repo::new("cl");
    for dir in ["data/bundles/b1/cal", "data/bundles/b1/CAL", "data/tier/cal-subsample", "data/rc/devit-cal"] {
        let a = allowlist(vec![repo.bundle(dir, &default_rows(), ALLOWED_SPLIT)]);
        assert!(refused(build(&repo.root, &a, &sel(1))).contains("cal"));
    }
}

#[test]
fn refuses_anything_under_tier_c_before_opening_it() {
    let repo = Repo::new("tc");
    // Present and correctly digested files under Tier C.
    for dir in ["data/p/tier-c/eval/dev-iter", "data/p/Tier_C/dev-iter", "data/p/tierc/dev-iter", "data/p/tier-c/test/dev-iter"] {
        let a = allowlist(vec![repo.bundle(dir, &default_rows(), ALLOWED_SPLIT)]);
        let e = refused(build(&repo.root, &a, &sel(1)));
        assert!(e.to_ascii_lowercase().contains("tier"), "{e}");
    }
    // Absent files: the refusal comes from the path alone, so nothing is opened.
    for path in [
        "data/grammar-private/a-competitive-20261027/tier-c/test/rows.jsonl",
        "data/grammar-private/a-competitive-20261027/tier-c/eval-humans-test/rows.jsonl",
        "data/grammar-private/a-competitive-20261027/tier-c/eval/dev-iter/rows.jsonl",
    ] {
        let manifest = path.replace("rows.jsonl", "MANIFEST.json");
        let a = raw_allowlist(path, &manifest, ALLOWED_SPLIT);
        let e = refused(build(&repo.root, &a, &sel(1)));
        assert!(!e.contains("No such file"), "opened before refusing: {e}");
    }
}

#[test]
fn refuses_test_and_sealed_paths() {
    for path in ["data/b/test/rows.jsonl", "data/b/eval-humans-test/rows.jsonl", "data/b/sealed/rows.jsonl"] {
        assert!(refusal(path).is_some(), "{path}");
    }
}

#[cfg(unix)]
#[test]
fn refuses_a_symlink_that_resolves_into_tier_c() {
    let repo = Repo::new("symlink");
    let mut s = repo.bundle("data/p/tier-c/eval/dev-iter", &default_rows(), ALLOWED_SPLIT);
    fs::create_dir_all(repo.root.join("data/bundles")).unwrap();
    std::os::unix::fs::symlink(repo.root.join("data/p/tier-c/eval/dev-iter"), repo.root.join("data/bundles/dev-iter"))
        .unwrap();
    s.path = "data/bundles/dev-iter/rows.jsonl".into();
    s.bundle_manifest_path = "data/bundles/dev-iter/MANIFEST.json".into();
    let e = refused(build(&repo.root, &allowlist(vec![s]), &sel(1)));
    assert!(e.contains("resolves to data/p/tier-c"), "{e}");
}

#[test]
fn refuses_an_allow_listed_split_other_than_dev_iter() {
    let repo = Repo::new("split");
    for split in ["cal", "dev_select", "test", "train"] {
        let mut s = repo.bundle(DEV_ITER, &default_rows(), ALLOWED_SPLIT);
        s.split = split.into();
        assert!(refused(build(&repo.root, &allowlist(vec![s]), &sel(1))).contains("allow-listed split"));
    }
}

#[test]
fn refuses_rows_labelled_with_another_split() {
    let repo = Repo::new("rowsplit");
    for split in ["cal", "dev_select", "test"] {
        let mut rows = default_rows();
        rows[3] = row("leak", split, "alpha", "human_only", "alpha-leak", "primary");
        let a = allowlist(vec![repo.bundle(DEV_ITER, &rows, ALLOWED_SPLIT)]);
        assert!(refused(build(&repo.root, &a, &sel(1))).contains("has split"));
    }
}

#[test]
fn refuses_seal_records_and_sealed_or_other_bundle_manifests() {
    let repo = Repo::new("seal");
    let s = repo.bundle(DEV_ITER, &default_rows(), ALLOWED_SPLIT);
    for marker in ["SEALED.json", "UNSEALED-final_selection.json"] {
        let p = repo.root.join(DEV_ITER).join(marker);
        fs::write(&p, b"{}").unwrap();
        assert!(refused(build(&repo.root, &allowlist(vec![s.clone()]), &sel(1))).contains("seal record"));
        fs::remove_file(p).unwrap();
    }
    for split in ["cal", "dev_select"] {
        let s = repo.bundle(DEV_ITER, &default_rows(), split);
        assert!(refused(build(&repo.root, &allowlist(vec![s]), &sel(1))).contains("bundle manifest"));
    }
}

#[test]
fn refuses_changed_rows_or_manifest_digests() {
    let repo = Repo::new("digest");
    let s = repo.bundle(DEV_ITER, &default_rows(), ALLOWED_SPLIT);
    let mut bad = s.clone();
    bad.sha256 = "0".repeat(64);
    assert!(refused(build(&repo.root, &allowlist(vec![bad]), &sel(1))).contains("bundle manifest rows differ"));
    let mut bad = s.clone();
    bad.bundle_manifest_sha256 = "0".repeat(64);
    assert!(refused(build(&repo.root, &allowlist(vec![bad]), &sel(1))).contains("MANIFEST.json digest"));
    // Rows changed after allow-listing, with the bundle manifest updated to match.
    let a = allowlist(vec![s]);
    let mut rows = default_rows();
    rows.pop();
    let fresh = repo.bundle(DEV_ITER, &rows, ALLOWED_SPLIT);
    let mut stale = Allowlist::parse(&a).unwrap();
    stale.sources[0].bundle_manifest_sha256 = fresh.bundle_manifest_sha256;
    stale.sources[0].rows = fresh.rows;
    stale.sources[0].sha256 = "1".repeat(64);
    let e = refused(build(&repo.root, &serde_json::to_vec(&stale).unwrap(), &sel(1)));
    assert!(e.contains("bundle manifest rows differ"), "{e}");
}

#[test]
fn refuses_rows_edited_after_allow_listing_with_the_manifest_unchanged() {
    let repo = Repo::new("rowsedit");
    let s = repo.bundle(DEV_ITER, &default_rows(), ALLOWED_SPLIT);
    // Same line count, valid dev_iter rows; only rows.jsonl changes.
    let mut rows = default_rows();
    rows[0] = row("alpha-0-edited", ALLOWED_SPLIT, "alpha", "human_only", "alpha-g0", "primary");
    let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
    fs::write(repo.root.join(DEV_ITER).join("rows.jsonl"), body).unwrap();
    let e = refused(build(&repo.root, &allowlist(vec![s]), &sel(1)));
    assert!(e.contains("digest differs from the allow-list"), "{e}");
}

#[cfg(unix)]
#[test]
fn refuses_a_data_directory_or_repository_root_that_resolves_into_tier_c() {
    use std::os::unix::fs::symlink;
    // `data` is a symlink into a Tier C directory holding correctly digested dev_iter rows.
    let repo = Repo::new("datalink");
    let mut s = repo.bundle("p/tier-c/eval/bundles/b1/dev-iter", &default_rows(), ALLOWED_SPLIT);
    fs::remove_dir_all(repo.root.join("data")).unwrap();
    symlink(repo.root.join("p/tier-c/eval"), repo.root.join("data")).unwrap();
    s.path = format!("{DEV_ITER}/rows.jsonl");
    s.bundle_manifest_path = format!("{DEV_ITER}/MANIFEST.json");
    let e = refused(build(&repo.root, &allowlist(vec![s.clone()]), &sel(1)));
    assert!(e.contains("data/ is a symlink"), "{e}");
    let out = repo.root.join("data/m.json");
    let e = format!("{:#}", write_manifest(&repo.root, &out, &json!({})).unwrap_err());
    assert!(e.contains("data/ is a symlink") && !out.exists(), "{e}");

    // The repository root itself lies under Tier C, given directly or through a symlink.
    let inner = repo.root.join("q/tier-c/clone");
    fs::create_dir_all(inner.join("data")).unwrap();
    fs::rename(repo.root.join("p/tier-c/eval/bundles"), inner.join("data/bundles")).unwrap();
    symlink(&inner, repo.root.join("clone")).unwrap();
    for root in [inner.clone(), repo.root.join("clone")] {
        let e = refused(build(&root, &allowlist(vec![s.clone()]), &sel(1)));
        assert!(e.contains("repository root") && e.contains("Tier C"), "{e}");
    }
}

#[test]
fn refuses_paths_outside_data_or_not_normalized() {
    for path in ["/abs/data/x/rows.jsonl", "data/../x/rows.jsonl", "data/./x/rows.jsonl", "x/dev-iter/rows.jsonl", "data", "data//rows.jsonl"] {
        assert!(refusal(path).is_some(), "{path}");
    }
    assert!(refusal("data/p/tier/c/dev-iter/rows.jsonl").is_some(), "tier, c across components");
    assert!(refusal("data/p/x-tier/c-y/dev-iter/rows.jsonl").is_some(), "tier, c across components");
    assert!(refusal("data/grammar-private/a-competitive-20261027/baseline-release/bundles/build-18b3682973982a3f/dev-iter/rows.jsonl").is_none());
}

#[test]
fn tracked_allowlist_names_only_dev_iter() {
    let a = Allowlist::parse(TRACKED_ALLOWLIST.as_bytes()).unwrap();
    assert_eq!(a.purpose, PURPOSE);
    for s in &a.sources {
        assert_eq!(s.split, ALLOWED_SPLIT);
        assert!(refusal(&s.path).is_none() && refusal(&s.bundle_manifest_path).is_none());
        assert!(s.path.ends_with("/dev-iter/rows.jsonl"));
    }
}

#[test]
fn manifests_are_written_only_as_new_files_under_data() {
    let repo = Repo::new("write");
    let m = json!({"schema": MANIFEST_SCHEMA});
    fs::create_dir_all(repo.root.join("data/out")).unwrap();
    let p = write_manifest(&repo.root, &repo.root.join("data/out/m.json"), &m).unwrap();
    assert!(p.is_file());
    assert!(write_manifest(&repo.root, &repo.root.join("data/out/m.json"), &m).is_err(), "no overwrite");
    let e = write_manifest(&repo.root, &repo.root.join("m.json"), &m).unwrap_err().to_string();
    assert!(e.contains("only under data/"), "{e}");
    fs::create_dir_all(repo.root.join("data/tier-c")).unwrap();
    assert!(write_manifest(&repo.root, &repo.root.join("data/tier-c/m.json"), &m).is_err());
}

#[test]
fn exports_a_content_free_pool_manifest_with_private_shards() {
    let repo = Repo::new("pool");
    let a = allowlist(vec![repo.bundle(DEV_ITER, &default_rows(), ALLOWED_SPLIT)]);
    fs::create_dir_all(repo.root.join("data/pools")).unwrap();
    let out = repo.root.join("data/pools/p1");
    let m = export_human_pool(&repo.root, &a, "p1", 5, &out).unwrap();
    assert_eq!(m["counts"]["rows"], 12, "human_only primary rows only");
    assert_eq!(m["shards"].as_array().unwrap().len(), 3);
    let text = fs::read_to_string(out.join("manifest.json")).unwrap();
    assert!(!text.contains("Synthetic") && !text.contains("\"text\""), "no text in the manifest");
    let mut lines = 0;
    for s in m["shards"].as_array().unwrap() {
        let body = fs::read(out.join(s["file"].as_str().unwrap())).unwrap();
        assert_eq!(sha256_hex(&body), s["sha256"]);
        for l in std::str::from_utf8(&body).unwrap().lines() {
            let r: Value = serde_json::from_str(l).unwrap();
            assert_eq!(sha256_hex(r["text"].as_str().unwrap().as_bytes()), r["text_sha256"]);
            lines += 1;
        }
    }
    assert_eq!(lines, 12);
    // Never replaced, and only under data/.
    assert!(export_human_pool(&repo.root, &a, "p1", 5, &out).is_err());
    let outside = std::env::temp_dir().join(format!("a-testnet-eval-pool-outside-{}", std::process::id()));
    assert!(export_human_pool(&repo.root, &a, "p1", 5, &outside).is_err());
    // A refused split is refused for pools too.
    let refused_src = repo.bundle("data/bundles/b1/dev-select", &default_rows(), ALLOWED_SPLIT);
    let bad = allowlist(vec![refused_src]);
    assert!(export_human_pool(&repo.root, &bad, "p2", 5, &repo.root.join("data/pools/p2")).is_err());
}
