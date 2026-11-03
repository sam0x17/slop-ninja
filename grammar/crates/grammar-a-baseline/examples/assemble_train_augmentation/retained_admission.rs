//! Narrow adapter for the retained original-Train admission contract.
//! This does not admit book roots or reinterpret the legacy quota contract.
use super::*;

pub(super) const SCHEMA: &str = "slop_ninja_retained_train_roots_v1";
const PLAN: &str = "slop_ninja_retained_train_admission_plan_v1";

pub(super) fn manifest(m: &Value, admitted: usize) -> Result<()> {
    ensure!(
        m["schema"] == SCHEMA
            && m["complete"] == true
            && m["train_only"] == true
            && m["test_text_opened"] == false
            && array(m, "test_ids")?.is_empty(),
        "retained admission scope/completion"
    );
    ensure!(
        counter(m, "records")? == admitted as u64
            && counter(m, "source_families")? == admitted as u64
            && counter(m, "requested")?
                == (admitted as u64)
                    .checked_add(counter(m, "excluded")?)
                    .context("admission count overflow")?,
        "retained requested/admitted/excluded counts"
    );
    ensure!(
        m.get("shortfall").is_none(),
        "retained admissions must not masquerade as quota shortfalls"
    );
    str_at(m, "batch_id")?;
    Ok(())
}

fn pinned_bytes(path: &Path, hash: &str, inputs: &mut Vec<Value>) -> Result<Vec<u8>> {
    let b = bytes(path)?;
    ensure!(
        sha(&b) == hash,
        "retained admission evidence changed: {}",
        path.display()
    );
    inputs.push(json!({"path":path,"sha256":hash}));
    Ok(b)
}
fn input_binding(pin: &Value, m: &Value) -> Result<()> {
    let path = str_at(pin, "path")?;
    let hash = digest_at(pin, "sha256")?;
    ensure!(
        m["input_files"][path]["sha256"] == hash,
        "retained admission input inventory mismatch: {path}"
    );
    Ok(())
}
fn normalize_sha(t: &str) -> String {
    sha(t
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .as_bytes())
}

