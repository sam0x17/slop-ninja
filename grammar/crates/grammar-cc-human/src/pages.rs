//! Pass 1: parse each WET file, gate and screen every record, and keep the
//! pages that pass every page-level screen.
//!
//! Per record, in order: conversion record with a matching block digest;
//! `WARC-Date` before 2022-11-30; http(s) URL with a registered domain that
//! no exclusion list names; 1,000 to 1,000,000 characters; Gopher quality;
//! Gopher repetition; no machine-text marker; no non-commercial licence
//! statement. The cohort domain (URL rules, then page content cues) is
//! assigned to every record first, so every stage is counted by domain.
//!
//! Outputs per WET file (`<crawl>/<stem>`): `pages/*.jsonl.zst` (surviving
//! pages with exact text and the record pointer), `rejects/*.jsonl.zst`
//! (one row per rejected record, no text) and `funnel/*.pages.json`
//! (counts, input digest, timings), written last as the completion marker.
use crate::{
    domain::{self, Rules},
    funnel::{self, Funnel},
    screens, store,
    warc::Members,
};
use anyhow::{Context, Result};
use grammar_a_pools::{psl::Psl, record, text};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    time::Instant,
};

pub const PAGE_SCHEMA: &str = "slop_ninja_cc_human_page_v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub schema: String,
    pub crawl: String,
    pub wet_path: String,
    pub wet_sha256: String,
    pub member_offset: u64,
    pub member_length: u64,
    pub record_id: String,
    pub refers_to: Option<String>,
    pub warc_date: String,
    pub url: String,
    pub host: String,
    pub registered_domain: String,
    pub block_digest: Option<String>,
    pub cld2: Option<String>,
    pub page_sha256: String,
    pub page_chars: usize,
    pub page_words: usize,
    pub page_lines: usize,
    pub domain: String,
    pub domain_rule: String,
    pub cues: BTreeMap<String, usize>,
    pub group: String,
    pub group_rule: String,
    pub era: String,
    pub text: String,
}

#[derive(Serialize, Deserialize)]
pub struct Reject {
    pub record_id: String,
    pub url: String,
    pub warc_date: String,
    pub domain: String,
    pub stage: String,
    pub reason: String,
    pub member_offset: u64,
    pub member_length: u64,
    pub page_words: usize,
}

pub struct Ctx<'a> {
    pub psl: &'a Psl,
    pub rules: &'a Rules,
    pub fineweb_excluded: &'a HashSet<String>,
    pub root: &'a Path,
}

/// `<crawl>/<file stem>` of a WET path.
pub fn stem(wet_rel: &str) -> String {
    let crawl = wet_rel.split('/').nth(1).unwrap_or("unknown");
    let name = wet_rel.rsplit('/').next().unwrap_or("").trim_end_matches(".warc.wet.gz");
    format!("{crawl}/{name}")
}

pub fn funnel_path(root: &Path, st: &str) -> PathBuf {
    root.join("funnel").join(format!("{st}.pages.json"))
}

pub fn pages_path(root: &Path, st: &str) -> PathBuf {
    root.join("pages").join(format!("{st}.jsonl.zst"))
}

