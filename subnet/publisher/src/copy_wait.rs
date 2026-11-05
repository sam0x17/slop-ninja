//! How long a server-side copy (`ARTIFACT_COPY = "server"`, an S3
//! `CopyObject` inside R2) is waited for, and which objects take it
//! (`slop_ninja_r2_copy_wait_v1`).
//!
//! R2 answers a `CopyObject` only once the whole object is copied, and it
//! abandons a copy whose caller disconnects: probe G7-x (2026-11-05, X11)
//! dropped the caller at 10 s during a 954,576,228 B copy and no object ever
//! appeared. A fixed wait would therefore leave every object above a size
//! pending on every run. The wait is instead derived from the object's length
//! and the copy rate the probe measured, with a margin:
//!
//! `wait(len) = REQUEST_MS + ceil(len * PROBE_MS * MARGIN / PROBE_BYTES)`
//!
//! - `PROBE_BYTES` / `PROBE_MS`: probe G7-x, X12, the slower of its two size
//!   points: 268,435,456 random bytes copied across buckets in 25.4 s (about
//!   10.6 MB/s). X10 copied 954,576,228 zero bytes in 42.5 s (about
//!   22.4 MB/s); it is recorded, not used, since model weights are not zeros.
//! - `MARGIN`: the measured time is doubled (one probe point; R2's rate under
//!   other load is not measured).
//! - `REQUEST_MS`: the fixed part, the bound every other outbound request of
//!   the publisher has (X1: a 1 MiB copy answered in 656 ms).
//!
//! The wait must end inside the step's lease, so that no other run starts the
//! same copy meanwhile: a step renews the run's lease (`PUBLISHER_LEASE_MS`)
//! before it asks the signer (at most `REQUEST_MS`), then waits for the copy,
//! then reads the copy's `head()` and the record (at most `REQUEST_MS` more).
//! `WINDOW_MS = PUBLISHER_LEASE_MS - 2 * REQUEST_MS` is what is left for the
//! copy, well inside the 15-minute wall time of a cron invocation. An object
//! whose derived wait exceeds the window ([`CopyWait::max_server_len`]) is
//! copied by stream instead, without asking the signer.
//!
//! At the wait the copy request is aborted (`AbortSignal.timeout`), which
//! closes its connection, so R2 abandons the copy. A copy that is not
//! confirmed (aborted, failed in transport, or answered an error status
//! other than a refusal or the stale source's 412) is never left pending:
//! the destination is read first (a copy that completed is checked as one
//! found there), and an absent one is copied by stream in the same step,
//! under a renewed lease. The margin therefore only decides how often the
//! stream fallback runs, never whether an object is published.

use slop_ninja_alerts_wire::{OUTBOUND_TIMEOUT_MS, PUBLISHER_LEASE_MS};

/// This derivation's identifier: a change to any value below is a new
/// version.
pub const COPY_WAIT_ID: &str = "slop_ninja_r2_copy_wait_v1";

/// Probe G7-x X12 (2026-11-05): bytes of the slower measured copy.
pub const PROBE_BYTES: u64 = 268_435_456;
/// Probe G7-x X12: its duration, request to answer, in milliseconds.
pub const PROBE_MS: u64 = 25_400;
/// The measured duration is multiplied by this.
pub const MARGIN: u64 = 2;
/// The fixed part of the wait: the publisher's outbound request bound.
pub const REQUEST_MS: u64 = OUTBOUND_TIMEOUT_MS;
/// The part of a step's lease left for the copy (the signer request before
/// it and the destination read after it take one `REQUEST_MS` each).
pub const WINDOW_MS: u64 = PUBLISHER_LEASE_MS - 2 * REQUEST_MS;

const _: () = assert!(WINDOW_MS > REQUEST_MS && WINDOW_MS < PUBLISHER_LEASE_MS);

/// The values one derivation uses. [`CopyWait::V1`] is the only one a
/// deployed publisher runs; tests use others to reach the stream fallback
/// with small objects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopyWait {
    pub probe_bytes: u64,
    pub probe_ms: u64,
    pub margin: u64,
    pub request_ms: u64,
    pub window_ms: u64,
}

