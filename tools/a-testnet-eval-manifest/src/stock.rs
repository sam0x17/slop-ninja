//! Testnet public stock (pool v2): the unassigned remainder of the evaluation
//! reservations behind the sealed Test.
//!
//! Testnet-only (`testnet_public_stock_only`). The stock is dated public text and
//! every document can be recognized; this exposure is a stated pilot limit, so the
//! stock is never a mainnet evaluation source. It is read only from files named in
//! a tracked allow-list (`slop_ninja_testnet_public_stock_allowlist_v2`) by exact
//! repository path and SHA-256:
//!
//! - group reservations: `reserved-unassigned/groups.jsonl` files, whose every row
//!   must be a `reserved_unassigned` split group;
//! - index reservations: a reservation index whose remainder is not written as
//!   groups (Wikipedia). The remainder is the index minus every evaluation
//!   assignment the gate lists for that domain, minus the reservation's own overlap
//!   exclusions, and must equal the count the allow-list records;
//! - unit URLs of reserved units, for the gate's URL check;
//! - unit titles (`title_sha256` of reserved wiki units), for the pre-registered
//!   RAID title screen (S1) of the reference model's training lineage.
//!
//! The allow-list also pins the split set's `SHA256SUMS`: every split manifest,
//! group, family and blocked-group file it lists must be a gate reference, a stock
//! file or a declared parent reservation, with the same digest, so no split file
//! can be left out of the gate.
//!
//! Stock paths pass the same name refusals as the Dev-iter tool (no Test, CAL,
//! Dev-select, sealed or Tier C component). The sealed Test's group records and its
//! acquisition summary are read for their rules and counts only (the per-group cap,
//! the per-unit yield, the realized yield and the domain mass): those references may
//! name Test paths, are read from `data/` only, and refuse any record that carries
//! a prose-named field or a string longer than the allow-list's measured bound
//! ([`ProseGuard`]). No prose is read from the reservations, which hold none.
//!
//! Expected documents per stock group follow the Test's excerpt rules: the Test
//! cap per group, and the smallest per-unit yield its uncapped groups record. Where
//! every Test group of a domain is capped, a stock group with at least as many units
//! as the smallest capped Test group is capped too (the yield is at least
//! `cap / units` there); a smaller one counts zero. Capacity then scales each
//! domain's expected documents by the yield the Test's own acquisition realized on
//! the groups it processed, never above 1.
use crate::{check_rel, data_dir, is_digest, rel_under, resolve, sha256_hex};
use anyhow::{Context, Result, bail, ensure};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf};

