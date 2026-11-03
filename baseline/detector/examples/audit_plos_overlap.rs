//! Screen prepared introductions against bound corpora without assigning splits.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::features::words;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const N: usize = 5;
type GramIndex = HashMap<[u32; N], Vec<(usize, usize)>>;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    plan: PathBuf,
    #[arg(long)]
    output: PathBuf,
}

fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("Missing {key}"))
}

fn bound(base: &Path, v: &Value) -> Result<Vec<u8>> {
    let path = base.join(string(v, "path")?);
    let bytes = fs::read(&path).with_context(|| format!("Read {}", path.display()))?;
    ensure!(
        sha256(&bytes) == string(v, "sha256")?,
        "Changed binding: {}",
        path.display()
    );
    Ok(bytes)
}

#[derive(Default)]
struct Match {
    covered: BTreeSet<usize>,
    longest: usize,
    candidate_start: usize,
    corpus_start: usize,
}

// Start only at a maximal run's first matching gram, including repeated words.
fn matches(candidates: &[Vec<u32>], index: &GramIndex, text: &[u32]) -> BTreeMap<usize, Match> {
    let mut found = BTreeMap::<usize, Match>::new();
    for (start, window) in text.windows(N).enumerate() {
        let key: [u32; N] = window.try_into().unwrap();
        if let Some(hits) = index.get(&key) {
            for &(candidate, position) in hits {
                let target = &candidates[candidate];
                if position > 0 && start > 0 && target[position - 1] == text[start - 1] {
                    continue;
                }
                let mut length = N;
                while position + length < target.len()
                    && start + length < text.len()
                    && target[position + length] == text[start + length]
                {
                    length += 1;
                }
                let item = found.entry(candidate).or_default();
                item.covered.extend(position..position + length);
                if length > item.longest {
                    item.longest = length;
                    item.candidate_start = position;
                    item.corpus_start = start;
                }
            }
        }
    }
    found
}

struct CorpusText {
    text: String,
    members: Vec<Value>,
}

fn insert(corpus: &mut BTreeMap<String, CorpusText>, text: &str, member: Value) {
    corpus
        .entry(sha256(text.as_bytes()))
        .or_insert_with(|| CorpusText {
            text: text.to_owned(),
            members: Vec::new(),
        })
        .members
        .push(member);
}

