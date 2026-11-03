//! Audit saved teacher coverage without fitting or changing provenance labels.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::{
    attribution,
    dataset::{OriginRecord, Split, read_records, sha256, validate_records},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::PathBuf,
};

const SHARDS: [(&str, Split, usize, &str); 4] = [
    (
        "train",
        Split::Train,
        3833,
        "023d6e5428236502e877bac0afe0e9e9ee4aa9d10ab38eb940ca8e915cf7502e",
    ),
    (
        "development",
        Split::Development,
        530,
        "c9ee825909a37b7aee0863c00437f2375d5569f702de80c12f515af1df29a78b",
    ),
    (
        "calibration",
        Split::Calibration,
        540,
        "e26ed741c1fbe2d64dc7e8d49211573eaa44b11589524ce76f2b487a9b711031",
    ),
    (
        "test",
        Split::Test,
        560,
        "9d49faf9f9415352d49f286c523f24af0677128ac149478d7e422190b5c896b4",
    ),
];
const COHORTS: [(&str, usize, &str, &str); 5] = [
    (
        "pilot",
        120,
        "pilot-job-v1/collection-1789241947604565000/annotations.jsonl",
        "c1f2a085eea1585ae7e87d5d0ff5aa260c2474f5e0c5926d48ebaf0f503c4a05",
    ),
    (
        "second",
        96,
        "second-cohort-job-v1/collection-1789242487810666000/annotations.jsonl",
        "af164306133bb14fe2256c5bf9e7f185b353b303ada72ee819034f79f74dafa7",
    ),
    (
        "third",
        42,
        "third-cohort-job-v1/collection-1789243065674662000/annotations.jsonl",
        "8943a9abc1aee77537db67df85bb8a855c3c6d2ea1ee746bd812ba832a354d92",
    ),
    (
        "fourth",
        117,
        "fourth-cohort-job-v1/collection-1789248421660201000/annotations.jsonl",
        "f907378843135e871e9a596b8936f4f803957a9a1f6a5efb25ab4c4b51a62fcd",
    ),
    (
        "fifth",
        110,
        "fifth-cohort-v1/job-v1/collection-1789263862969108000/annotations.jsonl",
        "475dc21c0b9bad4e0fd682183b0d4e9ee0164d5c2d7f90fc5c5dfbd376670aec",
    ),
];

#[derive(Parser)]
struct Args {
    #[arg(long)]
    shards: PathBuf,
    #[arg(long)]
    annotations_root: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("Missing {key}"))
}

fn score_bin(score: f64) -> &'static str {
    if (score - 1.0).abs() <= 0.00001 {
        "100"
    } else if score < 0.6 {
        "below-60"
    } else if score < 0.7 {
        "60-70"
    } else if score < 0.8 {
        "70-80"
    } else if score < 0.9 {
        "80-90"
    } else {
        "90-below-100"
    }
}

fn echo(submitted: &str, returned: &str) -> Result<&'static str> {
    if submitted == returned {
        return Ok("exact");
    }
    if submitted.split_whitespace().eq(returned.split_whitespace()) {
        return Ok("whitespace_only");
    }
    ensure!(
        submitted.contains('\u{00ad}')
            && submitted
                .replace('\u{00ad}', "")
                .split_whitespace()
                .eq(returned.split_whitespace()),
        "Changed provider echo"
    );
    Ok("source_soft_hyphens_removed")
}

#[derive(Default)]
struct Cell {
    total: usize,
    families: BTreeSet<String>,
    annotated: usize,
    annotated_families: BTreeSet<String>,
    bins: BTreeMap<String, usize>,
    zero: usize,
    below_tenth: usize,
}

