//! Offline provenance manifest for the bounded 2026-10-26 author acquisition.
//! Downloads are already present. Source prose is ONLY streamed into SHA-256;
//! this example neither decodes it nor extracts samples, including for Test.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

const PLAN: &str = include_str!("../../../../docs/a-baseline-author-sources-20261026.tsv");
const ROOT: &str =
    "data/grammar-private/a-baseline-20261026/authors-v1";
const MAX_SOURCE_BYTES: u64 = 8 * 1024 * 1024;
const RIGHTS: &str = "Public domain in the USA.";

fn sha_file(path: &Path, limit: u64) -> Result<(String, u64)> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    ensure!(
        file.metadata()?.len() <= limit,
        "file exceeds bound: {}",
        path.display()
    );
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buf = [0_u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        ensure!(bytes <= limit, "file grew beyond bound");
        hasher.update(&buf[..n]);
    }
    ensure!(bytes > 0, "empty file: {}", path.display());
    Ok((hex::encode(hasher.finalize()), bytes))
}

fn metadata_text(path: &Path) -> Result<String> {
    ensure!(
        fs::metadata(path)?.len() <= 2 * 1024 * 1024,
        "metadata exceeds bound"
    );
    fs::read_to_string(path).with_context(|| format!("read metadata {}", path.display()))
}

// Narrow reader for retained PG RDF snapshots, not a general-purpose XML parser.
// Nested fields are scoped explicitly; summaries and reading-level fields are ignored.
fn elements<'a>(xml: &'a str, tag: &str) -> Result<Vec<&'a str>> {
    let start = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut rest = xml;
    let mut result = Vec::new();
    while let Some(index) = rest.find(&start) {
        let candidate = &rest[index + start.len()..];
        let next = candidate.chars().next().context("truncated XML")?;
        if next != '>' && !next.is_ascii_whitespace() {
            rest = candidate;
            continue;
        }
        let end = candidate.find('>').context("missing XML tag end")?;
        ensure!(!candidate[..end].ends_with('/'), "unexpected empty {tag}");
        let body = &candidate[end + 1..];
        let end = body
            .find(&close)
            .with_context(|| format!("missing {close}"))?;
        result.push(&body[..end]);
        rest = &body[end + close.len()..];
    }
    Ok(result)
}

fn one<'a>(xml: &'a str, tag: &str) -> Result<&'a str> {
    let values = elements(xml, tag)?;
    ensure!(
        values.len() == 1,
        "expected one {tag}, found {}",
        values.len()
    );
    Ok(values[0])
}

fn xml_text(value: &str) -> Result<String> {
    ensure!(!value.contains('<'), "unexpected markup in scalar metadata");
    let mut out = String::new();
    let mut rest = value;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos + 1..];
        let end = rest.find(';').context("unterminated XML entity")?;
        let entity = &rest[..end];
        let ch = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let number = if let Some(hex) = entity.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16)?
                } else if let Some(decimal) = entity.strip_prefix('#') {
                    decimal.parse::<u32>()?
                } else {
                    bail!("unsupported XML entity: {entity}");
                };
                char::from_u32(number).context("invalid XML character")?
            }
        };
        out.push(ch);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out.trim().to_owned())
}

fn scalar(xml: &str, tag: &str) -> Result<String> {
    xml_text(one(xml, tag)?)
}

fn http_header(headers: &str, name: &str) -> Option<String> {
    headers
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
        .last()
}

