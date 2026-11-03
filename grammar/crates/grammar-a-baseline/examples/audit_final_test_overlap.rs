//! Freeze a cumulative, content-free overlap ledger before Test acquisition.
//! audit_final_test_overlap PLAN.json NEW_OUTPUT_DIRECTORY
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, io::Write, path::Path};
#[allow(dead_code)]
#[path = "final_test_binding/mod.rs"]
mod binding;
#[allow(dead_code)]
#[path = "assemble_book_augmentation/recipe.rs"]
mod recipe;

const SCOPES: [&str; 5] = [
    "original_cohort",
    "all_train_extensions",
    "all_references",
    "prior_admissions_and_exclusions",
    "historical_v6_reservations",
];
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn norm(t: &str) -> String {
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn compact(t: &str) -> String {
    t.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}
fn titles(t: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for raw in [t, t.split([':', ';', '(']).next().unwrap_or(t)] {
        let lower = raw.to_lowercase();
        let stripped = ["the ", "an ", "a "]
            .into_iter()
            .find_map(|p| lower.strip_prefix(p))
            .unwrap_or(&lower);
        for v in [compact(raw), compact(stripped)] {
            if !v.is_empty() {
                out.insert(format!("title:{v}"));
            }
        }
    }
    out
}
#[derive(Default)]
struct Protected {
    ids: BTreeSet<String>,
    groups: BTreeSet<String>,
    aliases: BTreeSet<String>,
    exact: BTreeSet<String>,
    normalized: BTreeSet<String>,
    prompts: BTreeSet<String>,
}
fn valid_digest(t: &str) -> bool {
    t.len() == 64
        && t.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
impl Protected {
    fn harvest(&mut self, v: &Value, metadata_only: bool) -> Result<()> {
        match v {
            Value::Array(a) => {
                for x in a {
                    self.harvest(x, metadata_only)?;
                }
            }
            Value::Object(o) => {
                if let Some(t) = o.get("text").and_then(Value::as_str) {
                    ensure!(!metadata_only, "metadata-only input contains prose");
                    ensure!(
                        v["frozen_split"] != "test" && v["split"] != "test",
                        "Test prose prohibited"
                    );
                    self.exact.insert(sha(t.as_bytes()));
                    self.normalized.insert(sha(norm(t).as_bytes()));
                }
                // Includes attempted roots that later failed or were excluded.
                if v["origin"] == "human_only" && v["purpose"] == "query" {
                    if let Some(id) = v["id"].as_str() {
                        self.prompts
                            .insert(sha(recipe::scratch_prompt(id).as_bytes()));
                    }
                }
                for (k, x) in o {
                    if let Some(t) = x.as_str().filter(|t| !t.is_empty()) {
                        match k.as_str() {
                            "id" | "original_id" => {
                                self.ids.insert(t.into());
                            }
                            "source_group" | "underlying_work_key" | "group" => {
                                self.groups.insert(t.into());
                                self.aliases.insert(format!("work:{t}"));
                            }
                            "title"
                            | "source_title"
                            | "expected_title"
                            | "catalog_title"
                            | "item_title"
                            | "canonical_work_title" => self.aliases.extend(titles(t)),
                            "text_sha256" | "original_text_sha256" => {
                                ensure!(valid_digest(t), "invalid text digest");
                                self.exact.insert(t.into());
                            }
                            "normalized_text_sha256" => {
                                ensure!(valid_digest(t), "invalid normalized digest");
                                self.normalized.insert(t.into());
                            }
                            "scratch_prompt_sha256" => {
                                ensure!(valid_digest(t), "invalid prompt digest");
                                self.prompts.insert(t.into());
                            }
                            _ => {}
                        }
                    }
                    if k == "pg_id" {
                        let id = x
                            .as_u64()
                            .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
                            .context("PG ID")?;
                        self.aliases.insert(format!("pg:{id}"));
                    }
                    if ["work_aliases", "title_aliases", "aliases"].contains(&k.as_str()) {
                        if let Some(a) = x.as_array() {
                            for t in a.iter().filter_map(Value::as_str) {
                                self.aliases.extend(titles(t));
                            }
                        }
                    }
                    if k != "text" && (x.is_array() || x.is_object()) {
                        self.harvest(x, metadata_only)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
fn pin(spec: &Value) -> Result<(Vec<u8>, Value)> {
    let p = Path::new(s(spec, "path")?);
    ensure!(p.is_absolute(), "absolute input path required");
    let b = fs::read(p)?;
    ensure!(
        sha(&b) == s(spec, "sha256")?,
        "input checksum differs: {}",
        p.display()
    );
    Ok((
        b,
        json!({"path":p,"sha256":spec["sha256"],"scope":spec["scope"],"pointer":spec["pointer"],"format":spec["format"],"metadata_only":spec["metadata_only"]}),
    ))
}
fn put(p: &Path, b: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(p)?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(())
}
fn reserved(tsv: &str) -> Result<Vec<Value>> {
    let mut lines = tsv.lines();
    let header = lines
        .next()
        .context("TSV header")?
        .split('\t')
        .collect::<Vec<_>>();
    let mut result = Vec::new();
    for line in lines {
        let fields = line.split('\t').collect::<Vec<_>>();
        ensure!(fields.len() == header.len(), "TSV width");
        let row: Value = header
            .iter()
            .zip(fields)
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect();
        if row["split"] == "test" || row["split"] == "test_reference" {
            result.push(row);
        }
    }
    ensure!(
        result.len() == 18,
        "reserved Test roster must have 18 works"
    );
    Ok(result)
}
fn collisions(p: &Protected, rows: &[Value]) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for r in rows {
        let family = s(r, "underlying_work_key")?;
        let pg = s(r, "pg_id")?;
        let mut aliases = titles(s(r, "expected_title")?);
        aliases.insert(format!("work:{family}"));
        aliases.insert(format!("pg:{pg}"));
        for a in aliases.intersection(&p.aliases) {
            out.push(json!({"pg_id":pg,"kind":"work_alias","key":a}));
        }
        for fraction in [20, 60] {
            let id = format!("author-pg{pg}-p{fraction}");
            if p.ids.contains(&id) {
                out.push(json!({"pg_id":pg,"kind":"id","key":id}));
            }
            if r["role"] == "query" {
                let hash = sha(recipe::scratch_prompt(&id).as_bytes());
                if p.prompts.contains(&hash) {
                    out.push(json!({"pg_id":pg,"kind":"scratch_prompt","sha256":hash}));
                }
            }
        }
    }
    Ok(out)
}
fn run(plan_path: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "output already exists");
    let plan_bytes = fs::read(plan_path)?;
    let plan: Value = serde_json::from_slice(&plan_bytes)?;
    ensure!(
        plan["schema"] == "slop_ninja_final_test_overlap_plan_v1",
        "plan schema"
    );
    let (selection_bytes, _) = pin(&plan["selection"])?;
    let selection = binding::validate_selection_receipt(Path::new(s(&plan["selection"], "path")?))?;
    let source_plan = &selection["source_plan"];
    let (tsv, _) = pin(source_plan)?;
    let rows = reserved(std::str::from_utf8(&tsv)?)?;
    let mut protected = Protected::default();
    let mut files = Vec::new();
    let mut counts = Vec::new();
    let mut scopes = BTreeSet::new();
    for input in plan["inputs"].as_array().context("inputs")? {
        let scope = s(input, "scope")?;
        ensure!(SCOPES.contains(&scope), "unrecognized input scope");
        scopes.insert(scope.to_owned());
        let (b, p) = pin(input)?;
        files.push(p);
        let metadata = input["metadata_only"]
            .as_bool()
            .context("metadata_only flag")?;
        let mut n = 0;
        match s(input, "format")? {
            "jsonl" => {
                ensure!(input.get("pointer").is_none(), "JSONL cannot use pointer");
                for line in std::str::from_utf8(&b)?.lines().filter(|l| !l.is_empty()) {
                    protected.harvest(&serde_json::from_str::<Value>(line)?, metadata)?;
                    n += 1;
                }
            }
            "json" => {
                let value: Value = serde_json::from_slice(&b)?;
                let value = match input.get("pointer") {
                    Some(x) => value
                        .pointer(x.as_str().context("pointer string")?)
                        .context("missing pointer")?,
                    None => &value,
                };
                protected.harvest(value, metadata)?;
                n = value.as_array().map_or(1, Vec::len);
            }
            _ => anyhow::bail!("unsupported input format"),
        }
        counts.push(json!({"path":input["path"],"pointer":input["pointer"],"rows":n}));
    }
    ensure!(
        scopes == SCOPES.into_iter().map(str::to_owned).collect(),
        "missing coverage scope"
    );
    // Candidate populations are mandatory, not dependent on the caller's list.
    for candidate in selection["candidates"].as_array().context("candidates")? {
        let directory = Path::new(s(candidate, "numeric_path")?);
        let population = directory.join("population.json");
        let b = fs::read(&population)?;
        let manifest: Value = serde_json::from_slice(&fs::read(directory.join("manifest.json"))?)?;
        let expected = s(&manifest, "population_file_sha256")?;
        ensure!(sha(&b) == expected, "candidate population hash differs");
        let value: Value = serde_json::from_slice(&b)?;
        protected.harvest(&value["rows"], false)?;
        protected.harvest(&value["excluded"], false)?;
        files.push(json!({"path":population,"sha256":sha(&b),"scope":"original_cohort","format":"json","pointer":"/rows","metadata_only":false}));
    }
    files.sort_by_key(|v| format!("{}:{}", v["path"], v["pointer"]));
    files.dedup();
    let conflicts = collisions(&protected, &rows)?;
    let report = json!({"schema":"slop_ninja_final_test_overlap_audit_v1","complete":conflicts.is_empty(),
        "test_text_read":false,"reserved_works":rows.len(),"conflicts":conflicts,"inputs":counts,
        "counts":{"ids":protected.ids.len(),"source_groups":protected.groups.len(),"work_aliases":protected.aliases.len(),
            "text_sha256":protected.exact.len(),"normalized_text_sha256":protected.normalized.len(),"scratch_prompts":protected.prompts.len()},
        "limitations":plan["limitations"],"plan_sha256":sha(&plan_bytes),"implementation_sha256":sha(include_bytes!("audit_final_test_overlap.rs"))});
    fs::create_dir(out)?;
    put(&out.join("plan.json"), &plan_bytes)?;
    put(
        &out.join("REPORT.json"),
        &serde_json::to_vec_pretty(&report)?,
    )?;
    ensure!(
        conflicts.is_empty(),
        "reserved Test conflicts; evidence retained, do not acquire or drop Test copies"
    );
    let ledger = json!({"schema":"slop_ninja_final_test_overlap_ledger_v1","complete":true,"test_text_read":false,
        "selection_receipt_sha256":sha(&selection_bytes),"coverage":SCOPES,"protected_files":files,
        "protected_ids":protected.ids,"protected_source_groups":protected.groups,"protected_work_aliases":protected.aliases,
        "protected_text_sha256":protected.exact,"protected_normalized_text_sha256":protected.normalized,
        "protected_scratch_prompt_sha256":protected.prompts,"audit_report_sha256":sha(&serde_json::to_vec_pretty(&report)?),
        "limitations":plan["limitations"],"reserved_source_plan_sha256":sha(&tsv)});
    put(
        &out.join("ledger.json"),
        &serde_json::to_vec_pretty(&ledger)?,
    )?;
    println!(
        "{}",
        json!({"complete":true,"reserved_works":18,"counts":report["counts"]})
    );
    Ok(())
}
fn check_excerpts(selection: &Path, ledger: &Path, queries: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "output already exists");
    binding::validate_selection_receipt(selection)?;
    let lb = fs::read(ledger)?;
    let l: Value = serde_json::from_slice(&lb)?;
    ensure!(
        l["schema"] == "slop_ninja_final_test_overlap_ledger_v1"
            && l["complete"] == true
            && l["selection_receipt_sha256"] == binding::sha(selection)?,
        "overlap ledger binding"
    );
    for file in l["protected_files"].as_array().context("protected files")? {
        ensure!(
            binding::sha(Path::new(s(file, "path")?))? == s(file, "sha256")?,
            "protected input changed"
        );
    }
    let mb = fs::read(queries.join("manifest.json"))?;
    let m: Value = serde_json::from_slice(&mb)?;
    ensure!(
        m["test_text_opened"] == true && m["test_mode"] == true,
        "Test extraction required"
    );
    let mut conflicts = Vec::new();
    let mut hashes = std::collections::BTreeMap::<String, String>::new();
    let mut counts = std::collections::BTreeMap::new();
    for (file, key, ids_key) in [
        ("humans.jsonl", "records_sha256", "emitted_human_ids"),
        (
            "references.jsonl",
            "references_sha256",
            "emitted_reference_ids",
        ),
    ] {
        let b = fs::read(queries.join(file))?;
        ensure!(m[key] == sha(&b), "excerpts hash changed");
        let expected = m[ids_key]
            .as_array()
            .context("emitted IDs")?
            .iter()
            .map(|v| v.as_str().context("ID").map(str::to_owned))
            .collect::<Result<BTreeSet<_>>>()?;
        let mut actual = BTreeSet::new();
        for line in std::str::from_utf8(&b)?.lines() {
            let row: Value = serde_json::from_str(line)?;
            let id = s(&row, "id")?;
            let text = s(&row, "text")?;
            ensure!(
                row["frozen_split"] == "test" && actual.insert(id.to_owned()),
                "duplicate or non-Test excerpt"
            );
            let exact = sha(text.as_bytes());
            let normalized = sha(norm(text).as_bytes());
            ensure!(row["text_sha256"] == exact, "excerpt text hash changed");
            for (key, value) in [
                ("protected_ids", id),
                ("protected_source_groups", s(&row, "source_group")?),
                ("protected_text_sha256", exact.as_str()),
                ("protected_normalized_text_sha256", normalized.as_str()),
            ] {
                if l[key]
                    .as_array()
                    .context("protected array")?
                    .iter()
                    .any(|v| v.as_str() == Some(value))
                {
                    conflicts.push(json!({"id":id,"kind":key}));
                }
            }
            if let Some(other) = hashes.insert(normalized, id.to_owned()) {
                conflicts.push(json!({"id":id,"other_id":other,"kind":"duplicate_excerpts"}));
            }
        }
        ensure!(actual == expected, "extracted ID roster mismatch");
        counts.insert(file, actual.len());
    }
    let report = json!({"schema":"slop_ninja_final_test_excerpt_overlap_v1","complete":conflicts.is_empty(),
        "selection_receipt_sha256":binding::sha(selection)?,"ledger_sha256":sha(&lb),"query_manifest_sha256":sha(&mb),
        "counts":counts,"conflicts":conflicts,"extraction_failures":m["failures"],"test_used_for_selection":false});
    fs::create_dir(out)?;
    put(
        &out.join("REPORT.json"),
        &serde_json::to_vec_pretty(&report)?,
    )?;
    ensure!(
        conflicts.is_empty(),
        "excerpt collisions; retained evidence, do not generate or silently drop Test copies"
    );
    println!("{}", json!({"complete":true,"counts":counts}));
    Ok(())
}
fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|s| s == "--check-excerpts") {
        ensure!(
            args.len() == 5,
            "--check-excerpts SELECTION LEDGER QUERIES_DIRECTORY NEW_OUTPUT_DIRECTORY"
        );
        return check_excerpts(
            Path::new(&args[1]),
            Path::new(&args[2]),
            Path::new(&args[3]),
            Path::new(&args[4]),
        );
    }
    ensure!(
        args.len() == 2,
        "audit_final_test_overlap PLAN.json NEW_OUTPUT_DIRECTORY"
    );
    run(Path::new(&args[0]), Path::new(&args[1]))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_cover_articles_subtitles_and_punctuation() {
        assert!(titles("The Copper Door: A Tale").contains("title:copperdoor"));
        assert!(titles("A Mariner’s Log").contains("title:marinerslog"));
    }
    #[test]
    fn nested_prose_is_rejected_in_metadata_inputs() {
        assert!(
            Protected::default()
                .harvest(&json!({"nested":{"text":"Synthetic line."}}), true)
                .is_err()
        );
    }
    #[test]
    fn test_prose_is_rejected() {
        assert!(
            Protected::default()
                .harvest(
                    &json!({"frozen_split":"test","text":"Synthetic line."}),
                    false
                )
                .is_err()
        );
    }
    #[test]
    fn hashes_and_rejected_families_are_retained() {
        let mut p = Protected::default();
        p.harvest(&json!({"source_group":"work:x","pg_id":123,"text":"One  two\nthree.","title_aliases":["The Copper Door"]}),false).unwrap();
        assert!(p.normalized.contains(&sha(b"One two three.")));
        assert!(p.aliases.contains("pg:123"));
        assert!(p.aliases.contains("title:copperdoor"));
    }
    #[test]
    fn scratch_collisions_are_checked_before_acquisition() {
        let mut p = Protected::default();
        p.prompts
            .insert(sha(recipe::scratch_prompt("author-pg321-p20").as_bytes()));
        let rows = vec![
            json!({"underlying_work_key":"work:new","pg_id":"321","expected_title":"Synthetic Novel","role":"query"}),
        ];
        assert_eq!(collisions(&p, &rows).unwrap()[0]["kind"], "scratch_prompt");
    }
    #[test]
    fn invalid_hash_fails_closed() {
        assert!(
            Protected::default()
                .harvest(&json!({"text_sha256":"invalid"}), true)
                .is_err()
        );
    }
}
