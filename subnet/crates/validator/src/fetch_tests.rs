//! Offline tests of the published-package fetcher, the supersession decision,
//! the continuous-run configuration and the installer.
//!
//! The public host is an in-memory map behind the fetcher's `Transport`, or a
//! loopback HTTP server for the real transport. Records are the protocol
//! crate's fixtures, signed with its public dev keys. No chain, RPC endpoint
//! or remote host is contacted.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Result, bail};
use serde_json::json;
use slop_ninja_protocol::fixtures_reward_v1 as fx;
use slop_ninja_protocol::{ActiveTarget, ChainBinding, Digest, Record, TrustedKeys, sign};

use crate::config::ValidatorConfig;
use crate::fetch::{self, Got, Outcome, Session, Sink, Staged, Transport};
use crate::package_tests::rv1::{self, Cohort};
use crate::package_v2::HeldTargetFile;
use crate::supersede::{self, Candidate, Decision, InstallPorts, Pending};

/// An in-memory public host. `cut` delivers that many bytes of an object
/// once and then fails, like a dropped connection.
#[derive(Default)]
struct Host {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    cut: Mutex<BTreeMap<String, usize>>,
    log: Mutex<Vec<(String, u64)>>,
    ignore_range: bool,
}

impl Host {
    fn put(&self, path: &str, bytes: Vec<u8>) {
        self.objects.lock().unwrap().insert(path.to_owned(), bytes);
    }

    fn remove(&self, path: &str) {
        self.objects.lock().unwrap().remove(path);
    }

    fn get_bytes(&self, path: &str) -> Vec<u8> {
        self.objects
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .expect("object")
    }

    fn offsets(&self, path: &str) -> Vec<u64> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p == path)
            .map(|(_, o)| *o)
            .collect()
    }

    fn clear_log(&self) {
        self.log.lock().unwrap().clear();
    }
}

impl Transport for Host {
    async fn get<'a, S: Sink>(
        &'a self,
        path: &'a str,
        offset: u64,
        sink: &'a mut S,
    ) -> Result<Got> {
        {
            self.log.lock().unwrap().push((path.to_owned(), offset));
            let Some(body) = self.objects.lock().unwrap().get(path).cloned() else {
                return Ok(Got::NotFound);
            };
            let (start, got) = if offset > 0 && !self.ignore_range {
                (usize::try_from(offset)?, Got::Partial)
            } else {
                (0, Got::Full)
            };
            let rest = body.get(start..).unwrap_or_default();
            sink.begin(Some(rest.len() as u64), got == Got::Partial)?;
            let cut = self.cut.lock().unwrap().remove(path);
            if let Some(n) = cut {
                sink.write(&rest[..n])?;
                bail!("connection reset");
            }
            for chunk in rest.chunks(997) {
                sink.write(chunk)?;
            }
            Ok(got)
        }
    }
}

fn sha(bytes: &[u8]) -> String {
    fetch::sha256_hex(bytes)
}

fn dir(c: &Cohort) -> String {
    format!("v1/cohorts/{}/", c.manifest.cohort_label)
}

const CHUNK: &str = "receipts/bundle-job-0.bin";

/// One cohort's objects at their published names.
fn cohort_objects(c: &Cohort) -> Vec<(String, Vec<u8>)> {
    let pk = slop_ninja_protocol::fixtures::package_key();
    let rk = slop_ninja_protocol::fixtures::receipt_key();
    let label = &c.manifest.cohort_label;
    let chunk_map = json!({
        "format": fetch::CHUNK_FORMAT,
        "cohort_manifest": c.manifest.digest().unwrap().to_string(),
        "receipt_root": c.receipt_index.digest().unwrap().to_string(),
        "receipt_count": c.receipt_index.count(),
        "objects": [{
            "key": format!("cohorts/{label}/{CHUNK}"),
            "sha256": sha(&c.chunk),
            "bytes": c.chunk.len(),
            "receipts": c.receipt_index.count(),
        }],
    });
    vec![
        (
            "manifest.signed".into(),
            sign(c.manifest.clone(), &pk).unwrap().to_bytes().unwrap(),
        ),
        (
            "package.signed".into(),
            sign(c.package.clone(), &pk).unwrap().to_bytes().unwrap(),
        ),
        (
            "target.signed".into(),
            sign(c.target.clone(), &pk).unwrap().to_bytes().unwrap(),
        ),
        (
            "closure.signed".into(),
            sign(c.closure.clone(), &pk).unwrap().to_bytes().unwrap(),
        ),
        (
            "receipt-index.signed".into(),
            sign(c.receipt_index.clone(), &rk)
                .unwrap()
                .to_bytes()
                .unwrap(),
        ),
        (
            "published-contents.bin".into(),
            c.index.contents.to_bytes().unwrap(),
        ),
        (
            "fingerprint-index.bin".into(),
            c.index.fingerprints.to_bytes().unwrap(),
        ),
        (
            "chunk-map.json".into(),
            serde_json::to_vec(&chunk_map).unwrap(),
        ),
        (CHUNK.into(), c.chunk.clone()),
    ]
}

/// Publishes a cohort's objects and its publication record.
fn publish(host: &Host, c: &Cohort) {
    let objects = cohort_objects(c);
    let publication = json!({
        "schema": fetch::PUBLICATION_SCHEMA,
        "cohort": c.manifest.cohort,
        "label": c.manifest.cohort_label,
        "publisher": "test",
        "receipts_verified": c.receipt_index.count(),
        "objects": objects.iter().map(|(rel, b)| json!({"rel": rel, "sha256": sha(b), "bytes": b.len()})).collect::<Vec<_>>(),
    });
    for (rel, bytes) in objects {
        host.put(&format!("{}{rel}", dir(c)), bytes);
    }
    host.put(
        &format!("{}publication.json", dir(c)),
        serde_json::to_vec(&publication).unwrap(),
    );
}

/// Deletes a cohort's detailed receipts from the host, as the N+2 rule does.
fn delete_receipts(host: &Host, c: &Cohort) {
    for rel in ["receipt-index.signed", "chunk-map.json", CHUNK] {
        host.remove(&format!("{}{rel}", dir(c)));
    }
}

fn chain_json(chain: &ChainBinding) -> serde_json::Value {
    json!({"genesis": hex::encode(chain.genesis.0), "netuid": chain.netuid, "mechanism": chain.mechanism})
}

/// Writes the cohort index; each cohort with whether its receipts are retained.
fn index(host: &Host, cohorts: &[(&Cohort, bool)]) {
    let entries: Vec<_> = cohorts
        .iter()
        .map(|(c, retained)| {
            json!({
                "cohort": c.manifest.cohort,
                "label": c.manifest.cohort_label,
                "path": dir(c),
                "mode": "margin",
                "target": hex::encode(c.target.digest().unwrap().0),
                "receipts_retained": retained,
                "entrants": c.package.entrants.len(),
            })
        })
        .collect();
    let body = json!({
        "schema": fetch::INDEX_SCHEMA,
        "network": "testnet",
        "chain": chain_json(&fx::chain()),
        "trust": {"path": "v1/trust/testnet-trust.json", "sha256": "00"},
        "active_target": {"digest": "", "source": "launch_burn", "cohort": null},
        "cohorts": entries,
    });
    host.put(fetch::INDEX_PATH, serde_json::to_vec(&body).unwrap());
}

/// Publishes an authorization (and renewal) and points at it.
fn point(
    host: &Host,
    chain: &ChainBinding,
    kind: &str,
    authorization: &[u8],
    renewal: Option<&[u8]>,
) {
    let object = |bytes: &[u8]| {
        let digest = sha(bytes);
        let path = format!("v1/authorizations/{digest}.signed");
        host.put(&path, bytes.to_vec());
        json!({"path": path, "sha256": digest, "bytes": bytes.len()})
    };
    let body = json!({
        "schema": fetch::AUTHORIZATION_SCHEMA,
        "network": "testnet",
        "chain": chain_json(chain),
        "kind": kind,
        "authorization": object(authorization),
        "renewal": renewal.map(object),
    });
    host.put(
        fetch::AUTHORIZATION_PATH,
        serde_json::to_vec(&body).unwrap(),
    );
}

fn scratch() -> Result<tempfile::TempDir> {
    let root = std::env::var_os("SLOP_NINJA_TEST_TMPDIR").ok_or_else(|| {
        anyhow::anyhow!("set SLOP_NINJA_TEST_TMPDIR to the recorded task temporary root")
    })?;
    Ok(tempfile::Builder::new()
        .prefix("w7-fetch-")
        .tempdir_in(root)?)
}

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn launch_held() -> ActiveTarget {
    ActiveTarget::launch(&fx::chain()).unwrap()
}

fn run(host: &Host, cache: &Path, held: ActiveTarget) -> Result<Outcome> {
    run_with(host, cache, held, None, &[])
}

/// A session with the last bundle's fingerprint and the installed cohorts.
fn run_with(
    host: &Host,
    cache: &Path,
    held: ActiveTarget,
    previous: Option<&str>,
    keep: &[String],
) -> Result<Outcome> {
    let keys: TrustedKeys = slop_ninja_protocol::fixtures::trusted_keys();
    let chain = fx::chain();
    block_on(
        Session {
            transport: host,
            cache,
            keys: &keys,
            chain: &chain,
            held,
            previous,
            keep,
        }
        .run(),
    )
}

fn ready(outcome: Outcome) -> Box<Staged> {
    match outcome {
        Outcome::Ready(staged) => staged,
        other => panic!("expected a verified bundle, got {other:?}"),
    }
}

fn target_hex(c: &Cohort) -> String {
    c.target.digest().unwrap().to_string()
}

