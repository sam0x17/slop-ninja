//! Parent-conditioned format normaliser for the CC-parented mock batch
//! (era-v1 follow-up to the failed format gate).
//!
//! Human excerpts reach the cohort through the pilot's excerpt rules
//! (`candidates`, `grammar_a_pools::fineweb::classify`): every paragraph is
//! one squashed line, paragraphs are joined by one blank line, and each
//! paragraph passes the shared prose screen. Generated text came out of the
//! local servers in its own form: trailing spaces (OLMo-2), its own quote,
//! dash and ellipsis glyphs, single line breaks, lists, headings and its own
//! paragraphing. This module puts every generated child into its parent's
//! form inside the acceptance path (`generation::PostProcess`), and applies
//! the one rule that is not parent-specific, the excerpt layout, to every
//! class at assembly.
//!
//! * **Parent profile.** Counts of curly and ASCII double and single quotes,
//!   dash punctuation by style (em closed or spaced, spaced en dash, spaced
//!   hyphen, double hyphen closed or spaced), numeric ranges (en dash or
//!   hyphen) and ellipses (character or three dots). Each convention is the
//!   parent's majority; a tie or no evidence falls back to the parent's other
//!   quote class (quotes only), then to a keyed draw from the batch prior: the
//!   share of each convention among batch parents that show it. Draws are
//!   keyed by `rank(seed, KEY_SCOPE, doc_id:field)`, so every attempt and the
//!   top-up of one document see the same choice.
//! * **Child normalisation.** Lines are squashed and blank lines dropped
//!   (trailing and leading spaces, tabs, NBSP and single breaks go). Paired
//!   markdown emphasis markers are removed. Every quote, dash, range and
//!   ellipsis site is rendered in the parent's convention; ASCII quotes become
//!   curly by context. Paragraphs are then rebuilt to the parent's structure:
//!   a mirror takes its parent's paragraph break fractions, a mixed child the
//!   break fractions of the human span it replaces, with breaks placed at the
//!   nearest admissible sentence boundaries (minimum total distance, every
//!   paragraph at least `MIN_PARAGRAPH_WORDS`, counting the parent words the
//!   first and last segments join).
//! * **Form screen.** The excerpt rules applied to humans apply to the child:
//!   each generated line must end a sentence and not be list-like, and every
//!   final paragraph holding generated text must pass `fineweb::classify`
//!   (length, termination, URL, markup, boilerplate, ASCII letters, long
//!   quotation). A child that fails is rejected like any other acceptance
//!   failure, so its request continues with its remaining attempts.
//! * **Every class.** At assembly every document, human negatives included,
//!   passes through `excerpt_layout` and the paragraph screen, and the record
//!   says whether either changed or flagged anything. Human negatives already
//!   have the excerpt form, so this is expected to leave them unchanged.
//!
//! **Profiles.** The cohort generation path runs under a versioned profile
//! (`Profile`): `raw` (era-v1: no post-processing), `cc_era_parent_norm_v1`
//! (format-v1: the normaliser and form screen; the every-class step is
//! recorded but excludes nothing) and `cc_cohort_profile_v1` (the default:
//! the normaliser and form screen, plus one final-document length window,
//! `LENGTH_WINDOW`, the human excerpt's own 150..=450 whitespace-word bound,
//! in the acceptance path; at assembly every document of every class goes
//! through the excerpt layout, the paragraph screen and the length window,
//! and a document that fails any of them leaves the cohort whatever its
//! class). It replaces the layout normaliser v0 (`era_gates::normalise_v0`),
//! which rewrote every class to ASCII and is kept only to reproduce era-v1.
//! A human-only document is its own parent, so the normaliser's target form
//! is the document itself and its text is never rewritten.
//!
//! The normaliser never adds or removes a word token's letters or digits:
//! folded (lowercase alphanumeric) token sequences are invariant, so the
//! overlap screens see the same windows. Whitespace word counts can change
//! when a dash style changes spacing. Records hold counts and digests, never
//! text.
//!
//! **Profile v2** (`cc_cohort_profile_v2`, protocol A2 §6.2 as amended by
//! amendments 7, 10 and 13) replaces v1's layout step with the
//! parent-conditioned layout transfer `layout_transfer_v2`
//! (`grammar_a_form::profile::child_v2`): the licence screen
//! `cc_form_licence_screen_v1` applies to every prose-path child (a source
//! passing the screen gives §6.2's screen exactly), the echo screen applies
//! to every child, and the keyed convention draws use the prior of the
//! child's own population and domain (`write_prior_v2`, schema
//! `PRIOR_SCHEMA_V2`: one batch's parents of one population per prior file,
//! the population named by the batch's `BATCH.json` from the closed list
//! `R2_POPULATIONS` (amendment 10 item 10.8), its own seed
//! `norm_prior_seed(population)`, a cell per parent domain, the
//! population-wide prior as the fallback, then none). The generation and
//! assembly records name the prior's SHA-256 (`norm_sha256`); `BATCH.json`
//! cannot, since the prior binds its digest. A
//! v2 prior serves only profile v2 and a v1 prior only the v1 profiles
//! (`check_profile_prior`, called by `generate`, `renorm` and `assemble`).
//! On CC excerpt-layout parents the v2 child is v1's byte for byte. The
//! record keeps v1's fields and adds the profile, the layout class, the
//! measured source properties, the transfer, the echo verdict and the
//! raw-response SHA-256, never text. Only the v2 step's verdicts keep the
//! cleaned response as `raw_text` (`EraPost::is_profile_v2`), so v1
//! generation and v1 `renorm` journals keep their pre-v2 bytes.
//!
//! **Where the rules live.** The pure form rules (conventions and their
//! rendering, the excerpt layout, break placement, the child normaliser, the
//! paragraph screen, the profiles, the length window, the every-class step,
//! the layout measurement and transfer and the v2 prior's resolution) are in
//! the shared crate `grammar-a-form` and re-exported here under their v1
//! names. This module keeps the parts with I/O or generation context: the
//! prior files (`write_prior`, `write_prior_v2`, `EraPost::load`), the
//! acceptance hook (`EraPost` as a `generation::PostProcess`) and `renorm`.
use crate::era_batch::{self, Parent};
use crate::measure::{file_sha256, sha256_hex};
use crate::store;
use anyhow::{Context, Result, ensure};
pub use grammar_a_form::conventions::{
    Changes, Choice, Conventions, Counts, DASHES, Dash, ELISIONS, ELLIPSES, Ellipsis, KEY_SCOPE, Own, Prior, Quote, RANGES, Range, Site, counts, own,
    resolve, sites,
};
#[cfg(test)]
use grammar_a_form::conventions::{render_quotes, render_sites};
pub use grammar_a_form::layout::{
    ABBREVIATIONS, EXCERPT_LAYOUT_ID, Layout, MIN_PARAGRAPH_WORDS, Normalised, Structure, excerpt_layout, normalise_child, place_breaks,
};
#[cfg(test)]
use grammar_a_form::layout::{ends_with_abbreviation, layout_lines, starts_sentence, strip_emphasis};
pub use grammar_a_form::profile::{
    NORM_ID, PROFILE_RAW, PROFILE_V1, PROFILE_V2, PriorV2, Profile, ProfileError, R2_POPULATIONS, child_v2, every_class, is_r2_population, norm_prior_seed, resolve_v2,
};
pub use grammar_a_form::transfer::LAYOUT_TRANSFER_ID;
pub use grammar_a_form::screen::paragraph_screen;
pub use grammar_a_form::window::{LENGTH_WINDOW, LENGTH_WINDOW_ID};
#[cfg(test)]
use grammar_a_pools::fineweb;
#[cfg(test)]
use grammar_a_positives::text as gtext;
use grammar_a_positives::{
    generation::{self, PostOut, PostProcess},
    overlap,
    plan::Request,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

pub const PRIOR_SCHEMA: &str = "slop_ninja_cc_era_norm_prior_v1";
/// Profile v2's prior file: one population, its seed, a cell per domain.
pub const PRIOR_SCHEMA_V2: &str = "slop_ninja_cc_era_norm_prior_v2";
pub const RENORM_SCHEMA: &str = "slop_ninja_cc_era_renorm_v1";
pub const DEFAULT_SEED: &str = "cc-era-parent-norm-20261031";

// ------------------------------------------------------------------ post-process

/// The prior file: tallies over every batch parent, with the key seed.
pub fn write_prior(batch: &Path, out: &Path, seed: &str) -> Result<Value> {
    ensure!(!out.exists(), "prior already written: {}", out.display());
    let parents = era_batch::read_parents(batch)?;
    let prior = Prior::of(parents.iter().map(|p| p.text.as_str()));
    let mut by_domain: BTreeMap<String, Prior> = BTreeMap::new();
    for p in &parents {
        let d = by_domain.entry(p.domain.clone()).or_default();
        let x = Prior::of(std::iter::once(p.text.as_str()));
        d.parents += 1;
        for (k, v) in x.quotes {
            *d.quotes.entry(k).or_default() += v;
        }
        for (k, v) in x.dash {
            *d.dash.entry(k).or_default() += v;
        }
        for (k, v) in x.range {
            *d.range.entry(k).or_default() += v;
        }
        for (k, v) in x.ellipsis {
            *d.ellipsis.entry(k).or_default() += v;
        }
    }
    let layout_conforming = parents.iter().filter(|p| excerpt_layout(&p.text) == p.text).count();
    let rec = json!({"schema": PRIOR_SCHEMA, "normaliser": NORM_ID, "seed": seed, "key_scope": KEY_SCOPE,
        "batch_sha256": file_sha256(&batch.join("BATCH.json"))?, "prior": prior, "prior_by_domain_descriptive": by_domain,
        "parents_in_excerpt_layout": layout_conforming,
        "rule": "pooled over every batch parent: a convention counts for a parent when it is that parent's strict majority; used only where a document's parent shows no majority (quotes fall back to the parent's other quote class first)",
        "min_paragraph_words": MIN_PARAGRAPH_WORDS, "excerpt_layout": EXCERPT_LAYOUT_ID});
    store::write_json(out, &rec)?;
    Ok(json!({"parents": parents.len(), "parents_in_excerpt_layout": layout_conforming, "prior": rec["prior"]}))
}

/// Profile v2's prior for a v2 prior file, with each batch parent's domain.
pub struct V2Prior {
    pub prior: PriorV2,
    pub domains: HashMap<String, String>,
}

pub struct EraPost {
    pub seed: String,
    pub prior: Prior,
    pub norm_sha256: String,
    /// Present for a v2 prior file: the child step is profile v2's.
    pub v2: Option<V2Prior>,
}

/// Refuses a population id outside the closed list `R2_POPULATIONS`
/// (protocol A2 amendment 10 item 10.8).
pub fn check_population(p: &str) -> Result<()> {
    ensure!(is_r2_population(p), "population {p:?} is not a round-2 population id (amendment 10 item 10.8: {})", R2_POPULATIONS.join(", "));
    Ok(())
}

/// The population a batch's `BATCH.json` names (profile v2 batches; B7 and
/// B8 write it), from the closed list `R2_POPULATIONS`.
pub fn batch_population(batch: &Path) -> Result<String> {
    let b: Value = store::read_json(&batch.join("BATCH.json"))?;
    let p = b["population"].as_str().context("BATCH.json names no population: a profile v2 prior needs a population batch")?;
    ensure!(!p.trim().is_empty() && p.trim() == p, "BATCH.json population {p:?}");
    check_population(p)?;
    Ok(p.to_string())
}

/// Profile v2's prior file: the population's parents in the batch, tallied
/// population-wide and per domain, with the population's own key seed
/// (`norm_prior_seed`, protocol A2 amendment 7 item 7.8). The batch must name
/// that population in `BATCH.json`, so no other population's parent informs
/// the prior, and a v2 prior holds one batch's parents (amendment 10 item
/// 10.8: a D-step's keyed pilot and its bulk are one batch, so the prior is
/// written once, over the whole plan's parents, before the pilot).
pub fn write_prior_v2(batch: &Path, out: &Path, population: &str) -> Result<Value> {
    ensure!(!out.exists(), "prior already written: {}", out.display());
    ensure!(!population.trim().is_empty(), "profile v2 needs a population");
    check_population(population)?;
    let named = batch_population(batch)?;
    ensure!(named == population, "the batch is population {named}, not {population}");
    let seed = norm_prior_seed(population);
    let seed = seed.as_str();
    let parents = era_batch::read_parents(batch)?;
    let prior = PriorV2::of(population, parents.iter().map(|p| (p.domain.as_str(), p.text.as_str())));
    let parents_by_domain: BTreeMap<&str, usize> = prior.by_domain.iter().map(|(d, p)| (d.as_str(), p.parents)).collect();
    let rec = json!({"schema": PRIOR_SCHEMA_V2, "profile": PROFILE_V2, "normaliser": NORM_ID, "population": population, "seed": seed,
        "key_scope": KEY_SCOPE, "batch_sha256": file_sha256(&batch.join("BATCH.json"))?, "prior": prior,
        "seed_rule": "norm_prior_seed: SHA-256 hex of 'slop_ninja_a_comp_r2_plan_v1:norm_prior:' + population (protocol A2 V21; amendment 7 item 7.8, amendment 10 item 10.8)",
        "rule": "one population: a convention counts for a parent when it is that parent's strict majority; a child's keyed draw (KEY_SCOPE, this seed) uses its parent's domain cell when that cell holds a parent with the convention, else the population-wide prior, else none; quotes fall back to the parent's other quote class first",
        "min_paragraph_words": MIN_PARAGRAPH_WORDS, "layout_transfer": LAYOUT_TRANSFER_ID});
    store::write_json(out, &rec)?;
    Ok(json!({"parents": parents.len(), "population": population, "parents_by_domain": parents_by_domain, "prior": rec["prior"]["prior"]}))
}

/// The prior file's schema (`PRIOR_SCHEMA` or `PRIOR_SCHEMA_V2`).
pub fn prior_schema(norm: &Path) -> Result<String> {
    let v: Value = store::read_json(norm)?;
    Ok(v["schema"].as_str().context("prior schema")?.to_string())
}

/// The profile takes the prior it needs: none for `raw`, a v2 prior for
/// profile v2 and a v1 prior for the other normalising profiles.
pub fn check_profile_prior(profile: Profile, norm: Option<&Path>) -> Result<()> {
    profile.check_prior(norm)?;
    if let Some(n) = norm {
        let schema = prior_schema(n)?;
        let want = if profile.transfers() { PRIOR_SCHEMA_V2 } else { PRIOR_SCHEMA };
        ensure!(schema == want, "profile {} needs a {want} prior, not {schema}", profile.id());
    }
    Ok(())
}

impl EraPost {
    /// Loads the prior file (v1 or v2) and checks it against the batch it names.
    pub fn load(norm: &Path, batch: &Path) -> Result<EraPost> {
        let v: Value = store::read_json(norm)?;
        ensure!(v["batch_sha256"] == file_sha256(&batch.join("BATCH.json"))?.as_str(), "prior belongs to another batch");
        let parents = era_batch::read_parents(batch)?;
        let seed = v["seed"].as_str().context("seed")?.to_string();
        if v["schema"] == PRIOR_SCHEMA_V2 {
            ensure!(v["normaliser"] == NORM_ID && v["profile"] == PROFILE_V2 && v["key_scope"] == KEY_SCOPE, "not a {PROFILE_V2} prior");
            let prior: PriorV2 = serde_json::from_value(v["prior"].clone())?;
            check_population(&prior.population)?;
            ensure!(v["population"] == prior.population.as_str(), "prior population mismatch");
            ensure!(batch_population(batch)? == prior.population, "prior population is not the batch's");
            ensure!(seed == norm_prior_seed(&prior.population), "prior seed is not its population's norm_prior_seed");
            let want = PriorV2::of(&prior.population, parents.iter().map(|p| (p.domain.as_str(), p.text.as_str())));
            ensure!(want == prior, "prior does not match the batch parents");
            let domains = parents.iter().map(|p| (p.id.clone(), p.domain.clone())).collect();
            return Ok(EraPost { seed, prior: prior.prior.clone(), norm_sha256: file_sha256(norm)?, v2: Some(V2Prior { prior, domains }) });
        }
        ensure!(v["schema"] == PRIOR_SCHEMA && v["normaliser"] == NORM_ID, "not a {NORM_ID} prior");
        let prior: Prior = serde_json::from_value(v["prior"].clone())?;
        ensure!(Prior::of(parents.iter().map(|p| p.text.as_str())) == prior, "prior does not match the batch parents");
        Ok(EraPost { seed, prior, norm_sha256: file_sha256(norm)?, v2: None })
    }

    /// Profile v2's child step: the conventions from the population x
    /// domain prior, then `child_v2`; the record adds v1's ids.
    fn apply_v2(&self, v2: &V2Prior, r: &Request, p: &str, clean: &str, doc: String) -> PostOut {
        let c = counts(p);
        let domain = v2.domains.get(&r.parent_id).map(String::as_str).unwrap_or("");
        let conv = resolve_v2(&c, &v2.prior, domain, &self.seed, &doc);
        let out = child_v2(p, r.source_span, &conv, clean);
        let mut record = out.record;
        record["norm_sha256"] = json!(self.norm_sha256);
        record["doc_id"] = json!(doc);
        record["parent_id"] = json!(r.parent_id);
        record["population"] = json!(v2.prior.population);
        record["domain"] = json!(domain);
        record["parent_counts"] = json!(c);
        PostOut { text: out.text, reasons: out.reasons, record }
    }
}

impl PostProcess for EraPost {
    fn apply(&self, r: &Request, parent: Option<&str>, clean: &str) -> PostOut {
        let role = r.cohort.clone().unwrap_or_default();
        let doc = era_batch::doc_id_of(&r.parent_id, &role);
        let Some(p) = parent else {
            return PostOut { text: clean.to_string(), reasons: vec!["form_parent_missing".into()], record: json!({"normaliser": NORM_ID, "doc_id": doc}) };
        };
        if let Some(v2) = &self.v2 {
            return self.apply_v2(v2, r, p, clean, doc);
        }
        let c = counts(p);
        let conv = resolve(&c, &self.prior, &self.seed, &doc);
        let st = match r.source_span {
            Some((s, e)) => Structure::span(p, s, e),
            None => Structure::whole(p),
        };
        let n = normalise_child(clean, &conv, &st);
        let mut reasons = n.line_reasons.clone();
        if n.text.is_empty() {
            reasons.push("form_empty".into());
        } else {
            let assembled = match r.source_span {
                Some(span) => generation::assemble(p, &[span], &[n.text.as_str()]).map(|(t, fin, _)| (t, fin)),
                None => Ok((n.text.clone(), vec![(0, n.text.len())])),
            };
            match assembled {
                Ok((t, fin)) => reasons.extend(paragraph_screen(&t, Some(fin.as_slice()))),
                Err(e) => reasons.push(format!("form_assembly_error:{e}")),
            }
        }
        reasons.sort();
        reasons.dedup();
        let record = json!({"normaliser": NORM_ID, "norm_sha256": self.norm_sha256, "doc_id": doc, "parent_counts": c,
            "conventions": conv, "changes": n.changes, "layout": n.layout, "form_reasons": reasons,
            "input_sha256": sha256_hex(clean.as_bytes()), "output_sha256": sha256_hex(n.text.as_bytes()), "changed": n.text != clean});
        PostOut { text: n.text, reasons, record }
    }

    /// Profile v2's step exactly when the prior is a v2 prior, which only
    /// profile v2 takes (`check_profile_prior`). A v1 normaliser's verdicts
    /// keep no `raw_text`, so v1 generation and v1 `renorm` journals keep
    /// the bytes they had before profile v2 existed.
    fn is_profile_v2(&self) -> bool {
        self.v2.is_some()
    }
}

// ------------------------------------------------------------------ renorm

pub struct RenormArgs {
    pub batch: PathBuf,
    pub norm: PathBuf,
    /// Cohort generation profile (a normalising one).
    pub profile: Profile,
    /// Journals to re-verdict: `<runs>/<generator>/journal-*.jsonl`.
    pub runs: PathBuf,
    /// Request set (a top-up directory); default the batch. Only its requests are carried over.
    pub requests: Option<PathBuf>,
    pub out: PathBuf,
    pub tokenizer: PathBuf,
}

/// Re-verdicts every journaled response under the normaliser, as if it had
/// been in the acceptance path: per generator, responses in completion order,
/// each with the full acceptance verdict (the post-processed text, every
/// standard check, the overlap index growing with accepted texts as in the
/// original run). Intents are carried over, so a request whose accepted
/// response is now rejected resumes with its remaining attempts.
pub fn renorm(a: &RenormArgs) -> Result<Value> {
    let set = a.requests.clone().unwrap_or_else(|| a.batch.clone());
    let meta_path = if set.join("TOPUP.json").exists() { set.join("TOPUP.json") } else { set.join("BATCH.json") };
    let meta: Value = store::read_json(&meta_path)?;
    if meta_path.ends_with("TOPUP.json") {
        ensure!(meta["batch_sha256"] == file_sha256(&a.batch.join("BATCH.json"))?.as_str(), "top-up belongs to another batch");
    }
    check_profile_prior(a.profile, Some(&a.norm))?;
    let post = Arc::new(EraPost::load(&a.norm, &a.batch)?);
    let parents: Vec<Parent> = era_batch::read_parents(&a.batch)?;
    let ptext_map: HashMap<String, String> = parents.iter().map(|p| (p.id.clone(), p.text.clone())).collect();
    let mut tok = tokenizers::Tokenizer::from_file(&a.tokenizer).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tok.with_truncation(None).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tok.with_padding(None);
    let t0 = generation::now_ms();
    let mut summary = BTreeMap::new();
    for g in meta["request_files"].as_object().context("request_files")?.keys() {
        let rp = set.join("requests").join(format!("{g}.jsonl"));
        ensure!(meta["request_files"][g]["sha256"] == file_sha256(&rp)?.as_str(), "request file changed: {g}");
        let reqs: HashMap<String, Request> = crate::measure::read_jsonl(&rp)?
            .into_iter()
            .map(|v| serde_json::from_value::<Request>(v).map(|r| (r.request_id.clone(), r)))
            .collect::<Result<_, _>>()?;
        let in_dir = a.runs.join(g);
        let out_dir = a.out.join(g);
        ensure!(generation::journal_files(&out_dir)?.is_empty(), "{} already holds journals", out_dir.display());
        let mut intents: Vec<Value> = Vec::new();
        let mut responses: Vec<(Value, String)> = Vec::new();
        let mut skipped = 0usize;
        for p in generation::journal_files(&in_dir)? {
            for (n, line) in BufReader::new(fs::File::open(&p)?).lines().enumerate() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    eprintln!("torn journal line {}:{}", p.display(), n + 1);
                    continue;
                };
                let rid = v["request_id"].as_str().context("request_id")?;
                if !reqs.contains_key(rid) {
                    skipped += 1;
                    continue;
                }
                match v["kind"].as_str() {
                    Some("intent") => intents.push(v),
                    Some("response") => responses.push((v, p.display().to_string())),
                    _ => anyhow::bail!("journal kind"),
                }
            }
        }
        responses.sort_by(|x, y| {
            let k = |v: &Value| (v["t_end"].as_u64().unwrap_or(0), v["request_id"].as_str().unwrap_or("").to_string(), v["attempt"].as_u64().unwrap_or(0));
            k(&x.0).cmp(&k(&y.0))
        });
        let ctx = generation::Ctx {
            endpoint: String::new(),
            alias: String::new(),
            config: Value::Null,
            tokenizer: tok.clone(),
            overlap: overlap::Overlap::new(overlap::PriorIndex::empty()),
            parents: ptext_map.clone(),
            client: generation::client()?,
            post: Some(post.clone() as Arc<dyn PostProcess>),
            doc_words: a.profile.window(),
        };
        fs::create_dir_all(&out_dir)?;
        let jpath = out_dir.join(format!("journal-{t0}-renorm.jsonl"));
        let mut f = std::io::BufWriter::new(fs::OpenOptions::new().create_new(true).write(true).open(&jpath)?);
        for v in &intents {
            serde_json::to_writer(&mut f, v)?;
            f.write_all(b"\n")?;
        }
        let mut before: HashSet<String> = HashSet::new();
        let mut after: HashSet<String> = HashSet::new();
        let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
        let mut changed_text = 0usize;
        for (mut v, src) in responses {
            let rid = v["request_id"].as_str().unwrap_or("").to_string();
            let old = v["verdict"].clone();
            if old["accepted"] == true {
                before.insert(rid.clone());
            }
            let transport = old["reasons"].as_array().is_some_and(|x| x.iter().any(|r| r == "transport_error"));
            let new = if transport {
                old.clone()
            } else {
                let parsed: Value = serde_json::from_str(v["raw_body"].as_str().unwrap_or("")).unwrap_or(Value::Null);
                let status = v["status"].as_u64().unwrap_or(0) as u16;
                generation::verdict(&ctx, &reqs[&rid], status, &parsed)
            };
            if new["accepted"] == true {
                ctx.overlap.add_generated(new["text"].as_str().unwrap_or(""));
                after.insert(rid.clone());
                if new["diag"]["post"]["changed"] == true {
                    changed_text += 1;
                }
            } else if old["accepted"] == true {
                for r in new["reasons"].as_array().into_iter().flatten() {
                    *reasons.entry(r.as_str().unwrap_or("").to_string()).or_default() += 1;
                }
            }
            v["verdict"] = new;
            v["renorm"] = json!({"profile": a.profile.id(), "normaliser": NORM_ID, "norm_sha256": post.norm_sha256, "source_journal": src,
                "original_accepted": old["accepted"], "original_reasons": old["reasons"]});
            if a.profile.transfers() {
                v["renorm"]["layout_transfer"] = json!(LAYOUT_TRANSFER_ID);
            }
            serde_json::to_writer(&mut f, &v)?;
            f.write_all(b"\n")?;
        }
        f.flush()?;
        f.get_ref().sync_all()?;
        drop(f);
        let states = generation::load_states(std::slice::from_ref(&out_dir))?;
        let pending = reqs.keys().filter(|r| states.get(*r).is_some_and(|s| !s.done())).count();
        let exhausted = reqs.keys().filter(|r| states.get(*r).is_some_and(|s| s.done() && s.accepted().is_none())).count();
        let not_run = reqs.keys().filter(|r| !states.contains_key(*r)).count();
        summary.insert(g.clone(), json!({"requests": reqs.len(), "accepted_before": before.len(), "accepted_after": after.len(),
            "accepted_now_rejected": before.difference(&after).count(), "rejected_now_accepted": after.difference(&before).count(),
            "accepted_with_changed_text": changed_text, "now_rejected_reasons": reasons, "pending_attempts_after": pending,
            "exhausted_after": exhausted, "not_run": not_run, "skipped_lines_outside_set": skipped, "journal": jpath}));
    }
    let rec = json!({"schema": RENORM_SCHEMA, "profile": a.profile.record(), "normaliser": NORM_ID, "norm_sha256": post.norm_sha256, "t0": t0,
        "batch_sha256": file_sha256(&a.batch.join("BATCH.json"))?, "request_set": meta_path, "runs": a.runs,
        "tokenizer_sha256": file_sha256(&a.tokenizer)?, "by_generator": summary,
        "rule": "per generator, every journaled response in completion order re-verdicted under the profile (normaliser, form screen and any length window) in the acceptance path; overlap index empty at start and grown with accepted texts; intents carried over"});
    store::write_json(&a.out.join(format!("RENORM-{t0}.json")), &rec)?;
    Ok(rec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(d: Option<Quote>, s: Option<Quote>, dash: Option<Dash>, range: Option<Range>, ell: Option<Ellipsis>) -> Conventions {
        let c = |v| Choice { value: v, source: "parent" };
        Conventions { double: c(d), single: c(s), dash: Choice { value: dash, source: "parent" }, range: Choice { value: range, source: "parent" }, ellipsis: Choice { value: ell, source: "parent" } }
    }

    #[test]
    fn min_paragraph_words_matches_the_prose_screen() {
        let seven = "One two three four five six seven.";
        let eight = "One two three four five six seven eight.";
        assert_eq!(fineweb::classify(seven, false).reason, Some("short_paragraph"));
        assert!(fineweb::classify(eight, false).prose);
        assert_eq!(gtext::words(eight), MIN_PARAGRAPH_WORDS);
    }

    #[test]
    fn sites_find_dashes_ranges_and_ellipses() {
        let t = "It rained\u{2014}hard. Then \u{2014} briefly \u{2013} it cleared - mostly -- or so. In 1990-1995 and 3\u{2013}4 days... wait\u{2026} 2010-05-01 COVID-19 well--known";
        let s = sites(t);
        let dashes: Vec<Dash> = s.iter().filter_map(|x| if let Site::Dash(_, _, d) = x { Some(*d) } else { None }).collect();
        assert_eq!(dashes, vec![Dash::EmClosed, Dash::EmSpaced, Dash::EnSpaced, Dash::HyphenSpaced, Dash::DoubleSpaced, Dash::DoubleClosed]);
        let ranges: Vec<Range> = s.iter().filter_map(|x| if let Site::Range(_, _, r) = x { Some(*r) } else { None }).collect();
        assert_eq!(ranges, vec![Range::Hyphen, Range::En]);
        let ell: Vec<Ellipsis> = s.iter().filter_map(|x| if let Site::Ellipsis(_, _, e) = x { Some(*e) } else { None }).collect();
        assert_eq!(ell, vec![Ellipsis::Dots, Ellipsis::Char]);
        // A site's range covers exactly the text it replaces.
        if let Site::Dash(a, b, _) = s[1] {
            assert_eq!(&t[a..b], " \u{2014} ");
        }
        assert!(sites("A \u{2014}\u{2014} B and \u{2014}lead").is_empty());
    }

    #[test]
    fn counts_and_majorities() {
        let c = counts("\u{201C}Hi,\u{201D} she said. It\u{2019}s \"fine\" \u{2014} really\u{2014}truly\u{2014}so.");
        assert_eq!((c.dq_curly, c.dq_ascii, c.sq_curly, c.sq_ascii), (2, 2, 1, 0));
        let o = own(&c);
        assert_eq!(o.double, None);
        assert_eq!(o.single, Some(Quote::Curly));
        assert_eq!(o.quotes, Some(Quote::Curly));
        assert_eq!(o.dash, Some(Dash::EmClosed));
        assert_eq!(o.range, None);
    }

    #[test]
    fn resolution_falls_back_to_the_other_quote_then_a_keyed_draw() {
        let prior = Prior {
            parents: 10,
            quotes: BTreeMap::from([(Quote::Curly, 6), (Quote::Ascii, 4)]),
            dash: BTreeMap::from([(Dash::EmClosed, 1)]),
            range: BTreeMap::new(),
            ellipsis: BTreeMap::from([(Ellipsis::Dots, 3)]),
        };
        let c = counts("It\u{2019}s here and it\u{2019}s fine.");
        let r = resolve(&c, &prior, "s", "doc-a");
        assert_eq!(r.single, Choice { value: Some(Quote::Curly), source: "parent" });
        assert_eq!(r.double, Choice { value: Some(Quote::Curly), source: "parent_other_quote" });
        assert_eq!(r.dash, Choice { value: Some(Dash::EmClosed), source: "keyed_prior" });
        assert_eq!(r.range, Choice { value: None, source: "none" });
        assert_eq!(r.ellipsis.value, Some(Ellipsis::Dots));
        // No quotes at all: both quote classes take the same keyed draw, stable per document.
        let none = counts("Plain words only here.");
        let mut seen = HashSet::new();
        for k in 0..64 {
            let a = resolve(&none, &prior, "s", &format!("doc-{k}"));
            assert_eq!(a.double.value, a.single.value);
            assert_eq!(a.double.source, "keyed_prior");
            assert_eq!(a, resolve(&none, &prior, "s", &format!("doc-{k}")));
            seen.insert(a.double.value);
        }
        assert_eq!(seen.len(), 2, "both conventions are drawn");
    }

    #[test]
    fn quotes_render_by_context() {
        let mut ch = Changes::default();
        let c = conv(Some(Quote::Curly), Some(Quote::Curly), None, None, None);
        assert_eq!(render_quotes("\"Don't,\" she said, 'twas the '90s and 'quiet' rock 'n' roll.", &c, &mut ch),
            "\u{201C}Don\u{2019}t,\u{201D} she said, \u{2019}twas the \u{2019}90s and \u{2018}quiet\u{2019} rock \u{2019}n\u{2019} roll.");
        assert_eq!(ch.double_quotes, 2);
        let a = conv(Some(Quote::Ascii), Some(Quote::Ascii), None, None, None);
        assert_eq!(render_quotes("\u{201C}It\u{2019}s\u{201D}", &a, &mut ch), "\"It's\"");
    }

    #[test]
    fn dashes_ranges_and_ellipses_take_the_parent_style() {
        let mut ch = Changes::default();
        let c = conv(None, None, Some(Dash::HyphenSpaced), Some(Range::En), Some(Ellipsis::Char));
        assert_eq!(render_sites("It rained\u{2014}hard, from 1990-1995... then -- stopped.", &c, &mut ch), "It rained - hard, from 1990\u{2013}1995\u{2026} then - stopped.");
        assert_eq!((ch.dashes, ch.ranges, ch.ellipses), (2, 1, 1));
        let c2 = conv(None, None, Some(Dash::EmClosed), None, None);
        assert_eq!(render_sites("a - b \u{2013} c", &c2, &mut ch), "a\u{2014}b\u{2014}c");
    }

    #[test]
    fn layout_drops_trailing_space_and_single_breaks() {
        let mut ch = Changes::default();
        let lines = layout_lines("First line here.  \n  Second\tline.\n\n\nThird line.", &mut ch);
        assert_eq!(lines, vec!["First line here.", "Second line.", "Third line."]);
        assert_eq!(ch.trailing_space_lines, 1);
        assert_eq!(ch.leading_space_lines, 1);
        assert_eq!(ch.single_breaks, 1);
        assert_eq!(ch.blank_lines_dropped, 1);
        assert_eq!(excerpt_layout("a b.\n\nc  d. \n e."), "a b.\n\nc d.\n\ne.");
        let mut ch2 = Changes::default();
        assert_eq!(strip_emphasis("A **bold** and __under__ word **x", &mut ch2), "A bold and under word **x");
        assert_eq!(ch2.markdown_emphasis_pairs, 2);
    }

    #[test]
    fn breaks_follow_the_parent_fractions_with_a_paragraph_floor() {
        // Ten 5-word sentences; parent paragraphs at 30% and 70%.
        let words = vec![5; 10];
        let allowed = vec![true; 11];
        let st = Structure { fractions: vec![0.3, 0.7], pre_words: 0, post_words: 0 };
        assert_eq!(place_breaks(&words, &allowed, &st), (vec![3, 7], 0));
        // A 5% paragraph cannot be kept at 8 words when sentences have 5: it moves.
        let st2 = Structure { fractions: vec![0.05], pre_words: 0, post_words: 0 };
        assert_eq!(place_breaks(&words, &allowed, &st2), (vec![2], 0));
        // Too few words for both breaks: the smallest parent pair is merged.
        let st3 = Structure { fractions: vec![0.3, 0.5], pre_words: 0, post_words: 0 };
        assert_eq!(place_breaks(&[5, 5, 5, 5], &[true; 5], &st3), (vec![2], 1));
        assert_eq!(place_breaks(&[5, 5, 5], &[true; 4], &st3), (vec![], 2));
        // Joined parent words count toward the first and last paragraphs.
        let st4 = Structure { fractions: vec![0.1], pre_words: 20, post_words: 0 };
        assert_eq!(place_breaks(&words, &allowed, &st4), (vec![1], 0));
        // Disallowed boundaries are never used.
        let mut al = vec![true; 11];
        al[3] = false;
        assert_eq!(place_breaks(&words, &al, &st).0, vec![2, 7]);
    }

    #[test]
    fn child_takes_the_parent_form_and_the_screen_rejects_lists_and_titles() {
        let parent = "\u{201C}We left at dawn,\u{201D} the captain\u{2019}s mate said to the crew that morning.\n\nThe harbour was quiet and the gulls followed the ferry for a while before turning back to land.";
        let c = counts(parent);
        let prior = Prior::of(std::iter::once(parent));
        let conv = resolve(&c, &prior, "s", "d");
        let child = "The ferry's horn sounded twice as it left the pier.  \nNobody on the deck said a word about the \"storm\" coming in from the west.\nThe passengers waited below. They talked about the weather and their plans for the evening ahead.";
        let n = normalise_child(child, &conv, &Structure::whole(parent));
        assert!(n.line_reasons.is_empty());
        assert_eq!(n.layout.realised_breaks, 1);
        assert!(!n.text.contains("  ") && !n.text.contains(" \n") && n.text.matches("\n\n").count() == 1);
        assert!(n.text.contains("ferry\u{2019}s") && n.text.contains("\u{201C}storm\u{201D}"));
        assert_eq!(n.changes.trailing_space_lines, 1);
        assert!(paragraph_screen(&n.text, None).is_empty());
        // Folded tokens are unchanged.
        assert_eq!(grammar_a_pools::text::folded_tokens(child), grammar_a_pools::text::folded_tokens(&n.text));
        let titled = normalise_child("A Morning at Sea\nThe ferry left the harbour at dawn and the gulls followed it out.", &conv, &Structure::whole(parent));
        assert_eq!(titled.line_reasons, vec!["form_line_no_sentence_end"]);
        let listed = normalise_child("1. Pack the bags before you leave for the harbour.\n2. Check the ferry times twice before leaving.", &conv, &Structure::whole(parent));
        assert!(listed.line_reasons.contains(&"form_line_list_like".to_string()));
        let quoted = "The mate said \u{201C}we will leave the harbour at dawn whatever the weather does\u{201D} and walked away.";
        assert_eq!(paragraph_screen(quoted, None), vec!["form_paragraph_long_quotation"]);
    }

    #[test]
    fn a_consistent_excerpt_is_a_fixed_point_of_its_own_profile() {
        let human = "The council met on Tuesday \u{2014} late, as usual \u{2014} to discuss the 2015\u{2013}2016 budget.\n\nMembers said the plan\u{2019}s costs were too high, and one called it \u{201C}a waste\u{201D}\u{2026} and left.\n\nA vote is expected next week, after the public hearing on the matter.";
        let c = counts(human);
        let conv = resolve(&c, &Prior::default(), "s", "h");
        let n = normalise_child(human, &conv, &Structure::whole(human));
        assert_eq!(n.text, human);
        assert_eq!(n.changes, Changes::default());
        assert_eq!(every_class(human, Profile::NormV1).0, human);
    }

    #[test]
    fn profiles_and_the_every_class_length_window() {
        for p in [Profile::Raw, Profile::NormV1, Profile::CohortV1] {
            assert_eq!(Profile::parse(p.id()).unwrap(), p);
        }
        assert!(Profile::parse("layout_v0").is_err());
        assert_eq!(Profile::DEFAULT, Profile::CohortV1);
        assert_eq!(Profile::CohortV1.window(), Some((150, 450)));
        assert_eq!(Profile::NormV1.window(), None);
        assert!(Profile::CohortV1.check_prior(None).is_err());
        assert!(Profile::Raw.check_prior(Some(Path::new("x"))).is_err());
        assert!(Profile::Raw.check_prior(None).is_ok());
        // The window is the human excerpt bound.
        assert_eq!(LENGTH_WINDOW, (grammar_a_pools::text::MIN_WORDS, grammar_a_pools::text::MAX_WORDS));
        let para = "The ferry left the harbour at dawn and the gulls followed it out to sea for a while.";
        let doc = |n: usize| vec![para; n].join("\n\n");
        // 18 words per paragraph: 8 paragraphs = 144 words (short), 9 = 162, 25 = 450 (inclusive), 26 = 468 (long).
        for (n, ok) in [(8usize, false), (9, true), (25, true), (26, false)] {
            let (_, rec, why) = every_class(&doc(n), Profile::CohortV1);
            assert_eq!(why.is_empty(), ok, "{n}: {why:?}");
            assert_eq!(rec["excluded"], json!(!ok));
            assert_eq!(rec["length_window"]["pass"], json!(ok));
            // The format-v1 profile records the same document but excludes nothing.
            let (_, r1, w1) = every_class(&doc(n), Profile::NormV1);
            assert!(w1.is_empty() && r1["excluded"] == json!(false) && r1.get("length_window").is_none());
        }
        // A paragraph failing the screen fails every class alike.
        let bad = format!("{}\n\nRead more at www.example.com today", doc(9));
        let (_, _, why) = every_class(&bad, Profile::CohortV1);
        assert!(why.iter().any(|r| r.starts_with("form_paragraph_")), "{why:?}");
    }

    // ------------------------------------------------------------ profile v2

    fn parent(id: &str, domain: &str, text: &str) -> Parent {
        Parent {
            schema: era_batch::PARENT_SCHEMA.into(),
            id: id.into(),
            role: era_batch::MIXED.into(),
            cohort_member: true,
            domain: domain.into(),
            year: 2021,
            crawl: "CC-MAIN-2021-04".into(),
            warc_date: "2021-01-20".into(),
            existence_by: "2021-01".into(),
            era: "2020_2022".into(),
            group: format!("g-{id}"),
            host: format!("{id}.example"),
            url: format!("https://{id}.example/a"),
            text_variant: "excerpt".into(),
            text: text.into(),
            text_sha256: sha256_hex(text.as_bytes()),
            pool_text_sha256: sha256_hex(text.as_bytes()),
            words: grammar_a_pools::text::words(text),
            generator: None,
            op: None,
            item_id: None,
            request_id: None,
            source_span: None,
            terms: vec![],
            notes: vec![],
        }
    }

    /// A synthetic batch directory: `BATCH.json` (naming `population` when
    /// given) and `parents.jsonl`.
    fn batch_dir_of(parents: &[Parent], population: Option<&str>) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let rows: Vec<String> = parents.iter().map(|p| serde_json::to_string(p).unwrap()).collect();
        fs::write(d.path().join("parents.jsonl"), rows.join("\n") + "\n").unwrap();
        let sha = file_sha256(&d.path().join("parents.jsonl")).unwrap();
        let mut b = json!({"schema": era_batch::BATCH_SCHEMA, "parents_sha256": sha});
        if let Some(p) = population {
            b["population"] = json!(p);
        }
        store::write_json(&d.path().join("BATCH.json"), &b).unwrap();
        d
    }

    fn batch_dir(parents: &[Parent]) -> tempfile::TempDir {
        batch_dir_of(parents, Some("train2"))
    }

    fn request(parent_id: &str, span: Option<(usize, usize)>) -> Request {
        serde_json::from_value(json!({"request_id": format!("r-{parent_id}"), "item_id": "i", "parent_id": parent_id, "split": "cc_era_mock",
            "generator": "g", "endpoint": "chat", "template_id": "t", "prompt": "p", "span_index": 0, "source_span": span, "target_words": 100,
            "cut_budget": null, "decoding": {"profile": "A", "temperature": 0.7, "top_p": 0.9, "top_k": 0, "min_p": 0.0, "repeat_penalty": 1.0, "max_tokens": 512},
            "request_sha256": "x", "seed": 1, "op": "span_rewrite", "cohort": "mixed"}))
        .unwrap()
    }

    const NEWS: &str = "\u{201C}We should leave before the tide turns,\u{201D} the warden told the group on the pier.\n\nThe boats were tied up early that evening\u{2014}nobody wanted to risk the channel after dark\u{2014}and the quay was empty.\n\nBy morning the water was calm again, and the ferry ran on time from 7\u{2013}9 as usual\u{2026} nobody complained.";
    const BLOG: &str = "I walked down to the harbour on Sunday and watched the boats come in one by one.\n\nThe cafe on the corner was open, so I sat outside for an hour with a coffee and a book.";

    #[test]
    fn v2_prior_files_and_the_profile_prior_check() {
        let d = batch_dir(&[parent("a", "news", NEWS), parent("b", "blog", BLOG), parent("c", "news", BLOG)]);
        let v2 = d.path().join("norm-v2.json");
        let v1 = d.path().join("norm-v1.json");
        assert!(write_prior_v2(d.path(), &v2, " ").is_err());
        // The population must be the batch's own, and on the closed list.
        assert!(write_prior_v2(d.path(), &v2, "cal2_t").is_err());
        let e = write_prior_v2(d.path(), &v2, "cal2").unwrap_err().to_string();
        assert!(e.contains("not a round-2 population id"), "{e}");
        let off_list = batch_dir_of(&[parent("a", "news", NEWS)], Some("train"));
        assert!(batch_population(off_list.path()).unwrap_err().to_string().contains("not a round-2 population id"));
        assert!(write_prior_v2(off_list.path(), &off_list.path().join("p.json"), "train").is_err());
        assert!(!off_list.path().join("p.json").exists());
        let unnamed = batch_dir_of(&[parent("a", "news", NEWS)], None);
        assert!(write_prior_v2(unnamed.path(), &unnamed.path().join("p.json"), "train2").is_err(), "a batch must name its population");
        let out = write_prior_v2(d.path(), &v2, "train2").unwrap();
        assert_eq!(out["parents"], 3);
        assert_eq!(out["parents_by_domain"], json!({"blog": 1, "news": 2}));
        assert!(write_prior_v2(d.path(), &v2, "train2").is_err(), "never overwritten");
        write_prior(d.path(), &v1, DEFAULT_SEED).unwrap();
        assert_eq!(prior_schema(&v2).unwrap(), PRIOR_SCHEMA_V2);
        let post = EraPost::load(&v2, d.path()).unwrap();
        let p2 = post.v2.as_ref().unwrap();
        assert_eq!(p2.prior.population, "train2");
        assert_eq!(p2.prior.by_domain["news"].parents, 2);
        // The seed is the population's own, by rule.
        assert_eq!(post.seed, norm_prior_seed("train2"));
        assert_ne!(norm_prior_seed("train2"), norm_prior_seed("cal2_t"));
        let rec: Value = store::read_json(&v2).unwrap();
        assert_eq!(rec["seed_rule"].as_str().unwrap().rsplit('(').next().unwrap(), "protocol A2 V21; amendment 7 item 7.8, amendment 10 item 10.8)");
        assert!(EraPost::load(&v1, d.path()).unwrap().v2.is_none());
        // Each profile takes its own prior.
        assert!(check_profile_prior(Profile::CohortV2, Some(&v2)).is_ok());
        assert!(check_profile_prior(Profile::CohortV2, Some(&v1)).is_err());
        assert!(check_profile_prior(Profile::CohortV1, Some(&v2)).is_err());
        assert!(check_profile_prior(Profile::NormV1, Some(&v1)).is_ok());
        assert!(check_profile_prior(Profile::CohortV2, None).is_err());
        assert!(check_profile_prior(Profile::Raw, None).is_ok());
        // The library entry points check it too, before reading anything else.
        let gen_args = |norm: &Path, profile| era_batch::GenArgs {
            batch: d.path().to_path_buf(),
            requests: None,
            runs: d.path().join("runs"),
            generator: "g".into(),
            config: d.path().join("missing-config.json"),
            endpoint: "http://127.0.0.1:9".into(),
            tokenizer: d.path().join("missing-tokenizer.json"),
            workers: 1,
            limit: None,
            norm: Some(norm.to_path_buf()),
            profile,
        };
        for (norm, profile) in [(&v1, Profile::CohortV2), (&v2, Profile::CohortV1)] {
            let e = era_batch::generate(&gen_args(norm, profile)).unwrap_err().to_string();
            assert!(e.contains("prior, not"), "{e}");
            let e = era_batch::assemble(d.path(), &d.path().join("runs"), None, &d.path().join("out"), 1, Some(norm), profile).unwrap_err().to_string();
            assert!(e.contains("prior, not"), "{e}");
        }
        // A prior edited after writing no longer loads.
        let mut v: Value = store::read_json(&v2).unwrap();
        v["prior"]["by_domain"]["blog"]["parents"] = json!(2);
        let bad = d.path().join("norm-v2-bad.json");
        store::write_json(&bad, &v).unwrap();
        assert!(EraPost::load(&bad, d.path()).is_err());
        let mut w: Value = store::read_json(&v2).unwrap();
        w["population"] = json!("cal2_t");
        store::write_json(&bad, &w).unwrap();
        assert!(EraPost::load(&bad, d.path()).is_err());
        // A prior whose seed is not its population's, or whose population is
        // not the batch's, does not load.
        let mut x: Value = store::read_json(&v2).unwrap();
        x["seed"] = json!("seed-train2");
        let bad2 = d.path().join("norm-v2-seed.json");
        store::write_json(&bad2, &x).unwrap();
        assert!(EraPost::load(&bad2, d.path()).is_err());
        let mut y: Value = store::read_json(&v2).unwrap();
        y["population"] = json!("cal2_t");
        y["prior"]["population"] = json!("cal2_t");
        y["seed"] = json!(norm_prior_seed("cal2_t"));
        let bad3 = d.path().join("norm-v2-pop.json");
        store::write_json(&bad3, &y).unwrap();
        assert!(EraPost::load(&bad3, d.path()).is_err());
        // A prior naming an id off the closed list does not load either.
        let mut z: Value = store::read_json(&v2).unwrap();
        z["population"] = json!("cal2");
        z["prior"]["population"] = json!("cal2");
        z["seed"] = json!(norm_prior_seed("cal2"));
        let bad4 = d.path().join("norm-v2-off-list.json");
        store::write_json(&bad4, &z).unwrap();
        let e = EraPost::load(&bad4, d.path()).err().expect("an off-list population does not load").to_string();
        assert!(e.contains("not a round-2 population id"), "{e}");
    }

    #[test]
    fn era_post_v2_is_profile_v2_and_equals_v1_on_excerpt_parents() {
        let d = batch_dir(&[parent("a", "news", NEWS), parent("b", "blog", BLOG)]);
        let v2 = d.path().join("norm-v2.json");
        let v1 = d.path().join("norm-v1.json");
        write_prior_v2(d.path(), &v2, "train2").unwrap();
        write_prior(d.path(), &v1, DEFAULT_SEED).unwrap();
        let (post2, post1) = (EraPost::load(&v2, d.path()).unwrap(), EraPost::load(&v1, d.path()).unwrap());
        let child = "The ferry's horn sounded twice as it left the pier.  \nNobody on the deck said a word about the \"storm\" coming in from the west -- not even the captain.\n\nThe passengers waited below. They talked about the weather and their plans for the evening ahead...";
        let s = NEWS.find("The boats").unwrap();
        let e = NEWS.find("the quay was empty.").unwrap() + "the quay was empty.".len();
        for span in [None, Some((s, e))] {
            let r = request("a", span);
            let o2 = post2.apply(&r, Some(NEWS), child);
            let o1 = post1.apply(&r, Some(NEWS), child);
            // NEWS shows its own quote, dash, range and ellipsis majorities, so
            // no keyed draw: the conventions and the child are v1's.
            assert_eq!(o2.record["conventions"], o1.record["conventions"]);
            assert_eq!(o2.text, o1.text, "{span:?}");
            assert_eq!(o2.reasons, o1.reasons, "{span:?}");
            assert_eq!(o2.record["normaliser"], NORM_ID);
            assert_eq!(o2.record["norm_sha256"], post2.norm_sha256.as_str());
            assert_eq!(o2.record["profile"], PROFILE_V2);
            assert_eq!(o2.record["population"], "train2");
            assert_eq!(o2.record["domain"], "news");
            assert_eq!(o2.record["source"]["class"], "excerpt");
            assert_eq!(o2.record["raw_response_sha256"], sha256_hex(child.as_bytes()).as_str());
            assert_eq!(o2.record["echo"]["rejected"], false);
            // The record is v2's child step with v1's ids added.
            let conv = resolve_v2(&counts(NEWS), &post2.v2.as_ref().unwrap().prior, "news", &post2.seed, &era_batch::doc_id_of("a", "mixed"));
            let c = child_v2(NEWS, span, &conv, child);
            assert_eq!(c.text, o2.text);
            assert_eq!(c.record["transfer"], o2.record["transfer"]);
        }
        // The echo screen is part of v2's acceptance: a verbatim mirror fails.
        let echo = post2.apply(&request("b", None), Some(BLOG), BLOG);
        assert!(echo.reasons.iter().any(|r| r == "echo_mirror_byte_identical"), "{:?}", echo.reasons);
        // A parent without a domain cell entry draws from the population.
        let plain = "Plain words only here and nothing else at all in this short line of text.";
        let o = post2.apply(&request("b", None), Some(BLOG), plain);
        assert_eq!(o.record["conventions"]["dash"]["source"], "keyed_prior_population");
    }

    #[test]
    fn mixed_span_structure_and_breaks_at_sentence_starts_only() {
        let parent = "Alpha one two three four five six. Beta one two three four five six.\n\nGamma one two three four five six. Delta one two three four five.";
        let s = parent.find("Beta").unwrap();
        let e = parent.find("Delta").unwrap() - 1;
        let st = Structure::span(parent, s, e);
        assert_eq!(st.pre_words, 7);
        assert_eq!(st.post_words, 6);
        assert_eq!(st.fractions.len(), 1);
        assert!(ends_with_abbreviation("He met Dr.") && ends_with_abbreviation("by J.") && !ends_with_abbreviation("He left."));
        assert!(starts_sentence("\u{201C}Yes") && !starts_sentence("and then"));
    }

    /// Synthetic prose of `n` sentences of `len` words (no corpus text).
    fn syn(seed: u64, n: usize, len: usize) -> String {
        const W: [&str; 16] = [
            "amber", "harbor", "lantern", "basalt", "quay", "delta", "ember", "garnet", "keeper", "fjord", "meadow", "juniper", "kestrel", "river", "orchid",
            "pebble",
        ];
        let mut st = seed;
        let mut out = Vec::new();
        for _ in 0..n {
            let ws: Vec<&str> = (0..len)
                .map(|_| {
                    st = st.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    W[((st >> 33) as usize) % W.len()]
                })
                .collect();
            let s = ws.join(" ");
            out.push(format!("{}{}.", s[..1].to_uppercase(), &s[1..]));
        }
        out.join(" ")
    }

    #[test]
    fn renorm_keeps_raw_text_only_under_profile_v2() {
        // B1c (BUILD-PLAN 10.3, ruling R1): a v1 renorm journal holds no
        // raw_text, as the recorded cohort-v3 journals hold none; profile v2
        // keeps the cleaned response its step read on every verdict it ran on.
        let long = format!("{}\n\n{}\n\n{}", syn(1, 6, 11), syn(2, 6, 12), syn(3, 5, 11));
        let parents = [parent("a", "news", NEWS), parent("l", "blog", &long)];
        let d = tempfile::tempdir().unwrap();
        let b = d.path();
        let rows: Vec<String> = parents.iter().map(|p| serde_json::to_string(p).unwrap()).collect();
        fs::write(b.join("parents.jsonl"), rows.join("\n") + "\n").unwrap();
        let s = NEWS.find("The boats").unwrap();
        let e = NEWS.find("the quay was empty.").unwrap() + "the quay was empty.".len();
        let mut span = request("a", Some((s, e)));
        span.target_words = 20;
        let mut mirror = request("l", None);
        mirror.target_words = grammar_a_pools::text::words(&long);
        let reqs = [span, mirror];
        fs::create_dir_all(b.join("requests")).unwrap();
        let lines: Vec<String> = reqs.iter().map(|r| serde_json::to_string(r).unwrap()).collect();
        fs::write(b.join("requests/g.jsonl"), lines.join("\n") + "\n").unwrap();
        store::write_json(&b.join("BATCH.json"), &json!({"schema": era_batch::BATCH_SCHEMA, "parents_sha256": file_sha256(&b.join("parents.jsonl")).unwrap(),
            "population": "train2", "request_files": {"g": {"sha256": file_sha256(&b.join("requests/g.jsonl")).unwrap()}}}))
        .unwrap();
        // the replies: a span rewrite with trailing spaces and a single break, and a mirror in one paragraph
        let replies = [
            "The boats were moored early that night  \nnobody wanted to cross the channel after dark, and the pier stayed empty.".to_string(),
            syn(9, 17, 11),
        ];
        let mut journal = Vec::new();
        for (r, reply) in reqs.iter().zip(&replies) {
            let body = json!({"choices":[{"message":{"content":reply},"finish_reason":"stop"}]});
            journal.push(json!({"kind":"intent","request_id":r.request_id,"item_id":r.item_id,"split":r.split,"generator":"g","attempt":0,"seed":1,"t":1,"body":{}}));
            journal.push(json!({"kind":"response","request_id":r.request_id,"item_id":r.item_id,"split":r.split,"generator":"g","attempt":0,"seed":1,
                "t_start":1,"t_end":2,"elapsed_ms":1,"status":200,"raw_body":body.to_string(),"verdict":{"accepted":true,"reasons":[],"text":reply,"diag":{}}}));
        }
        fs::create_dir_all(b.join("runs/g")).unwrap();
        let jl: Vec<String> = journal.iter().map(Value::to_string).collect();
        fs::write(b.join("runs/g/journal-1-w0.jsonl"), jl.join("\n") + "\n").unwrap();
        let tok = b.join("tokenizer.json");
        fs::write(&tok, r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#).unwrap();
        let (v1, v2) = (b.join("norm-v1.json"), b.join("norm-v2.json"));
        write_prior(b, &v1, DEFAULT_SEED).unwrap();
        write_prior_v2(b, &v2, "train2").unwrap();
        assert!(!EraPost::load(&v1, b).unwrap().is_profile_v2());
        assert!(EraPost::load(&v2, b).unwrap().is_profile_v2());
        let run = |profile: Profile, norm: &Path, out: &str| -> Vec<Value> {
            let args = RenormArgs { batch: b.to_path_buf(), norm: norm.to_path_buf(), profile, runs: b.join("runs"), requests: None, out: b.join(out), tokenizer: tok.clone() };
            renorm(&args).unwrap();
            let files = generation::journal_files(&b.join(out).join("g")).unwrap();
            assert_eq!(files.len(), 1);
            fs::read_to_string(&files[0]).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).filter(|v: &Value| v["kind"] == "response").collect()
        };
        let r1 = run(Profile::CohortV1, &v1, "out-v1");
        let r2 = run(Profile::CohortV2, &v2, "out-v2");
        assert_eq!((r1.len(), r2.len()), (2, 2));
        for (x, y) in r1.iter().zip(&r2) {
            let reply = replies[reqs.iter().position(|r| r.request_id == x["request_id"].as_str().unwrap()).unwrap()].as_str();
            // v1: the normaliser ran (its record, its text) and no raw_text
            assert_eq!(x["verdict"]["diag"]["post"]["normaliser"], NORM_ID);
            assert!(x["verdict"]["diag"]["post"].get("profile").is_none());
            assert!(x["verdict"]["text"].is_string());
            assert!(x["verdict"].get("raw_text").is_none(), "{}", x["verdict"]);
            // v2: its record and the cleaned response it read, whose digest the record names
            assert_eq!(y["verdict"]["diag"]["post"]["profile"], PROFILE_V2);
            assert_eq!(y["verdict"]["raw_text"].as_str(), Some(reply));
            assert_eq!(y["verdict"]["diag"]["post"]["raw_response_sha256"].as_str(), Some(sha256_hex(reply.as_bytes()).as_str()));
        }
        // the mirror passes v1's acceptance, so a v1 accepted verdict carries none either
        assert!(r1.iter().any(|x| x["verdict"]["accepted"] == true && x["verdict"].get("raw_text").is_none()), "{r1:?}");
    }
}
