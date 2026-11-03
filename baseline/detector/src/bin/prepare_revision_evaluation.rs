//! Freeze the declared v6 source allocation and exact per-composer profile inputs.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde::Serialize;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{self, Evidence, Origin, OriginRecord, Split, sha256},
    prompt_profiles::{self, Selection},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

const INPUT_SHA256: &str = "195a632721f370f84b5ab3bc8c71a5e7f620fdb1c3a4796e04c639cef2887ad4";
const SPLIT_DOMAIN: &str = "slop-ninja-v6-evaluation-split-v1";
const COMPOSER_DOMAIN: &str = "slop-ninja-v6-evaluation-composer-v1";
const COMPOSERS: [&str; 3] = ["qwen", "mistral", "olmo"];
const SPLITS: [Split; 3] = [Split::Development, Split::Calibration, Split::Test];
const PROFILES: [&str; 6] = [
    "source-matched",
    "plain",
    "informal",
    "direct",
    "anti-ai",
    "fix-slop",
];

#[derive(Parser)]
#[command(
    about = "Freeze v6 evaluation roots and profile plans after protocol declaration; no generation"
)]
struct Args {
    /// Exact admitted, unsplit 351-source input.
    #[arg(long)]
    input: PathBuf,
    /// Frozen protocol describing these assignments and both generation routes.
    #[arg(long)]
    protocol: PathBuf,
    /// Explicit protocol binding required before writing any source split.
    #[arg(long)]
    expected_protocol_sha256: String,
    /// New directory in ignored data; contains private source identities.
    #[arg(long)]
    output_dir: PathBuf,
}

#[derive(Clone)]
struct FamilyKey {
    source_group: String,
    collection: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct Assignment {
    split: Split,
    split_rank_sha256: String,
    split_flattened_index: usize,
    composer: &'static str,
    composer_rank_sha256: String,
    composer_flattened_index: usize,
}

fn assignments(keys: &[FamilyKey]) -> Result<BTreeMap<String, Assignment>> {
    let mut buckets = BTreeMap::<&str, Vec<(String, &str)>>::new();
    let mut seen = BTreeSet::new();
    for key in keys {
        ensure!(
            !key.source_group.is_empty()
                && !key.collection.is_empty()
                && seen.insert(key.source_group.as_str()),
            "Expected unique, nonempty source families"
        );
        buckets.entry(&key.collection).or_default().push((
            sha256(serde_json::to_vec(&(SPLIT_DOMAIN, &key.source_group))?),
            &key.source_group,
        ));
    }
    let mut assigned = BTreeMap::new();
    let flattened = buckets.values_mut().flat_map(|bucket| {
        bucket.sort();
        bucket.iter()
    });
    for (index, (rank, group)) in flattened.enumerate() {
        assigned.insert(
            (*group).to_owned(),
            Assignment {
                split: SPLITS[index % SPLITS.len()],
                split_rank_sha256: rank.clone(),
                split_flattened_index: index,
                composer: "",
                composer_rank_sha256: String::new(),
                composer_flattened_index: 0,
            },
        );
    }
    for split in SPLITS {
        let mut buckets = BTreeMap::<&str, Vec<(String, &str)>>::new();
        for key in keys
            .iter()
            .filter(|key| assigned[&key.source_group].split == split)
        {
            buckets.entry(&key.collection).or_default().push((
                sha256(serde_json::to_vec(&(COMPOSER_DOMAIN, &key.source_group))?),
                &key.source_group,
            ));
        }
        let flattened = buckets.values_mut().flat_map(|bucket| {
            bucket.sort();
            bucket.iter()
        });
        for (index, (rank, group)) in flattened.enumerate() {
            let assignment = assigned.get_mut(*group).unwrap();
            assignment.composer = COMPOSERS[index % COMPOSERS.len()];
            assignment.composer_rank_sha256 = rank.clone();
            assignment.composer_flattened_index = index;
        }
    }
    Ok(assigned)
}

fn record_bytes(records: &[OriginRecord]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend(serde_json::to_vec(record)?);
        bytes.push(b'\n');
    }
    Ok(bytes)
}

