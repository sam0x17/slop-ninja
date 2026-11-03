//! Fixed, date-balanced contraction preference estimators.
//!
//! This module consumes declared grammatical opportunities. It neither extracts
//! alternatives nor certifies their grammaticality or meaning preservation.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "slopninja-edit-preference-model-v1";
pub const AUTHOR_PRIOR_DATES: f64 = 4.0;
pub const POPULATION_PRIOR_SUCCESS: f64 = 0.5;
pub const POPULATION_PRIOR_FAILURE: f64 = 0.5;
pub const BOOTSTRAP_REPLICATES: usize = 2000;
pub const BOOTSTRAP_SEED: u64 = 0x6a09_e667_f3bc_c909;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation {
    pub conditioning_key: String,
    pub contracted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Post {
    pub id: String,
    pub author: String,
    pub date: String,
    pub panel: String,
    pub observations: Vec<Observation>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Support {
    pub authors: usize,
    pub dates: usize,
    pub posts: usize,
    pub opportunities: usize,
    pub contracted: usize,
}

impl Support {
    fn add(&mut self, other: Self) {
        self.authors += other.authors;
        self.dates += other.dates;
        self.posts += other.posts;
        self.opportunities += other.opportunities;
        self.contracted += other.contracted;
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Prediction {
    pub conditioning_key: String,
    pub excluded_date: Option<String>,
    pub population_probability: f64,
    pub author_probability: f64,
    pub population_log_odds: f64,
    pub author_log_odds: f64,
    pub population_support: Support,
    pub author_support: Support,
    /// Sum of other authors' equally date-balanced context rates.
    pub population_effective_successes: f64,
    pub population_effective_authors: usize,
    /// Sum of the target author's retained context-date rates.
    pub author_effective_successes: f64,
    pub author_effective_dates: usize,
    pub population_uses_prior_only: bool,
    pub author_uses_population_only: bool,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Loss {
    pub brier: f64,
    pub log_loss: f64,
}

impl Loss {
    fn add(&mut self, other: Self, weight: f64) {
        self.brier += weight * other.brier;
        self.log_loss += weight * other.log_loss;
    }

    fn minus(self, other: Self) -> Self {
        Self {
            brier: self.brier - other.brier,
            log_loss: self.log_loss - other.log_loss,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct LossPair {
    pub population_context: Loss,
    pub author_context: Loss,
    /// Positive means lower loss under the author estimator.
    pub improvement: Loss,
}

impl LossPair {
    fn add(&mut self, other: Self, weight: f64) {
        self.population_context
            .add(other.population_context, weight);
        self.author_context.add(other.author_context, weight);
        self.improvement.add(other.improvement, weight);
    }
}

fn bernoulli_loss(probability: f64, contracted: bool) -> Result<Loss> {
    ensure!(
        probability.is_finite() && probability > 0.0 && probability < 1.0,
        "Probability must be finite and strictly between zero and one"
    );
    let observed = f64::from(contracted);
    Ok(Loss {
        brier: (probability - observed).powi(2),
        log_loss: if contracted {
            -probability.ln()
        } else {
            -(-probability).ln_1p()
        },
    })
}

fn losses(prediction: &Prediction, contracted: bool) -> Result<LossPair> {
    let population_context = bernoulli_loss(prediction.population_probability, contracted)?;
    let author_context = bernoulli_loss(prediction.author_probability, contracted)?;
    Ok(LossPair {
        population_context,
        author_context,
        improvement: population_context.minus(author_context),
    })
}

fn average(values: &[LossPair]) -> Option<LossPair> {
    if values.is_empty() {
        return None;
    }
    let mut result = LossPair::default();
    for &value in values {
        result.add(value, 1.0 / values.len() as f64);
    }
    Some(result)
}

#[derive(Clone, Copy, Debug, Default)]
struct EventCounts {
    contracted: usize,
    total: usize,
}

type DatePosts = BTreeMap<String, EventCounts>;
type AuthorDates = BTreeMap<String, DatePosts>;
type ContextAuthors = BTreeMap<String, AuthorDates>;

fn date_rate(posts: &DatePosts) -> f64 {
    posts
        .values()
        .map(|counts| counts.contracted as f64 / counts.total as f64 / posts.len() as f64)
        .sum()
}

fn author_stats(dates: &AuthorDates, excluded_date: Option<&str>) -> (f64, Support) {
    let mut successes = 0.0;
    let mut support = Support::default();
    for (date, posts) in dates {
        if excluded_date == Some(date.as_str()) {
            continue;
        }
        successes += date_rate(posts);
        support.dates += 1;
        support.posts += posts.len();
        for counts in posts.values() {
            support.opportunities += counts.total;
            support.contracted += counts.contracted;
        }
    }
    support.authors = usize::from(support.dates > 0);
    (successes, support)
}

/// Validate the exact extractor key without inspecting source text.
pub fn validate_conditioning_key(key: &str) -> Result<()> {
    let parts: [String; 2] = serde_json::from_str(key)
        .context("Conditioning key must be compact JSON [inflected_auxiliary, clause_type]")?;
    ensure!(
        !parts[0].trim().is_empty()
            && parts[0] == parts[0].trim()
            && matches!(parts[1].as_str(), "declarative" | "do_imperative"),
        "Invalid auxiliary or clause type"
    );
    ensure!(
        serde_json::to_string(&parts)? == key,
        "Noncanonical conditioning key"
    );
    Ok(())
}

pub struct PreferenceModel {
    posts: Vec<Post>,
    // context -> author -> date -> contributing post -> event counts
    contexts: BTreeMap<String, ContextAuthors>,
    author_panels: BTreeMap<String, String>,
}

impl PreferenceModel {
    /// Observed canonical contexts only, in stable lexical order. A caller can
    /// still request a valid unseen context through `predict` and inspect its
    /// explicit prior-only support; no fictional observations are added here.
    pub fn context_keys(&self) -> Vec<String> {
        self.contexts.keys().cloned().collect()
    }

    /// Retains every input post, including posts with no accepted opportunity.
    pub fn new(mut posts: Vec<Post>) -> Result<Self> {
        ensure!(!posts.is_empty(), "At least one metadata row is required");
        posts.sort_by(|a, b| a.id.cmp(&b.id));
        let mut ids = BTreeSet::new();
        let mut author_panels = BTreeMap::new();
        let mut contexts = BTreeMap::<String, ContextAuthors>::new();
        let mut checked_contexts = BTreeSet::new();
        for post in &posts {
            ensure!(
                [&post.id, &post.author, &post.date, &post.panel]
                    .iter()
                    .all(|value| !value.trim().is_empty() && value.trim() == value.as_str()),
                "Post metadata must be nonempty with no surrounding whitespace"
            );
            ensure!(ids.insert(post.id.clone()), "Repeated post ID");
            if let Some(panel) = author_panels.insert(post.author.clone(), post.panel.clone()) {
                ensure!(panel == post.panel, "One author appears in multiple panels");
            }
            for observation in &post.observations {
                if checked_contexts.insert(observation.conditioning_key.clone()) {
                    validate_conditioning_key(&observation.conditioning_key)?;
                }
                let counts = contexts
                    .entry(observation.conditioning_key.clone())
                    .or_default()
                    .entry(post.author.clone())
                    .or_default()
                    .entry(post.date.clone())
                    .or_default()
                    .entry(post.id.clone())
                    .or_default();
                counts.total += 1;
                counts.contracted += usize::from(observation.contracted);
            }
        }
        Ok(Self {
            posts,
            contexts,
            author_panels,
        })
    }

    /// Population always excludes the complete target author. `None` includes
    /// every available date from the target profile for deployment diagnostics.
    pub fn predict(
        &self,
        author: &str,
        excluded_date: Option<&str>,
        conditioning_key: &str,
    ) -> Result<Prediction> {
        ensure!(!author.trim().is_empty(), "Target author must be nonempty");
        validate_conditioning_key(conditioning_key)?;
        let mut population_effective_successes = 0.0;
        let mut population_support = Support::default();
        let mut author_effective_successes = 0.0;
        let mut author_support = Support::default();
        if let Some(authors) = self.contexts.get(conditioning_key) {
            for (candidate, dates) in authors {
                if candidate == author {
                    (author_effective_successes, author_support) =
                        author_stats(dates, excluded_date);
                } else {
                    let (successes, support) = author_stats(dates, None);
                    population_effective_successes += successes / support.dates as f64;
                    population_support.add(support);
                }
            }
        }
        let population_probability = (population_effective_successes + POPULATION_PRIOR_SUCCESS)
            / (population_support.authors as f64
                + POPULATION_PRIOR_SUCCESS
                + POPULATION_PRIOR_FAILURE);
        let author_probability = (author_effective_successes
            + AUTHOR_PRIOR_DATES * population_probability)
            / (author_support.dates as f64 + AUTHOR_PRIOR_DATES);
        // The finite Beta priors keep both estimates inside (0,1); never clamp
        // a failed calculation into a seemingly valid probability.
        bernoulli_loss(population_probability, false)?;
        bernoulli_loss(author_probability, false)?;
        Ok(Prediction {
            conditioning_key: conditioning_key.to_owned(),
            excluded_date: excluded_date.map(str::to_owned),
            population_probability,
            author_probability,
            population_log_odds: population_probability.ln() - (-population_probability).ln_1p(),
            author_log_odds: author_probability.ln() - (-author_probability).ln_1p(),
            population_effective_successes,
            population_effective_authors: population_support.authors,
            author_effective_successes,
            author_effective_dates: author_support.dates,
            population_uses_prior_only: population_support.authors == 0,
            author_uses_population_only: author_support.dates == 0,
            population_support,
            author_support,
        })
    }

    /// Whole-date-out probabilities and Bernoulli losses. The returned value
    /// includes all observation, post, date, and author rows for local auditing.
    pub fn evaluate(&self) -> Result<Value> {
        let mut post_rows = Vec::new();
        let mut grouped = BTreeMap::<String, BTreeMap<String, Vec<PostResult>>>::new();
        let mut predictions = BTreeMap::<(String, String, String), Prediction>::new();
        let mut calibration_events = Vec::new();
        let mut population_prior_only = 0;
        let mut author_population_only = 0;
        for (post_index, post) in self.posts.iter().enumerate() {
            let mut event_rows = Vec::new();
            let mut event_losses = Vec::new();
            for (index, observation) in post.observations.iter().enumerate() {
                let key = (
                    post.author.clone(),
                    post.date.clone(),
                    observation.conditioning_key.clone(),
                );
                if !predictions.contains_key(&key) {
                    predictions.insert(
                        key.clone(),
                        self.predict(
                            &post.author,
                            Some(&post.date),
                            &observation.conditioning_key,
                        )?,
                    );
                }
                let prediction = &predictions[&key];
                let loss = losses(prediction, observation.contracted)?;
                population_prior_only += usize::from(prediction.population_uses_prior_only);
                author_population_only += usize::from(prediction.author_uses_population_only);
                event_losses.push(loss);
                calibration_events.push(ScoredEvent {
                    post_index,
                    contracted: observation.contracted,
                    population_probability: prediction.population_probability,
                    author_probability: prediction.author_probability,
                });
                event_rows.push(json!({
                    "index":index,"conditioning_key":observation.conditioning_key,
                    "contracted":observation.contracted,"prediction":prediction,"losses":loss
                }));
            }
            let post_result = PostResult {
                opportunities: post.observations.len(),
                losses: average(&event_losses),
            };
            post_rows.push(json!({
                "id":post.id,"author":post.author,"date":post.date,"panel":post.panel,
                "opportunities":post_result.opportunities,"losses":post_result.losses,
                "observations":event_rows
            }));
            grouped
                .entry(post.author.clone())
                .or_default()
                .entry(post.date.clone())
                .or_default()
                .push(post_result);
        }
        let mut date_rows = Vec::new();
        let mut authors = Vec::new();
        for (author, dates) in &grouped {
            let mut date_losses = Vec::new();
            let mut coverage = Coverage {
                authors: 1,
                dates: dates.len(),
                ..Coverage::default()
            };
            for (date, posts) in dates {
                let values: Vec<_> = posts.iter().filter_map(|post| post.losses).collect();
                let result = average(&values);
                coverage.posts += posts.len();
                coverage.scorable_posts += values.len();
                coverage.opportunities +=
                    posts.iter().map(|post| post.opportunities).sum::<usize>();
                if let Some(value) = result {
                    date_losses.push(value);
                }
                date_rows.push(json!({
                    "author":author,"date":date,"panel":self.author_panels[author],
                    "posts":posts.len(),"scorable_posts":values.len(),
                    "opportunities":posts.iter().map(|post| post.opportunities).sum::<usize>(),
                    "losses":result
                }));
            }
            coverage.scorable_dates = date_losses.len();
            coverage.scorable_authors = usize::from(!date_losses.is_empty());
            authors.push(AuthorResult {
                author: author.clone(),
                panel: self.author_panels[author].clone(),
                coverage,
                losses: average(&date_losses),
            });
        }
        let panel_names: BTreeSet<_> = self.author_panels.values().cloned().collect();
        let panels: BTreeMap<_, _> = panel_names
            .iter()
            .map(|panel| {
                let subset: Vec<_> = authors.iter().filter(|row| &row.panel == panel).collect();
                (panel.clone(), summary(&subset))
            })
            .collect();
        let pooled = summary(&authors.iter().collect::<Vec<_>>());
        Ok(json!({
            "schema":"slopninja-edit-preference-evaluation-v1",
            "model_schema":SCHEMA,
            "estimators":["population_context","author_context"],
            "settings":{
                "author_prior_effective_dates":AUTHOR_PRIOR_DATES,
                "population_beta_success":POPULATION_PRIOR_SUCCESS,
                "population_beta_failure":POPULATION_PRIOR_FAILURE,
                "population_exclusion":"whole target author",
                "author_exclusion":"whole query date",
                "aggregation":"equal opportunities within contributing post; equal scorable posts within date; equal scorable dates within author; equal scorable authors",
                "primary":"population Brier loss minus author Brier loss; positive favors author",
                "secondary":"population natural-log loss minus author natural-log loss",
                "no_opportunity":"retain all rows with null losses; never fabricate a Bernoulli event"
            },
            "coverage":pooled["coverage"],"pooled":pooled,"panels":panels,
            "opportunity_prediction_coverage":{
                "population_prior_only":population_prior_only,
                "author_population_only":author_population_only,
                "total":self.posts.iter().map(|post| post.observations.len()).sum::<usize>()
            },
            "post_rows":post_rows,"date_rows":date_rows,"author_rows":authors,
            "calibration":calibration(&calibration_events,&self.posts,&grouped,&authors),
            "paired_author_bootstrap":bootstrap(&authors),
            "optimizer_steps":0,"profile_estimation":true,
            "hyperparameter_search":false,"fresh_confirmation":false
        }))
    }
}

#[derive(Clone, Copy)]
struct PostResult {
    opportunities: usize,
    losses: Option<LossPair>,
}

struct ScoredEvent {
    post_index: usize,
    contracted: bool,
    population_probability: f64,
    author_probability: f64,
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
    grouped: &BTreeMap<String, BTreeMap<String, Vec<PostResult>>>,
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
            let scorable_dates = author_lookup[post.author.as_str()].coverage.scorable_dates;
            let scorable_posts = grouped[&post.author][&post.date]
                .iter()
                .filter(|row| row.losses.is_some())
                .count();
            let weight = 1.0
                / scorable_authors as f64
                / scorable_dates as f64
                / scorable_posts as f64
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
        let rows: Vec<_> = bins.iter().enumerate().map(|(index,bin)| json!({
            "index":index,"lower_probability":index as f64/10.0,
            "upper_probability":(index+1) as f64/10.0,
            "raw_opportunities":bin.opportunities,"raw_posts":bin.posts.len(),
            "raw_author_dates":bin.dates.len(),"raw_authors":bin.authors.len(),
            "metric_weight_mass":bin.weight,
            "mean_prediction":(bin.weight>0.0).then(||bin.prediction_sum/bin.weight),
            "observed_contraction_rate":(bin.weight>0.0).then(||bin.outcome_sum/bin.weight),
            "observed_minus_predicted":(bin.weight>0.0).then(||(bin.outcome_sum-bin.prediction_sum)/bin.weight)
        })).collect();
        models.insert(model,json!({
            "total_metric_weight":total_weight,"raw_opportunities":events.len(),
            "overall_mean_prediction":(total_weight>0.0).then(||prediction_sum/total_weight),
            "overall_observed_contraction_rate":(total_weight>0.0).then(||outcome_sum/total_weight),
            "bins":rows
        }));
    }
    json!({
        "schema":"slopninja-edit-preference-calibration-v1",
        "bin_count":10,"bin_rule":"floor(10*p), capped at9; [lower,upper), last bin includes1",
        "weighting":"same equal opportunity/post/date/scorable-author weights as pooled losses",
        "scorable_authors":scorable_authors,"models":models,
        "use":"descriptive diagnostic only; no calibration fitting or selection"
    })
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
struct Coverage {
    authors: usize,
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
        self.scorable_authors += other.scorable_authors;
        self.dates += other.dates;
        self.scorable_dates += other.scorable_dates;
        self.posts += other.posts;
        self.scorable_posts += other.scorable_posts;
        self.opportunities += other.opportunities;
    }
}

#[derive(Clone, Debug, Serialize)]
struct AuthorResult {
    author: String,
    panel: String,
    coverage: Coverage,
    losses: Option<LossPair>,
}

fn summary(authors: &[&AuthorResult]) -> Value {
    let mut coverage = Coverage::default();
    let mut values = Vec::new();
    for author in authors {
        coverage.add(author.coverage);
        if let Some(loss) = author.losses {
            values.push(loss);
        }
    }
    json!({"coverage":coverage,"macro_author_losses":average(&values)})
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
    let mut result = json!({
        "schema":"slopninja-edit-preference-bootstrap-v1",
        "replicates":BOOTSTRAP_REPLICATES,"seed_hex":"0x6a09e667f3bcc909",
        "rng":"xorshift64 shifts13,7,17; continuous state; historical next modulo n draws",
        "panel_order":strata.keys().collect::<Vec<_>>(),
        "author_order":"lexical within panel",
        "scorable_authors_per_panel":counts,"scorable_authors":count,
        "sampling":"sample the fixed number of scorable authors with replacement within each panel; pool equal author weights",
        "percentile_indices_zero_based":[49,1949],
        "interpretation":"descriptive reused-TRAIN interval conditional on scorable authors; no fresh confirmation or adoption threshold",
        "mean_improvement":null,"interval_95":null
    });
    if count == 0 {
        result["status"] = json!("no_scorable_authors");
        result["completed_replicates"] = json!(0);
        return result;
    }
    let mut point = Loss::default();
    for row in strata.values().flatten() {
        point.add(
            row.losses.expect("filtered scorable author").improvement,
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
                replicate.add(
                    row.losses.expect("filtered scorable author").improvement,
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

    fn key(auxiliary: &str, clause: &str) -> String {
        serde_json::to_string(&[auxiliary, clause]).unwrap()
    }
    fn post(id: &str, author: &str, date: &str, values: &[bool]) -> Post {
        Post {
            id: id.into(),
            author: author.into(),
            date: date.into(),
            panel: "one".into(),
            observations: values
                .iter()
                .map(|&contracted| Observation {
                    conditioning_key: key("do", "declarative"),
                    contracted,
                })
                .collect(),
        }
    }
    fn close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-13, "{actual} != {expected}");
    }

    #[test]
    fn whole_date_exclusion_and_complete_population_author_exclusion() {
        let model = PreferenceModel::new(vec![
            post("a1", "a", "d1", &[true, true]),
            post("a2", "a", "d1", &[true]),
            post("a3", "a", "d2", &[false]),
            post("b1", "b", "d1", &[true]),
        ])
        .unwrap();
        let p = model
            .predict("a", Some("d1"), &key("do", "declarative"))
            .unwrap();
        close(p.population_probability, 0.75);
        close(p.author_probability, 0.6);
        assert_eq!(
            p.author_support,
            Support {
                authors: 1,
                dates: 1,
                posts: 1,
                opportunities: 1,
                contracted: 0
            }
        );
        assert_eq!(p.population_support.authors, 1);
        assert_eq!(p.population_support.opportunities, 1);
        let full = model.predict("a", None, &key("do", "declarative")).unwrap();
        close(full.author_probability, 4.0 / 6.0);
        close(full.population_probability, p.population_probability);
        let changed = PreferenceModel::new(vec![
            post("a1", "a", "d1", &[false; 30]),
            post("a2", "a", "d1", &[false]),
            post("a3", "a", "d2", &[false]),
            post("b1", "b", "d1", &[true]),
        ])
        .unwrap()
        .predict("a", Some("d1"), &key("do", "declarative"))
        .unwrap();
        close(changed.author_probability, p.author_probability);
        close(changed.population_probability, p.population_probability);
    }

    #[test]
    fn population_balances_authors_dates_posts_and_not_event_volume() {
        let model = PreferenceModel::new(vec![
            post("a", "a", "q", &[false]),
            post("b1", "b", "d1", &[true; 100]),
            post("b2", "b", "d1", &[false]),
            post("b3", "b", "d2", &[false]),
            post("c1", "c", "d1", &[true]),
        ])
        .unwrap();
        // b's rate=(mean(1,0)+0)/2=.25; c's rate=1.
        let p = model
            .predict("a", Some("q"), &key("do", "declarative"))
            .unwrap();
        close(p.population_effective_successes, 1.25);
        close(p.population_probability, 1.75 / 3.0);
        close(p.author_probability, p.population_probability);
        assert_eq!(p.population_support.opportunities, 103);
        assert!(p.author_uses_population_only);
    }

    #[test]
    fn context_matching_and_no_support_fallbacks() {
        let mut b = post("b", "b", "d1", &[true]);
        b.observations[0].conditioning_key = key("do", "do_imperative");
        let model = PreferenceModel::new(vec![post("a", "a", "d1", &[false]), b]).unwrap();
        let p = model
            .predict("a", Some("d1"), &key("do", "declarative"))
            .unwrap();
        close(p.population_probability, 0.5);
        close(p.author_probability, 0.5);
        assert!(p.population_uses_prior_only && p.author_uses_population_only);
        let imperative = model
            .predict("a", None, &key("do", "do_imperative"))
            .unwrap();
        close(imperative.population_probability, 0.75);
        let unseen = model
            .predict("new_author", None, &key("does", "declarative"))
            .unwrap();
        close(unseen.author_probability, 0.5);
        assert_eq!(unseen.author_log_odds, 0.0);
    }

    #[test]
    fn null_rows_and_metric_denominators_remain_explicit() {
        let model = PreferenceModel::new(vec![
            post("empty", "empty", "d1", &[]),
            post("a0", "a", "d0", &[]),
            post("a1", "a", "d1", &[true, false]),
            post("a2", "a", "d2", &[true]),
        ])
        .unwrap();
        let report = model.evaluate().unwrap();
        assert_eq!(report["coverage"]["authors"], 2);
        assert_eq!(report["coverage"]["scorable_authors"], 1);
        assert_eq!(report["coverage"]["posts"], 4);
        assert_eq!(report["coverage"]["scorable_posts"], 2);
        assert_eq!(report["coverage"]["dates"], 4);
        assert_eq!(report["coverage"]["scorable_dates"], 2);
        let empty = report["author_rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["author"] == "empty")
            .unwrap();
        assert!(empty["losses"].is_null());
        assert_eq!(report["paired_author_bootstrap"]["scorable_authors"], 1);
        let all_empty = PreferenceModel::new(vec![post("empty", "empty", "d1", &[])])
            .unwrap()
            .evaluate()
            .unwrap();
        assert!(all_empty["pooled"]["macro_author_losses"].is_null());
        assert!(all_empty["paired_author_bootstrap"]["interval_95"].is_null());
        assert_eq!(
            all_empty["paired_author_bootstrap"]["completed_replicates"],
            0
        );
    }

    #[test]
    fn brier_log_loss_direction_and_equal_date_scoring() {
        close(bernoulli_loss(0.8, true).unwrap().brier, 0.04);
        close(bernoulli_loss(0.8, false).unwrap().log_loss, -0.2_f64.ln());
        assert!(bernoulli_loss(f64::NAN, true).is_err());
        assert!(bernoulli_loss(1.0, true).is_err());
        let report = PreferenceModel::new(vec![
            post("a1", "a", "d1", &[true; 50]),
            post("a2", "a", "d1", &[false]),
            post("a3", "a", "d2", &[false]),
            post("b1", "b", "d1", &[true]),
        ])
        .unwrap()
        .evaluate()
        .unwrap();
        let a = &report["author_rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["author"] == "a")
            .unwrap();
        // a has baseline .75 throughout, so date1 Brier=.3125, date2=.5625.
        close(
            a["losses"]["population_context"]["brier"].as_f64().unwrap(),
            0.4375,
        );
        let pop = a["losses"]["population_context"]["brier"].as_f64().unwrap();
        let own = a["losses"]["author_context"]["brier"].as_f64().unwrap();
        close(
            a["losses"]["improvement"]["brier"].as_f64().unwrap(),
            pop - own,
        );
    }

    #[test]
    fn bootstrap_stratifies_scorable_authors_and_preserves_actual_weights() {
        let row = |author: &str, panel: &str, value: f64| AuthorResult {
            author: author.into(),
            panel: panel.into(),
            coverage: Coverage::default(),
            losses: Some(LossPair {
                improvement: Loss {
                    brier: value,
                    log_loss: 2.0 * value,
                },
                ..LossPair::default()
            }),
        };
        let mut absent = row("empty", "zero", 0.0);
        absent.losses = None;
        let rows = vec![
            row("a", "a", 1.0),
            row("b", "b", 2.0),
            row("c", "b", 2.0),
            absent,
        ];
        let result = bootstrap(&rows);
        close(
            result["mean_improvement"]["brier"].as_f64().unwrap(),
            5.0 / 3.0,
        );
        close(
            result["interval_95"]["brier"][0].as_f64().unwrap(),
            5.0 / 3.0,
        );
        close(
            result["interval_95"]["brier"][1].as_f64().unwrap(),
            5.0 / 3.0,
        );
        assert_eq!(result["scorable_authors_per_panel"]["zero"], 0);
        assert_eq!(result, bootstrap(&rows));
    }

    #[test]
    fn calibration_uses_metric_weights_and_empty_bins_are_null() {
        let report = PreferenceModel::new(vec![
            post("a1", "a", "d1", &[true; 50]),
            post("a2", "a", "d1", &[false]),
            post("a3", "a", "d2", &[false]),
            post("b1", "b", "d1", &[true]),
            post("empty", "empty", "d1", &[]),
        ])
        .unwrap()
        .evaluate()
        .unwrap();
        for model in ["population_context", "author_context"] {
            let calibration = &report["calibration"]["models"][model];
            close(calibration["total_metric_weight"].as_f64().unwrap(), 1.0);
            // a contributes .25 after post/date averaging; b contributes 1.
            close(
                calibration["overall_observed_contraction_rate"]
                    .as_f64()
                    .unwrap(),
                0.625,
            );
            assert_eq!(calibration["raw_opportunities"], 53);
            assert_eq!(calibration["bins"].as_array().unwrap().len(), 10);
            for bin in calibration["bins"].as_array().unwrap() {
                if bin["raw_opportunities"] == 0 {
                    assert_eq!(bin["metric_weight_mass"], 0.0);
                    assert!(bin["mean_prediction"].is_null());
                    assert!(bin["observed_contraction_rate"].is_null());
                }
            }
        }
        close(
            report["calibration"]["models"]["population_context"]["overall_mean_prediction"]
                .as_f64()
                .unwrap(),
            0.5625,
        );
        let empty = PreferenceModel::new(vec![post("empty", "empty", "d1", &[])])
            .unwrap()
            .evaluate()
            .unwrap();
        assert!(
            empty["calibration"]["models"]["population_context"]["overall_mean_prediction"]
                .is_null()
        );
    }

    #[test]
    fn malformed_identity_and_key_inputs_fail_without_dropping_rows() {
        let p = post("same", "a", "d1", &[true]);
        assert!(PreferenceModel::new(vec![p.clone(), p]).is_err());
        let mut other = post("other", "a", "d2", &[false]);
        other.panel = "two".into();
        assert!(PreferenceModel::new(vec![post("first", "a", "d1", &[]), other]).is_err());
        for bad in [
            "not json",
            "[\"do\",\"unknown\"]",
            "[\"do\", \"declarative\"]",
            "[\"\",\"declarative\"]",
        ] {
            assert!(validate_conditioning_key(bad).is_err());
        }
        assert!(PreferenceModel::new(Vec::new()).is_err());
    }
}