pub const STOCK_ALLOWLIST_SCHEMA: &str = "slop_ninja_testnet_public_stock_allowlist_v2";
/// Allow-list v3: v2 plus the `r2_test2_carve_v3` exclusion (the Test-2 T carve and
/// the mock reserve), and optionally the active reference's training lineage.
/// It names sealed groups by file, so it lives in ignored storage and is never tracked.
pub const STOCK_ALLOWLIST_SCHEMA_V3: &str = "slop_ninja_testnet_public_stock_allowlist_v3";
/// The gate role for the active reference's training lineage (allow-list v3 with
/// `active_reference`). `r0_training_lineage` stays required: R0's lineage remains
/// excluded when a later reference is added.
pub const ACTIVE_LINEAGE_ROLE: (&str, &[&str]) = ("reference_training_lineage", &["trainer_bundle_rows", "title_sha256_list"]);
pub const STOCK_PURPOSE: &str = "testnet_public_stock_only";
pub const STOCK_SPLIT: &str = "reserved_unassigned";
pub const GROUP_SCHEMA: &str = "slop_ninja_a_competitive_split_group_v1";
pub const INDEX_UNIT_SCHEMA: &str = "slop_ninja_a_pool_reserved_unit_metadata_v1";
pub const EXPECTED_RULE: &str = "test_excerpt_rules_min_yield_v1";
/// Expected documents are kept in millionths of a document.
pub const MICRO: f64 = 1e6;
/// The tracked public-stock allow-list.
pub const TRACKED_STOCK_ALLOWLIST: &str = include_str!("../allowlists/testnet-public-stock-v2.json");
/// The pre-registered RAID lineage screen the allow-list binds.
pub const LINEAGE_SCREEN_SCHEMA: &str = "slop_ninja_a_eval_acquisition_raid_lineage_screen_v1";
/// Split-set files that must be accounted for, by base name.
pub const SPLIT_FILES: [&str; 4] = ["manifest.jsonl", "groups.jsonl", "families.txt", "blocked-groups.jsonl"];
/// URL key rules a source host may name.
pub const URL_KEY_RULES: [&str; 2] = ["openreview_forum_url_v1", "scipost_submission_url_v1"];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileRef {
    pub path: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GroupReservation {
    pub reservation: String,
    pub path: String,
    pub sha256: String,
    pub rows: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IndexReservation {
    pub reservation: String,
    pub domain: String,
    pub path: String,
    pub sha256: String,
    pub rows: u64,
    /// The reservation's own overlap exclusions (`group_excluded` receipts).
    pub excluded: FileRef,
    /// The remainder count the reservation recorded.
    pub remainder_units: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestRules {
    /// The sealed Test's group records (one per reservation).
    pub groups: Vec<FileRef>,
    /// The sealed Test's acquisition summary (per-domain counts).
    pub acquisition_summary: FileRef,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceHost {
    pub domain: String,
    /// `registered_domain` or `host`.
    pub field: String,
    pub value: String,
    /// How a page URL on this host names a stock key (one of [`URL_KEY_RULES`]), if it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_key_rule: Option<String>,
}

/// A host that republishes a stock domain's documents (court-opinion, article or
/// book mirrors). Keys cannot clear its pages; its Common Crawl rows are counted
/// for the text-level check at acquisition.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MirrorHost {
    pub domain: String,
    pub host: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GateRef {
    pub role: String,
    pub kind: String,
    pub path: String,
    pub sha256: String,
    /// Lines the file holds; a different count refuses.
    pub rows: u64,
}

/// A split-set file deliberately left out of the gate.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotGated {
    pub path: String,
    pub sha256: String,
    pub reason: String,
}

/// What an assignment record (sealed Test or evaluation split) may carry: no field
/// with a prose name, and no string longer than the longest one measured in the
/// allow-listed assignment files.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProseGuard {
    pub max_words: usize,
    pub refused_fields: Vec<String>,
    /// Where the bound was measured.
    pub measured: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StockAllowlist {
    pub schema: String,
    pub allowlist_id: String,
    pub purpose: String,
    pub split: String,
    pub removed_by: String,
    pub group_reservations: Vec<GroupReservation>,
    pub index_reservations: Vec<IndexReservation>,
    pub unit_urls: Vec<FileRef>,
    pub unit_titles: Vec<FileRef>,
    pub test_rules: TestRules,
    pub source_hosts: Vec<SourceHost>,
    pub mirror_hosts: Vec<MirrorHost>,
    pub assignment_prose_guard: ProseGuard,
    /// The split set's `SHA256SUMS`.
    pub split_sums: FileRef,
    pub not_gated: Vec<NotGated>,
    /// The pre-registered RAID lineage screen record (R0's trainer bundle).
    pub lineage_screen: FileRef,
    pub gate: Vec<GateRef>,
    /// v3 only: the carve and reserve the pool excludes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carve_exclusion: Option<CarveExclusion>,
    /// v3 only: the active reference whose training lineage is also excluded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_reference: Option<ActiveReference>,
}

/// The `r2_test2_carve_v3` groups a v3 pool excludes, and the weights the carve
/// record fixes for the remaining stock (W_A8, or a release's re-waterfill). A pool
/// build refuses before writing anything if its own weights differ.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CarveExclusion {
    pub rule: String,
    /// The carve groups file (`slop_ninja_a2_carve_group_v1` rows).
    pub groups: FileRef,
    pub rows: u64,
    /// The carve record (or release record) that produced the groups.
    pub record: FileRef,
    pub weights_ppm: BTreeMap<String, u64>,
    /// The v2 allow-list this one extends.
    pub base_allowlist_sha256: String,
}

/// The reference whose trainer bundle and RAID title screen the gate also applies
/// (for §14.2's lineage re-run with R_0′'s trainer bundle).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ActiveReference {
    /// The reference's identifier (an artifact id or label).
    pub reference: String,
    /// The trainer bundle's id, recorded with the gate result.
    pub trainer_bundle: String,
    /// The pre-registered lineage screen whose inputs are this bundle's rows and title list.
    pub screen: FileRef,
}

/// Gate roles and the kinds each may read. Every role is required.
pub const ROLES: [(&str, &[&str]); 7] = [
    ("train", &["split_manifest"]),
    ("train_held", &["split_manifest", "blocked_groups"]),
    ("sealed_test", &["split_groups", "split_families", "split_manifest", "acquired_metadata"]),
    ("evaluation", &["split_groups", "split_families", "split_manifest"]),
    ("r0_training_lineage", &["trainer_bundle_rows", "title_sha256_list"]),
    ("cc_mainnet_stock", &["cc_admitted"]),
    ("public_candidate_v1", &["public_candidate_texts", "public_candidate_records", "public_candidate_splits"]),
];

/// Roles whose files may name sealed or evaluation splits (read for keys and digests only).
pub fn is_assignment_role(role: &str) -> bool {
    matches!(role, "sealed_test" | "evaluation")
}

fn digest_ok(s: &str) -> bool {
    is_digest(s)
}

impl StockAllowlist {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let a: Self = serde_json::from_slice(bytes).context("public-stock allow-list JSON")?;
        let v3 = a.schema == STOCK_ALLOWLIST_SCHEMA_V3;
        ensure!(v3 || a.schema == STOCK_ALLOWLIST_SCHEMA, "allow-list schema {}", a.schema);
        if v3 {
            let c = a.carve_exclusion.as_ref().context("allow-list v3 needs its carve exclusion")?;
            ensure!(c.rule == crate::carve_v3::CARVE_RULE, "carve exclusion rule {}", c.rule);
            check_rel(&c.groups.path)?;
            check_reference(&c.record.path)?;
            ensure!(digest_ok(&c.groups.sha256) && digest_ok(&c.record.sha256) && digest_ok(&c.base_allowlist_sha256), "carve exclusion digests");
            ensure!(c.rows > 0, "an empty carve exclusion");
            ensure!(c.weights_ppm.values().sum::<u64>() == crate::capacity::PPM, "carve exclusion weights must sum to {} ppm", crate::capacity::PPM);
        } else {
            ensure!(a.carve_exclusion.is_none() && a.active_reference.is_none(), "a carve exclusion or an active reference needs allow-list v3");
        }
        if let Some(r) = &a.active_reference {
            ensure!(!r.reference.is_empty() && !r.trainer_bundle.is_empty(), "active reference id and trainer bundle");
            check_rel(&r.screen.path)?;
            ensure!(digest_ok(&r.screen.sha256), "active reference screen digest");
            ensure!(r.screen.path != a.lineage_screen.path, "the active reference needs its own lineage screen");
        }
        ensure!(a.purpose == STOCK_PURPOSE, "allow-list purpose must be {STOCK_PURPOSE}");
        ensure!(a.split == STOCK_SPLIT, "refused: allow-listed split {:?}; only {STOCK_SPLIT}", a.split);
        ensure!(!a.allowlist_id.is_empty() && !a.removed_by.is_empty(), "allow-list id and removal milestone");
        ensure!(!a.group_reservations.is_empty() || !a.index_reservations.is_empty(), "empty allow-list");
        let mut paths = BTreeSet::new();
        let mut stock_file = |p: &str, sha: &str| -> Result<()> {
            check_rel(p)?;
            ensure!(digest_ok(sha), "{p}: digest format");
            ensure!(paths.insert(p.to_string()), "duplicate allow-listed path {p}");
            Ok(())
        };
        for g in &a.group_reservations {
            stock_file(&g.path, &g.sha256)?;
            ensure!(g.rows > 0 && !g.reservation.is_empty(), "{}: rows and reservation", g.path);
        }
        for i in &a.index_reservations {
            stock_file(&i.path, &i.sha256)?;
            stock_file(&i.excluded.path, &i.excluded.sha256)?;
            ensure!(i.rows > 0 && i.remainder_units > 0 && !i.domain.is_empty(), "{}: rows, remainder and domain", i.path);
        }
        for u in a.unit_urls.iter().chain(&a.unit_titles) {
            stock_file(&u.path, &u.sha256)?;
        }
        let mut refs = BTreeSet::new();
        let mut reference = |p: &str, sha: &str, assignment: bool| -> Result<()> {
            if assignment {
                check_reference(p)?;
            } else {
                check_rel(p)?;
            }
            ensure!(digest_ok(sha), "{p}: digest format");
            ensure!(!paths.contains(p), "{p} is both a stock file and a reference");
            ensure!(refs.insert(p.to_string()), "duplicate reference {p}");
            Ok(())
        };
        ensure!(!a.test_rules.groups.is_empty(), "the Test rules need the Test group records");
        for f in &a.test_rules.groups {
            reference(&f.path, &f.sha256, true)?;
        }
        reference(&a.test_rules.acquisition_summary.path, &a.test_rules.acquisition_summary.sha256, true)?;
        reference(&a.split_sums.path, &a.split_sums.sha256, false)?;
        ensure!(a.split_sums.path.ends_with("/SHA256SUMS"), "split_sums must name a SHA256SUMS file");
        reference(&a.lineage_screen.path, &a.lineage_screen.sha256, false)?;
        for n in &a.not_gated {
            check_reference(&n.path)?;
            ensure!(digest_ok(&n.sha256) && !n.reason.is_empty(), "{}: digest and reason", n.path);
        }
        let g = &a.assignment_prose_guard;
        ensure!(g.max_words > 0 && !g.refused_fields.is_empty() && !g.measured.is_empty(), "assignment prose guard");
        // Test group records also serve the gate; the same file may appear there.
        let rules: BTreeSet<&str> = a.test_rules.groups.iter().map(|f| f.path.as_str()).collect();
        let mut gate_paths = BTreeSet::new();
        let active = a.active_reference.is_some().then_some(ACTIVE_LINEAGE_ROLE);
        for g in &a.gate {
            let kinds = ROLES.iter().chain(active.iter()).find(|(r, _)| *r == g.role).map(|(_, k)| *k);
            let kinds = kinds.with_context(|| format!("unknown gate role {:?} (the active reference's lineage needs allow-list v3 with active_reference)", g.role))?;
            ensure!(kinds.contains(&g.kind.as_str()), "gate role {} cannot read kind {}", g.role, g.kind);
            ensure!(gate_paths.insert(g.path.as_str()), "duplicate gate reference {}", g.path);
            if rules.contains(g.path.as_str()) {
                ensure!(g.role == "sealed_test" && g.kind == "split_groups", "{} is a Test rule file", g.path);
                continue;
            }
            reference(&g.path, &g.sha256, is_assignment_role(&g.role))?;
        }
        for (role, _) in ROLES.iter().chain(active.iter()) {
            ensure!(a.gate.iter().any(|g| g.role == *role), "the gate needs at least one {role} reference");
        }
        if let Some(r) = &a.active_reference {
            reference(&r.screen.path, &r.screen.sha256, false)?;
        }
        for h in &a.source_hosts {
            ensure!(matches!(h.field.as_str(), "registered_domain" | "host"), "source host field {}", h.field);
            ensure!(!h.value.is_empty() && !h.domain.is_empty(), "source host entry");
            ensure!(normalize_host(&h.value) == h.value, "source host {} is not normalized", h.value);
            if let Some(r) = &h.url_key_rule {
                ensure!(URL_KEY_RULES.contains(&r.as_str()), "unknown URL key rule {r}");
            }
        }
        for m in &a.mirror_hosts {
            ensure!(!m.domain.is_empty() && normalize_host(&m.host) == m.host && !m.host.is_empty(), "mirror host {}", m.host);
        }
        Ok(a)
    }
}

