//! The reservation gate: proves the public stock disjoint from Train (and the
//! Train-side held rows), the sealed Test and the other evaluation assignments,
//! the reference model's training lineage, the Common Crawl mainnet stock and
//! public-candidate-v1.
//!
//! The stock side is indexed once: every group key and family key, the normalized
//! site of every FineWeb group, the URL of every reserved unit the allow-list's unit
//! files name, the title hash of every reserved wiki unit, SciPost submission ids,
//! and (once rows are acquired) every text digest. Each reference is then streamed
//! and probed; nothing of it is kept. Keys are compared as the first 16 bytes of
//! their SHA-256, so a hashed key (`contributor_key_sha256`) matches the key it
//! hashes; a collision can only add a hit, never hide one.
//!
//! JSON references are probed field-blind: every string of every record is tried as
//! a key, as a site when it carries a site prefix (`fineweb_site:`, `fineweb_blog:`,
//! `cc_site:`, `cc_host:`), and, when it is a 64-digit hex digest, as a hashed key
//! and a text digest. A Train row keyed only by `split_group_key`, or a site under
//! another prefix, is found without a schema change. Assignment records (sealed Test,
//! evaluation) are refused when they carry a prose-named field or a string longer
//! than the allow-list's measured bound.
//!
//! What a hit means depends on the role:
//! - `train`, `sealed_test`, `evaluation`: the split construction excludes these by
//!   design, so any hit is a defect and refuses the stock;
//! - `train_held`, `r0_training_lineage`, `cc_mainnet_stock`, `public_candidate_v1`:
//!   built apart from the reservation (held Train-side rows and blocked writers, the
//!   reference model's trainer bundle and the pre-registered RAID title screen S1,
//!   the Common Crawl stock, the public candidate), so a hit excludes the stock group
//!   (closure) with a content-free receipt naming the checks that hit that group;
//! - `reference_training_lineage` (allow-list v3 with an active reference): the same
//!   for the active reference's trainer bundle and its own screen, beside R0's.
//!
//! Common Crawl rows are matched by WARC record id, normalized URL, normalized host,
//! registered domain and their own `cc_site:`/`cc_host:` group. A row on a source
//! host of a stock domain (declared in the allow-list, matched on the host or any
//! parent domain, the host taken from the URL when the row has none) is mapped to a
//! stock key by the host's URL rule; a row that no rule resolves refuses the stock
//! until text digests can be compared. Rows on declared mirror hosts are counted for
//! the text-level check at acquisition. Text digests are compared only when the
//! stock carries them (acquired rows); a reservation-stage stock has none and
//! records that.
//!
//! Before any reference is read, the split set's `SHA256SUMS` is checked: every
//! split manifest, group, family and blocked-group file it lists must be a gate
//! reference, a stock file or a declared parent reservation, with the same digest.
use crate::stock::{
    FileRef, GateRef, LINEAGE_SCREEN_SCHEMA, ProseGuard, SPLIT_FILES, SourceHost, StockAllowlist, StockGroup, UrlKey,
    for_each_line, group_site, is_assignment_role, key_site, normalize_host, orb_submission, read_checked,
    resolve_reference, split_url, url_key, url_rule,
};
use crate::{is_digest, resolve};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

pub const GATE_SCHEMA: &str = "slop_ninja_testnet_public_stock_gate_v2";

type Fp = [u8; 16];

pub fn fp(s: &str) -> Fp {
    let d = Sha256::digest(s.as_bytes());
    let mut out = [0u8; 16];
    out.copy_from_slice(&d[..16]);
    out
}

fn fp_hex(hex_digest: &str) -> Option<Fp> {
    if hex_digest.len() != 64 {
        return None;
    }
    let b = hex::decode(hex_digest).ok()?;
    let mut out = [0u8; 16];
    out.copy_from_slice(&b[..16]);
    Some(out)
}

/// Excludes the hit groups (true) or refuses the stock on a hit (false).
pub fn excludes(role: &str) -> bool {
    matches!(role, "train_held" | "r0_training_lineage" | "reference_training_lineage" | "cc_mainnet_stock" | "public_candidate_v1")
}

