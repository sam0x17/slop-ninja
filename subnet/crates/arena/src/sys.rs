//! Platform memory: wasm32 linear memory or one native region.
//!
//! Both back the same page pool. On wasm32 the pool begins at `__heap_base`
//! (rounded up to a page), the end of the module's static data and stack.
//! Pages the module's initial memory already holds above it are committed
//! from the start: a Worker linked with a fixed linear memory (`initial =
//! maximum`, sized from the build's memory model, `model::LINEAR_MEMORY_BYTES`)
//! never grows at run time. A module linked without one grows with
//! `memory.grow`, which only this crate calls. Natively the pool is one
//! `System` allocation of the ceiling's size, made once; the OS commits its
//! pages lazily.

use core::cell::UnsafeCell;

use crate::pool::PAGE;

/// A cell that is only touched under the pool lock (native) or on the single
/// wasm thread.
pub(crate) struct Racy<T>(UnsafeCell<T>);

// SAFETY: every access happens with `lock::hold()` held natively; wasm32
// without atomics is single-threaded (enforced by the crate's compile_error).
unsafe impl<T> Sync for Racy<T> {}

impl<T> Racy<T> {
    pub(crate) const fn new(v: T) -> Self {
        Self(UnsafeCell::new(v))
    }
    pub(crate) fn get(&self) -> *mut T {
        self.0.get()
    }
}

/// Aborts without allocating.
#[cold]
pub(crate) fn abort() -> ! {
    #[cfg(target_arch = "wasm32")]
    {
        core::arch::wasm32::unreachable()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::process::abort()
    }
}

pub(crate) struct Region {
    pub base: *mut u8,
    pub static_bytes: usize,
    /// Pages the platform can back without another grow (native: the whole
    /// region; wasm: the pool pages the initial memory holds above static
    /// data, all of them under a fixed linear memory).
    pub committed: u32,
}

