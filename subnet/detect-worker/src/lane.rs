//! The scorer instance's start options, and the check that a started
//! instance runs them, natively tested.
//!
//! The lane starts its Container with Internet access off and two variables
//! in the instance's environment: `A_SESSIONS`, one session (a standard-3
//! instance's memory holds one under the scorer's structural bound, not
//! two), and `A_THREADS`, two threads (the instance's two vCPUs). The
//! image's own defaults differ, so an instance that did not receive the
//! variables reports other values in its health report, and the lane
//! refuses it ([`runs_ours`]).
//!
//! The runtime reads the start options as a plain object whose own
//! properties are `enableInternet` and `env`, a record of strings:
//! [`StartOptions::to_json`] is that object, under the field names
//! `cf_lane` builds it with through `Reflect` before it calls the
//! Container's `start`, as the coordinator's evaluation lanes do. `worker`'s
//! typed `ContainerStartupOptions` is not used: it is a class instance whose
//! fields are accessors on its prototype, with `env` a `Map`, which has no
//! own keys, and an empty `entrypoint` list it always carries. The object
//! built here has no `entrypoint`, so the image's own entry point runs.
//!
//! **Keeping the instance** ([`keep_until`], [`keep`]). The lane keeps its
//! instance for the idle timeout (`IDLE_MS`) after the last scan, or after
//! the start of one no scan has used yet. It does not rely on the runtime's
//! own sleep timer alone: while it keeps an instance, the lane's alarm runs
//! at least every [`KEEPALIVE_MS`], so its Durable Object is never idle
//! long enough to be evicted (an eviction can end the instance), and each
//! run sets the Container's inactivity timeout to the time still left, so
//! an instance whose object went away anyway still stops at the same time.
//! When the time is up the alarm destroys the instance and stops.
//!
//! **A fresh start that fails is tried once more** ([`again_after`]), in the
//! same request and within its deadline, before the lane answers that it
//! could not be made ready (the reader's `restarting`). Failures that a new
//! instance meets the same way (the build identity, the sessions and
//! threads, the spec, the package's own manifest, a refusal the scorer
//! calls definitive) are not tried again.

use serde_json::{Map, Value};

/// The scorer's sessions on the lane's instance.
pub const SESSIONS: u64 = 1;
/// The scorer's threads on the lane's instance.
pub const THREADS: u64 = 2;

/// The runtime's name for the Internet access field.
pub const INTERNET_FIELD: &str = "enableInternet";
/// The runtime's name for the environment field (a record of strings).
pub const ENV_FIELD: &str = "env";

/// What the lane starts its instance with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartOptions {
    /// Whether the instance reaches the Internet (never).
    pub enable_internet: bool,
    /// The instance's environment variables, in order.
    pub env: Vec<(&'static str, String)>,
}

/// The lane's start options: Internet off, [`SESSIONS`] and [`THREADS`].
pub fn start_options() -> StartOptions {
    StartOptions {
        enable_internet: false,
        env: vec![("A_SESSIONS", SESSIONS.to_string()), ("A_THREADS", THREADS.to_string())],
    }
}

impl StartOptions {
    /// The plain object the runtime reads, as JSON: own `enableInternet`
    /// and `env` properties and nothing else.
    pub fn to_json(&self) -> Value {
        let env: Map<String, Value> =
            self.env.iter().map(|(k, v)| ((*k).to_owned(), Value::String(v.clone()))).collect();
        let mut o = Map::new();
        o.insert(INTERNET_FIELD.to_owned(), Value::Bool(self.enable_internet));
        o.insert(ENV_FIELD.to_owned(), Value::Object(env));
        Value::Object(o)
    }
}

/// Whether a health report's build runs the lane's sessions and threads,
/// the values [`start_options`] passes.
pub fn runs_ours(health: &Value) -> bool {
    let n = |p: &str| health.pointer(p).and_then(Value::as_u64);
    n("/build/sessions") == Some(SESSIONS) && n("/build/threads") == Some(THREADS)
}

