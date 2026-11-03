//! Sense-grouped lexical nominations and reference word counts. No semantic approval.
use super::{hash, menu, read, save, text};
use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use grammar_core::{
    features,
    syntax::{Document, Token},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

#[derive(Subcommand)]
pub enum Action {
    /// Nominate word alternatives using pinned WordNet senses and reference counts.
    Prepare {
        #[arg(long)]
        plan: PathBuf,
        /// Extracted WordNet-3.0 directory containing LICENSE and dict/data.*.
        #[arg(long)]
        dictionary: PathBuf,
        #[arg(long)]
        python: String,
        #[arg(long, default_value_t = 256)]
        max_candidates: usize,
        /// Restrict nominations to these exact token byte positions; repeat as needed.
        #[arg(long)]
        at_byte: Vec<usize>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Import chosen nomination IDs into an unreviewed menu with the original bindings.
    Import {
        #[arg(long)]
        pool: PathBuf,
        #[arg(long, required = true)]
        choice_id: Vec<String>,
        #[arg(long, default_value_t = 3)]
        max_depth: usize,
        #[arg(long, default_value_t = 512)]
        max_states: usize,
        #[arg(long)]
        out: PathBuf,
    },
}

const SCHEMA: &str = "slop_ninja_lexical_pool_v1";
const FILES: &[(&str, &str)] = &[
    (
        "LICENSE",
        "7731175a77952e259390b496fab905e57118b8d19ad3a8383c67eee724ff443f",
    ),
    (
        "dict/data.adj",
        "f24b635368be441501c9b8001e9271fd3b30b203f00d91e332979e6f8fe35646",
    ),
    (
        "dict/data.adv",
        "e66dbbda0e0359e41b7f225bff71dd0c263dc7c66c1b61abc9ba334973d92979",
    ),
    (
        "dict/data.noun",
        "489f145e0f68877c0be5bd0eb4117adaaac52f38f6204eb8d85dbe2158b614cc",
    ),
    (
        "dict/data.verb",
        "7fff397e93b4fb152fdaee116747cf1c40d7badedccb4b63f5503e8e962d9102",
    ),
];

#[derive(Clone, Serialize)]
struct Lemma {
    word: String,
    position: Option<String>,
}
#[derive(Clone, Serialize)]
struct Sense {
    id: String,
    pos: String,
    words: Vec<Lemma>,
    gloss: String,
}

fn sense(line: &str) -> Result<Sense> {
    let (fields, gloss) = line.split_once('|').context("WordNet synset lacks gloss")?;
    let fields: Vec<_> = fields.split_whitespace().collect();
    ensure!(fields.len() >= 5, "Incomplete WordNet synset");
    let _: u64 = fields[0].parse()?;
    let pos = match fields[2] {
        "a" | "s" => "a",
        "n" => "n",
        "v" => "v",
        "r" => "r",
        _ => anyhow::bail!("Unknown WordNet POS"),
    };
    let count = usize::from_str_radix(fields[3], 16)?;
    ensure!(
        count > 0 && fields.len() > 4 + 2 * count,
        "Invalid WordNet word count"
    );
    let mut words = vec![];
    for i in 0..count {
        let raw = fields[4 + 2 * i];
        let _: u8 = u8::from_str_radix(fields[5 + 2 * i], 16)?;
        let (word, position) = if let Some((word, marker)) = raw.split_once('(') {
            ensure!(
                pos == "a" && ["a)", "p)", "ip)"].contains(&marker),
                "Unknown adjective position"
            );
            (word, Some(marker.trim_end_matches(')').to_string()))
        } else {
            (raw, None)
        };
        // No collocations, punctuation changes, capitalization guesses or morphology.
        if !word.is_empty() && word.bytes().all(|c| c.is_ascii_lowercase()) {
            words.push(Lemma {
                word: word.into(),
                position,
            });
        }
    }
    Ok(Sense {
        id: format!("{}:{}", fields[2], fields[0]),
        pos: pos.into(),
        words,
        gloss: gloss.trim().into(),
    })
}

fn position_allowed(lemma: &Lemma, token: &Token) -> bool {
    match lemma.position.as_deref() {
        None => true,
        Some("a") => token.dep == "amod",
        Some("p") => ["acomp", "attr"].contains(&token.dep.as_str()),
        _ => false, // Immediately postnominal placement is not inferred.
    }
}
fn pos(token: &Token) -> Option<&'static str> {
    match (token.pos.as_str(), token.tag.as_str()) {
        ("NOUN", "NN") => Some("n"),
        ("VERB", "VB" | "VBP") => Some("v"),
        ("ADJ", "JJ") => Some("a"),
        ("ADV", "RB") => Some("r"),
        _ => None,
    }
}
fn context(doc: &Document, token: &Token) -> String {
    format!("{}|{}|{}", token.tag, token.dep, doc.tokens[token.head].pos)
}
struct Counts {
    total: usize,
    words: BTreeMap<String, usize>,
    contexts: BTreeMap<String, BTreeMap<String, usize>>,
}
fn counts(doc: &Document) -> Counts {
    let words = features::words(&doc.text);
    let total = words.len();
    let mut out = Counts {
        total,
        words: BTreeMap::new(),
        contexts: BTreeMap::new(),
    };
    for word in words {
        *out.words.entry(word).or_default() += 1;
    }
    for t in &doc.tokens {
        if pos(t).is_some() && t.text.bytes().all(|c| c.is_ascii_alphabetic()) {
            *out.contexts
                .entry(context(doc, t))
                .or_default()
                .entry(t.text.to_lowercase())
                .or_default() += 1;
        }
    }
    out
}
fn rate(c: &Counts, word: &str) -> f64 {
    *c.words.get(word).unwrap_or(&0) as f64 / c.total as f64
}
fn target(references: &[Counts], word: &str) -> f64 {
    references.iter().map(|r| rate(r, word)).sum::<f64>() / references.len() as f64
}
fn gain(source: &Counts, references: &[Counts], before: &str, after: &str) -> f64 {
    let (a, b) = (rate(source, before), rate(source, after));
    let (qa, qb) = (target(references, before), target(references, after));
    let step = 1.0 / source.total as f64;
    let d =
        |a: f64, b: f64| 0.5 * ((a.sqrt() - qa.sqrt()).powi(2) + (b.sqrt() - qb.sqrt()).powi(2));
    d(a, b) - d((a - step).max(0.0), b + step)
}

fn prepare(
    plan_path: PathBuf,
    dictionary: PathBuf,
    python: String,
    limit: usize,
    positions: Vec<usize>,
    out: PathBuf,
) -> Result<()> {
    ensure!(
        !out.exists() && (1..=512).contains(&limit),
        "Use a fresh output and 1..512 nominations"
    );
    let plan = menu::plan_at(&plan_path)?;
    let input = &plan["input"];
    let source = text(&input["source"], "text")?;
    let references = input["references"]
        .as_array()
        .context("Missing references")?;
    ensure!(references.len() >= 2, "Need independent reference works");
    let mut ids = BTreeSet::new();
    let mut hashes = BTreeSet::new();
    for r in references {
        ensure!(
            ids.insert(text(r, "id")?) && hashes.insert(hash(text(r, "text")?)),
            "Repeated reference work"
        );
    }
    let mut contents = BTreeMap::new();
    for (file, expected) in FILES {
        let bytes = fs::read(dictionary.join(file))?;
        ensure!(
            hash(&bytes) == *expected,
            "WordNet checksum mismatch: {file}"
        );
        contents.insert(*file, String::from_utf8(bytes)?);
    }
    let resource = json!({"name":"Princeton WordNet 3.0","url":"https://wordnetcode.princeton.edu/3.0/WordNet-3.0.tar.gz",
        "archive_sha256":"640db279c949a88f61f851dd54ebbb22d003f8b90b85267042ef85a3781d3a52",
        "files":FILES,"license_text":contents["LICENSE"],"citation":"Princeton University, WordNet 3.0 (2006)."});
    let mut senses = vec![];
    let mut index = BTreeMap::<(String, String), Vec<usize>>::new();
    for (file, data) in &contents {
        if *file == "LICENSE" {
            continue;
        }
        for line in data
            .lines()
            .filter(|line| !line.starts_with(' ') && !line.is_empty())
        {
            let s = sense(line)?;
            if s.words.len() < 2 {
                continue;
            }
            for w in &s.words {
                index
                    .entry((s.pos.clone(), w.word.clone()))
                    .or_default()
                    .push(senses.len());
            }
            senses.push(s);
        }
    }
    let mut texts = vec![source];
    texts.extend(
        references
            .iter()
            .map(|r| text(r, "text"))
            .collect::<Result<Vec<_>>>()?,
    );
    let docs = grammar_spacy::Parser {
        python: &python,
        model: super::parser_model(input)?,
    }
    .parse_batch(&texts)?
    .into_iter()
    .map(|d| d.map_err(anyhow::Error::msg))
    .collect::<Result<Vec<_>>>()?;
    ensure!(
        docs.len() == texts.len()
            && docs
                .iter()
                .all(|d| d.parser_identity == docs[0].parser_identity),
        "Inconsistent parse coverage"
    );
    let measured: Vec<_> = docs.iter().map(counts).collect();
    ensure!(
        docs[0].parser_identity == text(&plan["full_space"], "parser_identity")?,
        "Parser identity differs from the lexical source plan"
    );
    ensure!(
        measured.iter().all(|c| c.total > 0),
        "Empty reference/source word counts"
    );
    let original = &measured[0];
    let target_counts = &measured[1..];
    let selected: BTreeSet<_> = positions.iter().copied().collect();
    ensure!(
        selected.len() == positions.len()
            && selected
                .iter()
                .all(|p| docs[0].tokens.iter().any(|t| t.start_byte == *p)),
        "Every requested position must name a distinct source token byte start"
    );
    let mut buckets = vec![];
    for token in &docs[0].tokens {
        if !selected.is_empty() && !selected.contains(&token.start_byte) {
            continue;
        }
        let Some(category) = pos(token) else {
            continue;
        };
        if token.text != token.lemma || !token.text.bytes().all(|c| c.is_ascii_lowercase()) {
            continue;
        }
        let sentence = &docs[0].sentences[token.sentence];
        let sentence_text = &source[sentence.start_byte..sentence.end_byte];
        if sentence_text.contains(['"', '“', '”', '`', '{', '}', '[', ']', '\\']) {
            continue;
        }
        let mut alternatives = BTreeMap::<String, Vec<Value>>::new();
        for &i in index
            .get(&(category.into(), token.text.clone()))
            .into_iter()
            .flatten()
        {
            let s = &senses[i];
            if !s
                .words
                .iter()
                .any(|w| w.word == token.text && position_allowed(w, token))
            {
                continue;
            }
            for w in &s.words {
                if w.word != token.text && position_allowed(w, token) {
                    alternatives
                        .entry(w.word.clone())
                        .or_default()
                        .push(json!({"synset":s.id,"gloss":s.gloss,"position":w.position}));
                }
            }
        }
        let ctx = context(&docs[0], token);
        let mut rows = vec![];
        for (after, evidence) in alternatives {
            let delta = gain(original, target_counts, &token.text, &after);
            let counts: Vec<_> = references.iter().zip(target_counts).map(|(r,c)| {
                let context_words = c.contexts.get(&ctx);
                json!({"reference_id":r["id"],"total_words":c.total,"before_count":c.words.get(&token.text).copied().unwrap_or(0),
                    "after_count":c.words.get(&after).copied().unwrap_or(0),"context_opportunities":context_words.map_or(0,|m|m.values().sum::<usize>()),
                    "context_before_count":context_words.and_then(|m|m.get(&token.text)).copied().unwrap_or(0),
                    "context_after_count":context_words.and_then(|m|m.get(&after)).copied().unwrap_or(0)})
            }).collect();
            let id = format!("wordnet-{}-{}", token.start_byte, &hash(&after)[..12]);
            rows.push(json!({"id":id,"kind":"lexical_wordnet","rationale":"Same-synset nomination; review the applicable sense, grammar, meaning and register before selection.",
                "edits":[{"before":token.text,"after":after,"start_byte":token.start_byte}],
                "sentence":sentence_text,"context":ctx,"tag":token.tag,"dependency":token.dep,
                "word_hellinger_reduction":delta,"reference_counts":counts,"sense_evidence":evidence,
                "editorial_status":"unreviewed","eligible_output":false}));
        }
        rows.sort_by(|a, b| {
            b["word_hellinger_reduction"]
                .as_f64()
                .unwrap()
                .total_cmp(&a["word_hellinger_reduction"].as_f64().unwrap())
                .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
        });
        if !rows.is_empty() {
            buckets.push(rows);
        }
    }
    let total: usize = buckets.iter().map(Vec::len).sum();
    let mut candidates = vec![];
    for rank in 0..buckets.iter().map(Vec::len).max().unwrap_or(0) {
        for bucket in &buckets {
            if candidates.len() == limit {
                break;
            }
            if let Some(row) = bucket.get(rank) {
                candidates.push(row.clone());
            }
        }
        if candidates.len() == limit {
            break;
        }
    }
    fs::create_dir_all(&out)?;
    save(&out.join("input.json"), input)?;
    fs::write(out.join("LICENSE.wordnet"), contents["LICENSE"].as_bytes())?;
    save(
        &out.join("pool.json"),
        &json!({"schema":SCHEMA,"plan_sha256":hash(fs::read(plan_path)?),"input_sha256":hash(fs::read(out.join("input.json"))?),
        "source_sha256":hash(source),"resource":resource,"parser_identity":docs[0].parser_identity,
        "word_feature_schema":features::FEATURE_VERSION,"source_word_count":original.total,
        "target_policy":"Equal reference-work weight; report individual counts and denominators. Context counts are diagnostics, not a fitted conditional language model.",
        "ranking":"Unigram Hellinger reduction only, round-robin across source positions, sorted by reduction within each position. Full grammar changes require menu prepare and editorial review.",
        "morphology":"Only unchanged lowercase ASCII base forms with NN/VB/VBP/JJ/RB tags. No inflection, collocation or proper-name generation. WordNet adjective placement markers are checked.",
        "limitations":"Same synset does not prove contextual substitutability, predicate compatibility or meaning preservation. No word-sense disambiguation, calibrated author fit or detector scoring is supplied.",
        "requested_token_positions":positions,"positions_with_alternatives":buckets.len(),"total_nominations":total,"retained_nominations":candidates.len(),"limit":limit,"candidates":candidates}),
    )
}

fn import(
    pool_path: PathBuf,
    chosen: Vec<String>,
    depth: usize,
    states: usize,
    out: PathBuf,
) -> Result<()> {
    let pool = read(&pool_path)?;
    ensure!(
        pool["schema"] == SCHEMA && !out.exists(),
        "Wrong pool or existing output"
    );
    let input_path = pool_path
        .parent()
        .context("Pool directory")?
        .join("input.json");
    ensure!(
        hash(fs::read(&input_path)?) == text(&pool, "input_sha256")?,
        "Changed source input"
    );
    let mut input = read(&input_path)?;
    ensure!(
        hash(text(&input["source"], "text")?) == text(&pool, "source_sha256")?,
        "Wrong lexical source"
    );
    ensure!(
        !chosen.is_empty()
            && chosen.len() <= 16
            && chosen.iter().collect::<BTreeSet<_>>().len() == chosen.len(),
        "Choose 1..16 distinct IDs"
    );
    let mut choices = vec![];
    for id in chosen {
        let row = pool["candidates"]
            .as_array()
            .context("Missing nominations")?
            .iter()
            .find(|r| r["id"] == id)
            .context("Unknown nomination ID")?;
        choices.push(json!({"id":id,"kind":row["kind"],"rationale":row["rationale"],"edits":row["edits"],
            "provenance":{"pool_sha256":hash(fs::read(&pool_path)?),"resource":pool["resource"],"nomination":row,"review":"Unreviewed; importing a hypothesis is not editorial acceptance."}}));
    }
    input["choices"] = json!(choices);
    input["include_grammar_rules"] = json!(false);
    input
        .as_object_mut()
        .context("Menu input must be an object")?
        .remove("grammar_rules");
    input["max_depth"] = json!(depth);
    input["max_states"] = json!(states);
    save(&out, &input)
}

pub fn run(action: Action) -> Result<()> {
    match action {
        Action::Prepare {
            plan,
            dictionary,
            python,
            max_candidates,
            at_byte,
            out,
        } => prepare(plan, dictionary, python, max_candidates, at_byte, out),
        Action::Import {
            pool,
            choice_id,
            max_depth,
            max_states,
            out,
        } => import(pool, choice_id, max_depth, max_states, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sense_membership_keeps_position_restrictions_and_excludes_collocations() {
        let s = sense(
            "00000001 00 s 04 tiny(a) 0 small(p) 0 little_bit 0 Small 0 000 | synthetic size sense",
        )
        .unwrap();
        assert_eq!(s.pos, "a");
        assert_eq!(s.words.len(), 2);
        assert_eq!(s.words[0].position.as_deref(), Some("a"));
        assert_eq!(s.words[1].position.as_deref(), Some("p"));
        let mut token = Token {
            i: 0,
            start_byte: 0,
            end_byte: 4,
            text: "tiny".into(),
            lemma: "tiny".into(),
            pos: "ADJ".into(),
            tag: "JJ".into(),
            dep: "amod".into(),
            head: 0,
            sentence: 0,
            morph: BTreeMap::new(),
            is_punct: false,
            is_space: false,
        };
        assert!(position_allowed(&s.words[0], &token));
        assert!(!position_allowed(&s.words[1], &token));
        token.dep = "acomp".into();
        assert!(!position_allowed(&s.words[0], &token));
        assert!(position_allowed(&s.words[1], &token));
        assert!(sense("00000001 00 a 04 tiny 0 000 | truncated").is_err());
        assert!(sense("00000001 00 a 01 tiny(x) 0 000 | bad marker").is_err());
    }
    #[test]
    fn unigram_effect_matches_full_distribution_and_weights_reference_works_equally() {
        let c = |a, b| Counts {
            total: a + b,
            words: BTreeMap::from([("tiny".into(), a), ("small".into(), b)]),
            contexts: BTreeMap::new(),
        };
        let refs = [c(0, 2), c(90, 10)];
        assert_eq!(target(&refs, "tiny"), 0.45);
        let original = c(8, 2);
        let changed = c(7, 3);
        let distance = |x: &Counts| {
            0.5 * ["tiny", "small"]
                .iter()
                .map(|w| (rate(x, w).sqrt() - target(&refs, w).sqrt()).powi(2))
                .sum::<f64>()
        };
        assert!(
            (gain(&original, &refs, "tiny", "small") - (distance(&original) - distance(&changed)))
                .abs()
                < 1e-12
        );
        assert!(gain(&original, &refs, "tiny", "small") > 0.0);
    }
}
