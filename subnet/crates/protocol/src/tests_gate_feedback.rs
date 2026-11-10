//! The gate feedback record: classes from the private failures, the
//! result's outcomes and reasons, its counted term (`max(g - 1, 0)` bits per
//! executed entrant), the bit bound, and every inconsistency refused.

use crate::fixtures_quant::TESTNET_QUANT;
use crate::fixtures_reward_v1::{self as fx, FIXTURE_QUANT, Spec, bp, chain, finalize_fixture, result_of, sigma_permille};
use crate::gate_feedback::{self, EntrantFeedback, GateClasses, GateFeedbackV1, GateThresholds};
use crate::live::{LiveParams, segments_table};
use crate::*;

/// The testnet template's gates: share 0.3, attribution 0 (never fails),
/// loose tier 100 per mille at the 0.5 operating point, strict 50.
const TESTNET_GATES: GateThresholds = GateThresholds {
    provisional: true,
    operating_ppb: 1_000_000_000 / 2,
    loose_fpr_max_permille: 100,
    strict_fpr_max_permille: 50,
    share_mae_max_ppb: 300_000_000,
    attribution_min_permille: 0,
    span_gate: false,
};

fn failure(gate: Gate, reason: &str) -> GateFailure {
    GateFailure { gate, reason: reason.into() }
}

/// Six entrants: one passing, one failing share, one failing share and the
/// loose tier, one withheld, one exact copy of `R_k` (never executed) and
/// one that failed qualification.
fn roster() -> Vec<Spec> {
    let mut withheld = Spec::new(0x24, bp(9300));
    withheld.failed = 3;
    vec![
        Spec::new(0x21, bp(9600)),
        Spec::new(0x22, bp(9500)).failing(Gate::Share, "share.mae_above_threshold"),
        Spec::new(0x23, bp(9400)),
        withheld,
        Spec::published_copy(0x25, 0xe1),
        Spec::new(0x26, bp(9200)).failing(Gate::Qualification, "qualification.embedded_text_index"),
    ]
}

fn cohort(quant: QuantParams) -> Result<(CohortManifestV2, ScorePackageV2, QuantizedResult)> {
    let specs = roster();
    let mut m = fx::manifest(chain(), 7, 1000, sigma_permille(40), 1, &specs);
    m.params.quant = quant;
    let index = fx::default_index(&m.references);
    let mut p = fx::package(&m, bp(9100), Some(bp(8800)), &specs)?;
    p.entrants[2].gate_failures = vec![
        failure(Gate::Share, "share.mae_above_threshold"),
        failure(Gate::HumanFalsePositive, HUMAN_FPR_LOOSE_REASON),
    ];
    finalize_fixture(&mut p, &index, &mut fx::spec_measure(&specs, 1000, |_, _| false))?;
    let r = result_of(&p)?;
    Ok((m, p, r))
}

fn fixture_quant() -> QuantParams {
    QuantParams { min_success_max_failed: 2, ..FIXTURE_QUANT }
}

fn record(p: &ScorePackageV2, r: &QuantizedResult, m: &CohortManifestV2, t: GateThresholds) -> Result<GateFeedbackV1> {
    let bits = gate_feedback::terms(&t).bits(r.budget.executed)?;
    GateFeedbackV1::of_package(p, r, m.gate_policy, t, r.budget.total + bits)
}

#[test]
fn the_record_publishes_classes_and_reasons_and_counts_its_bits() -> Result<()> {
    let (m, p, r) = cohort(fixture_quant())?;
    assert_eq!(r.entrants[3].outcome, EntrantOutcome::Withheld);
    assert_eq!(r.entrants[4].outcome, EntrantOutcome::NotExecuted(NotExecuted::Content));
    assert_eq!(r.entrants[5].outcome, EntrantOutcome::NotExecuted(NotExecuted::Qualification));
    let rec = record(&p, &r, &m, TESTNET_GATES)?;
    assert_eq!(
        rec.entrants,
        vec![
            EntrantFeedback::Scored(GateClasses::NONE),
            EntrantFeedback::Scored(GateClasses::SHARE),
            EntrantFeedback::Scored(GateClasses::SHARE.union(GateClasses::HUMAN_FPR)),
            EntrantFeedback::Withheld,
            EntrantFeedback::NotExecuted(NotExecuted::Content),
            EntrantFeedback::NotExecuted(NotExecuted::Qualification),
        ]
    );
    assert_eq!(rec.entrants[2].classes().map(GateClasses::codes), Some(vec!["share", "human_fpr"]));
    assert_eq!((NotExecuted::Content.code(), NotExecuted::Qualification.code()), ("copy", "qualification"));
    // Four executed entrants (the withheld one included) at one bit each.
    assert_eq!((rec.executed(), rec.bits), (4, 4));
    assert_eq!(rec.result_bits, r.budget.total + 4);
    rec.validate()?;
    rec.check_result(&r)?;
    rec.check_manifest(&m)?;
    let signed = sign(rec.clone(), &fixtures::package_key())?;
    let bytes = signed.to_bytes()?;
    assert!(bytes.len() as u64 <= GateFeedbackV1::max_signed_len()?);
    let back = Signed::<GateFeedbackV1>::from_bytes(&bytes)?;
    assert_eq!(back.record, rec);
    // The reason codes never enter the record: the same classes from other
    // codes give the same bytes.
    let mut renamed = p.clone();
    renamed.entrants[1].gate_failures[0].reason = "share.other_code".into();
    assert_eq!(GateFeedbackV1::of_package(&renamed, &r, m.gate_policy, TESTNET_GATES, rec.result_bits)?, rec);
    Ok(())
}

