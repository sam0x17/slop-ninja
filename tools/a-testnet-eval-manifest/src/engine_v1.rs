//! The cohort engine's draw and sentence code that drew `testnet-human-pool-v1`,
//! frozen byte for byte from the coordinator at commit
//! `9051a52a7b397256a8183fc850fdd3204fc49bcf` (`subnet/coordinator/src/eval/`),
//! so the v1 replay keeps testing the engine that drew the recorded cohorts while
//! the live engine changes. The record in `fixtures/v1-draws.json` names the commit
//! and both files' SHA-256, and a test fails if the frozen copies drift from them.
//! Both files are pure functions over serde and sha2. Their own unit tests are
//! compiled with them and counted separately in the README.
#![allow(dead_code)]

#[path = "engine_v1/pool.rs"]
pub mod pool;

#[path = "engine_v1/textgen.rs"]
pub mod textgen;

/// The frozen sources, for the drift test.
pub const POOL_SOURCE: &[u8] = include_bytes!("engine_v1/pool.rs");
pub const TEXTGEN_SOURCE: &[u8] = include_bytes!("engine_v1/textgen.rs");

/// Counts per kind for `n` cases under a `[human_only, mixed, model_only]` mix:
/// largest remainder, ties by kind order. Mirrors `plan::kind_counts`, which lives
/// in a module that needs the whole coordinator; the tests pin it to the engine's
/// recorded counts.
pub fn kind_counts(n: u32, mix: [u32; 3]) -> [u32; 3] {
    let total: u32 = mix.iter().sum();
    let mut out = [0u32; 3];
    let mut rem = [(0u64, 0usize); 3];
    for i in 0..3 {
        let x = u64::from(n) * u64::from(mix[i]);
        out[i] = (x / u64::from(total)) as u32;
        rem[i] = (x % u64::from(total), i);
    }
    let mut left = n - out.iter().sum::<u32>();
    rem.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (_, i) in rem {
        if left == 0 {
            break;
        }
        out[i] += 1;
        left -= 1;
    }
    out
}
