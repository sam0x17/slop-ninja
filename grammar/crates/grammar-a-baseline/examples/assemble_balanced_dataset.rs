//! Assemble only the frozen 2026-10-26 non-Test balanced cohort.
//! OUT SOURCE_MANIFEST QUERY_MANIFEST HUMANS.jsonl CONFIG_DIR GEN_KNOWN1 GEN_KNOWN2 GEN_CAL
//! Reads completed generation evidence, never book paths, reference files, Test
//! prose, model weights or network services. A complete terminal-failure report
//! is allowed; an unaccounted/missing task is not. Requested denominators survive.
#![recursion_limit = "256"]
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

// Reuse the production numeric-preparation supervision contract, including its
// realized-share checker, without importing parsing/feature extraction machinery.
#[allow(dead_code)]
#[path = "prepare_supervised_forest/supervision.rs"]
mod supervision;
const PARTS: [&str; 3] = ["train", "development", "calibration"];
const TSV: &str = include_str!("../../../../docs/a-balanced-sources-20261026.tsv");
const TSV_SHA: &str = "3271a46bf0b1dd70a2429ee5a9ce3002fba8c8235f7a629219162c46471f903f";
const SOURCE_SHA: &str = "a6a64f4128e3516a41ef994a176c39bf660d4bc0cfb63a07d853b26c6e81050e";
const HUMANS_SHA: &str = "8de9f9a3d67a1f05f8d6513a95ae5b460288c5c9ffd68add8cba13537c29bfd7";
pub(crate) const SLOTS: [&str; 3] = ["known_model_1", "known_model_2", "heldout_CAL_only"];
pub(crate) const MODELS: [&str; 3] = [
    "Qwen/Qwen3.6-35B-A3B:649d7508507b84638732c4f52c24c8b15843c6dca2f3ff793ae07c14a67ebbb3",
    "microsoft/Phi-4-mini-instruct:01999f17c39cc3074afae5e9c539bc82d45f2dd7faa3917c66cbef76fce8c0c2",
    "google/gemma-3-4b-it:4996030242583a40aa151ff93f49ed787ac8c25e4120c3ae4588b2e2a7d1ae94",
];
const CONFIG_SHAS: [&str; 3] = [
    "d5171bc6eeb84b3ea5026cb7ebb2d9964fdabb3fe1ac5043b7f8d28872c51add",
    "2ac8021370b8238f61f1bfc89920a17bddfe006df93cb99d33b66b672d3b776a",
    "3124f9bfd20c95081239c4b39b7a8f231a213aa17074deba02af18cb70f6890b",
];
pub(crate) fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
pub(crate) fn compact_sha(v: &Value) -> Result<String> {
    Ok(sha(&serde_json::to_vec(v)?))
}
pub(crate) fn str_at<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("missing {k}"))
}
pub(crate) fn counter(v: &Value, k: &str) -> Result<u64> {
    v[k].as_u64().with_context(|| format!("counter {k}"))
}
pub(crate) fn number(v: &Value) -> Result<f64> {
    let n = v.as_f64().context("number")?;
    ensure!(n.is_finite() && n >= 0., "invalid number");
    Ok(n)
}
pub(crate) fn digest_at<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    let s = str_at(v, k)?;
    ensure!(
        s.len() == 64
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "invalid digest {k}"
    );
    Ok(s)
}
pub(crate) fn array<'a>(v: &'a Value, k: &str) -> Result<&'a Vec<Value>> {
    v[k].as_array().with_context(|| format!("array {k}"))
}
pub(crate) fn ids(v: &Value, k: &str) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for id in array(v, k)? {
        ensure!(
            out.insert(id.as_str().context("ID")?.to_owned()),
            "duplicate {k}"
        );
    }
    Ok(out)
}
pub(crate) fn bytes(p: &Path) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    File::open(p)
        .with_context(|| format!("open {}", p.display()))?
        .take(256 * 1024 * 1024 + 1)
        .read_to_end(&mut b)?;
    ensure!(b.len() <= 256 * 1024 * 1024, "input too large");
    Ok(b)
}
pub(crate) fn load(p: &Path, inputs: &mut Vec<Value>) -> Result<(Value, Vec<u8>)> {
    let b = bytes(p)?;
    let v = serde_json::from_slice(&b)?;
    inputs.push(json!({"path":p,"sha256":sha(&b)}));
    Ok((v, b))
}
pub(crate) fn records(b: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(b)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
pub(crate) fn unique_rows(rows: &[Value]) -> Result<BTreeMap<String, &Value>> {
    let mut map = BTreeMap::new();
    for r in rows {
        ensure!(
            map.insert(str_at(r, "id")?.to_owned(), r).is_none(),
            "duplicate record ID"
        );
    }
    Ok(map)
}
pub(crate) fn write_new(p: &Path, b: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new().create_new(true).write(true).open(p)?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(())
}
fn normalized_split(s: &str) -> &str {
    s.strip_suffix("_reference").unwrap_or(s)
}

#[derive(Clone)]
struct Work {
    pg: u64,
    author: String,
    title: String,
    catalog_author: String,
    group: String,
    assigned: String,
    purpose: String,
    enrollment: String,
    metadata_sha: String,
}
impl Work {
    fn split(&self) -> &str {
        normalized_split(&self.assigned)
    }
}
fn frozen_works() -> Result<Vec<Work>> {
    ensure!(
        sha(TSV.as_bytes()) == TSV_SHA,
        "compiled TSV differs from freeze"
    );
    let mut lines = TSV.lines();
    let header = lines
        .next()
        .context("header")?
        .split('\t')
        .collect::<Vec<_>>();
    let mut works = Vec::new();
    for line in lines {
        let fields = line.split('\t').collect::<Vec<_>>();
        ensure!(fields.len() == header.len(), "TSV width");
        let r: BTreeMap<_, _> = header.iter().copied().zip(fields).collect();
        works.push(Work {
            pg: r["pg_id"].parse()?,
            author: r["author_key"].into(),
            title: r["expected_title"].into(),
            catalog_author: r["catalog_author"].into(),
            group: r["underlying_work_key"].into(),
            assigned: r["split"].into(),
            purpose: r["role"].into(),
            enrollment: r["enrollment"].into(),
            metadata_sha: r["metadata_sha256"].into(),
        });
    }
    ensure!(works.len() == 72, "fixed work count");
    Ok(works)
}
fn roster(works: &[Work]) -> BTreeMap<String, Value> {
    works.iter().flat_map(|w|[20,60].map(move|fraction|(format!("author-pg{}-p{fraction}",w.pg),json!({"pg_id":w.pg,"frozen_split":w.split(),"purpose":w.purpose,"source_group":w.group,"fraction":fraction})))).collect()
}
fn check_roster(actual: &[Value], expected: &BTreeMap<String, Value>) -> Result<()> {
    let mut seen = BTreeSet::new();
    for r in actual {
        let id = str_at(r, "id")?;
        ensure!(seen.insert(id.to_owned()), "duplicate roster ID");
        let e = expected.get(id).context("unexpected roster ID")?;
        for k in ["frozen_split", "purpose", "source_group", "fraction"] {
            ensure!(r[k] == e[k], "roster field {k}: {id}");
        }
    }
    ensure!(
        seen == expected.keys().cloned().collect(),
        "roster population"
    );
    Ok(())
}
fn source_index<'a>(manifest: &'a Value, works: &[Work]) -> Result<BTreeMap<u64, &'a Value>> {
    ensure!(
        manifest["complete"] == true && manifest["selection_sha256"] == TSV_SHA,
        "source freeze/completeness"
    );
    ensure!(
        array(manifest, "sources")?.len() == 54 && array(manifest, "reserved_sources")?.len() == 18,
        "source counts"
    );
    let mut map = BTreeMap::new();
    for (r, reserved) in array(manifest, "sources")?
        .iter()
        .map(|r| (r, false))
        .chain(
            array(manifest, "reserved_sources")?
                .iter()
                .map(|r| (r, true)),
        )
    {
        let pg = counter(r, "pg_id")?;
        let w = works.iter().find(|w| w.pg == pg).context("unknown PGID")?;
        ensure!(
            map.insert(pg, r).is_none() && reserved == (w.split() == "test"),
            "source duplication/reservation"
        );
        ensure!(
            r["author_key"] == w.author
                && r["title"] == w.title
                && r["catalog_author"] == w.catalog_author
                && r["source_group"] == w.group
                && r["underlying_work_key"] == w.group
                && r["preassigned_partition"] == w.assigned
                && r["purpose"] == w.purpose
                && r["role"] == w.purpose
                && r["enrollment"] == w.enrollment,
            "source metadata differs from TSV"
        );
        ensure!(
            normalized_split(str_at(r, "split")?) == w.split()
                && r["catalog_evidence"]["sha256"] == w.metadata_sha,
            "source partition/catalog binding"
        );
        ensure!(
            r["rights"]["commercial_training"] == true
                && r["rights"]["external_evaluation"] == true
                && r["rights"]["catalog_statement"] == "Public domain in the USA.",
            "source rights"
        );
        if reserved {
            ensure!(
                r.get("text_path").is_none() && r.get("text_sha256").is_none(),
                "Test must remain metadata-only"
            );
        } else {
            digest_at(r, "text_sha256")?;
        }
    }
    ensure!(map.len() == 72, "missing source");
    Ok(map)
}
pub(crate) fn validate_human(r: &Value, source: &Value, expected: &Value, source_sha: &str) -> Result<()> {
    ensure!(
        r["schema"] == "slop_ninja_a_supervised_record_v1",
        "record schema"
    );
    for k in ["frozen_split", "purpose", "source_group"] {
        ensure!(r[k] == expected[k], "human {k} escaped roster");
    }
    ensure!(
        PARTS.contains(&str_at(r, "frozen_split")?),
        "Test forbidden"
    );
    ensure!(
        r["origin"] == "human_only"
            && r["human_role_present"] == true
            && r["ai_role_present"] == false
            && r["human_identity"] == source["author_key"]
            && r["ai_identity"].is_null()
            && r["parent_id"].is_null()
            && r.get("generation").is_none(),
        "human role/identity/history mismatch"
    );
    ensure!(
        r["evidence"] == "historical_public_domain_author_proxy"
            && r["documented_human_only"] == false,
        "historical proxy evidence"
    );
    ensure!(
        r["source"]["record"] == *source
            && r["source"]["source_manifest_sha256"] == source_sha
            && r["rights"] == source["rights"],
        "human source/rights binding"
    );
    let text = str_at(r, "text")?;
    ensure!(r["text_sha256"] == sha(text.as_bytes()), "human text hash");
    let span = array(&r["source"], "byte_span")?;
    ensure!(span.len() == 2, "excerpt span");
    let start = span[0].as_u64().context("span start")?;
    let end = span[1].as_u64().context("span end")?;
    ensure!(
        start < end && end <= counter(source, "text_bytes")? && end - start == text.len() as u64,
        "excerpt bounds"
    );
    ensure!(
        r["share_target"] == 0.0 && r["exposed_to_grammar"].is_boolean(),
        "human supervision/exposure"
    );
    supervision::Supervision::parse(r)?;
    Ok(())
}
fn check_slot(slot: usize, config: &Value, split: &str) -> Result<()> {
    ensure!(
        config["schema"] == "slop_ninja_local_generator_v1" && config["identity"] == MODELS[slot],
        "generator identity/slot mismatch"
    );
    let allowed = if slot == 2 {
        json!(["calibration"])
    } else {
        json!(["train", "development", "calibration", "test"])
    };
    ensure!(
        config["allowed_splits"] == allowed && config["rights"]["generation_permitted"] == true,
        "generator policy/rights"
    );
    ensure!(
        PARTS.contains(&split) && (slot != 2 || split == "calibration"),
        "generator leaked across partitions"
    );
    Ok(())
}
pub(crate) fn task_id(parent: &str, model: &str, percent: u64) -> String {
    let mode = if percent == 0 {
        "from_scratch"
    } else {
        "controlled_rewrite"
    };
    format!(
        "local-{}",
        &sha(format!("{parent}:{model}:{mode}:{percent}:span-v2").as_bytes())[..24]
    )
}
pub(crate) fn position(parent: &str, percent: u64) -> &'static str {
    ["start", "middle", "end"]
        [Sha256::digest(format!("{parent}:{percent}").as_bytes())[0] as usize % 3]
}
pub(crate) fn check_plan(plan: &Value, parent: &Value, binding: &Value, percent: u64, id: &str) -> Result<()> {
    check_plan_recipe(plan, parent, binding, percent, id, false)
}
pub(crate) fn check_encoder_plan(plan: &Value, parent: &Value, binding: &Value, percent: u64, id: &str) -> Result<()> {
    check_plan_recipe(plan, parent, binding, percent, id, true)
}
fn check_plan_recipe(plan: &Value, parent: &Value, binding: &Value, percent: u64, id: &str, encoder: bool) -> Result<()> {
    ensure!(
        plan["id"] == id
            && plan["parent_id"] == parent["id"]
            && plan["source_sha256"] == parent["text_sha256"]
            && plan["source_record_sha256"] == compact_sha(parent)?
            && plan["binding_sha256"] == compact_sha(binding)?,
        "plan source/binding/hash mismatch"
    );
    ensure!(
        plan["mode"]
            == if percent == 0 {
                "from_scratch"
            } else {
                "controlled_rewrite"
            }
            && plan["requested_source_span_percent"] == percent
            && plan["position"] == position(str_at(parent, "id")?, percent)
            && plan["sham_reassembly_exact"] == true,
        "plan task mismatch"
    );
    let span = array(plan, "source_byte_span")?;
    ensure!(span.len() == 2, "plan span");
    let s = span[0].as_u64().context("start")? as usize;
    let e = span[1].as_u64().context("end")? as usize;
    let t = str_at(parent, "text")?;
    ensure!(
        s <= e && e <= t.len() && t.is_char_boundary(s) && t.is_char_boundary(e),
        "plan byte bounds"
    );
    ensure!(
        match percent {
            0 => s == 0 && e == 0,
            100 => s == 0 && e == t.len(),
            20 | 60 if !encoder => s < e && (s > 0 || e < t.len()),
            10 | 40 | 80 if encoder => s < e && (s > 0 || e < t.len()),
            _ => false,
        },
        "plan requested span"
    );
    let req = &plan["request"];
    ensure!(
        req["model"] == binding["generator_config"]["alias"]
            && req["seed"] == 1729
            && req["temperature"] == 0.7
            && req["top_p"] == 0.9
            && req["max_tokens"] == 1024
            && req["stream"] == false
            && req["chat_template_kwargs"]["enable_thinking"] == false
            && req["reasoning_format"] == "none",
        "request settings"
    );
    let messages = array(req, "messages")?;
    ensure!(
        messages.len() == 1 && messages[0]["role"] == "user",
        "one user turn only"
    );
    let prompt = str_at(&messages[0], "content")?;
    if percent != 0 {
        let expected = if encoder {
            format!("Rewrite the following English passage in your own words while preserving its meaning, facts, perspective, register, and approximate length. Return only the replacement prose, retaining paragraph breaks where useful: no title, preface, analysis, quotation delimiters, or explanation.\n\n{}",&t[s..e])
        } else { format!(
                    "Rewrite the following English narrative passage in your own words, keeping its events, perspective, and approximate length. Return only the replacement prose in one paragraph: no title, preface, analysis, quotation delimiters, or explanation.\n\n{}",
                    &t[s..e]
                ) };
        ensure!(prompt == expected,
            "rewrite prompt/source mismatch"
        );
    }
    Ok(())
}
#[cfg(test)]
fn validate_descendant(
    r: &Value,
    parent: &Value,
    config: &Value,
    binding: &Value,
    plan: &Value,
    slot: usize,
    percent: u64,
) -> Result<()> {
    validate_descendant_with_slot(r, parent, config, binding, plan, slot, percent, check_slot)
}
pub(crate) fn validate_descendant_with_slot(
    r: &Value, parent: &Value, config: &Value, binding: &Value, plan: &Value,
    slot: usize, percent: u64, slot_check: fn(usize, &Value, &str) -> Result<()>,
) -> Result<()> {
    slot_check(slot, config, str_at(r, "frozen_split")?)?;
    ensure!(
        parent["purpose"] == "query" && r["purpose"] == "query",
        "reference generation forbidden"
    );
    for k in [
        "schema",
        "source_group",
        "frozen_split",
        "source",
        "rights",
        "exposed_to_grammar",
        "grammar_exposure",
        "documented_human_only",
    ] {
        ensure!(r[k] == parent[k], "descendant changed inherited {k}");
    }
    ensure!(
        r["parent_id"] == parent["id"]
            && r["id"] == plan["id"]
            && r["ai_identity"] == config["identity"]
            && r["ai_role_present"] == true,
        "descendant lineage/AI role"
    );
    ensure!(
        r["human_role_present"] == (percent != 0)
            && if percent == 0 {
                r["human_identity"].is_null()
            } else {
                r["human_identity"] == parent["human_identity"]
            },
        "descendant human role/identity"
    );
    ensure!(
        r["origin"] == if percent == 0 { "model_only" } else { "mixed" },
        "descendant origin"
    );
    let g = &r["generation"];
    ensure!(
        g["generator"] == config["identity"]
            && g["binding"] == *binding
            && g["request"] == plan["request"]
            && g["position"] == plan["position"]
            && g["mode"] == plan["mode"],
        "generation/plan binding"
    );
    let share = &r["share_evidence"];
    ensure!(
        share["schema"] == "slop_ninja_realized_span_share_v1"
            && share["source_text"] == parent["text"]
            && share["source_sha256"] == parent["text_sha256"]
            && share["source_byte_span"] == plan["source_byte_span"]
            && share["requested_percent_not_target"] == percent
            && share["full_rewrite_is_mixed_history"] == (percent == 100),
        "share/parent/plan binding"
    );
    ensure!(
        r["text_sha256"] == sha(str_at(r, "text")?.as_bytes()) && r["share_target"].is_number(),
        "final text/share missing"
    );
    supervision::Supervision::parse(r)?;
    Ok(())
}
pub(crate) fn exact_outcomes(
    expected: &BTreeSet<String>,
    accepted: &BTreeSet<String>,
    failed: &BTreeSet<String>,
) -> Result<()> {
    ensure!(accepted.is_disjoint(failed), "accepted/failed overlap");
    ensure!(
        accepted.union(failed).cloned().collect::<BTreeSet<_>>() == *expected,
        "incomplete/unexpected generation tasks"
    );
    Ok(())
}
pub(crate) fn prose(s: &str) -> Result<&str> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("<think>") {
        let (thinking, body) = rest.split_once("</think>").context("thinking wrapper")?;
        ensure!(thinking.trim().is_empty(), "nonempty thinking");
        Ok(body.trim())
    } else {
        Ok(s)
    }
}

