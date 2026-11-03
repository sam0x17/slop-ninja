//! Prepare exact surface-noise pairs for manual inspection before frozen-model inference.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const BOUNDARY_PLAN: &str = "7dddd4b1f7234e08fdbd08b7e1395b7fa9864c0e9b6ecd1d0cd580f7a9e9b36a";
const BOUNDARY_REQUESTS: &str = "2b734722841a8f1afca97a95bd95e50673fbe2d8e0cd01bdd51d5cedcf3f9cf6";
const KINDS: [&str; 4] = [
    "internal-space",
    "spelling-transposition",
    "repeated-determiner",
    "terminal-repeat",
];
#[derive(Parser)]
struct Args {
    #[arg(long)]
    boundary: PathBuf,
    #[arg(long)]
    out: PathBuf,
}
#[derive(Debug)]
struct Edit {
    start: usize,
    end: usize,
    replacement: String,
}
fn words(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (i, ch) in text.char_indices() {
        if ch.is_alphabetic() {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            spans.push((s, i));
        }
    }
    if let Some(s) = start {
        spans.push((s, text.len()));
    }
    spans
}
fn case_like(word: &str, replacement: &str) -> Option<String> {
    let lower = word.to_ascii_lowercase();
    if word == lower {
        Some(replacement.into())
    } else if word == word.to_ascii_uppercase() {
        Some(replacement.to_ascii_uppercase())
    } else if word[0..1] == word[0..1].to_ascii_uppercase() && word[1..] == lower[1..] {
        Some(format!(
            "{}{}",
            replacement[0..1].to_ascii_uppercase(),
            &replacement[1..]
        ))
    } else {
        None
    }
}
fn edit(text: &str, kind: &str) -> Option<Edit> {
    match kind {
        "internal-space" => text.char_indices().find_map(|(i, ch)| {
            (ch == ' '
                && text[..i]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_alphabetic)
                && text[i + 1..]
                    .chars()
                    .next()
                    .is_some_and(char::is_alphabetic))
            .then(|| Edit {
                start: i,
                end: i + 1,
                replacement: "  ".into(),
            })
        }),
        "spelling-transposition" => words(text).into_iter().find_map(|(start, end)| {
            let word = &text[start..end];
            let replacement = match word.to_ascii_lowercase().as_str() {
                "the" => "teh",
                "with" => "wiht",
                "which" => "whcih",
                "because" => "becuase",
                _ => return None,
            };
            case_like(word, replacement).map(|replacement| Edit {
                start,
                end,
                replacement,
            })
        }),
        "repeated-determiner" => words(text).into_iter().find_map(|(start, end)| {
            let word = &text[start..end];
            word.eq_ignore_ascii_case("the").then(|| Edit {
                start,
                end,
                replacement: format!("{word} {word}"),
            })
        }),
        "terminal-repeat" => text
            .chars()
            .next_back()
            .filter(|ch| matches!(ch, '?' | '!'))
            .map(|ch| Edit {
                start: text.len() - 1,
                end: text.len(),
                replacement: format!("{ch}{ch}"),
            }),
        _ => None,
    }
}
fn apply(text: &str, e: &Edit) -> String {
    format!("{}{}{}", &text[..e.start], e.replacement, &text[e.end..])
}
fn save(path: impl AsRef<Path>, value: &Value) -> Result<()> {
    let mut f = fs::File::create_new(path)?;
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn line(file: &mut fs::File, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}
fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a new panel directory");
    let plan_bytes = fs::read(args.boundary.join("run/plan.json"))?;
    let request_bytes = fs::read(args.boundary.join("run/requests.jsonl"))?;
    ensure!(
        sha256(&plan_bytes) == BOUNDARY_PLAN && sha256(&request_bytes) == BOUNDARY_REQUESTS,
        "Changed boundary panel"
    );
    let plan: Value = serde_json::from_slice(&plan_bytes)?;
    let requests: BTreeMap<String, Value> = String::from_utf8(request_bytes)?
        .lines()
        .map(|l| {
            let value: Value = serde_json::from_str(l)?;
            Ok((
                value["id"].as_str().context("Request ID")?.to_owned(),
                value,
            ))
        })
        .collect::<Result<_>>()?;
    let pairs = plan["pairs"].as_array().context("Source pairs")?;
    ensure!(pairs.len() == 126, "Panel size");
    fs::create_dir(&args.out)?;
    let mut output = fs::File::create_new(args.out.join("requests.jsonl"))?;
    let mut review = fs::File::create_new(args.out.join("review-contexts.jsonl"))?;
    let mut rows = Vec::new();
    let mut skipped = Vec::new();
    let mut counts = BTreeMap::<(String, String, String), usize>::new();
    for pair in pairs {
        let base = requests
            .get(pair["request_ids"][0].as_str().context("Baseline ID")?)
            .context("Baseline request")?;
        let text = base["text"].as_str().context("Baseline text")?;
        ensure!(sha256(text) == pair["text_sha256"][0], "Baseline identity");
        let base_id = sha256(serde_json::to_vec(&json!([
            "slop_ninja_surface_noise_v1",
            pair["id"],
            "baseline",
            sha256(text)
        ]))?);
        line(&mut output, &json!({"id":base_id,"text":text}))?;
        for kind in KINDS {
            let Some(e) = edit(text, kind) else {
                skipped.push(json!({"id":pair["id"],"group":pair["group"],"origin":pair["origin"],"collection":pair["collection"],"kind":kind,"reason":"No eligible span under the fixed rule"}));
                continue;
            };
            let candidate = apply(text, &e);
            ensure!(candidate != text, "No-op edit");
            let id = sha256(serde_json::to_vec(&json!([
                "slop_ninja_surface_noise_v1",
                pair["id"],
                kind,
                sha256(&candidate)
            ]))?);
            line(&mut output, &json!({"id":id,"text":candidate}))?;
            let before = &text[e.start..e.end];
            let row = json!({"id":pair["id"],"group":pair["group"],"origin":pair["origin"],"collection":pair["collection"],"kind":kind,"request_ids":[base_id,id],"text_sha256":[sha256(text),sha256(&candidate)],"edit":{"start_byte":e.start,"end_byte":e.end,"before":before,"after":e.replacement}});
            let mut start = e.start.saturating_sub(160);
            while !text.is_char_boundary(start) {
                start -= 1;
            }
            let mut end = (e.end + 240).min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
            line(
                &mut review,
                &json!({"metadata":row,"context_start_byte":start,"context_end_byte":end,"before_context":&text[start..end],"after_context":format!("{}{}{}",&text[start..e.start],e.replacement,&text[e.end..end]),"context_truncated":start>0 || end<text.len(),"review_status":"pending"}),
            )?;
            *counts
                .entry((
                    kind.into(),
                    pair["origin"].as_str().unwrap().into(),
                    pair["collection"].as_str().unwrap().into(),
                ))
                .or_default() += 1;
            rows.push(row);
        }
    }
    output.sync_all()?;
    review.sync_all()?;
    fs::write(
        args.out.join("source.rs"),
        include_bytes!("surface_noise_panel.rs"),
    )?;
    fs::write(
        args.out.join("PROTOCOL.md"),
        include_bytes!("../SURFACE_NOISE_PROBE_V1.md"),
    )?;
    let prepared = json!({"schema":"slop_ninja_surface_noise_panel_v1","at":chrono::Utc::now(),"boundary_plan_sha256":BOUNDARY_PLAN,"boundary_requests_sha256":BOUNDARY_REQUESTS,"source_sha256":sha256(include_bytes!("surface_noise_panel.rs")),"binary_sha256":sha256(fs::read(std::env::current_exe()?)?),"protocol_sha256":sha256(include_bytes!("../SURFACE_NOISE_PROBE_V1.md")),"requests_sha256":sha256(fs::read(args.out.join("requests.jsonl"))?),"review_contexts_sha256":sha256(fs::read(args.out.join("review-contexts.jsonl"))?),"sources":126,"families":16,"requests_per_model":126+rows.len(),"pairs":rows,"ineligible":skipped,"eligible_counts":counts.into_iter().map(|((kind,origin,collection),n)|json!({"kind":kind,"origin":origin,"collection":collection,"count":n})).collect::<Vec<_>>(),"python":plan["python"],"candidates":plan["candidates"],"artifact_bindings":plan["bindings"],"model_calls":0,"manual_review":"pending","inference_ready":false,"new_training_records":0});
    save(args.out.join("preparation.json"), &prepared)?;
    println!(
        "{}",
        json!({"requests_per_model":prepared["requests_per_model"],"eligible_pairs":prepared["pairs"].as_array().unwrap().len(),"ineligible":prepared["ineligible"].as_array().unwrap().len(),"eligible_counts":prepared["eligible_counts"],"model_calls":0,"manual_review":"pending"})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interventions_keep_unicode_context_and_apply_once_in_source_order() {
        let text = "Élodie noted the theorem, with care.\n\nWhy?!";
        assert_eq!(
            apply(text, &edit(text, "internal-space").unwrap()),
            "Élodie  noted the theorem, with care.\n\nWhy?!"
        );
        assert_eq!(
            apply(text, &edit(text, "spelling-transposition").unwrap()),
            "Élodie noted teh theorem, with care.\n\nWhy?!"
        );
        assert_eq!(
            apply(text, &edit(text, "repeated-determiner").unwrap()),
            "Élodie noted the the theorem, with care.\n\nWhy?!"
        );
        assert_eq!(
            apply(text, &edit(text, "terminal-repeat").unwrap()),
            "Élodie noted the theorem, with care.\n\nWhy?!!"
        );
        assert_eq!(
            apply(
                "WITH the map",
                &edit("WITH the map", "spelling-transposition").unwrap()
            ),
            "WIHT the map"
        );
        assert_eq!(
            apply(
                "The map",
                &edit("The map", "spelling-transposition").unwrap()
            ),
            "Teh map"
        );
    }
    #[test]
    fn eligibility_does_not_invent_words_or_collapse_layout() {
        assert!(edit("theorem either", "spelling-transposition").is_none());
        assert!(edit("tHe map", "spelling-transposition").is_none());
        assert!(edit("a  b\nc", "internal-space").is_none());
        assert!(edit("Ready?\"", "terminal-repeat").is_none());
        assert!(edit("Ready.", "terminal-repeat").is_none());
        assert!(edit("other", "repeated-determiner").is_none());
        assert!(edit("", "terminal-repeat").is_none());
    }
}