/// A reference that may name an evaluation split: relative, normalized and under
/// `data/`, but exempt from the split-name refusals.
pub fn check_reference(rel: &str) -> Result<()> {
    if rel.is_empty() || rel.starts_with('/') || rel.contains('\\') {
        bail!("refused: {rel:?} is not a relative repository path");
    }
    let comps: Vec<&str> = rel.split('/').collect();
    ensure!(!comps.iter().any(|c| c.is_empty() || *c == "." || *c == ".."), "refused: {rel:?} is not a normalized path");
    ensure!(comps[0] == "data" && comps.len() >= 2, "refused: {rel:?} is not under data/");
    Ok(())
}

/// Resolve a reference (see [`check_reference`]) to a regular file under `data/`.
pub fn resolve_reference(repo: &Path, rel: &str) -> Result<PathBuf> {
    check_reference(rel)?;
    let data = data_dir(repo)?;
    let real = data.parent().context("repository root")?.join(rel).canonicalize().with_context(|| format!("resolve {rel}"))?;
    rel_under(&data, &real).with_context(|| format!("{rel} resolves outside data/"))?;
    ensure!(real.is_file(), "{rel} is not a regular file");
    Ok(real)
}

/// Host normal form: lowercase, port and trailing dots dropped, and leading `www.`
/// and `m.` labels stripped while a dot remains.
pub fn normalize_host(h: &str) -> String {
    let mut s = h.trim().to_ascii_lowercase();
    if let Some(i) = s.find(':') {
        s.truncate(i);
    }
    while s.ends_with('.') {
        s.pop();
    }
    loop {
        let rest = s.strip_prefix("www.").or_else(|| s.strip_prefix("m."));
        match rest {
            Some(r) if r.contains('.') => s = r.to_string(),
            _ => break,
        }
    }
    s
}

