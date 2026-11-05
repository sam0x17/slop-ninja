//! slop_ninja_a_adversarial: write and run the scorer's adversarial artifact set.
//!
//! ```text
//! slop_ninja_a_adversarial list
//! slop_ninja_a_adversarial write --base DIR --out DIR [--case NAME]...
//! slop_ninja_a_adversarial run --base DIR --cases DIR --docs DOCS.jsonl --ort-lib PATH [--case NAME]...
//! ```
//!
//! `write` derives each case from the admitted model directory `--base` into
//! `--out/<case>/` (`a_container_serve::adversarial`). Graph cases rewrite the
//! 601 MB graph, so write one case at a time when disk is short.
//!
//! `run` gives each case to the scorer's own code paths, under the newest spec the
//! scorer admits by (`engine::NEWEST_SPEC`), and compares the outcome with the
//! case's expectation: manifest cases go through the scorer's manifest
//! check (`limits::check_manifest`), model cases through admission and, when
//! admitted, a full load and scoring of `--docs` (`{"id", "text"}` lines) with one
//! session of one thread; the base is scored the same way first, for timing. Each
//! entry records the process's peak resident memory so far (`peak_rss_bytes`), so
//! a case run alone after the base shows its own peak against the memory model
//! at the base's graph class (`memory_bound_1_session`). It prints one
//! content-free JSON report and exits 1
//! if any case missed its expectation. Nothing leaves the host.
use a_container_serve::adversarial::{CASES, Case, Expect, case, write_case};
use a_container_serve::artifact::FilesManifest;
use a_container_serve::engine::{Model, NEWEST_SPEC, admit};
use a_container_serve::limits;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Deserialize)]
struct Doc {
    id: String,
    text: String,
}

fn code(e: &anyhow::Error) -> String {
    let s = format!("{e:#}");
    s.strip_prefix("refused ")
        .and_then(|r| r.split_once(':'))
        .map(|(c, _)| c.to_string())
        .unwrap_or_else(|| "not_an_admission_refusal".into())
}

/// The process's peak resident memory so far, in bytes.
fn peak_rss_bytes() -> Option<u64> {
    // SAFETY: getrusage writes one initialised struct for this process.
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    (rc == 0).then(|| u.ru_maxrss as u64 * 1024)
}

fn score_all(dir: &Path, lib: &str, docs: &[Doc]) -> Result<(BTreeMap<String, usize>, f64, f64)> {
    let t = Instant::now();
    let header = a_container_serve::engine::header_of_dir(dir)?;
    let model = Model::load(dir, &header, NEWEST_SPEC, slop_ninja_artifact_check::spec::GRAPH_FILE, 1, 1, lib)?;
    let load_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let mut status = BTreeMap::new();
    for d in docs {
        let r = model.score(&d.id, &d.text);
        let key = match r["reason"].as_str() {
            Some(reason) => format!("{}:{reason}", r["status"].as_str().unwrap_or("?")),
            None => r["status"].as_str().unwrap_or("?").to_string(),
        };
        *status.entry(key).or_default() += 1;
    }
    Ok((status, load_s, t.elapsed().as_secs_f64()))
}