#[test]
fn the_term_is_g_minus_one_bits_per_executed_entrant() -> Result<()> {
    let t = |share: u32, attribution: u32, loose: u32| GateThresholds {
        share_mae_max_ppb: share,
        attribution_min_permille: attribution,
        loose_fpr_max_permille: loose,
        ..TESTNET_GATES
    };
    // Testnet: share and the loose tier can fail, attribution 0 cannot.
    assert_eq!(TESTNET_GATES.can_fail(), GateClasses::SHARE.union(GateClasses::HUMAN_FPR));
    assert_eq!(gate_feedback::terms(&TESTNET_GATES), quant::CountedTerms { per_executed: 1, per_cohort: 0 });
    // All three classes: two bits; one class: none (the gate bit says it);
    // no class: none.
    assert_eq!(gate_feedback::terms(&t(300_000_000, 500, 100)).per_executed, 2);
    assert_eq!(gate_feedback::terms(&t(1_000_000_000, 0, 100)).per_executed, 0);
    assert_eq!(gate_feedback::terms(&t(1_000_000_000, 0, 1000)).per_executed, 0);
    assert_eq!(t(1_000_000_000, 0, 1000).can_fail(), GateClasses::NONE);
    // The span flag changes no class: the span value is its own record's.
    assert_eq!(gate_feedback::terms(&GateThresholds { span_gate: true, ..TESTNET_GATES }).per_executed, 1);
    Ok(())
}

/// n 1,000, eps 2, h 8, F 10 with 40 entrants, the roster limit: today's worst case is
/// 746 of 800; the record adds 40 (786, fits); with the span gate's record
/// (X + 2) as well it is 828 and the plan is refused, fitting only rosters of
/// up to 38 entrants (785; 806 at 39).
#[test]
fn the_testnet_budget_with_the_record() -> Result<()> {
    let feedback = gate_feedback::terms(&TESTNET_GATES);
    let (c, ok) = quant::worst_case_terms(1000, 40, 2000, 8, 10, &feedback)?;
    assert!(ok);
    assert_eq!((c.quant.total, c.terms, c.total), (746, 40, 786));
    let both = feedback.plus(crate::localization::TERMS);
    let (c, ok) = quant::worst_case_terms(1000, 40, 2000, 8, 10, &both)?;
    assert!(!ok);
    assert_eq!((c.quant.total, c.terms, c.total), (746, 82, 828));
    let (c, ok) = quant::worst_case_terms(1000, 38, 2000, 8, 10, &both)?;
    assert_eq!((c.total, ok), (785, true));
    let (c, ok) = quant::worst_case_terms(1000, 39, 2000, 8, 10, &both)?;
    assert_eq!((c.total, ok), (806, false));
    // The `live-v1` segment count takes what is left (testnet's live block:
    // eps_live 1, at most four segments): L (without, with a predecessor)
    // drops at 29 to 31, 34 and 35 executed entrants and nowhere else.
    let live = LiveParams { eps_live_milli: 1000, segments_max: 4 };
    let without = segments_table(1000, 40, &TESTNET_QUANT, &live, &quant::CountedTerms::NONE)?;
    let with = segments_table(1000, 40, &TESTNET_QUANT, &live, &feedback)?;
    let changed: Vec<_> = (0..=40usize).filter(|&x| without[x] != with[x]).map(|x| (x, without[x], with[x])).collect();
    assert_eq!(
        changed,
        vec![
            (29, (4, 4), (4, 3)),
            (30, (4, 3), (3, 2)),
            (31, (3, 3), (2, 2)),
            (34, (2, 2), (2, 0)),
            (35, (2, 2), (0, 0)),
        ]
    );
    Ok(())
}

