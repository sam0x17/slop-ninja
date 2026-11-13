//! `auth-policy/v1`: the sign-in limits, each value with its reason.
//!
//! The account Worker reads the policy from a var as JSON
//! ([`AuthPolicy::from_json`]); every value carries its reason in that JSON,
//! a missing reason or an unknown field refuses the whole policy, and the
//! values change only by a versioned config deploy. [`AuthPolicy::v1`] is the
//! default the var is written from.
//!
//! The mail budgets are token buckets ([`Bucket`], [`BucketState`]) with
//! exact integer arithmetic, counted by the account Worker's quota object:
//!
//! - `email:<addr_h>` ([`AuthPolicy::email_ceiling`]) takes only sends whose
//!   link its holder can use: `signin`, `step_up`, `attach_pending` and
//!   `recover`;
//! - `link` sends never draw on it. They draw on `email_link:<addr_h>`
//!   ([`AuthPolicy::email_link_ceiling`]) and on the requesting account's
//!   `link_mail:<user_id>` ([`AuthPolicy::link_mail_quota`]); when either is
//!   empty the connect request is refused, and a sign-in mail is never
//!   stopped by them.
//!
//! So every send that draws on the sign-in bucket is a link its holder can
//! use, and no other account's requests can spend it.

use serde::{Deserialize, Serialize};

/// The version string.
pub const VERSION: &str = "auth-policy/v1";

/// A value with its reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Valued<T> {
    /// The value.
    pub value: T,
    /// Why it is this value.
    pub reason: String,
}

fn v<T>(value: T, reason: &str) -> Valued<T> {
    Valued {
        value,
        reason: reason.to_owned(),
    }
}

/// A token bucket: `capacity` sends, refilling one every `refill_every_s`
/// seconds (so `capacity` per rolling `capacity × refill_every_s`, with no
/// UTC-midnight edge).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bucket {
    /// Sends when full.
    pub capacity: u32,
    /// Seconds per refilled send.
    pub refill_every_s: u64,
}

/// A bucket's stored state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketState {
    /// Sends left.
    pub tokens: u32,
    /// The instant the last whole refill was credited (Unix milliseconds).
    pub refilled_at_ms: u64,
}

/// The result of taking one send from a bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Take {
    /// Taken; `emptied` is true for the send that left the bucket empty
    /// (the one that raises the ceiling alert point).
    Taken {
        /// The new state to store.
        state: BucketState,
        /// Whether this send emptied the bucket.
        emptied: bool,
    },
    /// Empty: nothing is sent. The state to store is unchanged except for
    /// refills credited.
    Empty {
        /// The state to store.
        state: BucketState,
    },
}

impl Bucket {
    /// A full bucket (a key never seen, or after the operator's
    /// `reset_quota`).
    #[must_use]
    pub const fn full(&self, now_ms: u64) -> BucketState {
        BucketState {
            tokens: self.capacity,
            refilled_at_ms: now_ms,
        }
    }

    /// Credits whole refills elapsed since `state`.
    #[must_use]
    pub fn refill(&self, state: BucketState, now_ms: u64) -> BucketState {
        let every_ms = self.refill_every_s.saturating_mul(1000).max(1);
        if state.tokens >= self.capacity {
            // A full bucket banks no refill credit.
            return self.full(now_ms.max(state.refilled_at_ms));
        }
        if now_ms <= state.refilled_at_ms {
            return state;
        }
        let whole = (now_ms - state.refilled_at_ms) / every_ms;
        let room = u64::from(self.capacity - state.tokens);
        if whole >= room {
            self.full(now_ms)
        } else {
            BucketState {
                // `whole < room <= u32::MAX`.
                tokens: state.tokens + whole as u32,
                refilled_at_ms: state.refilled_at_ms + whole * every_ms,
            }
        }
    }

