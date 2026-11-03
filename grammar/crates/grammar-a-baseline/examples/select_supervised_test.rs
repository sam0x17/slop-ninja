//! Frozen final-Test selection; compiling and synthetic tests never open the corpus.
//! Invoke only AFTER model selection and calibration are frozen:
//! select_supervised_test WORKTREE NEW_OUTPUT_DIR --open-final-test \
//!   --frozen-model-sha256 HEX --frozen-calibration-sha256 HEX
//! Parent owns Cargo scheduling. This example never reads reserved/private cohorts.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::PathBuf,
};

const QWEN: &str =
    "mlx-community/Qwen3-30B-A3B-Instruct-2507-4bit@e9675aa3ca5f900ccef55267914466d55ab325fa";
const MISTRAL: &str = "mlx-community/Mistral-Small-3.2-24B-Instruct-2506-4bit@2a1d5eabfc504747bdc24178394821a1efc0edde";
const OLMO: &str = "mlx-community/Olmo-3-7B-Instruct-4bit@d732c91ae02e90cd5d86e810fbeb9741794b4dd9";
const INPUT: &str = "datasets/detector-v6/test.jsonl";
// Published manifest binding, fixed without reading the Test shard during coding.
const INPUT_SHA256: &str = "9d49faf9f9415352d49f286c523f24af0677128ac149478d7e422190b5c896b4";
const SCHEMA: &str = "slop_ninja_a_supervised_test_selection_v1";
// Exactly the existing selector's hash rule, with bucket test/<initial revision>.
const HASH_DOMAIN: &str = "slop_ninja_a_supervised_source_selection_v2";
const PER_GENERATOR: usize = 3;

#[derive(Parser)]
struct Args {
    worktree: PathBuf,
    output: PathBuf,
    #[arg(long)]
    open_final_test: bool,
    /// Digest of the already selected model artifact/manifest, supplied by parent.
    #[arg(long)]
    frozen_model_sha256: String,
    /// Digest of the already frozen calibration artifact/manifest, supplied by parent.
    #[arg(long)]
    frozen_calibration_sha256: String,
}

struct Record {
    value: Value,
    raw: Vec<u8>,
}
struct Family {
    collection: String,
    composer: String,
    ids: Vec<String>,
}
struct Pool {
    records: BTreeMap<String, Record>,
    families: BTreeMap<String, Family>,
}

fn sha(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}
fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("Missing nonempty {key}"))
}
fn digest_ok(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn supervised_id(id: &str) -> String {
    format!(
        "historical-{}",
        &sha(format!("slop_ninja_a_historical_record_v1:{id}"))[..24]
    )
}
fn parent(v: &Value) -> Result<Option<&str>> {
    match v.get("parent_id") {
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s)),
        _ => anyhow::bail!("parent_id must be explicit null or nonempty string"),
    }
}
fn operation(v: &Value) -> Result<&str> {
    if v["generation"].is_null() {
        Ok("human_root")
    } else {
        field(&v["generation"], "operation")
    }
}
fn revision(v: &Value) -> Result<&str> {
    field(&v["generation"], "model_revision")
}

fn contributors(id: &str, records: &BTreeMap<String, Record>) -> Result<BTreeSet<String>> {
    let group = field(
        &records.get(id).context("Missing starting record")?.value,
        "source_group",
    )?;
    let mut current = id;
    let mut seen = BTreeSet::new();
    let mut result = BTreeSet::new();
    loop {
        ensure!(seen.insert(current.to_owned()), "Cyclic ancestry");
        let v = &records
            .get(current)
            .context("Missing ancestry parent")?
            .value;
        ensure!(
            v["split"] == "test" && v["source_group"] == group,
            "Ancestry crosses split or family"
        );
        if !v["generation"].is_null() {
            let model = revision(v)?;
            ensure!(
                [QWEN, MISTRAL, OLMO].contains(&model),
                "Unreviewed generator revision"
            );
            result.insert(model.to_owned());
        }
        match parent(v)? {
            Some(p) => current = p,
            None => break,
        }
    }
    Ok(result)
}

