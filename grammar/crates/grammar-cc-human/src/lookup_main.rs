//! `slop_ninja_cc_lookup`: lookup-risk measurements for the Common Crawl
//! human slice (inputs and outputs in ignored data/task-roots/cc-human).
//!
//! Steps, each writing JSON or TSV under `--out` (ignored storage):
//! * `keys`: per admitted pilot document, the index keys a lookup would
//!   need for its whole page (every valid run, sentences, winnowed 8-grams,
//!   LSH bands) and the query keys of the chosen excerpt;
//! * `bloom`: real Bloom filters over the member half of the pilot (split by
//!   source group), checked against theory and queried by the other half;
//! * `background`: how often text that is *not* indexed still hits the
//!   index (shared sentences and phrases), as the index grows;
//! * `nearmatch`: on the consumed Tier C Test (statistics only), how much of
//!   each AI child still matches its own human parent under each key type;
//! * `frame`: frame size from CC's published counts and the pilot's yields,
//!   index sizes against coverage, and the coverage an index can buy per
//!   budget under each sampling design;
//! * `gain`: a miner's cross-fitted Brier gain from a partial-coverage
//!   lookup, using the frozen seed-17 predictions;
//! * `canary`: power of a supervised anchor slice to detect such a gain.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_a_pools::{fineweb, splits::rank, text};
use grammar_cc_human::{
    candidates,
    lookup::{self, Bloom, DocKeys, binom, count_in, phi, phi_inv, unif},
    pages::{self, Page},
    shingle::{MH_BANDS, MH_PERMS, MinHasher, sorted_unique, splitmix},
    store,
};
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::{BufRead, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

#[derive(Parser)]
#[command(name = "slop_ninja_cc_lookup")]
struct Cli {
    /// Output root (data/task-roots/cc-human/lookup-v1/out).
    #[arg(long)]
    out: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Keys {
        #[arg(long)]
        pilot: PathBuf,
        #[arg(long, default_value_t = 4)]
        threads: usize,
    },
    Bloom {
        #[arg(long, default_value = "0.001,0.01,0.05")]
        fprs: String,
        #[arg(long, default_value_t = 20_000_000)]
        random_queries: u64,
    },
    Background {
        /// Tier C Test rows.jsonl (consumed; statistics only).
        #[arg(long)]
        rows: PathBuf,
    },
    Nearmatch {
        #[arg(long)]
        rows: PathBuf,
    },
    Lightedit {
        #[arg(long)]
        rows: PathBuf,
        #[arg(long, default_value = "0.01,0.02,0.05,0.1,0.2")]
        rates: String,
    },
    Frame {
        #[arg(long)]
        meta: PathBuf,
        #[arg(long)]
        pilot: PathBuf,
    },
    Gain {
        /// Seed-17 Tier C Test predictions (Faux-17.jsonl).
        #[arg(long)]
        preds: PathBuf,
        /// human_only, mixed, model_only shares of a cohort.
        #[arg(long, default_value = "0.4,0.4,0.2")]
        mix: String,
        #[arg(long, default_value = "0.01,0.02,0.05,0.1,0.25,0.5,1")]
        coverages: String,
        #[arg(long, default_value_t = 4)]
        reps: u64,
        /// Bloom false-positive rate per key for B, W and C; A uses 1e-4.
        #[arg(long, default_value_t = 0.01)]
        fpr: f64,
        #[arg(long, default_value = "gain")]
        tag: String,
        /// Replace this share of the mixed mass with synthetic light edits.
        #[arg(long)]
        light_edit_share: Option<f64>,
        #[arg(long, default_value_t = 0.05)]
        light_edit_rate: f64,
        /// Background hit rates: fit, pilot or none.
        #[arg(long, default_value = "fit")]
        bg_mode: String,
        #[arg(long, default_value_t = 0.5)]
        bg_cap: f64,
    },
    Canary {
        #[arg(long)]
        pred_dir: PathBuf,
        #[arg(long, default_value = "gain")]
        gain_tag: String,
        /// Coverage whose per-unit effect is used (small-coverage regime).
        #[arg(long, default_value_t = 0.05)]
        at: f64,
    },
}

#[derive(Deserialize)]
struct Adm {
    id: String,
    record_id: String,
    crawl: String,
    domain: String,
    era: String,
    group: String,
    words: usize,
    line_start: usize,
    line_end: usize,
    valid_runs: usize,
    text_sha256: String,
    wet_path: String,
}

#[derive(Deserialize, Clone)]
struct Row {
    id: String,
    origin: String,
    population: String,
    operation: String,
    domain: String,
    collection: String,
    text: String,
    share: Option<f64>,
    ai_byte_spans: Option<Vec<(usize, usize)>>,
    parent_id: Option<String>,
}

fn load_rows(p: &Path) -> Result<Vec<Row>> {
    let f = std::fs::File::open(p).with_context(|| format!("open {}", p.display()))?;
    let mut out = Vec::new();
    for l in std::io::BufReader::new(f).lines() {
        let r: Row = serde_json::from_str(&l?)?;
        if r.population == "primary" || r.population == "frontier" {
            out.push(r);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

fn cc_derived(r: &Row) -> bool {
    r.collection.contains("_cc_")
}

const HALF: u64 = 1 << 63;

fn grank(group: &str) -> u64 {
    rank(lookup::SPLIT_SEED, "member_split", group)
}

fn year_of(crawl: &str) -> u32 {
    crawl.get(8..12).and_then(|y| y.parse().ok()).unwrap_or(0)
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() { f64::NAN } else { v.iter().sum::<f64>() / v.len() as f64 }
}

fn sd(v: &[f64]) -> f64 {
    let m = mean(v);
    (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len().max(2) - 1) as f64).sqrt()
}

fn quantile(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() {
        return f64::NAN;
    }
    let i = ((v.len() - 1) as f64 * q).round() as usize;
    v[i]
}

fn parse_list(s: &str) -> Result<Vec<f64>> {
    s.split(',').map(|x| x.trim().parse::<f64>().with_context(|| format!("bad number {x}"))).collect()
}

fn write_text(p: &Path, s: &str) -> Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, s)?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

// ---------------------------------------------------------------- keys

fn keys(out: &Path, pilot: &Path, threads: usize) -> Result<()> {
    let t0 = Instant::now();
    let mut adm: Vec<Adm> = Vec::new();
    {
        let f = std::fs::File::open(pilot.join("pool/admitted.jsonl.zst"))?;
        for l in std::io::BufReader::with_capacity(1 << 20, zstd::Decoder::new(f)?).lines() {
            adm.push(serde_json::from_str(&l?)?);
        }
    }
    let mut by_stem: BTreeMap<String, HashMap<String, usize>> = BTreeMap::new();
    for (i, a) in adm.iter().enumerate() {
        by_stem.entry(pages::stem(&a.wet_path)).or_default().insert(a.record_id.clone(), i);
    }
    let raw = std::fs::read(pilot.join("templates.bin.zst"))?;
    let bytes = zstd::stream::decode_all(&raw[..])?;
    let templates: HashSet<u64> = bytes.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect();
    eprintln!("keys: {} admitted, {} stems, {} template lines ({:.0}s)", adm.len(), by_stem.len(), templates.len(), t0.elapsed().as_secs_f64());
    let stems: Vec<(&String, &HashMap<String, usize>)> = by_stem.iter().collect();
    let done = AtomicUsize::new(0);
    let n = stems.len();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads.max(1)).build()?;
    let mh = MinHasher::new();
    let lines: Vec<Vec<String>> = pool.install(|| {
        stems
            .par_iter()
            .map(|(st, want)| -> Result<Vec<String>> {
                let kpath = out.join("keys").join(format!("{st}.bin"));
                let tpath = out.join("keys").join(format!("{st}.tsv"));
                if kpath.exists() && tpath.exists() {
                    return Ok(std::fs::read_to_string(&tpath)?.lines().map(str::to_owned).collect());
                }
                let rows: Vec<Page> = store::rows(&pages::pages_path(pilot, st))?;
                let mut docs = Vec::new();
                let mut tsv = Vec::new();
                for p in &rows {
                    let Some(&i) = want.get(&p.record_id) else { continue };
                    let a = &adm[i];
                    let paras: Vec<text::Para> = candidates::lines(&p.text)
                        .map(|l| {
                            let rep = candidates::site_line_key(&p.registered_domain, l).is_some_and(|k| templates.contains(&k));
                            fineweb::classify(l, rep)
                        })
                        .collect();
                    let runs = candidates::valid_runs(&paras);
                    let join = |s: usize, e: usize| paras[s..e].iter().map(|x| x.text.as_str()).collect::<Vec<_>>().join("\n\n");
                    let ok_runs = runs.len() == a.valid_runs && runs.iter().any(|&(s, e, _)| s == a.line_start && e == a.line_end);
                    let ex_end = a.line_end.min(paras.len());
                    let ex = join(a.line_start.min(ex_end), ex_end);
                    let ok_sha = text::sha_hex(ex.as_bytes()) == a.text_sha256;
                    let mut mark = vec![false; paras.len()];
                    let mut ua = Vec::new();
                    let mut ul = Vec::new();
                    for &(s, e, _) in &runs {
                        for m in mark.iter_mut().take(e).skip(s) {
                            *m = true;
                        }
                        let t = join(s, e);
                        ua.push(lookup::excerpt_key(&t));
                        if let Some(b) = lookup::lsh_keys(&mh, &t) {
                            ul.extend(b);
                        }
                    }
                    let mut us = Vec::new();
                    let mut uw = Vec::new();
                    let mut sent_any = Vec::new();
                    let (mut u_lines, mut u_words) = (0usize, 0usize);
                    let mut i0 = 0;
                    while i0 < paras.len() {
                        if !mark[i0] {
                            i0 += 1;
                            continue;
                        }
                        let mut j = i0;
                        while j < paras.len() && mark[j] {
                            u_lines += 1;
                            u_words += text::words(&paras[j].text);
                            us.extend(lookup::sentence_keys(&paras[j].text));
                            sent_any.extend(lookup::sentence_keys_spans(&paras[j].text, 1).into_iter().map(|x| x.0));
                            j += 1;
                        }
                        uw.extend(lookup::winnow_keys(&join(i0, j)));
                        i0 = j;
                    }
                    let ex_lsh = lookup::lsh_keys(&mh, &ex).map(|b| b.to_vec()).unwrap_or_default();
                    let d = DocKeys {
                        idx: i as u32,
                        sets: [
                            sorted_unique(ua),
                            sorted_unique(us),
                            sorted_unique(uw),
                            sorted_unique(ul),
                            vec![lookup::excerpt_key(&ex)],
                            lookup::sentence_keys(&ex),
                            lookup::winnow_keys(&ex),
                            ex_lsh,
                        ],
                    };
                    let sent_any = sorted_unique(sent_any).len();
                    tsv.push(format!(
                        "{i}\t{}\t{}\t{}\t{}\t{}\t{}\t{:016x}\t{}\t{}\t{u_lines}\t{u_words}\t{sent_any}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        a.id,
                        a.crawl,
                        year_of(&a.crawl),
                        a.domain,
                        a.era,
                        a.group,
                        grank(&a.group),
                        a.words,
                        runs.len(),
                        d.sets[1].len(),
                        d.sets[2].len(),
                        d.sets[3].len(),
                        d.sets[0].len(),
                        d.sets[5].len(),
                        d.sets[6].len(),
                        ok_runs as u8,
                        ok_sha as u8
                    ));
                    docs.push(d);
                }
                lookup::write_docs(&kpath, &docs)?;
                write_text(&tpath, &(tsv.join("\n") + "\n"))?;
                let k = done.fetch_add(1, Ordering::SeqCst) + 1;
                if k.is_multiple_of(10) || k == n {
                    eprintln!("keys: {k}/{n} ({:.0}s)", t0.elapsed().as_secs_f64());
                }
                Ok(tsv)
            })
            .collect::<Result<Vec<_>>>()
    })?;
    let mut all: Vec<String> = lines.into_iter().flatten().filter(|l| !l.is_empty()).collect();
    all.sort_by_key(|l| l.split('\t').next().and_then(|x| x.parse::<usize>().ok()).unwrap_or(usize::MAX));
    ensure!(all.len() == adm.len(), "matched {} of {} admitted documents", all.len(), adm.len());
    let head = "idx\tid\tcrawl\tyear\tdomain\tera\tgroup\tgrank\twords\tn_runs\tunion_lines\tunion_words\tsent_any\tunion_sent\tunion_win\tunion_lsh\tunion_a\tex_sent\tex_win\tok_runs\tok_sha\n";
    write_text(&out.join("docs.tsv"), &(head.to_owned() + &all.join("\n") + "\n"))?;
    let bad_runs = all.iter().filter(|l| l.split('\t').nth(19) != Some("1")).count();
    let bad_sha = all.iter().filter(|l| l.split('\t').nth(20) != Some("1")).count();
    store::write_json(
        &out.join("keys.json"),
        &json!({"schema": "slop_ninja_cc_lookup_keys_v1", "method": lookup::KEY_METHOD, "docs": all.len(), "run_mismatch": bad_runs, "excerpt_sha_mismatch": bad_sha, "seconds": t0.elapsed().as_secs_f64()}),
    )?;
    println!("keys: {} docs, run mismatches {bad_runs}, excerpt sha mismatches {bad_sha}", all.len());
    Ok(())
}

struct DocMeta {
    idx: usize,
    domain: String,
    grank: u64,
    n_runs: f64,
    union_words: f64,
    sent_any: f64,
    union_sent: f64,
    union_win: f64,
    union_lsh: f64,
    union_a: f64,
    words: f64,
    ex_sent: f64,
}

fn load_docs(out: &Path) -> Result<Vec<DocMeta>> {
    let s = std::fs::read_to_string(out.join("docs.tsv")).context("run keys first")?;
    let mut v = Vec::new();
    for l in s.lines().skip(1) {
        let f: Vec<&str> = l.split('\t').collect();
        let g = |i: usize| f[i].parse::<f64>().unwrap_or(f64::NAN);
        v.push(DocMeta {
            idx: f[0].parse()?,
            domain: f[4].to_owned(),
            grank: u64::from_str_radix(f[7], 16)?,
            words: g(8),
            n_runs: g(9),
            union_words: g(11),
            sent_any: g(12),
            union_sent: g(13),
            union_win: g(14),
            union_lsh: g(15),
            union_a: g(16),
            ex_sent: g(17),
        });
    }
    v.sort_by_key(|d| d.idx);
    Ok(v)
}

fn key_files(out: &Path) -> Result<Vec<PathBuf>> {
    store::files_with_suffix(&out.join("keys"), ".bin")
}

// ---------------------------------------------------------------- bloom

/// (member union set index, query excerpt set index, name).
const TYPES: [(usize, usize, &str); 5] = [(4, 4, "A1_chosen_excerpt"), (0, 4, "A_all_runs"), (1, 5, "B_sentences"), (2, 6, "W_winnow8"), (3, 7, "C_lsh_bands")];

fn bloom(out: &Path, fprs: &[f64], random_queries: u64) -> Result<()> {
    let t0 = Instant::now();
    let docs = load_docs(out)?;
    let member: Vec<bool> = docs.iter().map(|d| d.grank < HALF).collect();
    let files = key_files(out)?;
    let mut res = serde_json::Map::new();
    for (mset, qset, name) in TYPES {
        let mut want = [false; 8];
        want[mset] = true;
        want[qset] = true;
        let mut keys: Vec<u64> = Vec::new();
        let mut q_mem: Vec<Vec<u64>> = Vec::new();
        let mut q_non: Vec<Vec<u64>> = Vec::new();
        let (mut nm, mut nn) = (0u64, 0u64);
        for f in &files {
            lookup::read_docs(f, want, |i, s| {
                if member[i as usize] {
                    keys.extend_from_slice(&s[mset]);
                    q_mem.push(s[qset].clone());
                    nm += 1;
                } else {
                    q_non.push(s[qset].clone());
                    nn += 1;
                }
            })?;
        }
        let total = keys.len() as u64;
        keys.par_sort_unstable();
        keys.dedup();
        let distinct = keys.len() as u64;
        // Exact (no false positive) query outcomes: genuine text repetition.
        let exact = |q: &[Vec<u64>]| -> Value {
            let hits: Vec<(usize, usize)> = q.par_iter().map(|v| (v.iter().filter(|k| keys.binary_search(k).is_ok()).count(), v.len())).collect();
            doc_hit_summary(&hits)
        };
        let ex_mem = exact(&q_mem);
        let ex_non = exact(&q_non);
        let mut per = Vec::new();
        for &p in fprs {
            let mut b = Bloom::new(distinct, p);
            for &k in &keys {
                b.insert(k);
            }
            let fp = (0..random_queries)
                .into_par_iter()
                .filter(|&i| {
                    let k = splitmix(i ^ 0xF00D_0000_0000_0000);
                    b.contains(k) && keys.binary_search(&k).is_err()
                })
                .count() as f64
                / random_queries as f64;
            let bq = |q: &[Vec<u64>]| -> Value {
                let hits: Vec<(usize, usize)> = q.par_iter().map(|v| (v.iter().filter(|k| b.contains(**k)).count(), v.len())).collect();
                doc_hit_summary(&hits)
            };
            let sample: Vec<u8> = b.bits.iter().take(1 << 23).flat_map(|w| w.to_le_bytes()).collect();
            let z = zstd::bulk::compress(&sample, 19)?;
            per.push(json!({
                "fpr_target": p, "m_bits": b.m, "k": b.k, "bytes": b.bytes(), "bits_per_key": b.m as f64 / distinct as f64,
                "bits_per_member_doc": b.m as f64 / nm as f64, "ideal_bits_per_key": -p.log2(), "fill": b.fill(),
                "fpr_at_fill": b.fpr_at_fill(), "fpr_random_empirical": fp, "random_queries": random_queries,
                "zstd19_ratio_on_first_64MiB": z.len() as f64 / sample.len().max(1) as f64,
                "member_excerpts": bq(&q_mem), "nonmember_excerpts": bq(&q_non),
            }));
            eprintln!("bloom: {name} p={p} m={} k={} fp={fp:.5} ({:.0}s)", b.m, b.k, t0.elapsed().as_secs_f64());
        }
        res.insert(
            name.to_owned(),
            json!({"member_docs": nm, "nonmember_docs": nn, "keys_total": total, "keys_distinct": distinct,
                   "distinct_over_total": distinct as f64 / total.max(1) as f64, "distinct_keys_per_member_doc": distinct as f64 / nm.max(1) as f64,
                   "exact_member_excerpts": ex_mem, "exact_nonmember_excerpts": ex_non, "bloom": per}),
        );
    }
    store::write_json(&out.join("bloom.json"), &json!({"schema": "slop_ninja_cc_lookup_bloom_v1", "split": lookup::SPLIT_SEED, "types": res, "seconds": t0.elapsed().as_secs_f64()}))?;
    println!("bloom: done ({:.0}s)", t0.elapsed().as_secs_f64());
    Ok(())
}

/// Doc-level hit summary from (hits, query keys) per document.
fn doc_hit_summary(h: &[(usize, usize)]) -> Value {
    let n = h.len().max(1) as f64;
    let keys: usize = h.iter().map(|x| x.1).sum();
    let hits: usize = h.iter().map(|x| x.0).sum();
    let ge = |t: usize| h.iter().filter(|x| x.0 >= t).count() as f64 / n;
    let all = h.iter().filter(|x| x.1 > 0 && x.0 == x.1).count() as f64 / n;
    let fr: Vec<f64> = h.iter().filter(|x| x.1 > 0).map(|x| x.0 as f64 / x.1 as f64).collect();
    let frac_ge = |t: f64| fr.iter().filter(|&&x| x >= t).count() as f64 / n;
    json!({"docs": h.len(), "query_keys": keys, "key_hit_rate": hits as f64 / keys.max(1) as f64, "docs_ge1": ge(1), "docs_ge2": ge(2), "docs_ge3": ge(3),
           "docs_all_keys": all, "docs_frac_ge_0.5": frac_ge(0.5), "docs_frac_ge_0.9": frac_ge(0.9), "mean_frac": mean(&fr), "docs_no_keys": h.iter().filter(|x| x.1 == 0).count()})
}

// ---------------------------------------------------------------- background

const LEVELS: [f64; 4] = [1.0 / 16.0, 1.0 / 8.0, 0.25, 0.5];

fn level_of(g: u64) -> Option<u8> {
    LEVELS.iter().position(|f| (g as f64) < f * 18_446_744_073_709_551_616.0).map(|i| i as u8)
}

/// Query keys of one population: (keys, per-key length or 0).
struct Pop {
    name: String,
    docs: Vec<Vec<u64>>,
}

fn len_bin(n: usize) -> usize {
    match n {
        0..=9 => 0,
        10..=14 => 1,
        15..=24 => 2,
        _ => 3,
    }
}
const LEN_BINS: [&str; 4] = ["6-9", "10-14", "15-24", "25+"];

fn tierc_pops(rows: &[Row], kind: usize, mh: &MinHasher) -> Vec<Pop> {
    // kind: 0 sentences, 1 winnow, 2 lsh
    let keys_of = |t: &str| -> Vec<u64> {
        match kind {
            0 => lookup::sentence_keys(t),
            1 => lookup::winnow_keys(t),
            _ => lookup::lsh_keys(mh, t).map(|b| b.to_vec()).unwrap_or_default(),
        }
    };
    let mut h_cc = Vec::new();
    let mut h_other = Vec::new();
    let mut mirror = Vec::new();
    let mut full = Vec::new();
    let mut span_ai = Vec::new();
    for r in rows {
        match (r.origin.as_str(), r.operation.as_str()) {
            ("human_only", _) => {
                if cc_derived(r) {
                    h_cc.push(keys_of(&r.text))
                } else {
                    h_other.push(keys_of(&r.text))
                }
            }
            ("model_only", _) => mirror.push(keys_of(&r.text)),
            ("mixed", "full") => full.push(keys_of(&r.text)),
            ("mixed", _) => {
                if kind == 2 {
                    continue;
                }
                let spans = r.ai_byte_spans.clone().unwrap_or_default();
                let mut v = Vec::new();
                if kind == 0 {
                    for (k, a, z) in lookup::sentence_keys_spans(&r.text, lookup::MIN_SENT_TOKENS) {
                        if spans.iter().any(|&(x, y)| a >= x && z <= y) {
                            v.push(k);
                        }
                    }
                } else {
                    for &(x, y) in &spans {
                        if let Some(t) = r.text.get(x..y.min(r.text.len())) {
                            v.extend(lookup::winnow_keys(t));
                        }
                    }
                }
                span_ai.push(sorted_unique(v));
            }
            _ => {}
        }
    }
    vec![
        Pop { name: "tierc_human_non_cc".into(), docs: h_other },
        Pop { name: "tierc_human_cc_derived".into(), docs: h_cc },
        Pop { name: "tierc_ai_mirror".into(), docs: mirror },
        Pop { name: "tierc_ai_full_rewrite".into(), docs: full },
        Pop { name: "tierc_ai_span_text".into(), docs: span_ai },
    ]
}

fn background(out: &Path, rows_path: &Path) -> Result<()> {
    let t0 = Instant::now();
    let docs = load_docs(out)?;
    let lv: Vec<Option<u8>> = docs.iter().map(|d| level_of(d.grank)).collect();
    let mut n_level = [0u64; 4];
    for l in lv.iter().flatten() {
        for (j, n) in n_level.iter_mut().enumerate() {
            if (*l as usize) <= j {
                *n += 1;
            }
        }
    }
    let rows = load_rows(rows_path)?;
    let mh = MinHasher::new();
    let files = key_files(out)?;
    let mut res = serde_json::Map::new();
    for (kind, mset, qset, name) in [(0usize, 1usize, 5usize, "B_sentences"), (1, 2, 6, "W_winnow8"), (2, 3, 7, "C_lsh_bands")] {
        let mut want = [false; 8];
        want[mset] = true;
        want[qset] = true;
        let mut idx: Vec<(u64, u8)> = Vec::new();
        let mut cc_non: Vec<Vec<u64>> = Vec::new();
        for f in &files {
            lookup::read_docs(f, want, |i, s| match lv[i as usize] {
                Some(l) => idx.extend(s[mset].iter().map(|&k| (k, l))),
                None => cc_non.push(s[qset].clone()),
            })?;
        }
        idx.par_sort_unstable();
        idx.dedup_by(|b, a| a.0 == b.0); // keeps the first, which has the lowest level
        let keys: Vec<u64> = idx.iter().map(|x| x.0).collect();
        let lvls: Vec<u8> = idx.iter().map(|x| x.1).collect();
        let mut pops = vec![Pop { name: "cc_nonmember_excerpts".into(), docs: cc_non }];
        pops.extend(tierc_pops(&rows, kind, &mh));
        let mut pr = serde_json::Map::new();
        for p in &pops {
            // hits[j][doc] at level j; by length bin for sentences.
            let per_doc: Vec<([usize; 4], usize, [[usize; 2]; 16])> = p
                .docs
                .par_iter()
                .map(|v| {
                    let mut h = [0usize; 4];
                    let mut lb = [[0usize; 2]; 16];
                    for &k in v {
                        let b = len_bin(lookup::key_len(k));
                        let hit = keys.binary_search(&k).ok().map(|i| lvls[i] as usize);
                        for j in 0..4 {
                            let on = hit.is_some_and(|l| l <= j);
                            if on {
                                h[j] += 1;
                            }
                            if kind == 0 {
                                lb[j * 4 + b][0] += on as usize;
                                lb[j * 4 + b][1] += 1;
                            }
                        }
                    }
                    (h, v.len(), lb)
                })
                .collect();
            let mut lev = Vec::new();
            let mut rates = Vec::new();
            for (j, nl) in n_level.iter().enumerate() {
                let hh: Vec<(usize, usize)> = per_doc.iter().map(|x| (x.0[j], x.1)).collect();
                let s = doc_hit_summary(&hh);
                rates.push(s["key_hit_rate"].as_f64().unwrap_or(0.0));
                let mut by_len = serde_json::Map::new();
                if kind == 0 {
                    for (b, nm) in LEN_BINS.iter().enumerate() {
                        let (hit, tot) = per_doc.iter().fold((0, 0), |a, x| (a.0 + x.2[j * 4 + b][0], a.1 + x.2[j * 4 + b][1]));
                        by_len.insert((*nm).into(), json!({"keys": tot, "hit_rate": hit as f64 / tot.max(1) as f64}));
                    }
                }
                lev.push(json!({"fraction": LEVELS[j], "index_docs": nl, "summary": s, "sentence_hit_rate_by_tokens": by_len}));
            }
            // log-log slope of the key hit rate against index size.
            let pts: Vec<(f64, f64)> = n_level.iter().zip(&rates).filter(|(_, r)| **r > 0.0).map(|(n, r)| ((*n as f64).ln(), r.ln())).collect();
            let slope = if pts.len() >= 2 {
                let mx = mean(&pts.iter().map(|p| p.0).collect::<Vec<_>>());
                let my = mean(&pts.iter().map(|p| p.1).collect::<Vec<_>>());
                pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>() / pts.iter().map(|p| (p.0 - mx).powi(2)).sum::<f64>()
            } else {
                f64::NAN
            };
            pr.insert(p.name.clone(), json!({"docs": p.docs.len(), "levels": lev, "loglog_slope": slope, "rate_at_half": rates[3]}));
        }
        res.insert(name.to_owned(), json!({"index_keys_at_half": keys.len(), "populations": pr}));
        eprintln!("background: {name} ({:.0}s)", t0.elapsed().as_secs_f64());
    }
    store::write_json(
        &out.join("background.json"),
        &json!({"schema": "slop_ninja_cc_lookup_background_v1", "levels": LEVELS, "index_docs": n_level, "types": res, "seconds": t0.elapsed().as_secs_f64()}),
    )?;
    println!("background: done ({:.0}s)", t0.elapsed().as_secs_f64());
    Ok(())
}

// ---------------------------------------------------------------- nearmatch

/// Near-match signals of `child` against its human `parent`, as the TSV
/// columns from `q_sent` on (see the header in `nearmatch`).
fn signals(mh: &MinHasher, child: &str, parent: &str, spans: &[(usize, usize)], all_ai: bool) -> String {
    let ps = lookup::sentence_keys(parent);
    let mut q = Vec::new();
    let (mut ai_s, mut ai_hit, mut hum_s, mut hum_hit) = (0, 0, 0, 0);
    for (k, a, z) in lookup::sentence_keys_spans(child, lookup::MIN_SENT_TOKENS) {
        q.push(k);
        let hit = ps.binary_search(&k).is_ok();
        let in_ai = spans.iter().any(|&(x, y)| a >= x && z <= y);
        let touch = spans.iter().any(|&(x, y)| a < y && z > x);
        if all_ai || in_ai {
            ai_s += 1;
            ai_hit += hit as usize;
        } else if !touch {
            hum_s += 1;
            hum_hit += hit as usize;
        }
    }
    let qs = sorted_unique(q);
    let hit_s = count_in(&qs, &ps);
    let qw = lookup::winnow_keys(child);
    let pw = lookup::winnow_keys(parent);
    let hit_w = count_in(&qw, &pw);
    let (bands, mj) = match (lookup::minhash_sig(mh, child), lookup::minhash_sig(mh, parent)) {
        (Some(a), Some(b)) => {
            let (ba, bb) = (MinHasher::bands(&a), MinHasher::bands(&b));
            let bands = (0..MH_BANDS).filter(|&j| ba[j] == bb[j]).count();
            let eq = (0..MH_PERMS).filter(|&j| a[j] == b[j]).count();
            (bands, eq as f64 / MH_PERMS as f64)
        }
        _ => (0, 0.0),
    };
    let exact = (lookup::excerpt_key(child) == lookup::excerpt_key(parent)) as u8;
    format!(
        "{}\t{}\t{hit_s}\t{ai_s}\t{ai_hit}\t{hum_s}\t{hum_hit}\t{}\t{}\t{hit_w}\t{bands}\t{mj:.4}\t{exact}\t{}",
        qs.len(),
        ps.len(),
        qw.len(),
        pw.len(),
        text::words(child)
    )
}

const NM_HEAD: &str = "id\tpopulation\torigin\top\tdomain\tcc_derived\tparent\tshare\tq_sent\tp_sent\thit_sent\tai_sent\tai_sent_hit\thum_sent\thum_sent_hit\tq_win\tp_win\thit_win\tbands\tminhash_j\texact\twords\n";

fn nearmatch(out: &Path, rows_path: &Path) -> Result<()> {
    let rows = load_rows(rows_path)?;
    let by_id: HashMap<&str, &Row> = rows.iter().map(|r| (r.id.as_str(), r)).collect();
    let mh = MinHasher::new();
    let lines: Vec<String> = rows
        .par_iter()
        .map(|r| -> Result<String> {
            let parent = if r.origin == "human_only" {
                r
            } else {
                by_id.get(r.parent_id.as_deref().unwrap_or("")).copied().with_context(|| format!("{} has no parent row", r.id))?
            };
            let spans = r.ai_byte_spans.clone().unwrap_or_default();
            let all_ai = r.origin == "model_only" || r.operation == "full";
            Ok(format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.6}\t{}",
                r.id,
                r.population,
                r.origin,
                r.operation,
                r.domain,
                cc_derived(r) as u8,
                parent.id,
                r.share.unwrap_or(0.0),
                signals(&mh, &r.text, &parent.text, &spans, all_ai)
            ))
        })
        .collect::<Result<_>>()?;
    write_text(&out.join("nearmatch.tsv"), &(NM_HEAD.to_owned() + &lines.join("\n") + "\n"))?;
    println!("nearmatch: {} rows", lines.len());
    Ok(())
}

