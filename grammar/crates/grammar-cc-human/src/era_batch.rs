//! Common Crawl-parented mock batch for the pre-mainnet era gates.
//!
//! - `batch`: draws parents from the admitted pilot pool at a versioned
//!   launch share table (`shares`; era-v1 used `cc_launch_shares_v1`), with crawl years weighted by first-seen unique documents
//!   (`lookup-v1` frame, about 44% in 2020–2022) and at most one document per
//!   source group. Each parent takes one role: a human-only negative, the
//!   parent of a mixed rewrite, or the parent of a model-only mirror in one of
//!   three conditioning arms (topic and existence-by date; topic only; neither).
//!   It writes local generation requests in the grammar-a-positives request
//!   schema.
//! - `generate`: runs one generator's requests against a local llama-server,
//!   reusing the grammar-a-positives journals and acceptance checks, under a
//!   cohort generation profile (`era_norm::Profile`; the default
//!   `cc_cohort_profile_v1` post-processes every response with the
//!   parent-conditioned normaliser and form screen and applies the
//!   final-document length window).
//! - `assemble`: splices accepted rewrites into their parents, collects the
//!   mirrors, applies the profile's every-class step to every document and
//!   writes the mock cohort and the A request shards.
//!
//! Parents never see their mirrors' text and mirrors never see their parents'
//! text: a mirror prompt carries the parent's domain register, word count,
//! TF-IDF key terms and (dated arm) the month of its capture. Outputs hold
//! Common Crawl page text and generated text and belong in private, ignored
//! storage. Nothing here labels, filters or removes a pool document.
use crate::era_norm::{self, Profile};
use crate::shares::ShareTable;
use crate::measure::{self, PoolRow, crawl_year, file_sha256, read_jsonl, sha256_hex, write_jsonl};
use crate::store;
use anyhow::{Context, Result, bail, ensure};
use grammar_a_pools::splits::rank;
use grammar_a_positives::{diverse, finalize, generation, overlap, plan, plan::Request, span_ops, text as ptext};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

pub const BATCH_SCHEMA: &str = "slop_ninja_cc_era_batch_v1";
pub const PARENT_SCHEMA: &str = "slop_ninja_cc_era_parent_v1";
pub const DOC_SCHEMA: &str = "slop_ninja_cc_era_doc_v2";
pub const SPLIT: &str = "cc_era_mock";
pub const SCOPE_GROUP_REP: &str = "cc_era_group_rep_v1";
pub const SCOPE_SELECT: &str = "cc_era_select_v1";
pub const SCOPE_ROLE: &str = "cc_era_role_v1";
pub const SCOPE_GEN: &str = "cc_era_generator_v1";
pub const SCOPE_OP: &str = "cc_era_op_v1";


pub const HUMAN: &str = "human_only";
pub const MIXED: &str = "mixed";
pub const MIRROR_DATED: &str = "mirror_dated";
pub const MIRROR_TOPIC: &str = "mirror_topic";
pub const MIRROR_PLAIN: &str = "mirror_plain";
pub const ROLES: [&str; 5] = [HUMAN, MIXED, MIRROR_DATED, MIRROR_TOPIC, MIRROR_PLAIN];

pub const OP_SPAN_REWRITE: &str = "span_rewrite";
pub const MIXED_OPS: [&str; 4] = [span_ops::OP_PREFIX, span_ops::OP_MIDDLE_REWRITE, span_ops::OP_MIDDLE_INFILL, OP_SPAN_REWRITE];

pub const T_MIRROR_DATED: &str = "cc_era_mirror_dated_v1";
pub const T_MIRROR_TOPIC: &str = "cc_era_mirror_topic_v1";
pub const T_MIRROR_PLAIN: &str = "cc_era_mirror_plain_v1";
pub const T_SPAN_REWRITE: &str = "cc_era_span_rewrite_v1";
pub const MIRROR_TERMS: usize = 8;

/// Prompt register of a cohort domain.
pub fn register(domain: &str) -> &'static str {
    match domain {
        "news" => "a news article",
        "essays" => "a blog post or personal essay",
        "technical" => "a passage of technical documentation or a tutorial",
        "reviews" => "a review of a product, service, place or work",
        "forum" => "a post in an online discussion forum",
        "legal" => "a passage of legal text, such as a court opinion, statute or legal notice",
        "speech" => "a speech or a transcript of spoken remarks",
        "fiction" => "a passage of fiction",
        "email" => "a press release or a mailing-list message",
        _ => "a passage from a web page",
    }
}

const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// Existence-by month of a capture date `YYYY-MM-DD...`, e.g. "May 2013".
pub fn existence_by(warc_date: &str) -> Result<String> {
    let y = warc_date.get(..4).context("warc year")?;
    let m: usize = warc_date.get(5..7).context("warc month")?.parse()?;
    ensure!((1..=12).contains(&m), "warc month {m}");
    Ok(format!("{} {y}", MONTHS[m - 1]))
}

/// Model-only mirror prompt for one conditioning arm.
pub fn mirror_prompt(role: &str, domain: &str, words: usize, terms: &[String], date: &str) -> String {
    let topic = if role == MIRROR_PLAIN || terms.is_empty() { String::new() } else { format!(" Cover these key terms: {}.", terms.join(", ")) };
    let era = if role == MIRROR_DATED {
        format!(
            " Write it as it would have been written and published in {date}: mention nothing that happened, appeared or became common after {date}, and do not add a date unless the text would naturally carry one."
        )
    } else {
        String::new()
    };
    format!(
        "Write {} in English, about {words} words long.{topic}{era} Write natural, original prose. Return only the passage, with no title, heading, preface, analysis, or explanation.",
        register(domain)
    )
}

fn unit_of(r: u64) -> f64 {
    (r as f64 + 0.5) / 18446744073709551616.0
}

