//! Operational coverage and a queue for manual syntax review, not parse accuracy.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::{
    features::{self, Family},
    rules::{self, RewriteRule},
    syntax::Document,
};
use rusqlite::{Connection, OpenFlags, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::process::Command;
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
};

#[derive(Parser)]
struct Args {
    /// JSON source manifest. No source snapshot or text is written to the audit DB.
    manifest: PathBuf,
    #[arg(long)]
    out: PathBuf,
    #[arg(long)]
    python: String,
    #[arg(long, default_value = grammar_spacy::DEFAULT_MODEL)]
    parser_model: String,
    /// Treat blank lines as hard paragraph boundaries; separate parser identity.
    #[arg(long)]
    paragraph_boundaries: bool,
    /// Experimental v2: parse paragraph content without edge whitespace.
    #[arg(long, conflicts_with = "paragraph_boundaries")]
    paragraph_content: bool,
    /// Optional annotation samples for manual inspection. Use private external storage.
    #[arg(long)]
    review_dir: Option<PathBuf>,
    #[arg(long, default_value_t = 64)]
    review_limit: usize,
    /// Deterministic hash sampling, including ordinary unflagged documents.
    #[arg(long, default_value_t = 997)]
    review_modulus: u64,
    /// Explicit unsupported-input limit; nothing is truncated.
    #[arg(long, default_value_t = 200_000)]
    max_chars: usize,
    /// Stop after this many new distinct texts; rerun to continue the same audit.
    #[arg(long)]
    limit: Option<usize>,
    #[arg(long, default_value = "pandoc")]
    pandoc: String,
    #[arg(long, default_value = "pdftotext")]
    pdftotext: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    sources: Vec<Source>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    id: String,
    path: PathBuf,
    /// jsonl, jsonl_array, json_array, tsv, text, blog_sqlite or a rendered format.
    format: String,
    #[serde(default = "text_pointer")]
    text_pointer: String,
    /// For json_array/jsonl_array, select the array within its enclosing JSON value.
    #[serde(default)]
    array_pointer: String,
    /// Required only for TSV. Split at most this many columns, preserving the last.
    #[serde(default)]
    tsv_columns: Option<usize>,
    /// Existing split/group/author metadata pointers; values remain in the private DB.
    #[serde(default)]
    metadata: BTreeMap<String, String>,
    /// Optional externally supplied printable macro definitions for a LaTeX view.
    #[serde(default)]
    latex_preamble: Option<PathBuf>,
    /// Resolve author/year citations in rendered views; never copy the bibliography.
    #[serde(default)]
    bibliography: Option<PathBuf>,
}
fn text_pointer() -> String {
    "/text".into()
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn file_hash(path: &Path) -> Result<String> {
    let mut input = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(hex::encode(digest.finalize()))
}
fn write_json(path: &Path, data: &Value) -> Result<()> {
    let temp = path.with_extension("json.tmp");
    let mut file = File::create(&temp)?;
    serde_json::to_writer_pretty(&mut file, data)?;
    file.write_all(b"\n")?;
    fs::rename(temp, path)?;
    Ok(())
}

/// Preserve every nested item, including malformed/missing text, with its locator.
fn nested_records(source: &Source, line: &str, line_number: usize) -> Vec<(Option<String>, Value)> {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => {
            return vec![(
                None,
                json!({"line_number":line_number,"input_status":"invalid_json","line_sha256":hash(line.as_bytes())}),
            )];
        }
    };
    let metadata: BTreeMap<_, _> = source
        .metadata
        .iter()
        .map(|(name, pointer)| (name, value.pointer(pointer).cloned().unwrap_or(Value::Null)))
        .collect();
    let Some(items) = value
        .pointer(&source.array_pointer)
        .and_then(Value::as_array)
    else {
        return vec![(
            None,
            json!({"line_number":line_number,"input_status":"missing_array","record":metadata}),
        )];
    };
    if items.is_empty() {
        return vec![(
            None,
            json!({"line_number":line_number,"input_status":"empty_array","record":metadata}),
        )];
    }
    items.iter().enumerate().map(|(index, item)| {
        let text = item.pointer(&source.text_pointer).and_then(Value::as_str).map(str::to_owned);
        let locator = json!({"line_number":line_number,"array_index":index,"input_status":if text.is_some(){"text_present"}else{"missing_text"},"record":metadata});
        (text, locator)
    }).collect()
}