fn receipt(root: &Path, relative: &str, requested_url: &str, content_type: &str) -> Result<Value> {
    let (sha256, bytes) = sha_file(&root.join(relative), MAX_SOURCE_BYTES)?;
    let transfer_path = format!("{relative}.transfer.json");
    let header_path = format!("{relative}.headers");
    let transfer: Value = serde_json::from_str(&metadata_text(&root.join(&transfer_path))?)?;
    let headers = metadata_text(&root.join(&header_path))?;
    ensure!(
        transfer["exitcode"] == 0 && transfer["http_code"] == 200,
        "failed transfer: {relative}"
    );
    ensure!(
        transfer["url"] == requested_url,
        "requested URL mismatch: {relative}"
    );
    ensure!(
        transfer["size_download"].as_u64() == Some(bytes),
        "transfer length mismatch: {relative}"
    );
    ensure!(
        transfer["ssl_verify_result"] == 0,
        "TLS verification failed: {relative}"
    );
    let effective = transfer["url_effective"]
        .as_str()
        .context("effective URL missing")?;
    ensure!(
        effective.starts_with("https://www.gutenberg.org/"),
        "unexpected source host: {relative}"
    );
    ensure!(
        transfer["content_type"]
            .as_str()
            .is_some_and(|s| s.starts_with(content_type)),
        "content type mismatch: {relative}"
    );
    let started_path = root.join(format!("{relative}.started-utc.txt"));
    let started = if started_path.exists() {
        Some(metadata_text(&started_path)?.trim().to_owned())
    } else {
        None
    };
    Ok(json!({
        "path": relative, "sha256": sha256, "bytes": bytes,
        "requested_url": requested_url, "effective_url": effective,
        "download_started_utc": started,
        "http_date": http_header(&headers, "date"),
        "http_last_modified": http_header(&headers, "last-modified"),
        "headers_path": header_path,
        "headers_sha256": sha_file(&root.join(&header_path), 2 * 1024 * 1024)?.0,
        "transfer_receipt_path": transfer_path,
        "transfer_receipt_sha256": sha_file(&root.join(&transfer_path), 2 * 1024 * 1024)?.0
    }))
}