fn parse_pool(bytes: &[u8]) -> Result<Pool> {
    let mut records = BTreeMap::<String, Record>::new();
    let mut groups = BTreeMap::<String, Vec<String>>::new();
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        let v: Value = serde_json::from_slice(line)?;
        ensure!(
            v["schema"] == "slop-ninja-origin-record-v1" && v["split"] == "test",
            "Expected original Test records only"
        );
        let id = field(&v, "id")?.to_owned();
        let group = field(&v, "source_group")?.to_owned();
        field(&v, "evidence")?;
        field(&v["source"], "collection")?;
        ensure!(
            v["text_sha256"] == sha(field(&v, "text")?),
            "Text checksum mismatch"
        );
        ensure!(
            [
                "commercial_training",
                "model_release",
                "external_evaluation",
                "redistribute_text"
            ]
            .iter()
            .all(|k| v["rights"][*k] == true),
            "Rights not admitted"
        );
        let op = operation(&v)?;
        match op {
            "human_root" => ensure!(
                v["origin"] == "human_only" && parent(&v)?.is_none(),
                "Invalid human root"
            ),
            "draft" | "revise" => ensure!(
                v["origin"] == "model_only" && parent(&v)?.is_some(),
                "Invalid model-only workflow"
            ),
            "edit" => ensure!(
                v["origin"] == "mixed" && parent(&v)?.is_some(),
                "Invalid human edit"
            ),
            _ => anyhow::bail!("Unsupported generation operation"),
        }
        groups.entry(group).or_default().push(id.clone());
        ensure!(
            records
                .insert(
                    id,
                    Record {
                        value: v,
                        raw: line.to_vec()
                    }
                )
                .is_none(),
            "Duplicate original record ID"
        );
    }
    let mut families = BTreeMap::new();
    for (group, mut ids) in groups {
        ids.sort();
        ensure!(
            ids.len() == 5,
            "Expected complete five-record v6 family; do not replace failed families"
        );
        let collection = field(&records[&ids[0]].value["source"], "collection")?.to_owned();
        let mut by_op = BTreeMap::<String, Vec<String>>::new();
        for id in &ids {
            let v = &records[id].value;
            ensure!(
                v["source"]["collection"] == collection,
                "Family spans source collections"
            );
            contributors(id, &records)?;
            by_op
                .entry(operation(v)?.to_owned())
                .or_default()
                .push(id.clone());
        }
        for (op, n) in [("human_root", 1), ("draft", 1), ("edit", 1), ("revise", 2)] {
            ensure!(
                by_op.get(op).map(Vec::len) == Some(n),
                "Family must contain root, one draft/edit pair, two revisions"
            );
        }
        let root = &by_op["human_root"][0];
        let draft = &by_op["draft"][0];
        let edit = &by_op["edit"][0];
        ensure!(
            parent(&records[draft].value)? == Some(root.as_str())
                && parent(&records[edit].value)? == Some(root.as_str()),
            "Draft/edit must share human root"
        );
        let composer = revision(&records[draft].value)?.to_owned();
        ensure!(
            revision(&records[edit].value)? == composer,
            "Draft/edit must have one initial generator"
        );
        let revisions = &by_op["revise"];
        let first = revisions
            .iter()
            .find(|id| parent(&records[*id].value).ok().flatten() == Some(draft.as_str()))
            .context("Missing first revision of draft")?;
        let second = revisions
            .iter()
            .find(|id| *id != first)
            .context("Missing second revision")?;
        ensure!(
            parent(&records[second].value)? == Some(first.as_str()),
            "Revision ancestry must form a complete chain"
        );
        families.insert(
            group,
            Family {
                collection,
                composer,
                ids,
            },
        );
    }
    Ok(Pool { records, families })
}

fn select(pool: &Pool) -> Result<Vec<String>> {
    let mut chosen = Vec::new();
    for generator in [QWEN, MISTRAL, OLMO] {
        let bucket = format!("test/{generator}");
        let mut bins = BTreeMap::<String, Vec<(String, String)>>::new();
        for (group, f) in &pool.families {
            if f.composer == generator {
                let rank = sha(serde_json::to_vec(&(HASH_DOMAIN, &bucket, group))?);
                bins.entry(f.collection.clone())
                    .or_default()
                    .push((rank, group.clone()));
            }
        }
        let mut bins: BTreeMap<_, VecDeque<_>> = bins
            .into_iter()
            .map(|(collection, mut rows)| {
                rows.sort();
                (collection, rows.into_iter().map(|(_, g)| g).collect())
            })
            .collect();
        let mut composer_chosen = Vec::new();
        while composer_chosen.len() < PER_GENERATOR {
            let before = composer_chosen.len();
            for queue in bins.values_mut() {
                if let Some(group) = queue.pop_front() {
                    composer_chosen.push(group);
                }
                if composer_chosen.len() == PER_GENERATOR {
                    break;
                }
            }
            ensure!(
                composer_chosen.len() > before,
                "Fewer than three eligible families for {generator}; no substitute generator"
            );
        }
        chosen.extend(composer_chosen);
    }
    ensure!(
        chosen.len() == 9 && chosen.iter().collect::<BTreeSet<_>>().len() == 9,
        "Expected nine distinct source families"
    );
    Ok(chosen)
}