/// The stock side of the gate.
#[derive(Default)]
pub struct StockIndex {
    keys: HashMap<Fp, u32>,
    sites: HashMap<Fp, Vec<u32>>,
    urls: HashMap<Fp, u32>,
    titles: HashMap<Fp, Vec<u32>>,
    /// Keys a source-host URL rule produces (SciPost submissions).
    url_rule_keys: HashMap<Fp, Vec<u32>>,
    digests: HashMap<Fp, u32>,
    pub site_groups: u64,
    pub url_units: u64,
    pub title_units: u64,
}

impl StockIndex {
    pub fn build(groups: &[StockGroup]) -> Result<Self> {
        let mut ix = StockIndex::default();
        for (i, g) in groups.iter().enumerate() {
            let i = u32::try_from(i).context("stock group index")?;
            let mut own = BTreeSet::new();
            for k in std::iter::once(g.group_key.as_str()).chain(g.families()) {
                if !own.insert(k) {
                    continue; // an index group's family key is its group key
                }
                if let Some(prev) = ix.keys.insert(fp(k), i) {
                    ensure!(prev == i, "stock key {k} names two groups");
                }
                if let Some(s) = orb_submission(k) {
                    let e = ix.url_rule_keys.entry(fp(&s)).or_default();
                    if !e.contains(&i) {
                        e.push(i);
                    }
                }
            }
            if let Some(site) = group_site(&g.group_key) {
                ix.sites.entry(fp(&normalize_host(site))).or_default().push(i);
                ix.site_groups += 1;
            }
        }
        Ok(ix)
    }

    /// Adds reserved-unit URLs by family key (units of groups outside the stock are skipped).
    pub fn add_unit_urls(&mut self, repo: &Path, files: &[FileRef]) -> Result<Vec<Value>> {
        #[derive(Deserialize)]
        struct Unit {
            family_key: String,
            url: Option<String>,
            #[serde(default)]
            prose_opened: bool,
            #[serde(default)]
            text: Option<IgnoredAny>,
        }
        let mut out = Vec::new();
        for f in files {
            let path = resolve(repo, &f.path)?;
            let mut added = 0u64;
            let n = for_each_line(&path, &f.path, &f.sha256, |_, l| {
                let u: Unit = serde_json::from_str(l)?;
                ensure!(u.text.is_none() && !u.prose_opened, "refused: a reserved unit carries prose");
                if let (Some(&g), Some(k)) = (self.keys.get(&fp(&u.family_key)), u.url.as_deref().and_then(url_key)) {
                    self.urls.insert(fp(&k), g);
                    added += 1;
                }
                Ok(())
            })?;
            self.url_units += added;
            out.push(json!({"path": f.path, "sha256": f.sha256, "rows": n, "stock_unit_urls": added}));
        }
        Ok(out)
    }

    /// Adds reserved-unit title hashes by family key (units outside the stock are skipped).
    pub fn add_unit_titles(&mut self, repo: &Path, files: &[FileRef]) -> Result<Vec<Value>> {
        #[derive(Deserialize)]
        struct Unit {
            family_key: String,
            #[serde(default)]
            title_sha256: Option<String>,
            #[serde(default)]
            prose_opened: bool,
            #[serde(default)]
            text: Option<IgnoredAny>,
        }
        let mut out = Vec::new();
        for f in files {
            let path = resolve(repo, &f.path)?;
            let (mut added, mut titled) = (0u64, 0u64);
            let n = for_each_line(&path, &f.path, &f.sha256, |_, l| {
                let u: Unit = serde_json::from_str(l)?;
                ensure!(u.text.is_none() && !u.prose_opened, "refused: a reserved unit carries prose");
                let Some(t) = u.title_sha256 else { return Ok(()) };
                titled += 1;
                let t = fp_hex(&t).context("title_sha256 format")?;
                if let Some(&g) = self.keys.get(&fp(&u.family_key)) {
                    let e = self.titles.entry(t).or_default();
                    if !e.contains(&g) {
                        e.push(g);
                    }
                    added += 1;
                }
                Ok(())
            })?;
            self.title_units += added;
            out.push(json!({"path": f.path, "sha256": f.sha256, "rows": n, "titled_units": titled, "stock_unit_titles": added}));
        }
        Ok(out)
    }

