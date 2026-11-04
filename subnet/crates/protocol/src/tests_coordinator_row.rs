//! The coordinator's signed row ([`crate::coordinator_row`]): what the copy
//! model's validators verify and commit.

use crate::coordinator_row::{self, CoordinatorRow, CoordinatorRowEntry, RowAuthorizing, RowSource};
use crate::fixtures_reward_v1::{self as fx, chain, finney};
use crate::{
    AccountId, ActiveTarget, Authorizing, CalculationTargetV2, Digest, Error, LaunchAuthorization, NeuronIdentity,
    Record, RegistrationSnapshotV2, Result, RewardIntentAuthorization, RewardIntentRenewal, RewardIntentRevocation,
    RewardRule, Signed, WeightOperationV2, sign, target_bundle,
};

fn launch_digest() -> Digest {
    CalculationTargetV2::launch_burn(chain()).digest().unwrap()
}

fn entry(uid: u16, tag: u8, weight: u16) -> CoordinatorRowEntry {
    CoordinatorRowEntry { uid, hotkey: AccountId([tag; 32]), registration_block: 5, weight }
}

/// A cohort authorization of `target` carried by cohort 7, issued at 300.
fn cohort_auth(target: Digest) -> RewardIntentAuthorization {
    RewardIntentAuthorization {
        chain: chain(),
        cohort: 7,
        cohort_manifest: Digest([0x71; 32]),
        score_package: Digest([0x72; 32]),
        target,
        checkpoint_generation: 3,
        issued_epoch: 300,
        renew_by_epoch: 340,
    }
}

fn cohort_row(a: &RewardIntentAuthorization, activated: Option<u64>, epoch: u64, entries: Vec<CoordinatorRowEntry>) -> CoordinatorRow {
    CoordinatorRow {
        chain: chain(),
        reward_rule: RewardRule::RewardV1,
        source: RowSource::Cohort { cohort: a.cohort, cohort_label: "testnet-c7".into(), cohort_manifest: a.cohort_manifest },
        target: a.target,
        activated_epoch: activated,
        authorization: a.digest().unwrap(),
        issued_epoch: a.issued_epoch,
        epoch,
        registration_block: 9_000,
        owner_hotkey: fx::owner_hotkey(),
        entries,
    }
}

fn launch_row(a: &LaunchAuthorization, epoch: u64) -> CoordinatorRow {
    CoordinatorRow {
        chain: chain(),
        reward_rule: RewardRule::RewardV1,
        source: RowSource::LaunchBurn,
        target: a.target,
        activated_epoch: None,
        authorization: a.digest().unwrap(),
        issued_epoch: a.issued_epoch,
        epoch,
        registration_block: 8_000,
        owner_hotkey: fx::owner_hotkey(),
        entries: vec![entry(4, 0x0f, u16::MAX)],
    }
}

fn bytes<R: Record>(r: R) -> Vec<u8> {
    fx::signed(r).unwrap().to_bytes().unwrap()
}

fn snapshot(neurons: &[(u16, u8)]) -> RegistrationSnapshotV2 {
    RegistrationSnapshotV2 {
        chain: chain(),
        block: 9_100,
        block_hash: Digest([2; 32]),
        owner_hotkey: fx::owner_hotkey(),
        neurons: neurons.iter().map(|&(uid, tag)| NeuronIdentity { uid, hotkey: AccountId([tag; 32]), registration_block: 5 }).collect(),
    }
}