/// The longest the lane's alarm waits between two runs while it keeps an
/// instance: half the 10 seconds after which the platform may evict a
/// Durable Object that has no event, so the lane's object always has one.
pub const KEEPALIVE_MS: u64 = 5_000;

/// Until when the lane keeps its instance after a scan, or after starting
/// one: the idle timeout and one hold from `now`. The gate counts the
/// instance awake for the idle timeout from the scan's done report, which
/// follows the lane's answer within one hold, so the lane never lets go of
/// an instance the gate still counts awake.
pub fn keep_until(now: u64, idle_ms: u64, hold_ms: u64) -> u64 {
    now.saturating_add(idle_ms).saturating_add(hold_ms)
}

/// What one run of the lane's alarm does with the instance it keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// The idle time is up: destroy the instance and stop.
    Stop,
    /// Keep it: set the Container's inactivity timeout to `sleep_ms` (the
    /// time left) and run the alarm again in `next_ms`.
    Tick { sleep_ms: u64, next_ms: u64 },
}

/// The alarm's decision at `now` for an instance kept until `until`.
pub fn keep(now: u64, until: u64) -> Keep {
    match until.checked_sub(now) {
        Some(left) if left > 0 => Keep::Tick { sleep_ms: left, next_ms: left.min(KEEPALIVE_MS) },
        _ => Keep::Stop,
    }
}

/// The package manifest failures: the deployment's own pointer or bucket
/// copy is wrong, so a new instance fails the same way.
pub const MANIFEST_CODES: &[&str] = &[
    "package bucket",
    "package manifest missing",
    "package manifest digest",
    "package manifest json",
    "package file outside the bound bucket or unpinned",
];

/// Why a prepare failed, and whether a new instance may do better.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failed {
    pub code: &'static str,
    /// `false` when a new instance would fail the same way.
    pub again: bool,
}

impl Failed {
    /// A failure a new instance may not meet (a start, a health check, a
    /// transfer or a load that did not finish).
    pub const fn transient(code: &'static str) -> Failed {
        Failed { code, again: true }
    }

    /// A failure every instance meets the same way.
    pub const fn fixed(code: &'static str) -> Failed {
        Failed { code, again: false }
    }

    /// A load's failure: the scorer's definitive refusal of the package and
    /// the manifest failures are fixed; the rest are transient.
    pub fn of_load(code: &'static str, definitive: bool) -> Failed {
        Failed { code, again: !definitive && !MANIFEST_CODES.contains(&code) }
    }
}

