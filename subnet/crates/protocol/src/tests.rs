use crate::fixtures::{self, cohort, miner_a, miner_b, package, snapshot, trusted_keys};
use crate::reward::{self, ScoredTask};
use crate::*;

const GOLDEN: &str = include_str!("../fixtures/golden-v1.txt");

#[test]
fn golden_report_is_unchanged() -> Result<()> {
    let report = fixtures::golden_report()?;
    if report != GOLDEN {
        for (i, (a, b)) in report.lines().zip(GOLDEN.lines()).enumerate() {
            assert_eq!(a, b, "golden line {}", i + 1);
        }
        assert_eq!(
            report.lines().count(),
            GOLDEN.lines().count(),
            "golden length"
        );
    }
    assert_eq!(report, GOLDEN);
    Ok(())
}

fn round_trip<R: Record + PartialEq + core::fmt::Debug>(record: &R) -> Result<()> {
    let bytes = record.to_bytes()?;
    let decoded = R::from_bytes(&bytes)?;
    assert_eq!(&decoded, record);
    assert_eq!(decoded.to_bytes()?, bytes);
    Ok(())
}

#[test]
fn every_record_round_trips_canonically() -> Result<()> {
    let c = cohort()?;
    round_trip(&c.manifest)?;
    round_trip(&fixtures::entrants()[1].artifact)?;
    for a in &c.assignments {
        round_trip(a)?;
    }
    for a in &c.attempts {
        round_trip(a)?;
    }
    for r in &c.results {
        round_trip(r)?;
    }
    for r in &c.receipts {
        round_trip(r)?;
    }
    round_trip(&c.index)?;
    let p = package(&c)?;
    round_trip(&p)?;
    let t = CalculationTarget::for_package(&p, Some(Digest([9; 32])))?;
    round_trip(&t)?;
    round_trip(&snapshot())?;
    round_trip(&reward::derive_row(&t, &p, &snapshot())?)?;
    Ok(())
}

#[test]
fn operation_id_matches_native_adapter_journal_identity() -> Result<()> {
    // Value fixed by the native adapter's `stable_id_binds_all_application_and_chain_coordinates`.
    assert_eq!(
        weight_operation_id(&[1; 32], 7, &[2; 32], &[3; 32], "synthetic-cohort", 1, 0)?,
        "slopninja:weights:v1:c32bdbedef1c2eb2fe1d13fbb298d2df62e157cbff62b199a24b71c19980489d"
    );
    let c = cohort()?;
    let t = CalculationTarget::for_package(&package(&c)?, None)?;
    let a = WeightOperation::for_target(&t, AccountId([1; 32]));
    let b = WeightOperation::for_target(&t, AccountId([2; 32]));
    assert_eq!(a.id()?, a.id()?);
    assert_ne!(a.id()?, b.id()?);
    assert_eq!(a.manifest, c.manifest.digest()?);
    assert_eq!(a.netuid, fixtures::chain().netuid);
    Ok(())
}

#[test]
fn tempo_operation_ids_are_distinct_per_epoch_target_and_validator() -> Result<()> {
    let base = weight_operation_id(&[1; 32], 7, &[2; 32], &[3; 32], "synthetic-cohort", 1, 0)?;
    let other = weight_operation_id(&[1; 32], 7, &[4; 32], &[3; 32], "synthetic-cohort", 1, 0)?;
    let first = weight_tempo_operation_id(&base, 23_083)?;
    assert!(first.starts_with(WEIGHT_TEMPO_OPERATION_PREFIX));
    assert_eq!(first.len(), WEIGHT_TEMPO_OPERATION_PREFIX.len() + 64);
    assert_eq!(first, weight_tempo_operation_id(&base, 23_083)?);
    assert_ne!(first, weight_tempo_operation_id(&base, 23_084)?);
    assert_ne!(first, weight_tempo_operation_id(&other, 23_083)?);
    assert_ne!(first, base);
    // Only a base operation identity is accepted, never a tempo identity or text.
    for bad in [
        first.as_str(),
        "",
        "slopninja:weights:v1:00",
        "slopninja:weights:v1:",
    ] {
        assert_eq!(
            weight_tempo_operation_id(bad, 1),
            Err(Error::Invalid("base_operation_id"))
        );
    }
    let upper = base
        .to_uppercase()
        .replace("SLOPNINJA:WEIGHTS:V1:", "slopninja:weights:v1:");
    assert!(weight_tempo_operation_id(&upper, 1).is_err());
    Ok(())
}

