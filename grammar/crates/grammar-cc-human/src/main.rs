//! `slop_ninja_cc_human`: the Common Crawl human-slice pilot sampler.
//!
//! Steps, each resumable and writing under `--root` (ignored storage):
//! `paths` (per-crawl `wet.paths.gz`), `select` (fixed-seed WET picks),
//! `fetch` (polite journaled download), `pages` (record parse, gates and
//! page screens), `candidates` (site templates, keyed paragraph run,
//! excerpt-level English), `dedup` (corpus-v1, host closure, within pool),
//! `pointers` (Range re-fetch of sampled record pointers) and `report`
//! (yield tables and the yearly projection).
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_a_pools::{psl::Psl, splits::rank};
use grammar_cc_human::{candidates, dedup, domain::Rules, fetch, pages, plan, report, store, warc};
use rayon::prelude::*;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

#[derive(Parser)]
#[command(name = "slop_ninja_cc_human")]
struct Cli {
    /// Pilot output root (for example data/task-roots/cc-human/pilot-v1).
    #[arg(long)]
    root: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Download `wet.paths.gz` for every frame crawl not already present.
    Paths {
        /// `per-crawl.tsv` whose first column lists the frame crawls.
        #[arg(long)]
        frame: PathBuf,
        #[arg(long, default_value_t = 2)]
        connections: usize,
        #[arg(long, default_value_t = 38_000_000_000)]
        budget_bytes: u64,
        #[arg(long, default_value_t = 400_000_000_000)]
        min_free_bytes: u64,
    },
    /// Rank each crawl's WET paths and write `selection.tsv`.
    Select {
        #[arg(long)]
        frame: PathBuf,
        #[arg(long, default_value_t = 2)]
        per_crawl: usize,
        #[arg(long, default_value = plan::PILOT_SEED)]
        seed: String,
    },
    /// Download the selected WET files.
    Fetch {
        #[arg(long, default_value_t = 2)]
        connections: usize,
        #[arg(long, default_value_t = 38_000_000_000)]
        budget_bytes: u64,
        #[arg(long, default_value_t = 400_000_000_000)]
        min_free_bytes: u64,
        /// Only the first N selected files (smoke runs).
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Pass 1: parse, gate and screen every record of every downloaded file.
    Pages {
        #[command(flatten)]
        common: Common,
        /// Only files whose path contains this substring (smoke runs).
        #[arg(long)]
        only: Option<String>,
    },
    /// Pass 2: site templates, keyed passage choice and excerpt English.
    Candidates {
        #[arg(long, default_value_t = 1)]
        threads: usize,
        #[arg(long, default_value = plan::PILOT_SEED)]
        passage_seed: String,
        /// Private domain rules (default: the committed pilot rules).
        #[arg(long)]
        rules: Option<PathBuf>,
    },
    /// Pass 3: corpus-v1 dedup, host closure and within-pool dedup.
    Dedup {
        #[arg(long, default_value_t = 1)]
        threads: usize,
        /// corpus-v1 store directory (opened read-only).
        #[arg(long)]
        store: PathBuf,
        #[arg(long)]
        psl: PathBuf,
        #[arg(long, default_value = plan::PILOT_SEED)]
        pool_seed: String,
    },
    /// Re-fetch sampled admitted records by HTTP Range and check every field.
    Pointers {
        #[arg(long, default_value_t = 50)]
        sample: usize,
    },
    /// Time each page screen over one WET file (diagnostic).
    Bench {
        #[arg(long)]
        file: PathBuf,
    },
    /// Yield tables and the yearly projection.
    Report {
        #[arg(long, default_value_t = 600_000.0)]
        docs_per_year: f64,
        #[arg(long, default_value_t = 0.05)]
        usd_per_vcpu_hour: f64,
        #[arg(long, default_value_t = 0.20)]
        usd_per_kwh: f64,
        #[arg(long, default_value_t = 12.0)]
        watts_per_busy_core: f64,
    },
}

#[derive(clap::Args)]
struct Common {
    #[arg(long, default_value_t = 1)]
    threads: usize,
    #[arg(long)]
    psl: PathBuf,
    /// FineWeb's `excluded-registered-domains-v1.txt`.
    #[arg(long)]
    fineweb_excluded: PathBuf,
    /// Private domain rules (default: the committed pilot rules).
    #[arg(long)]
    rules: Option<PathBuf>,
}

fn journal(root: &Path) -> PathBuf {
    root.join("journal/downloads.jsonl")
}

fn paths_file(root: &Path, crawl: &str) -> PathBuf {
    root.join("meta/paths").join(format!("{crawl}.wet.paths.gz"))
}

/// (crawl, rel path, local file) for every selected file.
fn selected(root: &Path) -> Result<Vec<(String, String, PathBuf)>> {
    let sel = std::fs::read_to_string(root.join("selection.tsv")).context("run select first")?;
    Ok(sel
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let (crawl, path) = (f.first()?, f.get(2)?);
            let name = path.rsplit('/').next()?;
            Some(((*crawl).to_owned(), (*path).to_owned(), root.join("raw").join(crawl).join(name)))
        })
        .collect())
}