struct Cohort {
    roots: Vec<u8>,
    profiles: Vec<u8>,
    summary: Value,
}

fn cohort(records: &[OriginRecord], selection: &Selection) -> Result<Cohort> {
    let roots = record_bytes(records)?;
    let input_sha256 = sha256(&roots);
    let profiles = prompt_profiles::assign(records, &input_sha256, selection)?;
    ensure!(
        profiles.len() == records.len(),
        "Profile plan is missing a source family"
    );
    let mut counts = BTreeMap::<String, usize>::new();
    let mut split_counts = BTreeMap::<Split, BTreeMap<String, usize>>::new();
    for profile in profiles.values() {
        ensure!(
            profile.assignment.input_sha256 == input_sha256,
            "Profile input differs"
        );
        *counts.entry(profile.profile.id.clone()).or_default() += 1;
        *split_counts
            .entry(profile.assignment.split)
            .or_default()
            .entry(profile.profile.id.clone())
            .or_default() += 1;
    }
    // generate_samples serializes this exact map without added whitespace/newline.
    let profile_bytes = serde_json::to_vec(&profiles)?;
    let summary = json!({
        "roots_sha256":input_sha256,"profile_plan_sha256":sha256(&profile_bytes),
        "dataset":dataset::summarize(records),"profile_counts":counts,"profile_counts_by_split":split_counts,
        "maximum_initial_calls":records.len()*2,"maximum_first_revision_calls":records.len(),"maximum_second_revision_calls":records.len()
    });
    Ok(Cohort {
        roots,
        profiles: profile_bytes,
        summary,
    })
}