fn diagnostics(doc: &Document) -> Result<Value> {
    let extracted = features::extract(doc)?;
    let mut family = BTreeMap::new();
    for (name, f) in extracted.families {
        let opportunities = match f {
            Family::Distribution { opportunities, .. }
            | Family::Rates { opportunities, .. }
            | Family::Metrics { opportunities, .. } => opportunities,
        };
        family.insert(name, opportunities);
    }
    let mut flags: BTreeMap<&str, usize> = BTreeMap::new();
    for s in &doc.sentences {
        let root = &doc.tokens[s.root];
        if !matches!(root.pos.as_str(), "VERB" | "AUX") {
            *flags.entry("nonverbal_root").or_default() += 1;
        }
        if !doc.tokens[s.token_start..s.token_end].iter().any(|t| {
            t.head == root.i
                && matches!(
                    t.dep.as_str(),
                    "nsubj" | "nsubjpass" | "csubj" | "csubjpass"
                )
        }) {
            *flags.entry("no_overt_main_subject").or_default() += 1;
        }
        if s.token_end - s.token_start > 80 {
            *flags.entry("sentence_over_80_tokens").or_default() += 1;
        }
        if doc.tokens[s.token_start..s.token_end]
            .iter()
            .any(|t| !t.is_space && !t.is_punct && (t.dep == "dep" || t.pos == "X"))
        {
            *flags.entry("unspecified_relation_or_pos").or_default() += 1;
        }
        if doc.tokens[s.token_start..s.token_end]
            .iter()
            .any(|t| !t.is_space && !t.is_punct && doc.tokens[t.head].is_space)
        {
            *flags.entry("content_head_is_space").or_default() += 1;
        }
        if doc.tokens[s.token_start..s.token_end]
            .iter()
            .any(|t| t.pos == "PUNCT" && t.text.chars().any(char::is_alphabetic))
        {
            *flags
                .entry("alphabetic_text_tagged_punctuation")
                .or_default() += 1;
        }
        if doc.tokens[s.token_start..s.token_end]
            .iter()
            .any(|t| t.text == ";")
        {
            *flags.entry("semicolon_scope_review").or_default() += 1;
        }
        let raw = doc.text[s.start_byte..s.end_byte].trim();
        if raw.contains("\n\n") || raw.contains("\r\n\r\n") {
            *flags.entry("sentence_crosses_blank_line").or_default() += 1;
        }
        if doc.tokens[s.token_start..s.token_end].iter().any(|t| {
            let chars: Vec<_> = t.text.chars().collect();
            !t.is_space
                && chars
                    .windows(3)
                    .any(|w| w[0].is_alphabetic() && w[1] == '.' && w[2].is_alphabetic())
        }) {
            *flags.entry("internal_period_token_review").or_default() += 1;
        }
    }
    let mut nominations = BTreeMap::new();
    for rule in rules::CATALOG {
        nominations.insert(rule.id, rule.apply(doc)?.len());
    }
    Ok(
        json!({"status":"parsed_unreviewed", "tokens":doc.tokens.len(), "sentences":doc.sentences.len(), "flags":flags, "feature_opportunities":family, "rule_nominations":nominations, "parse_correctness":"unreviewed", "meaning_preservation":"unreviewed"}),
    )
}

