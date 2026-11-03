//! Metadata and support audit for fixed-parser dative opportunities.
//! No probabilities are predicted, losses evaluated, or models fitted here.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "slopninja-dative-corpus-support-v1";
const PERSONAL_DATES: usize = 4;
const OTHER_AUTHORS: usize = 10;
const QUALIFYING_AUTHORS: usize = 30;
const QUALIFYING_BLOCKS: usize = 100;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Choice {
    pub event_id: String,
    pub lemma: String,
    pub double_object: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SupportPost {
    pub id: String,
    pub author: String,
    pub date: String,
    pub source_group: String,
    pub panel: String,
    /// Whether the source annotation is available, independent of counterpart checks.
    pub annotation_available: bool,
    /// False may retain confirmed choices while other candidates remain unresolved.
    pub assessment_complete: bool,
    pub choices: Vec<Choice>,
}

pub fn policy() -> Value {
    json!({
        "schema":"slopninja-dative-preference-policy-v1",
        "stage":"support_audit_only",
        "primary_parser":"isolated_trf",
        "sensitivity_parser":"incumbent_sm",
        "parser_policy":"One parser per report; never pool repeated parser observations or substitute sensitivity support for primary support.",
        "expected_input":{"authors":300,"posts":3889,"completeness_checked_by_runner":true},
        "primary_preference_eligibility":"One unique observed form per anchored predicate, at least one exact source proposal and at least one structurally preserved counterpart; retained resource alternatives do not multiply events. Caller verifies this for its reciprocally_checked tier.",
        "input_tier_policy":"report() can summarize raw-frame and source-proposal tiers for coverage, but only the caller-bound reciprocally_checked tier supplies the primary preference gate. Support counts alone never verify eligibility or authorize evaluation.",
        "assessment_availability":"Source annotation availability and complete eligibility assessment are separate. Candidate parse/comparison errors or unresolved comparisons can leave assessment incomplete while retaining other confirmed choices. Zero choices under incomplete assessment never mean observed zero opportunities.",
        "zero_count_availability":"available_zero_event_posts and fully_available_zero_event_dates/authors require complete assessment throughout, not merely available source annotations.",
        "response":"double_object=true is 1; prepositional is 0; missing observations are neither form",
        "context":"Normalized predicate lemma within the fixed give-13.1 frame pair and fixed eligibility scope; no later context grid selection.",
        "query_exclusion":"Entire target-author writing date, including every post in each source group on that date. Groups must have one author/date/panel owner.",
        "support_units":"Distinct eligible writing dates, not event volume or posts; population support excludes the complete target author.",
        "feasibility_gate":{"other_personal_lemma_dates_min":PERSONAL_DATES,
            "other_authors_with_lemma_min":OTHER_AUTHORS,"qualifying_authors_min":QUALIFYING_AUTHORS,
            "qualifying_author_date_blocks_min":QUALIFYING_BLOCKS,
            "block_rule":"A block qualifies if at least one observed query lemma meets both support thresholds.",
            "note":"With this leave-date-out rule, a qualifying author necessarily has at least five qualifying dates, so the 100-block threshold is redundant after 30 qualifying authors. Both prescribed thresholds are retained.",
            "failure_action":"Report insufficient support; do not fit/evaluate a preference model or relax the gate.",
            "success_action":"Model implementation and evaluation remain a separate authorized step; this audit performs neither."},
        "future_model_spec":{"estimators":["population_pooled","population_lemma","author_lemma"],
            "population_prior_success":0.5,"population_prior_failure":0.5,
            "population_units":"One effective observation per supported other author, after equal event/post/date averaging.",
            "population_formula":"(sum other-author rates + 0.5)/(supported other authors + 1)",
            "author_prior_effective_dates":4.0,
            "author_formula":"(sum retained author/lemma date rates + 4*population_lemma)/(retained dates + 4)",
            "zero_personal_support":"Exactly population_lemma; explicitly marked fallback.",
            "zero_population_support":"Exactly 0.5; explicitly marked prior-only.",
            "primary_comparison":"population_lemma Brier loss minus author_lemma Brier loss",
            "secondary_comparisons":["Same contrast in log loss","population_pooled loss minus population_lemma loss"],
            "evaluation_weighting":"Equal events within scorable posts, posts within dates, dates within authors, and authors; include all eligible events and fallback predictions on paired support.",
            "calibration":"Ten fixed equal-width probability bins; same evaluation weights; empty-bin rates null; no calibration fitting.",
            "bootstrap":{"status":"specified_for_later_evaluation_only","replicates":2000,
                "seed_hex":"0x6a09e667f3bcc909","rng":"xorshift64 shifts13,7,17; continuous state; historical next modulo n draws",
                "strata":"Fixed panels in lexical order; scorable authors in lexical order within panel.",
                "sampling":"Resample the fixed scorable-author count within each panel; retain saved held-date predictions; pool equal author weights.",
                "percentile_indices_zero_based":[49,1949],
                "scope":"Descriptive conditional-on-scorable interval; no refit, fresh-author confirmation, or parser uncertainty."}},
        "balanced_observed_rate":"Descriptive fractions conditional on recorded confirmed choices: equal events/post, contributing posts/date, contributing dates/author, scorable authors. No-event rows have null rates; assessment incompleteness never creates negative observations.",
        "automatic_edit_license":false,"semantic_equivalence_certified":false,
        "model_fit_calls":0,"preference_evaluation_calls":0
    })
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty() && value.trim() == value
}

fn validate_date(date: &str) -> Result<()> {
    let b = date.as_bytes();
    ensure!(
        b.len() == 10
            && b[4] == b'-'
            && b[7] == b'-'
            && b.iter()
                .enumerate()
                .all(|(i, c)| matches!(i, 4 | 7) || c.is_ascii_digit()),
        "Date must be exactly YYYY-MM-DD"
    );
    let year: u32 = date[..4].parse()?;
    let month: u32 = date[5..7].parse()?;
    let day: u32 = date[8..].parse()?;
    ensure!(year > 0 && (1..=12).contains(&month), "Invalid civil date");
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

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn form_counts(post: &SupportPost, lemma: Option<&str>) -> (usize, usize) {
    let mut total = 0;
    let mut doubles = 0;
    for choice in &post.choices {
        if lemma.is_none_or(|value| choice.lemma == value) {
            total += 1;
            doubles += usize::from(choice.double_object);
        }
    }
    (total, doubles)
}

#[derive(Default)]
struct Block {
    posts: usize,
    available: usize,
    complete: usize,
    events: usize,
    rates: Vec<f64>,
}

fn availability(block: &Block) -> &'static str {
    if block.available == 0 {
        "none_available"
    } else if block.available == block.posts {
        "all_available"
    } else {
        "partly_available"
    }
}

fn availability_counts<'a>(blocks: impl Iterator<Item = &'a Block>) -> Value {
    let mut counts = BTreeMap::from([
        ("all_available", 0usize),
        ("partly_available", 0),
        ("none_available", 0),
    ]);
    for block in blocks {
        *counts.get_mut(availability(block)).unwrap() += 1;
    }
    json!(counts)
}

