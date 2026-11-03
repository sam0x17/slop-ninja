//! Pin the third generator and run one synthetic local HTTP compatibility probe.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use reqwest::blocking::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use slop_ninja_detector::dataset::sha256;
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    python: PathBuf,
    #[arg(long, default_value = "http://127.0.0.1:18127/v1")]
    base_url: String,
}

fn file_hash(path: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    let mut file = fs::File::open(path)?;
    let mut buffer = [0_u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}
fn main() -> Result<()> {
    let args = Args::parse();
    let output = args.root.join("probe");
    ensure!(
        !output.exists(),
        "Use a fresh probe root; never repeat an uncertain POST"
    );
    let api: Value = serde_json::from_slice(&fs::read(args.root.join("mlx-model-api.json"))?)?;
    ensure!(
        api["sha"] == "d732c91ae02e90cd5d86e810fbeb9741794b4dd9"
            && api["cardData"]["license"] == "apache-2.0",
        "Unexpected model revision/license"
    );
    let mut files = BTreeMap::new();
    for item in api["siblings"]
        .as_array()
        .context("Missing file manifest")?
    {
        let name = item["rfilename"].as_str().context("File name")?;
        ensure!(
            !name.contains('/') && !name.contains('\\') && name != "." && name != "..",
            "Unexpected model path"
        );
        let path = args.root.join("model").join(name);
        ensure!(
            fs::metadata(&path)?.len() == item["size"].as_u64().context("Expected file size")?,
            "Model file size mismatch: {name}"
        );
        let digest = file_hash(&path)?;
        if item["lfs"].is_object() {
            ensure!(item["lfs"]["sha256"] == digest, "Weight hash mismatch");
        } else {
            let object = Command::new("git").arg("hash-object").arg(&path).output()?;
            ensure!(
                object.status.success()
                    && String::from_utf8(object.stdout)?.trim()
                        == item["blobId"].as_str().context("Git blob hash")?,
                "Small model file mismatch: {name}"
            );
        }
        files.insert(name.to_owned(), digest);
    }
    let license_path = args.root.join("UPSTREAM_README.md");
    let license = fs::read_to_string(&license_path)?;
    ensure!(
        license.starts_with("---\nlicense: apache-2.0\n"),
        "Expected pinned upstream license declaration"
    );
    let runtime = Command::new(&args.python).args(["-c", "import importlib.metadata as m,json,platform; print(json.dumps({'python':platform.python_version(),'packages':{p:m.version(p) for p in ['mlx-lm','mlx','mlx-metal','transformers','tokenizers']}}))"]).output()?;
    ensure!(runtime.status.success(), "Cannot read ML runtime identity");
    let runtime: Value = serde_json::from_slice(&runtime.stdout)?;
    let server_path = args
        .python
        .parent()
        .context("Python bin directory")?
        .parent()
        .context("Environment root")?
        .join("lib/python3.12/site-packages/mlx_lm/server.py");
    let server_hash = file_hash(&server_path)?;
    let spec = json!({"id":"default_model","revision":"mlx-community/Olmo-3-7B-Instruct-4bit@d732c91ae02e90cd5d86e810fbeb9741794b4dd9",
        "license":"Apache-2.0","license_url":"https://huggingface.co/allenai/Olmo-3-7B-Instruct/blob/6e5971d9eba42665f5bd5a0fcf047f299ce1dccc/README.md",
        "license_sha256":sha256(license.as_bytes()),"quantization":"MLX affine 4-bit, group size 64; checkpoint config retained",
        "runtime":format!("native MLX HTTP; {}; server_sha256={server_hash}; macOS aarch64; prompt cache disabled; decode/prompt concurrency 1", serde_json::to_string(&runtime)?),
        "temperature":0.7,"max_tokens":1200});
    fs::create_dir(&output)?;
    fs::write(
        output.join("model-spec.json"),
        serde_json::to_vec_pretty(&spec)?,
    )?;
    fs::write(
        output.join("model-files.json"),
        serde_json::to_vec_pretty(&files)?,
    )?;
    let request = json!({"model":"default_model","messages":[
        {"role":"system","content":"You are a careful prose writer and editor. Follow the requested register and return only the requested prose."},
        {"role":"user","content":"Write about 120 words explaining this hypothetical community garden to neighbors. Use direct, natural prose and preserve these facts: the garden has twelve shared beds; volunteers water on Tuesdays and Saturdays; rainwater is stored in covered tanks; visitors may walk the paths but must ask before harvesting; two beds are reserved for school projects. Do not invent fees, organizers or opening dates. Return only the paragraph."}],
        "temperature":0.0,"max_tokens":384,"stream":false});
    let payload = serde_json::to_vec(&request)?;
    fs::write(output.join("request.json"), &payload)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(180))
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let started = Instant::now();
    let response = client
        .post(format!("{}/chat/completions", args.base_url))
        .header("Content-Type", "application/json")
        .body(payload.clone())
        .send()?;
    let status = response.status();
    let mut bytes = Vec::new();
    response.take(2_000_001).read_to_end(&mut bytes)?;
    fs::write(output.join("response.json"), &bytes)?;
    ensure!(
        status.is_success() && bytes.len() <= 2_000_000,
        "Local model response failed"
    );
    let response: Value = serde_json::from_slice(&bytes)?;
    let text = response["choices"][0]["message"]["content"]
        .as_str()
        .context("Missing prose")?;
    ensure!(
        !text.trim().is_empty() && response["choices"][0]["finish_reason"] == "stop",
        "Probe did not finish normally"
    );
    let seconds = started.elapsed().as_secs_f64();
    let report = json!({"schema":"slop_ninja_olmo_generator_probe_v1","model":spec["revision"],
        "runtime":runtime,"model_files_sha256":sha256(fs::read(output.join("model-files.json"))?),
        "model_spec_sha256":sha256(fs::read(output.join("model-spec.json"))?),"request_sha256":sha256(payload),
        "response_sha256":sha256(&bytes),"http_status":status.as_u16(),"finish_reason":response["choices"][0]["finish_reason"],
        "usage":response["usage"],"response_model":response["model"],"elapsed_seconds":seconds,
        "completion_tokens_per_second_including_prompt_and_load":response["usage"]["completion_tokens"].as_f64().map(|n|n/seconds),
        "output_words":text.split_whitespace().count(),"output_sha256":sha256(text),
        "purpose":"One synthetic compatibility request. No training record, no writing-quality or throughput population claim.",
        "detector_calls":0});
    fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