    /// Adds acquired rows' text digests (none at the reservation stage).
    pub fn add_digest(&mut self, digest: &str, group: u32) -> Result<()> {
        let f = fp_hex(digest).context("text digest format")?;
        self.digests.insert(f, group);
        Ok(())
    }
}

/// Hits of one reference.
#[derive(Default)]
struct Hits {
    by_check: BTreeMap<&'static str, u64>,
    /// Hit stock groups and the checks that hit each.
    groups: BTreeMap<u32, BTreeSet<&'static str>>,
    unresolved_source_host_rows: u64,
    /// Rows on declared mirror hosts, by "domain host".
    mirror_rows: BTreeMap<String, u64>,
    rows: u64,
}

impl Hits {
    fn hit(&mut self, check: &'static str, gs: &[u32]) {
        if gs.is_empty() {
            return;
        }
        *self.by_check.entry(check).or_default() += 1;
        for g in gs {
            self.groups.entry(*g).or_default().insert(check);
        }
    }
    fn key(&mut self, ix: &StockIndex, check: &'static str, key: &str) {
        if let Some(&g) = ix.keys.get(&fp(key)) {
            self.hit(check, &[g]);
        }
    }
    fn site(&mut self, ix: &StockIndex, check: &'static str, site: &str) {
        if let Some(gs) = ix.sites.get(&fp(&normalize_host(site))) {
            self.hit(check, gs);
        }
    }
    fn url(&mut self, ix: &StockIndex, url: &str) {
        if let Some(&g) = url_key(url).and_then(|k| ix.urls.get(&fp(&k))) {
            self.hit("url", &[g]);
        }
    }
    fn digest(&mut self, ix: &StockIndex, d: Option<&str>) {
        if ix.digests.is_empty() {
            return;
        }
        if let Some(&g) = d.and_then(fp_hex).as_ref().and_then(|f| ix.digests.get(f)) {
            self.hit("text_digest", &[g]);
        }
    }
    /// One string of a JSON record, field-blind.
    fn string(&mut self, ix: &StockIndex, s: &str) {
        self.key(ix, "key", s);
        if let Some(site) = key_site(s) {
            self.site(ix, "site_key", site);
        }
        if let Some(f) = fp_hex(s) {
            if let Some(&g) = ix.keys.get(&f) {
                self.hit("hashed_key", &[g]);
            }
            if let Some(&g) = ix.digests.get(&f) {
                self.hit("text_digest", &[g]);
            }
        }
    }
    /// Every string of a record; prose (prose-named fields, strings over the bound)
    /// is skipped, never probed.
    fn record(&mut self, ix: &StockIndex, v: &Value, guard: &ProseGuard) {
        match v {
            Value::String(s) => {
                if s.split_whitespace().nth(guard.max_words).is_none() {
                    self.string(ix, s);
                }
            }
            Value::Array(xs) => xs.iter().for_each(|x| self.record(ix, x, guard)),
            Value::Object(m) => {
                for (k, x) in m {
                    if !guard.refused_fields.iter().any(|f| f == k) {
                        self.record(ix, x, guard);
                    }
                }
            }
            _ => {}
        }
    }
}

#[derive(Deserialize)]
struct CcRow {
    #[serde(default)]
    refers_to: Option<String>,
    #[serde(default)]
    record_id: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    registered_domain: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    text_sha256: Option<String>,
    #[serde(default)]
    normalized_sha256: Option<String>,
    #[serde(default)]
    folded_sha256: Option<String>,
    #[serde(default)]
    page_sha256: Option<String>,
}

#[derive(Deserialize)]
pub struct SplitGroupRow {
    pub domain: String,
    pub group_key: String,
    #[serde(default)]
    pub text: Option<IgnoredAny>,
}

/// `<urn:uuid:X>` as the FineWeb family key `fineweb_page:x`.
pub fn warc_family_key(id: &str) -> Option<String> {
    let u = id.strip_prefix("<urn:uuid:")?.strip_suffix('>')?;
    (!u.is_empty()).then(|| format!("fineweb_page:{}", u.to_ascii_lowercase()))
}