struct Audit<'a> {
    db: Connection,
    parser: grammar_spacy::Stream,
    identity: String,
    args: &'a Args,
    new: usize,
    reviews: usize,
}
impl Audit<'_> {
    fn record(
        &mut self,
        source: &Source,
        ordinal: usize,
        text: Option<&str>,
        metadata: Value,
    ) -> Result<bool> {
        if self.db.query_row(
            "SELECT COUNT(*) FROM records WHERE source=?1 AND ordinal=?2",
            params![source.id, ordinal],
            |r| r.get::<_, u64>(0),
        )? > 0
        {
            return Ok(true);
        }
        if self.args.limit.is_some_and(|limit| self.new >= limit) {
            return Ok(false);
        }
        let Some(text) = text else {
            self.db.execute(
                "INSERT INTO records VALUES (?1,?2,NULL,?3,'missing_text')",
                params![source.id, ordinal, metadata.to_string()],
            )?;
            return Ok(true);
        };
        let key = hash(text.as_bytes());
        let cached: u64 = self.db.query_row(
            "SELECT COUNT(*) FROM observations WHERE sha256=?1",
            [&key],
            |r| r.get(0),
        )?;
        if cached == 0 {
            let detail = if text.chars().count() > self.args.max_chars {
                json!({"status":"unsupported_size", "parse_correctness":"unreviewed"})
            } else {
                match self.parser.parse(text)? {
                    Err(error) => {
                        json!({"status":"annotation_failure", "error_sha256":hash(error.as_bytes()), "error_detail_saved":false, "parse_correctness":"unreviewed"})
                    }
                    Ok(doc) => {
                        ensure!(
                            doc.parser_identity == self.identity,
                            "Parser identity drift"
                        );
                        let selected = self.args.review_dir.is_some()
                            && self.reviews < self.args.review_limit
                            && u64::from_str_radix(&key[..16], 16)? % self.args.review_modulus == 0;
                        if selected {
                            let dir = self.args.review_dir.as_ref().unwrap();
                            let path = dir.join(format!("{key}.annotation.json"));
                            if !path.exists() {
                                let file = fs::OpenOptions::new()
                                    .write(true)
                                    .create_new(true)
                                    .open(&path)?;
                                serde_json::to_writer(file, &doc)?;
                            }
                            self.db.execute(
                                "INSERT OR IGNORE INTO review_queue VALUES (?1,?2,?3,'unreviewed')",
                                params![key, source.id, ordinal],
                            )?;
                            self.reviews += 1;
                        }
                        match diagnostics(&doc) {
                            Ok(value) => value,
                            Err(error) => {
                                json!({"status":"feature_or_rule_failure", "error_sha256":hash(format!("{error:#}").as_bytes()), "error_detail_saved":false, "parse_correctness":"unreviewed"})
                            }
                        }
                    }
                }
            };
            self.db.execute(
                "INSERT INTO observations VALUES (?1,?2,?3,?4)",
                params![
                    key,
                    text.len(),
                    detail["status"].as_str(),
                    detail.to_string()
                ],
            )?;
            self.new += 1;
            if self.new.is_multiple_of(100) {
                eprintln!(
                    "{} new distinct texts inspected mechanically; no correctness certification",
                    self.new
                );
            }
        }
        self.db.execute(
            "INSERT INTO records VALUES (?1,?2,?3,?4,'text_present')",
            params![source.id, ordinal, key, metadata.to_string()],
        )?;
        Ok(true)
    }
    fn json_record(&mut self, source: &Source, ordinal: usize, value: Value) -> Result<bool> {
        let metadata: BTreeMap<_, _> = source
            .metadata
            .iter()
            .map(|(name, pointer)| (name, value.pointer(pointer).cloned().unwrap_or(Value::Null)))
            .collect();
        self.record(
            source,
            ordinal,
            value.pointer(&source.text_pointer).and_then(Value::as_str),
            json!(metadata),
        )
    }
    fn source(&mut self, source: &Source, path: &Path) -> Result<bool> {
        match source.format.as_str() {
            "jsonl_array" => {
                let mut ordinal = 0;
                for (i, line) in BufReader::new(File::open(path)?).lines().enumerate() {
                    let line = line?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    for (text, metadata) in nested_records(source, &line, i + 1) {
                        if !self.record(source, ordinal, text.as_deref(), metadata)? {
                            return Ok(false);
                        }
                        ordinal += 1;
                    }
                }
            }
            "tsv" => {
                let columns = source.tsv_columns.context("TSV requires tsv_columns")?;
                for (i, line) in BufReader::new(File::open(path)?).lines().enumerate() {
                    let line = line?;
                    let fields: Vec<_> = line.splitn(columns, '\t').collect();
                    if fields.len() != columns {
                        if !self.record(source, i, None, json!({"line_number":i+1,"input_status":"wrong_column_count","observed_columns":fields.len(),"line_sha256":hash(line.as_bytes())}))? {
                            return Ok(false);
                        }
                    } else if !self.json_record(source, i, json!(fields))? {
                        return Ok(false);
                    }
                }
            }
            "jsonl" => {
                for (i, line) in BufReader::new(File::open(path)?).lines().enumerate() {
                    let line = line?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    let value: Value = match serde_json::from_str(&line) {
                        Ok(value) => value,
                        Err(_) => {
                            if !self.record(source,i,None,json!({"input_status":"invalid_json","line_sha256":hash(line.as_bytes())}))? { return Ok(false); }
                            continue;
                        }
                    };
                    if !self.json_record(source, i, value)? {
                        return Ok(false);
                    }
                }
            }
            "json_array" => {
                let value: Value = serde_json::from_reader(BufReader::new(File::open(path)?))?;
                let array = value
                    .pointer(&source.array_pointer)
                    .and_then(Value::as_array)
                    .context("Missing source array")?;
                for (i, value) in array.iter().enumerate() {
                    if !self.json_record(source, i, value.clone())? {
                        return Ok(false);
                    }
                }
            }
            "text" => {
                let bytes = fs::read(path)?;
                match std::str::from_utf8(&bytes) {
                    Ok(text) => {
                        if !self.record(source, 0, Some(text), json!({}))? {
                            return Ok(false);
                        }
                    }
                    Err(_) => {
                        if !self.record(source, 0, None, json!({"input_status":"invalid_utf8"}))? {
                            return Ok(false);
                        }
                    }
                }
            }
            "latex" | "markdown" | "docx" | "html" | "pdf" => {
                let path = path.canonicalize()?;
                let output = if source.format == "pdf" {
                    Command::new(&self.args.pdftotext)
                        .arg("-layout")
                        .arg(&path)
                        .arg("-")
                        .output()?
                } else {
                    let mut command = Command::new(&self.args.pandoc);
                    command
                        .arg("--sandbox")
                        .arg("--from")
                        .arg(&source.format)
                        .arg("--to=plain")
                        .arg("--wrap=none");
                    if let Some(preamble) = &source.latex_preamble {
                        ensure!(
                            source.format == "latex",
                            "A LaTeX preamble requires LaTeX source"
                        );
                        command.arg(
                            self.args
                                .manifest
                                .parent()
                                .unwrap_or(Path::new("."))
                                .join(preamble)
                                .canonicalize()?,
                        );
                    }
                    if let Some(bibliography) = &source.bibliography {
                        command
                            .arg("--citeproc")
                            .arg("--bibliography")
                            .arg(
                                self.args
                                    .manifest
                                    .parent()
                                    .unwrap_or(Path::new("."))
                                    .join(bibliography)
                                    .canonicalize()?,
                            )
                            .arg("--metadata=suppress-bibliography:true");
                    }
                    command
                        .arg(&path)
                        .current_dir(path.parent().unwrap())
                        .output()?
                };
                if !output.status.success() {
                    // Do not write converter stderr, which may contain source passages.
                    if !self.record(
                        source,
                        0,
                        None,
                        json!({"extraction":"failed","exit_code":output.status.code()}),
                    )? {
                        return Ok(false);
                    }
                } else {
                    let text = String::from_utf8(output.stdout)?;
                    if !self.record(source,0,Some(&text),json!({"extraction":source.format,"view":"rendered_plain_text","extraction_correctness":"unreviewed","not_raw_source_offsets":true,"converter_warning_bytes":output.stderr.len()}))? { return Ok(false); }
                }
            }
            "blog_sqlite" => {
                let input = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
                let mut query = input.prepare("SELECT id,exact_text,author_id,source_key,status,authored_date FROM posts ORDER BY id")?;
                let mut rows = query.query([])?;
                while let Some(row) = rows.next()? {
                    let ordinal: usize = row.get(0)?;
                    let text: Option<String> = row.get(1)?;
                    let metadata = json!({"author_id":row.get::<_,String>(2)?,"source_group":row.get::<_,String>(3)?,"import_status":row.get::<_,String>(4)?,"authored_date":row.get::<_,Option<String>>(5)?,"split":"not_assigned_by_coverage_audit"});
                    if !self.record(source, ordinal, text.as_deref(), metadata)? {
                        return Ok(false);
                    }
                }
            }
            _ => anyhow::bail!("Unsupported source format"),
        }
        Ok(true)
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        args.max_chars > 0 && args.review_modulus > 0,
        "Positive input limit and review modulus required"
    );
    let manifest_bytes = fs::read(&args.manifest)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    ensure!(!manifest.sources.is_empty(), "Empty manifest");
    let base = args.manifest.parent().unwrap_or(Path::new("."));
    let mut source_bindings = BTreeMap::new();
    for source in &manifest.sources {
        ensure!(
            (source.format == "tsv") == source.tsv_columns.is_some()
                && source.tsv_columns.is_none_or(|n| (1..=1024).contains(&n)),
            "Only TSV sources require tsv_columns, in 1..1024"
        );
        ensure!(
            !source.id.is_empty() && !source_bindings.contains_key(&source.id),
            "Empty or duplicate source ID"
        );
        source_bindings.insert(source.id.clone(), json!({"source_sha256":file_hash(&base.join(&source.path))?,"latex_preamble_sha256":source.latex_preamble.as_ref().map(|p| file_hash(&base.join(p))).transpose()?,"bibliography_sha256":source.bibliography.as_ref().map(|p| file_hash(&base.join(p))).transpose()?}));
    }
    fs::create_dir_all(&args.out)?;
    if let Some(dir) = &args.review_dir {
        fs::create_dir_all(dir)?;
    }
    let db = Connection::open(args.out.join("coverage.sqlite"))?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
        CREATE TABLE IF NOT EXISTS bindings (id INTEGER PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS observations (sha256 TEXT PRIMARY KEY, bytes INTEGER, status TEXT NOT NULL, detail TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS records (source TEXT NOT NULL, ordinal INTEGER NOT NULL, sha256 TEXT, metadata TEXT, status TEXT NOT NULL, PRIMARY KEY(source,ordinal));
        CREATE TABLE IF NOT EXISTS review_queue (sha256 TEXT PRIMARY KEY, source TEXT, ordinal INTEGER, judgment TEXT NOT NULL);")?;
    let config = grammar_spacy::Parser {
        python: &args.python,
        model: &args.parser_model,
    };
    let mut parser = if args.paragraph_content {
        config.stream_with_paragraph_content()?
    } else if args.paragraph_boundaries {
        config.stream_with_paragraph_boundaries()?
    } else {
        config.stream()?
    };
    let probe = parser
        .parse("The sample contains a sentence.")?
        .map_err(anyhow::Error::msg)?;
    let has_rendered = manifest.sources.iter().any(|s| {
        matches!(
            s.format.as_str(),
            "latex" | "markdown" | "docx" | "html" | "pdf"
        )
    });
    let converters = if has_rendered {
        let pandoc = Command::new(&args.pandoc).arg("--version").output()?;
        let pdf = Command::new(&args.pdftotext).arg("-v").output()?;
        ensure!(
            pandoc.status.success() && pdf.status.success(),
            "Cannot identify text converters"
        );
        json!({"pandoc":String::from_utf8(pandoc.stdout)?,"pdftotext":String::from_utf8(pdf.stderr)?,"policy":"sandbox-pandoc-plain-no-wrap;pdftotext-layout-v1"})
    } else {
        Value::Null
    };
    let binding = json!({"schema":"slop_ninja_grammar_coverage_v1", "audit_code_sha256":hash(include_bytes!("coverage.rs")), "manifest_sha256":hash(&manifest_bytes), "sources":source_bindings, "parser_identity":probe.parser_identity, "catalog":rules::CATALOG_VERSION, "features":features::FEATURE_VERSION, "max_chars":args.max_chars, "review_modulus":args.review_modulus, "review_limit":args.review_limit, "review_dir":args.review_dir,"converters":converters});
    let count: u64 = db.query_row("SELECT COUNT(*) FROM bindings", [], |r| r.get(0))?;
    if count == 0 {
        db.execute("INSERT INTO bindings VALUES(1,?1)", [binding.to_string()])?;
    } else {
        let old: String =
            db.query_row("SELECT value FROM bindings WHERE id=1", [], |r| r.get(0))?;
        ensure!(
            serde_json::from_str::<Value>(&old)? == binding,
            "Audit bindings changed; use a separate output directory"
        );
    }
    let reviews = db.query_row("SELECT COUNT(*) FROM review_queue", [], |r| r.get(0))?;
    let mut audit = Audit {
        db,
        parser,
        identity: probe.parser_identity,
        args: &args,
        new: 0,
        reviews,
    };
    let mut complete = true;
    for source in &manifest.sources {
        eprintln!("Inspecting source {}", source.id);
        let finished = audit.source(source, &base.join(&source.path))?;
        ensure!(
            file_hash(&base.join(&source.path))?
                == binding["sources"][&source.id]["source_sha256"]
                    .as_str()
                    .unwrap(),
            "Input changed during traversal"
        );
        if !finished {
            complete = false;
            break;
        }
    }
    let statuses: BTreeMap<String, u64> = audit
        .db
        .prepare("SELECT status,COUNT(*) FROM observations GROUP BY status")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let records: u64 = audit
        .db
        .query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))?;
    let missing: u64 = audit.db.query_row(
        "SELECT COUNT(*) FROM records WHERE status='missing_text'",
        [],
        |r| r.get(0),
    )?;
    write_json(
        &args.out.join("summary.json"),
        &json!({"binding":binding,"complete_input_traversal":complete,"records_visited":records,"records_without_text":missing,"distinct_text_statuses":statuses,"new_distinct_texts":audit.new,"review_queue_size":audit.reviews,"parse_accuracy":null,"correctness_certified":false,"no_error_is_not_correctness":true}),
    )?;
    audit.parser.finish()?;
    Ok(())
}
