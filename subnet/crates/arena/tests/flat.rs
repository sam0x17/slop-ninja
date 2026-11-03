//! Property: repeated scopes leave memory flat.
//!
//! For any workload of allocations, frees, growth and admitted blocks, run
//! in fresh scopes again and again (alone or two interleaved), every close
//! releases the scope, pool pages and claims return to idle, nothing is
//! quarantined, and the pool high-water and memory size stop growing after
//! the first repetition. Run with one test thread.
//!
//! The proptest runner's own data (generated cases, shrinking) lives in a
//! harness scope: the glue partition is sized for platform glue only.

use proptest::prelude::*;
use proptest::test_runner::TestRunner;
use slop_ninja_arena::io::{ExactBuf, Kinds};
use slop_ninja_arena::{ArenaAlloc, Closed, Route, Scope, Stats, stats};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

const R: Route = Route::new(10, "flat-a");
const R2: Route = Route::new(11, "flat-b");
const HARNESS: Route = Route::new(12, "flat-harness");

/// Runs a property inside a harness scope that holds the runner's data.
fn in_harness(f: impl FnOnce()) {
    let h = Scope::open(HARNESS, 8 << 20).expect("harness scope");
    h.enter(f);
    drop(h);
}

#[derive(Clone, Debug)]
enum Op {
    Alloc(usize),
    Free(usize),
    Grow(usize, usize),
    Block(usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (1usize..=64 * 1024).prop_map(Op::Alloc),
        3 => any::<usize>().prop_map(Op::Free),
        2 => (any::<usize>(), 1usize..=256 * 1024).prop_map(|(i, n)| Op::Grow(i, n)),
        1 => (1usize..=2 << 20).prop_map(Op::Block),
    ]
}

/// Live state of one workload inside its scope.
#[derive(Default)]
struct Held {
    vecs: Vec<Vec<u8>>,
    blocks: Vec<ExactBuf>,
}

/// Applies one op. Every byte it may allocate is reserved first, so a
/// workload never overdraws; an op whose reservation is refused is skipped.
fn apply(s: &Scope, held: &mut Held, op: &Op) {
    match *op {
        Op::Alloc(n) => {
            // A refused reservation is a legitimate outcome: skip the op. A
            // block admits one allocation of exactly its layout.
            if s.reserve(n).is_err() {
                return;
            }
            s.enter(|| {
                if held.vecs.len() == held.vecs.capacity() {
                    held.vecs.reserve(held.vecs.len().max(4));
                }
                held.vecs.push(vec![1u8; n]);
            });
        }
        Op::Free(i) => {
            if !held.vecs.is_empty() {
                let i = i % held.vecs.len();
                s.enter(|| drop(held.vecs.swap_remove(i)));
            }
        }
        Op::Grow(i, n) => {
            if !held.vecs.is_empty() {
                let i = i % held.vecs.len();
                // Old and new buffers are both live during the move: the old
                // one keeps its run, the new one is admitted at exactly its
                // size (`reserve_exact` grows the capacity to len + n).
                if s.reserve(held.vecs[i].len() + n).is_err() {
                    return;
                }
                s.enter(|| {
                    held.vecs[i].reserve_exact(n);
                    held.vecs[i].extend(std::iter::repeat_n(2u8, n));
                });
            }
        }
        Op::Block(n) => {
            if let Ok(b) = ExactBuf::admit(s, n, Kinds::ENCODER) {
                s.enter(|| held.blocks.push(b));
            }
        }
    }
}

fn control(ops: &[Op]) -> usize {
    // The `vecs`/`blocks` headers: at most one per op, doubled while growing.
    2 * ops.len() * (size_of::<Vec<u8>>() + size_of::<ExactBuf>()) + 1024
}

fn settle(base: &Stats, settled: &mut Option<(u32, usize)>, rep: usize) -> Result<(), TestCaseError> {
    let now = stats();
    prop_assert_eq!(now.in_use_pages, base.in_use_pages, "pages back to idle (rep {})", rep);
    prop_assert_eq!(now.free_pages, base.free_pages, "holds back to idle (rep {})", rep);
    // Against the baseline: a harness scope may hold proptest's lazily created
    // runner state (quarantined when it closes), never a workload's.
    prop_assert_eq!(now.quarantined_scopes, base.quarantined_scopes);
    prop_assert_eq!(now.open_scopes, base.open_scopes);
    match *settled {
        None => *settled = Some((now.high_water_pages, now.memory_bytes)),
        Some((hw, mem)) => {
            prop_assert_eq!(now.high_water_pages, hw, "high-water flat (rep {})", rep);
            prop_assert_eq!(now.memory_bytes, mem, "memory flat (rep {})", rep);
        }
    }
    Ok(())
}

#[test]
fn repeated_scopes_leave_memory_flat() {
    in_harness(|| {
        let mut runner = TestRunner::new(ProptestConfig { cases: 48, ..ProptestConfig::default() });
        let strategy = (prop::collection::vec(op(), 1..120), 2usize..10);
        let result = runner.run(&strategy, |(ops, reps)| {
            let base = stats();
            let mut settled = None;
            for rep in 0..reps {
                let s = Scope::open(R, control(&ops)).unwrap();
                let mut held = Held::default();
                for op in &ops {
                    apply(&s, &mut held, op);
                }
                s.enter(|| drop(held));
                let st = s.stats();
                prop_assert_eq!(st.overdraft_pages, 0, "the workload stays within its reservation");
                prop_assert!(st.peak_pages <= st.reserved_pages, "peak {} reserved {}", st.peak_pages, st.reserved_pages);
                let closed = s.close();
                prop_assert!(matches!(closed, Closed::Released { .. }), "closed: {:?}", closed);
                settle(&base, &mut settled, rep)?;
            }
            Ok(())
        });
        if let Err(e) = result {
            panic!("{e}");
        }
    });
}

#[test]
fn interleaved_scopes_leave_memory_flat() {
    in_harness(|| {
        let mut runner = TestRunner::new(ProptestConfig { cases: 48, ..ProptestConfig::default() });
        let strategy = (prop::collection::vec(op(), 1..60), prop::collection::vec(op(), 1..60), any::<bool>(), 2usize..8);
        let result = runner.run(&strategy, |(a, b, b_first, reps)| {
            let base = stats();
            let mut settled = None;
            for rep in 0..reps {
                let sa = Scope::open(R, control(&a)).unwrap();
                let sb = Scope::open(R2, control(&b)).unwrap();
                let (mut ha, mut hb) = (Held::default(), Held::default());
                for k in 0..a.len().max(b.len()) {
                    if let Some(op) = a.get(k) {
                        apply(&sa, &mut ha, op);
                    }
                    if let Some(op) = b.get(k) {
                        apply(&sb, &mut hb, op);
                    }
                }
                // Frees route to the owner whichever scope is current.
                sb.enter(|| drop(ha));
                sa.enter(|| drop(hb));
                let (first, second) = if b_first { (sb, sa) } else { (sa, sb) };
                let (c1, c2) = (first.close(), second.close());
                prop_assert!(matches!(c1, Closed::Released { .. }), "first: {:?}", c1);
                prop_assert!(matches!(c2, Closed::Released { .. }), "second: {:?}", c2);
                settle(&base, &mut settled, rep)?;
            }
            Ok(())
        });
        if let Err(e) = result {
            panic!("{e}");
        }
    });
}
