//! Regression tests for the 2026-11-01 review of the `reward-v1` live path:
//! gate-failure order in `finalize`, re-audit draws, the manifest's near-copy
//! distance, the committed (max-upscaled) row form, the holder's own active
//! target in bundle verification, and carrier verification.

use crate::fixtures::{package_key, trusted_keys};
use crate::fixtures_reward_v1::{self as fx, FIXTURE_QUANT, Spec, bp, chain, finalize_fixture, sigma_permille};
use crate::reward_v1::{self, ArchiveProbe, Measurements};
use crate::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NearTo {
    Current,
    Predecessor,
    Probe,
}

/// Entrant 0 is a rank-preserving copy of one published model; every other
/// comparison is twice `d` apart.
struct Copying {
    near: NearTo,
    succeeded: Vec<u32>,
    hotkeys: Vec<AccountId>,
}

impl Measurements for Copying {
    fn current(&mut self, e: usize) -> Result<NearStats> {
        Ok(if e == 0 && self.near == NearTo::Current { fx::aligned(self.succeeded[e]) } else { fx::far(self.succeeded[e]) })
    }
    fn predecessor(&mut self, e: usize) -> Result<NearStats> {
        Ok(if e == 0 && self.near == NearTo::Predecessor { fx::aligned(self.succeeded[e]) } else { fx::far(self.succeeded[e]) })
    }
    fn reference_probe(&mut self, e: usize, content: &Digest, version: u32) -> Result<ProbeMatch> {
        let near = e == 0 && self.near == NearTo::Probe;
        Ok(ProbeMatch {
            content: *content,
            version,
            stats: if near { fx::aligned(fx::EXAMPLE_PROBE_SIZE) } else { fx::far(fx::EXAMPLE_PROBE_SIZE) },
            entrant_probe_score: Score::ZERO,
            public_probe_score: Score::ZERO,
        })
    }
    fn archive_probe(&mut self, _e: usize) -> Result<ArchiveProbe> {
        Ok(ArchiveProbe::default())
    }
    fn pair(&mut self, h: usize, o: usize) -> Result<NearStats> {
        Ok(fx::far(self.succeeded[h].min(self.succeeded[o])))
    }
    fn promotion_audit(&mut self, h: usize) -> Result<PromotionAudit> {
        Ok(fx::promotion_audit(self.hotkeys[h], true, 0))
    }
}

fn reason(gate: Gate) -> &'static str {
    match gate {
        Gate::Qualification => "qualification.artifact_failure",
        Gate::Share => "share.mae_above_threshold",
        Gate::Attribution => "attribution.below_threshold",
        Gate::Span => "span.fpr_above_threshold",
        Gate::PublicCopy => "public.content",
        Gate::HumanFalsePositive => HUMAN_FPR_LOOSE_REASON,
        Gate::MinimumSuccess => MIN_SUCCESS_REASON,
    }
}