#[test]
fn receipts_cover_every_attempt_and_bind_to_the_package() -> Result<()> {
    let c = cohort()?;
    assert_eq!(c.receipts.len(), c.attempts.len());
    assert_eq!(c.index.count(), c.attempts.len() as u64);
    let p = package(&c)?;
    p.check_receipts(&c.index)?;
    for receipt in &c.receipts {
        let position = c
            .index
            .entries
            .binary_search_by_key(&receipt.attempt, |e| e.attempt)
            .expect("indexed");
        assert_eq!(
            &c.index.verify_object(position, &receipt.to_bytes()?)?,
            receipt
        );
    }
    // The failed attempt keeps its weight in the miner's denominator.
    let b = p
        .scores
        .iter()
        .find(|s| s.identity == miner_b())
        .expect("b");
    assert_eq!((b.assigned, b.succeeded, b.failed), (3, 2, 1));
    Ok(())
}

#[test]
fn derived_row_applies_eligibility_reuse_and_quantization() -> Result<()> {
    let c = cohort()?;
    let p = package(&c)?;
    let t = CalculationTarget::for_package(&p, None)?;
    let row = reward::derive_row(&t, &p, &snapshot())?;
    let uids: Vec<_> = row.entries.iter().map(|e| e.uid).collect();
    // A on its UID; C's credit on UID 0 because its UID was reused; B, below
    // the baseline after a parse failure, and the perfect but unlisted
    // stranger on UID 9 earn nothing.
    assert_eq!(uids, vec![0, miner_a().uid]);
    assert_eq!(row.entries.iter().map(|e| e.weight).max(), Some(u16::MAX));
    assert_eq!(row, reward::derive_row(&t, &p, &snapshot())?);
    Ok(())
}

#[test]
fn empty_pool_sends_full_row_to_uid_zero() -> Result<()> {
    let c = cohort()?;
    let mut p = package(&c)?;
    p.baseline = Score::ONE;
    let t = CalculationTarget::for_package(&p, None)?;
    let row = reward::derive_row(&t, &p, &snapshot())?;
    assert_eq!(
        row.entries,
        vec![RowEntry {
            uid: 0,
            weight: u16::MAX
        }]
    );
    Ok(())
}

#[test]
fn quantization_rounds_half_up_and_fails_closed_on_overflow() -> Result<()> {
    let row = reward::quantize(&[(1, 1), (2, 2)])?;
    assert_eq!(row[0].weight, 32768);
    assert_eq!(row[1].weight, u16::MAX);
    assert_eq!(
        reward::quantize(&[(1, u128::MAX)]).unwrap_err(),
        Error::Overflow("quantize")
    );
    Ok(())
}

#[test]
fn brier_aggregation_is_exact() -> Result<()> {
    let p = |v| Probability::new(v).expect("in range");
    assert_eq!(reward::brier_utility(p(1_000_000_000), true), SCORE_SCALE);
    assert_eq!(reward::brier_utility(p(1_000_000_000), false), 0);
    let tasks = [
        ScoredTask {
            group: 0,
            involvement: Some(p(1_000_000_000)),
            label: true,
        },
        ScoredTask {
            group: 0,
            involvement: None,
            label: true,
        },
        ScoredTask {
            group: 1,
            involvement: Some(p(0)),
            label: false,
        },
    ];
    // Group 0 mean 0.5, group 1 mean 1, equal group weights: 0.75.
    assert_eq!(
        reward::aggregate_score(&tasks, 2)?.get(),
        750_000_000_000_000_000
    );
    Ok(())
}

