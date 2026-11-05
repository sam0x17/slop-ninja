//! Spend: the cost of each call from the vendor's own usage and the
//! manifest's prices for the tier the call ran on, and per-cohort totals by
//! vendor, model and effort. Spend is tracked, never capped here: the only
//! thing that stops a vendor is the vendor's own spend-cap or quota answer
//! ([`crate::pause`]).

use crate::manifest::{ModelEntry, Price};
use crate::outcome::GenReply;
use crate::request::VendorRequest;
use crate::vendor::{Effort, Tier, Vendor};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A call's token usage, normalized across vendors: `input_tokens` are the
/// uncached input tokens (OpenAI reports cached ones inside its total;
/// Anthropic beside it), `output_tokens` include thinking or reasoning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    /// OpenAI `output_tokens_details.reasoning_tokens` (inside `output_tokens`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
}

/// The price of a prompt of `prompt_tokens` on `tier`: the tier's price, or
/// a documented condition's higher price once the prompt crosses it.
pub fn price_for(m: &ModelEntry, tier: Tier, prompt_tokens: u64) -> Option<Price> {
    let base = m.price(tier)?;
    let crossed = m.price_conditions.iter().filter(|c| prompt_tokens > c.prompt_tokens_above).max_by_key(|c| c.prompt_tokens_above);
    Some(match crossed {
        // A condition is written at standard rates; other tiers keep their ratio.
        Some(c) if tier == Tier::Standard => c.price,
        Some(c) => Price {
            input: scale(c.price.input, base.input, m.prices.standard.input),
            output: scale(c.price.output, base.output, m.prices.standard.output),
            cache_read: c.price.cache_read,
        },
        None => base,
    })
}

fn scale(cond: u64, tier: u64, standard: u64) -> u64 {
    if standard == 0 { cond } else { (u128::from(cond) * u128::from(tier)).div_ceil(u128::from(standard)) as u64 }
}

/// Cost in nano-USD, rounded up: prices are micro-USD per million tokens, so
/// one token at price `p` costs `p / 1000` nano-USD. Cached input without a
/// cache price is charged as input.
pub fn cost_nanousd(m: &ModelEntry, tier: Tier, u: &Usage) -> u64 {
    let p = price_for(m, tier, u.input_tokens + u.cached_input_tokens).unwrap_or(Price { input: 0, output: 0, cache_read: None });
    let cache = p.cache_read.unwrap_or(p.input);
    let total = u128::from(u.input_tokens) * u128::from(p.input)
        + u128::from(u.cached_input_tokens) * u128::from(cache)
        + u128::from(u.output_tokens) * u128::from(p.output);
    u64::try_from(total.div_ceil(1000)).unwrap_or(u64::MAX)
}

/// The most a sent call can be billed: every byte of its body a prompt token
/// (a token is at least one byte) and its requested output limit
/// ([`VendorRequest::max_tokens`], which the vendor never exceeds), at
/// `tier`'s prices.
pub fn reservation_nanousd(m: &ModelEntry, tier: Tier, r: &VendorRequest) -> u64 {
    let out = if r.max_tokens > 0 { u64::from(r.max_tokens) } else { u64::from(m.max_output_tokens) };
    cost_nanousd(m, tier, &Usage { input_tokens: r.body.len() as u64, cached_input_tokens: 0, output_tokens: out, reasoning_tokens: None })
}

/// What one call is charged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Charge {
    pub cost_nanousd: u64,
    /// The vendor reported no final usage for a call it may have billed:
    /// the charge is the reservation bound.
    pub unmeasured: bool,
}

