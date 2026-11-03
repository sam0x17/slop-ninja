//! Offline tests for pool v2: the public-stock allow-list, the reservation gate,
//! mix feasibility under the reuse rules, canary and probe draws, the reweighting,
//! and the v1 replay. Every fixture is synthetic, except the content-free record of
//! the v1 draws.
use crate::capacity::{self, Canary, PPM, Params, Reuse};
use crate::engine_v1::{kind_counts, pool};
use crate::gate;
use crate::pool_v2::{self, BuildParams, CanaryReport};
use crate::replay::{self, Record, TRACKED_RECORD};
use crate::stock::{
    self, FileRef, GateRef, GroupReservation, IndexReservation, MirrorHost, NotGated, ProseGuard, SourceHost, StockAllowlist,
    TestRules, UrlKey,
};
use crate::{TRACKED_ALLOWLIST, sha256_hex};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) struct Repo {
    pub(crate) root: PathBuf,
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Repo {
    /// A temporary repository; `tag` must not itself contain a refused token.
    pub(crate) fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("a-testnet-stock-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("data")).unwrap();
        Repo { root }
    }

    pub(crate) fn write(&self, rel: &str, body: &[u8]) -> String {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, body).unwrap();
        sha256_hex(body)
    }

    pub(crate) fn jsonl(&self, rel: &str, rows: &[Value]) -> FileRef {
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        FileRef { path: rel.into(), sha256: self.write(rel, body.as_bytes()) }
    }

    pub(crate) fn zst(&self, rel: &str, rows: &[Value]) -> FileRef {
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        enc.write_all(body.as_bytes()).unwrap();
        let bytes = enc.finish().unwrap();
        FileRef { path: rel.into(), sha256: self.write(rel, &bytes) }
    }
}

fn group(domain: &str, key: &str, families: &[&str]) -> Value {
    json!({"schema": stock::GROUP_SCHEMA, "split": "reserved_unassigned", "domain": domain, "group_key": key,
        "family_keys": families, "units": families.len(), "group_rank": "00"})
}

fn test_group(domain: &str, key: &str, units: u64, expected: f64, cap: f64, order: u64) -> Value {
    json!({"schema": stock::GROUP_SCHEMA, "split": "test", "domain": domain, "group_key": key, "units": units,
        "expected_docs_capped": expected, "max_docs_per_group": cap, "order_in_split_domain": order,
        "contributor_key_sha256": [sha256_hex(key.as_bytes())]})
}

pub(crate) fn title(s: &str) -> String {
    sha256_hex(s.as_bytes())
}

/// The synthetic split set lives under `data/res` with its own `SHA256SUMS`.
const SPLIT_ROOT: &str = "data/res";
const SCREEN: &str = "data/rc/records/screen.json";
const WIKI_INDEX: usize = 40;
/// 30 blog sites, 60 forum authors, 8 news desks, 3 peer-review forums, 1 platform blog.
const GROUPS: u64 = 102;
/// 40 index pages less 15 Test-held, 1 CAL-held and 2 excluded by the reservation.
const INDEX_LEFT: u64 = 22;

/// The synthetic stock: `wiki` (index), `blogs` (FineWeb-like sites), `forum`, `news`, `peer`.
pub(crate) struct Fixture {
    pub(crate) repo: Repo,
    pub(crate) allow: StockAllowlist,
    /// Regenerate `SHA256SUMS` and the lineage screen from the files before a build.
    pub(crate) auto: bool,
}

pub(crate) fn gref(role: &str, kind: &str, f: &FileRef, rows: usize) -> GateRef {
    GateRef { role: role.into(), kind: kind.into(), path: f.path.clone(), sha256: f.sha256.clone(), rows: rows as u64 }
}

