//! Merge admitted Train shards and their already qualified CFG/V6 exports.
//! merge_train_extensions PINNED_PLAN.json NEW_OUT
//! No parsing, inference, fitting, acquisition or Test reads.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn a<'a>(v: &'a Value, k: &str) -> Result<&'a Vec<Value>> {
    v[k].as_array()
        .with_context(|| format!("missing array {k}"))
}
fn n(v: &Value, k: &str) -> Result<u64> {
    v[k].as_u64().with_context(|| format!("missing count {k}"))
}
fn rows(b: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(b)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn encode(v: &[Value]) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    for r in v {
        serde_json::to_writer(&mut b, r)?;
        b.push(b'\n');
    }
    Ok(b)
}
fn norm(t: &str) -> String {
    sha(t
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .as_bytes())
}
fn pin(p: &Path, expected: &Value) -> Result<Vec<u8>> {
    let b = fs::read(p)?;
    ensure!(
        expected.as_str() == Some(sha(&b).as_str()),
        "changed input {}",
        p.display()
    );
    Ok(b)
}
fn put(p: &Path, b: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(p)?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(())
}
fn pretty(v: &Value) -> Result<Vec<u8>> {
    let mut b = serde_json::to_vec_pretty(v)?;
    b.push(b'\n');
    Ok(b)
}
fn map(v: Vec<Value>) -> Result<BTreeMap<String, Value>> {
    let mut m = BTreeMap::new();
    for r in v {
        ensure!(
            m.insert(s(&r, "id")?.into(), r).is_none(),
            "duplicate row ID"
        );
    }
    Ok(m)
}
fn ordered_ids(v: &Value, k: &str) -> Result<BTreeSet<String>> {
    let list = a(v, k)?;
    let ids = list
        .iter()
        .map(|x| Ok(x.as_str().context("ID")?.to_owned()))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(ids.len() == list.len(), "duplicate {k}");
    Ok(ids)
}

fn dataset(
    spec: &Value,
    base: &Path,
    extension: bool,
) -> Result<(PathBuf, Value, BTreeMap<String, Value>)> {
    let dir = base.join(s(spec, "directory")?);
    let mb = pin(&dir.join("manifest.json"), &spec["manifest_sha256"])?;
    let m: Value = serde_json::from_slice(&mb)?;
    ensure!(
        m["schema"] == "slop_ninja_a_supervised_dataset_v1" && m["test_text_opened"] == false,
        "non-Test dataset required"
    );
    let ids = ordered_ids(&m, "non_test_ids")?;
    let test = ordered_ids(&m, "test_ids")?;
    ensure!(ids.is_disjoint(&test), "Test ID in allow-list");
    if extension {
        ensure!(
            m["complete"] == true && m["train_extension_only"] == true && test.is_empty(),
            "incomplete/non-Train extension"
        );
    }
    let rb = pin(&dir.join("records.jsonl"), &m["records_sha256"])?;
    let rr = map(rows(&rb)?)?;
    ensure!(
        rr.keys().cloned().collect::<BTreeSet<_>>() == ids,
        "dataset roster"
    );
    for r in rr.values() {
        let split = s(r, "frozen_split")?;
        ensure!(
            matches!(split, "train" | "development" | "calibration"),
            "Test/unknown row"
        );
        ensure!(
            m["split_assignments"][s(r, "source_group")?] == split,
            "family/split mismatch"
        );
        ensure!(
            r["text_sha256"] == sha(s(r, "text")?.as_bytes()),
            "source text changed"
        );
        if extension {
            ensure!(
                split == "train" && r["purpose"] == "query",
                "Train query extension only"
            );
        }
    }
    if extension {
        ensure!(n(&m, "records")? == rr.len() as u64, "dataset count");
    }
    Ok((dir, m, rr))
}