/// Splits an absolute URL into its normalized host and the rest (path, query),
/// without the scheme, user info or fragment.
pub fn split_url(url: &str) -> Option<(String, &str)> {
    let rest = url.trim().split_once("://")?.1;
    let rest = rest.split('#').next()?;
    let (auth, tail) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = normalize_host(auth.rsplit('@').next()?);
    (!host.is_empty()).then_some((host, tail))
}

/// The unit-URL key: normalized host plus path and query, without a trailing slash.
pub fn url_key(url: &str) -> Option<String> {
    let (host, tail) = split_url(url)?;
    Some(format!("{host}{}", tail.strip_suffix('/').unwrap_or(tail)))
}

/// What a URL rule makes of a page on a source host.
#[derive(Debug, PartialEq, Eq)]
pub enum UrlKey {
    /// The page names this stock key.
    Key(String),
    /// The page is of a kind that holds no stock document.
    NoDocument,
    /// The rule cannot tell: the page refuses the stock until texts are compared.
    Unresolved,
}

/// `scipost_submission_url_v1`: `/submissions/<id>` names the SciPost submission
/// `<id>` (version suffix dropped); `/contributor/<n>` is a profile page.
pub fn scipost_url_key(url: &str) -> UrlKey {
    let Some((_, tail)) = split_url(url) else { return UrlKey::Unresolved };
    let path = tail.split('?').next().unwrap_or("");
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match segs.as_slice() {
        ["submissions", id, ..] => UrlKey::Key(format!("scipost_submission:{}", strip_version(id))),
        ["contributor", n] if n.bytes().all(|b| b.is_ascii_digit()) => UrlKey::NoDocument,
        _ => UrlKey::Unresolved,
    }
}

