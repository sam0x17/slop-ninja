//! Finite, source-bound edit menus. Score combinations before pruning their paths.
use super::{edits, hash, parse_model, parser_model, read, save, style, text, write_new};
use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use grammar_core::{
    features, rules,
    space::{self, Config, Reference},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Subcommand)]
pub enum Action {
    /// Freeze lexical/clause proposal requests, with and without measured style guidance.
    ProposalPlan {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        model_spec: PathBuf,
        #[arg(long)]
        python: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Import recorded model alternatives as unreviewed, bounded source edits.
    ImportProposals {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        run: PathBuf,
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Materialize every compatible subset through the declared depth, with no score pruning.
    Prepare {
        #[arg(long)]
        input: PathBuf,
        /// Required when the input continues a previously selected revision.
        #[arg(long)]
        parent_selection: Option<PathBuf>,
        /// Record a changed search-distance limit without replacing the original source.
        #[arg(long)]
        amend_word_budget_reason: Option<String>,
        /// Explain a changed per-state patch limit while retaining the original source.
        #[arg(long)]
        amend_patch_budget_reason: Option<String>,
        #[arg(long)]
        python: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Measure conditional edit effects and interactions using complete, pinned A scores.
    Rank {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        scores: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Join collected Pangram observations with A; retain every class and window.
    ExternalReport {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        scores: PathBuf,
        #[arg(long, required = true)]
        annotations: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Measure optional sentence-order features without changing a recorded menu.
    OrderAudit {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        python: String,
        #[arg(long, value_enum, default_value = "joint-v1")]
        representation: OrderRepresentation,
        #[arg(long)]
        out: PathBuf,
    },
    /// Accept an exact menu state only after a text-bound editorial review passes.
    Select {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        review: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum OrderRepresentation {
    JointV1,
    FactorizedV2,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Choice {
    id: String,
    kind: String,
    rationale: String,
    provenance: Value,
    edits: Vec<edits::Edit>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Writing {
    id: String,
    text: String,
    provenance: Value,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Input {
    schema: String,
    source: Writing,
    intent: String,
    claims: Vec<String>,
    references: Vec<Writing>,
    choices: Vec<Choice>,
    include_grammar_rules: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    grammar_rules: Option<Vec<String>>,
    max_depth: usize,
    max_states: usize,
    max_word_edit_fraction: f64,
    /// Counts physical source spans, including punctuation/capitalization patches.
    #[serde(default = "default_patch_budget")]
    max_source_patches: usize,
    detector_artifact_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parser_model: Option<String>,
    /// The prior selection hash, or null for a new walk. Always retain the root source.
    parent_selection_sha256: Option<String>,
}

fn subsets(n: usize, depth: usize, budget: usize) -> Result<Vec<Vec<usize>>> {
    let mut states = vec![vec![]];
    for size in 1..=depth.min(n) {
        let parents: Vec<_> = states
            .iter()
            .filter(|x| x.len() == size - 1)
            .cloned()
            .collect();
        for parent in parents {
            for next in parent.last().map_or(0, |i| i + 1)..n {
                ensure!(
                    states.len() < budget,
                    "Menu exceeds the declared state budget; reduce choices/depth explicitly"
                );
                let mut child = parent.clone();
                child.push(next);
                states.push(child);
            }
        }
    }
    Ok(states)
}

fn safe_id(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
}

fn default_patch_budget() -> usize {
    16
}

fn revise_patch_budget(binding: &mut Value, requested: usize, reason: Option<&str>) -> Result<()> {
    ensure!(
        (1..=edits::MAX_SOURCE_PATCHES).contains(&requested),
        "Invalid source patch limit"
    );
    let previous = match binding.get("max_source_patches") {
        None => default_patch_budget() as u64,
        Some(value) => value.as_u64().context("Invalid bound source patch limit")?,
    };
    ensure!(
        (1..=edits::MAX_SOURCE_PATCHES as u64).contains(&previous),
        "Invalid bound source patch limit"
    );
    if previous == requested as u64 {
        ensure!(reason.is_none(), "No source patch limit changed");
    } else {
        let reason = reason.filter(|s| !s.trim().is_empty()).context(
            "Changing a patch limit requires --amend-patch-budget-reason; do not reset the root source"
        )?;
        let mut history = match binding.get("patch_budget_history") {
            None => vec![],
            Some(value) => value
                .as_array()
                .context("Invalid patch budget history")?
                .clone(),
        };
        history.push(json!({"from":previous,"to":requested,"reason":reason}));
        binding["patch_budget_history"] = json!(history);
    }
    binding["max_source_patches"] = json!(requested);
    Ok(())
}

fn revise_word_budget(binding: &mut Value, requested: f64, reason: Option<&str>) -> Result<()> {
    ensure!(
        requested.is_finite() && requested > 0.0 && requested <= 1.0,
        "Word edit fraction must be in (0, 1]"
    );
    let previous = binding["max_word_edit_fraction"]
        .as_f64()
        .context("Missing original word edit limit")?;
    if previous == requested {
        ensure!(reason.is_none(), "No word edit limit changed");
        return Ok(());
    }
    let reason = reason.filter(|s| !s.trim().is_empty()).context(
        "Changing a search limit requires --amend-word-budget-reason; do not reset the root source",
    )?;
    let mut history = match binding.get("word_budget_history") {
        Some(v) => v.as_array().context("Invalid word budget history")?.clone(),
        None => vec![],
    };
    history.push(json!({"from":previous,"to":requested,"reason":reason}));
    binding["max_word_edit_fraction"] = json!(requested);
    binding["word_budget_history"] = json!(history);
    Ok(())
}

fn bind_parser_identity(binding: &mut Value, identity: &str) -> Result<()> {
    if let Some(previous) = binding.get("parser_identity") {
        ensure!(
            previous == identity,
            "Parser assets changed within a continued walk"
        );
    }
    binding["parser_identity"] = json!(identity);
    Ok(())
}

fn prepare(
    path: PathBuf,
    parent_selection: Option<PathBuf>,
    amend_word_budget_reason: Option<String>,
    amend_patch_budget_reason: Option<String>,
    python: String,
    out: PathBuf,
) -> Result<()> {
    let mut input: Input = serde_json::from_slice(&fs::read(&path)?)?;
    ensure!(
        input.schema == "slop_ninja_b_edit_menu_input_v1",
        "Wrong menu schema"
    );
    ensure!(!out.exists(), "Refusing to overwrite an experiment");
    let reference_hash = hash(serde_json::to_vec(&input.references)?);
    let model = input
        .parser_model
        .as_deref()
        .unwrap_or(grammar_spacy::DEFAULT_MODEL);
    ensure!(!model.trim().is_empty(), "Empty parser model");
    let parser = grammar_spacy::Parser {
        python: &python,
        model,
    };
    let mut walk_binding = if let Some(parent_path) = parent_selection {
        ensure!(
            input.parent_selection_sha256.as_deref()
                == Some(hash(fs::read(&parent_path)?).as_str()),
            "Parent selection hash mismatch"
        );
        let parent = read(&parent_path)?;
        ensure!(
            parent["schema"] == "slop_ninja_b_edit_menu_selection_v1"
                && super::editorial_gate(&parent["review"]) == Some(true),
            "Parent is not an editorially accepted selection"
        );
        ensure!(
            parent["state"]["text"] == input.source.text
                && parent["state"]["id"] == hash(&input.source.text),
            "New menu must start at the exact selected text"
        );
        let binding = &parent["walk_binding"];
        ensure!(
            binding["parser_identity"].is_string(),
            "Parent selection lacks parser identity; reselect its frozen plan with the existing review"
        );
        ensure!(
            binding["intent"] == input.intent
                && binding["claims"] == json!(input.claims)
                && binding["references_sha256"] == reference_hash
                && binding["detector_artifact_id"] == input.detector_artifact_id
                && parser_model(binding)? == model,
            "A continued walk must preserve its original intent, claims, reference works, detector and parser model"
        );
        let mut binding = binding.clone();
        revise_word_budget(
            &mut binding,
            input.max_word_edit_fraction,
            amend_word_budget_reason.as_deref(),
        )?;
        revise_patch_budget(
            &mut binding,
            input.max_source_patches,
            amend_patch_budget_reason.as_deref(),
        )?;
        binding
    } else {
        ensure!(
            input.parent_selection_sha256.is_none(),
            "Supply the bound parent selection file"
        );
        ensure!(
            amend_word_budget_reason.is_none() && amend_patch_budget_reason.is_none(),
            "A new walk has no prior word budget to amend"
        );
        ensure!(
            (1..=edits::MAX_SOURCE_PATCHES).contains(&input.max_source_patches),
            "Invalid source patch limit"
        );
        json!({"root_source":input.source,"intent":input.intent,"claims":input.claims,"references_sha256":reference_hash,"detector_artifact_id":input.detector_artifact_id,"parser_model":model,"max_word_edit_fraction":input.max_word_edit_fraction,"max_source_patches":input.max_source_patches})
    };
    let root_text = text(&walk_binding["root_source"], "text")?;
    let root_words = features::words(root_text).len();
    ensure!(root_words >= 40, "Invalid root source");
    ensure!(
        (1..=16).contains(&input.max_depth) && (2..=512).contains(&input.max_states),
        "Invalid search bounds"
    );
    ensure!(
        !input.intent.trim().is_empty() && !input.claims.is_empty(),
        "Missing intent or claims"
    );
    ensure!(
        input.detector_artifact_id.len() == 64
            && input
                .detector_artifact_id
                .bytes()
                .all(|x| x.is_ascii_hexdigit()),
        "Pin a detector artifact"
    );
    ensure!(
        features::words(&input.source.text).len() >= 40 && input.source.provenance.is_object(),
        "Missing source/provenance"
    );
    ensure!(
        input.references.len() >= 2,
        "Need independent author references"
    );
    let mut works = BTreeSet::new();
    let mut contents = BTreeSet::from([hash(&input.source.text)]);
    for sample in &input.references {
        ensure!(
            works.insert(&sample.id)
                && contents.insert(hash(&sample.text))
                && features::words(&sample.text).len() >= 40
                && sample.provenance.is_object(),
            "Duplicate, short or unprovenanced reference"
        );
    }
    let excluded_rules: Vec<Value> = vec![];
    ensure!(
        input.include_grammar_rules || input.grammar_rules.is_none(),
        "A grammar rule selection requires include_grammar_rules"
    );
    if input.include_grammar_rules {
        let doc = parser
            .parse_batch(&[input.source.text.as_str()])?
            .into_iter()
            .next()
            .context("No source parse")?
            .map_err(anyhow::Error::msg)?;
        let selected = input.grammar_rules.clone().unwrap_or_else(|| {
            rules::BUILTIN_RULES
                .iter()
                .map(|r| (*r).to_owned())
                .collect()
        });
        ensure!(!selected.is_empty(), "Empty grammar rule selection");
        for candidate in rules::generate_selected(&doc, rules::MAX_CANDIDATES, &selected)? {
            input.choices.push(Choice {
                id: candidate.id.clone(),
                kind: "grammar_rule".into(),
                rationale: candidate.rule.clone(),
                provenance: serde_json::to_value(&candidate)?,
                edits: candidate
                    .edits
                    .iter()
                    .map(|e| edits::Edit {
                        before: e.expected.clone(),
                        after: e.replacement.clone(),
                        start_byte: Some(e.start_byte),
                    })
                    .collect(),
            });
        }
    }
    ensure!(
        !input.choices.is_empty() && input.choices.len() <= 16,
        "Use 1..16 explicit alternatives"
    );
    let mut ids = BTreeSet::new();
    for choice in &input.choices {
        ensure!(
            safe_id(&choice.id)
                && ids.insert(&choice.id)
                && !choice.rationale.trim().is_empty()
                && !choice.kind.trim().is_empty()
                && choice.provenance.is_object()
                && !choice.edits.is_empty(),
            "Invalid menu entry"
        );
    }
    let mode = edits::Output::SourceEdits {
        max_edits: input.max_source_patches,
        max_word_edit_fraction: input.max_word_edit_fraction,
    };
    let mut states = vec![];
    let mut rejected = vec![];
    for selected in subsets(input.choices.len(), input.max_depth, input.max_states)? {
        let choice_ids: Vec<_> = selected
            .iter()
            .map(|i| input.choices[*i].id.clone())
            .collect();
        let patches: Vec<_> = selected
            .iter()
            .flat_map(|i| input.choices[*i].edits.clone())
            .collect();
        match edits::apply(
            &input.source.text,
            &json!({"edits":patches}).to_string(),
            &mode,
        ) {
            Ok(applied) => {
                if edits::word_edit_count(root_text, &applied.text) as f64 / root_words as f64
                    > input.max_word_edit_fraction
                {
                    rejected.push(json!({"choices":choice_ids,"reason":"Cumulative root-source word edit budget exceeded"}));
                    continue;
                }
                if !selected.is_empty() && applied.text == input.source.text {
                    rejected.push(json!({"choices":choice_ids,"reason":"No text change"}));
                    continue;
                }
                states.push(json!({"id":hash(&applied.text),"choices":choice_ids,"text":applied.text,"word_edit_count":applied.word_edit_count,"editorial_status":"unreviewed"}));
            }
            Err(error) => rejected.push(json!({"choices":choice_ids,"reason":error.to_string()})),
        }
    }
    ensure!(
        states.first().is_some_and(|s| s["choices"] == json!([])),
        "No unchanged state; invalid edit policy"
    );
    let mut texts: Vec<_> = input.references.iter().map(|r| r.text.clone()).collect();
    texts.extend(
        states
            .iter()
            .map(|s| s["text"].as_str().unwrap().to_owned()),
    );
    let raw = parse_model(&texts, &python, model)?;
    bind_parser_identity(&mut walk_binding, &raw[0].parser_identity)?;
    let views: Vec<_> = raw.iter().map(style::function_grammar).collect();
    let n = input.references.len();
    let fit = |features: &[features::Features]| -> Result<_> {
        let refs: Vec<_> = input
            .references
            .iter()
            .zip(features)
            .map(|(r, f)| Reference {
                source_group: r.id.clone(),
                features: f.clone(),
            })
            .collect();
        space::fit(&refs, &features[n..], &Config::default())
    };
    let full_space = fit(&raw)?;
    let function_space = fit(&views)?;
    for (index, state) in states.iter_mut().enumerate() {
        let p = function_space.project(&views[n + index])?;
        state["function_grammar_distance"] = json!(function_space.distance(&p)?.euclidean);
        state["full_word_grammar_distance"] = json!(
            full_space
                .distance(&full_space.project(&raw[n + index])?)?
                .euclidean
        );
        state["movements"] =
            json!(function_space.movements(&function_space.project(&views[n])?, &p)?);
        let words = features::words(state["text"].as_str().unwrap());
        state["copied_reference_8gram"] = json!(input.references.iter().any(|r| {
            let reference = features::words(&r.text);
            words
                .windows(8)
                .any(|w| reference.windows(8).any(|v| v == w))
        }));
    }
    fs::create_dir_all(&out)?;
    save(
        &out.join("plan.json"),
        &json!({"schema":"slop_ninja_b_edit_menu_plan_v1","input_sha256":hash(fs::read(path)?),"input":input,"walk_binding":walk_binding,"full_space":full_space,"function_space":function_space,"states":states,"rejected":rejected,"excluded_rules":excluded_rules,"semantic_preservation_certified":false,"research_only":true}),
    )?;
    let mut requests = BTreeMap::new();
    for state in &states {
        requests.insert(text(state, "id")?, text(state, "text")?);
    }
    let lines = requests
        .iter()
        .map(|(id, text)| json!({"id":id,"text":text}).to_string() + "\n")
        .collect::<String>();
    write_new(&out.join("detector-requests.jsonl"), lines.as_bytes())?;
    save(
        &out.join("selection.template.json"),
        &json!({"plan_sha256":hash(fs::read(out.join("plan.json"))?),"state_id":"","reviewer":"","method":"","meaning":null,"target_tone":null,"readability":null,"cleanup":null,"author_fit":null,"notes":"Review the complete candidate against the root source and intent; lexical and numeric guards do not establish fidelity."}),
    )?;
    Ok(())
}

pub(super) fn scores_by_id(
    path: &Path,
    artifact: &str,
    expected: &BTreeSet<String>,
) -> Result<BTreeMap<String, f64>> {
    let mut scores = BTreeMap::new();
    for row in super::json_lines(path)? {
        ensure!(
            row["artifact_id"] == artifact
                && row["status"] == "ok"
                && row["labels"] == json!(["human_only", "model_only", "mixed"]),
            "Wrong detector artifact or failed inference"
        );
        let p = row["probabilities"]
            .as_array()
            .context("Missing probabilities")?
            .iter()
            .map(|v| v.as_f64().context("Invalid probability"))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            p.len() == 3
                && p.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
                && (p.iter().sum::<f64>() - 1.0).abs() < 1e-6,
            "Invalid probability vector"
        );
        let h = row["model_or_assistance_probability"]
            .as_f64()
            .context("No combined probability")?;
        ensure!(
            h.is_finite() && (0.0..=1.0).contains(&h) && (h - p[1] - p[2]).abs() < 1e-6,
            "Inconsistent combined probability"
        );
        ensure!(
            scores.insert(text(&row, "id")?.to_owned(), h).is_none(),
            "Duplicate score"
        );
    }
    ensure!(
        scores.keys().cloned().collect::<BTreeSet<_>>() == *expected,
        "Missing or unexpected scores"
    );
    Ok(scores)
}

pub(super) fn plan_at(path: &Path) -> Result<Value> {
    let plan = read(path)?;
    ensure!(
        plan["schema"] == "slop_ninja_b_edit_menu_plan_v1",
        "Wrong plan schema"
    );
    let states = plan["states"].as_array().context("No states")?;
    ensure!(!states.is_empty(), "Empty menu");
    for state in states {
        ensure!(
            text(state, "id")? == hash(text(state, "text")?),
            "State text hash mismatch"
        );
    }
    ensure!(
        states[0]["choices"] == json!([]) && states[0]["text"] == plan["input"]["source"]["text"],
        "Missing unchanged source"
    );
    Ok(plan)
}

fn rank(path: PathBuf, scores_path: PathBuf, out: PathBuf) -> Result<()> {
    let plan = plan_at(&path)?;
    let states = plan["states"].as_array().unwrap();
    let expected = states
        .iter()
        .map(|s| text(s, "id").map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()?;
    let scores = scores_by_id(
        &scores_path,
        text(&plan["input"], "detector_artifact_id")?,
        &expected,
    )?;
    let source_h = scores[text(&states[0], "id")?];
    let mut lookup = BTreeMap::new();
    for state in states {
        let keys: Vec<String> = serde_json::from_value(state["choices"].clone())?;
        ensure!(
            lookup.insert(keys, state).is_none(),
            "Duplicate menu subset"
        );
    }
    let mut ranked = vec![];
    let mut edges = vec![];
    for (keys, state) in &lookup {
        let h = scores[text(state, "id")?];
        let mut single_effect = 0.0;
        let mut all_singles = true;
        for key in keys {
            if let Some(single) = lookup.get(&vec![key.clone()]) {
                single_effect += scores[text(single, "id")?] - source_h;
            } else {
                all_singles = false;
            }
            let parent_keys: Vec<_> = keys.iter().filter(|x| *x != key).cloned().collect();
            if let Some(parent) = lookup.get(&parent_keys) {
                let previous = scores[text(parent, "id")?];
                edges.push(json!({"from":parent["id"],"to":state["id"],"context_choices":parent_keys,"added_choice":key,"a_reduction":previous-h,"style_reduction":parent["function_grammar_distance"].as_f64().unwrap()-state["function_grammar_distance"].as_f64().unwrap()}));
            }
        }
        ranked.push(json!({"id":state["id"],"choices":keys,"a_probability":h,"a_reduction":source_h-h,"function_grammar_distance":state["function_grammar_distance"],"style_reduction":states[0]["function_grammar_distance"].as_f64().unwrap()-state["function_grammar_distance"].as_f64().unwrap(),"full_word_grammar_distance":state["full_word_grammar_distance"],"nonadditive_a_effect":if all_singles { Some(h-source_h-single_effect) } else { None },"editorial_status":"unreviewed","eligible_output":false}));
    }
    ranked.sort_by(|a, b| {
        a["a_probability"]
            .as_f64()
            .unwrap()
            .total_cmp(&b["a_probability"].as_f64().unwrap())
            .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
    edges.sort_by(|a, b| {
        b["a_reduction"]
            .as_f64()
            .unwrap()
            .total_cmp(&a["a_reduction"].as_f64().unwrap())
    });
    save(
        &out,
        &json!({"schema":"slop_ninja_b_edit_menu_ranking_v1","plan_sha256":hash(fs::read(path)?),"scores_sha256":hash(fs::read(scores_path)?),"source_a_probability":source_h,"ranked_states":ranked,"contextual_edit_effects":edges,"scope":"Exhaustive only within this finite menu and declared depth. No global attribution or independent validation. Every revision requires full-text editorial review; no greedy single-edit pruning.","pangram_status":"not_measured","attestation":"research_only_unattested"}),
    )
}

fn select(path: PathBuf, review_path: PathBuf, out: PathBuf) -> Result<()> {
    let plan = plan_at(&path)?;
    let review = read(&review_path)?;
    ensure!(
        review["plan_sha256"] == hash(fs::read(&path)?),
        "Review bound to a different plan"
    );
    ensure!(
        !text(&review, "reviewer")?.trim().is_empty()
            && !text(&review, "method")?.trim().is_empty()
            && !text(&review, "notes")?.trim().is_empty(),
        "Document reviewer and limitations"
    );
    ensure!(
        super::editorial_gate(&review) == Some(true),
        "All editorial judgments must pass; scores cannot overrule them"
    );
    let state = plan["states"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == review["state_id"])
        .context("Selector must choose an existing state ID; arbitrary rewrites are forbidden")?;
    let mut binding = plan["walk_binding"].clone();
    bind_parser_identity(&mut binding, text(&plan["full_space"], "parser_identity")?)?;
    save(
        &out,
        &json!({"schema":"slop_ninja_b_edit_menu_selection_v1","plan_sha256":hash(fs::read(path)?),"review_sha256":hash(fs::read(review_path)?),"review":review,"state":state,"walk_binding":binding,"root_comparison_required_each_round":true,"semantic_preservation_certified":false,"research_only":true}),
    )
}

fn order_audit(
    path: PathBuf,
    python: String,
    representation: OrderRepresentation,
    out: PathBuf,
) -> Result<()> {
    ensure!(!out.exists(), "Refusing to overwrite an audit");
    let plan = plan_at(&path)?;
    let references = plan["input"]["references"]
        .as_array()
        .context("No references")?;
    let states = plan["states"].as_array().unwrap();
    let texts = references
        .iter()
        .chain(states)
        .map(|s| text(s, "text"))
        .collect::<Result<Vec<_>>>()?;
    let version = match representation {
        OrderRepresentation::JointV1 => features::order::VERSION,
        OrderRepresentation::FactorizedV2 => features::order::FACTORIZED_VERSION,
    };
    let features = grammar_spacy::Parser {
        python: &python,
        model: parser_model(&plan["input"])?,
    }
    .parse_batch(&texts)?
    .into_iter()
    .map(|doc| {
        let doc = doc.map_err(anyhow::Error::msg)?;
        match representation {
            OrderRepresentation::JointV1 => features::order::extract(&doc),
            OrderRepresentation::FactorizedV2 => features::order::extract_factorized(&doc),
        }
    })
    .collect::<Result<Vec<_>>>()?;
    ensure!(features.len() == texts.len(), "Incomplete parse batch");
    ensure!(
        features[0].parser_identity == text(&plan["full_space"], "parser_identity")?,
        "Parser identity changed; re-extract in a separate experiment"
    );
    ensure!(
        features[0].schema
            == format!(
                "{};{}",
                text(&plan["full_space"], "feature_schema")?,
                version
            ),
        "Baseline feature version changed; re-extract in a new experiment"
    );
    let refs = references
        .iter()
        .zip(&features)
        .map(|(r, f)| {
            Ok(Reference {
                source_group: text(r, "id")?.to_owned(),
                features: f.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let n = references.len();
    let order_space = space::fit(&refs, &features[n..], &Config::default())?;
    let source = order_space.project(&features[n])?;
    let mut measured = vec![];
    for (state, f) in states.iter().zip(&features[n..]) {
        let point = order_space.project(f)?;
        let distance = order_space.distance(&point)?;
        let base = state["function_grammar_distance"]
            .as_f64()
            .context("Missing base distance")?;
        ensure!(base.is_finite() && base >= 0.0, "Invalid base distance");
        let mixtures: Vec<_> = [0.0_f64, 0.1, 0.25, 0.5].iter().map(|w| json!({
            "order_weight":w,"distance":((1.0-w)*base*base+w*distance.euclidean*distance.euclidean).sqrt()
        })).collect();
        measured.push(json!({"id":state["id"],"choices":state["choices"],
            "base_function_grammar_distance":base,"order_distance":distance,
            "order_movements":order_space.movements(&source, &point)?,
            "fixed_weight_mixtures":mixtures}));
    }
    save(
        &out,
        &json!({"schema":"slop_ninja_b_order_audit_v1","plan_sha256":hash(fs::read(path)?),
        "base_function_space_id":plan["function_space"]["id"],"order_space":order_space,
        "states":measured,"mixture":"independent-euclidean-blocks-v1; squared distance = (1-w)*base_squared + w*order_squared; diagnostic weights fixed at 0,0.1,0.25,0.5",
        "scope":"Exploratory reference-only fit. Defaults and old menus unchanged. Distinguishing an order change does not establish better ranking, author resemblance, fidelity or quality.",
        "research_only":true}),
    )
}

pub fn run(command: Action) -> Result<()> {
    match command {
        Action::ProposalPlan {
            plan,
            model_spec,
            python,
            out,
        } => super::proposals::plan(plan, model_spec, python, out),
        Action::ImportProposals {
            plan,
            run,
            request_id,
            out,
        } => super::proposals::import(plan, run, request_id, out),
        Action::Prepare {
            input,
            parent_selection,
            amend_word_budget_reason,
            amend_patch_budget_reason,
            python,
            out,
        } => prepare(
            input,
            parent_selection,
            amend_word_budget_reason,
            amend_patch_budget_reason,
            python,
            out,
        ),
        Action::Rank { plan, scores, out } => rank(plan, scores, out),
        Action::ExternalReport {
            plan,
            scores,
            annotations,
            out,
        } => super::pangram::report(plan, scores, annotations, out),
        Action::OrderAudit {
            plan,
            python,
            representation,
            out,
        } => order_audit(plan, python, representation, out),
        Action::Select { plan, review, out } => select(plan, review, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn patch_budget_continuations_preserve_root_and_record_explicit_changes() {
        let mut old =
            json!({"root_source":{"text":"Original source"},"claims":["Keep this claim"]});
        let saved = old.clone();
        assert!(revise_patch_budget(&mut old, 32, None).is_err());
        assert_eq!(old, saved);
        revise_patch_budget(&mut old, 16, None).unwrap();
        assert_eq!(old["max_source_patches"], 16);
        assert!(old.get("patch_budget_history").is_none());
        revise_patch_budget(
            &mut old,
            32,
            Some("Permit punctuation edits together with lexical repairs"),
        )
        .unwrap();
        assert_eq!(old["root_source"], saved["root_source"]);
        assert_eq!(old["claims"], saved["claims"]);
        assert_eq!(old["patch_budget_history"][0]["from"], 16);
        assert_eq!(old["patch_budget_history"][0]["to"], 32);
        let saved = old.clone();
        assert!(revise_patch_budget(&mut old, 0, Some("Invalid")).is_err());
        assert_eq!(old, saved);
        assert!(revise_patch_budget(&mut old, 32, Some("No change")).is_err());
    }
    #[test]
    fn parser_choice_defaults_without_allowing_model_or_asset_drift() {
        assert_eq!(
            parser_model(&json!({})).unwrap(),
            grammar_spacy::DEFAULT_MODEL
        );
        assert_eq!(
            parser_model(&json!({"parser_model":"en_core_web_trf"})).unwrap(),
            "en_core_web_trf"
        );
        assert!(parser_model(&json!({"parser_model":false})).is_err());
        let mut binding = json!({"root_source":{"text":"Original source"}});
        bind_parser_identity(&mut binding, "synthetic-parser;assets=first").unwrap();
        let saved = binding.clone();
        bind_parser_identity(&mut binding, "synthetic-parser;assets=first").unwrap();
        assert!(bind_parser_identity(&mut binding, "synthetic-parser;assets=changed").is_err());
        assert_eq!(binding, saved);
    }
    #[test]
    fn increasing_search_distance_retains_the_root_and_requires_a_recorded_reason() {
        let original = json!({"root_source":{"text":"We may close early."},"claims":["Closure is possible, not certain."],"max_word_edit_fraction":0.4});
        let mut binding = original.clone();
        assert!(revise_word_budget(&mut binding, 0.75, None).is_err());
        assert_eq!(binding, original);
        revise_word_budget(
            &mut binding,
            0.75,
            Some("Explore more edits; keep the same meaning review"),
        )
        .unwrap();
        assert_eq!(binding["root_source"], original["root_source"]);
        assert_eq!(binding["claims"], original["claims"]);
        assert_eq!(binding["word_budget_history"][0]["from"], 0.4);
        assert_eq!(binding["word_budget_history"][0]["to"], 0.75);
        let saved = binding.clone();
        assert!(revise_word_budget(&mut binding, f64::NAN, Some("invalid")).is_err());
        assert_eq!(binding, saved);
        revise_word_budget(&mut binding, 0.75, None).unwrap();
        assert_eq!(binding, saved);
    }
    #[test]
    fn lookahead_keeps_a_late_joint_gain_when_every_partial_path_is_worse() {
        let menu = subsets(9, 9, 512).unwrap();
        let score = |keys: &Vec<usize>| {
            if keys.len() == 9 {
                0.05_f64
            } else if keys.is_empty() {
                0.8
            } else {
                0.81
            }
        };
        assert!(
            menu.iter()
                .filter(|keys| !keys.is_empty() && keys.len() < 9)
                .all(|keys| score(keys) > score(&vec![]))
        );
        let best = menu
            .iter()
            .min_by(|a, b| score(a).total_cmp(&score(b)))
            .unwrap();
        assert_eq!(score(best), 0.05);
        assert_eq!(best, &(0..9).collect::<Vec<_>>());
        assert_eq!(menu.len(), 512);
        assert!(subsets(9, 9, 511).is_err());
        assert!(subsets(3, 3, 7).is_err());
        assert_eq!(subsets(3, 1, 4).unwrap().len(), 4);
    }
}