#[test]
fn signed_records_reject_any_single_bit_flip() -> Result<()> {
    let c = cohort()?;
    let signed = sign(package(&c)?, &fixtures::package_key())?;
    let keys = trusted_keys();
    let bytes = signed.to_bytes()?;
    assert_eq!(Signed::<ScorePackage>::open(&bytes, &keys)?, signed.record);
    // The raw form returns exactly the record's canonical bytes.
    assert_eq!(
        Signed::<ScorePackage>::open_raw(&bytes, &keys)?,
        signed.record.to_bytes()?.as_slice()
    );
    for i in 0..bytes.len() {
        let mut tampered = bytes.clone();
        tampered[i] ^= 0x01;
        assert!(
            Signed::<ScorePackage>::open(&tampered, &keys).is_err(),
            "accepted flip at byte {i}"
        );
        // A flip in the envelope or the record fails authentication itself.
        assert!(
            Signed::<ScorePackage>::open_raw(&tampered, &keys).is_err(),
            "raw accepted flip at byte {i}"
        );
    }
    Ok(())
}

#[test]
fn signing_roles_are_not_interchangeable() -> Result<()> {
    let c = cohort()?;
    let keys = trusted_keys();
    let index = sign(c.index.clone(), &fixtures::receipt_key())?;
    index.verify(&keys)?;
    let forged = sign(c.index.clone(), &fixtures::package_key())?;
    assert_eq!(forged.verify(&keys).unwrap_err(), Error::UntrustedKey);
    assert_eq!(
        signing_payload(&c.receipts[0]).unwrap_err(),
        Error::WrongRole
    );
    Ok(())
}

#[test]
fn allow_list_identity_includes_registration_block() -> Result<()> {
    let c = cohort()?;
    let mut p = package(&c)?;
    // Same UID and hotkey, different registration generation: not eligible.
    p.allow_list[0].registration_block += 1;
    let t = CalculationTarget::for_package(&p, None)?;
    let row = reward::derive_row(&t, &p, &snapshot())?;
    assert!(row.entries.iter().all(|e| e.uid != miner_a().uid));
    Ok(())
}

#[test]
fn burn_target_is_bound_to_the_owner_identity() -> Result<()> {
    let c = cohort()?;
    let p = package(&c)?;
    let t = CalculationTarget::for_package(&p, None)?;
    let mut foreign = snapshot();
    foreign.neurons[0] = fixtures::foreign_uid0();
    assert_eq!(
        reward::derive_row(&t, &p, &foreign).unwrap_err(),
        Error::Mismatch("burn identity")
    );
    let mut moved = p.clone();
    moved.burn_identity = fixtures::foreign_uid0();
    assert_eq!(
        moved.check_manifest(&c.manifest).unwrap_err(),
        Error::Mismatch("package manifest")
    );
    moved.burn_identity.uid = 1;
    assert_eq!(
        moved.to_bytes().unwrap_err(),
        Error::Invalid("burn identity uid")
    );
    Ok(())
}

#[test]
fn testnet_reward_v0_fails_closed_off_testnet() -> Result<()> {
    let c = cohort()?;
    let mut p = package(&c)?;
    let t = CalculationTarget::for_package(&p, None)?;
    let refused = Error::Mismatch("testnet_reward_v0 genesis");
    p.chain = fixtures::other_chain();
    assert_eq!(p.to_bytes().unwrap_err(), refused);
    assert_eq!(
        ScorePackage::from_bytes(&fixtures::unvalidated_bytes(&p)).unwrap_err(),
        refused
    );
    let mut s = snapshot();
    s.chain = fixtures::other_chain();
    assert_eq!(reward::derive_row(&t, &p, &s).unwrap_err(), refused);
    let mut m = c.manifest.clone();
    m.chain = fixtures::other_chain();
    assert_eq!(m.to_bytes().unwrap_err(), refused);
    Ok(())
}