/// The charge of `g`, the reply to `r` (`charge_v1`):
/// - final usage reported: its cost on the tier the call ran on;
/// - sent, no final usage, and the vendor may have generated (a 200 stream
///   that ended early, or a transport fault or timeout after sending, an
///   unreadable reply included): the larger of any partial usage's cost and
///   the reservation bound, marked unmeasured;
/// - not sent, or refused with a non-200 status before generating: nothing.
pub fn charge(m: &ModelEntry, r: &VendorRequest, g: &GenReply) -> Charge {
    if g.usage_reported {
        return Charge { cost_nanousd: cost_nanousd(m, g.tier, &g.usage), unmeasured: false };
    }
    if g.sent && matches!(g.http_status, 0 | 200) {
        let partial = cost_nanousd(m, g.tier, &g.usage);
        return Charge { cost_nanousd: partial.max(reservation_nanousd(m, g.tier, r)), unmeasured: true };
    }
    Charge::default()
}

/// Micro-USD, rounded up (the ledger's unit).
pub fn microusd(nanousd: u64) -> u64 {
    nanousd.div_ceil(1000)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Totals {
    pub calls: u64,
    pub unmeasured_calls: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost_nanousd: u64,
}

impl Totals {
    fn add(&mut self, u: &Usage, cost: u64, unmeasured: bool) {
        self.calls += 1;
        self.unmeasured_calls += u64::from(unmeasured);
        self.input_tokens += u.input_tokens;
        self.cached_input_tokens += u.cached_input_tokens;
        self.output_tokens += u.output_tokens;
        self.reasoning_tokens += u.reasoning_tokens.unwrap_or(0);
        self.cost_nanousd = self.cost_nanousd.saturating_add(cost);
    }
}

/// A cohort's generation spend (private cohort status and the eval ledger).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortSpend {
    pub by_vendor: BTreeMap<String, Totals>,
    /// Keyed `model/effort`.
    pub by_cell: BTreeMap<String, Totals>,
}

impl CohortSpend {
    pub fn record(&mut self, m: &ModelEntry, effort: Effort, usage: &Usage, cost: u64, unmeasured: bool) {
        self.by_vendor.entry(m.vendor.code().to_owned()).or_default().add(usage, cost, unmeasured);
        self.by_cell.entry(format!("{}/{}", m.id, effort.code())).or_default().add(usage, cost, unmeasured);
    }

    pub fn vendor_microusd(&self, v: Vendor) -> u64 {
        self.by_vendor.get(v.code()).map_or(0, |t| microusd(t.cost_nanousd))
    }
}