impl Cell {
    fn add(&mut self, family: &str, score: Option<f64>) {
        self.total += 1;
        self.families.insert(family.to_owned());
        if let Some(score) = score {
            self.annotated += 1;
            self.annotated_families.insert(family.to_owned());
            *self.bins.entry(score_bin(score).into()).or_default() += 1;
            self.zero += usize::from(score == 0.0);
            self.below_tenth += usize::from(score < 0.1);
        }
    }
    fn report(&self) -> Value {
        json!({"train_records":self.total,"train_families":self.families.len(),
            "annotated_records":self.annotated,"unannotated_records":self.total-self.annotated,
            "annotated_families":self.annotated_families.len(),"combined_fraction_bins":self.bins,
            "exact_zero":self.zero,"below_tenth":self.below_tenth})
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a new output directory");
    let mut records = Vec::<OriginRecord>::new();
    let mut bindings = Vec::new();
    for (name, split, count, hash) in SHARDS {
        let path = args.shards.join(format!("{name}.jsonl"));
        ensure!(sha256(fs::read(&path)?) == hash, "Changed {name} shard");
        let rows = read_records(&path)?;
        ensure!(
            rows.len() == count && rows.iter().all(|r| r.split == Some(split)),
            "Changed shard membership"
        );
        records.extend(rows);
        bindings.push(json!({"kind":"shard","name":name,"sha256":hash,"records":count}));
    }
    validate_records(&records)?;
    let index: BTreeMap<_, _> = records.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut by_hash = BTreeMap::<&str, Vec<&OriginRecord>>::new();
    for r in &records {
        by_hash.entry(&r.text_sha256).or_default().push(r);
    }
    let mut annotations = BTreeMap::<String, Value>::new();
    let mut submitted_hashes = BTreeSet::new();
    let mut split_matches = BTreeMap::<Split, usize>::new();
    let mut echoes = BTreeMap::<String, usize>::new();
    let mut versions = BTreeSet::new();
    for (name, count, relative, hash) in COHORTS {
        let bytes = fs::read(args.annotations_root.join(relative))?;
        ensure!(sha256(&bytes) == hash, "Changed {name} annotations");
        let mut seen = 0;
        for line in std::str::from_utf8(&bytes)?.lines() {
            let a: Value = serde_json::from_str(line)?;
            ensure!(
                a["schema"] == "slop_ninja_pangram_corpus_annotation_v1"
                    && a["observation"]["stage"] == "STAGE_SUCCESS",
                "Not a successful archived observation"
            );
            let text_hash = field(&a, "submitted_text_sha256")?;
            ensure!(
                submitted_hashes.insert(text_hash.to_owned()),
                "Repeated submitted text needs a declared aggregation policy"
            );
            let member = &a["member"];
            ensure!(
                member["text_sha256"] == text_hash && member["repeat_index"] == 0,
                "Wrong request binding"
            );
            let matches = by_hash
                .get(text_hash)
                .context("Submitted text absent from original shards")?;
            for matched in matches {
                let split = matched.split.context("Missing split")?;
                *split_matches.entry(split).or_default() += 1;
                ensure!(
                    split == Split::Train,
                    "Annotation text crosses into held-out data"
                );
            }
            let result = &a["observation"]["result"];
            let returned = field(result, "text")?;
            ensure!(
                a["returned_text_sha256"] == sha256(returned),
                "Returned text hash changed"
            );
            let fractions: Vec<f64> = ["fraction_ai", "fraction_ai_assisted", "fraction_human"]
                .iter()
                .map(|key| result[key].as_f64().context("Missing fraction"))
                .collect::<Result<_>>()?;
            ensure!(
                fractions
                    .iter()
                    .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
                    && (fractions.iter().sum::<f64>() - 1.0).abs() <= 0.00001,
                "Invalid provider fractions"
            );
            let version = field(result, "version")?;
            ensure!(!version.is_empty(), "Missing provider version");
            versions.insert(version.to_owned());
            let members = member["records"].as_array().context("Missing members")?;
            ensure!(!members.is_empty(), "Empty annotation membership");
            for r in members {
                let id = field(r, "record_id")?;
                let record = index
                    .get(id)
                    .context("Annotation ID absent from original shards")?;
                ensure!(
                    record.split == Some(Split::Train)
                        && record.text_sha256 == text_hash
                        && r["source_group"] == record.source_group
                        && r["origin"] == serde_json::to_value(record.origin)?
                        && r["evidence"] == serde_json::to_value(record.evidence)?
                        && r["parent_id"] == serde_json::to_value(&record.parent_id)?
                        && r["split"] == "train",
                    "Annotation provenance mismatch"
                );
                let echo_kind = echo(&record.text, returned)?;
                ensure!(
                    a["echo_match"] == echo_kind,
                    "Wrong declared echo normalization"
                );
                *echoes.entry(echo_kind.into()).or_default() += 1;
                let value = json!({"record_id":id,"source_group":record.source_group,"text_sha256":text_hash,"origin":record.origin,"split":"train","cohort":name,"fractions":fractions,"combined_fraction":fractions[0]+fractions[1],"provider_version":version,"collected_at":a["collected_at"],"echo_match":echo_kind});
                ensure!(
                    annotations.insert(id.to_owned(), value).is_none(),
                    "Repeated annotated record"
                );
            }
            seen += 1;
        }
        ensure!(seen == count, "Changed annotation row count");
        bindings.push(json!({"kind":"annotations","name":name,"sha256":hash,"records":seen}));
    }
    let mut total = Cell::default();
    let mut slices = BTreeMap::<&str, BTreeMap<String, Cell>>::new();
    let mut joined = Vec::new();
    let mut generator_identities = BTreeMap::<String, Value>::new();
    for r in records.iter().filter(|r| r.split == Some(Split::Train)) {
        let attribution = attribution::describe(r, &index)?;
        let root = index
            .get(field(&attribution, "source_root_id")?)
            .context("Missing root")?;
        let steps = attribution["steps"]
            .as_array()
            .context("Missing ancestry steps")?;
        let annotation = annotations.get(&r.id);
        let score =
            annotation.map(|v| v["combined_fraction"].as_f64().expect("Validated fraction"));
        let origin = serde_json::to_value(r.origin)?
            .as_str()
            .context("Bad origin")?
            .to_owned();
        for step in steps {
            let label = field(step, "generator_label")?;
            let identity = json!({"requested_model":step["requested_model"],
                "declared_revision":step["declared_revision"],"reported_model":step["reported_model"]});
            if let Some(previous) = generator_identities.insert(label.to_owned(), identity.clone())
            {
                ensure!(previous == identity, "Conflicting generator identity");
            }
        }
        let model = match steps.last() {
            Some(step) => field(step, "generator_label")?,
            None => "human_root",
        };
        let profile = r.generation.as_ref().map_or("human_root", |g| {
            g.prompt_profile
                .as_ref()
                .map_or("unrecorded", |p| p.profile.id.as_str())
        });
        let route = steps
            .iter()
            .map(|step| field(step, "generator_label"))
            .collect::<Result<Vec<_>>>()?
            .join(" -> ");
        let dimensions = [
            ("origin", origin.clone()),
            ("root_collection", root.source.collection.clone()),
            ("final_generator", model.to_owned()),
            ("generation_depth", steps.len().to_string()),
            ("final_profile", profile.to_owned()),
            ("origin_and_depth", format!("{origin}/{}", steps.len())),
            (
                "route",
                if route.is_empty() {
                    "human_root".into()
                } else {
                    route
                },
            ),
        ];
        total.add(&r.source_group, score);
        for (axis, key) in dimensions {
            slices
                .entry(axis)
                .or_default()
                .entry(key)
                .or_default()
                .add(&r.source_group, score);
        }
        if let Some(a) = annotation {
            let mut row = a.clone();
            row["generator_attribution"] = attribution;
            serde_json::to_writer(&mut joined, &row)?;
            joined.push(b'\n');
        }
    }
    let slices: BTreeMap<_, BTreeMap<_, _>> = slices
        .into_iter()
        .map(|(axis, cells)| {
            (
                axis,
                cells
                    .into_iter()
                    .map(|(key, c)| (key, c.report()))
                    .collect(),
            )
        })
        .collect();
    for (name, _, _, hash) in SHARDS {
        ensure!(
            sha256(fs::read(args.shards.join(format!("{name}.jsonl")))?) == hash,
            "Shard changed during audit"
        );
    }
    for (_, _, path, hash) in COHORTS {
        ensure!(
            sha256(fs::read(args.annotations_root.join(path))?) == hash,
            "Annotations changed during audit"
        );
    }
    let report = json!({"schema":"slop_ninja_pangram_train_coverage_v1","status":"audited_not_fitted","bindings":bindings,
        "source_sha256":sha256(include_bytes!("pangram_train_coverage.rs")),"protocol_sha256":sha256(include_bytes!("../PANGRAM_TRAIN_COVERAGE_V1.md")),
        "unique_annotated_texts":submitted_hashes.len(),"annotated_record_ids":annotations.len(),"exact_hash_matches_by_split":split_matches,
        "echo_counts":echoes,"provider_versions":versions,"total":total.report(),"slices":slices,"generator_identities":generator_identities,"joined_sha256":sha256(&joined),
        "provider_calls":0,"model_calls":0,"data_or_models_changed":false,"teacher_supervised_training_performed":false,
        "limits":"Five adaptive local-generator Train cohorts only. Content fractions are not origin probabilities. Hosted, held-out and private observations are excluded. No independent performance or training-rights claim."});
    fs::create_dir(&args.out)?;
    for (name, bytes) in [
        ("joined.jsonl", joined),
        ("report.json", serde_json::to_vec_pretty(&report)?),
    ] {
        let mut file = fs::File::create_new(args.out.join(name))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    println!(
        "{}",
        json!({"total":report["total"],"split_matches":report["exact_hash_matches_by_split"]})
    );
    Ok(())
}