/// The declared source host a Common Crawl row sits on: its host, or any parent
/// domain of it, or its registered domain equals the declared value.
fn source_host<'a>(hosts: &'a [SourceHost], host: Option<&str>, rd: Option<&str>) -> Option<&'a SourceHost> {
    let on = |h: Option<&str>, v: &str| h.is_some_and(|h| h == v || h.strip_suffix(v).is_some_and(|p| p.ends_with('.')));
    hosts.iter().find(|s| match s.field.as_str() {
        "host" => host == Some(s.value.as_str()),
        _ => on(host, &s.value) || rd == Some(s.value.as_str()),
    })
}

fn probe(repo: &Path, r: &GateRef, ix: &StockIndex, a: &StockAllowlist) -> Result<Hits> {
    let assignment = is_assignment_role(&r.role);
    let path = if assignment { resolve_reference(repo, &r.path)? } else { resolve(repo, &r.path)? };
    let guard = &a.assignment_prose_guard;
    let mut h = Hits::default();
    let rows = for_each_line(&path, &r.path, &r.sha256, |_, l| {
        match r.kind.as_str() {
            "split_families" => {
                if assignment {
                    guard.check_line(l)?;
                }
                if !l.is_empty() {
                    h.string(ix, l);
                }
            }
            "title_sha256_list" => {
                ensure!(is_digest(l), "a title hash line is not a SHA-256");
                if let Some(gs) = fp_hex(l).and_then(|f| ix.titles.get(&f)) {
                    h.hit("raid_train_title_lineage", gs);
                }
            }
            "cc_admitted" => {
                let c: CcRow = serde_json::from_str(l)?;
                for id in [&c.refers_to, &c.record_id].into_iter().flatten() {
                    if let Some(k) = warc_family_key(id) {
                        h.key(ix, "warc_record_id", &k);
                    }
                }
                if let Some(u) = &c.url {
                    h.url(ix, u);
                }
                let url_host = c.url.as_deref().and_then(split_url).map(|(host, _)| host);
                let host = c.host.as_deref().map(normalize_host).or(url_host);
                let rd = c.registered_domain.as_deref().map(normalize_host);
                for s in [&host, &rd].into_iter().flatten() {
                    h.site(ix, "site", s);
                }
                if let Some(site) = c.group.as_deref().and_then(key_site) {
                    h.site(ix, "site", site);
                }
                if let Some(sh) = source_host(&a.source_hosts, host.as_deref(), rd.as_deref()) {
                    let rule = sh.url_key_rule.as_deref().zip(c.url.as_deref()).map(|(rule, u)| url_rule(rule, u));
                    match rule {
                        Some(UrlKey::Key(k)) => {
                            h.key(ix, "source_host_url_key", &k);
                            if let Some(gs) = ix.url_rule_keys.get(&fp(&k)) {
                                h.hit("source_host_url_key", gs);
                            }
                        }
                        Some(UrlKey::NoDocument) => {}
                        _ => h.unresolved_source_host_rows += 1,
                    }
                }
                for m in &a.mirror_hosts {
                    let on = |x: &Option<String>| {
                        x.as_deref().is_some_and(|x| x == m.host || x.strip_suffix(m.host.as_str()).is_some_and(|p| p.ends_with('.')))
                    };
                    if on(&host) || on(&rd) {
                        *h.mirror_rows.entry(format!("{} {}", m.domain, m.host)).or_default() += 1;
                    }
                }
                for d in [&c.text_sha256, &c.normalized_sha256, &c.folded_sha256, &c.page_sha256] {
                    h.digest(ix, d.as_deref());
                }
            }
            "split_groups" | "split_manifest" | "acquired_metadata" | "blocked_groups" | "trainer_bundle_rows"
            | "public_candidate_texts" | "public_candidate_splits" | "public_candidate_records" => {
                let v: Value = serde_json::from_str(l)?;
                if assignment {
                    guard.check(&v)?;
                }
                h.record(ix, &v, guard);
            }
            k => anyhow::bail!("unknown reference kind {k}"),
        }
        Ok(())
    })?;
    ensure!(rows == r.rows, "refused: {} has {rows} rows, the allow-list records {}", r.path, r.rows);
    h.rows = rows;
    Ok(h)
}

