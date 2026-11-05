//! slop_ninja_a_local_score: score documents with an artifact on your own machine,
//! with the hidden scorer's engine.
//!
//! ```text
//! slop_ninja_a_local_score --model-dir DIR --docs DOCS.jsonl [--out OUT.jsonl]
//!     [--spec v2|v3] [--spans] [--sessions N] [--threads N] [--ort-lib PATH] [--reference FILE]
//! ```
//!
//! The model directory holds the six artifact files. It is admitted with an
//! artifact spec the hidden scorer admits by (the same checks, before ONNX Runtime
//! opens the graph): `--spec`, `v2`, `v3` or a full spec id, which should be the
//! spec your cohort names, the one the hidden scorer admits that cohort's
//! artifacts by; without it, the newest (spec v3). It is then loaded with the
//! same guards, graph contract and heads runtime as `a-scorer`. Each line of
//! `DOCS.jsonl` is `{"id", "text"}`, optionally with labels: `"involved"` (true when any AI was involved) and `"share"` (the
//! AI-written fraction, 0 to 1). Each output line is the hidden scorer's
//! per-document result, in input order: status, text digest, token count,
//! prediction and output digest (with `--spans` and a token head, sentences and
//! spans too). Output goes to `--out` (created new) or standard output.
//!
//! Standard error gets a content-free summary: counts by status, the time, and with
//! labels the mean confidence Brier score and the share error over scored documents.
//! The summary is a local diagnostic. It is not the cohort score, which uses the
//! cohort's hidden documents, its source balancing and its failure rules.
//!
//! `--reference FILE` compares each prediction with a reference prediction of the
//! same id (a JSON array or JSON lines of `{"id", "prediction"}`, such as an
//! export's FP64 reference predictions) and adds the largest differences to the
//! summary: document outputs on the 0..1 scale, attribution labels, and for
//! sentences and spans their offsets (exactly equal or not) and their share and
//! confidence.
//!
//! ONNX Runtime 1.31.1 is loaded from `--ort-lib` (or `A_ORT_LIB`). The hidden
//! scorer uses the library from the onnxruntime 1.31.1 cp312 manylinux_2_28_x86_64
//! wheel; another build can differ in the last bits, and the summary says so.
//! Nothing leaves the machine and nothing is logged beyond the summary.
use a_container_serve::engine::{Model, NEWEST_SPEC, spec_arg};
use a_container_serve::profile::{ServedProfile, sha256_file};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use slop_ninja_artifact_check::spec::Version;
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Instant;

const USAGE: &str = "usage: slop_ninja_a_local_score --model-dir DIR --docs DOCS.jsonl [--out OUT.jsonl] \
[--spec v2|v3] [--spans] [--sessions N] [--threads N] [--ort-lib PATH] [--reference FILE]";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Doc {
    id: String,
    text: String,
    #[serde(default)]
    involved: Option<bool>,
    #[serde(default)]
    share: Option<f64>,
}

struct Args {
    model: PathBuf,
    docs: PathBuf,
    out: Option<PathBuf>,
    spec: Version,
    spans: bool,
    sessions: usize,
    threads: usize,
    lib: String,
    reference: Option<PathBuf>,
}

fn args() -> Result<Args> {
    let mut a = std::env::args().skip(1);
    let (mut model, mut docs, mut out, mut spans, mut reference) = (None, None, None, false, None);
    let (mut sessions, mut threads, mut spec) = (1usize, 1usize, NEWEST_SPEC);
    let mut lib = std::env::var("A_ORT_LIB").ok().filter(|v| !v.is_empty());
    while let Some(k) = a.next() {
        let mut v = || a.next().with_context(|| format!("{k} needs a value\n{USAGE}"));
        match k.as_str() {
            "--model-dir" => model = Some(PathBuf::from(v()?)),
            "--docs" => docs = Some(PathBuf::from(v()?)),
            "--out" => out = Some(PathBuf::from(v()?)),
            "--spec" => spec = spec_arg(&v()?)?,
            "--spans" => spans = true,
            "--sessions" => sessions = v()?.parse()?,
            "--threads" => threads = v()?.parse()?,
            "--ort-lib" => lib = Some(v()?),
            "--reference" => reference = Some(PathBuf::from(v()?)),
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ => bail!("unknown argument {k}\n{USAGE}"),
        }
    }
    ensure!((1..=8).contains(&sessions) && (1..=16).contains(&threads), "--sessions 1-8, --threads 1-16");
    Ok(Args {
        model: model.context(USAGE)?,
        docs: docs.context(USAGE)?,
        out,
        spec,
        spans,
        sessions,
        threads,
        lib: lib.context("--ort-lib or A_ORT_LIB: the ONNX Runtime 1.31.1 shared library")?,
        reference,
    })
}