/// Synthetic light edits of every Tier C human document: each maximal
/// alphanumeric word is replaced, independently with probability `r`, by a
/// fresh token; punctuation and spacing are kept, so sentence boundaries do
/// not move. A stand-in for AI copy-editing of human text (the cohort
/// generator has no such operation today); labels would be AI-involved.
fn lightedit(out: &Path, rows_path: &Path, rates: &[f64]) -> Result<()> {
    let rows = load_rows(rows_path)?;
    let mh = MinHasher::new();
    let humans: Vec<&Row> = rows.iter().filter(|r| r.origin == "human_only").collect();
    let lines: Vec<String> = humans
        .par_iter()
        .flat_map_iter(|r| {
            let mh = &mh;
            rates.iter().map(move |&rate| {
                let seed = splitmix(text::h64(r.id.as_bytes()) ^ (rate * 1e6) as u64);
                let mut outt = String::with_capacity(r.text.len() + 64);
                let mut spans = Vec::new();
                let (mut words, mut changed) = (0usize, 0usize);
                let mut cur = String::new();
                let mut flush = |cur: &mut String, outt: &mut String, spans: &mut Vec<(usize, usize)>| {
                    if cur.is_empty() {
                        return;
                    }
                    words += 1;
                    if unif(seed, words as u64, 7) < rate {
                        changed += 1;
                        let a = outt.len();
                        outt.push_str(&format!("zq{:06x}", splitmix(seed ^ words as u64) & 0xFF_FFFF));
                        spans.push((a, outt.len()));
                    } else {
                        outt.push_str(cur);
                    }
                    cur.clear();
                };
                for c in r.text.chars() {
                    if c.is_alphanumeric() {
                        cur.push(c);
                    } else {
                        flush(&mut cur, &mut outt, &mut spans);
                        outt.push(c);
                    }
                }
                flush(&mut cur, &mut outt, &mut spans);
                let share = changed as f64 / words.max(1) as f64;
                format!(
                    "{}:le{rate}\tsynthetic\tmixed\tlight_edit_{rate}\t{}\t{}\t{}\t{share:.6}\t{}",
                    r.id,
                    r.domain,
                    cc_derived(r) as u8,
                    r.id,
                    signals(mh, &outt, &r.text, &spans, false)
                )
            })
        })
        .collect();
    write_text(&out.join("lightedit.tsv"), &(NM_HEAD.to_owned() + &lines.join("\n") + "\n"))?;
    println!("lightedit: {} rows", lines.len());
    Ok(())
}