fn generator(
    dir: &Path,
    slot: usize,
    config: &Value,
    config_sha: &str,
    humans: &[Value],
    human_sha: &str,
    inputs: &mut Vec<Value>,
    prompts: &mut BTreeMap<(String, u64), Value>,
) -> Result<(Vec<Value>, Value)> {
    generator_for_roster(dir, slot, config, config_sha, humans, human_sha, inputs, prompts,
        &GenerationPolicy {
            requested_tasks: if slot == 2 { 128 } else { 352 },
            producer_sha256: &sha(include_bytes!("../../../../examples/a_balanced_generate.rs")),
            slot_check: check_slot,
            plan_check: check_plan,
        })
}

/// Only the caller-supplied roster/config policy varies. The legacy wrapper
/// retains its fixed denominator, producer and exact config/split gates.
pub(crate) struct GenerationPolicy<'a> {
    pub requested_tasks: usize,
    pub producer_sha256: &'a str,
    pub slot_check: fn(usize, &Value, &str) -> Result<()>,
    pub plan_check: fn(&Value, &Value, &Value, u64, &str) -> Result<()>,
}
pub(crate) fn generator_for_roster(
    dir: &Path, slot: usize, config: &Value, config_sha: &str,
    humans: &[Value], human_sha: &str, inputs: &mut Vec<Value>,
    prompts: &mut BTreeMap<(String, u64), Value>, policy: &GenerationPolicy<'_>,
) -> Result<(Vec<Value>, Value)> {
    generator_for_roster_recipe(dir, slot, config, config_sha, humans, human_sha, inputs, prompts, policy, false)
}
pub(crate) fn generator_for_roster_recipe(
    dir: &Path, slot: usize, config: &Value, config_sha: &str,
    humans: &[Value], human_sha: &str, inputs: &mut Vec<Value>,
    prompts: &mut BTreeMap<(String, u64), Value>, policy: &GenerationPolicy<'_>, encoder: bool,
) -> Result<(Vec<Value>, Value)> {
    generator_for_recipe_scope(dir, slot, config, config_sha, humans, human_sha,
        inputs, prompts, policy, encoder, None)
}

