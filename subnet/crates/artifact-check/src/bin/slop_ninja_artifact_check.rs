//! `slop_ninja_artifact_check`: the *A* artifact spec checks.
//!
//! ```text
//! slop_ninja_artifact_check [--spec v1|v2|v3|v4] COMMAND ...
//! slop_ninja_artifact_check --version                 print the version and every spec digest
//! slop_ninja_artifact_check spec                      print the spec record and its digests
//! slop_ninja_artifact_check check-dir DIR             check a model directory
//! slop_ninja_artifact_check check-object FILE         check an artifact object
//! slop_ninja_artifact_check graph FILE                walk one ONNX graph: its class, parameters
//! slop_ninja_artifact_check structure FILE OUT        walk one ONNX graph and write its structure stream to OUT
//! slop_ninja_artifact_check pack DIR OUT              check DIR and write its artifact object to OUT
//! slop_ninja_artifact_check files DIR OUT             check DIR and write the scorer's files layout v2 under OUT (reference packages)
//! slop_ninja_artifact_check canonical DIR OUT         re-encode DIR's JSON files in their canonical layouts into OUT
//! ```
//!
//! `--spec` chooses the spec version the checks apply (`v1` to `v4` or a full
//! spec id); without it, spec v1. A cohort names its spec by id and digest.
//! `graph` walks the file with the parameter bound of the version's class of
//! that length (the plain and span classes' bound otherwise), compares its
//! structure stream with that class's reference stream, and names the class
//! when they are equal. `structure` writes the stream itself (how a class's
//! reference stream is made from its reference export); nothing is hashed.
//!
//! Exit status: 0 on success, 2 on a refusal (which prints
//! `refused <code>: <detail>`; a path that cannot be read is refused as
//! `object`), 64 on a usage error. `--help` prints the usage and exits 0.
//! Output is content-free: digests, counts and codes only.

use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::json;
use slop_ninja_artifact_check::spec::Version;
use slop_ninja_artifact_check::{Refusal, files, fs, onnx, spec, text};

const USAGE: &str = "usage: slop_ninja_artifact_check [--spec v1|v2|v3|v4] (--version | spec | check-dir DIR | check-object FILE | graph FILE | structure FILE OUT | pack DIR OUT | files DIR OUT | canonical DIR OUT)\n\
--spec: the spec version to check (v1, v2, v3, v4 or a full spec id; default v1)\n\
exit status: 0 success; 2 refused (prints `refused <code>: <detail>`); 64 usage error";

fn version_arg(v: &str) -> Option<Version> {
    match v {
        "v1" => Some(Version::V1),
        "v2" => Some(Version::V2),
        "v3" => Some(Version::V3),
        "v4" => Some(Version::V4),
        id => Version::from_id(id),
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(64)
}

fn refused(r: Refusal) -> ExitCode {
    println!("refused {}: {r}", r.code());
    ExitCode::from(2)
}

fn print(v: serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(&v).expect("json"));
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut version = Version::V1;
    if args.first().map(String::as_str) == Some("--spec") {
        match args.get(1).and_then(|v| version_arg(v)) {
            Some(v) => version = v,
            None => return usage(),
        }
        args.drain(..2);
    }
    let arg = |i: usize| args.get(i).map(PathBuf::from);
    match (args.first().map(String::as_str), args.len()) {
        (Some("--help" | "-h" | "help"), 1) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        (Some("--version" | "-V"), 1) => {
            let specs: Vec<String> = Version::ALL
                .iter()
                .map(|v| format!("{} {}", v.id(), v.spec_sha256()))
                .collect();
            println!(
                "slop_ninja_artifact_check {} ({})",
                env!("CARGO_PKG_VERSION"),
                specs.join("; ")
            );
            ExitCode::SUCCESS
        }
        (Some("spec"), 1) => {
            print(
                json!({"spec_sha256": version.spec_sha256(), "text_profile_sha256": text::profile_sha256(),
                "spec": version.spec_json()}),
            );
            ExitCode::SUCCESS
        }
        (Some("check-dir"), 2) => match fs::check_dir_for(&arg(1).unwrap(), version) {
            Ok(r) => {
                print(serde_json::to_value(&r).expect("report"));
                ExitCode::SUCCESS
            }
            Err(e) => refused(e),
        },
        (Some("check-object"), 2) => match fs::check_object_for(&arg(1).unwrap(), version) {
            Ok(r) => {
                print(serde_json::to_value(&r).expect("report"));
                ExitCode::SUCCESS
            }
            Err(e) => refused(e),
        },
        (Some("graph"), 2) => match walk(&arg(1).unwrap(), version, false) {
            Ok((r, len, _)) => {
                let class = files::walked_class(version, len, &r).ok().map(|c| c.id);
                print(json!({"graph": r, "graph_class": class}));
                ExitCode::SUCCESS
            }
            Err(e) => refused(e),
        },
        (Some("structure"), 3) => {
            let out = arg(2).unwrap();
            match walk(&arg(1).unwrap(), version, true).and_then(|(r, _, stream)| {
                std::fs::write(&out, &stream).map_err(|e| Refusal::Object(format!("{}: {e}", out.display())))?;
                Ok(r)
            }) {
                Ok(r) => {
                    print(json!({"out": out, "structure_bytes": r.structure_bytes}));
                    ExitCode::SUCCESS
                }
                Err(e) => refused(e),
            }
        }
        (Some("pack"), 3) => match fs::pack_for(&arg(1).unwrap(), &arg(2).unwrap(), version) {
            Ok(r) => {
                print(serde_json::to_value(&r).expect("report"));
                ExitCode::SUCCESS
            }
            Err(e) => refused(e),
        },
        (Some("files"), 3) => {
            let dir = arg(1).unwrap();
            match fs::check_dir_with_header(&dir, version)
                .and_then(|(h, r)| fs::write_files(&dir, &h, &r, version, &arg(2).unwrap()).map(|p| (r, p)))
            {
                Ok((r, (p, package))) => {
                    print(
                        json!({"package_sha256": package, "manifest": p, "content_sha256": r.content_sha256}),
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => refused(e),
            }
        }
        (Some("canonical"), 3) => {
            let out = arg(2).unwrap();
            match fs::canonicalize(&arg(1).unwrap(), &out)
                .and_then(|()| fs::check_dir_for(&out, version))
            {
                Ok(r) => {
                    print(json!({"out": out, "content_sha256": r.content_sha256, "spec": r.spec}));
                    ExitCode::SUCCESS
                }
                Err(e) => refused(e),
            }
        }
        _ => usage(),
    }
}

/// Walks the graph at `path` under `version` ([`files::graph_walk`]), or,
/// with `keep`, keeping its structure stream. Returns the report, the file's
/// length and the kept stream.
fn walk(path: &std::path::Path, version: Version, keep: bool) -> Result<(onnx::GraphReport, u64, Vec<u8>), Refusal> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| Refusal::Object(e.to_string()))?;
    let len = f.metadata().map_err(|e| Refusal::Object(e.to_string()))?.len();
    let mut scan = if keep {
        let policy = version.graph_class_by_len(len).map_or(spec::GRAPH_POLICY, spec::graph_policy);
        onnx::GraphScan::keeping(len, policy)
    } else {
        files::graph_walk(version, len)
    };
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| Refusal::Object(e.to_string()))?;
        if n == 0 {
            break;
        }
        scan.update(&buf[..n])?;
    }
    let (report, stream) = scan.finish_keeping()?;
    Ok((report, len, stream))
}