/// Largest-remainder allocation of `total` over `shares` (ties to the lower index).
pub fn allocate(total: usize, shares: &[f64]) -> Vec<usize> {
    let s: f64 = shares.iter().sum();
    let raw: Vec<f64> = shares.iter().map(|x| x / s * total as f64).collect();
    let mut n: Vec<usize> = raw.iter().map(|x| x.floor() as usize).collect();
    let mut rem: Vec<(f64, usize)> = raw.iter().enumerate().map(|(i, x)| (x - x.floor(), i)).collect();
    rem.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let short = total - n.iter().sum::<usize>();
    for &(_, i) in rem.iter().take(short) {
        n[i] += 1;
    }
    n
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Parent {
    pub schema: String,
    pub id: String,
    pub role: String,
    /// Member of the 20/55/25 mock cohort (every human-only and mixed parent; the
    /// first `cohort_model_only` dated mirrors in role-rank order).
    pub cohort_member: bool,
    pub domain: String,
    pub year: u16,
    pub crawl: String,
    pub warc_date: String,
    pub existence_by: String,
    pub era: String,
    pub group: String,
    pub host: String,
    pub url: String,
    pub text_variant: String,
    pub text: String,
    pub text_sha256: String,
    pub pool_text_sha256: String,
    pub words: usize,
    pub generator: Option<String>,
    pub op: Option<String>,
    pub item_id: Option<String>,
    pub request_id: Option<String>,
    pub source_span: Option<(usize, usize)>,
    pub terms: Vec<String>,
    pub notes: Vec<String>,
}

pub struct BatchArgs {
    pub pool: PathBuf,
    pub frame: PathBuf,
    pub out: PathBuf,
    pub seed: String,
    pub counts: [usize; 5],
    pub cohort_model_only: usize,
    pub generators: Vec<String>,
    /// Launch share table of human parents.
    pub shares: ShareTable,
}

/// Year weights: first-seen unique documents per crawl year (lookup-v1 frame).
pub fn year_weights(frame: &Path) -> Result<BTreeMap<u16, f64>> {
    let v: Value = store::read_json(frame)?;
    let mut out = BTreeMap::new();
    for (y, x) in v["per_year"].as_object().context("per_year")? {
        out.insert(y.parse()?, x["first_seen_unique"].as_f64().context("first_seen_unique")?);
    }
    let s: f64 = out.values().sum();
    ensure!(s > 0.0, "empty year weights");
    for x in out.values_mut() {
        *x /= s;
    }
    Ok(out)
}

/// (template id, prompt, replaced byte span, target words, operation details)
type MixedRequest = (String, String, (usize, usize), usize, Value);

fn mixed_request(p: &Parent, op: &str, idf: &ptext::Idf, stop: &HashSet<&'static str>) -> Option<MixedRequest> {
    let item_id = p.item_id.as_deref().unwrap();
    let text = &p.text;
    let sents = ptext::sentences(text);
    let cum = plan::prefix_words(&sents, text);
    let n = sents.len();
    if cum[n] != p.words {
        return None;
    }
    let reg = register(&p.domain);
    if op == span_ops::OP_PREFIX {
        let target = span_ops::draw_bp("prefix_bp", item_id, span_ops::PREFIX_MIN_BP, span_ops::PREFIX_MAX_BP);
        let j = span_ops::prefix_cut(&cum, target)?;
        let span = plan::byte_span(&sents, (j, n));
        let removed = cum[n] - cum[j];
        let prompt = span_ops::prefix_prompt(reg, text[..span.0].trim_end(), removed);
        let st = json!({"target_prefix_bp":target,"kept_prefix_words":cum[j],"replaced_words":removed});
        return Some((span_ops::PREFIX_TEMPLATE_ID.into(), prompt, span, removed, st));
    }
    let target = span_ops::draw_bp("middle_bp", item_id, span_ops::MIDDLE_MIN_BP, span_ops::MIDDLE_MAX_BP);
    let center = span_ops::draw_bp("middle_center", item_id, span_ops::MIDDLE_CENTER_MIN_BP, span_ops::MIDDLE_CENTER_MAX_BP);
    let run = span_ops::middle_run(&cum, target, center)?;
    let span = plan::byte_span(&sents, run);
    let removed = cum[run.1] - cum[run.0];
    let before = text[..span.0].trim_end();
    let after = text[span.1..].trim_start();
    let span_text = &text[span.0..span.1];
    let st = json!({"target_run_bp":target,"target_center_bp":center,"run_sentences":[run.0, run.1],"replaced_words":removed});
    Some(match op {
        span_ops::OP_MIDDLE_REWRITE => {
            (span_ops::MIDDLE_TEMPLATE_ID.into(), span_ops::middle_rewrite_prompt(reg, before, span_text, after, removed), span, removed, st)
        }
        span_ops::OP_MIDDLE_INFILL => {
            let terms = ptext::key_terms(span_text, idf, stop, span_ops::INFILL_TERMS);
            (span_ops::MIDDLE_TEMPLATE_ID.into(), span_ops::middle_infill_prompt(reg, before, after, removed, &terms), span, removed, st)
        }
        _ => (T_SPAN_REWRITE.into(), plan::rewrite_prompt(span_text), span, removed, st),
    })
}

pub fn batch(a: &BatchArgs) -> Result<Value> {
    ensure!(!a.out.join("BATCH.json").exists(), "batch already written: {}", a.out.display());
    ensure!(!a.generators.is_empty(), "no generators");
    ensure!(a.cohort_model_only <= a.counts[2], "cohort model-only above dated mirrors");
    let weights = year_weights(&a.frame)?;
    let pool = measure::read_pool(&a.pool)?;
    let launch = a.shares.fractions();
    let doms: Vec<&str> = launch.iter().map(|x| x.0).collect();
    let stop = ptext::stopwords();
    // Per-domain IDF over the whole admitted pool of that domain (topic terms only).
    let mut idf: HashMap<&str, ptext::Idf> = doms.iter().map(|d| (*d, ptext::Idf::default())).collect();
    for r in &pool {
        if let Some(t) = idf.get_mut(r.domain.as_str()) {
            t.add(r.scored_text().0, &stop);
        }
    }
    // One representative per (domain, group): the lowest keyed rank.
    let mut rep: HashMap<(&str, &str), (u64, usize)> = HashMap::new();
    for (i, r) in pool.iter().enumerate() {
        if !doms.contains(&r.domain.as_str()) {
            continue;
        }
        let y = crawl_year(&r.crawl)?;
        if !weights.contains_key(&y) {
            continue;
        }
        let k = rank(&a.seed, SCOPE_GROUP_REP, &r.id);
        let e = rep.entry((r.domain.as_str(), r.group.as_str())).or_insert((k, i));
        if k < e.0 {
            *e = (k, i);
        }
    }
    let mut cands: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (_, (_, i)) in rep {
        cands.entry(pool[i].domain.as_str()).or_default().push(i);
    }
    let shares: Vec<f64> = launch.iter().map(|x| x.1).collect();
    let per_role: Vec<Vec<usize>> = a.counts.iter().map(|&n| allocate(n, &shares)).collect();
    let cohort_mo = allocate(a.cohort_model_only, &shares);
    let mut parents: Vec<Parent> = Vec::new();
    let mut avail = BTreeMap::new();
    for (di, d) in doms.iter().enumerate() {
        let need: usize = per_role.iter().map(|v| v[di]).sum();
        let c = cands.get(d).cloned().unwrap_or_default();
        let mut by_year: BTreeMap<u16, usize> = BTreeMap::new();
        for &i in &c {
            *by_year.entry(crawl_year(&pool[i].crawl)?).or_default() += 1;
        }
        avail.insert(d.to_string(), json!({"group_representatives":c.len(),"by_year":by_year,"needed":need}));
        ensure!(c.len() >= need, "domain {d}: {} group representatives for {need} parents", c.len());
        // Efraimidis–Spirakis weighted draw without replacement: weight W_y / C_{d,y}
        // makes the expected year mix follow the frame's first-seen unique documents.
        let mut keyed: Vec<(f64, &str, usize)> = c
            .iter()
            .map(|&i| {
                let y = crawl_year(&pool[i].crawl).unwrap();
                let w = weights[&y] / by_year[&y] as f64;
                (unit_of(rank(&a.seed, SCOPE_SELECT, &pool[i].id)).ln() / w, pool[i].id.as_str(), i)
            })
            .collect();
        keyed.sort_by(|x, y| y.0.total_cmp(&x.0).then(x.1.cmp(y.1)));
        let mut chosen: Vec<usize> = keyed.iter().take(need).map(|x| x.2).collect();
        chosen.sort_by_key(|&i| (rank(&a.seed, SCOPE_ROLE, &pool[i].id), pool[i].id.clone()));
        let mut it = chosen.into_iter();
        for (ri, role) in ROLES.iter().enumerate() {
            for k in 0..per_role[ri][di] {
                let r: &PoolRow = &pool[it.next().unwrap()];
                let (text, variant) = r.scored_text();
                let year = crawl_year(&r.crawl)?;
                parents.push(Parent {
                    schema: PARENT_SCHEMA.into(),
                    id: r.id.clone(),
                    role: role.to_string(),
                    cohort_member: *role == HUMAN || *role == MIXED || (*role == MIRROR_DATED && k < cohort_mo[di]),
                    domain: d.to_string(),
                    year,
                    crawl: r.crawl.clone(),
                    warc_date: r.warc_date.clone(),
                    existence_by: existence_by(&r.warc_date)?,
                    era: r.era.clone(),
                    group: r.group.clone(),
                    host: r.host.clone(),
                    url: r.url.clone(),
                    text_variant: variant.into(),
                    text: text.to_string(),
                    text_sha256: sha256_hex(text.as_bytes()),
                    pool_text_sha256: r.text_sha256.clone(),
                    words: ptext::words(text),
                    generator: None,
                    op: None,
                    item_id: None,
                    request_id: None,
                    source_span: None,
                    terms: vec![],
                    notes: vec![],
                });
            }
        }
    }
    // Generators: round robin within each role in generator-rank order; mixed
    // operations: round robin within the mixed role in operation-rank order.
    for role in &ROLES[1..] {
        let mut idx: Vec<usize> = (0..parents.len()).filter(|&i| parents[i].role == *role).collect();
        idx.sort_by_key(|&i| (rank(&a.seed, SCOPE_GEN, &parents[i].id), parents[i].id.clone()));
        for (k, &i) in idx.iter().enumerate() {
            parents[i].generator = Some(a.generators[k % a.generators.len()].clone());
        }
    }
    let mut mixed_idx: Vec<usize> = (0..parents.len()).filter(|&i| parents[i].role == MIXED).collect();
    mixed_idx.sort_by_key(|&i| (rank(&a.seed, SCOPE_OP, &parents[i].id), parents[i].id.clone()));
    for (k, &i) in mixed_idx.iter().enumerate() {
        parents[i].op = Some(MIXED_OPS[k % MIXED_OPS.len()].into());
    }
    let mut requests: BTreeMap<String, Vec<Request>> = a.generators.iter().map(|g| (g.clone(), Vec::new())).collect();
    let mut failures = Vec::new();
    for p in parents.iter_mut() {
        if p.role == HUMAN {
            continue;
        }
        let g = p.generator.clone().unwrap();
        let item_id = format!("ccera-{}", &sha256_hex(format!("{}:{}:{}", SPLIT, p.id, p.role).as_bytes())[..24]);
        p.item_id = Some(item_id.clone());
        let decoding = diverse::sample_decoding(&item_id, &g, false);
        let (tmpl, prompt, span, target, op) = if p.role == MIXED {
            let want = p.op.clone().unwrap();
            let start = MIXED_OPS.iter().position(|o| *o == want).unwrap();
            let mut got = None;
            for k in 0..MIXED_OPS.len() {
                let op = MIXED_OPS[(start + k) % MIXED_OPS.len()];
                if let Some(x) = mixed_request(p, op, &idf[p.domain.as_str()], &stop) {
                    if k > 0 {
                        p.notes.push(format!("op_fallback:{want}->{op}"));
                    }
                    got = Some((op, x));
                    break;
                }
            }
            let Some((op, (tmpl, prompt, span, target, _st))) = got else {
                failures.push(json!({"parent_id":p.id,"role":p.role,"reason":"no_mixed_operation_fits","words":p.words}));
                p.notes.push("no_request".into());
                continue;
            };
            p.op = Some(op.into());
            p.source_span = Some(span);
            (tmpl, prompt, Some(span), target, Some(op.to_string()))
        } else {
            let terms = if p.role == MIRROR_PLAIN { vec![] } else { ptext::key_terms(&p.text, &idf[p.domain.as_str()], &stop, MIRROR_TERMS) };
            let tmpl = match p.role.as_str() {
                MIRROR_DATED => T_MIRROR_DATED,
                MIRROR_TOPIC => T_MIRROR_TOPIC,
                _ => T_MIRROR_PLAIN,
            };
            let prompt = mirror_prompt(&p.role, &p.domain, p.words, &terms, &p.existence_by);
            p.terms = terms;
            (tmpl.to_string(), prompt, None, p.words, None)
        };
        let rs = plan::request_sha(&g, "chat", &format!("{tmpl}:{}", op.clone().unwrap_or_default()), &prompt, &decoding);
        let req = Request {
            request_id: format!("ccera-req-{}", &sha256_hex(format!("{item_id}:0").as_bytes())[..24]),
            item_id,
            parent_id: p.id.clone(),
            split: SPLIT.into(),
            generator: g.clone(),
            endpoint: "chat".into(),
            template_id: tmpl,
            prompt,
            span_index: 0,
            source_span: span,
            target_words: target,
            cut_budget: None,
            decoding,
            seed: plan::seed_of(&rs),
            request_sha256: rs,
            op,
            cohort: Some(p.role.clone()),
            attempt_limit: None,
        };
        p.request_id = Some(req.request_id.clone());
        requests.get_mut(&g).unwrap().push(req);
    }
    fs::create_dir_all(a.out.join("requests"))?;
    let parents_sha = write_jsonl(&a.out.join("parents.jsonl"), &parents)?;
    let mut files = BTreeMap::new();
    for (g, rs) in &requests {
        let sha = write_jsonl(&a.out.join("requests").join(format!("{g}.jsonl")), rs)?;
        files.insert(g.clone(), json!({"requests":rs.len(),"sha256":sha}));
    }
    let mut counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut years: BTreeMap<String, BTreeMap<u16, usize>> = BTreeMap::new();
    let mut gens: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut ops: BTreeMap<String, usize> = BTreeMap::new();
    for p in &parents {
        *counts.entry(p.role.clone()).or_default().entry(p.domain.clone()).or_default() += 1;
        *years.entry(p.role.clone()).or_default().entry(p.year).or_default() += 1;
        if let Some(g) = &p.generator {
            *gens.entry(p.role.clone()).or_default().entry(g.clone()).or_default() += 1;
        }
        if let Some(o) = &p.op {
            *ops.entry(o.clone()).or_default() += 1;
        }
    }
    let meta = json!({
        "schema": BATCH_SCHEMA, "seed": a.seed, "split": SPLIT,
        "inputs": {"pool": a.pool, "pool_sha256": file_sha256(&a.pool)?, "frame": a.frame, "frame_sha256": file_sha256(&a.frame)?},
        "rules": {
            "domains": launch.iter().map(|x| json!({"domain":x.0,"share":x.1})).collect::<Vec<_>>(),
            "share_table": a.shares.id,
            "year_weights": weights,
            "selection": "per launch domain, one representative per source group (lowest rank in group-rep scope); Efraimidis-Spirakis draw without replacement with weight W_year / C_domain,year; roles in role-rank order (human_only, mixed, mirror_dated, mirror_topic, mirror_plain), each role allocated over domains by largest remainder at the launch shares",
            "cohort": "human_only + mixed + the first cohort_model_only dated mirrors per domain allocation (20/55/25)",
            "generators": "round robin within each role in generator-rank order",
            "mixed_ops": MIXED_OPS, "mixed_op_rule": "round robin in op-rank order; a passage that cannot host its op falls back to the next op",
            "mirror_terms": format!("{} TF-IDF key terms of the parent ({}), per-domain IDF over the admitted pool", MIRROR_TERMS, ptext::TERMS_ID),
            "mirror_arms": {"mirror_dated":"register, words, key terms and existence-by month (capture month)","mirror_topic":"register, words and key terms","mirror_plain":"register and words"},
            "decoding": "grammar-a-positives plan-v2 sampler (diverse::sample_decoding)",
            "mirror_prompt_dated_example": mirror_prompt(MIRROR_DATED, "news", 200, &["alpha".into(), "beta".into()], "May 2013"),
        },
        "counts": {"requested": a.counts, "cohort_model_only": a.cohort_model_only, "by_role_domain": counts, "by_role_year": years,
            "by_role_generator": gens, "mixed_ops": ops},
        "availability": avail, "plan_failures": failures,
        "parents_sha256": parents_sha, "request_files": files,
    });
    store::write_json(&a.out.join("BATCH.json"), &meta)?;
    Ok(json!({"parents": parents.len(), "requests": files, "plan_failures": failures.len()}))
}

pub fn read_parents(batch: &Path) -> Result<Vec<Parent>> {
    let meta: Value = store::read_json(&batch.join("BATCH.json"))?;
    let p = batch.join("parents.jsonl");
    ensure!(meta["parents_sha256"] == file_sha256(&p)?.as_str(), "parents.jsonl changed");
    store::rows_plain(&p)
}

pub struct GenArgs {
    pub batch: PathBuf,
    /// Directory holding the request set: `BATCH.json` or `TOPUP.json` and `requests/` (default: the batch).
    pub requests: Option<PathBuf>,
    pub runs: PathBuf,
    pub generator: String,
    pub config: PathBuf,
    pub endpoint: String,
    pub tokenizer: PathBuf,
    pub workers: usize,
    pub limit: Option<usize>,
    /// Normaliser prior (`era_norm`), required by every normalising profile.
    pub norm: Option<PathBuf>,
    /// Cohort generation profile.
    pub profile: Profile,
}

pub fn generate(a: &GenArgs) -> Result<Value> {
    // The profile takes the prior it needs (a v2 prior only for profile v2).
    era_norm::check_profile_prior(a.profile, a.norm.as_deref())?;
    let set = a.requests.clone().unwrap_or_else(|| a.batch.clone());
    let meta_path = if set.join("TOPUP.json").exists() { set.join("TOPUP.json") } else { set.join("BATCH.json") };
    let meta: Value = store::read_json(&meta_path)?;
    if meta_path.ends_with("TOPUP.json") {
        ensure!(meta["batch_sha256"] == file_sha256(&a.batch.join("BATCH.json"))?.as_str(), "top-up belongs to another batch");
    }
    let rp = set.join("requests").join(format!("{}.jsonl", a.generator));
    ensure!(meta["request_files"][&a.generator]["sha256"] == file_sha256(&rp)?.as_str(), "request file changed");
    let reqs: Vec<Request> = read_jsonl(&rp)?.into_iter().map(serde_json::from_value).collect::<Result<_, _>>()?;
    let cfg_bytes = fs::read(&a.config)?;
    let cfg: Value = serde_json::from_slice(&cfg_bytes)?;
    ensure!(cfg["key"] == a.generator.as_str(), "config key mismatch");
    let endpoint = a.endpoint.trim_end_matches('/').to_owned();
    let client = generation::client()?;
    let props: Value = client.get(format!("{endpoint}/props")).send()?.error_for_status()?.json()?;
    ensure!(props["model_path"] == cfg["model_path"], "server model path mismatch: {}", props["model_path"]);
    let models: Value = client.get(format!("{endpoint}/v1/models")).send()?.error_for_status()?.json()?;
    ensure!(models["data"].as_array().is_some_and(|v| v.iter().any(|m| m["id"] == cfg["alias"])), "server alias mismatch");
    let out = a.runs.join(&a.generator);
    fs::create_dir_all(&out)?;
    let t0 = generation::now_ms();
    store::write_json(&out.join(format!("server-{t0}.json")), &json!({"props":props,"models":models,"config":cfg,
        "config_sha256":sha256_hex(&cfg_bytes),"chat_template_sha256":props["chat_template"].as_str().map(|t| sha256_hex(t.as_bytes()))}))?;
    let parents: HashMap<String, String> = read_parents(&a.batch)?.into_iter().map(|p| (p.id, p.text)).collect();
    let states = generation::load_states(std::slice::from_ref(&out))?;
    let ov = overlap::Overlap::new(overlap::PriorIndex::empty());
    let mut preloaded = 0;
    for st in states.values() {
        if let Some(v) = st.accepted() {
            ov.add_generated(v["verdict"]["text"].as_str().unwrap_or(""));
            preloaded += 1;
        }
    }
    let mut pending = Vec::new();
    for r in reqs {
        let st = states.get(&r.request_id);
        if st.is_some_and(|s| s.done()) {
            continue;
        }
        let first = st.map_or(0, |s| s.used());
        pending.push((r, first));
    }
    let before = pending.len();
    if let Some(l) = a.limit {
        pending.truncate(l);
    }
    let mut tok = tokenizers::Tokenizer::from_file(&a.tokenizer).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tok.with_truncation(None).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tok.with_padding(None);
    let n = pending.len();
    eprintln!("{}: {before} pending, running {n}, preloaded {preloaded}", a.generator);
    let post = match &a.norm {
        Some(p) => Some(Arc::new(era_norm::EraPost::load(p, &a.batch)?)),
        None => None,
    };
    let norm_sha = post.as_ref().map(|p| p.norm_sha256.clone());
    let ctx = Arc::new(generation::Ctx {
        endpoint,
        alias: cfg["alias"].as_str().context("alias")?.to_owned(),
        config: cfg.clone(),
        tokenizer: tok,
        overlap: ov,
        parents,
        client,
        post: post.map(|p| p as Arc<dyn generation::PostProcess>),
        doc_words: a.profile.window(),
    });
    let stats = generation::run(ctx, pending, &out, a.workers)?;
    let rec = json!({"generator":a.generator,"t0":t0,"t_end":generation::now_ms(),"pending_before":before,"ran":n,"stats":stats,
        "profile":a.profile.record(),"normaliser":norm_sha.as_ref().map(|_| era_norm::NORM_ID),"norm_sha256":norm_sha,
        "tokenizer_sha256":file_sha256(&a.tokenizer)?,"batch_sha256":file_sha256(&a.batch.join("BATCH.json"))?,"request_set":meta_path,
        "binary":std::env::current_exe()?.display().to_string(),"journal_sync_policy":generation::JOURNAL_SYNC_POLICY});
    store::write_json(&out.join(format!("run-{t0}.json")), &rec)?;
    Ok(rec)
}


fn run_states(runs: &Path) -> Result<HashMap<String, generation::ReqState>> {
    let mut dirs = Vec::new();
    if runs.exists() {
        for e in fs::read_dir(runs)? {
            let p = e?.path();
            if p.is_dir() {
                dirs.push(p);
            }
        }
    }
    dirs.sort();
    generation::load_states(&dirs)
}

/// One top-up round: every mixed or mirror request whose attempts are spent
/// without an accepted response is re-issued once, with the same prompt and
/// span, to the next generator in `generators` (cyclic, never the same one),
/// with that generator's sampled decoding. A request that has not run is
/// refused, so a top-up never races its primary generation.
pub fn topup(batch: &Path, runs: &Path, out: &Path, generators: &[String]) -> Result<Value> {
    ensure!(!out.join("TOPUP.json").exists(), "top-up already written: {}", out.display());
    ensure!(generators.len() >= 2, "a top-up needs at least two generators");
    let meta: Value = store::read_json(&batch.join("BATCH.json"))?;
    let parents = read_parents(batch)?;
    let mut orig: HashMap<String, Request> = HashMap::new();
    for g in meta["request_files"].as_object().context("request_files")?.keys() {
        let rp = batch.join("requests").join(format!("{g}.jsonl"));
        ensure!(meta["request_files"][g]["sha256"] == file_sha256(&rp)?.as_str(), "request file changed: {g}");
        for v in read_jsonl(&rp)? {
            let r: Request = serde_json::from_value(v)?;
            orig.insert(r.request_id.clone(), r);
        }
    }
    let states = run_states(runs)?;
    let mut reqs: BTreeMap<String, Vec<Request>> = generators.iter().map(|g| (g.clone(), Vec::new())).collect();
    let mut items = Vec::new();
    for p in &parents {
        let Some(rid) = &p.request_id else { continue };
        let st = states.get(rid).with_context(|| format!("request {rid} has not run"))?;
        ensure!(st.done(), "request {rid} is still pending");
        if st.accepted().is_some() {
            continue;
        }
        let r = &orig[rid];
        let k = generators.iter().position(|g| *g == r.generator).context("generator not in the top-up list")?;
        let g = generators[(k + 1) % generators.len()].clone();
        let item_id = format!("{}-t1", r.item_id);
        let decoding = diverse::sample_decoding(&item_id, &g, false);
        let tmpl = format!("{}:{}", r.template_id, r.op.clone().unwrap_or_default());
        let rs = plan::request_sha(&g, "chat", &tmpl, &r.prompt, &decoding);
        let mut n = r.clone();
        n.request_id = format!("ccera-req-{}", &sha256_hex(format!("{item_id}:0").as_bytes())[..24]);
        n.item_id = item_id;
        n.generator = g.clone();
        n.decoding = decoding;
        n.seed = plan::seed_of(&rs);
        n.request_sha256 = rs;
        items.push(json!({"parent_id":p.id,"role":p.role,"op":p.op,"from_generator":r.generator,"generator":g,"request_id":n.request_id,"primary_request_id":rid}));
        reqs.get_mut(&g).unwrap().push(n);
    }
    fs::create_dir_all(out.join("requests"))?;
    let mut files = BTreeMap::new();
    for (g, rs) in &reqs {
        let sha = write_jsonl(&out.join("requests").join(format!("{g}.jsonl")), rs)?;
        files.insert(g.clone(), json!({"requests":rs.len(),"sha256":sha}));
    }
    let rec = json!({"schema":"slop_ninja_cc_era_topup_v1","batch_sha256":file_sha256(&batch.join("BATCH.json"))?,
        "rule":"re-issue each exhausted unaccepted request once to the next generator (cyclic) with the same prompt and span and that generator's sampled decoding",
        "generators":generators,"items":items,"request_files":files});
    store::write_json(&out.join("TOPUP.json"), &rec)?;
    Ok(json!({"items":rec["items"].as_array().map_or(0, |v| v.len()),"request_files":files}))
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Doc {
    pub schema: String,
    pub id: String,
    pub parent_id: String,
    pub role: String,
    pub cohort_member: bool,
    /// human_only | mixed | model_only
    pub origin: String,
    pub y: u8,
    pub domain: String,
    pub year: u16,
    pub group: String,
    pub generator: Option<String>,
    pub op: Option<String>,
    /// Realised final-word AI share.
    pub share: f64,
    pub ai_spans: Vec<(usize, usize)>,
    pub words: usize,
    pub text: String,
    pub text_sha256: String,
    pub request_id: Option<String>,
    pub attempt: Option<u64>,
    /// Accepted from the top-up round (generator then names the top-up generator).
    #[serde(default)]
    pub topup: bool,
    /// Normaliser record (`era_norm`): the child's post-processing record and
    /// the every-class layout and paragraph-screen record. Absent on raw cohorts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub norm: Option<Value>,
    /// R-form documents only (protocol A2 §7.2, `rform-cohort.jsonl`):
    /// `r_form`. Absent on cohort documents, whose bytes it leaves unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<String>,
    /// R-form documents only: `raw_twin` or `rform_screen_fail_v1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stratum: Option<String>,
    /// A raw twin's cohort document (its form-sensitivity pair).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub twin_of: Option<String>,
    /// R-form documents only: the raw-response digests and the rule; no text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rform: Option<Value>,
}

pub fn doc_id(p: &Parent) -> String {
    doc_id_of(&p.id, &p.role)
}

/// Document id of a parent's child (or of a human-only parent itself) in one role.
pub fn doc_id_of(parent_id: &str, role: &str) -> String {
    format!("ccera-doc-{}", &sha256_hex(format!("{SPLIT}:{parent_id}:{role}").as_bytes())[..24])
}

/// Length summary of cohort members by origin: word quantiles, the share
/// outside the profile window and the two-sample Kolmogorov-Smirnov
/// distance of each origin's word counts from the human-only documents'.
pub fn length_summary(docs: &[Doc]) -> Value {
    let mut by: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for d in docs.iter().filter(|d| d.cohort_member) {
        by.entry(d.origin.as_str()).or_default().push(d.words);
    }
    for v in by.values_mut() {
        v.sort_unstable();
    }
    let q = |v: &[usize], p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    let ks = |a: &[usize], b: &[usize]| -> f64 {
        let (mut i, mut j, mut d) = (0usize, 0usize, 0f64);
        while i < a.len() && j < b.len() {
            let x = a[i].min(b[j]);
            while i < a.len() && a[i] == x {
                i += 1;
            }
            while j < b.len() && b[j] == x {
                j += 1;
            }
            d = d.max((i as f64 / a.len() as f64 - j as f64 / b.len() as f64).abs());
        }
        d
    };
    let human = by.get(HUMAN).cloned().unwrap_or_default();
    let (lo, hi) = era_norm::LENGTH_WINDOW;
    let m: BTreeMap<String, Value> = by
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(o, v)| {
            (o.to_string(), json!({"n": v.len(), "words_p05_p25_p50_p75_p95": [q(v, 0.05), q(v, 0.25), q(v, 0.5), q(v, 0.75), q(v, 0.95)],
                "outside_window": v.iter().filter(|&&w| w < lo || w > hi).count(),
                "ks_vs_human_only": if human.is_empty() { Value::Null } else { json!(ks(v, &human)) }}))
        })
        .collect();
    json!(m)
}

/// The raw twin's rule in the CC cohort, as its document states it.
pub const RFORM_TWIN_RULE: &str = "protocol A2 section 7.2 raw twin: the accepted response's raw child (the verdict's raw_text, read exactly and refused unless its SHA-256 is the verdict's profile v2 raw_response_sha256) spliced into the same parent (the parent must match text_sha256; a mirror is its raw child, a mixed child by generation::assemble at the parent's span), with the cohort document's labels; it exists exactly when its cohort document passes the every-class step, whose verdict it takes, and goes through no step itself";
/// The screen-fail stratum's rule in the CC cohort.
pub const RFORM_SCREEN_FAIL_RULE: &str = "protocol A2 section 7.2 rform_screen_fail_v1 as read by amendment 19 items 19.1, 19.3 and 19.4: a child whose first raw response failed the form screen only; the first raw response is the primary request's lowest attempt whose response verdict carries none of transport_error, http_status_<code> or no_content, accepted or not (a response rejected before the profile step, or with empty content, is one), else, when the primary has none, the top-up request's; form screen only: each reason one of the post record's form_reasons and a form_line_, form_paragraph_ or form_frame_ code; that raw response spliced into the parent (a mirror is the raw response), window-checked at assembly (cc_final_length_window_v1) and through no other step; a window or assembly failure leaves it out; no cohort twin; the R-form documents of one assembly are unique by text, a duplicate refusing it (item 19.6)";

/// The splice of a raw child into its parent: a mixed child at the parent's
/// span (`generation::assemble`), a mirror as itself.
fn rform_splice(p: &Parent, raw: &str) -> Result<finalize::Spliced> {
    if p.role == MIXED {
        let span = p.source_span.context("mixed span")?;
        generation::assemble(&p.text, &[span], &[raw])
    } else {
        finalize::rebuild(&p.text, "mirror", &[], &[raw])
    }
}

fn check_parent_text(p: &Parent, id: &str) -> Result<()> {
    ensure!(sha256_hex(p.text.as_bytes()) == p.text_sha256, "{id}: the parent text does not match text_sha256");
    Ok(())
}

/// The raw twin of cohort document `d` (protocol A2 §7.2): `raw`, the
/// accepted verdict's checked raw child with digest `digest`, spliced into
/// the same parent, with `d`'s labels (see `assemble`).
fn rform_twin(p: &Parent, d: &Doc, raw: &str, digest: &str) -> Result<Doc> {
    check_parent_text(p, &d.id)?;
    let (text, ai_spans, share) = rform_splice(p, raw).with_context(|| format!("{}: raw twin", d.id))?;
    let same = grammar_a_pools::text::folded_tokens(&text) == grammar_a_pools::text::folded_tokens(&d.text);
    let mut t = d.clone();
    t.id = format!("{}{}", finalize::RAW_TWIN_ID_PREFIX, d.id);
    t.words = ptext::words(&text);
    t.text_sha256 = sha256_hex(text.as_bytes());
    t.text = text;
    t.ai_spans = ai_spans;
    t.share = share;
    t.norm = None;
    t.form = Some(finalize::R_FORM.into());
    t.stratum = Some(finalize::RAW_TWIN_STRATUM.into());
    t.twin_of = Some(d.id.clone());
    t.rform = Some(json!({"rule": RFORM_TWIN_RULE, "raw_response_sha256": digest, "parent_text_sha256": p.text_sha256,
        "cohort_text_sha256": d.text_sha256, "folded_tokens_equal_cohort_document": same,
        "window": "the cohort document's every-class verdict (protocol A2 section 7.2)"}));
    Ok(t)
}

/// One of a child's request journals: (state, generator, request id, top-up).
type ChildJournal<'a> = (&'a generation::ReqState, Option<String>, Option<String>, bool);