#[test]
fn the_record_refuses_values_the_result_does_not_support() -> Result<()> {
    let (m, p, r) = cohort(fixture_quant())?;
    let rec = record(&p, &r, &m, TESTNET_GATES)?;
    let refused = |f: &dyn Fn(&mut GateFeedbackV1)| {
        let mut x = rec.clone();
        f(&mut x);
        x.validate().and_then(|_| x.check_result(&r)).unwrap_err()
    };
    assert_eq!(
        refused(&|x| x.entrants[0] = EntrantFeedback::Scored(GateClasses::SHARE)),
        Error::Invalid("feedback class for a passing entrant")
    );
    assert_eq!(
        refused(&|x| x.entrants[1] = EntrantFeedback::Scored(GateClasses::NONE)),
        Error::Invalid("feedback failing entrant without a class")
    );
    // Under an enforced span gate a span-only failure has no class here.
    let mut span = rec.clone();
    span.thresholds.span_gate = true;
    span.entrants[1] = EntrantFeedback::Scored(GateClasses::NONE);
    span.validate()?;
    span.check_result(&r)?;
    assert_eq!(span.check_manifest(&m).unwrap_err(), Error::Mismatch("feedback gate policy"));
    // A class the thresholds cannot fail (attribution at 0) is refused.
    assert_eq!(
        refused(&|x| x.entrants[1] = EntrantFeedback::Scored(GateClasses::ATTRIBUTION)),
        Error::Invalid("feedback class the thresholds cannot fail")
    );
    // A class for the withheld entrant, an executed value for an unexecuted
    // one (refused for its bits, and with its bits for its outcome), or
    // another not-executed reason.
    assert_eq!(
        refused(&|x| x.entrants[3] = EntrantFeedback::Scored(GateClasses::SHARE)),
        Error::Mismatch("feedback outcome")
    );
    assert_eq!(refused(&|x| x.entrants[4] = EntrantFeedback::Withheld), Error::Mismatch("feedback bits"));
    assert_eq!(
        refused(&|x| {
            x.entrants[4] = EntrantFeedback::Withheld;
            x.bits += 1;
            x.result_bits += 1;
        }),
        Error::Mismatch("feedback outcome")
    );
    assert_eq!(
        refused(&|x| x.entrants[4] = EntrantFeedback::NotExecuted(NotExecuted::Qualification)),
        Error::Mismatch("feedback outcome")
    );
    // Another result, another roster, bits that do not count the term, a
    // total that leaves the term out or passes n h / F.
    assert_eq!(refused(&|x| x.result = Digest([1; 32])), Error::Mismatch("feedback result digest"));
    assert_eq!(
        refused(&|x| {
            x.entrants.pop();
        }),
        Error::Mismatch("feedback roster")
    );
    assert_eq!(refused(&|x| x.bits += 1), Error::Mismatch("feedback bits"));
    assert_eq!(refused(&|x| x.result_bits = r.budget.total), Error::Mismatch("feedback bit parameters"));
    assert_eq!(refused(&|x| x.result_bits = u32::MAX), Error::Invalid("feedback bits beyond n h / F"));
    assert_eq!(refused(&|x| x.thresholds.operating_ppb = 0), Error::OutOfRange("gate thresholds"));
    // The manifest binds the policy digest.
    let mut other = rec.clone();
    other.gate_policy = Digest([9; 32]);
    assert_eq!(other.check_manifest(&m).unwrap_err(), Error::Mismatch("feedback gate policy"));
    // A package whose private gates the thresholds cannot fail builds no
    // record.
    let mut odd = p.clone();
    odd.entrants[1].gate_failures = vec![failure(Gate::Attribution, "attribution.coverage_below_threshold")];
    assert!(GateFeedbackV1::of_package(&odd, &r, m.gate_policy, TESTNET_GATES, rec.result_bits).is_err());
    Ok(())
}

#[test]
fn the_record_fits_the_testnet_bound_or_is_refused() -> Result<()> {
    let (m, p, r) = cohort(QuantParams { min_success_max_failed: 2, ..TESTNET_QUANT })?;
    let rec = record(&p, &r, &m, TESTNET_GATES)?;
    assert!(rec.result_bits <= 800);
    let mut over = rec.clone();
    over.result_bits = 801;
    assert_eq!(over.validate().unwrap_err(), Error::Invalid("feedback bits beyond n h / F"));
    Ok(())
}

#[test]
fn unknown_class_bits_and_tags_do_not_decode() {
    for bits in [8u8, 0x80, 0xff] {
        assert_eq!(GateClasses::from_bits(bits), None);
        assert_eq!(
            <GateClasses as Canonical>::decode_from(&mut Decoder::new(&[bits])).unwrap_err(),
            Error::InvalidTag { field: "gate_classes", tag: bits }
        );
    }
    assert_eq!(GateClasses::from_bits(7).map(GateClasses::count), Some(3));
    assert_eq!(
        <EntrantFeedback as Canonical>::decode_from(&mut Decoder::new(&[4])).unwrap_err(),
        Error::InvalidTag { field: "entrant_feedback", tag: 4 }
    );
}