#[derive(Clone)]
struct Frozen {
    contract: Vec<u8>,
    cfg_manifest: Value,
    embedding_snapshot: Vec<u8>,
    representation: Value,
    representation_sha: String,
}
struct Shard {
    name: String,
    manifest: Value,
    raw: BTreeMap<String, Value>,
    cfg: BTreeMap<String, Value>,
    audit: BTreeMap<String, Value>,
    embeddings: BTreeMap<String, Value>,
    frozen: Frozen,
    receipt: Value,
}
fn same_space(left: &Frozen, right: &Frozen) -> Result<()> {
    ensure!(
        left.contract == right.contract,
        "different frozen CFG feature contract"
    );
    ensure!(
        left.cfg_manifest["training_extension_transform"]["frozen_manifest_sha256"]
            == right.cfg_manifest["training_extension_transform"]["frozen_manifest_sha256"],
        "different fitted CFG parent"
    );
    ensure!(
        left.embedding_snapshot == right.embedding_snapshot
            && left.representation_sha == right.representation_sha,
        "different V6 representation/preprocessing/runtime"
    );
    Ok(())
}
fn load(spec: &Value, base: &Path) -> Result<Shard> {
    let (dir, m, raw) = dataset(&spec["dataset"], base, true)?;
    let cfgdir = base.join(s(&spec["cfg"], "directory")?);
    let fb = pin(
        &cfgdir.join("manifest.json"),
        &spec["cfg"]["manifest_sha256"],
    )?;
    let fm: Value = serde_json::from_slice(&fb)?;
    let contract = pin(&cfgdir.join("contract.json"), &fm["contract_file_sha256"])?;
    let cv: Value = serde_json::from_slice(&contract)?;
    ensure!(
        fm["contract_sha256"] == sha(&serde_json::to_vec(&cv)?),
        "CFG semantic digest"
    );
    let t = &fm["training_extension_transform"];
    ensure!(
        t["final_test_opened"] == false
            && t["fit_performed"] == false
            && t["normalization_fitted"] == false
            && t["profiles_changed"] == false
            && t["excluded"] == 0,
        "frozen complete CFG transform required"
    );
    ensure!(
        t["dataset_manifest_sha256"] == spec["dataset"]["manifest_sha256"]
            && n(t, "requested")? == raw.len() as u64
            && t["available"] == t["requested"],
        "CFG dataset/roster binding"
    );
    let cfg = map(rows(&pin(
        &cfgdir.join("train_extension.jsonl"),
        &t["rows_sha256"],
    )?)?)?;
    let audit_bytes = pin(
        &cfgdir.join("train_extension_population.json"),
        &t["audit_file_sha256"],
    )?;
    let au: Value = serde_json::from_slice(&audit_bytes)?;
    ensure!(
        au["dataset_manifest_sha256"] == spec["dataset"]["manifest_sha256"]
            && au["records_sha256"] == m["records_sha256"]
            && au["contract_sha256"] == fm["contract_sha256"]
            && au["final_test_opened"] == false
            && a(&au, "excluded")?.is_empty(),
        "CFG audit binding"
    );
    let audit = map(a(&au, "rows")?.clone())?;
    let ed = base.join(s(&spec["embeddings"], "directory")?);
    let eb = pin(
        &ed.join("manifest.json"),
        &spec["embeddings"]["manifest_sha256"],
    )?;
    let em: Value = serde_json::from_slice(&eb)?;
    ensure!(
        em["schema"] == "slop_ninja_v6_embeddings_run_v1"
            && em["status"] == "complete"
            && em["unsupported"] == 0
            && n(&em, "requested")? == raw.len() as u64
            && em["supported"] == em["requested"],
        "complete supported V6 export required"
    );
    let snapshot = fs::read(ed.join("contract/manifest.json"))?;
    let canonical = snapshot
        .strip_suffix(b"\n")
        .context("representation newline")?;
    ensure!(
        em["representation_contract_sha256"] == sha(canonical)
            && serde_json::from_slice::<Value>(canonical)? == em["representation_contract"],
        "V6 representation digest"
    );
    let embeddings = map(rows(&pin(&ed.join("rows.jsonl"), &em["rows_sha256"])?)?)?;
    ensure!(
        raw.keys().eq(cfg.keys())
            && raw.keys().eq(audit.keys())
            && raw.keys().eq(embeddings.keys()),
        "incomplete matched row sets"
    );
    for (id, r) in &raw {
        let c = &cfg[id];
        let au = &audit[id];
        let e = &embeddings[id];
        ensure!(
            c["input"]["contract_sha256"] == fm["contract_sha256"]
                && c["source_group"] == r["source_group"]
                && c["frozen_split"] == "train"
                && c["purpose"] == "query",
            "CFG row identity"
        );
        let mut meta = r.clone();
        meta.as_object_mut().context("raw row")?.remove("text");
        ensure!(
            au["available"] == true
                && au["split"] == "train"
                && au["purpose"] == "query"
                && au["metadata"] == meta,
            "CFG metadata/provenance differs from raw row"
        );
        ensure!(
            e["status"] == "ok"
                && e["text_sha256"] == r["text_sha256"]
                && e["representation_contract_sha256"] == em["representation_contract_sha256"]
                && e["artifact_id"] == em["representation_contract"]["artifact_id"],
            "activation source binding"
        );
        ensure!(
            a(e, "embedding")?.len() == 768 && a(&c["input"], "x")?.len() == 1820,
            "feature dimensions"
        );
    }
    let frozen = Frozen {
        contract,
        cfg_manifest: fm,
        embedding_snapshot: snapshot,
        representation: em["representation_contract"].clone(),
        representation_sha: s(&em, "representation_contract_sha256")?.into(),
    };
    let receipt = json!({"name":spec["name"],"dataset_directory":dir,"dataset_manifest_sha256":spec["dataset"]["manifest_sha256"],"dataset_records_sha256":m["records_sha256"],"cfg_manifest_sha256":sha(&fb),"cfg_audit_sha256":sha(&audit_bytes),"embedding_manifest_sha256":sha(&eb),"embedding_rows_sha256":em["rows_sha256"],"embedding_original_elapsed_seconds":em["elapsed_seconds"],"cfg_original_source_report_sha256":au["source_report_sha256"],"extractors_reexecuted":false});
    Ok(Shard {
        name: s(spec, "name")?.into(),
        manifest: m,
        raw,
        cfg,
        audit,
        embeddings,
        frozen,
        receipt,
    })
}