/// An arXiv or SciPost identifier without its `vN` suffix.
pub fn strip_version(id: &str) -> &str {
    match id.rfind('v') {
        Some(i) if i > 0 && i + 1 < id.len() && id[i + 1..].bytes().all(|b| b.is_ascii_digit()) => &id[..i],
        _ => id,
    }
}

/// Applies a source host's URL key rule.
pub fn url_rule(rule: &str, url: &str) -> UrlKey {
    match rule {
        "openreview_forum_url_v1" => openreview_forum_key(url).map_or(UrlKey::Unresolved, UrlKey::Key),
        "scipost_submission_url_v1" => scipost_url_key(url),
        _ => UrlKey::Unresolved,
    }
}

impl ProseGuard {
    /// Refuses an assignment record that carries a prose-named field or a string
    /// longer than the bound; names the field, never its content.
    pub fn check(&self, v: &serde_json::Value) -> Result<()> {
        fn walk(g: &ProseGuard, v: &serde_json::Value, path: &mut String) -> Result<()> {
            match v {
                serde_json::Value::String(s) => {
                    let w = s.split_whitespace().count();
                    ensure!(w <= g.max_words, "refused: an assignment record carries {w} words at {path} (bound {})", g.max_words);
                }
                serde_json::Value::Array(xs) => {
                    for x in xs {
                        walk(g, x, path)?;
                    }
                }
                serde_json::Value::Object(m) => {
                    for (k, x) in m {
                        ensure!(!g.refused_fields.iter().any(|f| f == k), "refused: an assignment record carries text ({path}.{k})");
                        let n = path.len();
                        path.push('.');
                        path.push_str(k);
                        walk(g, x, path)?;
                        path.truncate(n);
                    }
                }
                _ => {}
            }
            Ok(())
        }
        walk(self, v, &mut String::new())
    }

    /// The same bound for a plain line (a family key).
    pub fn check_line(&self, l: &str) -> Result<()> {
        let w = l.split_whitespace().count();
        ensure!(w <= self.max_words, "refused: an assignment line carries {w} words (bound {})", self.max_words);
        Ok(())
    }
}

/// A reader that hashes every byte it passes on.
pub struct Hashing<R> {
    inner: R,
    hash: Sha256,
    bytes: u64,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hash.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
}

/// Streams the lines of an allow-listed file (zstd-decoded when it ends in `.zst`)
/// to `f`, then checks the stored file's SHA-256. Returns the line count.
pub fn for_each_line(path: &Path, rel: &str, sha256: &str, mut f: impl FnMut(usize, &str) -> Result<()>) -> Result<u64> {
    let file = fs::File::open(path).with_context(|| format!("open {rel}"))?;
    let mut hashing = Hashing { inner: BufReader::with_capacity(1 << 20, file), hash: Sha256::new(), bytes: 0 };
    let mut n = 0u64;
    {
        let reader: Box<dyn BufRead + '_> = if rel.ends_with(".zst") {
            Box::new(BufReader::with_capacity(1 << 20, zstd::stream::read::Decoder::new(&mut hashing)?))
        } else {
            Box::new(BufReader::with_capacity(1 << 20, &mut hashing))
        };
        let mut reader = reader;
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).with_context(|| format!("{rel} line {}", n + 1))? == 0 {
                break;
            }
            n += 1;
            let l = line.strip_suffix('\n').unwrap_or(&line);
            f(n as usize, l).with_context(|| format!("{rel} line {n}"))?;
        }
    }
    // Hash any bytes a decoder left unread (trailing frames or padding).
    std::io::copy(&mut hashing, &mut std::io::sink())?;
    let got = hex::encode(hashing.hash.finalize());
    ensure!(got == sha256, "refused: {rel} digest {got} differs from the allow-list");
    Ok(n)
}

