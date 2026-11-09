//! Automatic target handoff against a fake receipts bucket holding the eval
//! Worker's signed cohort records (synthetic reward-v1 fixtures). Every pass
//! runs the Worker's own pass ([`run_pass`]) through a native [`PassIo`].

use super::*;
use crate::eval::lane_logic::block_on;
use crate::signing::{SignConfig, active_target, install_revocation, install_target, pointer_object, pointer_plan};
use crate::sql::{CHAIN_JOURNAL_SCHEMA, migrate};
use crate::testing::{Sqlite, tx};
use crate::wire::Id;
use slop_ninja_protocol::fixtures as keys_fx;
use slop_ninja_protocol::fixtures_reward_v1::{self as fx, Spec, bp};
use slop_ninja_protocol::{
    CutoffMode, Fingerprint, OutcomeClass, ReceiptIndex, ReceiptIndexEntry, RewardIntentRevocation, TESTNET_GENESIS,
    domain_hash,
};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

const NETUID: u16 = 4242;
const T0: u64 = 1_793_000_000_000;

/// The fixture authorization key, from its secret's hex form.
#[derive(Clone)]
struct FixtureKey;

impl AuthorizationKeySource for FixtureKey {
    fn authorization_key(&self, keys: &TrustedKeys) -> Result<SigningKey, &'static str> {
        authorization_key_from(&hex::encode(keys_fx::authorization_key().to_bytes()), Some(&"77".repeat(32)), keys)
    }
}

/// The fixture labels' main record (`synthetic-reward-v1-<n>`).
fn record() -> Record {
    Record::prefixes(&["synthetic-reward-v1-"])
}

/// No authorization key installed.
#[derive(Clone)]
struct NoKey;

impl AuthorizationKeySource for NoKey {
    fn authorization_key(&self, _: &TrustedKeys) -> Result<SigningKey, &'static str> {
        Err("authorization_key_missing")
    }
}

struct H {
    db: Sqlite,
    cfg: SignConfig,
    trusted: TrustedKeys,
}

impl H {
    fn new() -> Self {
        let db = Sqlite::open();
        tx(&db, |sql| migrate(sql, &CHAIN_JOURNAL_SCHEMA)).unwrap();
        let h = Self { db, cfg: SignConfig::testnet(Id(TESTNET_GENESIS.0), NETUID, [7; 32]), trusted: keys_fx::trusted_keys() };
        tx(&h.db, |sql| install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(1), T0)).unwrap().unwrap();
        h
    }

    fn handle(&self, obs: &Observed) -> Handle {
        tx(&self.db, |sql| handle(sql, &self.cfg, &self.trusted, &record(), &FixtureKey, obs, T0)).unwrap()
    }

    fn held(&self) -> ActiveTarget {
        tx(&self.db, |sql| held_target(sql, &self.cfg)).unwrap()
    }

    fn installed(&self) -> Option<String> {
        tx(&self.db, signer_target_bundle).unwrap().map(|(t, _)| t)
    }

    fn pointer_kind(&self) -> String {
        let plan = tx(&self.db, |sql| pointer_plan(sql, &self.cfg, &self.trusted)).unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(&plan.body).unwrap();
        v["kind"].as_str().unwrap().to_owned()
    }

    fn view(&self) -> AutoView {
        tx(&self.db, |sql| view(sql, T0)).unwrap()
    }

    fn logged(&self, event: &str) -> Vec<Option<String>> {
        self.db
            .conn
            .prepare("SELECT detail FROM sign_log WHERE event = ?1 ORDER BY seq")
            .unwrap()
            .query_map([event], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn revoked(&self) -> bool {
        matches!(tx(&self.db, |sql| active_target(sql, &self.cfg, &self.trusted)).unwrap().unwrap(), Err(e) if e.code == "authorization_revoked")
    }

    /// Records a preflight schedule at `epoch` (the finalized epoch index).
    fn observe_epoch(&self, epoch: u64) {
        tx(&self.db, |sql| {
            sql.exec(
                "INSERT INTO chain_schedule (id, block_number, block_hash, tempo, last_epoch_block, pending_epoch_at,
                    subnet_epoch_index, blocks_since_last_step, reveal_period, observed_at_ms)
                 VALUES (1, 1000000000, '00', 360, 0, 0, ?, 1, 1, 0)
                 ON CONFLICT(id) DO UPDATE SET subnet_epoch_index = excluded.subnet_epoch_index",
                &[bind_u64(epoch)?],
            )
        })
        .unwrap();
    }
}

fn chain() -> ChainBinding {
    ChainBinding { genesis: TESTNET_GENESIS, netuid: NETUID, mechanism: 0 }
}

fn launch_bundle_json(issued_epoch: u64) -> String {
    let auth = LaunchAuthorization::new(&chain(), 1, issued_epoch).unwrap();
    serde_json::to_string(&target::BundleV2 {
        schema: target::BUNDLE_SCHEMA_V2.into(),
        shortcut: "T12".into(),
        source: None,
        authorization: target::AuthorizationHex {
            kind: "launch".into(),
            authorization: hex::encode(sign(auth, &keys_fx::authorization_key()).unwrap().to_bytes().unwrap()),
            renewal: None,
            carrier: None,
        },
        receipt_chunks: Vec::new(),
        carrier_receipt_chunks: Vec::new(),
    })
    .unwrap()
}

/// One cohort's objects as the eval Worker writes them under its prefix.
#[derive(Clone)]
struct Cohort {
    label: String,
    closed_epoch: u64,
    target: Option<Digest>,
    objects: BTreeMap<&'static str, Vec<u8>>,
}

impl Cohort {
    /// The raw records a handoff reads (receipts aside).
    fn source_bytes(&self) -> usize {
        [object::MANIFEST, object::PACKAGE, object::TARGET, object::CLOSURE, object::PUBLISHED_CONTENTS, object::FINGERPRINT_INDEX]
            .iter()
            .filter_map(|n| self.objects.get(n))
            .map(Vec::len)
            .sum()
    }
}

/// A signed receipt index binding `package`'s manifest with its receipt
/// count of distinct entries; `package.receipt_root` is set to its digest.
fn bind_receipts(package: &mut ScorePackageV2) -> Vec<u8> {
    let mut entries: Vec<ReceiptIndexEntry> = (0..package.receipt_count)
        .map(|i| ReceiptIndexEntry {
            attempt: domain_hash("test.attempt", &i.to_le_bytes()),
            outcome: OutcomeClass::Prediction,
            object_sha256: domain_hash("test.object", &i.to_le_bytes()),
            object_len: 64,
        })
        .collect();
    entries.sort_by_key(|a| a.attempt);
    let index = ReceiptIndex { cohort_manifest: package.cohort_manifest, entries };
    package.receipt_root = index.digest().unwrap();
    sign(index, &keys_fx::receipt_key()).unwrap().to_bytes().unwrap()
}

fn cohort_of(
    manifest: CohortManifestV2,
    package: ScorePackageV2,
    target: Option<&CalculationTargetV2>,
    closure: CohortClosure,
    index: &PublishedIndex,
    receipt_index: Option<Vec<u8>>,
) -> Cohort {
    let pk = keys_fx::package_key();
    let mut objects = BTreeMap::new();
    objects.insert(object::MANIFEST, sign(manifest.clone(), &pk).unwrap().to_bytes().unwrap());
    objects.insert(object::PACKAGE, sign(package, &pk).unwrap().to_bytes().unwrap());
    if let Some(t) = target {
        objects.insert(object::TARGET, sign(t.clone(), &pk).unwrap().to_bytes().unwrap());
    }
    objects.insert(object::CLOSURE, sign(closure.clone(), &pk).unwrap().to_bytes().unwrap());
    objects.insert(object::PUBLISHED_CONTENTS, index.contents.to_bytes().unwrap());
    objects.insert(object::FINGERPRINT_INDEX, index.fingerprints.to_bytes().unwrap());
    objects.insert(object::RECEIPT_INDEX, receipt_index.unwrap_or_else(|| vec![1; 8]));
    objects.insert(object::CHUNK_MAP, b"{}".to_vec());
    Cohort {
        label: manifest.cohort_label.clone(),
        closed_epoch: closure.closed_epoch,
        target: target.map(|t| t.digest().unwrap()),
        objects,
    }
}

/// A target cohort over `predecessor`, its receipts bound by a real signed
/// receipt index.
fn target_cohort(
    manifest: CohortManifestV2,
    mut package: ScorePackageV2,
    predecessor: Digest,
    specs: &[Spec],
    index: &PublishedIndex,
) -> Cohort {
    let ri = bind_receipts(&mut package);
    let target = CalculationTargetV2::for_package(&package, Some(predecessor)).unwrap();
    let closure = fx::closure(&manifest, &package, Some(&target), specs).unwrap();
    cohort_of(manifest, package, Some(&target), closure, index, Some(ri))
}

/// A cohort whose only entrant does not beat the reference: no target.
fn no_target(cohort: u64) -> Cohort {
    let specs = [Spec::new(0x60 + cohort as u8, bp(8000))];
    let manifest = fx::manifest(fx::chain(), cohort, 1000, fx::sigma_permille(40), 0, &specs);
    let (package, _) = fx::finalized(&manifest, bp(9100), None, &specs).unwrap();
    assert_eq!(package.mode, CutoffMode::NoTarget);
    let index = fx::default_index(&manifest.references);
    let closure = fx::closure(&manifest, &package, None, &specs).unwrap();
    cohort_of(manifest, package, None, closure, &index, None)
}

/// The successor cohort `number` (a margin target) over `predecessor`, its
/// published index padded with `padding` archived artifacts.
fn successor(number: u64, predecessor: Digest, padding: u32) -> Cohort {
    let specs = [Spec::new(0x92, bp(9900))];
    let base = fx::manifest(fx::chain(), number, 1000, fx::sigma_permille(40), 2, &specs);
    let defaults = fx::default_index(&base.references);
    let index = if padding == 0 {
        defaults
    } else {
        let pad = (0..padding).map(|i| domain_hash("test.padding", &i.to_le_bytes()));
        let contents: Vec<Digest> = defaults.contents.contents.iter().copied().chain(pad.clone()).collect();
        let fingerprints: Vec<Fingerprint> = defaults
            .fingerprints
            .entries
            .iter()
            .copied()
            .chain(pad.map(|content| Fingerprint {
                content,
                version: fx::PROBE_VERSION,
                fingerprint: domain_hash("test.padding-fingerprint", &content.0),
            }))
            .collect();
        PublishedIndex::new(contents, fingerprints).unwrap()
    };
    let manifest = fx::manifest_with(
        fx::chain(),
        number,
        1000,
        fx::sigma_permille(40),
        base.references,
        &index,
        fx::probe(fx::EXAMPLE_PROBE_SIZE),
        &specs,
    );
    let mut package = fx::package(&manifest, bp(9600), Some(bp(9100)), &specs).unwrap();
    let mut measure = fx::spec_measure(&specs, manifest.batch.cases, |h, o| specs[h].family == specs[o].family);
    fx::finalize_fixture(&mut package, &index, &mut measure).unwrap();
    assert_eq!(package.mode, CutoffMode::Margin);
    target_cohort(manifest, package, predecessor, &specs, &index)
}

