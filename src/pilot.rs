//! Descriptive matched-pair analysis restricted to the training split.

use crate::{features, statistics, store, util};
use anyhow::{Context, Result, ensure};
use regex::Regex;
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::LazyLock;

const FAMILIES: [&str; 3] = ["word", "construction", "pos_trigram"];
const TOP_FEATURES: usize = 30;
type GroupFeatures = BTreeMap<String, (u64, BTreeMap<String, u64>)>;
static NUMBERS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{Nd}+(?:[.,]\p{Nd}+)*").unwrap());

struct Sample {
    id: i64,
    sha256: String,
    text: String,
    domain: String,
    register: String,
    source_kind: String,
    provider: Option<String>,
    model: Option<String>,
    metadata: Value,
}

fn training(db: &Connection, corpus: &str, kind: &str) -> Result<BTreeMap<String, Sample>> {
    let mut query = db.prepare(
        "SELECT id,sha256,text,group_id,domain,register,source_kind,provider,model,metadata_json
         FROM documents WHERE corpus=? AND split='train' ORDER BY group_id,id",
    )?;
    let rows = query.query_map([corpus], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, Option<String>>(8)?,
            r.get::<_, String>(9)?,
        ))
    })?;
    let mut samples = BTreeMap::new();
    for row in rows {
        let (id, sha256, text, group, domain, register, source_kind, provider, model, metadata) =
            row?;
        ensure!(
            source_kind == kind,
            "Expected {kind} provenance for {corpus} training document {id}"
        );
        let sample = Sample {
            id,
            sha256,
            text,
            domain,
            register,
            source_kind,
            provider,
            model,
            metadata: serde_json::from_str(&metadata)
                .with_context(|| format!("Invalid metadata for training document {id}"))?,
        };
        ensure!(
            samples.insert(group.clone(), sample).is_none(),
            "Expected exactly one {corpus} training sample per source group: {group}"
        );
    }
    ensure!(!samples.is_empty(), "No training documents for {corpus}");
    Ok(samples)
}

fn match_sources(
    human: &BTreeMap<String, Sample>,
    model: &BTreeMap<String, Sample>,
    corpus: &str,
) -> Result<()> {
    ensure!(
        human.keys().eq(model.keys()),
        "Training source groups do not match between human and {corpus}"
    );
    for (group, source) in human {
        let candidate = &model[group];
        ensure!(
            source.domain == candidate.domain && source.register == candidate.register,
            "Training domain/register mismatch for {corpus} source group {group}"
        );
    }
    Ok(())
}

fn extractor_for(db: &Connection, samples: &[&Sample]) -> Result<String> {
    let mut common: Option<BTreeSet<String>> = None;
    for sample in samples {
        let mut query = db.prepare(
            "SELECT extractor FROM totals WHERE document_id=?
             AND family IN ('word','construction','pos_trigram')
             AND extractor LIKE 'unslop-features-rust-v1;%'
             GROUP BY extractor HAVING COUNT(DISTINCT family)=3",
        )?;
        let versions = query
            .query_map([sample.id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<BTreeSet<_>>>()?;
        common = Some(match common {
            None => versions,
            Some(old) => old.intersection(&versions).cloned().collect(),
        });
    }
    let common = common.unwrap_or_default();
    ensure!(
        common.len() == 1,
        "Matched report requires exactly one common Rust grammar extractor for every training document; found {}",
        common.len()
    );
    Ok(common.into_iter().next().unwrap())
}

fn document_features(
    db: &Connection,
    samples: &BTreeMap<String, Sample>,
    family: &str,
    extractor: &str,
) -> Result<GroupFeatures> {
    let mut observations = BTreeMap::new();
    let mut total_query =
        db.prepare("SELECT total FROM totals WHERE document_id=? AND extractor=? AND family=?")?;
    let mut count_query = db.prepare(
        "SELECT feature,count FROM features WHERE document_id=? AND extractor=? AND family=?",
    )?;
    for (group, sample) in samples {
        let total = total_query.query_row(params![sample.id, extractor, family], |r| {
            r.get::<_, u64>(0)
        })?;
        let counts = count_query
            .query_map(params![sample.id, extractor, family], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?))
            })?
            .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
        observations.insert(group.clone(), (total, counts));
    }
    Ok(observations)
}

