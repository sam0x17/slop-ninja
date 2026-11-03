//! Tests of the `public-candidate-v1` selection, rights and gate rules on a
//! synthetic store (no corpus text).

use crate::public_candidate::{self, testing};
use crate::{db, link, plan, runner};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

fn write(p: &Path, s: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, s).unwrap();
}

fn long(tag: &str) -> String {
    format!("{tag} {}", "This synthetic passage exists only for testing the release rules here. ".repeat(4))
}

fn h(s: &str) -> String {
    hex::encode(crate::skeleton::sha256(s.as_bytes()))
}

fn jsonl(rows: &[Value]) -> String {
    rows.iter().map(|r| format!("{r}\n")).collect()
}

fn read_table(dir: &Path, name: &str) -> Vec<Value> {
    let b = zstd::decode_all(fs::File::open(dir.join(format!("{name}.jsonl.zst"))).unwrap()).unwrap();
    String::from_utf8(b).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    store: PathBuf,
    texts: HashMap<&'static str, String>,
}

fn human(id: &str, text: &str, lic: &str, coll: &str, group: &str, flag: bool, extra: Value) -> Value {
    let mut v = json!({
        "id": id, "text": text, "licence": lic, "collection": coll, "source_group": group, "origin": "human_only",
        "rights": {"license": lic, "attribution": format!("Author of {id}"), "redistribute_text": flag, "admission_decision": "operator note"},
        "provenance": {"title": format!("Title {id}"), "source_url": format!("https://example.org/{id}"), "local_path": "/home/someone/x"},
    });
    if let (Value::Object(m), Value::Object(e)) = (&mut v, extra) {
        for (k, x) in e {
            if k == "rights" || k == "provenance" {
                for (kk, xx) in x.as_object().unwrap() {
                    m.get_mut(&k).unwrap()[kk] = xx.clone();
                }
            } else {
                m.insert(k, x);
            }
        }
    }
    v
}

