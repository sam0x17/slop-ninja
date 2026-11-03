//! The seeded rank every keyed assignment uses (`grammar_a_pools::splits`
//! re-exports it): the first eight bytes, big-endian, of
//! SHA-256(`RANK_DOMAIN` + seed + NUL + scope + NUL + key).
//!
//! Moved verbatim from `grammar_a_pools::splits`.
use crate::text;

pub const RANK_DOMAIN: &str = "slop_ninja_a_competitive_split_rank_v1\0";

/// Seeded rank of a key within a scope; the order every assignment uses.
pub fn rank(seed: &str, scope: &str, key: &str) -> u64 {
    text::h64(format!("{RANK_DOMAIN}{seed}\0{scope}\0{key}").as_bytes())
}