/// The computed cohort 3 (target T1 over the launch burn row, activating at
/// its close) and its successor, cohort 4 (T2 over T1).
fn targets() -> (Cohort, Cohort) {
    let c = fx::computed().unwrap();
    let launch = CalculationTargetV2::launch_burn(fx::chain()).digest().unwrap();
    let first = target_cohort(c.manifest.clone(), c.package.clone(), launch, &c.specs, &c.index);
    let second = successor(4, first.target.unwrap(), 0);
    (first, second)
}

/// The cohort's records as a step reads them (receipts retained).
fn observed(c: &Cohort) -> Observed {
    let get = |n: &str| c.objects.get(n).cloned();
    let receipts = match c.target {
        None => Receipts::Unread,
        Some(_) => {
            let mut s = ReceiptIndexStream::new();
            s.update(&c.objects[object::RECEIPT_INDEX]);
            Receipts::Index(s.finish(&keys_fx::trusted_keys()).unwrap())
        }
    };
    Observed {
        label: c.label.clone(),
        manifest: get(object::MANIFEST),
        package: get(object::PACKAGE),
        closure: c.objects[object::CLOSURE].clone(),
        target: get(object::TARGET),
        published_contents: get(object::PUBLISHED_CONTENTS),
        fingerprint_index: get(object::FINGERPRINT_INDEX),
        receipts,
        too_large: false,
    }
}

/// A fake receipts bucket: objects by key, listed in pages of `page`.
#[derive(Default)]
struct Bucket {
    objects: BTreeMap<String, Vec<u8>>,
    page: usize,
    lists: u32,
    reads: u32,
    /// Listing calls from this one on fail.
    fail_lists_from: Option<u32>,
    /// Keys whose operations fail.
    unavailable: BTreeSet<String>,
    /// The pass's deadline passes after this many reads.
    deadline_after_reads: Option<u32>,
}

impl Bucket {
    fn new(page: usize) -> Self {
        Self { page, ..Default::default() }
    }

    /// Writes a cohort's objects; without its closure while it is still open.
    fn put(&mut self, c: &Cohort, closed: bool) {
        for (name, bytes) in &c.objects {
            if *name == object::CLOSURE && !closed {
                continue;
            }
            self.objects.insert(key(&c.label, name), bytes.clone());
        }
    }

    fn close(&mut self, c: &Cohort) {
        self.objects.insert(key(&c.label, object::CLOSURE), c.objects[object::CLOSURE].clone());
    }

    fn retire(&mut self, c: &Cohort) {
        self.objects.insert(key(&c.label, object::RECEIPTS_RETIRED), b"{}".to_vec());
        self.objects.remove(&key(&c.label, object::RECEIPT_INDEX));
        self.objects.remove(&key(&c.label, object::CHUNK_MAP));
    }
}

struct Io<'a> {
    h: &'a H,
    b: &'a RefCell<Bucket>,
    /// Every bucket operation yields once first, so two passes polled in
    /// turn interleave at each of them as overlapping Worker requests do.
    yields: bool,
}

/// Pending once, then ready: one yield point.
struct YieldOnce(bool);

impl std::future::Future for YieldOnce {
    type Output = ();
    fn poll(mut self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        if std::mem::replace(&mut self.0, true) { std::task::Poll::Ready(()) } else { std::task::Poll::Pending }
    }
}

impl Io<'_> {
    async fn op(&self) {
        if self.yields {
            YieldOnce(false).await;
        }
    }
}

impl PassIo for Io<'_> {
    type Sql = Sqlite;

    fn tx<T: 'static>(&self, f: impl FnOnce(&Sqlite) -> Result<T, StoreError> + 'static) -> Result<T, StoreError> {
        tx(&self.h.db, f)
    }

    async fn list(&mut self, prefix: &str, cursor: Option<&str>) -> Result<(Vec<String>, Option<String>), ()> {
        self.op().await;
        let mut b = self.b.borrow_mut();
        b.lists += 1;
        if b.fail_lists_from.is_some_and(|n| b.lists >= n) {
            return Err(());
        }
        let mut prefixes: Vec<String> = b
            .objects
            .keys()
            .filter_map(|k| k.strip_prefix(prefix).and_then(|r| r.split_once('/')).map(|(l, _)| format!("{prefix}{l}/")))
            .collect();
        prefixes.dedup();
        let start: usize = cursor.map_or(0, |c| c.parse().unwrap());
        let end = (start + b.page).min(prefixes.len());
        let next = (end < prefixes.len()).then(|| end.to_string());
        Ok((prefixes[start..end].to_vec(), next))
    }

    async fn read(&mut self, key: &str, bound: usize) -> Result<Option<Vec<u8>>, ReadError> {
        self.op().await;
        // As `AutoIo`: every operation is raced against the pass's deadline.
        if self.past_deadline() {
            return Err(ReadError::Unavailable);
        }
        let mut b = self.b.borrow_mut();
        b.reads += 1;
        if b.unavailable.contains(key) {
            return Err(ReadError::Unavailable);
        }
        let Some(o) = b.objects.get(key) else {
            return Ok(None);
        };
        if o.len() > bound {
            return Err(ReadError::TooLarge);
        }
        // As `cf_io::r2_bytes`: the body is one admitted block of its length.
        let _ = slop_ninja_arena::admit_current_block(o.len(), 1);
        Ok(Some(o.to_vec()))
    }

    async fn exists(&mut self, key: &str) -> Result<bool, ()> {
        self.op().await;
        let b = self.b.borrow();
        if b.unavailable.contains(key) {
            return Err(());
        }
        Ok(b.objects.contains_key(key))
    }

    async fn fold(&mut self, key: &str, f: &mut dyn FnMut(&[u8])) -> Result<bool, ()> {
        self.op().await;
        let b = self.b.borrow();
        if b.unavailable.contains(key) {
            return Err(());
        }
        let Some(o) = b.objects.get(key) else {
            return Ok(false);
        };
        for piece in o.chunks(1000) {
            f(piece);
        }
        Ok(true)
    }

    fn now_ms(&self) -> u64 {
        T0
    }

    fn past_deadline(&self) -> bool {
        let b = self.b.borrow();
        b.deadline_after_reads.is_some_and(|n| b.reads >= n)
    }
}