/// Reads a whole allow-listed small file and checks its digest.
pub fn read_checked(path: &Path, rel: &str, sha256: &str) -> Result<Vec<u8>> {
    let b = fs::read(path).with_context(|| format!("read {rel}"))?;
    ensure!(sha256_hex(&b) == sha256, "refused: {rel} digest differs from the allow-list");
    Ok(b)
}

#[derive(Deserialize)]
struct GroupRow {
    schema: String,
    split: String,
    domain: String,
    group_key: String,
    #[serde(default)]
    family_keys: Vec<String>,
    units: u64,
    #[serde(default)]
    text: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct TestGroupRow {
    schema: String,
    split: String,
    domain: String,
    units: u64,
    expected_docs_capped: f64,
    max_docs_per_group: f64,
    order_in_split_domain: u64,
    #[serde(default)]
    text: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct IndexUnit {
    schema: String,
    family_key: String,
    #[serde(default)]
    prose_opened: bool,
}

#[derive(Deserialize)]
struct Receipt {
    action: String,
    domain: String,
    group_key: String,
}

/// One stock group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StockGroup {
    pub reservation: String,
    pub domain: String,
    pub group_key: String,
    /// Family keys; empty for an index group, whose family key is its group key.
    pub family_keys: Vec<String>,
    pub units: u64,
    pub expected_udocs: u64,
}

impl StockGroup {
    pub fn families(&self) -> impl Iterator<Item = &str> {
        let own = self.family_keys.is_empty().then_some(self.group_key.as_str());
        self.family_keys.iter().map(String::as_str).chain(own)
    }
}

/// The Test's excerpt rules for one domain.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct DomainRule {
    pub cap: f64,
    /// Smallest per-unit yield of an uncapped Test group (None: every group capped).
    pub min_yield: Option<f64>,
    /// Distinct per-unit yields seen (more than one: several collections).
    pub yields_seen: usize,
    /// Fewest units of a capped Test group.
    pub capped_min_units: Option<u64>,
    pub test_groups: u64,
    /// Expected documents of the groups the Test acquisition processed.
    pub processed_expected: f64,
    pub processed_groups: u64,
    pub realized_documents: u64,
    /// Test human documents (the domain mass).
    pub mass: u64,
}

impl DomainRule {
    pub fn expected(&self, units: u64) -> f64 {
        match (self.min_yield, self.capped_min_units) {
            (Some(y), _) => (units as f64 * y).min(self.cap),
            (None, Some(u)) if units >= u => self.cap,
            _ => 0.0,
        }
    }

    /// Realized over expected on the Test's processed groups, at most 1.
    pub fn realization(&self) -> f64 {
        if self.processed_expected > 0.0 { (self.realized_documents as f64 / self.processed_expected).min(1.0) } else { 0.0 }
    }

    /// The same at the one-sided lower bound of the realized count (Poisson,
    /// Wilson-Hilferty, normal quantile `z`), at most 1.
    pub fn realization_lower(&self, z: f64) -> f64 {
        if self.processed_expected <= 0.0 || self.realized_documents == 0 {
            return 0.0;
        }
        let x = self.realized_documents as f64;
        let lo = x * (1.0 - 1.0 / (9.0 * x) - z / (3.0 * x.sqrt())).powi(3);
        (lo.max(0.0) / self.processed_expected).min(1.0)
    }
}

#[derive(Deserialize)]
struct AcquisitionDomain {
    documents: u64,
    groups_processed: u64,
}