/// The screen-fail stratum document of a parent's child (protocol A2 §7.2
/// as read by amendment 19 items 19.1 and 19.3; see
/// `assemble`): `Ok(None)` when the child is not in the stratum,
/// `Some(Ok(doc))` inside the window, `Some(Err(note))` when the window or
/// the splice leaves it out (the note holds no text). `d` gives the labels
/// the document shares with the child (id, parent, role, cohort membership,
/// domain, year, group, operation); `cands` are the child's journals in
/// order (the primary request, then the top-up). The child's first raw
/// response is the first journal's that has one
/// (`finalize::first_raw_response`), so the top-up's counts only when the
/// primary returned no content in any attempt or never ran.
fn rform_screen_fail<'a>(p: &Parent, d: &Doc, cands: impl Iterator<Item = ChildJournal<'a>>) -> Result<Option<Result<Doc, Value>>> {
    let first = cands.filter_map(|(st, g, rid, topup)| finalize::first_raw_response(st).map(|(a, line)| (a, line, g, rid, topup))).next();
    let Some((attempt, line, generator, rid, topup)) = first else {
        return Ok(None);
    };
    let v = &line["verdict"];
    if !finalize::failed_form_screen_only(v) {
        return Ok(None);
    }
    let raw = finalize::raw_child(v).with_context(|| format!("{}: first raw response", d.id))?.with_context(|| format!("{}: first raw response has no raw child", d.id))?;
    let id = format!("{}:{}", finalize::SCREEN_FAIL_ID, d.id);
    check_parent_text(p, &id)?;
    let digest = finalize::raw_digest(&v["diag"]["post"]).unwrap_or_default();
    let origin = if p.role == MIXED { MIXED } else { "model_only" };
    let responses = json!([{"request_id": rid, "attempt": attempt, "raw_response_sha256": digest, "accepted": false, "reasons": v["reasons"], "topup": topup}]);
    let note = |reasons: Vec<String>, words: Option<usize>| {
        json!({"stratum": finalize::SCREEN_FAIL_ID, "id": id, "parent_id": d.parent_id, "role": d.role, "origin": origin, "cohort_member": d.cohort_member,
            "generator": generator, "left_out": true, "reasons": reasons, "words": words, "raw_responses": responses, "parent_text_sha256": p.text_sha256,
            "rule": RFORM_SCREEN_FAIL_RULE})
    };
    let (text, ai_spans, share) = match rform_splice(p, raw) {
        Ok(x) => x,
        Err(e) => return Ok(Some(Err(note(vec![format!("assembly_error:{e}")], None)))),
    };
    let n = ptext::words(&text);
    let why = generation::doc_window_reasons(n, era_norm::LENGTH_WINDOW);
    if !why.is_empty() {
        return Ok(Some(Err(note(why, Some(n)))));
    }
    let (lo, hi) = era_norm::LENGTH_WINDOW;
    let mut s = d.clone();
    s.id = id.clone();
    s.origin = origin.into();
    s.y = 1;
    s.generator = generator.clone();
    s.request_id = rid.clone();
    s.attempt = Some(u64::from(attempt));
    s.topup = topup;
    s.words = n;
    s.text_sha256 = sha256_hex(text.as_bytes());
    s.text = text;
    s.ai_spans = ai_spans;
    s.share = share;
    s.norm = None;
    s.form = Some(finalize::R_FORM.into());
    s.stratum = Some(finalize::SCREEN_FAIL_ID.into());
    s.twin_of = None;
    s.rform = Some(json!({"rule": RFORM_SCREEN_FAIL_RULE, "raw_responses": responses, "parent_text_sha256": p.text_sha256,
        "length_window": {"id": era_norm::LENGTH_WINDOW_ID, "min_words": lo, "max_words": hi, "words": n, "pass": true}}));
    Ok(Some(Ok(s)))
}

