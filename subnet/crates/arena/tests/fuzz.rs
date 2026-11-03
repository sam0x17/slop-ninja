//! Randomized check of the guarantee "inside an admitted footprint no
//! allocation can fail" (design §2.6, points 1 to 3), kept from the second
//! review (`data/task-roots/testnet/workers-arena/rereview/tests/rr_fuzz.rs`),
//! where it refused an admitted block allocation in 54 of 4,000 trials before
//! block runs were served directly. The same trial runs as the wasm32
//! self-test `f2_admitted_allocations_never_fail_fuzz`. Each trial fragments
//! the pool with blocker scopes, opens a subject scope with a `Footprint`
//! control set, adds block and footprint admissions (with blockers opened in
//! between so runs are not adjacent), then makes exactly the admitted
//! allocations, all live at once, in random order, in `Mode::Strict` (an
//! overdraft is refused). A FAIL lists every trial where an admitted
//! allocation was refused. Bookkeeping uses stack arrays only.

use std::alloc::{Layout, alloc, dealloc};

use slop_ninja_arena::{
    ArenaAlloc, Closed, Footprint, MAX_PAGES, Mode, PAGE, Route, SEGMENT_OVERHEAD, Scope, model, reset_latches,
    set_mode, set_page_limit, stats,
};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

const RS: Route = Route::new(70, "rr-subject");
const RB: Route = Route::new(71, "rr-blocker");
const MAXA: usize = 1024;
const MAXB: usize = 32;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }
}

fn bytes_for_pages(pages: u32) -> usize {
    (pages as usize - 1) * PAGE - SEGMENT_OVERHEAD
}

fn restore() {
    let _ = set_page_limit(MAX_PAGES as u32);
    let full = model::pool_pages(stats().static_bytes).min(MAX_PAGES) as u32;
    let _ = set_page_limit(full);
    reset_latches();
}

fn align(rng: &mut Rng) -> usize {
    match rng.below(100) {
        0..=59 => 1 << rng.below(4),
        60..=89 => 16,
        90..=94 => 32,
        95..=97 => 64,
        _ => 4096,
    }
}

fn small(rng: &mut Rng, big: bool) -> (usize, usize) {
    let size = match rng.below(100) {
        0..=39 => rng.range(1, 64),
        40..=79 => rng.range(65, 2048),
        80..=94 => rng.range(2049, 40_000),
        _ if big => rng.range(40_001, 140_000),
        _ => rng.range(2049, 40_000),
    } as usize;
    (size, align(rng))
}

struct Plan {
    n: usize,
    size: [usize; MAXA],
    align: [usize; MAXA],
    ptr: [*mut u8; MAXA],
    /// Admission kind per allocation: 0 control, 1 block, 2 footprint.
    via: [u8; MAXA],
}

#[derive(Default, Clone, Copy, Debug)]
struct Outcome {
    skipped: bool,
    failed_at: Option<(usize, usize, usize, u8)>,
    allocated: usize,
    planned: usize,
    reserved_pages: u32,
    used_pages: u32,
    held_pages: u32,
    admissions: u32,
    not_released: bool,
}

