//! Cross-target fixture check.
//!
//! `slop_ninja_protocol_check native [v1|reward-v1|quant-v1]` prints a native golden
//! report (`v1` by default). `slop_ninja_protocol_check wasm <module.wasm>`
//! executes the `wasm32-unknown-unknown` fixture module in the pure-Rust
//! `wasmi` interpreter and requires each of its reports to equal both the
//! native report and the checked-in golden file, byte for byte.

use std::process::ExitCode;

use slop_ninja_protocol::{fixtures, fixtures_quant, fixtures_reward_v1, object_sha256};
use wasmi::{Engine, Instance, Linker, Module, Store};

const GOLDEN_V1: &str = include_str!("../../protocol/fixtures/golden-v1.txt");
const GOLDEN_REWARD_V1: &str = include_str!("../../protocol/fixtures/golden-reward-v1.txt");
const GOLDEN_QUANT_V1: &str = include_str!("../../protocol/fixtures/golden-quant-v1.txt");

/// One report: its name, wasm export prefix, native builder and golden file.
struct Report {
    name: &'static str,
    export: &'static str,
    native: fn() -> slop_ninja_protocol::Result<String>,
    golden: &'static str,
}

const REPORTS: [Report; 3] = [
    Report {
        name: "v1",
        export: "slop_ninja_fixture_report",
        native: fixtures::golden_report,
        golden: GOLDEN_V1,
    },
    Report {
        name: "reward-v1",
        export: "slop_ninja_fixture_reward_v1_report",
        native: fixtures_reward_v1::golden_report,
        golden: GOLDEN_REWARD_V1,
    },
    Report {
        name: "quant-v1",
        export: "slop_ninja_fixture_quant_v1_report",
        native: fixtures_quant::golden_report,
        golden: GOLDEN_QUANT_V1,
    },
];

fn instantiate(path: &str) -> Result<(Store<()>, Instance), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
    let engine = Engine::default();
    let module = Module::new(&engine, &bytes).map_err(|e| format!("load module: {e}"))?;
    if module.imports().len() != 0 {
        return Err("fixture module must not import host functions".into());
    }
    let mut store = Store::new(&engine, ());
    let instance = Linker::<()>::new(&engine)
        .instantiate(&mut store, &module)
        .and_then(|pre| pre.start(&mut store))
        .map_err(|e| format!("instantiate: {e}"))?;
    Ok((store, instance))
}

fn wasm_report(
    store: &mut Store<()>,
    instance: &Instance,
    export: &str,
) -> Result<Vec<u8>, String> {
    let mut call = |name: String| -> Result<u32, String> {
        instance
            .get_typed_func::<(), u32>(&*store, &name)
            .and_then(|f| f.call(&mut *store, ()))
            .map_err(|e| format!("{name}: {e}"))
    };
    let len = call(format!("{export}_len"))? as usize;
    let ptr = call(format!("{export}_ptr"))? as usize;
    let memory = instance
        .get_memory(&*store, "memory")
        .ok_or("module exports no memory")?;
    memory
        .data(&*store)
        .get(ptr..ptr + len)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| "report outside linear memory".into())
}

fn describe(name: &str, bytes: &[u8]) {
    println!(
        "{name}: {} bytes, {} lines, sha256 {}",
        bytes.len(),
        bytes.iter().filter(|b| **b == b'\n').count(),
        object_sha256(bytes)
    );
}

fn native(report: &Report) -> Result<Vec<u8>, String> {
    (report.native)()
        .map(String::into_bytes)
        .map_err(|e| format!("native {} report: {e}", report.name))
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [mode] if mode == "native" => {
            print!("{}", String::from_utf8_lossy(&native(&REPORTS[0])?));
            Ok(())
        }
        [mode, name] if mode == "native" => {
            let report = REPORTS
                .iter()
                .find(|r| r.name == name)
                .ok_or_else(|| format!("unknown report {name}"))?;
            print!("{}", String::from_utf8_lossy(&native(report)?));
            Ok(())
        }
        [mode, path] if mode == "wasm" => {
            let (mut store, instance) = instantiate(path)?;
            for report in &REPORTS {
                let native = native(report)?;
                let wasm = wasm_report(&mut store, &instance, report.export)?;
                describe(&format!("{} native", report.name), &native);
                describe(&format!("{} wasm32", report.name), &wasm);
                describe(&format!("{} golden", report.name), report.golden.as_bytes());
                if wasm != native {
                    let line = native
                        .split(|b| *b == b'\n')
                        .zip(wasm.split(|b| *b == b'\n'))
                        .position(|(a, b)| a != b)
                        .map_or(0, |i| i + 1);
                    return Err(format!(
                        "{} wasm32 report differs from native at line {line}",
                        report.name
                    ));
                }
                if native != report.golden.as_bytes() {
                    return Err(format!(
                        "{} native report differs from the golden file",
                        report.name
                    ));
                }
            }
            println!("identical");
            Ok(())
        }
        _ => Err(
            "usage: slop_ninja_protocol_check native [v1|reward-v1] | wasm <module.wasm>".into(),
        ),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