fn paired_rates(feature: &str, human: &GroupFeatures, model: &GroupFeatures) -> Value {
    let mut groups = Vec::new();
    let (mut above, mut below, mut equal, mut unavailable) = (0_usize, 0_usize, 0_usize, 0_usize);
    let mut sum = 0.0;
    for (group, (human_total, human_counts)) in human {
        let (model_total, model_counts) = &model[group];
        let human_count = human_counts.get(feature).copied().unwrap_or(0);
        let model_count = model_counts.get(feature).copied().unwrap_or(0);
        let (human_rate, model_rate, delta, direction) = if *human_total == 0 || *model_total == 0 {
            unavailable += 1;
            (None, None, None, "unavailable")
        } else {
            let human_rate = human_count as f64 / *human_total as f64;
            let model_rate = model_count as f64 / *model_total as f64;
            let delta = model_rate - human_rate;
            sum += delta;
            // Exact rational comparison avoids classifying floating roundoff as a direction.
            let direction = match ((model_count as u128) * (*human_total as u128))
                .cmp(&((human_count as u128) * (*model_total as u128)))
            {
                std::cmp::Ordering::Greater => {
                    above += 1;
                    "model_higher"
                }
                std::cmp::Ordering::Less => {
                    below += 1;
                    "model_lower"
                }
                std::cmp::Ordering::Equal => {
                    equal += 1;
                    "equal"
                }
            };
            (Some(human_rate), Some(model_rate), Some(delta), direction)
        };
        groups.push(json!({
            "group_id":group, "human_count":human_count,"model_count":model_count,
            "human_opportunities":human_total,"model_opportunities":model_total,
            "human_rate":human_rate,"model_rate":model_rate,"model_minus_human_rate":delta,
            "direction":direction,
        }));
    }
    let comparable = above + below + equal;
    let direction = if above > below && above > equal && above >= 3 {
        Some("model_higher")
    } else if below > above && below > equal && below >= 3 {
        Some("model_lower")
    } else {
        None
    };
    json!({
        "group_equal_mean_model_minus_human_rate": if comparable > 0 {Some(sum/comparable as f64)} else {None},
        "groups_model_higher":above,"groups_model_lower":below,"groups_equal":equal,
        "groups_without_opportunities":unavailable,"comparable_groups":comparable,
        "most_common_direction_repeated_at_least_three_groups":direction,
        "groups":groups,
    })
}

fn number_tokens(text: &str) -> BTreeMap<String, usize> {
    let mut result = BTreeMap::new();
    for number in NUMBERS.find_iter(text) {
        *result.entry(number.as_str().to_string()).or_default() += 1;
    }
    result
}

fn advisory(human: &Sample, model: &Sample, group: &str) -> Value {
    let human_length = features::words(&human.text).len();
    let model_length = features::words(&model.text).len();
    let source_numbers = number_tokens(&human.text);
    let candidate_numbers = number_tokens(&model.text);
    let mut missing = Map::new();
    let mut added = Map::new();
    for (number, count) in &source_numbers {
        let removed = count.saturating_sub(candidate_numbers.get(number).copied().unwrap_or(0));
        if removed > 0 {
            missing.insert(number.clone(), json!(removed));
        }
    }
    for (number, count) in &candidate_numbers {
        let extra = count.saturating_sub(source_numbers.get(number).copied().unwrap_or(0));
        if extra > 0 {
            added.insert(number.clone(), json!(extra));
        }
    }
    json!({
        "group_id":group,"human_document_id":human.id,"model_document_id":model.id,
        "human_sha256":human.sha256,"model_sha256":model.sha256,
        "domain":human.domain,"register":human.register,
        "human_lexical_words":human_length,"model_lexical_words":model_length,
        "model_to_human_lexical_word_ratio":if human_length>0 {Some(model_length as f64/human_length as f64)} else {None},
        "number_token_multiset_equal":source_numbers==candidate_numbers,
        "missing_number_tokens":missing,"added_number_tokens":added,
        "fidelity_status":"not_reviewed",
    })
}