fn assessment_counts<'a>(blocks: impl Iterator<Item = &'a Block>) -> Value {
    let mut counts = BTreeMap::from([
        ("all_complete", 0usize),
        ("partly_complete", 0),
        ("none_complete", 0),
    ]);
    for block in blocks {
        let status = if block.complete == 0 {
            "none_complete"
        } else if block.complete == block.posts {
            "all_complete"
        } else {
            "partly_complete"
        };
        *counts.get_mut(status).unwrap() += 1;
    }
    json!(counts)
}

fn summarize(posts: &[&SupportPost], lemma: Option<&str>) -> Value {
    let mut authors = BTreeMap::<&str, Block>::new();
    let mut dates = BTreeMap::<(&str, &str), Block>::new();
    let mut groups = BTreeSet::new();
    let mut observed_groups = BTreeSet::new();
    let mut events = 0;
    let mut doubles = 0;
    let mut observed_posts = 0;
    let mut available_posts = 0;
    let mut complete_posts = 0;
    let mut complete_zero_posts = 0;
    let mut incomplete_zero_posts = 0;
    for post in posts {
        let (n, d) = form_counts(post, lemma);
        let author = authors.entry(&post.author).or_default();
        author.posts += 1;
        author.available += usize::from(post.annotation_available);
        author.complete += usize::from(post.assessment_complete);
        author.events += n;
        let date = dates.entry((&post.author, &post.date)).or_default();
        date.posts += 1;
        date.available += usize::from(post.annotation_available);
        date.complete += usize::from(post.assessment_complete);
        date.events += n;
        groups.insert(&post.source_group);
        events += n;
        doubles += d;
        available_posts += usize::from(post.annotation_available);
        complete_posts += usize::from(post.assessment_complete);
        complete_zero_posts += usize::from(post.assessment_complete && n == 0);
        incomplete_zero_posts += usize::from(!post.assessment_complete && n == 0);
        if n > 0 {
            observed_posts += 1;
            observed_groups.insert(&post.source_group);
            date.rates.push(d as f64 / n as f64);
        }
    }
    for ((author, _), date) in &dates {
        if let Some(rate) = mean(&date.rates) {
            authors.get_mut(author).unwrap().rates.push(rate);
        }
    }
    let rates = authors
        .values()
        .filter_map(|a| mean(&a.rates))
        .collect::<Vec<_>>();
    json!({
        "authors":authors.len(),"author_dates":dates.len(),"source_groups":groups.len(),"posts":posts.len(),
        "annotation_status":{"posts":{"available":available_posts,"unavailable":posts.len()-available_posts},
            "author_dates":availability_counts(dates.values()),"authors":availability_counts(authors.values())},
        "assessment_status":{"posts":{"complete":complete_posts,"incomplete":posts.len()-complete_posts},
            "author_dates":assessment_counts(dates.values()),"authors":assessment_counts(authors.values())},
        "eligible_events":events,"double_object":doubles,"prepositional":events-doubles,
        "observed_authors":rates.len(),"observed_author_dates":dates.values().filter(|d|d.events>0).count(),
        "observed_source_groups":observed_groups.len(),"observed_posts":observed_posts,
        "available_zero_event_posts":complete_zero_posts,
        "incomplete_assessment_zero_confirmed_event_posts":incomplete_zero_posts,
        "fully_available_zero_event_dates":dates.values().filter(|d|d.available==d.posts && d.complete==d.posts && d.events==0).count(),
        "fully_available_zero_event_authors":authors.values().filter(|a|a.available==a.posts && a.complete==a.posts && a.events==0).count(),
        "balanced_double_object_fraction":mean(&rates)
    })
}

