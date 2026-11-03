//! Later-date evaluation of unchanged, TRAIN-only conditional preferences.
use crate::edit_preference_model::{
    AUTHOR_PRIOR_DATES, BOOTSTRAP_REPLICATES, BOOTSTRAP_SEED, Loss, LossPair,
    POPULATION_PRIOR_FAILURE, POPULATION_PRIOR_SUCCESS, Post, Prediction, PreferenceModel,
    validate_conditioning_key,
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "slopninja-preference-chronology-evaluation-v1";

/// Exact ISO civil-date validation. Lexical comparison is chronological only
/// after this check has passed for both TRAIN and query metadata.
pub fn validate_date(date: &str) -> Result<()> {
    let bytes = date.as_bytes();
    ensure!(
        bytes.len() == 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit()),
        "Date must be exactly YYYY-MM-DD"
    );
    let year: u32 = date[0..4].parse()?;
    let month: u32 = date[5..7].parse()?;
    let day: u32 = date[8..10].parse()?;
    ensure!(
        year > 0 && (1..=12).contains(&month),
        "Invalid civil date year or month"
    );
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    ensure!((1..=days).contains(&day), "Invalid civil date day");
    Ok(())
}

#[derive(Default)]
struct TrainingAuthor {
    panel: String,
    posts: usize,
    dates: BTreeSet<String>,
    opportunities: usize,
}

#[derive(Clone, Copy, Default, Serialize)]
struct Coverage {
    /// All known TRAIN authors, including those without query metadata.
    authors: usize,
    authors_with_query_posts: usize,
    scorable_authors: usize,
    dates: usize,
    scorable_dates: usize,
    posts: usize,
    scorable_posts: usize,
    opportunities: usize,
}

impl Coverage {
    fn add(&mut self, other: Self) {
        self.authors += other.authors;
        self.authors_with_query_posts += other.authors_with_query_posts;
        self.scorable_authors += other.scorable_authors;
        self.dates += other.dates;
        self.scorable_dates += other.scorable_dates;
        self.posts += other.posts;
        self.scorable_posts += other.scorable_posts;
        self.opportunities += other.opportunities;
    }
}

#[derive(Serialize)]
struct AuthorResult {
    author: String,
    panel: String,
    training_posts: usize,
    training_dates: usize,
    training_opportunities: usize,
    maximum_training_date: String,
    first_query_date: Option<String>,
    coverage: Coverage,
    losses: Option<LossPair>,
}

#[derive(Clone, Copy)]
struct PostResult {
    opportunities: usize,
    losses: Option<LossPair>,
}

type GroupedQueries = BTreeMap<String, BTreeMap<String, Vec<PostResult>>>;

struct ScoredEvent {
    post_index: usize,
    contracted: bool,
    population_probability: f64,
    author_probability: f64,
}

fn loss(probability: f64, contracted: bool) -> Result<Loss> {
    ensure!(
        probability.is_finite() && probability > 0.0 && probability < 1.0,
        "Prediction must be finite and strictly between zero and one"
    );
    Ok(Loss {
        brier: (probability - f64::from(contracted)).powi(2),
        log_loss: if contracted {
            -probability.ln()
        } else {
            -(-probability).ln_1p()
        },
    })
}

fn difference(population: Loss, personal: Loss) -> Loss {
    Loss {
        brier: population.brier - personal.brier,
        log_loss: population.log_loss - personal.log_loss,
    }
}

fn add(total: &mut Loss, value: Loss, weight: f64) {
    total.brier += value.brier * weight;
    total.log_loss += value.log_loss * weight;
}

fn average(values: &[LossPair]) -> Option<LossPair> {
    if values.is_empty() {
        return None;
    }
    let mut result = LossPair::default();
    for value in values {
        let weight = 1.0 / values.len() as f64;
        add(
            &mut result.population_context,
            value.population_context,
            weight,
        );
        add(&mut result.author_context, value.author_context, weight);
        add(&mut result.improvement, value.improvement, weight);
    }
    Some(result)
}