/// A published cohort target assembles into exactly the bundle the
/// coordinator's tool writes, verified against the held target; a second
/// session reads only the three small documents and serves every object from
/// the cache.
#[test]
fn a_published_cohort_assembles_into_the_bundle_the_validator_verifies() -> Result<()> {
    let root = scratch()?;
    let (first, _, _) = rv1::cohorts();
    let host = Host::default();
    publish(&host, &first);
    index(&host, &[(&first, true)]);
    point(&host, &fx::chain(), "cohort", &first.authorization, None);
    let staged = ready(run(&host, root.path(), launch_held())?);
    assert_eq!(
        staged.bundle,
        rv1::bundle(&first),
        "byte-identical to the operator tool's bundle"
    );
    assert_eq!(staged.info.kind, "cohort");
    assert_eq!(staged.info.target, target_hex(&first));
    assert_eq!(
        staged.info.activated_epoch,
        Some(first.closure.closed_epoch)
    );
    assert_eq!(staged.info.receipts_verified, first.receipt_index.count());
    assert!(staged.advance.is_none());
    // The target record is read once to walk from the held target and once
    // for the bundle, so its second read is a cache hit.
    assert_eq!(
        (
            staged.stats.fetched,
            staged.stats.cached,
            staged.stats.documents
        ),
        (10, 1, 3)
    );
    // Files in the cache are private.
    use std::os::unix::fs::PermissionsExt as _;
    let local = root
        .path()
        .join("cohorts")
        .join(&first.manifest.cohort_label)
        .join("package.signed");
    assert_eq!(
        std::fs::metadata(&local)?.permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(root.path().join("cohorts"))?
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    host.clear_log();
    let again = ready(run(&host, root.path(), launch_held())?);
    assert_eq!(again.sha256, staged.sha256);
    // The authorization is a few hundred bytes, read into memory and
    // checked each session, never cached.
    assert_eq!(
        (
            again.stats.fetched,
            again.stats.cached,
            again.stats.documents
        ),
        (1, 10, 3)
    );
    assert!(!root.path().join("authorizations").exists());
    assert!(host.offsets(&format!("{}{CHUNK}", dir(&first))).is_empty());
    Ok(())
}

/// A request cut part-way leaves a partial file; the next session continues
/// it with a range request from the bytes it holds. A host that ignores the
/// range restarts the object from byte 0, and it still verifies.
#[test]
fn an_interrupted_object_resumes_with_a_range_request() -> Result<()> {
    let (first, _, _) = rv1::cohorts();
    let chunk_path = format!("{}{CHUNK}", dir(&first));
    assert!(
        first.chunk.len() > 200,
        "the fixture chunk spans several reads"
    );
    for ignore_range in [false, true] {
        let root = scratch()?;
        let host = Host {
            ignore_range,
            ..Host::default()
        };
        publish(&host, &first);
        index(&host, &[(&first, true)]);
        point(&host, &fx::chain(), "cohort", &first.authorization, None);
        host.cut.lock().unwrap().insert(chunk_path.clone(), 100);
        assert!(
            run(&host, root.path(), launch_held()).is_err(),
            "the dropped connection is an error"
        );
        let part = root
            .path()
            .join("cohorts")
            .join(&first.manifest.cohort_label)
            .join(format!("{CHUNK}.part"));
        assert_eq!(std::fs::metadata(&part)?.len(), 100);
        let staged = ready(run(&host, root.path(), launch_held())?);
        assert_eq!(host.offsets(&chunk_path), vec![0, 100]);
        assert_eq!(staged.stats.resumed, u32::from(!ignore_range));
        assert!(!part.exists());
        assert_eq!(staged.bundle, rv1::bundle(&first));
    }
    Ok(())
}

/// An object whose bytes differ from its listed digest is deleted and the
/// session reports it; nothing is assembled. A host that declares more bytes
/// than listed is refused before the body is read.
#[test]
fn a_digest_mismatch_deletes_the_partial_and_assembles_nothing() -> Result<()> {
    let root = scratch()?;
    let (first, _, _) = rv1::cohorts();
    let host = Host::default();
    publish(&host, &first);
    index(&host, &[(&first, true)]);
    point(&host, &fx::chain(), "cohort", &first.authorization, None);
    let path = format!("{}package.signed", dir(&first));
    let good = host.get_bytes(&path);
    let mut bad = good.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    host.put(&path, bad);
    match run(&host, root.path(), launch_held())? {
        Outcome::Incomplete { code, cohort, .. } => {
            assert_eq!(code, "object_digest_mismatch");
            assert_eq!(
                cohort.as_deref(),
                Some(first.manifest.cohort_label.as_str())
            );
        }
        other => panic!("expected a digest refusal, got {other:?}"),
    }
    let local = root
        .path()
        .join("cohorts")
        .join(&first.manifest.cohort_label)
        .join("package.signed");
    assert!(!local.exists());
    assert!(!local.with_file_name("package.signed.part").exists());
    let mut long = good.clone();
    long.push(0);
    host.put(&path, long);
    let error = run(&host, root.path(), launch_held()).unwrap_err();
    assert!(
        format!("{error:#}").contains("more bytes than the object's listed length"),
        "{error:#}"
    );
    // Restored, the same cache completes.
    host.put(&path, good);
    ready(run(&host, root.path(), launch_held())?);
    Ok(())
}

/// A successor whose records are not all on the host yet never produces a
/// bundle, so the installed target stays: not yet listed, listed with an
/// object missing, or listed without its receipts.
#[test]
fn a_partial_successor_keeps_the_installed_target() -> Result<()> {
    let root = scratch()?;
    let (first, second, _) = rv1::cohorts();
    let host = Host::default();
    publish(&host, &first);
    index(&host, &[(&first, true)]);
    point(&host, &fx::chain(), "cohort", &second.authorization, None);
    let held = ActiveTarget::activated(&first.closure.check(
        &first.manifest,
        &first.package,
        Some(&first.target),
    )?)?;
    let code = |outcome: Outcome| outcome.code();
    assert_eq!(
        code(run(&host, root.path(), held.clone())?),
        "target_unlisted"
    );
    publish(&host, &second);
    index(&host, &[(&first, true), (&second, true)]);
    host.remove(&format!("{}{CHUNK}", dir(&second)));
    assert_eq!(
        code(run(&host, root.path(), held.clone())?),
        "object_missing"
    );
    // Published without receipts (receipts withheld from the public host).
    let publication_path = format!("{}publication.json", dir(&second));
    let mut publication: serde_json::Value =
        serde_json::from_slice(&host.get_bytes(&publication_path))?;
    publication["objects"].as_array_mut().unwrap().retain(|o| {
        !matches!(
            o["rel"].as_str(),
            Some("receipt-index.signed" | "chunk-map.json")
        )
    });
    host.put(&publication_path, serde_json::to_vec(&publication)?);
    assert_eq!(
        code(run(&host, root.path(), held.clone())?),
        "receipts_unpublished"
    );
    // Complete: the successor verifies against the held first target.
    publish(&host, &second);
    let staged = ready(run(&host, root.path(), held)?);
    assert_eq!(staged.info.target, target_hex(&second));
    assert_eq!(
        staged.info.activated_epoch,
        Some(second.closure.closed_epoch)
    );
    Ok(())
}

/// Receipts deleted under the N+2 rule: a validator that never fetched them
/// cannot assemble a bundle for a new commitment; one that fetched them while
/// they were retained still can, from its own verified cache.
#[test]
fn receipts_deleted_under_n_plus_2_are_served_only_from_the_cache() -> Result<()> {
    let (first, _, _) = rv1::cohorts();
    let host = Host::default();
    publish(&host, &first);
    index(&host, &[(&first, true)]);
    point(&host, &fx::chain(), "cohort", &first.authorization, None);
    let early = scratch()?;
    ready(run(&host, early.path(), launch_held())?);
    delete_receipts(&host, &first);
    index(&host, &[(&first, false)]);
    let late = scratch()?;
    match run(&host, late.path(), launch_held())? {
        Outcome::ReceiptsExpired { cohort } => assert_eq!(cohort, first.manifest.cohort_label),
        other => panic!("expected expired receipts, got {other:?}"),
    }
    host.clear_log();
    let staged = ready(run(&host, early.path(), launch_held())?);
    assert_eq!(staged.bundle, rv1::bundle(&first));
    assert!(
        host.offsets(&format!("{}{CHUNK}", dir(&first))).is_empty(),
        "never asked the host for deleted receipts"
    );
    Ok(())
}

/// A validator still holding the launch burn row after two targets were
/// published reaches the current one: the intermediate target is checked
/// without its (deleted) receipts and recorded as an advance, and the current
/// bundle verifies against it. A held target outside the published chain is
/// refused.
#[test]
fn a_validator_behind_advances_through_intermediate_targets() -> Result<()> {
    let root = scratch()?;
    let (first, second, _) = rv1::cohorts();
    let host = Host::default();
    publish(&host, &first);
    publish(&host, &second);
    delete_receipts(&host, &first);
    index(&host, &[(&first, false), (&second, true)]);
    point(&host, &fx::chain(), "cohort", &second.authorization, None);
    let staged = ready(run(&host, root.path(), launch_held())?);
    let advance = staged.advance.clone().expect("an intermediate advance");
    assert_eq!(advance.target, hex::encode(first.target.digest()?.0));
    assert_eq!(advance.activated_epoch, Some(first.closure.closed_epoch));
    assert_eq!(staged.info.target, target_hex(&second));
    // The intermediate's package replaced by another validly signed one
    // (listed with matching digests): its bindings refuse it.
    let path = format!("{}package.signed", dir(&first));
    let forged = host.get_bytes(&format!("{}package.signed", dir(&second)));
    let publication_path = format!("{}publication.json", dir(&first));
    let mut publication: serde_json::Value =
        serde_json::from_slice(&host.get_bytes(&publication_path))?;
    for o in publication["objects"].as_array_mut().unwrap() {
        if o["rel"] == "package.signed" {
            o["sha256"] = json!(sha(&forged));
            o["bytes"] = json!(forged.len());
        }
    }
    host.put(&publication_path, serde_json::to_vec(&publication)?);
    host.put(&path, forged);
    let fresh = scratch()?;
    match run(&host, fresh.path(), launch_held())? {
        Outcome::Refused(refusal) => assert!(
            matches!(
                refusal.code,
                "package_manifest_mismatch" | "target_package_mismatch"
            ),
            "{refusal:?}"
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
    // A held target that is not an ancestor of the published one.
    let stranger = ActiveTarget::restore(&fx::chain(), Digest([9; 32]), Some(10))?;
    publish(&host, &first);
    delete_receipts(&host, &first);
    match run(&host, root.path(), stranger)? {
        Outcome::Refused(refusal) => assert_eq!(refusal.code, "held_target_unreachable"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    Ok(())
}

/// The launch pointer assembles the launch bundle, which only a validator
/// still holding the burn row accepts; a pointer for another subnet is an
/// error, and nothing published yet is reported as such.
#[test]
fn the_launch_pointer_and_foreign_or_missing_pointers() -> Result<()> {
    let root = scratch()?;
    let (first, _, _) = rv1::cohorts();
    let host = Host::default();
    assert_eq!(
        run(&host, root.path(), launch_held())?.code(),
        "unpublished"
    );
    let launch = slop_ninja_protocol::LaunchAuthorization::new(&fx::chain(), 1, 10)?;
    let launch = fx::signed(launch)?.to_bytes()?;
    point(&host, &fx::chain(), "launch", &launch, None);
    let staged = ready(run(&host, root.path(), launch_held())?);
    assert_eq!(staged.info.kind, "launch");
    assert_eq!(staged.info.activated_epoch, None);
    assert_eq!(
        staged.info.lease,
        (10, 10 + slop_ninja_protocol::LAUNCH_LEASE_TEMPOS)
    );
    let activated = ActiveTarget::activated(&first.closure.check(
        &first.manifest,
        &first.package,
        Some(&first.target),
    )?)?;
    assert_eq!(
        run(&host, root.path(), activated)?.code(),
        "bundle_unverified"
    );
    let other = ChainBinding {
        netuid: 7,
        ..fx::chain()
    };
    point(&host, &other, "launch", &launch, None);
    let error = run(&host, root.path(), launch_held()).unwrap_err();
    assert!(
        format!("{error:#}").contains("another chain or subnet"),
        "{error:#}"
    );
    Ok(())
}

/// A renewal published with the authorization extends the bundle's coverage.
#[test]
fn a_published_renewal_extends_the_lease_end() -> Result<()> {
    let root = scratch()?;
    let (first, _, _) = rv1::cohorts();
    let host = Host::default();
    publish(&host, &first);
    index(&host, &[(&first, true)]);
    point(&host, &fx::chain(), "cohort", &first.authorization, None);
    let plain = ready(run(&host, root.path(), launch_held())?);
    let checked_closure =
        first
            .closure
            .check(&first.manifest, &first.package, Some(&first.target))?;
    let active = ActiveTarget::activated(&checked_closure)?;
    let signed =
        slop_ninja_protocol::Signed::<slop_ninja_protocol::RewardIntentAuthorization>::from_bytes(
            &first.authorization,
        )?;
    let checked = slop_ninja_protocol::RewardIntentAuthorization::check(
        &signed,
        &slop_ninja_protocol::fixtures::trusted_keys(),
        &first.manifest,
        &first.package,
        &first.target,
        &active,
    )?;
    let renewal_at = plain.info.lease.1 - 1;
    let renewal = fx::signed(slop_ninja_protocol::RewardIntentRenewal::new(
        &checked, 2, renewal_at,
    )?)?
    .to_bytes()?;
    point(
        &host,
        &fx::chain(),
        "cohort",
        &first.authorization,
        Some(&renewal),
    );
    let renewed = ready(run(&host, root.path(), launch_held())?);
    assert_eq!(renewed.info.lease, plain.info.lease);
    assert!(renewed.info.lease_end > plain.info.lease_end);
    let installed = Candidate::of(&plain.sha256, &plain.info, true);
    let staged = Candidate::of(&renewed.sha256, &renewed.info, true);
    assert_eq!(
        supersede::decide(Some(&installed), &staged, 90, &[]),
        Decision::Refresh
    );
    Ok(())
}

fn candidate(
    sha: &str,
    target: &str,
    activated: Option<u64>,
    lease_end: u64,
    covers: bool,
) -> Candidate {
    Candidate {
        sha256: sha.into(),
        target: target.into(),
        activated_epoch: activated,
        lease_end,
        covers_now: covers,
    }
}

fn pending(id: &str, retirable: bool) -> Pending {
    Pending {
        id: id.into(),
        retirable,
    }
}

/// Every branch of the decision: unchanged, refresh by lease or coverage, a
/// lease that is not longer, a successor before its activation, waiting for
/// an in-flight commitment, and a switch that retires never-signed work.
#[test]
fn supersession_waits_for_in_flight_work_and_never_regresses_a_lease() {
    let installed = candidate("a", "t1", Some(80), 96, true);
    let decide = supersede::decide;
    assert_eq!(
        decide(Some(&installed), &installed.clone(), 85, &[]),
        Decision::Keep {
            reason: "unchanged"
        }
    );
    assert_eq!(
        decide(
            Some(&installed),
            &candidate("b", "t1", Some(80), 120, true),
            85,
            &[]
        ),
        Decision::Refresh
    );
    assert_eq!(
        decide(
            Some(&candidate("a", "t1", Some(80), 96, false)),
            &candidate("b", "t1", Some(80), 96, true),
            97,
            &[]
        ),
        Decision::Refresh,
        "a lapsed lease is refreshed by one covering the epoch"
    );
    assert_eq!(
        decide(
            Some(&installed),
            &candidate("b", "t1", Some(80), 90, true),
            85,
            &[]
        ),
        Decision::Keep {
            reason: "lease_not_longer"
        }
    );
    let successor = candidate("c", "t2", Some(100), 116, true);
    assert_eq!(
        decide(Some(&installed), &successor, 99, &[]),
        Decision::Keep {
            reason: "successor_not_yet_active"
        }
    );
    assert_eq!(
        decide(
            Some(&installed),
            &successor,
            100,
            &[pending("p1", false), pending("p2", true)]
        ),
        Decision::Wait {
            in_flight: vec!["p1".into()]
        }
    );
    assert_eq!(
        decide(Some(&installed), &successor, 101, &[pending("p2", true)]),
        Decision::Switch {
            retire: vec!["p2".into()]
        }
    );
    assert_eq!(
        decide(None, &successor, 101, &[]),
        Decision::Switch { retire: Vec::new() }
    );
    assert_eq!(
        decide(None, &successor, 101, &[pending("p1", false)]),
        Decision::Wait {
            in_flight: vec!["p1".into()]
        }
    );
}

#[derive(Default)]
struct FakeInstall {
    retired: Vec<(String, String)>,
    refuse_retire: bool,
    held: Vec<HeldTargetFile>,
    staged: Vec<Vec<u8>>,
    written: Vec<Vec<u8>>,
    order: Vec<&'static str>,
}

impl InstallPorts for FakeInstall {
    fn retire(&mut self, operation_id: &str, reason: &str) -> Result<bool> {
        self.retired.push((operation_id.into(), reason.into()));
        self.order.push("retire");
        Ok(!self.refuse_retire)
    }
    fn record_target(&mut self, held: &HeldTargetFile) -> Result<()> {
        self.held.push(held.clone());
        self.order.push("record");
        Ok(())
    }
    fn stage_bundle(&mut self, bytes: &[u8]) -> Result<()> {
        self.staged.push(bytes.to_vec());
        self.order.push("stage");
        Ok(())
    }
    fn write_bundle(&mut self, bytes: &[u8]) -> Result<()> {
        self.written.push(bytes.to_vec());
        self.order.push("write");
        Ok(())
    }
}

/// A switch retires never-signed work, moves the held record past checked
/// intermediates and installs; it installs nothing if retirement is refused.
/// Keep and wait install nothing; a refresh only rewrites the bundle.
#[test]
fn install_retires_then_advances_then_writes() -> Result<()> {
    let root = scratch()?;
    let (first, second, _) = rv1::cohorts();
    let host = Host::default();
    publish(&host, &first);
    publish(&host, &second);
    index(&host, &[(&first, true), (&second, true)]);
    point(&host, &fx::chain(), "cohort", &second.authorization, None);
    let staged = ready(run(&host, root.path(), launch_held())?);
    let mut ports = FakeInstall::default();
    let switch = Decision::Switch {
        retire: vec!["old".into()],
    };
    assert!(supersede::install(&mut ports, &staged, &switch)?);
    assert_eq!(
        ports.retired,
        vec![("old".to_owned(), "superseded".to_owned())]
    );
    // The bundle is staged before the held record moves past the
    // intermediate (so a crash leaves a staged successor the next start
    // completes, never an installed bundle older than the record), then
    // installed, then the successor's own activation is recorded.
    assert_eq!(
        ports.held,
        vec![
            staged.advance.clone().unwrap(),
            HeldTargetFile::of(&staged.authorized.verified.active)
        ]
    );
    assert_eq!(ports.staged, vec![staged.bundle.clone()]);
    assert_eq!(ports.written, vec![staged.bundle.clone()]);
    assert_eq!(ports.order, ["retire", "stage", "record", "write", "record"]);
    let mut refusing = FakeInstall {
        refuse_retire: true,
        ..FakeInstall::default()
    };
    assert!(supersede::install(&mut refusing, &staged, &switch).is_err());
    assert!(refusing.held.is_empty() && refusing.written.is_empty());
    let mut quiet = FakeInstall::default();
    for decision in [
        Decision::Keep {
            reason: "unchanged",
        },
        Decision::Wait {
            in_flight: vec!["x".into()],
        },
    ] {
        assert!(!supersede::install(&mut quiet, &staged, &decision)?);
    }
    assert!(supersede::install(&mut quiet, &staged, &Decision::Refresh)?);
    assert!(quiet.retired.is_empty() && quiet.held.is_empty() && quiet.staged.is_empty() && quiet.written.len() == 1);
    // The bundle names the cohorts it was built from.
    assert_eq!(staged.cohorts, vec![second.manifest.cohort_label.clone()]);
    assert_eq!(fetch::bundle_cohorts(&staged.bundle)?, staged.cohorts);
    Ok(())
}

/// The real HTTP transport against a loopback server: a full body, a range
/// continued with 206, a range the server ignores (200 restarts the object),
/// a missing object, and a redirect, which is never followed.
#[test]
fn the_http_transport_resumes_ranges_and_refuses_redirects() -> Result<()> {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let server = std::thread::spawn(move || {
        let mut heads = Vec::new();
        let replies: [(&str, &str, &[u8]); 5] = [
            ("200 OK", "", b"hello"),
            (
                "206 Partial Content",
                "Content-Range: bytes 3-4/5\r\n",
                b"lo",
            ),
            ("200 OK", "", b"hello"),
            ("404 Not Found", "", b""),
            (
                "302 Found",
                "Location: http://127.0.0.1:1/elsewhere\r\n",
                b"",
            ),
        ];
        for (status, extra, body) in replies {
            let (mut stream, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).unwrap() == 0 {
                    break;
                }
                head.push(byte[0]);
            }
            heads.push(String::from_utf8_lossy(&head).to_ascii_lowercase());
            let reply = format!(
                "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
        }
        heads
    });
    #[derive(Default)]
    struct Recorder {
        begins: Vec<(Option<u64>, bool)>,
        body: Vec<u8>,
    }
    impl Sink for Recorder {
        fn begin(&mut self, declared: Option<u64>, continues: bool) -> Result<()> {
            self.begins.push((declared, continues));
            Ok(())
        }
        fn write(&mut self, chunk: &[u8]) -> Result<()> {
            self.body.extend_from_slice(chunk);
            Ok(())
        }
    }
    let base = url::Url::parse(&format!("http://127.0.0.1:{port}/root/"))?;
    let transport = fetch::HttpTransport::new(
        base,
        std::time::Duration::from_secs(10),
        std::time::Duration::from_secs(30),
        "slop_ninja_validator/test",
    )?;
    let mut sink = Recorder::default();
    let got = block_on(async {
        let a = transport.get("v1/a.bin", 0, &mut sink).await?;
        let b = transport.get("v1/a.bin", 3, &mut sink).await?;
        let c = transport.get("v1/a.bin", 3, &mut sink).await?;
        let d = transport.get("v1/missing.bin", 0, &mut sink).await?;
        let e = transport.get("v1/a.bin", 0, &mut sink).await;
        Ok::<_, anyhow::Error>((a, b, c, d, e.is_err()))
    })?;
    assert_eq!(
        got,
        (Got::Full, Got::Partial, Got::Full, Got::NotFound, true)
    );
    assert_eq!(
        sink.begins,
        vec![(Some(5), false), (Some(2), true), (Some(5), false)]
    );
    assert_eq!(sink.body, b"hellolohello");
    let heads = server.join().unwrap();
    assert!(
        heads[0].starts_with("get /root/v1/a.bin http/1.1"),
        "{}",
        heads[0]
    );
    assert!(!heads[0].contains("range:"));
    assert!(heads[1].contains("range: bytes=3-"));
    assert!(heads.iter().all(|h| !h.contains("authorization:")));
    assert!(block_on(transport.get("../escape", 0, &mut Recorder::default())).is_err());
    Ok(())
}

fn base_config(root: &Path) -> String {
    include_str!("../validator.example.toml")
        .replace("netuid = 0", "netuid = 601")
        .replace(
            "/home/operator/.local/state/slop_ninja_validator/weights-journal.json",
            &root.join("journal.json").display().to_string(),
        )
}

fn package_section() -> String {
    let keys = slop_ninja_protocol::fixtures::trusted_keys();
    let k = |role| hex::encode(keys.key(role));
    format!(
        "\n[package]\npath = \"/abs/package.json\"\n[package.trusted_keys]\npackage = \"{}\"\nreceipt = \"{}\"\nauthorization = \"{}\"\n",
        k(slop_ninja_protocol::KeyRole::Package),
        k(slop_ninja_protocol::KeyRole::Receipt),
        k(slop_ninja_protocol::KeyRole::Authorization),
    )
}

/// `[fetch]` needs `[package]`, an anonymous HTTPS base URL (plain HTTP only
/// to loopback), an absolute cache and a poll interval no shorter than the
/// index documents' cache lifetime. Dispatch is continuous unless bounded.
#[test]
fn fetch_and_continuous_dispatch_configuration_is_checked() -> Result<()> {
    let root = scratch()?;
    let base = base_config(root.path());
    let dispatch = "\n[dispatch]\nhotkey_file = \"/abs/hotkeys/v2\"\n";
    let fetch =
        |extra: &str| format!("\n[fetch]\nbase_url = \"https://status.example/testnet\"\n{extra}");
    let config = ValidatorConfig::parse(&format!(
        "{base}{}{dispatch}{}",
        package_section(),
        fetch("")
    ))?;
    let f = config.fetch.clone().unwrap();
    assert_eq!(f.base()?.as_str(), "https://status.example/testnet/");
    assert_eq!(f.poll_secs, 300);
    assert_eq!(
        f.cache_dir(&config.journal_path)?,
        root.path().join("public-cache")
    );
    assert_eq!(config.dispatch.as_ref().unwrap().max_finalized_blocks, None);
    let summary = config.summary();
    assert_eq!(summary.dispatch_blocks.as_deref(), Some("continuous"));
    assert_eq!(summary.fetch_host.as_deref(), Some("status.example"));
    let bounded = ValidatorConfig::parse(&format!(
        "{base}{}\n[dispatch]\nhotkey_file = \"/abs/h\"\nmax_finalized_blocks = 1200\n",
        package_section()
    ))?;
    assert_eq!(bounded.summary().dispatch_blocks.as_deref(), Some("1200"));
    ValidatorConfig::parse(&format!(
        "{base}{}\n[fetch]\nbase_url = \"http://127.0.0.1:8080/\"\n",
        package_section()
    ))?;
    for bad in [
        format!("{base}{}", fetch("")),
        format!(
            "{base}{}\n[fetch]\nbase_url = \"http://status.example/\"\n",
            package_section()
        ),
        format!(
            "{base}{}\n[fetch]\nbase_url = \"https://user:pw@status.example/\"\n",
            package_section()
        ),
        format!(
            "{base}{}\n[fetch]\nbase_url = \"https://status.example/?k=v\"\n",
            package_section()
        ),
        format!("{base}{}{}", package_section(), fetch("poll_secs = 59\n")),
        format!(
            "{base}{}{}",
            package_section(),
            fetch("cache_dir = \"relative\"\n")
        ),
        format!(
            "{base}{}\n[dispatch]\nhotkey_file = \"/abs/h\"\nmax_finalized_blocks = 0\n",
            package_section()
        ),
    ] {
        assert!(ValidatorConfig::parse(&bad).is_err(), "accepted: {bad}");
    }
    Ok(())
}

/// The installer's dry run plans the units, a private configuration, the
/// state directory, lingering and the enabled dispatch unit, without
/// touching anything; it refuses an unsafe instance name and `--start`
/// without `--enable`. The dispatch unit restarts always, except on the
/// operator exits, and starts at boot.
#[test]
fn the_installer_plans_persistent_units_with_linger() -> Result<()> {
    let root = scratch()?;
    let home = root.path().join("home");
    std::fs::create_dir(&home)?;
    let deploy = Path::new(env!("CARGO_MANIFEST_DIR")).join("deploy");
    let config = root.path().join("v.toml");
    std::fs::write(&config, "netuid = 601\n")?;
    let script = deploy.join("install.sh");
    let installer = |args: &[&str]| {
        std::process::Command::new("bash")
            .arg(&script)
            .args(args)
            .env("HOME", &home)
            .env("USER", "operator")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_STATE_HOME")
            .output()
    };
    let out = installer(&[
        "--instance",
        "v2",
        "--config",
        &config.display().to_string(),
        "--init",
        "--enable",
        "--start",
        "--dry-run",
    ])?;
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let units = home.join(".config/systemd/user");
    for expected in [
        format!(
            "install -m 0644 {}/slop_ninja_validator@.service",
            deploy.display()
        ),
        format!(
            "install -m 0600 {} {}/.config/slop_ninja_validator/v2.toml",
            config.display(),
            home.display()
        ),
        format!(
            "install -d -m 0700 {}/.config/slop_ninja_validator {}/.local/state/slop_ninja_validator",
            home.display(),
            home.display()
        ),
        format!("{}/", units.display()),
        "loginctl enable-linger operator".to_owned(),
        "systemctl --user daemon-reload".to_owned(),
        "systemctl --user enable --now slop_ninja_validator-dispatch@v2.service".to_owned(),
        "init".to_owned(),
    ] {
        assert!(text.contains(&expected), "missing {expected:?} in:\n{text}");
    }
    assert!(!home.join(".config").exists(), "a dry run touches nothing");
    for bad in [
        vec!["--instance", "../x", "--dry-run"],
        vec!["--instance", "v2", "--start", "--dry-run"],
        vec!["--dry-run"],
    ] {
        let out = installer(&bad)?;
        assert_eq!(out.status.code(), Some(2), "{bad:?}");
    }
    let unit = std::fs::read_to_string(deploy.join("slop_ninja_validator-dispatch@.service"))?;
    for line in [
        "Restart=always",
        "RestartPreventExitStatus=2 3",
        "WantedBy=default.target",
        "MemorySwapMax=0",
        "run --dispatch",
    ] {
        assert!(
            unit.lines().any(|l| l.contains(line)),
            "dispatch unit lacks {line}"
        );
    }
    assert!(!unit.contains("Restart=on-failure"));
    // A fixed delay: systemd's step backoff never resets while the unit
    // keeps being restarted, so it would stay at its maximum.
    assert!(unit.lines().any(|l| l.trim() == "RestartSec=60"));
    assert!(!unit.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("RestartSteps=") || l.starts_with("RestartMaxDelaySec=")
    }));
    Ok(())
}

/// `--from` installs only binaries that SHA256SUMS lists exactly once each
/// and that match it; without SHA256SUMS only `--unverified` installs.
#[test]
fn the_installer_requires_both_binaries_in_sha256sums() -> Result<()> {
    let root = scratch()?;
    let home = root.path().join("home");
    let release = root.path().join("release");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&release)?;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("deploy/install.sh");
    let config = root.path().join("v.toml");
    std::fs::write(&config, "netuid = 601\n")?;
    std::fs::write(release.join("slop_ninja_validator"), b"validator")?;
    std::fs::write(release.join("slop-ninja-journal"), b"journal")?;
    std::fs::write(release.join("other"), b"other")?;
    let line = |name: &str| -> Result<String> {
        Ok(format!("{}  {name}\n", sha(&std::fs::read(release.join(name))?)))
    };
    let installer = |extra: &[&str]| -> Result<i32> {
        let mut args = vec![
            "--instance".to_owned(),
            "v2".to_owned(),
            "--from".to_owned(),
            release.display().to_string(),
            "--config".to_owned(),
            config.display().to_string(),
            "--dry-run".to_owned(),
        ];
        args.extend(extra.iter().map(|a| (*a).to_owned()));
        let out = std::process::Command::new("bash")
            .arg(&script)
            .args(&args)
            .env("HOME", &home)
            .env("USER", "operator")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_STATE_HOME")
            .output()?;
        Ok(out.status.code().unwrap_or(-1))
    };
    let sums = release.join("SHA256SUMS");
    // Missing: refused unless explicitly unverified.
    assert_eq!(installer(&[])?, 2);
    assert_eq!(installer(&["--unverified"])?, 0);
    // Lists only another file: the binaries are unchecked, so refused.
    std::fs::write(&sums, line("other")?)?;
    assert_eq!(installer(&[])?, 2);
    // Both listed and matching.
    std::fs::write(&sums, line("slop_ninja_validator")? + &line("slop-ninja-journal")?)?;
    assert_eq!(installer(&[])?, 0);
    // Listed twice, or tampered after listing: refused.
    std::fs::write(
        &sums,
        line("slop_ninja_validator")? + &line("slop_ninja_validator")? + &line("slop-ninja-journal")?,
    )?;
    assert_eq!(installer(&[])?, 2);
    std::fs::write(&sums, line("slop_ninja_validator")? + &line("slop-ninja-journal")?)?;
    std::fs::write(release.join("slop_ninja_validator"), b"tampered")?;
    assert_eq!(installer(&[])?, 2);
    Ok(())
}

/// The install path through the real file journal: a successor found while
/// an operation of the old target is queued but never signed is kept by a
/// dry run (which never writes the journal) and before its activation; at
/// the activation a dispatch run retires the operation as `superseded` and
/// installs the successor's bundle over `[package] path`.
#[test]
fn a_switch_retires_never_signed_work_in_the_real_journal_and_a_dry_run_never_does() -> Result<()> {
    use crate::app::AppPorts as _;
    let root = scratch()?;
    let (first, second, specs) = rv1::cohorts();
    let package_path = root.path().join("package.json");
    let text = format!(
        "{}{}\n[fetch]\nbase_url = \"https://status.example/\"\n",
        base_config(root.path())
            .replace("netuid = 601", &format!("netuid = {}", rv1::RV1_NETUID))
            .replace(
                "/home/operator/.bittensor/wallets",
                &root.path().join("wallets").display().to_string()
            ),
        package_section().replace("/abs/package.json", &package_path.display().to_string()),
    );
    let config = ValidatorConfig::parse(&text)?;
    let validator = [0x66; 32];
    let identity = slop_ninja_subnet::ChainIdentity {
        genesis: rtao_primitives::H256(slop_ninja_protocol::TESTNET_GENESIS.0),
        netuid: rtao_primitives::NetUid(rv1::RV1_NETUID),
        hotkey: rtao_primitives::AccountId(validator),
    };
    crate::live::init_journal(&config, identity.clone())?;
    let mut live = crate::live::LivePorts::new(config, identity);
    crate::startup::StartupPorts::open_journal(&mut live)?;
    live.unpause_for_offline_tests()?;

    // The first target is installed and held; its epoch-80 commitment is queued.
    let keys = slop_ninja_protocol::fixtures::trusted_keys();
    let first_bytes = rv1::bundle(&first);
    let authorized = std::sync::Arc::new(
        crate::package_v2::verify(
            &first_bytes,
            &keys,
            &fx::chain().genesis.0,
            rv1::RV1_NETUID,
            &launch_held(),
        )
        .unwrap(),
    );
    crate::app::AppPorts::record_target(&mut live, &HeldTargetFile::of(&authorized.verified.active))?;
    std::fs::write(&package_path, &first_bytes)?;
    let snapshot =
        crate::package_v2::snapshot(&rv1::snapshot(&specs), fx::owner_hotkey().0).unwrap();
    let derived = crate::package_v2::derive(authorized.clone(), &snapshot, validator, 80).unwrap();
    let op = match live.enqueue(&derived.work, derived.input.clone(), slop_ninja_subnet::OwnerRowPolicy::Hold)? {
        slop_ninja_subnet::Enqueued::Created(id) => id,
        other => panic!("expected a new operation, got {other:?}"),
    };
    let info = fetch::BundleInfo::of(&first_bytes, &authorized)?;
    let installed = Candidate::of(&sha(&first_bytes), &info, true);

    // The successor, fetched and verified against the held first target.
    let host = Host::default();
    publish(&host, &first);
    publish(&host, &second);
    index(&host, &[(&first, true), (&second, true)]);
    point(&host, &fx::chain(), "cohort", &second.authorization, None);
    let held = ActiveTarget::activated(&first.closure.check(
        &first.manifest,
        &first.package,
        Some(&first.target),
    )?)?;
    let staged = ready(run(&host, &root.path().join("public-cache"), held)?);
    let activation = second.closure.closed_epoch;

    let (decision, installed_now) =
        live.settle_staged(&staged, Some(&installed), activation, false)?;
    assert_eq!(
        (decision, installed_now),
        (
            Decision::Keep {
                reason: "switch_needs_dispatch"
            },
            false
        )
    );
    let (decision, installed_now) =
        live.settle_staged(&staged, Some(&installed), activation - 1, true)?;
    assert_eq!(
        (decision, installed_now),
        (
            Decision::Keep {
                reason: "successor_not_yet_active"
            },
            false
        )
    );
    assert_eq!(std::fs::read(&package_path)?, first_bytes);
    assert_eq!(live.pending_operations(0)?.len(), 1);

    let (decision, installed_now) =
        live.settle_staged(&staged, Some(&installed), activation, true)?;
    assert_eq!(
        decision,
        Decision::Switch {
            retire: vec![op.clone()]
        }
    );
    assert!(installed_now);
    assert_eq!(std::fs::read(&package_path)?, staged.bundle);
    assert!(live.pending_operations(0)?.is_empty());
    let controls = live.controls_for_tests().unwrap();
    assert_eq!(controls.retired(&op)?.as_deref(), Some("superseded"));
    // The switch recorded the successor's activation, bound to the keys.
    let held_path = crate::live::held_target_path(&root.path().join("journal.json"));
    let held = crate::live::read_held(&held_path, &keys, true)?.expect("a held record");
    assert_eq!(held.target, hex::encode(second.target.digest()?.0));
    assert_eq!(held.authority, Some(crate::package_v2::authority_digest(&keys)));
    // A stale or rolled-back pointer naming the first target right after the
    // switch: refused against the held successor, and never a switch back.
    point(&host, &fx::chain(), "cohort", &first.authorization, None);
    let held_now = held.restore(&fx::chain()).map_err(|r| anyhow::anyhow!("{}", r.code))?;
    match run(&host, &root.path().join("public-cache"), held_now)? {
        Outcome::Refused(_) => {}
        other => panic!("expected the stale pointer to be refused, got {other:?}"),
    }
    let stale = ready(run(
        &host,
        &root.path().join("public-cache"),
        ActiveTarget::activated(&first.closure.check(
            &first.manifest,
            &first.package,
            Some(&first.target),
        )?)?,
    )?);
    let now_installed = Candidate::of(&staged.sha256, &staged.info, true);
    let (decision, installed_now) =
        live.settle_staged(&stale, Some(&now_installed), activation + 1, true)?;
    assert_eq!(
        (decision, installed_now),
        (Decision::Keep { reason: "regression" }, false)
    );
    assert_eq!(std::fs::read(&package_path)?, staged.bundle);
    Ok(())
}

/// Review finding 8: a switch that moves the held record past intermediate
/// targets stages its bundle first. A crash after the staging but before the
/// record leaves the installed bundle (the staged one is discarded at the
/// next start); a crash after the record but before the install leaves a
/// staged successor the next start installs, never an installed bundle older
/// than the held record. `reset-target` sets a staged bundle aside too.
#[test]
fn an_interrupted_install_past_intermediates_is_completed_or_discarded() -> Result<()> {
    let root = scratch()?;
    let (first, second, _) = rv1::cohorts();
    let package_path = root.path().join("package.json");
    let next_path = crate::live::staged_bundle_path(&package_path);
    let text = format!(
        "{}{}\n[fetch]\nbase_url = \"https://status.example/\"\n",
        base_config(root.path())
            .replace("netuid = 601", &format!("netuid = {}", rv1::RV1_NETUID))
            .replace(
                "/home/operator/.bittensor/wallets",
                &root.path().join("wallets").display().to_string()
            ),
        package_section().replace("/abs/package.json", &package_path.display().to_string()),
    );
    let config = ValidatorConfig::parse(&text)?;
    let identity = slop_ninja_subnet::ChainIdentity {
        genesis: rtao_primitives::H256(slop_ninja_protocol::TESTNET_GENESIS.0),
        netuid: rtao_primitives::NetUid(rv1::RV1_NETUID),
        hotkey: rtao_primitives::AccountId([0x66; 32]),
    };
    crate::live::init_journal(&config, identity.clone())?;
    let mut live = crate::live::LivePorts::new(config.clone(), identity);
    crate::startup::StartupPorts::open_journal(&mut live)?;
    live.unpause_for_offline_tests()?;
    let keys = slop_ninja_protocol::fixtures::trusted_keys();
    let held_path = crate::live::held_target_path(&root.path().join("journal.json"));

    // The launch bundle is installed and nothing is held; the fetched
    // successor reaches the second target through the first.
    let launch = rv1::launch(70);
    std::fs::write(&package_path, &launch)?;
    let host = Host::default();
    publish(&host, &first);
    publish(&host, &second);
    index(&host, &[(&first, true), (&second, true)]);
    point(&host, &fx::chain(), "cohort", &second.authorization, None);
    let staged = ready(run(&host, &root.path().join("public-cache"), launch_held())?);
    let advance = staged.advance.clone().expect("an intermediate target");
    assert_eq!(live.complete_install()?, None);

    // Crash after staging, before the held record moved: discarded.
    InstallPorts::stage_bundle(&mut live, &staged.bundle)?;
    assert_eq!(std::fs::read(&next_path)?, staged.bundle);
    assert_eq!(live.complete_install()?, Some("discarded"));
    assert!(!next_path.exists());
    assert_eq!(std::fs::read(&package_path)?, launch);
    assert!(crate::live::read_held(&held_path, &keys, true)?.is_none());

    // Crash after the held record moved past the intermediate, before the
    // install: the staged successor is installed and its activation held.
    InstallPorts::stage_bundle(&mut live, &staged.bundle)?;
    crate::app::AppPorts::record_target(&mut live, &advance)?;
    assert_eq!(live.complete_install()?, Some("completed"));
    assert!(!next_path.exists());
    assert_eq!(std::fs::read(&package_path)?, staged.bundle);
    let held = crate::live::read_held(&held_path, &keys, true)?.expect("a held record");
    assert_eq!(held.target, hex::encode(second.target.digest()?.0));

    // A staged bundle left at a reset is set aside with the rest.
    InstallPorts::stage_bundle(&mut live, &staged.bundle)?;
    drop(live);
    let done = crate::live::reset_target(&config)?;
    assert_eq!(done["bundle_set_aside"], json!(true));
    assert!(!next_path.exists() && !package_path.exists() && !held_path.exists());
    Ok(())
}

/// Resident set size and its high-water mark, KiB.
fn rss_kib() -> (u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kib| kib.parse().ok())
            .unwrap_or(0)
    };
    (field("VmRSS:"), field("VmHWM:"))
}

