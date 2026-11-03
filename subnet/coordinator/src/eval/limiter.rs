//! The shared Workers AI rate limiter (pure): token buckets with AIMD, held in
//! memory by the `GenLimiter` Durable Object and shared by every generation lane
//! of the deployment (main and load instances).
//!
//! The Workers AI limits page gives text generation 300 requests a minute "per
//! task type, with some per-model limits", without saying whether 300 applies
//! per model or account-wide. Until that is measured, a **global** bucket
//! enforces the stricter reading, and a bucket **per model** the other. A token
//! bucket of rate r a minute and burst b admits up to r + b requests in any
//! 60 s window, so the configured rate sits below the documented limit.
//!
//! AIMD: a throttle (429 or capacity) halves that model's rate (to a floor) and
//! pauses its bucket briefly; a run of successes raises it 5 % at a time, up to
//! the configured rate. Lanes beyond rate x latency in flight would only earn
//! 429s, so the limiter, not the lane count, sets generation throughput.
//! A reset (eviction, deploy) restarts at the configured rate, which is safe.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LimiterConfig {
    pub global_rpm: u32,
    pub model_rpm: u32,
    pub burst: u32,
    /// The AIMD floor, requests a minute.
    pub floor_rpm: u32,
    /// Consecutive successes before a 5 % raise.
    pub raise_after: u32,
    /// How long a throttled model's bucket stays empty.
    pub penalty_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Bucket {
    pub rpm: f64,
    pub max_rpm: f64,
    pub tokens: f64,
    pub burst: f64,
    pub last_ms: u64,
    pub paused_until_ms: u64,
}

impl Bucket {
    fn new(rpm: u32, burst: u32, now: u64) -> Self {
        Self {
            rpm: f64::from(rpm),
            max_rpm: f64::from(rpm),
            tokens: f64::from(burst),
            burst: f64::from(burst),
            last_ms: now,
            paused_until_ms: 0,
        }
    }

    fn refill(&mut self, now: u64) {
        let start = self.last_ms.max(self.paused_until_ms);
        if now > start {
            self.tokens = (self.tokens + (now - start) as f64 * self.rpm / 60_000.0).min(self.burst);
        }
        self.last_ms = self.last_ms.max(now);
    }

    fn available(&self, now: u64) -> u32 {
        if now < self.paused_until_ms { 0 } else { self.tokens.floor().max(0.0) as u32 }
    }

    /// Milliseconds until one whole token is available.
    fn wait_ms(&self, now: u64) -> u64 {
        let pause = self.paused_until_ms.saturating_sub(now);
        let missing = (1.0 - self.tokens).max(0.0);
        pause + (missing * 60_000.0 / self.rpm.max(0.001)).ceil() as u64
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ModelStats {
    pub granted: u64,
    pub refunded: u64,
    pub ok: u64,
    pub throttled: u64,
    pub unavailable: u64,
    pub errors: u64,
    pub ok_in_row: u32,
    /// Sanitized, content-free error texts seen (first few distinct), for M10.
    pub error_texts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Limiter {
    pub config: LimiterConfig,
    pub global: Bucket,
    pub models: BTreeMap<String, Bucket>,
    pub stats: BTreeMap<String, ModelStats>,
}

/// Grants per model, or when to ask again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grants {
    pub grants: BTreeMap<String, u32>,
    pub retry_at_ms: Option<u64>,
}

/// One finished call, as a lane reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feedback {
    pub model: String,
    /// `ok`, `throttled`, `unavailable` or `error`.
    pub result: String,
    #[serde(default)]
    pub error_text: Option<String>,
}

/// Keeps only characters that cannot carry prose structure beyond a short code
/// message, and at most 120 of them.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric() || " :._-()/,'".contains(*c))
        .take(120)
        .collect()
}

impl Limiter {
    pub fn new(config: LimiterConfig, now: u64) -> Self {
        Self {
            global: Bucket::new(config.global_rpm, config.burst, now),
            config,
            models: BTreeMap::new(),
            stats: BTreeMap::new(),
        }
    }

    fn model(&mut self, m: &str, now: u64) -> &mut Bucket {
        let (rpm, burst) = (self.config.model_rpm, self.config.burst);
        self.models.entry(m.to_owned()).or_insert_with(|| Bucket::new(rpm, burst, now))
    }

    /// Grants up to `slots` calls in total, spread round-robin over `models`,
    /// each taking a token from both the global and the model's bucket.
    pub fn acquire(&mut self, models: &[String], slots: u32, now: u64) -> Grants {
        self.global.refill(now);
        for m in models {
            self.model(m, now).refill(now);
        }
        let mut grants: BTreeMap<String, u32> = BTreeMap::new();
        let mut left = slots;
        loop {
            let mut progress = false;
            for m in models {
                if left == 0 || self.global.available(now) == 0 {
                    break;
                }
                let b = self.model(m, now);
                if b.available(now) == 0 {
                    continue;
                }
                b.tokens -= 1.0;
                self.global.tokens -= 1.0;
                *grants.entry(m.clone()).or_default() += 1;
                self.stats.entry(m.clone()).or_default().granted += 1;
                left -= 1;
                progress = true;
            }
            if !progress || left == 0 {
                break;
            }
        }
        if !grants.is_empty() || models.is_empty() {
            return Grants { grants, retry_at_ms: None };
        }
        let model_wait = models.iter().map(|m| self.models[m].wait_ms(now)).min().unwrap_or(0);
        let wait = self.global.wait_ms(now).max(model_wait).clamp(50, 60_000);
        Grants { grants, retry_at_ms: Some(now + wait) }
    }