fn fixture() -> Fixture {
    let t = tempfile::tempdir().unwrap();
    let repo = t.path().join("repo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    let names = [
        "ok", "flag", "override", "fineweb", "unused", "reserved", "sealed", "uspto_old", "uspto_new", "unknown", "noattr", "private_ds",
        "published",
    ];
    let mut texts: HashMap<&'static str, String> = HashMap::new();
    for n in names {
        texts.insert(n, long(n));
    }
    let tx = |n: &str| texts[n].clone();
    let pool = vec![
        human("pool-ok", &tx("ok"), "CC-BY-4.0", "wiki", "g-ok", true, json!({})),
        human("pool-flag", &tx("flag"), "CC-BY-4.0", "flagcoll", "g-flag", false, json!({})),
        human("pool-override", &tx("override"), "CC-BY-SA-3.0", "overcoll", "g-over", false, json!({})),
        human("pool-fineweb", &tx("fineweb"), "ODC-By-1.0-database+CommonCrawl-terms;page-text-unlicensed", "fineweb", "g-fw", false, json!({})),
        human("pool-unused", &tx("unused"), "CC-BY-4.0", "wiki", "g-unused", true, json!({})),
        human("pool-reserved", &tx("reserved"), "CC-BY-4.0", "wiki", "g-res", true, json!({})),
        human("pool-sealed", &tx("sealed"), "CC-BY-4.0", "wiki", "g-sealed", true, json!({})),
        human("pool-uspto-old", &tx("uspto_old"), "uspto-published-application-public-record;x", "uspto", "g-u1", true,
              json!({"provenance": {"filing_date": "2000-12-15", "publication_date": "2001-04-26"}})),
        human("pool-uspto-new", &tx("uspto_new"), "uspto-published-application-public-record;x", "uspto", "g-u2", true,
              json!({"provenance": {"filing_date": "2017-12-15", "publication_date": "2019-04-26"}})),
        human("pool-unknown", &tx("unknown"), "some-unreviewed-licence", "wiki", "g-unk", true, json!({})),
        json!({"id": "pool-noattr", "text": tx("noattr"), "licence": "CC-BY-4.0", "collection": "nocredit", "source_group": "g-na",
               "origin": "human_only", "rights": {"license": "CC-BY-4.0", "redistribute_text": true}, "provenance": {}}),
    ];
    write(&repo.join("data/corpus/pool/docs.jsonl"), &jsonl(&pool));
    // A private root whose path contains `/datasets/`: never published, always refused.
    write(&repo.join("data/private/datasets/leak/rows.jsonl"),
          &jsonl(&[human("leak-1", &tx("private_ds"), "CC-BY-4.0", "wiki", "g-leak", true, json!({}))]));
    // A published snapshot rooted exactly at <repo>/datasets/<name>.
    write(&repo.join("datasets/snap/train.jsonl"),
          &jsonl(&[human("pub-1", &tx("published"), "CC-BY-4.0", "wiki", "g-pub", true, json!({})).as_object().unwrap().clone().into_iter()
              .chain([("split".to_string(), json!("train"))]).collect::<serde_json::Map<_, _>>().into()]));
    // Generations: a two-level revision chain, a child of a flag-false parent and a prompt without its parent.
    let g1 = long("generation one");
    let g2 = long("generation two");
    let gflag = long("generation of flagged");
    let gbad = long("generation bad prompt");
    let gseed = long("generation with seed voice");
    let gens = vec![
        json!({"id": "gen-1", "text": g1, "parent_text_sha256": h(&tx("ok")), "generator_key": "testmodel", "origin": "model_only",
               "operation": "controlled_rewrite", "prompt": format!("Rewrite this:\n{}", tx("ok")), "source_group": "g-ok"}),
        json!({"id": "gen-2", "text": g2, "parent_text_sha256": h(&g1), "generator_key": "testmodel", "origin": "model_only",
               "operation": "controlled_rewrite", "prompt": format!("Rewrite this:\n{g1}"), "source_group": "g-ok"}),
        json!({"id": "gen-flag", "text": gflag, "parent_text_sha256": h(&tx("flag")), "generator_key": "testmodel", "origin": "model_only",
               "operation": "controlled_rewrite", "prompt": format!("Rewrite this:\n{}", tx("flag")), "source_group": "g-flag"}),
        json!({"id": "gen-bad", "text": gbad, "parent_text_sha256": h(&tx("ok")), "generator_key": "testmodel", "origin": "model_only",
               "operation": "controlled_rewrite", "prompt": "Rewrite a passage you cannot see.", "source_group": "g-ok"}),
        // A prompt that embeds a private seed voice example (synthetic): published only with the prompt withheld.
        json!({"id": "gen-seed", "text": gseed, "parent_text_sha256": h(&tx("ok")), "generator_key": "testmodel", "origin": "model_only",
               "operation": "controlled_rewrite", "prompt": format!("Rewrite this so it would Sound Like A Synthetic Test Pirate:\n{}", tx("ok")),
               "source_group": "g-ok"}),
        // A generation record whose parent is its own (human) text: ignored, the text stays human.
        json!({"id": "gen-self", "text": tx("ok"), "parent_text_sha256": h(&tx("ok")), "generator_key": "testmodel", "origin": "mixed",
               "operation": "controlled_rewrite", "prompt": format!("Rewrite this:\n{}", tx("ok")), "source_group": "g-ok"}),
    ];
    write(&repo.join("data/corpus/gens/records.jsonl"), &jsonl(&gens));
    // A bundle view that uses the texts (train; one sealed Test row).
    let mut rows: Vec<Value> = ["ok", "flag", "override", "fineweb", "reserved", "uspto_old", "uspto_new", "unknown", "noattr", "private_ds"]
        .iter()
        .map(|n| json!({"id": format!("row-{n}"), "text": tx(n), "split": "train", "source_group": format!("g-{n}")}))
        .collect();
    rows.push(json!({"id": "row-sealed", "text": tx("sealed"), "split": "test_sealed", "source_group": "g-sealed"}));
    for (i, g) in [&g1, &g2, &gflag, &gbad, &gseed].iter().enumerate() {
        rows.push(json!({"id": format!("row-gen-{i}"), "text": g, "split": "dev", "source_group": "g-ok"}));
    }
    write(&repo.join("data/corpus/bundle/train/rows.jsonl"), &jsonl(&rows));
    // Pangram: with text, hash-only (rights) and dropped (reserved, private).
    let items: Vec<Value> = ["ok", "fineweb", "reserved", "private_ds"].iter().enumerate()
        .map(|(i, n)| json!({"id": format!("item-{i}"), "text": tx(n)})).collect();
    write(&repo.join("data/corpus/pangram/request.json"), &json!({"model": "m", "items": items}).to_string());
    let results: Vec<Value> = (0..4).map(|i| json!({"id": format!("item-{i}"), "index": i, "result": {"fraction_ai": 0.25, "version": "4.1",
        "windows": [{"start_index": 0, "end_index": 10, "label": "Human Written", "text": "secret window text"}]}})).collect();
    // A synthetic bulk id in the provider's format (built at run time).
    let bulk = format!("blk_{}", "0123456789abcdef".repeat(2));
    write(&repo.join("data/corpus/pangram/page-0.json"), &json!({"bulk_id": bulk, "items": results}).to_string());
    // Reservation: the group of `reserved`.
    let manifest = json!({"rows": [{"text_sha256": "0".repeat(64), "source_group": "g-res"}]}).to_string();
    write(&repo.join("data/testnet/pools/p1/manifest.json"), &manifest);
    // Private seed examples (synthetic) in a private JSON file.
    write(&repo.join("data/private/seed.json"), &json!({"examples": ["sound like a synthetic test pirate"]}).to_string());
    // Pinned licence files.
    for f in ["CC-BY-4.0.txt", "CC-BY-SA-3.0.txt", "CC-BY-SA-4.0.txt", "uspto.md", "model.LICENSE"] {
        write(&repo.join(format!("lic/{f}")), &format!("licence text {f}\n"));
    }

    let fields = json!({"licence": "licence", "collection": "collection", "source_group": "source_group", "origin": "origin",
                        "parent_text_sha256": "parent_text_sha256", "generator_key": "generator_key", "operation": "operation"});
    let p: plan::Plan = serde_json::from_value(json!({
        "schema": plan::PLAN_SCHEMA,
        "sources": [
            {"id": "pool", "adapter": "generic", "priority": 1, "root": repo.join("data/corpus/pool").to_string_lossy(),
             "inputs": [{"pattern": "docs.jsonl", "role": "rows"}],
             "params": {"text": "text", "id": "id", "role": "pool_document", "fields": fields}},
            {"id": "leak", "adapter": "generic", "priority": 2, "root": repo.join("data/private/datasets/leak").to_string_lossy(),
             "inputs": [{"pattern": "rows.jsonl", "role": "rows"}],
             "params": {"text": "text", "id": "id", "role": "pool_document", "fields": fields}},
            {"id": "ds-snap", "adapter": "generic", "priority": 3, "root": repo.join("datasets/snap").to_string_lossy(),
             "inputs": [{"pattern": "train.jsonl", "role": "records"}],
             "params": {"text": "text", "id": "id", "role": "published_record", "fields": fields,
                        "view": {"experiment": "snap", "split_paths": ["split"], "kind": "published_snapshot"}}},
            {"id": "gens", "adapter": "generic", "priority": 4, "root": repo.join("data/corpus/gens").to_string_lossy(),
             "inputs": [{"pattern": "records.jsonl", "role": "rows"}],
             "params": {"text": "text", "id": "id", "role": "local_record", "fields": fields,
                        "event": {"kind": "local_generate_record", "prompt": "prompt", "generator": "generator_key", "operation": "operation",
                                  "status_const": "accepted_record"}}},
            {"id": "bundle", "adapter": "generic", "priority": 5, "root": repo.join("data/corpus/bundle").to_string_lossy(),
             "inputs": [{"pattern": "*/rows.jsonl", "role": "rows"}],
             "params": {"text": "text", "id": "id", "role": "bundle_row", "fields": {"source_group": "source_group"},
                        "view": {"experiment": "bundle-x", "split_paths": ["split"], "kind": "bundle"}}},
            {"id": "pg", "adapter": "pangram", "priority": 6, "root": repo.join("data/corpus/pangram").to_string_lossy(),
             "inputs": [{"pattern": "request.json", "role": "bulk_request"}, {"pattern": "page-*.json", "role": "observations"}],
             "params": {"store": "tierc_cal", "split_default": "cal"}}
        ]
    }))
    .unwrap();
    let store = t.path().join("store");
    fs::create_dir_all(&store).unwrap();
    let conn = db::open(&store.join("corpus.sqlite")).unwrap();
    runner::run(&conn, &store, &p, &[], false).unwrap();
    link::run(&conn).unwrap();
    drop(conn);
    let mut all = texts.clone();
    all.insert("g1", g1);
    all.insert("g2", g2);
    all.insert("gflag", gflag);
    all.insert("gbad", gbad);
    all.insert("gseed", gseed);
    Fixture { _tmp: t, repo, store, texts: all }
}