fn trial(seed: u64, plan: &mut Plan, verbose: bool) -> Outcome {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut out = Outcome::default();
    restore();
    set_mode(Mode::Strict);
    let mut blockers: [Option<Scope>; MAXB] = [const { None }; MAXB];
    let nb = rng.below(20) as usize;
    for b in blockers.iter_mut().take(nb) {
        *b = Scope::open(RB, bytes_for_pages(rng.range(2, 14) as u32)).ok();
    }
    for b in blockers.iter_mut().take(nb) {
        if rng.below(2) == 0 {
            *b = None;
        }
    }
    plan.n = 0;
    let mut f = Footprint::ZERO;
    let n0 = rng.below(80) as usize;
    for _ in 0..n0 {
        let (s, a) = small(&mut rng, true);
        f = f.alloc(s, a);
        plan.size[plan.n] = s;
        plan.align[plan.n] = a;
        plan.via[plan.n] = 0;
        plan.n += 1;
    }
    let Ok(scope) = Scope::open_footprint(RS, f) else {
        out.skipped = true;
        return out;
    };
    if verbose {
        let st = scope.stats();
        eprintln!("  open control n0={n0} bytes={} -> id {} reserved {} held {} used {} STATE_BYTES {}", f.bytes(), scope.id(), st.reserved_pages, st.held_pages, st.used_pages, slop_ninja_arena::STATE_BYTES);
    }
    let m = rng.below(6);
    for _ in 0..m {
        if rng.below(2) == 0
            && let Some(slot) = blockers.iter_mut().find(|b| b.is_none())
        {
            *slot = Scope::open(RB, bytes_for_pages(rng.range(2, 6) as u32)).ok();
        }
        if rng.below(2) == 0 {
            let len = rng.range(1, 600_000) as usize;
            let a = align(&mut rng);
            if scope.admit_block(len, a).is_err() || plan.n >= MAXA {
                break;
            }
            if verbose {
                let st = scope.stats();
                eprintln!("  admit_block len={len} align={a} block_bytes={} -> reserved {} held {}", slop_ninja_arena::block_bytes(len, a), st.reserved_pages, st.held_pages);
            }
            plan.size[plan.n] = len;
            plan.align[plan.n] = a;
            plan.via[plan.n] = 1;
            plan.n += 1;
        } else {
            let k = rng.range(1, 120) as usize;
            if plan.n + k > MAXA {
                break;
            }
            let mut f = Footprint::ZERO;
            let start = plan.n;
            for j in 0..k {
                let (s, a) = small(&mut rng, false);
                f = f.alloc(s, a);
                plan.size[start + j] = s;
                plan.align[start + j] = a;
                plan.via[start + j] = 2;
            }
            if scope.admit_footprint(f).is_err() {
                break;
            }
            if verbose {
                let st = scope.stats();
                eprintln!("  admit_footprint k={k} bytes={} -> reserved {} held {}", f.bytes(), st.reserved_pages, st.held_pages);
            }
            plan.n += k;
        }
        out.admissions += 1;
    }
    out.planned = plan.n;
    // Random order (Fisher-Yates over an index array on the stack).
    let mut order = [0u16; MAXA];
    for (i, o) in order.iter_mut().enumerate().take(plan.n) {
        *o = i as u16;
    }
    for i in (1..plan.n).rev() {
        let j = rng.below(i as u64 + 1) as usize;
        order.swap(i, j);
    }
    for &i in order.iter().take(plan.n) {
        let i = i as usize;
        let l = Layout::from_size_align(plan.size[i], plan.align[i]).unwrap();
        let p = scope.enter(|| unsafe { alloc(l) });
        plan.ptr[i] = p;
        if verbose {
            let st = scope.stats();
            eprintln!("  alloc #{i} via {} size {} align {} -> {} page {:#x} used {} held {} overdraft {} last_failure {:?}", plan.via[i], plan.size[i], plan.align[i], if p.is_null() { "NULL" } else { "ok" }, (p as usize) / 65536, st.used_pages, st.held_pages, st.overdraft_pages, stats().last_failure.kind);
            if p.is_null() { eprintln!("  last_failure {:?}", stats().last_failure); }
        }
        if p.is_null() {
            let st = scope.stats();
            out.failed_at = Some((out.allocated, plan.size[i], plan.align[i], plan.via[i]));
            out.reserved_pages = st.reserved_pages;
            out.used_pages = st.used_pages;
            out.held_pages = st.held_pages;
            break;
        }
        out.allocated += 1;
    }
    for &i in order.iter().take(plan.n) {
        let i = i as usize;
        let p = plan.ptr[i];
        if !p.is_null() {
            unsafe { dealloc(p, Layout::from_size_align(plan.size[i], plan.align[i]).unwrap()) };
        }
        plan.ptr[i] = core::ptr::null_mut();
    }
    out.not_released = !matches!(scope.close(), Closed::Released { .. });
    drop(blockers);
    set_mode(Mode::Enforce);
    out
}

#[test]
fn rrf_admitted_allocations_never_fail() {
    let trials: u64 = std::env::var("RR_TRIALS").ok().and_then(|v| v.parse().ok()).unwrap_or(4000);
    let mut plan = Plan { n: 0, size: [0; MAXA], align: [0; MAXA], ptr: [core::ptr::null_mut(); MAXA], via: [0; MAXA] };
    let (mut ran, mut skipped, mut failed, mut not_released) = (0u64, 0u64, 0u64, 0u64);
    let mut first: [(u64, Outcome); 12] = [(0, Outcome::default()); 12];
    let mut nf = 0;
    let mut by_via = [0u64; 3];
    for seed in 1..=trials {
        let o = trial(seed, &mut plan, false);
        if o.skipped {
            skipped += 1;
            continue;
        }
        ran += 1;
        if o.not_released {
            not_released += 1;
        }
        if let Some((_, _, _, via)) = o.failed_at {
            failed += 1;
            by_via[via as usize] += 1;
            if nf < first.len() {
                first[nf] = (seed, o);
                nf += 1;
            }
        }
    }
    eprintln!("fuzz trials {trials} ran {ran} skipped {skipped} failed {failed} (by admission kind control/block/footprint {by_via:?}) not_released {not_released}");
    for (seed, o) in first.iter().take(nf) {
        eprintln!("fuzz seed {seed}: {o:?}");
    }
    assert_eq!(failed, 0, "admitted allocations were refused in {failed} of {ran} trials");
}

#[test]
fn rrf_trace_seed() {
    let Some(seed) = std::env::var("RR_SEED").ok().and_then(|v| v.parse().ok()) else { return };
    let mut plan = Plan { n: 0, size: [0; MAXA], align: [0; MAXA], ptr: [core::ptr::null_mut(); MAXA], via: [0; MAXA] };
    eprintln!("fuzz trace seed {seed}");
    let o = trial(seed, &mut plan, true);
    eprintln!("fuzz trace outcome {o:?}");
}
