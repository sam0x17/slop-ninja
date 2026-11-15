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
}
