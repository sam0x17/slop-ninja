//! Immutable, resumable author acquisition.
//! Usage: a_acquire_balanced_sources TSV METADATA_ROOT OUTPUT [--final-test SELECTION]
//! METADATA_ROOT contains the TSV-relative source-evidence/*.rdf paths.
//! Default excludes Test. The separate final-Test route requires frozen selection.
#[allow(dead_code)]
#[path = "../grammar/crates/grammar-a-baseline/examples/final_test_binding/mod.rs"]
mod final_test_binding;
#[allow(dead_code)]
#[path = "../grammar/crates/grammar-a-baseline/examples/freeze_last_block_test.rs"]
mod last_block_test_binding;
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Url, blocking::Client};
use roxmltree::{Document, Node};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};

const PG: &str = "http://www.gutenberg.org/2009/pgterms/";
const DC: &str = "http://purl.org/dc/terms/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RIGHTS: &str = "Public domain in the USA.";
const LIMIT: u64 = 16 * 1024 * 1024;
type Row = BTreeMap<String, String>;
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
fn read_json(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn relative(p: &str) -> Result<&Path> {
    let p = Path::new(p);
    ensure!(
        !p.as_os_str().is_empty() && p.components().all(|c| matches!(c, Component::Normal(_))),
        "unsafe relative path"
    );
    Ok(p)
}
fn read_bound(p: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    File::open(p)?.take(limit + 1).read_to_end(&mut b)?;
    ensure!(
        b.len() as u64 <= limit,
        "file exceeds limit: {}",
        p.display()
    );
    Ok(b)
}
// Publish complete bytes atomically without replacing any existing artifact.
fn put(p: &Path, b: &[u8]) -> Result<()> {
    if p.try_exists()? {
        ensure!(
            !fs::symlink_metadata(p)?.file_type().is_symlink() && fs::read(p)? == b,
            "refuse differing existing artifact: {}",
            p.display()
        );
        return Ok(());
    }
    let parent = p.parent().context("parent")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(b)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(p).map_err(|e| e.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
fn put_json(p: &Path, v: &Value) -> Result<()> {
    let mut b = serde_json::to_vec_pretty(v)?;
    b.push(b'\n');
    put(p, &b)
}
fn parse_plan(b: &[u8]) -> Result<Vec<Row>> {
    let mut lines = std::str::from_utf8(b)?.lines();
    let headers = lines
        .next()
        .context("TSV header")?
        .split('\t')
        .collect::<Vec<_>>();
    ensure!(
        headers.iter().collect::<BTreeSet<_>>().len() == headers.len(),
        "duplicate header"
    );
    for key in [
        "pg_id",
        "author_key",
        "author",
        "catalog_author",
        "expected_title",
        "underlying_work_key",
        "split",
        "role",
        "enrollment",
        "author_partition",
        "language",
        "rights",
        "rights_evidence_url",
        "metadata_path",
        "metadata_sha256",
    ] {
        ensure!(headers.contains(&key), "missing column {key}");
    }
    let mut rows = Vec::new();
    let mut ids = BTreeSet::new();
    let mut works = BTreeSet::new();
    for line in lines {
        let fields = line.split('\t').collect::<Vec<_>>();
        ensure!(fields.len() == headers.len(), "malformed TSV row");
        let r: Row = headers
            .iter()
            .zip(fields)
            .map(|(k, v)| (k.to_string(), v.to_owned()))
            .collect();
        let id = r["pg_id"].parse::<u32>()?;
        ensure!(
            id > 0 && ids.insert(id) && works.insert(r["underlying_work_key"].clone()),
            "duplicate/invalid work"
        );
        ensure!(
            !r["author_key"].is_empty()
                && r["author_key"]
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c == b'_'),
            "author key"
        );
        ensure!(
            matches!(r["enrollment"].as_str(), "known" | "withheld"),
            "enrollment"
        );
        let base = r["split"].strip_suffix("_reference").unwrap_or(&r["split"]);
        ensure!(
            matches!(base, "train" | "development" | "calibration" | "test"),
            "partition"
        );
        ensure!(
            r["role"]
                == if r["split"].ends_with("_reference") {
                    "reference"
                } else {
                    "query"
                },
            "role/split mismatch"
        );
        ensure!(
            if r["enrollment"] == "known" {
                r["author_partition"] == "all"
            } else {
                r["author_partition"] == base && base != "train"
            },
            "author partition"
        );
        ensure!(
            r["language"] == "en" && r["rights"] == RIGHTS,
            "TSV language/rights"
        );
        ensure!(
            r["rights_evidence_url"] == format!("https://www.gutenberg.org/ebooks/{id}.rdf"),
            "metadata URL"
        );
        ensure!(
            r["metadata_sha256"].len() == 64
                && r["metadata_sha256"].bytes().all(|c| c.is_ascii_hexdigit()),
            "metadata digest"
        );
        relative(&r["metadata_path"])?;
        rows.push(r);
    }
    ensure!(!rows.is_empty(), "empty plan");
    Ok(rows)
}
fn eligible(r: &Row) -> bool {
    r["split"] != "test" && r["split"] != "test_reference" && r["author_partition"] != "test"
}
#[derive(Default)]
struct AcquisitionMode {
    receipt: Option<(PathBuf, String)>,
    allowed: BTreeSet<String>,
}
impl AcquisitionMode {
    fn from_selection(path: &Path, tsv_bytes: &[u8], rows: &[Row]) -> Result<Self> {
        let encoder_test = read_json(path)?["schema"] == last_block_test_binding::SCHEMA;
        let selection = if encoder_test {
            last_block_test_binding::validate(path)?
        } else {
            final_test_binding::validate_selection_receipt(path)?
        };
        ensure!(
            selection["source_plan"]["sha256"] == sha(tsv_bytes),
            "Test acquisition source plan changed"
        );
        let selected = rows
            .iter()
            .filter(|r| matches!(r["split"].as_str(), "test" | "test_reference"))
            .collect::<Vec<_>>();
        let query_works = if encoder_test { 20 } else { 16 };
        ensure!(
            selected.len() == query_works + 2
                && selected.iter().filter(|r| r["split"] == "test").count() == query_works
                && selected
                    .iter()
                    .filter(|r| r["split"] == "test_reference")
                    .count()
                    == 2,
            "exact reserved query/two reference works required"
        );
        Ok(Self {
            receipt: Some((path.canonicalize()?, sha(&fs::read(path)?))),
            allowed: selected.iter().map(|r| r["pg_id"].clone()).collect(),
        })
    }
    fn is_test(&self) -> bool {
        self.receipt.is_some()
    }
    fn permits(&self, r: &Row) -> bool {
        if self.is_test() {
            matches!(r["split"].as_str(), "test" | "test_reference")
                && self.allowed.contains(&r["pg_id"])
        } else {
            eligible(r)
        }
    }
    fn check_boundary(&self, r: &Row) -> Result<()> {
        ensure!(self.permits(r), "source outside acquisition scope");
        if let Some((path, hash)) = &self.receipt {
            ensure!(
                sha(&fs::read(path)?) == *hash,
                "frozen selection receipt changed"
            );
        }
        Ok(())
    }
}
fn one<'a, 'i>(n: Node<'a, 'i>, ns: &str, name: &str) -> Result<Node<'a, 'i>> {
    let v = n
        .children()
        .filter(|n| n.has_tag_name((ns, name)))
        .collect::<Vec<_>>();
    ensure!(v.len() == 1, "expected one {name}");
    Ok(v[0])
}
fn scalar(n: Node<'_, '_>) -> Result<String> {
    ensure!(!n.children().any(|c| c.is_element()), "non-scalar metadata");
    // Same XML whitespace folding used by the pinned metadata scout; punctuation is exact.
    Ok(n.text()
        .context("empty scalar")?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" "))
}
fn metadata(r: &Row, b: &[u8]) -> Result<Value> {
    ensure!(
        sha(b) == r["metadata_sha256"],
        "RDF hash changed: PG{}",
        r["pg_id"]
    );
    let d = Document::parse(std::str::from_utf8(b)?)?;
    let e = one(d.root_element(), PG, "ebook")?;
    ensure!(
        e.attribute((RDF, "about")) == Some(format!("ebooks/{}", r["pg_id"]).as_str()),
        "RDF ID"
    );
    let title = scalar(one(e, DC, "title")?)?;
    ensure!(title == r["expected_title"], "catalog title mismatch");
    let creator = one(one(e, DC, "creator")?, PG, "agent")?;
    ensure!(
        scalar(one(creator, PG, "name")?)? == r["catalog_author"],
        "catalog author mismatch"
    );
    ensure!(scalar(one(e, DC, "rights")?)? == RIGHTS, "catalog rights");
    for (key, expected) in [("language", "en"), ("type", "Text")] {
        let v = one(one(one(e, DC, key)?, RDF, "Description")?, RDF, "value")?;
        ensure!(scalar(v)? == expected, "catalog {key}");
    }
    ensure!(
        !e.descendants()
            .any(|n| n.is_element() && n.tag_name().name() == "trl"),
        "credited translator"
    );
    let url = format!("https://www.gutenberg.org/ebooks/{}.txt.utf-8", r["pg_id"]);
    let files = e
        .descendants()
        .filter(|n| {
            n.has_tag_name((PG, "file")) && n.attribute((RDF, "about")) == Some(url.as_str())
        })
        .collect::<Vec<_>>();
    ensure!(files.len() == 1, "UTF-8 file missing/ambiguous");
    let f = files[0];
    let format = one(
        one(one(f, DC, "format")?, RDF, "Description")?,
        RDF,
        "value",
    )?;
    ensure!(
        scalar(format)? == "text/plain; charset=utf-8",
        "catalog format"
    );
    let extent = scalar(one(f, DC, "extent")?)?.parse::<u64>()?;
    ensure!(extent > 0 && extent <= LIMIT, "catalog size bound");
    Ok(
        json!({"url":url,"extent":extent,"pg_issued":scalar(one(e,DC,"issued")?)?,"pg_file_modified":scalar(one(f,DC,"modified")?)?}),
    )
}
fn checked_url(s: &str) -> Result<Url> {
    let u = Url::parse(s)?;
    ensure!(
        u.scheme() == "https"
            && u.host_str() == Some("www.gutenberg.org")
            && u.port_or_known_default() == Some(443)
            && u.username().is_empty()
            && u.password().is_none()
            && u.fragment().is_none(),
        "unexpected HTTP URL: {s}"
    );
    Ok(u)
}
fn redirect_url(base: &Url, location: &str) -> Result<Url> {
    let mut u = base.join(location)?;
    // PG's ebook endpoint advertises an HTTP Location for its own file server.
    // Retain that original response but issue the next request over HTTPS only.
    if u.scheme() == "http"
        && u.host_str() == Some("www.gutenberg.org")
        && u.port_or_known_default() == Some(80)
    {
        u.set_scheme("https")
            .map_err(|_| anyhow::anyhow!("redirect scheme"))?;
        u.set_port(None)
            .map_err(|_| anyhow::anyhow!("redirect port"))?;
    }
    checked_url(u.as_str())
}
fn verify_download(root: &Path, v: &Value, binding: &str, url: &str) -> Result<()> {
    ensure!(
        v["binding_sha256"] == binding && v["requested_url"] == url,
        "download binding differs"
    );
    checked_url(v["effective_url"].as_str().context("effective URL")?)?;
    for rec in v["receipts"].as_array().context("receipt index")? {
        let path = root.join(relative(rec["path"].as_str().context("receipt path")?)?);
        ensure!(
            sha(&read_bound(&path, 2 * 1024 * 1024)?) == rec["sha256"],
            "receipt changed"
        );
    }
    let path = root.join(relative(v["path"].as_str().context("raw path")?)?);
    let b = read_bound(&path, LIMIT)?;
    ensure!(
        !b.is_empty() && sha(&b) == v["sha256"] && Some(b.len() as u64) == v["bytes"].as_u64(),
        "raw bytes changed"
    );
    ensure!(
        v["status"] == 200
            && v["content_type"].as_str().is_some_and(|s| s
                .split(';')
                .next()
                .is_some_and(|x| x.trim().eq_ignore_ascii_case("text/plain"))),
        "cached response invalid"
    );
    Ok(())
}
fn receipt(root: &Path, path: &Path) -> Result<Value> {
    Ok(json!({"path":path.strip_prefix(root)?,"sha256":sha(&fs::read(path)?)}))
}
fn download(
    client: &Client,
    root: &Path,
    r: &Row,
    m: &Value,
    binding: &str,
    mode: &AcquisitionMode,
) -> Result<Value> {
    // This guard is repeated at the actual network boundary.
    mode.check_boundary(r)?;
    let id = &r["pg_id"];
    let cache = root.join(format!("downloads/pg{id}.json"));
    let requested = m["url"].as_str().context("source URL")?;
    if cache.exists() {
        let v = read_json(&cache)?;
        verify_download(root, &v, binding, requested)?;
        return Ok(v);
    }
    let base = root.join(format!("http/pg{id}"));
    fs::create_dir_all(&base)?;
    let attempt = (1..=1000)
        .find(|n| !base.join(format!("attempt-{n:04}")).exists())
        .context("attempt limit")?;
    let dir = base.join(format!("attempt-{attempt:04}"));
    fs::create_dir(&dir)?;
    File::open(&base)?.sync_all()?;
    let mut url = checked_url(requested)?;
    let mut receipts = Vec::new();
    for hop in 0..6 {
        mode.check_boundary(r)?;
        let req_path = dir.join(format!("{hop}.request.json"));
        let request = json!({"schema":"slop_ninja_source_http_request_v1","binding_sha256":binding,"implementation_sha256":sha(include_bytes!("a_acquire_balanced_sources.rs")),"pg_id":id,"method":"GET","url":url.as_str(),"started_utc":now(),"headers":{"accept":"text/plain","accept-encoding":"identity","user-agent":"Slop-Ninja-source-acquirer/1"},"automatic_redirects":false,"automatic_retries":false});
        put_json(&req_path, &request)?;
        receipts.push(receipt(root, &req_path)?);
        std::thread::sleep(Duration::from_millis(500));
        let response = client
            .get(url.clone())
            .header("Accept", "text/plain")
            .header("Accept-Encoding", "identity")
            .header("User-Agent", "Slop-Ninja-source-acquirer/1")
            .send();
        let mut response = match response {
            Ok(r) => r,
            Err(e) => {
                put_json(
                    &dir.join(format!("{hop}.error.json")),
                    &json!({"finished_utc":now(),"request_sha256":sha(&fs::read(&req_path)?),"error":e.to_string()}),
                )?;
                return Err(e.into());
            }
        };
        let status = response.status();
        let effective = response.url().to_string();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        };
        let content_type = header("content-type");
        let http_date = header("date");
        let modified = header("last-modified");
        let encoding = header("content-encoding");
        let location = header("location");
        let length = response.content_length();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| json!({"name":k.as_str(),"value_hex":hex::encode(v.as_bytes())}))
            .collect::<Vec<_>>();
        let resp_path = dir.join(format!("{hop}.response.json"));
        put_json(
            &resp_path,
            &json!({"received_utc":now(),"request_sha256":sha(&fs::read(&req_path)?),"status":status.as_u16(),"effective_url":effective,"headers":headers,"content_type":content_type,"http_date":http_date,"http_last_modified":modified}),
        )?;
        receipts.push(receipt(root, &resp_path)?);
        let body_path = dir.join(format!("{hop}.body"));
        let mut body = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&body_path)?;
        let copied = std::io::copy(&mut response.by_ref().take(LIMIT + 1), &mut body);
        body.sync_all()?;
        File::open(&dir)?.sync_all()?;
        let bytes = read_bound(&body_path, LIMIT + 1)?;
        let complete = copied.is_ok()
            && bytes.len() as u64 <= LIMIT
            && length.is_none_or(|n| n == bytes.len() as u64);
        let outcome_path = dir.join(format!("{hop}.outcome.json"));
        put_json(
            &outcome_path,
            &json!({"finished_utc":now(),"body_sha256":sha(&bytes),"bytes":bytes.len(),"body_complete":complete,"body_limit":LIMIT,"read_error":copied.as_ref().err().map(ToString::to_string),"request_sha256":sha(&fs::read(&req_path)?),"response_sha256":sha(&fs::read(&resp_path)?)}),
        )?;
        receipts.push(receipt(root, &outcome_path)?);
        ensure!(
            complete,
            "incomplete/oversized HTTP body retained: {}",
            body_path.display()
        );
        if status.is_redirection() {
            ensure!(
                matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308),
                "unsupported redirect"
            );
            url = redirect_url(&url, location.as_deref().context("redirect location")?)?;
            continue;
        }
        ensure!(status.as_u16() == 200, "HTTP {status}; response retained");
        ensure!(
            encoding
                .as_deref()
                .is_none_or(|s| s.eq_ignore_ascii_case("identity")),
            "encoded response refused"
        );
        let v = json!({"binding_sha256":binding,"requested_url":requested,"effective_url":effective,"status":status.as_u16(),"content_type":content_type,"http_date":http_date,"http_last_modified":modified,"download_started_utc":request["started_utc"],"path":body_path.strip_prefix(root)?,"sha256":sha(&bytes),"bytes":bytes.len(),"receipts":receipts});
        verify_download(root, &v, binding, requested)?;
        put_json(&cache, &v)?;
        return Ok(v);
    }
    bail!("redirect limit; all attempted hops retained")
}
fn source_record(
    root: &Path,
    r: &Row,
    m: &Value,
    raw: &Value,
    mode: &AcquisitionMode,
) -> Result<Value> {
    mode.check_boundary(r)?;
    let split = if r["split"] == "train_reference" {
        "train_reference"
    } else {
        r["split"].strip_suffix("_reference").unwrap_or(&r["split"])
    };
    Ok(
        json!({"pg_id":r["pg_id"].parse::<u32>()?,"author_key":r["author_key"],"author":r["author"],"catalog_author":r["catalog_author"],"title":r["expected_title"],"split":split,"role":r["role"],"purpose":r["role"],"enrollment":r["enrollment"],"author_partition":r["author_partition"],"preassigned_partition":r["split"],"underlying_work_key":r["underlying_work_key"],"source_group":r["underlying_work_key"],"source_group_policy":"All editions, excerpts, and generated derivatives inherit this underlying work and assigned partition.","text_path":root.join(relative(raw["path"].as_str().context("raw path")?)?),"text_relative_path":raw["path"],"text_sha256":raw["sha256"],"text_bytes":raw["bytes"],"source_url":m["url"],"catalog_url":r["rights_evidence_url"],"catalog_evidence":{"path":format!("metadata/pg{}.rdf",r["pg_id"]),"sha256":r["metadata_sha256"],"url":r["rights_evidence_url"]},"catalog_extent_bytes":m["extent"],"catalog_extent_matches_download":m["extent"]==raw["bytes"],"raw_download":raw,"selection_row":r,
        "rights":{"commercial_training":true,"external_evaluation":true,"jurisdiction":"US","catalog_statement":RIGHTS,"basis":"Retained hash-bound Project Gutenberg RDF labels the underlying English work public domain in the USA; these flags do not establish rights elsewhere or external provider terms.","license_url":"https://www.gutenberg.org/policy/license.html","permission_url":"https://www.gutenberg.org/policy/permission.html","conditions":"Preserve PG notices and applicable trademark/license terms in PG-branded copies; no notice stripping is performed here.","evidence_sha256":r["metadata_sha256"],"evidence_url":r["rights_evidence_url"]},
        "dates":{"pg_ebook_release_date":m["pg_issued"],"pg_selected_file_modified":m["pg_file_modified"],"publication_year":null,"first_publication_date":null,"original_composition_date":null,"note":"PG issue/update dates are ebook metadata, never substituted for publication or composition dates."},
        "evidence":"historical_public_domain_author_proxy","exposure_caveats":{"pretraining_exposure":"unknown","human_only_production_history":"not_authenticated","textual_edition_contamination":"not_audited","prior_source_exclusion":"parent frozen source-scout metadata audit; not proof against undisclosed exposure"},"content_processing":"opaque HTTP bytes, size and SHA-256 only; no excerpts or prose analysis"}),
    )
}
fn run(tsv: &Path, metadata_root: &Path, output: &Path, selection: Option<&Path>) -> Result<()> {
    let input = read_bound(tsv, 2 * 1024 * 1024)?;
    let rows = parse_plan(&input)?;
    let mode = match selection {
        Some(path) => AcquisitionMode::from_selection(path, &input, &rows)?,
        None => AcquisitionMode::default(),
    };
    // Scope is frozen before metadata loading or construction of any text request.
    let selected = rows.iter().filter(|r| mode.permits(r)).collect::<Vec<_>>();
    ensure!(!selected.is_empty(), "no eligible works");
    let metadata_root = metadata_root.canonicalize()?;
    let mut checked = Vec::new();
    for r in &selected {
        let p = metadata_root
            .join(relative(&r["metadata_path"])?)
            .canonicalize()?;
        ensure!(p.starts_with(&metadata_root), "metadata escaped root");
        let b = read_bound(&p, 2 * 1024 * 1024)?;
        checked.push((metadata(r, &b)?, b));
    }
    // Reserved records contain catalog metadata only: never invent a text digest.
    let mut reserved = Vec::new();
    for r in rows.iter().filter(|r| !mode.is_test() && !eligible(r)) {
        let p = metadata_root
            .join(relative(&r["metadata_path"])?)
            .canonicalize()?;
        ensure!(p.starts_with(&metadata_root), "metadata escaped root");
        let b = read_bound(&p, 2 * 1024 * 1024)?;
        let m = metadata(r, &b)?;
        reserved.push(json!({"pg_id":r["pg_id"].parse::<u32>()?,"author_key":r["author_key"],"author":r["author"],"catalog_author":r["catalog_author"],"title":r["expected_title"],"split":r["split"],"preassigned_partition":r["split"],"role":r["role"],"purpose":r["role"],"enrollment":r["enrollment"],"author_partition":r["author_partition"],"source_group":r["underlying_work_key"],"underlying_work_key":r["underlying_work_key"],"text_downloaded":false,"metadata_only":true,"catalog_evidence":{"path":p,"sha256":r["metadata_sha256"],"url":r["rights_evidence_url"]},"source_url":m["url"],"rights":{"commercial_training":true,"external_evaluation":true,"jurisdiction":"US","catalog_statement":RIGHTS,"evidence_sha256":r["metadata_sha256"]},"dates":{"pg_ebook_release_date":m["pg_issued"],"publication_year":null,"first_publication_date":null,"original_composition_date":null},"evidence":"historical_public_domain_author_proxy","pretraining_exposure":"unknown","selection_row":r}));
    }
    let expected_roster = rows.iter().filter(|r|!mode.is_test() || mode.permits(r)).flat_map(|r| [20,60].map(|fraction| json!({"id":format!("author-pg{}-p{fraction}",r["pg_id"]),"pg_id":r["pg_id"].parse::<u32>().expect("validated PGID"),"frozen_split":r["split"].strip_suffix("_reference").unwrap_or(&r["split"]),"purpose":r["role"],"source_group":r["underlying_work_key"],"fraction":fraction}))).collect::<Vec<_>>();
    fs::create_dir_all(output)?;
    let root = output.canonicalize()?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(".lock"))?;
    lock.try_lock()
        .context("another acquirer holds output lock")?;
    // Keep the original build provenance when a compatible operational fix resumes
    // an unchanged acquisition contract; each new HTTP request records its build.
    let initial_implementation = if root.join("binding.json").exists() {
        read_json(&root.join("binding.json"))?["implementation_sha256"].clone()
    } else {
        json!(sha(include_bytes!("a_acquire_balanced_sources.rs")))
    };
    let mut binding = json!({"schema":"slop_ninja_balanced_author_acquisition_v1","selection_sha256":sha(&input),"test_policy":"test and test_reference excluded without override","max_source_bytes":LIMIT,"selected_rows":selected,"metadata_semantics":"RDF namespace-aware; exact scalar text after XML whitespace folding","implementation_sha256":initial_implementation});
    if let Some((path, hash)) = &mode.receipt {
        binding["schema"] = json!("slop_ninja_balanced_final_test_acquisition_v1");
        binding["test_policy"] =
            json!("Only exact reserved Test query/reference works after immutable v3 selection");
        binding["selection_receipt_path"] = json!(path);
        binding["selection_receipt_sha256"] = json!(hash);
        binding["selection_validator_sha256"] = json!(sha(include_bytes!(
            "../grammar/crates/grammar-a-baseline/examples/final_test_binding/mod.rs"
        )));
    }
    let binding_sha = sha(&serde_json::to_vec(&binding)?);
    if !root.join("binding.json").exists() {
        ensure!(
            fs::read_dir(&root)?.all(|e| e.is_ok_and(|e| e.file_name() == ".lock")),
            "unbound nonempty output directory"
        );
    }
    put_json(&root.join("binding.json"), &binding)?;
    put(&root.join("selection.tsv"), &input)?;
    for dir in ["metadata", "downloads", "http", "sources", "manifests"] {
        fs::create_dir_all(root.join(dir))?;
    }
    for (r, (_, b)) in selected.iter().zip(&checked) {
        put(&root.join(format!("metadata/pg{}.rdf", r["pg_id"])), b)?;
    }
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .http1_only()
        .timeout(Duration::from_secs(90))
        .connect_timeout(Duration::from_secs(20))
        .build()?;
    let mut sources = Vec::new();
    let mut total = 0_u64;
    let mut previous: Option<String> = None;
    let mut last = Value::Null;
    for (r, (m, _)) in selected.iter().zip(&checked) {
        let raw = download(&client, &root, r, m, &binding_sha, &mode)?;
        total += raw["bytes"].as_u64().context("size")?;
        ensure!(total <= 256 * 1024 * 1024, "aggregate acquisition bound");
        ensure!(
            !sources
                .iter()
                .any(|s: &Value| s["text_sha256"] == raw["sha256"]),
            "duplicate work bytes"
        );
        let record = source_record(&root, r, m, &raw, &mode)?;
        put_json(
            &root.join(format!("sources/pg{}.json", r["pg_id"])),
            &record,
        )?;
        sources.push(record);
        let references = sources
            .iter()
            .filter(|s| s["purpose"] == "reference")
            .count();
        last = json!({"schema":"slop_ninja.author_sources.v1","acquisition_schema":"slop_ninja_balanced_author_acquisition_v1","acquisition_id":"balanced-author-sources-v1","acquisition_root":root,"selection_sha256":sha(&input),"selection_path":"selection.tsv","binding_sha256":binding_sha,"source_count":sources.len(),"query_count":sources.len()-references,"reference_count":references,"total_text_bytes":total,"complete":sources.len()==selected.len(),"expected_non_test_sources":selected.len(),"excluded_test_sources":rows.len()-selected.len(),"planned_source_count":rows.len(),"reserved_source_count":reserved.len(),"reserved_sources":reserved,"expected_roster":expected_roster,"test_policy":"No Test or test_reference text requests; no override. Reserved sources have catalog metadata only, with no text path/hash.","previous_manifest_sha256":previous,"sources":sources});
        if let Some((_, hash)) = &mode.receipt {
            last["acquisition_schema"] = binding["schema"].clone();
            last["acquisition_id"] = json!("balanced-final-test-sources-v1");
            last["test_policy"] = binding["test_policy"].clone();
            last["expected_non_test_sources"] = json!(0);
            last["expected_test_sources"] = json!(selected.len());
            last["excluded_test_sources"] = json!(0);
            last["excluded_non_test_sources"] = json!(rows.len() - selected.len());
            last["planned_source_count"] = json!(selected.len());
            last["selection_receipt_sha256"] = json!(hash);
            last["test_text_opened"] = json!(true);
        }
        let snapshot = root.join(format!("manifests/{:04}.json", sources.len()));
        put_json(&snapshot, &last)?;
        previous = Some(sha(&fs::read(&snapshot)?));
        println!(
            "{}",
            json!({"completed":sources.len(),"expected":selected.len(),"pg_id":r["pg_id"],"manifest":snapshot})
        );
    }
    put_json(&root.join("manifest.json"), &last)?;
    put(
        &root.join("manifest.json.sha256"),
        format!(
            "{}  manifest.json\n",
            sha(&fs::read(root.join("manifest.json"))?)
        )
        .as_bytes(),
    )?;
    println!(
        "{}",
        json!({"manifest":root.join("manifest.json"),"sources":last["source_count"],"queries":last["query_count"],"references":last["reference_count"],"excluded_test_sources":last["excluded_test_sources"]})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    ensure!(
        a.len() == 3 || (a.len() == 5 && a[3] == Path::new("--final-test")),
        "a_acquire_balanced_sources TSV METADATA_ROOT OUTPUT [--final-test SELECTION]"
    );
    run(&a[0], &a[1], &a[2], a.get(4).map(PathBuf::as_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_test_mode_never_permits_unlisted_or_non_test_work() {
        let mode = AcquisitionMode {
            receipt: Some((PathBuf::from("synthetic"), "digest".into())),
            allowed: BTreeSet::from(["7".into()]),
        };
        let mut row = Row::from([
            ("pg_id".into(), "7".into()),
            ("split".into(), "test".into()),
        ]);
        assert!(mode.permits(&row));
        row.insert("split".into(), "train".into());
        assert!(!mode.permits(&row));
        row.insert("split".into(), "test_reference".into());
        row.insert("pg_id".into(), "8".into());
        assert!(!mode.permits(&row));
    }
    #[test]
    fn test_roles_never_eligible() {
        for split in ["test", "test_reference"] {
            let r = Row::from([
                ("split".into(), split.into()),
                ("author_partition".into(), "all".into()),
            ]);
            assert!(!eligible(&r));
        }
        let r = Row::from([
            ("split".into(), "calibration_reference".into()),
            ("author_partition".into(), "calibration".into()),
        ]);
        assert!(eligible(&r));
    }
    #[test]
    fn artifacts_never_overwritten() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("evidence");
        put(&p, b"exact").unwrap();
        put(&p, b"exact").unwrap();
        assert!(put(&p, b"changed").is_err());
        assert_eq!(fs::read(p).unwrap(), b"exact");
    }
    #[test]
    fn reject_traversal_and_foreign_redirect() {
        for p in ["../x", "/x", "a/../x", ""] {
            assert!(relative(p).is_err());
        }
        for u in [
            "http://www.gutenberg.org/x",
            "https://example.com/x",
            "https://www.gutenberg.org:444/x",
            "https://x@www.gutenberg.org/x",
        ] {
            assert!(checked_url(u).is_err());
        }
    }
    #[test]
    fn pg_http_location_is_upgraded_before_request() {
        let base = checked_url("https://www.gutenberg.org/ebooks/507.txt.utf-8").unwrap();
        assert_eq!(
            redirect_url(&base, "http://www.gutenberg.org/cache/epub/507/pg507.txt")
                .unwrap()
                .as_str(),
            "https://www.gutenberg.org/cache/epub/507/pg507.txt"
        );
        assert!(redirect_url(&base, "http://example.com/pg507.txt").is_err());
    }
    #[test]
    fn resume_rejects_tampered_body_or_binding() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("body"), b"exact").unwrap();
        let mut v = json!({"binding_sha256":"bound","requested_url":"https://www.gutenberg.org/ebooks/1.txt.utf-8","effective_url":"https://www.gutenberg.org/ebooks/1.txt.utf-8","receipts":[],"path":"body","sha256":sha(b"exact"),"bytes":5,"status":200,"content_type":"text/plain; charset=utf-8"});
        let url = "https://www.gutenberg.org/ebooks/1.txt.utf-8";
        verify_download(d.path(), &v, "bound", url).unwrap();
        v["binding_sha256"] = json!("other");
        assert!(verify_download(d.path(), &v, "bound", url).is_err());
        v["binding_sha256"] = json!("bound");
        fs::write(d.path().join("body"), b"alter").unwrap();
        assert!(verify_download(d.path(), &v, "bound", url).is_err());
    }
}