fn rules(f: &Fixture, generators: Value) -> PathBuf {
    let pin = |p: &str| h(&fs::read_to_string(f.repo.join(p)).unwrap());
    let r = json!({
        "schema": public_candidate::RULES_SCHEMA, "release": "public-candidate-test",
        "decisions": {"flag_override": "test decision"},
        "private_sources": [], "hard_private_sources": [], "hard_private_record_prefixes": [],
        "private_adapters": ["pangram", "archive"], "private_roles": ["pangram_submission_file"], "private_contexts": ["b_private"],
        "private_roots": ["data/private"], "corpus_roots": ["data/corpus"],
        "published_sources": {"ds-snap": "snap"},
        "reservations": [{"label": "test pool", "path": "data/testnet/pools/p1/manifest.json", "sha256": pin("data/testnet/pools/p1/manifest.json"), "kind": "rows_manifest"}],
        "reservation_globs": ["data/testnet/pools/*/manifest.json"],
        "splits_experiment_prefix": "a-comp-splits-",
        "used_roles": ["train", "dev", "cal", "test_consumed"], "sealed_roles": ["test_sealed", "test", "heldout_test"],
        "licences": [
            {"recorded": "CC-BY-4.0", "class": "cc_by", "licence_id": "CC-BY-4.0", "decision": "allowed"},
            {"recorded": "CC-BY-SA-3.0", "class": "cc_by_sa", "licence_id": "CC-BY-SA-3.0", "decision": "allowed"},
            {"recorded": "uspto-published-application-public-record", "prefix": true, "class": "uspto_pre2019", "licence_id": "uspto-public-record", "decision": "allowed_pre2019"},
            {"recorded": "ODC-By-1.0-database+CommonCrawl-terms;page-text-unlicensed", "class": "excluded", "decision": "fineweb_page_text_unlicensed"}
        ],
        "excluded_collections": {"reddit": "reddit_user_content"},
        "flag_override": {"decision": "test decision", "collections": ["overcoll"]},
        "record_roles": ["pool_document", "published_record", "local_record"],
        "skip_roles": ["bundle_row"],
        "split_document_sources": [],
        "templates": ["controlled_rewrite"], "embed_full_templates": ["controlled_rewrite"], "embed_span_templates": [], "embed_prefix_templates": [],
        "generators": generators,
        "licence_ids": {
            "CC-BY-4.0": {"name": "CC BY 4.0", "url": "https://creativecommons.org/licenses/by/4.0/", "file": "LICENSES/sources/CC-BY-4.0.txt"},
            "CC-BY-SA-3.0": {"name": "CC BY-SA 3.0", "file": "LICENSES/sources/CC-BY-SA-3.0.txt"},
            "CC-BY-SA-4.0": {"name": "CC BY-SA 4.0", "file": "LICENSES/sources/CC-BY-SA-4.0.txt"},
            "uspto-public-record": {"name": "USPTO public record", "file": "LICENSES/sources/uspto.md"}
        },
        "files": [
            {"dest": "LICENSES/sources/CC-BY-4.0.txt", "src": "lic/CC-BY-4.0.txt", "sha256": pin("lic/CC-BY-4.0.txt")},
            {"dest": "LICENSES/sources/CC-BY-SA-3.0.txt", "src": "lic/CC-BY-SA-3.0.txt", "sha256": pin("lic/CC-BY-SA-3.0.txt")},
            {"dest": "LICENSES/sources/CC-BY-SA-4.0.txt", "src": "lic/CC-BY-SA-4.0.txt", "sha256": pin("lic/CC-BY-SA-4.0.txt")},
            {"dest": "LICENSES/sources/uspto.md", "src": "lic/uspto.md", "sha256": pin("lic/uspto.md")},
            {"dest": "LICENSES/models/model.LICENSE", "src": "lic/model.LICENSE", "sha256": pin("lic/model.LICENSE")}
        ],
        "collection_credits": {"wiki": "Wiki contributors", "overcoll": "Override collection", "flagcoll": "Flag collection"},
        "forbidden_substrings": ["grammar-private", "example-private-label", "user decision", "user_decision", "laptop-import"],
        "operator_values": ["operator@example.invalid"], "operator_value_files": [],
        "max_ancestor_depth": 8, "uspto_cutoff": "2019-01-01"
    });
    let mut r = r;
    let extra = json!({
        "private_string_files": [{"path": "data/private/seed.json", "pointer": "/examples"}],
        "withheld_voice_origins": ["seed_hand_written"],
        "inventor_aliases": {"tm": "test-model"},
        "public_names": {"contexts": {"tierc_cal": "public-cal"}, "experiments": {"bundle-x": "bundle-public"}, "view_kinds": {"bundle": "bundle_rows"}}
    });
    for (k, v) in extra.as_object().unwrap() {
        r[k] = v.clone();
    }
    let p = f.repo.join("rules.json");
    fs::write(&p, serde_json::to_vec_pretty(&r).unwrap()).unwrap();
    p
}