/// Review finding 31: the fetch path's peak memory with a bundle at
/// [`crate::package::MAX_BUNDLE_BYTES`], for the dispatch unit's
/// `MemoryMax`. The worst a continuous run holds at once: the installed
/// bundle's verification, the loop's newest staged bundle, one staged
/// bundle waiting in the fetcher's channel, and a session assembling and
/// verifying another. Linux only; run alone (`--ignored --nocapture
/// --test-threads=1`); it prints the peak above the starting set.
#[test]
#[ignore = "measures peak memory at the bundle bound; run alone"]
fn peak_memory_at_the_bundle_bound() -> Result<()> {
    let root = scratch()?;
    let size = |n: u64| rv1::bundle(&rv1::sized(n)).len() as u64;
    let (small, large) = (size(1_000), size(2_000));
    let per_receipt = (large - small) / 1_000;
    let fixed = small - 1_000 * per_receipt;
    // Within the bound with a little room for the length estimate.
    let receipts = (crate::package::MAX_BUNDLE_BYTES - fixed) / per_receipt * 99 / 100;
    let cohort = rv1::sized(receipts);
    let host = Host::default();
    publish(&host, &cohort);
    index(&host, &[(&cohort, true)]);
    point(&host, &fx::chain(), "cohort", &cohort.authorization, None);
    drop(cohort);
    let cache = root.path().join("public-cache");
    let keys: TrustedKeys = slop_ninja_protocol::fixtures::trusted_keys();
    let (base, _) = rss_kib();
    std::fs::write("/proc/self/clear_refs", "5")?;
    let first = ready(run(&host, &cache, launch_held())?);
    let (_, one_session) = rss_kib();
    let installed = crate::package_v2::verify(&first.bundle, &keys, &fx::chain().genesis.0, rv1::RV1_NETUID, &launch_held())
        .map_err(|r| anyhow::anyhow!("{}", r.code))?;
    let latest = ready(run(&host, &cache, launch_held())?);
    let waiting = ready(run(&host, &cache, launch_held())?);
    let (_, all) = rss_kib();
    let report = json!({
        "receipts": receipts,
        "bundle_bytes": first.bundle.len(),
        "bound_bytes": crate::package::MAX_BUNDLE_BYTES,
        "base_kib": base,
        "peak_one_session_kib": one_session.saturating_sub(base),
        "peak_held_and_session_kib": all.saturating_sub(base),
    });
    println!("peak_memory_at_the_bundle_bound {report}");
    assert!(first.bundle.len() as u64 <= crate::package::MAX_BUNDLE_BYTES);
    assert_eq!(installed.receipts_verified, receipts);
    drop((first, latest, waiting, installed));
    Ok(())
}

