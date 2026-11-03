//! Library side of `a-container-serve`: the release full-A heads runtime, the
//! tracked served profile and the engine the batch and local scorers share.
//!
//! The heads, span emission, calibration and shared modules compile the tracked
//! `grammar-a-baseline` sources directly. Their bytes are embedded as well, so
//! the servers can refuse to start when they differ from the digests the served
//! profile records. `a-container-serve` serves R0 (no token head), so it serves
//! no span. The scorer engine also compiles the span tower runtime
//! (`full_a_tower.rs`), which reads heads v3 and passes heads v1 and v2 to the
//! pinned runtime unchanged; [`TOWER_RUNTIME_SHA256`] pins it separately, so the
//! served profile's four pins are unchanged.
#![allow(dead_code, clippy::all)]

#[path = "../../../grammar/crates/grammar-a-baseline/src/full_a.rs"]
pub mod full_a;
#[path = "../../../grammar/crates/grammar-a-baseline/src/full_a_spans.rs"]
pub mod full_a_spans;
#[path = "../../../grammar/crates/grammar-a-baseline/src/full_a_tower.rs"]
pub mod full_a_tower;
#[path = "../../../grammar/crates/grammar-a-baseline/src/involvement_calibration.rs"]
pub mod involvement_calibration;
#[path = "../../../grammar/crates/grammar-a-baseline/src/shared.rs"]
pub mod shared;

pub mod adversarial;
pub mod artifact;
pub mod engine;
pub mod instance;
pub mod limits;
pub mod load_guard;
pub mod profile;

pub const FULL_A_RS: &[u8] = include_bytes!("../../../grammar/crates/grammar-a-baseline/src/full_a.rs");
pub const FULL_A_SPANS_RS: &[u8] = include_bytes!("../../../grammar/crates/grammar-a-baseline/src/full_a_spans.rs");
pub const CALIBRATION_RS: &[u8] =
    include_bytes!("../../../grammar/crates/grammar-a-baseline/src/involvement_calibration.rs");
pub const SHARED_RS: &[u8] = include_bytes!("../../../grammar/crates/grammar-a-baseline/src/shared.rs");

/// The compiled heads runtime sources, by the file names the served profile uses.
pub const HEADS_RUNTIME_SOURCES: [(&str, &[u8]); 4] = [
    ("full_a.rs", FULL_A_RS),
    ("full_a_spans.rs", FULL_A_SPANS_RS),
    ("involvement_calibration.rs", CALIBRATION_RS),
    ("shared.rs", SHARED_RS),
];

/// The span tower runtime the scorer engine compiles (heads v3).
pub const TOWER_RS: &[u8] = include_bytes!("../../../grammar/crates/grammar-a-baseline/src/full_a_tower.rs");
/// SHA-256 of [`TOWER_RS`]: the spans-S2 tower runtime as landed on main. A
/// scorer refuses to start on another source (`a_scorer.rs`).
pub const TOWER_RUNTIME_SHA256: &str = "bdb83f4ae65b06272572075865fc559f75f13081d019d3a63878ba67057434b1";

/// Refuse a compiled span tower runtime that differs from [`TOWER_RUNTIME_SHA256`].
pub fn check_tower_runtime() -> anyhow::Result<()> {
    anyhow::ensure!(
        profile::sha256_hex(TOWER_RS) == TOWER_RUNTIME_SHA256,
        "full_a_tower.rs differs from the scorer's span tower runtime pin"
    );
    Ok(())
}