/// The caller binds the full input catalog and one parser identity. This helper
/// never reads corpus/cache files and cannot invent omitted author metadata.
pub fn report(posts: &[SupportPost]) -> Result<Value> {
    ensure!(!posts.is_empty(), "Full metadata catalog must not be empty");
    let mut ids = BTreeSet::new();
    let mut author_panels = BTreeMap::new();
    let mut group_owners = BTreeMap::new();
    let mut dates = BTreeMap::<(&str, &str), Vec<&SupportPost>>::new();
    let mut contexts = BTreeMap::<&str, BTreeMap<&str, BTreeSet<&str>>>::new();
    for post in posts {
        ensure!(
            [
                &post.id,
                &post.author,
                &post.date,
                &post.source_group,
                &post.panel
            ]
            .iter()
            .all(|s| valid_identity(s)),
            "Invalid metadata identity"
        );
        ensure!(ids.insert(&post.id), "Duplicate post ID");
        validate_date(&post.date)?;
        if let Some(panel) = author_panels.insert(post.author.as_str(), post.panel.as_str()) {
            ensure!(panel == post.panel, "Author occurs in multiple panels");
        }
        let owner = (
            post.author.as_str(),
            post.date.as_str(),
            post.panel.as_str(),
        );
        if let Some(previous) = group_owners.insert(post.source_group.as_str(), owner) {
            ensure!(
                previous == owner,
                "Source group has inconsistent author/date/panel ownership"
            );
        }
        ensure!(
            post.annotation_available || (!post.assessment_complete && post.choices.is_empty()),
            "Unavailable annotation must have incomplete assessment and no choices"
        );
        dates
            .entry((&post.author, &post.date))
            .or_default()
            .push(post);
        let mut events = BTreeSet::new();
        for choice in &post.choices {
            ensure!(
                valid_identity(&choice.event_id) && valid_identity(&choice.lemma),
                "Invalid event ID or lemma"
            );
            ensure!(
                events.insert(&choice.event_id),
                "Repeated event ID within post"
            );
            contexts
                .entry(&choice.lemma)
                .or_default()
                .entry(&post.author)
                .or_default()
                .insert(&post.date);
        }
    }
    // Stable order makes means and all report arrays independent of input order.
    let mut ordered = posts.iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| a.id.cmp(&b.id));
    for block in dates.values_mut() {
        block.sort_by(|a, b| a.id.cmp(&b.id));
    }
    let mut post_rows = Vec::new();
    for post in &ordered {
        let (total, doubles) = form_counts(post, None);
        post_rows.push(json!({"id":post.id,"author":post.author,"date":post.date,"panel":post.panel,
            "source_group":post.source_group,"annotation_available":post.annotation_available,"assessment_complete":post.assessment_complete,
            "opportunity_status":if !post.annotation_available {"annotation_unavailable"}
                else if !post.assessment_complete && total==0 {"incomplete_assessment_no_confirmed_events"}
                else if !post.assessment_complete {"confirmed_events_observed_incomplete_assessment"}
                else if total==0 {"observed_zero"} else {"eligible_events_observed"},
            "eligible_events":total,"double_object":doubles,"prepositional":total-doubles,
            "double_object_fraction":(total>0).then(||doubles as f64/total as f64)}));
    }
    let mut qualifying_authors = BTreeSet::new();
    let mut qualifying_blocks = 0;
    let mut qualifying_events = 0;
    let mut qualifying_by_panel = BTreeMap::<&str, (BTreeSet<&str>, usize)>::new();
    for panel in author_panels.values() {
        qualifying_by_panel.entry(panel).or_default();
    }
    let mut histogram = BTreeMap::<(usize, usize), usize>::new();
    let mut date_rows = Vec::new();
    for ((author, date), block) in &dates {
        let lemmas = block
            .iter()
            .flat_map(|p| p.choices.iter().map(|c| c.lemma.as_str()))
            .collect::<BTreeSet<_>>();
        let mut query_lemmas = Vec::new();
        let mut supported_lemmas = BTreeSet::new();
        for lemma in lemmas {
            let other_dates = contexts[lemma][author].len() - 1;
            let other_authors = contexts[lemma].len() - 1;
            let qualifies = other_dates >= PERSONAL_DATES && other_authors >= OTHER_AUTHORS;
            *histogram.entry((other_dates, other_authors)).or_default() += 1;
            if qualifies {
                supported_lemmas.insert(lemma);
            }
            query_lemmas.push(json!({"lemma":lemma,"other_personal_dates":other_dates,
                "other_authors":other_authors,"qualifies":qualifies}));
        }
        let qualifies = !supported_lemmas.is_empty();
        let supported_events = block
            .iter()
            .flat_map(|p| &p.choices)
            .filter(|c| supported_lemmas.contains(c.lemma.as_str()))
            .count();
        qualifying_events += supported_events;
        if qualifies {
            qualifying_authors.insert(*author);
            qualifying_blocks += 1;
            let entry = qualifying_by_panel.get_mut(author_panels[author]).unwrap();
            entry.0.insert(*author);
            entry.1 += 1;
        }
        date_rows.push(
            json!({"author":author,"date":date,"panel":author_panels[author],
            "coverage":summarize(block,None),"query_lemma_support":query_lemmas,
            "qualifies":qualifies,"qualifying_events":supported_events}),
        );
    }
    let panels = author_panels.values().copied().collect::<BTreeSet<_>>().into_iter().map(|panel| {
        let rows = ordered.iter().copied().filter(|p|p.panel==panel).collect::<Vec<_>>();
        json!({"panel":panel,"coverage":summarize(&rows,None),"qualifying_authors":qualifying_by_panel[panel].0.len(),"qualifying_author_date_blocks":qualifying_by_panel[panel].1})
    }).collect::<Vec<_>>();
    let authors = author_panels.iter().map(|(author,panel)| {
        let rows = ordered.iter().copied().filter(|p|p.author==*author).collect::<Vec<_>>();
        let by_lemma = contexts.keys().filter(|lemma|contexts[*lemma].contains_key(author)).map(|lemma|json!({"lemma":lemma,"coverage":summarize(&rows,Some(lemma))})).collect::<Vec<_>>();
        json!({"author":author,"panel":panel,"coverage":summarize(&rows,None),"per_lemma":by_lemma,
            "qualifying_author_date_blocks":date_rows.iter().filter(|d|d["author"]==*author && d["qualifies"]==true).count()})
    }).collect::<Vec<_>>();
    let lemmas = contexts
        .keys()
        .map(|lemma| json!({"lemma":lemma,"coverage":summarize(&ordered,Some(lemma))}))
        .collect::<Vec<_>>();
    let passed =
        qualifying_authors.len() >= QUALIFYING_AUTHORS && qualifying_blocks >= QUALIFYING_BLOCKS;
    Ok(
        json!({"schema":SCHEMA,"policy":policy(),"coverage":summarize(&ordered,None),
        "panels":panels,"authors":authors,"lemmas":lemmas,"post_rows":post_rows,"date_rows":date_rows,
        "query_lemma_date_support_histogram":histogram.into_iter().map(|((personal,others),count)|json!({"other_personal_dates":personal,"other_authors":others,"query_lemma_date_blocks":count})).collect::<Vec<_>>(),
        "feasibility":{"passed":passed,"support_only":true,"conditional_on_recorded_confirmed_choices":true,"preference_eligibility_verified_here":false,"qualifying_authors":qualifying_authors.len(),
            "qualifying_author_date_blocks":qualifying_blocks,"qualifying_events":qualifying_events,
            "status":if passed {"support_gate_met_no_model_evaluated"} else {"insufficient_support_no_model_evaluated"}},
        "model_fit_calls":0,"preference_evaluation_calls":0,"automatic_edit_license":false,"semantic_equivalence_certified":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(id: &str, author: &str, date: &str, values: &[bool]) -> SupportPost {
        SupportPost {
            id: id.into(),
            author: author.into(),
            date: date.into(),
            source_group: format!("blog:{author}:date:{date}"),
            panel: "panel".into(),
            annotation_available: true,
            assessment_complete: true,
            choices: values
                .iter()
                .enumerate()
                .map(|(i, y)| Choice {
                    event_id: format!("event-{i}"),
                    lemma: "give".into(),
                    double_object: *y,
                })
                .collect(),
        }
    }

    fn cohort(authors: usize, dates: usize) -> Vec<SupportPost> {
        (0..authors)
            .flat_map(|a| {
                (1..=dates).map(move |d| {
                    post(
                        &format!("post-{a}-{d}"),
                        &format!("author-{a}"),
                        &format!("2004-01-{d:02}"),
                        &[true],
                    )
                })
            })
            .collect()
    }

    #[test]
    fn gate_requires_four_other_dates_ten_other_authors_and_thirty_qualifying_authors() {
        assert_eq!(
            report(&cohort(30, 4)).unwrap()["feasibility"]["qualifying_authors"],
            0
        );
        assert_eq!(
            report(&cohort(10, 5)).unwrap()["feasibility"]["qualifying_authors"],
            0
        );
        let eleven = report(&cohort(11, 5)).unwrap();
        assert_eq!(eleven["feasibility"]["qualifying_authors"], 11);
        assert_eq!(eleven["feasibility"]["passed"], false);
        assert_eq!(
            report(&cohort(29, 5)).unwrap()["feasibility"]["passed"],
            false
        );
        let thirty = report(&cohort(30, 5)).unwrap();
        assert_eq!(thirty["feasibility"]["qualifying_author_date_blocks"], 150);
        assert_eq!(thirty["feasibility"]["passed"], true);
    }

    #[test]
    fn repeated_posts_and_events_do_not_create_personal_dates_or_query_blocks() {
        let mut rows = cohort(11, 4);
        for a in 0..11 {
            rows.push(post(
                &format!("extra-{a}"),
                &format!("author-{a}"),
                "2004-01-01",
                &[true; 100],
            ));
        }
        let value = report(&rows).unwrap();
        assert_eq!(value["coverage"]["author_dates"], 44);
        assert_eq!(value["feasibility"]["qualifying_author_date_blocks"], 0);
        assert_eq!(
            value["query_lemma_date_support_histogram"][0]["other_personal_dates"],
            3
        );
        assert_eq!(
            value["query_lemma_date_support_histogram"][0]["query_lemma_date_blocks"],
            44
        );
    }

    #[test]
    fn counts_balance_posts_dates_and_authors_without_inventing_zero_events() {
        let rows = vec![
            post("a1", "a", "2004-01-01", &[true; 100]),
            post("a2", "a", "2004-01-01", &[false]),
            post("a3", "a", "2004-01-02", &[false]),
            post("b1", "b", "2004-01-01", &[true]),
            post("c1", "c", "2004-01-01", &[]),
        ];
        let value = report(&rows).unwrap();
        // a: mean(mean(1,0),0)=.25; b:1; c:null. Macro mean=.625.
        assert_eq!(value["coverage"]["balanced_double_object_fraction"], 0.625);
        assert_eq!(value["coverage"]["authors"], 3);
        assert_eq!(value["coverage"]["observed_authors"], 2);
        assert!(value["authors"][2]["coverage"]["balanced_double_object_fraction"].is_null());
        let mut reversed = rows;
        reversed.reverse();
        assert_eq!(report(&reversed).unwrap(), value);
    }

    #[test]
    fn unavailable_annotations_are_separate_from_observed_zero_and_partial_dates() {
        let mut absent = post("a1", "a", "2004-01-01", &[]);
        absent.annotation_available = false;
        absent.assessment_complete = false;
        let mut absent_b = post("b1", "b", "2004-01-01", &[]);
        absent_b.annotation_available = false;
        absent_b.assessment_complete = false;
        let rows = vec![
            absent,
            post("a2", "a", "2004-01-01", &[]),
            absent_b,
            post("c1", "c", "2004-01-01", &[]),
        ];
        let value = report(&rows).unwrap();
        assert_eq!(
            value["coverage"]["annotation_status"]["author_dates"]["partly_available"],
            1
        );
        assert_eq!(
            value["coverage"]["annotation_status"]["authors"]["none_available"],
            1
        );
        assert_eq!(value["coverage"]["fully_available_zero_event_dates"], 1);
        assert_eq!(value["coverage"]["available_zero_event_posts"], 2);
        assert!(value["coverage"]["balanced_double_object_fraction"].is_null());
        assert_eq!(
            value["post_rows"][0]["opportunity_status"],
            "annotation_unavailable"
        );
    }

    #[test]
    fn support_is_lemma_matched_and_counts_all_forms_without_a_label_gate() {
        let mut rows = cohort(30, 5);
        for p in &mut rows {
            p.choices[0].double_object = false;
        }
        assert_eq!(report(&rows).unwrap()["feasibility"]["passed"], true);
        for p in &mut rows {
            if p.date == "2004-01-05" {
                p.choices[0].lemma = "lend".into();
            }
        }
        let value = report(&rows).unwrap();
        assert_eq!(value["feasibility"]["passed"], false);
        assert_eq!(value["coverage"]["double_object"], 0);
        assert_eq!(value["coverage"]["prepositional"], 150);
        assert_eq!(value["lemmas"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn missing_counterpart_assessment_is_not_observed_zero_and_keeps_confirmed_choices() {
        let mut unresolved = post("a1", "a", "2004-01-01", &[]);
        unresolved.assessment_complete = false;
        let mut mixed = post("b1", "b", "2004-01-01", &[true]);
        mixed.assessment_complete = false;
        let rows = vec![unresolved, post("a2", "a", "2004-01-01", &[]), mixed];
        let value = report(&rows).unwrap();
        assert_eq!(
            value["post_rows"][0]["opportunity_status"],
            "incomplete_assessment_no_confirmed_events"
        );
        assert_eq!(
            value["post_rows"][2]["opportunity_status"],
            "confirmed_events_observed_incomplete_assessment"
        );
        assert_eq!(
            value["coverage"]["annotation_status"]["authors"]["all_available"],
            2
        );
        assert_eq!(
            value["coverage"]["assessment_status"]["author_dates"]["partly_complete"],
            1
        );
        assert_eq!(value["coverage"]["fully_available_zero_event_dates"], 0);
        assert_eq!(value["coverage"]["fully_available_zero_event_authors"], 0);
        assert_eq!(value["coverage"]["available_zero_event_posts"], 1);
        assert_eq!(
            value["coverage"]["incomplete_assessment_zero_confirmed_event_posts"],
            1
        );
        assert_eq!(value["coverage"]["eligible_events"], 1);
        assert_eq!(value["coverage"]["balanced_double_object_fraction"], 1.0);
        let mut invalid = post("c1", "c", "2004-01-01", &[]);
        invalid.annotation_available = false;
        assert!(report(&[invalid]).is_err());
    }

    #[test]
    fn malformed_metadata_repeated_events_and_false_available_labels_are_rejected() {
        let base = post("p", "a", "2004-02-29", &[true]);
        assert!(report(&[base.clone(), base.clone()]).is_err());
        for date in [
            "2003-02-29",
            "1900-02-29",
            "2004-13-01",
            "2004-04-31",
            "0000-01-01",
            "2004-1-01",
        ] {
            let mut p = base.clone();
            p.date = date.into();
            assert!(report(&[p]).is_err());
        }
        let mut duplicate = base.clone();
        duplicate.choices.push(duplicate.choices[0].clone());
        assert!(report(&[duplicate]).is_err());
        let mut unavailable = base.clone();
        unavailable.annotation_available = false;
        assert!(report(&[unavailable]).is_err());
        let mut other = base.clone();
        other.id = "q".into();
        other.date = "2004-03-01".into();
        assert!(report(&[base.clone(), other]).is_err());
        let mut other = base.clone();
        other.id = "q".into();
        other.panel = "elsewhere".into();
        assert!(report(&[base.clone(), other]).is_err());
        assert!(report(&[base]).is_ok());
    }
}