/// One pass, the Worker's own (`run_pass`).
fn pass_with(h: &H, b: &mut Bucket, keys: &(impl AuthorizationKeySource + Clone + 'static)) -> PassReport {
    let cell = RefCell::new(std::mem::take(b));
    let r = block_on(run_pass(&mut Io { h, b: &cell, yields: false }, &h.cfg, &h.trusted, Ok(&record()), keys));
    *b = cell.into_inner();
    r
}

fn pass(h: &H, b: &mut Bucket) -> PassReport {
    pass_with(h, b, &FixtureKey)
}

/// One pass under another main record.
fn pass_under(h: &H, b: &mut Bucket, record: &Record) -> PassReport {
    let cell = RefCell::new(std::mem::take(b));
    let r = block_on(run_pass(&mut Io { h, b: &cell, yields: false }, &h.cfg, &h.trusted, Ok(record), &FixtureKey));
    *b = cell.into_inner();
    r
}

fn outcomes(r: &PassReport) -> Vec<(u64, Option<&str>)> {
    r.handled.iter().map(|h| (h.cohort.unwrap_or(0), h.outcome.as_deref())).collect()
}

/// A cohort with no target signs nothing: the launch burn row stays held
/// and installed, the pointer keeps naming the launch authorization, and a
/// second pass records nothing again.
#[test]
fn a_no_target_cohort_signs_nothing_and_keeps_the_held_target() {
    let h = H::new();
    let launch = h.installed().unwrap();
    let mut b = Bucket::new(1000);
    let c2 = no_target(2);
    b.put(&c2, false);
    let r = pass(&h, &mut b);
    assert!(r.handled.is_empty(), "an open cohort is not handled");
    assert_eq!(r.idle.as_deref(), Some("waiting"));
    assert_eq!(h.view().next, Some((2, c2.label.clone())));
    b.close(&c2);
    let done = pass(&h, &mut b);
    assert_eq!(outcomes(&done), vec![(2, Some("no_target"))]);
    assert!(done.handled[0].new && done.handled[0].authorization.is_none());
    assert!(h.held().is_launch_burn());
    assert_eq!(h.installed(), Some(launch));
    assert_eq!(h.pointer_kind(), "launch");
    assert!(pass(&h, &mut b).handled.is_empty(), "handled once");
    let Handle::Done(again) = h.handle(&observed(&c2)) else { panic!("recorded") };
    assert_eq!((again[0].outcome.as_deref(), again[0].new), (Some("no_target"), false));
    let v = h.view();
    assert_eq!((v.handled, v.error.as_deref(), v.next.as_ref()), (1, None, None));
    assert_eq!(h.logged("auto_target_cohort").len(), 1);
    assert_eq!((v.key_ok, v.key_error.as_deref()), (Some(true), None));
}

/// A closed cohort with a target is verified, its receipt index checked
/// against its package, authorized at its closing epoch under the trusted
/// authorization key and installed: the held target moves to it, the
/// coordinator's operation at that epoch derives from the new
/// authorization, and the pointer names it. Nothing is signed twice.
#[test]
fn a_cohort_with_a_target_is_installed_and_authorized_once_with_no_manual_step() {
    let h = H::new();
    let (t1, _) = targets();
    let mut b = Bucket::new(1000);
    b.put(&no_target(2), true);
    b.put(&t1, true);
    let done = pass(&h, &mut b);
    assert_eq!(outcomes(&done), vec![(2, Some("no_target")), (3, Some("installed"))]);
    let installed = &done.handled[1];
    assert_eq!(installed.target.as_deref(), Some(t1.target.unwrap().to_string().as_str()));
    assert_eq!(h.installed().as_deref(), installed.target.as_deref());
    assert_eq!((h.held().digest(), h.held().activated_epoch()), (t1.target.unwrap(), Some(t1.closed_epoch)));
    // The signed authorization: the cohort's own, issued when its target
    // activates, under the launch authorization's generation.
    let plan = tx(&h.db, |sql| pointer_plan(sql, &h.cfg, &h.trusted)).unwrap().unwrap();
    assert_eq!(h.pointer_kind(), "cohort");
    let bytes = tx(&h.db, |sql| pointer_object(sql, &h.cfg, &h.trusted, &plan.objects[0].sha256)).unwrap().unwrap();
    let auth = Signed::<RewardIntentAuthorization>::open(&bytes, &h.trusted).unwrap();
    assert_eq!((auth.cohort, auth.issued_epoch, auth.checkpoint_generation), (3, t1.closed_epoch, 1));
    assert_eq!(auth.renew_by_epoch, t1.closed_epoch + 40);
    assert_eq!(auth.target, t1.target.unwrap());
    assert_eq!(installed.authorization.as_deref(), Some(auth.digest().unwrap().to_string().as_str()));
    let active = tx(&h.db, |sql| active_target(sql, &h.cfg, &h.trusted)).unwrap().unwrap().ok().unwrap();
    assert!(active.op_id(&h.trusted, [7; 32], t1.closed_epoch).is_ok());
    assert!(active.op_id(&h.trusted, [7; 32], t1.closed_epoch - 1).is_err(), "never before the target activates");
    // Duplicate wakeups: nothing new.
    assert!(pass(&h, &mut b).handled.is_empty());
    let Handle::Done(again) = h.handle(&observed(&t1)) else { panic!("recorded") };
    assert_eq!((again[0].outcome.as_deref(), again[0].new), (Some("installed"), false));
    assert_eq!(h.logged("target_installed").len(), 2, "the launch bundle, then the target once");
    // The bundle the coordinator stored is what serde writes for it and
    // fits one value with its framing.
    let (_, stored) = tx(&h.db, signer_target_bundle).unwrap().unwrap();
    let parsed = target::parse_v2(stored.as_bytes()).unwrap();
    assert_eq!(serde_json::to_string(&parsed).unwrap(), stored);
    assert!(stored.len() - 2 * t1.source_bytes() <= BUNDLE_FRAMING_BYTES, "framing {} B", stored.len() - 2 * t1.source_bytes());
}

/// Duplicate and reordered wakeups: a later cohort handed in before an
/// earlier closed one is refused (its predecessor is not held) and the
/// earlier one is not skipped; each cohort is then installed exactly once,
/// in order, and a stale wakeup for an earlier cohort changes nothing.
#[test]
fn duplicate_and_reordered_wakeups_never_skip_or_sign_twice() {
    let h = H::new();
    let (t1, t2) = targets();
    let mut b = Bucket::new(1);
    b.put(&t1, false);
    b.put(&t2, false);
    // Discover both (one label per listing page) without handling.
    assert!(pass(&h, &mut b).handled.is_empty());
    assert_eq!(b.lists, 2, "two pages");
    assert_eq!(h.view().waiting, 2);
    let outcome = |r: Handle| match r {
        Handle::Done(v) => (v[0].outcome.clone(), None, v[0].new),
        Handle::Failed(f) => (None, f.error, false),
        Handle::Defer(..) => panic!("deferred"),
    };
    // Reordered: cohort 4 first is refused and leaves nothing behind.
    assert_eq!(outcome(h.handle(&observed(&t2))), (None, Some("predecessor_not_held".into()), false));
    assert!(h.held().is_launch_burn());
    assert_eq!(h.view().error.as_deref(), Some("predecessor_not_held"));
    // In order, each once.
    assert_eq!(outcome(h.handle(&observed(&t1))), (Some("installed".into()), None, true));
    assert_eq!(outcome(h.handle(&observed(&t1))), (Some("installed".into()), None, false));
    assert_eq!(outcome(h.handle(&observed(&t2))), (Some("installed".into()), None, true));
    assert_eq!(outcome(h.handle(&observed(&t2))), (Some("installed".into()), None, false));
    // A stale wakeup for cohort 3 after 4: the recorded outcome.
    assert_eq!(outcome(h.handle(&observed(&t1))), (Some("installed".into()), None, false));
    assert_eq!((h.held().digest(), h.held().activated_epoch()), (t2.target.unwrap(), Some(t2.closed_epoch)));
    assert_eq!(h.logged("target_installed").len(), 3, "launch, T1, T2");
    assert_eq!(h.view().error, None, "the refused cohort's later success clears its failure");
    b.close(&t1);
    b.close(&t2);
    assert!(pass(&h, &mut b).handled.is_empty());
}

/// An eviction or failure after the authorization is signed but before the
/// transaction commits leaves nothing: the next pass signs the same bytes
/// (deterministic signature over the same terms) and installs once.
#[test]
fn a_restart_mid_sign_leaves_nothing_and_signs_the_same_bytes_again() {
    let h = H::new();
    let (t1, _) = targets();
    let mut b = Bucket::new(1000);
    b.put(&t1, true);
    // The handoff fails at its last write, after the bundle was installed in
    // the same transaction: the whole transaction rolls back.
    h.db.fail_on.set(Some("INSERT INTO sign_auto_cohorts"));
    let r = pass(&h, &mut b);
    h.db.fail_on.set(None);
    assert_eq!(r.error.as_deref(), Some("store_unavailable"), "the store failure ends the pass");
    assert!(h.held().is_launch_burn(), "nothing committed");
    assert_eq!(h.pointer_kind(), "launch");
    assert_eq!(h.view().handled, 0);
    let first_attempt_auth = {
        // The terms the failed attempt signed, re-signed outside any store.
        let o = observed(&t1);
        let m = Signed::<CohortManifestV2>::open(o.manifest.as_deref().unwrap(), &h.trusted).unwrap();
        let p = Signed::<ScorePackageV2>::open(o.package.as_deref().unwrap(), &h.trusted).unwrap();
        let a = RewardIntentAuthorization {
            chain: chain(),
            cohort: 3,
            cohort_manifest: m.digest().unwrap(),
            score_package: p.digest().unwrap(),
            target: t1.target.unwrap(),
            checkpoint_generation: 1,
            issued_epoch: t1.closed_epoch,
            renew_by_epoch: t1.closed_epoch + 40,
        };
        sign(a, &keys_fx::authorization_key()).unwrap().to_bytes().unwrap()
    };
    let done = pass(&h, &mut b);
    assert_eq!(outcomes(&done), vec![(3, Some("installed"))]);
    let plan = tx(&h.db, |sql| pointer_plan(sql, &h.cfg, &h.trusted)).unwrap().unwrap();
    let bytes = tx(&h.db, |sql| pointer_object(sql, &h.cfg, &h.trusted, &plan.objects[0].sha256)).unwrap().unwrap();
    assert_eq!(bytes, first_attempt_auth, "the same signed bytes");
    assert_eq!(h.logged("target_installed").len(), 2);
}

fn revoke(h: &H, c: &Cohort, authorization: Digest, issued_epoch: u64) {
    let revocation = RewardIntentRevocation {
        chain: chain(),
        target: c.target.unwrap(),
        authorization,
        checkpoint_generation: 1,
        issued_epoch,
    };
    let hex_rev = hex::encode(sign(revocation, &keys_fx::authorization_key()).unwrap().to_bytes().unwrap());
    tx(&h.db, |sql| install_revocation(sql, &h.cfg, &h.trusted, &hex_rev, None, T0)).unwrap().unwrap();
}

/// A revocation after automatic signing: the operator's signed revocation of
/// the automatically signed authorization stops the target at once; later
/// passes never re-authorize it (duplicate wakeups answer the recorded
/// outcome, a no-target cohort leaves it withdrawn), and the next cohort's
/// target installs as its successor and commits.
#[test]
fn a_revocation_after_automatic_signing_stops_the_target_and_is_never_reauthorized() {
    let h = H::new();
    let (t1, t2) = targets();
    let mut b = Bucket::new(1000);
    b.put(&t1, true);
    let done = pass(&h, &mut b);
    let auth = Digest(hex::decode(done.handled[0].authorization.as_deref().unwrap()).unwrap().try_into().unwrap());
    h.observe_epoch(t1.closed_epoch + 2);
    revoke(&h, &t1, auth, t1.closed_epoch + 2);
    assert!(h.revoked());
    // Duplicate wakeups and a full pass: nothing is signed again.
    assert!(pass(&h, &mut b).handled.is_empty());
    let Handle::Done(again) = h.handle(&observed(&t1)) else { panic!("recorded") };
    assert!(!again[0].new);
    assert!(h.revoked(), "never re-authorized automatically");
    assert_eq!(h.logged("target_installed").len(), 2);
    // The successor installs and commits (its label is found at the next
    // epoch's listing).
    b.put(&t2, true);
    h.observe_epoch(t1.closed_epoch + 3);
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(4, Some("installed"))]);
    assert!(!h.revoked());
    let active = tx(&h.db, |sql| active_target(sql, &h.cfg, &h.trusted)).unwrap().unwrap().ok().unwrap();
    assert!(active.op_id(&h.trusted, [7; 32], t2.closed_epoch).is_ok());
    // A later no-target cohort changes nothing.
    b.put(&no_target(5), true);
    h.observe_epoch(t1.closed_epoch + 4);
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(5, Some("no_target"))]);
    assert_eq!(h.held().digest(), t2.target.unwrap());
}

/// The authorization the handoff will sign for the computed cohort 3.
fn t1_terms(h: &H, t1: &Cohort) -> RewardIntentAuthorization {
    let o = observed(t1);
    RewardIntentAuthorization {
        chain: chain(),
        cohort: 3,
        cohort_manifest: Signed::<CohortManifestV2>::open(o.manifest.as_deref().unwrap(), &h.trusted).unwrap().digest().unwrap(),
        score_package: Signed::<ScorePackageV2>::open(o.package.as_deref().unwrap(), &h.trusted).unwrap().digest().unwrap(),
        target: t1.target.unwrap(),
        checkpoint_generation: 1,
        issued_epoch: t1.closed_epoch,
        renew_by_epoch: t1.closed_epoch + 40,
    }
}

/// A revocation journaled before the handoff: the target is installed (its
/// closure activated it) as revoked, so nothing commits for it, exactly as
/// after a revocation that follows the install; the coordinator never signs
/// a re-authorization, and the next cohort's target installs as its
/// successor with no operator step.
#[test]
fn an_authorization_revoked_before_its_handoff_is_installed_as_revoked_and_its_successor_follows() {
    let h = H::new();
    let (t1, t2) = targets();
    h.observe_epoch(t1.closed_epoch);
    revoke(&h, &t1, t1_terms(&h, &t1).digest().unwrap(), t1.closed_epoch);
    let mut b = Bucket::new(1000);
    b.put(&t1, true);
    let r = pass(&h, &mut b);
    assert_eq!(outcomes(&r), vec![(3, Some("revoked"))]);
    assert_eq!((h.held().digest(), h.held().activated_epoch()), (t1.target.unwrap(), Some(t1.closed_epoch)));
    assert!(h.revoked(), "nothing commits for the revoked target");
    assert_eq!(h.logged("target_installed_revoked").len(), 1);
    assert_eq!(h.view().error, None);
    // The successor, found at the next epoch's listing.
    b.put(&t2, true);
    h.observe_epoch(t1.closed_epoch + 1);
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(4, Some("installed"))]);
    assert!(!h.revoked());
    assert_eq!(h.held().digest(), t2.target.unwrap());
    assert_eq!(h.pointer_kind(), "cohort");
    assert_eq!(h.view().error, None);
}

