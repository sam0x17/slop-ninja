//! Operator tool for generation v2's measurement run (design section 12.3).
//! It never talks to the network: the operator's script calls the eval
//! Worker's `/eval/gen/measure` route once per line `calls` prints and
//! appends each answer's `observation` to a JSONL file, which `apply` turns
//! into the measured manifest.
//!
//! ```text
//! gen_measure ref KEY FILE                 the {key, sha256, bytes} binding of FILE (a library adds its version)
//! gen_measure check-manifest MANIFEST.json the manifest's status, models and cells (validated)
//! gen_measure check-library LIBRARY.jsonl  the library's entries per category (validated)
//! gen_measure calls MANIFEST.json LIBRARY.jsonl
//!                                          every measurement call as `model<TAB>effort<TAB>call`,
//!                                          after building each request (synthetic text only)
//! gen_measure summary OBSERVATIONS.jsonl   per cell: calls, accepted, refusals, token and latency maxima
//! gen_measure apply PLANNED.json OBSERVATIONS.jsonl VERSION OUT.json CALL_TIMEOUT_CEILING_MS
//!                                          the measured manifest (report digest = the observations file's SHA-256);
//!                                          a frontier cell slower than the ceiling (the measure route's
//!                                          `call_timeout_ms`) is marked unusable
//! ```
//!
//! Every output is content-free except the files the operator names.

use slop_ninja_gen_core::hash::sha256_hex;
use slop_ninja_gen_core::manifest::{Manifest, Status};
use slop_ninja_gen_core::measure::{self, Observation};
use slop_ninja_gen_core::vendor::Lane;
use slop_ninja_gen_core::voice::Library;
use std::collections::BTreeMap;
use std::process::exit;

fn fail(msg: &str) -> ! {
    eprintln!("gen_measure: {msg}");
    exit(2)
}

fn read(path: &str) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| fail(&format!("read {path}: {e}")))
}

fn manifest(path: &str) -> Manifest {
    Manifest::parse(&read(path)).unwrap_or_else(|e| fail(&format!("{path}: {e}")))
}

fn library(path: &str) -> Library {
    Library::parse(&read(path)).unwrap_or_else(|e| fail(&format!("{path}: {e}")))
}

fn observations(path: &str) -> (Vec<Observation>, Vec<u8>) {
    let raw = read(path);
    let text = std::str::from_utf8(&raw).unwrap_or_else(|_| fail("observations: utf-8"));
    let obs = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| serde_json::from_str::<Observation>(l).unwrap_or_else(|e| fail(&format!("observation line {}: {e}", i + 1))))
        .collect();
    (obs, raw)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["ref", key, file] => {
            let bytes = read(file);
            let mut v = serde_json::json!({"key": key, "sha256": sha256_hex(&bytes), "bytes": bytes.len()});
            if let Ok(l) = Library::parse(&bytes) {
                v["version"] = l.header.version.into();
            }
            println!("{v}");
        }
        ["check-manifest", file] => {
            let m = manifest(file);
            let cells = |lane| m.lane_models(lane).iter().map(|x| x.effort_levels.len()).sum::<usize>();
            println!(
                "{}",
                serde_json::json!({
                    "version": m.version,
                    "status": m.status,
                    "frontier_models": m.lane_models(Lane::Frontier).len(),
                    "workers_ai_models": m.lane_models(Lane::WorkersAi).len(),
                    "frontier_cells": cells(Lane::Frontier),
                    "workers_ai_cells": cells(Lane::WorkersAi),
                    "registers": m.registers.len(),
                    "measurement_calls": measure::calls(&m).len(),
                })
            );
        }
        ["check-library", file] => {
            let l = library(file);
            let mut by: BTreeMap<&str, usize> = BTreeMap::new();
            for e in &l.entries {
                *by.entry(e.category.code()).or_default() += 1;
            }
            println!("{}", serde_json::json!({"version": l.header.version, "entries": l.entries.len(), "per_category": by}));
        }
        ["calls", mfile, lfile] => {
            let m = manifest(mfile);
            let l = library(lfile);
            if m.voice_library.sha256 != sha256_hex(&read(lfile)) {
                fail("the library is not the manifest's (digest)");
            }
            for (model, effort, call) in measure::calls(&m) {
                if let Err(e) = measure::request(&m, &l, &model, effort, call) {
                    fail(&format!("{model} {} {call}: {e}", effort.code()));
                }
                println!("{model}\t{}\t{call}", effort.code());
            }
        }
        ["summary", ofile] => {
            let (obs, _) = observations(ofile);
            for ((model, effort), s) in measure::summarize(&obs) {
                println!("{}", serde_json::json!({"model": model, "effort": effort, "measured": s}));
            }
        }
        ["apply", pfile, ofile, version, out, ceiling] => {
            let ceiling: u64 = ceiling.parse().unwrap_or_else(|_| fail("call timeout ceiling"));
            let m = manifest(pfile);
            if m.status != Status::Planned {
                fail("apply reads a planned manifest");
            }
            let version: u32 = version.parse().unwrap_or_else(|_| fail("version"));
            if version <= m.version {
                fail("the measured manifest is a new version");
            }
            let (obs, raw) = observations(ofile);
            let report = sha256_hex(&raw);
            let measured = measure::apply(&m, &measure::summarize(&obs), &report, version, ceiling).unwrap_or_else(|e| fail(&e.to_string()));
            let mut bytes = serde_json::to_vec_pretty(&measured).unwrap_or_else(|e| fail(&e.to_string()));
            bytes.push(b'\n');
            std::fs::write(out, &bytes).unwrap_or_else(|e| fail(&format!("write {out}: {e}")));
            let unusable: Vec<serde_json::Value> = measured
                .models
                .iter()
                .flat_map(|x| x.unusable_reasons.iter().map(move |(e, why)| serde_json::json!({"cell": format!("{}/{e}", x.id), "why": why})))
                .collect();
            println!("{}", serde_json::json!({"report_sha256": report, "sha256": sha256_hex(&bytes), "bytes": bytes.len(), "unusable": unusable}));
        }
        _ => fail("usage: ref KEY FILE | check-manifest M | check-library L | calls M L | summary OBS | apply PLANNED OBS VERSION OUT CEILING_MS"),
    }
}
