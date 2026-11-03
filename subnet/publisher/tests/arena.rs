//! Measures the real heap peak of verifying a wide cohort under a counting
//! allocator, and checks that the run's accounting covers it: the budget's
//! accounted peak plus a fixed allowance for the decoded records and the
//! outcome recomputation is at least the measured peak. This is the
//! measurement behind `arena::receipt_verification_peak`,
//! `arena::RESERVED_BYTES` and `arena::max_receipts`.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use slop_ninja_publisher::arena::{self, Budget};
use slop_ninja_publisher::store::block_on;
use slop_ninja_publisher::verify::{self, Options};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            if new_size >= layout.size() {
                let live = LIVE.fetch_add(new_size - layout.size(), Ordering::Relaxed) + new_size
                    - layout.size();
                PEAK.fetch_max(live, Ordering::Relaxed);
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        p
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Records and recomputation outside the per-object accounting: the decoded
/// manifest, package, closure and published indexes of a cohort this wide,
/// and the outcome evaluation's working set. Measured below; part of
/// `arena::RESERVED_BYTES`.
const UNACCOUNTED_ALLOWANCE: usize = 2 << 20;

fn measure(receipts: u64) -> (usize, u64) {
    let b = build(receipts);
    let x = buckets(&b);
    let cfg = config(Some(0), false);
    let opts = Options {
        keys: &cfg.keys,
        chain: &cfg.chain,
        receipts: true,
    };
    let budget = Budget::worker();
    let label = b.label.clone();
    drop(b);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let v = block_on(verify::verify_cohort(
        &x.receipts,
        &dir(&label),
        &label,
        &opts,
        &budget,
    ))
    .unwrap();
    assert_eq!(v.receipts, receipts);
    let peak = PEAK.load(Ordering::Relaxed) - before;
    drop(v);
    (peak, budget.peak())
}

#[test]
fn accounting_covers_the_measured_peak() {
    // One test so the global counters see one measurement at a time.
    for receipts in [10_000u64, 100_000] {
        let (real, accounted) = measure(receipts);
        eprintln!(
            "receipts {receipts}: measured heap peak {real} B, accounted {accounted} B, {:.1} B/receipt measured",
            real as f64 / receipts as f64
        );
        assert!(
            real <= accounted as usize + UNACCOUNTED_ALLOWANCE,
            "measured {real} exceeds accounted {accounted} plus the allowance"
        );
    }
    // The largest admissible index fits the budget, with the reserve left for
    // everything else, at the coordinator's per-job chunking.
    let per_bundle = 2_224 * 3;
    let bundle = per_bundle * (4 + arena::receipt_object_len());
    let max = arena::max_receipts(per_bundle);
    assert!(arena::receipt_verification_peak(max, bundle) <= arena::READBACK_BUDGET);
    assert!(UNACCOUNTED_ALLOWANCE as u64 <= arena::RESERVED_BYTES / 4);
    eprintln!(
        "max receipts admitted at {per_bundle} receipts per chunk object: {max}; \
         receipt object {} B",
        arena::receipt_object_len()
    );
}