    /// Takes one send, after crediting refills. `state` is `None` for a key
    /// never seen.
    #[must_use]
    pub fn take(&self, state: Option<BucketState>, now_ms: u64) -> Take {
        let state = self.refill(state.unwrap_or_else(|| self.full(now_ms)), now_ms);
        if state.tokens == 0 {
            return Take::Empty { state };
        }
        let state = BucketState {
            tokens: state.tokens - 1,
            ..state
        };
        Take::Taken {
            state,
            emptied: state.tokens == 0,
        }
    }
}

/// The wallet challenge rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WalletChallenges {
    /// Always `None`: nothing is counted per public key.
    pub per_key_quota: Option<u32>,
    /// Outstanding challenges per browser and key (a newer one replaces the
    /// older).
    pub per_browser_and_key: u32,
}

/// Where a sign-in may return to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReturnToPaths {
    /// Paths allowed exactly.
    pub exact: Vec<String>,
    /// Path prefixes allowed (each starts and ends with `/`).
    pub prefixes: Vec<String>,
}

/// The flow-table sweep.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowSweep {
    /// Seconds between scheduled sweeps.
    pub every_s: u64,
    /// Expired rows deleted alongside each insert.
    pub per_insert: u32,
}

/// `auth-policy/v1`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthPolicy {
    /// Always [`VERSION`].
    pub version: String,
    /// OAuth flow lifetime.
    pub flow_ttl_s: Valued<u64>,
    /// Email link lifetime.
    pub email_link_ttl_s: Valued<u64>,
    /// Wallet challenge lifetime.
    pub wallet_challenge_ttl_s: Valued<u64>,
    /// Session idle limit (also the session cookie's `Max-Age`).
    pub session_idle_s: Valued<u64>,
    /// Session absolute limit.
    pub session_absolute_s: Valued<u64>,
    /// How old a session's last renewal must be before it is renewed.
    pub session_renew_after_s: Valued<u64>,
    /// How recent a sign-in or step-up must be for sensitive actions.
    pub step_up_window_s: Valued<u64>,
    /// Spacing between links to one address from one browser.
    pub email_spacing_s: Valued<u64>,
    /// The per-address sign-in bucket `email:<addr_h>`.
    pub email_ceiling: Valued<Bucket>,
    /// The per-address connect bucket `email_link:<addr_h>`.
    pub email_link_ceiling: Valued<Bucket>,
    /// The per-account connect bucket `link_mail:<user_id>`.
    pub link_mail_quota: Valued<Bucket>,
    /// The wallet challenge rule.
    pub wallet_challenges: Valued<WalletChallenges>,
    /// The longest stored `return_to`, in bytes.
    pub return_to_max: Valued<usize>,
    /// The allowed `return_to` paths.
    pub return_to_paths: Valued<ReturnToPaths>,
    /// The flow-table sweep.
    pub flow_sweep: Valued<FlowSweep>,
}

const MIN: u64 = 60;
const HOUR: u64 = 3600;
const DAY: u64 = 86_400;

/// Why a policy was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyError {
    /// Not valid JSON for the schema (a missing value or reason, an unknown
    /// field, a wrong type).
    Schema(String),
    /// The version is not [`VERSION`].
    Version,
    /// A value is out of its structural range; names the field.
    Value(&'static str),
}