// ---------------------------------------------------------------- frame

fn read_csv(p: &Path) -> Result<Vec<Vec<String>>> {
    Ok(std::fs::read_to_string(p)
        .with_context(|| format!("read {}", p.display()))?
        .lines()
        .map(|l| l.split(',').map(str::to_owned).collect())
        .collect())
}

const COHORT_DOMAINS: [&str; 9] = ["legal", "email", "essays", "fiction", "news", "reviews", "technical", "forum", "speech"];
/// (name, frame false-positive rate) for each lookup key strategy.
const STRATS: [(&str, f64); 5] = [("A1_chosen_excerpt", 1e-4), ("A_all_runs", 1e-4), ("B_sentences", 1e-2), ("W_winnow8", 1e-2), ("C_lsh_bands", 1e-2)];

fn keys_per_doc(d: &DocMeta, s: &str) -> f64 {
    match s {
        "A1_chosen_excerpt" => 1.0,
        "A_all_runs" => d.union_a,
        "B_sentences" => d.union_sent,
        "W_winnow8" => d.union_win,
        _ => d.union_lsh,
    }
}

/// Budgets in bytes: learned capacity of the released artifact at 2 and 3.6
/// bits per parameter, the raw artifact, and candidate caps.
fn budgets() -> Vec<(String, f64)> {
    let params = 149_879_643.0; // encoder 149,607,171 + heads ~272.5k
    vec![
        ("learned_2bpp_current".into(), params * 2.0 / 8.0),
        ("learned_3.6bpp_current".into(), params * 3.6 / 8.0),
        ("int8_artifact_384.5MB".into(), 384.5e6),
        ("fp32_artifact_610.6MB".into(), 610.6e6),
        ("cap_1GB".into(), 1e9),
        ("cap_2GB".into(), 2e9),
        ("cap_4GB".into(), 4e9),
        ("cap_8GB".into(), 8e9),
    ]
}

