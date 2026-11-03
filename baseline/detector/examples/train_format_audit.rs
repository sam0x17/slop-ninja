//! Describe formatting confounds in the unchanged Train archive.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{Origin, OriginRecord, Split, read_records, sha256},
    model::CLASS_NAMES,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::PathBuf,
};

const RAW_SHA: &str = "023d6e5428236502e877bac0afe0e9e9ee4aa9d10ab38eb940ca8e915cf7502e";
const WHITE_SHA: &str = "5074105ad388d664106b62b0d82f2faf7dbc7b6771b09a2ec2ecb26dbde64ab8";
type Flags = BTreeMap<&'static str, bool>;
type Observation<'a> = (&'a OriginRecord, Flags);

#[derive(Parser)]
struct Args {
    #[arg(long)]
    train: PathBuf,
    #[arg(long)]
    whitespace: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

fn flags(text: &str, canonical: &str) -> Flags {
    BTreeMap::from([
        ("leading_ascii_space", text.starts_with(' ')),
        (
            "leading_unicode_whitespace",
            text.chars().next().is_some_and(char::is_whitespace),
        ),
        (
            "trailing_unicode_whitespace",
            text.chars().next_back().is_some_and(char::is_whitespace),
        ),
        ("contains_lf", text.contains('\n')),
        ("contains_exact_lf_lf", text.contains("\n\n")),
        ("contains_cr", text.contains('\r')),
        ("contains_tab", text.contains('\t')),
        ("contains_nbsp", text.contains('\u{a0}')),
        ("contains_narrow_nbsp", text.contains('\u{202f}')),
        ("contains_repeated_ascii_space", text.contains("  ")),
        ("canonical_differs", text != canonical),
        (
            "changed_only_by_boundary_trim",
            text != canonical && text.trim() == canonical,
        ),
        (
            "changed_only_by_removing_one_leading_ascii_space",
            text.strip_prefix(' ') == Some(canonical),
        ),
    ])
}

fn summarize(rows: &[Observation<'_>]) -> Value {
    let families: BTreeSet<_> = rows.iter().map(|(r, _)| &r.source_group).collect();
    let mut cells: BTreeMap<(&str, usize), Vec<&Flags>> = BTreeMap::new();
    for (r, f) in rows {
        cells
            .entry((&r.source_group, r.origin.index()))
            .or_default()
            .push(f);
    }
    let measures: BTreeMap<_, _> = rows[0].1.keys().map(|&name| {
        let count = rows.iter().filter(|(_, f)| f[name]).count();
        let rate = cells.values().map(|cell| cell.iter().filter(|f| f[name]).count() as f64 / cell.len() as f64).sum::<f64>() / cells.len() as f64;
        (name, json!({"rows":count,"row_fraction":count as f64 / rows.len() as f64,"equal_family_origin_fraction":rate}))
    }).collect();
    json!({"rows":rows.len(),"source_families":families.len(),"family_origin_cells":cells.len(),"features":measures})
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a new report path");
    ensure!(
        sha256(fs::read(&args.train)?) == RAW_SHA
            && sha256(fs::read(&args.whitespace)?) == WHITE_SHA,
        "Changed Train archive"
    );
    let mut raw = read_records(&args.train)?;
    let mut white = read_records(&args.whitespace)?;
    raw.sort_by(|a, b| a.id.cmp(&b.id));
    white.sort_by(|a, b| a.id.cmp(&b.id));
    ensure!(
        raw.len() == 3833 && raw.len() == white.len(),
        "Changed coverage"
    );
    let roots: BTreeMap<_, _> = raw
        .iter()
        .filter(|r| r.origin == Origin::HumanOnly)
        .map(|r| (&r.source_group, &r.source.collection))
        .collect();
    ensure!(roots.len() == 471, "Changed human roots");
    let mut all = Vec::new();
    let mut buckets: BTreeMap<Vec<String>, Vec<Observation<'_>>> = BTreeMap::new();
    for (a, b) in raw.iter().zip(&white) {
        ensure!(
            a.split == Some(Split::Train) && b.split == Some(Split::Train),
            "Wrong partition"
        );
        ensure!(
            a.id == b.id
                && a.source_group == b.source_group
                && a.origin == b.origin
                && a.parent_id == b.parent_id
                && a.evidence == b.evidence,
            "Changed identity or provenance"
        );
        ensure!(
            a.text.split_whitespace().collect::<Vec<_>>().join(" ") == b.text,
            "Wrong canonical view"
        );
        let f = flags(&a.text, &b.text);
        let origin = CLASS_NAMES[a.origin.index()].to_owned();
        let collection = roots.get(&a.source_group).context("Missing human root")?;
        let mut keys = vec![
            vec!["origin".into(), origin.clone()],
            vec![
                "collection_origin".into(),
                (*collection).clone(),
                origin.clone(),
            ],
        ];
        if let Some(g) = &a.generation {
            keys.push(vec![
                "generator_operation_origin".into(),
                g.model_revision.clone(),
                g.operation.clone(),
                origin,
            ]);
        }
        for key in keys {
            buckets.entry(key).or_default().push((a, f.clone()));
        }
        all.push((a, f));
    }
    ensure!(
        (0..3)
            .map(|i| all.iter().filter(|(r, _)| r.origin.index() == i).count())
            .collect::<Vec<_>>()
            == [471, 2135, 1227],
        "Changed origin counts"
    );
    let summaries: Vec<_> = buckets
        .iter()
        .map(|(key, rows)| json!({"key":key,"summary":summarize(rows)}))
        .collect();
    ensure!(
        sha256(fs::read(&args.train)?) == RAW_SHA
            && sha256(fs::read(&args.whitespace)?) == WHITE_SHA,
        "Inputs changed during audit"
    );
    let report = json!({"schema":"slop_ninja_train_format_audit_v1","at":chrono::Utc::now(),"source_sha256":sha256(include_bytes!("train_format_audit.rs")),"protocol_sha256":sha256(include_bytes!("../TRAIN_FORMAT_AUDIT_V1.md")),"cargo_lock_sha256":sha256(include_bytes!("../Cargo.lock")),"binary_sha256":sha256(fs::read(std::env::current_exe()?)?),"raw_sha256":RAW_SHA,"whitespace_sha256":WHITE_SHA,"coverage":summarize(&all),"buckets":summaries,"input_views_verified":true,"model_calls":0,"provider_calls":0,"data_or_training_changed":false});
    let mut out = fs::File::create_new(&args.out)?;
    serde_json::to_writer_pretty(&mut out, &report)?;
    out.write_all(b"\n")?;
    println!(
        "{} Train rows in {} report buckets",
        all.len(),
        summaries.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_boundaries_and_literal_layout_have_distinct_counts() {
        let f = flags(" One\n\npoint. ", "One point.");
        assert!(f["leading_ascii_space"] && f["contains_exact_lf_lf"]);
        assert!(!f["changed_only_by_boundary_trim"]);
        let f = flags("\u{a0}One point.\u{202f}", "One point.");
        assert!(
            f["changed_only_by_boundary_trim"] && f["contains_nbsp"] && f["contains_narrow_nbsp"]
        );
        assert!(!f["leading_ascii_space"]);
        assert!(
            flags(" One point.", "One point.")["changed_only_by_removing_one_leading_ascii_space"]
        );
        assert!(
            !flags("One point.", "One point.")["changed_only_by_removing_one_leading_ascii_space"]
        );
    }
}
