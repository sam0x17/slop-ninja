//! Prepare and count one matched model/style acquisition within the shared corpus.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};
#[allow(dead_code)]
#[path = "a_balanced_generate/random_style_recipe.rs"]
mod random_style_recipe;
#[allow(dead_code)]
#[path = "a_balanced_generate/style_recipe.rs"]
mod style_recipe;

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn put(path: &Path, value: &Value) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn generation_id(parent: &str, model: &str, style: &str, percent: usize) -> String {
    format!(
        "local-{}",
        &sha(
            format!("{parent}:{model}:controlled_rewrite:{percent}:style-diversity-v1:{style}")
                .as_bytes()
        )[..24]
    )
}
fn select(input: &str, works_per_genre: usize) -> Result<Vec<Value>> {
    ensure!(
        (1..=100).contains(&works_per_genre),
        "works per genre must be 1..100"
    );
    let mut strata = BTreeMap::<String, Vec<Value>>::new();
    let mut families = BTreeSet::new();
    for line in input.lines() {
        let r: Value = serde_json::from_str(line)?;
        if r["origin"] != "human_only" || r["frozen_split"] != "train" || r["purpose"] != "query" {
            continue;
        }
        let Some(stratum) = r["metadata"]["acquisition_stratum"].as_str() else {
            continue;
        };
        if !matches!(
            stratum,
            "fiction" | "essays_ideas" | "history_biography" | "science_nature" | "travel"
        ) || strata
            .get(stratum)
            .is_some_and(|rows| rows.len() >= works_per_genre)
        {
            continue;
        }
        ensure!(
            r["rights"]["commercial_training"] == true
                && r["rights"]["external_evaluation"] == true,
            "source rights missing"
        );
        ensure!(
            r["source"]["record"]["pg_id"].is_number()
                || r["source"]["record"]["pg_id"].is_string(),
            "public PG source required"
        );
        ensure!(
            r["rights"]["catalog_statement"] == "Public domain in the USA.",
            "public-domain source evidence missing"
        );
        let text = r["text"].as_str().context("text")?;
        ensure!(
            r["text_sha256"] == sha(text.as_bytes()),
            "source hash mismatch"
        );
        let family = r["source_group"]
            .as_str()
            .context("source group")?
            .to_owned();
        if !families.insert(family) {
            continue; // One passage per work; descendants never increase family count.
        }
        strata.entry(stratum.to_owned()).or_default().push(r);
    }
    ensure!(
        strata.len() == 5,
        "five public Train source strata required, got {}",
        strata.len()
    );
    ensure!(
        strata.values().all(|rows| rows.len() == works_per_genre),
        "insufficient independent Train works in a genre; never fill from evaluation splits"
    );
    Ok(strata.into_values().flatten().collect())
}
fn prepare(
    input: &Path,
    out: &Path,
    configs: &[String],
    random: bool,
    breadth: Option<(usize, usize)>,
) -> Result<()> {
    ensure!(configs.len() >= 2, "multiple actual model configs required");
    let bytes = fs::read(input)?;
    let (works_per_genre, draws) = breadth.unwrap_or((1, random_style_recipe::REPLICATES));
    ensure!((1..=20).contains(&draws), "draws must be 1..20");
    let parents = select(std::str::from_utf8(&bytes)?, works_per_genre)?;
    ensure!(!out.exists(), "new collection directory required");
    fs::create_dir_all(out.join("configs"))?;
    let mut models = Vec::new();
    let mut ids = BTreeSet::new();
    for (i, path) in configs.iter().enumerate() {
        let bytes = fs::read(path)?;
        let mut c: Value = serde_json::from_slice(&bytes)?;
        let identity = c["identity"].as_str().context("identity")?.to_owned();
        ensure!(ids.insert(identity.clone()), "duplicate model");
        ensure!(
            c["rights"]["generation_permitted"] == true,
            "model rights missing"
        );
        let slot = format!("model-{i}");
        c["allowed_splits"] = json!(["train"]);
        c["endpoint"] = json!("http://127.0.0.1:18087");
        c["alias"] = json!(format!("a-style-{i}"));
        c["acquisition_parent_config_sha256"] = json!(sha(&bytes));
        let cfg = out.join("configs").join(format!("{slot}.json"));
        put(&cfg, &c)?;
        models.push(json!({"slot":slot,"identity":identity,"config":cfg,"config_sha256":sha(&fs::read(&cfg)?)}));
    }
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(out.join("humans.jsonl"))?;
    for parent in &parents {
        serde_json::to_writer(&mut f, parent)?;
        f.write_all(b"\n")?;
    }
    f.sync_all()?;
    let mut cells = Vec::new();
    for parent in &parents {
        for model in &models {
            let cases = if random {
                (0..draws)
                    .flat_map(|r| [40, 100].into_iter().map(move |p| (r, p)))
                    .map(|(r, p)| {
                        (
                            random_style_recipe::collection_id(
                                parent["id"].as_str().unwrap(),
                                model["identity"].as_str().unwrap(),
                                p,
                                r,
                                if breadth.is_some() {
                                    "wide-source-v2"
                                } else {
                                    "pilot-v1"
                                },
                            ),
                            p,
                        )
                    })
                    .collect::<Vec<_>>()
            } else {
                style_recipe::cases(true, &[])
                    .into_iter()
                    .map(|(s, p)| (s.to_owned(), p))
                    .collect()
            };
            for (style, percent) in cases {
                let id = if random {
                    format!(
                        "local-{}",
                        &sha(format!(
                            "{}:{}:controlled_rewrite:{percent}:{}:{style}",
                            parent["id"].as_str().unwrap(),
                            model["identity"].as_str().unwrap(),
                            random_style_recipe::VERSION
                        )
                        .as_bytes())[..24]
                    )
                } else {
                    generation_id(
                        parent["id"].as_str().unwrap(),
                        model["identity"].as_str().unwrap(),
                        &style,
                        percent,
                    )
                };
                let draw = Sha256::digest(style.as_bytes())[0] % 12;
                let modifier = match draw {
                    0 => {
                        "Use idiomatic British English without changing the setting or inventing nationality"
                    }
                    1 => {
                        "Make the prose sound human rather than AI-generated without deliberate mistakes or invented experiences"
                    }
                    2 => {
                        "The later rewrite also applies fix-slop: prefer precise prose, no em dashes, rhetorical contrasts, inflated praise, or press-release language"
                    }
                    _ => "none; invent the style freely",
                };
                cells.push(json!({"id":id,
                    "parent_id":parent["id"],"source_group":parent["source_group"],"source_sha256":parent["text_sha256"],
                    "model":model["identity"],"slot":model["slot"],"style":style,"requested_percent":percent,
                    "genre":parent["metadata"]["acquisition_stratum"],"modifier":modifier,"use_fix_slop":random && draw==2}));
            }
        }
    }
    put(
        &out.join("plan.json"),
        &json!({"schema":"slop_ninja_corpus_acquisition_v1","dataset":"slop_ninja_a_corpus",
        "batch":if random { random_style_recipe::VERSION } else { "style_diversity_v1" },"input":input,"input_sha256":sha(&bytes),
        "parents_sha256":sha(&fs::read(out.join("humans.jsonl"))?),"models":models,"cells":cells,
        "instruction_matrix":if random {random_style_recipe::contract_for(draws)} else {style_recipe::contract()},"families":parents.len(),"requested":cells.len(),
        "works_per_genre":works_per_genre,"draws_per_source_model_extent":draws,
        "additional_requested_families":[{"family":"Astra","status":"pending_access_and_generation"},{"family":"Claude","status":"pending_access_and_generation"}],
        "split":"train","fitting_admitted":false,"prior_exposure":"source families were fitted in stage3; this is acquisition evidence, not clean evaluation",
        "historical_role_warning":"A model previously withheld in an older experiment is not withheld from this new acquisition; old sealed results do not change",
        "selection":"first available distinct Train works in each of five declared source strata, one query per work, before any new generation or detector response",
        "population":"historical human proxies; no authenticated contemporary human-only claim",
        "completed_outputs":"counted from bound record and response evidence by status; model names alone are not results"}),
    )?;
    println!("{}", out.join("plan.json").display());
    Ok(())
}
fn status(out: &Path, destination: &Path) -> Result<Value> {
    let plan = read(&out.join("plan.json"))?;
    let mut counts = BTreeMap::<String, BTreeMap<String, usize>>::new();
    let mut by_state = BTreeMap::<String, usize>::new();
    let mut completed = 0;
    for cell in plan["cells"].as_array().context("cells")? {
        let dir = out
            .join(cell["slot"].as_str().context("slot")?)
            .join(cell["id"].as_str().context("id")?);
        let mut state = "pending";
        if dir.join("record.sha256.json").exists() {
            let row = read(&dir.join("record.json"))?;
            let h = read(&dir.join("record.sha256.json"))?;
            ensure!(
                h["sha256"] == sha(&fs::read(dir.join("record.json"))?)
                    && h["plan_sha256"] == sha(&fs::read(dir.join("plan.json"))?),
                "record integrity"
            );
            ensure!(
                row["id"] == cell["id"]
                    && row["parent_id"] == cell["parent_id"]
                    && row["ai_identity"] == cell["model"]
                    && row["source_group"] == cell["source_group"]
                    && row["frozen_split"] == "train"
                    && row["generation"]["instruction_style"] == cell["style"],
                "matrix binding differs"
            );
            let response = Path::new(
                row["generation"]["response_path"]
                    .as_str()
                    .context("response path")?,
            );
            ensure!(
                row["generation"]["response_sha256"] == sha(&fs::read(response)?),
                "response integrity"
            );
            let text = row["text"].as_str().context("text")?;
            ensure!(row["text_sha256"] == sha(text.as_bytes()), "text integrity");
            completed += 1;
            state = "completed";
        } else if dir.join("attempt-1.json").exists() {
            state = "failed";
        } else if dir.join("context.json").exists()
            && read(&dir.join("context.json"))?["fits"] == false
        {
            state = "unsupported_context";
        } else if dir.join("attempt-0.intent.json").exists() {
            state = "in_progress_or_uncertain";
        } else {
            let prompt_receipt = out
                .join("prompts")
                .join(cell["style"].as_str().context("style")?)
                .join("record.json");
            if prompt_receipt.exists() {
                state = if read(&prompt_receipt)?["accepted"] == true {
                    "prompt_ready"
                } else {
                    "prompt_rejected"
                };
            }
        }
        *by_state.entry(state.to_owned()).or_default() += 1;
        let key = format!(
            "{} / {}",
            cell["model"].as_str().unwrap(),
            cell["style"].as_str().unwrap()
        );
        *counts
            .entry(key)
            .or_default()
            .entry(state.to_owned())
            .or_default() += 1;
    }
    let result = json!({"schema":"slop_ninja_instruction_matrix_status_v1","dataset":plan["dataset"],"plan_sha256":sha(&fs::read(out.join("plan.json"))?),"requested":plan["requested"],"completed":completed,"by_state":by_state,"by_model_style":counts,"pending_requested_families":plan["additional_requested_families"],"fitting_performed":false,"style_compliance_verified":false});
    put(destination, &result)?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(result)
}
fn terminal_status(report: &Value) -> Result<()> {
    let states = report["by_state"].as_object().context("state counts")?;
    ensure!(
        states.keys().all(|s| matches!(
            s.as_str(),
            "completed" | "prompt_rejected" | "failed" | "unsupported_context"
        )),
        "collection still has unfinished or uncertain work"
    );
    let total: u64 = states.values().map(|v| v.as_u64().unwrap_or(0)).sum();
    ensure!(
        report["requested"] == total && report["completed"].as_u64().unwrap_or(0) > 0,
        "collection population incomplete or empty"
    );
    Ok(())
}
fn export(out: &Path, destination: &Path) -> Result<()> {
    ensure!(!destination.exists(), "new export directory required");
    fs::create_dir(destination)?;
    let report = status(out, &destination.join("status.json"))?;
    terminal_status(&report)?;
    let plan = read(&out.join("plan.json"))?;
    let parent_bytes = fs::read(out.join("humans.jsonl"))?;
    ensure!(
        plan["split"] == "train" && plan["parents_sha256"] == sha(&parent_bytes),
        "Train parent binding differs"
    );
    let mut records = Vec::new();
    for line in std::str::from_utf8(&parent_bytes)?.lines() {
        records.push(serde_json::from_str::<Value>(line)?);
    }
    for cell in plan["cells"].as_array().context("cells")? {
        let path = out
            .join(cell["slot"].as_str().context("slot")?)
            .join(cell["id"].as_str().context("id")?)
            .join("record.json");
        if path.with_file_name("record.sha256.json").exists() {
            records.push(read(&path)?);
        }
    }
    ensure!(
        records.len() as u64
            == plan["families"].as_u64().context("families")?
                + report["completed"].as_u64().unwrap(),
        "export count differs"
    );
    let mut ids = BTreeSet::new();
    let mut bytes = Vec::new();
    for row in &records {
        ensure!(
            row["frozen_split"] == "train"
                && row["purpose"] == "query"
                && row["text_sha256"] == sha(row["text"].as_str().context("text")?.as_bytes())
                && ids.insert(row["id"].as_str().context("id")?),
            "export row binding or split differs"
        );
        serde_json::to_writer(&mut bytes, row)?;
        bytes.push(b'\n');
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination.join("records.jsonl"))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    put(
        &destination.join("manifest.json"),
        &json!({"schema":"slop_ninja_corpus_collection_export_v1","dataset":plan["dataset"],"complete":true,"test_text_opened":false,"split":"train","records":records.len(),"records_sha256":sha(&bytes),"collection_plan":out.join("plan.json").canonicalize()?,"collection_plan_sha256":sha(&fs::read(out.join("plan.json"))?),"collection_status_sha256":sha(&fs::read(destination.join("status.json"))?),"families":plan["families"],"source_and_generation_fields_preserved":true,"fitting_admitted":false,"prose_fidelity_verified":false,"scope":"Provenance-preserving growing-corpus batch for frozen CFG preprocessing; no change to existing fits or experiment views."}),
    )?;
    Ok(())
}
fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.first().map(String::as_str) {
        Some("prepare") if a.len() >= 5 => {
            prepare(Path::new(&a[1]), Path::new(&a[2]), &a[3..], false, None)
        }
        Some("prepare-random") if a.len() >= 5 => {
            prepare(Path::new(&a[1]), Path::new(&a[2]), &a[3..], true, None)
        }
        Some("prepare-random-wide") if a.len() >= 7 => prepare(
            Path::new(&a[1]),
            Path::new(&a[2]),
            &a[5..],
            true,
            Some((a[3].parse()?, a[4].parse()?)),
        ),
        Some("status") if a.len() == 3 => status(Path::new(&a[1]), Path::new(&a[2])).map(|_| ()),
        Some("export") if a.len() == 3 => export(Path::new(&a[1]), Path::new(&a[2])),
        _ => anyhow::bail!(
            "a_style_dataset prepare|prepare-random INPUT OUT CONFIG CONFIG ... | prepare-random-wide INPUT OUT WORKS_PER_GENRE DRAWS CONFIG CONFIG ... | status OUT NEW_REPORT.json | export COLLECTION NEW_OUTPUT"
        ),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn style_model_and_span_each_change_record_identity() {
        let a = generation_id("parent", "qwen", "plain", 40);
        assert_ne!(a, generation_id("parent", "phi", "plain", 40));
        assert_ne!(a, generation_id("parent", "qwen", "british", 40));
        assert_ne!(a, generation_id("parent", "qwen", "plain", 100));
    }
    #[test]
    fn test_or_private_sources_cannot_fill_the_train_matrix() {
        assert!(
            select(
                r#"{"origin":"human_only","frozen_split":"test","purpose":"query"}"#,
                1
            )
            .is_err()
        );
        assert!(select(r#"{"origin":"human_only","frozen_split":"train","purpose":"query","metadata":{"acquisition_stratum":"fiction"},"rights":{"commercial_training":false}}"#, 1).is_err());
    }
    #[test]
    fn breadth_counts_works_and_never_borrows_test_sources() {
        let mut rows = Vec::new();
        for genre in [
            "fiction",
            "essays_ideas",
            "history_biography",
            "science_nature",
            "travel",
        ] {
            for (family, split) in [(0, "train"), (0, "train"), (1, "train"), (2, "test")] {
                rows.push(json!({"origin":"human_only","frozen_split":split,"purpose":"query",
                    "metadata":{"acquisition_stratum":genre},"rights":{"commercial_training":true,"external_evaluation":true,"catalog_statement":"Public domain in the USA."},
                    "source":{"record":{"pg_id":1}},"source_group":format!("{genre}-{family}"),"text":"Synthetic text.","text_sha256":sha(b"Synthetic text.")}));
            }
        }
        let input = rows
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let selected = select(&input, 2).unwrap();
        assert_eq!(selected.len(), 10);
        assert_eq!(
            selected
                .iter()
                .map(|r| r["source_group"].as_str().unwrap())
                .collect::<BTreeSet<_>>()
                .len(),
            10
        );
        assert!(select(&input, 3).is_err());
    }
    #[test]
    fn export_refuses_pending_or_uncertain_generation() {
        terminal_status(
            &json!({"requested":3,"completed":2,"by_state":{"completed":2,"prompt_rejected":1}}),
        )
        .unwrap();
        for state in ["not_started", "prompt_ready", "in_progress_or_uncertain"] {
            let mut counts = json!({"completed":2});
            counts[state] = json!(1);
            assert!(
                terminal_status(&json!({"requested":3,"completed":2,"by_state":counts})).is_err()
            );
        }
        assert!(
            terminal_status(&json!({"requested":4,"completed":2,"by_state":{"completed":2}}))
                .is_err()
        );
    }
}