/// Reads the Test's rules: caps and yields from its group records, realized yield
/// and domain mass from its acquisition summary.
pub fn test_rules(repo: &Path, rules: &TestRules, guard: &ProseGuard) -> Result<BTreeMap<String, DomainRule>> {
    let s = &rules.acquisition_summary;
    let bytes = read_checked(&resolve_reference(repo, &s.path)?, &s.path, &s.sha256)?;
    let summary: serde_json::Value = serde_json::from_slice(&bytes).context("acquisition summary JSON")?;
    guard.check(&summary).context("the acquisition summary")?;
    let per: BTreeMap<String, AcquisitionDomain> =
        serde_json::from_value(summary["per_domain"].clone()).context("acquisition summary per_domain")?;
    let mut out: BTreeMap<String, DomainRule> = BTreeMap::new();
    let mut yields: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    for f in &rules.groups {
        let path = resolve_reference(repo, &f.path)?;
        for_each_line(&path, &f.path, &f.sha256, |_, l| {
            let v: serde_json::Value = serde_json::from_str(l)?;
            guard.check(&v)?;
            let r: TestGroupRow = serde_json::from_value(v)?;
            ensure!(r.text.is_none(), "refused: a Test group record carries text");
            ensure!(r.schema == GROUP_SCHEMA && r.split == "test", "not a Test group record");
            ensure!(r.units > 0 && r.max_docs_per_group > 0.0 && r.expected_docs_capped >= 0.0, "group fields");
            let e = out.entry(r.domain.clone()).or_default();
            ensure!(e.test_groups == 0 || e.cap == r.max_docs_per_group, "{}: two Test caps", r.domain);
            e.cap = r.max_docs_per_group;
            e.test_groups += 1;
            if r.expected_docs_capped < r.max_docs_per_group {
                let y = r.expected_docs_capped / r.units as f64;
                e.min_yield = Some(e.min_yield.map_or(y, |m: f64| m.min(y)));
                yields.entry(r.domain.clone()).or_default().insert(y.to_bits());
            } else {
                e.capped_min_units = Some(e.capped_min_units.map_or(r.units, |m| m.min(r.units)));
            }
            let processed = per.get(&r.domain).map_or(0, |a| a.groups_processed);
            if r.order_in_split_domain < processed {
                e.processed_expected += r.expected_docs_capped;
                e.processed_groups += 1;
            }
            Ok(())
        })?;
    }
    for (d, a) in &per {
        let e = out.get_mut(d).with_context(|| format!("{d}: acquisition summary names a domain with no Test groups"))?;
        ensure!(e.processed_groups == a.groups_processed, "{d}: processed groups {} of {}", e.processed_groups, a.groups_processed);
        e.realized_documents = a.documents;
        e.mass = a.documents;
        e.yields_seen = yields.get(d).map_or(0, BTreeSet::len);
    }
    Ok(out)
}

/// The loaded stock before the gate.
#[derive(Debug, Default)]
pub struct Stock {
    pub groups: Vec<StockGroup>,
    pub inputs: Vec<serde_json::Value>,
}