/// Construct the estimator from TRAIN only and evaluate disjoint later posts.
/// The runner must separately bind source groups, text, and annotation hashes;
/// `Post` exposes only identities, dates and declared choice labels.
pub fn evaluate(training: Vec<Post>, mut queries: Vec<Post>) -> Result<Value> {
    ensure!(!training.is_empty(), "TRAIN metadata cannot be empty");
    let mut training_authors = BTreeMap::<String, TrainingAuthor>::new();
    let mut training_ids = BTreeSet::new();
    for post in &training {
        validate_date(&post.date).with_context(|| format!("Invalid TRAIN date for {}", post.id))?;
        ensure!(
            training_ids.insert(post.id.clone()),
            "Duplicate TRAIN post ID"
        );
        let author = training_authors.entry(post.author.clone()).or_default();
        if author.posts > 0 {
            ensure!(author.panel == post.panel, "TRAIN author spans panels");
        }
        author.panel = post.panel.clone();
        author.posts += 1;
        author.dates.insert(post.date.clone());
        author.opportunities += post.observations.len();
    }
    let training_posts = training.len();
    let training_opportunities: usize = training.iter().map(|post| post.observations.len()).sum();
    // This is the only estimator construction. Query records are neither
    // appended here nor used to update the model at any later point.
    let model = PreferenceModel::new(training)?;
    queries.sort_by(|a, b| a.id.cmp(&b.id));
    let mut query_ids = BTreeSet::new();
    for post in &queries {
        ensure!(
            [&post.id, &post.author, &post.date, &post.panel]
                .iter()
                .all(|value| !value.trim().is_empty() && value.trim() == value.as_str()),
            "Query metadata must be nonempty without surrounding whitespace"
        );
        ensure!(
            !training_ids.contains(&post.id) && query_ids.insert(post.id.clone()),
            "Duplicate or overlapping query post ID"
        );
        validate_date(&post.date).with_context(|| format!("Invalid query date for {}", post.id))?;
        let author = training_authors
            .get(&post.author)
            .context("Unknown query author")?;
        ensure!(post.panel == author.panel, "Query author changed panel");
        let maximum = author
            .dates
            .last()
            .expect("TRAIN author has at least one date");
        ensure!(
            &post.date > maximum,
            "Query date must follow the author's maximum TRAIN date"
        );
        for observation in &post.observations {
            validate_conditioning_key(&observation.conditioning_key)?;
        }
    }
    let mut predictions = BTreeMap::<(String, String), Prediction>::new();
    let mut grouped = GroupedQueries::new();
    let mut post_rows = Vec::new();
    let mut calibration_events = Vec::new();
    let mut population_prior_only = 0;
    let mut author_population_only = 0;
    for (post_index, post) in queries.iter().enumerate() {
        let mut event_rows = Vec::new();
        let mut event_losses = Vec::new();
        for (index, observation) in post.observations.iter().enumerate() {
            let key = (post.author.clone(), observation.conditioning_key.clone());
            if !predictions.contains_key(&key) {
                predictions.insert(
                    key.clone(),
                    model.predict(&post.author, None, &observation.conditioning_key)?,
                );
            }
            let prediction = &predictions[&key];
            let population_context =
                loss(prediction.population_probability, observation.contracted)?;
            let author_context = loss(prediction.author_probability, observation.contracted)?;
            let losses = LossPair {
                population_context,
                author_context,
                improvement: difference(population_context, author_context),
            };
            event_losses.push(losses);
            event_rows.push(
                json!({"index":index,"conditioning_key":observation.conditioning_key,
                "contracted":observation.contracted,"prediction":prediction,"losses":losses}),
            );
            population_prior_only += usize::from(prediction.population_uses_prior_only);
            author_population_only += usize::from(prediction.author_uses_population_only);
            calibration_events.push(ScoredEvent {
                post_index,
                contracted: observation.contracted,
                population_probability: prediction.population_probability,
                author_probability: prediction.author_probability,
            });
        }
        let result = PostResult {
            opportunities: post.observations.len(),
            losses: average(&event_losses),
        };
        grouped
            .entry(post.author.clone())
            .or_default()
            .entry(post.date.clone())
            .or_default()
            .push(result);
        post_rows.push(
            json!({"id":post.id,"author":post.author,"date":post.date,"panel":post.panel,
            "opportunities":result.opportunities,"losses":result.losses,"observations":event_rows}),
        );
    }
    let mut date_rows = Vec::new();
    let mut authors = Vec::new();
    // Traverse TRAIN author metadata rather than query support, retaining
    // authors who have no query records at all.
    for (author, train) in &training_authors {
        let mut coverage = Coverage {
            authors: 1,
            ..Coverage::default()
        };
        let mut date_losses = Vec::new();
        let mut first_query_date = None;
        if let Some(dates) = grouped.get(author) {
            coverage.authors_with_query_posts = 1;
            coverage.dates = dates.len();
            first_query_date = dates.keys().next().cloned();
            for (date, posts) in dates {
                let values: Vec<_> = posts.iter().filter_map(|post| post.losses).collect();
                let losses = average(&values);
                let opportunities: usize = posts.iter().map(|post| post.opportunities).sum();
                coverage.posts += posts.len();
                coverage.scorable_posts += values.len();
                coverage.opportunities += opportunities;
                if let Some(value) = losses {
                    date_losses.push(value);
                }
                date_rows.push(json!({"author":author,"date":date,"panel":train.panel,
                    "posts":posts.len(),"scorable_posts":values.len(),"opportunities":opportunities,"losses":losses}));
            }
        }
        coverage.scorable_dates = date_losses.len();
        coverage.scorable_authors = usize::from(!date_losses.is_empty());
        authors.push(AuthorResult {
            author: author.clone(),
            panel: train.panel.clone(),
            training_posts: train.posts,
            training_dates: train.dates.len(),
            training_opportunities: train.opportunities,
            maximum_training_date: train.dates.last().expect("nonempty TRAIN dates").clone(),
            first_query_date,
            coverage,
            losses: average(&date_losses),
        });
    }
    let panel_names: BTreeSet<_> = training_authors
        .values()
        .map(|row| row.panel.clone())
        .collect();
    let panels: BTreeMap<_, _> = panel_names
        .iter()
        .map(|panel| {
            let subset: Vec<_> = authors.iter().filter(|row| &row.panel == panel).collect();
            (panel.clone(), summary(&subset))
        })
        .collect();
    let pooled = summary(&authors.iter().collect::<Vec<_>>());
    Ok(json!({
        "schema":SCHEMA,"model_schema":crate::edit_preference_model::SCHEMA,
        "estimators":["population_context","author_context"],
        "training_coverage":{"authors":training_authors.len(),"posts":training_posts,
            "author_dates":training_authors.values().map(|row|row.dates.len()).sum::<usize>(),
            "opportunities":training_opportunities},
        "settings":{
            "author_prior_effective_dates":AUTHOR_PRIOR_DATES,
            "population_beta_success":POPULATION_PRIOR_SUCCESS,"population_beta_failure":POPULATION_PRIOR_FAILURE,
            "estimator_inputs":"TRAIN only; query labels never inserted or used for profile estimation",
            "population_exclusion":"whole target author from TRAIN population",
            "population_calendar_policy":"fixed TRAIN background; no global as-of cutoff across other authors",
            "personal_history":"all available TRAIN dates; excluded_date=None",
            "chronology":"every query date strictly after that author's maximum TRAIN date; both dates strictly validated YYYY-MM-DD",
            "aggregation":"equal opportunities within contributing post; equal scorable posts within date; equal scorable dates within author; equal scorable authors",
            "primary":"population Brier loss minus author Brier loss; positive favors author",
            "secondary":"population natural-log loss minus author natural-log loss",
            "no_opportunity":"all known TRAIN authors and all query metadata retained; null losses for unavailable support"
        },
        "coverage":pooled["coverage"],"pooled":pooled,"panels":panels,
        "opportunity_prediction_coverage":{"population_prior_only":population_prior_only,
            "author_population_only":author_population_only,"total":calibration_events.len()},
        "post_rows":post_rows,"date_rows":date_rows,"author_rows":authors,
        "calibration":calibration(&calibration_events,&queries,&grouped,&authors),
        "paired_author_bootstrap":bootstrap(&authors),
        "optimizer_steps":0,"profile_estimation":true,"query_profile_updates":0,
        "hyperparameter_search":false,"new_author_evaluation":false,
        "interpretation":"later writing from known authors; authors and estimator design were previously examined; source groups and annotation hashes must be verified by the runner"
    }))
}