impl Fixture {
    pub(crate) fn new(tag: &str) -> Self {
        let repo = Repo::new(tag);
        let mut stock_groups = Vec::new();
        for i in 0..30 {
            let fam = format!("fineweb_page:00000000-0000-0000-0000-{i:012}");
            stock_groups.push(group("blogs", &format!("fineweb_site:blog{i}.example"), &[&fam]));
        }
        stock_groups.push(group("blogs", "fineweb_blog:foo.platform.example", &["fineweb_page:00000000-0000-0000-0000-999999999999"]));
        for i in 0..60 {
            let f1 = format!("forum_post:{i}a");
            let f2 = format!("forum_post:{i}b");
            stock_groups.push(group("forum", &format!("forum_author:{i}"), &[&f1, &f2]));
        }
        for i in 0..8 {
            stock_groups.push(group("news", &format!("news_desk:{i}"), &[&format!("news_story:{i}")]));
        }
        stock_groups.push(group("peer", "openreview:forum:AbC123", &["openreview:note:n1"]));
        stock_groups.push(group("peer", "openreview:forum:scipost_202001_00001v2", &["orb:scipost_202001_00001v2:v1:r0"]));
        stock_groups.push(group("peer", "openreview:forum:1606.04401v2", &["orb:1606.04401v2:v1:r0", "orb:1606.04401v2:v1:r1"]));
        assert_eq!(stock_groups.len() as u64, GROUPS);
        let res = repo.jsonl("data/res/base/reserved-unassigned/groups.jsonl", &stock_groups);
        // Index: 40 pages; 15 held by the Test, 1 by CAL, 2 excluded by the reservation.
        let index: Vec<Value> = (0..WIKI_INDEX)
            .map(|i| json!({"schema": stock::INDEX_UNIT_SCHEMA, "family_key": format!("wikipedia:page:{i}"),
                "bucket": 950, "prose_opened": false, "title_sha256": title(&format!("Page {i}"))}))
            .collect();
        let ix = repo.jsonl("data/res/index/reserved-metadata.jsonl", &index);
        let receipts = repo.jsonl("data/res/base/overlap-receipts.jsonl", &[
            json!({"action": "group_excluded", "check": "reservation_overlap", "domain": "wiki", "group_key": "wikipedia:page:38", "reasons": ["r"]}),
            json!({"action": "group_excluded", "check": "reservation_overlap", "domain": "wiki", "group_key": "wikipedia:page:39", "reasons": ["r"]}),
            json!({"action": "group_excluded", "check": "reservation_overlap", "domain": "news", "group_key": "news_desk:99", "reasons": ["r"]}),
        ]);
        let units: Vec<Value> = (0..30)
            .map(|i| json!({"family_key": format!("fineweb_page:00000000-0000-0000-0000-{i:012}"),
                "url": format!("https://blog{i}.example/post"), "prose_opened": false}))
            .collect();
        let units = repo.jsonl("data/res/blogs/units.jsonl", &units);
        // Titles of the forum's reserved units (a group reservation with unit metadata).
        let ftitles: Vec<Value> = (0..60)
            .map(|i| json!({"family_key": format!("forum_post:{i}a"), "prose_opened": false, "title_sha256": title(&format!("Thread {i}"))}))
            .collect();
        let ftitles = repo.jsonl("data/titles/forum/reserved-metadata.jsonl", &ftitles);
        // Sealed Test rules: caps, yields, and an acquisition that processed some groups.
        let mut tg = Vec::new();
        for (d, cap, y) in [("blogs", 1.0, 0.5), ("forum", 2.0, 0.8), ("news", 1.0, 0.25), ("wiki", 1.0, 0.5), ("peer", 2.0, 0.5)] {
            for o in 0..10u64 {
                // The index domain's Test groups are index pages, one unit each.
                let (key, units) = if d == "wiki" { (format!("wikipedia:page:{}", 10 + o), 1) } else { (format!("{d}:test{o}"), 1 + o % 3) };
                tg.push(test_group(d, &key, units, (units as f64 * y).min(cap), cap, o));
            }
        }
        for o in 0..5u64 {
            tg.push(test_group("wiki", &format!("wikipedia:page:{o}"), 1, 0.5, 1.0, 10 + o));
        }
        tg.push(test_group("speech", "speech:test0", 4, 20.0, 20.0, 0));
        let tgroups = repo.jsonl("data/res/rules/test/groups.jsonl", &tg);
        let summary = json!({"per_domain": {
            "blogs": {"documents": 4, "groups_processed": 8}, "forum": {"documents": 9, "groups_processed": 10},
            "news": {"documents": 3, "groups_processed": 10}, "wiki": {"documents": 12, "groups_processed": 14},
            "peer": {"documents": 3, "groups_processed": 4}, "speech": {"documents": 20, "groups_processed": 1}}});
        let sb = serde_json::to_vec(&summary).unwrap();
        let ssum = FileRef { path: "data/res/rules/test/summary.json".into(), sha256: repo.write("data/res/rules/test/summary.json", &sb) };
        // Gate references.
        let train_rows = [json!({"source_group": "forum_post:train1", "split_component": "forum_post:train1", "writer_key": "forum_author:t1",
            "text_sha256": sha256_hex(b"t1")})];
        let train = repo.jsonl("data/res/train/manifest.jsonl", &train_rows);
        let held = repo.jsonl("data/res/held/manifest.jsonl", &[json!({"split": "held", "writer_key": "held_writer:1", "source_group": "held:1"})]);
        let blocked = repo.jsonl("data/res/blogs/blocked-groups.jsonl", &[
            json!({"blocked": "writer_has_fineweb_blog_pool_text", "group_key": "fineweb_site:elsewhere.example", "family_keys": ["fineweb_page:x"]}),
        ]);
        let parent = repo.jsonl("data/res/blogs/groups.jsonl", &[json!({"allocation": "unallocated", "group_key": "fineweb_site:blog1.example"})]);
        let test_fam = FileRef {
            path: "data/res/rules/test/families.txt".into(),
            sha256: repo.write("data/res/rules/test/families.txt", b"forum_post:test-only\n"),
        };
        let test_manifest = repo.jsonl("data/res/rules/test/manifest.jsonl", &[
            json!({"family_key": "forum_post:test-only", "group_key": "forum:test0", "source_unit": {"writer_key": "author:held-out"},
                "text_sha256": null, "acquisition": "run the adapter on this split's works"}),
        ]);
        let cal = repo.jsonl("data/res/cal/groups.jsonl", &[
            json!({"domain": "wiki", "group_key": "wikipedia:page:5", "contributor_key_sha256": []}),
            json!({"domain": "forum", "group_key": "forum_author:cal1"}),
        ]);
        let meta = repo.jsonl("data/res/rules/test/metadata.jsonl", &[
            json!({"id": "t-1", "aliases": ["news_story:test"], "source_group": "news_story:test", "text_sha256": sha256_hex(b"x")}),
        ]);
        let bundle = repo.jsonl("data/rc/bundle/rows.jsonl", &[
            json!({"id": "raid-1", "cohort_id": "raid", "source_group": "raid:wiki:1", "split": "train", "text": "A trained passage.",
                "text_sha256": sha256_hex(b"A trained passage.")}),
        ]);
        let titles = FileRef { path: "data/rc/checks/titles.txt".into(), sha256: repo.write("data/rc/checks/titles.txt", format!("{}\n", title("Not reserved")).as_bytes()) };
        let cc = repo.zst("data/res/cc/admitted.jsonl.zst", &[
            json!({"refers_to": "<urn:uuid:11111111-1111-1111-1111-111111111111>", "url": "https://other.example/a",
                "host": "other.example", "registered_domain": "other.example", "group": "cc_site:other.example", "text_sha256": sha256_hex(b"cc1")}),
        ]);
        let pc_texts = repo.zst("data/res/pc/texts.jsonl.zst", &[json!({"text_sha256": sha256_hex(b"pc1"), "text": "public candidate prose"})]);
        let pc_records = repo.zst("data/res/pc/records.jsonl.zst", &[
            json!({"source_group": "author_work_v2:x", "writer_key": "author:x", "provenance": {"aliases": ["pg:1"]}}),
        ]);
        let pc_splits = repo.zst("data/res/pc/splits.jsonl.zst", &[json!({"source_group": "doi:1", "text_sha256": sha256_hex(b"pc1")})]);
        let allow = StockAllowlist {
            schema: stock::STOCK_ALLOWLIST_SCHEMA.into(),
            allowlist_id: "synthetic-stock".into(),
            purpose: stock::STOCK_PURPOSE.into(),
            split: stock::STOCK_SPLIT.into(),
            removed_by: "mainnet hidden evaluation".into(),
            group_reservations: vec![GroupReservation { reservation: "base".into(), path: res.path, sha256: res.sha256, rows: GROUPS }],
            index_reservations: vec![IndexReservation {
                reservation: "base".into(),
                domain: "wiki".into(),
                path: ix.path,
                sha256: ix.sha256,
                rows: WIKI_INDEX as u64,
                excluded: receipts,
                remainder_units: INDEX_LEFT,
            }],
            unit_urls: vec![units],
            unit_titles: vec![ftitles],
            test_rules: TestRules { groups: vec![tgroups], acquisition_summary: ssum },
            source_hosts: vec![
                SourceHost { domain: "peer".into(), field: "registered_domain".into(), value: "openreview.net".into(), url_key_rule: Some("openreview_forum_url_v1".into()) },
                SourceHost { domain: "peer".into(), field: "registered_domain".into(), value: "scipost.org".into(), url_key_rule: Some("scipost_submission_url_v1".into()) },
                SourceHost { domain: "forum".into(), field: "host".into(), value: "forum.example".into(), url_key_rule: None },
            ],
            mirror_hosts: vec![MirrorHost { domain: "news".into(), host: "mirror.example".into() }],
            assignment_prose_guard: ProseGuard {
                max_words: 12,
                refused_fields: ["text", "excerpt", "passage", "body", "content", "prose"].iter().map(|s| s.to_string()).collect(),
                measured: "synthetic".into(),
            },
            split_sums: FileRef { path: format!("{SPLIT_ROOT}/SHA256SUMS"), sha256: String::new() },
            not_gated: vec![NotGated { path: parent.path, sha256: parent.sha256, reason: "the parent reservation".into() }],
            lineage_screen: FileRef { path: SCREEN.into(), sha256: String::new() },
            gate: vec![
                gref("train", "split_manifest", &train, 1),
                gref("train_held", "split_manifest", &held, 1),
                gref("train_held", "blocked_groups", &blocked, 1),
                gref("sealed_test", "split_families", &test_fam, 1),
                gref("sealed_test", "acquired_metadata", &meta, 1),
                gref("sealed_test", "split_manifest", &test_manifest, 1),
                gref("evaluation", "split_groups", &cal, 2),
                gref("r0_training_lineage", "trainer_bundle_rows", &bundle, 1),
                gref("r0_training_lineage", "title_sha256_list", &titles, 1),
                gref("cc_mainnet_stock", "cc_admitted", &cc, 1),
                gref("public_candidate_v1", "public_candidate_texts", &pc_texts, 1),
                gref("public_candidate_v1", "public_candidate_records", &pc_records, 1),
                gref("public_candidate_v1", "public_candidate_splits", &pc_splits, 1),
            ],
            carve_exclusion: None,
            active_reference: None,
        };
        let mut f = Fixture { repo, allow, auto: true };
        f.sync();
        f
    }