#[test]
fn a_launch_row_verifies_and_commits_exactly_its_entries_with_no_end() -> Result<()> {
    let keys = crate::fixtures::trusted_keys();
    let held = ActiveTarget::launch(&chain())?;
    let a = LaunchAuthorization::new(&chain(), 1, 100)?;
    let auth = bytes(a.clone());
    let row = bytes(launch_row(&a, 120));
    let v = coordinator_row::verify(&chain(), &keys, &held, RowAuthorizing::Launch(&auth), None, &row)?;
    assert_eq!(v.active, held);
    assert_eq!(v.target_digest(), launch_digest());
    let terms = v.terms();
    assert_eq!((terms.digest, terms.issued_epoch, terms.renew_by_epoch), (a.digest()?, 100, 140));
    let hk = AccountId([0x66; 32]);
    // The same operation identity as the full launch bundle's.
    let full = target_bundle::verify(&chain(), &keys, &held, None, Authorizing::Launch { authorization: &auth, renewal: None })?;
    for e in [100, 140, 141, 1_000_000] {
        assert_eq!(v.operation(&[], &held, hk, e)?, full.operation(&keys, &[], hk, e)?);
    }
    assert_eq!(v.operation(&[], &held, hk, 99).unwrap_err(), Error::Invalid("epoch before authorization"));
    // The burn goes to the UID the holder's own snapshot's owner hotkey
    // holds; with none, the tempo's row is deferred.
    for (s, uid) in [(snapshot(&[(4, 0x0f)]), 4), (snapshot(&[(4, 0x55), (9, 0x0f)]), 9)] {
        let r = v.derive_row(&s)?;
        assert_eq!(r.entries, vec![crate::RowEntry { uid, weight: u16::MAX }]);
        assert_eq!((r.calculation_target, r.registration_block), (launch_digest(), 9_100));
    }
    assert_eq!(v.derive_row(&snapshot(&[])).unwrap_err(), Error::Deferred("owner hotkey not registered"));
    assert_eq!(v.moved_uids(&snapshot(&[(4, 0x0f)])), Vec::<u16>::new());
    assert_eq!(v.moved_uids(&snapshot(&[(4, 0x55)])), Vec::<u16>::new(), "the burn entry never counts as moved");
    // A launch row once a cohort target activated is refused.
    let activated = ActiveTarget::restore(&chain(), Digest([9; 32]), Some(200))?;
    assert_eq!(
        coordinator_row::verify(&chain(), &keys, &activated, RowAuthorizing::Launch(&auth), None, &row).unwrap_err(),
        Error::Invalid("launch burn row after activation")
    );
    assert_eq!(v.operation(&[], &activated, hk, 300).unwrap_err(), Error::Invalid("launch authorization after activation"));
    // A signed revocation of the burn row withdraws it; a later
    // authorization of the same target re-authorizes it.
    let checked = {
        let s = Signed::<LaunchAuthorization>::from_bytes(&auth)?;
        let c = LaunchAuthorization::check(&s, &keys, &chain(), &held)?;
        RewardIntentRevocation::new(&c, 1, 150)?
    };
    let journal = std::slice::from_ref(&checked);
    assert_eq!(v.revoked_by(journal)?, Some(checked.digest()?));
    assert_eq!(v.operation(journal, &held, hk, 151).unwrap_err(), Error::Revoked("reward intent authorization"));
    let mut other = checked.clone();
    other.target = Digest([7; 32]);
    let mut elsewhere = checked.clone();
    elsewhere.chain.netuid += 1;
    v.operation(&[other, elsewhere], &held, hk, 151)?;
    let again = LaunchAuthorization::new(&chain(), 2, 151)?;
    let v2 = coordinator_row::verify(&chain(), &keys, &held, RowAuthorizing::Launch(&bytes(again.clone())), None, &bytes(launch_row(&again, 151)))?;
    assert_eq!(v2.revoked_by(journal)?, None);
    v2.operation(journal, &held, hk, 151)?;
    Ok(())
}