fn activated(c: &Cohort) -> Result<ActiveTarget> {
    Ok(ActiveTarget::activated(&c.closure.check(
        &c.manifest,
        &c.package,
        Some(&c.target),
    )?)?)
}

fn cohort_dirs(cache: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(cache.join("cohorts"))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Review R1: an authorization object must have its record's exact signed
/// length (refused before any read otherwise) and a valid signature (refused
/// before anything else is fetched); it is never written to the cache.
#[test]
fn authorization_objects_are_checked_in_memory_and_never_cached() -> Result<()> {
    let root = scratch()?;
    let (first, _, _) = rv1::cohorts();
    let host = Host::default();
    publish(&host, &first);
    index(&host, &[(&first, true)]);
    let (launch_len, cohort_len, renewal_len) = fetch::authorization_lengths(&fx::chain())?;
    // The schema's lengths (the README's pointer example states them).
    assert_eq!((launch_len, cohort_len, renewal_len), (230, 309, 196));
    assert_eq!(first.authorization.len() as u64, cohort_len);
    let junk = vec![7u8; 1 << 20];
    point(&host, &fx::chain(), "cohort", &junk, None);
    host.clear_log();
    match run(&host, root.path(), launch_held())? {
        Outcome::Refused(refusal) => assert_eq!(refusal.code, "authorization_malformed"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(
        host.offsets(&format!("v1/authorizations/{}.signed", sha(&junk)))
            .is_empty(),
        "an object of the wrong length is never requested"
    );
    let forged = vec![7u8; usize::try_from(cohort_len)?];
    point(&host, &fx::chain(), "cohort", &forged, None);
    let outcome = run(&host, root.path(), launch_held())?;
    assert!(outcome.alert());
    assert_eq!(outcome.code(), "authorization_unverified");
    assert!(host.offsets(fetch::INDEX_PATH).is_empty(), "nothing else was read");
    assert!(!root.path().join("authorizations").exists());
    assert!(cohort_dirs(root.path()).is_empty());
    Ok(())
}

/// Rewrites the chunk map and its publication listing.
fn edit_chunk_map(host: &Host, c: &Cohort, edit: impl Fn(&mut serde_json::Value)) -> Result<()> {
    let map_path = format!("{}chunk-map.json", dir(c));
    let mut map: serde_json::Value = serde_json::from_slice(&host.get_bytes(&map_path))?;
    edit(&mut map);
    let map_bytes = serde_json::to_vec(&map)?;
    host.put(&map_path, map_bytes.clone());
    let pub_path = format!("{}publication.json", dir(c));
    let mut publication: serde_json::Value = serde_json::from_slice(&host.get_bytes(&pub_path))?;
    for o in publication["objects"].as_array_mut().unwrap() {
        if o["rel"] == "chunk-map.json" {
            o["sha256"] = json!(sha(&map_bytes));
            o["bytes"] = json!(map_bytes.len());
        }
    }
    host.put(&pub_path, serde_json::to_vec(&publication)?);
    Ok(())
}

/// Lists an object in a cohort's publication record.
fn list_object(host: &Host, c: &Cohort, rel: &str, bytes: &[u8]) -> Result<()> {
    let pub_path = format!("{}publication.json", dir(c));
    let mut publication: serde_json::Value = serde_json::from_slice(&host.get_bytes(&pub_path))?;
    publication["objects"]
        .as_array_mut()
        .unwrap()
        .push(json!({"rel": rel, "sha256": sha(bytes), "bytes": bytes.len()}));
    host.put(&pub_path, serde_json::to_vec(&publication)?);
    Ok(())
}

/// Review R2: receipt bundles are bound to the signed receipt index before
/// any is read. An extra bundle (listed, digest-correct, but not accounted
/// for by the index's entries), another receipt root, or a bundle the
/// publication record does not list is refused without reading a bundle,
/// so nothing unaccounted for reaches the cache or memory.
#[test]
fn receipt_bundles_are_bound_to_the_signed_index_before_any_is_read() -> Result<()> {
    let (first, _, _) = rv1::cohorts();
    let label = first.manifest.cohort_label.clone();
    let chunk_path = format!("{}{CHUNK}", dir(&first));
    let fresh_host = || {
        let host = Host::default();
        publish(&host, &first);
        index(&host, &[(&first, true)]);
        point(&host, &fx::chain(), "cohort", &first.authorization, None);
        host
    };

    // An extra bundle, listed in both documents with its true digest.
    let host = fresh_host();
    let extra_rel = "receipts/bundle-job-9.bin";
    let extra = vec![1u8; 1 << 20];
    host.put(&format!("{}{extra_rel}", dir(&first)), extra.clone());
    list_object(&host, &first, extra_rel, &extra)?;
    edit_chunk_map(&host, &first, |map| {
        map["objects"].as_array_mut().unwrap().push(json!({
            "key": format!("cohorts/{label}/{extra_rel}"),
            "sha256": sha(&extra),
            "bytes": extra.len(),
            "receipts": 0,
        }));
    })?;
    let root = scratch()?;
    let outcome = run(&host, root.path(), launch_held())?;
    assert_eq!(outcome.code(), "chunk_map_mismatch");
    assert!(outcome.alert());
    assert!(host.offsets(&format!("{}{extra_rel}", dir(&first))).is_empty());
    assert!(host.offsets(&chunk_path).is_empty());

    // Another receipt root.
    let host = fresh_host();
    edit_chunk_map(&host, &first, |map| map["receipt_root"] = json!("00".repeat(32)))?;
    let root = scratch()?;
    assert_eq!(run(&host, root.path(), launch_held())?.code(), "chunk_map_mismatch");
    assert!(host.offsets(&chunk_path).is_empty());

    // The same bytes under a name only the chunk map gives.
    let host = fresh_host();
    let hidden_rel = "receipts/bundle-job-7.bin";
    host.put(&format!("{}{hidden_rel}", dir(&first)), first.chunk.clone());
    edit_chunk_map(&host, &first, |map| {
        map["objects"][0]["key"] = json!(format!("cohorts/{label}/{hidden_rel}"));
    })?;
    let root = scratch()?;
    assert_eq!(run(&host, root.path(), launch_held())?.code(), "object_unlisted");
    assert!(host.offsets(&format!("{}{hidden_rel}", dir(&first))).is_empty());
    Ok(())
}

/// Every session that located a cohort prunes the cache to the cohorts it
/// named and those of the installed bundle, whatever its outcome.
#[test]
fn every_session_prunes_the_cache_to_what_it_named_and_the_installed_cohorts() -> Result<()> {
    let root = scratch()?;
    let (first, second, _) = rv1::cohorts();
    let host = Host::default();
    publish(&host, &first);
    publish(&host, &second);
    index(&host, &[(&first, true), (&second, true)]);
    let (one, two) = (
        first.manifest.cohort_label.clone(),
        second.manifest.cohort_label.clone(),
    );
    point(&host, &fx::chain(), "cohort", &second.authorization, None);
    ready(run(&host, root.path(), activated(&first)?)?);
    assert_eq!(cohort_dirs(root.path()), vec![two.clone()]);
    // A pointer to the first target, with the second installed: both kept.
    point(&host, &fx::chain(), "cohort", &first.authorization, None);
    ready(run_with(&host, root.path(), activated(&first)?, None, std::slice::from_ref(&two))?);
    assert_eq!(cohort_dirs(root.path()), vec![one.clone(), two.clone()]);
    // A failed session prunes too: only what it named survives. (The
    // publication record now lists other bytes than the host serves.)
    let pub_path = format!("{}publication.json", dir(&first));
    let mut publication: serde_json::Value = serde_json::from_slice(&host.get_bytes(&pub_path))?;
    for o in publication["objects"].as_array_mut().unwrap() {
        if o["rel"] == "package.signed" {
            let mut other = host.get_bytes(&format!("{}package.signed", dir(&first)));
            let last = other.len() - 1;
            other[last] ^= 1;
            o["sha256"] = json!(sha(&other));
        }
    }
    host.put(&pub_path, serde_json::to_vec(&publication)?);
    assert_eq!(
        run(&host, root.path(), activated(&first)?)?.code(),
        "object_digest_mismatch"
    );
    assert_eq!(cohort_dirs(root.path()), vec![one]);
    Ok(())
}

/// A session whose documents and held target equal the last bundle's
/// reports `unchanged` without requesting, hashing or verifying any cohort
/// object; a changed held target is a full session again.
#[test]
fn an_unchanged_session_reads_no_object() -> Result<()> {
    let root = scratch()?;
    let (first, _, _) = rv1::cohorts();
    let host = Host::default();
    publish(&host, &first);
    index(&host, &[(&first, true)]);
    point(&host, &fx::chain(), "cohort", &first.authorization, None);
    let staged = ready(run(&host, root.path(), launch_held())?);
    host.clear_log();
    let outcome = run_with(&host, root.path(), launch_held(), Some(&staged.fingerprint), &[])?;
    assert!(matches!(outcome, Outcome::Unchanged), "{outcome:?}");
    for rel in ["package.signed", "target.signed", CHUNK] {
        assert!(host.offsets(&format!("{}{rel}", dir(&first))).is_empty(), "{rel}");
    }
    let again = ready(run_with(
        &host,
        root.path(),
        activated(&first)?,
        Some(&staged.fingerprint),
        &[],
    )?);
    assert_eq!(again.info.target, staged.info.target);
    assert_ne!(again.fingerprint, staged.fingerprint);
    // Content changes in a document: a full session.
    index(&host, &[(&first, true)]);
    let pub_path = format!("{}publication.json", dir(&first));
    let mut publication: serde_json::Value = serde_json::from_slice(&host.get_bytes(&pub_path))?;
    publication["note"] = json!("rewritten");
    host.put(&pub_path, serde_json::to_vec(&publication)?);
    ready(run_with(&host, root.path(), launch_held(), Some(&staged.fingerprint), &[])?);
    Ok(())
}

/// Review R3: a host trickling one byte every 1.5 s (never stalling for the
/// 2 s stall bound) is cut off by the request's total bound.
#[test]
fn a_trickling_host_is_cut_off_by_the_total_bound() -> Result<()> {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).unwrap_or(0) == 0 {
                break;
            }
            head.push(byte[0]);
        }
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n");
        for _ in 0..1000 {
            if stream.write_all(b"x").is_err() {
                break;
            }
            let _ = stream.flush();
            std::thread::sleep(std::time::Duration::from_millis(1500));
        }
    });
    #[derive(Default)]
    struct Count(usize);
    impl Sink for Count {
        fn begin(&mut self, _: Option<u64>, _: bool) -> Result<()> {
            Ok(())
        }
        fn write(&mut self, chunk: &[u8]) -> Result<()> {
            self.0 += chunk.len();
            Ok(())
        }
    }
    let base = url::Url::parse(&format!("http://127.0.0.1:{port}/"))?;
    let transport = fetch::HttpTransport::new(
        base,
        std::time::Duration::from_secs(2),
        std::time::Duration::from_secs(4),
        "slop_ninja_validator/test",
    )?;
    let mut sink = Count::default();
    let start = std::time::Instant::now();
    let result = block_on(async {
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            transport.get("v1/a.bin", 0, &mut sink),
        )
        .await
    });
    let elapsed = start.elapsed();
    assert!(matches!(result, Ok(Err(_))), "the request ends with an error");
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "cut off by the total bound, after {elapsed:?}"
    );
    Ok(())
}