fn test_generator() -> Value {
    json!([{"key": "test-model", "patterns": ["testmodel"], "family": "test", "display": "Test Model", "kind": "instruct",
            "provider": "test", "licence_id": "MIT", "notice_files": ["LICENSES/models/model.LICENSE"]}])
}

fn export(f: &Fixture, generators: Value) -> anyhow::Result<(PathBuf, Value)> {
    let rules = rules(f, generators);
    let out = f.repo.join("out");
    let _ = fs::remove_dir_all(&out);
    let conn = db::open_ro(&f.store.join("corpus.sqlite")).unwrap();
    let m = public_candidate::run(&conn, &f.store, &out, &rules, Some(&f.repo))?;
    Ok((out, m))
}

#[test]
fn selection_gates_rights_and_pangram() {
    let f = fixture();
    let (out, m) = export(&f, test_generator()).unwrap();
    let texts: BTreeMap<String, Value> = read_table(&out, "texts").into_iter().map(|r| (r["text_sha256"].as_str().unwrap().to_string(), r)).collect();
    let has = |n: &str| texts.contains_key(&h(&f.texts[n]));
    // Released: used, permitted, attributed texts and their generations.
    for n in ["ok", "override", "uspto_old", "published", "g1", "g2", "gbad", "gseed"] {
        assert!(has(n), "{n} should be released");
    }
    // Each exclusion predicate fires.
    for n in ["flag", "fineweb", "unused", "reserved", "sealed", "uspto_new", "unknown", "noattr", "private_ds", "gflag"] {
        assert!(!has(n), "{n} must not be released");
    }
    let ex: Value = serde_json::from_slice(&fs::read(out.join("exclusions.json")).unwrap()).unwrap();
    let reasons = ex["texts_by_reason_and_kind"].as_object().unwrap();
    for r in ["redistribute_text_false", "licence:fineweb_page_text_unlicensed", "private_or_unprovenanced", "reserved_evaluation_stock",
              "sealed_or_unconsumed_test", "uspto_date_rule", "licence_unclassified", "attribution_missing", "ancestor_redistribute_text_false"] {
        assert!(reasons.contains_key(r), "missing exclusion reason {r}: {reasons:?}");
    }
    assert!(reasons.keys().any(|k| k.starts_with("unused_holdout:")), "{reasons:?}");

    // Rights: inheritance through a two-level chain; the override decision is recorded.
    let rights: BTreeMap<String, Value> = read_table(&out, "rights").into_iter().map(|r| (r["text_sha256"].as_str().unwrap().to_string(), r)).collect();
    let g2 = &rights[&h(&f.texts["g2"])];
    let chain: Vec<&str> = g2["inherited_from"].as_array().unwrap().iter().map(|x| x["text_sha256"].as_str().unwrap()).collect();
    assert_eq!(chain, vec![h(&f.texts["g1"]).as_str(), h(&f.texts["ok"]).as_str()]);
    assert_eq!(g2["licence_id"], json!("CC-BY-4.0"));
    assert_eq!(g2["generated_additions"]["licence_id"], json!("CC-BY-SA-4.0"));
    assert_eq!(g2["share_alike"], json!(true));
    let ov = &rights[&h(&f.texts["override"])];
    assert_eq!(ov["redistribute_text_basis"]["decision"], json!("test decision"));
    assert_eq!(ov["class"], json!("cc_by_sa"));
    assert_eq!(rights[&h(&f.texts["ok"])]["attribution"]["source_url"], json!("https://example.org/pool-ok"));

    // Records carry allowlisted provenance only.
    let recs = read_table(&out, "records");
    let all = serde_json::to_string(&recs).unwrap();
    assert!(!all.contains("/home/someone"), "host path leaked");
    assert!(!all.contains("operator note"), "admission note leaked");
    assert!(recs.iter().all(|r| r.get("rights_basis").is_none() && r.get("file_sha256").is_none() && r.get("extra_json").is_none()));

    // Pangram: P and R texts get no row; a rights-only exclusion is hash-only without text or ids.
    let pg = read_table(&out, "pangram_observations");
    let by: HashMap<String, &Value> = pg.iter().map(|r| (r["text_sha256"].as_str().unwrap().to_string(), r)).collect();
    assert_eq!(pg.len(), 2, "{pg:?}");
    assert_eq!(by[&h(&f.texts["ok"])]["text_included"], json!(true));
    let ho = by[&h(&f.texts["fineweb"])];
    assert_eq!(ho["text_included"], json!(false));
    assert!(ho.get("text").is_none() && ho["item_id"].is_null());
    assert!(ho["label_context"]["source_group_sha256"].is_string());
    let pg_all = serde_json::to_string(&pg).unwrap();
    assert!(!pg_all.contains("blk_") && !pg_all.contains("secret window text"));
    assert!(!by.contains_key(&h(&f.texts["reserved"])) && !by.contains_key(&h(&f.texts["private_ds"])));

    // Prompts: published only with the parent embedded.
    let open = read_table(&out, "open_generations");
    let row = |n: &str| open.iter().find(|r| r["output_text_sha256"] == json!(h(&f.texts[n]))).unwrap();
    assert!(row("g1")["prompt"].as_str().unwrap().contains(&f.texts["ok"]));
    assert!(row("gseed")["prompt"].is_null());
    assert_eq!(row("gseed")["prompt_withheld"], json!("private_seed_voice"));

    // A self-parent generation record leaves its text human and is not published.
    assert_eq!(texts[&h(&f.texts["ok"])]["kind"], json!("human"));
    assert!(recs.iter().all(|r| !(r["text_sha256"] == json!(h(&f.texts["ok"])) && r["role"] == json!("local_record"))));
    assert_eq!(m["gates"]["self_parent_generation_records_ignored"], json!(1));

    // Public names: campaigns, experiments and view kinds are renamed; view ids carry no file path.
    let splits = read_table(&out, "splits");
    assert!(!splits.is_empty());
    for r in &splits {
        let v = r["view_id"].as_str().unwrap();
        assert!(!v.contains('/') && !v.contains(".json"), "{v}");
    }
    assert!(splits.iter().any(|r| r["experiment"] == json!("bundle-public") && r["view_id"].as_str().unwrap().starts_with("bundle-public:view-")));
    let exps = read_table(&out, "experiments");
    assert!(exps.iter().all(|e| e["experiment"] != json!("bundle-x")));
    assert!(exps.iter().flat_map(|e| e["views"].as_array().unwrap().clone()).any(|v| v["kind"] == json!("bundle_rows")));
    assert_eq!(by[&h(&f.texts["ok"])]["campaign"], json!("public-cal"));

    // The private seed string occurs in no file of the release, in any case.
    let mut every = String::new();
    for t in ["texts", "records", "rights", "splits", "experiments", "pangram_observations", "hosted_generations", "open_generations", "voices"] {
        every.push_str(&serde_json::to_string(&read_table(&out, t)).unwrap());
    }
    for f in ["MANIFEST.json", "README.md", "exclusions.json", "generators.json", "LICENSES/INDEX.json", "LICENSES/NOTICE.md"] {
        every.push_str(&fs::read_to_string(out.join(f)).unwrap());
    }
    assert!(!every.to_lowercase().contains("sound like a synthetic test pirate"));
    assert!(row("gbad")["prompt"].is_null());
    assert_eq!(row("gbad")["prompt_withheld"], json!("parent_not_embedded"));
    assert_eq!(row("g2")["model"]["licence_id"], json!("MIT"));

    // Published status only for the anchored datasets/ source.
    assert_eq!(texts[&h(&f.texts["published"])]["published_before"], json!(["snap"]));
    assert_eq!(texts[&h(&f.texts["ok"])]["published_before"], json!([]));

    let mtext = fs::read_to_string(out.join("MANIFEST.json")).unwrap();
    assert!(mtext.contains("\"label\": \"test pool\"") && !mtext.contains("data/testnet/pools"), "reservation paths must stay private");
    // MANIFEST digests match the files; SHA256SUMS covers every file.
    for (name, meta) in m["files"].as_object().unwrap() {
        let p = out.join(name);
        assert_eq!(meta["sha256"].as_str().unwrap(), crate::archive::hash_file_raw(&p).unwrap(), "{name}");
        let dec = zstd::decode_all(fs::File::open(&p).unwrap()).unwrap();
        assert_eq!(meta["decoded_sha256"].as_str().unwrap(), hex::encode(crate::skeleton::sha256(&dec)), "{name}");
        assert_eq!(meta["rows"].as_u64().unwrap(), dec.iter().filter(|b| **b == b'\n').count() as u64);
    }
    let sums = fs::read_to_string(out.join("SHA256SUMS")).unwrap();
    for l in sums.lines() {
        let (d, p) = l.split_once("  ").unwrap();
        assert_eq!(d, crate::archive::hash_file_raw(&out.join(p)).unwrap(), "{p}");
    }
    assert!(sums.contains("LICENSES/sources/CC-BY-4.0.txt") && sums.contains("MANIFEST.json"));
}

