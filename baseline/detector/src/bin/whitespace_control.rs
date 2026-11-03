//! A deterministic formatting view of frozen test records. No new writing occurs.
use anyhow::{Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{self, OriginRecord, Split};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const TRANSFORMATION: &str = "slop-ninja-unicode-whitespace-control-v1";
const RULE: &str = "Collapse each maximal run of Unicode White_Space code points to one ASCII space (U+0020), and remove leading/trailing whitespace. The fixed set is U+0009..U+000D, U+0020, U+0085, U+00A0, U+1680, U+2000..U+200A, U+2028, U+2029, U+202F, U+205F, U+3000. Preserve every other character in order.";

#[derive(Parser)]
#[command(about = "Create a separate whitespace-normalized view of frozen test families")]
struct Args {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    output_dir: PathBuf,
    /// Derive all frozen partitions for a separately declared training experiment.
    #[arg(long)]
    all_splits: bool,
}

fn is_white_space(c: char) -> bool {
    matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{0085}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}')
}

fn collapse_whitespace(text: &str) -> String {
    text.split(is_white_space)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn derive_views(
    records: &[OriginRecord],
    original_manifest_sha256: &str,
    all_splits: bool,
) -> Result<(Vec<OriginRecord>, Vec<Value>)> {
    dataset::validate_records(records)?;
    ensure!(
        records.iter().all(|r| r.split.is_some()),
        "Input must contain frozen partitions"
    );
    let selected: Vec<_> = records
        .iter()
        .filter(|r| all_splits || r.split == Some(Split::Test))
        .collect();
    ensure!(
        !selected.is_empty() && selected.len() <= 10_000,
        "Formatting control requires 1..10000 selected frozen records"
    );
    let mut views = Vec::new();
    let mut bindings = Vec::new();
    for original in selected {
        let mut view = original.clone();
        view.text = collapse_whitespace(&original.text);
        view.text_sha256 = dataset::sha256(&view.text);
        if let Some(rights) = &mut view.rights.share_alike {
            rights.changes.push_str(" Later formatting view: Unicode whitespace collapsed; non-whitespace characters preserved.");
        }
        let scope = if all_splits { "all-split" } else { "test" };
        view.evidence_notes.push_str(&format!(" Formatting-derived {scope} view {TRANSFORMATION}. {RULE} Original text SHA256: {}. Original manifest SHA256: {original_manifest_sha256}. Origin/evidence labels and source/generation metadata describe the original writing and are retained as lineage; no new writing or model invocation occurred. The derived text is not asserted to be the raw model response.",original.text_sha256));
        view.validate()?;
        bindings.push(json!({"id":view.id,"parent_id":view.parent_id,"source_group":view.source_group,"split":view.split,"origin":view.origin,"original_text_sha256":original.text_sha256,"derived_text_sha256":view.text_sha256,"original_record_sha256":dataset::sha256(serde_json::to_vec(original)?),"derived_record_sha256":dataset::sha256(serde_json::to_vec(&view)?),"text_changed":original.text!=view.text}));
        views.push(view);
    }
    // This also rejects a test descendant whose required ancestor is absent.
    dataset::validate_records(&views)?;
    Ok((views, bindings))
}

fn write_identical_or_new(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        ensure!(
            fs::read(path)? == bytes,
            "Existing formatting-control artifact differs: {}",
            path.display()
        );
    } else {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    let original_manifest_sha256 = dataset::sha256(fs::read(&args.input)?);
    let records = dataset::read_records(&args.input)?;
    ensure!(
        dataset::sha256(fs::read(&args.input)?) == original_manifest_sha256,
        "Input manifest changed while reading; wait for the admitted cohort to be frozen"
    );
    let (views, bindings) = derive_views(&records, &original_manifest_sha256, args.all_splits)?;
    fs::create_dir_all(&args.output_dir)?;
    let output = args.output_dir.join("records.jsonl");
    if output.exists() {
        ensure!(
            output.canonicalize()? != args.input.canonicalize()?,
            "Formatting control requires a separate output manifest"
        );
        let existing = dataset::read_records(&output)?;
        ensure!(
            serde_json::to_value(&existing)? == serde_json::to_value(&views)?,
            "Existing derived records differ"
        );
    } else {
        dataset::write_records(&output, &views)?;
    }
    let summary = json!({"schema":"slop-ninja-formatting-control-summary-v1","transformation":TRANSFORMATION,"rule":RULE,"original_manifest_sha256":original_manifest_sha256,"derived_manifest_sha256":dataset::sha256(fs::read(&output)?),"record_hash_encoding":"SHA256 of serde_json::to_vec(OriginRecord); manifest hashes cover exact file bytes","selection":if args.all_splits { "all frozen records; preserve partitions and ancestor closure" } else { "all and only existing Split::Test records; preserve ancestor closure" },"identity_policy":"IDs are preserved within a separate derived-view manifest. Do not concatenate with the original corpus or treat the views as new independent observations.","lineage_policy":"Original source and generation metadata remain lineage evidence. They do not describe new writing or a new raw model response.","changed_texts":bindings.iter().filter(|b|b["text_changed"]==true).count(),"summary":dataset::summarize(&views),"records":bindings});
    write_identical_or_new(
        &args.output_dir.join("summary.json"),
        &serde_json::to_vec_pretty(&summary)?,
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"records":views.len(),"changed_texts":summary["changed_texts"],"transformation":TRANSFORMATION,"original_manifest_sha256":original_manifest_sha256,"derived_manifest_sha256":summary["derived_manifest_sha256"]})
        )?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dataset::{Evidence, Origin, RECORD_SCHEMA, Rights, Source};

    fn fixture(id: &str, text: &str, split: Split) -> OriginRecord {
        OriginRecord {
            schema: RECORD_SCHEMA.into(),
            id: id.into(),
            source_group: id.into(),
            split: Some(split),
            origin: Origin::HumanOnly,
            evidence: Evidence::SyntheticFixture,
            evidence_notes: "Synthetic test text; no human-authorship claim.".into(),
            text: text.into(),
            text_sha256: dataset::sha256(text),
            source: Source {
                collection: "synthetic_fixture".into(),
                url: "https://example.invalid/fixture".into(),
                version: "v1".into(),
                published_at: None,
                author_ids: vec![],
                raw_path: "synthetic".into(),
                raw_sha256: dataset::sha256(text),
                extraction: "synthetic-v1".into(),
            },
            rights: Rights {
                license: "Synthetic-Original".into(),
                evidence_url: "https://example.invalid/fixture-rights".into(),
                evidence_sha256: dataset::sha256("synthetic rights"),
                attribution: "Synthetic test fixture".into(),
                commercial_training: true,
                model_release: true,
                external_evaluation: false,
                redistribute_text: true,
                share_alike: None,
            },
            parent_id: None,
            generation: None,
        }
    }

    #[test]
    fn preserves_test_family_words_punctuation_and_lineage() {
        let root = fixture(
            "test-root",
            "  A\u{00a0}small\tcheck.\n\nKeep punctuation—exactly!\u{3000}",
            Split::Test,
        );
        let mut child = fixture(
            "test-child",
            "  Revised\u{2028}wording;\u{0085}keep\u{202f}it. ",
            Split::Test,
        );
        child.parent_id = Some(root.id.clone());
        child.source_group = root.source_group.clone();
        child.origin = Origin::Mixed;
        let train = fixture(
            "training-root",
            "A separate training example.",
            Split::Train,
        );
        let original = vec![root, child, train];
        let (views, bindings) =
            derive_views(&original, &dataset::sha256("manifest"), false).unwrap();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].text, "A small check. Keep punctuation—exactly!");
        assert_eq!(views[1].parent_id.as_deref(), Some("test-root"));
        for (before, after) in original.iter().zip(&views) {
            assert_eq!(
                before.text.split_whitespace().collect::<Vec<_>>(),
                after.text.split_whitespace().collect::<Vec<_>>()
            );
            assert_eq!(
                before
                    .text
                    .chars()
                    .filter(|c| !is_white_space(*c))
                    .collect::<String>(),
                after
                    .text
                    .chars()
                    .filter(|c| !is_white_space(*c))
                    .collect::<String>()
            );
            assert_eq!(before.id, after.id);
            assert_eq!(before.origin, after.origin);
            assert_eq!(
                serde_json::to_value(&before.source).unwrap(),
                serde_json::to_value(&after.source).unwrap()
            );
        }
        assert_eq!(bindings.len(), 2);
        let (all_views, _) = derive_views(&original, &dataset::sha256("manifest"), true).unwrap();
        assert_eq!(all_views.len(), 3);
        assert_eq!(all_views[2].split, Some(Split::Train));
        assert!(
            derive_views(
                &original[1..],
                &dataset::sha256("incomplete manifest"),
                false
            )
            .is_err()
        );
    }
}
