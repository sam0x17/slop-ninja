//! The liveness waits and the scan lease derived from them, natively tested.
//!
//! The waits are deployment parameters with measured sources (`wrangler.toml`
//! states each one; the launch gate re-measures them). They are not abuse
//! thresholds. The scan lease is not a parameter: it is derived here, so the
//! gate can never admit the next ticket while the lane may still be working
//! on the last one.
//!
//! - The lane bounds one admitted scan by [`Waits::lane_ms`]: a start, a load
//!   and one score call, each cut at its own wait and all of them at the
//!   time the Worker passes. Waiting for a load already running in the
//!   lane's alarm counts against the same time.
//! - The Worker passes the lane what is left of [`Waits::lane_ms`] since its
//!   admission and allows the lane call that time plus one hold.
//! - The gate's lease for an admitted ticket is [`Waits::scan_lease_ms`]: the
//!   lane's bound plus one hold, the bound on the internal calls around it
//!   (the admission reply, the lane call and reply, the done report), which
//!   take milliseconds.

/// The liveness waits, in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Waits {
    /// A started instance answers its health check within this.
    pub start_ms: u64,
    /// A load (or a wait for one) reaches ready within this.
    pub load_ms: u64,
    /// One score call and its reply finish within this.
    pub score_ms: u64,
    /// A waiting ticket's ask is held this long (its call gets two), and it
    /// bounds the score path's other internal calls and every line written
    /// to the reader.
    pub hold_ms: u64,
}

impl Waits {
    /// The deployment's values (`wrangler.toml`), used when a var is absent.
    pub const DEFAULT: Waits = Waits { start_ms: 60_000, load_ms: 180_000, score_ms: 60_000, hold_ms: 20_000 };

    /// The lane's bound for one admitted scan.
    pub const fn lane_ms(&self) -> u64 {
        self.start_ms.saturating_add(self.load_ms).saturating_add(self.score_ms)
    }

    /// An admitted ticket's lease at the gate.
    pub const fn scan_lease_ms(&self) -> u64 {
        self.lane_ms().saturating_add(self.hold_ms)
    }

    /// The time the Worker passes the lane, `elapsed_ms` after its
    /// admission arrived. `None`: nothing is left, so the Worker reports done
    /// without calling the lane.
    pub fn lane_left_ms(&self, elapsed_ms: u64) -> Option<u64> {
        Some(self.lane_ms().saturating_sub(elapsed_ms)).filter(|b| *b > 0)
    }

    /// The lane's deadline for a call carrying `left_ms`, from `now`
    /// (never beyond its own bound, whatever it is sent).
    pub fn lane_deadline(&self, now: u64, left_ms: Option<u64>) -> u64 {
        now.saturating_add(left_ms.unwrap_or(u64::MAX).min(self.lane_ms()))
    }

    /// One stage's end: its own wait from `now`, cut at `deadline`.
    pub fn stage_until(now: u64, wait_ms: u64, deadline: u64) -> u64 {
        now.saturating_add(wait_ms).min(deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lease_covers_the_lanes_worst_case() {
        let w = Waits::DEFAULT;
        // The longest path: a wait for the alarm's load, then a start, a
        // load and the scan. The wait and the stages share one deadline.
        let deadline = w.lane_deadline(0, w.lane_left_ms(0));
        assert_eq!(deadline, w.start_ms + w.load_ms + w.score_ms);
        let after_alarm_wait = Waits::stage_until(0, w.load_ms, deadline);
        let after_start = Waits::stage_until(after_alarm_wait, w.start_ms, deadline);
        let after_load = Waits::stage_until(after_start, w.load_ms, deadline);
        let after_score = Waits::stage_until(after_load, w.score_ms, deadline);
        assert!(after_score <= deadline);
        // The gate's lease ends a hold after the lane's deadline.
        assert_eq!(w.scan_lease_ms(), deadline + w.hold_ms);
    }

    #[test]
    fn the_lanes_time_shrinks_with_the_time_since_admission() {
        let w = Waits::DEFAULT;
        assert_eq!(w.lane_left_ms(0), Some(w.lane_ms()));
        assert_eq!(w.lane_left_ms(1_000), Some(w.lane_ms() - 1_000));
        assert_eq!(w.lane_left_ms(w.lane_ms()), None);
        // The lane never takes more than its own bound, whatever it is sent.
        assert_eq!(w.lane_deadline(5, Some(u64::MAX)), 5 + w.lane_ms());
        assert_eq!(w.lane_deadline(5, None), 5 + w.lane_ms());
        assert_eq!(w.lane_deadline(5, Some(10)), 15);
    }
}