pub(crate) const TEST_MODELS: [&str; 3] = [MODELS[0], MODELS[1],
    "mistralai/Mistral-7B-Instruct-v0.3:1270d22c0fbb3d092fb725d4d96c457b7b687a5f5a715abe1e818da303e562b6"];
pub(crate) const TEST_SLOTS: [&str; 3] = ["known_model_1", "known_model_2", "heldout_Test_only"];

/// The caller first verifies the complete frozen seal and source admission.
/// This distinct route cannot be used by any non-Test assembly entry point.
pub(crate) fn generator_for_sealed_encoder_test(
    dir: &Path, slot: usize, config: &Value, config_sha: &str,
    humans: &[Value], human_sha: &str, inputs: &mut Vec<Value>,
    prompts: &mut BTreeMap<(String, u64), Value>, policy: &GenerationPolicy<'_>,
    test_binding: &Value,
) -> Result<(Vec<Value>, Value)> {
    for key in ["selection_receipt_sha256", "admission_manifest_sha256", "source_manifest_sha256"] {
        digest_at(test_binding, key)?;
    }
    ensure!(config["allowed_splits"] == json!(["test"])
        && humans.iter().all(|r| r["purpose"] == "query" && r["frozen_split"] == "test"),
        "sealed Test-only roster/config required");
    generator_for_recipe_scope(dir, slot, config, config_sha, humans, human_sha,
        inputs, prompts, policy, true, Some(test_binding))
}

