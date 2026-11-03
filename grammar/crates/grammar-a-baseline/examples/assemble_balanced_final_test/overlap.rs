//! Required external cumulative metadata audit, followed by exact local checks.
use super::*;
const SCOPES: [&str; 5] = [
    "original_cohort",
    "all_train_extensions",
    "all_references",
    "prior_admissions_and_exclusions",
    "historical_v6_reservations",
];
pub(super) struct Ledger {
    ids: BTreeSet<String>,
    groups: BTreeSet<String>,
    aliases: BTreeSet<String>,
    exact: BTreeSet<String>,
    normalized: BTreeSet<String>,
    prompts: BTreeSet<String>,
}
pub(super) fn aliases(work: &Value) -> Result<BTreeSet<String>> {
    let title = str_at(&work["selection_row"], "expected_title")?;
    let mut out = BTreeSet::from([
        format!("work:{}", str_at(work, "source_group")?),
        format!("pg:{}", counter(work, "pg_id")?),
    ]);
    for raw in [title, title.split([':', ';', '(']).next().unwrap_or(title)] {
        let lower = raw.to_lowercase();
        let stripped = ["the ", "an ", "a "]
            .into_iter()
            .find_map(|p| lower.strip_prefix(p))
            .unwrap_or(&lower);
        for candidate in [lower.as_str(), stripped] {
            let compact = candidate
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>();
            if !compact.is_empty() {
                out.insert(format!("title:{compact}"));
            }
        }
    }
    Ok(out)
}
fn set(v: &Value, key: &str, digests: bool) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for item in array(v, key)? {
        let s = item
            .as_str()
            .filter(|s| !s.is_empty())
            .context("ledger string")?;
        if digests {
            ensure!(
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "ledger digest"
            );
        }
        ensure!(out.insert(s.to_owned()), "duplicate ledger value");
    }
    Ok(out)
}
impl Ledger {
    pub(super) fn load(
        plan: &Value,
        selection: &Value,
        selection_sha: &str,
        inputs: &mut Vec<Value>,
    ) -> Result<Self> {
        let l = pinned_json(&plan["overlap_ledger"], inputs)?;
        ensure!(
            l["schema"] == "slop_ninja_final_test_overlap_ledger_v1"
                && l["complete"] == true
                && l["test_text_read"] == false
                && l["selection_receipt_sha256"] == selection_sha,
            "complete pre-admission metadata audit required"
        );
        ensure!(
            set(&l, "coverage", false)? == SCOPES.into_iter().map(str::to_owned).collect(),
            "cumulative audit scope incomplete"
        );
        let mut files = BTreeMap::new();
        let mut scopes = BTreeSet::new();
        for p in array(&l, "protected_files")? {
            let scope = str_at(p, "scope")?;
            ensure!(SCOPES.contains(&scope), "unknown cumulative scope");
            scopes.insert(scope.to_owned());
            let path = Path::new(str_at(p, "path")?);
            ensure!(path.is_absolute(), "absolute protected file");
            let hash = digest_at(p, "sha256")?;
            ensure!(
                final_test_binding::sha(path)? == hash,
                "changed protected audit input"
            );
            let path = path.canonicalize()?;
            if let Some(prior) = files.insert(path, hash.to_owned()) {
                ensure!(prior == hash, "conflicting protected file pins");
            }
            inputs.push(p.clone());
        }
        ensure!(
            scopes == SCOPES.into_iter().map(str::to_owned).collect(),
            "audit scope lacks evidence files"
        );
        for c in array(selection, "candidates")? {
            let root = Path::new(str_at(c, "numeric_path")?);
            let (m, _) = load(&root.join("manifest.json"), inputs)?;
            ensure!(
                final_test_binding::sha(&root.join("manifest.json"))?
                    == digest_at(c, "numeric_manifest_sha256")?,
                "frozen numeric manifest changed"
            );
            ensure!(
                files
                    .get(&root.join("population.json").canonicalize()?)
                    .map(String::as_str)
                    == Some(digest_at(&m, "population_file_sha256")?),
                "candidate population omitted from cumulative audit"
            );
        }
        Self::from_value(&l)
    }
    fn from_value(l: &Value) -> Result<Self> {
        Ok(Self {
            ids: set(l, "protected_ids", false)?,
            groups: set(l, "protected_source_groups", false)?,
            aliases: set(l, "protected_work_aliases", false)?,
            exact: set(l, "protected_text_sha256", true)?,
            normalized: set(l, "protected_normalized_text_sha256", true)?,
            prompts: set(l, "protected_scratch_prompt_sha256", true)?,
        })
    }
    pub(super) fn check(
        &self,
        works: &BTreeMap<u64, Value>,
        rows: &[Value],
        refs: &[Value],
        prompts: &BTreeMap<(String, u64), Value>,
    ) -> Result<()> {
        for w in works.values() {
            ensure!(
                !self.groups.contains(str_at(w, "source_group")?)
                    && self.aliases.is_disjoint(&aliases(w)?),
                "reserved Test work conflicts with protected work/alias; stop without Test-only removal"
            );
            // Check planned IDs and prompts even when excerpt extraction failed.
            for fraction in [20, 60] {
                let id = format!("author-pg{}-p{fraction}", counter(w, "pg_id")?);
                ensure!(!self.ids.contains(&id), "reserved Test ID conflict");
                if w["purpose"] == "query" {
                    ensure!(
                        !self
                            .prompts
                            .contains(&sha(recipe::scratch_prompt(&id).as_bytes())),
                        "reserved scratch prompt conflicts with protected task"
                    );
                    for model in MODELS {
                        for percent in PERCENTAGES {
                            ensure!(
                                !self.ids.contains(&balanced::task_id(&id, model, percent)),
                                "reserved task ID conflict"
                            );
                        }
                    }
                }
            }
        }
        for r in rows.iter().chain(refs) {
            ensure!(
                !self.ids.contains(str_at(r, "id")?)
                    && !self.groups.contains(str_at(r, "source_group")?)
                    && !self.exact.contains(digest_at(r, "text_sha256")?)
                    && !self.normalized.contains(&normalized(str_at(r, "text")?)),
                "cross-split ID/family/text collision; stop without Test-only removal: {}",
                r["id"]
            );
        }
        for ((_, percent), p) in prompts {
            if *percent == 0 {
                ensure!(
                    !self
                        .prompts
                        .contains(&sha(str_at(&p["messages"][0], "content")?.as_bytes())),
                    "cross-split scratch prompt collision"
                );
            }
        }
        Ok(())
    }
}
pub(super) fn within_test(
    rows: Vec<Value>,
    refs: &[Value],
) -> Result<(Vec<Value>, Vec<Value>, Vec<Value>)> {
    let mut groups = BTreeMap::<String, BTreeSet<String>>::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut roots = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut refhash = BTreeSet::new();
    for r in rows.iter().chain(refs) {
        ensure!(
            ids.insert(str_at(r, "id")?.to_owned()),
            "query/reference ID overlap"
        );
        let h = normalized(str_at(r, "text")?);
        groups
            .entry(h.clone())
            .or_default()
            .insert(str_at(r, "source_group")?.to_owned());
        if r["purpose"] == "reference" {
            ensure!(refhash.insert(h.clone()), "duplicate Test references");
        } else {
            *counts.entry(h.clone()).or_default() += 1;
            if r["origin"] == "human_only" {
                ensure!(roots.insert(h.clone()), "duplicate Test human excerpts");
            }
        }
    }
    ensure!(
        groups.values().all(|g| g.len() == 1),
        "text collision across Test work families; stop without replacement"
    );
    ensure!(
        counts.keys().all(|h| !refhash.contains(h)),
        "query/reference text overlap"
    );
    let (mut kept, mut excluded, mut raw) = (Vec::new(), Vec::new(), Vec::new());
    for r in rows {
        let h = normalized(str_at(&r, "text")?);
        if r["origin"] != "human_only" && (counts[&h] > 1 || roots.contains(&h)) {
            excluded.push(json!({"id":r["id"],"source_group":r["source_group"],"text_sha256":r["text_sha256"],"normalized_text_sha256":h,
                "reason":"within_Test_family_generated_duplicate_or_no_change","generation_evidence_retained":true}));
            raw.push(r);
        } else {
            kept.push(r);
        }
    }
    Ok((kept, excluded, raw))
}

#[cfg(test)]
pub(super) fn synthetic_ledger(v: &Value) -> Result<Ledger> {
    Ledger::from_value(v)
}