fn pool(threads: usize) -> Result<rayon::ThreadPool> {
    Ok(rayon::ThreadPoolBuilder::new().num_threads(threads.max(1)).build()?)
}

fn unix_of(ts: &str) -> Option<i64> {
    let b = ts.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let n = |a: usize, z: usize| ts.get(a..z)?.parse::<i64>().ok();
    let (y, m, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (hh, mm, ss) = (n(11, 13)?, n(14, 16)?, n(17, 19)?);
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400 + hh * 3600 + mm * 60 + ss)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = &cli.root;
    std::fs::create_dir_all(root)?;
    match cli.cmd {
        Cmd::Paths {
            frame,
            connections,
            budget_bytes,
            min_free_bytes,
        } => {
            let crawls = plan::frame(&frame)?;
            let jobs = crawls
                .iter()
                .map(|c| fetch::Job {
                    rel: format!("crawl-data/{c}/wet.paths.gz"),
                    dest: paths_file(root, c),
                })
                .collect();
            let s = fetch::run(
                jobs,
                fetch::Limits {
                    budget_bytes,
                    min_free_bytes,
                    connections,
                },
                &journal(root),
            )?;
            println!(
                "{}",
                json!({"done": s.done, "skipped": s.skipped, "failed": s.failed, "received_bytes": s.received_bytes, "stop": s.stop_reason})
            );
            ensure!(s.failed == 0 && s.stop_reason.is_none(), "paths download incomplete");
        }
        Cmd::Select { frame, per_crawl, seed } => {
            let crawls = plan::frame(&frame)?;
            let mut tsv = String::from("crawl\trank\tpath\twet_paths\teligible_paths\twet_paths_sha256\n");
            let mut rows = Vec::new();
            for c in &crawls {
                let f = paths_file(root, c);
                let paths = plan::read_paths(&f)?;
                let (picks, eligible) = plan::pick(&paths, &seed, per_crawl);
                ensure!(picks.len() == per_crawl, "{c}: only {} eligible paths", picks.len());
                let sha = fetch::sha256_file(&f)?;
                for p in picks {
                    tsv.push_str(&format!("{c}\t{:016x}\t{}\t{}\t{eligible}\t{sha}\n", p.rank, p.path, paths.len()));
                    rows.push(json!({"crawl": c, "rank": format!("{:016x}", p.rank), "path": p.path, "wet_paths": paths.len(), "eligible_paths": eligible, "wet_paths_sha256": sha}));
                }
            }
            std::fs::write(root.join("selection.tsv"), &tsv)?;
            store::write_json(
                &root.join("selection.json"),
                &json!({"schema": plan::SELECTION_SCHEMA, "seed": seed, "scope": plan::WET_SCOPE, "per_crawl": per_crawl,
                        "eligibility": "segment timestamp before 20221130000000 (CC-MAIN-2022-49 only); records still need WARC-Date before 2022-11-30T00:00:00Z",
                        "crawls": crawls.len(), "files": rows}),
            )?;
            println!("selected {} files over {} crawls", rows.len(), crawls.len());
        }
        Cmd::Fetch {
            connections,
            budget_bytes,
            min_free_bytes,
            limit,
        } => {
            let mut jobs: Vec<fetch::Job> = selected(root)?.into_iter().map(|(_, rel, dest)| fetch::Job { rel, dest }).collect();
            if let Some(n) = limit {
                jobs.truncate(n);
            }
            let s = fetch::run(
                jobs,
                fetch::Limits {
                    budget_bytes,
                    min_free_bytes,
                    connections,
                },
                &journal(root),
            )?;
            println!(
                "{}",
                json!({"done": s.done, "skipped": s.skipped, "failed": s.failed, "received_bytes": s.received_bytes, "stop": s.stop_reason})
            );
            ensure!(s.failed == 0 && s.stop_reason.is_none(), "fetch incomplete");
        }
        Cmd::Pages { common, only } => {
            let psl = Psl::parse(&std::fs::read_to_string(&common.psl)?);
            let excluded: HashSet<String> = std::fs::read_to_string(&common.fineweb_excluded)?
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_lowercase)
                .collect();
            let rules = match &common.rules {
                Some(p) => Rules::parse(&std::fs::read_to_string(p)?)?,
                None => Rules::pilot(),
            };
            let ctx = pages::Ctx {
                psl: &psl,
                rules: &rules,
                fineweb_excluded: &excluded,
                root,
            };
            let mut files = selected(root)?;
            if let Some(o) = &only {
                files.retain(|(_, rel, _)| rel.contains(o.as_str()));
            }
            let n = files.len();
            let done = AtomicUsize::new(0);
            let t0 = Instant::now();
            pool(common.threads)?.install(|| {
                files.par_iter().try_for_each(|(crawl, rel, local)| -> Result<()> {
                    ensure!(local.exists(), "missing download {}", local.display());
                    let ran = pages::process(&ctx, crawl, rel, local)?;
                    let k = done.fetch_add(1, Ordering::SeqCst) + 1;
                    eprintln!(
                        "pages: {k}/{n} {} {} ({:.0}s)",
                        if ran { "done" } else { "skip" },
                        rel,
                        t0.elapsed().as_secs_f64()
                    );
                    Ok(())
                })
            })?;
            println!(
                "{}",
                json!({"files": n, "seconds": t0.elapsed().as_secs_f64(), "threads": common.threads})
            );
        }
        Cmd::Candidates {
            threads,
            passage_seed,
            rules,
        } => {
            let rules = match &rules {
                Some(p) => Rules::parse(&std::fs::read_to_string(p)?)?,
                None => Rules::pilot(),
            };
            let t0 = Instant::now();
            let stems: Vec<String> = selected(root)?.iter().map(|(_, rel, _)| pages::stem(rel)).collect();
            for st in &stems {
                ensure!(pages::funnel_path(root, st).exists(), "pages pass incomplete for {st}");
            }
            let p = pool(threads)?;
            // Site templates over every surviving page.
            let tpath = root.join("templates.bin.zst");
            let templates: HashSet<u64> = p.install(|| -> Result<HashSet<u64>> {
                let per: Vec<Vec<u64>> = stems
                    .par_iter()
                    .map(|st| -> Result<Vec<u64>> {
                        let rows: Vec<pages::Page> = store::rows(&pages::pages_path(root, st))?;
                        Ok(rows.iter().flat_map(candidates::page_line_keys).collect())
                    })
                    .collect::<Result<_>>()?;
                let mut all: Vec<u64> = per.into_iter().flatten().collect();
                let total = all.len();
                all.par_sort_unstable();
                let mut set = HashSet::new();
                let mut i = 0;
                while i < all.len() {
                    let mut j = i + 1;
                    while j < all.len() && all[j] == all[i] {
                        j += 1;
                    }
                    if j - i >= 2 {
                        set.insert(all[i]);
                    }
                    i = j;
                }
                eprintln!(
                    "candidates: {total} page-line keys, {} site template lines ({:.0}s)",
                    set.len(),
                    t0.elapsed().as_secs_f64()
                );
                Ok(set)
            })?;
            let mut keys: Vec<u64> = templates.iter().copied().collect();
            keys.sort_unstable();
            let bytes: Vec<u8> = keys.iter().flat_map(|k| k.to_le_bytes()).collect();
            std::fs::write(&tpath, zstd::bulk::compress(&bytes, 3)?)?;
            let n = stems.len();
            let done = AtomicUsize::new(0);
            p.install(|| {
                stems.par_iter().try_for_each(|st| -> Result<()> {
                    candidates::process(root, st, &templates, &passage_seed, &rules)?;
                    let k = done.fetch_add(1, Ordering::SeqCst) + 1;
                    if k.is_multiple_of(10) || k == n {
                        eprintln!("candidates: {k}/{n} ({:.0}s)", t0.elapsed().as_secs_f64());
                    }
                    Ok(())
                })
            })?;
            println!(
                "{}",
                json!({"files": n, "template_lines": templates.len(), "seconds": t0.elapsed().as_secs_f64(), "threads": threads})
            );
        }
        Cmd::Dedup {
            threads,
            store: st,
            psl,
            pool_seed,
        } => {
            let psl = Psl::parse(&std::fs::read_to_string(&psl)?);
            let inp = dedup::Inputs {
                root: root.clone(),
                store: st,
                psl,
                pool_seed,
            };
            let r = pool(threads)?.install(|| dedup::run(&inp))?;
            let mut r = r;
            r["threads"] = json!(threads);
            store::write_json(&root.join("dedup/report.json"), &r)?;
            println!(
                "{}",
                json!({"admitted": r["admitted"], "corpus_v1_excluded": r["corpus_v1_excluded"], "seconds": r["seconds"]})
            );
        }
        Cmd::Pointers { sample } => {
            // Stream the admitted rows, keeping the `sample` lowest-ranked.
            let f = std::fs::File::open(root.join("pool/admitted.jsonl.zst"))?;
            let mut keep: std::collections::BTreeMap<(u64, String), Value> = std::collections::BTreeMap::new();
            for l in std::io::BufRead::lines(std::io::BufReader::new(zstd::Decoder::new(f)?)) {
                let v: Value = serde_json::from_str(&l?)?;
                let id = v["id"].as_str().unwrap_or("").to_owned();
                let k = (rank(plan::PILOT_SEED, "cc_human_pointer_check_v1", &id), id);
                if keep.len() < sample || keep.keys().next_back().is_some_and(|m| k < *m) {
                    keep.insert(k, v);
                    if keep.len() > sample {
                        keep.pop_last();
                    }
                }
            }
            let rows: Vec<Value> = keep.into_values().collect();
            let idx: Vec<usize> = (0..rows.len()).collect();
            let agent = fetch::agent();
            let j = fetch::Journal::open(&journal(root))?;
            let mut out = Vec::new();
            let mut ok = 0;
            for i in idx {
                let r = &rows[i];
                let rel = r["wet_path"].as_str().unwrap_or("");
                let (off, len) = (r["member_offset"].as_u64().unwrap_or(0), r["member_length"].as_u64().unwrap_or(0));
                let res: Result<Value> = (|| {
                    let b = fetch::range(&agent, rel, off, len, &j)?;
                    let (raw, used) = warc::decode_member(&b)?;
                    ensure!(used as u64 == len, "member length {used} != {len}");
                    let rec = warc::parse_record(&raw, off, len)?;
                    let page = String::from_utf8_lossy(&rec.block).into_owned();
                    let lines: Vec<&str> = candidates::lines(&page).collect();
                    let (s, e) = (
                        r["line_start"].as_u64().unwrap_or(0) as usize,
                        r["line_end"].as_u64().unwrap_or(0) as usize,
                    );
                    let ex = lines
                        .get(s..e)
                        .map(|ls| ls.iter().map(|l| grammar_a_pools::text::squash(l)).collect::<Vec<_>>().join("\n\n"));
                    let checks = json!({
                        "record_id": rec.header("WARC-Record-ID") == r["record_id"].as_str(),
                        "warc_date": rec.header("WARC-Date") == r["warc_date"].as_str(),
                        "url": rec.header("WARC-Target-URI") == r["url"].as_str(),
                        "block_digest": rec.block_digest_ok() == Some(true),
                        "page_sha256": grammar_a_pools::text::sha_hex(&rec.block) == r["page_sha256"].as_str().unwrap_or(""),
                        "excerpt_text": ex.as_deref() == r["text"].as_str(),
                    });
                    let all = checks.as_object().unwrap().values().all(|v| v == true);
                    Ok(json!({"id": r["id"], "wet_path": rel, "offset": off, "length": len, "checks": checks, "ok": all}))
                })();
                let v = res.unwrap_or_else(|e| json!({"id": r["id"], "error": format!("{e:#}"), "ok": false}));
                if v["ok"] == true {
                    ok += 1;
                }
                out.push(v);
            }
            store::write_json(
                &root.join("report/pointers.json"),
                &json!({"schema": "slop_ninja_cc_human_pointer_check_v1", "sample": out.len(), "ok": ok, "rows": out}),
            )?;
            println!("pointers: {ok}/{} ok", out.len());
        }
        Cmd::Bench { file } => {
            let data = std::fs::read(&file)?;
            let rules = Rules::pilot();
            let mut t = [0f64; 7];
            let mut n = 0;
            let mut bytes = 0usize;
            let t0 = Instant::now();
            let recs: Vec<warc::Record> = warc::Members::new(&data).collect::<Result<_>>()?;
            t[0] = t0.elapsed().as_secs_f64();
            for r in recs.iter().filter(|r| r.kind() == "conversion") {
                let page = String::from_utf8_lossy(&r.block).into_owned();
                bytes += page.len();
                n += 1;
                let url = r.header("WARC-Target-URI").unwrap_or("");
                let host = grammar_a_pools::psl::url_host(url).unwrap_or_default();
                let path = grammar_cc_human::domain::url_path(url);
                let a = Instant::now();
                let _ = rules.url_rule(&host, path);
                t[1] += a.elapsed().as_secs_f64();
                let a = Instant::now();
                let _ = rules.cues(&page);
                t[2] += a.elapsed().as_secs_f64();
                let a = Instant::now();
                let _ = grammar_cc_human::screens::gopher_quality(&page);
                t[3] += a.elapsed().as_secs_f64();
                let a = Instant::now();
                let _ = grammar_cc_human::screens::gopher_repetition(&page);
                t[4] += a.elapsed().as_secs_f64();
                let a = Instant::now();
                let _ = grammar_cc_human::screens::machine_text(&page, false);
                t[5] += a.elapsed().as_secs_f64();
                let a = Instant::now();
                let _ = grammar_cc_human::screens::nc_licence(&page);
                t[6] += a.elapsed().as_secs_f64();
            }
            println!(
                "{}",
                json!({"records": n, "text_bytes": bytes, "decode": t[0], "url_rule": t[1], "cues": t[2], "gopher_quality": t[3], "gopher_repetition": t[4], "machine_text": t[5], "nc_licence": t[6]})
            );
        }
        Cmd::Report {
            docs_per_year,
            usd_per_vcpu_hour,
            usd_per_kwh,
            watts_per_busy_core,
        } => {
            // Aggregate download throughput of the WET files.
            let jt = std::fs::read_to_string(journal(root)).unwrap_or_default();
            let (mut b, mut t_min, mut t_max, mut xfer_s) = (0u64, i64::MAX, i64::MIN, 0f64);
            let (mut all_bytes, mut range_reads) = (0u64, 0u64);
            for l in jt.lines() {
                let v: Value = serde_json::from_str(l)?;
                all_bytes += v["bytes_received"].as_u64().unwrap_or(0);
                if v["kind"] == "range" {
                    range_reads += 1;
                }
                if v["status"] == "ok" && v["rel"].as_str().is_some_and(|r| r.ends_with(".warc.wet.gz")) {
                    b += v["bytes"].as_u64().unwrap_or(0);
                    xfer_s += v["seconds"].as_f64().unwrap_or(0.0);
                    if let (Some(s), Some(e)) = (
                        unix_of(v["started_utc"].as_str().unwrap_or("")),
                        unix_of(v["finished_utc"].as_str().unwrap_or("")),
                    ) {
                        t_min = t_min.min(s);
                        t_max = t_max.max(e);
                    }
                }
            }
            let wall = (t_max - t_min).max(1) as f64;
            let dd: Value = store::read_json(&root.join("dedup/report.json")).unwrap_or(json!({}));
            let extra = json!({"download_wet_bytes": b, "download_wall_seconds": wall, "download_mb_per_s": b as f64 / 1e6 / wall,
                               "download_transfer_seconds": xfer_s, "journal_bytes_received_total": all_bytes, "range_reads": range_reads,
                               "dedup_seconds": dd["seconds"], "dedup_threads": dd["threads"], "dedup_phase_seconds": dd["phase_seconds"]});
            let s = report::run(
                root,
                &report::Costs {
                    docs_per_year,
                    usd_per_vcpu_hour,
                    usd_per_kwh,
                    watts_per_busy_core,
                },
                &extra,
            )?;
            println!("{}", serde_json::to_string_pretty(&s["projection"])?);
        }
    }
    Ok(())
}