/// Without the authorization key (or with a wrong one) every pass reports it
/// (`key_ok`) without holding cohorts that need no signature; a target
/// cohort waits, visibly, and is handled at the first pass once the key is
/// installed; pausing holds the pass and a resume continues from the same
/// cohort.
#[test]
fn a_missing_key_is_reported_at_once_and_a_target_cohort_or_an_operator_hold_waits_on_the_same_cohort() {
    let h = H::new();
    let (t1, _) = targets();
    let mut b = Bucket::new(1000);
    b.put(&no_target(2), true);
    let r = pass_with(&h, &mut b, &NoKey);
    assert_eq!(outcomes(&r), vec![(2, Some("no_target"))], "a cohort with no target needs no key");
    assert_eq!(r.key_error.as_deref(), Some("authorization_key_missing"));
    let v = h.view();
    assert_eq!((v.key_ok, v.key_error.as_deref(), v.error.as_deref()), (Some(false), Some("authorization_key_missing"), None));
    b.put(&t1, true);
    h.observe_epoch(1);
    let r = pass_with(&h, &mut b, &NoKey);
    assert_eq!((r.handled.len(), r.handled[0].error.as_deref()), (1, Some("authorization_key_missing")));
    let v = h.view();
    assert_eq!((v.error.as_deref(), v.error_label.as_deref(), v.next.map(|n| n.0)), (Some("authorization_key_missing"), Some(t1.label.as_str()), Some(3)));
    assert_eq!(h.logged("auto_target_failed").len(), 1);
    pass_with(&h, &mut b, &NoKey);
    assert_eq!(h.logged("auto_target_failed").len(), 1, "one log line per distinct failure");
    assert_eq!(h.logged("auto_target_key_refused").len(), 1, "the key check logs changes only");
    assert!(tx(&h.db, |sql| set_paused(sql, true, T0)).unwrap());
    assert!(pass(&h, &mut b).handled.is_empty(), "held");
    assert!(h.view().paused);
    assert!(h.held().is_launch_burn());
    assert!(tx(&h.db, |sql| set_paused(sql, false, T0)).unwrap());
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(3, Some("installed"))]);
    let v = h.view();
    assert_eq!((v.error, v.key_ok), (None, Some(true)));
    assert_eq!(h.logged("auto_target_recovered").len(), 1);
}

/// A target cohort whose detailed receipts are already retired is never
/// installed or authorized on its own: it is journaled as pending and waits
/// (`receipts_retired`) with the held target unchanged; a later closed
/// target cohort whose install is refused installs nothing; once the later
/// one can be installed, both are recorded in one transaction, the retired
/// one as an intermediate, and the held target moves straight to the later
/// one.
#[test]
fn a_target_cohort_with_retired_receipts_is_installed_only_with_its_successor_in_one_transaction() {
    let h = H::new();
    let (t1, t2) = targets();
    let mut b = Bucket::new(1000);
    b.put(&t1, true);
    b.retire(&t1);
    let r = pass(&h, &mut b);
    assert!(r.handled.is_empty());
    assert_eq!(r.deferred, 1);
    let v = h.view();
    assert_eq!((v.error.as_deref(), v.error_label.as_deref()), (Some("receipts_retired"), Some(t1.label.as_str())));
    assert_eq!((v.pending, v.pending_first.as_ref().map(|p| p.0), v.handled, v.next), (1, Some(3), 0, None));
    assert!(h.held().is_launch_burn());
    assert_eq!(h.pointer_kind(), "launch");
    // The successor closes but its install is refused (no key): nothing.
    b.put(&t2, true);
    h.observe_epoch(1);
    let r = pass_with(&h, &mut b, &NoKey);
    assert_eq!(r.handled.iter().map(|x| x.error.as_deref()).collect::<Vec<_>>(), vec![Some("authorization_key_missing")]);
    assert!(h.held().is_launch_burn(), "the intermediate is not installed alone");
    assert_eq!(h.installed().as_deref(), Some(CalculationTargetV2::launch_burn(fx::chain()).digest().unwrap().to_string().as_str()));
    assert_eq!((h.view().handled, h.view().pending), (0, 1));
    // Then both, in one transaction, without reading cohort 3 again.
    let reads = b.reads;
    let r = pass(&h, &mut b);
    assert_eq!(b.reads - reads, 6, "the successor's closure and records only");
    assert_eq!(outcomes(&r), vec![(3, Some("intermediate")), (4, Some("installed"))]);
    assert!(r.handled[0].authorization.is_none(), "no authorization is signed for the intermediate");
    assert_eq!((h.held().digest(), h.held().activated_epoch()), (t2.target.unwrap(), Some(t2.closed_epoch)));
    assert_eq!(h.logged("target_installed").len(), 2, "the launch bundle, then T2: T1 never installed");
    assert_eq!(h.logged("auto_target_via"), vec![Some("4 after 3".into())]);
    let v = h.view();
    assert_eq!((v.error, v.pending, v.handled), (None, 0, 2));
}

/// After an outage longer than several cohorts the chain of intermediates
/// is longer than one pass's deadline: each pass journals what it verified
/// and the next resumes after it, so the chain completes over several
/// passes and installs once, and a pass that gets nowhere says so.
#[test]
fn a_chain_longer_than_one_pass_completes_over_several_passes() {
    let h = H::new();
    let (t1, t2) = targets();
    let t3 = successor(5, t2.target.unwrap(), 0);
    let mut b = Bucket::new(1000);
    // Found and resolved while still open; they close during the outage.
    for c in [&t1, &t2, &t3] {
        b.put(c, false);
    }
    assert!(pass(&h, &mut b).handled.is_empty());
    for c in [&t1, &t2, &t3] {
        b.close(c);
    }
    b.retire(&t1);
    b.retire(&t2);
    // Each pass's deadline holds one cohort's reads (its closure and five
    // records); the reviewer's case of five passes in a row of `budget`
    // with nothing journaled no longer arises.
    let budgeted = |b: &mut Bucket| {
        b.reads = 0;
        b.deadline_after_reads = Some(6);
    };
    let mut seen = Vec::new();
    for _ in 0..3 {
        budgeted(&mut b);
        seen.push(pass(&h, &mut b));
        assert_ne!(h.view().error.as_deref(), Some("pass_budget"), "progress every pass");
    }
    assert_eq!(seen[0].deferred, 1, "cohort 3 journaled");
    assert_eq!(seen[1].deferred, 1, "cohort 4 journaled, 3 not read again");
    assert_eq!(outcomes(&seen[2]), vec![(3, Some("intermediate")), (4, Some("intermediate")), (5, Some("installed"))]);
    assert_eq!((h.held().digest(), h.held().activated_epoch()), (t3.target.unwrap(), Some(t3.closed_epoch)));
    assert_eq!(h.logged("target_installed").len(), 2, "the launch bundle, then T3 once");
    assert_eq!(h.logged("auto_target_via"), vec![Some("5 after 3,4".into())]);
    // A deadline too short for even one cohort: nothing journaled, and the
    // stall is on `/status` (the alert reads it), not silent.
    let h = H::new();
    let mut b = Bucket::new(1000);
    b.put(&t1, true);
    pass_with(&h, &mut b, &NoKey);
    b.reads = 0;
    b.deadline_after_reads = Some(3);
    let r = pass(&h, &mut b);
    assert_eq!((r.idle.as_deref(), r.handled.len(), r.deferred), (Some("budget"), 0, 0));
    let v = h.view();
    assert_eq!((v.error.as_deref(), v.error_label.as_deref()), (Some("pass_budget"), Some(t1.label.as_str())));
    b.deadline_after_reads = None;
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(3, Some("installed"))]);
    assert_eq!(h.view().error, None);
}

/// An install by another path (the operator's bundle) moves the held target
/// the pending cohorts were checked from: they are dropped and checked again
/// from the new one. A journaled chain that no longer starts at the held
/// target (a move this module did not see) is dropped by the installing
/// transaction instead of being installed against the old target.
#[test]
fn pending_cohorts_are_dropped_when_another_install_moves_the_held_target() {
    let (t1, t2) = targets();
    // The operator installs cohort 3's own bundle while it is pending.
    let h = H::new();
    let mut b = Bucket::new(1000);
    b.put(&t1, true);
    b.retire(&t1);
    pass(&h, &mut b);
    assert_eq!(h.view().pending, 1);
    let auth = sign(t1_terms(&h, &t1), &keys_fx::authorization_key()).unwrap().to_bytes().unwrap();
    let json = bundle_json(&observed(&t1), &auth);
    let held = h.held();
    tx(&h.db, move |sql| install_target_v2_from(sql, &crate::signing::SignConfig::testnet(Id(TESTNET_GENESIS.0), NETUID, [7; 32]), &keys_fx::trusted_keys(), &json, T0, &held, false))
        .unwrap()
        .unwrap();
    assert_eq!((h.view().pending, h.held().digest()), (0, t1.target.unwrap()));
    b.put(&t2, true);
    h.observe_epoch(1);
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(3, Some("held")), (4, Some("installed"))]);
    // A journaled chain whose start is no longer held.
    let h = H::new();
    let mut b = Bucket::new(1000);
    b.put(&t1, true);
    b.retire(&t1);
    pass(&h, &mut b);
    let other = hex::encode(t2.target.unwrap().0);
    tx(&h.db, move |sql| sql.exec("UPDATE sign_auto_pending SET from_target = ?, from_activated = 1", &[other.into()]).map(|_| ()))
        .unwrap();
    b.put(&t2, true);
    h.observe_epoch(1);
    let r = pass(&h, &mut b);
    assert_eq!(r.handled.iter().map(|x| x.error.as_deref()).collect::<Vec<_>>(), vec![Some("held_target_changed")]);
    assert!(h.held().is_launch_burn());
    assert_eq!(h.view().pending, 0, "dropped");
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(3, Some("intermediate")), (4, Some("installed"))]);
    assert_eq!(h.view().error, None);
}

/// The receipts a target is installed with are the ones its package binds:
/// an index that does not authenticate, or binds another root, refuses the
/// handoff; a missing chunk map with no retirement marker keeps the cohort
/// waiting like retired receipts but names the defect (`receipts_missing`).
#[test]
fn the_receipt_index_is_checked_against_the_package_before_any_install() {
    let (t1, _) = targets();
    for (change, code) in [("tamper", "receipt_index_unverified"), ("other", "receipt_index_mismatch"), ("map", "receipts_missing")] {
        let h = H::new();
        let mut b = Bucket::new(1000);
        b.put(&t1, true);
        let k = key(&t1.label, object::RECEIPT_INDEX);
        match change {
            "tamper" => *b.objects.get_mut(&k).unwrap().last_mut().unwrap() ^= 1,
            "other" => {
                let mut p = Signed::<ScorePackageV2>::open(&t1.objects[object::PACKAGE], &h.trusted).unwrap();
                p.receipt_count += 1;
                b.objects.insert(k, bind_receipts(&mut p));
            }
            _ => {
                b.objects.remove(&key(&t1.label, object::CHUNK_MAP));
            }
        }
        pass(&h, &mut b);
        assert_eq!(h.view().error.as_deref(), Some(code), "{change}");
        assert!(h.held().is_launch_burn(), "{change}");
    }
}

/// An installed bundle whose authorization does not read refuses the
/// handoff instead of signing under a guessed generation.
#[test]
fn an_unreadable_installed_bundle_refuses_instead_of_guessing_the_generation() {
    let h = H::new();
    let (t1, _) = targets();
    tx(&h.db, |sql| {
        let (_, bundle) = signer_target_bundle(sql)?.unwrap();
        let mut b = target::parse_v2(bundle.as_bytes()).unwrap();
        b.authorization.authorization = "00".into();
        sql.exec("UPDATE sign_targets SET bundle = ? WHERE active = 1", &[serde_json::to_string(&b).unwrap().into()])
    })
    .unwrap();
    let mut b = Bucket::new(1000);
    b.put(&t1, true);
    let r = pass(&h, &mut b);
    assert_eq!(r.handled[0].error.as_deref(), Some("installed_generation_unreadable"));
    assert!(h.held().is_launch_burn());
}

