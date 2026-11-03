//! Publish the two remaining frozen Train batches and the full fitted roster.
//! REPO STAGE LARGE_BATCH NEW_DATASETS_CHILD. No fitting, model/provider calls or Test reads.
#![recursion_limit = "256"]
#[allow(dead_code)]
#[path = "audit_encoder_publication.rs"]
mod audit;
#[allow(dead_code, unused_attributes)]
#[path = "publish_balanced_snapshot.rs"]
mod books;
#[allow(dead_code, unused_attributes)]
#[path = "publish_train_augmentation.rs"]
mod prior;
use anyhow::{Context, Result, ensure};
use prior::{array, compact_sha, flat, load, public_safety, read_local, s, sha};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path},
};

const REGISTRY: &str = "2b5fc26d0de6593745f1eb7e4cac4e611173107a7dc2c0bbd0bb0ef0416b5daa";
const FIT_TRAIN: &str = "d26549d2063d0acedbf05f7aa53f95a03649369f6c1768ab6b25e2c833e14420";
const FIT_CONTRACT: &str = "25ab2507718ab3ba3862e174458f13031020a46ee0bcf3c96387dc4740120042";
const V6_TRAIN: &str = "023d6e5428236502e877bac0afe0e9e9ee4aa9d10ab38eb940ca8e915cf7502e";
const RELEASE: &str = "a-retained-train-20261026-v1";
type Files = BTreeMap<String, Vec<u8>>;
struct Batch {
    name: &'static str,
    dataset: &'static str,
    generation: &'static str,
    manifest: &'static str,
    records: &'static str,
    count: usize,
    works: usize,
    book: bool,
}
const BATCHES: [Batch; 2] = [
    Batch {
        name: "retained196",
        dataset: "generation-retained-v1/dataset-v1",
        generation: "generation-retained-v1",
        manifest: "b11e7f74664b3fdf68d060ae2aac32a63055777a5041740721bac3c1e17b5d69",
        records: "63172c072543c8b7a77e3a5c10f652d2cf0fed7234b9884ee2025d718f450e1e",
        count: 2104,
        works: 196,
        book: false,
    },
    Batch {
        name: "books29",
        dataset: "books/dataset-v1",
        generation: "generation-books-v1",
        manifest: "e4b051a649fdd994bc8992b8e922b98a4425d9a4a6f246b14c0ad316752db02a",
        records: "932f8cf6064da0fa7fcd232b6ed66a353dcc8126af23967f0e117458ad10dcc6",
        count: 261,
        works: 29,
        book: true,
    },
];
fn lines(bytes: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(bytes)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn jsonl(rows: impl IntoIterator<Item = Value>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut out, &row)?;
        out.push(b'\n');
    }
    Ok(out)
}
fn add_json(files: &mut Files, name: &str, v: &Value) -> Result<()> {
    public_safety(v)?;
    let mut b = serde_json::to_vec_pretty(v)?;
    b.push(b'\n');
    ensure!(
        files.insert(name.into(), b).is_none(),
        "duplicate output file"
    );
    Ok(())
}
fn path_key(path: &Path, root: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    (rel.components().all(|c| matches!(c, Component::Normal(_))))
        .then(|| rel.to_str().map(str::to_owned))
        .flatten()
}
fn evidence(index: &Value, generation: &Path) -> Result<(Value, BTreeMap<String, String>)> {
    let mut entries = Vec::new();
    let mut hashes = BTreeMap::new();
    for item in array(index, "inputs")? {
        if let Some(path) = item["path"].as_str() {
            flat(item, &["path", "sha256"], &[])?;
            let hash = s(item, "sha256")?;
            ensure!(
                hash.len() == 64 && hash.bytes().all(|x| x.is_ascii_hexdigit()),
                "evidence hash"
            );
            let key = if let Some(rel) = path_key(Path::new(path), generation) {
                if let Some(old) = hashes.insert(rel.clone(), hash.into()) {
                    ensure!(old == hash, "evidence collision");
                }
                format!("generation/{rel}")
            } else {
                format!("external-evidence/{}", sha(path.as_bytes()))
            };
            entries.push(json!({"archive_reference":key,"sha256":hash}));
        } else {
            let mut task = flat(
                item,
                &[
                    "task_id",
                    "intent_count",
                    "response_count",
                    "uncertain_attempt_indices",
                ],
                &["task"],
            )?;
            if let Some(path) = item["task"].as_str() {
                task["archive_reference"] = path_key(Path::new(path), generation)
                    .context("task outside generation archive")?
                    .into();
            }
            entries.push(task);
        }
    }
    let value = json!({"schema":"slop_ninja_train_public_evidence_index_v1","entries":entries,
        "archive_references_are_not_distributed_files":true,"raw_invocations_retained_privately":true,
        "external_locators_are_opaque_hashes":true});
    public_safety(&value)?;
    Ok((value, hashes))
}
fn scope(row: &Value, book: bool) -> Result<()> {
    ensure!(
        row["schema"] == "slop_ninja_a_supervised_record_v1"
            && row["frozen_split"] == "train"
            && row["purpose"] == "query"
            && row["documented_human_only"] == false,
        "Train historical scope"
    );
    ensure!(
        row["text_sha256"] == sha(s(row, "text")?.as_bytes()),
        "text hash"
    );
    prior::validate_targets(row)?;
    if book {
        ensure!(
            (if row["origin"] == "model_only" {
                row["human_identity"].is_null()
            } else {
                row["human_identity"].is_string()
                    && row["human_identity"] == row["source"]["record"]["author_key"]
            }) && row["exposed_to_grammar"] == true
                && row["grammar_exposure"]
                    == "conservative_unknown_prior_exposure;fresh_acquisition_is_not_an_exposure_audit",
            "book writer/exposure scope"
        );
        ensure!(
            row["rights"]["redistribute_text"] != false && row["rights"]["model_release"] != false,
            "book permission denied"
        );
    } else {
        ensure!(
            row["human_identity"].is_null()
                && row["exposed_to_grammar"].is_null()
                && row["grammar_exposure"] == "unknown",
            "retained identity/exposure scope"
        );
        ensure!(
            row["rights"]["redistribute_text"] == true && row["rights"]["model_release"] == true,
            "retained publication denied"
        );
    }
    ensure!(
        row["rights"]["commercial_training"] == true
            && row["rights"]["external_evaluation"] == true,
        "source permission denied"
    );
    Ok(())
}
fn parent(row: &Value, p: &Value, book: bool) -> Result<()> {
    ensure!(
        p["origin"] == "human_only" && p["generation"].is_null() && row["parent_id"] == p["id"],
        "human parent"
    );
    for k in [
        "source",
        "rights",
        "metadata",
        "source_group",
        "frozen_split",
        "purpose",
    ] {
        ensure!(row[k] == p[k], "descendant inherited field differs: {k}");
    }
    let e = &row["share_evidence"];
    ensure!(
        e["source_text"] == p["text"] && e["source_sha256"] == p["text_sha256"],
        "parent bytes"
    );
    let percent = e["requested_percent_not_target"]
        .as_u64()
        .context("requested percent")?;
    ensure!(
        if percent == 0 {
            row["human_identity"].is_null()
        } else {
            row["human_identity"] == p["human_identity"]
        },
        "descendant human attribution role"
    );
    let allowed: &[u64] = if book {
        &[0, 20, 60, 100]
    } else {
        &[0, 10, 40, 80, 100]
    };
    ensure!(
        allowed.contains(&percent)
            && row["origin"] == if percent == 0 { "model_only" } else { "mixed" }
            && e["full_rewrite_is_mixed_history"] == (percent == 100),
        "recipe production history"
    );
    // validate_targets verifies exact assembly, UTF-8 spans, word counts and realized share.
    prior::validate_targets(row)
}
fn bind_book(root: &Value, large: &Path, files: &mut Files) -> Result<()> {
    let source = &root["source"];
    let r = &source["record"];
    books::project_source_scoped(root, false)?;
    ensure!(
        root["origin"] == "human_only"
            && root["generation"].is_null()
            && root["parent_id"].is_null(),
        "book root"
    );
    let acquisition = large.join("books/acquisition-v1");
    let source_path = Path::new(s(r, "text_path")?);
    let relative =
        path_key(source_path, &acquisition).context("book outside original acquisition")?;
    ensure!(
        r["text_relative_path"] == relative && r["raw_download"]["path"] == relative,
        "book location binding"
    );
    let bytes = read_local(&acquisition, &relative)?;
    ensure!(
        sha(&bytes) == r["text_sha256"] && r["text_bytes"] == bytes.len(),
        "underlying book bytes"
    );
    let start = source["byte_span"][0].as_u64().context("excerpt start")? as usize;
    let end = source["byte_span"][1].as_u64().context("excerpt end")? as usize;
    ensure!(
        bytes.get(start..end) == Some(s(root, "text")?.as_bytes()),
        "book excerpt changed"
    );
    let catalog = read_local(&acquisition, s(&r["catalog_evidence"], "path")?)?;
    ensure!(
        sha(&catalog) == r["rights"]["evidence_sha256"]
            && r["catalog_evidence"]["sha256"] == r["rights"]["evidence_sha256"],
        "catalog hash"
    );
    let name = format!("sources/pg-{}.rdf", r["pg_id"].as_u64().context("PG ID")?);
    if let Some(old) = files.insert(name, catalog.clone()) {
        ensure!(old == catalog, "catalog conflict");
    }
    Ok(())
}
fn project(
    row: &Value,
    line_hash: &str,
    batch: &Batch,
    generation: &Path,
    generators: &[prior::Generator],
    evidence: &BTreeMap<String, String>,
) -> Result<Value> {
    let mut out = flat(
        row,
        &[
            "schema",
            "id",
            "text",
            "text_sha256",
            "source_group",
            "frozen_split",
            "purpose",
            "origin",
            "evidence",
            "documented_human_only",
            "human_identity",
            "human_role_present",
            "ai_identity",
            "ai_role_present",
            "share_target",
            "exposed_to_grammar",
            "grammar_exposure",
            "parent_id",
        ],
        &[
            "source",
            "rights",
            "metadata",
            "share_evidence",
            "generation",
        ],
    )?;
    let metadata = if batch.book {
        flat(
            &row["metadata"],
            &[
                "admission_plan_sha256",
                "batch",
                "source_collection",
                "source_title",
                "underlying_work_aliases",
                "word_bound_is_not_tokenizer_qualification",
            ],
            &[],
        )?
    } else {
        flat(
            &row["metadata"],
            &[
                "source_collection",
                "original_record_id",
                "word_count",
                "selection_rank_sha256",
                "v6_upstream_exposure",
                "historical_date",
                "historical_date_basis",
                "admission_plan_sha256",
                "admission_protocol_sha256",
                "source_batch",
            ],
            &[],
        )?
    };
    out["metadata"] = metadata;
    out["source"] = if batch.book {
        books::project_source_scoped(row, false)?
    } else {
        prior::project_source(&row["source"])?
    };
    out["rights"] = prior::public_rights(&row["rights"])?;
    out["share_evidence"] = row["share_evidence"].clone();
    out["generation"] = if row["generation"].is_null() {
        Value::Null
    } else {
        let g = generators
            .iter()
            .find(|g| g.config["identity"] == row["ai_identity"])
            .context("unqualified generator")?;
        prior::project_generation_scoped(
            row,
            g,
            generation,
            evidence,
            "",
            if batch.book {
                prior::GenerationRecipe::BookV2
            } else {
                prior::GenerationRecipe::TrainScale
            },
        )?
    };
    out["publication"] = json!({"batch":batch.name,"original_line_sha256":line_hash,"line_hash_basis":"original JSONL line including newline",
        "original_row_sha256":compact_sha(row)?,"original_records_sha256":batch.records,"original_manifest_sha256":batch.manifest,
        "public_exposed_population":true,"future_hidden_test":false,"source_rights_are_ancestral":true,
        "curation_and_generated_text_license":"CC-BY-SA-4.0 to the extent rights exist, subject to underlying rights and generator terms",
        "changes":if row["generation"].is_null(){"Source excerpt unchanged"}else{"Recorded scratch generation or controlled rewrite; exact production history and replacement spans retained"}});
    for key in [
        "id",
        "text",
        "text_sha256",
        "origin",
        "share_target",
        "share_evidence",
        "human_identity",
        "human_role_present",
        "ai_identity",
        "ai_role_present",
        "metadata",
        "grammar_exposure",
        "exposed_to_grammar",
    ] {
        ensure!(out[key] == row[key], "projection changed {key}");
    }
    public_safety(&out)?;
    Ok(out)
}
fn targets_match(label: &Value, contract: &Value) -> Result<()> {
    for role in ["human", "ai"] {
        let ids = array(contract, &format!("{role}_profiles"))?;
        ensure!(
            ids.iter().all(Value::is_string),
            "identity roster must contain names only"
        );
        let index = ids
            .iter()
            .position(|id| *id == label["roles"][role]["identity"]);
        ensure!(
            label["roles"][role]["known"] == index.is_some(),
            "known identity mask differs"
        );
        let target = if label["roles"][role]["present"] == true {
            json!(index)
        } else {
            Value::Null
        };
        ensure!(
            label["targets"][role] == target,
            "attribution target index differs"
        );
    }
    Ok(())
}
fn public_audit(v: &Value) -> Result<Value> {
    let mut out = prior::fields(
        v,
        &[
            "schema",
            "requested_roots",
            "requested_generation_tasks",
            "accepted_before_exclusions",
            "included",
            "excluded",
            "generators",
            "origins",
            "ai_identities",
            "unknown_human_identity_rows",
            "full_rewrite_mixed_share_one_rows",
            "test_text_opened",
            "evidence_files_modified",
            "root_admission",
            "source_admission_exclusions",
            "metadata_exclusions",
            "requested_book_candidates",
            "source_admission_requested",
            "later_union_must_recheck_generated_batches",
            "generation_failures",
        ],
        &["protected_populations", "prospective_roots_manifest"],
    )?;
    if !v["prospective_roots_manifest"].is_null() {
        out["prospective_roots_manifest"] =
            flat(&v["prospective_roots_manifest"], &["sha256"], &["path"])?;
    }
    // Original protected-population receipts are retained in the separately pinned evidence index.
    out["protected_population_count"] = array(v, "protected_populations")?.len().into();
    public_safety(&out)?;
    Ok(out)
}
fn training_index(
    repo: &Path,
    stage: &Path,
    new_rows: &[Value],
    new_bytes: &[u8],
    files: &mut Files,
) -> Result<Value> {
    let cm = load(repo, "datasets/compression-manifest.json", None)?;
    let mut inventory = BTreeMap::<String, (String, Value)>::new();
    let mut sources = Vec::new();
    for path in [
        "datasets/a-balanced-20261026-v1/train.jsonl",
        "datasets/a-train-augmentation-20261026-v1/records.jsonl",
        "datasets/a-encoder-20261026-v1/train.jsonl",
    ] {
        let bytes = read_local(repo, path)?;
        let entry = array(&cm, "files")?
            .iter()
            .find(|f| f["decoded_path"] == path)
            .context("published binding")?;
        ensure!(
            entry["decoded_sha256"] == sha(&bytes),
            "previous publication changed"
        );
        sources.push(json!({"logical_path":path,"decoded_sha256":sha(&bytes)}));
        for row in lines(&bytes)? {
            ensure!(
                inventory
                    .insert(s(&row, "id")?.into(), (path.into(), row))
                    .is_none(),
                "duplicate published ID"
            );
        }
    }
    let new_path = format!("datasets/{RELEASE}/records.jsonl");
    sources.push(json!({"logical_path":new_path,"decoded_sha256":sha(new_bytes)}));
    for row in new_rows {
        ensure!(
            inventory
                .insert(s(row, "id")?.into(), (new_path.clone(), row.clone()))
                .is_none(),
            "new published ID collision"
        );
    }
    let fit = load(stage, "fit-bundle-v1/manifest.json", None)?;
    let bytes = read_local(stage, "fit-bundle-v1/train.jsonl")?;
    let contract = load(stage, "fit-bundle-v1/contract.json", Some(FIT_CONTRACT))?;
    ensure!(
        sha(&bytes) == FIT_TRAIN
            && fit["files_sha256"]["train.jsonl"] == FIT_TRAIN
            && fit["contract_sha256"] == FIT_CONTRACT
            && fit["populations"]["train"]["rows"] == 4157,
        "frozen fit binding"
    );
    let target_contract = json!({"schema":"slop_ninja_public_training_targets_v1","human_profiles":contract["human_profiles"],
        "ai_profiles":contract["ai_profiles"],"origin_order":contract["origin_order"],"original_fit_contract_sha256":FIT_CONTRACT,
        "private_profile_vectors_included":false,"missing_target":"null remains masked; conditional identity does not assert role involvement"});
    let mut members = Vec::new();
    let mut seen = BTreeSet::new();
    let mut groups = BTreeSet::new();
    let mut counts = BTreeMap::<String, usize>::new();
    for (index, label) in lines(&bytes)?.into_iter().enumerate() {
        ensure!(
            label["frozen_split"] == "train" && label["purpose"] == "query",
            "fit scope"
        );
        let id = s(&label, "id")?;
        ensure!(seen.insert(id.to_owned()), "duplicate fitted ID");
        let (path, p) = inventory
            .get(id)
            .context("fitted input missing from publication")?;
        ensure!(
            audit::matches(&label, p) && p["text_sha256"] == sha(s(p, "text")?.as_bytes()),
            "fitted labels/roles/text differ: {id}"
        );
        targets_match(&label, &target_contract)?;
        let normalized = s(p, "text")?
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        ensure!(
            label["normalized_text_sha256"] == sha(normalized.as_bytes()),
            "normalized fitted bytes differ"
        );
        groups.insert(s(&label, "source_group")?.to_owned());
        *counts.entry(path.clone()).or_default() += 1;
        let mut member = flat(
            &label,
            &[
                "id",
                "text_sha256",
                "normalized_text_sha256",
                "source_group",
                "frozen_split",
                "purpose",
                "exposed_to_grammar",
                "documented_human_only",
                "acquisition_stratum",
                "cohort",
            ],
            &["prefix", "roles", "targets"],
        )?;
        member["roles"] = label["roles"].clone();
        member["targets"] = label["targets"].clone();
        member["fit_row_index"] = index.into();
        member["published_file"] = path.clone().into();
        member["published_row_sha256"] = compact_sha(p)?.into();
        public_safety(&member)?;
        members.push(member);
    }
    ensure!(
        members.len() == 4157 && groups.len() == 357 && inventory.len() == members.len(),
        "full fitted roster differs"
    );
    files.insert("training-membership.jsonl".into(), jsonl(members)?);
    add_json(files, "target-contract.json", &target_contract)?;
    let summary = json!({"schema":"slop_ninja_fitted_training_publication_v1","complete":true,"records":4157,"source_families":357,
        "original_fit_train_sha256":FIT_TRAIN,"original_fit_manifest_sha256":sha(&read_local(stage,"fit-bundle-v1/manifest.json")?),
        "original_fit_contract_sha256":FIT_CONTRACT,"sources":sources,"by_published_file":counts,
        "training_membership_sha256":sha(&files["training-membership.jsonl"]),"target_contract_sha256":sha(&files["target-contract.json"]),
        "matching":"exact text and normalized hashes, original fit order, source family, Train/query scope, origin, involvement, measured share/missingness, human/AI identity, involvement, known masks and target indices",
        "fitting_reexecuted":false,"learned_inputs_include_metadata":false,"profile_vectors_or_weights_included":false});
    add_json(files, "training-reconstruction.json", &summary)?;
    Ok(summary)
}
fn run(repo: &Path, stage: &Path, large: &Path, out: &Path) -> Result<Value> {
    ensure!(
        !out.exists()
            && out.file_name().and_then(|p| p.to_str()) == Some(RELEASE)
            && out.parent().context("output parent")?.canonicalize()?
                == repo.join("datasets").canonicalize()?,
        "new pinned dataset child required"
    );
    let registry = load(
        repo,
        "datasets/a-balanced-20261026-v1/generators.json",
        Some(REGISTRY),
    )?;
    let original_bytes = read_local(repo, "datasets/detector-v6/train.jsonl")?;
    ensure!(sha(&original_bytes) == V6_TRAIN, "V6 Train binding");
    let mut originals = BTreeMap::new();
    for line in std::str::from_utf8(&original_bytes)?.lines() {
        let r: Value = serde_json::from_str(line)?;
        ensure!(
            originals
                .insert(s(&r, "id")?.to_owned(), (r, sha(line.as_bytes())))
                .is_none(),
            "V6 duplicate ID"
        );
    }
    let mut files = Files::new();
    let mut output = Vec::new();
    let mut indexes = Vec::new();
    let mut summaries = Vec::new();
    let mut all_groups = BTreeSet::new();
    let mut all_ids = BTreeSet::new();
    for batch in &BATCHES {
        let dir = large.join(batch.dataset);
        let generation = large.join(batch.generation);
        let m = load(&dir, "manifest.json", Some(batch.manifest))?;
        ensure!(
            m["complete"] == true
                && m["train_extension_only"] == true
                && m["test_text_opened"] == false
                && array(&m, "test_ids")?.is_empty()
                && m["records"] == batch.count
                && m["split_assignments"]
                    .as_object()
                    .context("splits")?
                    .values()
                    .all(|v| v == "train"),
            "batch Train scope"
        );
        let bytes = read_local(&dir, "records.jsonl")?;
        ensure!(
            sha(&bytes) == batch.records && m["records_sha256"] == batch.records,
            "batch record digest"
        );
        let mut rows = BTreeMap::new();
        let mut line_hashes = BTreeMap::new();
        for line in bytes.split_inclusive(|x| *x == b'\n') {
            ensure!(line.last() == Some(&b'\n'), "final newline");
            let row: Value = serde_json::from_slice(line)?;
            scope(&row, batch.book)?;
            let id = s(&row, "id")?.to_owned();
            line_hashes.insert(id.clone(), sha(line));
            ensure!(
                all_ids.insert(id.clone()) && rows.insert(id, row).is_none(),
                "duplicate batch ID"
            );
        }
        let roster = array(&m, "non_test_ids")?
            .iter()
            .map(|x| x.as_str().context("roster ID"))
            .collect::<Result<BTreeSet<_>>>()?;
        ensure!(
            rows.len() == batch.count
                && roster.len() == batch.count
                && rows.keys().map(String::as_str).eq(roster.into_iter()),
            "exact roster"
        );
        let mut roots = 0;
        let mut groups = BTreeSet::new();
        for row in rows.values().filter(|r| r["generation"].is_null()) {
            roots += 1;
            ensure!(
                groups.insert(s(row, "source_group")?.to_owned())
                    && all_groups.insert(s(row, "source_group")?.to_owned()),
                "duplicate root work"
            );
            if batch.book {
                bind_book(row, large, &mut files)?;
            } else {
                let (original, line) = originals
                    .get(s(&row["source"]["record"], "id")?)
                    .context("missing original V6 row")?;
                prior::bind_original(row, original, line)?;
            }
        }
        ensure!(roots == batch.works, "root population count");
        for row in rows.values().filter(|r| !r["generation"].is_null()) {
            parent(
                row,
                rows.get(s(row, "parent_id")?).context("missing parent")?,
                batch.book,
            )?;
        }
        let index = load(
            &dir,
            "evidence-index.json",
            Some(s(&m["sources"][2], "sha256")?),
        )?;
        let (public_index, evidence) = evidence(&index, &generation)?;
        indexes.push(json!({"batch":batch.name,"original_sha256":m["sources"][2]["sha256"],"index":public_index}));
        let mut generators = Vec::new();
        let mut public_generators = Vec::new();
        for slot in ["known_model_1", "known_model_2"] {
            let report = array(&m, "generation_reports")?
                .iter()
                .find(|r| r["slot"] == slot)
                .context("generation report")?;
            ensure!(report["complete"] == true, "generation incomplete");
            let config_sha = s(report, "config_sha256")?;
            let config = load(
                &generation,
                &format!("configs/{slot}.json"),
                Some(config_sha),
            )?;
            let bpath = format!("{slot}/binding.json");
            let binding = load(
                &generation,
                &bpath,
                Some(
                    evidence
                        .get(&bpath)
                        .context("binding absent from evidence")?,
                ),
            )?;
            ensure!(
                compact_sha(&binding)? == s(report, "binding_sha256")?
                    && config["identity"] == report["generator"],
                "binding/report identity"
            );
            let published = array(&registry, "generators")?
                .iter()
                .find(|g| g["configuration"]["identity"] == config["identity"])
                .context("model licensing")?;
            let model = prior::registry_model(&published["model"])?;
            let pc = prior::project_config(&config)?;
            for k in ["filename", "sha256", "revision", "size_bytes"] {
                ensure!(pc[k] == model[k], "model quantizer identity");
            }
            ensure!(
                pc["repo"] == model["quantized_repo"]
                    && pc["rights"]["model_license"] == model["original_license"],
                "model license binding"
            );
            public_generators.push(json!({"slot":slot,"configuration":pc,"model":model,"original_config_sha256":config_sha,"original_binding_sha256":report["binding_sha256"]}));
            generators.push(prior::Generator {
                slot: slot.into(),
                config,
                config_sha: config_sha.into(),
                registry: model,
                binding,
            });
        }
        for (id, row) in &rows {
            output.push(
                project(
                    row,
                    &line_hashes[id],
                    batch,
                    &generation,
                    &generators,
                    &evidence,
                )
                .with_context(|| format!("project {id}"))?,
            );
        }
        let audit = load(&dir, "audit.json", Some(s(&m, "audit_sha256")?))?;
        summaries.push(json!({"batch":batch.name,"source_families":roots,"records":rows.len(),"original_manifest_sha256":batch.manifest,"original_records_sha256":batch.records,
            "requested_human_records":m["requested_human_records"],"requested_generation_records":m["requested_generation_records"],"requested_records":m["requested_records"],
            "accepted_before_exclusions":m["accepted_before_exclusions"],"excluded":m["excluded"],"metadata_exclusions":m["metadata_exclusions"],"source_admission_exclusions":m["source_admission_exclusions"],
            "source_admission_requested":m["source_admission_requested"],"root_admission":m["root_admission"],"generation_reports":m["generation_reports"],
            "original_audit_sha256":m["audit_sha256"],"audit_counts":public_audit(&audit).context("public aggregate audit")?}));
        add_json(
            &mut files,
            &format!("generators-{}.json", batch.name),
            &json!({"generators":public_generators,"original_registry_sha256":REGISTRY,"weights_included":false}),
        )?;
    }
    ensure!(
        output.len() == 2365 && all_groups.len() == 225,
        "publication count"
    );
    output.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let bytes = jsonl(output.clone())?;
    let reconstructed = training_index(repo, stage, &output, &bytes, &mut files)?;
    files.insert("records.jsonl".into(), bytes);
    add_json(
        &mut files,
        "evidence-index.json",
        &json!({"schema":"slop_ninja_retained_train_evidence_v1","batches":indexes}),
    )?;
    add_json(
        &mut files,
        "audit.json",
        &json!({"schema":"slop_ninja_retained_train_publication_audit_v1","batches":summaries,"records":2365,"source_families":225,"complete":true,"test_text_opened":false}),
    )?;
    for (name, expected) in [
        (
            "qwen-base.LICENSE",
            "50cbab8a892c5f2993b8c7351a99182507472def3b1374558308605d99b86b32",
        ),
        (
            "phi-base.LICENSE",
            "fa8235e5b48faca34e3ca98cf4f694ef08bd216d28b58071a1f85b1d50cb814d",
        ),
    ] {
        let b = read_local(
            repo,
            &format!("datasets/a-balanced-20261026-v1/licenses/{name}"),
        )?;
        ensure!(sha(&b) == expected, "license changed");
        files.insert(format!("licenses/{name}"), b);
    }
    files.insert("NOTICE.md".into(), NOTICE.as_bytes().to_vec());
    files.insert("README.md".into(), README.as_bytes().to_vec());
    let manifest = json!({"schema":"slop_ninja_a_supervised_dataset_v1","cohort":RELEASE,"complete":true,"records":2365,"source_families":225,
        "records_file":"records.jsonl","records_sha256":sha(&files["records.jsonl"]),"input_sha256":sha(&files["records.jsonl"]),
        "non_test_ids":output.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"test_ids":[],
        "split_assignments":all_groups.iter().map(|g|(g.clone(),"train")).collect::<BTreeMap<_,_>>(),
        "train_extension_only":true,"test_text_opened":false,"clean_holdout":false,"labels_are_not_inputs":true,
        "publication":{"publisher_sha256":sha(include_bytes!("publish_retained_encoder_train.rs")),"shared_train_publisher_sha256":sha(include_bytes!("publish_train_augmentation.rs")),"shared_book_publisher_sha256":sha(include_bytes!("publish_balanced_snapshot.rs")),"training_matcher_sha256":sha(include_bytes!("audit_encoder_publication.rs")),"original_inputs_modified":false,"raw_invocations_included":false,"model_weights_included":false},
        "training_reconstruction":"training-reconstruction.json","files":files.iter().map(|(n,b)|json!({"file":n,"bytes":b.len(),"sha256":sha(b)})).collect::<Vec<_>>()});
    add_json(&mut files, "manifest.json", &manifest)?;
    let sums = files
        .iter()
        .map(|(n, b)| format!("{}  {n}\n", sha(b)))
        .collect::<String>();
    files.insert("SHA256SUMS".into(), sums.into_bytes());
    fs::create_dir(out)?;
    fs::create_dir(out.join("licenses"))?;
    fs::create_dir(out.join("sources"))?;
    for (name, b) in &files {
        let mut f = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(out.join(name))?;
        f.write_all(b)?;
        f.sync_all()?;
    }
    fs::File::open(out)?.sync_all()?;
    Ok(
        json!({"records":2365,"source_families":225,"full_fitted_train":reconstructed["records"],"fitted_source_families":reconstructed["source_families"],"manifest_sha256":sha(&files["manifest.json"])}),
    )
}
const NOTICE: &str = concat!(
    "# Source and generator notices\n\n",
    "PLOS ONE retains CC BY 4.0. Historical Wikipedia retains CC BY-SA 3.0 Unported and contributor/history notices; Wikinews retains CC BY 2.5. CMU Book Summaries additionally credits David Bamman and Noah Smith (2013), under its CC BY-SA 3.0 United States collection notice. Book authors are not asserted to be authors of their summaries. Per-row original rights, attribution, URLs and changes control.\n\n",
    "Book sources retain hash-bound Project Gutenberg catalog evidence for US public-domain status, authors, source URLs, dates and acquisition/exposure caveats. No rights outside the stated jurisdiction or authentic contemporary human-only workflow are inferred. No PG notice stripping is performed.\n\n",
    "Original excerpts are unchanged; generated descendants record scratch/rewrite history and exact final-word replacement spans. New curation/generated text is CC BY-SA 4.0 to the extent rights exist, subject to original source rights and generator terms; attribute Slop Ninja and the recorded source contributors. Retain notices, changes and ShareAlike conditions. Model weights/profiles are not distributed. Qwen's base license is Apache-2.0 and Phi's is MIT; see pinned notices and generator metadata. A quantizer metadata revision is not proof of original conversion weights.\n"
);
const README: &str = "# Retained Slop Ninja encoder Train data\n\nThis immutable snapshot publishes the remaining 2,365 Train queries from 225 works already used in the completed encoder experiment: 2,104 retained-source rows from 196 original V6 Train works and 261 rows from 29 independently acquired historical books. No new fitting, generation, detector call or Test opening occurs. Original inputs, fitted models and selection remain unchanged. These are exposed research inputs, not hidden evaluation data.\n\nExact text, source history, dates, author/generator identities, measured spans, missing targets and conservative exposure records are preserved. Retained-source generation used 10/40/80/100 source-span requests; the earlier book recipe used 20/60/100. Both retain separate scratch outputs, full rewrites remain mixed history at realized share one, and requested fractions are never substituted for measured final-word share. All failures/exclusions remain in audit.json; raw paid/model invocation bodies stay in ignored/private storage. Historical human proxies are not documented contemporary human-only histories.\n\ntraining-membership.jsonl and training-reconstruction.json identify all 4,157 fitted Train queries from 357 works, in original fit order, across four published snapshots. Each row binds exact/normalized text hashes, source family, supervision, roles, missingness and its published row. target-contract.json retains public named-identity index meanings only; no fitted private profile vectors or weights. Metadata and targets never enter learned inputs. Full-fit source checksums remain original evidence.\n\nRestore the lossless .zst payloads with tools/dataset-storage, then run sha256sum -c SHA256SUMS. Reproduce with publish_retained_encoder_train REPO STAGE LARGE_BATCH NEW_DATASETS_CHILD. The publisher pins both original batches and the exact fitted Train/target contract, checks ancestral rights and source bytes, validates replacement spans and reconstructs every training target before writing a new output. Parent publications remain unchanged.\n";
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 4, "REPO STAGE LARGE_BATCH NEW_DATASETS_CHILD");
    println!(
        "{}",
        run(
            Path::new(&a[0]),
            Path::new(&a[1]),
            Path::new(&a[2]),
            Path::new(&a[3])
        )?
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn role_index_and_known_masks_cannot_change() -> Result<()> {
        let c = json!({"human_profiles":["writer"],"ai_profiles":["generator"]});
        let r = json!({"roles":{"human":{"identity":"writer","present":true,"known":true},"ai":{"identity":"generator","present":false,"known":true}},"targets":{"human":0,"ai":null}});
        targets_match(&r, &c)?;
        let mut bad = r.clone();
        bad["targets"]["ai"] = 0.into();
        assert!(targets_match(&bad, &c).is_err());
        let mut bad = r;
        bad["roles"]["human"]["known"] = false.into();
        assert!(targets_match(&bad, &c).is_err());
        Ok(())
    }
    #[test]
    fn external_evidence_is_not_a_public_filesystem_locator() -> Result<()> {
        let i = json!({"inputs":[{"path":"/private/protected/sample.json","sha256":"a".repeat(64)},{"path":"/private/run/known_model_1/binding.json","sha256":"b".repeat(64)}]});
        let (out, map) = evidence(&i, Path::new("/private/run"))?;
        public_safety(&out)?;
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("known_model_1/binding.json"));
        Ok(())
    }
}
