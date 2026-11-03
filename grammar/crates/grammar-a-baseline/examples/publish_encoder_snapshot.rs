//! Publish the completed Stage3 cohort without changing fitting/evaluation data.
//! REPO STAGE GENERATOR_EVIDENCE_COHORT NEW_DATASETS_CHILD
//! Exact permitted text/spans survive; provider bodies, machine paths and models do not.
#![recursion_limit = "256"]
#[allow(dead_code, unused_attributes)]
#[path = "publish_balanced_snapshot.rs"]
mod prior;
#[allow(dead_code)]
#[path = "freeze_last_block_test.rs"]
mod seal;
use anyhow::{Context, Result, ensure};
use prior::{array, compact_sha, digest, flat, public_safety, s, sha};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path},
};
const NON_TEST: &str = "67bcdb4868b355e18b3449be7ecff78d32fb3ecbabfc9cba35fbf03a8ac2e42e";
const TEST: &str = "12aef4c1b9db59f4959566733759ffb64bd608126f96afb99ef0971b244231f3";
const REGISTRY: &str = "dc1eae8acbb64a7d998c0eb1dd20f5474ca5db01135ba387410d0e0225f81fb1";
const LICENSE_INDEX: &str = "46489865139768e5d3a4669c02475663d63e7d1c9dfe43a258e2d5694587ba5a";
type Files = BTreeMap<String, Vec<u8>>;
fn bytes(root: &Path, relative: &str, expected: Option<&str>) -> Result<Vec<u8>> {
    let mut path = root.to_owned();
    for part in Path::new(relative).components() {
        ensure!(
            matches!(part, Component::Normal(_)),
            "unsafe relative source path"
        );
        path.push(part);
        ensure!(
            !fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "symlink source"
        );
    }
    ensure!(
        fs::metadata(&path)?.is_file() && fs::metadata(&path)?.len() <= 64 * 1024 * 1024,
        "source size/type"
    );
    let b = fs::read(&path)?;
    if let Some(h) = expected {
        ensure!(sha(&b) == h, "source checksum differs: {relative}");
    }
    Ok(b)
}
fn load(root: &Path, name: &str, expected: Option<&str>) -> Result<Value> {
    Ok(serde_json::from_slice(&bytes(root, name, expected)?)?)
}
fn json_file(files: &mut Files, name: &str, v: &Value) -> Result<()> {
    public_safety(v)?;
    let mut b = serde_json::to_vec_pretty(v)?;
    b.push(b'\n');
    ensure!(files.insert(name.into(), b).is_none(), "duplicate output");
    Ok(())
}
fn source(row: &Value) -> Result<Value> {
    let mut r = row.clone();
    let stratum = r["source"]["record"]["selection_row"]
        .as_object_mut()
        .context("selection")?
        .remove("stratum")
        .context("source stratum")?;
    ensure!(
        stratum.is_string() && stratum == row["metadata"]["acquisition_stratum"],
        "source stratum changed"
    );
    let mut out = prior::project_source_scoped(&r, row["frozen_split"] == "test")?;
    out["record"]["selection_row"]["stratum"] = stratum;
    Ok(out)
}
fn config(v: &Value) -> Result<Value> {
    let mut copy = v.clone();
    let parent = copy
        .as_object_mut()
        .context("config")?
        .remove("parent_config_sha256");
    let mut out = prior::project_config(&copy)?;
    if let Some(p) = parent {
        out["parent_config_sha256"] = p;
        digest(&out, "parent_config_sha256")?;
    }
    Ok(out)
}
fn evidence(root: &Path, m: &Value, key: &str) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for e in array(m, key)? {
        let original = s(e, "path")?;
        // Protected overlap populations are evidence, not publication inputs.
        // Retain their hashes with opaque locator IDs without opening them.
        let external;
        let path = match Path::new(original).strip_prefix(root) {
            Ok(path) => path,
            Err(_) => {
                external = format!("external-overlap-evidence/{}", sha(original.as_bytes()));
                Path::new(&external)
            }
        };
        ensure!(
            path.components().all(|p| matches!(p, Component::Normal(_))),
            "unsafe evidence path"
        );
        let name = path.to_str().context("UTF8 path")?.to_owned();
        digest(e, "sha256")?;
        if let Some(old) = out.insert(name, s(e, "sha256")?.to_owned()) {
            ensure!(old == e["sha256"], "conflicting evidence hash");
        }
    }
    Ok(out)
}
fn generation(
    row: &Value,
    stage: &Path,
    scope: &str,
    registry: &Value,
    ev: &BTreeMap<String, String>,
) -> Result<Value> {
    let g = &row["generation"];
    let b = &g["binding"];
    let c = &b["generator_config"];
    let test = scope == "final-test-v1";
    ensure!(
        b["schema"] == "slop_ninja_controlled_generation_v4"
            && b["test_opened"] == test
            && b["recipe"] == "encoder_stage3_v1"
            && b["test_policy"]
                == if test {
                    "frozen_last_block_Test_v1; no fitting or reselection"
                } else {
                    "Test unavailable in this recipe; separate frozen selection route required later"
                }
            && b["thinking"] == false
            && c["allowed_splits"]
                .as_array()
                .context("allowed splits")?
                .contains(&row["frozen_split"])
            && g["generator"] == row["ai_identity"]
            && g["generator"] == c["identity"],
        "generation scope/identity"
    );
    let model = array(registry, "generators")?
        .iter()
        .find(|r| r["sha256"] == c["sha256"])
        .context("unreviewed generator")?;
    for (a, z) in [
        ("repo", "quantized_repo"),
        ("revision", "revision"),
        ("filename", "filename"),
        ("size_bytes", "size_bytes"),
    ] {
        ensure!(c[a] == model[z], "quantizer identity mismatch");
    }
    let slot = s(model, "role")?;
    let prefix = if test {
        format!("final-test-v1/generation-v1/{slot}")
    } else {
        format!("generation-v1/{slot}")
    };
    let cp = if test {
        format!("final-test-v1/configs/{slot}.json")
    } else {
        format!("generation-v1/configs/{slot}.json")
    };
    let stored = load(stage, &cp, Some(s(b, "generator_config_sha256")?))?;
    ensure!(stored == *c, "stored generator differs");
    let bp = format!("{prefix}/binding.json");
    ensure!(
        load(stage, &bp, Some(ev.get(&bp).context("unbound binding")?))? == *b,
        "stored binding differs"
    );
    let mut out = flat(
        g,
        &[
            "generator",
            "position",
            "response_sha256",
            "mode",
            "attempt",
            "response_format_policy",
        ],
        &["binding", "request", "response_path"],
    )?;
    let mut binding = flat(
        b,
        &[
            "schema",
            "input_sha256",
            "generator",
            "generator_config_sha256",
            "producer_sha256",
            "share_word_policy",
            "requested_span_percentages",
            "span_policy",
            "seed",
            "temperature",
            "top_p",
            "max_tokens",
            "thinking",
            "input_schema",
            "failures",
            "test_opened",
            "recipe",
            "rewrite_policy",
            "scratch_implementation_sha256",
            "scratch_policy",
            "test_policy",
        ],
        &["generator_config", "endpoint", "test_binding"],
    )?;
    binding["generator_config"] = config(c)?;
    binding["original_binding_sha256"] = json!(compact_sha(b)?);
    if test {
        binding["test_binding"] = flat(
            &b["test_binding"],
            &[
                "admission_manifest_sha256",
                "selection_receipt_sha256",
                "source_manifest_sha256",
            ],
            &[],
        )?;
    }
    out["binding"] = binding;
    let request = &g["request"];
    out["request"] = flat(
        request,
        &[
            "model",
            "temperature",
            "top_p",
            "seed",
            "max_tokens",
            "stream",
            "reasoning_format",
        ],
        &["messages", "chat_template_kwargs"],
    )?;
    ensure!(request["model"] == c["alias"], "request alias mismatch");
    for key in ["temperature", "top_p", "seed", "max_tokens"] {
        ensure!(request[key] == b[key], "sampler mismatch");
    }
    let messages = array(request, "messages")?
        .iter()
        .map(|m| {
            ensure!(m["role"] == "user", "unexpected message role");
            flat(m, &["role", "content"], &[])
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!messages.is_empty(), "empty generation prompts");
    out["request"]["messages"] = json!(messages);
    out["request"]["chat_template_kwargs"] =
        flat(&request["chat_template_kwargs"], &["enable_thinking"], &[])?;
    out["request_sha256"] = json!(compact_sha(request)?);
    let id = s(row, "id")?;
    ensure!(
        id.starts_with("local-") && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "unsafe generation ID"
    );
    let prefix = format!("{prefix}/{id}/");
    let context_name = format!("{prefix}context.json");
    let context = load(
        stage,
        &context_name,
        Some(ev.get(&context_name).context("unbound context")?),
    )?;
    out["context"] = flat(
        &context,
        &[
            "request_sha256",
            "rendered_prompt_sha256",
            "prompt_tokens",
            "max_output_tokens",
            "context_capacity",
            "fits",
        ],
        &[],
    )?;
    ensure!(
        context["request_sha256"] == out["request_sha256"] && context["fits"] == true,
        "context mismatch"
    );
    out["context_receipt_sha256"] = json!(ev[&context_name]);
    let response_name = format!(
        "{prefix}attempt-{}.json",
        g["attempt"].as_u64().context("attempt")?
    );
    ensure!(
        ev.get(&response_name)
            .is_some_and(|h| g["response_sha256"] == *h),
        "response not bound"
    );
    out["attempt_history"] = json!(
        ev.iter()
            .filter_map(|(p, h)| {
                let n = p.strip_prefix(&prefix)?;
                (n.starts_with("attempt-") && !n.contains('/'))
                    .then(|| json!({"receipt":n,"sha256":h}))
            })
            .collect::<Vec<_>>()
    );
    out["quantizer_and_original_license"] = prior::registry_model(model)?;
    out["raw_invocations_retained_privately"] = json!(true);
    Ok(out)
}
fn project(
    row: &Value,
    stage: &Path,
    scope: &str,
    registry: &Value,
    ev: &BTreeMap<String, String>,
    line_hash: &str,
) -> Result<Value> {
    ensure!(
        row["schema"] == "slop_ninja_a_supervised_record_v1"
            && row["text_sha256"] == sha(s(row, "text")?.as_bytes())
            && row["documented_human_only"] == false
            && row["exposed_to_grammar"].is_boolean(),
        "record scope/hash/proxy mismatch"
    );
    prior::validate_targets(row)?;
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
            "share_evidence",
            "source",
            "rights",
            "generation",
            "metadata",
        ],
    )?;
    out["share_evidence"] = row["share_evidence"].clone();
    out["source"] = source(row)?;
    out["rights"] = prior::public_rights(&row["rights"])?;
    out["metadata"] = flat(
        &row["metadata"],
        &[
            "acquisition_stratum",
            "normalization_or_split_metadata_is_not_learned_input",
            "source_collection",
        ],
        &[],
    )?;
    if !row["generation"].is_null() {
        out["generation"] = generation(row, stage, scope, registry, ev)?;
    }
    out["publication"] = json!({"original_line_sha256":line_hash,"line_hash_basis":"exact JSONL line including newline",
        "original_row_sha256":compact_sha(row)?,"public_exposed_population":true,"future_hidden_test":false,
        "projection":"field-allowlisted metadata; exact text, spans, targets and original history unchanged"});
    public_safety(&out)?;
    Ok(out)
}
fn completed_test(stage: &Path) -> Result<String> {
    let sp = stage.join("final-test-v1/seal.json");
    let seal_hash = seal::sha(&sp)?;
    // Publication binds the already evaluated seal and reports. It does not
    // reload or requalify large model and generator weights.
    ensure!(
        seal_hash == "bd52f22386b367c69fc7136e03410fe7a5c24bf5e3d23e8e12e8cfe79fca8673",
        "Test seal changed"
    );
    let em = load(stage, "final-test-v1/evaluation-v2/manifest.json", None)?;
    let nm = load(stage, "final-test-v1/numeric-v1/manifest.json", None)?;
    ensure!(
        em["schema"] == "slop_ninja_last_block_test_evaluation_v1"
            && em["complete"] == true
            && em["selection_receipt_sha256"] == seal_hash
            && em["dataset_manifest_sha256"]
                == seal::sha(&stage.join("final-test-v1/numeric-v1/manifest.json"))?
            && nm["source_dataset"]["sha256"] == TEST
            && nm["test_used_for_selection"] == false,
        "completed frozen Test required"
    );
    let mut seen = BTreeSet::new();
    for r in array(&em, "reports")? {
        let condition = s(r, "condition")?;
        let seed = r["seed"].as_u64().context("seed")?;
        ensure!(
            ["adapt_last_block", "frozen_last_block"].contains(&condition)
                && [17, 29, 41].contains(&seed),
            "unexpected Test run"
        );
        let name = format!("{condition}-seed-{seed}");
        ensure!(seen.insert(name.clone()), "duplicate Test run");
        for (file, key) in [
            ("test.json", "report_sha256"),
            ("test-predictions.json", "predictions_sha256"),
        ] {
            bytes(
                stage,
                &format!("final-test-v1/evaluation-v2/{name}/{file}"),
                Some(s(r, key)?),
            )?;
        }
    }
    ensure!(seen.len() == 6, "incomplete Test runs");
    Ok(seal_hash)
}
fn run(repo: &Path, stage: &Path, licenses: &Path, out: &Path) -> Result<Value> {
    let repo = repo.canonicalize()?;
    let stage = stage.canonicalize()?;
    let licenses = licenses.canonicalize()?;
    ensure!(
        !out.exists()
            && out.parent().context("output parent")?.canonicalize()? == repo.join("datasets"),
        "new direct datasets child required"
    );
    let seal_hash = completed_test(&stage)?;
    let non = load(&stage, "dataset-v1/manifest.json", Some(NON_TEST))?;
    let test = load(&stage, "final-test-v1/dataset-v1/manifest.json", Some(TEST))?;
    ensure!(
        non["complete"] == true
            && non["test_text_opened"] == false
            && test["complete"] == true
            && test["selection_receipt_sha256"] == seal_hash,
        "pinned dataset scope"
    );
    let registry = load(&licenses, "generator-identities.json", Some(REGISTRY))?;
    let license_index = bytes(&licenses, "generator-evidence.sha256", Some(LICENSE_INDEX))?;
    let license_hashes = std::str::from_utf8(&license_index)?
        .lines()
        .map(|l| {
            let (h, p) = l.split_once("  ").context("license index")?;
            Ok((p.to_owned(), h.to_owned()))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut ev = evidence(&stage, &non, "inputs")?;
    for (k, v) in evidence(&stage, &test, "sources")? {
        if let Some(old) = ev.insert(k, v.clone()) {
            ensure!(old == v, "evidence collision");
        }
    }
    let rm = load(
        &stage,
        "final-test-v1/dataset-v1/reference-manifest.json",
        None,
    )?;
    ensure!(
        rm["provenance"]["query_manifest_sha256"] == TEST,
        "reference manifest differs"
    );
    let mut rows = BTreeMap::new();
    let mut line_hashes = BTreeMap::new();
    let mut scopes = BTreeMap::new();
    for (name, hash, scope) in [
        (
            "dataset-v1/records.jsonl",
            s(&non, "records_sha256")?,
            "non-test",
        ),
        (
            "final-test-v1/dataset-v1/records.jsonl",
            s(&test, "records_sha256")?,
            "final-test-v1",
        ),
        (
            "final-test-v1/dataset-v1/references.jsonl",
            s(&rm, "records_sha256")?,
            "final-test-v1",
        ),
    ] {
        for line in bytes(&stage, name, Some(hash))?.split_inclusive(|b| *b == b'\n') {
            ensure!(line.last() == Some(&b'\n'), "newline required");
            let r: Value = serde_json::from_slice(line)?;
            let id = s(&r, "id")?.to_owned();
            ensure!(
                (scope == "final-test-v1") == (r["frozen_split"] == "test"),
                "publication partition differs"
            );
            ensure!(
                rows.insert(id.clone(), r).is_none(),
                "duplicate publication ID"
            );
            line_hashes.insert(id.clone(), sha(line));
            scopes.insert(id, scope);
        }
    }
    ensure!(
        rows.len() == 2268,
        "pinned accepted population count differs"
    );
    prior::validate_population_scoped(&rows, true)?;
    let mut files = Files::new();
    let mut splits = BTreeMap::<String, usize>::new();
    let mut groups = BTreeMap::new();
    let mut original_bindings = Vec::new();
    let mut catalogs = BTreeMap::new();
    for (id, r) in &rows {
        let projected = project(r, &stage, scopes[id], &registry, &ev, &line_hashes[id])
            .with_context(|| format!("publication row {id}"))?;
        let file = if r["purpose"] == "reference" {
            "references.jsonl".into()
        } else {
            format!("{}.jsonl", s(r, "frozen_split")?)
        };
        let b = files.entry(file.clone()).or_default();
        serde_json::to_writer(&mut *b, &projected)?;
        b.push(b'\n');
        *splits.entry(file).or_default() += 1;
        groups.insert(
            s(r, "source_group")?.to_owned(),
            json!({"split":r["frozen_split"],"purpose":r["purpose"]}),
        );
        original_bindings.push(json!({"id":id,"text_sha256":r["text_sha256"],"original_line_sha256":line_hashes[id],"original_row_sha256":compact_sha(r)?}));
        let catalog = &r["source"]["record"]["catalog_evidence"];
        let pg = r["source"]["record"]["pg_id"].as_u64().context("PG ID")?;
        let relative = format!("metadata/pg{pg}.rdf");
        ensure!(catalog["path"] == relative, "catalog locator differs");
        let expected = s(catalog, "sha256")?;
        if let Some(old) = catalogs.insert(pg, expected.to_owned()) {
            ensure!(old == expected, "catalog changed");
        } else {
            let b = bytes(
                &stage,
                &format!("source-metadata-v2/{relative}"),
                Some(expected),
            )?;
            ensure!(
                std::str::from_utf8(&b)?.contains("Public domain in the USA."),
                "catalog public-domain evidence absent"
            );
            files.insert(format!("sources/pg-{pg}.rdf"), b);
        }
    }
    for name in [
        "qwen-base.LICENSE",
        "phi-base.LICENSE",
        "gemma-terms.html",
        "gemma-prohibited-use.html",
        "mistral-base.README.md",
    ] {
        let path = format!("generator-evidence/{name}");
        let b = bytes(
            &licenses,
            &path,
            Some(license_hashes.get(&path).context("unbound license")?),
        )?;
        ensure!(
            !std::str::from_utf8(&b)?.contains("~/"),
            "private license locator"
        );
        files.insert(format!("licenses/{name}"), b);
    }
    let models = array(&registry, "generators")?
        .iter()
        .map(prior::registry_model)
        .collect::<Result<Vec<_>>>()?;
    json_file(
        &mut files,
        "generators.json",
        &json!({"schema":"slop_ninja_stage3_published_generators_v1","models":models,
        "original_registry_sha256":REGISTRY,"license_evidence_index_sha256":LICENSE_INDEX,
        "runtime":flat(&registry["runtime"],&["name","tag","commit","variant"],&["directory","runtime_smoke_test_performed"])? ,
        "model_weights_included":false,"configuration_and_runtime_hashes":"per generated row; original model metadata revision is not a proven quantizer input"}),
    )?;
    let failures=array(&non,"generation_reports")?.iter().map(|r| Ok(json!({"slot":r["slot"],"requested":r["requested"],"accepted":r["accepted"],"failed":r["failed"],
        "failures":array(r,"failures")?.iter().map(|f| flat(f,&["id","mode","parent_id","reason","requested_span_percent"],&[])).collect::<Result<Vec<_>>>()?}))).collect::<Result<Vec<_>>>()?;
    json_file(
        &mut files,
        "failures.json",
        &json!({"non_test_generation":failures,"Test_requested_generation":600,"Test_generation_succeeded":600,
        "Test_assembly_exclusions":test["excluded"],"raw_evidence_retained":true}),
    )?;
    json_file(
        &mut files,
        "evidence-index.json",
        &json!({"schema":"slop_ninja_stage3_published_evidence_v1","rows":original_bindings,
        "inputs":ev.iter().map(|(p,h)|json!({"logical_archive_path":p,"sha256":h})).collect::<Vec<_>>(),
        "local_roots_and_raw_invocation_bodies_omitted":true}),
    )?;
    let manifest = json!({"schema":"slop_ninja_encoder_publication_v1","complete":true,"records":rows.len(),"source_families":groups.len(),
        "partitions":splits,"source_work_partitions":groups,"original_non_test_manifest_sha256":NON_TEST,
        "original_test_manifest_sha256":TEST,"selection_receipt_sha256":seal_hash,"test_evaluated_before_publication":true,
        "all_published_populations_exposed":true,"future_hidden_benchmark":false,"fitting_or_calibration_performed":false,
        "original_fitting_inputs_changed":false,"supervision":"history and measured replacement-span final-word share; no detector labels",
        "files_sha256":files.iter().map(|(p,b)|(p.clone(),sha(b))).collect::<BTreeMap<_,_>>(),
        "implementation_sha256":sha(include_bytes!("publish_encoder_snapshot.rs")),"shared_validation_sha256":sha(include_bytes!("publish_balanced_snapshot.rs"))});
    json_file(&mut files, "manifest.json", &manifest)?;
    files.insert("README.md".into(), README.as_bytes().to_vec());
    files.insert("NOTICE.md".into(), NOTICE.as_bytes().to_vec());
    let checksums = files
        .iter()
        .map(|(p, b)| format!("{}  {p}\n", sha(b)))
        .collect::<String>();
    files.insert("SHA256SUMS".into(), checksums.into_bytes());
    // No output becomes visible until every row and metadata projection passed.
    fs::create_dir(out)?;
    for (name, b) in files {
        let p = out.join(name);
        fs::create_dir_all(p.parent().unwrap())?;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(p)?;
        f.write_all(&b)?;
        f.sync_all()?;
    }
    Ok(
        json!({"records":rows.len(),"source_families":groups.len(),"partitions":splits,"output":out}),
    )
}
const README: &str = "# Slop Ninja encoder-adaptation cohort\n\nThis rights-checked snapshot preserves 2,268 records from 98 source works: 528 new Train queries, 439 Development queries, 637 Calibration queries, 636 consumed Test queries and 28 independent references. The experiment also reused 3,629 earlier Train records; this snapshot is the new cohort, not the entire fitted population. References retain their own frozen split and must not enter fitting. Every work and descendant stays in one partition.\n\nHistorical author text is a US public-domain proxy, not an authenticated contemporary human-only workflow. Exact source text, final text, original/replacement spans, final-word counts, production history, authors, generator/quantizer/runtime identities, prompts, sampler settings, source dates and exposure caveats survive publication. A complete rewrite remains mixed history at unit replacement share. Requested rewrite size is not the share target. Four bounded generation failures and four Test duplicate/no-change exclusions remain explicit.\n\nAll Test evaluation and model selection were complete before this publication. These populations are exposed research/regression evidence, not future hidden benchmarks. The publication changes no original fitting bytes, predictions, normalization or calibration. Metadata is provenance, not learned model input. No provider labels are substituted for truth.\n\nThe native Rust publisher validates pinned datasets, completed sealed Test runs, exact text, parentage, source-family/reference splits, source rights, generation bindings, UTF-8 replacement spans and a field allowlist. Machine paths, endpoints, private profiles, weights and raw provider logs are excluded. Catalog RDF and generator license evidence accompany this snapshot. Raw generation and paid detector receipts remain in the permanent ignored archive.\n\nPayloads are lossless zstd. Restore with `tools/dataset-storage`, then run `sha256sum -c SHA256SUMS`. Checksums bind decoded bytes. Reproduce the checked projection with `publish_encoder_snapshot REPO STAGE GENERATOR_EVIDENCE_COHORT NEW_DATASETS_CHILD`, then pack using the storage utility. See `docs/a-encoder-adaptation.md` for model results, qualified scope and recovery.\n";
const NOTICE: &str = "# Source and generator notices\n\nAuthor excerpts retain per-work Project Gutenberg RDF establishing public-domain status in the USA, not worldwide rights. The excerpts contain no PG branding; underlying rights, attribution, dates and exact changes remain recorded. These proxies do not document modern human-only production.\n\nQwen3.6-35B-A3B original model: Apache-2.0. Phi-4-mini-instruct: MIT. Gemma-3-4b-it: custom Gemma Terms of Use and Prohibited Use Policy, reproduced in licenses/. Gemma is provided under and subject to the Gemma Terms of Use at https://ai.google.dev/gemma/terms. Mistral-7B-Instruct-v0.3: Apache-2.0 as recorded in the retained original model card. No model weights are distributed. Original metadata revisions do not prove quantization input revisions; actual used quantizers, weights, runtime and sampler are pinned separately. Preserve per-row provenance and generator restrictions.\n\nNew generated text and curation: CC BY-SA 4.0 to the extent rights exist, subject to underlying rights and generator terms. Attribution: Slop Ninja and the recorded source authors. See https://creativecommons.org/licenses/by-sa/4.0/ and baseline/detector/SHARE_ALIKE_POLICY.md. The repository code license does not replace dataset terms.\n";
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 4,
        "REPO STAGE GENERATOR_EVIDENCE_COHORT NEW_DATASETS_CHILD"
    );
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
    fn publication_never_follows_symlinks_or_traversal() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("safe"), b"safe").unwrap();
        assert!(bytes(d.path(), "../safe", None).is_err());
        assert!(bytes(d.path(), "safe", Some("wrong")).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.path().join("safe"), d.path().join("link")).unwrap();
            assert!(bytes(d.path(), "link", None).is_err());
        }
    }
    #[test]
    fn metadata_projection_rejects_unrecognized_and_private_fields() {
        let v = json!({"source_collection":"synthetic","unreviewed_rights_term":"no distribution"});
        assert!(flat(&v, &["source_collection"], &[]).is_err());
        assert!(public_safety(&json!({"evidence":"/home/private/file"})).is_err());
    }
    #[test]
    fn external_overlap_evidence_keeps_hashes_without_opening_or_revealing_locators() {
        let v = json!({"inputs":[{"path":"/nonexistent/private/protected.jsonl","sha256":"a".repeat(64)}]});
        let e = evidence(Path::new("/task"), &v, "inputs").unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e.values().next().unwrap(), &"a".repeat(64));
        assert!(
            e.keys()
                .next()
                .unwrap()
                .starts_with("external-overlap-evidence/")
        );
        public_safety(&json!(e)).unwrap();
    }
}