fn check_generation_scope(report: &Value, binding: &Value, test: Option<&Value>) -> Result<()> {
    ensure!(report["test_opened"] == test.is_some() && binding["test_opened"] == test.is_some(),
        "generator Test scope mismatch");
    if let Some(expected) = test {
        ensure!(binding["test_binding"] == *expected
            && binding["recipe"] == "encoder_stage3_v1"
            && binding["test_policy"] == "frozen_last_block_Test_v1; no fitting or reselection",
            "sealed Test generation binding changed");
    } else {
        ensure!(binding.get("test_binding").is_none(), "Test binding on non-Test generation");
    }
    Ok(())
}

fn generator_for_recipe_scope(
    dir: &Path, slot: usize, config: &Value, config_sha: &str,
    humans: &[Value], human_sha: &str, inputs: &mut Vec<Value>,
    prompts: &mut BTreeMap<(String, u64), Value>, policy: &GenerationPolicy<'_>,
    encoder: bool, test: Option<&Value>,
) -> Result<(Vec<Value>, Value)> {
    ensure!(slot < MODELS.len(), "unknown generator slot");
    let model = if test.is_some() {TEST_MODELS[slot]} else {MODELS[slot]};
    let slot_name = if test.is_some() {TEST_SLOTS[slot]} else {SLOTS[slot]};
    // A report is published only at the end; absence/incomplete status aborts.
    let (report, _) = load(&dir.join("report.json"), inputs)?;
    ensure!(
        report["schema"] == "slop_ninja_controlled_generation_report_v2"
            && report["complete"] == true,
        "generator incomplete/Test report"
    );
    let (binding, _) = load(&dir.join("binding.json"), inputs)?;
    check_generation_scope(&report, &binding, test)?;
    ensure!(
        binding["schema"] == if encoder {"slop_ninja_controlled_generation_v4"} else {"slop_ninja_controlled_generation_v2"}
            && binding["input_sha256"] == human_sha
            && binding["generator"] == model
            && binding["generator_config"] == *config
            && binding["generator_config_sha256"] == config_sha,
        "generator input/config binding"
    );
    ensure!(
        binding["producer_sha256"] == policy.producer_sha256
            && binding["share_word_policy"] == "unicode_split_whitespace_v1"
            && binding["requested_span_percentages"] == if encoder {json!([10,40,80,100])} else {json!([20, 60, 100])}
            && binding["span_policy"] == "nearest_sentence_start_middle_end_by_source_digest_v2",
        "generation contract changed"
    );
    ensure!(
        report["input_sha256"] == human_sha && report["binding_sha256"] == compact_sha(&binding)?,
        "report binding"
    );
    let b = bytes(&dir.join("records.jsonl"))?;
    ensure!(
        report["records_sha256"] == sha(&b),
        "generator records hash"
    );
    inputs.push(json!({"path":dir.join("records.jsonl"),"sha256":sha(&b)}));
    let generated = records(&b)?;
    let accepted = unique_rows(&generated)?;
    ensure!(report["accepted"] == generated.len(), "accepted count");
    let mut expected = BTreeMap::new();
    for parent in humans
        .iter()
        .filter(|r| r["purpose"] == "query" && (test.is_some() || slot != 2 || r["frozen_split"] == "calibration"))
    {
        for &percent in if encoder {&[0,10,40,80,100][..]} else {&[0,20,60,100][..]} {
            let id = if encoder {
                let parent=str_at(parent,"id")?;
                let mode=if percent==0 {"from_scratch"} else {"controlled_rewrite"};
                format!("local-{}",&sha(format!("{parent}:{model}:{mode}:{percent}:span-v4-encoder-stage3").as_bytes())[..24])
            } else {task_id(str_at(parent, "id")?, MODELS[slot], percent)};
            ensure!(
                expected.insert(id, (parent, percent)).is_none(),
                "task ID collision"
            );
        }
    }
    ensure!(
        expected.len() == policy.requested_tasks,
        "fixed requested generation denominator"
    );
    let failures = array(&report, "failures")?;
    let failed = unique_rows(failures)?;
    exact_outcomes(
        &expected.keys().cloned().collect(),
        &accepted.keys().cloned().collect(),
        &failed.keys().cloned().collect(),
    )?;
    for (id, (parent, percent)) in &expected {
        let task = dir.join(id);
        let (plan, plan_bytes) = load(&task.join("plan.json"), inputs)?;
        (policy.plan_check)(&plan, parent, &binding, *percent, id)?;
        let key = (str_at(parent, "id")?.to_owned(), *percent);
        let mut semantic = plan["request"].clone();
        semantic.as_object_mut().context("request")?.remove("model");
        if let Some(previous) = prompts.insert(key, semantic.clone()) {
            ensure!(
                previous == semantic,
                "generators received different semantic requests"
            );
        }
        let (context, _) = load(&task.join("context.json"), inputs)?;
        let tokens = counter(&context, "prompt_tokens")?;
        ensure!(
            context["request_sha256"] == compact_sha(&plan["request"])?
                && context["max_output_tokens"] == 1024
                && context["context_capacity"] == 8192
                && context["fits"] == (tokens.checked_add(1024).context("token overflow")? < 8192),
            "context receipt mismatch"
        );
        let mut attempts = BTreeMap::new();
        let mut intents = 0;
        for n in 0..2 {
            let intent_path = task.join(format!("attempt-{n}.intent.json"));
            let response_path = task.join(format!("attempt-{n}.json"));
            if intent_path.exists() {
                let (intent, _) = load(&intent_path, inputs)?;
                ensure!(
                    intent["request"] == plan["request"]
                        && intent["plan_sha256"] == sha(&plan_bytes)
                        && intent["attempt"] == n,
                    "attempt intent mismatch"
                );
                intents += 1;
            } else {
                ensure!(!response_path.exists(), "response without intent");
            }
            if response_path.exists() {
                let (response, raw) = load(&response_path, inputs)?;
                attempts.insert(n, (response, sha(&raw)));
            }
        }
        if let Some(row) = accepted.get(id) {
            ensure!(context["fits"] == true, "accepted unsupported context");
            validate_descendant_with_slot(row, parent, config, &binding, &plan, slot, *percent, policy.slot_check)?;
            let (saved, raw) = load(&task.join("record.json"), inputs)?;
            let (evidence, _) = load(&task.join("record.sha256.json"), inputs)?;
            ensure!(
                saved == **row
                    && evidence["sha256"] == sha(&raw)
                    && evidence["plan_sha256"] == sha(&plan_bytes),
                "saved record/plan hash mismatch"
            );
            let n = counter(&row["generation"], "attempt")?;
            let (response, response_sha) = attempts.get(&n).context("accepted response absent")?;
            let response_path = task.join(format!("attempt-{n}.json")).canonicalize()?;
            ensure!(
                Path::new(str_at(&row["generation"], "response_path")?).canonicalize()?
                    == response_path
                    && row["generation"]["response_sha256"] == *response_sha,
                "response path/hash mismatch"
            );
            ensure!(
                response["status"] == 200
                    && serde_json::from_str::<Value>(str_at(response, "raw_body")?)?
                        == response["body"]
                    && response["body"]["choices"][0]["finish_reason"] == "stop",
                "response not accepted stop"
            );
            let replacement = prose(str_at(
                &response["body"]["choices"][0]["message"],
                "content",
            )?)?;
            ensure!(
                replacement.split_whitespace().count() >= 8
                    && !replacement.contains("<think>")
                    && row["share_evidence"]["replacement"] == replacement,
                "response/replacement mismatch"
            );
        } else {
            let failure = failed[id];
            ensure!(
                failure["parent_id"] == parent["id"],
                "failed parent mismatch"
            );
            match str_at(failure, "reason")? {
                "unsupported_context" => ensure!(
                    context["fits"] == false && failure["context"] == context && intents == 0,
                    "context failure receipt"
                ),
                "no_accepted_response_after_bounded_attempts" => ensure!(
                    context["fits"] == true
                        && intents == 2
                        && failure["mode"] == plan["mode"]
                        && failure["requested_span_percent"] == *percent,
                    "bounded failure receipt"
                ),
                _ => anyhow::bail!("unsupported failure reason"),
            }
            ensure!(
                !task.join("record.json").exists(),
                "failure has accepted record"
            );
        }
    }
    let summary = json!({"slot":slot_name,"generator":model,"requested":expected.len(),"accepted":accepted.len(),"failed":failures.len(),"failures":failures,"complete":true,"input_sha256":human_sha,"config_sha256":config_sha,"binding_sha256":compact_sha(&binding)?,"report":report});
    Ok((generated, summary))
}