fn provenance(samples: &BTreeMap<String, Sample>) -> Vec<Value> {
    samples.iter().map(|(group,sample)| {
        let requested=sample.metadata.get("model_requested").cloned().unwrap_or(Value::Null);
        let identity_source=sample.metadata.get("model_identity_source")
            .or_else(||sample.metadata["generation_metadata"].get("model_identity_source"))
            .cloned().unwrap_or(Value::Null);
        let model_resolved=sample.model.as_deref().filter(|model|!model.starts_with("requested:"));
        json!({
            "group_id":group,"document_id":sample.id,"sha256":sample.sha256,
            "source_kind":sample.source_kind,"provider":sample.provider,
            "model_requested":requested,"model_stored":sample.model,
            "model_resolved":model_resolved,"model_identity_source":identity_source,
            "transport":sample.metadata["transport"],"cli_version":sample.metadata["cli_version"],
            "source_record_id":sample.metadata["id"],
            "prompt_record_id":sample.metadata["prompt_record"]["id"],
            "generation_regime":sample.metadata["prompt_record"]["generation_regime"],
        })
    }).collect()
}

fn heldout_counts(db: &Connection, corpus: &str) -> Result<Vec<Value>> {
    // Deliberately select only counts. Never deserialize held-out text or metadata.
    let mut query = db.prepare("SELECT split,COUNT(*),COUNT(DISTINCT group_id) FROM documents WHERE corpus=? AND split IN ('dev','test') GROUP BY split ORDER BY split")?;
    Ok(query.query_map([corpus], |r| Ok(json!({
        "split":r.get::<_,String>(0)?,"documents":r.get::<_,u64>(1)?,"source_groups":r.get::<_,u64>(2)?,
    })))?.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Write one training-only report, leaving development/test text and detector runs unread.
pub fn report(db: &Connection, human: &str, models: &[String], out: &Path) -> Result<Value> {
    ensure!(!models.is_empty(), "At least one model corpus is required");
    ensure!(
        models.iter().collect::<BTreeSet<_>>().len() == models.len()
            && !models.iter().any(|model| model == human),
        "Model corpora must be distinct from each other and the human corpus"
    );
    let human_samples = training(db, human, "human")?;
    let mut model_samples = BTreeMap::new();
    for model in models {
        let samples = training(db, model, "model")?;
        match_sources(&human_samples, &samples, model)?;
        model_samples.insert(model.clone(), samples);
    }
    let all_samples: Vec<_> = human_samples
        .values()
        .chain(model_samples.values().flat_map(|samples| samples.values()))
        .collect();
    let extractor = extractor_for(db, &all_samples)?;
    let mut comparisons = Vec::new();
    for model in models {
        let samples = &model_samples[model];
        let mut families = Map::new();
        for family in FAMILIES {
            let human_profile = store::profile(
                db,
                human,
                family,
                "train",
                None,
                None,
                Some(&extractor),
                &[],
            )?;
            let model_profile = store::profile(
                db,
                model,
                family,
                "train",
                None,
                None,
                Some(&extractor),
                &[],
            )?;
            let contrasts = statistics::contrast(&model_profile, &human_profile, 5, 3)?;
            let human_features = document_features(db, &human_samples, family, &extractor)?;
            let model_features = document_features(db, samples, family, &extractor)?;
            let mut top_features = Vec::new();
            for row in contrasts
                .as_array()
                .context("Contrast must return rows")?
                .iter()
                .take(TOP_FEATURES)
            {
                let mut row = row.clone();
                let feature = row["feature"]
                    .as_str()
                    .context("Contrast feature missing")?
                    .to_string();
                row["paired"] = paired_rates(&feature, &human_features, &model_features);
                top_features.push(row);
            }
            families.insert(family.to_string(), json!({
                "left_corpus":model,"right_corpus":human,
                "human_profile":human_profile,"model_profile":model_profile,
                "features_ranked":contrasts.as_array().unwrap().len(),"top_features":top_features,
            }));
        }
        comparisons.push(json!({
            "model_corpus":model,"matched_training_groups":samples.len(),
            "provenance":provenance(samples),"families":families,
            "retention_advisories":human_samples.iter().map(|(group,source)|advisory(source,&samples[group],group)).collect::<Vec<_>>(),
            "heldout_counts":heldout_counts(db,model)?,
        }));
    }
    let result = json!({
        "report_version":"matched-pilot-rust-v1","created_at":chrono::Utc::now().to_rfc3339(),
        "split_analyzed":"train","human_corpus":human,"extractor":extractor,
        "training_groups":human_samples.len(),"human_provenance":provenance(&human_samples),
        "human_heldout_counts":heldout_counts(db,human)?,"comparisons":comparisons,
        "interpretation":{
            "generation_regime":"Human originals paired with source-conditioned model rewrites; this is not an unconditioned model vocabulary fingerprint.",
            "ranking":"Pooled half-count log odds with approximate z used only for ranking. Paired rates weight each source group equally. Neither is a classifier probability or calibrated significance test.",
            "evidence_floor":"Pooled enough_evidence requires five occurrences and three documents/source groups on at least one side. A repeated paired direction requires at least three groups and more groups than either the opposite direction or zero differences.",
            "selection":"Top 30 absolute pooled z ranks per family; differences are exploratory and require replication on new groups before selecting a general rewriting rule.",
            "matching":"Exactly one training sample per source group and identical source-group/domain/register assignments across all compared corpora.",
            "retention":"Lexical length and number-token multisets are advisory only. Numeric formatting, signs, units, equations, and meaning require review; these checks never certify argument, detail, tone, or readability preservation.",
            "heldout":"Development and test text, metadata, features, and all detector outputs were not read. Only held-out document/group counts appear.",
        },
    });
    util::write_json(out, &result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(db: &Connection, corpus: &str, group: &str, model: bool, shift: u64) {
        let source_kind = if model { "model" } else { "human" };
        let text = format!("{corpus} {group} has 42 observations.");
        let doc = json!({"corpus":corpus,"text":text,"source_kind":source_kind,"provider":if model {Some("fixture")}else{None},
            "model":if model {Some("requested:model-v1")}else{None},"domain":"science","register":"abstract","group_id":group,"split":"train",
            "metadata":{"model_requested":"model-v1","generation_metadata":{"model_identity_source":"explicit_cli_request; resolved_model_not_reported"}}});
        let x = 1 + shift;
        let extracted = json!({"extractor":"unslop-features-rust-v1;test-parser-v1",
            "families":{"word":{"x":x,"other":100-x},"construction":{"passive":shift},"pos_trigram":{"NOUN VERB NOUN":1}},
            "totals":{"word":100,"construction":1,"pos_trigram":1},"metrics":{}});
        store::add_document(db, &doc, &extracted).unwrap();
    }

    fn prepared() -> Connection {
        let db = store::connect(Path::new(":memory:")).unwrap();
        for group in ["a", "b", "c"] {
            fixture(&db, "human", group, false, 0);
            fixture(&db, "model", group, true, 1);
        }
        db
    }

    #[test]
    fn requires_exact_groups_strata_and_one_sample_per_group() {
        let directory = tempfile::tempdir().unwrap();
        let out = directory.path().join("report.json");
        let db = prepared();
        db.execute(
            "UPDATE documents SET group_id='different' WHERE corpus='model' AND group_id='c'",
            [],
        )
        .unwrap();
        assert!(
            report(&db, "human", &["model".into()], &out)
                .unwrap_err()
                .to_string()
                .contains("source groups do not match")
        );
        assert!(!out.exists());
        db.execute("UPDATE documents SET group_id='c',domain='other' WHERE corpus='model' AND group_id='different'",[]).unwrap();
        assert!(
            report(&db, "human", &["model".into()], &out)
                .unwrap_err()
                .to_string()
                .contains("domain/register mismatch")
        );
        db.execute(
            "UPDATE documents SET domain='science' WHERE corpus='model'",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO documents(corpus,sha256,text,source_kind,provider,model,domain,register,group_id,split,metadata_json,created_at) SELECT corpus,'different-sha','additional text',source_kind,provider,model,domain,register,group_id,split,metadata_json,created_at FROM documents WHERE corpus='model' AND group_id='c'",[]).unwrap();
        assert!(
            report(&db, "human", &["model".into()], &out)
                .unwrap_err()
                .to_string()
                .contains("exactly one")
        );
    }

    #[test]
    fn pairs_equal_weight_and_retains_model_identity_uncertainty() {
        let db = prepared();
        let directory = tempfile::tempdir().unwrap();
        let result = report(
            &db,
            "human",
            &["model".into()],
            &directory.path().join("report.json"),
        )
        .unwrap();
        let comparison = &result["comparisons"][0];
        let word = comparison["families"]["word"]["top_features"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["feature"] == "x")
            .unwrap();
        assert!(
            (word["paired"]["group_equal_mean_model_minus_human_rate"]
                .as_f64()
                .unwrap()
                - 0.01)
                .abs()
                < 1e-12
        );
        assert_eq!(word["paired"]["groups_model_higher"], 3);
        assert_eq!(
            word["paired"]["most_common_direction_repeated_at_least_three_groups"],
            "model_higher"
        );
        assert_eq!(comparison["provenance"][0]["model_requested"], "model-v1");
        assert!(comparison["provenance"][0]["model_resolved"].is_null());
        assert_eq!(
            comparison["retention_advisories"][0]["fidelity_status"],
            "not_reviewed"
        );
    }

    #[test]
    fn zero_differences_and_missing_opportunities_are_separate() {
        let counts = |x, total| (total, BTreeMap::from([("x".into(), x)]));
        let human = BTreeMap::from([
            ("a".into(), counts(1, 2)),
            ("b".into(), counts(1, 2)),
            ("c".into(), counts(0, 0)),
        ]);
        let model = BTreeMap::from([
            ("a".into(), counts(2, 4)),
            ("b".into(), counts(0, 2)),
            ("c".into(), counts(1, 2)),
        ]);
        let result = paired_rates("x", &human, &model);
        assert_eq!(result["groups_equal"], 1);
        assert_eq!(result["groups_model_lower"], 1);
        assert_eq!(result["groups_without_opportunities"], 1);
        assert_eq!(result["group_equal_mean_model_minus_human_rate"], -0.25);
        assert!(result["most_common_direction_repeated_at_least_three_groups"].is_null());
    }

    #[test]
    fn heldout_text_metadata_and_detector_outputs_are_not_read() {
        let db = prepared();
        db.execute("INSERT INTO documents(corpus,sha256,text,source_kind,domain,register,group_id,split,metadata_json,created_at) VALUES ('human','heldout-sha','SECRET_HELDOUT_TEXT','human','other','other','heldout','test','NOT JSON','old')",[]).unwrap();
        db.execute("INSERT INTO detector_runs(detector,run_key,document_id,record_json,imported_at) VALUES ('unused','id',1,'NOT JSON','old')",[]).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let result = report(
            &db,
            "human",
            &["model".into()],
            &directory.path().join("report.json"),
        )
        .unwrap();
        assert_eq!(result["human_heldout_counts"][0]["documents"], 1);
        assert!(!result.to_string().contains("SECRET_HELDOUT_TEXT"));
    }

    #[test]
    fn number_advisories_preserve_multiplicities_and_never_certify_fidelity() {
        assert_eq!(
            number_tokens("42 observations, 42 controls; p=0.05."),
            BTreeMap::from([("0.05".into(), 1), ("42".into(), 2)])
        );
        let db = prepared();
        let human = training(&db, "human", "human").unwrap();
        let mut model = training(&db, "model", "model").unwrap();
        model.get_mut("a").unwrap().text = "There were 43 observations.".into();
        let result = advisory(&human["a"], &model["a"], "a");
        assert_eq!(result["missing_number_tokens"]["42"], 1);
        assert_eq!(result["added_number_tokens"]["43"], 1);
        assert_eq!(result["number_token_multiset_equal"], false);
        assert_eq!(result["fidelity_status"], "not_reviewed");
    }

    #[test]
    fn missing_shared_extractor_fails_before_writing_report() {
        let db = prepared();
        db.execute("DELETE FROM features WHERE document_id=2", [])
            .unwrap();
        db.execute("DELETE FROM totals WHERE document_id=2", [])
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let out = directory.path().join("report.json");
        assert!(
            report(&db, "human", &["model".into()], &out)
                .unwrap_err()
                .to_string()
                .contains("common Rust grammar extractor")
        );
        assert!(!out.exists());
    }
}
