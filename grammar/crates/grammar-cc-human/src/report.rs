//! Yield tables and the yearly projection.
//!
//! Every count comes from the per-file funnels. "Per GB" divides by the WET
//! bytes downloaded for that crawl (10^9 bytes). Words are page words for
//! the page stages and excerpt words from `excerpt` on.
use crate::{
    funnel::{Cell, Funnel, STAGES},
    store,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    path::Path,
};

pub struct Costs {
    pub docs_per_year: f64,
    pub usd_per_vcpu_hour: f64,
    pub usd_per_kwh: f64,
    pub watts_per_busy_core: f64,
}

fn words_of(stage: &str, c: &Cell) -> u64 {
    if STAGES.iter().position(|s| *s == stage) >= STAGES.iter().position(|s| *s == "excerpt") {
        c.excerpt_words
    } else {
        c.page_words
    }
}

fn year_of(crawl: &str) -> &str {
    crawl.get(8..12).unwrap_or("")
}

pub fn run(root: &Path, costs: &Costs, extra: &Value) -> Result<Value> {
    let fun = root.join("funnel");
    let mut per_crawl: BTreeMap<String, Funnel> = BTreeMap::new();
    let mut gb: BTreeMap<String, f64> = BTreeMap::new();
    let mut files: BTreeMap<String, u64> = BTreeMap::new();
    let (mut s_pages, mut s_cand, mut records, mut nonconv, mut digest_missing, mut parse_err) = (0f64, 0f64, 0u64, 0u64, 0u64, 0u64);
    let mut machine: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut lang_patterns: BTreeMap<String, u64> = BTreeMap::new();
    let mut runs_hist: BTreeMap<String, u64> = BTreeMap::new();
    let mut breaks: BTreeMap<String, u64> = BTreeMap::new();
    let mut cld2_records = 0u64;
    for p in store::files_with_suffix(&fun, ".pages.json")? {
        let v: Value = store::read_json(&p)?;
        let crawl = v["crawl"].as_str().context("crawl")?.to_owned();
        let f: Funnel = serde_json::from_value(v["funnel"].clone())?;
        per_crawl.entry(crawl.clone()).or_default().merge(&f);
        *gb.entry(crawl.clone()).or_default() += v["wet_bytes"].as_f64().unwrap_or(0.0) / 1e9;
        *files.entry(crawl.clone()).or_default() += 1;
        s_pages += v["seconds"].as_f64().unwrap_or(0.0);
        records += f.get("records", None).docs;
        nonconv += v["non_conversion_records"].as_u64().unwrap_or(0);
        digest_missing += v["records_without_block_digest"].as_u64().unwrap_or(0);
        parse_err += v["parse_errors"].as_u64().unwrap_or(0);
        cld2_records += v["records_with_cld2_header"].as_u64().unwrap_or(0);
        if let Some(m) = v["machine_text_rules_fired"].as_object() {
            for (r, dm) in m {
                for (d, n) in dm.as_object().into_iter().flatten() {
                    *machine.entry(r.clone()).or_default().entry(d.clone()).or_default() += n.as_u64().unwrap_or(0);
                }
            }
        }
        let st = p.strip_prefix(&fun)?.to_string_lossy().trim_end_matches(".pages.json").to_owned();
        let cp = fun.join(format!("{st}.candidates.json"));
        if cp.exists() {
            let c: Value = store::read_json(&cp)?;
            per_crawl.get_mut(&crawl).unwrap().merge(&serde_json::from_value(c["funnel"].clone())?);
            s_cand += c["seconds"].as_f64().unwrap_or(0.0);
            for (k, n) in c["language_patterns"].as_object().into_iter().flatten() {
                *lang_patterns.entry(k.clone()).or_default() += n.as_u64().unwrap_or(0);
            }
            for (k, n) in c["valid_runs_histogram_capped_50"].as_object().into_iter().flatten() {
                *runs_hist.entry(k.clone()).or_default() += n.as_u64().unwrap_or(0);
            }
            for (k, n) in c["paragraph_breaks"].as_object().into_iter().flatten() {
                *breaks.entry(k.clone()).or_default() += n.as_u64().unwrap_or(0);
            }
        }
        let dp = fun.join(format!("{st}.dedup.json"));
        if dp.exists() {
            let d: Value = store::read_json(&dp)?;
            per_crawl.get_mut(&crawl).unwrap().merge(&serde_json::from_value(d["funnel"].clone())?);
        }
    }
    let mut all = Funnel::default();
    for f in per_crawl.values() {
        all.merge(f);
    }
    let total_gb: f64 = gb.values().sum();
    let mut domains: BTreeSet<String> = BTreeSet::new();
    for m in all.cells.values() {
        domains.extend(m.keys().cloned());
    }
    let out = root.join("report");
    std::fs::create_dir_all(&out)?;

    // Per crawl x domain x stage.
    let mut t = String::from("crawl\tyear\twet_files\twet_gb\tdomain\tstage\tdocs\tpage_words\texcerpt_words\tdocs_per_gb\twords_per_gb\n");
    for (crawl, f) in &per_crawl {
        let g = gb[crawl];
        for d in domains.iter().map(|d| Some(d.as_str())).chain([None]) {
            for s in STAGES {
                let c = f.get(s, d);
                let w = words_of(s, &c);
                let _ = writeln!(
                    t,
                    "{crawl}\t{}\t{}\t{g:.4}\t{}\t{s}\t{}\t{}\t{}\t{:.1}\t{:.0}",
                    year_of(crawl),
                    files[crawl],
                    d.unwrap_or("ALL"),
                    c.docs,
                    c.page_words,
                    c.excerpt_words,
                    c.docs as f64 / g,
                    w as f64 / g
                );
            }
        }
    }
    std::fs::write(out.join("yield-by-crawl-domain.tsv"), &t)?;

    // Pooled domain x stage, and year x stage.
    let mut pooled = String::from("domain\tstage\tdocs\tpage_words\texcerpt_words\tdocs_per_gb\twords_per_gb\tshare_of_records\n");
    let recs_all = all.get("records", None).docs.max(1) as f64;
    for d in domains.iter().map(|d| Some(d.as_str())).chain([None]) {
        for s in STAGES {
            let c = all.get(s, d);
            let _ = writeln!(
                pooled,
                "{}\t{s}\t{}\t{}\t{}\t{:.1}\t{:.0}\t{:.6}",
                d.unwrap_or("ALL"),
                c.docs,
                c.page_words,
                c.excerpt_words,
                c.docs as f64 / total_gb,
                words_of(s, &c) as f64 / total_gb,
                c.docs as f64 / recs_all
            );
        }
    }
    std::fs::write(out.join("yield-by-domain.tsv"), &pooled)?;
    let mut by_year: BTreeMap<String, (Funnel, f64)> = BTreeMap::new();
    for (crawl, f) in &per_crawl {
        let e = by_year.entry(year_of(crawl).to_owned()).or_default();
        e.0.merge(f);
        e.1 += gb[crawl];
    }
    let mut yt = String::from("year\twet_gb\tstage\tdocs\tdocs_per_gb\twords_per_gb\n");
    for (y, (f, g)) in &by_year {
        for s in STAGES {
            let c = f.get(s, None);
            let _ = writeln!(
                yt,
                "{y}\t{g:.3}\t{s}\t{}\t{:.1}\t{:.0}",
                c.docs,
                c.docs as f64 / g,
                words_of(s, &c) as f64 / g
            );
        }
    }
    std::fs::write(out.join("yield-by-year.tsv"), &yt)?;

    // Rejection reasons, pooled.
    let mut rt = String::from("stage\treason\tdomain\tdocs\n");
    for (s, m) in &all.reasons {
        for (r, dm) in m {
            for (d, n) in dm {
                let _ = writeln!(rt, "{s}\t{r}\t{d}\t{n}");
            }
        }
    }
    std::fs::write(out.join("reasons.tsv"), &rt)?;

    // Projection.
    let admitted = all.get("pool_dedup", None);
    let english = all.get("english", None);
    let adm_per_gb = admitted.docs as f64 / total_gb;
    let gb_year = costs.docs_per_year / adm_per_gb.max(1e-9);
    let cpu_s_per_gb = (s_pages + s_cand) / total_gb;
    let dedup_s = extra["dedup_seconds"].as_f64().unwrap_or(0.0);
    let dedup_threads = extra["dedup_threads"].as_f64().unwrap_or(1.0);
    let ph = &extra["dedup_phase_seconds"];
    // Core-seconds of one dedup run: parallel phases at the run's thread
    // count, single-threaded phases at one core; without phase timings,
    // the whole run at the thread count (an upper bound).
    let dedup_core_s = if ph.is_object() {
        (ph["load_and_index"].as_f64().unwrap_or(0.0) + ph["corpus_stream"].as_f64().unwrap_or(0.0) + ph["write"].as_f64().unwrap_or(0.0))
            * dedup_threads
            + ph["host_roles_single_thread"].as_f64().unwrap_or(0.0)
            + ph["pool_single_thread"].as_f64().unwrap_or(0.0)
    } else {
        dedup_s * dedup_threads
    };
    let batches_per_year = 52.0;
    let cpu_h_year = gb_year * cpu_s_per_gb / 3600.0 + batches_per_year * dedup_core_s / 3600.0;
    let mbps = extra["download_mb_per_s"].as_f64().unwrap_or(0.0);
    let dl_hours = if mbps > 0.0 { gb_year * 1000.0 / mbps / 3600.0 } else { f64::NAN };
    let usd_cloud = cpu_h_year * costs.usd_per_vcpu_hour;
    let usd_power = cpu_h_year * costs.watts_per_busy_core / 1000.0 * costs.usd_per_kwh;
    let per_domain_need: BTreeMap<String, Value> = domains
        .iter()
        .map(|d| {
            let a = all.get("pool_dedup", Some(d)).docs as f64 / total_gb;
            let need = costs.docs_per_year / 9.0;
            (
                d.clone(),
                json!({"admitted_per_gb": a, "gb_per_year_for_one_ninth_from_random_wet": if a > 0.0 { need / a } else { f64::INFINITY }}),
            )
        })
        .collect();
    // Balanced scenario: every cohort domain fills one ninth from random
    // WET alone; the slowest domain sets the volume.
    let cohort: Vec<&String> = domains.iter().filter(|d| d.as_str() != crate::domain::GENERAL).collect();
    let (slow_dom, bal_gb) = cohort
        .iter()
        .map(|d| {
            let a = all.get("pool_dedup", Some(d)).docs as f64 / total_gb;
            (d.as_str(), if a > 0.0 { costs.docs_per_year / 9.0 / a } else { f64::INFINITY })
        })
        .fold(("", 0f64), |m, x| if x.1 > m.1 { x } else { m });
    let bal_cpu_h = bal_gb * cpu_s_per_gb / 3600.0 + batches_per_year * dedup_core_s / 3600.0;
    let balanced = json!({
        "limiting_domain": slow_dom, "wet_gb_per_year": bal_gb, "share_of_frame_wet_bytes": bal_gb * 1e9 / (640.4 * 1024f64.powi(4)),
        "cpu_core_hours_per_year": bal_cpu_h,
        "download_hours_per_year": if mbps > 0.0 { bal_gb * 1000.0 / mbps / 3600.0 } else { f64::NAN },
        "usd_host_time_cloud_equivalent": bal_cpu_h * costs.usd_per_vcpu_hour,
        "usd_host_time_build_host_power": bal_cpu_h * costs.watts_per_busy_core / 1000.0 * costs.usd_per_kwh,
        "note": "dedup is assumed to run on the kept per-domain quota only (surplus general_web pages are dropped before dedup)",
    });
    let proj = json!({
        "balanced_nine_domains_from_random_wet": balanced,
        "dedup_core_seconds_per_run": dedup_core_s,
        "docs_per_year": costs.docs_per_year, "pilot_wet_gb": total_gb, "pilot_admitted": admitted.docs,
        "admitted_per_gb": adm_per_gb, "english_candidates_per_gb": english.docs as f64 / total_gb,
        "wet_gb_per_year": gb_year, "wet_files_per_year_at_pilot_mean": gb_year / (total_gb / files.values().sum::<u64>() as f64),
        "share_of_frame_wet_bytes": gb_year * 1e9 / (640.4 * 1024f64.powi(4)),
        "single_thread_cpu_seconds_per_gb": {"pages": s_pages / total_gb, "candidates": s_cand / total_gb},
        "dedup_run_seconds": dedup_s, "dedup_threads": dedup_threads, "dedup_runs_per_year": batches_per_year,
        "cpu_core_hours_per_year": cpu_h_year, "download_mb_per_s_observed": mbps, "download_hours_per_year": dl_hours,
        "usd_data_access": 0.0,
        "usd_host_time_cloud_equivalent": usd_cloud, "usd_per_vcpu_hour_assumed": costs.usd_per_vcpu_hour,
        "usd_host_time_build_host_power": usd_power, "watts_per_busy_core_assumed": costs.watts_per_busy_core, "usd_per_kwh_assumed": costs.usd_per_kwh,
        "per_domain": per_domain_need,
    });
    // Admitted documents: distinct groups and era bins per domain, and
    // excerpt length quartiles.
    let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut eras: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut lens: Vec<u64> = Vec::new();
    let adm = root.join("pool/admitted.jsonl.zst");
    if adm.exists() {
        let r = std::io::BufReader::new(zstd::Decoder::new(std::fs::File::open(&adm)?)?);
        for l in std::io::BufRead::lines(r) {
            let v: Value = serde_json::from_str(&l?)?;
            let d = v["domain"].as_str().unwrap_or("").to_owned();
            groups.entry(d.clone()).or_default().insert(v["group"].as_str().unwrap_or("").to_owned());
            *eras.entry(d).or_default().entry(v["era"].as_str().unwrap_or("").to_owned()).or_default() += 1;
            lens.push(v["words"].as_u64().unwrap_or(0));
        }
    }
    lens.sort_unstable();
    let q = |f: f64| lens.get(((lens.len().max(1) - 1) as f64 * f) as usize).copied().unwrap_or(0);
    let admitted_detail = json!({
        "groups_by_domain": groups.iter().map(|(d, g)| (d.clone(), g.len())).collect::<BTreeMap<_, _>>(),
        "groups_all": groups.values().flatten().collect::<BTreeSet<_>>().len(),
        "era_bins_by_domain": eras,
        "excerpt_words_quartiles": [q(0.0), q(0.25), q(0.5), q(0.75), q(1.0)],
    });
    let summary = json!({
        "admitted_detail": admitted_detail,
        "schema": "slop_ninja_cc_human_pilot_report_v1", "crawls": per_crawl.len(), "wet_files": files.values().sum::<u64>(),
        "wet_gb": total_gb, "records": records, "non_conversion_records": nonconv, "records_without_block_digest": digest_missing,
        "parse_errors": parse_err, "records_with_cld2_header": cld2_records,
        "funnel_all": STAGES.iter().map(|s| { let c = all.get(s, None); json!({"stage": s, "docs": c.docs, "page_words": c.page_words, "excerpt_words": c.excerpt_words}) }).collect::<Vec<_>>(),
        "machine_text_rules_fired": machine, "language_patterns_on_excerpts": lang_patterns,
        "valid_runs_histogram_capped_50": runs_hist, "paragraph_breaks": breaks,
        "seconds": {"pages_single_thread_sum": s_pages, "candidates_single_thread_sum": s_cand},
        "projection": proj, "extra": extra,
    });
    store::write_json(&out.join("summary.json"), &summary)?;
    Ok(summary)
}