/// Largest absolute difference between two numbers or two arrays of numbers.
fn diff(a: &Value, b: &Value) -> Option<f64> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Some((x.as_f64()? - y.as_f64()?).abs()),
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            x.iter().zip(y).try_fold(0f64, |m, (p, q)| Some(m.max(diff(p, q)?)))
        }
        (Value::Object(x), Value::Object(y)) if x.len() == y.len() => {
            x.iter().try_fold(0f64, |m, (k, p)| Some(m.max(diff(p, y.get(k)?)?)))
        }
        _ => None,
    }
}

#[derive(Default)]
struct Comparison {
    documents: usize,
    missing: usize,
    document_max: f64,
    document_incomparable: usize,
    label_mismatches: usize,
    span_documents: usize,
    offsets_differ: BTreeMap<&'static str, usize>,
    span_max: BTreeMap<&'static str, f64>,
}

/// Compares one prediction with its reference.
fn compare(c: &mut Comparison, got: &Value, want: &Value) {
    c.documents += 1;
    let mut pairs: Vec<(&Value, &Value, f64)> = vec![
        (&got["origin"], &want["origin"], 1.0),
        (&got["ai_confidence_percent"], &want["ai_confidence_percent"], 100.0),
        (&got["ai_share_percent"], &want["ai_share_percent"], 100.0),
    ];
    for role in ["human", "ai"] {
        let (g, w) = (&got["attribution"][role], &want["attribution"][role]);
        pairs.push((&g["involvement_probability"], &w["involvement_probability"], 1.0));
        pairs.push((&g["conditional_named_scores"], &w["conditional_named_scores"], 1.0));
        if g["profile"] != w["profile"] {
            c.label_mismatches += 1;
        }
    }
    for (a, b, scale) in pairs {
        match diff(a, b) {
            Some(d) => c.document_max = c.document_max.max(d / scale),
            None => c.document_incomparable += 1,
        }
    }
    if want.get("spans").is_none() || want["spans"].is_null() {
        return;
    }
    c.span_documents += 1;
    for kind in ["sentences", "spans"] {
        let (g, w) = (got[kind].as_array(), want[kind].as_array());
        let same = match (g, w) {
            (Some(g), Some(w)) if g.len() == w.len() => g.iter().zip(w).all(|(a, b)| {
                ["start_char", "end_char", "start_byte", "end_byte", "words"].iter().all(|k| a[*k] == b[*k])
            }),
            _ => false,
        };
        if !same {
            *c.offsets_differ.entry(kind).or_default() += 1;
            continue;
        }
        for (a, b) in g.unwrap().iter().zip(w.unwrap()) {
            for k in ["ai_share_percent", "ai_confidence_percent"] {
                if let Some(d) = diff(&a[k], &b[k]) {
                    let e = c.span_max.entry(kind).or_default();
                    *e = e.max(d / 100.0);
                }
            }
        }
    }
}

/// Peak resident memory of this process (Linux `VmHWM`), when readable.
fn peak_resident_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb: u64 = status.lines().find_map(|l| l.strip_prefix("VmHWM:"))?.trim().strip_suffix("kB")?.trim().parse().ok()?;
    Some(kb * 1024)
}

fn read_reference(path: &std::path::Path) -> Result<BTreeMap<String, Value>> {
    let bytes = std::fs::read(path).context("--reference")?;
    let rows: Vec<Value> = match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Array(a)) => a,
        _ => std::str::from_utf8(&bytes)?
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?,
    };
    let mut out = BTreeMap::new();
    for r in rows {
        let id = r["id"].as_str().context("reference id")?.to_string();
        out.insert(id, r["prediction"].clone());
    }
    Ok(out)
}

