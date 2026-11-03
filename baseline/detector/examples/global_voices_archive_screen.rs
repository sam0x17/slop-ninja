//! Inspect historical-date coverage in the unchanged saved publisher archive.
use anyhow::{Context, Result, ensure};
use chrono::NaiveDateTime;
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const RECORDS: &str = "88ba5f33757d72d78a1b43bad01829102b130be3f74bee6d8ac1514a442d02e7";
#[derive(Parser)]
struct Args {
    #[arg(long)]
    cache: PathBuf,
    #[arg(long)]
    out: PathBuf,
}
fn s(v: &Value) -> Result<&str> {
    v.as_str().context("Expected string")
}
fn dt(v: &Value) -> Result<NaiveDateTime> {
    Ok(NaiveDateTime::parse_from_str(s(v)?, "%Y-%m-%dT%H:%M:%S")?)
}
fn write(path: &Path, value: &Value) -> Result<()> {
    let file = fs::File::create_new(path)?;
    serde_json::to_writer_pretty(file, value)?;
    Ok(())
}
fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a fresh output directory");
    let bytes = fs::read(args.cache.join("records.json"))?;
    ensure!(sha256(&bytes) == RECORDS, "Changed legacy records");
    let rows: Vec<Value> = serde_json::from_slice(&bytes)?;
    ensure!(rows.len() == 840, "Changed observation count");
    let mut bindings = BTreeMap::new();
    let mut responses = BTreeMap::new();
    for entry in fs::read_dir(args.cache.join("raw"))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let meta_bytes = fs::read(&path)?;
        let meta: Value = serde_json::from_slice(&meta_bytes)?;
        let url = s(&meta["url"])?;
        ensure!(
            path.file_stem().and_then(|p| p.to_str()) == Some(sha256(url).as_str()),
            "Cache URL key mismatch"
        );
        let body = fs::read(path.with_extension("body"))?;
        ensure!(
            meta["status"] == 200 && sha256(&body) == meta["sha256"],
            "Changed cached response"
        );
        bindings.insert(path.clone(), sha256(meta_bytes));
        bindings.insert(path.with_extension("body"), sha256(&body));
        ensure!(
            responses.insert(url.to_owned(), (meta, body)).is_none(),
            "Duplicate response URL"
        );
    }
    ensure!(responses.len() == 62, "Changed saved-response count");
    let cutoff = NaiveDateTime::parse_from_str("2022-11-01T00:00:00", "%Y-%m-%dT%H:%M:%S")?;
    let selected_old: Value =
        serde_json::from_slice(&fs::read(args.cache.join("source-manifest.json"))?)?;
    let old_ids: BTreeSet<_> = selected_old["articles"]
        .as_array()
        .context("Missing previous selection")?
        .iter()
        .map(|r| s(&r["id"]))
        .collect::<Result<_>>()?;
    ensure!(old_ids.len() == 168, "Changed previous selected works");
    let mut years = BTreeMap::<String, BTreeSet<String>>::new();
    let mut exclusions = BTreeMap::<String, usize>::new();
    let mut all = BTreeSet::new();
    let mut published_old = BTreeSet::new();
    let mut both_old = BTreeSet::new();
    let mut ordered_both_old = BTreeSet::new();
    let mut eligible = BTreeMap::<String, Value>::new();
    let mut anomalies = Vec::new();
    let mut eligible_observations = 0;
    for row in &rows {
        let id = s(&row["id"])?;
        all.insert(id.to_owned());
        let p = &row["provenance"];
        ensure!(
            sha256(s(&row["text"])?) == row["text_sha256"],
            "Source text changed"
        );
        let (meta, body) = responses
            .get(s(&p["api_url"])?)
            .context("Missing original API response")?;
        ensure!(meta["sha256"] == p["api_sha256"], "API binding mismatch");
        let posts: Vec<Value> = serde_json::from_slice(body)?;
        let matches: Vec<_> = posts
            .iter()
            .filter(|a| a["link"] == p["source_url"])
            .collect();
        ensure!(matches.len() == 1, "Missing or duplicated source link");
        let post = matches[0];
        ensure!(
            id == format!("gv:{}", post["id"])
                && post["date_gmt"] == p["publication_gmt"]
                && post["modified_gmt"] == p["modification_gmt"]
                && sha256(s(&post["content"]["rendered"])?) == p["content_html_sha256"],
            "Publisher metadata/content mismatch"
        );
        for (url_key, sha_key) in [
            ("archive_url", "archive_sha256"),
            ("license_evidence_url", "license_evidence_sha256"),
        ] {
            ensure!(
                responses
                    .get(s(&p[url_key])?)
                    .context("Missing provenance response")?
                    .0["sha256"]
                    == p[sha_key],
                "Provenance binding mismatch"
            );
        }
        let reasons = row["reasons"]
            .as_array()
            .context("Missing extraction reasons")?;
        for reason in reasons {
            *exclusions.entry(s(reason)?.to_owned()).or_default() += 1;
        }
        if reasons.is_empty() {
            eligible_observations += 1;
        }
        let (Ok(publication), Ok(modification)) =
            (dt(&p["publication_gmt"]), dt(&p["modification_gmt"]))
        else {
            anomalies
                .push(json!({"id":id,"author_id":row["author_id"],"reason":"invalid_timestamp"}));
            continue;
        };
        years
            .entry(publication.format("%Y").to_string())
            .or_default()
            .insert(id.to_owned());
        let retrieved =
            chrono::DateTime::parse_from_rfc3339(s(&meta["retrieved_at"])?)?.naive_utc();
        if publication < cutoff {
            published_old.insert(id.to_owned());
        }
        if publication < cutoff && modification < cutoff {
            both_old.insert(id.to_owned());
        }
        let mut flags = Vec::new();
        if modification < publication {
            flags.push("modification_before_publication");
        }
        if publication > retrieved {
            flags.push("publication_after_retrieval");
        }
        if modification > retrieved {
            flags.push("modification_after_retrieval");
        }
        if !flags.is_empty() {
            anomalies.push(json!({"id":id,"author_id":row["author_id"],"flags":flags}));
            continue;
        }
        if publication < cutoff && modification < cutoff {
            ordered_both_old.insert(id.to_owned());
            if reasons.is_empty() {
                ensure!(
                    eligible.insert(id.to_owned(), row.clone()).is_none(),
                    "Multiple eligible writers for one work"
                );
            }
        }
    }
    ensure!(all.len() == 820, "Changed distinct-work population");
    let mut ranked = eligible
        .values()
        .map(|r| {
            Ok((
                sha256(serde_json::to_vec(&(
                    "slop-ninja-global-voices-source-screen-v1",
                    s(&r["id"])?,
                ))?),
                r,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    ranked.sort_by(|a, b| (&a.0, s(&a.1["id"]).unwrap()).cmp(&(&b.0, s(&b.1["id"]).unwrap())));
    let selected: Vec<_> = ranked
        .iter()
        .take(8)
        .map(|(rank, row)| json!({"selection_sha256":rank,"record":row}))
        .collect();
    let mut authors = BTreeMap::<String, usize>::new();
    for row in eligible.values() {
        *authors.entry(s(&row["author_id"])?.to_owned()).or_default() += 1;
    }
    for name in [
        "records.json",
        "plan.json",
        "summary.json",
        "source-manifest.json",
    ] {
        let path = args.cache.join(name);
        bindings.insert(path.clone(), sha256(fs::read(path)?));
    }
    fs::create_dir(&args.out)?;
    let packet = json!({"schema":"slop_ninja_global_voices_source_review_packet_v1","selected":selected,"manual_review_complete":false,"commercial_training_admitted":false});
    write(&args.out.join("packet.json"), &packet)?;
    let report = json!({"schema":"slop_ninja_global_voices_source_screen_v1","at":chrono::Utc::now().to_rfc3339(),
        "observations":rows.len(),"distinct_works":all.len(),"verified_saved_responses":responses.len(),
        "legacy_eligible_observations":eligible_observations,"legacy_exclusion_counts_overlap":exclusions,
        "distinct_publication_years":years.iter().map(|(year,ids)|(year,ids.len())).collect::<BTreeMap<_,_>>(),
        "cutoff_exclusive":"2022-11-01T00:00:00Z","published_before_cutoff":published_old.len(),
        "publication_and_modification_before_cutoff":both_old.len(),"ordered_publication_and_modification_before_cutoff":ordered_both_old.len(),"date_and_legacy_eligible_works":eligible.len(),
        "eligible_by_author_account":authors,"eligible_in_prior_author_experiment":eligible.keys().filter(|id|old_ids.contains(id.as_str())).count(),
        "timestamp_flags":anomalies,"selection_requires_ordered_timestamps":true,"selected_for_review":selected.len(),
        "selected":selected.iter().map(|x|json!({"selection_sha256":x["selection_sha256"],"id":x["record"]["id"],"text_sha256":x["record"]["text_sha256"],"author_id":x["record"]["author_id"],"date":x["record"]["date"],"source_url":x["record"]["provenance"]["source_url"]})).collect::<Vec<_>>(),
        "packet_sha256":sha256(fs::read(args.out.join("packet.json"))?),"bindings":bindings,
        "source_sha256":sha256(include_bytes!("global_voices_archive_screen.rs")),"protocol_sha256":sha256(include_bytes!("../GLOBAL_VOICES_SOURCE_SCREEN_V1.md")),
        "manual_review_complete":false,"historical_bytes_independently_verified":false,"commercial_training_admitted":false,
        "model_calls":0,"pangram_calls":0,"new_source_requests":0,"training_changed":false});
    write(&args.out.join("report.json"), &report)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