fn run_case(c: &Case, expect: Expect, dir: &Path, lib: &str, docs: &[Doc]) -> Value {
    if c.manifest_only {
        let observed = std::fs::read(dir.join("artifact-files.json"))
            .map_err(anyhow::Error::from)
            .and_then(|b| {
                let package = FilesManifest::package_of(&b);
                FilesManifest::parse(&b, &package, NEWEST_SPEC).map_err(|e| anyhow::anyhow!("refused {}: {e}", e.code()))
            })
            .and_then(|m| limits::check_manifest(&m, NEWEST_SPEC));
        let got = match &observed {
            Ok(()) => "accepted".to_string(),
            Err(e) => code(e),
        };
        let pass = matches!(expect, Expect::Refused(want) if want == got);
        return json!({"case": c.name, "stage": "manifest", "observed": got, "pass": pass});
    }
    let t = Instant::now();
    if let Err(e) = a_container_serve::engine::header_of_dir(dir).and_then(|h| admit(dir, &h, NEWEST_SPEC)) {
        let got = code(&e);
        let pass = matches!(expect, Expect::Refused(want) if want == got);
        return json!({"case": c.name, "stage": "admission", "observed": got,
            "admission_s": t.elapsed().as_secs_f64(), "onnxruntime_opened": false, "pass": pass});
    }
    match score_all(dir, lib, docs) {
        Err(e) => {
            let detail = format!("{e:#}");
            let pass = matches!(expect, Expect::LoadFails(want) if detail.contains(want));
            json!({"case": c.name, "stage": "load", "observed": "load failed", "detail_matches": pass,
                "onnxruntime_opened": true, "pass": pass})
        }
        Ok((status, load_s, score_s)) => {
            let pass = match expect {
                Expect::Refused(_) | Expect::LoadFails(_) => false,
                Expect::EveryDocument(want) => status.len() == 1 && status.contains_key(want),
                Expect::Bounded => true,
            };
            json!({"case": c.name, "stage": "scored", "status": status, "load_s": load_s, "score_s": score_s,
                "pass": pass})
        }
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut opt: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut it = args.iter().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().with_context(|| format!("{k} needs a value"))?;
        opt.entry(k.as_str()).or_default().push(v.clone());
    }
    let one = |k: &str| opt.get(k).and_then(|v| v.first()).map(PathBuf::from).with_context(|| k.to_string());
    let chosen: Vec<&Case> = match opt.get("--case") {
        None => CASES.iter().collect(),
        Some(names) => names.iter().map(|n| case(n).with_context(|| format!("no case {n}"))).collect::<Result<_>>()?,
    };
    match args.first().map(String::as_str) {
        Some("list") => {
            for c in CASES {
                println!("{}\t{:?}\t{}", c.name, c.expect, c.what);
            }
        }
        Some("write") => {
            let (base, out) = (one("--base")?, one("--out")?);
            std::fs::create_dir_all(&out)?;
            for c in &chosen {
                write_case(&base, &out.join(c.name), c)?;
                println!("wrote {}", c.name);
            }
        }
        Some("run") => {
            let (base, cases) = (one("--base")?, one("--cases")?);
            let lib = one("--ort-lib")?.to_string_lossy().into_owned();
            let mut docs = Vec::new();
            for line in std::io::BufReader::new(std::fs::File::open(one("--docs")?)?).lines() {
                let line = line?;
                if !line.trim().is_empty() {
                    docs.push(serde_json::from_str::<Doc>(&line)?);
                }
            }
            let (status, load_s, score_s) = score_all(&base, &lib, &docs)?;
            let mut report = vec![json!({"case": "base", "stage": "scored", "status": status,
                "load_s": load_s, "score_s": score_s, "peak_rss_bytes": peak_rss_bytes()})];
            let mut all = true;
            for c in &chosen {
                let mut r = run_case(c, c.expect_for(&base)?, &cases.join(c.name), &lib, &docs);
                r["peak_rss_bytes"] = json!(peak_rss_bytes());
                all &= r["pass"] == true;
                report.push(r);
            }
            let graph = std::fs::metadata(base.join("a-fp32band.onnx"))?.len();
            let class = limits::class_of_len(graph).context("the base graph has no served class")?;
            println!("{}", serde_json::to_string_pretty(&json!({"documents": docs.len(), "cases": report,
                "graph_class": class.id, "memory_bound_1_session": limits::memory_bound(class, 1),
                "all_pass": all}))?);
            if !all {
                std::process::exit(1);
            }
        }
        _ => bail!("usage: slop_ninja_a_adversarial list | write --base DIR --out DIR [--case NAME]... | run --base DIR --cases DIR --docs FILE --ort-lib PATH [--case NAME]..."),
    }
    Ok(())
}
