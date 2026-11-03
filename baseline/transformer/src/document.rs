//! Exact document coverage. Window probabilities are diagnostics, not book scores.
use super::{hash, json_lines, read, save, text, write_new};
use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

const SCHEMA: &str = "slop_ninja_b_document_plan_v1";
const POLICY: &str = "exact_utf8_whitespace_snap_768_stride_two_views_v1";
const BRIDGE: &str = include_str!("../python/document_tokenizer.py");
const STRIDE: usize = 768;

#[derive(Subcommand)]
pub(super) enum Action {
    /// Freeze complete source bytes, two overlapping views and optional probes.
    Prepare {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        artifact: PathBuf,
        #[arg(long)]
        python: String,
        /// JSON array of {label, start, end}; offsets are source UTF-8 bytes.
        #[arg(long)]
        probes: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Report every window and uncovered range, including missing responses.
    Report {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long, required = true)]
        scores: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Reuse only exact-input, same-artifact/policy successful observations.
    Reuse {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        prior_plan: PathBuf,
        #[arg(long, required = true)]
        prior_scores: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Artifact {
    artifact_id: String,
    manifest_sha256: String,
    max_tokens: usize,
    runtime: String,
    tokenizer_packages: BTreeMap<String, String>,
    inference_runner_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Window {
    view: String,
    start: usize,
    end: usize,
    text_sha256: String,
    token_count: usize,
    unsupported_reason: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema: String,
    policy: String,
    bridge_sha256: String,
    artifact: Artifact,
    source: String,
    source_sha256: String,
    windows: Vec<Window>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Probe {
    label: String,
    start: usize,
    end: usize,
}

#[derive(Deserialize)]
struct Encoding {
    text_sha256: String,
    token_count: usize,
    offsets: Option<Vec<[usize; 2]>>,
}

struct Tokenizer {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    artifact: Artifact,
}

impl Tokenizer {
    fn start(python: &str, artifact: &Path) -> Result<Self> {
        let mut child = Command::new(python)
            .args(["-c", BRIDGE, "--artifact"])
            .arg(artifact)
            .env("HF_HUB_OFFLINE", "1")
            .env("TRANSFORMERS_OFFLINE", "1")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("TOKENIZERS_PARALLELISM", "false")
            .env("USE_TORCH", "0")
            .env("USE_TF", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take().context("No tokenizer stdin")?;
        let mut stdout = BufReader::new(child.stdout.take().context("No tokenizer stdout")?);
        let mut line = String::new();
        stdout.read_line(&mut line)?;
        let parsed = serde_json::from_str::<Artifact>(&line);
        let artifact = match parsed {
            Ok(a) => a,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e).context("Tokenizer initialization failed");
            }
        };
        Ok(Self {
            child,
            stdin,
            stdout,
            artifact,
        })
    }

    fn encode(&mut self, source: &str, offsets: bool) -> Result<Encoding> {
        serde_json::to_writer(&mut self.stdin, &json!({"text":source,"offsets":offsets}))?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        let mut line = String::new();
        self.stdout.read_line(&mut line)?;
        let result: Encoding = serde_json::from_str(&line).context("Tokenizer response failed")?;
        ensure!(
            result.text_sha256 == hash(source),
            "Tokenizer text binding differs"
        );
        Ok(result)
    }
}

impl Drop for Tokenizer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Map scalar token starts to the preceding whitespace-delimited word start.
/// The lookup is linear in document length, including long whitespace spans.
fn word_starts(source: &str, offsets: &[[usize; 2]]) -> Result<Vec<usize>> {
    let mut starts = Vec::new();
    let mut start = 0;
    let mut in_word = false;
    for (i, c) in source.char_indices() {
        if c.is_whitespace() {
            in_word = false;
            starts.push(i);
        } else {
            if !in_word {
                start = i;
            }
            starts.push(start);
            in_word = true;
        }
    }
    let mut previous = 0;
    offsets
        .iter()
        .map(|&[a, b]| {
            ensure!(
                a >= previous && a < b && b <= starts.len(),
                "Invalid tokenizer character offsets"
            );
            previous = a;
            Ok(starts[a])
        })
        .collect()
}

fn ranges(source: &str, offsets: &[[usize; 2]], shift: usize) -> Result<Vec<(usize, usize)>> {
    let starts = word_starts(source, offsets)?;
    let mut cuts = vec![0];
    for i in (if shift == 0 { STRIDE } else { shift }..starts.len()).step_by(STRIDE) {
        if starts[i] > *cuts.last().unwrap() {
            cuts.push(starts[i]);
        }
    }
    if *cuts.last().unwrap() != source.len() {
        cuts.push(source.len());
    }
    if cuts.len() == 2 {
        return Ok(vec![(0, source.len())]);
    }
    Ok(cuts.windows(3).map(|w| (w[0], w[2])).collect())
}

fn holes(length: usize, ranges: impl IntoIterator<Item = (usize, usize)>) -> Vec<(usize, usize)> {
    let mut ranges: Vec<_> = ranges.into_iter().collect();
    ranges.sort_unstable();
    let mut end = 0;
    let mut missing = Vec::new();
    for (a, b) in ranges {
        if a > end {
            missing.push((end, a));
        }
        end = end.max(b);
    }
    if end < length {
        missing.push((end, length));
    }
    missing
}

fn unsupported(source: &str, tokens: usize, limit: usize) -> Option<String> {
    if source.trim().is_empty() {
        Some("empty_input".into())
    } else if tokens > limit {
        Some(format!("unsupported_length:{tokens}>{limit}"))
    } else {
        None
    }
}

fn requests(plan: &Plan) -> BTreeMap<String, (String, usize)> {
    plan.windows
        .iter()
        .filter(|w| w.unsupported_reason.is_none())
        .map(|w| {
            (
                w.text_sha256.clone(),
                (plan.source[w.start..w.end].to_owned(), w.token_count),
            )
        })
        .collect()
}

fn write_requests(path: &Path, rows: &BTreeMap<String, (String, usize)>) -> Result<()> {
    let mut bytes = Vec::new();
    for (id, (text, _)) in rows {
        serde_json::to_writer(&mut bytes, &json!({"id":id,"text":text}))?;
        bytes.push(b'\n');
    }
    write_new(path, &bytes)
}

fn prepare(
    source: PathBuf,
    artifact: PathBuf,
    python: String,
    probes: Option<PathBuf>,
    out: PathBuf,
) -> Result<()> {
    ensure!(!out.exists(), "Output already exists");
    let source = fs::read_to_string(source)?;
    ensure!(!source.trim().is_empty(), "Empty source");
    let mut tokenizer = Tokenizer::start(&python, &artifact)?;
    let whole = tokenizer.encode(&source, true)?;
    let offsets = whole.offsets.context("Missing tokenizer offsets")?;
    ensure!(!offsets.is_empty(), "No content tokens");
    let mut spans = vec![("whole".to_string(), 0, source.len())];
    for (view, shift) in [("primary", 0), ("shifted", STRIDE / 2)] {
        for (a, b) in ranges(&source, &offsets, shift)? {
            spans.push((view.to_string(), a, b));
        }
    }
    if let Some(path) = probes {
        let probes: Vec<Probe> = serde_json::from_value(read(&path)?)?;
        let mut labels = std::collections::BTreeSet::new();
        for p in probes {
            ensure!(
                !p.label.trim().is_empty() && labels.insert(p.label.clone()),
                "Missing/duplicate probe label"
            );
            spans.push((format!("probe:{}", p.label), p.start, p.end));
        }
    }
    let mut windows = Vec::new();
    let mut counts = BTreeMap::from([(hash(&source), whole.token_count)]);
    for (view, start, end) in spans {
        ensure!(start < end, "Empty/reversed source range");
        let slice = source
            .get(start..end)
            .context("Invalid UTF-8 source range")?;
        let h = hash(slice);
        let token_count = match counts.get(&h) {
            Some(n) => *n,
            None => {
                let n = tokenizer.encode(slice, false)?.token_count;
                counts.insert(h.clone(), n);
                n
            }
        };
        windows.push(Window {
            view,
            start,
            end,
            text_sha256: h,
            token_count,
            unsupported_reason: unsupported(slice, token_count, tokenizer.artifact.max_tokens),
        });
    }
    let plan = Plan {
        schema: SCHEMA.into(),
        policy: POLICY.into(),
        bridge_sha256: hash(BRIDGE),
        artifact: tokenizer.artifact.clone(),
        source_sha256: hash(&source),
        source,
        windows,
    };
    validate(&plan)?;
    fs::create_dir_all(&out)?;
    save(&out.join("plan.json"), &plan)?;
    write_requests(&out.join("detector-requests.jsonl"), &requests(&plan))
}

fn validate(plan: &Plan) -> Result<()> {
    ensure!(
        plan.schema == SCHEMA && plan.policy == POLICY && plan.bridge_sha256 == hash(BRIDGE),
        "Unsupported document protocol"
    );
    ensure!(
        !plan.source.trim().is_empty() && plan.source_sha256 == hash(&plan.source),
        "Source binding differs"
    );
    ensure!(
        plan.artifact.max_tokens > 0 && plan.artifact.runtime == "cpu_float32_eager",
        "Unsupported runtime"
    );
    for w in &plan.windows {
        ensure!(w.start < w.end, "Empty/reversed window");
        let slice = plan
            .source
            .get(w.start..w.end)
            .context("Invalid window range")?;
        ensure!(
            w.text_sha256 == hash(slice) && w.token_count > 0,
            "Window binding differs"
        );
        ensure!(
            w.unsupported_reason == unsupported(slice, w.token_count, plan.artifact.max_tokens),
            "Inconsistent length status"
        );
        ensure!(
            ["whole", "primary", "shifted"].contains(&w.view.as_str())
                || w.view.starts_with("probe:"),
            "Unknown view"
        );
    }
    let whole: Vec<_> = plan.windows.iter().filter(|w| w.view == "whole").collect();
    ensure!(
        whole.len() == 1 && whole[0].start == 0 && whole[0].end == plan.source.len(),
        "Missing/invalid whole view"
    );
    for view in ["primary", "shifted"] {
        ensure!(
            holes(
                plan.source.len(),
                plan.windows
                    .iter()
                    .filter(|w| w.view == view)
                    .map(|w| (w.start, w.end))
            )
            .is_empty(),
            "Planned coverage gap"
        );
    }
    let mut counts = BTreeMap::new();
    for w in &plan.windows {
        if let Some(prior) = counts.insert(&w.text_sha256, w.token_count) {
            ensure!(prior == w.token_count, "Conflicting token counts");
        }
    }
    Ok(())
}

fn plan_at(path: &Path) -> Result<Plan> {
    let plan: Plan = serde_json::from_value(read(path)?)?;
    validate(&plan)?;
    Ok(plan)
}

fn scores(plan: &Plan, paths: &[PathBuf]) -> Result<BTreeMap<String, Value>> {
    let expected = requests(plan);
    let mut scores = BTreeMap::new();
    for path in paths {
        for row in json_lines(path)? {
            let id = text(&row, "id")?.to_owned();
            let (_, tokens) = expected.get(&id).context("Unexpected response text hash")?;
            ensure!(
                row["artifact_id"] == plan.artifact.artifact_id
                    && row["runtime"] == plan.artifact.runtime
                    && row["labels"] == json!(["human_only", "model_only", "mixed"]),
                "Response artifact/runtime differs"
            );
            match text(&row, "status")? {
                "ok" => {
                    let p: Vec<f64> = serde_json::from_value(row["probabilities"].clone())?;
                    let combined = row["model_or_assistance_probability"]
                        .as_f64()
                        .context("No combined probability")?;
                    ensure!(
                        p.len() == 3
                            && p.iter().all(|p| p.is_finite() && (0.0..=1.0).contains(p))
                            && (p.iter().sum::<f64>() - 1.0).abs() < 1e-6
                            && combined.is_finite()
                            && (0.0..=1.0).contains(&combined)
                            && (combined - p[1] - p[2]).abs() < 1e-6,
                        "Invalid probabilities"
                    );
                    ensure!(
                        row["token_count"].as_u64() == Some(*tokens as u64),
                        "Scored token count differs"
                    );
                }
                "unsupported_input" => {
                    ensure!(
                        row["probabilities"].is_null()
                            && row["model_or_assistance_probability"].is_null()
                            && row["reason"].is_string(),
                        "Malformed unsupported response"
                    );
                }
                _ => anyhow::bail!("Unknown response status"),
            }
            ensure!(scores.insert(id, row).is_none(), "Duplicate response");
        }
    }
    Ok(scores)
}

fn report(plan_path: PathBuf, score_paths: Vec<PathBuf>, out: PathBuf) -> Result<()> {
    let plan = plan_at(&plan_path)?;
    let scores = scores(&plan, &score_paths)?;
    let mut coverage = Vec::new();
    for view in ["primary", "shifted"] {
        let windows: Vec<_> = plan.windows.iter().filter(|w| w.view == view).collect();
        let complete: Vec<_> = windows
            .iter()
            .filter(|w| {
                scores
                    .get(&w.text_sha256)
                    .is_some_and(|r| r["status"] == "ok")
            })
            .collect();
        let gaps = holes(plan.source.len(), complete.iter().map(|w| (w.start, w.end)));
        let probabilities: Vec<_> = complete
            .iter()
            .map(|w| {
                scores[&w.text_sha256]["model_or_assistance_probability"]
                    .as_f64()
                    .unwrap()
            })
            .collect();
        coverage.push(json!({"view":view,"planned_windows":windows.len(),"scored_windows":complete.len(),
            "unscored_byte_ranges":gaps,"all_windows_scored":complete.len()==windows.len(),
            "minimum_window_probability":probabilities.iter().copied().reduce(f64::min),
            "maximum_window_probability":probabilities.iter().copied().reduce(f64::max),
            "all_windows_below_0_1":if complete.len()==windows.len(){Some(probabilities.iter().all(|p|*p<0.1))}else{None}}));
    }
    let observations: Vec<_> = plan.windows.iter().map(|w|json!({"window":w,
        "status":if w.unsupported_reason.is_some(){"unsupported_length_or_empty"}else if scores.contains_key(&w.text_sha256){text(&scores[&w.text_sha256],"status").unwrap()}else{"missing_response"},
        "observation":scores.get(&w.text_sha256)})).collect();
    save(
        &out,
        &json!({"schema":"slop_ninja_b_document_report_v1","plan_sha256":hash(fs::read(&plan_path)?),
        "source_sha256":plan.source_sha256,"source_bytes":plan.source.len(),"artifact":plan.artifact,
        "policy":plan.policy,"score_files":score_paths.iter().map(|p|Ok(hash(fs::read(p)?))).collect::<Result<Vec<_>>>()?,
        "coverage":coverage,"observations":observations,"document_probability":null,
        "interpretation":"Window probabilities are uncalibrated document diagnostics. A supported whole view is a direct A observation. Neither certifies meaning, author fit, Pangram success or book transformation quality."}),
    )
}

fn reuse(
    plan_path: PathBuf,
    prior_path: PathBuf,
    prior_scores: Vec<PathBuf>,
    out: PathBuf,
) -> Result<()> {
    ensure!(!out.exists(), "Output already exists");
    let plan = plan_at(&plan_path)?;
    let prior = plan_at(&prior_path)?;
    ensure!(
        plan.artifact == prior.artifact
            && plan.policy == prior.policy
            && plan.bridge_sha256 == prior.bridge_sha256,
        "Cache artifact/execution/tokenizer policy differs"
    );
    let observed = scores(&prior, &prior_scores)?;
    let mut pending = requests(&plan);
    let mut cached = Vec::new();
    let mut count = 0;
    for (id, row) in observed {
        if row["status"] == "ok" && pending.remove(&id).is_some() {
            serde_json::to_writer(&mut cached, &row)?;
            cached.push(b'\n');
            count += 1;
        }
    }
    fs::create_dir_all(&out)?;
    write_new(&out.join("cached-scores.jsonl"), &cached)?;
    write_requests(&out.join("detector-requests.jsonl"), &pending)?;
    save(
        &out.join("reuse.json"),
        &json!({"schema":"slop_ninja_b_document_reuse_v1",
        "plan_sha256":hash(fs::read(plan_path)?),"prior_plan_sha256":hash(fs::read(prior_path)?),
        "prior_score_files":prior_scores.iter().map(|p|Ok(hash(fs::read(p)?))).collect::<Result<Vec<_>>>()?,
        "cached_requests":count,"remaining_requests":pending.len()}),
    )
}

pub(super) fn run(action: Action) -> Result<()> {
    match action {
        Action::Prepare {
            source,
            artifact,
            python,
            probes,
            out,
        } => prepare(source, artifact, python, probes, out),
        Action::Report { plan, scores, out } => report(plan, scores, out),
        Action::Reuse {
            plan,
            prior_plan,
            prior_scores,
            out,
        } => reuse(plan, prior_plan, prior_scores, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_views_cover_every_byte_without_splitting_words() {
        let source = format!("  {}\r\n", "café e\u{301} 🙂 text\n\n".repeat(1000));
        let offsets: Vec<_> = source
            .chars()
            .enumerate()
            .filter(|(_, c)| !c.is_whitespace())
            .map(|(i, _)| [i, i + 1])
            .collect();
        for shift in [0, STRIDE / 2] {
            let windows = ranges(&source, &offsets, shift).unwrap();
            assert!(holes(source.len(), windows.clone()).is_empty());
            assert!(windows.len() > 10);
            for (a, b) in windows {
                assert!(source.get(a..b).is_some());
                for i in [a, b] {
                    assert!(
                        i == 0
                            || i == source.len()
                            || source[..i].chars().last().unwrap().is_whitespace()
                            || source[i..].chars().next().unwrap().is_whitespace()
                    );
                }
            }
        }
        assert!(word_starts(&source, &[[1, source.chars().count() + 1]]).is_err());
    }

    #[test]
    fn missing_evidence_cannot_disappear_in_overlap() {
        assert_eq!(holes(100, [(0, 30), (60, 100), (20, 40)]), vec![(40, 60)]);
        assert_eq!(holes(100, []), vec![(0, 100)]);
        assert!(holes(100, [(0, 60), (40, 100)]).is_empty());
        assert!(unsupported("text", 2048, 2048).is_none());
        assert!(unsupported("text", 2049, 2048).is_some());
        assert!(unsupported(" \n", 2, 2048).is_some());
    }
}
