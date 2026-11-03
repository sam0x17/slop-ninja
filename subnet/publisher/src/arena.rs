//! Bounded memory for the publisher's Wasm build.
//!
//! The rules (`slop_ninja.publisher.arena/v2`), following the coordinator's
//! bounded-arena design and the rtao lessons of 2026-10-31:
//!
//! 1. **Fixed linear memory.** The module's initial memory equals its maximum
//!    ([`LINEAR_MEMORY_BYTES`], set by `.cargo/config.toml` for
//!    `wasm32-unknown-unknown`; a test checks the two agree), so memory never
//!    grows at run time.
//! 2. **Refuse declared sizes first.** Every read names a bound before the
//!    body is read ([`crate::store::Store::get`]). The bounds come from the
//!    signed records already verified: the receipt index from the package's
//!    receipt count, the receipt chunks from the index's object lengths, the
//!    published sets from the manifest's binding counts, and everything else
//!    from the remaining budget.
//! 3. **Sum what is held.** One [`Budget`] per run accounts every raw object,
//!    every status body and every large decoded structure while it is held;
//!    a reservation that would exceed [`READBACK_BUDGET`] is refused before
//!    allocating. Objects are read one at a time. The receipt index is never
//!    decoded into memory: it is authenticated and walked in place
//!    ([`crate::verify::RawIndex`]).
//! 4. **Stream copies.** Bucket-to-bucket copies, artifacts included, stream
//!    inside the runtime and never enter Wasm memory.
//!
//! The accounting is checked against measurement: `tests/arena.rs` runs the
//! verification of wide synthetic cohorts under a counting allocator and
//! checks that the accounted peak covers the real heap peak, and that the
//! largest admissible receipt index fits the budget. Measured on 2026-11-01
//! (native, counting allocator): see the README.

use std::cell::Cell;

use slop_ninja_protocol::{Canonical, Digest, Encoder, OutcomeClass, ReceiptIndexEntry};

pub const ARENA_RULE: &str = "slop_ninja.publisher.arena/v2";

/// Fixed Wasm linear memory, initial and maximum alike. Must equal the
/// `--initial-memory` and `--max-memory` link arguments in
/// `.cargo/config.toml`.
pub const LINEAR_MEMORY_BYTES: u64 = 64 << 20;

/// Memory outside the readback budget: the 1 MiB stack rustc links first,
/// static data, the runtime glue's own allocations, the JSON documents a run
/// builds (status, index, leaderboard; each bounded by the roster and the
/// cohort count) and allocator fragmentation.
pub const RESERVED_BYTES: u64 = 16 << 20;

/// What verification may hold at once: raw objects and decoded records.
pub const READBACK_BUDGET: u64 = LINEAR_MEMORY_BYTES - RESERVED_BYTES;

/// Envelope and record header of a signed or unsigned protocol record:
/// the envelope's kind, version, role, length prefix, 32-byte key and 64-byte
/// signature, and the record's kind string, version and fixed fields.
pub const RECORD_OVERHEAD_BYTES: u64 = 1024;

/// Encoded length of one receipt-index entry.
pub fn receipt_entry_len() -> u64 {
    let entry = ReceiptIndexEntry {
        attempt: Digest::default(),
        outcome: OutcomeClass::Prediction,
        object_sha256: Digest::default(),
        object_len: 0,
    };
    let mut e = Encoder::new();
    entry.encode_to(&mut e);
    e.into_bytes().len() as u64
}

/// Exact upper bound on the signed receipt index of `count` receipts.
pub fn receipt_index_bound(count: u64) -> u64 {
    RECORD_OVERHEAD_BYTES.saturating_add(count.saturating_mul(receipt_entry_len()))
}

/// The verification bitmap: one bit per index entry.
pub fn receipt_bitmap_bytes(count: u64) -> u64 {
    count.div_ceil(64) * 8
}