fn summary(authors: &[&AuthorResult]) -> Value {
    let mut coverage = Coverage::default();
    let mut values = Vec::new();
    for author in authors {
        coverage.add(author.coverage);
        if let Some(losses) = author.losses {
            values.push(losses);
        }
    }
    json!({"coverage":coverage,"macro_author_losses":average(&values)})
}

#[derive(Default)]
struct CalibrationBin {
    weight: f64,
    prediction_sum: f64,
    outcome_sum: f64,
    opportunities: usize,
    authors: BTreeSet<String>,
    dates: BTreeSet<(String, String)>,
    posts: BTreeSet<String>,
}

fn calibration(
    events: &[ScoredEvent],
    posts: &[Post],
    grouped: &GroupedQueries,
    authors: &[AuthorResult],
) -> Value {
    let author_lookup: BTreeMap<_, _> = authors
        .iter()
        .map(|row| (row.author.as_str(), row))
        .collect();
    let scorable_authors = authors.iter().filter(|row| row.losses.is_some()).count();
    let mut models = BTreeMap::new();
    for model in ["population_context", "author_context"] {
        let mut bins: [CalibrationBin; 10] = std::array::from_fn(|_| CalibrationBin::default());
        for event in events {
            let post = &posts[event.post_index];
            let dates = author_lookup[post.author.as_str()].coverage.scorable_dates;
            let date_posts = grouped[&post.author][&post.date]
                .iter()
                .filter(|row| row.losses.is_some())
                .count();
            let weight = 1.0
                / scorable_authors as f64
                / dates as f64
                / date_posts as f64
                / post.observations.len() as f64;
            let probability = if model == "population_context" {
                event.population_probability
            } else {
                event.author_probability
            };
            let bin = &mut bins[(probability * 10.0).floor().min(9.0) as usize];
            bin.weight += weight;
            bin.prediction_sum += weight * probability;
            bin.outcome_sum += weight * f64::from(event.contracted);
            bin.opportunities += 1;
            bin.authors.insert(post.author.clone());
            bin.dates.insert((post.author.clone(), post.date.clone()));
            bin.posts.insert(post.id.clone());
        }
        let total_weight: f64 = bins.iter().map(|bin| bin.weight).sum();
        let prediction_sum: f64 = bins.iter().map(|bin| bin.prediction_sum).sum();
        let outcome_sum: f64 = bins.iter().map(|bin| bin.outcome_sum).sum();
        let rows:Vec<_>=bins.iter().enumerate().map(|(index,bin)|json!({
            "index":index,"lower_probability":index as f64/10.0,"upper_probability":(index+1) as f64/10.0,
            "raw_opportunities":bin.opportunities,"raw_posts":bin.posts.len(),"raw_author_dates":bin.dates.len(),
            "raw_authors":bin.authors.len(),"metric_weight_mass":bin.weight,
            "mean_prediction":(bin.weight>0.0).then(||bin.prediction_sum/bin.weight),
            "observed_contraction_rate":(bin.weight>0.0).then(||bin.outcome_sum/bin.weight),
            "observed_minus_predicted":(bin.weight>0.0).then(||(bin.outcome_sum-bin.prediction_sum)/bin.weight)
        })).collect();
        models.insert(model,json!({"total_metric_weight":total_weight,"raw_opportunities":events.len(),
            "overall_mean_prediction":(total_weight>0.0).then(||prediction_sum/total_weight),
            "overall_observed_contraction_rate":(total_weight>0.0).then(||outcome_sum/total_weight),"bins":rows}));
    }
    json!({"schema":"slopninja-preference-chronology-calibration-v1","bin_count":10,
        "bin_rule":"floor(10*p), capped at9; [lower,upper), last bin includes1",
        "weighting":"same equal opportunity/post/date/scorable-author weights as pooled losses",
        "scorable_authors":scorable_authors,"models":models,"use":"descriptive only; no calibration fitting or selection"})
}