/// Labels: only plain cohort prefixes are read; a manifest under other keys
/// (a load cohort) is foreign for good; one not yet sealed is read again at
/// the next listing; a later cohort is never handed in past an earlier
/// sealed one that has not closed (the stall is shown once a later one has
/// closed), until the operator records the earlier one as abandoned;
/// cohorts before the restarted record are ignored.
#[test]
fn labels_resolve_by_manifest_and_an_unclosed_earlier_cohort_is_passed_only_on_the_operators_record() {
    assert_eq!(label_of("cohorts/testnet-r0-18/"), Some("testnet-r0-18"));
    for bad in ["cohorts//", "cohorts/a/b/", "cohorts/../", "other/x/", "cohorts/x", "cohorts/sp ace/"] {
        assert_eq!(label_of(bad), None, "{bad}");
    }
    let h = H::new();
    let mut b = Bucket::new(2);
    // A load cohort: its manifest signed under another package key.
    let foreign = {
        let mut c = no_target(6);
        let other = SigningKey::from_bytes(&[0x42; 32]);
        let mut m = Signed::<CohortManifestV2>::from_bytes(&c.objects[object::MANIFEST]).unwrap().record;
        m.cohort_label = "load-x".into();
        c.objects.insert(object::MANIFEST, sign(m, &other).unwrap().to_bytes().unwrap());
        c.label = "load-x".into();
        c
    };
    for (name, bytes) in &foreign.objects {
        b.objects.insert(key("load-x", name), bytes.clone());
    }
    b.objects.insert("cohorts/unsealed-1/published-contents.bin".into(), vec![0]);
    let c2 = no_target(2);
    b.put(&c2, false); // never closes (abandoned)
    let c5 = no_target(5);
    b.put(&c5, true);
    let r = pass(&h, &mut b);
    assert!(r.handled.is_empty(), "never past the unclosed cohort 2");
    let v = h.view();
    assert_eq!((v.error.as_deref(), v.error_label.as_deref()), (Some("earlier_cohort_unclosed"), Some(c2.label.as_str())));
    assert_eq!((v.next, v.waiting), (Some((2, c2.label.clone())), 2));
    let counts: BTreeMap<String, u64> = v.labels.into_iter().collect();
    assert_eq!((counts.get("foreign"), counts.get("unsealed"), counts.get("in_record")), (Some(&1), Some(&1), Some(&2)));
    // The operator's record of the abandonment is the evidence.
    assert_eq!(tx(&h.db, |sql| mark_abandoned(sql, "unknown", T0)).unwrap(), Err("label_unknown"));
    assert_eq!(tx(&h.db, |sql| mark_abandoned(sql, &c2.label, T0)).unwrap(), Ok(2));
    assert_eq!(h.view().error, None);
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(5, Some("no_target"))]);
    let counts: BTreeMap<String, u64> = h.view().labels.into_iter().collect();
    assert_eq!((counts.get("passed"), counts.get("handled")), (Some(&1), Some(&1)));
    assert_eq!(tx(&h.db, |sql| mark_abandoned(sql, &c5.label, T0)).unwrap(), Err("label_not_in_record"));
    // A later listing (a new epoch) reads the unsealed label again.
    h.observe_epoch(7);
    let lists = b.lists;
    pass(&h, &mut b);
    assert!(b.lists > lists);
    assert_eq!(h.view().listed_epoch, Some(7));
    // Same epoch: no listing.
    let lists = b.lists;
    pass(&h, &mut b);
    assert_eq!(b.lists, lists, "one listing per finalized epoch");
}

/// A transient failure clears itself when its step next succeeds: a failed
/// listing at the next listing, an unreadable object at the next read; a
/// failed listing never holds a cohort already known to be closed, and a
/// failed continuation restarts the listing from its first page.
#[test]
fn transient_failures_clear_themselves_and_a_failed_listing_holds_no_known_cohort() {
    let h = H::new();
    let mut b = Bucket::new(1);
    let (c2, c3) = (no_target(2), no_target(3));
    b.put(&c2, false);
    b.put(&c3, false);
    pass(&h, &mut b);
    assert_eq!(h.view().waiting, 2);
    // A new epoch is due a listing; the listing fails, the closed cohort 2 is
    // still handed in.
    b.close(&c2);
    h.observe_epoch(9);
    b.fail_lists_from = Some(b.lists + 1);
    let r = pass(&h, &mut b);
    assert_eq!(outcomes(&r), vec![(2, Some("no_target"))]);
    assert_eq!(r.error.as_deref(), Some("list_unavailable"));
    assert_eq!(h.view().error.as_deref(), Some("list_unavailable"));
    // The listing works again: the failure clears.
    b.fail_lists_from = None;
    pass(&h, &mut b);
    let v = h.view();
    assert_eq!((v.error.as_deref(), v.listed_epoch), (None, Some(9)));
    // A failed continuation (the second page) is dropped.
    h.observe_epoch(10);
    b.fail_lists_from = Some(b.lists + 2);
    pass(&h, &mut b);
    assert!(!h.view().listing, "the continuation is dropped");
    b.fail_lists_from = None;
    let before = b.lists;
    pass(&h, &mut b);
    assert_eq!((b.lists - before, h.view().listed_epoch), (2, Some(10)), "listed again from the first page");
    assert_eq!(h.view().error, None);
    // An unreadable closure, then a readable one.
    let ck = key(&c3.label, object::CLOSURE);
    b.unavailable.insert(ck.clone());
    let r = pass(&h, &mut b);
    assert_eq!(r.error.as_deref(), Some("object_unavailable"));
    assert_eq!((h.view().error.as_deref(), h.view().error_label.as_deref()), (Some("object_unavailable"), Some(c3.label.as_str())));
    b.unavailable.remove(&ck);
    pass(&h, &mut b);
    assert_eq!(h.view().error, None, "cleared by the next read of the same label, still open");
    // A note of the pass itself, cleared by the next pass whose steps succeed.
    tx(&h.db, |sql| note_failure(sql, "", "bucket_binding", T0)).unwrap();
    pass(&h, &mut b);
    assert_eq!(h.view().error, None);
}

/// The authorization key must derive the trusted authorization root and
/// must not be the hotkey's seed.
#[test]
fn the_authorization_key_is_the_trusted_root_and_not_the_hotkey() {
    let keys = keys_fx::trusted_keys();
    let seed = hex::encode(keys_fx::authorization_key().to_bytes());
    assert!(authorization_key_from(&seed, None, &keys).is_ok());
    assert!(authorization_key_from(&format!("0x{seed}\n"), Some(&"11".repeat(32)), &keys).is_ok());
    assert_eq!(authorization_key_from(&seed, Some(&format!("0x{seed}")), &keys).err(), Some("authorization_key_is_hotkey"));
    let package = hex::encode(keys_fx::package_key().to_bytes());
    assert_eq!(authorization_key_from(&package, None, &keys).err(), Some("authorization_key_mismatch"));
    assert_eq!(authorization_key_from("zz", None, &keys).err(), Some("authorization_key_malformed"));
}

/// The largest padding of the successor's published index whose records fit
/// [`SOURCE_BYTES`] (each archived artifact adds a content digest and a
/// fingerprint).
fn padding_at_the_bound() -> u32 {
    let base = successor(4, Digest([0; 32]), 0).source_bytes();
    let per = successor(4, Digest([0; 32]), 1).source_bytes() - base;
    ((SOURCE_BYTES - base) / per) as u32
}

/// A cohort at the source bound is handed in natively (the witness below
/// runs the same pass inside the route's footprint).
#[test]
fn a_cohort_at_the_source_bound_is_handed_in() {
    let h = H::new();
    let (t1, _) = targets();
    let mut b = Bucket::new(1000);
    b.put(&t1, true);
    pass(&h, &mut b);
    let big = successor(4, t1.target.unwrap(), padding_at_the_bound());
    assert!(big.source_bytes() <= SOURCE_BYTES && big.source_bytes() + 200 > SOURCE_BYTES, "{} B", big.source_bytes());
    b.put(&big, true);
    h.observe_epoch(1);
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(4, Some("installed"))]);
    let (_, stored) = tx(&h.db, signer_target_bundle).unwrap().unwrap();
    assert!(stored.len() <= target::MAX_BUNDLE_V2_BYTES, "{} B", stored.len());
}

/// Arena witness (`--features arena-witness`): the automatic-handoff pass
/// that installs a cohort at the source bound (its records read as exact
/// blocks, verified, the bundle written, parsed, verified and stored, its
/// receipt index folded) fits the `/sign-auto` route's derived footprint.
#[cfg(feature = "arena-witness")]
#[test]
fn arena_witness_the_automatic_handoff_fits_at_its_source_bound() {
    use crate::arena_routes::{JOURNAL_AUTO_BYTES, R_JOURNAL_AUTO};
    let (t1, _) = targets();
    // A small cohort first: its peak is the footprint's base.
    {
        let h = H::new();
        let mut b = Bucket::new(1000);
        b.put(&t1, true);
        tx(&h.db, |sql| note_page(sql, &[format!("{COHORT_PREFIX}{}/", t1.label)], None, T0)).unwrap();
        let _ = h.view();
        eprintln!("source {} B", t1.source_bytes());
        let installed = crate::arena_witness::witness(R_JOURNAL_AUTO.route(), JOURNAL_AUTO_BYTES, || {
            let r = pass(&h, &mut b);
            r.handled.len() == 1 && r.handled[0].outcome.as_deref() == Some("installed")
        });
        assert!(installed);
    }
    let h = H::new();
    let mut b = Bucket::new(1000);
    b.put(&t1, true);
    pass(&h, &mut b);
    let big = successor(4, t1.target.unwrap(), padding_at_the_bound());
    b.put(&big, true);
    h.observe_epoch(1);
    // Warm the database outside the witnessed scope, as a live journal is.
    let _ = h.view();
    eprintln!("source {} B", big.source_bytes());
    let installed = crate::arena_witness::witness(R_JOURNAL_AUTO.route(), JOURNAL_AUTO_BYTES, || {
        let r = pass(&h, &mut b);
        r.handled.len() == 1 && r.handled[0].outcome.as_deref() == Some("installed")
    });
    assert!(installed);
    assert_eq!(h.held().digest(), big.target.unwrap());
}

/// `c` under another label, its manifest signed again under the trusted
/// package key (a scratch authority's cohort in the same bucket).
fn relabelled(c: &Cohort, label: &str) -> Cohort {
    let mut m = Signed::<CohortManifestV2>::from_bytes(&c.objects[object::MANIFEST]).unwrap().record;
    m.cohort_label = label.into();
    let mut out = c.clone();
    out.objects.insert(object::MANIFEST, sign(m, &keys_fx::package_key()).unwrap().to_bytes().unwrap());
    out.label = label.into();
    out
}

fn counts(h: &H) -> BTreeMap<String, u64> {
    h.view().labels.into_iter().collect()
}