/// The SpendLedger key of a finished cohort's spend with one vendor, so the
/// monthly ledger and the private tracker see vendor spend apart from
/// Cloudflare's. `cohort_label` is the cohort's own ledger key
/// (`<instance>:<label>`), so the key is `gen:<vendor>:<instance>:<label>`.
pub fn ledger_key(v: Vendor, cohort_label: &str) -> String {
    format!("gen:{}:{cohort_label}", v.code())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::fixture::planned;

    #[test]
    fn costs_follow_the_reported_tier_and_conditions() {
        let m = planned();
        let luna = m.model("gpt-6-luna").unwrap();
        let u = Usage { input_tokens: 1_000_000, cached_input_tokens: 0, output_tokens: 1_000_000, reasoning_tokens: Some(10) };
        // $0.10 + $0.50 standard; half on Flex.
        assert_eq!(cost_nanousd(luna, Tier::Standard, &u), 600_000_000);
        assert_eq!(cost_nanousd(luna, Tier::Flex, &u), 300_000_000);
        // One token rounds up to a whole nano-USD.
        let one = Usage { input_tokens: 1, cached_input_tokens: 0, output_tokens: 0, reasoning_tokens: None };
        assert_eq!(cost_nanousd(luna, Tier::Standard, &one), 100);
        // Haiku past its 100K-token prompt condition.
        let haiku = m.model("claude-haiku-5-5").unwrap();
        let big = Usage { input_tokens: 100_001, cached_input_tokens: 0, output_tokens: 0, reasoning_tokens: None };
        assert_eq!(cost_nanousd(haiku, Tier::Standard, &big), 100_001 * 500);
        assert_eq!(cost_nanousd(haiku, Tier::Batch, &big), 100_001 * 250);
        let small = Usage { input_tokens: 1000, ..big };
        assert_eq!(cost_nanousd(haiku, Tier::Standard, &small), 1000 * 100);
        assert_eq!(microusd(1), 1);
        assert_eq!(ledger_key(Vendor::Anthropic, "c7"), "gen:anthropic:c7");
    }

    #[test]
    fn cohort_totals_are_kept_by_vendor_and_cell() {
        let m = planned();
        let mut s = CohortSpend::default();
        let sol = m.model("gpt-6.1-sol").unwrap();
        let u = Usage { input_tokens: 200, cached_input_tokens: 0, output_tokens: 800, reasoning_tokens: Some(600) };
        let c = cost_nanousd(sol, Tier::Flex, &u);
        s.record(sol, Effort::High, &u, c, false);
        s.record(sol, Effort::High, &u, c, false);
        let opus = m.model("claude-opus-5-5").unwrap();
        let (req, _) = crate::measure::request(&m, &crate::voice::fixture::library(), "claude-opus-5-5", Effort::Max, 12).unwrap();
        let r = reservation_nanousd(opus, Tier::Standard, &req);
        s.record(opus, Effort::Max, &Usage::default(), r, true);
        assert_eq!(s.by_vendor["openai"].calls, 2);
        assert_eq!(s.by_vendor["openai"].reasoning_tokens, 1200);
        assert_eq!(s.by_cell["gpt-6.1-sol/high"].cost_nanousd, 2 * c);
        assert_eq!(s.by_vendor["anthropic"].unmeasured_calls, 1);
        // Planned: the request asks for the documented maximum output, and
        // every body byte counts as a prompt token.
        assert_eq!(req.max_tokens, 128_000);
        assert_eq!(r, req.body.len() as u64 * 4_000 + 128_000 * 20_000);
        assert_eq!(s.vendor_microusd(Vendor::Openai), microusd(2 * c));
    }

    #[test]
    fn every_call_the_vendor_may_have_billed_is_charged() {
        use crate::outcome::{GenReply, OutcomeCode};
        let m = crate::manifest::fixture::measured(planned());
        let lib = crate::voice::fixture::library();
        let opus = m.model("claude-opus-5-5").unwrap();
        let (req, _) = crate::measure::request(&m, &lib, "claude-opus-5-5", Effort::High, 2).unwrap();
        let reserve = reservation_nanousd(opus, Tier::Standard, &req);
        // Measured: the request's own limit bounds the reservation.
        assert!(req.max_tokens < opus.max_output_tokens);
        assert_eq!(reserve, req.body.len() as u64 * 4_000 + u64::from(req.max_tokens) * 20_000);
        let mut g = GenReply::without_answer(OutcomeCode::Answer, &req, 10);
        // Not sent (no key, a refused admission, an interface mismatch): nothing.
        assert_eq!(charge(opus, &req, &g), Charge::default());
        // A 200 stream cut after message_start: partial usage, the reservation.
        g.sent = true;
        g.http_status = 200;
        g.outcome = OutcomeCode::Outage;
        g.usage = Usage { input_tokens: 900, cached_input_tokens: 0, output_tokens: 1, reasoning_tokens: None };
        assert_eq!(charge(opus, &req, &g), Charge { cost_nanousd: reserve, unmeasured: true });
        // A timeout or transport fault after sending, or an unreadable reply.
        g.http_status = 0;
        g.usage = Usage::default();
        assert_eq!(charge(opus, &req, &g), Charge { cost_nanousd: reserve, unmeasured: true });
        // A vendor refusal before generating (429, 400, 5xx): nothing billed.
        g.http_status = 429;
        assert_eq!(charge(opus, &req, &g), Charge::default());
        // Final usage: its cost.
        g.http_status = 200;
        g.usage_reported = true;
        g.usage = Usage { input_tokens: 900, cached_input_tokens: 0, output_tokens: 700, reasoning_tokens: None };
        assert_eq!(charge(opus, &req, &g), Charge { cost_nanousd: 900 * 4_000 + 700 * 20_000, unmeasured: false });
    }
}