struct Random(u64);
impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn bootstrap(authors: &[AuthorResult]) -> Value {
    let mut strata = BTreeMap::<String, Vec<&AuthorResult>>::new();
    for author in authors {
        let group = strata.entry(author.panel.clone()).or_default();
        if author.losses.is_some() {
            group.push(author);
        }
    }
    for group in strata.values_mut() {
        group.sort_by(|a, b| a.author.cmp(&b.author));
    }
    let count: usize = strata.values().map(Vec::len).sum();
    let counts: BTreeMap<_, _> = strata
        .iter()
        .map(|(panel, rows)| (panel, rows.len()))
        .collect();
    let mut result = json!({"schema":"slopninja-preference-chronology-bootstrap-v1",
        "replicates":BOOTSTRAP_REPLICATES,"seed_hex":"0x6a09e667f3bcc909",
        "rng":"xorshift64 shifts13,7,17; continuous state; historical next modulo n draws",
        "panel_order":strata.keys().collect::<Vec<_>>(),"author_order":"lexical within panel",
        "scorable_authors_per_panel":counts,"scorable_authors":count,
        "sampling":"sample the fixed number of scorable authors with replacement within each panel; pool equal author weights",
        "percentile_indices_zero_based":[49,1949],
        "interpretation":"paired interval conditional on scorable known authors and fixed TRAIN estimates; no bootstrap profile re-estimation or adoption threshold",
        "mean_improvement":null,"interval_95":null});
    if count == 0 {
        result["status"] = json!("no_scorable_authors");
        result["completed_replicates"] = json!(0);
        return result;
    }
    let mut point = Loss::default();
    for row in strata.values().flatten() {
        add(
            &mut point,
            row.losses.expect("scorable author").improvement,
            1.0 / count as f64,
        );
    }
    let mut brier = Vec::with_capacity(BOOTSTRAP_REPLICATES);
    let mut log_loss = Vec::with_capacity(BOOTSTRAP_REPLICATES);
    let mut random = Random(BOOTSTRAP_SEED);
    for _ in 0..BOOTSTRAP_REPLICATES {
        let mut replicate = Loss::default();
        for group in strata.values() {
            for _ in 0..group.len() {
                let row = group[(random.next() % group.len() as u64) as usize];
                add(
                    &mut replicate,
                    row.losses.expect("scorable author").improvement,
                    1.0 / count as f64,
                );
            }
        }
        brier.push(replicate.brier);
        log_loss.push(replicate.log_loss);
    }
    brier.sort_by(f64::total_cmp);
    log_loss.sort_by(f64::total_cmp);
    result["status"] = json!("scored");
    result["completed_replicates"] = json!(BOOTSTRAP_REPLICATES);
    result["mean_improvement"] = json!(point);
    result["interval_95"] =
        json!({"brier":[brier[49],brier[1949]],"log_loss":[log_loss[49],log_loss[1949]]});
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit_preference_model::Observation;

    fn post(id: &str, author: &str, date: &str, values: &[bool]) -> Post {
        Post {
            id: id.into(),
            author: author.into(),
            date: date.into(),
            panel: "one".into(),
            observations: values
                .iter()
                .map(|&contracted| Observation {
                    conditioning_key: "[\"does\",\"declarative\"]".into(),
                    contracted,
                })
                .collect(),
        }
    }
    fn training() -> Vec<Post> {
        vec![
            post("a0", "a", "2004-01-01", &[false]),
            post("b0", "b", "2004-01-02", &[true]),
        ]
    }
    fn close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-13, "{actual} != {expected}");
    }

    #[test]
    fn query_labels_and_later_queries_never_update_profiles() {
        let queries = vec![
            post("q1", "a", "2004-02-01", &[true]),
            post("q2", "a", "2004-03-01", &[false]),
        ];
        let report = evaluate(training(), queries.clone()).unwrap();
        let first = &report["post_rows"][0]["observations"][0]["prediction"];
        let second = &report["post_rows"][1]["observations"][0]["prediction"];
        assert_eq!(first, second);
        close(first["population_probability"].as_f64().unwrap(), 0.75);
        close(first["author_probability"].as_f64().unwrap(), 0.6);
        assert!(first["excluded_date"].is_null());
        let changed = evaluate(
            training(),
            vec![
                post("q1", "a", "2004-02-01", &[false; 25]),
                post("q2", "a", "2004-03-01", &[true]),
            ],
        )
        .unwrap();
        assert_eq!(
            first,
            &changed["post_rows"][0]["observations"][0]["prediction"]
        );
        assert_eq!(
            second,
            &changed["post_rows"][1]["observations"][0]["prediction"]
        );
        assert_ne!(
            report["pooled"]["macro_author_losses"],
            changed["pooled"]["macro_author_losses"]
        );
    }

    #[test]
    fn validates_actual_train_maximum_even_without_train_opportunities() {
        let mut train = training();
        train.push(post("a-empty-later", "a", "2004-03-03", &[]));
        for date in ["2003-12-31", "2004-02-01", "2004-03-03"] {
            assert!(evaluate(train.clone(), vec![post("q", "a", date, &[])]).is_err());
        }
        assert!(evaluate(train, vec![post("q", "a", "2004-03-04", &[])]).is_ok());
        let bad_train = vec![post("a", "a", "2004-02-30", &[])];
        assert!(evaluate(bad_train, Vec::new()).is_err());
    }

    #[test]
    fn strict_civil_dates_and_identity_disjointness() {
        for good in ["2004-02-29", "2000-02-29", "1999-12-31", "0001-01-01"] {
            validate_date(good).unwrap();
        }
        for bad in [
            "1900-02-29",
            "2003-02-29",
            "2004-04-31",
            "2004-00-01",
            "0000-01-01",
            "2004-1-01",
            "2004-01-01Z",
            "２００４-01-01",
        ] {
            assert!(validate_date(bad).is_err(), "{bad}");
        }
        assert!(evaluate(training(), vec![post("a0", "a", "2004-04-01", &[true])]).is_err());
        assert!(
            evaluate(
                training(),
                vec![post("q", "unknown", "2004-04-01", &[true])]
            )
            .is_err()
        );
        let query = post("q", "a", "2004-04-01", &[true]);
        assert!(evaluate(training(), vec![query.clone(), query.clone()]).is_err());
        let mut wrong_panel = query;
        wrong_panel.panel = "other".into();
        assert!(evaluate(training(), vec![wrong_panel]).is_err());
    }

    #[test]
    fn all_training_authors_retained_with_missing_or_empty_queries() {
        let mut train = training();
        let mut no_query = post("c0", "c", "2004-01-01", &[]);
        no_query.panel = "empty-panel".into();
        train.push(no_query);
        let report = evaluate(
            train.clone(),
            vec![
                post("q1", "a", "2004-02-01", &[true]),
                post("q2", "b", "2004-02-01", &[]),
            ],
        )
        .unwrap();
        assert_eq!(report["coverage"]["authors"], 3);
        assert_eq!(report["coverage"]["authors_with_query_posts"], 2);
        assert_eq!(report["coverage"]["scorable_authors"], 1);
        assert_eq!(report["author_rows"].as_array().unwrap().len(), 3);
        let c = report["author_rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["author"] == "c")
            .unwrap();
        assert!(c["losses"].is_null());
        assert!(c["first_query_date"].is_null());
        assert_eq!(c["coverage"]["posts"], 0);
        assert!(report["panels"]["empty-panel"]["macro_author_losses"].is_null());
        assert_eq!(
            report["paired_author_bootstrap"]["scorable_authors_per_panel"]["empty-panel"],
            0
        );
        let empty = evaluate(train, Vec::new()).unwrap();
        assert_eq!(empty["coverage"]["authors"], 3);
        assert_eq!(empty["coverage"]["posts"], 0);
        assert!(empty["pooled"]["macro_author_losses"].is_null());
        assert!(empty["paired_author_bootstrap"]["interval_95"].is_null());
        assert!(
            empty["calibration"]["models"]["author_context"]["overall_mean_prediction"].is_null()
        );
    }

    #[test]
    fn losses_calibration_and_bootstrap_use_same_equal_author_date_post_weights() {
        let report = evaluate(
            training(),
            vec![
                post("a1", "a", "2004-02-01", &[true; 100]),
                post("a2", "a", "2004-02-01", &[false]),
                post("a3", "a", "2004-02-02", &[false]),
                post("b1", "b", "2004-02-01", &[true]),
            ],
        )
        .unwrap();
        // TRAIN-only probabilities: a baseline .75/personal .6; b baseline .25/personal .4.
        // a's date-balanced observed fraction=.25; b's=1.
        close(
            report["pooled"]["macro_author_losses"]["population_context"]["brier"]
                .as_f64()
                .unwrap(),
            0.5,
        );
        close(
            report["pooled"]["macro_author_losses"]["author_context"]["brier"]
                .as_f64()
                .unwrap(),
            0.335,
        );
        close(
            report["paired_author_bootstrap"]["mean_improvement"]["brier"]
                .as_f64()
                .unwrap(),
            0.165,
        );
        for model in ["population_context", "author_context"] {
            let calibration = &report["calibration"]["models"][model];
            close(calibration["total_metric_weight"].as_f64().unwrap(), 1.0);
            close(
                calibration["overall_observed_contraction_rate"]
                    .as_f64()
                    .unwrap(),
                0.625,
            );
            assert_eq!(calibration["raw_opportunities"], 103);
            assert_eq!(calibration["bins"].as_array().unwrap().len(), 10);
            for bin in calibration["bins"].as_array().unwrap() {
                if bin["raw_opportunities"] == 0 {
                    assert!(bin["mean_prediction"].is_null());
                }
            }
        }
        let repeated = evaluate(
            training(),
            vec![
                post("a", "a", "2004-02-01", &[true]),
                post("b", "b", "2004-02-01", &[false]),
            ],
        )
        .unwrap();
        let repeated_again = evaluate(
            training(),
            vec![
                post("b", "b", "2004-02-01", &[false]),
                post("a", "a", "2004-02-01", &[true]),
            ],
        )
        .unwrap();
        assert_eq!(repeated, repeated_again);
    }

    #[test]
    fn unseen_query_context_uses_train_prior_without_learning_query_distribution() {
        let mut q = post("q", "a", "2004-02-01", &[true; 20]);
        for observation in &mut q.observations {
            observation.conditioning_key = "[\"could\",\"declarative\"]".into();
        }
        let report = evaluate(training(), vec![q]).unwrap();
        let p = &report["post_rows"][0]["observations"][0]["prediction"];
        close(p["population_probability"].as_f64().unwrap(), 0.5);
        close(p["author_probability"].as_f64().unwrap(), 0.5);
        assert_eq!(
            report["opportunity_prediction_coverage"]["population_prior_only"],
            20
        );
        assert_eq!(
            report["opportunity_prediction_coverage"]["author_population_only"],
            20
        );
        close(
            report["pooled"]["macro_author_losses"]["improvement"]["brier"]
                .as_f64()
                .unwrap(),
            0.0,
        );
    }
}