fn main() -> Result<()> {
    let a = args()?;
    let profile = ServedProfile::release_candidate()?;
    profile.check_heads_runtime(&a_container_serve::HEADS_RUNTIME_SOURCES)?;
    a_container_serve::check_tower_runtime()?;
    let ort_sha = sha256_file(std::path::Path::new(&a.lib))?.0;
    let ort_pinned = profile.runtime.get("onnxruntime_library_sha256").is_some_and(|d| *d == ort_sha);

    let mut docs = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let reader = std::io::BufReader::new(std::fs::File::open(&a.docs).context("--docs")?);
    for (n, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let d: Doc = serde_json::from_str(&line).with_context(|| format!("docs line {}", n + 1))?;
        ensure!(!d.id.is_empty() && seen.insert(d.id.clone()), "docs line {}: empty or repeated id", n + 1);
        ensure!(d.share.is_none_or(|s| (0.0..=1.0).contains(&s)), "docs line {}: share outside 0..1", n + 1);
        docs.push(d);
    }
    ensure!(!docs.is_empty(), "no documents");

    let t = Instant::now();
    let graph = slop_ninja_artifact_check::spec::GRAPH_FILE;
    let header = a_container_serve::engine::header_of_dir(&a.model)?;
    let model = Model::load(&a.model, &header, a.spec, graph, a.sessions, a.threads, &a.lib)?;
    let load_s = t.elapsed().as_secs_f64();

    let t = Instant::now();
    let next = AtomicUsize::new(0);
    let results: Vec<Mutex<Value>> = docs.iter().map(|_| Mutex::new(Value::Null)).collect();
    std::thread::scope(|s| {
        for _ in 0..a.sessions.min(docs.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, SeqCst);
                    let Some(d) = docs.get(i) else { break };
                    *results[i].lock().unwrap() = model.score_with(&d.id, &d.text, a.spans);
                }
            });
        }
    });
    let score_s = t.elapsed().as_secs_f64();
    let results: Vec<Value> = results.into_iter().map(|m| m.into_inner().unwrap()).collect();

    let mut sink: Box<dyn Write> = match &a.out {
        Some(p) => Box::new(std::fs::OpenOptions::new().write(true).create_new(true).open(p).context("--out")?),
        None => Box::new(std::io::stdout().lock()),
    };
    let mut status: BTreeMap<String, usize> = BTreeMap::new();
    let (mut brier, mut brier_n, mut share_err, mut share_n) = (0.0, 0usize, 0.0, 0usize);
    for (d, r) in docs.iter().zip(&results) {
        serde_json::to_writer(&mut sink, r)?;
        sink.write_all(b"\n")?;
        let key = match r["reason"].as_str() {
            Some(reason) => format!("{}:{reason}", r["status"].as_str().unwrap_or("?")),
            None => r["status"].as_str().unwrap_or("?").to_string(),
        };
        *status.entry(key).or_default() += 1;
        if r["status"] != "ok" {
            continue;
        }
        let p = &r["prediction"];
        if let (Some(y), Some(c)) = (d.involved, p["ai_confidence_percent"].as_f64()) {
            let y = if y { 1.0 } else { 0.0 };
            brier += (c / 100.0 - y).powi(2);
            brier_n += 1;
        }
        if let (Some(y), Some(s)) = (d.share, p["ai_share_percent"].as_f64()) {
            share_err += (s / 100.0 - y).abs();
            share_n += 1;
        }
    }
    sink.flush()?;
    let comparison = match &a.reference {
        None => Value::Null,
        Some(path) => {
            let reference = read_reference(path)?;
            let mut c = Comparison::default();
            for r in &results {
                match (r["status"] == "ok", reference.get(r["id"].as_str().unwrap_or_default())) {
                    (true, Some(want)) => compare(&mut c, &r["prediction"], want),
                    _ => c.missing += 1,
                }
            }
            json!({"documents": c.documents, "not_compared": c.missing, "document_max_abs": c.document_max,
                "document_incomparable": c.document_incomparable, "label_mismatches": c.label_mismatches,
                "span_documents": c.span_documents, "offsets_differ": c.offsets_differ,
                "span_max_abs": c.span_max})
        }
    };
    let summary = json!({
        "reference": comparison,
        "documents": docs.len(),
        "status": status,
        "admission": model.load["admission"],
        "spans": a.spans && model.emits_spans(),
        "load_s": load_s,
        "score_s": score_s,
        "confidence_brier": (brier_n > 0).then(|| brier / brier_n as f64),
        "confidence_brier_documents": brier_n,
        "share_mean_absolute_error": (share_n > 0).then(|| share_err / share_n as f64),
        "share_documents": share_n,
        "onnxruntime_library_pinned": ort_pinned,
        "peak_resident_bytes": peak_resident_bytes(),
        "note": "local diagnostic over scored documents only; not the cohort score",
    });
    eprintln!("{}", serde_json::to_string_pretty(&summary)?);
    if !ort_pinned {
        eprintln!("warning: this ONNX Runtime library is not the hidden scorer's; outputs can differ in the last bits");
    }
    Ok(())
}