    /// Returns tokens a lane was granted but did not use.
    pub fn refund(&mut self, model: &str, n: u32, now: u64) {
        let n = f64::from(n);
        let b = self.model(model, now);
        b.tokens = (b.tokens + n).min(b.burst);
        self.global.tokens = (self.global.tokens + n).min(self.global.burst);
        self.stats.entry(model.to_owned()).or_default().refunded += n as u64;
    }

    pub fn feedback(&mut self, f: &Feedback, now: u64) {
        let cfg = self.config;
        let raise = {
            let st = self.stats.entry(f.model.clone()).or_default();
            if let Some(t) = &f.error_text {
                let t = sanitize(t);
                if st.error_texts.len() < 8 && !st.error_texts.contains(&t) {
                    st.error_texts.push(t);
                }
            }
            match f.result.as_str() {
                "ok" => {
                    st.ok += 1;
                    st.ok_in_row += 1;
                    if st.ok_in_row >= cfg.raise_after {
                        st.ok_in_row = 0;
                        Some(true)
                    } else {
                        None
                    }
                }
                "throttled" => {
                    st.throttled += 1;
                    st.ok_in_row = 0;
                    Some(false)
                }
                "unavailable" => {
                    st.unavailable += 1;
                    None
                }
                _ => {
                    st.errors += 1;
                    None
                }
            }
        };
        let b = self.model(&f.model, now);
        match raise {
            Some(true) => b.rpm = (b.rpm * 1.05).min(b.max_rpm),
            Some(false) => {
                b.refill(now);
                b.rpm = (b.rpm / 2.0).max(f64::from(cfg.floor_rpm));
                b.tokens = 0.0;
                b.paused_until_ms = now + cfg.penalty_ms;
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> LimiterConfig {
        LimiterConfig { global_rpm: 290, model_rpm: 290, burst: 10, floor_rpm: 10, raise_after: 50, penalty_ms: 5_000 }
    }

    #[test]
    fn no_window_admits_more_than_rate_plus_burst() {
        let models = vec!["a".to_owned(), "b".to_owned()];
        let mut l = Limiter::new(cfg(), 0);
        let mut granted = 0;
        let mut t = 0;
        while t <= 60_000 {
            granted += l.acquire(&models, 6, t).grants.values().sum::<u32>();
            t += 100;
        }
        // 290 a minute plus the burst of 10: the documented 300 is never exceeded.
        assert!(granted <= 300, "{granted}");
        assert!(granted >= 295, "{granted}");
        // The global bucket bounds both models together.
        let per: u64 = l.stats.values().map(|s| s.granted).sum();
        assert_eq!(per, u64::from(granted));
    }

    #[test]
    fn empty_buckets_say_when_to_retry_and_refunds_return_tokens() {
        let models = vec!["a".to_owned()];
        let mut l = Limiter::new(cfg(), 0);
        assert_eq!(l.acquire(&models, 100, 0).grants["a"], 10);
        let g = l.acquire(&models, 1, 0);
        assert!(g.grants.is_empty());
        let at = g.retry_at_ms.unwrap();
        assert!((200..=260).contains(&at), "one token at 290/min arrives in about 207 ms: {at}");
        assert_eq!(l.acquire(&models, 1, at).grants["a"], 1);
        l.refund("a", 3, at);
        assert_eq!(l.acquire(&models, 10, at).grants["a"], 3);
    }

    #[test]
    fn aimd_halves_on_a_throttle_and_recovers_slowly() {
        let models = vec!["a".to_owned(), "b".to_owned()];
        let mut l = Limiter::new(cfg(), 0);
        let throttle = Feedback { model: "a".into(), result: "throttled".into(),
            error_text: Some("AiError: 3040: Capacity temporarily exceeded, please try again.".into()) };
        l.feedback(&throttle, 1000);
        assert_eq!(l.models["a"].rpm, 145.0);
        // The throttled model is paused; the other keeps its tokens.
        let g = l.acquire(&models, 4, 1000);
        assert_eq!((g.grants.get("a"), g.grants.get("b")), (None, Some(&4)));
        for _ in 0..10 {
            l.feedback(&throttle, 2000);
        }
        assert_eq!(l.models["a"].rpm, 10.0, "floored");
        let ok = Feedback { model: "a".into(), result: "ok".into(), error_text: None };
        for _ in 0..50 {
            l.feedback(&ok, 3000);
        }
        assert_eq!(l.models["a"].rpm, 10.5);
        assert_eq!(l.stats["a"].error_texts.len(), 1, "distinct texts only");
        assert_eq!(sanitize("x\ny<z>"), "xyz");
    }
}