fn prepare(args: Args) -> Result<Value> {
    ensure!(
        !args.output_dir.exists(),
        "Use a new immutable output directory"
    );
    ensure!(
        args.expected_protocol_sha256.len() == 64
            && args
                .expected_protocol_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit()),
        "Expected a protocol SHA256"
    );
    let protocol = fs::read(&args.protocol)?;
    ensure!(
        sha256(&protocol) == args.expected_protocol_sha256,
        "Protocol bytes differ from the explicit freeze"
    );
    let input = fs::read(&args.input)?;
    ensure!(
        sha256(&input) == INPUT_SHA256,
        "Expected the exact admitted 351-source input"
    );
    let mut records: Vec<OriginRecord> = std::str::from_utf8(&input)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<serde_json::Result<_>>()?;
    dataset::validate_records(&records)?;
    ensure!(
        records.len() == 351
            && records.iter().all(|r| r.split.is_none()
                && r.origin == Origin::HumanOnly
                && r.evidence == Evidence::HistoricalProxy
                && r.parent_id.is_none()
                && r.generation.is_none()
                && r.rights.commercial_training
                && r.rights.model_release
                && r.rights.external_evaluation
                && r.rights.redistribute_text),
        "Expected 351 unsplit, rights-admitted historical human roots"
    );
    let expected = BTreeMap::from([
        ("cmu-books-historical".to_string(), 151),
        ("plos_one".to_string(), 100),
        ("wikinews".to_string(), 100),
    ]);
    ensure!(
        dataset::summarize(&records).collections == expected,
        "Fresh register counts differ"
    );
    let keys: Vec<_> = records
        .iter()
        .map(|r| FamilyKey {
            source_group: r.source_group.clone(),
            collection: r.source.collection.clone(),
        })
        .collect();
    let assignments = assignments(&keys)?;
    ensure!(assignments.len() == 351, "Expected 351 unique families");
    for record in &mut records {
        record.split = Some(assignments[&record.source_group].split);
    }
    records.sort_by_key(|r| assignments[&r.source_group].split_flattened_index);
    dataset::validate_records(&records)?;
    for split in SPLITS {
        ensure!(
            records.iter().filter(|r| r.split == Some(split)).count() == 117,
            "Expected 117 roots per split"
        );
        for composer in COMPOSERS {
            ensure!(
                records
                    .iter()
                    .filter(|r| r.split == Some(split)
                        && assignments[&r.source_group].composer == composer)
                    .count()
                    == 39,
                "Expected 39 roots per composer and split"
            );
        }
    }
    let profile_ids: Vec<_> = PROFILES.iter().map(|s| (*s).to_string()).collect();
    let selection = Selection::new(prompt_profiles::SET_ID, Some(&profile_ids))?;
    ensure!(
        selection.selected_profile_ids == profile_ids,
        "Canonical profile order differs"
    );
    let mut files = BTreeMap::<String, Vec<u8>>::new();
    files.insert("frozen-roots.jsonl".into(), record_bytes(&records)?);
    let mut cohorts = BTreeMap::new();
    for composer in COMPOSERS.into_iter().chain(["phi4"]) {
        let selected: Vec<_> = records
            .iter()
            .filter(|r| {
                if composer == "phi4" {
                    r.split == Some(Split::Test)
                } else {
                    assignments[&r.source_group].composer == composer
                }
            })
            .cloned()
            .collect();
        ensure!(
            selected.len() == 117,
            "Expected 117 roots in each composer input"
        );
        let cohort = cohort(&selected, &selection)?;
        files.insert(format!("{composer}-roots.jsonl"), cohort.roots);
        files.insert(format!("{composer}-profile-plan.json"), cohort.profiles);
        cohorts.insert(composer, cohort.summary);
    }
    let decisions: Vec<_> = records.iter().map(|r|json!({"id":r.id,"source_group":r.source_group,"collection":r.source.collection,"text_sha256":r.text_sha256,"source_raw_sha256":r.source.raw_sha256,"assignment":assignments[&r.source_group]})).collect();
    let assignment = json!({"schema":"slop_ninja_v6_revision_evaluation_assignment_v1","input_sha256":INPUT_SHA256,"split_domain":SPLIT_DOMAIN,"composer_domain":COMPOSER_DOMAIN,"decisions":decisions});
    files.insert(
        "assignments.json".into(),
        serde_json::to_vec_pretty(&assignment)?,
    );
    let hashes: BTreeMap<_, _> = files
        .iter()
        .map(|(name, bytes)| (name.clone(), sha256(bytes)))
        .collect();
    let summary = json!({"schema":"slop_ninja_v6_revision_evaluation_plan_summary_v1","status":"prepared","families":351,"dataset":dataset::summarize(&records),"cohorts":cohorts,"phi4_test_intersection":117,"maximum_primary_calls":1404,"maximum_phi4_calls":468,"generation_calls":0,"detector_calls":0});
    files.insert("summary.json".into(), serde_json::to_vec_pretty(&summary)?);
    let plan = json!({
        "schema":"slop_ninja_v6_revision_evaluation_plan_v1","frozen_at":chrono::Utc::now().to_rfc3339(),"input_sha256":INPUT_SHA256,"protocol_sha256":args.expected_protocol_sha256,
        "selector_source_sha256":sha256(include_bytes!("prepare_revision_evaluation.rs")),"selector_executable_sha256":sha256(fs::read(std::env::current_exe()?)?),
        "split_assignment":"BTree collection order; within each collection ascending SHA256(JSON tuple(split_domain, source_group)); flatten all collections, cycle development/calibration/test without restarting at collection boundaries",
        "composer_assignment":"Within each split, BTree collection order; within each collection ascending SHA256(JSON tuple(composer_domain, source_group)); flatten, cycle qwen/mistral/olmo without restarting at collection boundaries",
        "profile_policy":"Assign separately against each exact frozen composer root file using the existing assign function before any generator --split filtering; no later root subsetting or profile reassignment",
        "profile_selection":selection,"files_sha256":hashes,"summary_sha256":sha256(&files["summary.json"]),
        "primary_route":["assigned composer draft and direct human edit","Qwen AntiAI revision of accepted original model draft","Mistral FixSlop revision of accepted first revision"],
        "phi4_route":["Phi4 draft and direct human edit","Phi4 AntiAI revision of accepted original model draft","Phi4 FixSlop revision of accepted first revision"],
        "phi4_policy":"All 117 assigned Test roots fixed before primary generation outcomes; separate profile plan; excluded from fitting; previously observed generator, without a known-model reviser",
        "ancestry_policy":"Preserve frozen roots and every request/response/failure. Advance only accepted complete-family branches; retain the separate full archive and attrition denominators",
        "runtime_policy":"Generator executables, model specifications and owned native runtime must be frozen separately before generation",
        "summary":summary
    });
    files.insert("plan.json".into(), serde_json::to_vec_pretty(&plan)?);
    // All validation and serialization finish before the first output is created.
    fs::create_dir_all(&args.output_dir)?;
    for (name, bytes) in files {
        let path = args.output_dir.join(name);
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("write {}", path.display()))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    Ok(summary)
}