/// A held-target record written under other trust roots is set aside and
/// the validator starts from the launch burn row; one written before records
/// named their authority is bound to the current keys. A dry run moves
/// nothing.
#[test]
fn a_held_record_of_another_authority_is_set_aside() -> Result<()> {
    let root = scratch()?;
    let (first, _, _) = rv1::cohorts();
    let path = crate::live::held_target_path(&root.path().join("journal.json"));
    let keys = slop_ninja_protocol::fixtures::trusted_keys();
    let legacy = HeldTargetFile::of(&activated(&first)?);
    std::fs::write(&path, serde_json::to_vec(&legacy)?)?;
    assert_eq!(crate::live::read_held(&path, &keys, false)?, Some(legacy.clone()));
    assert_eq!(std::fs::read(&path)?, serde_json::to_vec(&legacy)?, "a dry run binds nothing");
    assert_eq!(crate::live::read_held(&path, &keys, true)?, Some(legacy.clone()));
    let bound: HeldTargetFile = serde_json::from_slice(&std::fs::read(&path)?)?;
    assert_eq!(bound.authority, Some(crate::package_v2::authority_digest(&keys)));
    assert_eq!(crate::live::read_held(&path, &keys, true)?, Some(bound.clone()));
    let rotated = TrustedKeys::new([1; 32], [2; 32], [3; 32])?;
    assert_eq!(crate::live::read_held(&path, &rotated, false)?, None);
    assert!(path.exists(), "a dry run sets nothing aside");
    assert_eq!(crate::live::read_held(&path, &rotated, true)?, None);
    assert!(!path.exists());
    let aside = std::fs::read_dir(root.path())?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(".held-target.json.authority-"))
        .count();
    assert_eq!(aside, 1);
    Ok(())
}