#[derive(Default)]
struct Seen {
    ids: BTreeSet<String>,
    reserved_ids: BTreeSet<String>,
    families: BTreeSet<String>,
    normalized: BTreeSet<String>,
}
fn admit_shard(shard: &Shard, seen: &mut Seen) -> Result<(Vec<String>, Vec<Value>)> {
    let mut families = BTreeSet::new();
    let mut retained = Vec::new();
    let mut excluded = Vec::new();
    for r in shard.raw.values() {
        ensure!(
            !seen.reserved_ids.contains(s(r, "id")?),
            "reserved Test ID reused by Train shard"
        );
        let g = s(r, "source_group")?;
        ensure!(
            !seen.families.contains(g),
            "source family reused between shards/protected populations"
        );
        families.insert(g.to_owned());
        ensure!(seen.ids.insert(s(r, "id")?.into()), "duplicate ID");
    }
    // All source roots win over generated copies inside this shard. Existing
    // shards/populations have precedence; a duplicated human source aborts.
    let mut order = shard.raw.iter().collect::<Vec<_>>();
    order.sort_by_key(|(id, r)| (!r["parent_id"].is_null(), (*id).clone()));
    for (id, r) in order {
        let digest = norm(s(r, "text")?);
        if !seen.normalized.insert(digest.clone()) {
            ensure!(!r["parent_id"].is_null(), "duplicate human source root");
            excluded.push(json!({"id":id,"source_group":r["source_group"],"text_sha256":r["text_sha256"],"normalized_sha256":digest,"reason":"duplicate_across_cumulative_train_shards","shard":shard.name}));
        } else {
            retained.push(id.clone());
        }
    }
    seen.families.extend(families);
    Ok((retained, excluded))
}
fn accounting(m: &Value) -> Result<()> {
    let roots = n(m, "requested_human_records")?;
    let tasks = n(m, "requested_generation_records")?;
    let mut accepted = 0;
    let mut failed = 0;
    let mut requested = 0;
    let mut ids = ordered_ids(m, "non_test_ids")?;
    for r in a(m, "generation_reports")? {
        ensure!(r["complete"] == true, "unfinished generator report");
        let ac = n(r, "accepted")?;
        let fa = n(r, "failed")?;
        let re = n(r, "requested")?;
        ensure!(
            ac + fa == re && a(r, "failures")?.len() as u64 == fa,
            "generation counts"
        );
        for f in a(r, "failures")? {
            ensure!(ids.insert(s(f, "id")?.into()), "failure ID repeated");
        }
        accepted += ac;
        failed += fa;
        requested += re;
    }
    for r in a(m, "excluded")? {
        ensure!(ids.insert(s(r, "id")?.into()), "exclusion ID repeated");
    }
    ensure!(
        requested == tasks
            && n(m, "requested_records")? == roots + tasks
            && n(m, "accepted_before_exclusions")? == roots + accepted
            && n(m, "records")? + a(m, "excluded")?.len() as u64 == roots + accepted
            && roots + tasks == n(m, "records")? + a(m, "excluded")?.len() as u64 + failed,
        "terminal count reconciliation"
    );
    Ok(())
}
fn run(plan_path: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let pb = fs::read(plan_path)?;
    let plan: Value = serde_json::from_slice(&pb)?;
    ensure!(
        plan["schema"] == "slop_ninja_train_shard_merge_plan_v1"
            && plan["order_policy"] == "listed_shards_earliest_first_roots_before_generated"
            && plan["final_test_opened"] == false,
        "unsupported merge plan"
    );
    let base = plan_path.parent().context("plan parent")?;
    let mut seen = Seen::default();
    for p in a(&plan, "protected_populations")? {
        let (_, m, rr) = dataset(p, base, false)?;
        let reservations = ordered_ids(&m, "test_ids")?;
        ensure!(
            seen.ids.is_disjoint(&reservations),
            "materialized protected ID reserved as Test"
        );
        seen.reserved_ids.extend(reservations);
        for (g, split) in m["split_assignments"]
            .as_object()
            .context("protected groups")?
        {
            ensure!(
                matches!(
                    split.as_str(),
                    Some("train" | "development" | "calibration" | "test")
                ),
                "unknown reserved partition"
            );
            seen.families.insert(g.clone());
        }
        for (id, r) in rr {
            ensure!(
                !seen.reserved_ids.contains(&id),
                "reserved Test ID in protected prose"
            );
            ensure!(seen.ids.insert(id), "duplicate protected ID");
            seen.normalized.insert(norm(s(&r, "text")?));
        }
    }
    let specs = a(&plan, "shards")?;
    ensure!(!specs.is_empty(), "empty shard roster");
    let mut names = BTreeSet::new();
    let mut frozen: Option<Frozen> = None;
    let mut raw = Vec::new();
    let mut cfg = Vec::new();
    let mut audit = Vec::new();
    let mut embeddings = Vec::new();
    let mut exclusions = Vec::new();
    let mut reports = Vec::new();
    let mut receipts = Vec::new();
    let mut source_roots = Vec::new();
    let mut assignments = BTreeMap::new();
    let mut roots = 0;
    let mut tasks = 0;
    let mut before = 0;
    for spec in specs {
        ensure!(
            names.insert(s(spec, "name")?.to_owned()),
            "duplicate shard name"
        );
        let sh = load(spec, base)?;
        accounting(&sh.manifest)?;
        if let Some(f) = &frozen {
            same_space(f, &sh.frozen)?;
        } else {
            frozen = Some(sh.frozen.clone());
        }
        let (ids, extra) = admit_shard(&sh, &mut seen)?;
        for id in ids {
            let r = &sh.raw[&id];
            assignments.insert(s(r, "source_group")?.to_owned(), "train");
            raw.push(r.clone());
            cfg.push(sh.cfg[&id].clone());
            audit.push(sh.audit[&id].clone());
            embeddings.push(sh.embeddings[&id].clone());
        }
        roots += n(&sh.manifest, "requested_human_records")?;
        tasks += n(&sh.manifest, "requested_generation_records")?;
        before += n(&sh.manifest, "accepted_before_exclusions")?;
        exclusions.extend(a(&sh.manifest, "excluded")?.clone());
        exclusions.extend(extra);
        reports.extend(a(&sh.manifest, "generation_reports")?.clone());
        source_roots.push(json!({"shard":sh.name,"source_manifest_sha256":sh.manifest["source_manifest_sha256"],"requested_human_records":sh.manifest["requested_human_records"],"assembly_manifest_sha256":spec["dataset"]["manifest_sha256"]}));
        receipts.push(sh.receipt);
    }
    for rr in [&mut raw, &mut cfg, &mut audit, &mut embeddings] {
        rr.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    }
    let raw_bytes = encode(&raw)?;
    let cb = encode(&cfg)?;
    let eb = encode(&embeddings)?;
    let f = frozen.context("no frozen space")?;
    let roots_manifest = json!({"schema":"slop_ninja_cumulative_train_source_roots_v1","plan_sha256":sha(&pb),"shards":source_roots,"source_families":assignments.len(),"requested_human_records":roots,"source_groups":assignments.keys().collect::<Vec<_>>(),"test_text_opened":false});
    let rmb = pretty(&roots_manifest)?;
    let manifest = json!({"schema":"slop_ninja_a_supervised_dataset_v1","cohort":"cumulative_train_shards_v1","complete":true,"train_extension_only":true,"test_text_opened":false,"clean_holdout":false,"labels_are_not_inputs":true,"records":raw.len(),"records_sha256":sha(&raw_bytes),"input_sha256":sha(&raw_bytes),"non_test_ids":raw.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"test_ids":[],"split_assignments":assignments,"source_families":assignments.len(),"source_manifest_sha256":sha(&rmb),"requested_human_records":roots,"requested_generation_records":tasks,"requested_records":roots+tasks,"accepted_before_exclusions":before,"excluded":exclusions,"generation_reports":reports,"sources":receipts,"merge_plan_sha256":sha(&pb),"implementation_sha256":sha(include_bytes!("merge_train_extensions.rs")),"rights_scope":"Inherited per-record source/generator evidence; no new permission or public redistribution inferred"});
    accounting(&manifest)?;
    let mb = pretty(&manifest)?;
    let au = json!({"schema":"slop_ninja_train_extension_union_v1","rows":audit,"requested":raw.len(),"available":raw.len(),"excluded":[],"dataset_manifest_sha256":sha(&mb),"records_sha256":sha(&raw_bytes),"contract_sha256":f.cfg_manifest["contract_sha256"],"frozen_manifest_sha256":f.cfg_manifest["training_extension_transform"]["frozen_manifest_sha256"],"final_test_opened":false,"fit_performed":false,"source_shards":receipts,"extraction_work":"cached qualified exports; no new parsing; original run timings remain in source receipts"});
    let aub = pretty(&au)?;
    let mut fm = f.cfg_manifest.clone();
    fm["training_extension_transform"] = json!({"audit_file":"train_extension_population.json","audit_file_sha256":sha(&aub),"available":raw.len(),"requested":raw.len(),"excluded":0,"dataset_manifest_sha256":sha(&mb),"final_test_opened":false,"fit_performed":false,"frozen_manifest_sha256":f.cfg_manifest["training_extension_transform"]["frozen_manifest_sha256"],"normalization_fitted":false,"profiles_changed":false,"requires_explicit_training_bundle_assembly":true,"rows_file":"train_extension.jsonl","rows_sha256":sha(&cb),"merged_shards":receipts,"merge_plan_sha256":sha(&pb)});
    let em = json!({"schema":"slop_ninja_v6_embeddings_union_v1","status":"complete","unsupported":0,"supported":raw.len(),"processed":raw.len(),"requested":raw.len(),"dimension":768,"rows_sha256":sha(&eb),"representation_contract":f.representation,"representation_contract_sha256":f.representation_sha,"source_shards":receipts,"extraction_work":"cached qualified activations; no forward pass executed; original execution costs are retained per shard","merge_plan_sha256":sha(&pb)});
    fs::create_dir(out)?;
    for dir in ["dataset", "cfg", "embeddings", "embeddings/contract"] {
        fs::create_dir(out.join(dir))?;
    }
    put(&out.join("plan.json"), &pb)?;
    put(&out.join("source-roots.json"), &rmb)?;
    put(&out.join("dataset/records.jsonl"), &raw_bytes)?;
    put(&out.join("dataset/manifest.json"), &mb)?;
    put(&out.join("cfg/contract.json"), &f.contract)?;
    put(&out.join("cfg/train_extension.jsonl"), &cb)?;
    put(&out.join("cfg/train_extension_population.json"), &aub)?;
    put(&out.join("cfg/manifest.json"), &pretty(&fm)?)?;
    // The original parent split files are immutable context, not enlarged Train
    // files; copy the same frozen context from the first qualified fragment.
    let first = base.join(s(&specs[0]["cfg"], "directory")?);
    put(
        &out.join("cfg/population.json"),
        &pin(
            &first.join("population.json"),
            &fm["population_file_sha256"],
        )?,
    )?;
    for b in a(&fm, "splits")? {
        let part = s(b, "split")?;
        ensure!(
            matches!(part, "train" | "development" | "calibration" | "reference"),
            "no Test split context"
        );
        let name = format!("{part}.jsonl");
        put(
            &out.join("cfg").join(&name),
            &pin(&first.join(&name), &b["sha256"])?,
        )?;
    }
    put(&out.join("embeddings/rows.jsonl"), &eb)?;
    put(
        &out.join("embeddings/contract/manifest.json"),
        &f.embedding_snapshot,
    )?;
    put(&out.join("embeddings/manifest.json"), &pretty(&em)?)?;
    put(
        &out.join("manifest.json"),
        &pretty(
            &json!({"schema":"slop_ninja_train_shard_merge_v1","complete":true,"records":raw.len(),"families":assignments.len(),"requested_roots":roots,"requested_generation_tasks":tasks,"dataset_manifest_sha256":sha(&mb),"plan_sha256":sha(&pb),"final_test_opened":false,"normalization_fitted":false,"fit_performed":false,"new_inference_calls":0}),
        )?,
    )?;
    println!(
        "{}",
        json!({"records":raw.len(),"families":assignments.len(),"roots":roots,"generation_tasks":tasks})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 2, "merge_train_extensions PINNED_PLAN NEW_OUT");
    run(Path::new(&a[0]), Path::new(&a[1]))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frozen() -> Frozen {
        Frozen {
            contract: b"contract".to_vec(),
            cfg_manifest: json!({"training_extension_transform":{"frozen_manifest_sha256":"frozen"}}),
            embedding_snapshot: b"snapshot".to_vec(),
            representation: json!({}),
            representation_sha: "rep".into(),
        }
    }
    fn shard(v: Vec<Value>) -> Shard {
        Shard {
            name: "synthetic".into(),
            manifest: json!({}),
            raw: map(v).unwrap(),
            cfg: BTreeMap::new(),
            audit: BTreeMap::new(),
            embeddings: BTreeMap::new(),
            frozen: frozen(),
            receipt: json!({}),
        }
    }
    fn row(id: &str, family: &str, text: &str, parent: Value) -> Value {
        json!({"id":id,"source_group":family,"text":text,"text_sha256":sha(text.as_bytes()),"parent_id":parent})
    }
    #[test]
    fn contracts_and_runtime_must_match() {
        let f = frozen();
        let mut g = f.clone();
        same_space(&f, &g).unwrap();
        g.contract.push(1);
        assert!(same_space(&f, &g).is_err());
        g = f.clone();
        g.embedding_snapshot.push(1);
        assert!(same_space(&f, &g).is_err());
        g = f.clone();
        g.cfg_manifest["training_extension_transform"]["frozen_manifest_sha256"] =
            json!("different");
        assert!(same_space(&f, &g).is_err());
    }
    #[test]
    fn duplicates_drop_only_new_generated_copies() {
        let mut seen = Seen::default();
        seen.normalized.insert(norm("retained prior"));
        let sh = shard(vec![
            row("h", "new", "real human", Value::Null),
            row("g", "new", "retained\nprior", json!("h")),
        ]);
        let (ids, excluded) = admit_shard(&sh, &mut seen).unwrap();
        assert_eq!(ids, vec!["h"]);
        assert_eq!(excluded.len(), 1);
        assert!(admit_shard(&sh, &mut seen).is_err());
    }
    #[test]
    fn human_duplicate_or_family_reuse_is_fatal() {
        let sh = shard(vec![row("h", "f", "original", Value::Null)]);
        let mut seen = Seen::default();
        seen.normalized.insert(norm("original"));
        assert!(admit_shard(&sh, &mut seen).is_err());
        let mut seen = Seen::default();
        seen.families.insert("f".into());
        assert!(admit_shard(&sh, &mut seen).is_err());
    }
    #[test]
    fn reserved_test_id_cannot_be_relabelled_train_with_another_family() {
        let mut seen = Seen::default();
        seen.reserved_ids.insert("reserved-test".into());
        let sh = shard(vec![row(
            "reserved-test",
            "different-family",
            "distinct words",
            Value::Null,
        )]);
        assert!(admit_shard(&sh, &mut seen).is_err());
    }
    #[test]
    fn terminal_accounting_preserves_failed_and_excluded_tasks() {
        let m = json!({"requested_human_records":1,"requested_generation_records":3,"requested_records":4,"accepted_before_exclusions":3,"records":2,"non_test_ids":["h","g"],"excluded":[{"id":"duplicate"}],"generation_reports":[{"complete":true,"requested":3,"accepted":2,"failed":1,"failures":[{"id":"failed"}]}]});
        accounting(&m).unwrap();
        let mut bad = m.clone();
        bad["generation_reports"][0]["failures"][0]["id"] = json!("g");
        assert!(accounting(&bad).is_err());
        let mut bad = m;
        bad["records"] = json!(3);
        assert!(accounting(&bad).is_err());
    }
}
