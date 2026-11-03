//! Compare raw and rendered annotations on Studio and project every rendered token.
use anyhow::{Context, Result, ensure};
use grammar_markdown::{POLICY, VERSION};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::{self, Read};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    text: String,
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        args.len() == 3,
        "Usage: parse_views PYTHON MODEL < cases.json > report.json"
    );
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let cases: Vec<Case> = serde_json::from_str(&input)?;
    ensure!(!cases.is_empty(), "No input cases");
    let views = cases
        .iter()
        .map(|case| grammar_markdown::render(&case.text))
        .collect::<Result<Vec<_>>>()?;
    for (case, view) in cases.iter().zip(&views) {
        ensure!(
            view.unsupported().is_empty(),
            "Unsupported Markdown constructs in case {}",
            case.id
        );
    }
    let texts: Vec<_> = cases
        .iter()
        .zip(&views)
        .flat_map(|(case, view)| [case.text.as_str(), view.text()])
        .collect();
    let parser = grammar_spacy::Parser {
        python: &args[1],
        model: &args[2],
    };
    let mut parsed = parser.parse_batch(&texts)?.into_iter();
    let policy_hash = hex::encode(Sha256::digest(POLICY.as_bytes()));
    let mut rows = Vec::new();
    for (case, view) in cases.iter().zip(views) {
        let raw = parsed
            .next()
            .context("Missing raw annotation")?
            .map_err(anyhow::Error::msg)?;
        let mut rendered = parsed
            .next()
            .context("Missing rendered annotation")?
            .map_err(anyhow::Error::msg)?;
        ensure!(
            raw.text == case.text && rendered.text == view.text(),
            "Source/view mismatch"
        );
        let projections = rendered
            .tokens
            .iter()
            .map(|token| view.project(token.start_byte..token.end_byte))
            .collect::<Result<Vec<_>>>()?;
        rendered
            .parser_identity
            .push_str(&format!(";view={VERSION};view-policy-sha256={policy_hash}"));
        rows.push(
            json!({"id":case.id,"view":view,"raw_annotation":raw,"rendered_annotation":rendered,
            "rendered_token_projections":projections}),
        );
    }
    ensure!(parsed.next().is_none(), "Unexpected extra annotations");
    let report = json!({"schema":"slop_ninja_markdown_annotation_comparison_v1", "view_version":VERSION,
        "policy_sha256":policy_hash,"input_sha256":hex::encode(Sha256::digest(input.as_bytes())),
        "requested_model":args[2],"manual_review_complete":false,"parse_accuracy":null,"rows":rows});
    serde_json::to_writer_pretty(io::stdout().lock(), &report)?;
    Ok(())
}
