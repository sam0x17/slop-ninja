use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
};

const SOURCE: &str = include_str!("v6_source_credits.rs");
const SPLITS: [&str; 3] = ["train", "development", "calibration"];

/// Extract release attribution from the frozen v6 fitting corpus.
#[derive(Parser)]
struct Args {
    #[arg(long)]
    run_root: PathBuf,
    /// SHA256 recorded before fitting, not a newly accepted protocol identity.
    #[arg(long)]
    protocol_sha256: String,
    #[arg(long)]
    output: PathBuf,
}

// Unknown top-level fields, including text and evidence_notes, are discarded
// during deserialization. Only these attribution fields can reach the output.
#[derive(Deserialize, Serialize)]
struct Credit {
    #[serde(skip_serializing)]
    schema: String,
    id: String,
    origin: String,
    evidence: String,
    parent_id: Option<String>,
    source_group: String,
    split: String,
    text_sha256: String,
    source: Value,
    rights: Value,
    generation: Option<Value>,
}

fn hash(path: &Path) -> String {
    let mut input = File::open(path).expect("Cannot read pinned input");
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = input.read(&mut buffer).expect("Cannot hash input");
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    format!("{:x}", digest.finalize())
}

fn read_json(path: &Path) -> Value {
    serde_json::from_reader(File::open(path).expect("Cannot read metadata"))
        .expect("Invalid metadata JSON")
}

fn verify_public_metadata(value: &Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                assert!(
                    !matches!(
                        key.as_str(),
                        "text"
                            | "raw_path"
                            | "prompt"
                            | "predictions"
                            | "raw_response"
                            | "response_body"
                    ),
                    "Excluded content field remains in attribution output"
                );
                verify_public_metadata(value);
            }
        }
        Value::Array(values) => values.iter().for_each(verify_public_metadata),
        Value::String(value) => {
            assert!(
                !["/Users/", "/home/", "grammar-private", "file://"]
                    .iter()
                    .any(|prefix| value.contains(prefix)),
                "Private or local absolute path remains in attribution output"
            );
        }
        _ => {}
    }
}

