//! Execute the frozen private document comparison after public-result review.
//! Source text and execution outputs must remain outside repositories/datasets.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[path = "private_origin_transfer/capacity.rs"]
mod capacity;

const POLICY: &str = include_str!("../ORIGIN_PRIVATE_TRANSFER_V1.md");
const ROLES: [&str; 2] = ["control", "consistency"];
const VARIANTS: [&str; 3] = ["original", "repairs", "joins"];

#[derive(Parser)]
struct Args {
    #[arg(long)]
    plan: PathBuf,
    /// Hash-bound record of the assistant/human's completed public-result review.
    #[arg(long)]
    public_review: PathBuf,
    /// New directory outside repositories and datasets. Never resumes a run.
    #[arg(long)]
    out: PathBuf,
    /// Required only for capacity plans: the earlier actual public review.
    #[arg(long)]
    base_public_review: Option<PathBuf>,
    /// Required only for capacity plans: the completed base private output.
    #[arg(long)]
    base_private_output: Option<PathBuf>,
    /// Required only for capacity plans: reviewed corresponding-role comparisons.
    #[arg(long)]
    cross_size_review: Option<PathBuf>,
    /// Verify all prerequisites without invoking a tokenizer or scorer.
    #[arg(long)]
    preflight_only: bool,
}

fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn digest(p: &Path) -> Result<String> {
    Ok(sha256(fs::read(p)?))
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("Missing {key}"))
}
fn path(v: &Value, key: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(string(v, key)?))
}
fn save(p: &Path, v: &Value) -> Result<()> {
    let mut f = fs::File::create_new(p)?;
    serde_json::to_writer_pretty(&mut f, v)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn bind(map: &mut BTreeMap<PathBuf, String>, p: &Path) -> Result<()> {
    let key = p.canonicalize()?;
    let hash = digest(p)?;
    if let Some(old) = map.insert(key, hash.clone()) {
        ensure!(old == hash, "Conflicting file binding");
    }
    Ok(())
}
fn check(map: &BTreeMap<PathBuf, String>) -> Result<()> {
    for (p, h) in map {
        ensure!(digest(p)? == *h, "Changed bound file: {}", p.display());
    }
    Ok(())
}
fn manifest(p: &Path) -> Result<BTreeMap<PathBuf, String>> {
    let mut map = BTreeMap::new();
    for line in fs::read_to_string(p)?.lines() {
        let bytes = line.as_bytes();
        ensure!(
            bytes.len() > 66
                && bytes[..64].iter().all(u8::is_ascii_hexdigit)
                && bytes[64] == b' '
                && matches!(bytes[65], b' ' | b'*'),
            "Invalid input binding"
        );
        let name = p
            .parent()
            .context("Missing manifest parent")?
            .join(&line[66..]);
        ensure!(
            digest(&name)? == line[..64],
            "Private input binding changed"
        );
        bind(&mut map, &name)?;
    }
    bind(&mut map, p)?;
    Ok(map)
}
fn command(
    logs: &Path,
    id: &str,
    program: &Path,
    args: &[String],
    input: Option<&Path>,
    output: Option<&Path>,
) -> Result<()> {
    let output = output
        .map(PathBuf::from)
        .unwrap_or_else(|| logs.join(format!("{id}.stdout")));
    save(
        &logs.join(format!("{id}.command.json")),
        &json!({"program":program,"args":args,"stdin":input,"stdout":output}),
    )?;
    let mut c = Command::new(program);
    c.args(args)
        .env("HF_HUB_OFFLINE", "1")
        .env("TRANSFORMERS_OFFLINE", "1")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("TOKENIZERS_PARALLELISM", "false")
        .env("OMP_NUM_THREADS", "1")
        .env("OPENBLAS_NUM_THREADS", "1")
        .env("MKL_NUM_THREADS", "1")
        .env("VECLIB_MAXIMUM_THREADS", "1")
        .stdout(fs::File::create_new(output)?)
        .stderr(fs::File::create_new(logs.join(format!("{id}.stderr")))?);
    c.stdin(match input {
        Some(p) => Stdio::from(fs::File::open(p)?),
        None => Stdio::null(),
    });
    let status = c.status()?;
    save(
        &logs.join(format!("{id}.exit.json")),
        &json!({"success":status.success(),"exit_code":status.code()}),
    )?;
    ensure!(
        status.success(),
        "Command {id} failed; partial outputs retained"
    );
    Ok(())
}
fn arg(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}
fn complete_document(report: &Value, prepared: &Value, plan_path: &Path) -> Result<()> {
    let windows = prepared["windows"].as_array().context("Missing windows")?;
    let observations = report["observations"]
        .as_array()
        .context("Missing observations")?;
    ensure!(
        report["plan_sha256"] == digest(plan_path)?
            && report["policy"] == prepared["policy"]
            && !windows.is_empty()
            && observations.len() == windows.len()
            && observations
                .iter()
                .zip(windows)
                .all(|(r, w)| r["status"] == "ok" && r["window"] == *w)
            && report["coverage"].as_array().is_some_and(|rs| rs.len() == 2
                && rs
                    .iter()
                    .all(|r| r["all_windows_scored"] == true
                        && r["unscored_byte_ranges"] == json!([]))),
        "Incomplete candidate window coverage"
    );
    Ok(())
}
fn whole(report: &Value, hash: &str, artifact: &str, tokens: &Value) -> Result<Value> {
    ensure!(
        report["source_sha256"] == hash && report["artifact"]["artifact_id"] == artifact,
        "Document identity differs"
    );
    let rows = report["observations"]
        .as_array()
        .context("Missing observations")?;
    let selected: Vec<_> = rows
        .iter()
        .filter(|r| r["window"]["view"] == "whole")
        .collect();
    ensure!(selected.len() == 1, "Expected one whole-text observation");
    let w = selected[0];
    let s = &w["observation"];
    ensure!(
        w["status"] == "ok"
            && w["window"]["text_sha256"] == hash
            && s["id"] == hash
            && s["artifact_id"] == artifact
            && s["runtime"] == "cpu_float32_eager"
            && s["labels"] == json!(["human_only", "model_only", "mixed"])
            && s["token_count"] == *tokens,
        "Unsupported or unmatched whole-text score"
    );
    let q: [f64; 3] = serde_json::from_value(s["probabilities"].clone())?;
    let p = s["model_or_assistance_probability"]
        .as_f64()
        .context("Missing probability")?;
    ensure!(
        q.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x))
            && (q.iter().sum::<f64>() - 1.0).abs() < 1e-6
            && (p - q[1] - q[2]).abs() < 1e-6,
        "Invalid whole-text probabilities"
    );
    Ok(s.clone())
}
fn reviewed_public(
    public_plan: &Path,
    review_path: &Path,
    bindings: &mut BTreeMap<PathBuf, String>,
) -> Result<(Value, BTreeMap<PathBuf, String>)> {
    let public = read(public_plan)?;
    let public_root = path(&public, "output")?;
    let completion_path = public_root.join("completion.json");
    let completed =
        read(&completion_path).context("Public comparison must close before private execution")?;
    ensure!(
        completed["public_diagnostics_complete"] == true
            && completed["error"].is_null()
            && completed["private_comparison_pending"] == true
            && completed["reference_promoted"] == false
            && completed["provider_calls"] == 0,
        "Public comparison did not close successfully"
    );
    let review = read(review_path)?;
    let opening_path = public_root.join("opening.json");
    ensure!(
        review["schema"] == "slop_ninja_public_comparison_review_v1"
            && !string(&review, "reviewer")?.trim().is_empty()
            && review["notes"].as_array().is_some_and(|v| !v.is_empty())
            && review["public_plan_sha256"] == digest(public_plan)?
            && review["public_completion_sha256"] == digest(&completion_path)?
            && review["public_opening_sha256"] == digest(&opening_path)?,
        "Missing or stale public-result review"
    );

    let public_bindings: BTreeMap<PathBuf, String> =
        serde_json::from_value(public["bindings"].clone())?;
    check(&public_bindings)?;
    for p in public_bindings.keys() {
        bind(bindings, p)?;
    }
    let artifact_bindings: BTreeMap<PathBuf, String> =
        serde_json::from_value(read(&opening_path)?["bindings"].clone())?;
    check(&artifact_bindings)?;
    for p in artifact_bindings.keys() {
        bind(bindings, p)?;
    }
    let reviewed: BTreeMap<PathBuf, String> =
        serde_json::from_value(review["reviewed_reports"].clone())?;
    let cases = public["cases"].as_array().context("Missing public cases")?;
    let mut required = Vec::new();
    for role in ROLES {
        for c in cases {
            let name = string(c, "name")?;
            required.extend([
                public_root.join(role).join(name).join("report.json"),
                public_root.join(role).join(name).join("vs-v6.json"),
            ]);
        }
        required.extend([
            public_root.join(role).join("hosted/report.json"),
            public_root.join(role).join("grammar/report.json"),
        ]);
    }
    required.extend(
        cases
            .iter()
            .map(|c| {
                Ok(public_root.join(format!("treatment-vs-control-{}.json", string(c, "name")?)))
            })
            .collect::<Result<Vec<_>>>()?,
    );
    for p in required {
        ensure!(
            reviewed.get(&p) == Some(&digest(&p)?),
            "Public review is missing a required report"
        );
        bind(bindings, &p)?;
    }
    for p in [public_plan, review_path, &completion_path, &opening_path] {
        bind(bindings, p)?;
    }
    Ok((public, artifact_bindings))
}