    /// Rewrites the lineage screen and `SHA256SUMS` from the current files.
    pub(crate) fn sync(&mut self) {
        if !self.auto {
            return;
        }
        let lineage = |kind: &str| self.allow.gate.iter().find(|g| g.kind == kind).cloned().unwrap();
        let (t, b) = (lineage("title_sha256_list"), lineage("trainer_bundle_rows"));
        let screen = json!({"schema": stock::LINEAGE_SCREEN_SCHEMA, "date": "2026-10-28",
            "inputs": {"raid_train_title_sha256": {"path": t.path, "sha256": t.sha256, "entries": t.rows},
                "raid_train_rows": {"rows_sha256": b.sha256, "rows": b.rows}},
            "rules": [{"id": "S1_title", "rule": "skip by title hash"}, {"id": "S2_reddit_author", "rule": "compare author posts"},
                {"id": "S3_text", "rule": "compare excerpt text"}]});
        self.allow.lineage_screen.sha256 = self.repo.write(SCREEN, &serde_json::to_vec(&screen).unwrap());
        let mut lines = Vec::new();
        let root = self.repo.root.join(SPLIT_ROOT);
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.file_name().unwrap() != "SHA256SUMS" {
                    let rel = p.strip_prefix(&root).unwrap().to_str().unwrap().to_string();
                    lines.push(format!("{}  ./{rel}\n", sha256_hex(&fs::read(&p).unwrap())));
                }
            }
        }
        lines.sort();
        self.allow.split_sums.sha256 = self.repo.write(&format!("{SPLIT_ROOT}/SHA256SUMS"), lines.concat().as_bytes());
    }

    pub(crate) fn bytes(&mut self) -> Vec<u8> {
        self.sync();
        serde_json::to_vec(&self.allow).unwrap()
    }

    fn parsed(&mut self) -> StockAllowlist {
        StockAllowlist::parse(&self.bytes()).unwrap()
    }

    /// Replace a gate reference's rows (the digest and the count follow).
    fn set_ref(&mut self, kind: &str, rows: &[Value]) {
        let i = self.allow.gate.iter().position(|g| g.kind == kind).unwrap();
        let p = self.allow.gate[i].path.clone();
        let f = if p.ends_with(".zst") { self.repo.zst(&p, rows) } else { self.repo.jsonl(&p, rows) };
        self.allow.gate[i].sha256 = f.sha256;
        self.allow.gate[i].rows = rows.len() as u64;
    }

    /// Replace a plain-line gate reference (families, title list).
    fn set_lines(&mut self, kind: &str, lines: &[String]) {
        let i = self.allow.gate.iter().position(|g| g.kind == kind).unwrap();
        let p = self.allow.gate[i].path.clone();
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        self.allow.gate[i].sha256 = self.repo.write(&p, body.as_bytes());
        self.allow.gate[i].rows = lines.len() as u64;
    }

    pub(crate) fn build(&mut self, tag: &str) -> anyhow::Result<Value> {
        self.build_with(tag, &params())
    }

    pub(crate) fn build_with(&mut self, tag: &str, bp: &BuildParams) -> anyhow::Result<Value> {
        let out = self.repo.root.join("data/out").join(tag);
        fs::create_dir_all(out.parent().unwrap()).unwrap();
        let bytes = self.bytes();
        pool_v2::build(&self.repo.root, &bytes, bp, &out)
    }

    pub(crate) fn read_out(&self, tag: &str, file: &str) -> String {
        fs::read_to_string(self.repo.root.join("data/out").join(tag).join(file)).unwrap()
    }

    pub(crate) fn out_json(&self, tag: &str, file: &str) -> Value {
        serde_json::from_str(&self.read_out(tag, file)).unwrap()
    }

    /// Excluded group key -> receipt reasons.
    pub(crate) fn receipts(&self, tag: &str) -> BTreeMap<String, Vec<String>> {
        self.read_out(tag, "exclusions.jsonl")
            .lines()
            .map(|l| {
                let v: Value = serde_json::from_str(l).unwrap();
                let r = v["reasons"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
                (v["group_key"].as_str().unwrap().to_string(), r)
            })
            .collect()
    }
}

pub(crate) fn params() -> BuildParams {
    BuildParams {
        pool_id: "testnet-human-pool-v2".into(),
        horizon: 3,
        report_horizons: vec![1, 2, 1000],
        plan: Params::new(10, [20, 55, 25], Reuse::Ledger { max_uses: 4, min_gap: 7 }, 0),
        engine_mix: [40, 40, 20],
        roster: 40,
        cohorts_per_day: 1,
        canary_report: Some(CanaryReport { docs: 5, flagged_audits: 2, every: vec![2] }),
    }
}

pub(crate) fn err(r: anyhow::Result<Value>) -> String {
    format!("{:#}", r.expect_err("expected a refusal"))
}

// ------------------------------------------------------------------ the gate

