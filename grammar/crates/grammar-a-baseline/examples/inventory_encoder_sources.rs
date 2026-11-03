//! Metadata-only catalog inventory for the bounded encoder adaptation experiment.
//! CATALOG.csv PRIOR_LEDGER.json CONSUMED_SOURCE_MANIFEST.json NEW_OUTPUT
//! No source text, model, HTTP, labels or new Test acquisition is opened.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn compact(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}
fn aliases(s: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for raw in [s, s.split([':', ';', '(', '\n']).next().unwrap_or(s)] {
        let lower = raw.to_lowercase();
        let short = ["the ", "an ", "a "]
            .into_iter()
            .find_map(|p| lower.strip_prefix(p))
            .unwrap_or(&lower);
        for t in [raw, short] {
            if !compact(t).is_empty() {
                out.insert(format!("title:{}", compact(t)));
            }
        }
    }
    out
}

// Handles quoted commas, escaped quotes and multiline PG titles. Refuse malformed
// CSV rather than silently shifting author/language columns.
fn csv(bytes: &[u8]) -> Result<Vec<Vec<String>>> {
    let s = std::str::from_utf8(bytes)?;
    let mut chars = s.chars().peekable();
    let (mut rows, mut row, mut field) = (Vec::new(), Vec::new(), String::new());
    let (mut quoted, mut closed) = (false, false);
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                    closed = true;
                }
            } else {
                field.push(c);
            }
        } else {
            match c {
                '"' => {
                    ensure!(field.is_empty() && !closed, "quote inside unquoted field");
                    quoted = true;
                }
                ',' => {
                    row.push(std::mem::take(&mut field));
                    closed = false;
                }
                '\r' | '\n' => {
                    if c == '\r' && chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    closed = false;
                }
                _ => {
                    ensure!(!closed, "content after closing quote");
                    field.push(c);
                }
            }
        }
    }
    ensure!(!quoted, "unterminated CSV quote");
    if !row.is_empty() || !field.is_empty() || closed {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

fn scalar<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn author_identity(s: &str) -> Option<(String, i32)> {
    if s.contains(';') || s.contains('[') {
        return None;
    }
    let (name, dates) = s.rsplit_once(", ")?;
    let (birth, death) = dates.split_once('-')?;
    let birth = birth.parse::<i32>().ok()?;
    let death = death.parse::<i32>().ok()?;
    (birth > 1500 && death >= birth && death <= 1945).then(|| (name.to_owned(), death))
}
fn prose_stratum(title: &str, subjects: &str, shelves: &str) -> Option<&'static str> {
    let title = title.to_lowercase();
    let subjects = subjects.to_lowercase();
    let shelves = shelves.to_lowercase();
    if [
        "poetry",
        "poems",
        "drama",
        "plays",
        "periodicals",
        "dictionaries",
        "bibliography",
    ]
    .iter()
    .any(|s| subjects.contains(s))
    {
        return None;
    }
    if [
        "complete works",
        "collected works",
        "volume ",
        "vol. ",
        " vol ",
        "letters",
        "correspondence",
        "anthology",
    ]
    .iter()
    .any(|s| title.contains(s))
    {
        return None;
    }
    if subjects.contains("fiction") {
        return Some("fiction");
    }
    if ["travel", "description and travel", "voyages", "exploration"]
        .iter()
        .any(|s| subjects.contains(s))
    {
        return Some("travel");
    }
    if [
        "natural history",
        "science",
        "botany",
        "zoology",
        "evolution",
        "geology",
        "astronomy",
        "nature",
    ]
    .iter()
    .any(|s| subjects.contains(s))
    {
        return Some("science_nature");
    }
    if subjects.contains("history")
        || subjects.contains("biography")
        || subjects.contains("autobiograph")
    {
        return Some("history_biography");
    }
    if subjects.contains("essays")
        || shelves.contains("essays")
        || subjects.contains("philosophy")
        || subjects.contains("criticism")
    {
        return Some("essays_ideas");
    }
    None
}
fn put(p: &Path, b: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(())
}
fn run(catalog: &Path, ledger: &Path, consumed: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output directory required");
    let cb = fs::read(catalog)?;
    let lb = fs::read(ledger)?;
    let sb = fs::read(consumed)?;
    let l: Value = serde_json::from_slice(&lb)?;
    let sources: Value = serde_json::from_slice(&sb)?;
    ensure!(
        l["complete"] == true && sources["complete"] == true,
        "complete retained evidence required"
    );
    let mut blocked: BTreeSet<String> = l["protected_work_aliases"]
        .as_array()
        .context("prior aliases")?
        .iter()
        .map(|v| Ok(v.as_str().context("alias string")?.to_owned()))
        .collect::<Result<_>>()?;
    for s in sources["sources"].as_array().context("consumed sources")? {
        blocked.insert(format!("pg:{}", s["pg_id"].as_u64().context("PG ID")?));
        blocked.extend(aliases(scalar(s, "title")?));
    }
    let rows = csv(&cb)?;
    let header = rows.first().context("catalog header")?;
    let col = |key: &str| {
        header
            .iter()
            .position(|s| s == key)
            .with_context(|| format!("column {key}"))
    };
    let [id, typ, title, language, author, subjects, shelves] = [
        "Text#",
        "Type",
        "Title",
        "Language",
        "Authors",
        "Subjects",
        "Bookshelves",
    ]
    .map(col);
    let (id, typ, title, language, author, subjects, shelves) =
        (id?, typ?, title?, language?, author?, subjects?, shelves?);
    let mut candidates = Vec::new();
    let mut exclusions = Vec::new();
    let mut counts = BTreeMap::<(String, String), usize>::new();
    let mut seen_ids = BTreeSet::new();
    for r in rows.iter().skip(1) {
        ensure!(r.len() == header.len(), "catalog width");
        let pg = r[id].parse::<u64>()?;
        ensure!(seen_ids.insert(pg), "duplicate catalog ID");
        if r[typ] != "Text" || r[language] != "en" {
            continue;
        }
        let Some((identity, death)) = author_identity(&r[author]) else {
            continue;
        };
        let Some(stratum) = prose_stratum(&r[title], &r[subjects], &r[shelves]) else {
            continue;
        };
        let mut keys = aliases(&r[title]);
        keys.insert(format!("pg:{pg}"));
        let overlaps = keys.intersection(&blocked).cloned().collect::<Vec<_>>();
        let item = json!({"pg_id":pg,"catalog_author":identity,"catalog_author_with_dates":r[author],
            "death_year":death,"title":r[title],"subjects":r[subjects],"stratum":stratum,
            "aliases":keys,"catalog_overlap":overlaps,"language":"en",
            "rights_status":"unverified_pending_individual_RDF","human_history":"historical_proxy_only",
            "composition_date":null,"independent_work_review":"pending"});
        if overlaps.is_empty() {
            *counts.entry((identity, stratum.into())).or_default() += 1;
            candidates.push(item);
        } else {
            exclusions.push(item);
        }
    }
    fs::create_dir(out)?;
    for (name, values) in [
        ("candidates.jsonl", &candidates),
        ("excluded_overlap.jsonl", &exclusions),
    ] {
        let mut bytes = Vec::new();
        for v in values {
            serde_json::to_writer(&mut bytes, v)?;
            bytes.push(b'\n');
        }
        put(&out.join(name), &bytes)?;
    }
    let report = json!({"schema":"slop_ninja_encoder_source_inventory_v1","complete":true,
        "metadata_only":true,"source_text_opened":false,"rights_qualified":false,"split_assigned":false,
        "catalog_records":rows.len()-1,"candidate_records":candidates.len(),"overlap_exclusions":exclusions.len(),
        "protected_aliases":blocked.len(),"inputs":{"catalog_sha256":sha(&cb),"prior_ledger_sha256":sha(&lb),"consumed_sources_sha256":sha(&sb)},
        "candidate_sha256":sha(&fs::read(out.join("candidates.jsonl"))?),
        "by_author_stratum":counts.into_iter().map(|((author,stratum),n)|json!({"author":author,"stratum":stratum,"catalog_records":n})).collect::<Vec<_>>(),
        "limits":["Catalog entries are not necessarily independent works; editions, compilations, volume aliases and subjects require review before split assignment.",
            "Metadata categories are acquisition strata, not learned inputs or labels.","No unaided-human production history is established.",
            "Prior ledger preserves its documented historical V6 and grammar-exposure limitations; further exact/normalized text checks are required at admission."]});
    put(
        &out.join("manifest.json"),
        &serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "{}",
        json!({"candidates":candidates.len(),"overlap_excluded":exclusions.len(),"metadata_only":true})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args_os().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 4,
        "CATALOG.csv PRIOR_LEDGER.json CONSUMED_SOURCE_MANIFEST.json NEW_OUTPUT"
    );
    run(
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn csv_multiline_and_quotes_preserve_column_identity() {
        assert_eq!(
            csv(b"id,title,author\r\n1,\"Two, \"\"quoted\"\"\nlines\",Writer\r\n").unwrap(),
            vec![
                vec!["id", "title", "author"],
                vec!["1", "Two, \"quoted\"\nlines", "Writer"]
            ]
        );
        for bad in ["a,\"open", "a,\"closed\"oops", "a,b\"c"] {
            assert!(csv(bad.as_bytes()).is_err());
        }
    }
    #[test]
    fn metadata_does_not_invent_authors_or_treat_issues_as_composition() {
        assert_eq!(
            author_identity("Example, Ada, 1800-1870"),
            Some(("Example, Ada".into(), 1870))
        );
        for a in [
            "Example, Ada",
            "Example, Ada, 1800-1870; Someone",
            "Example, Ada, 1800-1870 [Editor]",
            "Example, Ada, 1950-",
        ] {
            assert!(author_identity(a).is_none());
        }
    }
    #[test]
    fn aliases_block_editions_and_missing_rights_are_not_admitted() {
        assert!(!aliases("The Example: A new edition").is_disjoint(&aliases("Example")));
        assert_eq!(prose_stratum("Collected works", "Essays", ""), None);
        assert_eq!(
            prose_stratum("Journeys", "Description and travel", ""),
            Some("travel")
        );
        assert_eq!(prose_stratum("Invented title", "Unknown subject", ""), None);
    }
}