fn main() -> Result<()> {
    let summary = prepare(Args::parse())?;
    let counts: BTreeMap<_, _> = summary["cohorts"]
        .as_object()
        .context("Missing cohort summary")?
        .iter()
        .map(|(name, cohort)| (name, cohort["dataset"]["records"].clone()))
        .collect();
    println!(
        "{}",
        serde_json::to_string(&json!({
            "status":summary["status"],"families":summary["families"],
            "split_counts":summary["dataset"]["splits"],"cohort_roots":counts,
            "maximum_primary_calls":summary["maximum_primary_calls"],
            "maximum_phi4_calls":summary["maximum_phi4_calls"],"generation_calls":0,"detector_calls":0
        }))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Vec<FamilyKey> {
        [("cmu", 151), ("news", 100), ("science", 100)]
            .into_iter()
            .flat_map(|(collection, n)| {
                (0..n).map(move |i| FamilyKey {
                    source_group: format!("synthetic-{collection}-{i}"),
                    collection: collection.into(),
                })
            })
            .collect()
    }

    #[test]
    fn balanced_assignments_are_independent_of_input_order() {
        let mut input = keys();
        let first = assignments(&input).unwrap();
        input.reverse();
        assert_eq!(first, assignments(&input).unwrap());
        for split in SPLITS {
            assert_eq!(first.values().filter(|a| a.split == split).count(), 117);
            for composer in COMPOSERS {
                assert_eq!(
                    first
                        .values()
                        .filter(|a| a.split == split && a.composer == composer)
                        .count(),
                    39
                );
            }
        }
    }

    #[test]
    fn split_cycle_continues_across_collection_boundaries() {
        let input = vec![
            FamilyKey {
                source_group: "first-a".into(),
                collection: "a".into(),
            },
            FamilyKey {
                source_group: "first-b".into(),
                collection: "a".into(),
            },
            FamilyKey {
                source_group: "second-a".into(),
                collection: "b".into(),
            },
        ];
        let assigned = assignments(&input).unwrap();
        assert_eq!(assigned["second-a"].split, Split::Test);
        assert_eq!(assigned["second-a"].split_flattened_index, 2);
        let mut duplicated = input;
        duplicated.push(duplicated[0].clone());
        assert!(assignments(&duplicated).is_err());
    }

    #[test]
    fn composer_cycle_continues_across_collection_boundaries() {
        let assigned = assignments(&keys()).unwrap();
        for split in SPLITS {
            let mut ordered: Vec<_> = assigned.values().filter(|a| a.split == split).collect();
            ordered.sort_by_key(|a| a.composer_flattened_index);
            for (i, a) in ordered.iter().enumerate() {
                assert_eq!(a.composer, COMPOSERS[i % 3]);
            }
        }
    }
}