fn adapt(id: &str, pool: &Pool, input_sha256: &str) -> Result<Value> {
    let record = &pool.records[id];
    let v = &record.value;
    let models = contributors(id, &pool.records)?;
    let ai_identity = if models.len() == 1 {
        models.iter().next().cloned()
    } else {
        None
    };
    let origin = field(v, "origin")?;
    let (share, evidence) = match origin {
        "human_only" => (
            Some(0.0),
            json!({"basis":"historical_human_proxy_endpoint","not_documented_contemporary_workflow":true}),
        ),
        "model_only" => (
            Some(1.0),
            json!({"basis":"recorded_model_only_workflow_endpoint","source_conditioned":true,"not_measured_word_spans":true}),
        ),
        "mixed" => (
            None,
            json!({"basis":"model_edit_of_historical_proxy_without_realized_span_provenance","fractional_share_supervision_masked":true}),
        ),
        _ => anyhow::bail!("Unsupported origin"),
    };
    let final_generator = if v["generation"].is_null() {
        "human_proxy"
    } else {
        revision(v)?
    };
    Ok(json!({
        "schema":"slop_ninja_a_supervised_record_v1","id":supervised_id(id),
        "text":v["text"],"text_sha256":v["text_sha256"],"source_group":v["source_group"],
        "frozen_split":"test","origin":origin,"evidence":v["evidence"],"purpose":"query",
        "exposed_to_grammar":true,"grammar_exposure":"conservative_unknown_prior_exposure;historical_public_detector_corpus",
        "share_target":share,"share_evidence":evidence,
        "human_identity":null,"human_role_present":origin!="model_only",
        "ai_identity":ai_identity,"ai_role_present":origin!="human_only",
        "rights":v["rights"],"generation":v["generation"],
        "parent_source_id":id,"parent_id":parent(v)?.map(supervised_id),
        "metadata":{
            "all_model_contributor_revisions":models,"final_generator_revision":final_generator,
            "original_record_id":id,"original_parent_id":v["parent_id"],
            "operation":operation(v)?,"source_collection":v["source"]["collection"],
            "human_identity_supervision_mask":false,"ai_identity_supervision_mask":ai_identity.is_some(),
            "multi_model_identity_masked":models.len()>1,"contains_train_withheld_generator":models.contains(OLMO),
            "eligible_for_parameter_fit":false,"eligible_for_calibration_temperature_and_rejection_threshold":false,
            "included_in_primary_evaluation_population":true,"final_test_only":true,
            "open_set_claim":"OLMo was excluded from Train/named-profile enrollment; earlier OLMo Calibration observations may fit temperature/rejection thresholds. Test interpretation is conditional on that frozen calibration.",
            "lineage_policy":"Only one contributor in the entire ancestry yields an identity target; multi-model identity masked with every contributor retained."
        },
        "source":{
            "record":v,"path":INPUT,"record_id":id,"record_sha256":sha(&record.raw),
            "record_checksum_basis":"exact original JSONL line bytes including terminating newline when present",
            "file_sha256":input_sha256,"rights":v["rights"],"original_parent_id":v["parent_id"]
        }
    }))
}