fn frame(out: &Path, meta: &Path, pilot: &Path) -> Result<()> {
    let docs = load_docs(out)?;
    // Per-crawl CC figures.
    let pc = std::fs::read_to_string(meta.join("per-crawl.tsv"))?;
    let mut crawls: Vec<(String, f64, f64, f64)> = Vec::new(); // crawl, captures (B), wet files, wet TiB
    for l in pc.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() >= 6 && f[0].starts_with("CC-MAIN-") {
            crawls.push((f[0].to_owned(), f[1].parse()?, f[4].parse()?, f[5].parse()?));
        }
    }
    let mut url: HashMap<String, f64> = HashMap::new();
    for r in read_csv(&meta.join("stats/monthly.csv"))?.iter().skip(1) {
        url.insert(r[0].clone(), r[3].parse().unwrap_or(f64::NAN));
    }
    let mut new: HashMap<String, f64> = HashMap::new();
    for r in read_csv(&meta.join("stats/monthly_new.csv"))?.iter().skip(1) {
        new.insert(r[0].clone(), r[1].parse().unwrap_or(f64::NAN));
    }
    let lastn = read_csv(&meta.join("stats/url_last_n_crawls.csv"))?;
    let ncols: Vec<usize> = lastn[0][1..].iter().map(|x| x.parse().unwrap_or(0)).collect();
    let mut union: HashMap<String, Vec<(usize, f64)>> = HashMap::new();
    for r in lastn.iter().skip(1) {
        let mut v: Vec<(usize, f64)> = ncols.iter().zip(&r[1..]).filter_map(|(n, x)| x.parse::<f64>().ok().filter(|y| y.is_finite()).map(|y| (*n, y))).collect();
        v.sort_by_key(|x| x.0);
        union.insert(r[0].clone(), v);
    }
    let union_at = |c: &str, n: usize| -> f64 {
        let v = &union[c];
        if let Some(x) = v.iter().find(|x| x.0 == n) {
            return x.1;
        }
        let lo = v.iter().rev().find(|x| x.0 < n).copied();
        let hi = v.iter().find(|x| x.0 > n).copied();
        match (lo, hi) {
            (Some(a), Some(b)) => a.1 + (b.1 - a.1) * (n - a.0) as f64 / (b.0 - a.0) as f64,
            (Some(a), None) => a.1,
            (None, Some(b)) => b.1,
            _ => f64::NAN,
        }
    };
    // Pilot yields per crawl and domain.
    let yt = std::fs::read_to_string(pilot.join("report/yield-by-crawl-domain.tsv"))?;
    let mut pgb: HashMap<String, f64> = HashMap::new();
    let mut padm: HashMap<(String, String), f64> = HashMap::new();
    let mut peng: HashMap<(String, String), f64> = HashMap::new();
    let mut prec: HashMap<String, f64> = HashMap::new();
    for l in yt.lines().skip(1) {
        let f: Vec<&str> = l.split('\t').collect();
        pgb.insert(f[0].to_owned(), f[3].parse()?);
        if f[4] == "ALL" {
            continue; // per-crawl totals; the per-domain rows are summed instead
        }
        let d: f64 = f[6].parse()?;
        match f[5] {
            "pool_dedup" => *padm.entry((f[0].to_owned(), f[4].to_owned())).or_default() += d,
            "english" => *peng.entry((f[0].to_owned(), f[4].to_owned())).or_default() += d,
            "records" => *prec.entry(f[0].to_owned()).or_default() += d,
            _ => {}
        }
    }
    let mut domains: Vec<String> = padm.keys().map(|k| k.1.clone()).collect::<HashSet<_>>().into_iter().collect();
    domains.sort();
    // Year membership.
    let mut year_crawls: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for (c, ..) in &crawls {
        year_crawls.entry(year_of(c)).or_default().push(c.clone());
    }
    let tib_gb = 1.099_511_627_776e3;
    let first = crawls.first().map(|c| c.0.clone()).unwrap_or_default();
    // Unique admitted docs by (domain, year): first-seen (URL novelty) and within-year distinct.
    let mut uniq: BTreeMap<(String, u32), f64> = BTreeMap::new();
    let mut ydist: BTreeMap<(String, u32), f64> = BTreeMap::new();
    let mut capt: BTreeMap<(String, u32), f64> = BTreeMap::new();
    let mut eng_uniq = 0.0;
    let mut records_frame = 0.0;
    let mut crawl_rows = Vec::new();
    for (y, cs) in &year_crawls {
        let last = cs.last().unwrap();
        let u_year = union_at(last, cs.len());
        let sum_url: f64 = cs.iter().map(|c| url[c]).sum();
        for c in cs {
            let (_, capb, files, tib) = crawls.iter().find(|x| &x.0 == c).unwrap();
            let elig = if c == "CC-MAIN-2022-49" { 22_880.0 / 88_000.0 } else { 1.0 };
            let gb = tib * tib_gb * elig;
            let u = if *c == first { 1.0 } else { new[c] / url[c] };
            let ygb = pgb[c];
            records_frame += prec[c] / ygb * gb;
            let mut adm_c = 0.0;
            for d in &domains {
                let a = padm.get(&(c.clone(), d.clone())).copied().unwrap_or(0.0) / ygb * gb;
                adm_c += a;
                *capt.entry((d.clone(), *y)).or_default() += a;
                *uniq.entry((d.clone(), *y)).or_default() += a * u;
                *ydist.entry((d.clone(), *y)).or_default() += a * (u_year / sum_url);
                eng_uniq += peng.get(&(c.clone(), d.clone())).copied().unwrap_or(0.0) / ygb * gb * u;
            }
            crawl_rows.push(json!({"crawl": c, "year": y, "captures_b": capb, "wet_files": files, "frame_wet_gb": gb, "novel_url_share": u,
                                   "admitted_captures": adm_c, "admitted_first_seen": adm_c * u, "pilot_wet_gb": ygb}));
        }
    }
    let total_uniq: f64 = uniq.values().sum();
    let total_capt: f64 = capt.values().sum();
    // Keys per doc by domain (pilot means).
    let mut kpd: BTreeMap<(String, String), f64> = BTreeMap::new();
    let mut words_pd: BTreeMap<String, f64> = BTreeMap::new();
    let mut per_dom_n: BTreeMap<String, f64> = BTreeMap::new();
    for d in &docs {
        *per_dom_n.entry(d.domain.clone()).or_default() += 1.0;
        *words_pd.entry(d.domain.clone()).or_default() += d.union_words;
        for (s, _) in STRATS {
            *kpd.entry((d.domain.clone(), s.to_owned())).or_default() += keys_per_doc(d, s);
        }
    }
    for ((dom, _), v) in kpd.iter_mut() {
        *v /= per_dom_n[dom];
    }
    for (dom, v) in words_pd.iter_mut() {
        *v /= per_dom_n[dom];
    }
    let all_mean = |s: &str| mean(&docs.iter().map(|d| keys_per_doc(d, s)).collect::<Vec<_>>());
    let mut dom_rows = serde_json::Map::new();
    for d in &domains {
        let u: f64 = uniq.iter().filter(|(k, _)| &k.0 == d).map(|x| x.1).sum();
        let mut ks = serde_json::Map::new();
        for (s, _) in STRATS {
            ks.insert(s.into(), json!(kpd[&(d.clone(), s.to_owned())]));
        }
        let dd: Vec<&DocMeta> = docs.iter().filter(|x| &x.domain == d).collect();
        let m = |f: fn(&DocMeta) -> f64| mean(&dd.iter().map(|x| f(x)).collect::<Vec<_>>());
        dom_rows.insert(
            d.clone(),
            json!({"unique_docs": u, "pilot_docs": per_dom_n.get(d), "union_words_per_doc": words_pd.get(d), "excerpt_words": m(|x| x.words),
                   "valid_runs": m(|x| x.n_runs), "union_sentences_any_length": m(|x| x.sent_any), "excerpt_sentences_ge6": m(|x| x.ex_sent),
                   "keys_per_doc": ks}),
        );
    }
    let mut year_rows = serde_json::Map::new();
    for y in year_crawls.keys() {
        let u: f64 = uniq.iter().filter(|(k, _)| k.1 == *y).map(|x| x.1).sum();
        let yd: f64 = ydist.iter().filter(|(k, _)| k.1 == *y).map(|x| x.1).sum();
        let ca: f64 = capt.iter().filter(|(k, _)| k.1 == *y).map(|x| x.1).sum();
        year_rows.insert(y.to_string(), json!({"first_seen_unique": u, "distinct_within_year": yd, "admitted_captures": ca}));
    }
    // Index size vs coverage (uniform over unique docs).
    let covs = [0.01, 0.02, 0.05, 0.1, 0.25, 0.5, 1.0];
    let fprs: [f64; 3] = [0.001, 0.01, 0.05];
    let mut size_rows = Vec::new();
    let mut tsv = String::from("strategy\tfpr\tkeys_frame\tbloom_bytes_per_doc\tcov_1pct\tcov_2pct\tcov_5pct\tcov_10pct\tcov_25pct\tcov_50pct\tcov_100pct\n");
    let frame_keys = |s: &str| -> f64 { domains.iter().map(|d| uniq.iter().filter(|(k, _)| &k.0 == d).map(|x| x.1).sum::<f64>() * kpd[&(d.clone(), s.to_owned())]).sum() };
    for (s, _) in STRATS {
        let kf = frame_keys(s);
        for &p in &fprs {
            let bits_key = 1.44 * (1.0 / p).log2();
            let sizes: Vec<f64> = covs.iter().map(|c| c * kf * bits_key / 8.0).collect();
            tsv.push_str(&format!("{s}\t{p}\t{kf:.4e}\t{:.2}", kf / total_uniq * bits_key / 8.0));
            for z in &sizes {
                tsv.push_str(&format!("\t{}", human_bytes(*z)));
            }
            tsv.push('\n');
            size_rows.push(json!({"strategy": s, "fpr": p, "keys_frame": kf, "bloom_bits_per_key": bits_key, "ideal_bits_per_key": (1.0/p).log2(), "bytes_at_coverage": covs.iter().zip(&sizes).map(|(c, z)| json!({"coverage": c, "bloom_bytes": z, "ideal_bytes": z / 1.44})).collect::<Vec<_>>()}));
        }
    }
    write_text(&out.join("frame-index-sizes.tsv"), &tsv)?;
    // Coverage per budget under each sampling design (greedy over strata).
    let strata: Vec<(String, u32)> = uniq.keys().cloned().collect();
    let design = |name: &str| -> Vec<(f64, f64, (String, u32))> {
        // (mass, unique docs the index must cover, stratum)
        strata
            .iter()
            .map(|k| {
                let (d, y) = (&k.0, k.1);
                let cohort = COHORT_DOMAINS.contains(&d.as_str());
                let u = uniq[k];
                let yd = ydist[k];
                let dom_u: f64 = uniq.iter().filter(|(kk, _)| &kk.0 == d).map(|x| x.1).sum();
                let yr_cap: f64 = capt.iter().filter(|(kk, _)| kk.1 == y).map(|x| x.1).sum();
                let (m, cost) = match name {
                    "uniform_unique" => (u / total_uniq, u),
                    "capture_proportional" => (capt[k] / total_capt, u),
                    "year_equal" => (0.1 * capt[k] / yr_cap, yd),
                    "domain_equal_unique" => (if cohort { u / dom_u / 9.0 } else { 0.0 }, u),
                    _ => (if cohort { 1.0 / 90.0 } else { 0.0 }, yd),
                };
                (m, cost, k.clone())
            })
            .collect()
    };
    let designs = ["uniform_unique", "capture_proportional", "year_equal", "domain_equal_unique", "domain_and_year_equal"];
    let mut cov_rows = Vec::new();
    let mut ctsv = String::from("design\tstrategy\tbudget\tbytes\tcoverage\tbest_strata\n");
    for dn in designs {
        let st = design(dn);
        let msum: f64 = st.iter().map(|x| x.0).sum();
        for (s, p) in STRATS {
            let bits_key = 1.44 * (1.0 / p).log2();
            // cost in bytes per stratum
            let mut items: Vec<(f64, f64, String)> = st
                .iter()
                .filter(|x| x.0 > 0.0)
                .map(|(m, cost, k)| (*m / msum, cost * kpd[&(k.0.clone(), s.to_owned())] * bits_key / 8.0, format!("{}:{}", k.0, k.1)))
                .collect();
            items.sort_by(|a, b| (b.0 / b.1).partial_cmp(&(a.0 / a.1)).unwrap());
            for (bn, bytes) in budgets() {
                let mut left = bytes;
                let mut cov = 0.0;
                let mut used = Vec::new();
                for (m, c, k) in &items {
                    if left <= 0.0 {
                        break;
                    }
                    let f = (left / c).min(1.0);
                    cov += m * f;
                    left -= c * f;
                    if used.len() < 3 {
                        used.push(format!("{k}({:.0}%)", f * 100.0));
                    }
                }
                ctsv.push_str(&format!("{dn}\t{s}\t{bn}\t{bytes:.3e}\t{cov:.5}\t{}\n", used.join(",")));
                cov_rows.push(json!({"design": dn, "strategy": s, "fpr": p, "budget": bn, "bytes": bytes, "coverage": cov, "first_strata": used}));
            }
        }
    }
    write_text(&out.join("frame-coverage-by-budget.tsv"), &ctsv)?;
    let mut strata_rows = Vec::new();
    for k in &strata {
        strata_rows.push(json!({"domain": k.0, "year": k.1, "first_seen_unique": uniq[k], "distinct_within_year": ydist[k], "admitted_captures": capt[k]}));
    }
    store::write_json(
        &out.join("frame.json"),
        &json!({"schema": "slop_ninja_cc_lookup_frame_v1",
                "records_frame": records_frame, "admitted_captures": total_capt, "unique_admitted_docs": total_uniq,
                "unique_english_candidates": eng_uniq, "captures_per_unique": total_capt / total_uniq,
                "keys_per_doc_all": STRATS.iter().map(|(s, _)| (s.to_string(), json!(all_mean(s)))).collect::<serde_json::Map<_, _>>(),
                "keys_frame": STRATS.iter().map(|(s, _)| (s.to_string(), json!(frame_keys(s)))).collect::<serde_json::Map<_, _>>(),
                "per_domain": dom_rows, "per_year": year_rows, "strata": strata_rows, "per_crawl": crawl_rows,
                "index_sizes": size_rows, "coverage_by_budget": cov_rows,
                "budgets": budgets().iter().map(|(n, b)| json!({"name": n, "bytes": b})).collect::<Vec<_>>()}),
    )?;
    println!("frame: unique admitted docs {total_uniq:.4e}, captures {total_capt:.4e}");
    Ok(())
}