/// Whether a prepare that failed with `f` is tried once more in the same
/// request: only after a fresh start (an instance this prepare started),
/// only for a failure a new instance may not meet, only once (`tried`),
/// and only while the request's deadline leaves time to start again.
pub fn again_after(f: Failed, fresh: bool, tried: bool, now: u64, deadline: u64) -> bool {
    fresh && f.again && !tried && now < deadline
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_options_carry_internet_off_and_the_lanes_env() {
        let o = start_options();
        assert!(!o.enable_internet);
        assert_eq!(o.env, vec![("A_SESSIONS", "1".to_owned()), ("A_THREADS", "2".to_owned())]);
    }

    #[test]
    fn the_object_has_own_env_keys_and_no_entrypoint() {
        let v = start_options().to_json();
        assert_eq!(v, json!({"enableInternet": false, "env": {"A_SESSIONS": "1", "A_THREADS": "2"}}));
        let o = v.as_object().expect("object");
        assert_eq!(o.keys().collect::<Vec<_>>(), [INTERNET_FIELD, ENV_FIELD]);
        assert!(!o.contains_key("entrypoint"), "the image's own entry point runs");
        // Every env value is a string, as the runtime's record of strings
        // takes it, and a number would not be.
        let env = o[ENV_FIELD].as_object().expect("env object");
        assert!(env.values().all(Value::is_string));
        // The scorer parses each value back to the number the lane checks.
        assert_eq!(env["A_SESSIONS"].as_str().and_then(|s| s.parse::<u64>().ok()), Some(SESSIONS));
        assert_eq!(env["A_THREADS"].as_str().and_then(|s| s.parse::<u64>().ok()), Some(THREADS));
    }

    #[test]
    fn the_health_check_takes_what_the_options_pass_and_nothing_else() {
        let ours = json!({"build": {"sessions": 1, "threads": 2}});
        assert!(runs_ours(&ours));
        // The image's own defaults: an instance that did not receive the env.
        assert!(!runs_ours(&json!({"build": {"sessions": 4, "threads": 1}})));
        for bad in [
            json!({"build": {"sessions": 1}}),
            json!({"build": {"threads": 2}}),
            json!({"build": {"sessions": "1", "threads": "2"}}),
            json!({"build": {"sessions": 2, "threads": 2}}),
            json!({"build": {"sessions": 1, "threads": 1}}),
            json!({"sessions": 1, "threads": 2}),
            json!({}),
        ] {
            assert!(!runs_ours(&bad), "{bad}");
        }
    }

    #[test]
    fn the_instance_is_kept_for_the_idle_time_and_a_hold() {
        assert_eq!(keep_until(1_000, 600_000, 20_000), 621_000);
        assert_eq!(keep_until(u64::MAX - 1, 600_000, 20_000), u64::MAX);
    }

    #[test]
    fn the_alarm_ticks_until_the_time_is_up_then_stops() {
        let until = keep_until(0, 600_000, 20_000);
        // Every tick is at most KEEPALIVE_MS apart, and the inactivity
        // timeout it sets is what is left, so an instance whose object went
        // away still stops at `until`.
        assert_eq!(keep(0, until), Keep::Tick { sleep_ms: until, next_ms: KEEPALIVE_MS });
        assert_eq!(keep(until - 3_000, until), Keep::Tick { sleep_ms: 3_000, next_ms: 3_000 });
        assert_eq!(keep(until - 1, until), Keep::Tick { sleep_ms: 1, next_ms: 1 });
        assert_eq!(keep(until, until), Keep::Stop);
        assert_eq!(keep(until + 1, until), Keep::Stop);
        // Nothing kept (0) stops at once.
        assert_eq!(keep(5, 0), Keep::Stop);
        // A run of ticks reaches `until` and never waits longer than the
        // keepalive between two runs.
        let (mut now, mut runs) = (0u64, 0u32);
        while let Keep::Tick { sleep_ms, next_ms } = keep(now, until) {
            assert!((1..=KEEPALIVE_MS).contains(&next_ms));
            assert_eq!(now + sleep_ms, until);
            now += next_ms;
            runs += 1;
        }
        assert_eq!(now, until);
        assert_eq!(runs, 124);
    }

    // The keepalive is half the platform's shortest idle eviction.
    const _: () = assert!(KEEPALIVE_MS * 2 <= 10_000);

    #[test]
    fn only_a_fresh_transient_failure_is_tried_again_once() {
        let t = Failed::transient("container did not answer");
        assert!(again_after(t, true, false, 0, 1));
        assert!(!again_after(t, false, false, 0, 1), "a reused instance's failure is not a fresh start");
        assert!(!again_after(t, true, true, 0, 1), "only once");
        assert!(!again_after(t, true, false, 1, 1), "no time left");
        let f = Failed::fixed("scorer build identity");
        assert!(!again_after(f, true, false, 0, 1));
    }

    #[test]
    fn a_load_failure_is_tried_again_unless_a_new_instance_meets_it_too() {
        for code in MANIFEST_CODES {
            assert!(!Failed::of_load(code, false).again, "{code}");
        }
        assert!(!Failed::of_load("refused", true).again, "a definitive refusal");
        for code in ["chunk read", "package stream timeout", "load failed", "load outlasted its wait", "prepare call"] {
            assert!(Failed::of_load(code, false).again, "{code}");
        }
    }
}