/// Only the main record's labels are handed in: a scratch authority's
/// cohorts, signed with the same package key into the same bucket, one
/// numbered far above the main sequence and one with a main cohort's
/// number, are `outside` and never move the numbering or stall it.
#[test]
fn a_scratch_authority_in_the_same_bucket_never_reaches_the_main_record() {
    let h = H::new();
    let mut b = Bucket::new(1000);
    b.put(&relabelled(&no_target(90), "scratch-drill-90"), true);
    b.put(&relabelled(&no_target(3), "scratch-drill-3"), true);
    b.put(&no_target(2), true);
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(2, Some("no_target"))]);
    b.put(&no_target(3), true);
    h.observe_epoch(1);
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(3, Some("no_target"))]);
    let v = h.view();
    assert_eq!((v.error, v.unclassified.clone()), (None, vec![("outside:authority".to_owned(), 2)]));
    assert_eq!(counts(&h).get("handled"), Some(&2));
}

/// Cohort numbers continue: a cohort after a number with no in-record
/// label waits (`cohort_gap`) instead of being handed in past it, until the
/// operator records the gap; a label that later resolves to that number is
/// reported, never silently behind the record.
#[test]
fn cohort_numbers_continue_without_gaps_unless_the_operator_records_one() {
    let h = H::new();
    let mut b = Bucket::new(1000);
    b.put(&no_target(2), true);
    pass(&h, &mut b);
    let c4 = no_target(4);
    b.put(&c4, true);
    h.observe_epoch(1);
    let r = pass(&h, &mut b);
    assert_eq!(r.handled.iter().map(|x| (x.cohort, x.error.as_deref())).collect::<Vec<_>>(), vec![(Some(4), Some("cohort_gap"))]);
    let v = h.view();
    assert_eq!((v.error.as_deref(), v.error_label.as_deref(), v.next.map(|n| n.0)), (Some("cohort_gap"), Some(c4.label.as_str()), Some(4)));
    assert_eq!(tx(&h.db, |sql| mark_gap(sql, 2, T0)).unwrap(), Err("cohort_already_handled"));
    assert_eq!(tx(&h.db, |sql| mark_gap(sql, 4, T0)).unwrap(), Err("cohort_has_a_label"));
    assert_eq!(tx(&h.db, |sql| mark_gap(sql, 3, T0)).unwrap(), Ok(3));
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(4, Some("no_target"))]);
    assert_eq!((h.view().error, h.view().gaps), (None, 1));
    let c3 = no_target(3);
    b.put(&c3, true);
    h.observe_epoch(2);
    pass(&h, &mut b);
    let v = h.view();
    assert_eq!((v.error.as_deref(), v.error_label.as_deref()), (Some("cohort_marked_absent"), Some(c3.label.as_str())));
}

/// A manifest the trusted package key signed that this build cannot decode
/// (a newer schema) is not foreign: it is reported, read again at every
/// listing, and the cohorts after it wait (`cohort_gap`) instead of being
/// handed in past it; once it decodes, both are handed in, in order.
#[test]
fn an_undecodable_manifest_signed_by_the_package_key_is_retried_and_never_skipped() {
    let h = H::new();
    let mut b = Bucket::new(1000);
    b.put(&no_target(2), true);
    pass(&h, &mut b);
    let (c3, c4) = (no_target(3), no_target(4));
    let mut newer = c3.clone();
    newer.objects.insert(
        object::MANIFEST,
        slop_ninja_protocol::sign_raw::<CohortManifestV2>(b"a record of a newer schema", &keys_fx::package_key()).unwrap(),
    );
    b.put(&newer, true);
    b.put(&c4, true);
    h.observe_epoch(1);
    let r = pass(&h, &mut b);
    assert!(r.handled.iter().all(|x| x.outcome.is_none()), "nothing handed in");
    assert_eq!((counts(&h).get("unreadable"), counts(&h).get("foreign")), (Some(&1), None));
    assert_eq!(h.view().error.as_deref(), Some("cohort_gap"));
    assert!(h.logged("auto_target_failed").contains(&Some(format!("{}: manifest_undecodable", c3.label))));
    b.put(&c3, true);
    h.observe_epoch(2);
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(3, Some("no_target")), (4, Some("no_target"))]);
    assert_eq!(h.view().error, None);
}

/// A label classified under a wrong trust root or record (a deploy that
/// briefly carried another package key or authority list) is read again
/// once they change, so no cohort is lost to the bad window.
#[test]
fn foreign_and_outside_labels_are_read_again_when_the_trust_roots_or_record_change() {
    let h = H::new();
    let mut b = Bucket::new(1000);
    b.put(&no_target(2), true);
    let r = pass_under(&h, &mut b, &Record::prefixes(&["testnet-r9-"]));
    assert!(r.handled.is_empty());
    assert_eq!(counts(&h).get("outside"), Some(&1));
    let r = pass(&h, &mut b);
    assert_eq!((r.requeued, outcomes(&r)), (1, vec![(2, Some("no_target"))]));
    // Another package key: the next cohort is foreign until it is back.
    b.put(&no_target(3), true);
    h.observe_epoch(1);
    let wrong = TrustedKeys::new(
        SigningKey::from_bytes(&[0x42; 32]).verifying_key().to_bytes(),
        *h.trusted.key(KeyRole::Receipt),
        *h.trusted.key(KeyRole::Authorization),
    )
    .unwrap();
    let cell = RefCell::new(std::mem::take(&mut b));
    let r = block_on(run_pass(&mut Io { h: &h, b: &cell, yields: false }, &h.cfg, &wrong, Ok(&record()), &FixtureKey));
    b = cell.into_inner();
    assert!(r.handled.is_empty());
    assert_eq!(h.view().unclassified, vec![("foreign:manifest_unverified".to_owned(), 1)]);
    let r = pass(&h, &mut b);
    assert_eq!((r.requeued, outcomes(&r)), (1, vec![(3, Some("no_target"))]));
    // A lower cohort that resolves only after later ones were handled (its
    // label was foreign under a wrong key while 2 and 3 went on) is reported.
    let late = relabelled(&no_target(1), "synthetic-reward-v1-1");
    b.put(&late, true);
    h.observe_epoch(2);
    let r = pass(&h, &mut b);
    assert!(r.handled.is_empty());
    let v = h.view();
    assert_eq!((v.error.as_deref(), v.error_label.as_deref()), (Some("cohort_behind_record"), Some("synthetic-reward-v1-1")));
    // Unset or malformed authorities: nothing is read, and it is shown.
    let cell = RefCell::new(std::mem::take(&mut b));
    let r = block_on(run_pass(&mut Io { h: &h, b: &cell, yields: false }, &h.cfg, &h.trusted, Err("record_authorities_unset"), &FixtureKey));
    assert_eq!((r.error.as_deref(), r.steps), (Some("record_authorities_unset"), 0));
    assert_eq!(h.view().error.as_deref(), Some("record_authorities_unset"));
}

/// The tracked coordinator configuration binds the handoff to the eval
/// Worker's tracked main authority, and the binding refuses what it cannot
/// bound.
#[test]
fn the_tracked_record_authorities_name_the_eval_workers_main_authority() {
    let toml = include_str!("../../wrangler.toml");
    let var = |name: &str| {
        toml.lines().find_map(|l| {
            l.trim().strip_prefix(name).and_then(|r| r.trim().strip_prefix('=')).map(|v| v.trim().trim_matches('"').to_owned())
        })
    };
    let record = Record::parse(var("AUTO_TARGET_AUTHORITIES").as_deref()).unwrap();
    let main = var("EVAL_MAIN_AUTHORITY").unwrap();
    assert!(record.admits(&format!("testnet-{main}-15")));
    assert!(!record.admits(&format!("testnet-{main}-")));
    assert!(!record.admits("testnet-scratch-1"));
    for bad in [None, Some(""), Some(" , "), Some("main"), Some("R0"), Some("r0,x y")] {
        assert!(Record::parse(bad).is_err(), "{bad:?}");
    }
    assert_eq!(Record::parse(Some("r1, r0,r0")).unwrap(), Record::prefixes(&["testnet-r0-", "testnet-r1-"]));
}

/// An abandonment record is refused for a pending or unknown label and can
/// be taken back while nothing after it was handled.
#[test]
fn an_abandonment_record_is_taken_back_while_nothing_after_it_was_handled() {
    let h = H::new();
    let mut b = Bucket::new(1000);
    let (c2, c3) = (no_target(2), no_target(3));
    b.put(&c2, false);
    b.put(&c3, false);
    pass(&h, &mut b);
    assert_eq!(tx(&h.db, |sql| mark_abandoned(sql, &c2.label, T0)).unwrap(), Ok(2));
    assert_eq!(tx(&h.db, |sql| unmark_abandoned(sql, &c2.label, T0)).unwrap(), Ok(2));
    assert_eq!(h.view().next.map(|n| n.0), Some(2), "in record again");
    assert_eq!(tx(&h.db, |sql| unmark_abandoned(sql, &c2.label, T0)).unwrap(), Err("label_not_passed"));
    assert_eq!(tx(&h.db, |sql| mark_abandoned(sql, &c2.label, T0)).unwrap(), Ok(2));
    b.close(&c3);
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(3, Some("no_target"))]);
    assert_eq!(tx(&h.db, |sql| unmark_abandoned(sql, &c2.label, T0)).unwrap(), Err("later_cohort_handled"));
    assert_eq!(tx(&h.db, |sql| unmark_abandoned(sql, "unknown", T0)).unwrap(), Err("label_unknown"));
}

/// A key source that counts how often the signing key (with the hotkey
/// comparison) is read.
#[derive(Clone)]
struct CountingKey(std::rc::Rc<Cell<u32>>);

impl AuthorizationKeySource for CountingKey {
    fn authorization_key(&self, keys: &TrustedKeys) -> Result<SigningKey, &'static str> {
        self.0.set(self.0.get() + 1);
        FixtureKey.authorization_key(keys)
    }

    fn check_key(&self, keys: &TrustedKeys) -> Result<(), &'static str> {
        authorization_key_from(&hex::encode(keys_fx::authorization_key().to_bytes()), None, keys).map(|_| ())
    }
}

/// The per-pass key check never takes the signing path (the Worker's reads
/// the hotkey's secret only there); a pass that signs reads it once.
#[test]
fn the_per_pass_key_check_never_reads_the_signing_path() {
    let h = H::new();
    let reads = CountingKey(std::rc::Rc::new(Cell::new(0)));
    let mut b = Bucket::new(1000);
    b.put(&no_target(2), true);
    let r = pass_with(&h, &mut b, &reads);
    assert_eq!((outcomes(&r), r.key_error.as_deref(), reads.0.get()), (vec![(2, Some("no_target"))], None, 0));
    pass_with(&h, &mut b, &reads);
    assert_eq!(reads.0.get(), 0);
    let (t1, _) = targets();
    b.put(&t1, true);
    h.observe_epoch(1);
    assert_eq!(outcomes(&pass_with(&h, &mut b, &reads)), vec![(3, Some("installed"))]);
    assert_eq!(reads.0.get(), 1);
}