fn human_bytes(b: f64) -> String {
    if b >= 1e12 {
        format!("{:.1} TB", b / 1e12)
    } else if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{:.1} kB", b / 1e3)
    }
}

// ---------------------------------------------------------------- gain

#[derive(Clone)]
struct Nm {
    origin: u8, // 0 human, 1 mixed, 2 model_only
    op: String,
    parent: String,
    share: f64,
    q_sent: usize,
    hit_sent: usize,
    hum_sent: usize,
    q_win: usize,
    hit_win: usize,
    bands: usize,
    a: f64,
    w: f64,
}

fn load_nm(out: &Path, preds: &Path, mix: [f64; 3], le: Option<(f64, f64)>) -> Result<Vec<Nm>> {
    let mut p: HashMap<String, f64> = HashMap::new();
    for l in std::io::BufReader::new(std::fs::File::open(preds)?).lines() {
        let v: Value = serde_json::from_str(&l?)?;
        p.insert(v["id"].as_str().unwrap_or("").to_owned(), 1.0 - v["p_human"].as_f64().unwrap_or(f64::NAN));
    }
    let mut s = std::fs::read_to_string(out.join("nearmatch.tsv")).context("run nearmatch first")?;
    if let Some((rate, _)) = le {
        let want = format!("light_edit_{rate}");
        let t = std::fs::read_to_string(out.join("lightedit.tsv")).context("run lightedit first")?;
        // Rows where no word changed are identical to the parent and are dropped.
        let rows: Vec<&str> = t
            .lines()
            .skip(1)
            .filter(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                f.get(3) == Some(&want.as_str()) && f.get(7).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0) > 0.0
            })
            .collect();
        ensure!(!rows.is_empty(), "no {want} rows in lightedit.tsv");
        s.push_str(&(rows.join("\n") + "\n"));
    }
    let mut v = Vec::new();
    for l in s.lines().skip(1) {
        let f: Vec<&str> = l.split('\t').collect();
        let origin = match f[2] {
            "human_only" => 0,
            "mixed" => 1,
            _ => 2,
        };
        // A synthetic light edit gets its human parent's prediction: the
        // detector without lookup is assumed not to see the edit (stated limit).
        let pid = if f[1] == "synthetic" { f[6] } else { f[0] };
        let a = *p.get(pid).with_context(|| format!("no prediction for {pid}"))?;
        v.push(Nm {
            origin,
            op: f[3].to_owned(),
            parent: f[6].to_owned(),
            share: f[7].parse()?,
            q_sent: f[8].parse()?,
            hit_sent: f[10].parse()?,
            hum_sent: f[13].parse()?,
            q_win: f[15].parse()?,
            hit_win: f[17].parse()?,
            bands: f[18].parse()?,
            a,
            w: 0.0,
        });
    }
    let synth = |d: &Nm| d.op.starts_with("light_edit");
    let mut cnt = [0f64; 4];
    for d in &v {
        cnt[if synth(d) { 3 } else { d.origin as usize }] += 1.0;
    }
    let phi_le = le.map(|x| x.1).unwrap_or(0.0);
    for d in v.iter_mut() {
        d.w = if synth(d) {
            mix[1] * phi_le / cnt[3]
        } else if d.origin == 1 {
            mix[1] * (1.0 - phi_le) / cnt[1]
        } else {
            mix[d.origin as usize] / cnt[d.origin as usize]
        };
    }
    Ok(v)
}

