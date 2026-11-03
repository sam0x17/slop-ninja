//! Train-only independent PG book descendants, using the unchanged default v2
//! narrative recipe. No network, model invocation, fitting or Test prose.
//! --plan ROOTS RETAINED_ROOTS CONFIG1 CONFIG2 PROPS1 PROPS2 NEW_PLAN.json
//! --verify-roots PLAN.json NEW_RECEIPT.json
//! PLAN.json NEW_OUTPUT_DIRECTORY
#![recursion_limit = "256"]
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    path::{Path, PathBuf},
};
#[allow(dead_code, unused_attributes)]
#[path = "assemble_balanced_dataset.rs"]
mod balanced;
#[cfg(test)]
use balanced::compact_sha;
use balanced::{
    MODELS, SLOTS, array, bytes, counter, digest_at, ids, load, number, records, sha, str_at,
    unique_rows, write_new,
};
const PARTS: [&str; 3] = ["train", "development", "calibration"];
const PLAN_SCHEMA: &str = "slop_ninja_book_augmentation_assembly_plan_v1";
const RECIPE: &str = "default_v2_narrative_scratch_20_60_100";
const PERCENTAGES: [u64; 4] = [0, 20, 60, 100];
#[path = "assemble_book_augmentation/admission.rs"]
mod admission;
#[path = "assemble_book_augmentation/generation.rs"]
mod generation;
#[path = "assemble_book_augmentation/recipe.rs"]
mod recipe;
#[allow(dead_code)]
#[path = "prepare_supervised_forest/supervision.rs"]
mod supervision;
#[cfg(test)]
#[path = "assemble_book_augmentation/tests.rs"]
mod tests;

fn pin(path: &Path) -> Result<Value> {
    Ok(json!({"path":path.canonicalize()?,"sha256":sha(&bytes(path)?)}))
}
fn pinned(pin: &Value, inputs: &mut Vec<Value>) -> Result<Vec<u8>> {
    let path = Path::new(str_at(pin, "path")?);
    let b = bytes(path)?;
    ensure!(
        sha(&b) == digest_at(pin, "sha256")?,
        "pinned input changed: {}",
        path.display()
    );
    inputs.push(pin.clone());
    Ok(b)
}
fn pinned_json(pin: &Value, inputs: &mut Vec<Value>) -> Result<Value> {
    Ok(serde_json::from_slice(&pinned(pin, inputs)?)?)
}
fn resolve(base: &Path, value: &Value, key: &str) -> Result<PathBuf> {
    Ok(base.join(str_at(value, key)?))
}
fn normalized(text: &str) -> String {
    sha(text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .as_bytes())
}
fn jsonl(rows: &[Value]) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut b, row)?;
        b.push(b'\n');
    }
    Ok(b)
}