impl AuthPolicy {
    /// The default values.
    #[must_use]
    pub fn v1() -> Self {
        Self {
            version: VERSION.to_owned(),
            flow_ttl_s: v(
                10 * MIN,
                "GitHub codes live 10 minutes; covers a slow consent screen",
            ),
            email_link_ttl_s: v(
                15 * MIN,
                "short enough for a credential sent in clear mail; long enough for slow delivery",
            ),
            wallet_challenge_ttl_s: v(
                5 * MIN,
                "a signature takes seconds; limits relayed-challenge phishing",
            ),
            session_idle_s: v(
                30 * DAY,
                "a consumer site, and nothing behind a session moves money at launch",
            ),
            session_absolute_s: v(180 * DAY, "a fresh sign-in at least twice a year"),
            session_renew_after_s: v(DAY, "at most one cookie and row rewrite a day"),
            step_up_window_s: v(
                10 * MIN,
                "link, unlink, merge, email change, export, delete and later billing need a sign-in or a step-up this recent, with one of the account's own identities",
            ),
            email_spacing_s: v(
                MIN,
                "stops double sends from one browser; keyed by address and browser, so nobody else's request can delay or void a link",
            ),
            email_ceiling: v(
                Bucket {
                    capacity: 10,
                    refill_every_s: 24 * HOUR / 10,
                },
                "complaint control only: no address receives more than ten unrequested sign-in links a day; a person needs one or two, and reaching it raises auth_mail_ceiling",
            ),
            email_link_ceiling: v(
                Bucket {
                    capacity: 3,
                    refill_every_s: 24 * HOUR / 3,
                },
                "connect mails draw on their own per-address budget, so another account's requests never spend the holder's sign-in budget; when it is empty, connect requests to the address are refused",
            ),
            link_mail_quota: v(
                Bucket {
                    capacity: 5,
                    refill_every_s: 24 * HOUR / 5,
                },
                "one account cannot send connect mails to many addresses",
            ),
            wallet_challenges: v(
                WalletChallenges {
                    per_key_quota: None,
                    per_browser_and_key: 1,
                },
                "a public key is public on chain, so a per-key quota would let anyone lock its holder out; edge rules, the expiry and the sweep bound the rows",
            ),
            return_to_max: v(
                512,
                "the longest allowed path plus its query; anything longer is refused, not cut",
            ),
            return_to_paths: v(
                ReturnToPaths {
                    exact: vec!["/".to_owned()],
                    prefixes: ["/detect/", "/d/", "/account/"].map(str::to_owned).to_vec(),
                },
                "the pages a sign-in starts from; no other path is a destination",
            ),
            flow_sweep: v(
                FlowSweep {
                    every_s: 5 * MIN,
                    per_insert: 20,
                },
                "keeps the flow tables near their live size",
            ),
        }
    }

    /// Parses and validates the var.
    pub fn from_json(text: &str) -> Result<Self, PolicyError> {
        let p: Self = serde_json::from_str(text).map_err(|e| PolicyError::Schema(e.to_string()))?;
        p.validate()?;
        Ok(p)
    }