impl CopyWait {
    pub const V1: Self = Self {
        probe_bytes: PROBE_BYTES,
        probe_ms: PROBE_MS,
        margin: MARGIN,
        request_ms: REQUEST_MS,
        window_ms: WINDOW_MS,
    };

    /// The wait for a copy of `len` bytes, in milliseconds (saturating).
    pub fn wait_ms(&self, len: u64) -> u64 {
        let bytes = u128::from(self.probe_bytes.max(1));
        let copy = (u128::from(len) * u128::from(self.probe_ms))
            .checked_mul(u128::from(self.margin))
            .map_or(u128::MAX, |scaled| scaled.div_ceil(bytes));
        u64::try_from(copy)
            .unwrap_or(u64::MAX)
            .saturating_add(self.request_ms)
    }

    /// The largest object whose wait fits the window.
    pub fn max_server_len(&self) -> u64 {
        let rate_ms = u128::from(self.probe_ms) * u128::from(self.margin);
        if rate_ms == 0 {
            return u64::MAX;
        }
        let room = u128::from(self.window_ms.saturating_sub(self.request_ms));
        u64::try_from(room * u128::from(self.probe_bytes) / rate_ms).unwrap_or(u64::MAX)
    }

    /// The wait for a server-side copy of `len` bytes, or `None` when it
    /// does not fit the window and the object is copied by stream.
    pub fn plan(&self, len: u64) -> Option<u64> {
        let wait = self.wait_ms(len);
        (wait <= self.window_ms).then_some(wait)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe's two size points (G7-x X10 and X12), in bytes and ms.
    const X12: (u64, u64) = (268_435_456, 25_400);
    const X10: (u64, u64) = (954_576_228, 42_500);

    #[test]
    fn the_wait_covers_each_measured_copy_with_its_margin() {
        let w = CopyWait::V1;
        for (len, ms) in [X12, X10] {
            assert!(w.wait_ms(len) >= MARGIN * ms, "{len}");
        }
        // The slower point exactly: twice its time, plus the request bound.
        assert_eq!(w.wait_ms(X12.0), REQUEST_MS + 2 * X12.1);
        assert_eq!(w.wait_ms(0), REQUEST_MS);
        // Monotonic, without overflow at any length.
        let mut last = 0;
        for len in [1, 1 << 20, 1 << 28, 1 << 30, 5 << 30, u64::MAX] {
            let wait = w.wait_ms(len);
            assert!(wait >= last, "{len}");
            last = wait;
        }
        assert!(w.wait_ms(u64::MAX) > WINDOW_MS);
        let fast = CopyWait {
            probe_bytes: 1,
            probe_ms: u64::MAX,
            ..CopyWait::V1
        };
        assert_eq!(fast.wait_ms(u64::MAX), u64::MAX);
    }

    #[test]
    fn objects_above_the_window_are_copied_by_stream() {
        let w = CopyWait::V1;
        assert_eq!(WINDOW_MS, 100_000);
        let max = w.max_server_len();
        assert_eq!(max, 475_574_626);
        assert_eq!(w.plan(max), Some(w.wait_ms(max)));
        assert!(w.wait_ms(max) <= WINDOW_MS);
        assert_eq!(w.plan(max + 1), None);
        // The 256 MiB point is copied inside R2; the largest probed object
        // (the largest admission object) streams.
        assert!(w.plan(X12.0).is_some());
        assert_eq!(w.plan(X10.0), None);
        // A header (at most `MAX_ARTIFACT_HEADER_LEN`, 1 MiB) always fits.
        assert_eq!(
            w.plan(u64::from(slop_ninja_protocol::MAX_ARTIFACT_HEADER_LEN)),
            Some(REQUEST_MS + 199)
        );
    }

    #[test]
    fn a_zero_rate_never_divides_by_zero() {
        let w = CopyWait {
            probe_ms: 0,
            ..CopyWait::V1
        };
        assert_eq!(w.wait_ms(1 << 30), REQUEST_MS);
        assert_eq!(w.max_server_len(), u64::MAX);
        let w = CopyWait {
            probe_bytes: 0,
            ..CopyWait::V1
        };
        assert!(w.plan(1 << 30).is_none());
    }
}