fn write_new_or_identical(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        ensure!(
            fs::read(path)? == bytes,
            "refusing to replace different manifest: {}",
            path.display()
        );
        return Ok(());
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(args.len() == 1, "acquire_author_sources {ROOT}");
    let root = Path::new(&args[0]);
    ensure!(
        root.canonicalize()? == Path::new(ROOT).canonicalize()?,
        "use the authorized acquisition directory"
    );
    ensure!(
        fs::read_to_string("/proc/sys/kernel/hostname")?.trim() == "build-host",
        "build-host only"
    );
    ensure!(
        metadata_text(&root.join("selection.tsv"))? == PLAN,
        "selection differs from frozen plan"
    );

    let policy = [
        receipt(
            root,
            "evidence/permission.html",
            "https://www.gutenberg.org/policy/permission.html",
            "text/html",
        )?,
        receipt(
            root,
            "evidence/license.html",
            "https://www.gutenberg.org/policy/license.html",
            "text/html",
        )?,
    ];
    let known = ["jane_austen", "charles_dickens", "mark_twain", "h_g_wells"];
    let withheld = [
        ("arthur_conan_doyle", "development"),
        ("r_l_stevenson", "calibration"),
        ("mary_shelley", "test"),
    ];
    let mut by_author: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut digests = BTreeSet::new();
    let mut sources = Vec::new();
    let mut total_bytes = 0_u64;
    for line in PLAN.lines().skip(1) {
        let c = line.split('\t').collect::<Vec<_>>();
        ensure!(c.len() == 8, "invalid selection row");
        let pg_id = c[0].parse::<u32>()?;
        let (author_key, author, expected_author, expected_title, assigned, purpose, enrollment) =
            (c[1], c[2], c[3], c[4], c[5], c[6], c[7]);
        ensure!(ids.insert(pg_id), "repeated PG id");
        let reference = assigned == "train_reference";
        ensure!(
            purpose == if reference { "reference" } else { "query" },
            "invalid purpose"
        );
        if known.contains(&author_key) {
            ensure!(enrollment == "known", "known enrollment mismatch");
        } else {
            let expected_split = withheld
                .iter()
                .find(|(key, _)| *key == author_key)
                .context("unexpected author")?
                .1;
            ensure!(
                enrollment == "withheld" && assigned == expected_split && !reference,
                "withheld author escaped assigned split"
            );
        }
        by_author
            .entry(author_key.to_owned())
            .or_default()
            .push(assigned.to_owned());
        let split = if reference { "train" } else { assigned };
        ensure!(
            ["train", "development", "calibration", "test"].contains(&split),
            "invalid split"
        );
        let role = if reference {
            "train_reference"
        } else {
            "query"
        };

        let rdf_path = format!("evidence/pg{pg_id}.rdf");
        let page_path = format!("evidence/pg{pg_id}.html");
        let text_path = format!("downloads/pg{pg_id}.txt");
        let primary_url = format!("https://www.gutenberg.org/ebooks/{pg_id}");
        let source_url = format!("{primary_url}.txt.utf-8");
        let rdf = metadata_text(&root.join(&rdf_path))?;
        ensure!(
            rdf.contains(&format!("<pgterms:ebook rdf:about=\"ebooks/{pg_id}\">")),
            "catalog ID mismatch"
        );
        let title = scalar(&rdf, "dcterms:title")?;
        ensure!(title == expected_title, "PG{pg_id} title mismatch: {title}");
        let creator = one(&rdf, "dcterms:creator")?;
        ensure!(
            scalar(creator, "pgterms:name")? == expected_author,
            "PG{pg_id} author mismatch"
        );
        ensure!(
            scalar(&rdf, "dcterms:rights")? == RIGHTS,
            "PG{pg_id} rights mismatch"
        );
        ensure!(
            scalar(one(&rdf, "dcterms:language")?, "rdf:value")? == "en",
            "PG{pg_id} not English"
        );
        ensure!(
            scalar(one(&rdf, "dcterms:type")?, "rdf:value")? == "Text",
            "PG{pg_id} not text"
        );
        ensure!(
            !rdf.contains("<marcrel:trl>"),
            "PG{pg_id} has a credited translator"
        );
        let page = metadata_text(&root.join(&page_path))?;
        ensure!(
            page.contains("property=\"dcterms:rights\">Public domain in the USA.</td>"),
            "PG{pg_id} primary page rights mismatch"
        );
        let file_marker = format!("<pgterms:file rdf:about=\"{source_url}\">");
        let file_metadata = rdf
            .split_once(&file_marker)
            .context("selected URL absent from catalog")?
            .1
            .split_once("</pgterms:file>")
            .context("file metadata unterminated")?
            .0;
        ensure!(
            file_metadata.contains(">text/plain; charset=utf-8</rdf:value>"),
            "PG{pg_id} selected format not UTF-8 plain text"
        );

        let raw = receipt(root, &text_path, &source_url, "text/plain")?;
        let size = raw["bytes"].as_u64().context("byte length missing")?;
        total_bytes += size;
        ensure!(
            total_bytes <= 64 * 1024 * 1024,
            "acquisition exceeds 64 MiB bound"
        );
        ensure!(
            digests.insert(raw["sha256"].as_str().context("digest missing")?.to_owned()),
            "duplicate raw work bytes"
        );
        let catalog_extent = scalar(file_metadata, "dcterms:extent")?.parse::<u64>()?;
        let catalog = receipt(
            root,
            &rdf_path,
            &format!("{primary_url}.rdf"),
            "application/rdf+xml",
        )?;
        let primary_page = receipt(root, &page_path, &primary_url, "text/html")?;
        let publication = elements(&rdf, "pgterms:marc260")?
            .into_iter()
            .map(xml_text)
            .collect::<Result<Vec<_>>>()?;
        sources.push(json!({
            "pg_id": pg_id, "author_key": author_key, "author": author,
            "catalog_author": expected_author, "title": title,
            "split": split, "role": role, "purpose": purpose, "enrollment": enrollment,
            "preassigned_partition": assigned,
            "source_group": format!("author_work_v1:{author_key}:pg{pg_id}"),
            "source_group_policy": "All editions, excerpts and derivatives of this complete original work inherit this group and partition; do not split chapters or paragraphs across partitions.",
            "text_path": root.join(&text_path), "text_relative_path": text_path,
            "text_sha256": raw["sha256"], "text_bytes": size,
            "source_url": source_url, "primary_page_url": primary_url,
            "catalog_url": format!("{primary_url}.rdf"),
            "rights": {
                "commercial_training": true, "external_evaluation": true,
                "jurisdiction": "US", "catalog_statement": RIGHTS,
                "basis": "The retained Project Gutenberg primary page and RDF catalog independently label this English text public domain in the USA. PG's retained permission policy permits commercial use and derivatives of US public-domain works. These flags cover the underlying work for US-based training and external evaluation; PG trademark/license terms still apply to copies retaining PG material.",
                "license_url": "https://www.gutenberg.org/policy/license.html",
                "permission_url": "https://www.gutenberg.org/policy/permission.html",
                "conditions": "Raw bytes and embedded notices are retained unchanged. Retain required notices when forwarding PG-branded copies, or use underlying text with all PG references removed under license section 1.C. Rights in other jurisdictions and external service terms are not established here.",
                "evidence_paths": [rdf_path, page_path, "evidence/permission.html", "evidence/license.html"],
                "embedded_notice_review": "retained in opaque raw bytes; not read analytically or parsed by this acquisition"
            },
            "dates": {
                "pg_ebook_release_date": scalar(&rdf, "dcterms:issued")?,
                "pg_selected_file_modified": scalar(file_metadata, "dcterms:modified")?,
                "catalog_publication_statement_verbatim": publication,
                "original_composition_date": null, "first_publication_date": null,
                "note": "PG release/update dates describe the ebook. MARC 260 is retained as supplied and may describe a later edition; no original composition or first-publication date is inferred from it or from automated summaries."
            },
            "catalog_extent_bytes": catalog_extent, "catalog_extent_matches_download": catalog_extent == size,
            "raw_download": raw, "catalog_evidence": catalog, "primary_page_evidence": primary_page,
            "content_processing": "opaque byte count and SHA-256 only; no decoding, excerpts, parsing, scoring or tuning"
        }));
    }
    ensure!(
        sources.len() == 26 && by_author.len() == 7,
        "expected 26 works and seven authors"
    );
    for author in known {
        let partitions = by_author.get(author).context("known author missing")?;
        let actual = partitions
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected = [
            "train_reference",
            "train",
            "development",
            "calibration",
            "test",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        ensure!(
            partitions.len() == 5 && actual == expected,
            "incomplete known author: {author}"
        );
    }
    for (author, _) in withheld {
        ensure!(
            by_author.get(author).is_some_and(|v| v.len() == 2),
            "withheld author needs two works"
        );
    }
    let manifest = json!({
        "schema": "slop_ninja.author_sources.v1", "acquisition_id": "a-baseline-20261026/authors-v1",
        "acquisition_root": root, "source_count": sources.len(), "total_text_bytes": total_bytes,
        "selection_sha256": hex::encode(Sha256::digest(PLAN.as_bytes())),
        "selection_path": "selection.tsv", "cfg_policy": "frozen; no grammar changes",
        "split_counts": {"train": 8, "development": 6, "calibration": 6, "test": 6},
        "reference_count": 4, "query_count": 22,
        "withheld_authors": {"arthur_conan_doyle": "development", "r_l_stevenson": "calibration", "mary_shelley": "test"},
        "test_policy": "Mechanically acquired and hashed only. No prose analysis, sample selection, feature extraction or tuning; final transform belongs to a separately frozen parent run.",
        "policy_evidence": policy, "sources": sources
    });
    let mut bytes = serde_json::to_vec_pretty(&manifest)?;
    bytes.push(b'\n');
    write_new_or_identical(&root.join("manifest.json"), &bytes)?;
    let checksum = format!("{}  manifest.json\n", hex::encode(Sha256::digest(&bytes)));
    write_new_or_identical(&root.join("manifest.json.sha256"), checksum.as_bytes())?;
    println!(
        "26 works; 7 authors; {total_bytes} raw bytes; manifest: {}",
        root.join("manifest.json").display()
    );
    Ok(())
}