fn run(args: Args) -> Result<()> {
    // These guards run BEFORE any corpus filesystem operation.
    ensure!(
        args.open_final_test,
        "Explicit --open-final-test required after model selection and calibration are frozen"
    );
    ensure!(
        digest_ok(&args.frozen_model_sha256) && digest_ok(&args.frozen_calibration_sha256),
        "Frozen model/calibration SHA256 bindings required"
    );
    ensure!(!args.output.exists(), "Use a new output directory");
    let input = args.worktree.join(INPUT);
    ensure!(
        !fs::symlink_metadata(&input)?.file_type().is_symlink(),
        "Test shard symlink refused"
    );
    let bytes = fs::read(&input)?;
    ensure!(
        sha(&bytes) == INPUT_SHA256,
        "Test shard differs from the predeclared public manifest binding"
    );
    let pool = parse_pool(&bytes)?;
    let groups = select(&pool)?;
    let mut selected_ids = BTreeSet::new();
    let mut families = Vec::new();
    for group in &groups {
        let f = &pool.families[group];
        selected_ids.extend(f.ids.iter().cloned());
        families.push(json!({"source_group":group,"frozen_split":"test","collection":f.collection,"initial_generator_revision":f.composer,"original_record_ids":f.ids,"supervised_record_ids":f.ids.iter().map(|id|supervised_id(id)).collect::<Vec<_>>()}));
    }
    ensure!(
        selected_ids.len() == 45,
        "Expected all five ancestry records in each of nine families"
    );
    let mut output = Vec::new();
    let mut safe_ids = BTreeSet::new();
    let mut hashes = BTreeSet::new();
    for id in &selected_ids {
        let v = &pool.records[id].value;
        if let Some(parent) = parent(v)? {
            ensure!(selected_ids.contains(parent), "Selected parent missing");
        }
        ensure!(safe_ids.insert(supervised_id(id)), "Adapted ID collision");
        ensure!(
            hashes.insert(field(v, "text_sha256")?),
            "Selected exact-text duplicate requires review; do not silently discard lineage"
        );
        serde_json::to_writer(&mut output, &adapt(id, &pool, INPUT_SHA256)?)?;
        output.push(b'\n');
    }
    let receipt = json!({
        "schema":SCHEMA,"algorithm_source_sha256":sha(include_bytes!("select_supervised_test.rs")),
        "algorithm":{"rank":"SHA256(JSON tuple [hash_domain, bucket, source_group]); ascending (digest, source_group)","hash_domain":HASH_DOMAIN,"bucket":"test/<full initial generator revision>","collection_order":"lexical BTreeMap round robin; take one available family per collection per pass","families_per_initial_generator":PER_GENERATOR,"generator_order":[QWEN,MISTRAL,OLMO],"family_contract":"One historical root, one draft/edit pair from a single initial generator, two successive model-only revisions; retain all five records","selection_uses_text_or_metrics":false},
        "input":{"path":INPUT,"sha256":INPUT_SHA256,"records":pool.records.len(),"source_families":pool.families.len()},
        "frozen_model_sha256":args.frozen_model_sha256,"frozen_calibration_sha256":args.frozen_calibration_sha256,
        "freeze_binding_policy":"Parent-supplied digests assert already frozen artifacts; selector does not open or independently verify those artifacts or read model metrics.",
        "selected_families":families,"records":45,"source_families":9,
        "output":{"path":"records.jsonl","schema":"slop_ninja_a_supervised_record_v1","sha256":sha(&output)},
        "test_text_opened":true,"frozen_split":"test","model_calls":0,"detector_calls":0,
        "calibration_or_parameter_fit_permitted":false,"clean_holdout":false,
        "limits":["Historic public corpus and conservative grammar exposure; no pristine-data claim","OLMo transfer claim conditional on previously frozen OLMo-aware calibration","Historical human proxies do not establish individual human attribution","Mixed share unmeasured; multi-model identity target masked with full lineage retained","Three source families per initial generator is a bounded final check","Ancestry stages share families; 45 records are not 45 independent works","Reserved earlier exposed cohort and private grammar holdout are neither read nor modified"]
    });
    // Output only after every input, family, ancestry and checksum check passes.
    fs::create_dir(&args.output)?;
    fs::write(args.output.join("records.jsonl"), &output)?;
    let mut receipt_bytes = serde_json::to_vec_pretty(&receipt)?;
    receipt_bytes.push(b'\n');
    fs::write(args.output.join("selection.json"), receipt_bytes)?;
    println!(
        "Selected 45 final-Test records / 9 original families: {}",
        args.output.join("records.jsonl").display()
    );
    Ok(())
}
fn main() -> Result<()> {
    run(Args::parse())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Entirely synthetic in-memory inputs. No corpus path or data file is read.
    fn family(group: &str, collection: &str, composer: &str) -> Vec<Value> {
        let root = format!("{group}/human");
        let draft = format!("{group}/draft");
        let first = format!("{group}/revision1");
        [("human", "human_only", None, None),
         ("draft", "model_only", Some(root.as_str()), Some((composer,"draft"))),
         ("edit", "mixed", Some(root.as_str()), Some((composer,"edit"))),
         ("revision1", "model_only", Some(draft.as_str()), Some((QWEN,"revise"))),
         ("revision2", "model_only", Some(first.as_str()), Some((MISTRAL,"revise")))]
        .into_iter().map(|(suffix,origin,parent,generation)| {
            let text = format!("Synthetic fixture {group}, role {suffix}. This sentence is invented for a unit test.");
            json!({"schema":"slop-ninja-origin-record-v1","id":format!("{group}/{suffix}"),"source_group":group,"split":"test","origin":origin,"evidence":"synthetic_fixture","text_sha256":sha(&text),"text":text,"source":{"collection":collection},"rights":{"commercial_training":true,"model_release":true,"external_evaluation":true,"redistribute_text":true,"license":"synthetic"},"parent_id":parent,"generation":generation.map(|(model,operation)|json!({"model_revision":model,"operation":operation}))})
        }).collect()
    }
    fn encode(rows: &[Value]) -> Vec<u8> {
        let mut b = Vec::new();
        for v in rows {
            serde_json::to_writer(&mut b, v).unwrap();
            b.push(b'\n');
        }
        b
    }
    fn populated() -> Vec<Value> {
        let mut rows = Vec::new();
        for (m, generator) in [QWEN, MISTRAL, OLMO].into_iter().enumerate() {
            for collection in ["alpha", "beta", "gamma"] {
                for n in 0..4 {
                    rows.extend(family(
                        &format!("synthetic-{m}-{collection}-{n}"),
                        collection,
                        generator,
                    ));
                }
            }
        }
        rows
    }

    #[test]
    fn deterministic_balanced_selection_ignores_input_order_and_metrics() {
        let mut rows = populated();
        let first_pool = parse_pool(&encode(&rows)).unwrap();
        let expected = select(&first_pool).unwrap();
        rows.reverse();
        for r in &mut rows {
            r["irrelevant_detector_score"] = json!(0.999);
        }
        let pool = parse_pool(&encode(&rows)).unwrap();
        assert_eq!(expected, select(&pool).unwrap());
        assert_eq!(expected.len(), 9);
        for generator in [QWEN, MISTRAL, OLMO] {
            let collections: BTreeSet<_> = expected
                .iter()
                .filter(|g| pool.families[*g].composer == generator)
                .map(|g| pool.families[g].collection.as_str())
                .collect();
            assert_eq!(collections, BTreeSet::from(["alpha", "beta", "gamma"]));
        }
        let ids: BTreeSet<_> = expected
            .iter()
            .flat_map(|g| pool.families[g].ids.iter().cloned())
            .collect();
        assert_eq!(ids.len(), 45);
        for id in &ids {
            if let Some(p) = parent(&pool.records[id].value).unwrap() {
                assert!(ids.contains(p));
            }
        }
    }

    #[test]
    fn full_lineage_masks_identity_and_preserves_endpoint_supervision() {
        let pool = parse_pool(&encode(&family("synthetic-olmo", "alpha", OLMO))).unwrap();
        let draft = adapt("synthetic-olmo/draft", &pool, "synthetic-binding").unwrap();
        assert_eq!(draft["ai_identity"], OLMO);
        assert_eq!(draft["share_target"], 1.0);
        let final_row = adapt("synthetic-olmo/revision2", &pool, "synthetic-binding").unwrap();
        assert!(final_row["ai_identity"].is_null());
        assert_eq!(final_row["ai_role_present"], true);
        assert_eq!(
            final_row["metadata"]["all_model_contributor_revisions"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(final_row["metadata"]["ai_identity_supervision_mask"], false);
        assert_eq!(
            final_row["parent_id"],
            supervised_id("synthetic-olmo/revision1")
        );
        assert_eq!(final_row["parent_source_id"], "synthetic-olmo/revision2");
        for suffix in ["human", "draft", "edit", "revision1", "revision2"] {
            let id = format!("synthetic-olmo/{suffix}");
            let v = adapt(&id, &pool, "synthetic-binding").unwrap();
            assert_eq!(v["schema"], "slop_ninja_a_supervised_record_v1");
            assert_eq!(v["frozen_split"], "test");
            assert_eq!(v["purpose"], "query");
            assert_eq!(v["exposed_to_grammar"], true);
            assert!(v["human_identity"].is_null());
            assert_eq!(v["metadata"]["eligible_for_parameter_fit"], false);
            assert_eq!(
                v["metadata"]["eligible_for_calibration_temperature_and_rejection_threshold"],
                false
            );
            assert_eq!(v["source"]["record"], pool.records[&id].value);
            assert_eq!(v["source"]["record_sha256"], sha(&pool.records[&id].raw));
        }
        let human = adapt("synthetic-olmo/human", &pool, "synthetic-binding").unwrap();
        assert_eq!(human["share_target"], 0.0);
        assert_eq!(human["ai_role_present"], false);
        let mixed = adapt("synthetic-olmo/edit", &pool, "synthetic-binding").unwrap();
        assert!(mixed["share_target"].is_null());
        assert_eq!(mixed["human_role_present"], true);
        assert_eq!(mixed["ai_identity"], OLMO);
    }

    #[test]
    fn repeating_same_generator_does_not_create_a_second_contributor() {
        let pool = parse_pool(&encode(&family("synthetic-qwen", "alpha", QWEN))).unwrap();
        let first = adapt("synthetic-qwen/revision1", &pool, "synthetic-binding").unwrap();
        assert_eq!(first["ai_identity"], QWEN);
        assert_eq!(
            first["metadata"]["all_model_contributor_revisions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(
            adapt("synthetic-qwen/revision2", &pool, "synthetic-binding").unwrap()["ai_identity"]
                .is_null()
        );
    }

    #[test]
    fn rejects_split_checksum_rights_and_duplicate_corruption() {
        let base = family("synthetic-errors", "alpha", QWEN);
        for (key, value) in [
            ("split", json!("development")),
            ("text_sha256", json!("bad")),
        ] {
            let mut rows = base.clone();
            rows[0][key] = value;
            assert!(parse_pool(&encode(&rows)).is_err());
        }
        let mut rows = base.clone();
        rows[0]["rights"]["external_evaluation"] = json!(false);
        assert!(parse_pool(&encode(&rows)).is_err());
        let mut rows = base.clone();
        rows.push(base[0].clone());
        assert!(parse_pool(&encode(&rows)).is_err());
    }

    #[test]
    fn rejects_missing_cyclic_cross_family_or_ambiguous_ancestry() {
        let base = family("synthetic-ancestry", "alpha", QWEN);
        let mut rows = base.clone();
        rows[4]["parent_id"] = json!("absent");
        assert!(parse_pool(&encode(&rows)).is_err());
        let mut rows = base.clone();
        rows[3]["parent_id"] = json!("synthetic-ancestry/revision2");
        assert!(parse_pool(&encode(&rows)).is_err());
        let mut rows = base.clone();
        rows.extend(family("synthetic-other", "alpha", MISTRAL));
        rows[1]["parent_id"] = json!("synthetic-other/human");
        assert!(parse_pool(&encode(&rows)).is_err());
        let mut rows = base.clone();
        rows[2]["generation"]["model_revision"] = json!(OLMO);
        assert!(parse_pool(&encode(&rows)).is_err());
        let mut rows = base;
        rows[4]["parent_id"] = json!("synthetic-ancestry/draft");
        assert!(parse_pool(&encode(&rows)).is_err());
    }

    #[test]
    fn insufficient_generator_population_is_not_backfilled() {
        let pool = parse_pool(&encode(&family("synthetic-only-one", "alpha", QWEN))).unwrap();
        assert!(
            select(&pool)
                .unwrap_err()
                .to_string()
                .contains("Fewer than three")
        );
    }

    #[test]
    fn explicit_open_and_freeze_guards_precede_input_access() {
        let mut args = Args {
            worktree: PathBuf::from("/nonexistent-synthetic-test-input"),
            output: PathBuf::from("/nonexistent-synthetic-test-output"),
            open_final_test: false,
            frozen_model_sha256: "a".repeat(64),
            frozen_calibration_sha256: "b".repeat(64),
        };
        assert!(
            run(args)
                .unwrap_err()
                .to_string()
                .contains("Explicit --open-final-test")
        );
        args = Args {
            worktree: PathBuf::from("/nonexistent-synthetic-test-input"),
            output: PathBuf::from("/nonexistent-synthetic-test-output"),
            open_final_test: true,
            frozen_model_sha256: "invalid".into(),
            frozen_calibration_sha256: "b".repeat(64),
        };
        assert!(
            run(args)
                .unwrap_err()
                .to_string()
                .contains("Frozen model/calibration")
        );
    }
}