/// Group keys per domain held by the evaluation splits, from the gate's assignment
/// group files (used to take an index reservation's remainder).
pub fn evaluation_assignments(repo: &Path, a: &StockAllowlist, domains: &BTreeSet<String>) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut files: Vec<(&str, &str)> = a.test_rules.groups.iter().map(|f| (f.path.as_str(), f.sha256.as_str())).collect();
    for g in a.gate.iter().filter(|g| is_assignment_role(&g.role) && g.kind == "split_groups") {
        if !files.iter().any(|(p, _)| *p == g.path) {
            files.push((&g.path, &g.sha256));
        }
    }
    for (p, sha) in files {
        let path = resolve_reference(repo, p)?;
        for_each_line(&path, p, sha, |_, l| {
            let v: Value = serde_json::from_str(l)?;
            a.assignment_prose_guard.check(&v)?;
            let g: SplitGroupRow = serde_json::from_value(v)?;
            ensure!(g.text.is_none(), "refused: an assignment record carries text");
            if domains.contains(&g.domain) {
                out.entry(g.domain).or_default().insert(g.group_key);
            }
            Ok(())
        })?;
    }
    Ok(out)
}

/// Checks the split set's `SHA256SUMS` against the allow-list: every split
/// manifest, group, family and blocked-group file it lists must be a gate
/// reference, a stock file, a Test rule file or a declared parent reservation with
/// the same digest, and every allow-listed file under the split set must carry the
/// digest `SHA256SUMS` records. Text files (`documents.jsonl`) are covered by the
/// `manifest.jsonl` beside them.
pub fn split_completeness(repo: &Path, a: &StockAllowlist) -> Result<Value> {
    let s = &a.split_sums;
    let root = s.path.strip_suffix("/SHA256SUMS").context("split_sums path")?;
    let body = read_checked(&resolve(repo, &s.path)?, &s.path, &s.sha256)?;
    let mut sums: BTreeMap<String, String> = BTreeMap::new();
    for line in std::str::from_utf8(&body).context("SHA256SUMS UTF-8")?.lines().filter(|l| !l.is_empty()) {
        let (sha, rel) = line.split_once("  ").context("SHA256SUMS line")?;
        ensure!(is_digest(sha), "SHA256SUMS digest format");
        let rel = rel.strip_prefix("./").unwrap_or(rel);
        ensure!(sums.insert(format!("{root}/{rel}"), sha.to_string()).is_none(), "SHA256SUMS lists {rel} twice");
    }
    let mut covered: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for g in &a.gate {
        covered.insert(&g.path, (&g.sha256, "gate"));
    }
    for f in &a.test_rules.groups {
        covered.insert(&f.path, (&f.sha256, "test_rules"));
    }
    for g in &a.group_reservations {
        covered.insert(&g.path, (&g.sha256, "stock"));
    }
    for i in &a.index_reservations {
        covered.insert(&i.excluded.path, (&i.excluded.sha256, "stock_receipts"));
    }
    for u in a.unit_urls.iter().chain(&a.unit_titles) {
        covered.insert(&u.path, (&u.sha256, "stock_units"));
    }
    for n in &a.not_gated {
        covered.insert(&n.path, (&n.sha256, "not_gated"));
    }
    let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
    for (path, sha) in &sums {
        let base = path.rsplit('/').next().unwrap_or(path);
        if !SPLIT_FILES.contains(&base) {
            continue;
        }
        let (got, how) = covered.get(path.as_str()).with_context(|| format!("refused: {path} is a split file in SHA256SUMS that the gate does not account for"))?;
        ensure!(got == sha, "refused: {path} is allow-listed with digest {got}, SHA256SUMS records {sha}");
        *counts.entry(how).or_default() += 1;
    }
    for (path, (sha, _)) in &covered {
        if let Some(rest) = path.strip_prefix(root).filter(|r| r.starts_with('/')) {
            let want = sums.get(*path).with_context(|| format!("refused: {path} is under the split set but not in SHA256SUMS ({rest})"))?;
            ensure!(want == sha, "refused: {path} is allow-listed with digest {sha}, SHA256SUMS records {want}");
        }
    }
    Ok(json!({"sums": s.path, "sha256": s.sha256, "entries": sums.len(), "split_files_by_disposition": counts,
        "not_gated": a.not_gated.iter().map(|n| json!({"path": n.path, "reason": n.reason})).collect::<Vec<_>>()}))
}