/// Background (non-indexed) per-key hit rates at the lookup index size;
/// `human` and `ai` are equal (phrase level) in the current model.
#[derive(Clone, Copy, Default)]
struct Bg {
    human: [f64; 3], // B, W, C
    ai: [f64; 3],
}

/// `mode`: "fit" extrapolates the pilot's log-log growth to the lookup
/// index (coverage x frame docs), capped at `cap`; "pilot" uses the rates
/// measured at the pilot's half index; "none" sets every rate to zero.
fn bg_rates(out: &Path, cov: f64, mode: &str, cap: f64) -> Result<(Bg, Value)> {
    let b: Value = store::read_json(&out.join("background.json"))?;
    let f: Value = store::read_json(&out.join("frame.json"))?;
    let n_frame = f["unique_admitted_docs"].as_f64().unwrap_or(f64::NAN);
    let n_half = b["index_docs"][3].as_f64().unwrap_or(f64::NAN);
    let mut g = Bg::default();
    let mut info = serde_json::Map::new();
    for (i, t) in ["B_sentences", "W_winnow8", "C_lsh_bands"].iter().enumerate() {
        let pops = &b["types"][t]["populations"];
        let ext = |name: &str| -> (f64, f64, f64) {
            let r = pops[name]["rate_at_half"].as_f64().unwrap_or(0.0);
            let s = pops[name]["loglog_slope"].as_f64().filter(|x| x.is_finite()).unwrap_or(1.0).clamp(0.0, 1.0);
            let scale = (cov * n_frame / n_half).max(0.0);
            let v = match mode {
                "none" => 0.0,
                "pilot" => r,
                _ => (r * scale.powf(s)).min(cap),
            };
            (v, r, s)
        };
        let h = ext("cc_nonmember_excerpts");
        let ai_names: &[&str] = if i == 2 { &["tierc_ai_mirror", "tierc_ai_full_rewrite"] } else { &["tierc_ai_mirror", "tierc_ai_full_rewrite", "tierc_ai_span_text"] };
        let ai: Vec<(f64, f64, f64)> = ai_names.iter().map(|n| ext(n)).collect();
        let ai_max = ai.iter().map(|x| x.0).fold(0.0, f64::max);
        // Phrase-level background applies to human and AI text alike. The
        // AI populations have no near-copies in CC, so their rate is the
        // phrase-level rate; the CC human rate also contains near-duplicate
        // pages, which act as coverage, not as noise (reported only).
        g.human[i] = ai_max;
        g.ai[i] = ai_max;
        info.insert(t.to_string(), json!({"human_rate": h.0, "human_rate_at_half": h.1, "human_slope": h.2, "ai_rate": ai_max, "ai_detail": ai.iter().zip(ai_names).map(|(x, n)| json!({"pop": n, "rate": x.0, "rate_at_half": x.1, "slope": x.2})).collect::<Vec<_>>()}));
    }
    Ok((g, json!({"coverage": cov, "mode": mode, "cap": cap, "n_frame": n_frame, "n_half": n_half, "types": info})))
}

#[derive(Clone, Copy, PartialEq)]
enum Pol {
    Cal,
    Strict,
    Loose,
}

/// Bucket of document `i` for strategy `s` (0 = no lookup evidence).
fn bucket(s: &str, d: &Nm, covered: bool, bg: &Bg, p: f64, seed: u64, i: u64) -> usize {
    let p_a = 1e-4;
    let frac_bucket = |hit: usize, q: usize, edges: &[f64]| -> usize {
        if hit == 0 || q == 0 {
            return 0;
        }
        if hit == 1 {
            return 1;
        }
        let f = hit as f64 / q as f64;
        if f >= 1.0 - 1e-12 {
            return edges.len() + 3;
        }
        2 + edges.iter().filter(|&&e| f >= e).count()
    };
    match s {
        "A1_chosen_excerpt" | "A_all_runs" => {
            let hit = (covered && d.origin == 0) || unif(seed, i, 1) < p_a;
            hit as usize
        }
        "B_sentences" => {
            let (true_hits, hum_part, ai_part) = if covered {
                (d.hit_sent, 0, d.q_sent - d.hit_sent.min(d.q_sent))
            } else if d.origin == 0 {
                (0, d.q_sent, 0)
            } else {
                (0, d.hum_sent.min(d.q_sent), d.q_sent - d.hum_sent.min(d.q_sent))
            };
            let n = true_hits + binom(hum_part, bg.human[0] + p, seed, i * 4) + binom(ai_part, bg.ai[0] + p, seed, i * 4 + 1);
            frac_bucket(n, d.q_sent, &[0.25, 0.75])
        }
        "W_winnow8" => {
            let hum = ((1.0 - d.share) * d.q_win as f64).round() as usize;
            let (true_hits, hum_part, ai_part) = if covered {
                (d.hit_win, 0, d.q_win - d.hit_win.min(d.q_win))
            } else if d.origin == 0 {
                (0, d.q_win, 0)
            } else {
                (0, hum.min(d.q_win), d.q_win - hum.min(d.q_win))
            };
            let n = true_hits + binom(hum_part, bg.human[1] + p, seed, i * 4 + 2) + binom(ai_part, bg.ai[1] + p, seed, i * 4 + 3);
            frac_bucket(n, d.q_win, &[0.1, 0.5, 0.9, 0.99])
        }
        _ => {
            let (true_hits, rate) = if covered {
                (d.bands, if d.origin == 0 { bg.human[2] } else { bg.ai[2] })
            } else {
                (0, if d.origin == 0 { bg.human[2] } else { bg.ai[2] })
            };
            let n = true_hits + binom(MH_BANDS - true_hits.min(MH_BANDS), rate + p, seed, i * 4 + 5);
            match n {
                0 => 0,
                1..=2 => 1,
                3..=7 => 2,
                8..=12 => 3,
                13..=15 => 4,
                _ => 5,
            }
        }
    }
}

fn top_buckets(s: &str) -> (usize, usize) {
    match s {
        "A1_chosen_excerpt" | "A_all_runs" => (1, 1),
        "B_sentences" => (5, 4),
        "W_winnow8" => (7, 6),
        _ => (5, 4),
    }
}