    /// The var's JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("the policy always serializes")
    }

    /// Every field's name and reason, in declaration order.
    #[must_use]
    pub fn reasons(&self) -> Vec<(&'static str, &str)> {
        vec![
            ("flow_ttl_s", &self.flow_ttl_s.reason),
            ("email_link_ttl_s", &self.email_link_ttl_s.reason),
            (
                "wallet_challenge_ttl_s",
                &self.wallet_challenge_ttl_s.reason,
            ),
            ("session_idle_s", &self.session_idle_s.reason),
            ("session_absolute_s", &self.session_absolute_s.reason),
            ("session_renew_after_s", &self.session_renew_after_s.reason),
            ("step_up_window_s", &self.step_up_window_s.reason),
            ("email_spacing_s", &self.email_spacing_s.reason),
            ("email_ceiling", &self.email_ceiling.reason),
            ("email_link_ceiling", &self.email_link_ceiling.reason),
            ("link_mail_quota", &self.link_mail_quota.reason),
            ("wallet_challenges", &self.wallet_challenges.reason),
            ("return_to_max", &self.return_to_max.reason),
            ("return_to_paths", &self.return_to_paths.reason),
            ("flow_sweep", &self.flow_sweep.reason),
        ]
        .into_iter()
        .map(|(n, r)| (n, r.as_str()))
        .collect()
    }

    /// Checks the structural rules every policy must meet.
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.version != VERSION {
            return Err(PolicyError::Version);
        }
        for (name, reason) in self.reasons() {
            if reason.trim().is_empty() {
                return Err(PolicyError::Value(name));
            }
        }
        let positive = [
            ("flow_ttl_s", self.flow_ttl_s.value),
            ("email_link_ttl_s", self.email_link_ttl_s.value),
            ("wallet_challenge_ttl_s", self.wallet_challenge_ttl_s.value),
            ("session_idle_s", self.session_idle_s.value),
            ("session_absolute_s", self.session_absolute_s.value),
            ("session_renew_after_s", self.session_renew_after_s.value),
            ("step_up_window_s", self.step_up_window_s.value),
            ("email_spacing_s", self.email_spacing_s.value),
            ("flow_sweep", self.flow_sweep.value.every_s),
        ];
        for (name, value) in positive {
            if value == 0 {
                return Err(PolicyError::Value(name));
            }
        }
        if self.session_renew_after_s.value >= self.session_idle_s.value {
            return Err(PolicyError::Value("session_renew_after_s"));
        }
        if self.session_idle_s.value > self.session_absolute_s.value {
            return Err(PolicyError::Value("session_idle_s"));
        }
        if self.step_up_window_s.value > self.session_idle_s.value {
            return Err(PolicyError::Value("step_up_window_s"));
        }
        for (name, b) in [
            ("email_ceiling", self.email_ceiling.value),
            ("email_link_ceiling", self.email_link_ceiling.value),
            ("link_mail_quota", self.link_mail_quota.value),
        ] {
            if b.capacity == 0 || b.refill_every_s == 0 {
                return Err(PolicyError::Value(name));
            }
        }
        // No lockout by identifier: nothing is ever counted per public key.
        let w = self.wallet_challenges.value;
        if w.per_key_quota.is_some() || w.per_browser_and_key != 1 {
            return Err(PolicyError::Value("wallet_challenges"));
        }
        if self.flow_sweep.value.per_insert == 0 {
            return Err(PolicyError::Value("flow_sweep"));
        }
        let paths = &self.return_to_paths.value;
        let exact_ok = paths
            .exact
            .iter()
            .all(|p| p.starts_with('/') && !p.starts_with("//"));
        let prefix_ok = paths
            .prefixes
            .iter()
            .all(|p| p.len() > 1 && p.starts_with('/') && p.ends_with('/') && !p.starts_with("//"));
        if paths.exact.is_empty() || !exact_ok || !prefix_ok {
            return Err(PolicyError::Value("return_to_paths"));
        }
        let longest = paths
            .exact
            .iter()
            .chain(&paths.prefixes)
            .map(String::len)
            .max()
            .unwrap_or(1);
        if self.return_to_max.value < longest {
            return Err(PolicyError::Value("return_to_max"));
        }
        Ok(())
    }
}

/// The quota buckets a mail send draws on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BucketKind {
    /// `email:<addr_h>`: sign-in, step-up, attach-pending and recover mail.
    Email,
    /// `email_link:<addr_h>`: connect mail, per address.
    EmailLink,
    /// `link_mail:<user_id>`: connect mail, per requesting account.
    LinkMail,
}

impl BucketKind {
    /// The bucket's parameters in `policy`.
    #[must_use]
    pub fn bucket(self, policy: &AuthPolicy) -> Bucket {
        match self {
            Self::Email => policy.email_ceiling.value,
            Self::EmailLink => policy.email_link_ceiling.value,
            Self::LinkMail => policy.link_mail_quota.value,
        }
    }
}

/// The quota key `email:<addr_h hex>`.
#[must_use]
pub fn email_key(addr_h: &[u8; 32]) -> String {
    format!("email:{}", hex::encode(addr_h))
}

/// The quota key `email_link:<addr_h hex>`.
#[must_use]
pub fn email_link_key(addr_h: &[u8; 32]) -> String {
    format!("email_link:{}", hex::encode(addr_h))
}