/// Form-sensitivity pairs (protocol A2 §11.7) between cohort documents and
/// R-form documents: (cohort id, raw twin id) for each raw twin whose
/// `twin_of` is a cohort document, sorted. The screen-fail stratum is never
/// paired.
pub fn fs_pairs(cohort: &[Doc], rform: &[Doc]) -> Vec<(String, String)> {
    let ids: HashSet<&str> = cohort.iter().map(|d| d.id.as_str()).collect();
    let mut out: Vec<(String, String)> = rform
        .iter()
        .filter(|d| d.stratum.as_deref() == Some(finalize::RAW_TWIN_STRATUM))
        .filter_map(|d| Some((d.twin_of.clone().filter(|t| ids.contains(t.as_str()))?, d.id.clone())))
        .collect();
    out.sort();
    out
}

/// Assembles the mock cohort under `profile`. A normalising profile needs
/// `norm` (the `era_norm` prior): every accepted child must carry that
/// normaliser's post-processing record, and every document of every class
/// goes through the profile's every-class step (`era_norm::every_class`); a
/// child's text must already be in the excerpt layout. Under a profile that
/// excludes, a document failing that step (paragraph screen or length
/// window) leaves the cohort whatever its class, and `excluded.jsonl` lists
/// it with its reasons (no text).
///
/// Under profile v2 it also writes the R-form population (protocol A2
/// §7.2): `rform-cohort.jsonl` holds, in parent order, every cohort
/// human-only document unchanged, each cohort child's raw twin and the
/// screen-fail stratum `rform_screen_fail_v1`; `rform-excluded.jsonl` lists
/// the stratum documents left out (no text); `requests-rform/` shards the
/// population like the cohort; `ASSEMBLY.json` gains an `rform` record.
/// * A raw twin is the accepted response's raw child, the verdict's
///   `raw_text` read exactly and refused unless its SHA-256 is the verdict's
///   `raw_response_sha256` (every accepted child must carry one), spliced
///   into the same parent, with the cohort document's labels. It exists
///   exactly when its cohort document passes the every-class step (its
///   verdict is that document's) and goes through no step itself. Its id is
///   `rform:<doc id>`, and `twin_of` names the cohort document (`fs_pairs`).
/// * The stratum covers each child whose first raw response failed the form
///   screen only (`grammar_a_positives::finalize::failed_form_screen_only`),
///   whatever became of the child: that raw response spliced into the
///   parent, window-checked and through no other step, with id
///   `rform_screen_fail_v1:<doc id>` and no `twin_of`. The first raw
///   response (amendment 19 items 19.1 and 19.3) is the
///   primary request's lowest attempt that returned model content
///   (`finalize::first_raw_response`), accepted or not, else the top-up's
///   when the primary has none. A first raw response rejected by anything
///   else, before the profile step included, keeps the child out.
/// * The R-form documents are unique by text: a duplicate refuses the
///   assembly (item 19.6). The stratum enters only R-form evaluation
///   populations (item 19.5): a cohort assembled for a Train population
///   gives none of its stratum documents to Train, N0 or S0-13, which take
///   raw twins only (`stratum` names each document's).
pub fn assemble(batch: &Path, runs: &Path, topup_set: Option<(&Path, &Path)>, out: &Path, shards: usize, norm: Option<&Path>, profile: Profile) -> Result<Value> {
    ensure!(shards >= 1, "shards");
    era_norm::check_profile_prior(profile, norm)?;
    let norm_sha = norm.map(file_sha256).transpose()?;
    if let Some(n) = norm {
        era_norm::EraPost::load(n, batch)?;
    }
    let parents = read_parents(batch)?;
    let states = run_states(runs)?;
    // parent id -> (top-up generator, top-up request id)
    let mut tmap: HashMap<String, (String, String)> = HashMap::new();
    let mut tstates = HashMap::new();
    let mut topup_sha = Value::Null;
    if let Some((tdir, truns)) = topup_set {
        let t: Value = store::read_json(&tdir.join("TOPUP.json"))?;
        ensure!(t["batch_sha256"] == file_sha256(&batch.join("BATCH.json"))?.as_str(), "top-up belongs to another batch");
        for it in t["items"].as_array().context("items")? {
            tmap.insert(it["parent_id"].as_str().context("parent_id")?.into(),
                (it["generator"].as_str().context("generator")?.into(), it["request_id"].as_str().context("request_id")?.into()));
        }
        tstates = run_states(truns)?;
        topup_sha = json!(file_sha256(&tdir.join("TOPUP.json"))?);
    }
    // Profile v2's R-form population: (parent index, order, document), and
    // the stratum documents left out.
    let rform_on = profile.transfers();
    let mut rpop: Vec<(usize, u8, Doc)> = Vec::new();
    let mut rleft: Vec<Value> = Vec::new();
    let mut rcounts: BTreeMap<String, usize> = BTreeMap::new();
    let mut docs = Vec::new();
    let mut excluded: Vec<Value> = Vec::new();
    let mut excluded_by: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut missing: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for (pi, p) in parents.iter().enumerate() {
        let mut d = Doc {
            schema: DOC_SCHEMA.into(),
            id: doc_id(p),
            parent_id: p.id.clone(),
            role: p.role.clone(),
            cohort_member: p.cohort_member,
            origin: String::new(),
            y: 0,
            domain: p.domain.clone(),
            year: p.year,
            group: p.group.clone(),
            generator: p.generator.clone(),
            op: p.op.clone(),
            share: 0.0,
            ai_spans: vec![],
            words: 0,
            text: String::new(),
            text_sha256: String::new(),
            request_id: p.request_id.clone(),
            attempt: None,
            topup: false,
            norm: None,
            form: None,
            stratum: None,
            twin_of: None,
            rform: None,
        };
        let mut post_rec = Value::Null;
        // the accepted raw child and its digest (profile v2)
        let mut twin_raw: Option<(String, String)> = None;
        if p.role == HUMAN {
            d.origin = HUMAN.into();
            d.text = p.text.clone();
        } else {
            if rform_on {
                let primary = p.request_id.as_ref().and_then(|r| states.get(r)).map(|s| (s, p.generator.clone(), p.request_id.clone(), false));
                let top = tmap.get(&p.id).and_then(|(g, rid)| tstates.get(rid).map(|s| (s, Some(g.clone()), Some(rid.clone()), true)));
                match rform_screen_fail(p, &d, primary.into_iter().chain(top))? {
                    None => {}
                    Some(Ok(s)) => {
                        *rcounts.entry("screen_fail".into()).or_default() += 1;
                        rpop.push((pi, 1, s));
                    }
                    Some(Err(note)) => {
                        for r in note["reasons"].as_array().into_iter().flatten() {
                            let r = r.as_str().unwrap_or("");
                            *rcounts.entry(format!("screen_fail_left_out:{}", r.split(':').next().unwrap_or(r))).or_default() += 1;
                        }
                        rleft.push(note);
                    }
                }
            }
            let mut acc = p.request_id.as_ref().and_then(|r| states.get(r)).and_then(|s| s.accepted().cloned());
            if acc.is_none()
                && let Some((g, rid)) = tmap.get(&p.id)
                && let Some(v) = tstates.get(rid).and_then(|s| s.accepted().cloned())
            {
                d.generator = Some(g.clone());
                d.request_id = Some(rid.clone());
                d.topup = true;
                acc = Some(v);
            }
            let Some(v) = acc else {
                *missing.entry(p.role.clone()).or_default().entry(p.generator.clone().unwrap_or_default()).or_default() += 1;
                if let Some(st) = p.request_id.as_ref().and_then(|r| states.get(r)) {
                    for r in st.responses.values() {
                        for x in r["verdict"]["reasons"].as_array().into_iter().flatten() {
                            *reasons.entry(x.as_str().unwrap_or("").to_string()).or_default() += 1;
                        }
                    }
                } else {
                    *reasons.entry("not_run".into()).or_default() += 1;
                }
                continue;
            };
            let clean = v["verdict"]["text"].as_str().context("accepted text")?;
            if let Some(sha) = &norm_sha {
                post_rec = v["verdict"]["diag"]["post"].clone();
                ensure!(post_rec["normaliser"] == era_norm::NORM_ID && post_rec["norm_sha256"] == sha.as_str(),
                    "{}: accepted response was not post-processed with this normaliser", d.id);
            }
            if rform_on {
                let raw = finalize::raw_child(&v["verdict"])
                    .with_context(|| format!("{}: accepted response", d.id))?
                    .with_context(|| format!("{}: accepted response carries no profile v2 raw_response_sha256", d.id))?;
                let digest = finalize::raw_digest(&v["verdict"]["diag"]["post"]).unwrap_or_default();
                twin_raw = Some((raw.to_string(), digest.to_string()));
            }
            d.attempt = v["attempt"].as_u64();
            d.y = 1;
            if p.role == MIXED {
                let span = p.source_span.context("mixed span")?;
                let (t, fin, share) = generation::assemble(&p.text, &[span], &[clean])?;
                d.origin = MIXED.into();
                d.text = t;
                d.ai_spans = fin;
                d.share = share;
            } else {
                d.origin = "model_only".into();
                d.text = clean.to_string();
                d.ai_spans = vec![(0, d.text.len())];
                d.share = 1.0;
            }
        }
        if profile.normalises() {
            let (t, every, why) = era_norm::every_class(&d.text, profile);
            if t != d.text {
                // Only a human text may change here; a child is already in the layout.
                ensure!(d.origin == HUMAN, "{}: normalised child is not in the excerpt layout", d.id);
                d.text = t;
            }
            d.norm = Some(json!({"post": post_rec, "every_class": every}));
            if profile.excludes() && !why.is_empty() {
                let e = excluded_by.entry(d.origin.clone()).or_default();
                for r in &why {
                    *e.entry(r.clone()).or_default() += 1;
                }
                excluded.push(json!({"id": d.id, "parent_id": d.parent_id, "role": d.role, "origin": d.origin, "cohort_member": d.cohort_member,
                    "generator": d.generator, "words": ptext::words(&d.text), "reasons": why}));
                if twin_raw.is_some() {
                    *rcounts.entry("raw_twins_left_out_with_cohort_document".into()).or_default() += 1;
                }
                continue;
            }
        }
        d.words = ptext::words(&d.text);
        d.text_sha256 = sha256_hex(d.text.as_bytes());
        if rform_on {
            match &twin_raw {
                Some((raw, digest)) => {
                    let t = rform_twin(p, &d, raw, digest)?;
                    *rcounts.entry("raw_twins".into()).or_default() += 1;
                    if t.rform.as_ref().is_some_and(|r| r["folded_tokens_equal_cohort_document"] == false) {
                        *rcounts.entry("raw_twins_folded_tokens_differ".into()).or_default() += 1;
                    }
                    rpop.push((pi, 0, t));
                }
                None => {
                    *rcounts.entry("humans".into()).or_default() += 1;
                    rpop.push((pi, 0, d.clone()));
                }
            }
        }
        docs.push(d);
    }
    let mut seen = HashSet::new();
    for d in &docs {
        if !seen.insert(d.text_sha256.clone()) {
            bail!("duplicate assembled text {}", d.id);
        }
    }
    fs::create_dir_all(out.join("requests"))?;
    let cohort_sha = write_jsonl(&out.join("cohort.jsonl"), &docs)?;
    let excluded_sha = if profile.excludes() { json!(write_jsonl(&out.join("excluded.jsonl"), &excluded)?) } else { Value::Null };
    let mut shard_sha = Vec::new();
    for s in 0..shards {
        let rows: Vec<Value> = docs.iter().enumerate().filter(|(i, _)| i % shards == s).map(|(_, d)| json!({"id":d.id,"text":d.text})).collect();
        shard_sha.push(write_jsonl(&out.join("requests").join(format!("shard-{s:02}.jsonl")), &rows)?);
    }
    let rform_rec = if rform_on {
        rpop.sort_by_key(|x| (x.0, x.1));
        let rdocs: Vec<Doc> = rpop.into_iter().map(|x| x.2).collect();
        // unique by text (amendment 19 item 19.6)
        let mut seen: HashMap<&str, &str> = HashMap::new();
        for d in &rdocs {
            if let Some(first) = seen.insert(d.text_sha256.as_str(), d.id.as_str()) {
                bail!("duplicate R-form text: {} has the text of {first}", d.id);
            }
        }
        let pairs = fs_pairs(&docs, &rdocs).len();
        let rcohort_sha = write_jsonl(&out.join("rform-cohort.jsonl"), &rdocs)?;
        let rexcluded_sha = write_jsonl(&out.join("rform-excluded.jsonl"), &rleft)?;
        let mut rshard_sha = Vec::new();
        for s in 0..shards {
            let rows: Vec<Value> = rdocs.iter().enumerate().filter(|(i, _)| i % shards == s).map(|(_, d)| json!({"id":d.id,"text":d.text})).collect();
            rshard_sha.push(write_jsonl(&out.join("requests-rform").join(format!("shard-{s:02}.jsonl")), &rows)?);
        }
        Some(json!({"population": "the cohort's human-only documents unchanged, the raw twins and rform_screen_fail_v1 (protocol A2 section 7.2; the stratum only for R-form evaluation populations, amendment 19 item 19.5; unique by text, item 19.6)",
            "raw_twin_rule": RFORM_TWIN_RULE, "screen_fail_rule": RFORM_SCREEN_FAIL_RULE, "docs": rdocs.len(), "counts": rcounts,
            "fs_pairs": pairs, "cohort_sha256": rcohort_sha, "excluded": rleft.len(), "excluded_sha256": rexcluded_sha, "request_shards": rshard_sha}))
    } else {
        None
    };
    let mut by: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut members: BTreeMap<String, usize> = BTreeMap::new();
    let topped = docs.iter().filter(|d| d.topup).count();
    // Normaliser summary by origin: texts the post-processing changed, texts the
    // every-class layout changed, and paragraph-screen flags.
    let mut norm_by: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for d in &docs {
        *by.entry(d.role.clone()).or_default().entry(d.generator.clone().unwrap_or_else(|| "-".into())).or_default() += 1;
        if d.cohort_member {
            *members.entry(d.origin.clone()).or_default() += 1;
        }
        if let Some(n) = &d.norm {
            let e = norm_by.entry(d.origin.clone()).or_default();
            *e.entry("documents".into()).or_default() += 1;
            *e.entry("child_text_changed".into()).or_default() += (n["post"]["changed"] == true) as usize;
            *e.entry("every_class_layout_changed".into()).or_default() += (n["every_class"]["layout_changed"] == true) as usize;
            for r in n["every_class"]["paragraph_screen"].as_array().into_iter().flatten() {
                *e.entry(format!("every_class_{}", r.as_str().unwrap_or(""))).or_default() += 1;
            }
            for (k, x) in n["post"]["changes"].as_object().into_iter().flatten() {
                *e.entry(format!("child_changed_{k}")).or_default() += (x.as_u64().unwrap_or(0) > 0) as usize;
            }
        }
    }
    let mut rec = json!({"schema":"slop_ninja_cc_era_assembly_v3","batch_sha256":file_sha256(&batch.join("BATCH.json"))?,
        "profile":profile.record(),
        "docs":docs.len(),"cohort_members_by_origin":members,"docs_by_role_generator":by,"missing_by_role_generator":missing,
        "missing_reasons":reasons,"topup_sha256":topup_sha,"accepted_from_topup":topped,
        "excluded_at_assembly":excluded.len(),"excluded_by_origin_reason":excluded_by,"excluded_sha256":excluded_sha,
        "length_by_origin":length_summary(&docs),
        "normaliser":norm_sha.as_ref().map(|_| era_norm::NORM_ID),"norm_sha256":norm_sha,"norm_by_origin":norm_by,
        "cohort_sha256":cohort_sha,"request_shards":shard_sha});
    if let Some(r) = rform_rec {
        rec["rform"] = r;
    }
    store::write_json(&out.join("ASSEMBLY.json"), &rec)?;
    Ok(rec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_and_dates() {
        // The era-v1 batch table reproduces its allocation.
        let v1 = ShareTable::by_id("cc_launch_shares_v1").unwrap();
        let shares: Vec<f64> = v1.fractions().iter().map(|x| x.1).collect();
        let a = allocate(400, &shares);
        assert_eq!(a.iter().sum::<usize>(), 400);
        assert_eq!(a, vec![110, 111, 48, 40, 34, 30, 14, 10, 3]);
        assert_eq!(existence_by("2013-05-22T11:12:40Z").unwrap(), "May 2013");
        assert!(existence_by("2013-13-01").is_err());
    }

    #[test]
    fn mirror_prompts_by_arm() {
        let t = vec!["harbour".to_string(), "ferry".to_string()];
        let d = mirror_prompt(MIRROR_DATED, "news", 210, &t, "March 2016");
        assert!(d.contains("about 210 words") && d.contains("harbour, ferry") && d.contains("published in March 2016"));
        let tp = mirror_prompt(MIRROR_TOPIC, "news", 210, &t, "March 2016");
        assert!(tp.contains("harbour") && !tp.contains("2016"));
        let pl = mirror_prompt(MIRROR_PLAIN, "forum", 99, &t, "March 2016");
        assert!(!pl.contains("harbour") && pl.contains("online discussion forum"));
    }

    #[test]
    fn mixed_requests_cover_every_operation() {
        let text = "The ferry left the harbour at dawn. Gulls followed it for a while.\n\nBy noon the wind had turned. The captain ordered the sails down and waited.\n\nIn the evening they reached the island. Nobody met them at the pier, so they walked to the village alone.";
        let stop = ptext::stopwords();
        let mut idf = ptext::Idf::default();
        idf.add(text, &stop);
        let mut p = Parent {
            schema: PARENT_SCHEMA.into(), id: "cc-x".into(), role: MIXED.into(), cohort_member: true, domain: "fiction".into(), year: 2015,
            crawl: "CC-MAIN-2015-11".into(), warc_date: "2015-03-01T00:00:00Z".into(), existence_by: "March 2015".into(), era: "E0".into(),
            group: "g".into(), host: "h".into(), url: "u".into(), text_variant: "text".into(), text: text.into(), text_sha256: String::new(),
            pool_text_sha256: String::new(), words: ptext::words(text), generator: Some("glm4".into()), op: None, item_id: Some("ccera-test".into()),
            request_id: None, source_span: None, terms: vec![], notes: vec![],
        };
        for op in MIXED_OPS {
            let (tmpl, prompt, span, target, _) = mixed_request(&p, op, &idf, &stop).unwrap();
            assert!(span.0 < span.1 && span.1 <= text.len() && target > 0, "{op}");
            assert!(!prompt.is_empty() && !tmpl.is_empty());
            if op == span_ops::OP_PREFIX {
                assert_eq!(span.1, text.len());
            } else {
                assert!(span.0 > 0 && span.1 < text.len());
            }
        }
        p.words += 1;
        assert!(mixed_request(&p, span_ops::OP_PREFIX, &idf, &stop).is_none());
    }

    // ------------------------------------------------------------ R-form (protocol A2 §7.2)
    //
    // Synthetic text only, no model, detector or network call: the verdicts
    // come from the acceptance path itself (`generation::verdict` with
    // `era_norm::EraPost` under profile v2).

    const SYN: [&str; 20] = [
        "amber", "basalt", "cobalt", "delta", "ember", "fjord", "garnet", "harbor", "indigo", "juniper", "kestrel", "lantern", "meadow",
        "nectar", "orchid", "pebble", "quartz", "river", "saffron", "timber",
    ];
    const ALT: [&str; 20] = [
        "acorn", "bramble", "canyon", "dune", "estuary", "fern", "glacier", "heather", "inlet", "jasper", "knoll", "lichen", "marsh",
        "nettle", "oasis", "prairie", "quarry", "ridge", "sorrel", "thicket",
    ];

    /// `n` sentences of `len` words by a seeded LCG; in the first `dashed`
    /// sentences words 2 and 3 are joined by a closed em dash.
    fn prose_d(vocab: &[&str], seed: u64, n: usize, len: usize, dashed: usize) -> String {
        let mut st = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut out = Vec::new();
        for k in 0..n {
            let mut s = String::new();
            for i in 0..len {
                st = st.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                if i > 0 {
                    s.push_str(if i == 2 && k < dashed { "\u{2014}" } else { " " });
                }
                s.push_str(vocab[((st >> 33) as usize) % vocab.len()]);
            }
            out.push(format!("{}{}.", s[..1].to_uppercase(), &s[1..]));
        }
        out.join(" ")
    }

    fn prose(vocab: &[&str], seed: u64, n: usize, len: usize) -> String {
        prose_d(vocab, seed, n, len, 0)
    }

    /// An excerpt-layout parent of about 194 words; curly double quotes and
    /// spaced em dashes are its majorities.
    fn cc_text(k: u64) -> String {
        let s = |i: u64| 10 * k + i;
        let a = format!("{} The keeper called it \u{201C}amber harbor\u{201D} again \u{2014} twice. {}", prose(&SYN, s(1), 3, 11), prose(&SYN, s(2), 2, 10));
        let b = prose(&SYN, s(3), 6, 11);
        let c = format!("{} They named it \u{201C}quiet fjord\u{201D} later \u{2014} much later. {}", prose(&SYN, s(4), 2, 12), prose(&SYN, s(5), 3, 11));
        format!("{a}\n\n{b}\n\n{c}")
    }

    fn cc_parent(id: &str, role: &str, text: &str, span: Option<(usize, usize)>) -> Parent {
        let child = role != HUMAN;
        Parent {
            schema: PARENT_SCHEMA.into(),
            id: id.into(),
            role: role.into(),
            cohort_member: true,
            domain: "news".into(),
            year: 2021,
            crawl: "CC-MAIN-2021-04".into(),
            warc_date: "2021-01-20T00:00:00Z".into(),
            existence_by: "January 2021".into(),
            era: "E3".into(),
            group: format!("g-{id}"),
            host: format!("{id}.example"),
            url: format!("https://{id}.example/a"),
            text_variant: "text".into(),
            text: text.into(),
            text_sha256: sha256_hex(text.as_bytes()),
            pool_text_sha256: sha256_hex(text.as_bytes()),
            words: ptext::words(text),
            generator: child.then(|| "g".to_string()),
            op: (role == MIXED).then(|| OP_SPAN_REWRITE.to_string()),
            item_id: child.then(|| format!("item-{id}")),
            request_id: child.then(|| format!("req-{id}")),
            source_span: span,
            terms: vec![],
            notes: vec![],
        }
    }

    fn cc_request(p: &Parent) -> Request {
        let target = match p.source_span {
            Some((s, e)) => ptext::words(&p.text[s..e]),
            None => p.words,
        };
        serde_json::from_value(json!({"request_id": p.request_id, "item_id": p.item_id, "parent_id": p.id, "split": SPLIT, "generator": "g",
            "endpoint": "chat", "template_id": "t", "prompt": format!("prompt {}", p.id), "span_index": 0, "source_span": p.source_span,
            "target_words": target, "cut_budget": null,
            "decoding": {"profile": "A", "temperature": 0.7, "top_p": 0.9, "top_k": 0, "min_p": 0.0, "repeat_penalty": 1.0, "max_tokens": 512},
            "request_sha256": "x", "seed": 1, "op": p.op, "cohort": p.role}))
        .unwrap()
    }

    fn sentence(p: &str, k: usize) -> (usize, usize) {
        let s = ptext::sentences(p)[k];
        (s.start, s.end)
    }

    fn tok_file(dir: &Path) -> PathBuf {
        let p = dir.join("tokenizer.json");
        fs::write(&p, r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#).unwrap();
        p
    }

    /// A population batch under profile v2: the parents, their requests'
    /// replies per attempt (the primary request's, and the top-up's for a
    /// topped-up child), the v2 prior and the journals of every attempt
    /// judged by the acceptance path until one is accepted.
    struct Fixture {
        dir: tempfile::TempDir,
        parents: Vec<Parent>,
        replies: HashMap<String, Vec<String>>,
        topup: HashMap<String, Vec<String>>,
    }

    impl Fixture {
        fn norm(&self) -> PathBuf {
            self.dir.path().join("norm-v2.json")
        }
        fn runs(&self) -> PathBuf {
            self.dir.path().join("runs")
        }
        fn parent(&self, id: &str) -> &Parent {
            self.parents.iter().find(|p| p.id == id).unwrap()
        }
        fn raw(&self, id: &str, attempt: usize) -> String {
            self.replies[id][attempt].trim().to_string()
        }
        fn raw_top(&self, id: &str, attempt: usize) -> String {
            self.topup[id][attempt].trim().to_string()
        }
    }

    fn title(t: &str, body: String) -> String {
        format!("{t}\n{body}")
    }

    fn fixture() -> Fixture {
        fixture_with(vec![])
    }

    /// The fixture's parents and replies (see each line), then `extra`
    /// parents with their primary replies.
    fn fixture_with(extra: Vec<(Parent, Vec<String>)>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let (tm, ts) = (cc_text(2), cc_text(6));
        // a parent whose last paragraph fails the screen away from the span:
        // its child is accepted, then excluded by the every-class step
        let tx = format!("{}\n\nShort closing line.", cc_text(7));
        let mut parents = vec![
            cc_parent("h1", HUMAN, &cc_text(1), None),
            cc_parent("m1", MIXED, &tm, Some(sentence(&tm, 1))),
            cc_parent("o1", MIRROR_TOPIC, &cc_text(3), None),
            cc_parent("s1", MIRROR_DATED, &cc_text(4), None),
            cc_parent("w1", MIRROR_PLAIN, &cc_text(5), None),
            cc_parent("e1", MIXED, &ts, Some(sentence(&ts, 3))),
            cc_parent("x1", MIXED, &tx, Some(sentence(&tx, 1))),
            cc_parent("t1", MIRROR_TOPIC, &cc_text(8), None),
            cc_parent("t2", MIRROR_DATED, &cc_text(9), None),
            cc_parent("r1", MIRROR_PLAIN, &cc_text(10), None),
        ];
        let e1_span = sentence(&ts, 3);
        let mut replies: HashMap<String, Vec<String>> = [
            // a span in ASCII quotes: accepted
            ("m1", vec!["The ranger said \"marsh nettle\" and walked on past the oasis ridge.".to_string()]),
            // a one-paragraph mirror in ASCII quotes: accepted
            ("o1", vec![format!("{} The ranger said \"marsh nettle\" twice. {}", prose(&ALT, 11, 9, 11), prose(&ALT, 12, 8, 11))]),
            // a title line first (the form screen only), then accepted
            ("s1", vec![title("A Quiet Harbor Morning", prose(&ALT, 41, 17, 11)), prose(&ALT, 42, 18, 11)]),
            // the form screen twice; the first raw response has 140 words, its laid-out child 152
            ("w1", vec![title("Quiet Harbor Notes", prose_d(&ALT, 51, 13, 11, 6)), title("Quiet Harbor Notes", prose_d(&ALT, 52, 13, 11, 6))]),
            // an echo of the span twice (not the form screen)
            ("e1", vec![ts[e1_span.0..e1_span.1].to_string(), ts[e1_span.0..e1_span.1].to_string()]),
            // accepted, but its cohort document fails the every-class step
            ("x1", vec!["The warden said \"quiet inlet\" and rowed on past the glacier knoll.".to_string()]),
            // t1: the primary request never ran (its replies are the top-up's)
            // the form screen twice, so the primary is spent; the top-up is accepted
            ("t2", vec![title("Notes From The Fjord", prose(&ALT, 83, 17, 11)), title("More Notes From The Fjord", prose(&ALT, 84, 17, 11))]),
            // a nonempty reasoning block first (content, rejected before the profile step), then the form screen only
            ("r1", vec![format!("<think>Plan the mirror of the page.</think>\n{}", prose(&ALT, 85, 17, 11)), title("Notes From The Marsh", prose(&ALT, 86, 17, 11))]),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let topup: HashMap<String, Vec<String>> = [
            // the top-up's first response has a title line, then accepted
            ("t1", vec![title("Notes From The Ridge", prose(&ALT, 81, 17, 11)), prose(&ALT, 82, 18, 11)]),
            // accepted at once
            ("t2", vec![prose(&ALT, 87, 18, 11)]),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        for (p, r) in extra {
            replies.insert(p.id.clone(), r);
            parents.push(p);
        }
        let rows: Vec<String> = parents.iter().map(|p| serde_json::to_string(p).unwrap()).collect();
        fs::write(dir.path().join("parents.jsonl"), rows.join("\n") + "\n").unwrap();
        let sha = file_sha256(&dir.path().join("parents.jsonl")).unwrap();
        store::write_json(&dir.path().join("BATCH.json"), &json!({"schema": BATCH_SCHEMA, "parents_sha256": sha, "population": "deviter2_c"})).unwrap();
        let f = Fixture { dir, parents, replies, topup };
        era_norm::write_prior_v2(f.dir.path(), &f.norm(), "deviter2_c").unwrap();
        let post = Arc::new(era_norm::EraPost::load(&f.norm(), f.dir.path()).unwrap());
        let ctx = generation::Ctx {
            endpoint: String::new(),
            alias: "m".into(),
            config: json!({}),
            tokenizer: tokenizers::Tokenizer::from_file(tok_file(f.dir.path())).unwrap(),
            overlap: overlap::Overlap::new(overlap::PriorIndex::empty()),
            parents: f.parents.iter().map(|p| (p.id.clone(), p.text.clone())).collect(),
            client: generation::client().unwrap(),
            post: Some(post as Arc<dyn generation::PostProcess>),
            doc_words: Profile::CohortV2.window(),
        };
        let judge = |r: &Request, replies: &[String], lines: &mut Vec<String>| {
            for (a, reply) in replies.iter().enumerate() {
                let v = generation::verdict(&ctx, r, 200, &json!({"choices":[{"message":{"content":reply},"finish_reason":"stop"}]}));
                lines.push(json!({"kind":"intent","request_id":r.request_id,"item_id":r.item_id,"split":r.split,"generator":r.generator,"attempt":a,"seed":1,"t":0,"body":{}}).to_string());
                let ok = v["accepted"] == true;
                lines.push(json!({"kind":"response","request_id":r.request_id,"item_id":r.item_id,"split":r.split,"generator":r.generator,"attempt":a,"seed":1,
                    "t_start":0,"t_end":0,"elapsed_ms":0,"status":200,"raw_body":"","verdict":v}).to_string());
                if ok {
                    break;
                }
            }
        };
        let mut lines: Vec<String> = Vec::new();
        let mut top: Vec<String> = Vec::new();
        let mut items = Vec::new();
        for p in f.parents.iter().filter(|p| p.role != HUMAN) {
            let r = cc_request(p);
            if let Some(rs) = f.replies.get(&p.id) {
                judge(&r, rs, &mut lines);
            }
            if let Some(rs) = f.topup.get(&p.id) {
                let mut t = r.clone();
                t.request_id = format!("req-{}-top", p.id);
                t.item_id = format!("item-{}-t1", p.id);
                t.generator = "g2".into();
                judge(&t, rs, &mut top);
                items.push(json!({"parent_id": p.id, "role": p.role, "generator": "g2", "request_id": t.request_id, "primary_request_id": r.request_id}));
            }
        }
        fs::create_dir_all(f.runs().join("g")).unwrap();
        fs::write(f.runs().join("g").join("journal-1-w0.jsonl"), lines.join("\n") + "\n").unwrap();
        // the top-up round: t1 and t2 re-issued to g2
        let tdir = f.dir.path().join("topup");
        store::write_json(&tdir.join("TOPUP.json"), &json!({"schema": "slop_ninja_cc_era_topup_v1", "batch_sha256": file_sha256(&f.dir.path().join("BATCH.json")).unwrap(),
            "items": items, "request_files": {}}))
        .unwrap();
        fs::create_dir_all(tdir.join("runs").join("g2")).unwrap();
        fs::write(tdir.join("runs").join("g2").join("journal-1-w0.jsonl"), top.join("\n") + "\n").unwrap();
        f
    }

    fn run_v2(f: &Fixture, out: &Path) -> Result<Value> {
        let tdir = f.dir.path().join("topup");
        assemble(f.dir.path(), &f.runs(), Some((&tdir, &tdir.join("runs"))), out, 2, Some(&f.norm()), Profile::CohortV2)
    }

    #[test]
    fn era_batch_rform_population_raw_twins_rebuild_byte_exactly() {
        let f = fixture();
        let out = f.dir.path().join("out");
        let rec = run_v2(&f, &out).unwrap();
        let cohort: Vec<Doc> = store::rows_plain(&out.join("cohort.jsonl")).unwrap();
        let rform: Vec<Doc> = store::rows_plain(&out.join("rform-cohort.jsonl")).unwrap();
        let ids = |ds: &[Doc]| ds.iter().map(|d| d.id.clone()).collect::<Vec<_>>();
        let did = |p: &str| doc_id(f.parent(p));
        // the cohort: the human and the accepted children; w1, e1 and r1 are missing, x1 is excluded at assembly
        assert_eq!(ids(&cohort), vec![did("h1"), did("m1"), did("o1"), did("s1"), did("t1"), did("t2")]);
        // the R-form population, in parent order: the human unchanged, each child's twin, the stratum after its parent's twin
        let (tw, sf) = (|p: &str| format!("rform:{}", did(p)), |p: &str| format!("rform_screen_fail_v1:{}", did(p)));
        assert_eq!(ids(&rform), vec![did("h1"), tw("m1"), tw("o1"), tw("s1"), sf("s1"), tw("t1"), sf("t1"), tw("t2"), sf("t2")]);
        assert_eq!(serde_json::to_value(&rform[0]).unwrap(), serde_json::to_value(&cohort[0]).unwrap());
        // the twins: the accepted raw responses (the top-up's for t1 and t2) spliced into the same parents, byte for byte
        for (k, r, id, attempt, topped) in [(1usize, 1usize, "m1", 0usize, false), (2, 2, "o1", 0, false), (3, 3, "s1", 1, false), (4, 5, "t1", 1, true), (5, 7, "t2", 0, true)] {
            let (p, n, t) = (f.parent(id), &cohort[k], &rform[r]);
            let raw = if topped { f.raw_top(id, attempt) } else { f.raw(id, attempt) };
            let want = match p.source_span {
                Some(span) => generation::assemble(&p.text, &[span], &[raw.as_str()]).unwrap(),
                None => (raw.clone(), vec![(0, raw.len())], 1.0),
            };
            assert_eq!(t.text, want.0, "{id}");
            assert_eq!((t.ai_spans.clone(), t.share), (want.1.clone(), want.2));
            assert_eq!(t.text_sha256, sha256_hex(want.0.as_bytes()));
            assert_eq!(&t.text[t.ai_spans[0].0..t.ai_spans[0].1], raw);
            assert_eq!((t.form.as_deref(), t.stratum.as_deref(), t.twin_of.as_deref()), (Some(finalize::R_FORM), Some(finalize::RAW_TWIN_STRATUM), Some(n.id.as_str())));
            let ev = t.rform.as_ref().unwrap();
            assert_eq!(ev["raw_response_sha256"].as_str(), Some(sha256_hex(raw.as_bytes()).as_str()));
            assert_eq!(ev["cohort_text_sha256"].as_str(), Some(n.text_sha256.as_str()));
            assert_eq!(ev["folded_tokens_equal_cohort_document"], true);
            // the cohort document's labels; measured fields are the twin's
            assert_eq!((&t.origin, t.y, &t.role, &t.domain, &t.group, &t.generator, &t.request_id, t.attempt, t.cohort_member), (&n.origin, n.y, &n.role, &n.domain, &n.group, &n.generator, &n.request_id, n.attempt, n.cohort_member));
            assert_eq!(t.words, ptext::words(&t.text));
            assert!(t.norm.is_none() && n.norm.is_some());
            // the cohort document is laid out (curly quotes) where the twin is raw
            if id == "m1" || id == "o1" {
                assert!(t.text.contains('"') && !n.text.contains('"') && n.text.contains('\u{201C}'), "{id}");
            }
        }
        // a top-up child's twin is the top-up's accepted raw response
        let (t1, t2) = (&rform[5], &rform[7]);
        assert_eq!((t1.topup, t1.generator.as_deref(), t1.request_id.as_deref(), t1.attempt), (true, Some("g2"), Some("req-t1-top"), Some(1)));
        assert_eq!((t2.topup, t2.generator.as_deref(), t2.request_id.as_deref(), t2.attempt), (true, Some("g2"), Some("req-t2-top"), Some(0)));
        // x1: its cohort document left at the every-class step, and its twin with it
        let excluded: Vec<Value> = read_jsonl(&out.join("excluded.jsonl")).unwrap();
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0]["id"].as_str(), Some(did("x1").as_str()));
        assert!(excluded[0]["reasons"].as_array().unwrap().iter().all(|r| r.as_str().unwrap().starts_with("form_paragraph_")), "{}", excluded[0]);
        assert!(rform.iter().all(|d| !d.id.contains(&did("x1"))));
        // the cohort's bytes carry no R-form field
        for l in fs::read_to_string(out.join("cohort.jsonl")).unwrap().lines() {
            let v: Value = serde_json::from_str(l).unwrap();
            for k in ["form", "stratum", "twin_of", "rform"] {
                assert!(v.get(k).is_none(), "{k}");
            }
        }
        // the R-form request shards hold the population, sharded like the cohort
        let mut shard_ids = Vec::new();
        for s in 0..2 {
            for r in read_jsonl(&out.join("requests-rform").join(format!("shard-{s:02}.jsonl"))).unwrap() {
                let d = rform.iter().find(|d| d.id == r["id"].as_str().unwrap()).unwrap();
                assert_eq!(r["text"].as_str(), Some(d.text.as_str()));
                shard_ids.push(d.id.clone());
            }
        }
        shard_ids.sort();
        let mut all = ids(&rform);
        all.sort();
        assert_eq!(shard_ids, all);
        let r = &rec["rform"];
        assert_eq!(r["docs"], 9);
        assert_eq!(r["counts"], json!({"humans": 1, "raw_twins": 5, "raw_twins_left_out_with_cohort_document": 1, "screen_fail": 3,
            "screen_fail_left_out:final_document_below_150_words": 1}));
        assert_eq!(r["fs_pairs"], 5);
        assert_eq!(r["cohort_sha256"].as_str(), Some(file_sha256(&out.join("rform-cohort.jsonl")).unwrap().as_str()));
        assert_eq!(rec["profile"]["profile"], era_norm::PROFILE_V2);
    }

    #[test]
    fn era_batch_screen_fail_stratum_and_window_left_out() {
        let f = fixture();
        let out = f.dir.path().join("out");
        run_v2(&f, &out).unwrap();
        let rform: Vec<Doc> = store::rows_plain(&out.join("rform-cohort.jsonl")).unwrap();
        // s1: its first raw response (a title line) exactly, window-checked only
        let strata: Vec<&Doc> = rform.iter().filter(|d| d.stratum.as_deref() == Some(finalize::SCREEN_FAIL_ID)).collect();
        assert_eq!(strata.len(), 3);
        // t1: the primary request never ran, so its first raw response is the top-up's
        let t = strata[1];
        assert_eq!(t.id, format!("rform_screen_fail_v1:{}", doc_id(f.parent("t1"))));
        assert_eq!(t.text, f.raw_top("t1", 0));
        assert_eq!((t.topup, t.generator.as_deref(), t.request_id.as_deref(), t.attempt), (true, Some("g2"), Some("req-t1-top"), Some(0)));
        assert_eq!(t.rform.as_ref().unwrap()["raw_responses"][0]["topup"], true);
        let s = strata[0];
        assert_eq!(s.id, format!("rform_screen_fail_v1:{}", doc_id(f.parent("s1"))));
        assert_eq!(s.text, f.raw("s1", 0));
        assert_eq!(s.text.lines().next(), Some("A Quiet Harbor Morning"));
        assert_eq!((s.origin.as_str(), s.y, s.share, s.attempt, s.topup, s.twin_of.as_deref()), ("model_only", 1, 1.0, Some(0), false, None));
        assert_eq!(s.ai_spans, vec![(0, s.text.len())]);
        let ev = s.rform.as_ref().unwrap();
        assert_eq!(ev["raw_responses"][0]["reasons"], json!(["form_line_no_sentence_end"]));
        assert_eq!(ev["raw_responses"][0]["raw_response_sha256"].as_str(), Some(sha256_hex(f.raw("s1", 0).as_bytes()).as_str()));
        assert_eq!(ev["length_window"]["words"].as_u64(), Some(ptext::words(&s.text) as u64));
        // w1: both attempts failed the form screen; its first raw response is under 150 words, so it is left out, without text
        let left: Vec<Value> = read_jsonl(&out.join("rform-excluded.jsonl")).unwrap();
        assert_eq!(left.len(), 1);
        let w = &left[0];
        assert_eq!(w["id"].as_str(), Some(format!("rform_screen_fail_v1:{}", doc_id(f.parent("w1")))).as_deref());
        assert_eq!((w["left_out"].as_bool(), w["words"].as_u64()), (Some(true), Some(140)));
        assert_eq!(w["reasons"], json!(["final_document_below_150_words"]));
        assert!(w.get("text").is_none());
        let s = w.to_string();
        for t in [f.raw("w1", 0), f.parent("w1").text.clone()] {
            for sent in ptext::sentences(&t) {
                assert!(ptext::words(&t[sent.start..sent.end]) < 4 || !s.contains(&t[sent.start..sent.end]));
            }
        }
        // e1 (an echo, not the form screen) and r1 (a reasoning block first: its first raw response,
        // rejected before the profile step) have no stratum document; no R-form id names them
        for p in ["e1", "r1"] {
            let id = doc_id(f.parent(p));
            assert!(rform.iter().all(|d| !d.id.contains(&id)) && left.iter().all(|x| !x["id"].as_str().unwrap().contains(&id)), "{p}");
        }
    }

    #[test]
    fn era_batch_stratum_reads_the_primary_before_the_top_up() {
        let f = fixture();
        let out = f.dir.path().join("out");
        run_v2(&f, &out).unwrap();
        let rform: Vec<Doc> = store::rows_plain(&out.join("rform-cohort.jsonl")).unwrap();
        let did = doc_id(f.parent("t2"));
        // t2's primary failed the form screen twice and its top-up was accepted: both journals exist
        let states = run_states(&f.runs()).unwrap();
        assert_eq!(states["req-t2"].responses.len(), 2);
        assert!(states["req-t2"].accepted().is_none() && states["req-t2"].done());
        // the stratum document is the primary's first raw response
        let s = rform.iter().find(|d| d.id == format!("rform_screen_fail_v1:{did}")).unwrap();
        assert_eq!(s.text, f.raw("t2", 0));
        assert_eq!((s.topup, s.generator.as_deref(), s.request_id.as_deref(), s.attempt), (false, Some("g"), Some("req-t2"), Some(0)));
        let r0 = &s.rform.as_ref().unwrap()["raw_responses"][0];
        assert_eq!((r0["request_id"].as_str(), r0["attempt"].as_u64(), r0["topup"].as_bool()), (Some("req-t2"), Some(0), Some(false)));
        assert_eq!(r0["reasons"], json!(["form_line_no_sentence_end"]));
        // the raw twin is the top-up's accepted raw response
        let t = rform.iter().find(|d| d.id == format!("rform:{did}")).unwrap();
        assert_eq!(t.text, f.raw_top("t2", 0));
        assert_eq!((t.topup, t.generator.as_deref(), t.request_id.as_deref(), t.attempt), (true, Some("g2"), Some("req-t2-top"), Some(0)));
        assert_ne!(s.text, t.text);
    }

    #[test]
    fn era_batch_refuses_a_parent_text_that_is_not_its_digest() {
        let f = fixture();
        let out = f.dir.path().join("out");
        run_v2(&f, &out).unwrap();
        let cohort: Vec<Doc> = store::rows_plain(&out.join("cohort.jsonl")).unwrap();
        let doc = |p: &str| cohort.iter().find(|d| d.id == doc_id(f.parent(p))).unwrap().clone();
        let tampered = |p: &str| {
            let mut x = f.parent(p).clone();
            x.text.push(' ');
            x
        };
        // a twin: m1's accepted raw response
        let raw = f.raw("m1", 0);
        let digest = sha256_hex(raw.as_bytes());
        assert!(rform_twin(f.parent("m1"), &doc("m1"), &raw, &digest).is_ok());
        let e = format!("{:#}", rform_twin(&tampered("m1"), &doc("m1"), &raw, &digest).unwrap_err());
        assert!(e.contains(&doc_id(f.parent("m1"))) && e.contains("does not match text_sha256"), "{e}");
        // a stratum document: s1's first raw response
        let states = run_states(&f.runs()).unwrap();
        let journal = || std::iter::once((&states["req-s1"], Some("g".to_string()), Some("req-s1".to_string()), false));
        assert!(matches!(rform_screen_fail(f.parent("s1"), &doc("s1"), journal()).unwrap(), Some(Ok(_))));
        let e = format!("{:#}", rform_screen_fail(&tampered("s1"), &doc("s1"), journal()).unwrap_err());
        assert!(e.contains(&format!("rform_screen_fail_v1:{}", doc_id(f.parent("s1")))) && e.contains("does not match text_sha256"), "{e}");
    }

    #[test]
    fn era_batch_refuses_a_duplicate_rform_text() {
        // a second dated mirror whose first raw response is s1's: two stratum documents with one text
        let s2 = |first: String| (cc_parent("s2", MIRROR_DATED, &cc_text(11), None), vec![first, prose(&ALT, 44, 18, 11)]);
        let f = fixture_with(vec![s2(title("A Quiet Harbor Morning", prose(&ALT, 41, 17, 11)))]);
        assert_eq!(f.raw("s2", 0), f.raw("s1", 0));
        let e = run_v2(&f, &f.dir.path().join("out")).err().map(|e| format!("{e:#}")).unwrap_or_default();
        let sf = |p: &str| format!("rform_screen_fail_v1:{}", doc_id(f.parent(p)));
        assert!(e.contains(&format!("duplicate R-form text: {} has the text of {}", sf("s2"), sf("s1"))), "{e}");
        // with another first response it assembles, s2 in the stratum
        let g = fixture_with(vec![s2(title("Notes From The Pier", prose(&ALT, 45, 17, 11)))]);
        let rec = run_v2(&g, &g.dir.path().join("out")).unwrap();
        assert_eq!(rec["rform"]["counts"]["screen_fail"], 4);
    }

    #[test]
    fn era_norm_form_codes_are_each_classified() {
        // every form code literal of the CC profile step is a screen code or a listed code
        // outside the screen (amendment 19 item 19.4); a new one fails here
        let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/era_norm.rs")).unwrap();
        let keys = ["form_reasons", "form_screen", "form_screen_applied", "form_screen_reading", "form_licence"];
        let mut codes = std::collections::BTreeSet::new();
        for (i, _) in src.match_indices("\"form_") {
            let c: String = src[i + 1..].chars().take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == ':').collect();
            if !keys.contains(&c.as_str()) {
                codes.insert(c);
            }
        }
        assert!(codes.contains("form_empty") && codes.contains("form_parent_missing"), "{codes:?}");
        for c in &codes {
            let outside = finalize::OUTSIDE_SCREEN_FORM_REASONS.iter().any(|o| if o.ends_with(':') { c.starts_with(*o) } else { c.as_str() == *o });
            assert!(finalize::is_screen_reason(c) != outside, "unclassified or doubly classified form code {c:?}");
        }
    }

    #[test]
    fn era_batch_fs_pairs_exclude_the_screen_fail_stratum() {
        let f = fixture();
        let out = f.dir.path().join("out");
        run_v2(&f, &out).unwrap();
        let cohort: Vec<Doc> = store::rows_plain(&out.join("cohort.jsonl")).unwrap();
        let rform: Vec<Doc> = store::rows_plain(&out.join("rform-cohort.jsonl")).unwrap();
        let pairs = fs_pairs(&cohort, &rform);
        let mut want: Vec<(String, String)> = ["m1", "o1", "s1", "t1", "t2"].iter().map(|p| (doc_id(f.parent(p)), format!("rform:{}", doc_id(f.parent(p))))).collect();
        want.sort();
        assert_eq!(pairs, want);
        // the stratum and the human are never paired
        let strata: Vec<Doc> = rform.iter().filter(|d| d.stratum.as_deref() != Some(finalize::RAW_TWIN_STRATUM)).cloned().collect();
        assert_eq!(strata.len(), 4);
        assert!(fs_pairs(&cohort, &strata).is_empty());
    }

    #[test]
    fn era_batch_refuses_a_raw_digest_mismatch() {
        let f = fixture();
        let jp = f.runs().join("g").join("journal-1-w0.jsonl");
        let orig = fs::read_to_string(&jp).unwrap();
        let tamper = |rid: &str, attempt: u64, g: &dyn Fn(&mut Value)| {
            let lines: Vec<String> = orig
                .lines()
                .map(|l| {
                    let mut v: Value = serde_json::from_str(l).unwrap();
                    if v["kind"] == "response" && v["request_id"] == rid && v["attempt"] == attempt {
                        g(&mut v);
                    }
                    v.to_string()
                })
                .collect();
            fs::write(&jp, lines.join("\n") + "\n").unwrap();
        };
        let err = |name: &str| run_v2(&f, &f.dir.path().join(name)).err().map(|e| format!("{e:#}")).unwrap_or_default();
        // an accepted raw text that is not the digested one
        tamper("req-m1", 0, &|v: &mut Value| {
            let t = format!("{} ", v["verdict"]["raw_text"].as_str().unwrap());
            v["verdict"]["raw_text"] = json!(t);
        });
        let e = err("o1");
        assert!(e.contains(&doc_id(f.parent("m1"))) && e.contains("raw_text does not match"), "{e}");
        // an accepted response with no digest under profile v2
        tamper("req-o1", 0, &|v: &mut Value| {
            v["verdict"]["diag"]["post"].as_object_mut().unwrap().remove("raw_response_sha256");
        });
        let e = err("o2");
        assert!(e.contains(&doc_id(f.parent("o1"))) && e.contains("no profile v2 raw_response_sha256"), "{e}");
        // a first raw response (the stratum of a missing child) that is not the digested one
        tamper("req-w1", 0, &|v: &mut Value| {
            v["verdict"]["raw_text"] = json!("Quiet Harbor Notes\nSomething else.");
        });
        let e = err("o3");
        assert!(e.contains(&doc_id(f.parent("w1"))) && e.contains("raw_text does not match"), "{e}");
        // the untouched journal assembles
        fs::write(&jp, &orig).unwrap();
        assert!(run_v2(&f, &f.dir.path().join("o4")).is_ok());
    }
}
