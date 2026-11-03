//! The Slop Ninja testnet alerts relay: a private Worker (no route, no
//! `workers.dev` name) that turns alerts into email through a `send_email`
//! binding (decision D18).
//!
//! It has three inputs:
//!
//! 1. **The publisher's alert posts**, through the publisher's service
//!    binding ([`slop_ninja_alerts_wire::Post`]). Each event id is emailed
//!    once ([`ledger`]): a post retried after a lost answer sends nothing new.
//! 2. **A dead-man check on the publisher** ([`rules`]): the publisher posts
//!    a heartbeat after every run whose alert phase finished; the relay's own
//!    one-minute cron emails when the heartbeats stop for longer than a bound
//!    derived from the publisher's cron period, lease and hold, and then also
//!    reads the coordinator Workers' status itself. A crashed publisher, a
//!    refused configuration that cannot alert, a stopped cron or a broken
//!    binding are all caught. Each heartbeat also says how the run's alert
//!    posts fared, and the relay records the posts it refused, so alerts
//!    that keep failing while heartbeats arrive are emailed too.
//! 3. **The spend tracker's upload** ([`tracker`]): the tracker host uploads
//!    its latest alert fields and month totals to the relay's private bucket
//!    after each run; the cron emails when a month's alert becomes active, a
//!    reminder while it is unacknowledged, a resolution once it is
//!    acknowledged (`track.sh --ack`) or falls below, an alert when the
//!    tracker's output is older than two of its schedule periods, and one
//!    when its runs have not priced everything for that long.
//!
//! The cron is outside the coordinator and the publisher and advances no
//! protocol state. It can also ping an external dead-man URL after each
//! tick that finished with its email path working, so a stopped relay or a
//! failing send path is caught outside Cloudflare. Only the network and the
//! two addresses are required settings; an optional one that does not parse
//! is ignored and emailed (`relay_config_warning`). Messages are plain text
//! ([`mime`]) and content-free: rule names, subjects, ages and epochs; spend
//! figures only in a spend alert's body, and only when
//! `SPEND_FIGURES_IN_EMAIL` is enabled. All logic but
//! `cf.rs` is plain Rust and tested natively; `cf.rs` holds the Workers entry
//! points and compiles only for `wasm32`.

pub mod arena;
pub mod config;
pub mod ledger;
pub mod mime;
pub mod relay;
pub mod rules;
pub mod status;
pub mod tracker;

#[cfg(target_arch = "wasm32")]
mod cf;

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

/// Runs a future that never waits on outside I/O (native tests: every
/// future here is ready when polled) to completion on the current thread.
pub fn block_on<F: Future>(future: F) -> F::Output {
    fn noop(_: *const ()) {}
    fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(core::ptr::null(), &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    // SAFETY: the vtable's functions ignore the data pointer.
    let waker = unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
    }
}