#[test]
fn a_cohort_row_moves_the_held_target_forward_and_never_back() -> Result<()> {
    let keys = crate::fixtures::trusted_keys();
    let launch = ActiveTarget::launch(&chain())?;
    let t1 = Digest([0x31; 32]);
    let a1 = cohort_auth(t1);
    let entries = vec![entry(2, 0x22, 40_000), entry(4, 0x0f, 25_535)];
    let r1 = bytes(cohort_row(&a1, Some(310), 312, entries.clone()));
    let v = coordinator_row::verify(&chain(), &keys, &launch, RowAuthorizing::Cohort(&bytes(a1.clone())), None, &r1)?;
    let active = ActiveTarget::restore(&chain(), t1, Some(310))?;
    assert_eq!(v.active, active);
    let hk = AccountId([0x66; 32]);
    assert_eq!(
        v.operation(&[], &active, hk, 312)?,
        WeightOperationV2 { genesis: chain().genesis, netuid: chain().netuid, mechanism: 0, hotkey: hk, target: t1, epoch: 312 }
    );
    assert_eq!(v.operation(&[], &active, hk, 309).unwrap_err(), Error::Invalid("epoch before target activation"));
    // An authorization issued after the activation commits from its issue.
    let mut late = a1.clone();
    late.issued_epoch = 320;
    late.renew_by_epoch = 360;
    let vl = coordinator_row::verify(&chain(), &keys, &launch, RowAuthorizing::Cohort(&bytes(late.clone())), None, &bytes(cohort_row(&late, Some(310), 321, entries.clone())))?;
    assert_eq!(vl.operation(&[], &active, hk, 315).unwrap_err(), Error::Invalid("epoch before authorization"));
    vl.operation(&[], &active, hk, 320)?;
    // Still the launch row held: the cohort target is not active there.
    assert_eq!(v.operation(&[], &launch, hk, 312).unwrap_err(), Error::Mismatch("authorized target not active"));
    let row = v.derive_row(&snapshot(&[(2, 0x22), (4, 0x0f)]))?;
    assert_eq!(row.entries, vec![crate::RowEntry { uid: 2, weight: 40_000 }, crate::RowEntry { uid: 4, weight: 25_535 }]);
    assert_eq!(v.moved_uids(&snapshot(&[(2, 0x22), (4, 0x0f)])), Vec::<u16>::new());
    // The same target at the same activation verifies against itself.
    coordinator_row::verify(&chain(), &keys, &active, RowAuthorizing::Cohort(&bytes(a1.clone())), None, &r1)?;
    // Another activation epoch of the held target is refused.
    let wrong = ActiveTarget::restore(&chain(), t1, Some(305))?;
    assert_eq!(
        coordinator_row::verify(&chain(), &keys, &wrong, RowAuthorizing::Cohort(&bytes(a1.clone())), None, &r1).unwrap_err(),
        Error::Mismatch("held target activation epoch")
    );
    // A target activating no later than the held one is never accepted.
    let later = ActiveTarget::restore(&chain(), Digest([0x41; 32]), Some(400))?;
    assert_eq!(
        coordinator_row::verify(&chain(), &keys, &later, RowAuthorizing::Cohort(&bytes(a1.clone())), None, &r1).unwrap_err(),
        Error::Invalid("row target activates no later than the held target")
    );
    // A no-target cohort's authorization of the launch burn row, before any
    // activation, commits the burn row.
    let carried = cohort_auth(launch_digest());
    let burn = bytes(cohort_row(&carried, None, 305, vec![entry(4, 0x0f, u16::MAX)]));
    let vb = coordinator_row::verify(&chain(), &keys, &launch, RowAuthorizing::Cohort(&bytes(carried)), None, &burn)?;
    assert_eq!(vb.active, launch);
    vb.operation(&[], &launch, hk, 305)?;
    Ok(())
}

#[test]
fn every_binding_of_the_row_to_its_authorization_is_checked() -> Result<()> {
    let keys = crate::fixtures::trusted_keys();
    let launch = ActiveTarget::launch(&chain())?;
    let t1 = Digest([0x31; 32]);
    let a1 = cohort_auth(t1);
    let good = cohort_row(&a1, Some(310), 312, vec![entry(2, 0x22, u16::MAX)]);
    let auth = bytes(a1.clone());
    let check = |row: &[u8], auth: &[u8]| coordinator_row::verify(&chain(), &keys, &launch, RowAuthorizing::Cohort(auth), None, row);
    check(&bytes(good.clone()), &auth)?;
    let refused = |edit: &dyn Fn(&mut CoordinatorRow)| {
        let mut r = good.clone();
        edit(&mut r);
        check(&bytes(r), &auth).unwrap_err()
    };
    assert_eq!(refused(&|r| r.authorization = Digest([1; 32])), Error::Mismatch("row authorization"));
    assert_eq!(refused(&|r| r.issued_epoch = 299), Error::Mismatch("row authorization"));
    assert_eq!(refused(&|r| r.target = Digest([0x32; 32])), Error::Mismatch("row authorization"));
    assert_eq!(
        refused(&|r| r.source = RowSource::Cohort { cohort: 8, cohort_label: "testnet-c8".into(), cohort_manifest: a1.cohort_manifest }),
        Error::Mismatch("row source")
    );
    // Not signed by the authorization key.
    let other = sign(good.clone(), &crate::fixtures::package_key())?.to_bytes()?;
    assert!(check(&other, &auth).is_err());
    let mut forged = fx::signed(good.clone())?;
    forged.signature[3] ^= 1;
    assert_eq!(check(&forged.to_bytes()?, &auth).unwrap_err(), Error::BadSignature);
    // Another chain's row or authorization.
    assert!(coordinator_row::verify(&finney(), &keys, &ActiveTarget::launch(&finney())?, RowAuthorizing::Cohort(&auth), None, &bytes(good.clone())).is_err());
    let mut elsewhere = a1.clone();
    elsewhere.chain.netuid += 1;
    assert!(check(&bytes(good.clone()), &bytes(elsewhere)).is_err());
    // A launch authorization does not carry a cohort row.
    let la = LaunchAuthorization::new(&chain(), 1, 100)?;
    assert_eq!(
        coordinator_row::verify(&chain(), &keys, &launch, RowAuthorizing::Launch(&bytes(la)), None, &bytes(good.clone())).unwrap_err(),
        Error::Mismatch("row source")
    );
    // A renewal must name the authorization and never precede it.
    let renewal = RewardIntentRenewal { authorization: a1.digest()?, checkpoint_generation: 3, issued_epoch: 320, renew_by_epoch: 360 };
    let v = coordinator_row::verify(&chain(), &keys, &launch, RowAuthorizing::Cohort(&auth), Some(&bytes(renewal.clone())), &bytes(good.clone()))?;
    assert_eq!(v.terms().renew_by_epoch, 360);
    let mut stray = renewal.clone();
    stray.authorization = Digest([5; 32]);
    assert_eq!(
        coordinator_row::verify(&chain(), &keys, &launch, RowAuthorizing::Cohort(&auth), Some(&bytes(stray)), &bytes(good.clone())).unwrap_err(),
        Error::Mismatch("renewed authorization")
    );
    let mut early = renewal;
    early.issued_epoch = 299;
    early.renew_by_epoch = 339;
    assert_eq!(
        coordinator_row::verify(&chain(), &keys, &launch, RowAuthorizing::Cohort(&auth), Some(&bytes(early)), &bytes(good)).unwrap_err(),
        Error::Invalid("renewal before its authorization")
    );
    Ok(())
}