// Metadata-only plan creation. In particular, do not open any records or source
// bytes just to prepare the generation/assembly contract.
fn make_plan(a: &[String]) -> Result<()> {
    ensure!(
        a.len() == 7,
        "--plan ROOTS RETAINED_ROOTS CONFIG1 CONFIG2 PROPS1 PROPS2 NEW_PLAN"
    );
    let out = Path::new(&a[6]);
    ensure!(!out.exists(), "new plan required");
    let generation_root = out.parent().context("plan parent")?;
    let mut evidence = Vec::new();
    let root_pin = pin(&Path::new(&a[0]).join("manifest.json"))?;
    let root = pinned_json(&root_pin, &mut evidence)?;
    admission::root_manifest(&root)?;
    let book_plan = pinned_json(&root["plan"], &mut evidence)?;
    let population = pinned_json(&book_plan["inputs"]["population_plan"], &mut evidence)?;
    ensure!(
        book_plan["population_plan"] == population,
        "embedded population plan differs"
    );
    let retained_pin = pin(&Path::new(&a[1]).join("manifest.json"))?;
    let retained = pinned_json(&retained_pin, &mut evidence)?;
    admission::retained_manifest(&retained)?;
    let mut generators = Vec::new();
    for slot in 0..2 {
        let config_pin = pin(Path::new(&a[2 + slot]))?;
        let config = pinned_json(&config_pin, &mut evidence)?;
        generation::check_slot(slot, &config, "train")?;
        let props_pin = pin(Path::new(&a[4 + slot]))?;
        let props = pinned_json(&props_pin, &mut evidence)?;
        generation::check_server(&props, &props, &config)?;
        generators.push(json!({"slot":SLOTS[slot],"identity":MODELS[slot],
            "config":config_pin,"server_properties":props_pin,"run_directory":SLOTS[slot],"requested":116}));
    }
    let mut protected = Vec::new();
    for p in array(&population, "populations")? {
        protected.push(json!({"kind":p["kind"],"manifest":population["files"][str_at(p,"manifest_key")?],
            "records":p["records"],"count":p["count"],"allowed_splits":p["allowed_splits"],"references_only":p["references_only"]}));
    }
    let plan = json!({"schema":PLAN_SCHEMA,"recipe":RECIPE,"roots_manifest":root_pin,
        "generation_protocol":pin(&generation_root.join("PROTOCOL.txt"))?,
        "generator_binary":pin(&generation_root.join("bin/a_balanced_generate"))?,
        "runtime_launch":pin(&generation_root.join("serve.sh"))?,
        "roots_records_sha256":root["records_sha256"],"prospective_roots_manifest":retained_pin,
        "prospective_records_sha256":retained["records_sha256"],"protected_populations":protected,
        "numeric_contract":population["files"]["numeric_contract"],"generators":generators,
        "producer_sha256":sha(include_bytes!("../../../../examples/a_balanced_generate.rs")),
        "extractor_sha256":sha(include_bytes!("prepare_author_queries.rs")),
        "admission_implementation_sha256":sha(include_bytes!("prepare_train_books.rs")),
        "requested_book_candidates":32,"metadata_exclusions":root["metadata_exclusions"],
        "requested_human_records":29,"requested_generation_records":232,"requested_records":261,
        "test_text_opened":false,"train_extension_only":true,
        "duplicate_policy":"Reject colliding roots/IDs/families; exclude every generated copy matching a root, cumulative population, or another generated normalized text. Preserve every task outcome. Later union rechecks independently generated batches.",
        "exposure":"Pretraining and prior grammar exposure unknown/conservative; historical human proxies are not authenticated human-only production histories.",
        "generation_invocation":"a_balanced_generate ROOTS/humans.jsonl RUN_DIRECTORY PINNED_CONFIG; no --train-scale or --test"});
    validate_plan(&plan)?;
    fs::create_dir_all(out.parent().context("plan parent")?)?;
    write_new(out, &serde_json::to_vec_pretty(&plan)?)?;
    println!(
        "{}",
        json!({"plan":out,"sha256":sha(&bytes(out)?),"roots":29,"generation_tasks":232})
    );
    Ok(())
}
fn validate_plan(p: &Value) -> Result<()> {
    ensure!(
        p["schema"] == PLAN_SCHEMA
            && p["recipe"] == RECIPE
            && p["train_extension_only"] == true
            && p["test_text_opened"] == false,
        "book assembly plan scope/recipe"
    );
    ensure!(
        p["requested_book_candidates"] == 32
            && p["requested_human_records"] == 29
            && p["requested_generation_records"] == 232
            && p["requested_records"] == 261
            && array(p, "metadata_exclusions")?.len() == 3,
        "frozen book denominators"
    );
    ensure!(
        p["producer_sha256"]
            == sha(include_bytes!(
                "../../../../examples/a_balanced_generate.rs"
            ))
            && p["extractor_sha256"] == sha(include_bytes!("prepare_author_queries.rs"))
            && p["admission_implementation_sha256"]
                == sha(include_bytes!("prepare_train_books.rs")),
        "producer/extractor/admission version differs"
    );
    let g = array(p, "generators")?;
    ensure!(g.len() == 2, "exactly two known models required");
    for slot in 0..2 {
        ensure!(
            g[slot]["slot"] == SLOTS[slot]
                && g[slot]["identity"] == MODELS[slot]
                && g[slot]["requested"] == 116,
            "known model slot/denominator"
        );
    }
    ensure!(
        g[0]["run_directory"] != g[1]["run_directory"],
        "shared generator output directory"
    );
    let expected = BTreeMap::from([("current", 929), ("references", 8), ("augmentation", 1049)]);
    let mut actual = BTreeMap::new();
    for spec in array(p, "protected_populations")? {
        ensure!(
            actual
                .insert(str_at(spec, "kind")?, counter(spec, "count")?)
                .is_none(),
            "duplicate protected kind"
        );
        let parts = array(spec, "allowed_splits")?;
        ensure!(
            !parts.is_empty()
                && parts
                    .iter()
                    .all(|s| s.as_str().is_some_and(|s| PARTS.contains(&s))),
            "protected Test requested"
        );
    }
    ensure!(actual == expected, "missing/changed cumulative populations");
    Ok(())
}