/// Reads the pre-registered RAID lineage screen of R0's trainer bundle and checks
/// that the `r0_training_lineage` references are its inputs. Returns the screen's rules.
pub fn lineage_screen(repo: &Path, a: &StockAllowlist) -> Result<Value> {
    lineage_screen_for(repo, a, "r0_training_lineage", &a.lineage_screen)
}

/// The same for any reference's lineage role: the role's title-hash list and
/// trainer-bundle rows must be the screen's inputs, by digest (and the list by entry
/// count). This is how the gate is re-run with the active reference's trainer bundle.
pub fn lineage_screen_for(repo: &Path, a: &StockAllowlist, role: &str, s: &FileRef) -> Result<Value> {
    let v: Value = serde_json::from_slice(&read_checked(&resolve(repo, &s.path)?, &s.path, &s.sha256)?).context("lineage screen JSON")?;
    ensure!(v["schema"] == LINEAGE_SCREEN_SCHEMA, "lineage screen schema");
    let lineage = |kind: &str| a.gate.iter().filter(|g| g.role == role && g.kind == kind).collect::<Vec<_>>();
    let titles = lineage("title_sha256_list");
    let bundle = lineage("trainer_bundle_rows");
    ensure!(titles.len() == 1 && bundle.len() == 1, "the lineage role {role} needs one title list and one trainer bundle");
    let t = &v["inputs"]["raid_train_title_sha256"];
    ensure!(t["sha256"] == titles[0].sha256.as_str() && t["entries"] == titles[0].rows, "the title list is not the screen's input");
    ensure!(v["inputs"]["raid_train_rows"]["rows_sha256"] == bundle[0].sha256.as_str(), "the trainer bundle is not the screen's input");
    let rules: Vec<Value> = v["rules"].as_array().context("lineage screen rules")?.iter()
        .map(|r| json!({"id": r["id"], "rule": r["rule"]})).collect();
    for id in ["S1_title", "S2_reddit_author", "S3_text"] {
        ensure!(rules.iter().any(|r| r["id"] == id), "lineage screen lacks {id}");
    }
    Ok(json!({"path": s.path, "sha256": s.sha256, "schema": v["schema"], "date": v["date"], "rules": rules}))
}

/// Gate result.
pub struct Gate {
    pub report: Value,
    /// Stock groups to exclude, with the `role:check` pairs that hit each.
    pub excluded: BTreeMap<u32, BTreeSet<String>>,
}