#[cfg(target_arch = "wasm32")]
unsafe extern "C" {
    /// The end of static data and the stack (rust-lld synthesises it).
    static __heap_base: u8;
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn init(pages: impl Fn(usize) -> usize) -> Option<(Region, usize)> {
    let heap_base = &raw const __heap_base as usize;
    let static_bytes = heap_base.checked_next_multiple_of(PAGE)?;
    let static_pages = static_bytes / PAGE;
    let n = pages(static_bytes);
    // A fixed linear memory (`initial = maximum`) already holds every pool
    // page: nothing is grown at run time. Otherwise only the pages the
    // initial memory happens to hold above static data are committed.
    let present = core::arch::wasm32::memory_size(0).saturating_sub(static_pages);
    let committed = u32::try_from(present.min(n)).ok()?;
    let base = core::ptr::with_exposed_provenance_mut::<u8>(static_bytes);
    Some((Region { base, static_bytes, committed }, n))
}

/// Grows linear memory so that pool pages `[0, upto)` are backed.
#[cfg(target_arch = "wasm32")]
pub(crate) fn commit(r: &mut Region, upto: u32) -> bool {
    if upto <= r.committed {
        return true;
    }
    let delta = (upto - r.committed) as usize;
    let prev = core::arch::wasm32::memory_grow(0, delta);
    if prev == usize::MAX {
        return false;
    }
    // This crate is the only caller of memory.grow; anything else would break
    // the page arithmetic. (Pages beyond the pool that the initial memory
    // held are never handed out, so growth always starts at the pool's end.)
    if prev * PAGE < r.static_bytes + r.committed as usize * PAGE {
        abort();
    }
    r.committed = upto;
    true
}

/// Current linear memory size (`memory.size`).
#[cfg(target_arch = "wasm32")]
pub(crate) fn memory_bytes(_r: &Region, _high_water: u32) -> usize {
    core::arch::wasm32::memory_size(0) * PAGE
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn init(pages: impl Fn(usize) -> usize) -> Option<(Region, usize)> {
    use std::alloc::{GlobalAlloc, Layout, System};
    let n = pages(0);
    let layout = Layout::from_size_align(n.checked_mul(PAGE)?, PAGE).ok()?;
    // SAFETY: non-zero size; the region is never freed.
    let base = unsafe { System.alloc(layout) };
    if base.is_null() {
        return None;
    }
    Some((Region { base, static_bytes: 0, committed: n as u32 }, n))
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn commit(r: &mut Region, upto: u32) -> bool {
    upto <= r.committed
}

/// Native analogue of `memory.size`: the region up to its high-water page.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn memory_bytes(r: &Region, high_water: u32) -> usize {
    r.static_bytes + high_water as usize * PAGE
}

pub(crate) mod lock {
    #[cfg(not(target_arch = "wasm32"))]
    use core::sync::atomic::{AtomicBool, Ordering};

    #[cfg(not(target_arch = "wasm32"))]
    static LOCK: AtomicBool = AtomicBool::new(false);

    pub(crate) struct Held(());

    /// Takes the pool lock. Never allocates; code under it never panics.
    #[inline]
    pub(crate) fn hold() -> Held {
        #[cfg(not(target_arch = "wasm32"))]
        while LOCK.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        Held(())
    }

    impl Drop for Held {
        #[inline]
        fn drop(&mut self) {
            #[cfg(not(target_arch = "wasm32"))]
            LOCK.store(false, Ordering::Release);
        }
    }
}

/// The arena allocations go to, as `(scope id, epoch)`: per thread natively,
/// global on wasm32. The epoch makes a stale handle detectable: the
/// allocator serves an allocation only when the id still names a live scope
/// of that epoch (design §2.3).
pub(crate) mod current {
    use crate::pool::NONE;

    pub(crate) type Cur = (u16, u64);

    #[cfg(not(target_arch = "wasm32"))]
    // scan-ok: const-initialised `Cell`s without destructors (no heap, no
    // destructor registration), usable from the allocator itself.
    std::thread_local! {
        // Const-initialised `Cell`s without destructors: no allocation and no
        // destructor registration, so they are usable from the allocator.
        static CURRENT: core::cell::Cell<Cur> = const { core::cell::Cell::new((NONE, 0)) };
        static ARMED: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
        static VIOLATIONS: core::cell::Cell<[u32; 3]> = const { core::cell::Cell::new([0; 3]) };
    }

    #[cfg(target_arch = "wasm32")]
    static CURRENT: super::Racy<Cur> = super::Racy::new((NONE, 0));
    #[cfg(target_arch = "wasm32")]
    static ARMED: super::Racy<bool> = super::Racy::new(false);
    #[cfg(target_arch = "wasm32")]
    static VIOLATIONS: super::Racy<[u32; 3]> = super::Racy::new([0; 3]);

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn get() -> Cur {
        CURRENT.try_with(|c| c.get()).unwrap_or((NONE, 0))
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn set(v: Cur) -> Cur {
        CURRENT.try_with(|c| c.replace(v)).unwrap_or((NONE, 0))
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn armed() -> bool {
        ARMED.try_with(|c| c.get()).unwrap_or(false)
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn set_armed(v: bool) -> bool {
        ARMED.try_with(|c| c.replace(v)).unwrap_or(false)
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn violation(k: usize) {
        let _ = VIOLATIONS.try_with(|c| {
            let mut v = c.get();
            v[k] = v[k].saturating_add(1);
            c.set(v)
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn take_violations() -> [u32; 3] {
        VIOLATIONS.try_with(|c| c.replace([0; 3])).unwrap_or([0; 3])
    }

    // SAFETY (all below): single-threaded wasm32.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn get() -> Cur {
        unsafe { *CURRENT.get() }
    }
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn set(v: Cur) -> Cur {
        unsafe { core::mem::replace(&mut *CURRENT.get(), v) }
    }
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn armed() -> bool {
        unsafe { *ARMED.get() }
    }
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn set_armed(v: bool) -> bool {
        unsafe { core::mem::replace(&mut *ARMED.get(), v) }
    }
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn violation(k: usize) {
        unsafe {
            let v = &mut *VIOLATIONS.get();
            v[k] = v[k].saturating_add(1);
        }
    }
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn take_violations() -> [u32; 3] {
        unsafe { core::mem::take(&mut *VIOLATIONS.get()) }
    }

    /// Restores the previous arena when dropped, including on unwind.
    pub(crate) struct Guard(Cur);

    impl Guard {
        pub(crate) fn enter(id: u16, epoch: u64) -> Self {
            Self(set((id, epoch)))
        }
        /// No scope current: allocations go to the glue partition.
        pub(crate) fn outside() -> Self {
            Self(set((NONE, 0)))
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            set(self.0);
        }
    }
}

/// The wasm32 stack (design §2.1 and §5.1). rustc's `wasm32-unknown-unknown`
/// links the stack first: `[__stack_low, __stack_high)` at the bottom of
/// linear memory, outside the pool. Its size is a versioned memory-model
/// input (`stack_bytes`); its high-water is measured by painting: at pool
/// initialisation every word below the current frame is filled with a
/// pattern, and the lowest word no longer holding it marks the deepest the
/// stack has reached. Natively the stack belongs to the OS thread; nothing is
/// measured.
pub(crate) mod stack {
    #[cfg(target_arch = "wasm32")]
    unsafe extern "C" {
        static __stack_low: u8;
        static __stack_high: u8;
    }

    /// The paint pattern.
    #[cfg(target_arch = "wasm32")]
    const PAINT: u32 = 0x5A17_A5E1;
    /// Bytes left unpainted below the painting frame: `paint` keeps one
    /// byte on the shadow stack and calls nothing, so its frame ends within
    /// one 16-byte alignment of that byte; 256 bytes is the margin. The
    /// measured high-water is therefore at least the depth at the first
    /// allocation plus this.
    #[cfg(target_arch = "wasm32")]
    const GUARD: usize = 256;
    /// The first word painted: address 0 is null to Rust, so painting starts
    /// one word above it.
    #[cfg(target_arch = "wasm32")]
    const FIRST: usize = 16;

    #[cfg(target_arch = "wasm32")]
    static PAINTED_TO: super::Racy<usize> = super::Racy::new(0);

    /// `(low, high)` addresses of the linked stack (wasm32), or `(0, 0)`.
    pub(crate) fn bounds() -> (usize, usize) {
        #[cfg(target_arch = "wasm32")]
        {
            (&raw const __stack_low as usize, &raw const __stack_high as usize)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            (0, 0)
        }
    }

    /// Paints the unused stack once, at pool initialisation. Allocation-free.
    #[inline(never)]
    pub(crate) fn paint() {
        #[cfg(target_arch = "wasm32")]
        unsafe {
            let here = 0u8;
            let sp = core::hint::black_box(&here) as *const u8 as usize;
            let (low, _) = bounds();
            let start = low.max(FIRST).next_multiple_of(4);
            let end = sp.saturating_sub(GUARD) & !3;
            let mut a = start;
            while a < end {
                core::ptr::write_volatile(core::ptr::with_exposed_provenance_mut::<u32>(a), PAINT);
                a += 4;
            }
            *PAINTED_TO.get() = end.max(start);
        }
    }

    /// The deepest the stack has reached since the pool was initialised, in
    /// bytes below `__stack_high` (0 natively). Allocation-free; scans the
    /// painted region from the bottom.
    pub(crate) fn high_water() -> usize {
        #[cfg(target_arch = "wasm32")]
        unsafe {
            let (low, high) = bounds();
            let start = low.max(FIRST).next_multiple_of(4);
            let end = *PAINTED_TO.get();
            if end <= start {
                return 0;
            }
            let mut a = start;
            while a < end && core::ptr::read_volatile(core::ptr::with_exposed_provenance::<u32>(a)) == PAINT {
                a += 4;
            }
            high.saturating_sub(a)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            0
        }
    }
}