#[derive(Default)]
struct Protected {
    ids: BTreeSet<String>,
    families: BTreeSet<String>,
    texts: BTreeSet<String>,
}
impl Protected {
    fn add(&mut self, row: &Value) -> Result<()> {
        let text = str_at(row, "text")?;
        ensure!(
            row["text_sha256"] == sha(text.as_bytes()),
            "protected text hash"
        );
        ensure!(
            self.ids.insert(str_at(row, "id")?.into()),
            "duplicate protected ID"
        );
        self.families.insert(str_at(row, "source_group")?.into());
        self.texts.insert(normalized(text));
        Ok(())
    }
}
fn protected(plan: &Value, inputs: &mut Vec<Value>) -> Result<Protected> {
    let mut out = Protected::default();
    // Validate every metadata allow-list before opening any protected prose file.
    let mut verified = Vec::new();
    for spec in array(plan, "protected_populations")? {
        let m = pinned_json(&spec["manifest"], inputs)?;
        ensure!(
            m["schema"] == "slop_ninja_a_supervised_dataset_v1" && m["test_text_opened"] == false,
            "protected non-Test manifest required"
        );
        let permitted = ids(&m, "non_test_ids")?;
        let reserved = ids(&m, "test_ids")?;
        ensure!(
            permitted.is_disjoint(&reserved)
                && permitted.len() as u64 == counter(spec, "count")?
                && m["records_sha256"] == spec["records"]["sha256"],
            "protected manifest roster/hash"
        );
        out.ids.extend(reserved);
        verified.push((spec, m, permitted));
    }
    for (spec, m, permitted) in verified {
        let rows = records(&pinned(&spec["records"], inputs)?)?;
        ensure!(
            unique_rows(&rows)?.keys().cloned().collect::<BTreeSet<_>>() == permitted,
            "protected exact roster"
        );
        for row in rows {
            let split = str_at(&row, "frozen_split")?;
            ensure!(
                PARTS.contains(&split)
                    && array(spec, "allowed_splits")?.contains(&row["frozen_split"])
                    && m["split_assignments"][str_at(&row, "source_group")?] == split,
                "protected split differs"
            );
            ensure!(
                matches!(str_at(&row, "purpose")?, "query" | "reference")
                    && (spec["references_only"] != true || row["purpose"] == "reference"),
                "protected purpose"
            );
            out.add(&row)?;
        }
    }
    let m = pinned_json(&plan["prospective_roots_manifest"], inputs)?;
    admission::retained_manifest(&m)?;
    ensure!(
        m["records_sha256"] == plan["prospective_records_sha256"],
        "prospective root hash"
    );
    let file = Path::new(str_at(&plan["prospective_roots_manifest"], "path")?)
        .parent()
        .context("prospective parent")?
        .join("humans.jsonl");
    ensure!(m["records_file"] == "humans.jsonl", "prospective filename");
    let rows = records(&pinned(
        &json!({"path":file,"sha256":m["records_sha256"]}),
        inputs,
    )?)?;
    ensure!(
        unique_rows(&rows)?.keys().cloned().collect::<BTreeSet<_>>() == ids(&m, "non_test_ids")?
            && rows.len() == 196,
        "prospective exact admitted roster"
    );
    for row in rows {
        ensure!(
            row["frozen_split"] == "train"
                && row["purpose"] == "query"
                && row["origin"] == "human_only"
                && row["parent_id"].is_null(),
            "prospective Train humans only"
        );
        out.add(&row)?;
    }
    Ok(out)
}
fn exclude_duplicates(
    humans: &[Value],
    generated: Vec<Value>,
    protected: &Protected,
) -> Result<(Vec<Value>, Vec<Value>)> {
    let mut root_texts = BTreeSet::new();
    for r in humans {
        ensure!(
            !protected.ids.contains(str_at(r, "id")?)
                && !protected.families.contains(str_at(r, "source_group")?),
            "book root ID/family crosses protected population"
        );
        let hash = normalized(str_at(r, "text")?);
        ensure!(
            !protected.texts.contains(&hash) && root_texts.insert(hash),
            "book root duplicates protected/selected text"
        );
    }
    let mut groups = BTreeMap::<String, Vec<Value>>::new();
    for r in generated {
        ensure!(
            !protected.ids.contains(str_at(&r, "id")?)
                && !protected.families.contains(str_at(&r, "source_group")?),
            "generated ID/family crosses protected population"
        );
        groups
            .entry(normalized(str_at(&r, "text")?))
            .or_default()
            .push(r);
    }
    let mut rows = humans.to_vec();
    let mut excluded = Vec::new();
    for (hash, rs) in groups {
        let reason = if protected.texts.contains(&hash) {
            Some("matches_cumulative_or_prospective_text")
        } else if root_texts.contains(&hash) {
            Some("matches_book_human_root")
        } else if rs.len() > 1 {
            Some("all_copies_of_duplicate_generated_text")
        } else {
            None
        };
        for r in rs {
            if let Some(reason) = reason {
                excluded.push(json!({"id":r["id"],"parent_id":r["parent_id"],
                "source_group":r["source_group"],"text_sha256":r["text_sha256"],"normalized_text_sha256":hash,
                "reason":reason,"retained_generation_evidence":true}));
            } else {
                rows.push(r);
            }
        }
    }
    Ok((rows, excluded))
}
fn assemble(plan_path: &Path, out: &Path, verify_only: bool) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let mut inputs = Vec::new();
    let (p, pb) = load(plan_path, &mut inputs)?;
    validate_plan(&p)?;
    for key in ["generation_protocol", "generator_binary", "runtime_launch"] {
        pinned(&p[key], &mut inputs)?;
    }
    let base = plan_path.parent().context("plan parent")?;
    let specs = array(&p, "generators")?;
    // Refuse incomplete generation before opening any corpus records.
    if !verify_only {
        for spec in specs {
            let (r, _) = load(
                &resolve(base, spec, "run_directory")?.join("report.json"),
                &mut inputs,
            )?;
            ensure!(
                r["schema"] == "slop_ninja_controlled_generation_report_v2"
                    && r["complete"] == true
                    && r["test_opened"] == false,
                "incomplete generation; no dataset emitted"
            );
        }
    }
    let (humans, root_manifest) = admission::roots(&p, &mut inputs)?;
    let protected = protected(&p, &mut inputs)?;
    exclude_duplicates(&humans, Vec::new(), &protected)?;
    if verify_only {
        let receipt = json!({"schema":"slop_ninja_book_augmentation_root_check_v1","assembly_plan_sha256":sha(&pb),
            "roots":humans.len(),"protected_records_including_reserved_ids":protected.ids.len(),"inputs":inputs,
            "generation_read":false,"test_text_opened":false,"passed":true});
        write_new(out, &serde_json::to_vec_pretty(&receipt)?)?;
        return Ok(());
    }
    let mut generated = Vec::new();
    let mut reports = Vec::new();
    let mut prompts = BTreeMap::new();
    for slot in 0..2 {
        let spec = &specs[slot];
        let config = pinned_json(&spec["config"], &mut inputs)?;
        generation::check_slot(slot, &config, "train")?;
        let dir = resolve(base, spec, "run_directory")?;
        let (rows, report) = balanced::generator_for_roster(
            &dir,
            slot,
            &config,
            digest_at(&spec["config"], "sha256")?,
            &humans,
            digest_at(&p, "roots_records_sha256")?,
            &mut inputs,
            &mut prompts,
            &balanced::GenerationPolicy {
                requested_tasks: 116,
                producer_sha256: digest_at(&p, "producer_sha256")?,
                slot_check: generation::check_slot,
                plan_check: generation::check_plan,
            },
        )?;
        generation::evidence(&dir, spec, &config, &humans, &rows, &mut inputs)?;
        generated.extend(rows);
        reports.push(report);
    }
    unique_rows(&humans.iter().chain(&generated).cloned().collect::<Vec<_>>())?;
    let accepted = humans.len() + generated.len();
    let failed: u64 = reports
        .iter()
        .map(|r| counter(r, "failed"))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .sum();
    ensure!(
        accepted as u64 + failed == 261,
        "requested outcomes accounting"
    );
    let (mut rows, excluded) = exclude_duplicates(&humans, generated, &protected)?;
    ensure!(
        rows.len() + excluded.len() == accepted,
        "post-exclusion accounting"
    );
    rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let mut assignments = BTreeMap::new();
    for r in &rows {
        ensure!(
            r["frozen_split"] == "train" && r["purpose"] == "query",
            "Train queries only"
        );
        assignments.insert(str_at(r, "source_group")?, "train");
        supervision::Supervision::parse(r)?;
    }
    let output = jsonl(&rows)?;
    let evidence = serde_json::to_vec_pretty(
        &json!({"schema":"slop_ninja_book_augmentation_evidence_v1","inputs":inputs}),
    )?;
    let audit = json!({"schema":"slop_ninja_book_augmentation_audit_v1","requested_book_candidates":32,
        "metadata_exclusions":root_manifest["metadata_exclusions"],"source_admission_requested":32,"source_admission_exclusions":root_manifest["metadata_exclusions"],"requested_roots":29,"requested_generation_tasks":232,
        "accepted_before_exclusions":accepted,"generation_failures":failed,"included":rows.len(),"excluded":excluded,
        "generators":reports,"protected_populations":p["protected_populations"],"prospective_roots_manifest":p["prospective_roots_manifest"],
        "test_text_opened":false,"later_union_must_recheck_generated_batches":true});
    let ab = serde_json::to_vec_pretty(&audit)?;
    let m = json!({"schema":"slop_ninja_a_supervised_dataset_v1","cohort":"large_batch_v1_books_default_v2",
        "records":rows.len(),"records_sha256":sha(&output),"input_sha256":sha(&output),
        "non_test_ids":rows.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"test_ids":[],"split_assignments":assignments,
        "source_families":assignments.len(),"sources":[{"assembly_plan":plan_path,"sha256":sha(&pb)},
            p["roots_manifest"],{"evidence_index":"evidence-index.json","sha256":sha(&evidence)}],
        "source_manifest_sha256":p["roots_manifest"]["sha256"],"generation_reports":reports,
        "requested_book_candidates":32,"metadata_exclusions":root_manifest["metadata_exclusions"],
        "source_admission_requested":32,"source_admission_exclusions":root_manifest["metadata_exclusions"],
        "requested_human_records":29,"requested_generation_records":232,"requested_records":261,
        "accepted_before_exclusions":accepted,"excluded":excluded,"audit_sha256":sha(&ab),
        "complete":true,"test_text_opened":false,"clean_holdout":false,"train_extension_only":true,
        "reference_policy":"Train query extension only; all reference galleries remain frozen.",
        "rights_scope":"Source-specific US public-domain basis and unchanged source ancestry; generator configs retain separate model/output permission evidence. No automatic publication or worldwide-rights claim.",
        "exposure":p["exposure"],"labels_are_not_inputs":true,"tokenizer_qualified":false,
        "later_union_must_recheck_generated_batches":true,"implementation_sha256":sha(include_bytes!("assemble_book_augmentation.rs")),
        "helper_sha256":{"admission":sha(include_bytes!("assemble_book_augmentation/admission.rs")),
            "generation":sha(include_bytes!("assemble_book_augmentation/generation.rs")),"recipe":sha(include_bytes!("assemble_book_augmentation/recipe.rs")),
            "shared_checker":sha(include_bytes!("assemble_balanced_dataset.rs")),"supervision":sha(include_bytes!("prepare_supervised_forest/supervision.rs"))}});
    let mb = serde_json::to_vec_pretty(&m)?;
    supervision::Dataset::parse(&mb)?;
    fs::create_dir(out)?;
    for (name, b) in [
        ("records.jsonl", output),
        ("evidence-index.json", evidence),
        ("audit.json", ab),
        ("manifest.json", mb),
    ] {
        write_new(&out.join(name), &b)?;
    }
    File::open(out)?.sync_all()?;
    println!(
        "{}",
        json!({"manifest":out.join("manifest.json"),"records":rows.len(),"requested_generation":232,"failed":failed,"excluded":excluded.len()})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    if a.first().is_some_and(|s| s == "--plan") {
        return make_plan(&a[1..]);
    }
    if a.first().is_some_and(|s| s == "--verify-roots") {
        ensure!(a.len() == 3, "--verify-roots PLAN NEW_RECEIPT");
        return assemble(Path::new(&a[1]), Path::new(&a[2]), true);
    }
    ensure!(a.len() == 2, "assemble_book_augmentation PLAN.json NEW_OUT");
    assemble(Path::new(&a[0]), Path::new(&a[1]), false)
}