fn main() {
    let args = Args::parse();
    let root = args.run_root;
    let source_sha256 = format!("{:x}", Sha256::digest(SOURCE.as_bytes()));
    let output = args.output;
    assert!(!output.exists(), "Attribution output must be new");
    let protocol_path = root.join("encoder-frontier-v6/protocol.json");
    let protocol_hash = hash(&protocol_path);
    assert_eq!(
        protocol_hash, args.protocol_sha256,
        "Protocol differs from the supplied pre-fit identity"
    );
    let protocol = read_json(&protocol_path);
    let rights_path = root.join("shards-v1/data_rights.json");
    let rights_hash = hash(&rights_path);
    assert_eq!(
        rights_hash,
        protocol["data_rights_sha256"].as_str().unwrap()
    );
    let rights = read_json(&rights_path);
    let notices: BTreeMap<_, _> = rights["content"]["share_alike_notices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|notice| (notice["record_id"].as_str().unwrap(), notice))
        .collect();
    assert_eq!(
        notices.len(),
        rights["content"]["share_alike_notices"]
            .as_array()
            .unwrap()
            .len()
    );
    let mut records = Vec::new();
    let mut partitions = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut family_splits = BTreeMap::new();
    let mut collection_rows: BTreeMap<String, usize> = BTreeMap::new();
    let mut collection_works: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut row_licenses: BTreeMap<String, usize> = BTreeMap::new();
    let mut human_work_licenses: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut share_alike_rows = 0;
    for split in SPLITS {
        let path = root.join("shards-v1").join(format!("{split}.jsonl"));
        let expected = protocol["partition_sha256"][split].as_str().unwrap();
        assert_eq!(
            hash(&path),
            expected,
            "Shard differs from the frozen protocol"
        );
        let mut count = 0;
        for line in BufReader::new(File::open(&path).unwrap()).lines() {
            let mut record: Credit = serde_json::from_str(&line.expect("Cannot read shard row"))
                .expect("Invalid attribution record");
            assert_eq!(record.schema, "slop-ninja-origin-record-v1");
            assert_eq!(record.split, split);
            assert!(matches!(
                record.origin.as_str(),
                "human_only" | "model_only" | "mixed"
            ));
            assert!(
                ids.insert(record.id.clone()),
                "Duplicate attribution record ID"
            );
            assert_eq!(
                family_splits
                    .entry(record.source_group.clone())
                    .or_insert(split),
                &split
            );
            assert_eq!(record.rights["commercial_training"], true);
            assert_eq!(record.rights["model_release"], true);
            assert!(
                !record.rights["attribution"]
                    .as_str()
                    .unwrap()
                    .trim()
                    .is_empty()
            );
            record.source.as_object_mut().unwrap().remove("raw_path");
            if let Some(generation) = &mut record.generation {
                generation.as_object_mut().unwrap().remove("prompt");
            }
            let license = record.rights["license"].as_str().unwrap().to_owned();
            let collection = record.source["collection"].as_str().unwrap().to_owned();
            *collection_rows.entry(collection.clone()).or_default() += 1;
            collection_works
                .entry(collection)
                .or_default()
                .insert(record.source_group.clone());
            *row_licenses.entry(license.clone()).or_default() += 1;
            if record.origin == "human_only" {
                human_work_licenses
                    .entry(license.clone())
                    .or_default()
                    .insert(record.source_group.clone());
            }
            if license.starts_with("CC-BY-SA-") {
                let notice = notices
                    .get(record.id.as_str())
                    .expect("Missing ShareAlike notice");
                assert_eq!(notice["text_sha256"], record.text_sha256);
                assert_eq!(notice["text_license"], license);
                assert_eq!(notice["attribution"], record.rights["attribution"]);
                assert_eq!(notice["obligations"], record.rights["share_alike"]);
                share_alike_rows += 1;
            }
            let credit = serde_json::to_value(record).unwrap();
            verify_public_metadata(&credit);
            records.push(credit);
            count += 1;
        }
        assert_eq!(rights["content"]["partitions"][split]["rows"], count);
        assert_eq!(rights["content"]["partitions"][split]["sha256"], expected);
        assert_eq!(hash(&path), expected, "Shard changed during extraction");
        partitions.insert(split, json!({"rows":count,"sha256":expected}));
    }
    for record in &records {
        if let Some(parent) = record["parent_id"].as_str() {
            assert!(
                ids.contains(parent),
                "Attribution parent is absent from the included partitions"
            );
        }
    }
    let collections: Vec<_> = collection_rows.iter().map(|(collection, rows)| {
        json!({"collection":collection,"records":rows,"source_works":collection_works[collection].len()})
    }).collect();
    let human_work_license_counts: BTreeMap<_, _> = human_work_licenses
        .iter()
        .map(|(license, works)| (license, works.len()))
        .collect();
    let summary = json!({
        "records":records.len(), "source_works":family_splits.len(), "collections":collections,
        "record_license_counts":row_licenses, "human_work_license_counts":human_work_license_counts,
        "share_alike_records_checked":share_alike_rows,
    });
    let document = json!({
        "schema":"slop_ninja_corpus_attribution_v2",
        "scope":"All Train, Development and Calibration records in the frozen v6 primary corpus, including complete revision ancestry. The selected model's training metadata records the views actually used. This projection excludes corpus text and raw generation prompts.",
        "partitions":partitions,
        "provenance":{
            "protocol_sha256":protocol_hash,"data_rights_sha256":rights_hash,
            "helper_sha256":source_sha256,"executable_sha256":hash(&std::env::current_exe().unwrap()),
            "projection":"id, origin, evidence, parent_id, source_group, split, text_sha256, source without raw_path, rights, generation without prompt",
            "test_partition_opened_by_extractor":false,
        },
        "summary":summary, "records":records,
    });
    verify_public_metadata(&document);
    assert_eq!(hash(&protocol_path), protocol_hash);
    assert_eq!(hash(&rights_path), rights_hash);
    let mut destination = File::create_new(&output).expect("Attribution output must be new");
    serde_json::to_writer_pretty(&mut destination, &document).unwrap();
    destination.write_all(b"\n").unwrap();
    destination.sync_all().unwrap();
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"output":output,"sha256":hash(&output),"summary":summary})
        )
        .unwrap()
    );
}