#[test]
fn a_row_record_is_well_formed_or_refused_at_encode_and_decode() -> Result<()> {
    let a1 = cohort_auth(Digest([0x31; 32]));
    let good = cohort_row(&a1, Some(310), 312, vec![entry(2, 0x22, 30_000), entry(3, 0x23, 35_535)]);
    good.validate()?;
    let bad = |edit: &dyn Fn(&mut CoordinatorRow)| {
        let mut r = good.clone();
        edit(&mut r);
        r.validate().unwrap_err()
    };
    assert_eq!(bad(&|r| r.entries[1].weight -= 1), Error::Invalid("row total"));
    assert_eq!(bad(&|r| r.entries.swap(0, 1)), Error::Unsorted("row uid"));
    assert_eq!(bad(&|r| r.entries[1].uid = 2), Error::Duplicate("row uid"));
    assert_eq!(bad(&|r| r.entries[1].hotkey = AccountId([0x22; 32])), Error::Duplicate("row hotkey"));
    assert_eq!(
        bad(&|r| {
            r.entries[0].weight = 0;
            r.entries[1].weight = u16::MAX;
        }),
        Error::Invalid("zero row weight")
    );
    assert_eq!(bad(&|r| r.activated_epoch = None), Error::Invalid("row activation"));
    assert_eq!(bad(&|r| r.source = RowSource::LaunchBurn), Error::Mismatch("launch row target"));
    assert_eq!(bad(&|r| r.epoch = 299), Error::Invalid("row epoch"));
    assert_eq!(bad(&|r| r.epoch = 309), Error::Invalid("row epoch"));
    assert_eq!(bad(&|r| r.reward_rule = RewardRule::TestnetRewardV0), Error::Mismatch("reward rule record version"));
    // Decoding validates too.
    let mut e = crate::Encoder::new();
    e.str(CoordinatorRow::KIND);
    e.u16(CoordinatorRow::VERSION);
    let mut unsorted = good.clone();
    unsorted.entries.swap(0, 1);
    crate::Canonical::encode_to(&unsorted, &mut e);
    assert_eq!(CoordinatorRow::from_bytes(&e.into_bytes()).unwrap_err(), Error::Unsorted("row uid"));
    // The read bound covers the longest record of every UID.
    let n = 3;
    let full = CoordinatorRow {
        source: RowSource::Cohort { cohort: 7, cohort_label: "a".repeat(crate::MAX_LABEL_LEN), cohort_manifest: Digest([0; 32]) },
        entries: (0..n).map(|i| entry(i as u16, i as u8 + 1, if i == 0 { u16::MAX - (n as u16 - 1) } else { 1 })).collect(),
        ..good
    };
    assert_eq!(fx::signed(full)?.to_bytes()?.len() as u64, CoordinatorRow::signed_len(n)?);
    assert!(CoordinatorRow::max_signed_len()? < crate::MAX_SIGNED_RECORD_LEN as u64);
    // One entry more than a roster and its burn UID can pay is refused.
    let over = coordinator_row::MAX_ROW_ENTRIES + 1;
    let mut long = cohort_row(&a1, Some(310), 312, Vec::new());
    long.entries = (0..over).map(|i| entry(i as u16, 0, 1)).collect();
    assert!(matches!(long.validate().unwrap_err(), Error::LengthLimit { field: "row entries", .. }));
    Ok(())
}