/// Encoded length of one attempt receipt (fixed fields only).
pub fn receipt_object_len() -> u64 {
    use slop_ninja_protocol::{AttemptReceipt, Record};
    let r = AttemptReceipt {
        attempt: Digest::default(),
        assignment: Digest::default(),
        artifact: Digest::default(),
        number: 1,
        outcome: OutcomeClass::Prediction,
        result_commitment: Digest::default(),
    };
    r.to_bytes().map_or(0, |b| b.len() as u64)
}

/// The peak of verifying `count` receipts whose largest chunk object holds
/// `largest_bundle` bytes: the raw signed index (held throughout), the
/// bitmap, and one chunk object at a time. The chunk map, which names every
/// object's length, is read first, so this is known before the index is.
pub fn receipt_verification_peak(count: u64, largest_bundle: u64) -> u64 {
    receipt_index_bound(count)
        .saturating_add(receipt_bitmap_bytes(count))
        .saturating_add(largest_bundle)
}

/// The largest receipt count the budget admits when every chunk object holds
/// at most `per_bundle` receipts (the coordinator writes one object per job:
/// a job's tasks times the manifest's `max_attempts`).
pub fn max_receipts(per_bundle: u64) -> u64 {
    let bundle = per_bundle.saturating_mul(4 + receipt_object_len());
    // 8 bytes for the bitmap's rounding up to whole words.
    let room = READBACK_BUDGET.saturating_sub(RECORD_OVERHEAD_BYTES + bundle + 8);
    // 69 bytes and one bit per receipt.
    room.saturating_mul(8) / (8 * receipt_entry_len() + 1)
}

/// The receipt count of a cohort of `entrants` jobs (plus the reference
/// job), each scoring `tasks` documents with at most `max_attempts` attempts
/// each: the bound a plan must stay within (see `max_receipts`).
pub fn cohort_receipts(entrants: u64, tasks: u64, max_attempts: u64) -> u64 {
    entrants
        .saturating_add(1)
        .saturating_mul(tasks)
        .saturating_mul(max_attempts)
}

/// Bound of a stored `SubmissionUpload` envelope (fixed fields only).
pub const ADMISSION_RECORD_BOUND: u64 = RECORD_OVERHEAD_BYTES;

/// Bound of a signed authorization or renewal object (fixed fields only),
/// shared with the coordinator (`slop_ninja_admission_wire::receipts`).
pub const AUTHORIZATION_OBJECT_BOUND: u64 =
    slop_ninja_admission_wire::receipts::AUTHORIZATION_OBJECT_BOUND;
const _: () = assert!(AUTHORIZATION_OBJECT_BOUND == RECORD_OVERHEAD_BYTES);

/// Bound of the private authorization pointer: its schema, chain, kind and
/// up to three object references, each a key under `authorizations/`, a
/// 64-hex digest and a length; pretty-printed. Shared with the coordinator
/// (`slop_ninja_admission_wire::receipts`).
pub const AUTHORIZATION_POINTER_BOUND: u64 =
    slop_ninja_admission_wire::receipts::AUTHORIZATION_POINTER_BOUND;
const _: () = assert!(AUTHORIZATION_POINTER_BOUND == 4 * RECORD_OVERHEAD_BYTES);

/// What one status body may hold: the remaining budget less a doubling
/// buffer's old copy while it grows. The body is reserved while it is held
/// and parsed into typed views without an intermediate JSON tree.
pub fn status_body_bound(remaining: u64) -> u64 {
    remaining / 3
}

/// Bound of a published-contents record of `count` digests.
pub fn contents_bound(count: u64) -> u64 {
    RECORD_OVERHEAD_BYTES.saturating_add(count.saturating_mul(32))
}

/// Bound of a fingerprint-index record of `count` entries (content 32,
/// version 4, fingerprint 32).
pub fn fingerprints_bound(count: u64) -> u64 {
    RECORD_OVERHEAD_BYTES.saturating_add(count.saturating_mul(68))
}

/// Bound of `closure-fingerprints.json` with `count` entries: one pretty
/// printed object of a 64-hex hotkey, a `u32` version and a 64-hex digest is
/// under 256 bytes.
pub fn closure_fingerprints_bound(count: u64) -> u64 {
    RECORD_OVERHEAD_BYTES.saturating_add(count.saturating_mul(256))
}