fn exclude_collisions(rows: Vec<Value>) -> Result<(Vec<Value>, Vec<Value>)> {
    let mut groups = BTreeMap::<String, BTreeSet<String>>::new();
    for r in &rows {
        let key = sha(str_at(r, "text")?
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .as_bytes());
        groups
            .entry(key)
            .or_default()
            .insert(str_at(r, "frozen_split")?.into());
    }
    let (mut kept, mut excluded) = (Vec::new(), Vec::new());
    for r in rows {
        let key = sha(str_at(&r, "text")?
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .as_bytes());
        if groups[&key].len() > 1 {
            ensure!(
                r["origin"] == "model_only" && r["generation"]["mode"] == "from_scratch",
                "source/descendant collision across splits: {}",
                r["id"]
            );
            excluded.push(json!({"id":r["id"],"source_group":r["source_group"],"split":r["frozen_split"],"text_sha256":r["text_sha256"],"normalized_sha256":key,"reason":"all_copies_of_cross_split_from_scratch_duplicate_excluded"}));
        } else {
            kept.push(r);
        }
    }
    Ok((kept, excluded))
}
fn assemble(a: &[PathBuf]) -> Result<()> {
    ensure!(
        a.len() == 8,
        "assemble_balanced_dataset NEW_OUT SOURCE_MANIFEST QUERY_MANIFEST HUMANS.jsonl CONFIG_DIR KNOWN1_DIR KNOWN2_DIR CAL_ONLY_DIR"
    );
    let out = &a[0];
    ensure!(!out.exists(), "new output required");
    // Refuse incomplete runs before materializing any human or generated prose.
    for dir in &a[5..8] {
        let report: Value = serde_json::from_slice(&bytes(&dir.join("report.json"))?)?;
        ensure!(
            report["complete"] == true && report["test_opened"] == false,
            "generation not complete/non-Test"
        );
    }
    let mut inputs = Vec::new();
    let works = frozen_works()?;
    let expected = roster(&works);
    let (sources, source_bytes) = load(&a[1], &mut inputs)?;
    ensure!(
        sha(&source_bytes) == SOURCE_SHA,
        "source manifest differs from fixed cohort"
    );
    let index = source_index(&sources, &works)?;
    check_roster(array(&sources, "expected_roster")?, &expected)?;
    let (query, query_bytes) = load(&a[2], &mut inputs)?;
    ensure!(
        query["schema"] == "slop_ninja_author_queries_v2"
            && query["mode"] == "fresh-balanced"
            && query["source_manifest_sha256"] == SOURCE_SHA
            && query["test_mode"] == false
            && query["test_text_opened"] == false,
        "query manifest binding/mode"
    );
    check_roster(array(&query, "planned_records")?, &expected)?;
    let test_ids = expected
        .iter()
        .filter(|(_, r)| r["frozen_split"] == "test")
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    ensure!(ids(&query, "test_ids")? == test_ids, "Test reservations");
    let expected_humans = expected
        .iter()
        .filter(|(_, r)| {
            r["frozen_split"] != "test" && (r["purpose"] == "query" || r["frozen_split"] == "train")
        })
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    let b = bytes(&a[3])?;
    let human_sha = sha(&b);
    ensure!(
        human_sha == HUMANS_SHA && query["records_sha256"] == human_sha,
        "fixed human/query hash"
    );
    inputs.push(json!({"path":a[3],"sha256":human_sha}));
    let humans = records(&b)?;
    let human_map = unique_rows(&humans)?;
    ensure!(
        humans.len() == 100
            && human_map.keys().cloned().collect::<BTreeSet<_>>() == expected_humans
            && ids(&query, "emitted_human_ids")? == expected_humans
            && ids(&query, "non_test_ids")? == expected_humans,
        "human roster/count"
    );
    ensure!(
        array(&query, "failures")?.is_empty(),
        "frozen extraction must be complete"
    );
    for (id, r) in &human_map {
        let e = &expected[id];
        validate_human(r, index[&counter(e, "pg_id")?], e, SOURCE_SHA)?;
    }
    let mut all = humans.clone();
    let mut reports = Vec::new();
    let mut prompts = BTreeMap::new();
    for slot in 0..3 {
        let (config, b) = load(&a[4].join(format!("{}.json", SLOTS[slot])), &mut inputs)?;
        let config_sha = sha(&b);
        ensure!(
            config_sha == CONFIG_SHAS[slot],
            "config differs from frozen slot {}",
            SLOTS[slot]
        );
        check_slot(
            slot,
            &config,
            if slot == 2 { "calibration" } else { "train" },
        )?;
        let (rows, report) = generator(
            &a[5 + slot],
            slot,
            &config,
            &config_sha,
            &humans,
            &human_sha,
            &mut inputs,
            &mut prompts,
        )?;
        all.extend(rows);
        reports.push(report);
    }
    unique_rows(&all)?;
    let mut assignments = BTreeMap::new();
    for row in &all {
        let group = str_at(row, "source_group")?;
        let split = str_at(row, "frozen_split")?;
        ensure!(
            PARTS.contains(&split) && !test_ids.contains(str_at(row, "id")?),
            "Test forbidden"
        );
        if let Some(old) = assignments.insert(group.to_owned(), split.to_owned()) {
            ensure!(old == split, "work group crosses partitions");
        }
    }
    let accepted_before_exclusions = all.len();
    let (mut rows, excluded) = exclude_collisions(all)?;
    rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let mut output = Vec::new();
    for row in &rows {
        serde_json::to_writer(&mut output, row)?;
        output.push(b'\n');
    }
    let manifest = json!({"schema":"slop_ninja_a_supervised_dataset_v1","cohort":"a-balanced-sources-20261026","records_sha256":sha(&output),"input_sha256":sha(&output),"non_test_ids":rows.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"test_ids":test_ids,"split_assignments":assignments,"sources":inputs,"source_manifest_sha256":SOURCE_SHA,"reserved_manifest_sha256":sha(&query_bytes),"frozen_selection_sha256":TSV_SHA,"requested_human_records":100,"requested_generation_records":832,"requested_records":932,"accepted_before_exclusions":accepted_before_exclusions,"records":rows.len(),"excluded":excluded,"generation_reports":reports,"extraction_failures":query["failures"],"separate_withheld_reference_records":8,"complete":true,"test_text_opened":false,"source_families":assignments.len(),"clean_holdout":false,"exposure":"Fresh underlying-work families under the frozen metadata audit; pretrained-model and prior grammar exposure remain unknown. Historical public-domain human proxies do not establish authenticated human-only production histories.","labels_are_not_inputs":true,"parser_frozen":"grammar/a-document-input-v1.json","reference_policy":"12 known Train reference excerpts retained for gallery construction only; 8 withheld non-Test reference excerpts remain in the separate reference file. Fit parameters, vocabulary, normalization and identities on Train queries only.","rights_scope":"Per-work US public-domain assertions and unchanged descendant source rights; each pinned generator configuration retains its own model/output-use evidence."});
    supervision::Dataset::parse(&serde_json::to_vec(&manifest)?)?;
    fs::create_dir(out)?;
    write_new(&out.join("records.jsonl"), &output)?;
    let mut m = serde_json::to_vec_pretty(&manifest)?;
    m.push(b'\n');
    write_new(&out.join("manifest.json"), &m)?;
    File::open(out)?.sync_all()?;
    println!(
        "{}",
        json!({"manifest":out.join("manifest.json"),"requested":932,"records":rows.len(),"excluded":excluded.len()})
    );
    Ok(())
}
fn main() -> Result<()> {
    assemble(
        &std::env::args_os()
            .skip(1)
            .map(PathBuf::from)
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sealed_test_and_non_test_generation_routes_are_disjoint() {
        let report = json!({"test_opened":true});
        let expected = json!({"selection_receipt_sha256":"a".repeat(64),
            "admission_manifest_sha256":"b".repeat(64),"source_manifest_sha256":"c".repeat(64)});
        let mut binding = json!({"test_opened":true,"recipe":"encoder_stage3_v1",
            "test_policy":"frozen_last_block_Test_v1; no fitting or reselection", "test_binding":expected});
        check_generation_scope(&report, &binding, Some(&expected)).unwrap();
        assert!(check_generation_scope(&report, &binding, None).is_err());
        binding["test_binding"]["selection_receipt_sha256"] = json!("d".repeat(64));
        assert!(check_generation_scope(&report, &binding, Some(&expected)).is_err());
        let report = json!({"test_opened":false});
        binding = json!({"test_opened":false});
        check_generation_scope(&report, &binding, None).unwrap();
        assert!(check_generation_scope(&report, &binding, Some(&expected)).is_err());
        binding["test_binding"] = expected;
        assert!(check_generation_scope(&report, &binding, None).is_err());
    }
    fn parent() -> Value {
        let text = "One two three four five six seven eight nine ten.";
        json!({"schema":"slop_ninja_a_supervised_record_v1","id":"author-pg1-p20","text":text,"text_sha256":sha(text.as_bytes()),"frozen_split":"train","source_group":"synthetic-work","purpose":"query","origin":"human_only","human_role_present":true,"human_identity":"writer","ai_role_present":false,"ai_identity":null,"parent_id":null,"share_target":0.0,"share_evidence":{},"evidence":"historical_public_domain_author_proxy","documented_human_only":false,"source":{},"rights":{"commercial_training":true},"exposed_to_grammar":true,"grammar_exposure":"unknown"})
    }
    fn config(slot: usize) -> Value {
        json!({"schema":"slop_ninja_local_generator_v1","identity":MODELS[slot],"allowed_splits":if slot==2{json!(["calibration"])}else{json!(["train","development","calibration","test"])},"rights":{"generation_permitted":true}})
    }
    fn rewritten() -> (Value, Value, Value, Value, Value) {
        let p = parent();
        let c = config(0);
        let binding = json!({"generator_config":c});
        let t = "New two three four five six seven eight nine ten.";
        let plan = json!({"id":"child","source_byte_span":[0,3],"request":{},"position":"start","mode":"controlled_rewrite"});
        let mut r = p.clone();
        r["id"] = json!("child");
        r["parent_id"] = p["id"].clone();
        r["origin"] = json!("mixed");
        r["ai_role_present"] = json!(true);
        r["ai_identity"] = c["identity"].clone();
        r["text"] = json!(t);
        r["text_sha256"] = json!(sha(t.as_bytes()));
        r["share_target"] = json!(0.1);
        r["generation"] = json!({"generator":c["identity"],"binding":binding,"request":{},"position":"start","mode":"controlled_rewrite"});
        r["share_evidence"] = json!({"schema":"slop_ninja_realized_span_share_v1","source_text":p["text"],"source_sha256":p["text_sha256"],"source_byte_span":[0,3],"replacement":"New","final_byte_span":[0,3],"final_sha256":r["text_sha256"],"replacement_words":1,"final_words":10,"word_policy":"unicode_split_whitespace_v1","realized_share":0.1,"requested_percent_not_target":20,"full_rewrite_is_mixed_history":false});
        (r, p, c, binding, plan)
    }
    #[test]
    fn realized_span_reuses_shared_checker() {
        let (r, p, c, b, plan) = rewritten();
        validate_descendant(&r, &p, &c, &b, &plan, 0, 20).unwrap();
        let mut bad = r.clone();
        bad["share_evidence"]["final_byte_span"] = json!([0, 4]);
        assert!(validate_descendant(&bad, &p, &c, &b, &plan, 0, 20).is_err());
        let mut bad = r;
        bad["share_target"] = json!(0.2);
        assert!(validate_descendant(&bad, &p, &c, &b, &plan, 0, 20).is_err());
    }
    #[test]
    fn reject_human_role_and_partition_leakage() {
        let (r, p, c, b, plan) = rewritten();
        let mut bad = r.clone();
        bad["human_role_present"] = json!(false);
        assert!(validate_descendant(&bad, &p, &c, &b, &plan, 0, 20).is_err());
        let mut bad = r.clone();
        bad["human_identity"] = json!("different_writer");
        assert!(validate_descendant(&bad, &p, &c, &b, &plan, 0, 20).is_err());
        let mut bad = r;
        bad["frozen_split"] = json!("development");
        assert!(validate_descendant(&bad, &p, &c, &b, &plan, 0, 20).is_err());
    }
    #[test]
    fn generator_slots_and_withheld_calibration_are_enforced() {
        check_slot(0, &config(0), "development").unwrap();
        check_slot(2, &config(2), "calibration").unwrap();
        assert!(check_slot(2, &config(2), "train").is_err());
        assert!(check_slot(2, &config(2), "development").is_err());
        assert!(check_slot(0, &config(1), "train").is_err());
        assert!(check_slot(0, &config(0), "test").is_err());
    }
    #[test]
    fn exact_terminal_coverage_including_failures() {
        let e = BTreeSet::from(["a".into(), "b".into()]);
        exact_outcomes(
            &e,
            &BTreeSet::from(["a".into()]),
            &BTreeSet::from(["b".into()]),
        )
        .unwrap();
        assert!(exact_outcomes(&e, &BTreeSet::from(["a".into()]), &BTreeSet::new()).is_err());
        assert!(exact_outcomes(&e, &e, &BTreeSet::from(["a".into()])).is_err());
    }
    #[test]
    fn all_scratch_copies_excluded_but_descendant_collision_fails() {
        let a = json!({"id":"a","text":"same text","frozen_split":"train","origin":"model_only","generation":{"mode":"from_scratch"}});
        let mut b = a.clone();
        b["id"] = json!("b");
        b["text"] = json!("same\ntext");
        b["frozen_split"] = json!("development");
        let (kept, excluded) = exclude_collisions(vec![a.clone(), b.clone()]).unwrap();
        assert!(kept.is_empty());
        assert_eq!(excluded.len(), 2);
        b["origin"] = json!("mixed");
        assert!(exclude_collisions(vec![a, b]).is_err());
    }
    #[test]
    fn frozen_roster_preserves_reserved_and_reference_counts() {
        let works = frozen_works().unwrap();
        let r = roster(&works);
        assert_eq!(r.len(), 144);
        assert_eq!(
            r.values().filter(|x| x["frozen_split"] == "test").count(),
            36
        );
        assert_eq!(
            r.values()
                .filter(|x| x["purpose"] == "reference" && x["frozen_split"] == "train")
                .count(),
            12
        );
        assert_eq!(
            r.values()
                .filter(|x| x["purpose"] == "query" && x["frozen_split"] != "test")
                .count(),
            88
        );
    }
}