/// Every gate a coordinator records before `finalize`, combined with every
/// public-copy reason `finalize` adds, packages, with the failures in gate
/// order. Before the fix a near copy that also failed the loose
/// false-positive tier (gate 6) left the list unsorted and the cohort could
/// never be packaged.
#[test]
fn finalize_records_every_gate_with_every_public_copy_reason_in_gate_order() -> Result<()> {
    let measured = [Gate::Share, Gate::Attribution, Gate::HumanFalsePositive];
    for gate in measured {
        for near in [NearTo::Current, NearTo::Predecessor, NearTo::Probe] {
            // R_1 = 0.9600, R_0 = 0.9500, sigma 0.04: w = 0.0032. Entrant 0
            // copies the named model within w/2; 0x11 is independent.
            let copy_score = match near {
                NearTo::Predecessor => bp(9505),
                _ => bp(9605),
            };
            let specs = [Spec::new(0x10, copy_score).failing(gate, reason(gate)), Spec::new(0x11, bp(9900))];
            let m = fx::manifest(chain(), 5, 1000, sigma_permille(40), 1, &specs);
            let index = fx::default_index(&m.references);
            let mut p = fx::package(&m, bp(9600), Some(bp(9500)), &specs)?;
            let mut measure = Copying {
                near,
                succeeded: vec![1000, 1000],
                hotkeys: specs.iter().map(|s| s.identity().hotkey).collect(),
            };
            finalize_fixture(&mut p, &index, &mut measure)
                .unwrap_or_else(|e| panic!("{gate:?} with a {near:?} copy: {e:?}"));
            reward_v1::check_outcome(&p, &index)?;
            let gates: Vec<Gate> = p.entrants[0].gate_failures.iter().map(|g| g.gate).collect();
            let mut sorted = gates.clone();
            sorted.sort();
            assert_eq!(gates, sorted, "{gate:?} with a {near:?} copy");
            assert!(gates.contains(&gate) && gates.contains(&Gate::PublicCopy));
            let expected = match near {
                NearTo::Current => PublicCopy::Current,
                NearTo::Predecessor => PublicCopy::Predecessor,
                NearTo::Probe => PublicCopy::Probe,
            };
            assert_eq!(p.entrants[0].public_copy(), Some(expected));
        }
    }
    // An exact copy found at admission (`public.content`) with any other
    // recorded failure is ordered the same way.
    for gate in [Gate::Qualification, Gate::Share, Gate::Attribution, Gate::HumanFalsePositive] {
        let specs = [
            Spec::published_copy(0x10, 0xe1).failing(gate, reason(gate)),
            Spec::new(0x11, bp(9900)),
        ];
        let m = fx::manifest(chain(), 5, 1000, sigma_permille(40), 1, &specs);
        let index = fx::default_index(&m.references);
        let mut p = fx::package(&m, bp(9600), Some(bp(9500)), &specs)?;
        p.entrants[0].score = None;
        p.entrants[0].assigned = 0;
        p.entrants[0].succeeded = 0;
        p.entrants[0].failed = 0;
        let mut measure = Copying {
            near: NearTo::Current,
            succeeded: vec![0, 1000],
            hotkeys: specs.iter().map(|s| s.identity().hotkey).collect(),
        };
        finalize_fixture(&mut p, &index, &mut measure).unwrap_or_else(|e| panic!("{gate:?} with an exact copy: {e:?}"));
        let gates: Vec<Gate> = p.entrants[0].gate_failures.iter().map(|g| g.gate).collect();
        let mut sorted = gates.clone();
        sorted.sort();
        assert_eq!(gates, sorted, "{gate:?} with an exact copy");
        assert_eq!(p.entrants[0].public_copy(), Some(PublicCopy::Content));
    }
    Ok(())
}

#[test]
fn a_canary_reaudit_must_use_a_fresh_draw() {
    let flagged = |draw| CanaryAudit { draw, flag: Some("canary.statistic".into()) };
    let same = PromotionAudit {
        head: AccountId([1; 32]),
        strict_fpr_passed: true,
        canary: vec![flagged(fx::canary_draw(1)), flagged(fx::canary_draw(1))],
    };
    assert_eq!(same.validate().unwrap_err(), Error::Invalid("canary re-audit reuses its draw"));
    let fresh = PromotionAudit {
        canary: vec![flagged(fx::canary_draw(1)), flagged(fx::canary_draw(2))],
        ..same
    };
    fresh.validate().unwrap();
    assert!(fresh.blocked());
}

#[test]
fn a_manifest_may_lower_the_near_copy_distance_but_never_raise_it() {
    let mut p = RewardV1Params::new(sigma_permille(35), FIXTURE_QUANT);
    p.validate().unwrap();
    p.near_copy_ppb = 20_000_000;
    p.validate().unwrap();
    p.near_copy_ppb = NEAR_COPY_PPB + 1;
    assert_eq!(p.validate().unwrap_err(), Error::OutOfRange("near_copy_ppb"));
    p.near_copy_ppb = 0;
    assert_eq!(p.validate().unwrap_err(), Error::OutOfRange("near_copy_ppb"));
    // The fixed parts of the rule are not manifest parameters.
    let mut q = RewardV1Params::new(sigma_permille(35), FIXTURE_QUANT);
    q.decay_bps += 1;
    assert_eq!(q.validate().unwrap_err(), Error::Mismatch("reward_v2 parameters"));
    let mut q = RewardV1Params::new(sigma_permille(35), FIXTURE_QUANT);
    q.fallback_bps -= 1;
    assert_eq!(q.validate().unwrap_err(), Error::Mismatch("reward_v2 parameters"));
}

fn row(entries: &[(u16, u16)]) -> WeightRowV2 {
    WeightRowV2 {
        chain: chain(),
        calculation_target: Digest([1; 32]),
        reward_rule: RewardRule::RewardV2,
        registration_block: 1,
        registration: Digest([2; 32]),
        owner_hotkey: AccountId([3; 32]),
        entries: entries.iter().map(|&(uid, weight)| RowEntry { uid, weight }).collect(),
    }
}