#[test]
fn private_root_containing_datasets_is_refused() {
    let f = fixture();
    let (out, _) = export(&f, test_generator()).unwrap();
    let texts = read_table(&out, "texts");
    assert!(texts.iter().all(|r| r["text_sha256"] != json!(h(&f.texts["private_ds"]))));
    let all = serde_json::to_string(&read_table(&out, "records")).unwrap();
    assert!(!all.contains("leak-1"));
    // Listing that source as published does not make it published: its root is not <repo>/datasets/<name>.
    let rules = rules(&f, test_generator());
    let mut r: Value = serde_json::from_slice(&fs::read(&rules).unwrap()).unwrap();
    r["published_sources"]["leak"] = json!("leak");
    fs::write(&rules, serde_json::to_vec(&r).unwrap()).unwrap();
    let conn = db::open_ro(&f.store.join("corpus.sqlite")).unwrap();
    let e = public_candidate::run(&conn, &f.store, &f.repo.join("out2"), &rules, Some(&f.repo)).unwrap_err();
    assert!(e.to_string().contains("not rooted at <repo>/datasets/<name>"), "{e}");
}

#[test]
fn unresolved_generator_and_unpinned_reservation_fail() {
    let f = fixture();
    let e = export(&f, json!([])).unwrap_err();
    assert!(e.to_string().contains("no resolved generator"), "{e}");
    let f = fixture();
    write(&f.repo.join("data/testnet/pools/p2/manifest.json"), "{\"rows\": []}");
    let e = export(&f, test_generator()).unwrap_err();
    assert!(e.to_string().contains("not pinned"), "{e}");
}