/// A fake dispatch loop for [`supersede::FetchState`]: the finalized epoch,
/// the journal's pending operations and the installed bundle.
struct FakeLoop {
    epoch: u64,
    epoch_reads: u32,
    pending: Vec<Pending>,
    installed: Option<Candidate>,
    install: FakeInstall,
}

impl InstallPorts for FakeLoop {
    fn retire(&mut self, operation_id: &str, reason: &str) -> Result<bool> {
        self.install.retire(operation_id, reason)
    }
    fn record_target(&mut self, held: &HeldTargetFile) -> Result<()> {
        self.install.record_target(held)
    }
    fn stage_bundle(&mut self, bytes: &[u8]) -> Result<()> {
        self.install.stage_bundle(bytes)
    }
    fn write_bundle(&mut self, bytes: &[u8]) -> Result<()> {
        self.install.write_bundle(bytes)
    }
}

impl supersede::LoopPorts for FakeLoop {
    async fn finalized_epoch(&mut self) -> Result<u64> {
        self.epoch_reads += 1;
        Ok(self.epoch)
    }
    fn pending(&self) -> Result<Vec<Pending>> {
        Ok(self.pending.clone())
    }
    fn installed_sha(&self) -> Option<String> {
        self.installed.as_ref().map(|c| c.sha256.clone())
    }
    fn installed(&mut self, _epoch: u64) -> Result<Option<Candidate>> {
        Ok(self.installed.clone())
    }
    fn covers(&self, staged: &Staged, epoch: u64) -> bool {
        staged.info.lease.0 <= epoch && epoch <= staged.info.lease_end
    }
}

