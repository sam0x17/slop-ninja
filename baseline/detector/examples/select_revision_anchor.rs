//! Select the preregistered, budgeted v6 Pangram comparison without scores.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{self, Origin, OriginRecord, Split, sha256},
    evaluation_view,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

const DOMAIN: &str = "slop-ninja-v6-pangram-confirmation-v1";
const MAX_UNITS: usize = 600;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    primary_records: PathBuf,
    #[arg(long)]
    primary_view: PathBuf,
    #[arg(long)]
    phi4_records: PathBuf,
    #[arg(long)]
    phi4_view: PathBuf,
    #[arg(long, required = true)]
    historical_records: Vec<PathBuf>,
    #[arg(long)]
    historical_ledger: PathBuf,
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    expected_protocol_sha256: String,
    #[arg(long)]
    output_dir: PathBuf,
}

#[derive(Clone)]
struct Candidate {
    group: String,
    rank: String,
    units: BTreeMap<String, usize>,
}

fn choose(
    strata: &mut BTreeMap<String, Vec<Candidate>>,
    budget: usize,
) -> Result<(BTreeSet<String>, Value)> {
    ensure!(
        !strata.is_empty() && strata.values().all(|v| !v.is_empty()),
        "Empty sampling cell"
    );
    for candidates in strata.values_mut() {
        candidates.sort_by(|a, b| (&a.rank, &a.group).cmp(&(&b.rank, &b.group)));
    }
    let mut selected = BTreeSet::new();
    let mut texts = BTreeMap::new();
    let mut rounds = Vec::new();
    let depth = strata.values().map(Vec::len).min().unwrap();
    let mut stop = "cell_exhausted";
    for round in 0..depth {
        let mut next = texts.clone();
        for candidates in strata.values() {
            for (hash, units) in &candidates[round].units {
                if let Some(previous) = next.insert(hash.clone(), *units) {
                    ensure!(previous == *units, "Conflicting cost for one text hash");
                }
            }
        }
        let units: usize = next.values().sum();
        let accepted = units <= budget;
        rounds.push(json!({"round":round+1,"cumulative_unique_texts":next.len(),"cumulative_units":units,"accepted":accepted}));
        if !accepted {
            stop = "next_complete_round_exceeds_budget";
            break;
        }
        for candidates in strata.values() {
            ensure!(
                selected.insert(candidates[round].group.clone()),
                "Family occurs in multiple cells"
            );
        }
        texts = next;
    }
    ensure!(
        !selected.is_empty(),
        "Budget cannot cover one complete balanced round"
    );
    Ok((
        selected,
        json!({"rounds":rounds,"stop_reason":stop,"selected_unique_texts":texts.len(),"selected_units":texts.values().sum::<usize>()}),
    ))
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output_dir.exists(), "Use a new output directory");
    let protocol_bytes = fs::read(&args.protocol)?;
    ensure!(
        sha256(&protocol_bytes) == args.expected_protocol_sha256,
        "Protocol binding differs"
    );
    let mut inputs = Vec::new();
    for (path, expected) in [
        (
            &args.primary_records,
            "9d49faf9f9415352d49f286c523f24af0677128ac149478d7e422190b5c896b4",
        ),
        (
            &args.primary_view,
            "21387b914c1cd0130fb5d224c5df61a2c6eef082448fa6c1b2db0d92cd5e64c1",
        ),
        (
            &args.phi4_records,
            "cb22b1a710fee583d22a56c3c0eb084d31ab56c7b981fc94a92bbfb5efe27843",
        ),
        (
            &args.phi4_view,
            "fa5a9ac42c77e3fe7574e55634d61311073ff4b84fedf7f0577559ffe17df67f",
        ),
    ] {
        ensure!(
            sha256(fs::read(path)?) == expected,
            "Frozen route input differs"
        );
        inputs.push(json!({"path":path,"sha256":expected}));
    }
    let primary =
        evaluation_view::load_view(&args.primary_view, &args.primary_records, Split::Test)?;
    let phi4 = evaluation_view::load_view(&args.phi4_view, &args.phi4_records, Split::Test)?;
    ensure!(
        primary.view.families.len() == 112 && phi4.view.families.len() == 104,
        "Unexpected route coverage"
    );
    let mut historical_groups = BTreeSet::new();
    let mut historical_hashes = BTreeSet::new();
    let ledger_bytes = fs::read(&args.historical_ledger)?;
    let ledger_hash = sha256(&ledger_bytes);
    ensure!(
        ledger_hash == "62a4f08c839fdbc05700a224187933910bfa6cf1d94aabecdb6a367527822e8f",
        "Historical ledger binding differs"
    );
    let ledger: Value = serde_json::from_slice(&ledger_bytes)?;
    ensure!(
        ledger["schema"] == "slop-ninja-v6-canonical-source-ledger-v1",
        "Historical ledger schema differs"
    );
    historical_groups.extend(
        ledger["indexes"]["source_group"]
            .as_object()
            .context("Missing historical family index")?
            .keys()
            .cloned(),
    );
    historical_hashes.extend(
        ledger["indexes"]["exact_text_sha256"]
            .as_object()
            .context("Missing historical text index")?
            .keys()
            .cloned(),
    );
    ensure!(
        historical_groups.len() == 852 && historical_hashes.len() == 6507,
        "Historical coverage differs"
    );
    inputs.push(json!({"path":args.historical_ledger,"sha256":ledger_hash}));
    for path in &args.historical_records {
        let hash = sha256(fs::read(path)?);
        for record in dataset::read_records(path)? {
            historical_groups.insert(record.source_group);
            historical_hashes.insert(record.text_sha256);
        }
        ensure!(sha256(fs::read(path)?) == hash, "Historical input changed");
        inputs.push(json!({"path":path,"sha256":hash}));
    }
    let phi_groups: BTreeMap<_, _> = phi4
        .view
        .families
        .iter()
        .map(|f| (f.source_group.as_str(), f))
        .collect();
    let mut all_records = BTreeMap::<String, OriginRecord>::new();
    for record in primary.archive.iter().chain(&phi4.archive) {
        ensure!(
            record.split == Some(Split::Test) && record.rights.external_evaluation,
            "Expected admitted Test records"
        );
        ensure!(
            !historical_groups.contains(&record.source_group)
                && !historical_hashes.contains(&record.text_sha256),
            "Historical overlap in a fresh route"
        );
        if let Some(previous) = all_records.insert(record.id.clone(), record.clone()) {
            ensure!(
                serde_json::to_value(previous)? == serde_json::to_value(record)?,
                "Shared record differs between routes"
            );
        }
    }
    let merged: Vec<_> = all_records.into_values().collect();
    dataset::validate_records(&merged)?;
    let mut strata = BTreeMap::<String, Vec<Candidate>>::new();
    let mut profiles = BTreeMap::new();
    for family in &primary.view.families {
        let Some(other) = phi_groups.get(family.source_group.as_str()) else {
            continue;
        };
        ensure!(
            family.human == other.human
                && family.chain.revision_depth == 2
                && other.chain.revision_depth == 2,
            "Paired route ancestry differs"
        );
        let records: Vec<_> = merged
            .iter()
            .filter(|r| r.source_group == family.source_group)
            .collect();
        ensure!(
            records.len() == 9,
            "Paired family requires one shared root and eight generated records"
        );
        let counts = [Origin::HumanOnly, Origin::ModelOnly, Origin::Mixed]
            .map(|o| records.iter().filter(|r| r.origin == o).count());
        ensure!(counts == [1, 6, 2], "Paired family origins differ");
        let human = records
            .iter()
            .find(|r| r.origin == Origin::HumanOnly)
            .unwrap();
        let key = format!(
            "{}|{}",
            human.source.collection, family.chain.composer.model_revision
        );
        let mut units = BTreeMap::new();
        for record in records {
            let words = record.text.split_whitespace().count();
            ensure!(words >= 50, "Pangram requires at least 50 words");
            units.insert(record.text_sha256.clone(), words.div_ceil(100));
        }
        profiles.insert(family.source_group.clone(),json!({"primary":family.chain.initial_profile_id,"phi4":other.chain.initial_profile_id}));
        strata.entry(key).or_default().push(Candidate {
            group: family.source_group.clone(),
            rank: sha256(serde_json::to_vec(&(DOMAIN, &family.source_group))?),
            units,
        });
    }
    ensure!(strata.len() == 9, "Expected nine collection/composer cells");
    let population: usize = strata.values().map(Vec::len).sum();
    let (selected, decision) = choose(&mut strata, MAX_UNITS)?;
    let chosen: Vec<_> = merged
        .into_iter()
        .filter(|r| selected.contains(&r.source_group))
        .collect();
    let mut profile_counts = BTreeMap::<String, usize>::new();
    for group in &selected {
        for route in ["primary", "phi4"] {
            let profile = profiles[group][route]
                .as_str()
                .unwrap_or("legacy-unprofiled");
            *profile_counts
                .entry(format!("{route}|{profile}"))
                .or_default() += 1;
        }
    }
    let cells: BTreeMap<_,_> = strata.iter().map(|(key,members)|(key,json!({"available_families":members.len(),"ranked_families":members.iter().map(|c|json!({"source_group":c.group,"rank":c.rank,"selected":selected.contains(&c.group)})).collect::<Vec<_>>()}))).collect();
    for input in &inputs {
        ensure!(
            sha256(fs::read(
                input["path"].as_str().context("Missing input path")?
            )?) == input["sha256"],
            "Input changed during selection"
        );
    }
    ensure!(
        fs::read(&args.protocol)? == protocol_bytes,
        "Protocol changed"
    );
    fs::create_dir(&args.output_dir)?;
    dataset::write_records(&args.output_dir.join("records.jsonl"), &chosen)?;
    let units = decision["selected_units"].as_u64().unwrap();
    let summary = json!({"schema":"slop_ninja_v6_pangram_membership_v1","protocol_sha256":args.expected_protocol_sha256,"inputs":inputs,"rank_domain":DOMAIN,"maximum_units":MAX_UNITS,"paired_population_families":population,"selected_families":selected.len(),"families_per_cell":selected.len()/9,"decision":decision,"cells":cells,"profile_counts":profile_counts,"records_sha256":sha256(fs::read(args.output_dir.join("records.jsonl"))?),"summary":dataset::summarize(&chosen),"estimated_bulk_usd_cents":units*4,"reserve_usd_cents":units*5,"historical_families_checked":historical_groups.len(),"historical_exact_hashes_checked":historical_hashes.len(),"model_calls":0,"pangram_calls":0,"detector_scores_used":false,"source_sha256":sha256(include_bytes!("select_revision_anchor.rs")),"executable_sha256":sha256(fs::read(std::env::current_exe()?)?)});
    fs::write(
        args.output_dir.join("membership.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    let mut public = summary;
    public.as_object_mut().unwrap().remove("cells");
    public.as_object_mut().unwrap().remove("inputs");
    println!("{}", serde_json::to_string_pretty(&public)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn c(group: &str, rank: &str, hash: &str, cost: usize) -> Candidate {
        Candidate {
            group: group.into(),
            rank: rank.into(),
            units: BTreeMap::from([(hash.into(), cost)]),
        }
    }
    #[test]
    fn budget_stops_at_a_whole_round_without_skipping_expensive_families() {
        let mut strata = BTreeMap::from([
            (
                "a".into(),
                vec![
                    c("a3", "3", "a3", 1),
                    c("a1", "1", "a1", 2),
                    c("a2", "2", "a2", 20),
                ],
            ),
            (
                "b".into(),
                vec![
                    c("b1", "1", "b1", 2),
                    c("b2", "2", "b2", 1),
                    c("b3", "3", "b3", 1),
                ],
            ),
        ]);
        let (selected, report) = choose(&mut strata, 10).unwrap();
        assert_eq!(selected, BTreeSet::from(["a1".into(), "b1".into()]));
        assert_eq!(report["stop_reason"], "next_complete_round_exceeds_budget");
        assert_eq!(report["rounds"][1]["cumulative_units"], 25);
        assert!(choose(&mut strata, 3).is_err());
    }
    #[test]
    fn shared_texts_are_billed_once_and_cell_exhaustion_preserves_balance() {
        let mut strata = BTreeMap::from([
            ("a".into(), vec![c("a1", "1", "shared", 3)]),
            (
                "b".into(),
                vec![c("b2", "2", "extra", 1), c("b1", "1", "shared", 3)],
            ),
        ]);
        let (selected, report) = choose(&mut strata, 3).unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!(report["selected_units"], 3);
        assert_eq!(report["stop_reason"], "cell_exhausted");
    }
}
