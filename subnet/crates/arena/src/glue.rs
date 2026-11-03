//! Glue admission (design §2.2): the bounded, admitted arena for platform
//! glue.
//!
//! Glue is everything allocated while no scope is current: the per-entry
//! export future box and js-sys `Task`, the `worker::Request` conversion
//! strings, error responses, and each Durable Object instance's
//! `Rc<WasmRefCell<T>>`. It lives in a fixed partition of the pool
//! (`GLUE_BUDGET_PAGES`), which scopes can never take and glue can never
//! exceed, so glue usage never changes how a scope refusal is classed.
//!
//! The partition is sized from measured per-entry costs (`build.rs`): an
//! export first takes a [`Ticket`] (allocation-free, before anything
//! allocates) and refuses with a JS-only 503 (or a thrown JS error for an
//! alarm) when every ticket is in flight. A Durable Object constructor takes
//! a [`Resident`] the same way. With tickets and residents bounded, glue live
//! bytes are bounded by `glue_static + tickets × ticket + residents ×
//! resident`, which is the partition.

use core::marker::PhantomData;

use crate::pool;
use crate::scope::Refusal;
use crate::sys::lock;

/// One in-flight entry's glue admission. `!Send`; released on drop.
#[must_use = "the ticket is released when dropped"]
pub struct Ticket(PhantomData<*mut ()>);

impl Ticket {
    /// Admits one entry. Allocation-free. Refuses `Retry`/`Glue` when every
    /// ticket is in flight.
    pub fn admit() -> Result<Ticket, Refusal> {
        pool::ticket_acquire()?;
        Ok(Ticket(PhantomData))
    }

    /// Hands the admission to the platform task (the promise's completion
    /// calls [`release`]). Allocation-free.
    pub fn into_raw(self) {
        let _ = core::mem::ManuallyDrop::new(self);
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        pool::ticket_release();
    }
}

/// Runs `f` with no scope current: what it allocates goes to the glue
/// partition. For handing a future to the platform (`future_to_promise`,
/// `ctx.wait_until`) from inside a scope, under a ticket that covers the
/// task box (see `Continuation::spawn`).
pub fn outside<R>(f: impl FnOnce() -> R) -> R {
    let _g = crate::sys::current::Guard::outside();
    f()
}

/// Releases a ticket handed over with [`Ticket::into_raw`].
pub fn release() {
    pool::ticket_release();
}

/// One co-resident Durable Object instance's glue admission.
#[must_use = "the residency is released when dropped"]
pub struct Resident(PhantomData<*mut ()>);

static RESIDENTS: crate::sys::Racy<u32> = crate::sys::Racy::new(0);

impl Resident {
    /// Admits one instance. Allocation-free. Refuses `Retry`/`Glue` once the
    /// model's resident count is reached; the constructor then throws a JS
    /// error and the caller retries after an idle instance is evicted.
    pub fn admit() -> Result<Resident, Refusal> {
        let _g = lock::hold();
        // SAFETY: under the pool lock (native) or single-threaded wasm32.
        let n = unsafe { &mut *RESIDENTS.get() };
        if *n >= crate::model::GLUE_RESIDENTS {
            return Err(Refusal::new(0, crate::RefusalKind::Retry, crate::Reason::Glue, crate::model::GLUE_RESIDENT_BYTES, 0));
        }
        *n += 1;
        Ok(Resident(PhantomData))
    }

    /// Instances currently admitted.
    pub fn count() -> u32 {
        let _g = lock::hold();
        unsafe { *RESIDENTS.get() }
    }
}

impl Drop for Resident {
    fn drop(&mut self) {
        let _g = lock::hold();
        let n = unsafe { &mut *RESIDENTS.get() };
        *n = n.saturating_sub(1);
    }
}