/// Cross-fitted policy: the miner keeps its detector score `a` and applies
/// the lookup bucket as evidence, `odds(q) = odds(a) x LR_b`, with LR_b the
/// smoothed likelihood ratio of bucket `b` estimated on the other fold.
/// With no lookup (every bucket 0) this returns `a` exactly.
fn fit_eval(docs: &[Nm], buckets: &[usize], fold: &[u8], pol: Pol, s: &str) -> Vec<f64> {
    const NB: usize = 8;
    let mut w = [[[0f64; 2]; NB]; 2]; // fold, bucket, class
    for (i, d) in docs.iter().enumerate() {
        w[fold[i] as usize][buckets[i]][(d.origin != 0) as usize] += d.w;
    }
    let wbar = docs.iter().map(|d| d.w).sum::<f64>() / docs.len() as f64;
    let k = 0.5 * wbar;
    let lr: Vec<[f64; NB]> = (0..2)
        .map(|f| {
            let t0: f64 = (0..NB).map(|b| w[f][b][0]).sum();
            let t1: f64 = (0..NB).map(|b| w[f][b][1]).sum();
            let mut v = [1f64; NB];
            for (b, x) in v.iter_mut().enumerate() {
                if w[f][b][0] + w[f][b][1] > 0.0 {
                    *x = ((w[f][b][1] + k) / (t1 + k * NB as f64)) / ((w[f][b][0] + k) / (t0 + k * NB as f64));
                }
            }
            v
        })
        .collect();
    let (top, loose) = top_buckets(s);
    let all_zero = buckets.iter().all(|&x| x == 0);
    docs.iter()
        .enumerate()
        .map(|(i, d)| {
            let b = buckets[i];
            match pol {
                Pol::Strict => {
                    if b == top && b > 0 { 0.001 } else { d.a }
                }
                Pol::Loose => {
                    if b >= loose && b > 0 { 0.001 } else { d.a }
                }
                Pol::Cal => {
                    if all_zero {
                        return d.a;
                    }
                    let l = lr[1 - fold[i] as usize][b];
                    let a = d.a.clamp(1e-6, 1.0 - 1e-6);
                    let o = a / (1.0 - a) * l;
                    (o / (1.0 + o)).clamp(1e-4, 1.0 - 1e-4)
                }
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn gain(out: &Path, preds: &Path, mix: [f64; 3], covs: &[f64], reps: u64, p: f64, tag: &str, le: Option<(f64, f64)>, bgm: (&str, f64)) -> Result<()> {
    let t0 = Instant::now();
    let docs = load_nm(out, preds, mix, le)?;
    let n = docs.len();
    let fold: Vec<u8> = docs.iter().map(|d| (rank(lookup::SPLIT_SEED, "gain_fold", &d.parent) >> 63) as u8).collect();
    let y: Vec<f64> = docs.iter().map(|d| (d.origin != 0) as u8 as f64).collect();
    let loss = |q: &[f64]| -> f64 { (0..n).map(|i| docs[i].w * (q[i] - y[i]).powi(2)).sum() };
    let raw = loss(&docs.iter().map(|d| d.a).collect::<Vec<_>>());
    let zero_b = vec![0usize; n];
    let base_q = fit_eval(&docs, &zero_b, &fold, Pol::Cal, "B_sentences");
    let base = loss(&base_q);
    let groups: Vec<String> = docs.iter().map(|d| if d.origin == 0 { "human_only".into() } else { format!("{}:{}", if d.origin == 1 { "mixed" } else { "model_only" }, d.op) }).collect();
    let gnames: Vec<String> = {
        let mut g: Vec<String> = groups.iter().cloned().collect::<HashSet<_>>().into_iter().collect();
        g.sort();
        g
    };
    let strategies = ["A1_chosen_excerpt", "A_all_runs", "B_sentences", "W_winnow8", "C_lsh_bands"];
    let mut rows = Vec::new();
    let mut per_doc_c1: HashMap<String, Vec<f64>> = HashMap::new();
    let mut tsv = String::from("strategy\tpolicy\tcoverage\tdelta_brier\tdelta_pct_B0\tsd_reps\tper_unit_coverage_pct_B0\tfalse_human_covered_ai\tai_call_covered_human\n");
    for &c in covs {
        let (bg, bginfo) = bg_rates(out, c, bgm.0, bgm.1)?;
        for s in strategies {
            for pol in [Pol::Cal, Pol::Strict, Pol::Loose] {
                if s.starts_with('A') && pol != Pol::Cal {
                    continue;
                }
                let mut deltas = Vec::new();
                let mut by_group: BTreeMap<String, f64> = BTreeMap::new();
                let (mut fh_num, mut fh_den, mut ac_num, mut ac_den, mut unc_fh) = (0.0, 0.0, 0.0, 0.0, 0.0);
                let mut doc_gain = vec![0f64; n];
                let (mut fam_cov, mut fam_unc): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
                for r in 0..reps {
                    let seed = splitmix(0xC0DE ^ r.wrapping_mul(7919) ^ (c * 1e6) as u64);
                    let covered: Vec<bool> = docs.iter().map(|d| c >= 1.0 || (rank(lookup::SPLIT_SEED, &format!("cover:{r}"), &d.parent) as f64) < c * 18_446_744_073_709_551_616.0).collect();
                    let b: Vec<usize> = (0..n).map(|i| bucket(s, &docs[i], covered[i], &bg, p, seed, i as u64)).collect();
                    let q = fit_eval(&docs, &b, &fold, pol, s);
                    deltas.push(base - loss(&q));
                    // Family statistic (0.5 x human gain + 0.5 x mean mixed-child gain), by coverage.
                    let mut fam: HashMap<&str, (f64, f64, f64, bool)> = HashMap::new();
                    for i in 0..n {
                        let g = (base_q[i] - y[i]).powi(2) - (q[i] - y[i]).powi(2);
                        let e = fam.entry(docs[i].parent.as_str()).or_insert((0.0, 0.0, 0.0, covered[i]));
                        if docs[i].origin == 0 {
                            e.0 = g;
                        } else if docs[i].origin == 1 {
                            e.1 += g;
                            e.2 += 1.0;
                        }
                    }
                    for (h, m, k, cv) in fam.values() {
                        let v = if *k > 0.0 { 0.5 * h + 0.5 * m / k } else { *h };
                        if *cv {
                            fam_cov.push(v);
                        } else {
                            fam_unc.push(v);
                        }
                    }
                    for i in 0..n {
                        let g = docs[i].w * ((base_q[i] - y[i]).powi(2) - (q[i] - y[i]).powi(2));
                        *by_group.entry(groups[i].clone()).or_default() += g / reps as f64;
                        doc_gain[i] += g / docs[i].w / reps as f64;
                        // Flips the lookup causes: an AI document the detector
                        // called AI (a >= 0.5) now called human, and the reverse.
                        if covered[i] && y[i] == 1.0 {
                            fh_den += docs[i].w;
                            if docs[i].a >= 0.5 && q[i] < 0.5 {
                                fh_num += docs[i].w;
                            }
                        }
                        if !covered[i] && y[i] == 1.0 && docs[i].a >= 0.5 && q[i] < 0.5 {
                            unc_fh += docs[i].w;
                        }
                        if covered[i] && y[i] == 0.0 {
                            ac_den += docs[i].w;
                            if docs[i].a < 0.5 && q[i] >= 0.5 {
                                ac_num += docs[i].w;
                            }
                        }
                    }
                }
                let m = mean(&deltas);
                let polname = match pol {
                    Pol::Cal => "calibrated",
                    Pol::Strict => "strict_top_bucket_human",
                    Pol::Loose => "loose_top2_buckets_human",
                };
                if (c - 1.0).abs() < 1e-9 {
                    per_doc_c1.insert(format!("{s}:{polname}"), doc_gain.clone());
                }
                tsv.push_str(&format!(
                    "{s}\t{polname}\t{c}\t{m:.6}\t{:.3}\t{:.6}\t{:.3}\t{:.4}\t{:.4}\n",
                    100.0 * m / base,
                    sd(&deltas),
                    100.0 * m / base / c,
                    fh_num / fh_den.max(1e-300),
                    ac_num / ac_den.max(1e-300)
                ));
                rows.push(json!({"strategy": s, "policy": polname, "coverage": c, "delta_brier": m, "delta_pct_b0": 100.0 * m / base, "sd_reps": sd(&deltas),
                                 "per_unit_coverage_pct_b0": 100.0 * m / base / c, "false_human_rate_covered_ai": fh_num / fh_den.max(1e-300),
                                 "ai_call_rate_covered_human": ac_num / ac_den.max(1e-300), "false_human_mass_uncovered_ai": unc_fh / reps as f64,
                                 "family_gain_covered": mean(&fam_cov), "family_gain_uncovered": mean(&fam_unc), "family_sd_covered": sd(&fam_cov),
                                 "by_group_delta": gnames.iter().map(|g| (g.clone(), json!(by_group.get(g).copied().unwrap_or(0.0)))).collect::<serde_json::Map<_, _>>(),
                                 "background": bginfo["types"].clone()}));
            }
        }
        eprintln!("gain: coverage {c} ({:.0}s)", t0.elapsed().as_secs_f64());
    }
    // Parent-bootstrap CI of the full-coverage gain.
    let parents: Vec<String> = {
        let mut v: Vec<String> = docs.iter().map(|d| d.parent.clone()).collect::<HashSet<_>>().into_iter().collect();
        v.sort();
        v
    };
    let pidx: HashMap<&str, usize> = parents.iter().enumerate().map(|(i, p)| (p.as_str(), i)).collect();
    let mut boot = serde_json::Map::new();
    for (k, g) in &per_doc_c1 {
        let mut fam = vec![0f64; parents.len()];
        for (i, d) in docs.iter().enumerate() {
            fam[pidx[d.parent.as_str()]] += d.w * g[i];
        }
        let np = parents.len();
        let mut bs: Vec<f64> = (0..400u64)
            .map(|b| (0..np).map(|j| fam[(unif(0xB007, b, j as u64) * np as f64) as usize % np]).sum::<f64>() / base * 100.0)
            .collect();
        let lo = quantile(&mut bs, 0.025);
        let hi = quantile(&mut bs, 0.975);
        // Family-level statistic for the canary: mean of the human document's gain and the mean gain of its mixed children.
        let mut fh: HashMap<usize, (f64, f64, f64)> = HashMap::new();
        for (i, d) in docs.iter().enumerate() {
            let e = fh.entry(pidx[d.parent.as_str()]).or_default();
            if d.origin == 0 {
                e.0 = g[i];
            } else if d.origin == 1 {
                e.1 += g[i];
                e.2 += 1.0;
            }
        }
        let fam_stat: Vec<f64> = fh.values().map(|(h, m, k)| if *k > 0.0 { 0.5 * h + 0.5 * m / k } else { *h }).collect();
        boot.insert(k.clone(), json!({"pct_b0_ci95": [lo, hi], "family_stat_mean_gain": mean(&fam_stat), "family_stat_sd": sd(&fam_stat)}));
    }
    write_text(&out.join(format!("{tag}.tsv")), &tsv)?;
    store::write_json(
        &out.join(format!("{tag}.json")),
        &json!({"schema": "slop_ninja_cc_lookup_gain_v1", "mix": mix, "fpr": p, "bg_mode": bgm.0, "bg_cap": bgm.1, "light_edit": le.map(|x| json!({"rate": x.0, "share_of_mixed": x.1})), "reps": reps, "docs": n, "B0_raw": raw, "B0_crossfit_recalibrated": base,
                "groups": gnames, "rows": rows, "full_coverage_bootstrap": boot, "seconds": t0.elapsed().as_secs_f64()}),
    )?;
    println!("gain: B0 raw {raw:.5}, recalibrated {base:.5} ({:.0}s)", t0.elapsed().as_secs_f64());
    Ok(())
}

// ---------------------------------------------------------------- canary

fn canary(out: &Path, pred_dir: &Path, gain_tag: &str, at: f64) -> Result<()> {
    let s = std::fs::read_to_string(out.join("nearmatch.tsv"))?;
    let mut meta: Vec<(String, u8, String, String)> = Vec::new(); // id, origin, parent, domain
    for l in s.lines().skip(1) {
        let f: Vec<&str> = l.split('\t').collect();
        let o = match f[2] {
            "human_only" => 0,
            "mixed" => 1,
            _ => 2,
        };
        meta.push((f[0].to_owned(), o, f[6].to_owned(), f[4].to_owned()));
    }
    let load = |name: &str| -> Result<HashMap<String, f64>> {
        let mut p = HashMap::new();
        for l in std::io::BufReader::new(std::fs::File::open(pred_dir.join(format!("{name}.jsonl")))?).lines() {
            let v: Value = serde_json::from_str(&l?)?;
            p.insert(v["id"].as_str().unwrap_or("").to_owned(), 1.0 - v["p_human"].as_f64().unwrap_or(f64::NAN));
        }
        Ok(p)
    };
    let r0 = load("Faux-17")?;
    let gain: Value = store::read_json(&out.join(format!("{gain_tag}.json")))?;
    let b0 = gain["B0_crossfit_recalibrated"].as_f64().unwrap_or(f64::NAN);
    let mut no_lookup = serde_json::Map::new();
    let mut sig: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for name in ["Faux-29", "Faux-41", "E0-17", "V6-17", "S3A-17"] {
        let Ok(pj) = load(name) else { continue };
        // family statistic of the paired utility difference u_j - u_0 = (a0-y)^2 - (aj-y)^2
        let mut fam: HashMap<String, (f64, f64, f64, String)> = HashMap::new();
        let mut missing = 0;
        for (id, o, parent, dom) in &meta {
            let (Some(a0), Some(aj)) = (r0.get(id), pj.get(id)) else {
                missing += 1;
                continue;
            };
            let y = (*o != 0) as u8 as f64;
            let d = (a0 - y).powi(2) - (aj - y).powi(2);
            let e = fam.entry(parent.clone()).or_insert((f64::NAN, 0.0, 0.0, dom.clone()));
            if *o == 0 {
                e.0 = d;
            } else if *o == 1 {
                e.1 += d;
                e.2 += 1.0;
            }
        }
        let fs: Vec<(f64, String)> = fam
            .values()
            .filter(|v| v.0.is_finite())
            .map(|(h, m, k, dom)| (if *k > 0.0 { 0.5 * h + 0.5 * m / k } else { *h }, dom.clone()))
            .collect();
        if fs.len() < 1000 {
            continue;
        }
        let vals: Vec<f64> = fs.iter().map(|x| x.0).collect();
        let sigma = sd(&vals);
        let mut by_dom: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for (v, d) in &fs {
            by_dom.entry(d.clone()).or_default().push(*v);
        }
        let dm: Vec<f64> = by_dom.values().map(|v| mean(v)).collect();
        let noise = mean(&by_dom.values().map(|v| sd(v).powi(2) / v.len() as f64).collect::<Vec<_>>());
        let between = sd(&dm).powi(2);
        let tau = (between - noise).max(0.0).sqrt();
        sig.insert(name.to_owned(), (sigma, tau));
        no_lookup.insert(name.into(), json!({"families": fs.len(), "missing_docs": missing, "sigma_family": sigma, "mean_family": mean(&vals), "domain_sd_of_means": between.sqrt(), "tau_between_domains": tau, "domains": by_dom.len()}));
    }
    let close: Vec<&(f64, f64)> = ["Faux-29", "Faux-41"].iter().filter_map(|n| sig.get(*n)).collect();
    let diff: Vec<&(f64, f64)> = ["E0-17", "V6-17", "S3A-17"].iter().filter_map(|n| sig.get(*n)).collect();
    let avg = |v: &[&(f64, f64)]| (mean(&v.iter().map(|x| x.0).collect::<Vec<_>>()), mean(&v.iter().map(|x| x.1).collect::<Vec<_>>()));
    let panels = [("close_fine_tune", avg(&close)), ("different_model", avg(&diff))];
    let z = phi_inv(0.999);
    let n_cc: f64 = 20_000.0;
    let anchors: [f64; 8] = [4.0, 25.0, 100.0, 300.0, 625.0, 1250.0, 2500.0, 20_000.0];
    let xs = [1.0, 2.0, 4.0, 8.0, 16.0, 32.0];
    let mut rows = Vec::new();
    let mut tsv = String::from("strategy\tpanel\tsigma\ttau\tn_anchor\tse\tthreshold\tno_lookup_flag_rate\tc80\tc80_cohort_gain_pct_B0\tpower_at_X_pct_B0(1,2,4,8,16,32)\n");
    // Effect on T per unit coverage: covered-family gain minus uncovered-family
    // change (the uncovered change also applies to anchors and cancels), and
    // the cohort gain per unit coverage, both at coverage `at`.
    let row_at = |s: &str| -> Option<&Value> {
        gain["rows"].as_array()?.iter().find(|r| r["strategy"] == s && r["policy"] == "calibrated" && (r["coverage"].as_f64().unwrap_or(0.0) - at).abs() < 1e-9)
    };
    for key in ["A_all_runs:calibrated", "B_sentences:calibrated", "W_winnow8:calibrated", "C_lsh_bands:calibrated"] {
        let s = key.split(':').next().unwrap();
        let r = row_at(s).with_context(|| format!("no {s} row at coverage {at}"))?;
        let ell = r["family_gain_covered"].as_f64().unwrap_or(f64::NAN) - r["family_gain_uncovered"].as_f64().unwrap_or(f64::NAN);
        let g1 = r["delta_brier"].as_f64().unwrap_or(f64::NAN) / at;
        for (pn, (sigma, tau)) in panels {
            for &na in &anchors {
                let se = sigma * (1.0 / na + 1.0 / n_cc).sqrt();
                let tau_hat = 1.96 * tau;
                let thr = tau_hat + z * se;
                let tot = (se * se + tau * tau).sqrt();
                let no_lookup_flag = 1.0 - phi(thr / tot);
                // coverage with 80% power: c*ell = thr + 0.8416*tot
                let c80 = (thr + phi_inv(0.8) * tot) / ell;
                let pw: Vec<f64> = xs
                    .iter()
                    .map(|x| {
                        let c = x / 100.0 * b0 / g1;
                        phi((c * ell - thr) / tot)
                    })
                    .collect();
                tsv.push_str(&format!(
                    "{s}\t{pn}\t{sigma:.4}\t{tau:.4}\t{na}\t{se:.5}\t{thr:.5}\t{no_lookup_flag:.5}\t{:.3}\t{:.2}\t{}\n",
                    c80,
                    100.0 * c80 * g1 / b0,
                    pw.iter().map(|x| format!("{x:.3}")).collect::<Vec<_>>().join(",")
                ));
                rows.push(json!({"strategy": s, "panel": pn, "sigma": sigma, "tau": tau, "n_anchor": na, "n_cc": n_cc, "se": se, "threshold": thr,
                                 "no_lookup_flag_rate": no_lookup_flag, "ell_family_full_coverage": ell, "cohort_gain_full_coverage": g1,
                                 "c80": c80, "c80_cohort_gain_pct_b0": 100.0 * c80 * g1 / b0, "power_at_x_pct_b0": xs.iter().zip(&pw).map(|(x, p)| json!({"x_pct_b0": x, "power": p})).collect::<Vec<_>>()}));
            }
        }
    }
    // Same table with the heterogeneity floor removed (tau = 0): what more anchors buy if topics, era and domains were perfectly matched.
    let mut rows0 = Vec::new();
    for key in ["A_all_runs:calibrated", "B_sentences:calibrated"] {
        let s = key.split(':').next().unwrap();
        let r = row_at(s).with_context(|| format!("no {s} row at coverage {at}"))?;
        let ell = r["family_gain_covered"].as_f64().unwrap_or(f64::NAN) - r["family_gain_uncovered"].as_f64().unwrap_or(f64::NAN);
        for (pn, (sigma, _)) in panels {
            for &na in &anchors {
                let se = sigma * (1.0 / na + 1.0 / n_cc).sqrt();
                rows0.push(json!({"strategy": key, "panel": pn, "n_anchor": na, "c80_tau0": (z + phi_inv(0.8)) * se / ell}));
            }
        }
    }
    write_text(&out.join("canary.tsv"), &tsv)?;
    store::write_json(
        &out.join("canary.json"),
        &json!({"schema": "slop_ninja_cc_lookup_canary_v2", "gain_tag": gain_tag, "effect_at_coverage": at, "alpha_one_sided": 0.001, "n_cc_matched": n_cc, "no_lookup_panels": no_lookup, "rows": rows, "rows_tau0": rows0,
                "cost": {"usd_per_anchor": 6.43, "anchors_per_year": [1000, 1250, 1500], "usd_per_year": [6430.0, 8037.5, 9645.0], "cohorts_per_year": 365, "anchors_per_cohort_at_1250": 1250.0 / 365.0}}),
    )?;
    println!("canary: done");
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let out = &cli.out;
    std::fs::create_dir_all(out)?;
    match cli.cmd {
        Cmd::Keys { pilot, threads } => keys(out, &pilot, threads),
        Cmd::Bloom { fprs, random_queries } => bloom(out, &parse_list(&fprs)?, random_queries),
        Cmd::Background { rows } => background(out, &rows),
        Cmd::Nearmatch { rows } => nearmatch(out, &rows),
        Cmd::Lightedit { rows, rates } => lightedit(out, &rows, &parse_list(&rates)?),
        Cmd::Frame { meta, pilot } => frame(out, &meta, &pilot),
        Cmd::Gain { preds, mix, coverages, reps, fpr, tag, light_edit_share, light_edit_rate, bg_mode, bg_cap } => {
            let m = parse_list(&mix)?;
            ensure!(m.len() == 3, "mix needs three shares");
            let le = light_edit_share.map(|s| (light_edit_rate, s));
            gain(out, &preds, [m[0], m[1], m[2]], &parse_list(&coverages)?, reps, fpr, &tag, le, (&bg_mode, bg_cap))
        }
        Cmd::Canary { pred_dir, gain_tag, at } => canary(out, &pred_dir, &gain_tag, at),
    }?;
    std::io::stdout().flush()?;
    Ok(())
}
