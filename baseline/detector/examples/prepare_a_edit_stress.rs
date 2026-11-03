//! Fixed Development interventions, selected without detector observations.
use anyhow::{Result, ensure};
use clap::Parser;
use grammar_core::{edits, rules};
use grammar_spacy::Parser as SyntaxParser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{self, Evidence, Origin, OriginRecord, Split, sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    records: PathBuf,
    #[arg(long)]
    expected_sha256: String,
    #[arg(long)]
    python: String,
    #[arg(long)]
    out: PathBuf,
}
fn save(path: &Path, value: &Value) -> Result<()> {
    let mut f = fs::File::create_new(path)?;
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn choose<'a>(rows: &[&'a OriginRecord]) -> Result<Vec<(&'static str, &'a OriginRecord)>> {
    let human: Vec<_> = rows
        .iter()
        .copied()
        .filter(|r| r.origin == Origin::HumanOnly)
        .collect();
    let draft: Vec<_> = rows
        .iter()
        .copied()
        .filter(|r| r.evidence == Evidence::RecordedModelGeneration)
        .collect();
    ensure!(
        human.len() == 1 && draft.len() == 1,
        "Expected one original human and model draft"
    );
    let next = |parent: &str| -> Result<&'a OriginRecord> {
        let found: Vec<_> = rows
            .iter()
            .copied()
            .filter(|r| {
                r.evidence == Evidence::RecordedModelRevision
                    && r.parent_id.as_deref() == Some(parent)
            })
            .collect();
        ensure!(
            found.len() == 1,
            "Expected a complete unbranched revision chain"
        );
        Ok(found[0])
    };
    let r1 = next(&draft[0].id)?;
    let r2 = next(&r1.id)?;
    ensure!(
        draft[0].parent_id.as_deref() == Some(human[0].id.as_str()),
        "Wrong draft ancestry"
    );
    Ok(vec![
        ("human", human[0]),
        ("draft", draft[0]),
        ("revision2", r2),
    ])
}
fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a new study directory");
    ensure!(
        sha256(fs::read(&args.records)?) == args.expected_sha256,
        "Changed source archive"
    );
    let records = dataset::read_records(&args.records)?;
    let mut families = BTreeMap::<&str, Vec<&OriginRecord>>::new();
    for row in &records {
        ensure!(
            row.split == Some(Split::Development),
            "Stress discovery uses Development only"
        );
        families.entry(&row.source_group).or_default().push(row);
    }
    let mut strata = BTreeMap::<&str, Vec<(&str, String)>>::new();
    for (group, rows) in &families {
        let collection = rows[0].source.collection.as_str();
        ensure!(
            rows.iter().all(|r| r.source.collection == collection),
            "Mixed collection family"
        );
        let rank = sha256(serde_json::to_vec(&("slop-ninja-a-edit-stress-v1", group))?);
        strata.entry(collection).or_default().push((group, rank));
    }
    ensure!(
        strata.len() == 3,
        "Expected the three V6 Development collections"
    );
    let mut selected = vec![];
    for (collection, groups) in &mut strata {
        groups.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(b.0)));
        ensure!(groups.len() >= 4, "Insufficient source families");
        for (group, rank) in groups.iter().take(4) {
            for (stage, row) in choose(&families[group])? {
                selected.push((
                    stage,
                    row,
                    json!({"group":group,"collection":collection,"rank":rank}),
                ));
            }
        }
    }
    fs::create_dir_all(args.out.join("documents"))?;
    let selected_rules: Vec<String> = [
        "contract_negative_auxiliary",
        "expand_negative_auxiliary",
        "split_complete_semicolon_clauses",
        "join_complete_sentences",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    save(
        &args.out.join("protocol.json"),
        &json!({"schema":"slop_ninja_a_edit_stress_protocol_v1",
        "records_sha256":args.expected_sha256,"families_per_collection":4,"stages":["human","draft","revision2"],
        "selection":selected.iter().map(|(stage,row,family)| json!({"stage":stage,"record_id":row.id,"family":family})).collect::<Vec<_>>(),
        "rules":selected_rules,"catalog":rules::CATALOG_VERSION,
        "nomination_policy":"First negative-form change and first two boundary changes by original byte position; enumerate all subsets and retain overlaps as rejections. No detector selection.",
        "parser":"en_core_web_sm with paragraph-content-v2; actual identity recorded per document",
        "review_policy":"Read original and complete candidate texts plus syntax before scoring. Mechanical parsing success is not semantic approval.",
        "training_admitted":false,"provider_calls":0,"detector_calls":0,
        "limits":"Small, previously opened Development cohort. Not a fresh Test or independent Pangram comparison. Parser/rule nominations can be wrong; retain and inspect failures."}),
    )?;
    let mut parser = SyntaxParser {
        python: &args.python,
        model: "en_core_web_sm",
    }
    .stream_with_paragraph_content()?;
    let mut parser_identity = None;
    let mut summary = vec![];
    let mut requests = BTreeMap::new();
    for (i, (stage, row, family)) in selected.iter().enumerate() {
        let source = parser.parse(&row.text)?.map_err(anyhow::Error::msg)?;
        if let Some(previous) = parser_identity.replace(source.parser_identity.clone()) {
            ensure!(
                previous == source.parser_identity,
                "Parser identity changed"
            );
        }
        let mut available =
            rules::generate_selected(&source, rules::MAX_CANDIDATES, &selected_rules)?;
        available.sort_by(|a, b| {
            a.edits[0]
                .start_byte
                .cmp(&b.edits[0].start_byte)
                .then(a.id.cmp(&b.id))
        });
        let mut nominations = vec![];
        let mut negative = 0;
        let mut boundary = 0;
        for candidate in &available {
            let is_negative = candidate.rule.contains("negative_auxiliary");
            if is_negative && negative == 0 {
                negative += 1;
            } else if !is_negative && boundary < 2 {
                boundary += 1;
            } else {
                continue;
            }
            nominations.push(candidate.clone());
        }
        let mut states = vec![];
        let mut rejected = vec![];
        let mut seen = BTreeSet::new();
        for mask in 0..(1 << nominations.len()) {
            let picked: Vec<_> = nominations
                .iter()
                .enumerate()
                .filter(|(j, _)| mask & (1 << j) != 0)
                .map(|(_, c)| c)
                .collect();
            let patches: Vec<_> = picked
                .iter()
                .flat_map(|c| c.edits.iter().cloned())
                .collect();
            let text = match edits::apply(&row.text, &patches) {
                Ok(text) => text,
                Err(error) => {
                    rejected.push(json!({"mask":mask,"error":error.to_string()}));
                    continue;
                }
            };
            let hash = sha256(&text);
            ensure!(
                seen.insert(hash.clone()),
                "Duplicate distinct stress states"
            );
            let parsed = if mask == 0 {
                Ok(source.clone())
            } else {
                parser.parse(&text)?
            };
            let syntax = match parsed {
                Ok(doc) => serde_json::to_value(doc)?,
                Err(error) => json!({"error":error}),
            };
            requests.insert(hash.clone(), text.clone());
            states.push(json!({"id":hash,"mask":mask,"choices":picked.iter().map(|c| &c.id).collect::<Vec<_>>(),
                "text":text,"edits":patches,"syntax":syntax,"semantic_review":"pending"}));
        }
        let path = format!("documents/{i:03}-{stage}.json");
        save(
            &args.out.join(&path),
            &json!({"schema":"slop_ninja_a_edit_stress_document_v1",
            "stage":stage,"source":row,"family":family,"all_nominations":available,
            "selected_nominations":nominations,"states":states,"rejected":rejected,"training_admitted":false}),
        )?;
        summary.push(
            json!({"path":path,"stage":stage,"origin":row.origin,"family":family,
            "source_id":row.id,"source_sha256":row.text_sha256,"nominations":nominations.len(),
            "states":states.len(),"rejected":rejected.len()}),
        );
        eprintln!("Prepared stress document {} of {}", i + 1, selected.len());
    }
    parser.finish()?;
    ensure!(
        sha256(fs::read(&args.records)?) == args.expected_sha256,
        "Source archive changed"
    );
    let mut file = fs::File::create_new(args.out.join("requests-after-review.jsonl"))?;
    for (id, text) in &requests {
        serde_json::to_writer(&mut file, &json!({"id":id,"text":text}))?;
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    save(
        &args.out.join("summary.json"),
        &json!({"schema":"slop_ninja_a_edit_stress_summary_v1",
        "protocol_sha256":sha256(fs::read(args.out.join("protocol.json"))?),
        "requests_sha256":sha256(fs::read(args.out.join("requests-after-review.jsonl"))?),
        "parser_identity":parser_identity,"documents":summary,"unique_texts":requests.len(),
        "semantic_review_complete":false,"training_admitted":false,"detector_calls":0,"provider_calls":0}),
    )?;
    println!(
        "{}",
        json!({"status":"prepared_for_manual_review","documents":selected.len(),"unique_texts":requests.len()})
    );
    Ok(())
}