/// Two passes in one Durable Object at once (an overlapping cron tick),
/// interleaved at every bucket operation: the paged listing, a pending
/// intermediate and the final install happen once; each cohort has one
/// record, and the installed authorization is the one recorded.
#[test]
fn two_overlapping_passes_interleaved_at_every_bucket_operation_install_once() {
    use std::task::{Context, Poll, Waker};
    let h = H::new();
    let (t1, t2) = targets();
    let mut bucket = Bucket::new(1);
    bucket.put(&no_target(2), true);
    bucket.put(&t1, true);
    bucket.retire(&t1);
    bucket.put(&t2, true);
    let cell = RefCell::new(bucket);
    let rec = record();
    let (mut a, mut b) = (Io { h: &h, b: &cell, yields: true }, Io { h: &h, b: &cell, yields: true });
    let mut fa = std::pin::pin!(run_pass(&mut a, &h.cfg, &h.trusted, Ok(&rec), &FixtureKey));
    let mut fb = std::pin::pin!(run_pass(&mut b, &h.cfg, &h.trusted, Ok(&rec), &FixtureKey));
    let mut cx = Context::from_waker(Waker::noop());
    let (mut ra, mut rb) = (None, None);
    let mut polls = 0;
    while ra.is_none() || rb.is_none() {
        polls += 1;
        assert!(polls < 10_000);
        if ra.is_none()
            && let Poll::Ready(r) = fa.as_mut().poll(&mut cx)
        {
            ra = Some(r);
        }
        if rb.is_none()
            && let Poll::Ready(r) = fb.as_mut().poll(&mut cx)
        {
            rb = Some(r);
        }
    }
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    assert_eq!((ra.error.as_deref(), rb.error.as_deref()), (None, None));
    let mut new: Vec<(u64, Option<String>)> =
        ra.handled.iter().chain(&rb.handled).filter(|x| x.new).map(|x| (x.cohort.unwrap(), x.outcome.clone())).collect();
    new.sort();
    assert_eq!(
        new,
        vec![(2, Some("no_target".into())), (3, Some("intermediate".into())), (4, Some("installed".into()))]
    );
    let rows: Vec<(String, i64)> = h
        .db
        .conn
        .prepare("SELECT label, COUNT(*) FROM sign_auto_cohorts GROUP BY label ORDER BY label")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|(_, n)| *n == 1));
    assert_eq!(h.logged("target_installed").len(), 2, "the launch bundle, then T2 once");
    assert_eq!(h.held().digest(), t2.target.unwrap());
    let plan = tx(&h.db, |sql| pointer_plan(sql, &h.cfg, &h.trusted)).unwrap().unwrap();
    let bytes = tx(&h.db, |sql| pointer_object(sql, &h.cfg, &h.trusted, &plan.objects[0].sha256)).unwrap().unwrap();
    let auth = Signed::<RewardIntentAuthorization>::open(&bytes, &h.trusted).unwrap();
    let recorded = ra.handled.iter().chain(&rb.handled).find(|x| x.new && x.cohort == Some(4)).unwrap();
    assert_eq!(recorded.authorization.as_deref(), Some(auth.digest().unwrap().to_string().as_str()));
    assert_eq!((auth.cohort, auth.target), (4, t2.target.unwrap()));
    assert_eq!(h.view().pending, 0);
}

/// The signing steps' reads of the stored bundle (`journal/fetch`): the
/// active target parsed, verified and held as a step holds it, while the
/// pointer plan and `/status` parse the bundle again.
#[cfg(feature = "arena-witness")]
fn signing_reads(h: &H) -> usize {
    let active = tx(&h.db, |sql| active_target(sql, &h.cfg, &h.trusted)).unwrap().unwrap().ok().unwrap();
    let plan = tx(&h.db, |sql| pointer_plan(sql, &h.cfg, &h.trusted)).unwrap().unwrap();
    let status = tx(&h.db, |sql| crate::signing::status(sql, &h.cfg, 8, T0)).unwrap();
    let generation = tx(&h.db, installed_generation).unwrap().unwrap();
    let n = plan.body.len() + status.ops.len() + generation as usize;
    drop(active);
    n
}

/// A journal holding a cohort with `padding` archived artifacts as its
/// installed target, and the stored bundle's length.
fn installed_at(padding: u32) -> (H, usize) {
    let h = H::new();
    let (t1, _) = targets();
    let mut b = Bucket::new(1000);
    b.put(&t1, true);
    pass(&h, &mut b);
    let big = successor(4, t1.target.unwrap(), padding);
    b.put(&big, true);
    h.observe_epoch(1);
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(4, Some("installed"))]);
    let (_, stored) = tx(&h.db, signer_target_bundle).unwrap().unwrap();
    (h, stored.len())
}

/// No path installs a bundle a later signing step could not re-read within
/// its footprint: the operator's bundle above the bound is refused, and the
/// handoff refuses a cohort above the source bound before signing.
#[test]
fn no_installed_bundle_exceeds_what_the_signing_steps_reread() {
    let (h, len) = installed_at(padding_at_the_bound());
    assert!(len <= SIGNED_BUNDLE_BYTES, "{len} B");
    const { assert!(2 * SOURCE_BYTES + BUNDLE_FRAMING_BYTES <= SIGNED_BUNDLE_BYTES) };
    let (t1, _) = targets();
    let over = successor(4, t1.target.unwrap(), padding_at_the_bound() + 64);
    assert!(over.source_bytes() > SOURCE_BYTES);
    let o = observed(&over);
    let m = Signed::<CohortManifestV2>::open(o.manifest.as_deref().unwrap(), &h.trusted).unwrap();
    let p = Signed::<ScorePackageV2>::open(o.package.as_deref().unwrap(), &h.trusted).unwrap();
    let auth = RewardIntentAuthorization {
        chain: chain(),
        cohort: 4,
        cohort_manifest: m.digest().unwrap(),
        score_package: p.digest().unwrap(),
        target: over.target.unwrap(),
        checkpoint_generation: 1,
        issued_epoch: over.closed_epoch,
        renew_by_epoch: over.closed_epoch + 40,
    };
    let json = bundle_json(&o, &sign(auth, &keys_fx::authorization_key()).unwrap().to_bytes().unwrap());
    let fresh = H::new();
    let mut b = Bucket::new(1000);
    b.put(&t1, true);
    pass(&fresh, &mut b);
    let held = fresh.held();
    let refused = tx(&fresh.db, move |sql| {
        install_target_v2_from(sql, &crate::signing::SignConfig::testnet(Id(TESTNET_GENESIS.0), NETUID, [7; 32]), &keys_fx::trusted_keys(), &json, T0, &held, false)
    })
    .unwrap()
    .unwrap_err();
    assert_eq!(refused.code, "bundle_exceeds_signing_footprint");
    b.put(&over, true);
    fresh.observe_epoch(1);
    let r = pass(&fresh, &mut b);
    assert_eq!(r.handled.iter().map(|x| x.error.as_deref()).collect::<Vec<_>>(), vec![Some("cohort_too_large")]);
    assert_eq!(fresh.held().digest(), t1.target.unwrap());
}

/// Measurement (`--features arena-witness`): the signing reads' peak at
/// three bundle sizes, the inputs of `arena_routes::SIGNING_BASE_BYTES` and
/// `SIGNING_PER_BUNDLE_BYTE`.
#[cfg(feature = "arena-witness")]
#[test]
fn arena_witness_measure_the_signing_reads_of_a_stored_bundle() {
    use slop_ninja_arena::{Mode, set_mode};
    set_mode(Mode::Enforce);
    let full = padding_at_the_bound();
    for padding in [0, full / 4, full / 2, full] {
        let (h, len) = installed_at(padding);
        let _ = signing_reads(&h);
        let (pages, live) = crate::arena_witness::measure(|| {
            signing_reads(&h);
        });
        eprintln!("signing reads: bundle {len} B, peak {pages} pages, peak live {live} B");
    }
}

/// Arena witness (`--features arena-witness`): at the bundle bound the
/// signing reads fit half the journal's base footprint (the other half is
/// the margin `JOURNAL_BASE_BYTES` keeps for wasm-bindgen's string transfer).
#[cfg(feature = "arena-witness")]
#[test]
fn arena_witness_the_signing_reads_fit_the_journal_footprint_at_the_bundle_bound() {
    use crate::arena_routes::{JOURNAL_BASE_BYTES, R_JOURNAL_FETCH};
    use slop_ninja_arena::{Mode, set_mode};
    set_mode(Mode::Enforce);
    let (h, len) = installed_at(padding_at_the_bound());
    eprintln!("bundle {len} B of {SIGNED_BUNDLE_BYTES} B");
    let _ = signing_reads(&h);
    crate::arena_witness::witness(R_JOURNAL_FETCH.route(), JOURNAL_BASE_BYTES / 2, || signing_reads(&h));
}

// ------------------------------------------------- eval-signed ends (D2)

fn abandonment(label: &str, cohort: u64, chain: ChainBinding, epoch: u64) -> Vec<u8> {
    let r = slop_ninja_protocol::decisions::CohortAbandonmentV2 {
        chain,
        cohort,
        cohort_label: label.into(),
        cohort_manifest: None,
        fault: "none".into(),
        reason: "operator_decision".into(),
        abandoned_epoch: Some(epoch),
        phase_at_abandon: "queued".into(),
        score_package: None,
        result: None,
        target: None,
        operator: true,
        decision: None,
        statement: None,
    };
    sign(r, &keys_fx::package_key()).unwrap().to_bytes().unwrap()
}

/// D2: an in-record cohort that never closes is passed by the eval's signed
/// abandonment record, and the next cohort is handed in with no operator
/// step; a queued cohort abandoned before its seal (no manifest) and a probe
/// cut pass by number; a record of another chain passes nothing.
#[test]
fn eval_signed_ends_pass_their_numbers_with_no_operator_step() {
    let h = H::new();
    let mut b = Bucket::new(1000);
    let c2 = no_target(2);
    b.put(&c2, false); // never closes
    let c3 = no_target(3);
    b.put(&c3, true);
    let r = pass(&h, &mut b);
    assert!(r.handled.is_empty());
    assert_eq!(h.view().error.as_deref(), Some("earlier_cohort_unclosed"));
    // A record of another chain: refused, nothing passed.
    let mut other = chain();
    other.netuid += 1;
    b.objects.insert(key(&c2.label, object::ABANDONED), abandonment(&c2.label, 2, other, 1));
    h.observe_epoch(1);
    let r = pass(&h, &mut b);
    assert!(r.handled.is_empty());
    assert_eq!(h.view().error.as_deref(), Some("record_chain_mismatch"));
    // The eval's own record passes 2, and 3 is handed in.
    b.objects.insert(key(&c2.label, object::ABANDONED), abandonment(&c2.label, 2, chain(), 1));
    h.observe_epoch(2);
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(3, Some("no_target"))]);
    assert!(h.logged("auto_target_abandoned").iter().any(|d| d.as_deref() == Some(&format!("2 {} eval_abandoned", c2.label)[..])));
    // Number 4 was abandoned while queued (no manifest), 5 is a probe cut
    // (no manifest), 6 closes: all pass by their records.
    let l4 = "synthetic-reward-v1-4-queued".to_owned();
    b.objects.insert(key(&l4, object::ABANDONED), abandonment(&l4, 4, chain(), 3));
    let l5 = "synthetic-reward-v1-probe-v2".to_owned();
    let cut = slop_ninja_protocol::decisions::ProbeCutRecord { chain: chain(), cohort: 5, cohort_label: l5.clone(), probe_version: 2, cut_epoch: 3 };
    b.objects.insert(key(&l5, object::PROBE_CUT), sign(cut, &keys_fx::package_key()).unwrap().to_bytes().unwrap());
    let c6 = no_target(6);
    b.put(&c6, true);
    h.observe_epoch(3);
    assert_eq!(outcomes(&pass(&h, &mut b)), vec![(6, Some("no_target"))]);
    assert_eq!(h.view().error, None);
    let counts: BTreeMap<String, u64> = h.view().labels.into_iter().collect();
    assert_eq!(counts.get("passed"), Some(&3));
}