fn main() -> Result<()> {
    let a = Args::parse();
    ensure!(!a.output.exists(), "Use a new report path");
    let base = a.plan.parent().context("Plan has no parent")?;
    let plan_bytes = fs::read(&a.plan)?;
    let plan: Value = serde_json::from_slice(&plan_bytes)?;
    ensure!(
        plan["schema"] == "slop_ninja_plos_overlap_plan_v1",
        "Wrong plan schema"
    );
    ensure!(
        plan["screen"]
            == json!({"minimum_shared_normalized_words":N,
            "review_contiguous_words":20,"review_candidate_word_coverage":0.2}),
        "Plan screen differs from implementation"
    );
    let preparation: Value = serde_json::from_slice(&bound(base, &plan["preparation"])?)?;
    let ledger: Value = serde_json::from_slice(&bound(base, &plan["ledger"])?)?;
    let manifest_bytes = bound(base, &plan["historical_manifest"])?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
    // The original ledger hashes compact serde JSON; bound() checks file bytes.
    ensure!(
        ledger["manifest_sha256"] == sha256(serde_json::to_vec(&manifest)?),
        "Ledger manifest mismatch"
    );
    let ledger_rows = ledger["rows"].as_array().context("Missing ledger rows")?;
    let mut occurrences = BTreeMap::<(usize, usize), Vec<usize>>::new();
    for (row, item) in ledger_rows.iter().enumerate() {
        for location in item["occurrences"]
            .as_array()
            .context("Missing ledger occurrences")?
        {
            let input = location["input"].as_u64().context("Missing ledger input")? as usize;
            let line = location["line"].as_u64().context("Missing ledger line")? as usize;
            occurrences.entry((input, line)).or_default().push(row);
        }
    }
    let mut recovered = BTreeSet::new();
    let mut corpus = BTreeMap::new();
    let mut input_counts = Vec::new();
    let mut historical_inputs = BTreeSet::new();
    for input in plan["inputs"].as_array().context("Missing corpus inputs")? {
        let bytes = bound(base, input)?;
        let historical = input["historical_manifest_index"]
            .as_u64()
            .map(|v| v as usize);
        if let Some(index) = historical {
            ensure!(
                historical_inputs.insert(index),
                "Duplicate historical input"
            );
            ensure!(
                manifest["inputs"][index]["sha256"] == input["sha256"],
                "Historical input hash mismatch"
            );
        }
        let mut count = 0;
        for (line, text) in std::str::from_utf8(&bytes)?.lines().enumerate() {
            let row: Value = serde_json::from_str(text)
                .with_context(|| format!("{} line {}", input["path"], line + 1))?;
            let prose = string(&row, "text")?;
            let text_hash = sha256(prose.as_bytes());
            ensure!(row["text_sha256"] == text_hash, "Record text hash mismatch");
            let id = string(&row, input["id_field"].as_str().unwrap_or("id"))?;
            let group = string(&row, "source_group")?;
            let mut canonical = BTreeSet::from([group.to_owned()]);
            if let Some(index) = historical {
                let found = occurrences
                    .get(&(index, line + 1))
                    .context("Historical row absent from ledger")?;
                let lexical_hash = sha256(words(prose).join(" ").as_bytes());
                for &number in found {
                    let expected = &ledger_rows[number];
                    ensure!(
                        expected["id"] == id
                            && expected["source_group"] == group
                            && expected["exact_text_sha256"] == text_hash
                            && expected["lexical_text_sha256"] == lexical_hash,
                        "Historical row differs from ledger at input {index}, line {}",
                        line + 1
                    );
                    canonical.insert(string(expected, "canonical_work")?.to_owned());
                    recovered.insert(number);
                }
            }
            insert(
                &mut corpus,
                prose,
                json!({"input":input["name"],"line":line+1,
                "id":id,"groups":canonical,"split":row["split"],"origin":row["origin"],"kind":"existing_corpus"}),
            );
            count += 1;
        }
        ensure!(
            Some(count) == input["expected_rows"].as_u64(),
            "Corpus row count mismatch"
        );
        if let Some(index) = historical {
            ensure!(
                Some(count) == manifest["inputs"][index]["rows"].as_u64(),
                "Historical row count mismatch"
            );
        }
        input_counts.push(json!({"name":input["name"],"rows":count,"sha256":input["sha256"]}));
    }
    ensure!(
        recovered.len() == ledger_rows.len(),
        "Historical coverage incomplete: {} of {}",
        recovered.len(),
        ledger_rows.len()
    );
    let rows = preparation["rows"]
        .as_array()
        .context("Missing candidate rows")?;
    let prepared: Vec<_> = rows.iter().filter(|r| r["prepared"].is_object()).collect();
    ensure!(
        Some(prepared.len() as u64) == plan["expected_prepared"].as_u64(),
        "Candidate count mismatch"
    );
    let mut word_ids = HashMap::<String, u32>::new();
    let mut candidates = Vec::new();
    for row in &prepared {
        let projection = &row["prepared"]["projection"];
        let text = string(projection, "text")?;
        ensure!(
            projection["text_sha256"] == sha256(text.as_bytes()),
            "Candidate hash mismatch"
        );
        let token_ids = words(text)
            .into_iter()
            .map(|word| {
                let next = word_ids.len() as u32 + 1;
                *word_ids.entry(word).or_insert(next)
            })
            .collect::<Vec<_>>();
        ensure!(token_ids.len() >= N, "Candidate too short for this screen");
        candidates.push(token_ids);
        insert(
            &mut corpus,
            text,
            json!({"kind":"prepared_candidate","groups":[row["source_group"]],"id":row["source_group"]}),
        );
    }
    let mut index = GramIndex::new();
    for (candidate, tokens) in candidates.iter().enumerate() {
        for (start, gram) in tokens.windows(N).enumerate() {
            index
                .entry(gram.try_into().unwrap())
                .or_default()
                .push((candidate, start));
        }
    }
    let existing_unique = corpus
        .values()
        .filter(|c| c.members.iter().any(|m| m["kind"] == "existing_corpus"))
        .count();
    let mut results = vec![Vec::<Value>::new(); prepared.len()];
    let mut all_groups = BTreeSet::new();
    for (hash, item) in &corpus {
        for member in &item.members {
            for group in member["groups"].as_array().context("Missing groups")? {
                all_groups.insert(group.as_str().context("Invalid group")?.to_owned());
            }
        }
        let lexical = words(&item.text);
        let tokens = lexical
            .iter()
            .map(|w| word_ids.get(w).copied().unwrap_or(0))
            .collect::<Vec<_>>();
        let normalized = sha256(lexical.join(" ").as_bytes());
        for (candidate, hit) in matches(&candidates, &index, &tokens) {
            let row = prepared[candidate];
            let group = &row["source_group"];
            let members: Vec<_> = item
                .members
                .iter()
                .filter(|m| !(m["kind"] == "prepared_candidate" && m["id"] == *group))
                .collect();
            if members.is_empty() {
                continue;
            }
            let target = string(&row["prepared"]["projection"], "text")?;
            let same_lexical = normalized == sha256(words(target).join(" ").as_bytes());
            let coverage = hit.covered.len() as f64 / candidates[candidate].len() as f64;
            let requires_review = hit.longest >= 20 || coverage >= 0.2 || same_lexical;
            let snippet = if requires_review {
                Some(lexical[hit.corpus_start..hit.corpus_start + hit.longest].join(" "))
            } else {
                None
            };
            results[candidate].push(json!({"corpus_text_sha256":hash,"members":members,
                "exact_duplicate":sha256(target.as_bytes()) == *hash,"lexical_duplicate":same_lexical,
                "shared_candidate_words":hit.covered.len(),"candidate_word_coverage":coverage,
                "longest_contiguous_shared_words":hit.longest,
                "candidate_lexical_range":[hit.candidate_start,hit.candidate_start+hit.longest],
                "corpus_lexical_range":[hit.corpus_start,hit.corpus_start+hit.longest],
                "shared_lexical_passage":snippet,"requires_manual_review":requires_review}));
        }
    }
    let mut report_rows = Vec::new();
    for (i, row) in prepared.iter().enumerate() {
        let mut hits = std::mem::take(&mut results[i]);
        hits.sort_by_key(|h| {
            std::cmp::Reverse((
                h["longest_contiguous_shared_words"].as_u64().unwrap(),
                h["shared_candidate_words"].as_u64().unwrap(),
            ))
        });
        let flags: Vec<_> = hits
            .iter()
            .filter(|h| h["requires_manual_review"] == true)
            .collect();
        report_rows.push(json!({"doi":row["doi"],"source_group":row["source_group"],
            "text_sha256":row["prepared"]["projection"]["text_sha256"],
            "lexical_words":candidates[i].len(),"matching_texts_at_least_five_words":hits.len(),
            "existing_group_match":corpus.values().any(|c|c.members.iter().any(|m|m["kind"] == "existing_corpus" && m["groups"].as_array().unwrap().contains(&row["source_group"]))),
            "flagged_matches":flags,"top_five_matches":hits.iter().take(5).collect::<Vec<_>>(),
            "declared_related_items":row["prepared"]["related_items"],
            "maximum_shared_run":hits.first().map(|h| &h["longest_contiguous_shared_words"]),
            "maximum_candidate_word_coverage":hits.iter().map(|h|h["candidate_word_coverage"].as_f64().unwrap()).reduce(f64::max)}));
    }
    let report = json!({"schema":"slop_ninja_plos_overlap_report_v1","plan_sha256":sha256(plan_bytes),
        "recovered_historical_rows":recovered.len(),"historical_inputs_used":historical_inputs.len(),
        "corpus_inputs":input_counts,"existing_unique_texts":existing_unique,
        "observed_groups":all_groups,"candidate_rows":report_rows,"unsupported_candidates":rows.len()-prepared.len(),
        "method":{"tokens":"grammar_core::features::words; NFC lowercase letter words, digits and punctuation excluded",
            "minimum_shared_run":N,"review_minimum_contiguous_words":20,"review_minimum_candidate_word_coverage":0.2,
            "same_candidate_self_comparisons_excluded":true,"candidate_pair_comparisons_included":true},
        "limitations":["A lexical screen cannot establish independence or detect all paraphrased overlap.",
            "No matching run means none of five or more normalized words; shorter overlap is not measured.",
            "Related-work metadata and flagged passages require separate inspection before admission.",
            "Only declared bound inputs are covered. No split assignment, training admission or detector call occurs."],
        "admitted_records":0,"model_calls":0});
    let mut output = fs::File::create_new(&a.output)?;
    serde_json::to_writer_pretty(&mut output, &report)?;
    output.write_all(b"\n")?;
    println!(
        "{}",
        json!({"historical_rows_recovered":recovered.len(),"existing_unique_texts":existing_unique,
        "candidates":prepared.len(),"candidates_with_flags":report_rows.iter().filter(|r|!r["flagged_matches"].as_array().unwrap().is_empty()).count()})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_runs_match_exhaustive_search_with_repeated_words_and_disjoint_passages() {
        let cases = [
            vec![1, 1, 1, 1, 1, 1, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            vec![7, 8, 9, 1, 2, 3, 4, 5, 1, 2, 3, 4, 5, 6],
        ];
        for target in &cases {
            let mut index = GramIndex::new();
            for (i, gram) in target.windows(N).enumerate() {
                index
                    .entry(gram.try_into().unwrap())
                    .or_default()
                    .push((0, i));
            }
            for text in &cases {
                let mut longest = 0;
                let mut covered = BTreeSet::new();
                for i in 0..target.len() {
                    for j in 0..text.len() {
                        let length = target[i..]
                            .iter()
                            .zip(&text[j..])
                            .take_while(|(a, b)| a == b)
                            .count();
                        if length >= N {
                            longest = longest.max(length);
                            covered.extend(i..i + length);
                        }
                    }
                }
                let result = matches(std::slice::from_ref(target), &index, text);
                let observed = result.get(&0).unwrap();
                assert_eq!(observed.longest, longest);
                assert_eq!(observed.covered, covered);
            }
            assert!(matches(std::slice::from_ref(target), &index, &[0, 0, 0, 0, 0, 0]).is_empty());
        }
    }
}
