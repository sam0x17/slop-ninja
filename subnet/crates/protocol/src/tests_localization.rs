//! The span gate's public record: its own bits as a counted term of the
//! complete total, the bit bound, no value for a withheld or unexecuted
//! entrant, and no `below_anchor` outside an applied state.

use crate::fixtures_quant::TESTNET_QUANT;
use crate::fixtures_reward_v1::{self as fx, FIXTURE_QUANT, Spec, bp, chain, finalize_fixture, result_of, sigma_permille};
use crate::localization::{LocalizationMode, LocalizationOutcomeV1, LocalizationState, LocalizationValue, STATE_BITS, TERMS};
use crate::*;

fn specs() -> Vec<Spec> {
    vec![Spec::new(0x21, bp(9600)), Spec::new(0x22, bp(9500)), Spec::new(0x23, bp(9400)), Spec::new(0x24, bp(9300))]
}

fn result_with(specs: &[Spec], q: QuantParams) -> Result<QuantizedResult> {
    let mut m = fx::manifest(chain(), 7, 1000, sigma_permille(40), 0, specs);
    m.params.quant = q;
    let index = fx::default_index(&m.references);
    let mut p = fx::package(&m, bp(9100), None, specs)?;
    finalize_fixture(&mut p, &index, &mut fx::spec_measure(specs, 1000, |_, _| false))?;
    result_of(&p)
}

fn record_for(r: &QuantizedResult, entrants: Vec<Option<LocalizationValue>>) -> Result<LocalizationOutcomeV1> {
    Ok(LocalizationOutcomeV1 {
        chain: r.chain.clone(),
        cohort: r.cohort,
        cohort_label: r.cohort_label.clone(),
        cohort_manifest: r.cohort_manifest,
        result: r.digest()?,
        mode: LocalizationMode::Report,
        gate_policy: Digest([7; 32]),
        anchor_content: Digest([9; 32]),
        state: LocalizationState::Applied,
        bits: LocalizationOutcomeV1::count_bits(&entrants),
        entrants,
        // The complete counted total when the gate is the plan's only term.
        result_bits: r.budget.total + TERMS.bits(r.budget.executed)?,
        cases: r.cases,
        identify_bits: r.params.quant.identify_bits,
        budget_factor: r.params.quant.budget_factor,
    })
}

#[test]
fn the_record_counts_its_bits_and_round_trips_signed() -> Result<()> {
    let r = result_with(&specs(), FIXTURE_QUANT)?;
    let values = vec![Some(LocalizationValue::Passed), Some(LocalizationValue::BelowAnchor), None, Some(LocalizationValue::NoTokenHead)];
    let rec = record_for(&r, values)?;
    assert_eq!(rec.bits, 3 + STATE_BITS);
    rec.validate()?;
    rec.check_result(&r)?;
    let key = SigningKey::from_bytes(&[5; 32]);
    let signed = sign(rec.clone(), &key)?;
    let bytes = signed.to_bytes()?;
    let back = Signed::<LocalizationOutcomeV1>::from_bytes(&bytes)?;
    assert_eq!(back.record, rec);
    // Bits that do not count the values are refused.
    let mut bad = rec.clone();
    bad.bits += 1;
    assert_eq!(bad.validate().unwrap_err(), Error::Mismatch("localization bits"));
    // Another result, or another roster length, is refused.
    let mut other = rec.clone();
    other.result = Digest([1; 32]);
    assert_eq!(other.check_result(&r).unwrap_err(), Error::Mismatch("localization result digest"));
    let mut short = rec.clone();
    short.entrants.pop();
    short.bits = LocalizationOutcomeV1::count_bits(&short.entrants);
    assert_eq!(short.check_result(&r).unwrap_err(), Error::Mismatch("localization roster"));
    // A total that leaves out the record's term (the result's budget alone)
    // is refused; one that also counts another rule's term is accepted.
    let mut alone = rec.clone();
    alone.result_bits = r.budget.total;
    assert_eq!(alone.check_result(&r).unwrap_err(), Error::Mismatch("localization bit parameters"));
    let mut more = rec.clone();
    more.result_bits += 5;
    more.check_result(&r)?;
    Ok(())
}

#[test]
fn below_anchor_needs_an_applied_state() -> Result<()> {
    let r = result_with(&specs(), FIXTURE_QUANT)?;
    let values = vec![Some(LocalizationValue::Passed), Some(LocalizationValue::BelowAnchor), None, Some(LocalizationValue::NoTokenHead)];
    for state in [LocalizationState::Waived, LocalizationState::Void] {
        let mut rec = record_for(&r, values.clone())?;
        rec.state = state;
        assert_eq!(rec.validate().unwrap_err(), Error::Invalid("localization below_anchor outside an applied state"));
        // Structural values and passed stay valid in any state.
        rec.entrants[1] = Some(LocalizationValue::DecoderUnderFloor);
        rec.validate()?;
    }
    Ok(())
}

#[test]
fn a_withheld_entrant_carries_no_value() -> Result<()> {
    let mut specs = specs();
    let k_n = 2;
    specs[2].failed = k_n + 1;
    let r = result_with(&specs, QuantParams { min_success_max_failed: k_n, ..FIXTURE_QUANT })?;
    assert_eq!(r.entrants[2].outcome, EntrantOutcome::Withheld);
    let ok = record_for(&r, vec![Some(LocalizationValue::Passed), Some(LocalizationValue::Passed), None, Some(LocalizationValue::Passed)])?;
    ok.check_result(&r)?;
    let leaky = record_for(&r, vec![Some(LocalizationValue::Passed), Some(LocalizationValue::Passed), Some(LocalizationValue::BelowAnchor), Some(LocalizationValue::Passed)])?;
    leaky.validate()?;
    assert_eq!(
        leaky.check_result(&r).unwrap_err(),
        Error::Invalid("localization value for a withheld or unexecuted entrant")
    );
    Ok(())
}

#[test]
fn the_complete_total_is_788_of_800_with_40_entrants() -> Result<()> {
    // n 1,000, eps 2, h 8, F 10: today's worst case is 746; the record's
    // counted term adds one bit per entrant and two for the state.
    let (w, ok) = quant::worst_case(1000, 40, 2000, 8, 10)?;
    assert!(ok);
    assert_eq!(w.total, 746);
    assert_eq!(TERMS.bits(40)?, 42);
    let (c, ok) = quant::worst_case_terms(1000, 40, 2000, 8, 10, &TERMS)?;
    assert!(ok);
    assert_eq!((c.quant.total, c.terms, c.total), (746, 42, 788));
    // Rosters of 41 and 42 entrants fit without the record but not with it.
    for roster in [41u16, 42] {
        let (_, ok) = quant::worst_case(1000, roster, 2000, 8, 10)?;
        assert!(ok, "{roster} entrants fit without the record");
        let (_, ok) = quant::worst_case_terms(1000, roster, 2000, 8, 10, &TERMS)?;
        assert!(!ok, "{roster}");
    }
    // A complete total past n h / F is refused.
    let r = result_with(&specs(), TESTNET_QUANT)?;
    let mut rec = record_for(&r, vec![Some(LocalizationValue::Passed); 4])?;
    rec.validate()?;
    rec.result_bits = 801;
    assert_eq!(rec.validate().unwrap_err(), Error::Invalid("localization bits beyond n h / F"));
    Ok(())
}