/// D2: a declined closure passes its number; a closure written beside an
/// end record is a conflict, and nothing is installed from it.
#[test]
fn a_declined_closure_passes_and_a_closure_beside_an_end_record_conflicts() {
    let h = H::new();
    let mut b = Bucket::new(1000);
    let c2 = no_target(2);
    b.put(&c2, false);
    let decline = slop_ninja_protocol::decisions::ClosureDeclined {
        chain: chain(),
        cohort: 2,
        cohort_label: c2.label.clone(),
        cohort_manifest: None,
        score_package: None,
        declined_target: None,
        phase: "closed".into(),
        fault: "closure_failed".into(),
        declined_epoch: Some(2),
        decision: None,
        statement: Digest([9; 32]),
    };
    b.objects.insert(key(&c2.label, object::CLOSURE_DECLINED), sign(decline, &keys_fx::package_key()).unwrap().to_bytes().unwrap());
    let c3 = no_target(3);
    b.put(&c3, true);
    b.objects.insert(key(&c3.label, object::ABANDONED), abandonment(&c3.label, 3, chain(), 2));
    let r = pass(&h, &mut b);
    assert!(r.handled.is_empty(), "{r:?}");
    let v = h.view();
    assert_eq!((v.error.as_deref(), v.error_label.as_deref()), (Some("abandon_conflict"), Some(c3.label.as_str())));
    assert!(h.logged("auto_target_abandoned").iter().any(|d| d.as_deref() == Some(&format!("2 {} eval_closure_declined", c2.label)[..])));
}

// --------------------------------------------------- burn now (P1-R)

fn op_key() -> (SigningKey, slop_ninja_protocol::operator::OperatorKey) {
    let k = SigningKey::from_bytes(&[0x61; 32]);
    let p = slop_ninja_protocol::operator::OperatorKey::from_bytes(k.verifying_key().to_bytes()).unwrap();
    (k, p)
}

fn restart_body(h: &H, decision: &str, floor: u64, issued: u64, past: &[u64], unlisted: &[u64], last: Option<u64>) -> serde_json::Value {
    let held = h.held().digest().to_string();
    serde_json::json!({"action": "restart", "decision": decision, "reason": "operator_decision", "floor": floor,
        "bundle": launch_bundle_json(issued), "last": last, "past": past, "unlisted": unlisted, "expect": {"held": held}})
}

fn signed(body: serde_json::Value) -> serde_json::Value {
    let c = chain();
    let to = crate::signing::decide::address(&c);
    crate::operator::tests::signed_body(&op_key().0, "coordinator.restart", &to, body)
}

fn decide_restart(h: &H, body: serde_json::Value, observed: u64) -> Result<serde_json::Value, &'static str> {
    let (_, key) = op_key();
    tx(&h.db, |sql| crate::signing::decide::decide(sql, &h.cfg, &h.trusted, body, Some(&key), Some(observed), T0)).unwrap()
}

/// P1-R with review-p1-final M1: a regular cohort is open while the number
/// after it was passed out of order; the restart names the open one in `past`
/// (bounded by opening epoch, never by number), refuses a body that omits
/// it, and every refusal (no signature, a floor past finalized + 1, a launch
/// issued at another epoch, a stale held target) changes nothing. The
/// accepted restart journals it as `restarted_past`, writes nothing to
/// `sign_auto_absent`, installs the launch row from the floor, and queues the
/// signed `ChainRestart` record; a later replay answers the same.
#[test]
fn burn_now_names_every_pre_floor_cohort_and_refusals_change_nothing() {
    let h = H::new();
    let mut b = Bucket::new(1000);
    let c11 = no_target(11);
    b.put(&c11, false);
    let c12 = no_target(12);
    b.put(&c12, false);
    pass(&h, &mut b);
    assert_eq!(tx(&h.db, |sql| mark_abandoned(sql, &c12.label, T0)).unwrap(), Ok(12));
    let counts = |h: &H| (h.db.count("sign_held_target_resets"), h.db.count("sign_restart_past"), h.db.count("operator_actions"));
    let before = counts(&h);
    // Only the token: refused.
    assert_eq!(decide_restart(&h, restart_body(&h, "drill-p0", 250, 250, &[11], &[], None), 300), Err("authorization_required"));
    // A floor past finalized + 1 (D13).
    assert_eq!(decide_restart(&h, signed(restart_body(&h, "drill-p1", 302, 302, &[11], &[], None)), 300), Err("restart_future"));
    // The launch authorization issued at another epoch than the floor.
    assert_eq!(decide_restart(&h, signed(restart_body(&h, "drill-p2", 250, 251, &[11], &[], None)), 300), Err("restart_issue_mismatch"));
    // The set S: 11 opened before the floor and was never handled.
    assert_eq!(decide_restart(&h, signed(restart_body(&h, "drill-p3", 250, 250, &[], &[], None)), 300), Err("past_changed"));
    let mut stale = restart_body(&h, "drill-p4", 250, 250, &[11], &[], None);
    let other_target = hex::encode([0u8; 32]);
    stale["expect"]["held"] = serde_json::json!(other_target);
    assert_eq!(decide_restart(&h, signed(stale), 300), Err("expect_mismatch"));
    assert_eq!(counts(&h), before, "every refusal changes nothing");
    // Signing is paused (it starts paused): the restart goes ahead.
    let v = decide_restart(&h, signed(restart_body(&h, "drill-p5", 250, 250, &[11], &[], Some(12))), 300).unwrap();
    assert_eq!(v["past"][0]["cohort"], 11);
    assert_eq!(v["unlisted"], serde_json::json!([]));
    assert_eq!(tx(&h.db, restart_floor).unwrap(), Some(250));
    let (reason, state): (String, String) = h
        .db
        .conn
        .query_row("SELECT reason, state FROM sign_restart_past WHERE floor = 250 AND cohort = 11", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!((reason.as_str(), state.as_str()), ("restarted_past", "in_record"));
    assert_eq!(h.db.count("sign_auto_absent"), 0, "a restart never marks a number absent");
    assert_eq!(h.db.count("sign_restart_records"), 1);
    let rec: String = h.db.conn.query_row("SELECT record_hex FROM sign_restart_records", [], |r| r.get(0)).unwrap();
    let rec = <slop_ninja_protocol::decisions::ChainRestart as Record_>::from_bytes(&hex::decode(rec).unwrap()).unwrap();
    assert_eq!((rec.floor, rec.last, rec.past.len()), (250, Some(12), 1));
    // The same decision replays its recorded effect and changes nothing.
    let again = decide_restart(&h, signed(restart_body(&h, "drill-p5", 250, 250, &[11], &[], Some(12))), 300);
    assert!(again.is_ok() || again == Err("expect_mismatch"), "{again:?}");
    let resets = h.db.count("sign_held_target_resets");
    assert_eq!(resets, 1, "one restart");
}

/// P1-R `launch_needs_restart`: after the first install, a different launch
/// bundle on the operator route is refused, and a byte-identical re-post of
/// the active one is an idempotent replay that writes nothing.
#[test]
fn a_later_launch_bundle_installs_only_through_a_restart() {
    let h = H::new();
    let first = launch_bundle_json(1);
    let rows = |h: &H| h.db.count("sign_log");
    let n = rows(&h);
    let replay = tx(&h.db, |sql| install_target(sql, &h.cfg, &h.trusted, &first, T0)).unwrap();
    assert!(replay.is_ok());
    assert_eq!(rows(&h), n, "the replay writes nothing");
    let r = tx(&h.db, |sql| install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(5), T0)).unwrap().unwrap_err();
    assert_eq!(r.code, "launch_needs_restart");
}

/// P1-R with S1: a cohort found after the restart that opened before its
/// floor and that the signed record did not name is journaled
/// `restarted_past_unlisted` with a signed addendum; one the record named
/// as unlisted is journaled with no addendum.
#[test]
fn a_pre_floor_cohort_found_after_a_restart_is_recorded_with_its_addendum() {
    let h = H::new();
    let mut b = Bucket::new(1000);
    let c11 = no_target(11);
    b.put(&c11, false);
    pass(&h, &mut b);
    // 12 is not listed at the restart; the operator read 12 as the highest
    // opened number, so the record names it as unlisted.
    let v = decide_restart(&h, signed(restart_body(&h, "drill-u1", 250, 250, &[11], &[12], Some(12))), 300).unwrap();
    assert_eq!(v["unlisted"], serde_json::json!([12]));
    // 12 and 13 appear later, both opened before the floor (240, 260 > 250:
    // 13 opened after it and is not a pre-floor cohort).
    let c12 = no_target(12);
    b.put(&c12, false);
    let c13 = no_target(13);
    b.put(&c13, false);
    h.observe_epoch(301);
    pass(&h, &mut b);
    let reason: String = h.db.conn.query_row("SELECT reason FROM sign_restart_past WHERE floor = 250 AND cohort = 12", [], |r| r.get(0)).unwrap();
    assert_eq!(reason, "restarted_past_unlisted");
    assert_eq!(h.db.count("sign_restart_records"), 1, "named as unlisted: no addendum");
    assert!(h.db.conn.query_row("SELECT 1 FROM sign_restart_past WHERE cohort = 13", [], |r| r.get::<_, i64>(0)).is_err());
    // A second restart; then a pre-floor cohort it never named gets an addendum.
    tx(&h.db, |sql| sql.exec("DELETE FROM sign_auto_labels WHERE label = ?", &[c13.label.as_str().into()]).map(|_| ())).unwrap();
    let past: Vec<u64> = vec![];
    let v = decide_restart(&h, signed(restart_body(&h, "drill-u2", 300, 300, &past, &[], None)), 300);
    assert!(v.is_ok(), "{v:?}");
    h.observe_epoch(302);
    pass(&h, &mut b);
    let reason: String = h.db.conn.query_row("SELECT reason FROM sign_restart_past WHERE floor = 300 AND cohort = 13", [], |r| r.get(0)).unwrap();
    assert_eq!(reason, "restarted_past_unlisted");
    assert_eq!(h.db.conn.query_row("SELECT COUNT(*) FROM sign_restart_records WHERE kind = 'addendum'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
}