/// Loads every allow-listed reservation as stock groups with expected documents.
/// `assigned` maps an index domain to the group keys the evaluation splits hold.
pub fn load_stock(
    repo: &Path,
    a: &StockAllowlist,
    rules: &BTreeMap<String, DomainRule>,
    assigned: &BTreeMap<String, BTreeSet<String>>,
) -> Result<Stock> {
    let mut stock = Stock::default();
    let mut keys = BTreeSet::new();
    let expected = |domain: &str, units: u64| -> Result<u64> {
        let r = rules.get(domain).with_context(|| format!("{domain}: no Test rule for a stock domain"))?;
        Ok((r.expected(units) * MICRO).round() as u64)
    };
    for g in &a.group_reservations {
        let path = resolve(repo, &g.path)?;
        let mut units = 0u64;
        let n = for_each_line(&path, &g.path, &g.sha256, |_, l| {
            let r: GroupRow = serde_json::from_str(l)?;
            ensure!(r.text.is_none(), "refused: a reservation group carries text");
            ensure!(r.schema == GROUP_SCHEMA, "group schema {}", r.schema);
            ensure!(r.split == STOCK_SPLIT, "refused: group split {:?}; only {STOCK_SPLIT}", r.split);
            ensure!(r.units > 0 && !r.group_key.is_empty() && !r.domain.is_empty(), "group fields");
            ensure!(r.family_keys.len() as u64 == r.units, "{}: units and family keys differ", r.group_key);
            ensure!(keys.insert(r.group_key.clone()), "duplicate stock group {}", r.group_key);
            units += r.units;
            stock.groups.push(StockGroup {
                reservation: g.reservation.clone(),
                expected_udocs: expected(&r.domain, r.units)?,
                domain: r.domain,
                group_key: r.group_key,
                family_keys: r.family_keys,
                units: r.units,
            });
            Ok(())
        })?;
        ensure!(n == g.rows, "{}: {n} rows, allow-list records {}", g.path, g.rows);
        stock.inputs.push(serde_json::json!({"kind": "group_reservation", "reservation": g.reservation,
            "path": g.path, "sha256": g.sha256, "rows": n, "units": units}));
    }
    for i in &a.index_reservations {
        let ex_path = resolve(repo, &i.excluded.path)?;
        let mut excluded = BTreeSet::new();
        for_each_line(&ex_path, &i.excluded.path, &i.excluded.sha256, |_, l| {
            let r: Receipt = serde_json::from_str(l)?;
            if r.domain == i.domain {
                ensure!(r.action == "group_excluded", "receipt action {}", r.action);
                excluded.insert(r.group_key);
            }
            Ok(())
        })?;
        let held = assigned.get(&i.domain).cloned().unwrap_or_default();
        ensure!(!held.is_empty(), "{}: no evaluation assignments listed for the index domain", i.domain);
        let path = resolve(repo, &i.path)?;
        let (mut seen_assigned, mut seen_excluded, mut left) = (0u64, 0u64, 0u64);
        let n = for_each_line(&path, &i.path, &i.sha256, |_, l| {
            let u: IndexUnit = serde_json::from_str(l)?;
            ensure!(u.schema == INDEX_UNIT_SCHEMA, "index unit schema {}", u.schema);
            ensure!(!u.prose_opened, "refused: an index unit records opened prose");
            if held.contains(&u.family_key) {
                seen_assigned += 1;
            } else if excluded.contains(&u.family_key) {
                seen_excluded += 1;
            } else {
                ensure!(keys.insert(u.family_key.clone()), "duplicate stock group {}", u.family_key);
                left += 1;
                stock.groups.push(StockGroup {
                    reservation: i.reservation.clone(),
                    domain: i.domain.clone(),
                    expected_udocs: expected(&i.domain, 1)?,
                    group_key: u.family_key,
                    family_keys: Vec::new(),
                    units: 1,
                });
            }
            Ok(())
        })?;
        ensure!(n == i.rows, "{}: {n} rows, allow-list records {}", i.path, i.rows);
        ensure!(seen_assigned == held.len() as u64, "{}: {} evaluation groups, {seen_assigned} in the index", i.domain, held.len());
        ensure!(seen_excluded == excluded.len() as u64, "{}: {} excluded groups, {seen_excluded} in the index", i.domain, excluded.len());
        ensure!(left == i.remainder_units, "{}: remainder {left}, the reservation records {}", i.domain, i.remainder_units);
        stock.inputs.push(serde_json::json!({"kind": "index_reservation", "reservation": i.reservation,
            "domain": i.domain, "path": i.path, "sha256": i.sha256, "rows": n, "evaluation_assigned": seen_assigned,
            "reservation_excluded": seen_excluded, "remainder": left,
            "excluded": {"path": i.excluded.path, "sha256": i.excluded.sha256}}));
    }
    stock.groups.sort_by(|x, y| (&x.domain, &x.group_key).cmp(&(&y.domain, &y.group_key)));
    Ok(stock)
}

/// The URL-key rule for OpenReview forum pages: `https://openreview.net/forum?id=X`
/// names the stock group `openreview:forum:X`.
pub fn openreview_forum_key(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let q = rest.strip_prefix("openreview.net/forum?")?;
    let id = q.split('&').find_map(|kv| kv.strip_prefix("id="))?;
    let id = id.split('#').next()?;
    (!id.is_empty()).then(|| format!("openreview:forum:{id}"))
}

/// The site a FineWeb stock group names (`fineweb_site:X` or `fineweb_blog:X`).
pub fn group_site(group_key: &str) -> Option<&str> {
    group_key.strip_prefix("fineweb_site:").or_else(|| group_key.strip_prefix("fineweb_blog:"))
}

/// The site any site-scoped key names, whatever its prefix: FineWeb sites and
/// blogs, and Common Crawl site and host groups.
pub fn key_site(key: &str) -> Option<&str> {
    group_site(key).or_else(|| key.strip_prefix("cc_site:")).or_else(|| key.strip_prefix("cc_host:"))
}

/// The SciPost submission a peer-review family key names (`orb:<id>:v1:r0`).
pub fn orb_submission(family_key: &str) -> Option<String> {
    let id = family_key.strip_prefix("orb:")?.split(':').next()?;
    (!id.is_empty()).then(|| format!("scipost_submission:{}", strip_version(id)))
}

/// The output directory: new, under `data/`, passing the name refusals.
pub fn new_output_dir(repo: &Path, out: &Path) -> Result<PathBuf> {
    let name = out.file_name().and_then(|n| n.to_str()).context("output directory name")?;
    let parent = out.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let parent = parent.canonicalize().context("output parent must exist")?;
    let data = data_dir(repo)?;
    let rel = rel_under(&data, &parent.join(name)).context("refused: pools are written only under data/")?;
    check_rel(&rel)?;
    let dir = parent.join(name);
    ensure!(
        dir.components().all(|c| !matches!(c, Component::ParentDir)),
        "unusual output path"
    );
    fs::create_dir(&dir).with_context(|| format!("create {rel} (existing pools are never replaced)"))?;
    Ok(dir)
}