/// The whitepaper's two-UID row, as rtao and the chain store it after their
/// max-upscale; a single-UID row is unchanged; ties round to even.
#[test]
fn the_committed_row_is_the_canonical_row_max_upscaled() {
    let r = row(&[(33, 35424), (34, 30111)]);
    r.validate().unwrap();
    assert_eq!(r.chain_values(), vec![(33, 65535), (34, 55706)]);
    assert_eq!(row(&[(3, 65535)]).chain_values(), vec![(3, 65535)]);
    assert_eq!(row(&[(0, 32768), (3, 32767)]).chain_values(), vec![(0, 65535), (3, 65533)]);
    // 1 * 65535 / 26214 = 2.5 exactly: half to even gives 2.
    assert_eq!(row(&[(1, 1), (2, 26214)]).chain_values(), vec![(1, 2), (2, 65535)]);
    // 3 * 65535 / 26214 = 7.5 exactly: half to even gives 8.
    assert_eq!(row(&[(1, 3), (2, 26214)]).chain_values(), vec![(1, 8), (2, 65535)]);
    // Agreement with rtao's f64 ties-to-even normalization on every two-entry
    // canonical row, and the result is a fixed point of a second upscale.
    for a in 1..=32767u16 {
        let b = 65535 - a;
        let r = row(&[(1, a), (2, b)]);
        let top = f64::from(b);
        let f = |w: u16| ((f64::from(w) / top) * 65535.0).round_ties_even() as u16;
        assert_eq!(r.chain_values(), vec![(1, f(a)), (2, f(b))], "row ({a}, {b})");
        let again = row(&r.chain_values());
        assert_eq!(again.chain_values(), r.chain_values());
    }
}

fn signed_bytes<R: Record + Clone>(record: &R) -> Vec<u8> {
    sign(record.clone(), &package_key()).unwrap().to_bytes().unwrap()
}

struct Bytes {
    manifest: Vec<u8>,
    package: Vec<u8>,
    target: Vec<u8>,
    closure: Vec<u8>,
    contents: Vec<u8>,
    fingerprints: Vec<u8>,
}

impl Bytes {
    fn of(
        manifest: &CohortManifestV2,
        package: &ScorePackageV2,
        target: &CalculationTargetV2,
        closure: &submission::CohortClosure,
        index: &PublishedIndex,
    ) -> Self {
        Self {
            manifest: signed_bytes(manifest),
            package: signed_bytes(package),
            target: signed_bytes(target),
            closure: signed_bytes(closure),
            contents: index.contents.to_bytes().unwrap(),
            fingerprints: index.fingerprints.to_bytes().unwrap(),
        }
    }
    fn source(&self) -> Source<'_> {
        Source {
            manifest: &self.manifest,
            package: &self.package,
            target: &self.target,
            closure: &self.closure,
            receipt_index: None,
            published_contents: &self.contents,
            fingerprint_index: &self.fingerprints,
        }
    }
}

fn cohort(authorization: &[u8]) -> Authorizing<'_> {
    Authorizing::Cohort { authorization, renewal: None, carrier: None }
}