/// Review finding 1, the dispatch loop's decisions end to end: a successor
/// found while the old target's last commitment is in flight waits (and
/// re-checks each block without a chain read, queuing no old-target
/// commitment), switches once that commitment is handed back, and the
/// old commitment's reveal, completed after the switch, is compared with
/// the old target's own row from the reveal ledger, never with the
/// successor's. A reveal no ledger entry accounts for is only observed.
#[test]
fn the_dispatch_loop_waits_then_switches_then_compares_the_old_reveal() -> Result<()> {
    use crate::reveals::{Check, Completed, OpState, RevealLedger};
    use crate::supersede::{FetchState, LoopStep};
    let root = scratch()?;
    let (first, second, _) = rv1::cohorts();
    let host = Host::default();
    publish(&host, &first);
    publish(&host, &second);
    index(&host, &[(&first, true), (&second, true)]);
    point(&host, &fx::chain(), "cohort", &first.authorization, None);
    let installed = ready(run(&host, root.path(), activated(&first)?)?);
    point(&host, &fx::chain(), "cohort", &second.authorization, None);
    let successor = ready(run(&host, root.path(), activated(&first)?)?);
    let activation = second.closure.closed_epoch;
    let mut ports = FakeLoop {
        epoch: activation,
        epoch_reads: 0,
        pending: vec![pending("old-tempo", false)],
        installed: Some(Candidate::of(&installed.sha256, &installed.info, true)),
        install: FakeInstall::default(),
    };
    let mut state = FetchState::default();
    let step = |state: &mut FetchState, ports: &mut FakeLoop, outcome| {
        block_on(state.step(ports, outcome))
    };
    // A refusal while nothing waits: no chain read, nothing installed.
    let refused = Outcome::Refused(crate::package::Refusal::new("outcome_mismatch", "test"));
    assert_eq!(step(&mut state, &mut ports, Some(refused))?, LoopStep::Continue);
    assert_eq!((ports.epoch_reads, state.last_code()), (0, "outcome_mismatch"));
    // The successor arrives while the old commitment is in flight.
    assert_eq!(
        step(&mut state, &mut ports, Some(Outcome::Ready(successor)))?,
        LoopStep::Continue
    );
    assert!(state.waiting(), "no old-target commitment is queued meanwhile");
    assert_eq!(ports.epoch_reads, 1);
    assert!(ports.install.written.is_empty());
    // Blocks without a new outcome re-check at the stored epoch.
    ports.epoch = activation + 5;
    assert_eq!(step(&mut state, &mut ports, None)?, LoopStep::Continue);
    assert!(state.waiting());
    assert_eq!(ports.epoch_reads, 1, "no chain read while waiting");
    // The commitment is handed back: the next block switches.
    ports.pending.clear();
    assert_eq!(step(&mut state, &mut ports, None)?, LoopStep::Switched);
    assert!(!state.waiting());
    assert_eq!(ports.install.written.len(), 1);
    assert_eq!(
        ports.install.held.last().map(|h| h.target.clone()),
        Some(hex::encode(second.target.digest()?.0))
    );

    // The old target's last commitment was recorded when it was enqueued.
    let ledger_path = crate::reveals::ledger_path(&root.path().join("journal.json"));
    let old_row = vec![(0u16, 65_535u16), (3, 12_000)];
    let successor_row = vec![(0u16, 65_535u16)];
    let mut before = RevealLedger::load(ledger_path.clone());
    before.record("old-tempo", "hash-old", old_row.clone());
    before.persist()?;
    // After the switch (a fresh loop, or a restart) it is still known.
    let mut ledger = RevealLedger::load(ledger_path.clone());
    ledger.record("new-tempo", "hash-new", successor_row);
    ledger.settle(|entry| {
        Ok(match entry.operation_id.as_str() {
            "old-tempo" => OpState::Landed(8_351_284),
            _ => OpState::Pending,
        })
    })?;
    // Its reveal already completed, so its commit epoch is read at the block.
    assert_eq!(ledger.assign_epochs(&[], 1), vec![8_351_284]);
    ledger.set_epoch(8_351_284, 54);
    let earlier = Completed {
        commit_epoch: 53,
        reveal_epoch: 54,
        round: 6,
        observed_block: 8_351_000,
        missed: false,
    };
    let old_reveal = Completed {
        commit_epoch: 54,
        reveal_epoch: 55,
        round: 7,
        observed_block: 8_351_368,
        missed: false,
    };
    let checks = ledger.checks(&[earlier, old_reveal]);
    assert_eq!(checks.len(), 1, "the earlier, unattributed outcome predates the run");
    match &checks[0] {
        Check::Compare { entry, outcome } => {
            assert_eq!(entry.operation_id, "old-tempo");
            assert_eq!(entry.entries, old_row, "compared with its own row");
            assert_eq!(*outcome, old_reveal);
        }
        other => panic!("expected a comparison, got {other:?}"),
    }
    // Compared once only.
    assert!(ledger.checks(&[earlier, old_reveal]).is_empty());
    // A later reveal nothing accounts for is observed, never a mismatch.
    let stranger = Completed {
        commit_epoch: 56,
        reveal_epoch: 57,
        round: 9,
        observed_block: 8_352_100,
        missed: false,
    };
    assert_eq!(
        ledger.checks(&[earlier, old_reveal, stranger]),
        vec![Check::Unattributed(stranger)]
    );
    ledger.persist()?;
    let reloaded = RevealLedger::load(ledger_path);
    assert_eq!(reloaded.entries().len(), 1);
    assert_eq!(reloaded.entries()[0].operation_id, "new-tempo");
    Ok(())
}