#[test]
fn gate_rejects_seeded_identifiers() {
    let f = fixture();
    let rules_path = rules(&f, test_generator());
    let (r, _) = public_candidate::load_rules(&rules_path).unwrap();
    let mut g = testing::Gate::new(&r, &f.repo).unwrap();
    g.scan("t", 0, &json!({"note": "/home/x/data/file.json"}), &[]);
    g.scan("t", 1, &json!({"job": format!("blk_{}", "0123456789abcdef".repeat(2))}), &[]);
    g.scan("t", 2, &json!({"cost_usd": 0.5}), &[]);
    g.scan("t", 3, &json!({"call_log_model": "x"}), &[]);
    g.scan("t", 4, &json!({"endpoint": "ok"}), &[]);
    g.scan("t", 5, &json!({"url": "http://172.16.1.20:8080/x"}), &[]);
    g.scan("t", 6, &json!({"who": "Operator@Example.invalid"}), &[]);
    g.scan("t", 7, &json!({"note": "via example-private-label"}), &[]);
    let rows: Vec<u64> = g.hits.iter().map(|h| h["row"].as_u64().unwrap()).collect();
    for i in 0..8 {
        assert!(rows.contains(&i), "row {i} not flagged: {:?}", g.hits);
    }
    // Corpus text may mention localhost; only operator values are checked there.
    let mut g = testing::Gate::new(&r, &f.repo).unwrap();
    g.scan("t", 0, &json!({"text": "connect to localhost and 10.1.2.3", "doi": "10.1371/journal.pone.0125168"}), &["text"]);
    assert!(g.hits.is_empty(), "{:?}", g.hits);
    assert!(testing::private_ipv4("at 10.0.0.1 now") && !testing::private_ipv4("doi 10.1371/x.0125168"));
    assert_eq!(testing::provider_id("id req_011CabcdefGHIJKLmnop"), Some("provider_request_id"));
    assert_eq!(testing::provider_id("our id req-0123456789abcdef"), None);
}

