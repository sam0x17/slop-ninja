//! Builds the protocol golden reports inside a Wasm module with no imports.
//! `slop_ninja_protocol_check` runs it and compares the bytes with the native
//! reports and the checked-in golden files.

use std::sync::OnceLock;

static REPORT: OnceLock<Vec<u8>> = OnceLock::new();
static REWARD_V1_REPORT: OnceLock<Vec<u8>> = OnceLock::new();
static QUANT_V1_REPORT: OnceLock<Vec<u8>> = OnceLock::new();

fn bytes(report: slop_ninja_protocol::Result<String>) -> Vec<u8> {
    match report {
        Ok(report) => report.into_bytes(),
        Err(error) => format!("error:{}\n", error.code()).into_bytes(),
    }
}

fn report() -> &'static [u8] {
    REPORT.get_or_init(|| bytes(slop_ninja_protocol::fixtures::golden_report()))
}

fn reward_v1_report() -> &'static [u8] {
    REWARD_V1_REPORT.get_or_init(|| bytes(slop_ninja_protocol::fixtures_reward_v1::golden_report()))
}

fn quant_v1_report() -> &'static [u8] {
    QUANT_V1_REPORT.get_or_init(|| bytes(slop_ninja_protocol::fixtures_quant::golden_report()))
}

/// Address of the version-1 report in linear memory.
#[unsafe(no_mangle)]
pub extern "C" fn slop_ninja_fixture_report_ptr() -> usize {
    report().as_ptr() as usize
}

/// Length of the version-1 report in bytes.
#[unsafe(no_mangle)]
pub extern "C" fn slop_ninja_fixture_report_len() -> usize {
    report().len()
}

/// Address of the `quant-v1` report in linear memory.
#[unsafe(no_mangle)]
pub extern "C" fn slop_ninja_fixture_quant_v1_report_ptr() -> usize {
    quant_v1_report().as_ptr() as usize
}

/// Length of the `quant-v1` report in bytes.
#[unsafe(no_mangle)]
pub extern "C" fn slop_ninja_fixture_quant_v1_report_len() -> usize {
    quant_v1_report().len()
}

/// Address of the `reward-v2` report in linear memory.
#[unsafe(no_mangle)]
pub extern "C" fn slop_ninja_fixture_reward_v1_report_ptr() -> usize {
    reward_v1_report().as_ptr() as usize
}

/// Length of the `reward-v2` report in bytes.
#[unsafe(no_mangle)]
pub extern "C" fn slop_ninja_fixture_reward_v1_report_len() -> usize {
    reward_v1_report().len()
}