fn candidate_catalog(
    public: &Value,
    artifact_bindings: &BTreeMap<PathBuf, String>,
) -> Result<BTreeMap<&'static str, Value>> {
    let fits = path(public, "fits")?;
    let public_root = path(public, "output")?;
    let mut candidates = BTreeMap::new();
    for role in ROLES {
        let artifact = fits.join(role);
        let m = read(&artifact.join("manifest.json"))?;
        let id = string(&m, "artifact_id")?;
        ensure!(
            artifact_bindings.get(&artifact.join("manifest.json"))
                == Some(&digest(&artifact.join("manifest.json"))?),
            "Candidate manifest not bound by the public opening"
        );
        for (name, hash) in m["files"].as_object().context("Missing artifact files")? {
            ensure!(
                artifact_bindings
                    .get(&artifact.join(name))
                    .map(String::as_str)
                    == Some(hash.as_str().context("Invalid artifact hash")?),
                "Artifact file not sealed in public opening"
            );
        }

        let tpath = public_root.join(role).join("calibration/thresholds.json");
        ensure!(
            artifact_bindings.contains_key(&tpath),
            "Thresholds not frozen in the public opening"
        );
        let threshold = read(&tpath)?;
        ensure!(
            threshold["content"]["artifact_id"] == id
                && threshold["content_sha256"]
                    == sha256(serde_json::to_vec(&threshold["content"])?),
            "Threshold identity changed"
        );
        let points = threshold["content"]["points"]
            .as_array()
            .context("Missing operating points")?;
        ensure!(points.len() == 2, "Expected original two operating points");
        for (point, target) in points.iter().zip([0.01, 0.05]) {
            ensure!(
                point["target_human_false_positive_rate"] == target
                    && point["comparison"] == "strictly_greater_than"
                    && point["threshold"]
                        .as_f64()
                        .is_some_and(|v| v.is_finite() && (0.0..=1.0).contains(&v)),
                "Changed operating point"
            );
        }
        candidates.insert(role, json!({"artifact":artifact,"id":id,"points":points}));
    }
    Ok(candidates)
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        !args.out.exists(),
        "Use a new private output directory; no automatic resume"
    );
    let plan = read(&args.plan)?;
    ensure!(
        plan["schema"] == "slop_ninja.private_origin_transfer_plan.v1"
            && plan["candidate_roles"] == json!(ROLES)
            && plan["source_groups"] == 1
            && plan["detector_training_admitted"] == false
            && plan["commercial_training_admitted"] == false
            && plan["auto_launch"] == false
            && plan["require_successful_public_comparison"] == true
            && plan["require_both_original_calibration_thresholds"] == true
            && plan["new_pangram_calls"] == 0,
        "Unexpected private comparison plan"
    );
    capacity::validate_arguments(&args, &plan)?;
    let root = args.plan.parent().context("Missing study directory")?;
    ensure!(
        fs::read_to_string(root.join(string(&plan, "protocol")?))? == POLICY,
        "Private protocol changed"
    );
    let public_plan = path(&plan, "public_comparison")?;
    let mut bindings = BTreeMap::new();
    let (public, artifact_bindings) =
        reviewed_public(&public_plan, &args.public_review, &mut bindings)?;
    for p in manifest(&root.join(string(&plan, "input_bindings")?))?.keys() {
        bind(&mut bindings, p)?;
    }
    ensure!(
        public["fits"] == plan["fits"],
        "Candidate roles refer to different fits"
    );
    let source_cases = plan["cases"].as_array().context("Missing private cases")?;
    ensure!(
        source_cases.len() == 3
            && source_cases
                .iter()
                .map(|c| string(c, "variant"))
                .collect::<Result<Vec<_>>>()?
                == VARIANTS,
        "Private source variants changed"
    );
    let document_tool = root.join(string(&plan, "document_tool")?);
    let python = path(&public, "python")?;
    let reference = string(&plan, "reference_artifact_id")?;
    let prior = read(&path(&plan, "existing_study")?.join("comparison.json"))?;
    let candidates = candidate_catalog(&public, &artifact_bindings)?;
    let capacity = capacity::prepare(&args, &plan, &public, &candidates, &mut bindings)?;
    for p in [
        &args.plan,
        &args.public_review,
        &public_plan,
        &document_tool,
        &std::env::current_exe()?,
    ] {
        bind(&mut bindings, p)?;
    }
    check(&bindings)?;
    fs::create_dir(&args.out)?;
    let logs = args.out.join("logs");
    fs::create_dir(&logs)?;
    save(
        &args.out.join("preflight.json"),
        &json!({"bindings":bindings,"candidate_roles":ROLES,"review_receipt_sha256":digest(&args.public_review)?,"provider_calls":0,"training_admission":false,"private_only":true,"capacity_comparison":capacity.is_some()}),
    )?;
    if args.preflight_only {
        save(
            &args.out.join("completion.json"),
            &json!({"preflight_complete":true,
            "preflight_only":true,"private_comparison_complete":false,"provider_calls":0,
            "reference_promoted":false,"error":null,"automatic_retry":false}),
        )?;
        return Ok(());
    }
    let result = (|| -> Result<()> {
        let mut rows = Vec::new();
        for role in ROLES {
            let candidate = &candidates[role];
            let artifact = path(candidate, "artifact")?;
            let id = string(candidate, "id")?;
            let role_dir = args.out.join(role);
            fs::create_dir(&role_dir)?;
            for c in source_cases {
                let name = string(c, "variant")?;
                let hash = string(c, "text_sha256")?;
                let dir = role_dir.join(name);
                let source = path(c, "source")?;
                ensure!(
                    digest(&source)? == hash,
                    "Source changed before preparation"
                );
                command(
                    &logs,
                    &format!("{role}-{name}-prepare"),
                    &document_tool,
                    &[
                        "document".into(),
                        "prepare".into(),
                        "--source".into(),
                        arg(&source),
                        "--artifact".into(),
                        arg(&artifact),
                        "--python".into(),
                        arg(&python),
                        "--out".into(),
                        arg(&dir),
                    ],
                    None,
                    None,
                )?;
                let prepared = read(&dir.join("plan.json"))?;
                let old_plan = read(&path(c, "prior_document_plan")?)?;
                for key in [
                    "schema",
                    "policy",
                    "bridge_sha256",
                    "source",
                    "source_sha256",
                    "windows",
                ] {
                    ensure!(
                        prepared[key] == old_plan[key],
                        "Document geometry or policy changed: {key}"
                    );
                }
                ensure!(
                    prepared["artifact"]["artifact_id"] == id
                        && prepared["artifact"]["max_tokens"] == 2048,
                    "Wrong candidate document artifact"
                );
                let scores = dir.join("responses.jsonl");
                command(
                    &logs,
                    &format!("{role}-{name}-score"),
                    &python,
                    &[
                        arg(&artifact.join("runner/infer.py")),
                        "--artifact".into(),
                        arg(&artifact),
                    ],
                    Some(&dir.join("detector-requests.jsonl")),
                    Some(&scores),
                )?;
                let report_path = dir.join("report.json");
                command(
                    &logs,
                    &format!("{role}-{name}-report"),
                    &document_tool,
                    &[
                        "document".into(),
                        "report".into(),
                        "--plan".into(),
                        arg(&dir.join("plan.json")),
                        "--scores".into(),
                        arg(&scores),
                        "--out".into(),
                        arg(&report_path),
                    ],
                    None,
                    None,
                )?;
                let report = read(&report_path)?;
                complete_document(&report, &prepared, &dir.join("plan.json"))?;
                let direct = whole(&report, hash, id, &c["tokens"])?;
                let baseline = whole(
                    &read(&path(c, "prior_document_report")?)?,
                    hash,
                    reference,
                    &c["tokens"],
                )?;
                let pairs = prior["comparisons"]
                    .as_array()
                    .context("Missing prior comparison")?;
                let pair: Vec<_> = pairs.iter().filter(|p| p["label"] == name).collect();
                ensure!(
                    pair.len() == 1
                        && pair[0]["a"] == baseline
                        && pair[0]["pangram"]["text_sha256"] == hash
                        && pair[0]["pangram"]["stage"] == "STAGE_SUCCESS",
                    "Prior whole/Pangram evidence differs"
                );
                let p = direct["model_or_assistance_probability"].as_f64().unwrap();
                let points = candidate["points"].as_array().unwrap();
                rows.push(json!({"role":role,"variant":name,"source_sha256":hash,"whole_observation":direct,"baseline_whole_observation":baseline,"pangram":pair[0]["pangram"],"operating_points":points,"flags":points.iter().map(|t|p>t["threshold"].as_f64().unwrap()).collect::<Vec<_>>(),"candidate_all_windows_scored":true,"direct_whole_is_not_a_window_average":true,"report_sha256":digest(&report_path)?}));
            }
        }
        for role in ROLES {
            let original=rows.iter().find(|r|r["role"]==role && r["variant"]=="original").context("Missing original")?["whole_observation"]["model_or_assistance_probability"].as_f64().context("Missing original score")?;
            for row in rows.iter_mut().filter(|r| r["role"] == role) {
                row["change_from_original_pp"] = json!(
                    100.0
                        * (row["whole_observation"]["model_or_assistance_probability"]
                            .as_f64()
                            .unwrap()
                            - original)
                );
            }
        }
        let capacity_report = capacity::comparisons(capacity.as_ref(), &mut rows)?;
        check(&bindings)?;
        save(
            &args.out.join("report.json"),
            &json!({"schema":if capacity.is_some() { "slop_ninja_private_capacity_transfer_results_v1" } else { "slop_ninja_private_origin_transfer_results_v1" },"rows":rows,"capacity_comparison":capacity_report,"source_groups":1,"source_versions":3,"candidate_roles":ROLES,"previously_inspected":true,"independent_validation":false,"training_admission":false,"provider_calls":0,"reference_promoted":false,"interpretation":"Direct whole-text scores use each candidate's frozen public operating points. Pangram content fractions remain distinct quantities. Baseline V6 has saved whole-text scores; its missing window scores are not imputed. These known private failures cannot select a new reference by themselves."}),
        )?;
        Ok(())
    })();
    save(
        &args.out.join("completion.json"),
        &json!({"private_comparison_complete":result.is_ok(),"error":result.as_ref().err().map(|e|e.to_string()),"provider_calls":0,"reference_promoted":false,"automatic_retry":false}),
    )?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_text_observation_cannot_be_replaced_by_a_low_scoring_window() -> Result<()> {
        let score = json!({"id":"source","artifact_id":"candidate","runtime":"cpu_float32_eager","labels":["human_only","model_only","mixed"],"probabilities":[0.1,0.5,0.4],"model_or_assistance_probability":0.9,"token_count":42});
        let mut report = json!({"source_sha256":"source","artifact":{"artifact_id":"candidate"},"observations":[{"window":{"view":"primary","text_sha256":"window"},"status":"ok","observation":{"model_or_assistance_probability":0.001}},{"window":{"view":"whole","text_sha256":"source"},"status":"ok","observation":score}]});
        assert_eq!(whole(&report, "source", "candidate", &json!(42))?, score);
        assert!(whole(&report, "source", "another-artifact", &json!(42)).is_err());
        assert!(whole(&report, "source", "candidate", &json!(43)).is_err());
        report["observations"].as_array_mut().unwrap().pop();
        assert!(whole(&report, "source", "candidate", &json!(42)).is_err());
        Ok(())
    }
}