/// The holder's own history decides the active target: the launch bundle is
/// refused once a cohort target has activated, a superseded cohort bundle is
/// refused once its successor has, and only the held target or its
/// successor is accepted.
#[test]
fn bundles_are_checked_against_the_holders_active_target() -> Result<()> {
    let c = fx::computed()?;
    let keys = trusted_keys();
    let hk = AccountId([0x66; 32]);
    let launch_held = ActiveTarget::launch(&chain())?;
    let after = c.active_after()?;
    let boundary = c.closure.closed_epoch;

    // Launch authorization: accepted only while the burn row is held.
    let launch = fx::signed(LaunchAuthorization::new(&chain(), 1, c.manifest.opened_epoch)?)?.to_bytes()?;
    let auth = Authorizing::Launch { authorization: &launch, renewal: None };
    target_bundle::verify(&chain(), &keys, &launch_held, None, auth)?;
    assert_eq!(
        target_bundle::verify(&chain(), &keys, &after, None, auth).unwrap_err(),
        Error::Invalid("launch authorization after activation")
    );

    // The computed cohort's bundle: accepted when the launch burn row is held
    // (its target's predecessor) and then held itself, with its activation.
    let b = Bytes::of(&c.manifest, &c.package, &c.target, &c.closure, &c.index);
    let a = c.authorization.to_bytes()?;
    let v = target_bundle::verify(&chain(), &keys, &launch_held, Some(b.source()), cohort(&a))?;
    assert_eq!(v.active, after);
    v.operation(&keys, &[], hk, boundary)?;
    let v = target_bundle::verify(&chain(), &keys, &after, Some(b.source()), cohort(&a))?;
    assert_eq!(v.active, after);
    // A held record of the same target with another activation epoch is refused.
    let skewed = ActiveTarget::restore(&chain(), after.digest(), Some(boundary + 1))?;
    assert_eq!(
        target_bundle::verify(&chain(), &keys, &skewed, Some(b.source()), cohort(&a)).unwrap_err(),
        Error::Mismatch("held target activation epoch")
    );

    // Its successor activates at epoch 100.
    let s = fx::successor_of(&c)?;
    let s_index = fx::default_index(&s.manifest.references);
    let sb = Bytes::of(&s.manifest, &s.package, &s.target, &s.closure, &s_index);
    let sa = fx::signed(fx::authorization(&s.manifest, &s.package, &s.target, 1, s.manifest.opened_epoch + 16)?)?
        .to_bytes()?;
    let sv = target_bundle::verify(&chain(), &keys, &after, Some(sb.source()), cohort(&sa))?;
    let next = sv.active.clone();
    assert_eq!(next.activated_epoch(), Some(s.closure.closed_epoch));
    sv.operation(&keys, &[], hk, s.closure.closed_epoch)?;
    // Not the launch burn row's successor: refused while the launch row is held.
    assert_eq!(
        target_bundle::verify(&chain(), &keys, &launch_held, Some(sb.source()), cohort(&sa)).unwrap_err(),
        Error::Mismatch("bundle target is neither the held target nor its successor")
    );
    // Once the successor is held, the superseded bundle and the launch bundle are refused.
    assert_eq!(
        target_bundle::verify(&chain(), &keys, &next, Some(b.source()), cohort(&a)).unwrap_err(),
        Error::Mismatch("bundle target is neither the held target nor its successor")
    );
    assert!(target_bundle::verify(&chain(), &keys, &next, None, auth).is_err());
    // A held record on another chain is refused.
    let other = ActiveTarget::launch(&fx::finney())?;
    assert_eq!(
        target_bundle::verify(&chain(), &keys, &other, None, auth).unwrap_err(),
        Error::Mismatch("held target chain")
    );
    Ok(())
}

/// A carrier cohort of the computed cohort: its index is the computed
/// index after the computed closure, and `specs` are scored against the next
/// references.
fn carrier_cohort(c: &fx::Computed, specs: &[Spec], reference: Score, predecessor: Score) -> Result<(CohortManifestV2, ScorePackageV2, PublishedIndex)> {
    let index = c.index.after_closure(&c.package, fx::closure_fingerprints(c))?;
    let refs = reward_v1::next_references(&c.manifest.references, &c.package, &c.index)?;
    let manifest = fx::manifest_with(chain(), 4, 1000, sigma_permille(40), refs, &index, fx::probe(fx::EXAMPLE_PROBE_SIZE), specs);
    let mut package = fx::package(&manifest, reference, refs.predecessor.map(|_| predecessor), specs)?;
    let mut measure = fx::spec_measure(specs, 1000, |h, o| specs[h].family == specs[o].family);
    finalize_fixture(&mut package, &index, &mut measure)?;
    Ok((manifest, package, index))
}

