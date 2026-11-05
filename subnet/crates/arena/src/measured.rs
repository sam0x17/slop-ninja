//! Measured footprints of work more than one Worker does, so every Worker
//! that does it admits it from the same measurements.

use crate::PAGE;

/// Measured peaks of parsing recorded V15 runtime metadata with rtao-core's
/// `Runtime::parse` (the coordinator's `chain::ChainRuntime::parse` and the
/// upload Worker's `registry::parse_runtime` both call it), as (metadata
/// bytes, peak scope bytes, state page included), native 64-bit, printed by
/// the coordinator's `arena_witness_home_caches_fit`:
///
/// - the testnet runtime-486 fixture (`fixtures/runtime-486`), 34 pages,
///   2026-11-01;
/// - the runtime-487 build of subtensor PR #3294 (`fixtures/runtime-487`),
///   35 pages, 2026-11-04.
///
/// A parse's need is linear in the metadata length at the steepest measured
/// ratio ([`runtime_parse_bytes`]), so no recorded runtime parses above its
/// own scaled bound; the coordinator's witness checks that for every entry,
/// and a newly recorded runtime is added here with its measurement.
pub const RUNTIME_PARSE_MEASUREMENTS: [(usize, usize); 2] = [(411_830, 34 * PAGE), (435_714, 35 * PAGE)];

/// The measurement with the largest peak per metadata byte.
const fn steepest_parse_measurement() -> (usize, usize) {
    // Starts at a zero ratio, so the first measurement replaces it.
    let mut best = (1, 0);
    let mut rest: &[(usize, usize)] = &RUNTIME_PARSE_MEASUREMENTS;
    while let [(len, peak), tail @ ..] = rest {
        if *peak as u128 * best.0 as u128 > best.1 as u128 * *len as u128 {
            best = (*len, *peak);
        }
        rest = tail;
    }
    best
}
const _: () = assert!(steepest_parse_measurement().1 > 0, "at least one recorded parse measurement");

/// The bytes a parse of `metadata_len` bytes of metadata needs: the steepest
/// measured parse peak per metadata byte, scaled to the length.
pub const fn runtime_parse_bytes(metadata_len: usize) -> usize {
    let (len, peak) = steepest_parse_measurement();
    let scaled = (peak as u128 * metadata_len as u128).div_ceil(len as u128);
    if scaled > usize::MAX as u128 { usize::MAX } else { scaled as usize }
}

/// The longest recorded metadata in [`RUNTIME_PARSE_MEASUREMENTS`].
pub const fn largest_recorded_metadata() -> usize {
    let mut most = 0;
    let mut rest: &[(usize, usize)] = &RUNTIME_PARSE_MEASUREMENTS;
    while let [(len, _), tail @ ..] = rest {
        if *len > most {
            most = *len;
        }
        rest = tail;
    }
    most
}
