//! Numeric starter A comparison and the full-A heads runtime. This crate never
//! generates prose; `full_a_spans` only segments exact request text into
//! whitespace words and sealed-splitter sentences for span offsets.
pub mod comp_diagnostics;
pub mod comp_eval;
pub mod contract;
pub mod full_a;
pub mod full_a_spans;
pub mod full_a_tower;
pub mod full_a_v2;
pub mod involvement_calibration;
pub mod mining;
pub mod model;
pub mod pangram_map;
pub mod pangram_store;
pub mod pangram_subsample;
pub mod r2_eval;
pub mod shared;
pub mod tierc;
pub mod warm_shared;

pub use contract::*;
pub use model::*;

use anyhow::Result;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Hash deterministic serde JSON bytes, including the supplied domain.
pub fn digest<T: Serialize>(domain: &str, value: &T) -> Result<String> {
    let mut h = Sha256::new();
    h.update(domain.as_bytes());
    h.update([0]);
    h.update(serde_json::to_vec(value)?);
    Ok(hex::encode(h.finalize()))
}