/// A carrier's outcome is recomputed: a package whose true outcome is a new
/// target, relabelled as producing none, cannot carry the old target; nor can
/// a carrier whose index drops what the target's cohort published.
#[test]
fn a_carrier_package_is_recomputed_and_must_produce_no_target() -> Result<()> {
    let c = fx::computed()?;
    let keys = trusted_keys();
    let held = c.active_after()?;
    let hk = AccountId([0x66; 32]);
    let b = Bytes::of(&c.manifest, &c.package, &c.target, &c.closure, &c.index);
    let verify_with = |m: &CohortManifestV2, p: &ScorePackageV2, index: &PublishedIndex| -> Result<VerifiedTarget> {
        let auth = fx::signed(fx::authorization(m, p, &c.target, 1, m.opened_epoch + 16)?)?.to_bytes()?;
        let (mb, pb) = (signed_bytes(m), signed_bytes(p));
        let (contents, fingerprints) = (index.contents.to_bytes()?, index.fingerprints.to_bytes()?);
        let carrier = Carrier {
            manifest: &mb,
            package: &pb,
            receipt_index: None,
            published_contents: &contents,
            fingerprint_index: &fingerprints,
        };
        target_bundle::verify(
            &chain(),
            &keys,
            &held,
            Some(b.source()),
            Authorizing::Cohort { authorization: &auth, renewal: None, carrier: Some(carrier) },
        )
    };
    // A genuine no-target carrier: nobody reaches the references.
    let low = [Spec::new(0x93, bp(8000))];
    let (m, p, index) = carrier_cohort(&c, &low, bp(9600), bp(9500))?;
    assert_eq!(p.mode, CutoffMode::NoTarget);
    let v = verify_with(&m, &p, &index)?;
    assert!(v.carrier().is_some());
    v.operation(&keys, &[], hk, m.opened_epoch + 16)?;
    // A margin outcome relabelled `NoTarget` is structurally valid but refused.
    let high = [Spec::new(0x94, bp(9990))];
    let (m, p, index) = carrier_cohort(&c, &high, bp(9600), bp(9500))?;
    assert_eq!(p.mode, CutoffMode::Margin);
    let mut forged = p.clone();
    forged.mode = CutoffMode::NoTarget;
    forged.promoted = None;
    forged.promotion_audits.clear();
    forged.credits.clear();
    forged.quant.cutoff = None;
    for e in &mut forged.entrants {
        e.cluster_head = None;
        e.cluster_stats = None;
        e.units = 0;
    }
    forged.validate()?;
    assert!(verify_with(&m, &forged, &index).is_err());
    // The honest margin package is a new target, so it is no carrier either.
    assert_eq!(verify_with(&m, &p, &index).unwrap_err(), Error::Mismatch("carrier produced a target"));
    // A carrier whose index lost the target cohort's contents is refused.
    let s = fx::successor_of(&c)?;
    let low_s = [Spec::new(0x95, bp(8000))];
    let s_index = fx::default_index(&s.manifest.references);
    let s_manifest = fx::manifest_with(chain(), 4, 1000, sigma_permille(40), s.manifest.references, &s_index, fx::probe(fx::EXAMPLE_PROBE_SIZE), &low_s);
    let mut s_package = fx::package(&s_manifest, bp(9600), Some(bp(9100)), &low_s)?;
    finalize_fixture(&mut s_package, &s_index, &mut fx::spec_measure(&low_s, 1000, |_, _| false))?;
    assert_eq!(
        verify_with(&s_manifest, &s_package, &s_index).unwrap_err(),
        Error::Mismatch("carrier index drops published artifacts")
    );
    Ok(())
}

/// A signed receipt index streamed in pieces of any size authenticates to
/// the same digest and count as the decoded record, in a fixed amount of
/// memory; a changed byte, a missing or extra byte, or another key fails.
#[test]
fn a_receipt_index_streams_to_its_binding_and_refuses_any_change() -> Result<()> {
    use crate::fixtures::receipt_key;
    let mut entries: Vec<ReceiptIndexEntry> = (0..300u32)
        .map(|i| ReceiptIndexEntry {
            attempt: domain_hash("test.attempt", &i.to_le_bytes()),
            outcome: OutcomeClass::Prediction,
            object_sha256: domain_hash("test.object", &i.to_le_bytes()),
            object_len: 100 + i,
        })
        .collect();
    entries.sort_by_key(|a| a.attempt);
    let index = ReceiptIndex { cohort_manifest: Digest([0x5a; 32]), entries };
    let bytes = sign(index.clone(), &receipt_key())?.to_bytes()?;
    let keys = trusted_keys();
    let stream = |b: &[u8], piece: usize| {
        let mut s = ReceiptIndexStream::new();
        for c in b.chunks(piece) {
            s.update(c);
        }
        s.finish(&keys)
    };
    for piece in [1, 7, 64, 4096, bytes.len()] {
        let b = stream(&bytes, piece)?;
        assert_eq!((b.cohort_manifest, b.root, b.count), (index.cohort_manifest, index.digest()?, 300), "piece {piece}");
    }
    let mut changed = bytes.clone();
    changed[bytes.len() / 2] ^= 1;
    assert_eq!(stream(&changed, 64).unwrap_err(), Error::BadSignature);
    assert_eq!(stream(&bytes[..bytes.len() - 1], 64).unwrap_err(), Error::UnexpectedEnd);
    let mut longer = bytes.clone();
    longer.push(0);
    assert_eq!(stream(&longer, 64).unwrap_err(), Error::TrailingBytes);
    let other = sign(index.clone(), &package_key())?.to_bytes()?;
    assert_eq!(stream(&other, 64).unwrap_err(), Error::UntrustedKey);
    assert!(stream(&bytes[..10], 64).is_err());
    Ok(())
}
