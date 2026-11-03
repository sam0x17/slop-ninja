//! Freeze a reviewed metadata roster and acquire RDF only, never source prose.
//! ROSTER.tsv CATALOG_INVENTORY_DIR OUTPUT
//! The resulting selection.tsv is accepted by a_acquire_balanced_sources, whose
//! default mode excludes Test. Existing final-Test gates remain unchanged.
use anyhow::{Context, Result, ensure};
use reqwest::blocking::Client;
use roxmltree::{Document, Node};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::Path,
    time::Duration,
};
const PG: &str = "http://www.gutenberg.org/2009/pgterms/";
const DC: &str = "http://purl.org/dc/terms/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
type Row = BTreeMap<String, String>;
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn put(p: &Path, b: &[u8]) -> Result<()> {
    if p.exists() {
        ensure!(
            fs::read(p)? == b,
            "immutable artifact changed: {}",
            p.display()
        );
        return Ok(());
    }
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(())
}
fn json_file(p: &Path, v: &Value) -> Result<()> {
    put(p, &serde_json::to_vec_pretty(v)?)
}
fn roster(b: &[u8]) -> Result<Vec<Row>> {
    let mut lines = std::str::from_utf8(b)?.lines();
    let header = lines
        .next()
        .context("header")?
        .split('\t')
        .collect::<Vec<_>>();
    ensure!(
        header
            == [
                "pg_id",
                "author_key",
                "author",
                "split",
                "enrollment",
                "stratum"
            ],
        "roster header"
    );
    let mut ids = BTreeSet::new();
    let mut writers = BTreeMap::<String, BTreeSet<String>>::new();
    let mut out = Vec::new();
    for line in lines {
        let cols = line.split('\t').collect::<Vec<_>>();
        ensure!(cols.len() == header.len(), "roster width");
        let r: Row = header
            .iter()
            .zip(cols)
            .map(|(k, v)| (k.to_string(), v.to_owned()))
            .collect();
        ensure!(
            r["pg_id"].parse::<u64>()? > 0 && ids.insert(r["pg_id"].clone()),
            "duplicate/invalid PGID"
        );
        ensure!(
            !r["author_key"].is_empty()
                && r["author_key"]
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_'),
            "author key"
        );
        let part = r["split"].strip_suffix("_reference").unwrap_or(&r["split"]);
        ensure!(
            ["train", "development", "calibration", "test"].contains(&part),
            "split"
        );
        ensure!(
            ["known", "withheld"].contains(&r["enrollment"].as_str()),
            "enrollment"
        );
        ensure!(
            r["enrollment"] != "withheld" || part != "train",
            "withheld training writer"
        );
        ensure!(
            [
                "fiction",
                "travel",
                "science_nature",
                "history_biography",
                "essays_ideas"
            ]
            .contains(&r["stratum"].as_str()),
            "stratum"
        );
        writers
            .entry(r["author_key"].clone())
            .or_default()
            .insert(format!("{}:{part}", r["enrollment"]));
        out.push(r);
    }
    for states in writers.values() {
        if states.iter().any(|x| x.starts_with("withheld:")) {
            ensure!(
                states.len() == 1,
                "withheld writer crosses partitions/enrollment"
            );
        } else {
            ensure!(
                states.contains("known:train"),
                "known writer without Train work"
            );
        }
    }
    ensure!(!out.is_empty(), "empty roster");
    Ok(out)
}
fn one<'a, 'i>(n: Node<'a, 'i>, ns: &str, key: &str) -> Result<Node<'a, 'i>> {
    let v = n
        .children()
        .filter(|x| x.has_tag_name((ns, key)))
        .collect::<Vec<_>>();
    ensure!(v.len() == 1, "expected one {key}");
    Ok(v[0])
}
fn scalar(n: Node<'_, '_>) -> Result<String> {
    ensure!(
        !n.children().any(|c| c.is_element()),
        "scalar metadata required"
    );
    Ok(n.text()
        .context("empty metadata")?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" "))
}
fn same_title(catalog: &str, rdf: &str) -> bool {
    // PG's metadata catalog sometimes omits the MARC subtitle. Accept only an
    // identical principal title, preserving the complete RDF title in outputs.
    let principal = |s: &str| {
        s.split(':')
            .next()
            .unwrap_or(s)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    principal(catalog) == principal(rdf)
}
fn validate_metadata(b: &[u8], id: u64, c: &Value) -> Result<(String, String)> {
    let d = Document::parse(std::str::from_utf8(b)?)?;
    let e = one(d.root_element(), PG, "ebook")?;
    ensure!(
        e.attribute((RDF, "about")) == Some(format!("ebooks/{id}").as_str()),
        "RDF id"
    );
    let title = scalar(one(e, DC, "title")?)?;
    let catalog_title = c["title"]
        .as_str()
        .context("catalog title")?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    ensure!(
        same_title(&catalog_title, &title),
        "catalog/RDF principal title changed"
    );
    let agent = one(one(e, DC, "creator")?, PG, "agent")?;
    let author = scalar(one(agent, PG, "name")?)?;
    ensure!(c["catalog_author"] == author, "catalog/RDF author changed");
    ensure!(
        scalar(one(e, DC, "rights")?)? == "Public domain in the USA.",
        "rights unavailable"
    );
    for (key, expect) in [("language", "en"), ("type", "Text")] {
        ensure!(
            scalar(one(
                one(one(e, DC, key)?, RDF, "Description")?,
                RDF,
                "value"
            )?)? == expect,
            "RDF {key}"
        );
    }
    ensure!(!e.descendants().any(|n| n.is_element() && matches!(n.tag_name().name(),"trl"|"edt"|"com"|"ctb")),"credited translator/editor/compiler/contributor needs separate review");
    Ok((title, author))
}
fn run(roster_path: &Path, inventory: &Path, out: &Path) -> Result<()> {
    let rb = fs::read(roster_path)?;
    let rows = roster(&rb)?;
    let cb = fs::read(inventory.join("candidates.jsonl"))?;
    let mb = fs::read(inventory.join("manifest.json"))?;
    let m: Value = serde_json::from_slice(&mb)?;
    ensure!(
        m["complete"] == true && m["metadata_only"] == true && m["candidate_sha256"] == sha(&cb),
        "inventory binding"
    );
    let mut catalog = BTreeMap::new();
    for line in std::str::from_utf8(&cb)?.lines() {
        let v: Value = serde_json::from_str(line)?;
        let id = v["pg_id"].as_u64().context("candidate id")?;
        ensure!(catalog.insert(id, v).is_none(), "duplicate catalog row");
    }
    let mut aliases = BTreeSet::new();
    let mut author_keys = BTreeMap::new();
    let mut reverse = BTreeMap::new();
    for r in &rows {
        let id = r["pg_id"].parse::<u64>()?;
        let c = catalog
            .get(&id)
            .context("candidate excluded by prior exposure or unavailable")?;
        ensure!(
            c["catalog_overlap"] == json!([]),
            "prior exposure collision"
        );
        for alias in c["aliases"].as_array().context("aliases")? {
            ensure!(
                aliases.insert(alias.as_str().context("alias string")?.to_owned()),
                "duplicate work alias"
            );
        }
        let name = c["catalog_author"].as_str().context("catalog author")?;
        if let Some(old) = author_keys.insert(r["author_key"].clone(), name.to_owned()) {
            ensure!(old == name, "writer key aliases authors");
        }
        if let Some(old) = reverse.insert(name.to_owned(), r["author_key"].clone()) {
            ensure!(old == r["author_key"], "author has multiple identities");
        }
    }
    fs::create_dir_all(out)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(out.join(".lock"))?;
    lock.try_lock()?;
    let implementation = sha(include_bytes!("a_freeze_encoder_sources.rs"));
    let initial = if out.join("reservation.json").exists() {
        serde_json::from_slice::<Value>(&fs::read(out.join("reservation.json"))?)?["implementation_sha256"].clone()
    } else {
        json!(implementation)
    };
    let binding = json!({"schema":"slop_ninja_encoder_source_reservation_v1","roster_sha256":sha(&rb),"inventory_sha256":sha(&mb),"catalog_sha256":sha(&cb),"metadata_only":true,"test_prose_opened":false,"reserved_works":rows.len(),"rows":rows,"prior_exposure_inputs":m["inputs"],"implementation_sha256":initial});
    json_file(&out.join("reservation.json"), &binding)?;
    put(&out.join("roster.tsv"), &rb)?;
    fs::create_dir_all(out.join("implementations"))?;
    put(
        &out.join(format!("implementations/{implementation}.rs")),
        include_bytes!("a_freeze_encoder_sources.rs"),
    )?;
    fs::create_dir_all(out.join("metadata"))?;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(60))
        .build()?;
    let mut tsv = String::from(
        "pg_id\tauthor_key\tauthor\tcatalog_author\texpected_title\tunderlying_work_key\tsplit\trole\tenrollment\tauthor_partition\tlanguage\trights\trights_evidence_url\tmetadata_path\tmetadata_sha256\tstratum\n",
    );
    for r in &rows {
        let id = r["pg_id"].parse::<u64>()?;
        let c = &catalog[&id];
        let path = out.join(format!("metadata/pg{id}.rdf"));
        let url = format!("https://www.gutenberg.org/ebooks/{id}.rdf");
        // The ebook URL redirects to this stable PG cache path using HTTP.
        // Request the same RDF over HTTPS directly and retain both URLs.
        let request_url = format!("https://www.gutenberg.org/cache/epub/{id}/pg{id}.rdf");
        if !path.exists() {
            let intent = out.join(format!("metadata/pg{id}.request.json"));
            ensure!(
                !intent.exists(),
                "interrupted RDF request needs explicit recovery: PG{id}"
            );
            json_file(
                &intent,
                &json!({"url":request_url,"catalog_url":url,"started_utc":chrono::Utc::now().to_rfc3339(),"reservation_sha256":sha(&fs::read(out.join("reservation.json"))?)}),
            )?;
            let response = client.get(&request_url).send()?;
            let status = response.status().as_u16();
            let headers = format!("{:?}", response.headers());
            let effective = response.url().to_string();
            let mut bytes = Vec::new();
            response.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            put(&path, &bytes)?;
            json_file(
                &out.join(format!("metadata/pg{id}.response.json")),
                &json!({"status":status,"effective_url":effective,"headers":headers,"sha256":sha(&bytes),"bytes":bytes.len(),"finished_utc":chrono::Utc::now().to_rfc3339()}),
            )?;
        }
        let response: Value = serde_json::from_slice(&fs::read(
            out.join(format!("metadata/pg{id}.response.json")),
        )?)?;
        let bytes = fs::read(&path)?;
        ensure!(
            response["status"] == 200
                && response["effective_url"] == request_url
                && response["sha256"] == sha(&bytes)
                && bytes.len() <= 2 * 1024 * 1024,
            "RDF HTTP response failed"
        );
        let (title, author) = validate_metadata(&bytes, id, c)?;
        let part = r["split"].strip_suffix("_reference").unwrap_or(&r["split"]);
        let fields = vec![
            id.to_string(),
            r["author_key"].clone(),
            r["author"].clone(),
            author,
            title,
            format!("encoder_stage3_work_v1:{}:pg{id}", r["author_key"]),
            r["split"].clone(),
            if r["split"].ends_with("_reference") {
                "reference"
            } else {
                "query"
            }
            .into(),
            r["enrollment"].clone(),
            if r["enrollment"] == "known" {
                "all"
            } else {
                part
            }
            .into(),
            "en".into(),
            "Public domain in the USA.".into(),
            url,
            format!("metadata/pg{id}.rdf"),
            sha(&bytes),
            r["stratum"].clone(),
        ];
        ensure!(
            fields.iter().all(|x| !x.contains(['\n', '\r', '\t'])),
            "TSV cell"
        );
        tsv.push_str(&fields.join("\t"));
        tsv.push('\n');
        println!(
            "{}",
            json!({"pg_id":id,"metadata_verified":true,"split":r["split"]})
        );
    }
    put(&out.join("selection.tsv"), tsv.as_bytes())?;
    json_file(
        &out.join("manifest.json"),
        &json!({"schema":"slop_ninja_encoder_source_metadata_v1","complete":true,"reservation_sha256":sha(&fs::read(out.join("reservation.json"))?),"selection_sha256":sha(tsv.as_bytes()),"works":rows.len(),"source_prose_opened":false,"test_reserved":rows.iter().filter(|r|r["split"].starts_with("test")).count(),"limitations":["Rights basis is US public domain; original composition dates remain unknown.","Historical author proxies, not authenticated human-only production logs.","Metadata-reviewed distinct titles; no full-content independence claim. Anthology/edition exclusion and mechanical text overlap gates are required at admission.","A fresh experimental Test is not guaranteed absent from foundation-model pretraining."]}),
    )?;
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args_os().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 3, "ROSTER.tsv INVENTORY OUTPUT");
    run(Path::new(&a[0]), Path::new(&a[1]), Path::new(&a[2]))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn withheld_writers_cannot_leak_between_partitions() {
        let h = "pg_id\tauthor_key\tauthor\tsplit\tenrollment\tstratum\n";
        assert!(roster(format!("{h}1\twriter\tWriter\ttest\twithheld\tfiction\n2\twriter\tWriter\ttrain\tknown\tfiction\n").as_bytes()).is_err());
        assert!(roster(format!("{h}1\twriter\tWriter\tcalibration\twithheld\tfiction\n2\twriter\tWriter\ttest\twithheld\tfiction\n").as_bytes()).is_err());
        assert!(roster(format!("{h}1\twriter\tWriter\ttest\twithheld\tfiction\n2\twriter\tWriter\ttest_reference\twithheld\tfiction\n").as_bytes()).is_ok());
    }
    #[test]
    fn duplicate_ids_and_untrained_known_rosters_fail() {
        let h = "pg_id\tauthor_key\tauthor\tsplit\tenrollment\tstratum\n";
        assert!(roster(format!("{h}1\twriter\tWriter\ttrain\tknown\tfiction\n1\twriter\tWriter\ttest\tknown\tfiction\n").as_bytes()).is_err());
        assert!(
            roster(format!("{h}1\twriter\tWriter\tdevelopment\tknown\tfiction\n").as_bytes())
                .is_err()
        );
    }
    #[test]
    fn catalog_subtitle_omission_does_not_allow_a_different_work() {
        assert!(same_title(
            "A Synthetic Example",
            "A synthetic example : $b A study"
        ));
        assert!(!same_title(
            "A Synthetic Example",
            "Another synthetic example : $b A study"
        ));
    }
}