/// The quota key `link_mail:<user_id>`.
#[must_use]
pub fn link_mail_key(user_id: impl core::fmt::Display) -> String {
    format!("link_mail:{user_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate_and_round_trip() {
        let p = AuthPolicy::v1();
        p.validate().unwrap();
        assert_eq!(AuthPolicy::from_json(&p.to_json()).unwrap(), p);
        assert_eq!(p.email_ceiling.value.refill_every_s, 8640);
        assert_eq!(p.email_link_ceiling.value.refill_every_s, 28_800);
        assert_eq!(p.link_mail_quota.value.refill_every_s, 17_280);
        assert_eq!(p.reasons().len(), 15);
    }

    #[test]
    fn missing_reason_or_unknown_field_refused() {
        let json = AuthPolicy::v1().to_json();
        let no_reason = json.replacen(
            "\"reason\": \"a fresh sign-in at least twice a year\"",
            "\"reason\": \" \"",
            1,
        );
        assert_eq!(
            AuthPolicy::from_json(&no_reason),
            Err(PolicyError::Value("session_absolute_s"))
        );
        let dropped = json.replacen(
            ",\n    \"reason\": \"a fresh sign-in at least twice a year\"",
            "",
            1,
        );
        assert!(matches!(
            AuthPolicy::from_json(&dropped),
            Err(PolicyError::Schema(_))
        ));
        let extra = json.replacen("{", "{\"surprise\": 1,", 1);
        assert!(matches!(
            AuthPolicy::from_json(&extra),
            Err(PolicyError::Schema(_))
        ));
        let v2 = json.replacen("auth-policy/v1", "auth-policy/v2", 1);
        assert_eq!(AuthPolicy::from_json(&v2), Err(PolicyError::Version));
    }

    #[test]
    fn structural_rules() {
        let mut p = AuthPolicy::v1();
        p.wallet_challenges.value.per_key_quota = Some(5);
        assert_eq!(p.validate(), Err(PolicyError::Value("wallet_challenges")));
        let mut p = AuthPolicy::v1();
        p.return_to_paths.value.prefixes.push("/".to_owned());
        assert_eq!(p.validate(), Err(PolicyError::Value("return_to_paths")));
        let mut p = AuthPolicy::v1();
        p.return_to_max.value = 5;
        assert_eq!(p.validate(), Err(PolicyError::Value("return_to_max")));
        let mut p = AuthPolicy::v1();
        p.session_renew_after_s.value = p.session_idle_s.value;
        assert_eq!(
            p.validate(),
            Err(PolicyError::Value("session_renew_after_s"))
        );
    }

    #[test]
    fn bucket_arithmetic() {
        let b = AuthPolicy::v1().email_ceiling.value;
        let t0 = 1_000_000;
        let mut state = None;
        for i in 0..10 {
            match b.take(state, t0) {
                Take::Taken { state: s, emptied } => {
                    assert_eq!(emptied, i == 9);
                    state = Some(s);
                }
                Take::Empty { .. } => panic!("send {i} refused"),
            }
        }
        // The eleventh inside 24 hours is refused.
        let Take::Empty { state: s } = b.take(state, t0 + 1000) else {
            panic!("eleventh send taken")
        };
        // One refill after 2.4 hours, not before.
        assert!(matches!(
            b.take(Some(s), t0 + 8_639_999),
            Take::Empty { .. }
        ));
        let Take::Taken { state: s, emptied } = b.take(Some(s), t0 + 8_640_000) else {
            panic!("refill not credited")
        };
        assert!(emptied);
        // A full day refills the whole bucket and no more.
        let full = b.refill(s, t0 + 8_640_000 + 30 * 86_400_000);
        assert_eq!(full.tokens, 10);
        // The operator's reset_quota is a full bucket.
        assert_eq!(b.full(5).tokens, 10);
    }

    #[test]
    fn keys() {
        assert_eq!(email_key(&[0xab; 32]), format!("email:{}", "ab".repeat(32)));
        assert_eq!(
            email_link_key(&[0; 32]),
            format!("email_link:{}", "00".repeat(32))
        );
        assert_eq!(link_mail_key(42), "link_mail:42");
    }
}