/// A renewal fetched while the lease lapsed refreshes in place (one chain
/// read), and the same bundle again needs none.
#[test]
fn a_fetched_renewal_refreshes_the_loop_in_place() -> Result<()> {
    use crate::supersede::{FetchState, LoopStep};
    let root = scratch()?;
    let (first, _, _) = rv1::cohorts();
    let host = Host::default();
    publish(&host, &first);
    index(&host, &[(&first, true)]);
    point(&host, &fx::chain(), "cohort", &first.authorization, None);
    let plain = ready(run(&host, root.path(), launch_held())?);
    let active = activated(&first)?;
    let signed =
        slop_ninja_protocol::Signed::<slop_ninja_protocol::RewardIntentAuthorization>::from_bytes(
            &first.authorization,
        )?;
    let checked = slop_ninja_protocol::RewardIntentAuthorization::check(
        &signed,
        &slop_ninja_protocol::fixtures::trusted_keys(),
        &first.manifest,
        &first.package,
        &first.target,
        &active,
    )?;
    let renewal = fx::signed(slop_ninja_protocol::RewardIntentRenewal::new(
        &checked,
        2,
        plain.info.lease.1 - 1,
    )?)?
    .to_bytes()?;
    point(&host, &fx::chain(), "cohort", &first.authorization, Some(&renewal));
    let renewed = ready(run(&host, root.path(), launch_held())?);
    let lapsed = plain.info.lease_end + 1;
    let mut ports = FakeLoop {
        epoch: lapsed,
        epoch_reads: 0,
        pending: Vec::new(),
        installed: Some(Candidate::of(&plain.sha256, &plain.info, false)),
        install: FakeInstall::default(),
    };
    let mut state = FetchState::default();
    let renewed_sha = renewed.sha256.clone();
    assert_eq!(
        block_on(state.step(&mut ports, Some(Outcome::Ready(renewed))))?,
        LoopStep::Refreshed
    );
    assert_eq!(ports.epoch_reads, 1);
    assert_eq!(ports.install.written.len(), 1);
    assert!(ports.install.held.is_empty(), "a refresh keeps the held target");
    ports.installed = Some(Candidate::of(&renewed_sha, &state.latest().unwrap().info, true));
    assert_eq!(
        block_on(state.step(&mut ports, Some(Outcome::Unchanged)))?,
        LoopStep::Continue
    );
    assert_eq!(ports.epoch_reads, 1, "an installed bundle needs no chain read");
    Ok(())
}

/// The reveal ledger drops operations that never commit, keeps one entry
/// per operation, detects new outcomes by key once the watcher's ring is
/// full, expires entries the watcher passed, and sets an unreadable file
/// aside.
#[test]
fn the_reveal_ledger_tracks_only_unrevealed_commitments() -> Result<()> {
    use crate::reveals::{Check, Completed, OpState, RevealLedger};
    let root = scratch()?;
    let path = root.path().join("journal.json.reveal-rows.json");
    let mut ledger = RevealLedger::load(path.clone());
    ledger.record("a", "ha", vec![(1, 65_535)]);
    ledger.record("a", "ha", vec![(1, 65_535)]);
    ledger.record("b", "hb", vec![(2, 65_535)]);
    ledger.record("c", "hc", vec![(3, 65_535)]);
    assert_eq!(ledger.entries().len(), 3);
    ledger.settle(|e| {
        Ok(match e.operation_id.as_str() {
            "a" => OpState::Landed(100),
            "b" => OpState::Retired,
            _ => OpState::Gone,
        })
    })?;
    assert_eq!(ledger.entries().len(), 1);
    // Located by its outstanding reveal: reveal epoch 11, period 1.
    assert!(ledger.assign_epochs(&[(100, Some(11))], 1).is_empty());
    assert_eq!(ledger.entries()[0].commit_epoch, Some(10));
    // A full ring of 256 unrelated outcomes, then one more pushed in.
    let outcome = |epoch: u64| Completed {
        commit_epoch: epoch,
        reveal_epoch: epoch + 1,
        round: epoch * 3,
        observed_block: epoch * 361,
        missed: false,
    };
    let ring: Vec<Completed> = (1000..1256).map(outcome).collect();
    assert!(ledger.checks(&ring).is_empty(), "history before the run");
    let mut next: Vec<Completed> = ring[1..].to_vec();
    next.push(outcome(1256));
    assert_eq!(ledger.checks(&next), vec![Check::Unattributed(outcome(1256))]);
    // The watcher passed entry a's reveal epoch without reporting it.
    let stale = ledger.expire(1257, 1);
    assert_eq!(stale.len(), 1);
    assert!(ledger.entries().is_empty());
    ledger.persist()?;
    std::fs::write(&path, b"not json")?;
    let reloaded = RevealLedger::load(path.clone());
    assert!(reloaded.entries().is_empty());
    assert!(root.path().join("journal.json.reveal-rows.json.unreadable").is_file());
    Ok(())
}

/// A dry run that would install a fetched successor stages it in memory:
/// the bundle file and the held-target record are unchanged, and the run's
/// own verification reads the staged bundle.
#[test]
fn a_dry_run_stages_a_fetched_bundle_in_memory() -> Result<()> {
    use crate::app::AppPorts as _;
    let root = scratch()?;
    let (first, second, _) = rv1::cohorts();
    let package_path = root.path().join("package.json");
    let text = format!(
        "{}{}\n[fetch]\nbase_url = \"https://status.example/\"\n",
        base_config(root.path())
            .replace("netuid = 601", &format!("netuid = {}", rv1::RV1_NETUID))
            .replace(
                "/home/operator/.bittensor/wallets",
                &root.path().join("wallets").display().to_string()
            ),
        package_section().replace("/abs/package.json", &package_path.display().to_string()),
    );
    let config = ValidatorConfig::parse(&text)?;
    let identity = slop_ninja_subnet::ChainIdentity {
        genesis: rtao_primitives::H256(slop_ninja_protocol::TESTNET_GENESIS.0),
        netuid: rtao_primitives::NetUid(rv1::RV1_NETUID),
        hotkey: rtao_primitives::AccountId([0x66; 32]),
    };
    crate::live::init_journal(&config, identity.clone())?;
    let mut live = crate::live::LivePorts::new(config, identity).with_dry_run();
    crate::startup::StartupPorts::open_journal(&mut live)?;
    let first_bytes = rv1::bundle(&first);
    std::fs::write(&package_path, &first_bytes)?;
    let held_path = crate::live::held_target_path(&root.path().join("journal.json"));
    let held = HeldTargetFile::of(&activated(&first)?);
    std::fs::write(&held_path, serde_json::to_vec(&held)?)?;
    let host = Host::default();
    publish(&host, &first);
    publish(&host, &second);
    index(&host, &[(&first, true), (&second, true)]);
    point(&host, &fx::chain(), "cohort", &second.authorization, None);
    let staged = ready(run(&host, &root.path().join("public-cache"), activated(&first)?)?);
    let installed = {
        let authorized = crate::package_v2::verify(
            &first_bytes,
            &slop_ninja_protocol::fixtures::trusted_keys(),
            &fx::chain().genesis.0,
            rv1::RV1_NETUID,
            &activated(&first)?,
        )
        .unwrap();
        Candidate::of(&sha(&first_bytes), &fetch::BundleInfo::of(&first_bytes, &authorized)?, true)
    };
    let (decision, installed_now) =
        live.settle_staged(&staged, Some(&installed), second.closure.closed_epoch, false)?;
    assert_eq!((decision, installed_now), (Decision::Switch { retire: vec![] }, true));
    assert_eq!(std::fs::read(&package_path)?, first_bytes, "the bundle file is unchanged");
    assert_eq!(std::fs::read(&held_path)?, serde_json::to_vec(&held)?, "the held record too");
    assert!(live.has_staged_bundle());
    assert_eq!(live.read_bundle()?, staged.bundle);
    Ok(())
}
