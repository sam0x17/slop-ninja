//! Retained operator metadata for the privacy display after a cohort closes.
//!
//! This fixed-size report lives beside the private result. It is not a signed
//! protocol record and never changes a result, reward, row or validator input.
//! Its authenticity relies on the coordinator's private object-store boundary,
//! as the live report relies on its authenticated service binding. Digest checks
//! bind it to one signed result; they do not authenticate the live policy.
//! Missing metadata means unavailable, never zero live cost.

use crate::{BudgetBlock, Canonical, Decoder, Digest, Encoder, Error, QuantizedResult, Record, Result};

const PREFIX: &[u8] = b"slop_ninja.live_privacy/v1\0";
/// Exact size: version prefix, two digests, epsilon (u32), segments (u8).
pub const REPORT_BYTES: u64 = PREFIX.len() as u64 + 32 + 32 + 4 + 1;
pub const OBJECT: &str = "live-privacy.bin";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LivePrivacy {
    pub cohort_manifest: Digest,
    pub result: Digest,
    pub eps_live_milli: u32,
    pub segments: u8,
}

impl LivePrivacy {
    /// The full policy cost is retained even after a stop. Zero segments means
    /// a checked policy with no readings, not an absent observation.
    pub fn of(result: &QuantizedResult, eps_live_milli: u32, segments: u8) -> Result<Self> {
        let report = Self { cohort_manifest: result.cohort_manifest, result: result.digest()?, eps_live_milli, segments };
        report.check_result(result)?;
        Ok(report)
    }

    pub fn check_result(&self, result: &QuantizedResult) -> Result<()> {
        if !matches!(result.budget, BudgetBlock::V2(_)) || self.cohort_manifest != result.cohort_manifest
            || self.result != result.digest()? || self.segments == 1
            || u32::from(self.segments) > result.cases || (self.segments >= 2 && self.eps_live_milli == 0) {
            return Err(Error::Invalid("live privacy binding or policy"));
        }
        self.costs(result)?;
        Ok(())
    }

    /// Quant, live and combined rho in micro-units, rounding upward. No
    /// observed release count or stopped flag can reduce this policy bound.
    pub fn costs(&self, result: &QuantizedResult) -> Result<(u64, u64, u64)> {
        let BudgetBlock::V2(q) = &result.budget else { return Err(Error::Invalid("live privacy quant rule")) };
        let live = if self.segments >= 2 { u64::from(self.eps_live_milli).pow(2).div_ceil(2) } else { 0 };
        let total = q.rho_micro.checked_add(live).ok_or(Error::Overflow("live privacy total"))?;
        Ok((q.rho_micro, live, total))
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.raw(PREFIX);
        self.cohort_manifest.encode_to(&mut e);
        self.result.encode_to(&mut e);
        e.u32(self.eps_live_milli);
        e.u8(self.segments);
        e.into_bytes()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(bytes);
        if d.take(PREFIX.len())? != PREFIX { return Err(Error::Invalid("live privacy version")); }
        let out = Self { cohort_manifest: Digest::decode_from(&mut d)?, result: Digest::decode_from(&mut d)?,
            eps_live_milli: d.u32()?, segments: d.u8()? };
        d.finish()?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{QuantBlock, RewardRule, fixtures_quant2::TESTNET_QUANT2, fixtures_reward_v1 as fx};

    #[test]
    fn bounded_report_binds_the_exact_result_and_keeps_the_full_policy_cost() {
        let q = QuantBlock::V2(crate::QuantParamsV2 { identify_bits: u16::MAX, budget_factor: 1,
            min_success_max_failed: fx::FIXTURE_QUANT.min_success_max_failed, ..TESTNET_QUANT2 });
        let (_, package) = fx::computed_package(RewardRule::RewardV3, q).unwrap();
        let result = QuantizedResult::of_package(&package).unwrap();
        let p = LivePrivacy::of(&result, 1000, 4).unwrap();
        let bytes = p.to_bytes();
        assert_eq!(bytes.len() as u64, REPORT_BYTES);
        assert_eq!(LivePrivacy::from_bytes(&bytes).unwrap(), p);
        let (q, l, t) = p.costs(&result).unwrap();
        assert_eq!(l, 500_000);
        assert_eq!(t, q + l);
        let none = LivePrivacy::of(&result, 1000, 0).unwrap();
        assert_eq!(none.costs(&result).unwrap(), (q, 0, q));
        assert_eq!(LivePrivacy::of(&result, 1, 2).unwrap().costs(&result).unwrap().1, 1);
        assert!(LivePrivacy::of(&result, 0, 2).is_err());
        assert!(LivePrivacy::of(&result, 1000, 1).is_err());
        let mut wrong = p.clone();
        wrong.cohort_manifest = Digest([0; 32]);
        assert!(wrong.check_result(&result).is_err());
        let mut wrong = p.clone();
        wrong.result = Digest([0; 32]);
        assert!(wrong.check_result(&result).is_err());
        let mut extra = bytes.clone(); extra.push(0);
        assert!(LivePrivacy::from_bytes(&extra).is_err());
        for n in 0..bytes.len() { assert!(LivePrivacy::from_bytes(&bytes[..n]).is_err()); }
        let mut version = bytes; version[0] ^= 1;
        assert!(LivePrivacy::from_bytes(&version).is_err());
        let legacy = QuantizedResult::of_package(&fx::computed().unwrap().package).unwrap();
        assert!(p.check_result(&legacy).is_err());
    }
}