#[test]
fn a_clean_stock_passes_the_gate_and_writes_a_reservation_manifest() {
    let mut f = Fixture::new("clean");
    let m = f.build("clean").unwrap();
    assert_eq!(m["schema"], pool_v2::POOL_V2_SCHEMA);
    assert_eq!(m["stage"], "reservation");
    assert_eq!(m["gate"]["verdict"], "pass");
    assert_eq!(m["exclusions"]["rows"], 0);
    assert_eq!(m["groups"]["rows"], GROUPS + INDEX_LEFT);
    let gate = f.out_json("clean", "gate.json");
    assert_eq!(gate["remaining_overlap"], 0);
    assert!(gate["text_digest_check"].as_str().unwrap().starts_with("not applicable"));
    let roles: BTreeSet<&str> = gate["references"].as_array().unwrap().iter().map(|r| r["role"].as_str().unwrap()).collect();
    let all: BTreeSet<&str> = stock::ROLES.iter().map(|(r, _)| *r).collect();
    assert_eq!(roles, all);
    // The manifest names exactly what each disjointness claim covers.
    assert!(m["gate"]["checked"]["train"].as_array().unwrap().iter().any(|p| p == "data/res/train/manifest.jsonl"));
    assert!(m["gate"]["checked"]["r0_training_lineage"].as_array().unwrap().iter().any(|p| p == "data/rc/bundle/rows.jsonl"));
    // Split completeness was checked against SHA256SUMS.
    assert!(gate["split_completeness"]["split_files_by_disposition"]["gate"].as_u64().unwrap() >= 5);
    // Every written file is owner-only, and the v1 engine refuses the manifest.
    for file in ["manifest.json", "groups.jsonl", "gate.json", "capacity.json", "exclusions.jsonl"] {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(f.repo.root.join("data/out/clean").join(file)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{file}");
    }
    let mb = f.read_out("clean", "manifest.json");
    assert!(pool::PoolManifest::parse(mb.as_bytes(), "testnet-human-pool-v2").is_err());
    assert!(!mb.contains("\"text\""), "no text in the manifest");
    // Existing output directories are never replaced.
    let e = err(f.build("clean"));
    assert!(e.contains("never replaced"), "{e}");
}

#[test]
fn train_overlap_refuses_the_stock() {
    let mut f = Fixture::new("train");
    f.set_ref("split_manifest", &[json!({"source_group": "forum_post:3a", "split_component": "forum_post:3a"})]);
    let e = err(f.build("t"));
    assert!(e.contains("(train) shares 1 stock groups"), "{e}");
    let mut f = Fixture::new("train-writer");
    f.set_ref("split_manifest", &[json!({"writer_key": "forum_author:7"})]);
    let e = err(f.build("t"));
    assert!(e.contains("(train) shares 1 stock groups"), "{e}");
}

#[test]
fn a_train_row_keyed_only_by_split_group_key_or_under_another_site_prefix_refuses() {
    // Keys are read field-blind, so split_group_key (or any other field) is checked.
    let mut f = Fixture::new("sgk");
    f.set_ref("split_manifest", &[json!({"source_group": "fineweb_page:train-x", "split_group_key": "forum_author:3",
        "text_sha256": sha256_hex(b"t1")})]);
    let e = err(f.build("t"));
    assert!(e.contains("(train) shares 1 stock groups"), "{e}");
    // fineweb_blog:X in Train against fineweb_site:X in the stock.
    let mut f = Fixture::new("prefix");
    f.set_ref("split_manifest", &[json!({"source_group": "fineweb_page:train-y", "writer_key": "fineweb_blog:blog3.example"})]);
    let e = err(f.build("t"));
    assert!(e.contains("(train) shares 1 stock groups") && e.contains("site_key"), "{e}");
    // And with a www. prefix and upper case.
    let mut f = Fixture::new("prefix-www");
    f.set_ref("split_manifest", &[json!({"writer_key": "fineweb_site:WWW.Blog4.example"})]);
    assert!(err(f.build("t")).contains("(train) shares 1 stock groups"));
}

#[test]
fn sealed_test_overlap_refuses_the_stock_by_group_family_and_contributor_hash() {
    // A Test group record whose group key is a stock group.
    let mut f = Fixture::new("held-group");
    let p = f.allow.test_rules.groups[0].path.clone();
    let mut rows: Vec<Value> = fs::read_to_string(f.repo.root.join(&p)).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    rows.push(test_group("forum", "forum_author:12", 1, 0.8, 2.0, 99));
    f.allow.test_rules.groups[0] = f.repo.jsonl(&p, &rows);
    let e = err(f.build("t"));
    assert!(e.contains("(sealed_test) shares 1 stock groups"), "{e}");
    // A hashed contributor key that names a stock group, under another group key.
    let mut f = Fixture::new("held-contrib");
    rows.pop();
    let mut r = test_group("forum", "forum_author:other", 1, 0.8, 2.0, 99);
    r["contributor_key_sha256"] = json!([sha256_hex(b"forum_author:20")]);
    rows.push(r);
    f.allow.test_rules.groups[0] = f.repo.jsonl(&p, &rows);
    let e = err(f.build("t"));
    assert!(e.contains("(sealed_test) shares 1 stock groups") && e.contains("hashed_key"), "{e}");
    // A Test family line.
    let mut f = Fixture::new("held-family");
    f.set_lines("split_families", &["news_story:3".into()]);
    let e = err(f.build("t"));
    assert!(e.contains("(sealed_test) shares 1 stock groups"), "{e}");
    // Acquired Test metadata aliases.
    let mut f = Fixture::new("held-meta");
    f.set_ref("acquired_metadata", &[json!({"aliases": ["fineweb_page:00000000-0000-0000-0000-000000000004"]})]);
    let e = err(f.build("t"));
    assert!(e.contains("(sealed_test) shares 1 stock groups"), "{e}");
    // A Test reservation manifest's nested writer key.
    let mut f = Fixture::new("held-manifest");
    let i = f.allow.gate.iter().position(|g| g.role == "sealed_test" && g.kind == "split_manifest").unwrap();
    let path = f.allow.gate[i].path.clone();
    let fr = f.repo.jsonl(&path, &[json!({"family_key": "x:1", "source_unit": {"writer_key": "news_desk:2"}})]);
    f.allow.gate[i].sha256 = fr.sha256;
    let e = err(f.build("t"));
    assert!(e.contains("(sealed_test) shares 1 stock groups"), "{e}");
}

#[test]
fn evaluation_overlap_refuses_and_assignments_never_enter_the_index_remainder() {
    let mut f = Fixture::new("calib");
    f.set_ref("split_groups", &[
        json!({"domain": "wiki", "group_key": "wikipedia:page:5"}),
        json!({"domain": "news", "group_key": "news_desk:4"}),
    ]);
    let e = err(f.build("t"));
    assert!(e.contains("(evaluation) shares 1 stock groups"), "{e}");
    // A reservation count that disagrees with the index remainder refuses.
    let mut f = Fixture::new("remainder");
    f.allow.index_reservations[0].remainder_units += 1;
    let e = err(f.build("t"));
    assert!(e.contains("remainder 22, the reservation records 23"), "{e}");
}

#[test]
fn assignment_files_carrying_prose_are_refused_before_use() {
    let mut f = Fixture::new("text-meta");
    f.set_ref("acquired_metadata", &[json!({"aliases": ["x"], "text": "prose"})]);
    let e = err(f.build("t"));
    assert!(e.contains("carries text"), "{e}");
    let mut f = Fixture::new("text-assign");
    f.set_ref("split_groups", &[json!({"domain": "wiki", "group_key": "wikipedia:page:5", "text": "prose"})]);
    let e = err(f.build("t"));
    assert!(e.contains("carries text"), "{e}");
    // Prose under another field name.
    let mut f = Fixture::new("excerpt");
    let p = f.allow.test_rules.groups[0].path.clone();
    let mut rows: Vec<Value> = fs::read_to_string(f.repo.root.join(&p)).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    rows[0]["excerpt"] = json!("A sealed sentence.");
    f.allow.test_rules.groups[0] = f.repo.jsonl(&p, &rows);
    let e = err(f.build("t"));
    assert!(e.contains("carries text") && e.contains("excerpt"), "{e}");
    // Any string longer than the measured bound, whatever its field, and without echoing it.
    let mut f = Fixture::new("long-note");
    let long = "one two three four five six seven eight nine ten eleven twelve thirteen";
    let i = f.allow.gate.iter().position(|g| g.role == "sealed_test" && g.kind == "split_manifest").unwrap();
    let path = f.allow.gate[i].path.clone();
    f.allow.gate[i].sha256 = f.repo.jsonl(&path, &[json!({"family_key": "x:2", "source_unit": {"note": long}})]).sha256;
    let e = err(f.build("t"));
    assert!(e.contains("13 words at .source_unit.note") && !e.contains("thirteen"), "{e}");
    // A family line with prose.
    let mut f = Fixture::new("long-line");
    f.set_lines("split_families", &[long.to_string()]);
    assert!(err(f.build("t")).contains("assignment line carries 13 words"));
}

#[test]
fn cc_and_public_candidate_hits_exclude_whole_groups_with_receipts() {
    let mut f = Fixture::new("cc");
    f.set_ref("cc_admitted", &[
        // The same WARC record as a stock FineWeb page.
        json!({"refers_to": "<urn:uuid:00000000-0000-0000-0000-000000000002>", "host": "x.example", "registered_domain": "x.example"}),
        // The URL of a reserved unit, under another scheme, case, www. and a trailing slash.
        json!({"url": "http://WWW.blog5.example/post/", "host": "y.example", "registered_domain": "y.example"}),
        // A page on a stock site.
        json!({"url": "https://blog9.example/other", "host": "blog9.example", "registered_domain": "blog9.example"}),
        // An OpenReview forum page that names a stock group.
        json!({"url": "https://openreview.net/forum?id=AbC123", "host": "openreview.net", "registered_domain": "openreview.net"}),
    ]);
    f.set_ref("public_candidate_records", &[json!({"source_group": "news_story:6", "provenance": {"aliases": ["forum_post:40b"]}})]);
    let m = f.build("cc").unwrap();
    assert_eq!(m["gate"]["verdict"], "pass");
    let r = f.receipts("cc");
    let want: BTreeSet<&str> = ["fineweb_site:blog2.example", "fineweb_site:blog5.example", "fineweb_site:blog9.example",
        "openreview:forum:AbC123", "news_desk:6", "forum_author:40"].into();
    assert_eq!(r.keys().map(String::as_str).collect::<BTreeSet<_>>(), want);
    // Each receipt names only the checks that hit its own group.
    assert_eq!(r["fineweb_site:blog5.example"], ["cc_mainnet_stock:url"]);
    assert_eq!(r["fineweb_site:blog9.example"], ["cc_mainnet_stock:site"]);
    assert_eq!(r["fineweb_site:blog2.example"], ["cc_mainnet_stock:warc_record_id"]);
    assert_eq!(r["openreview:forum:AbC123"], ["cc_mainnet_stock:source_host_url_key"]);
    assert_eq!(r["forum_author:40"], ["public_candidate_v1:key"]);
    let groups = f.read_out("cc", "groups.jsonl");
    for k in &want {
        assert!(!groups.contains(&format!("\"{k}\"")), "{k} stays out of the stock");
    }
    assert_eq!(m["groups"]["rows"], GROUPS + INDEX_LEFT - 6);
    let gate = f.out_json("cc", "gate.json");
    assert_eq!(gate["remaining_overlap"], 0);
    assert_eq!(gate["excluded_by_reason"]["cc_mainnet_stock:url"], 1);
}

#[test]
fn cc_hosts_are_normalized_and_cc_group_keys_are_matched() {
    let mut f = Fixture::new("www");
    f.set_ref("cc_admitted", &[
        // The beatblog shape: a www. host on a platform domain, the row's own group naming the blog.
        json!({"url": "http://www.foo.platform.example/", "host": "www.foo.platform.example",
            "registered_domain": "platform.example", "group": "cc_host:foo.platform.example"}),
        // Only the row's group names a stock site.
        json!({"url": "https://elsewhere.example/a", "host": "elsewhere.example", "registered_domain": "elsewhere.example",
            "group": "cc_site:blog7.example"}),
        // A mobile host with a trailing dot.
        json!({"url": "https://m.blog8.example./a", "host": "m.blog8.example.", "registered_domain": "blog8.example."}),
    ]);
    f.build("t").unwrap();
    let r = f.receipts("t");
    assert_eq!(r.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        ["fineweb_blog:foo.platform.example", "fineweb_site:blog7.example", "fineweb_site:blog8.example"].into());
}

#[test]
fn a_cc_row_on_a_source_host_without_a_key_rule_refuses() {
    let mut f = Fixture::new("host");
    f.set_ref("cc_admitted", &[json!({"url": "https://forum.example/t/1", "host": "forum.example", "registered_domain": "example"})]);
    let e = err(f.build("t"));
    assert!(e.contains("rows on a stock source host that no key rule resolves"), "{e}");
    // Without host fields, the host comes from the URL.
    let mut f = Fixture::new("nohost");
    f.set_ref("cc_admitted", &[json!({"url": "https://forum.example/t/1"})]);
    assert!(err(f.build("t")).contains("no key rule resolves"));
    // A subdomain of a declared registered domain is on it too.
    let mut f = Fixture::new("subdomain");
    f.set_ref("cc_admitted", &[json!({"url": "https://api.openreview.net/notes?id=1", "host": "api.openreview.net"})]);
    assert!(err(f.build("t")).contains("no key rule resolves"));
    // An OpenReview page that names no stock group is cleared by its key.
    let mut f = Fixture::new("host-ok");
    f.set_ref("cc_admitted", &[json!({"url": "https://openreview.net/forum?id=Zzz", "registered_domain": "openreview.net"})]);
    assert_eq!(f.build("t").unwrap()["exclusions"]["rows"], 0);
}

#[test]
fn scipost_pages_resolve_by_submission_and_profiles_hold_no_document() {
    assert_eq!(stock::scipost_url_key("https://scipost.org/submissions/scipost_202001_00001v2/"),
        UrlKey::Key("scipost_submission:scipost_202001_00001".into()));
    assert_eq!(stock::scipost_url_key("https://scipost.org/contributor/1200"), UrlKey::NoDocument);
    assert_eq!(stock::scipost_url_key("https://scipost.org/SciPostPhys.1.2.003"), UrlKey::Unresolved);
    let mut f = Fixture::new("scipost");
    f.set_ref("cc_admitted", &[
        json!({"url": "https://scipost.org/submissions/scipost_202001_00001v2/", "host": "scipost.org", "registered_domain": "scipost.org"}),
        // Another version of an arXiv-based submission, no host fields.
        json!({"url": "https://www.scipost.org/submissions/1606.04401v3/#report_1"}),
        json!({"url": "https://scipost.org/contributor/1200", "host": "scipost.org", "registered_domain": "scipost.org"}),
    ]);
    f.build("t").unwrap();
    let r = f.receipts("t");
    assert_eq!(r.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        ["openreview:forum:1606.04401v2", "openreview:forum:scipost_202001_00001v2"].into());
    // A SciPost page the rule cannot place refuses.
    let mut f = Fixture::new("scipost-other");
    f.set_ref("cc_admitted", &[json!({"url": "https://scipost.org/SciPostPhys.1.2.003", "host": "scipost.org"})]);
    assert!(err(f.build("t")).contains("no key rule resolves"));
}

#[test]
fn mirror_host_rows_are_counted_for_the_text_check_without_excluding_or_refusing() {
    let mut f = Fixture::new("mirror");
    f.set_ref("cc_admitted", &[json!({"url": "https://www.mirror.example/story/3", "host": "www.mirror.example", "registered_domain": "mirror.example"})]);
    let m = f.build("t").unwrap();
    assert_eq!(m["exclusions"]["rows"], 0);
    let gate = f.out_json("t", "gate.json");
    assert_eq!(gate["mirror_host_rows_pending_text_check"]["news mirror.example"], 1);
    let screens = m["acquisition_screens"].as_array().unwrap();
    let mirror = screens.iter().find(|s| s["id"] == "mirror_hosts").unwrap();
    assert_eq!(mirror["rows_by_domain_host"]["news mirror.example"], 1);
}

#[test]
fn held_rows_and_blocked_writers_exclude_their_groups() {
    let mut f = Fixture::new("held");
    // A held Train-side row by a stock writer, under the other FineWeb prefix.
    let i = f.allow.gate.iter().position(|g| g.role == "train_held" && g.kind == "split_manifest").unwrap();
    let path = f.allow.gate[i].path.clone();
    f.allow.gate[i].sha256 = f.repo.jsonl(&path, &[json!({"split": "held", "writer_key": "fineweb_blog:blog3.example",
        "collection": "fineweb_blog_cc_historical"})]).sha256;
    f.set_ref("blocked_groups", &[json!({"blocked": "writer_has_fineweb_blog_pool_text", "group_key": "fineweb_site:blog4.example",
        "family_keys": ["fineweb_page:00000000-0000-0000-0000-000000000004"], "split_group_keys": ["fineweb_site:blog4.example"]})]);
    f.build("t").unwrap();
    let r = f.receipts("t");
    assert_eq!(r["fineweb_site:blog3.example"], ["train_held:site_key"]);
    // The blocked group's own key is also a site key.
    assert_eq!(r["fineweb_site:blog4.example"], ["train_held:key", "train_held:site_key"]);
    assert_eq!(r.len(), 2);
}

#[test]
fn the_reference_models_training_lineage_excludes_by_title_and_by_key() {
    let mut f = Fixture::new("lineage");
    // S1: titles of an index page and of a group reservation's unit.
    f.set_lines("title_sha256_list", &[title("Page 20"), title("Thread 9"), title("Unreserved")]);
    // The trainer bundle holds a row keyed by a stock group (its text is never read as a key).
    f.set_ref("trainer_bundle_rows", &[
        json!({"id": "b-1", "source_group": "news_desk:5", "text": "news_desk:6", "text_sha256": sha256_hex(b"b")}),
    ]);
    let m = f.build("t").unwrap();
    let r = f.receipts("t");
    assert_eq!(r["wikipedia:page:20"], ["r0_training_lineage:raid_train_title_lineage"]);
    assert_eq!(r["forum_author:9"], ["r0_training_lineage:raid_train_title_lineage"]);
    assert_eq!(r["news_desk:5"], ["r0_training_lineage:key"]);
    assert!(!r.contains_key("news_desk:6"), "text is skipped");
    assert_eq!(r.len(), 3);
    // The manifest records S1 as applied and S2, S3 and the text-level screens as mandatory.
    let screens = m["acquisition_screens"].as_array().unwrap();
    let ids: Vec<&str> = screens.iter().map(|s| s["id"].as_str().unwrap()).collect();
    for id in ["S1_title", "S2_reddit_author", "S3_text", "near_duplicate_text", "mirror_hosts", "length_filter", "text_digest_gate"] {
        assert!(ids.contains(&id), "{id} missing from {ids:?}");
    }
    assert!(screens[0]["status"].as_str().unwrap().contains("2 groups excluded"));
    assert!(screens[1]["status"].as_str().unwrap().starts_with("mandatory at acquisition"));
    // The lineage references must be the pre-registered screen's inputs.
    let mut f = Fixture::new("lineage-bind");
    f.auto = false;
    f.set_lines("title_sha256_list", &[title("Another list")]);
    let e = err(f.build("t"));
    assert!(e.contains("the title list is not the screen's input"), "{e}");
}

#[test]
fn every_split_file_in_sha256sums_must_be_gated_with_its_recorded_digest() {
    // A split file the allow-list does not account for refuses.
    let mut f = Fixture::new("sums-extra");
    f.repo.jsonl("data/res/dev/groups.jsonl", &[json!({"domain": "forum", "group_key": "forum_author:7"})]);
    let e = err(f.build("t"));
    assert!(e.contains("data/res/dev/groups.jsonl is a split file in SHA256SUMS that the gate does not account for"), "{e}");
    // An evaluation file left out: the role is required, and so is the file.
    let mut f = Fixture::new("sums-noeval");
    f.allow.gate.retain(|g| g.role != "evaluation");
    let e = format!("{:#}", StockAllowlist::parse(&f.bytes()).unwrap_err());
    assert!(e.contains("needs at least one evaluation"), "{e}");
    // An empty substitute for a Train file, digest and count updated, differs from SHA256SUMS.
    let mut f = Fixture::new("sums-subst");
    f.auto = false;
    f.set_ref("split_manifest", &[]);
    let e = err(f.build("t"));
    assert!(e.contains("data/res/train/manifest.jsonl is allow-listed with digest") && e.contains("SHA256SUMS records"), "{e}");
    // A recorded row count that differs refuses.
    let mut f = Fixture::new("rows");
    f.allow.gate[0].rows += 1;
    let e = err(f.build("t"));
    assert!(e.contains("has 1 rows, the allow-list records 2"), "{e}");
    // A declared parent reservation whose digest differs refuses.
    let mut f = Fixture::new("sums-parent");
    f.allow.not_gated[0].sha256 = sha256_hex(b"other");
    assert!(err(f.build("t")).contains("SHA256SUMS records"));
}

#[test]
fn text_digests_are_compared_once_the_stock_carries_them() {
    let mut f = Fixture::new("digest");
    let a = f.parsed();
    let groups = vec![stock::StockGroup {
        reservation: "base".into(),
        domain: "forum".into(),
        group_key: "forum_author:solo".into(),
        family_keys: vec!["forum_post:solo".into()],
        units: 1,
        expected_udocs: 1,
    }];
    let mut ix = gate::StockIndex::build(&groups).unwrap();
    ix.add_digest(&sha256_hex(b"cc1"), 0).unwrap();
    let g = gate::run(&f.repo.root, &a, &groups, &ix).unwrap();
    assert_eq!(g.excluded[&0].iter().collect::<Vec<_>>(), ["cc_mainnet_stock:text_digest"]);
    assert_eq!(g.report["text_digest_check"], "run");
    // The same digest in Train refuses, and a trainer-bundle digest excludes.
    let mut f = Fixture::new("digest-train");
    f.set_ref("split_manifest", &[json!({"text_sha256": sha256_hex(b"cc1")})]);
    let a = f.parsed();
    assert!(format!("{:#}", gate::run(&f.repo.root, &a, &groups, &ix).err().unwrap()).contains("(train) shares 1"));
    let mut f = Fixture::new("digest-bundle");
    f.set_ref("cc_admitted", &[json!({"url": "https://other.example/b", "host": "other.example"})]);
    f.set_ref("trainer_bundle_rows", &[json!({"id": "b", "text": "x", "text_sha256": sha256_hex(b"cc1")})]);
    let a = f.parsed();
    let g = gate::run(&f.repo.root, &a, &groups, &ix).unwrap();
    assert_eq!(g.excluded[&0].iter().collect::<Vec<_>>(), ["r0_training_lineage:text_digest"]);
}

#[test]
fn the_allow_list_refuses_sealed_paths_and_wrong_roles() {
    let mut base = Fixture::new("allow");
    base.sync();
    let check = |edit: &dyn Fn(&mut StockAllowlist), want: &str| {
        let mut a = base.allow.clone();
        edit(&mut a);
        let e = format!("{:#}", StockAllowlist::parse(&serde_json::to_vec(&a).unwrap()).unwrap_err());
        assert!(e.contains(want), "{e} lacks {want}");
    };
    check(&|a| a.group_reservations[0].path = "data/res/test/reserved-unassigned/groups.jsonl".into(), "refused");
    check(&|a| a.group_reservations[0].path = "data/res/tier-c/groups.jsonl".into(), "Tier C");
    check(&|a| a.unit_urls[0].path = "data/res/sealed/units.jsonl".into(), "refused");
    check(&|a| a.unit_titles[0].path = "data/res/cal/titles.jsonl".into(), "refused");
    check(&|a| a.split = "test".into(), "only reserved_unassigned");
    check(&|a| a.purpose = "testnet_plumbing_only".into(), "purpose");
    check(&|a| a.gate[0].path = "data/res/test/manifest.jsonl".into(), "refused");
    check(&|a| a.gate[0].kind = "cc_admitted".into(), "cannot read kind");
    check(&|a| a.gate[0].role = "dev_iter".into(), "unknown gate role");
    check(&|a| a.gate.retain(|g| g.role != "public_candidate_v1"), "needs at least one public_candidate_v1");
    check(&|a| a.gate.retain(|g| g.role != "r0_training_lineage"), "needs at least one r0_training_lineage");
    check(&|a| a.gate.retain(|g| g.role != "train_held"), "needs at least one train_held");
    check(&|a| a.gate[9].path = "/etc/passwd".into(), "not a relative repository path");
    check(&|a| a.gate[3].path = "data/../x".into(), "normalized");
    check(&|a| a.source_hosts[0].url_key_rule = Some("guess".into()), "unknown URL key rule");
    check(&|a| a.source_hosts[0].value = "WWW.openreview.net".into(), "not normalized");
    check(&|a| a.split_sums.path = "data/res/sums.txt".into(), "SHA256SUMS");
    check(&|a| a.assignment_prose_guard.max_words = 0, "prose guard");
    // A stock file whose digest differs refuses.
    let mut f = Fixture::new("digest-differs");
    f.allow.group_reservations[0].sha256 = sha256_hex(b"other");
    let e = err(f.build("t"));
    assert!(e.contains("differs from the allow-list") || e.contains("SHA256SUMS records"), "{e}");
    // A stock row outside the reserved remainder refuses.
    let mut f = Fixture::new("split-row");
    let p = f.allow.group_reservations[0].path.clone();
    let mut g = group("forum", "forum_author:x", &["forum_post:x"]);
    g["split"] = json!("test");
    let r = f.repo.jsonl(&p, &[g]);
    f.allow.group_reservations[0].sha256 = r.sha256;
    f.allow.group_reservations[0].rows = 1;
    let e = err(f.build("t"));
    assert!(e.contains("only reserved_unassigned"), "{e}");
}

#[test]
fn expected_documents_follow_the_test_rules_and_realized_yield() {
    let f = Fixture::new("rules");
    let rules = stock::test_rules(&f.repo.root, &f.allow.test_rules, &f.allow.assignment_prose_guard).unwrap();
    let forum = &rules["forum"];
    assert_eq!((forum.cap, forum.min_yield), (2.0, Some(0.8)));
    assert_eq!(forum.expected(2), 1.6);
    assert_eq!(forum.expected(3), 2.0);
    // Every speech group is capped: a group with at least as many units is capped too.
    let speech = &rules["speech"];
    assert_eq!((speech.min_yield, speech.capped_min_units), (None, Some(4)));
    assert_eq!((speech.expected(4), speech.expected(3)), (20.0, 0.0));
    // Realized over expected on the processed Test groups, never above 1 (wiki realized 12 of 10).
    assert!((rules["news"].realization() - 3.0 / 4.75).abs() < 1e-12);
    assert!((rules["blogs"].realization() - 4.0 / 6.5).abs() < 1e-12);
    assert_eq!(rules["wiki"].realization(), 1.0);
    assert_eq!(rules["wiki"].mass, 12);
    let mut f = Fixture::new("rules-build");
    let m = f.build("r").unwrap();
    assert!(m["capacity"]["capacity_documents"].as_u64().unwrap() <= m["capacity"]["expected_documents"].as_u64().unwrap());
}

#[test]
fn the_yield_margin_is_a_poisson_lower_bound_and_the_weights_hold_at_it() {
    let f = Fixture::new("margin");
    let rules = stock::test_rules(&f.repo.root, &f.allow.test_rules, &f.allow.assignment_prose_guard).unwrap();
    let z = pool_v2::YIELD_MARGIN_Z;
    // Wilson-Hilferty lower bound of 9 realized documents at 95% one-sided: about 4.7.
    let forum = &rules["forum"];
    let lo = forum.realization_lower(z) * forum.processed_expected;
    assert!((lo - 4.70).abs() < 0.05, "{lo}");
    for r in rules.values() {
        assert!(r.realization_lower(z) <= r.realization());
    }
    let mut f = Fixture::new("margin-build");
    let m = f.build("m").unwrap();
    let c = &m["capacity"];
    assert!(c["capacity_lower_documents"].as_u64().unwrap() <= c["capacity_documents"].as_u64().unwrap());
    // At the lower bound the manifest weights last the horizon; at the realized yield at least as long.
    let lower = c["days_before_first_bind_lower_yield"].as_f64().unwrap();
    assert!(lower >= 3.0, "{lower}");
    assert!(c["days_before_first_bind"].as_f64().unwrap() >= lower);
    let cap = f.out_json("m", "capacity.json");
    assert_eq!(cap["yield_margin"]["rule"], pool_v2::YIELD_MARGIN_RULE);
    // Whole-group lumpiness is reported per domain.
    assert!(cap["stock"]["by_domain"]["forum"]["largest_group"]["units"].as_u64().unwrap() == 2);
}

// --------------------------------------------------------------- feasibility

fn one(stock: u64) -> (BTreeMap<String, u64>, BTreeMap<String, u64>) {
    ([("d".to_string(), stock)].into(), [("d".to_string(), PPM)].into())
}

#[test]
fn the_engine_rule_cannot_fill_twenty_fifty_five_twenty_five() {
    assert_eq!(kind_counts(1000, [20, 55, 25]), [200, 550, 250]);
    let (s, w) = one(1_000_000);
    let p = Params::new(1000, [20, 55, 25], Reuse::EngineV1, 0);
    let o = capacity::simulate(&s, &w, &p).unwrap();
    assert_eq!(o.cohorts_with_reweighting, 0);
    assert!(o.stop.contains("mixed parents 200 of 550"), "{}", o.stop);
    // As coded (40/40/20) it fills, taking 400 fresh humans a cohort.
    let p = Params { mix: [40, 40, 20], ..p };
    let o = capacity::simulate(&one(4000).0, &w, &p).unwrap();
    assert_eq!(o.cohorts_with_reweighting, 10);
    assert_eq!(o.fresh_taken["cohort_1"], 400);
    // Over several domains and after a probe, children still follow the drawn negatives.
    let stock: BTreeMap<String, u64> = [("a", 3_000), ("b", 1_000), ("c", 410)].iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let p = Params { probe_docs: 1024, ..p };
    let o = capacity::simulate(&stock, &capacity::stock_weights(&stock), &p).unwrap();
    assert!(o.probe_filled);
    assert_eq!(o.fresh_taken["probe"], 410);
    assert_eq!(o.cohorts_with_reweighting, 10, "{}", o.stop);
}

#[test]
fn reuse_with_four_uses_seven_apart_fills_the_mix_and_settles_at_the_negatives() {
    let p = Params::new(1000, [20, 55, 25], Reuse::Ledger { max_uses: 4, min_gap: 7 }, 1024);
    assert_eq!(kind_counts(1024, [20, 55, 25]), [205, 563, 256]);
    let (s, w) = one(100_000);
    let o = capacity::simulate(&s, &w, &p).unwrap();
    // Probe: 205 negatives and 358 fresh parents, never reused.
    assert_eq!(o.fresh_taken["probe"], 563);
    // Cohorts 1-7 need 350 fresh parents each; from cohort 8 the first cohorts' passages return.
    assert_eq!(o.fresh_taken["cohort_1"], 550);
    assert_eq!(o.fresh_taken["cohort_8"], 200);
    assert_eq!(o.fresh_taken["last_cohort"], 200);
    let used = 563 + 7 * 550;
    assert_eq!(o.cohorts_with_reweighting as u64, 7 + (100_000 - used) / 200);
    assert_eq!(capacity::demand(&w, &p, 30).unwrap()["d"], 563 + 7 * 550 + 23 * 200);
}

#[test]
fn canary_audits_and_new_probe_versions_take_fresh_stock() {
    let base = Params::new(1000, [20, 55, 25], Reuse::Ledger { max_uses: 4, min_gap: 7 }, 1024);
    let (s, w) = one(100_000);
    let plain = capacity::simulate(&s, &w, &base).unwrap();
    // One candidate every 10 cohorts with a re-audit: 2 x 2,000 documents per candidate.
    let p = Params { canary: Some(Canary { docs: 2_000, audits: 2, every: 10 }), ..base.clone() };
    let o = capacity::simulate(&s, &w, &p).unwrap();
    assert_eq!(o.fresh_by_purpose["canary"], u64::from(o.canary_audits) * 2_000);
    assert_eq!(o.canary_audits, 2 * (o.cohorts_with_reweighting / 10));
    assert!(o.cohorts_with_reweighting < plain.cohorts_with_reweighting);
    let total: u64 = o.fresh_by_purpose.values().sum();
    assert!(total <= 100_000 && 100_000 - total < 4_000 + 550, "{total}");
    // The demand the weights are filled on includes the plan's canary audits.
    assert_eq!(capacity::demand(&w, &p, 20).unwrap()["d"], capacity::demand(&w, &base, 20).unwrap()["d"] + 2 * 4_000);
    // A canary the stock cannot fill stops the run at that cohort.
    let big = Params { canary: Some(Canary { docs: 60_000, audits: 2, every: 5 }), ..base.clone() };
    let o = capacity::simulate(&s, &w, &big).unwrap();
    assert_eq!(o.cohorts_with_reweighting, 4);
    assert!(o.stop.contains("cohort 5 canary audit"), "{}", o.stop);
    // With probe re-draws, every reweighting draws a new probe version.
    let stock: BTreeMap<String, u64> = [("a", 30_000), ("b", 2_000)].iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let weights: BTreeMap<String, u64> = [("a", 500_000), ("b", 500_000)].iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let once = capacity::simulate(&stock, &weights, &base).unwrap();
    let redraw = Params { probe_redraw_on_reweight: true, ..base.clone() };
    let o = capacity::simulate(&stock, &weights, &redraw).unwrap();
    assert_eq!(once.probe_versions, 1);
    assert_eq!(o.probe_versions, 2);
    assert_eq!(o.fresh_by_purpose["probe"], 563 * 2);
    assert!(o.cohorts_with_reweighting < once.cohorts_with_reweighting);
}

#[test]
fn the_capacity_report_states_canary_days_under_promotion_rates() {
    let mut f = Fixture::new("canary");
    let m = f.build("c").unwrap();
    let cap = f.out_json("c", "capacity.json");
    let rows = cap["canary"]["scenarios"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "one interval, one and two audits");
    let one_audit = rows.iter().find(|r| r["audits_per_candidate"] == 1).unwrap();
    let two = rows.iter().find(|r| r["audits_per_candidate"] == 2).unwrap();
    assert!(two["days_with_reweighting"].as_f64().unwrap() <= one_audit["days_with_reweighting"].as_f64().unwrap());
    assert_eq!(cap["canary"]["in_manifest_weights"], false);
    assert!(m["capacity"]["canary"].as_str().unwrap().contains("reserve no canary audit"));
    // A plan canary is reserved in the weights.
    let mut f = Fixture::new("canary-plan");
    let mut bp = params();
    bp.plan.canary = Some(Canary { docs: 2, audits: 1, every: 3 });
    let m = f.build_with("p", &bp).unwrap();
    assert!(m["capacity"]["canary"].as_str().unwrap().contains("reserve the plan's canary audits"));
}

/// Passage-level reference: every passage is tracked on its own.
fn brute_force(stock: u64, p: &Params, cohorts: u32) -> Vec<u64> {
    let Reuse::Ledger { max_uses, min_gap } = p.reuse else { unreachable!() };
    let [h, m, _] = kind_counts(p.n, p.mix);
    struct P {
        last: Option<u32>,
        uses: u32,
        neg_at: u32,
    }
    let mut ps: Vec<P> = Vec::new();
    let mut fresh_left = stock;
    let mut out = Vec::new();
    for c in 1..=cohorts {
        if fresh_left < u64::from(h) {
            break;
        }
        let mut taken = 0u64;
        let start = ps.len();
        for _ in 0..h {
            ps.push(P { last: None, uses: 0, neg_at: c });
        }
        fresh_left -= u64::from(h);
        taken += u64::from(h);
        let mut need = m;
        for q in ps[start..].iter_mut() {
            if need == 0 {
                break;
            }
            q.last = Some(c);
            q.uses = 1;
            need -= 1;
        }
        // Oldest-eligible first: eligible since max(neg_at + 1 if never used, last + gap).
        let mut cands: Vec<(u32, u32, usize)> = ps[..start]
            .iter()
            .enumerate()
            .filter(|(_, q)| q.uses < max_uses && q.last.is_none_or(|l| c >= l + min_gap))
            .map(|(i, q)| (q.last.map_or(q.neg_at + 1, |l| l + min_gap), max_uses - q.uses, i))
            .collect();
        cands.sort();
        for (_, _, i) in cands {
            if need == 0 {
                break;
            }
            ps[i].last = Some(c);
            ps[i].uses += 1;
            need -= 1;
        }
        if u64::from(need) > fresh_left {
            break;
        }
        for _ in 0..need {
            ps.push(P { last: Some(c), uses: 1, neg_at: 0 });
        }
        fresh_left -= u64::from(need);
        taken += u64::from(need);
        out.push(taken);
    }
    out
}

#[test]
fn the_bucketed_ledger_equals_a_passage_level_reference() {
    for (n, mix, k, g) in [(10, [20, 55, 25], 4, 7), (10, [20, 55, 25], 2, 1), (9, [30, 60, 10], 3, 2), (12, [50, 40, 10], 4, 7), (7, [15, 70, 15], 4, 3)] {
        let p = Params::new(n, mix, Reuse::Ledger { max_uses: k, min_gap: g }, 0);
        let want = brute_force(200, &p, 10_000);
        let mut got = Vec::new();
        for c in 1..=want.len() as u32 {
            let total: u64 = capacity::demand(&one(0).1, &p, c).unwrap()["d"];
            got.push(total);
        }
        let mut cum = 0;
        let want_cum: Vec<u64> = want.iter().map(|x| {
            cum += x;
            cum
        }).collect();
        assert_eq!(got, want_cum, "n {n} mix {mix:?} K {k} G {g}");
        let o = capacity::simulate(&one(200).0, &one(0).1, &p).unwrap();
        assert_eq!(o.cohorts_with_reweighting as usize, want.len(), "n {n} mix {mix:?} K {k} G {g}");
    }
}

#[test]
fn reweighting_retires_empty_domains_and_holds_small_ones_to_the_horizon() {
    let base: BTreeMap<String, u64> = [("big", 300), ("mid", 300), ("small", 300), ("empty", 100)].iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let stock: BTreeMap<String, u64> = [("big", 50_000), ("mid", 3_000), ("small", 400), ("empty", 0)].iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let p = Params::new(1000, [20, 55, 25], Reuse::Ledger { max_uses: 4, min_gap: 7 }, 1024);
    let rw = capacity::waterfill(&base, &stock, &p, 30).unwrap();
    assert_eq!(rw.retired, ["empty"]);
    assert!(rw.reduced.contains(&"small".to_string()) && rw.reduced.contains(&"mid".to_string()));
    assert_eq!(rw.weights_ppm.values().sum::<u64>(), PPM);
    let o = capacity::simulate(&stock, &rw.weights_ppm, &p).unwrap();
    assert!(o.cohorts_before_first_bind >= 30, "first bind after {}", o.cohorts_before_first_bind);
    assert!(rw.estimand_change_tv_ppm > 100_000);
    // At the stock-proportional limit every domain lasts about equally long.
    let sp = capacity::simulate(&stock, &capacity::stock_weights(&stock), &p).unwrap();
    assert!(sp.cohorts_with_reweighting >= o.cohorts_before_first_bind);
    // A horizon the whole stock cannot reach is refused.
    assert!(capacity::waterfill(&base, &stock, &p, 10_000).is_err());
}

#[test]
fn quotas_track_small_weights_over_time() {
    // A domain at 1% of 200 negatives takes two a cohort; one at 0.25% takes one every other cohort.
    let w: BTreeMap<String, u64> = [("a", 987_500), ("b", 10_000), ("c", 2_500)].iter().map(|(k, v)| (k.to_string(), *v)).collect();
    // Negatives only (no mixed children), so the demand is the quota itself.
    let p = Params::new(1000, [20, 0, 80], Reuse::Ledger { max_uses: 4, min_gap: 7 }, 0);
    for cohorts in [1, 2, 3, 7, 40] {
        let d = capacity::demand(&w, &p, cohorts).unwrap();
        assert_eq!(d.values().sum::<u64>(), 200 * u64::from(cohorts));
        for (k, share) in [("b", 0.01), ("c", 0.0025)] {
            let exact = share * 200.0 * f64::from(cohorts);
            assert!((d[k] as f64 - exact).abs() <= 1.0, "{k} after {cohorts}: {} vs {exact}", d[k]);
        }
    }
}

// -------------------------------------------------------------------- replay

#[test]
fn the_frozen_engine_is_the_one_that_drew_the_record() {
    let r = Record::parse(TRACKED_RECORD.as_bytes()).unwrap();
    r.check_engine().unwrap();
    assert_eq!(r.engine.commit, "9051a52a7b397256a8183fc850fdd3204fc49bcf");
    // A record naming other engine files refuses both replays.
    let mut v: Value = serde_json::from_str(TRACKED_RECORD).unwrap();
    v["engine"]["pool_rs_sha256"] = json!(sha256_hex(b"a later pool.rs"));
    let bad = Record::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(format!("{:#}", replay::replay_counts(&bad).unwrap_err()).contains("is not the engine that drew the record"));
}

#[test]
fn the_recorded_v1_draws_replay_from_counts() {
    let r = Record::parse(TRACKED_RECORD.as_bytes()).unwrap();
    let out = replay::replay_counts(&r).unwrap();
    assert_eq!(out["used_rows"], 484);
    assert_eq!(out["remaining_rows"], 86);
    // Any change to a recorded quota is caught.
    let mut v: Value = serde_json::from_str(TRACKED_RECORD).unwrap();
    v["cohorts"][1]["draw"]["quotas"]["wikinews"] = json!(17);
    v["cohorts"][1]["draw"]["quotas"]["wikipedia"] = json!(114);
    let bad = Record::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(format!("{:#}", replay::replay_counts(&bad).unwrap_err()).contains("quotas"));
    // So is a shortfall drill that would have drawn.
    let mut v: Value = serde_json::from_str(TRACKED_RECORD).unwrap();
    v["cohorts"][3]["n"] = json!(200);
    let bad = Record::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(replay::replay_counts(&bad).is_err());
}

/// A synthetic v1 pool in the engine's layout, with whole groups of one to three rows.
fn v1_pool(repo: &Repo, dir: &str, by_domain: &BTreeMap<String, u64>) -> String {
    let mut rows = Vec::new();
    let mut body = String::new();
    for (d, n) in by_domain {
        for i in 0..*n {
            let id = format!("{d}-{i}");
            let text = format!("Synthetic passage {id}. It has two sentences.");
            let sha = sha256_hex(text.as_bytes());
            // Mostly single-row groups, with a few pairs, as in the real pool.
            let g = if i % 10 == 9 { format!("{d}-g{}", i - 1) } else { format!("{d}-g{i}") };
            rows.push(json!({"row_id": id, "text_sha256": sha, "domain": d, "source_group": g, "shard": 0}));
            body.push_str(&format!("{}\n", json!({"schema": pool::SHARD_SCHEMA, "row_id": id, "text_sha256": sha,
                "domain": d, "source_group": g, "text": text})));
        }
    }
    let shard_sha = repo.write(&format!("{dir}/shard-0000.jsonl"), body.as_bytes());
    let m = json!({"schema": pool::POOL_SCHEMA, "pool_id": "testnet-human-pool-v1",
        "shards": [{"index": 0, "file": "shard-0000.jsonl", "rows": rows.len(), "sha256": shard_sha}], "rows": rows});
    repo.write(&format!("{dir}/manifest.json"), &serde_json::to_vec(&m).unwrap())
}

#[test]
fn the_engine_draw_reproduces_the_seed_free_quantities_of_the_record() {
    let repo = Repo::new("replay");
    let mut v: Value = serde_json::from_str(TRACKED_RECORD).unwrap();
    let by_domain: BTreeMap<String, u64> = serde_json::from_value(v["pool"]["by_domain"].clone()).unwrap();
    let dir = "data/pools/testnet-human-pool-v1";
    let sha = v1_pool(&repo, dir, &by_domain);
    v["pool"]["manifest_sha256"] = json!(sha);
    let r = Record::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    let out = replay::replay_pool(&repo.root, dir, &r).unwrap();
    assert_eq!(out["used_rows"], 484);
    assert_eq!(out["rows_with_two_sentences"], 570);
    // A parent with one sentence cannot carry a mixed child: the recorded mixed count no longer holds.
    let mut v2 = v.clone();
    v2["cohorts"][0]["draw"]["mixed"] = json!(79);
    let bad = Record::parse(&serde_json::to_vec(&v2).unwrap()).unwrap();
    assert!(format!("{:#}", replay::replay_pool(&repo.root, dir, &bad).unwrap_err()).contains("mixed"));
}

#[test]
#[ignore = "reads the private testnet-human-pool-v1 under data/; set SLOP_NINJA_REPO to the main checkout"]
fn the_recorded_v1_draws_replay_on_the_private_pool() {
    let repo = std::env::var("SLOP_NINJA_REPO").expect("SLOP_NINJA_REPO");
    let r = Record::parse(TRACKED_RECORD.as_bytes()).unwrap();
    let out = replay::replay_pool(Path::new(&repo), "data/task-roots/testnet/m4-eval/pools/testnet-human-pool-v1", &r).unwrap();
    assert_eq!(out["used_rows"], 484);
}

#[test]
fn the_tracked_allow_lists_parse() {
    assert!(TRACKED_ALLOWLIST.contains("testnet_plumbing_only"));
    let a = StockAllowlist::parse(stock::TRACKED_STOCK_ALLOWLIST.as_bytes()).unwrap();
    assert_eq!(a.purpose, stock::STOCK_PURPOSE);
    for (role, _) in stock::ROLES {
        assert!(a.gate.iter().any(|g| g.role == role), "{role}");
    }
    assert!(a.source_hosts.iter().any(|h| h.value == "scipost.org"));
}

#[test]
fn hosts_and_urls_normalize() {
    assert_eq!(stock::normalize_host("WWW.Beatblog.Typepad.com."), "beatblog.typepad.com");
    assert_eq!(stock::normalize_host("m.example.org:8080"), "example.org");
    assert_eq!(stock::normalize_host("m.co"), "m.co");
    assert_eq!(stock::url_key("HTTP://www.Blog5.example/post/#frag").as_deref(), Some("blog5.example/post"));
    assert_eq!(stock::strip_version("1606.04401v12"), "1606.04401");
    assert_eq!(stock::strip_version("scipost_202001_00001"), "scipost_202001_00001");
}