/// Runs every gate reference. Refuses on a hit from a failing role or on an
/// unresolved source-host row; returns the closure exclusions otherwise.
pub fn run(repo: &Path, a: &StockAllowlist, groups: &[StockGroup], ix: &StockIndex) -> Result<Gate> {
    let completeness = split_completeness(repo, a)?;
    let screen = lineage_screen(repo, a)?;
    let active = match &a.active_reference {
        Some(r) => {
            let s = lineage_screen_for(repo, a, crate::stock::ACTIVE_LINEAGE_ROLE.0, &r.screen)?;
            json!({"reference": r.reference, "trainer_bundle": r.trainer_bundle, "role": crate::stock::ACTIVE_LINEAGE_ROLE.0, "screen": s})
        }
        None => Value::Null,
    };
    let mut refs = Vec::new();
    let mut excluded: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
    let mut refused: BTreeSet<u32> = BTreeSet::new();
    let mut hit_any: BTreeSet<u32> = BTreeSet::new();
    let mut failures = Vec::new();
    let mut mirror: BTreeMap<String, u64> = BTreeMap::new();
    let mut gate_refs: Vec<GateRef> = a.gate.clone();
    // The Test's group records are part of the sealed Test assignments.
    for f in &a.test_rules.groups {
        if !gate_refs.iter().any(|g| g.path == f.path) {
            let path = resolve_reference(repo, &f.path)?;
            let rows = for_each_line(&path, &f.path, &f.sha256, |_, _| Ok(()))?;
            gate_refs.push(GateRef { role: "sealed_test".into(), kind: "split_groups".into(), path: f.path.clone(), sha256: f.sha256.clone(), rows });
        }
    }
    for r in &gate_refs {
        let h = probe(repo, r, ix, a).with_context(|| format!("gate reference {}", r.path))?;
        let hit_groups = h.groups.len() as u64;
        hit_any.extend(h.groups.keys().copied());
        if hit_groups > 0 {
            if excludes(&r.role) {
                for (g, checks) in &h.groups {
                    excluded.entry(*g).or_default().extend(checks.iter().map(|c| format!("{}:{c}", r.role)));
                }
            } else {
                refused.extend(h.groups.keys().copied());
                failures.push(format!("{} ({}) shares {hit_groups} stock groups: {:?}", r.path, r.role, h.by_check));
            }
        }
        if h.unresolved_source_host_rows > 0 {
            failures.push(format!("{} has {} rows on a stock source host that no key rule resolves", r.path, h.unresolved_source_host_rows));
        }
        for (k, n) in &h.mirror_rows {
            *mirror.entry(k.clone()).or_default() += n;
        }
        let mut domains: BTreeMap<&str, u64> = BTreeMap::new();
        for g in h.groups.keys() {
            *domains.entry(groups[*g as usize].domain.as_str()).or_default() += 1;
        }
        refs.push(json!({"role": r.role, "kind": r.kind, "path": r.path, "sha256": r.sha256, "rows": h.rows,
            "hits": h.by_check, "hit_groups": hit_groups, "hit_groups_by_domain": domains,
            "unresolved_source_host_rows": h.unresolved_source_host_rows, "mirror_host_rows": h.mirror_rows,
            "disposition": if excludes(&r.role) { "exclude_hit_groups" } else { "refuse_on_hit" }}));
    }
    ensure!(failures.is_empty(), "refused by the reservation gate:\n  {}", failures.join("\n  "));
    for (role, _) in crate::stock::ROLES {
        ensure!(gate_refs.iter().any(|g| g.role == role), "no {role} reference was checked");
    }
    if a.active_reference.is_some() {
        let role = crate::stock::ACTIVE_LINEAGE_ROLE.0;
        ensure!(gate_refs.iter().any(|g| g.role == role), "no {role} reference was checked");
    }
    // Overlap left in the stock: groups any reference hit that are not excluded.
    let remaining: BTreeSet<u32> = hit_any.iter().copied().filter(|g| !excluded.contains_key(g)).collect();
    ensure!(remaining.is_empty() && refused.is_empty(), "{} hit stock groups would remain", remaining.len());
    let mut by_domain: BTreeMap<&str, u64> = BTreeMap::new();
    let mut by_reason: BTreeMap<&str, u64> = BTreeMap::new();
    for (g, reasons) in &excluded {
        *by_domain.entry(groups[*g as usize].domain.as_str()).or_default() += 1;
        for r in reasons {
            *by_reason.entry(r.as_str()).or_default() += 1;
        }
    }
    let report = json!({
        "schema": GATE_SCHEMA,
        "split_completeness": completeness,
        "lineage_screen": screen,
        "stock_index": {"keys": ix.keys.len(), "site_groups": ix.site_groups, "unit_urls": ix.url_units,
            "unit_titles": ix.title_units, "url_rule_keys": ix.url_rule_keys.len(), "text_digests": ix.digests.len()},
        "text_digest_check": if ix.digests.is_empty() {
            "not applicable: the reservation-stage stock holds no text; the check runs on acquired rows"
        } else { "run" },
        "references": refs,
        "excluded_groups": excluded.len(),
        "excluded_by_domain": by_domain,
        "excluded_by_reason": by_reason,
        "mirror_host_rows_pending_text_check": mirror,
        "remaining_overlap": remaining.len(),
        "verdict": "pass",
    });
    let mut report = report;
    if !active.is_null() {
        report["active_reference_lineage"] = active;
    }
    Ok(Gate { report, excluded })
}