pub(super) fn validate(
    dir: &Path,
    m: &Value,
    rows: &[Value],
    selection: &Value,
    audit_bytes: &[u8],
    inputs: &mut Vec<Value>,
) -> Result<()> {
    manifest(m, rows.len())?;
    let plan = pinned_json(
        &dir.join("admission-plan.json"),
        digest_at(m, "plan_sha256")?,
        inputs,
    )?;
    let protocol = pinned_bytes(
        &dir.join("ADMISSION_PROTOCOL.md"),
        digest_at(m, "protocol_sha256")?,
        inputs,
    )?;
    ensure!(
        plan["schema"] == PLAN
            && plan["batch_id"] == m["batch_id"]
            && plan["requested"] == m["requested"],
        "retained admission plan identity/count"
    );
    ensure!(
        std::str::from_utf8(&protocol)?
            .starts_with("Slop Ninja retained Train expansion admission, version 1"),
        "retained protocol version"
    );
    let files = plan["files"]
        .as_object()
        .context("retained admission plan files")?;
    for k in [
        "protocol",
        "candidates",
        "candidate_audit",
        "candidate_validation",
        "reservations",
        "prior_reservations",
        "pg_candidates",
        "prior_roots_manifest",
        "prior_selection",
        "snapshot_manifest",
        "credits",
        "numeric_contract",
        "current_manifest",
        "references_manifest",
        "augmentation_manifest",
        "original_train",
    ] {
        input_binding(
            files
                .get(k)
                .with_context(|| format!("missing admission input {k}"))?,
            m,
        )?;
    }
    ensure!(
        files["protocol"]["sha256"] == m["protocol_sha256"]
            && files["candidates"]["sha256"] == m["requests_sha256"],
        "admission protocol/request pins"
    );
    ensure!(
        Path::new(str_at(&files["original_train"], "path")?)
            .file_name()
            .is_some_and(|s| s == "train.jsonl"),
        "original Train shard required"
    );
    let train_sha = digest_at(&files["original_train"], "sha256")?;
    let snapshot_sha = digest_at(&files["snapshot_manifest"], "sha256")?;
    // These are immutable admission inputs, not live provider records. Binding
    // their recorded hashes preserves the admission evidence without expanding
    // the assembler into another acquisition or source-selection implementation.
    let mut kinds = BTreeSet::new();
    for spec in array(&plan, "populations")? {
        let kind = str_at(spec, "kind")?;
        ensure!(
            ["current", "references", "augmentation"].contains(&kind) && kinds.insert(kind),
            "retained population kind"
        );
        ensure!(
            array(spec, "allowed_splits")?
                .iter()
                .all(|s| s.as_str().is_some_and(|p| PARTS.contains(&p))),
            "Test population in retained admission"
        );
        input_binding(&spec["records"], m)?;
        input_binding(
            files
                .get(str_at(spec, "manifest_key")?)
                .context("population manifest pin")?,
            m,
        )?;
    }
    ensure!(
        kinds == BTreeSet::from(["current", "references", "augmentation"]),
        "missing retained cumulative population"
    );

    let request_bytes = pinned_bytes(
        &dir.join("requests.jsonl"),
        digest_at(m, "requests_sha256")?,
        inputs,
    )?;
    let requests = records(&request_bytes)?;
    let requested = unique_rows_by(&requests, "root_id")?;
    let audit = records(audit_bytes)?;
    let decisions = unique_rows_by(&audit, "original_id")?;
    let excluded_bytes = pinned_bytes(
        &dir.join("exclusions.jsonl"),
        digest_at(m, "exclusions_sha256")?,
        inputs,
    )?;
    let exclusions = records(&excluded_bytes)?;
    let excluded = unique_rows_by(&exclusions, "original_id")?;
    let selected = unique_rows_by(array(selection, "selected")?, "original_id")?;
    ensure!(
        selection["schema"] == "slop_ninja_retained_train_selection_v1"
            && selection["batch_id"] == m["batch_id"]
            && selection["plan_sha256"] == m["plan_sha256"]
            && selection["protocol_sha256"] == m["protocol_sha256"],
        "retained selection bindings"
    );
    ensure!(
        requested.len() as u64 == counter(m, "requested")?
            && decisions.keys().eq(requested.keys())
            && excluded.len() as u64 == counter(m, "excluded")?
            && selected.len() == rows.len(),
        "retained request/decision populations"
    );
    let mut requested_families = BTreeSet::new();
    let mut admitted_ids = BTreeSet::new();
    for (id, c) in &requested {
        ensure!(
            requested_families.insert(str_at(c, "source_group")?),
            "duplicate requested retained family"
        );
        ensure!(
            c["length_lane"] == "existing_150_450_quota_only"
                && c["prior_selection_decision"]["selected"] == false
                && c["prior_selection_decision"]["reasons"] == json!(["collection_quota"]),
            "nonstandard/book candidate in retained admission"
        );
        let d = decisions[id];
        let reasons = array(d, "reasons")?;
        ensure!(
            reasons
                .iter()
                .all(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                && d["selected"].is_boolean()
                && d["selected"] == reasons.is_empty(),
            "inconsistent retained admission decision"
        );
        ensure!(
            d["source_group"] == c["source_group"] && d["collection"] == c["source"]["collection"],
            "request/decision source identity"
        );
        if reasons.is_empty() {
            ensure!(
                selected.get(id) == Some(&d) && !excluded.contains_key(id),
                "admitted decision not selected exactly once"
            );
            admitted_ids.insert(id.clone());
        } else {
            ensure!(
                excluded.get(id) == Some(&d) && !selected.contains_key(id),
                "excluded decision not retained exactly once"
            );
        }
    }
    ensure!(
        admitted_ids == selected.keys().cloned().collect(),
        "selected/admitted ID reconciliation"
    );
    let mut root_ids = BTreeSet::new();
    for r in rows {
        // Retain all legacy original Train / whole-record / rights / role checks.
        validate_root(r, train_sha)?;
        let old = &r["source"]["record"];
        let id = str_at(old, "id")?;
        ensure!(
            root_ids.insert(id.to_owned()) && admitted_ids.contains(id),
            "unadmitted/duplicate original root"
        );
        let c = requested[id];
        let d = decisions[id];
        let text = str_at(r, "text")?;
        let mut source = old["source"].clone();
        source
            .as_object_mut()
            .context("original source")?
            .remove("raw_path");
        ensure!(
            c["source"] == source
                && c["rights"] == old["rights"]
                && c["text_sha256"] == r["text_sha256"]
                && c["source_group"] == r["source_group"],
            "retained request/full original provenance"
        );
        let n = words(text);
        let normalized = normalize_sha(text);
        ensure!(
            d["words"] == n
                && d["text_sha256"] == r["text_sha256"]
                && d["normalized_text_sha256"] == normalized
                && c["prior_selection_decision"]["words"] == n
                && c["prior_selection_decision"]["text_sha256"] == r["text_sha256"]
                && c["prior_selection_decision"]["normalized_text_sha256"] == normalized,
            "retained exact/normalized text audit"
        );
        ensure!(
            d["request_sha256"] == compact_sha(c)?
                && d["original_jsonl_line_sha256"] == r["source"]["original_jsonl_line_sha256"],
            "retained request/line hashes"
        );
        ensure!(
            r["source"]["source_manifest_sha256"] == snapshot_sha,
            "retained source snapshot pin"
        );
        ensure!(
            r["metadata"]["source_batch"] == m["batch_id"]
                && r["metadata"]["admission_plan_sha256"] == m["plan_sha256"]
                && r["metadata"]["admission_protocol_sha256"] == m["protocol_sha256"],
            "retained row admission identity"
        );
        let expected_id = format!(
            "train-retained-human-{}",
            sha(format!(
                "slop_ninja_retained_train_human_v1\0{}\0{id}",
                str_at(m, "batch_id")?
            )
            .as_bytes())
        );
        let expected_rank = sha(format!(
            "slop_ninja_train_augmentation_roots_v1\0{}",
            str_at(r, "source_group")?
        )
        .as_bytes());
        ensure!(
            r["id"] == expected_id
                && d["rank_sha256"] == expected_rank
                && r["metadata"]["selection_rank_sha256"] == expected_rank,
            "retained batch ID/rank"
        );
    }
    ensure!(root_ids == admitted_ids, "admitted root roster incomplete");
    Ok(())
}

pub(super) fn summary(m: &Value) -> Value {
    json!({"schema":m["schema"],"batch_id":m["batch_id"],"requested":m["requested"],"admitted":m["records"],"excluded":m["excluded"],
        "plan_sha256":m["plan_sha256"],"protocol_sha256":m["protocol_sha256"],"requests_sha256":m["requests_sha256"],
        "selection_sha256":m["selection_sha256"],"exclusion_audit_sha256":m["exclusion_audit_sha256"],"exclusions_sha256":m["exclusions_sha256"]})
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        dir: tempfile::TempDir,
        m: Value,
        rows: Vec<Value>,
        selection: Value,
        audit: Vec<u8>,
    }
    fn json_bytes(v: &Value) -> Vec<u8> {
        serde_json::to_vec_pretty(v).unwrap()
    }
    fn lines(rows: &[Value]) -> Vec<u8> {
        let mut b = Vec::new();
        for r in rows {
            serde_json::to_writer(&mut b, r).unwrap();
            b.push(b'\n');
        }
        b
    }
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let t = std::iter::repeat_n("synthetic", 180)
            .collect::<Vec<_>>()
            .join(" ");
        let text_sha = sha(t.as_bytes());
        let batch = "synthetic-retained";
        let protocol =
            b"Slop Ninja retained Train expansion admission, version 1\nSynthetic fixture.";
        let rights = json!({"license":"CC-BY-4.0","attribution":"Synthetic test fixture","evidence_url":"https://example.invalid/license","evidence_sha256":"a".repeat(64),"commercial_training":true,"model_release":true,"external_evaluation":true,"redistribute_text":true});
        let source = json!({"collection":"plos_one","published_at":"2018-01-01","url":"https://example.invalid/source","version":"synthetic","raw_sha256":"b".repeat(64),"extraction":"unchanged","author_ids":[]});
        let c = json!({"root_id":"original-one","source_group":"family-one","length_lane":"existing_150_450_quota_only","title":"Synthetic One","text_sha256":text_sha,"source":source,"rights":rights,
            "prior_selection_decision":{"selected":false,"reasons":["collection_quota"],"words":180,"text_sha256":text_sha,"normalized_text_sha256":normalize_sha(&t)}});
        let mut other = c.clone();
        other["root_id"] = json!("original-two");
        other["source_group"] = json!("family-two");
        let request_bytes = lines(&[c.clone(), other]);
        let mut files = serde_json::Map::new();
        let mut inventory = serde_json::Map::new();
        for k in [
            "protocol",
            "candidates",
            "candidate_audit",
            "candidate_validation",
            "reservations",
            "prior_reservations",
            "pg_candidates",
            "prior_roots_manifest",
            "prior_selection",
            "snapshot_manifest",
            "credits",
            "numeric_contract",
            "current_manifest",
            "references_manifest",
            "augmentation_manifest",
            "original_train",
        ] {
            let path = if k == "original_train" {
                "/synthetic/train.jsonl".to_owned()
            } else {
                format!("/synthetic/{k}")
            };
            let hash = if k == "protocol" {
                sha(protocol)
            } else if k == "candidates" {
                sha(&request_bytes)
            } else {
                "c".repeat(64)
            };
            files.insert(k.into(), json!({"path":path,"sha256":hash}));
            inventory.insert(path, json!({"sha256":hash}));
        }
        let populations=[("current","current_manifest"),("references","references_manifest"),("augmentation","augmentation_manifest")].iter().map(|(kind,key)|{
            let path=format!("/synthetic/{kind}.jsonl");inventory.insert(path.clone(),json!({"sha256":"d".repeat(64)}));
            json!({"kind":kind,"manifest_key":key,"records":{"path":path,"sha256":"d".repeat(64)},"allowed_splits":["train"]})
        }).collect::<Vec<_>>();
        let plan = json!({"schema":PLAN,"batch_id":batch,"requested":2,"files":files,"populations":populations});
        let plan_bytes = json_bytes(&plan);
        let plan_sha = sha(&plan_bytes);
        let rank = sha(b"slop_ninja_train_augmentation_roots_v1\0family-one");
        let id = format!(
            "train-retained-human-{}",
            sha(format!("slop_ninja_retained_train_human_v1\0{batch}\0original-one").as_bytes())
        );
        let mut original_source = source.clone();
        original_source["raw_path"] = json!("/synthetic/raw/source.json");
        let old = json!({"id":"original-one","source_group":"family-one","split":"train","origin":"human_only","evidence":"historical_proxy","generation":null,"parent_id":null,"source":original_source,"rights":rights,"text_sha256":text_sha});
        let row = json!({"schema":"slop_ninja_a_supervised_record_v1","id":id,"source_group":"family-one","frozen_split":"train","purpose":"query","origin":"human_only","evidence":"historical_proxy","documented_human_only":false,"human_identity":null,"ai_identity":null,"human_role_present":true,"ai_role_present":false,"parent_id":null,"text":t,"text_sha256":text_sha,"share_target":0.0,"share_evidence":{"basis":"historical_human_proxy_endpoint","not_documented_contemporary_workflow":true},"grammar_exposure":"unknown","exposed_to_grammar":null,"rights":rights,
            "metadata":{"source_collection":"plos_one","selection_rank_sha256":rank,"source_batch":batch,"admission_plan_sha256":plan_sha,"admission_protocol_sha256":sha(protocol)},
            "source":{"record":old,"original_train_sha256":"c".repeat(64),"source_manifest_sha256":"c".repeat(64),"original_text_sha256":text_sha,"original_jsonl_line_sha256":"e".repeat(64),"byte_span":[0,t.len()]}});
        let accepted = json!({"original_id":"original-one","source_group":"family-one","collection":"plos_one","selected":true,"reasons":[],"words":180,"text_sha256":text_sha,"normalized_text_sha256":normalize_sha(&t),"request_sha256":compact_sha(&c).unwrap(),"original_jsonl_line_sha256":"e".repeat(64),"rank_sha256":rank});
        let excluded = json!({"original_id":"original-two","source_group":"family-two","collection":"plos_one","selected":false,"reasons":["reserved_pg32_underlying_book"]});
        let exclusion_bytes = lines(&[excluded.clone()]);
        let audit = lines(&[accepted.clone(), excluded]);
        let selection = json!({"schema":"slop_ninja_retained_train_selection_v1","batch_id":batch,"plan_sha256":plan_sha,"protocol_sha256":sha(protocol),"selected":[accepted]});
        let m = json!({"schema":SCHEMA,"batch_id":batch,"complete":true,"train_only":true,"test_text_opened":false,"test_ids":[],"non_test_ids":[id],"requested":2,"records":1,"source_families":1,"excluded":1,"input_files":inventory,
            "records_sha256":sha(&lines(&[row.clone()])),"selection_sha256":sha(&json_bytes(&selection)),"exclusion_audit_sha256":sha(&audit),"exclusions_sha256":sha(&exclusion_bytes),"requests_sha256":sha(&request_bytes),"plan_sha256":plan_sha,"protocol_sha256":sha(protocol)});
        for (name, b) in [
            ("admission-plan.json", plan_bytes),
            ("ADMISSION_PROTOCOL.md", protocol.to_vec()),
            ("requests.jsonl", request_bytes),
            ("exclusions.jsonl", exclusion_bytes),
            ("exclusion-audit.jsonl", audit.clone()),
            ("selection.json", json_bytes(&selection)),
            ("humans.jsonl", lines(&[row.clone()])),
            ("manifest.json", json_bytes(&m)),
        ] {
            write_new(&dir.path().join(name), &b).unwrap();
        }
        Fixture {
            dir,
            m,
            rows: vec![row],
            selection,
            audit,
        }
    }
    impl Fixture {
        fn check(&self) -> Result<()> {
            validate(
                self.dir.path(),
                &self.m,
                &self.rows,
                &self.selection,
                &self.audit,
                &mut Vec::new(),
            )
        }
    }
    #[test]
    fn nonzero_exclusions_preserve_real_requested_denominator() {
        let f = fixture();
        f.check().unwrap();
        let plan = json!({"roots_manifest_sha256":sha(&json_bytes(&f.m)),"roots_records_sha256":f.m["records_sha256"]});
        let (rows, m, _) = roots(f.dir.path(), &plan, &mut Vec::new()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(m["requested"], 2);
        assert_eq!(m["excluded"], 1);
        assert_eq!(summary(&m)["requested"], 2);
        assert_eq!(summary(&m)["admitted"], 1);
    }
    #[test]
    fn missing_exclusions_bad_counts_and_changed_inventory_are_rejected() {
        let mut f = fixture();
        f.m["requested"] = json!(1);
        assert!(f.check().is_err());
        let mut f = fixture();
        f.m["excluded"] = json!(0);
        assert!(f.check().is_err());
        let mut f = fixture();
        f.m["shortfall"] = json!(0);
        assert!(f.check().is_err());
        let mut f = fixture();
        f.m["input_files"]["/synthetic/train.jsonl"]["sha256"] = json!("f".repeat(64));
        assert!(f.check().is_err());
        let f = fixture();
        fs::write(f.dir.path().join("exclusions.jsonl"), b"").unwrap();
        assert!(f.check().is_err());
        let mut f = fixture();
        f.audit = lines(&[f.selection["selected"][0].clone()]);
        assert!(f.check().is_err());
    }
    #[test]
    fn changed_source_audit_and_book_roots_remain_rejected() {
        let mut f = fixture();
        f.rows[0]["source"]["record"]["source"]["raw_sha256"] = json!("f".repeat(64));
        assert!(f.check().is_err());
        let mut f = fixture();
        f.rows[0]["source"]["record"]["rights"]["attribution"] = json!("Changed attribution");
        f.rows[0]["rights"]["attribution"] = json!("Changed attribution");
        assert!(f.check().is_err());
        let mut f = fixture();
        f.rows[0]["metadata"]["source_collection"] = json!("project-gutenberg");
        f.rows[0]["source"]["record"]["source"]["collection"] = json!("project-gutenberg");
        assert!(f.check().is_err());
        let mut f = fixture();
        f.rows[0]["metadata"]["admission_plan_sha256"] = json!("f".repeat(64));
        assert!(f.check().is_err());
        let mut f = fixture();
        f.selection["selected"][0]["normalized_text_sha256"] = json!("f".repeat(64));
        assert!(f.check().is_err());
    }
    #[test]
    fn legacy_route_retains_its_zero_shortfall_guard() {
        let mut f = fixture();
        f.m["schema"] = json!("slop_ninja_train_augmentation_roots_v1");
        f.m["requested"] = json!(1);
        f.m["shortfall"] = json!(0);
        fs::write(f.dir.path().join("manifest.json"), json_bytes(&f.m)).unwrap();
        let mut plan = json!({"roots_manifest_sha256":sha(&json_bytes(&f.m)),"roots_records_sha256":f.m["records_sha256"]});
        roots(f.dir.path(), &plan, &mut Vec::new()).unwrap();
        f.m["shortfall"] = json!(1);
        fs::write(f.dir.path().join("manifest.json"), json_bytes(&f.m)).unwrap();
        plan["roots_manifest_sha256"] = json!(sha(&json_bytes(&f.m)));
        assert!(roots(f.dir.path(), &plan, &mut Vec::new()).is_err());
        f.m["schema"] = json!("slop_ninja_book_roots_v1");
        f.m["shortfall"] = json!(0);
        fs::write(f.dir.path().join("manifest.json"), json_bytes(&f.m)).unwrap();
        plan["roots_manifest_sha256"] = json!(sha(&json_bytes(&f.m)));
        assert!(roots(f.dir.path(), &plan, &mut Vec::new()).is_err());
    }
}