#[test]
fn archived_lines_rebuild_and_check_their_digest() {
    let f = fixture();
    let conn = db::open_ro(&f.store.join("corpus.sqlite")).unwrap();
    let (file, line, sha): (String, i64, String) = conn
        .query_row("SELECT file_sha256, line_no, line_sha256 FROM record WHERE record_id = 'pool-ok'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    let b = crate::archive::rebuild_line(&conn, &f.store.join("packs"), &file, line, &sha).unwrap();
    assert!(String::from_utf8(b).unwrap().contains("\"redistribute_text\":true"));
    assert!(crate::archive::rebuild_line(&conn, &f.store.join("packs"), &file, line, &"0".repeat(64)).is_err());
}

#[test]
fn journal_epoch_milliseconds_convert_to_utc() {
    assert_eq!(testing::ms_to_iso("1793112442800").as_deref(), Some("2026-10-27T14:47:22.800Z"));
    assert_eq!(testing::ms_to_iso("not a time"), None);
}

#[test]
fn prompt_embedding_kinds() {
    let parent = b"The quick brown fox jumps over the lazy dog, and then it runs far away into the forest beyond.";
    assert_eq!(testing::embedding(b"Rewrite: The quick brown fox jumps over the lazy dog, and then it runs far away into the forest beyond.", Some(parent), None), "full");
    assert_eq!(testing::embedding(b"Rewrite only: jumps over the lazy dog", Some(parent), Some((20, 41))), "span");
    assert_eq!(testing::embedding(b"The quick brown fox jumps over the lazy dog, and then it runs far", Some(parent), None), "prefix");
    assert_eq!(testing::embedding(b"Rewrite only this part: brown fox jumps over the lazy dog, and then it runs far away into the", Some(parent), None), "partial");
    assert_eq!(testing::embedding(b"Write about foxes.", Some(parent), None), "missing");
}

#[test]
fn gate_flags_private_strings_ports_and_decision_wording() {
    let f = fixture();
    let rules_path = rules(&f, test_generator());
    let (r, _) = public_candidate::load_rules(&rules_path).unwrap();
    let mut g = testing::Gate::new(&r, &f.repo).unwrap();
    // Private strings are found in text fields too, in any case.
    g.scan("t", 0, &json!({"text": "then SOUND LIKE A SYNTHETIC TEST PIRATE, please"}), &["text"]);
    g.scan("t", 1, &json!({"runtime": "native server; loopback port 18125"}), &[]);
    g.scan("t", 2, &json!({"runtime": "owned port18127"}), &[]);
    g.scan("t", 3, &json!({"basis": "non-commercial use, user decision 2026-10-27"}), &[]);
    g.scan("t", 4, &json!({"campaign": "laptop-import-20990101"}), &[]);
    let rows: Vec<u64> = g.hits.iter().map(|h| h["row"].as_u64().unwrap()).collect();
    for i in 0..5 {
        assert!(rows.contains(&i), "row {i} not flagged: {:?}", g.hits);
    }
    assert!(g.hits.iter().all(|h| !h.to_string().to_lowercase().contains("pirate")), "hits must not print private strings");
    let mut g = testing::Gate::new(&r, &f.repo).unwrap();
    g.scan("t", 0, &json!({"title": "Airport 1975", "t2": "Report 2019", "t3": "Port Said in 1956", "t4": "The MP's Laptop"}), &[]);
    assert!(g.hits.is_empty(), "{:?}", g.hits);
    // Share-evidence spans are text: a model span about an audio loopback cable is not metadata.
    let mut g = testing::Gate::new(&r, &f.repo).unwrap();
    g.scan("records", 0, &json!({"provenance": {"share_evidence": {"replacements": ["an audio loopback cable on port 1234"]}}}), &["source_text", "replacement", "replacements"]);
    assert!(g.hits.is_empty(), "{:?}", g.hits);
    g.scan("records", 1, &json!({"provenance": {"share_evidence": {"replacements": ["Sound like a synthetic test pirate"]}}}), &["source_text", "replacement", "replacements"]);
    assert_eq!(g.hits.len(), 1, "{:?}", g.hits);
    assert!(testing::local_port("port 8080") && testing::local_port("port18126") && !testing::local_port("airport 19750") && !testing::local_port("port 123456"));
    // A missing private string file fails rather than silently checking nothing.
    let mut r2 = r.clone();
    r2.private_string_files[0].path = "data/private/missing.json".into();
    assert!(testing::Gate::new(&r2, &f.repo).is_err());
}

#[test]
fn runtime_descriptions_keep_versions_only() {
    let s = "native MLX-LM 0.1.2; MLX/MLX-Metal 0.3.4; Python 3.12.1; Transformers 5.0.0; tokenizers 0.2.0; Apple M9 Max 64 GiB; macOS 99.1 (A1B2); \
             GPU testgpu_x; loopback port 18125; decode_concurrency=1; no truncation; trust_remote_code=false; HF offline; HF offline; task-owned HF cache; \
             local_server.py SHA256=0000000000000000000000000000000000000000000000000000000000000000; \
             mlx_lm/server.py SHA256=1111111111111111111111111111111111111111111111111111111111111111; \
             complete verification receipt SHA256=2222222222222222222222222222222222222222222222222222222222222222; \
             concurrent campaigns may run on the same host";
    let v = testing::runtime_struct(s);
    assert_eq!(v["name"], json!("MLX-LM"));
    assert_eq!(v["version"], json!("0.1.2"));
    assert_eq!(v["packages"]["mlx-metal"], json!("0.3.4"));
    assert_eq!(v["packages"]["python"], json!("3.12.1"));
    assert_eq!(v["upstream_file_sha256"]["mlx_lm/server.py"], json!("1".repeat(64)));
    assert_eq!(v["platform"], json!("macOS arm64 (Apple silicon)"));
    assert_eq!(v["settings"], json!(["decode_concurrency=1", "no truncation", "trust_remote_code=false", "HF offline"]));
    let t = v.to_string();
    for bad in ["port", "loopback", "M9", "99.1", "testgpu", "task-owned", "0000", "2222", "receipt", "same host"] {
        assert!(!t.contains(bad), "{bad} in {t}");
    }
    let l = testing::runtime_struct("llama.cpp-b11756-CUDA12.8");
    assert_eq!(l, json!({"name": "llama.cpp", "build": "b11756", "backend": "CUDA 12.8"}));
}

#[test]
fn voices_and_styles_resolve_inventors_and_withhold_private_seeds() {
    let f = fixture();
    let rules_path = rules(&f, test_generator());
    let (r, _) = public_candidate::load_rules(&rules_path).unwrap();
    let private = vec!["sound like a synthetic test pirate".to_string()];
    // Inventor labels resolve through the exact alias table only.
    let inv = json!({"id": "v1", "origin": "invented", "text": "sound like a lighthouse keeper", "axes": {"inventor": "tm"}});
    assert_eq!(testing::voice_inventor(&r, &inv).unwrap(), Some(0));
    assert!(testing::voice_inventor(&r, &json!({"id": "v2", "origin": "invented", "axes": {"inventor": "testmodel-x"}})).is_err());
    assert!(testing::voice_inventor(&r, &json!({"id": "v3", "origin": "human_derived", "axes": {}})).is_err());
    assert_eq!(testing::voice_inventor(&r, &json!({"id": "v4", "origin": "pilot_hand_written", "axes": {}})).unwrap(), None);
    // Seed voices are withheld by origin or by content.
    assert!(testing::voice_withheld(&r, &json!({"origin": "seed_hand_written", "text": "anything"}), &private));
    assert!(testing::voice_withheld(&r, &json!({"origin": "invented", "text": "Sound like a synthetic test pirate who sails"}), &private));
    assert!(!testing::voice_withheld(&r, &inv, &private));
    // Style blocks: allowlisted fields, released families and items only, private text withheld, inventors resolved.
    let groups: std::collections::HashSet<String> = ["family:released".to_string()].into();
    let ids: std::collections::HashSet<String> = ["pos-released".to_string()].into();
    let x = json!({"style_kind": "voice", "voice_text": "sound like a synthetic test pirate", "voice_inventor": "tm",
                   "voice_source_family": "family:unreleased", "brief_source_family": "family:released",
                   "cross_base_item": "pos-unreleased", "ladder_base_item": "pos-released", "brief_text": {"$len": 9, "$sha256": "ab"},
                   "local_note": "/home/someone/x", "modifier": {"type": "audience", "text": "as if at a campfire", "extra": 1}});
    let mut inventors = std::collections::BTreeSet::new();
    let mut counts = testing::Counter::default();
    let st = testing::style_of(&r, &x, &private, &groups, &ids, &mut inventors, &mut counts).unwrap().unwrap();
    assert_eq!(st["text_withheld"], json!("private_seed_voice"));
    assert!(st.get("voice_text").is_none() && st.get("brief_text").is_none() && st.get("local_note").is_none());
    assert!(st.get("voice_source_family").is_none() && st.get("cross_base_item").is_none());
    assert_eq!(st["brief_source_family"], json!("family:released"));
    assert_eq!(st["ladder_base_item"], json!("pos-released"));
    assert_eq!(st["modifier"], json!({"type": "audience", "text": "as if at a campfire"}));
    assert_eq!(st["voice_inventor_generator"], json!("test-model"));
    assert_eq!(st["inventor_notice_files"], json!(["LICENSES/models/model.LICENSE"]));
    assert!(inventors.contains(&0));
    assert!(testing::style_of(&r, &json!({"voice_inventor": "nobody"}), &private, &groups, &ids, &mut inventors, &mut counts).is_err());
}