/// Process one downloaded WET file. Skips files whose funnel already exists.
pub fn process(ctx: &Ctx, crawl: &str, wet_rel: &str, local: &Path) -> Result<bool> {
    let st = stem(wet_rel);
    let fpath = funnel_path(ctx.root, &st);
    if fpath.exists() {
        return Ok(false);
    }
    let t0 = Instant::now();
    let data = std::fs::read(local).with_context(|| format!("read {}", local.display()))?;
    let wet_sha = text::sha_hex(&data);
    let t_read = t0.elapsed().as_secs_f64();
    let mut pages = store::ZWriter::create(&pages_path(ctx.root, &st))?;
    let mut rejects = store::ZWriter::create(&ctx.root.join("rejects").join(format!("{st}.jsonl.zst")))?;
    let mut f = Funnel::default();
    let mut machine_rules: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let (mut non_conversion, mut digest_missing, mut parse_errors) = (0u64, 0u64, 0u64);
    let mut lang_header = 0u64;
    let mut years: BTreeMap<String, u64> = BTreeMap::new();
    for rec in Members::new(&data) {
        let rec = match rec {
            Ok(r) => r,
            Err(e) => {
                parse_errors += 1;
                eprintln!("pages: {wet_rel}: {e:#}");
                continue;
            }
        };
        if rec.kind() != "conversion" {
            non_conversion += 1;
            continue;
        }
        let url = rec.header("WARC-Target-URI").unwrap_or("").to_owned();
        let date = rec.header("WARC-Date").unwrap_or("").to_owned();
        let rid = rec.header("WARC-Record-ID").unwrap_or("").to_owned();
        let cld2 = rec.header("WARC-Identified-Content-Language").map(str::to_owned);
        if cld2.is_some() {
            lang_header += 1;
        }
        *years.entry(date.get(..4).unwrap_or("").to_owned()).or_default() += 1;
        let page = String::from_utf8_lossy(&rec.block).into_owned();
        let pw = text::words(&page);
        let host = grammar_a_pools::psl::url_host(&url).unwrap_or_default();
        let path = domain::url_path(&url);
        let a = ctx.rules.assign(&host, path, &page);
        let dom = a.domain.clone();
        let mut reject = |f: &mut Funnel, stage: &str, reason: &str| -> Result<()> {
            f.reject(stage, reason, &dom);
            rejects.write(&Reject {
                record_id: rid.clone(),
                url: url.clone(),
                warc_date: date.clone(),
                domain: dom.clone(),
                stage: stage.to_owned(),
                reason: reason.to_owned(),
                member_offset: rec.offset,
                member_length: rec.length,
                page_words: pw,
            })
        };
        f.pass("records", &dom, pw, 0);
        match rec.block_digest_ok() {
            Some(true) => {}
            Some(false) => {
                reject(&mut f, "date_gate", "block_digest_mismatch")?;
                continue;
            }
            None => digest_missing += 1,
        }
        if !record::before_cutoff(&date) {
            reject(&mut f, "date_gate", "warc_date_not_before_2022_11_30")?;
            continue;
        }
        f.pass("date_gate", &dom, pw, 0);
        let reg = if url.starts_with("http://") || url.starts_with("https://") {
            ctx.psl.registered_domain(&host)
        } else {
            None
        };
        let Some(reg) = reg else {
            reject(&mut f, "url_gate", "no_http_url_registered_domain")?;
            continue;
        };
        if let Some(r) = screens::url_exclusion(&host, &reg, ctx.fineweb_excluded).or_else(|| screens::path_exclusion(&host, path)) {
            reject(&mut f, "url_gate", r)?;
            continue;
        }
        f.pass("url_gate", &dom, pw, 0);
        let chars = page.chars().count();
        if !(screens::MIN_PAGE_CHARS..=screens::MAX_PAGE_CHARS).contains(&chars) {
            reject(
                &mut f,
                "length_gate",
                if chars < screens::MIN_PAGE_CHARS {
                    "page_below_1000_chars"
                } else {
                    "page_above_1m_chars"
                },
            )?;
            continue;
        }
        f.pass("length_gate", &dom, pw, 0);
        if let Some(r) = screens::gopher_quality(&page) {
            reject(&mut f, "gopher_quality", r)?;
            continue;
        }
        f.pass("gopher_quality", &dom, pw, 0);
        if let Some(r) = screens::gopher_repetition(&page) {
            reject(&mut f, "gopher_repetition", r)?;
            continue;
        }
        f.pass("gopher_repetition", &dom, pw, 0);
        let mt = screens::machine_text(&page, dom == "technical");
        for r in &mt {
            *machine_rules.entry((*r).to_owned()).or_default().entry(dom.clone()).or_default() += 1;
        }
        if let Some(r) = mt.first() {
            reject(&mut f, "machine_text", r)?;
            continue;
        }
        f.pass("machine_text", &dom, pw, 0);
        if screens::nc_licence(&page) {
            reject(&mut f, "rights_text", "noncommercial_licence_statement")?;
            continue;
        }
        f.pass("rights_text", &dom, pw, 0);
        let (group, group_rule) = ctx.rules.group(&host, &reg, path);
        pages.write(&Page {
            schema: PAGE_SCHEMA.to_owned(),
            crawl: crawl.to_owned(),
            wet_path: wet_rel.to_owned(),
            wet_sha256: wet_sha.clone(),
            member_offset: rec.offset,
            member_length: rec.length,
            record_id: rid.clone(),
            refers_to: rec.header("WARC-Refers-To").map(str::to_owned),
            warc_date: date.clone(),
            url: url.clone(),
            host: host.clone(),
            registered_domain: reg,
            block_digest: rec.header("WARC-Block-Digest").map(str::to_owned),
            cld2,
            page_sha256: text::sha_hex(&rec.block),
            page_chars: chars,
            page_words: pw,
            page_lines: page.lines().filter(|l| !l.trim().is_empty()).count(),
            domain: dom.clone(),
            domain_rule: a.rule.clone(),
            cues: a.cues.clone(),
            group,
            group_rule: group_rule.to_owned(),
            era: funnel::era_bin(&date).to_owned(),
            text: page,
        })?;
    }
    let (pages_sha, pages_rows) = pages.finish()?;
    let (rejects_sha, rejects_rows) = rejects.finish()?;
    let secs = t0.elapsed().as_secs_f64();
    store::write_json(
        &fpath,
        &json!({"schema": "slop_ninja_cc_human_pages_funnel_v1", "crawl": crawl, "wet_path": wet_rel, "local": local,
                "wet_bytes": data.len(), "wet_sha256": wet_sha, "non_conversion_records": non_conversion,
                "records_without_block_digest": digest_missing, "parse_errors": parse_errors,
                "records_with_cld2_header": lang_header, "warc_date_years": years,
                "pages_rows": pages_rows, "pages_sha256": pages_sha, "rejects_rows": rejects_rows, "rejects_sha256": rejects_sha,
                "machine_text_rules_fired": machine_rules, "seconds": secs, "read_seconds": t_read,
                "screens": [screens::SCREENS_VERSION, screens::MACHINE_TEXT_VERSION, screens::RIGHTS_TEXT_VERSION],
                "domain_rules": ctx.rules.version, "domain_rules_sha256": ctx.rules.sha256,
                "funnel": f}),
    )?;
    Ok(true)
}