#[test]
fn the_coordinator_builds_its_row_from_its_verified_bundle() -> Result<()> {
    let keys = crate::fixtures::trusted_keys();
    let held = ActiveTarget::launch(&chain())?;
    let a = LaunchAuthorization::new(&chain(), 1, 100)?;
    let auth = bytes(a.clone());
    let full = target_bundle::verify(&chain(), &keys, &held, None, Authorizing::Launch { authorization: &auth, renewal: None })?;
    let record = CoordinatorRow::of_verified(&full, &keys, 130, 8_000, fx::owner_hotkey(), vec![entry(4, 0x0f, u16::MAX)])?;
    assert_eq!(record, launch_row(&a, 130));
    let v = coordinator_row::verify(&chain(), &keys, &held, RowAuthorizing::Launch(&auth), None, &bytes(record))?;
    assert_eq!(v.terms(), full.checked_terms(&keys)?);
    // An epoch before the authorization is refused at build time.
    assert_eq!(
        CoordinatorRow::of_verified(&full, &keys, 99, 8_000, fx::owner_hotkey(), vec![entry(4, 0x0f, u16::MAX)]).unwrap_err(),
        Error::Invalid("row epoch")
    );
    Ok(())
}

/// Review fix round 1 (finding 8): a paid UID whose registration the
/// holder's snapshot no longer holds (handed to another hotkey, the same
/// hotkey re-registered, deregistered) never pays its new holder: its units
/// burn through the UID the snapshot's owner hotkey holds, as the
/// coordinator's own derivation burns them, and with no owner UID the
/// tempo's row is deferred.
#[test]
fn a_moved_uid_burns_instead_of_paying_its_new_hotkey() -> Result<()> {
    let keys = crate::fixtures::trusted_keys();
    let launch = ActiveTarget::launch(&chain())?;
    let a1 = cohort_auth(Digest([0x31; 32]));
    let entries = vec![entry(2, 0x22, 40_000), entry(3, 0x33, 5_000), entry(4, 0x0f, 20_535)];
    let r1 = bytes(cohort_row(&a1, Some(310), 312, entries));
    let v = coordinator_row::verify(&chain(), &keys, &launch, RowAuthorizing::Cohort(&bytes(a1)), None, &r1)?;
    let paid = |r: crate::WeightRowV2| r.entries.iter().map(|e| (e.uid, e.weight)).collect::<Vec<_>>();
    // Unchanged registrations: the record's entries.
    let same = snapshot(&[(2, 0x22), (3, 0x33), (4, 0x0f)]);
    assert_eq!(paid(v.derive_row(&same)?), vec![(2, 40_000), (3, 5_000), (4, 20_535)]);
    // UID 2 now held by an unrelated registrant: burned, never paid to it.
    let handed = snapshot(&[(2, 0x99), (3, 0x33), (4, 0x0f)]);
    assert_eq!(paid(v.derive_row(&handed)?), vec![(3, 5_000), (4, 60_535)]);
    assert_eq!(v.moved_uids(&handed), vec![2]);
    // The same hotkey re-registered (a new registration block) is a new
    // registration: burned too.
    let mut again = same.clone();
    again.neurons[0].registration_block = 77;
    assert_eq!(paid(v.derive_row(&again)?), vec![(3, 5_000), (4, 60_535)]);
    // Deregistered, and the owner hotkey moved to UID 9: everything that
    // burns follows the owner.
    let moved_owner = snapshot(&[(3, 0x33), (9, 0x0f)]);
    assert_eq!(paid(v.derive_row(&moved_owner)?), vec![(3, 5_000), (9, 60_535)]);
    // Units must burn and the owner holds no UID: deferred, never another UID.
    assert_eq!(
        v.derive_row(&snapshot(&[(2, 0x99), (3, 0x33)])).unwrap_err(),
        Error::Deferred("owner hotkey not registered")
    );
    Ok(())
}