/// Bound of `chunk-map.json` listing `objects` bundles under a label of
/// `label_len` bytes: each entry is a key, a 64-hex digest and two integers.
pub fn chunk_map_bound(objects: u64, label_len: u64) -> u64 {
    RECORD_OVERHEAD_BYTES.saturating_add(objects.saturating_mul(512 + label_len))
}

/// Bound of `components.json` (`slop_ninja_eval_score_components_v2`) for
/// `entrants`: a fixed part (rule texts and the gate policy) and one record
/// per entrant (hotkey, UID, score, at most one code per gate, cluster head,
/// units). A larger object is withheld, never a reason to refuse the cohort.
pub fn components_bound(entrants: u64) -> u64 {
    (64u64 << 10).saturating_add(entrants.saturating_mul(1024))
}

/// A refused reservation: nothing was allocated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused {
    pub want: u64,
    pub held: u64,
    pub limit: u64,
}

/// The run's accounting of held memory.
#[derive(Debug)]
pub struct Budget {
    limit: u64,
    held: Cell<u64>,
    peak: Cell<u64>,
}

impl Budget {
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            held: Cell::new(0),
            peak: Cell::new(0),
        }
    }

    /// The Worker's budget.
    pub fn worker() -> Self {
        Self::new(READBACK_BUDGET)
    }

    pub fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.held.get())
    }

    pub fn peak(&self) -> u64 {
        self.peak.get()
    }

    pub fn held(&self) -> u64 {
        self.held.get()
    }

    /// Reserves `n` bytes until the returned hold drops.
    pub fn reserve(&self, n: u64) -> Result<Hold<'_>, Refused> {
        let held = self.held.get();
        let next = held
            .checked_add(n)
            .filter(|&t| t <= self.limit)
            .ok_or(Refused {
                want: n,
                held,
                limit: self.limit,
            })?;
        self.held.set(next);
        self.peak.set(self.peak.get().max(next));
        Ok(Hold { budget: self, n })
    }
}

/// A reservation; released on drop.
#[derive(Debug)]
pub struct Hold<'a> {
    budget: &'a Budget,
    n: u64,
}

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        self.budget.held.set(self.budget.held.get() - self.n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservations_sum_and_release() {
        let b = Budget::new(100);
        let a = b.reserve(60).unwrap();
        assert_eq!(
            b.reserve(41).unwrap_err(),
            Refused {
                want: 41,
                held: 60,
                limit: 100
            }
        );
        let c = b.reserve(40).unwrap();
        assert_eq!(b.remaining(), 0);
        drop(a);
        drop(c);
        assert_eq!(b.held(), 0);
        assert_eq!(b.peak(), 100);
        assert!(b.reserve(u64::MAX).is_err());
    }

    #[test]
    fn receipt_bounds_are_exact_encodings() {
        assert_eq!(receipt_entry_len(), 69);
        assert!(receipt_object_len() > 0);
        for per_bundle in [0u64, 2_224 * 3, 100_000] {
            let bundle = per_bundle * (4 + receipt_object_len());
            let max = max_receipts(per_bundle);
            assert!(receipt_verification_peak(max, bundle) <= READBACK_BUDGET);
            assert!(receipt_verification_peak(max + 8, bundle) > READBACK_BUDGET);
        }
        // A roster-cap cohort at three attempts per task fits with room: 40
        // entrants and the reference job at 2,224 tasks each.
        let worst = cohort_receipts(40, 2_224, 3);
        assert!(worst <= max_receipts(2_224 * 3), "{worst}");
    }

    /// The fixed memory the Wasm build links with is the one accounted here.
    #[test]
    fn linker_memory_matches() {
        let config = include_str!("../.cargo/config.toml");
        let want = LINEAR_MEMORY_BYTES.to_string();
        assert!(
            config.contains(&format!("--initial-memory={want}")),
            "initial memory"
        );
        assert!(
            config.contains(&format!("--max-memory={want}")),
            "max memory"
        );
    }
}
