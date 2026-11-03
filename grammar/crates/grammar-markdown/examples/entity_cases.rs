//! Prepare controlled entity substitutions; no NLP, scoring or corpus admission.
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{self, Read},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    text: String,
}

fn main() -> Result<()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let sources: Vec<Case> = serde_json::from_str(&input)?;
    ensure!(!sources.is_empty(), "No source cases");
    let mut ids = BTreeSet::new();
    let mut cases = Vec::new();
    let mut manifests = Vec::new();
    for source in sources {
        ensure!(ids.insert(source.id.clone()), "Duplicate source id");
        let positions: Vec<_> = source
            .text
            .match_indices("&nbsp;")
            .map(|(i, _)| i)
            .collect();
        ensure!(
            positions.len() <= 16,
            "More than 16 entities in {}",
            source.id
        );
        let mut choices = vec![("raw".to_owned(), Vec::new())];
        if !positions.is_empty() {
            choices.push(("decode-all".to_owned(), (0..positions.len()).collect()));
            for index in 0..positions.len() {
                choices.push((format!("decode-only-{index}"), vec![index]));
                choices.push((
                    format!("keep-only-{index}"),
                    (0..positions.len()).filter(|&i| i != index).collect(),
                ));
            }
        }
        let control = grammar_markdown::render(&source.text)?;
        ensure!(control.unsupported().is_empty(), "Unsupported source view");
        let mut variants = Vec::new();
        for (role, selected) in choices {
            let id = format!("{}::{role}", source.id);
            let mut text = source.text.clone();
            for &index in selected.iter().rev() {
                let start = positions[index];
                text.replace_range(start..start + "&nbsp;".len(), "\u{a0}");
            }
            let view = grammar_markdown::render(&text)?;
            ensure!(view.unsupported().is_empty(), "Unsupported variant view");
            ensure!(
                view.text() == control.text(),
                "Rendered control changed for {id}"
            );
            variants.push(json!({
                "id":id,"role":role,"decoded_entity_indices":selected,
                "text_sha256":hex::encode(Sha256::digest(text.as_bytes()))
            }));
            cases.push(json!({"id":id,"text":text}));
        }
        manifests.push(json!({
            "source_id":source.id,
            "source_sha256":hex::encode(Sha256::digest(source.text.as_bytes())),
            "entity_source_ranges":positions.iter().map(|&start| json!({"start":start,"end":start+6})).collect::<Vec<_>>(),
            "rendered_control_sha256":hex::encode(Sha256::digest(control.text().as_bytes())),
            "variants":variants
        }));
    }
    serde_json::to_writer_pretty(
        io::stdout().lock(),
        &json!({
            "schema":"slop_ninja_entity_substitution_cases_v1",
            "input_sha256":hex::encode(Sha256::digest(input.as_bytes())),
            "policy":"Literal &nbsp; to U+00A0: raw, all, each alone, and all except each. Indices are zero-based; ranges address original UTF-8 bytes. Preserve duplicate texts with distinct experimental roles. Reject unsupported views or any change to rendered control text.",
            "sources":manifests,"cases":cases,"training_admission":false,"detector_calls":0
        }),
    )?;
    Ok(())
}
