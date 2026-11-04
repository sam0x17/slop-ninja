//! The signing journal against a fake chain built on recorded testnet
//! metadata: every storage value, block body and event list is SCALE-encoded
//! from live V15 metadata by name, exactly as the real chain serves them.
//!
//! The fake includes pooled extrinsics in the next block, checks the nonce,
//! keys commits by the pallet's post-coinbase epoch, and reveals them at the
//! lookahead epoch boundary (it cannot decrypt, so it reveals the row the
//! journal recorded). A harness plays the Worker: it asks for a step, makes
//! the one call, posts the outcome back, and can lose, delay or misreport any
//! of them.
//!
//! Runtimes are per block, as on a real chain: a runtime upgrade enacted in
//! block `E` puts the new code in `E`'s post-state (so the runtime version
//! and metadata *at* `E` are the new runtime's), while `E` itself was
//! executed and its storage written by the old runtime, whose spec its
//! `LastRuntimeUpgrade` still holds. The second runtime (487) is the
//! recorded metadata with `frame_system`'s `ExtrinsicSuccess` and
//! `ExtrinsicFailed` event indices swapped, so decoding a block with the
//! wrong runtime visibly fails.

use super::*;
use crate::sql::{CHAIN_JOURNAL_SCHEMA, migrate};
use crate::testing::{Sqlite, chain_hash, tx};
use rtao_core::timelock::epoch_schedule::{current_epoch_pre_run_coinbase, simulate_run_coinbase};
use slop_ninja_protocol::fixtures as fx;
use slop_ninja_protocol::{CalculationTarget, NeuronIdentity, Record, sign};
use std::cell::Cell;

const METADATA: &[u8] = include_bytes!("../../../fixtures/runtime-486/testnet-486.v15.scale");
const NETUID: u16 = 4242;
const ALICE_SEED: &str = "0xe5be9a5092b81bca64be81d212e7f2f9eba183bb7a90954f7b76361f6edb5c0a";
const COORD_UID: u16 = 1;
const POPULATION: u16 = 10;
const START: u64 = 5_000;
const EPOCH_START: u64 = 4_800;
const T0: u64 = 1_793_000_000_000;

/// The recorded metadata with `frame_system::Event`'s first two variant
/// indices swapped: a runtime whose events decode differently.
fn metadata_487() -> Vec<u8> {
    use frame_metadata::{RuntimeMetadata, RuntimeMetadataPrefixed};
    use parity_scale_codec::{Decode, Encode};
    let mut m = RuntimeMetadataPrefixed::decode(&mut &METADATA[..]).unwrap();
    let RuntimeMetadata::V15(v15) = &mut m.1 else {
        panic!("V15 fixture")
    };
    let mut swapped = 0;
    for t in v15.types.types.iter_mut() {
        if t.ty.path.segments == ["frame_system", "pallet", "Event"]
            && let scale_info::TypeDef::Variant(v) = &mut t.ty.type_def
        {
            for variant in v.variants.iter_mut() {
                match variant.name.as_str() {
                    "ExtrinsicSuccess" => variant.index = 1,
                    "ExtrinsicFailed" => variant.index = 0,
                    _ => continue,
                }
                swapped += 1;
            }
        }
    }
    assert_eq!(swapped, 2);
    let out = m.encode();
    assert_ne!(out, METADATA);
    out
}

fn alice() -> Hotkey {
    Hotkey::from_seed_hex(ALICE_SEED).unwrap()
}

struct AliceKeys {
    reads: Cell<u32>,
}

impl KeySource for AliceKeys {
    fn hotkey(&self) -> Result<Hotkey, ChainError> {
        self.reads.set(self.reads.get() + 1);
        Ok(alice())
    }
}

fn neurons() -> Vec<NeuronIdentity> {
    let mut all = vec![
        fx::owner(),
        fx::miner_a(),
        fx::miner_b(),
        NeuronIdentity {
            uid: 7,
            hotkey: slop_ninja_protocol::AccountId([0xc9; 32]),
            registration_block: 1500,
        },
        fx::stranger(),
        NeuronIdentity {
            uid: COORD_UID,
            hotkey: slop_ninja_protocol::AccountId(alice().public()),
            registration_block: 500,
        },
    ];
    for uid in [2u16, 4, 6, 8] {
        all.push(NeuronIdentity {
            uid,
            hotkey: slop_ninja_protocol::AccountId([0x20 + uid as u8; 32]),
            registration_block: 600,
        });
    }
    all.sort_by_key(|n| n.uid);
    all
}

/// The protocol fixture cohort as a signed T12 bundle.
fn bundle_json() -> String {
    let cohort = fx::cohort().unwrap();
    let package = fx::package(&cohort).unwrap();
    let target = CalculationTarget::for_package(&package, None).unwrap();
    let pk = fx::package_key();
    let hexr = |b: Vec<u8>| hex::encode(b);
    let b = target::Bundle {
        schema: target::BUNDLE_SCHEMA.into(),
        shortcut: "T12".into(),
        manifest: hexr(
            sign(cohort.manifest.clone(), &pk)
                .unwrap()
                .to_bytes()
                .unwrap(),
        ),
        package: hexr(sign(package, &pk).unwrap().to_bytes().unwrap()),
        target: hexr(sign(target, &pk).unwrap().to_bytes().unwrap()),
        receipt_index: hexr(
            sign(cohort.index.clone(), &fx::receipt_key())
                .unwrap()
                .to_bytes()
                .unwrap(),
        ),
        receipts: cohort
            .index
            .entries
            .iter()
            .map(|e| {
                let r = cohort
                    .receipts
                    .iter()
                    .find(|r| r.attempt == e.attempt)
                    .unwrap();
                hexr(r.to_bytes().unwrap())
            })
            .collect(),
    };
    serde_json::to_string(&b).unwrap()
}

// ------------------------------------------------------------- fake chain

#[derive(Clone)]
struct Entry {
    who: [u8; 32],
    block: u64,
    ciphertext: Vec<u8>,
    round: u64,
}

struct Fake {
    /// Runtimes by spec, with their prefixed metadata.
    runtimes: BTreeMap<u32, (ChainRuntime, Vec<u8>)>,
    /// (enactment block, new spec): the code in that block's post-state.
    upgrades: Vec<(u64, u32)>,
    /// Blocks whose `System.Events` value is undecodable bytes.
    corrupt_events: std::collections::BTreeSet<u64>,
    head: u64,
    sched: EpochScheduleState,
    nonce: u64,
    states: BTreeMap<u64, BTreeMap<Vec<u8>, Vec<u8>>>,
    bodies: BTreeMap<u64, Vec<String>>,
    pool: Vec<Vec<u8>>,
    /// Every extrinsic the node accepted, in order.
    accepted: Vec<Vec<u8>>,
    /// Every extrinsic a block included.
    included: Vec<(u64, Vec<u8>)>,
    queue: BTreeMap<u64, Vec<Entry>>,
    last_update: Vec<u64>,
    weights: Vec<(u16, u16)>,
    reveal_row: Vec<(u16, u16)>,
    reveals: Vec<u64>,
    cr_enabled: bool,
    /// A foreign extrinsic consumes the nonce at the next block.
    foreign_next: bool,
    /// Blocks after the first reveal block before the drand pulse arrives.
    reveal_delay: u64,
    /// The pallet drops the commit without revealing it.
    reveal_drop: bool,
    reveal_first: BTreeMap<u64, u64>,
}

fn put(
    rt: &ChainRuntime,
    state: &mut BTreeMap<Vec<u8>, Vec<u8>>,
    pallet: &str,
    item: &str,
    params: &[Value],
    value: Value,
) {
    let key = rt.storage_key(pallet, item, params).unwrap();
    let entry = rt.core().storage_entry(pallet, item).unwrap();
    let mut out = Vec::new();
    rt.core()
        .encode_id(entry.value_type, &value, &mut out)
        .unwrap_or_else(|e| panic!("{pallet}.{item}: {e}"));
    state.insert(key, out);
}

fn int(v: u64) -> Value {
    Value::Int(i128::from(v))
}

fn record(phase: Option<u32>, pallet: &str, event: &str, payload: Value) -> Value {
    Value::record(vec![
        (
            "phase".into(),
            match phase {
                Some(i) => Value::record(vec![("ApplyExtrinsic".into(), int(u64::from(i)))]),
                None => Value::str("Initialization"),
            },
        ),
        (
            "event".into(),
            Value::record(vec![(
                pallet.into(),
                if payload == Value::Null {
                    Value::str(event)
                } else {
                    Value::record(vec![(event.into(), payload)])
                },
            )]),
        ),
        ("topics".into(), Value::List(vec![])),
    ])
}

fn dispatch_info() -> Value {
    Value::record(vec![(
        "dispatch_info".into(),
        Value::record(vec![
            (
                "weight".into(),
                Value::record(vec![
                    ("ref_time".into(), int(1)),
                    ("proof_size".into(), int(1)),
                ]),
            ),
            ("class".into(), Value::str("Normal")),
            ("pays_fee".into(), Value::str("Yes")),
        ]),
    )])
}

impl Fake {
    fn new() -> Self {
        let mut runtimes = BTreeMap::new();
        runtimes.insert(
            486,
            (
                ChainRuntime::parse(METADATA, 486, 1).unwrap(),
                METADATA.to_vec(),
            ),
        );
        let m487 = metadata_487();
        runtimes.insert(487, (ChainRuntime::parse(&m487, 487, 1).unwrap(), m487));
        let mut f = Self {
            runtimes,
            upgrades: vec![],
            corrupt_events: Default::default(),
            head: START - 1,
            sched: chain::schedule(360, EPOCH_START, 0, 6, START - 1 - EPOCH_START, START - 1),
            nonce: 7,
            states: BTreeMap::new(),
            bodies: BTreeMap::new(),
            pool: vec![],
            accepted: vec![],
            included: vec![],
            queue: BTreeMap::new(),
            last_update: vec![0; usize::from(POPULATION)],
            weights: vec![],
            reveal_row: vec![],
            reveals: vec![],
            cr_enabled: true,
            foreign_next: false,
            reveal_delay: 0,
            reveal_drop: false,
            reveal_first: BTreeMap::new(),
        };
        f.snapshot(START - 1, vec![]);
        f
    }

    /// The spec of the code in block `n`'s post-state (what the runtime
    /// version and metadata APIs report at `n`).
    fn code_spec(&self, n: u64) -> u32 {
        self.upgrades
            .iter()
            .filter(|(at, _)| *at <= n)
            .map(|(_, spec)| *spec)
            .next_back()
            .unwrap_or(486)
    }

    /// The runtime that executed block `n` and wrote its storage.
    fn exec_spec(&self, n: u64) -> u32 {
        self.code_spec(n.saturating_sub(1))
    }

    fn runtime(&self, spec: u32) -> &ChainRuntime {
        &self.runtimes[&spec].0
    }

    /// Enacts `spec` in the next block produced.
    fn upgrade_next(&mut self, spec: u32) {
        self.upgrades.push((self.head + 1, spec));
    }

    fn produce(&mut self) {
        let n = self.head + 1;
        let exec = self.exec_spec(n);
        let mut events = vec![record(None, "System", "NewAccount", Value::Null)];
        events.clear();
        // Reveal processing runs before the coinbase, at the lookahead epoch;
        // commits whose drand pulse has not arrived stay queued and are
        // retried every block.
        let lookahead = current_epoch_pre_run_coinbase(&self.sched, n);
        let target = lookahead.saturating_sub(1);
        if self.queue.contains_key(&target) {
            let first = *self.reveal_first.entry(target).or_insert(n);
            if n >= first + self.reveal_delay
                && let Some(entries) = self.queue.remove(&target)
            {
                for e in entries {
                    if self.reveal_drop {
                        continue;
                    }
                    // With commit-reveal on, the pallet's reveal sets
                    // `Weights` and emits events but leaves `LastUpdate`.
                    self.weights = self.reveal_row.clone();
                    self.reveals.push(n);
                    events.push(record(
                        None,
                        chain::SUBTENSOR,
                        "TimelockedWeightsRevealed",
                        Value::Tuple(vec![int(u64::from(NETUID)), Value::Bytes(e.who.to_vec())]),
                    ));
                }
            }
        }
        self.sched = simulate_run_coinbase(&self.sched, n);
        let mut body = vec!["0x0400".to_owned()];
        if self.foreign_next {
            self.foreign_next = false;
            self.nonce += 1;
            body.push("0xdeadbeef".into());
        }
        for ext in std::mem::take(&mut self.pool) {
            let decoded = self
                .runtime(exec)
                .core()
                .decode_extrinsic(&ext, true)
                .unwrap();
            let nonce = as_u64(field(&decoded, "nonce").unwrap()).unwrap();
            if nonce != self.nonce {
                continue;
            }
            let call = field(&decoded, "call").unwrap();
            let args = as_list(field(call, "call_args").unwrap()).unwrap();
            let arg = |name: &str| {
                args.iter()
                    .find(|a| field(a, "name").and_then(chain::as_str) == Some(name))
                    .and_then(|a| field(a, "value"))
                    .unwrap()
                    .clone()
            };
            let ciphertext = as_bytes(&arg("commit")).unwrap();
            let round = as_u64(&arg("reveal_round")).unwrap();
            let index = body.len() as u32;
            body.push(format!("0x{}", hex::encode(&ext)));
            self.nonce += 1;
            let epoch = current_epoch_pre_run_coinbase(&self.sched, n);
            let who = alice().public();
            self.queue.entry(epoch).or_default().push(Entry {
                who,
                block: n,
                ciphertext: ciphertext.clone(),
                round,
            });
            self.last_update[usize::from(COORD_UID)] = n;
            events.push(record(
                Some(index),
                chain::SUBTENSOR,
                "TimelockedWeightsCommitted",
                Value::Tuple(vec![
                    Value::Bytes(who.to_vec()),
                    int(u64::from(NETUID)),
                    Value::Bytes(chain::sha256(&ciphertext).to_vec()),
                    int(round),
                ]),
            ));
            events.push(record(
                Some(index),
                "System",
                "ExtrinsicSuccess",
                dispatch_info(),
            ));
            self.included.push((n, ext));
        }
        self.head = n;
        self.snapshot(n, events);
        self.bodies.insert(n, body);
    }

    fn snapshot(&mut self, n: u64, events: Vec<Value>) {
        let writer = self.exec_spec(n);
        let rt = self.runtime(writer);
        let mut st = BTreeMap::new();
        let net = [int(u64::from(NETUID))];
        let alice = alice().public();
        put(
            rt,
            &mut st,
            "System",
            "LastRuntimeUpgrade",
            &[],
            Value::record(vec![
                ("spec_version".into(), int(u64::from(writer))),
                ("spec_name".into(), Value::str("node-subtensor")),
            ]),
        );
        let mut account = rt.decode_storage("System", "Account", None).unwrap();
        if let Value::Dict(entries) = &mut account {
            for (k, v) in entries.iter_mut() {
                if *k == Value::str("nonce") {
                    *v = int(self.nonce);
                }
            }
        }
        put(
            rt,
            &mut st,
            "System",
            "Account",
            &[Value::Bytes(alice.to_vec())],
            account,
        );
        put(rt, &mut st, "System", "Events", &[], Value::List(events));
        if self.corrupt_events.contains(&n) {
            st.insert(
                rt.storage_key("System", "Events", &[]).unwrap(),
                vec![0x04, 0xff, 0xff],
            );
        }
        put(rt, &mut st, "Timestamp", "Now", &[], int(T0 + n * 12_000));
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "NetworksAdded",
            &net,
            Value::Bool(true),
        );
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "Uids",
            &[net[0].clone(), Value::Bytes(alice.to_vec())],
            int(u64::from(COORD_UID)),
        );
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "CommitRevealWeightsEnabled",
            &net,
            Value::Bool(self.cr_enabled),
        );
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "CommitRevealWeightsVersion",
            &[],
            int(4),
        );
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "RevealPeriodEpochs",
            &net,
            int(1),
        );
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "WeightsVersionKey",
            &net,
            int(0),
        );
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "WeightsSetRateLimit",
            &net,
            int(100),
        );
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "LastUpdate",
            &net,
            Value::List(self.last_update.iter().map(|v| int(*v)).collect()),
        );
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "Tempo",
            &net,
            int(u64::from(self.sched.tempo)),
        );
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "LastEpochBlock",
            &net,
            int(self.sched.last_epoch_block),
        );
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "PendingEpochAt",
            &net,
            int(self.sched.pending_epoch_at),
        );
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "SubnetEpochIndex",
            &net,
            int(self.sched.subnet_epoch_index),
        );
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "BlocksSinceLastStep",
            &net,
            int(self.sched.blocks_since_last_step),
        );
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "SubnetworkN",
            &net,
            int(u64::from(POPULATION)),
        );
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "MinAllowedWeights",
            &net,
            int(1),
        );
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "MaxWeightsLimit",
            &net,
            int(65535),
        );
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "SubnetOwnerHotkey",
            &net,
            Value::Bytes(fx::owner().hotkey.0.to_vec()),
        );
        for n in neurons() {
            let p = [net[0].clone(), int(u64::from(n.uid))];
            put(
                rt,
                &mut st,
                chain::SUBTENSOR,
                "Keys",
                &p,
                Value::Bytes(n.hotkey.0.to_vec()),
            );
            put(
                rt,
                &mut st,
                chain::SUBTENSOR,
                "BlockAtRegistration",
                &p,
                int(n.registration_block),
            );
        }
        for (epoch, entries) in &self.queue {
            put(
                rt,
                &mut st,
                chain::SUBTENSOR,
                "TimelockedWeightCommits",
                &[net[0].clone(), int(*epoch)],
                Value::List(
                    entries
                        .iter()
                        .map(|e| {
                            Value::Tuple(vec![
                                Value::Bytes(e.who.to_vec()),
                                int(e.block),
                                Value::Bytes(e.ciphertext.clone()),
                                int(e.round),
                            ])
                        })
                        .collect(),
                ),
            );
        }
        put(
            rt,
            &mut st,
            chain::SUBTENSOR,
            "Weights",
            &[net[0].clone(), int(u64::from(COORD_UID))],
            Value::List(
                self.weights
                    .iter()
                    .map(|(u, w)| Value::Tuple(vec![int(u64::from(*u)), int(u64::from(*w))]))
                    .collect(),
            ),
        );
        self.states.insert(n, st);
    }

    fn read(&self, n: u64, keys: &[String]) -> Vec<(String, Option<String>)> {
        let st = &self.states[&n];
        keys.iter()
            .map(|k| {
                let raw = hex::decode(k.trim_start_matches("0x")).unwrap();
                (
                    k.clone(),
                    st.get(&raw).map(|v| format!("0x{}", hex::encode(v))),
                )
            })
            .collect()
    }

    /// The metadata API's answer at block `n`: the code in its post-state.
    fn metadata_hex(&self, n: u64) -> String {
        let metadata = &self.runtimes[&self.code_spec(n)].1;
        let mut scale = vec![1u8];
        rtao_core::codec::encode::compact(metadata.len() as u128, &mut scale).unwrap();
        scale.extend_from_slice(metadata);
        format!("0x{}", hex::encode(scale))
    }
}

// ---------------------------------------------------------------- harness

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Send {
    /// The node accepts and the Worker reports it.
    Accept,
    /// The node accepts; the response is lost and the Worker reports uncertainty.
    AcceptReportUncertain,
    /// The request never reaches the node; the Worker reports uncertainty.
    Lose,
    /// The node accepts; the invocation dies before reporting (eviction).
    AcceptThenDie,
    /// The node refuses it.
    Reject,
}

struct H {
    db: Sqlite,
    chain: Fake,
    cfg: SignConfig,
    trusted: slop_ninja_protocol::TrustedKeys,
    runtime: RuntimeCache,
    keys: AliceKeys,
    now: Cell<u64>,
    send: Cell<Send>,
    marks_refused: Cell<u32>,
    /// Blocks the journal does not retain (as after a re-anchor).
    unretained: std::collections::BTreeSet<u64>,
    /// Every runtime-version and metadata request: (step, block).
    runtime_requests: Vec<(&'static str, u64)>,
    /// The next outcomes of these steps fail with this RPC error.
    rpc_fail: Option<(&'static str, RpcError, u32)>,
    /// State reads at blocks below this one answer a JSON-RPC error (a
    /// pruning node after a long outage); block bodies are still served.
    pruned_below: Option<u64>,
    /// Metadata the node serves instead of the real one.
    bad_metadata: bool,
    /// Every call the harness made: (step, block).
    calls: Vec<(&'static str, u64)>,
    /// Every read: (purpose, block).
    reads: Vec<(String, u64)>,
}

impl H {
    fn new() -> Self {
        let db = Sqlite::open();
        tx(&db, |sql| migrate(sql, &CHAIN_JOURNAL_SCHEMA)).unwrap();
        let mut cfg = SignConfig::testnet(
            Id(slop_ninja_protocol::TESTNET_GENESIS.0),
            NETUID,
            alice().public(),
        );
        cfg.rpc_calls_per_minute = 1_000;
        let h = Self {
            db,
            chain: Fake::new(),
            cfg,
            trusted: fx::trusted_keys(),
            runtime: RuntimeCache::default(),
            keys: AliceKeys {
                reads: Cell::new(0),
            },
            now: Cell::new(T0),
            send: Cell::new(Send::Accept),
            marks_refused: Cell::new(0),
            unretained: Default::default(),
            runtime_requests: vec![],
            rpc_fail: None,
            pruned_below: None,
            bad_metadata: false,
            calls: vec![],
            reads: vec![],
        };
        h.confirm_to(START - 1);
        let digest = tx(&h.db, |sql| {
            install_target(sql, &h.cfg, &h.trusted, &bundle_json(), T0)
        })
        .unwrap()
        .unwrap();
        assert!(digest.starts_with("0x") || !digest.is_empty());
        h
    }

    fn ctx(&self) -> Ctx<'_> {
        Ctx {
            cfg: &self.cfg,
            trusted: &self.trusted,
            runtime: &self.runtime,
            keys: &self.keys,
        }
    }

    /// Retains and RPC-confirms every finalized block up to `n`.
    fn confirm_to(&self, n: u64) {
        for b in self
            .chain
            .states
            .keys()
            .copied()
            .filter(|b| *b <= n && !self.unretained.contains(b))
        {
            self.db
                .conn
                .execute(
                    "INSERT OR IGNORE INTO finalized_blocks (block_number, block_hash, source, applied_at_ms) VALUES (?1, ?2, 'webhook', 0)",
                    rusqlite::params![b as i64, chain_hash(b).to_hex()],
                )
                .unwrap();
        }
        self.db
            .conn
            .execute(
                "INSERT INTO rpc_confirmed (id, block_number, block_hash, basis, updated_at_ms) VALUES (1, ?1, ?2, 'rpc_head', 0)
                 ON CONFLICT(id) DO UPDATE SET block_number = excluded.block_number, block_hash = excluded.block_hash",
                rusqlite::params![n as i64, chain_hash(n).to_hex()],
            )
            .unwrap();
        self.db
            .conn
            .execute(
                "INSERT INTO finalized_cursor (id, block_number, block_hash, updated_at_ms) VALUES (1, ?1, ?2, 0)
                 ON CONFLICT(id) DO UPDATE SET block_number = excluded.block_number, block_hash = excluded.block_hash",
                rusqlite::params![n as i64, chain_hash(n).to_hex()],
            )
            .unwrap();
    }

    fn tick(&self, ms: u64) {
        self.now.set(self.now.get() + ms);
    }

    fn plan(&self, holder: &str) -> Option<SignPlan> {
        tx(&self.db, |sql| {
            plan(sql, &self.ctx(), holder, self.now.get())
        })
        .unwrap()
    }

    fn outcome(&mut self, p: &SignPlan) -> Option<SignOutcome> {
        let step = match p {
            SignPlan::RuntimeVersion { .. } => "runtime_version",
            SignPlan::Metadata { .. } => "metadata",
            SignPlan::Read { .. } => "read",
            SignPlan::Block { .. } => "block",
            SignPlan::Submit { .. } => "submit",
        };
        let at = match p {
            SignPlan::RuntimeVersion { at, .. }
            | SignPlan::Metadata { at, .. }
            | SignPlan::Read { at, .. }
            | SignPlan::Block { at, .. } => at.number,
            SignPlan::Submit { .. } => self.chain.head,
        };
        self.calls.push((step, at));
        if let SignPlan::Read { read, .. } = p {
            let purpose = serde_json::to_value(read).unwrap()["purpose"]
                .as_str()
                .unwrap()
                .to_owned();
            self.reads.push((purpose, at));
        }
        if let (SignPlan::Read { at, .. }, Some(pruned)) = (p, self.pruned_below)
            && at.number < pruned
        {
            return Some(SignOutcome::Read { result: Err(RpcError::Remote) });
        }
        if let Some((s, e, n)) = self.rpc_fail
            && s == step
            && n > 0
        {
            self.rpc_fail = (n > 1).then_some((s, e, n - 1));
            return Some(match p {
                SignPlan::RuntimeVersion { .. } => SignOutcome::RuntimeVersion { result: Err(e) },
                SignPlan::Metadata { .. } => SignOutcome::Metadata { result: Err(e) },
                SignPlan::Read { .. } => SignOutcome::Read { result: Err(e) },
                SignPlan::Block { .. } => SignOutcome::Block { result: Err(e) },
                SignPlan::Submit { .. } => unreachable!(),
            });
        }
        Some(match p {
            SignPlan::RuntimeVersion { at, .. } => {
                self.runtime_requests.push(("runtime_version", at.number));
                SignOutcome::RuntimeVersion {
                    result: Ok((self.chain.code_spec(at.number), 1)),
                }
            }
            SignPlan::Metadata { at, .. } => {
                self.runtime_requests.push(("metadata", at.number));
                SignOutcome::Metadata {
                    result: Ok(if self.bad_metadata {
                        "0x011001020304".to_owned()
                    } else {
                        self.chain.metadata_hex(at.number)
                    }),
                }
            }
            SignPlan::Read { at, keys, .. } => SignOutcome::Read {
                result: Ok(self.chain.read(at.number, keys)),
            },
            SignPlan::Block { at, .. } => SignOutcome::Block {
                result: Ok(self.chain.bodies[&at.number].clone()),
            },
            SignPlan::Submit {
                claim,
                op_id,
                attempt,
                extrinsic,
                ..
            } => {
                let marked = tx(&self.db, |sql| {
                    mark(sql, claim, op_id, *attempt, self.now.get())
                })
                .unwrap();
                if !marked {
                    self.marks_refused.set(self.marks_refused.get() + 1);
                    return None;
                }
                let bytes = hex::decode(extrinsic.trim_start_matches("0x")).unwrap();
                let hash = format!("0x{}", hex::encode(sp_hash(&bytes)));
                match self.send.get() {
                    Send::Accept => {
                        self.chain.accepted.push(bytes.clone());
                        self.chain.pool.push(bytes);
                        SignOutcome::Submit {
                            result: SubmitOutcome::Accepted { hash },
                        }
                    }
                    Send::AcceptReportUncertain => {
                        self.chain.accepted.push(bytes.clone());
                        self.chain.pool.push(bytes);
                        SignOutcome::Submit {
                            result: SubmitOutcome::Uncertain {
                                reason: "timeout".into(),
                            },
                        }
                    }
                    Send::Lose => SignOutcome::Submit {
                        result: SubmitOutcome::Uncertain {
                            reason: "transport".into(),
                        },
                    },
                    Send::AcceptThenDie => {
                        self.chain.accepted.push(bytes.clone());
                        self.chain.pool.push(bytes);
                        return None;
                    }
                    Send::Reject => SignOutcome::Submit {
                        result: SubmitOutcome::Rejected {
                            code: 1010,
                            message: "Invalid Transaction: Custom error: 6".into(),
                        },
                    },
                }
            }
        })
    }

    /// One Worker invocation: at most `rounds` steps.
    fn invoke(&mut self, holder: &str, rounds: usize) -> usize {
        let mut plan = self.plan(holder);
        let mut done = 0;
        while let Some(p) = plan.take() {
            if done == rounds {
                break;
            }
            done += 1;
            let Some(outcome) = self.outcome(&p) else {
                break;
            };
            let more = done < rounds;
            let step = tx(&self.db, |sql| {
                on_result_more(sql, &self.ctx(), p.claim(), &outcome, more, self.now.get())
            })
            .unwrap();
            plan = step.plan;
        }
        done
    }

    /// One block: the chain produces it, the journal confirms it (one block
    /// of finality lag), and one delivery invocation runs.
    fn block(&mut self) {
        self.chain.produce();
        self.confirm_to(self.chain.head);
        self.tick(12_000);
        let holder = format!("delivery:{}", self.chain.head);
        self.invoke(&holder, 8);
    }

    /// `n` blocks with no confirmation (the watermark advances per minute
    /// live), then one confirmation and one invocation.
    fn jump(&mut self, n: u64) {
        for _ in 0..n {
            self.chain.produce();
            self.tick(12_000);
        }
        self.confirm_to(self.chain.head);
        let holder = format!("tick:{}", self.chain.head);
        self.invoke(&holder, 8);
    }

    /// `n` blocks, then one confirmation and a delivery-sized invocation of
    /// `rounds` steps.
    fn jump_rounds(&mut self, n: u64, rounds: usize) {
        for _ in 0..n {
            self.chain.produce();
            self.tick(12_000);
        }
        self.confirm_to(self.chain.head);
        let holder = format!("delivery:{}", self.chain.head);
        self.invoke(&holder, rounds);
    }

    fn counter(&self, name: &str) -> u64 {
        tx(&self.db, journal::counters)
            .unwrap()
            .into_iter()
            .find(|(k, _)| k == name)
            .map_or(0, |(_, v)| v)
    }

    fn blocks(&mut self, n: u64) {
        for _ in 0..n {
            self.block();
        }
    }

    fn status(&self) -> SigningStatus {
        tx(&self.db, |sql| status(sql, &self.cfg, 10, self.now.get())).unwrap()
    }

    fn resume(&self) {
        assert!(tx(&self.db, |sql| resume(sql, self.now.get())).unwrap());
    }

    fn ops(&self) -> Vec<OpStatus> {
        let mut ops = self.status().ops;
        ops.reverse();
        ops
    }

    fn set_reveal_row(&mut self) {
        if let Some(row) = self.ops().last().and_then(|o| o.row.clone()) {
            let summary: RowSummary = serde_json::from_value(row).unwrap();
            self.chain.reveal_row = summary.entries;
        }
    }

    /// Runs blocks until `pred` holds, at most `limit` blocks.
    fn until(&mut self, limit: u64, pred: impl Fn(&H) -> bool) {
        for _ in 0..limit {
            if pred(self) {
                return;
            }
            self.block();
            self.set_reveal_row();
        }
        assert!(
            pred(self),
            "condition not reached within {limit} blocks: {:#?}",
            self.status()
        );
    }
}

fn sp_hash(bytes: &[u8]) -> [u8; 32] {
    // blake2-256, as the node reports extrinsic hashes; compare through the
    // journal's own hash of the same bytes.
    let rt = ChainRuntime::parse(METADATA, 486, 1).unwrap();
    let decoded = rt.core().decode_extrinsic(bytes, true).unwrap();
    let h = chain::as_str(field(&decoded, "extrinsic_hash").unwrap())
        .unwrap()
        .to_owned();
    hex::decode(h.trim_start_matches("0x"))
        .unwrap()
        .try_into()
        .unwrap()
}

fn committed(h: &H) -> bool {
    h.ops()
        .iter()
        .any(|o| o.state == "committed" && o.readback.as_deref() == Some("ok"))
}

// ------------------------------------------------------------------ tests

#[test]
fn signing_starts_paused_and_nothing_is_signed_or_read_until_resumed() {
    let mut h = H::new();
    h.blocks(30);
    let s = h.status();
    assert_eq!(
        (s.state.as_str(), s.reason.as_deref()),
        ("paused", Some("operator_hold"))
    );
    assert!(s.ops.is_empty());
    assert_eq!(
        h.keys.reads.get(),
        0,
        "the hotkey secret is never read while paused"
    );
    assert!(h.chain.accepted.is_empty());
}

#[test]
fn one_commit_per_epoch_with_finalized_readback_and_observed_reveal() {
    let mut h = H::new();
    h.resume();
    h.until(80, committed);
    let ops = h.ops();
    assert_eq!(ops.len(), 1);
    let first = &ops[0];
    assert!(first.op_id.starts_with(COMMIT_OP_PREFIX));
    assert_eq!(first.epoch, 6);
    assert_eq!(first.attempts.len(), 1);
    let a = &first.attempts[0];
    assert_eq!((a.state.as_str(), a.sends, a.nonce), ("included", 1, 7));
    assert_eq!(a.dispatch.as_deref(), Some("success"));
    assert_eq!(h.chain.included.len(), 1);
    // The row is the protocol's v0 row for the fake chain's registrations.
    let row: RowSummary = serde_json::from_value(first.row.clone().unwrap()).unwrap();
    assert!(row.entries.iter().all(|(_, w)| *w > 0));
    assert_eq!(row.entries.iter().map(|e| e.1).max(), Some(65535));
    assert_eq!(row.rule, "testnet-reward-v0");
    // The reveal at the next epoch boundary is observed with matching weights.
    h.until(400, |h| h.ops()[0].state == "revealed");
    let r = &h.ops()[0];
    assert_eq!(r.reveal_weights_match, Some(true));
    assert_eq!(
        Some(r.reveal_block.unwrap()),
        h.chain.reveals.first().copied()
    );
    assert_eq!(
        r.reveal_block, r.predicted_reveal_block,
        "reveal at the predicted block"
    );
    // The next epoch gets its own operation, identity and single commit.
    h.until(200, |h| h.ops().len() == 2 && committed(h));
    let ops = h.ops();
    assert_eq!(ops[1].epoch, 7);
    assert_ne!(ops[0].op_id, ops[1].op_id);
    assert_eq!(ops[1].attempts[0].nonce, 8);
    assert_eq!(h.chain.included.len(), 2);
    assert_eq!(
        h.chain.accepted.len(),
        2,
        "exactly one submission per epoch"
    );
    assert_eq!(
        h.keys.reads.get(),
        2,
        "the secret is read once per signature"
    );
}

#[test]
fn an_uncertain_response_is_reconciled_from_finalized_state_without_resending() {
    let mut h = H::new();
    h.resume();
    h.send.set(Send::AcceptReportUncertain);
    h.until(80, committed);
    let a = &h.ops()[0].attempts[0];
    assert_eq!(a.submit_detail.as_deref(), Some("timeout"));
    assert_eq!((a.state.as_str(), a.sends), ("included", 1));
    assert_eq!(h.chain.accepted.len(), 1);
    assert_eq!(h.chain.included.len(), 1);
}

#[test]
fn eviction_after_submission_recovers_as_uncertain_and_never_resends() {
    let mut h = H::new();
    h.resume();
    h.send.set(Send::AcceptThenDie);
    h.until(40, |h| !h.chain.accepted.is_empty());
    // The invocation died after sending; the Durable Object is evicted too.
    h.runtime.forget();
    h.send.set(Send::Accept);
    // The dead holder's lease must lapse before anyone else may act.
    h.tick(h.cfg.lease_ms + 1);
    h.until(40, committed);
    let a = &h.ops()[0].attempts[0];
    assert_eq!(a.submit_detail.as_deref(), Some("abandoned_after_mark"));
    assert_eq!((a.state.as_str(), a.sends), ("included", 1));
    assert_eq!(h.chain.accepted.len(), 1, "no resubmission after eviction");
    assert_eq!(h.chain.included.len(), 1);
    let counters = tx(&h.db, journal::counters).unwrap();
    assert!(
        counters
            .iter()
            .any(|(k, v)| k == "sign_recovered_uncertain" && *v == 1)
    );
}

#[test]
fn a_lost_submission_waits_for_era_expiry_before_a_new_attempt() {
    let mut h = H::new();
    h.resume();
    h.send.set(Send::Lose);
    h.until(40, |h| {
        h.ops()
            .first()
            .is_some_and(|o| !o.attempts.is_empty() && o.attempts[0].state == "uncertain")
    });
    h.send.set(Send::Accept);
    let expiry = h.ops()[0].attempts[0].expiry;
    // No second attempt while the first could still be included.
    while h.chain.head + 1 < expiry {
        h.block();
        assert_eq!(
            h.ops()[0].attempts.len(),
            1,
            "re-signed before expiry at {}",
            h.chain.head
        );
    }
    h.until(40, committed);
    let ops = h.ops();
    let attempts = &ops[0].attempts;
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].state, "expired");
    assert_eq!(attempts[1].state, "included");
    assert_eq!(
        attempts[0].nonce, attempts[1].nonce,
        "the unconsumed nonce is reused"
    );
    assert_eq!(h.chain.included.len(), 1);
}

#[test]
fn a_stale_invocation_is_fenced_before_it_can_send() {
    let mut h = H::new();
    h.resume();
    // Drive until a Submit step is issued, then stall that holder.
    let mut stale = None;
    for _ in 0..40 {
        h.chain.produce();
        h.confirm_to(h.chain.head);
        h.tick(12_000);
        let mut plan = h.plan("stale");
        while let Some(p) = plan.take() {
            if matches!(p, SignPlan::Submit { .. }) {
                stale = Some(p);
                break;
            }
            let o = h.outcome(&p).unwrap();
            plan = tx(&h.db, |sql| {
                on_result(sql, &h.ctx(), p.claim(), &o, h.now.get())
            })
            .unwrap()
            .plan;
        }
        if stale.is_some() {
            break;
        }
    }
    let stale = stale.expect("a submit step");
    // Its lease lapses; a later delivery takes the claim and sends.
    h.tick(h.cfg.lease_ms + 1);
    h.until(20, |h| !h.chain.accepted.is_empty());
    // The stale holder wakes: its mark and its outcome are refused.
    let SignPlan::Submit {
        claim,
        op_id,
        attempt,
        ..
    } = &stale
    else {
        unreachable!()
    };
    assert!(!tx(&h.db, |sql| mark(sql, claim, op_id, *attempt, h.now.get())).unwrap());
    let late = SignOutcome::Submit {
        result: SubmitOutcome::Accepted {
            hash: "0x00".into(),
        },
    };
    assert!(
        tx(&h.db, |sql| on_result(
            sql,
            &h.ctx(),
            claim,
            &late,
            h.now.get()
        ))
        .unwrap()
        .fenced
    );
    h.until(40, committed);
    assert_eq!(h.chain.accepted.len(), 1);
    assert_eq!(h.ops()[0].attempts[0].sends, 1);
}

#[test]
fn duplicate_wakeups_take_one_claim() {
    let mut h = H::new();
    h.resume();
    h.chain.produce();
    h.confirm_to(h.chain.head);
    let first = h.plan("a");
    assert!(first.is_some());
    assert_eq!(
        h.plan("b"),
        None,
        "a concurrent invocation skips the held claim"
    );
    assert_eq!(h.plan("a-again"), None);
    assert_eq!(
        h.counter("sign_claim_held_skips"),
        2,
        "each skipped wakeup is counted"
    );
}

#[test]
fn an_existing_chain_commit_refuses_a_second_one() {
    let mut h = H::new();
    // Another journal (or an earlier life of this one) already committed for
    // epoch 6 with this hotkey.
    h.chain.queue.insert(
        6,
        vec![Entry {
            who: alice().public(),
            block: START - 2,
            ciphertext: vec![1, 2, 3],
            round: 9,
        }],
    );
    h.chain.snapshot(START - 1, vec![]);
    h.resume();
    h.blocks(40);
    let ops = h.ops();
    assert_eq!(ops[0].state, "failed");
    assert_eq!(ops[0].detail.as_deref(), Some("commit_already_present"));
    assert!(ops[0].attempts.is_empty());
    assert!(h.chain.accepted.is_empty());
}

#[test]
fn disabled_commit_reveal_pauses_new_commitments_without_fallback() {
    let mut h = H::new();
    h.chain.cr_enabled = false;
    h.chain.snapshot(START - 1, vec![]);
    h.resume();
    h.blocks(40);
    let s = h.status();
    assert_eq!(
        (s.state.as_str(), s.reason.as_deref()),
        ("paused", Some("commit_reveal_disabled"))
    );
    assert!(h.chain.accepted.is_empty());
}

#[test]
fn a_pool_rejection_fails_the_epoch_without_retrying() {
    let mut h = H::new();
    h.resume();
    h.send.set(Send::Reject);
    h.blocks(60);
    let ops = h.ops();
    assert_eq!(ops.len(), 1);
    assert_eq!(
        (ops[0].state.as_str(), ops[0].detail.as_deref()),
        ("failed", Some("pool_rejected"))
    );
    assert_eq!(ops[0].attempts.len(), 1);
}

#[test]
fn a_nonce_consumed_by_foreign_bytes_is_a_fault() {
    let mut h = H::new();
    h.resume();
    h.send.set(Send::Lose);
    h.until(40, |h| {
        h.ops().first().is_some_and(|o| !o.attempts.is_empty())
    });
    h.chain.foreign_next = true;
    h.blocks(20);
    let s = h.status();
    assert_eq!(s.reason.as_deref(), Some("fault:foreign_nonce_use"));
    assert_eq!(h.ops()[0].attempts[0].state, "displaced");
    assert!(
        !tx(&h.db, |sql| resume(sql, h.now.get())).unwrap(),
        "a fault needs an operator clear"
    );
    assert!(tx(&h.db, |sql| clear_fault(sql, h.now.get())).unwrap());
    assert!(tx(&h.db, |sql| resume(sql, h.now.get())).unwrap());
}

fn submitted(h: &H) -> bool {
    h.ops()
        .first()
        .is_some_and(|o| o.attempts.iter().any(|a| a.state == "submitted"))
}

fn failure<'a>(s: &'a SigningStatus, prefix: &str) -> Option<&'a FailureStatus> {
    s.failures.iter().find(|f| f.subject.starts_with(prefix))
}

#[test]
fn the_genuinely_different_runtime_misdecodes_the_other_ones_events() {
    // Sanity for the fake: a block's events decode only with its writer.
    let f = Fake::new();
    let key = f.runtime(486).storage_key("System", "Events", &[]).unwrap();
    let mut st = BTreeMap::new();
    put(
        f.runtime(486),
        &mut st,
        "System",
        "Events",
        &[],
        Value::List(vec![record(
            Some(1),
            "System",
            "ExtrinsicSuccess",
            dispatch_info(),
        )]),
    );
    let raw = st[&key].clone();
    let ok = f.runtime(486).events(Some(&raw)).unwrap();
    assert_eq!(ok[0].name, "ExtrinsicSuccess");
    if let Ok(wrong) = f.runtime(487).events(Some(&raw)) {
        assert_ne!(wrong[0].name, "ExtrinsicSuccess");
    }
}

#[test]
fn an_upgrade_enacted_in_the_inclusion_block_decodes_it_with_its_writer() {
    let mut h = H::new();
    h.resume();
    h.until(40, submitted);
    let start_learns = h.runtime_requests.len();
    // The next block both includes the attempt and enacts runtime 487: its
    // post-state code is 487, its events and LastRuntimeUpgrade are 486's.
    h.chain.upgrade_next(487);
    let preflight = h.ops()[0].preflight_block.unwrap();
    let enact = h.chain.head + 1;
    // The watermark then jumps by seven blocks, so the nonce search probes
    // the enactment block itself as a midpoint before reading its body and
    // events.
    h.jump(7);
    assert_eq!(h.chain.included[0].0, enact);
    h.until(20, committed);
    let o = &h.ops()[0];
    let a = &o.attempts[0];
    assert_eq!(
        (a.state.as_str(), a.dispatch.as_deref(), a.included_block),
        ("included", Some("success"), Some(enact))
    );
    assert_eq!(a.spec_version, 486);
    assert_eq!(o.readback.as_deref(), Some("ok"));
    // The search probed the enactment block, and no runtime version or
    // metadata was ever asked of the block it decoded with an older writer.
    assert!(
        h.calls.contains(&("read", enact)),
        "probe at the enactment block: {:?}",
        h.calls
    );
    assert!(
        !h.runtime_requests[start_learns..]
            .iter()
            .any(|(_, b)| *b == enact),
        "{:?}",
        h.runtime_requests
    );
    // 487 was learned once, at a parent whose post-state code is 487.
    let learned: Vec<_> = h.runtime_requests[start_learns..].to_vec();
    assert_eq!(learned.len(), 2, "{learned:?}");
    assert!(learned.iter().all(|(_, b)| h.chain.code_spec(*b) == 487));
    let s = h.status();
    assert_eq!(s.runtime, Some((487, 1)));
    assert_eq!(s.health, "ok", "{s:#?}");
    assert!(s.failures.is_empty());
    assert_eq!(
        s.runtimes.iter().map(|r| r.0).collect::<Vec<_>>(),
        vec![486, 487]
    );
    assert!(preflight < enact);
    // The next epoch signs with the new runtime.
    h.until(400, |h| {
        h.ops().len() == 2 && committed(h) && h.ops()[1].state != "preparing"
    });
    assert_eq!(h.ops()[1].attempts[0].spec_version, 487);
    assert_eq!(h.ops()[1].attempts[0].dispatch.as_deref(), Some("success"));
}

#[test]
fn an_upgrade_enacted_in_the_reveal_block_decodes_its_events_with_its_writer() {
    let mut h = H::new();
    h.resume();
    h.until(80, committed);
    let predicted = h.ops()[0].predicted_reveal_block.unwrap();
    // The reveal block also enacts 487; checks come every five blocks.
    while h.chain.head + 1 < predicted {
        h.block();
    }
    h.chain.upgrade_next(487);
    h.set_reveal_row();
    for _ in 0..12 {
        h.jump(5);
        if h.ops()[0].state == "revealed" {
            break;
        }
    }
    let r = &h.ops()[0];
    assert_eq!(r.state, "revealed", "{:#?}", h.status());
    assert_eq!(r.reveal_block, Some(predicted));
    assert_eq!(r.reveal_weights_match, Some(true));
    assert!(!h.runtime_requests.iter().any(|(_, b)| *b == predicted));
    assert!(h.status().failures.is_empty());
}

#[test]
fn a_runtime_upgrade_while_reconciling_learns_the_new_runtime_once() {
    let mut h = H::new();
    h.resume();
    h.block();
    assert_eq!(h.status().runtime, Some((486, 1)));
    // The runtime upgrades while the first attempt is reconciled.
    h.chain.upgrade_next(487);
    h.until(80, committed);
    assert_eq!(h.status().runtime, Some((487, 1)));
    h.until(400, |h| {
        h.ops().len() == 2 && committed(h) && h.ops()[1].state != "preparing"
    });
    assert_eq!(h.ops()[1].attempts[0].spec_version, 487);
    assert!(h.counter("sign_runtime_changes") >= 1);
    let rows: i64 =
        h.db.conn
            .query_row("SELECT COUNT(*) FROM runtime_cache", [], |r| r.get(0))
            .unwrap();
    assert_eq!(rows, 2);
    // Each runtime's metadata was fetched exactly once.
    assert_eq!(
        h.runtime_requests
            .iter()
            .filter(|(s, _)| *s == "metadata")
            .count(),
        2
    );
}

#[test]
fn undecodable_events_settle_into_a_diagnostic_and_the_next_epoch_commits() {
    let mut h = H::new();
    h.resume();
    h.until(40, submitted);
    let inclusion = h.chain.head + 1;
    h.chain.corrupt_events.insert(inclusion);
    h.until(120, |h| h.ops()[0].state == "committed");
    let o = &h.ops()[0];
    assert!(
        o.diagnostic
            .as_deref()
            .is_some_and(|d| d.starts_with("events_unverifiable")),
        "{o:#?}"
    );
    assert_eq!(o.attempts[0].dispatch.as_deref(), Some("unverifiable"));
    // The read-back still verified the commit in the chain's own queue.
    h.until(5, |h| h.ops()[0].readback.is_some());
    assert_eq!(h.ops()[0].readback.as_deref(), Some("ok"));
    let s = h.status();
    assert_eq!(s.nonce_floor, 8, "the nonce floor still advanced");
    assert_eq!(s.state, "active");
    let f = failure(&s, "attempt").unwrap();
    assert!(f.settled && f.failures == DIAG_CAP);
    assert_eq!(h.counter("sign_step_failures"), DIAG_CAP);
    // Exactly one events read per failure: no retry loop.
    assert_eq!(
        h.reads
            .iter()
            .filter(|(p, b)| p == "events" && *b == inclusion)
            .count() as u64,
        DIAG_CAP
    );
    h.until(400, |h| h.ops().len() == 2 && committed(h));
    assert_eq!(h.ops()[1].attempts[0].nonce, 8);
}

/// Audit B4: after a long outage the endpoint no longer serves state at the
/// blocks the live attempt's inclusion search probes (a pruning node), so
/// every nonce probe and the events read there answer a JSON-RPC error.
/// Before, that was retried forever behind the signer-wide backoff, holding
/// every later commit with no fault. Now each answer counts against the
/// attempt (a visible failing subject), the search goes on by block bodies,
/// which need no state, the inclusion is found, its events settle as
/// unverifiable, and the next epoch commits.
#[test]
fn pruned_history_resolves_the_live_attempt_by_block_bodies() {
    let mut h = H::new();
    h.resume();
    h.send.set(Send::AcceptReportUncertain);
    h.until(40, |h| {
        h.ops()
            .first()
            .is_some_and(|o| o.attempts.iter().any(|a| a.state == "uncertain"))
    });
    // Forty blocks pass unconfirmed; then the node serves no state below
    // the head the journal confirms next.
    h.pruned_below = Some(h.chain.head + 40);
    h.jump(40);
    let s = h.status();
    let f = failure(&s, "attempt").expect("the refused probe is a failing subject");
    assert_eq!(f.code, "state_unavailable");
    assert_eq!(s.health, "failing", "visible in /status");
    assert_eq!(h.counter("sign_search_scans"), 1);
    h.until(200, |h| h.ops()[0].state == "committed");
    let o = &h.ops()[0];
    assert_eq!(o.attempts[0].state, "included", "{o:#?}");
    assert_eq!(o.attempts[0].sends, 1, "never resent");
    assert!(
        o.diagnostic
            .as_deref()
            .is_some_and(|d| d.starts_with("events_unverifiable:state_unavailable")),
        "{o:#?}"
    );
    assert_eq!(h.status().nonce_floor, 8, "the nonce floor advanced past it");
    h.until(400, |h| h.ops().len() == 2 && h.ops()[1].state == "committed");
    assert_eq!(h.ops()[1].attempts[0].nonce, 8);
}

/// Audit B4, fix round 1: when the bodies are gone too, the attempt (whose
/// nonce the head showed consumed) settles only after the endpoint refused
/// it through `MAX_BACKOFF_BLOCKS` of chain progress, and never pauses the
/// signer: its inclusion is `inclusion_unverifiable`, the nonce floor moves
/// past its consumed nonce, its operation fails, and the next epoch commits
/// with no operator clear.
#[test]
fn an_unsearchable_attempt_settles_without_pausing_the_signer() {
    assert_eq!(UNAVAILABLE_SETTLE_FAILURES, 7, "backoffs 1+2+4+8+16+32 span the 60-block maximum");
    let mut h = H::new();
    h.resume();
    h.send.set(Send::AcceptReportUncertain);
    h.until(40, |h| {
        h.ops()
            .first()
            .is_some_and(|o| o.attempts.iter().any(|a| a.state == "uncertain"))
    });
    h.pruned_below = Some(h.chain.head + 40);
    h.jump(40);
    h.rpc_fail = Some(("block", RpcError::Remote, 1_000));
    let start = h.chain.head;
    h.until(200, |h| h.ops()[0].state == "failed");
    assert!(h.chain.head >= start + 63, "settled only after the refusals spanned the maximum backoff");
    let o = &h.ops()[0];
    assert_eq!(o.detail.as_deref(), Some("inclusion_unverifiable"), "{o:#?}");
    // Fix round 2: labelled as unknown, never as a plain failed commit.
    assert_eq!(o.inclusion, Some("unknown"));
    assert_eq!(o.diagnostic.as_deref(), Some("inclusion_unverifiable:state_unavailable"));
    assert_eq!(o.attempts[0].dispatch.as_deref(), Some("unverifiable"));
    assert_eq!(o.attempts[0].sends, 1, "never resent");
    let s = h.status();
    assert_eq!(s.state, "active", "the signer is not paused: {:?}", s.reason);
    assert_eq!(s.nonce_floor, 8, "the consumed nonce is behind the floor");
    let f = failure(&s, "attempt").unwrap();
    assert!(f.settled && f.failures == UNAVAILABLE_SETTLE_FAILURES, "{f:?}");
    assert_eq!(h.counter("signing_faults"), 0);
    assert_eq!(h.counter("sign_inclusions_unverifiable"), 1);
    // The endpoint serves bodies again: the next epoch commits on the next
    // nonce, with no clear.
    h.rpc_fail = None;
    h.until(400, |h| h.ops().len() == 2 && h.ops()[1].state == "committed");
    assert_eq!(h.ops()[1].attempts[0].nonce, 8);
}

/// Fix round 1 (B4): a short burst of JSON-RPC errors on the body scan (an
/// overloaded endpoint, not pruned history) is retried and recovers by
/// itself: the scan goes on, the inclusion is found, nothing settles and no
/// operator clear is needed.
#[test]
fn a_burst_of_endpoint_errors_during_the_body_scan_recovers_by_itself() {
    let mut h = H::new();
    h.resume();
    h.send.set(Send::AcceptReportUncertain);
    h.until(40, |h| {
        h.ops()
            .first()
            .is_some_and(|o| o.attempts.iter().any(|a| a.state == "uncertain"))
    });
    h.pruned_below = Some(h.chain.head + 40);
    h.jump(40);
    // `DIAG_CAP` refused bodies after the refused nonce probe: one more
    // failure than used to fault the signer, fewer than the settle span.
    h.rpc_fail = Some(("block", RpcError::Remote, DIAG_CAP as u32));
    h.until(300, |h| h.ops()[0].state == "committed");
    let o = &h.ops()[0];
    assert_eq!(o.attempts[0].state, "included", "{o:#?}");
    let s = h.status();
    assert_eq!(s.state, "active", "{:?}", s.reason);
    assert_eq!(h.counter("signing_faults"), 0);
    assert_eq!(h.counter("sign_inclusions_unverifiable"), 0);
    assert!(h.counter("sign_step_failures") > DIAG_CAP, "the burst was counted against the attempt");
    h.until(400, |h| h.ops().len() == 2 && h.ops()[1].state == "committed");
    assert_eq!(h.ops()[1].attempts[0].nonce, 8);
}

#[test]
fn a_failing_reveal_search_never_starves_the_next_commit() {
    let mut h = H::new();
    h.resume();
    h.until(80, committed);
    let predicted = h.ops()[0].predicted_reveal_block.unwrap();
    // The blocks around the reveal are unretained (as after a re-anchor), so
    // the queue search cannot finish; the next epoch's window opens while it
    // is failing.
    for b in predicted - 1..=predicted + 3 {
        h.unretained.insert(b);
    }
    h.until(400, |h| {
        h.ops().len() == 2 && h.ops()[1].state == "committed"
    });
    let first = &h.ops()[0];
    assert_eq!(first.state, "committed");
    // The next epoch committed while the reveal search was failing.
    assert_eq!(h.ops()[1].attempts[0].state, "included");
    h.until(200, |h| h.ops()[0].diagnostic.is_some());
    assert!(
        h.ops()[0]
            .diagnostic
            .as_deref()
            .is_some_and(|d| d == "reveal_unverifiable:block_hash_unretained"),
        "{:#?}",
        h.ops()[0]
    );
    let s = h.status();
    assert_eq!(s.health, "diagnostic");
    assert_eq!(s.state, "active");
}

#[test]
fn an_rpc_error_ends_the_round_and_backs_off() {
    let mut h = H::new();
    // Paused, the signer still learns the head runtime.
    h.block();
    assert_eq!(h.status().runtime, Some((486, 1)));
    h.resume();
    h.chain.produce();
    h.confirm_to(h.chain.head);
    h.tick(12_000);
    // The first read fails: the invocation stops after that one call.
    h.rpc_fail = Some(("read", RpcError::RateLimited, 1));
    let done = h.invoke("delivery:1", 8);
    let reads = h.calls.iter().filter(|c| c.0 == "read").count();
    assert_eq!((done, reads), (1, 1), "{:?}", h.calls);
    let s = h.status();
    assert_eq!(s.health, "backing_off");
    assert!(s.backoff_ms.is_some_and(|b| b >= 29_000));
    // Within the backoff nothing is planned and no claim is taken.
    h.tick(10_000);
    assert_eq!(h.plan("delivery:2"), None);
    assert!(h.counter("sign_backoff_skips") >= 1);
    h.tick(25_000);
    h.until(80, committed);
    assert_eq!(h.status().rpc_failures, 0);
}

#[test]
fn unparsable_metadata_backs_off_instead_of_refetching_every_round() {
    let mut h = H::new();
    h.bad_metadata = true;
    h.blocks(40);
    let fetched = h
        .runtime_requests
        .iter()
        .filter(|(s, _)| *s == "metadata")
        .count();
    // Backoff 1, 2, 4, 8, 16 blocks: at most six fetches in 40 blocks.
    assert!((1..=6).contains(&fetched), "{fetched}");
    let s = h.status();
    let f = failure(&s, "head").unwrap();
    assert_eq!(f.code, "metadata_unparsable");
    assert!(
        !f.settled,
        "the head runtime never settles; it keeps retrying"
    );
    assert!(s.last_error.is_some());
    // A served runtime is picked up after the backoff.
    h.bad_metadata = false;
    h.resume();
    h.until(120, committed);
}

#[test]
fn signing_has_its_own_share_of_the_rpc_budget() {
    let h = H::new();
    let mut cfg = h.cfg.clone();
    cfg.rpc_calls_per_minute = 12;
    cfg.sign_rpc_calls_per_minute = 8;
    let t = T0 + 1;
    for _ in 0..8 {
        assert!(tx(&h.db, |sql| reserve_sign_rpc(sql, &cfg, t)).unwrap());
    }
    assert!(!tx(&h.db, |sql| reserve_sign_rpc(sql, &cfg, t)).unwrap());
    // The heartbeat keeps its four.
    assert!(tx(&h.db, |sql| claims::reserve_rpc(sql, 4, 12, t)).unwrap());
    assert!(
        tx(&h.db, |sql| reserve_sign_rpc(
            sql,
            &cfg,
            t + claims::BUDGET_WINDOW_MS
        ))
        .unwrap()
    );
}

#[test]
fn reveal_checks_and_probes_are_logged() {
    let mut h = H::new();
    h.chain.reveal_delay = 13;
    h.resume();
    h.until(80, committed);
    h.until(400, |h| h.ops()[0].state == "revealed");
    let events: Vec<String> =
        h.db.conn
            .prepare("SELECT event, COALESCE(detail, '') FROM sign_log ORDER BY seq")
            .unwrap()
            .query_map([], |r| {
                Ok(format!(
                    "{} {}",
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
    assert!(
        events
            .iter()
            .any(|e| e.starts_with("reveal_check queued, next"))
    );
    assert!(
        events
            .iter()
            .any(|e| e.starts_with("reveal_check gone, search"))
    );
    assert!(
        events
            .iter()
            .any(|e| e == "reveal_probe queued" || e == "reveal_probe gone")
    );
}

#[test]
fn a_reveal_search_cut_short_by_the_step_limit_continues_from_its_bounds() {
    let mut h = H::new();
    h.chain.reveal_delay = 13;
    h.resume();
    h.until(80, committed);
    h.set_reveal_row();
    // Deliveries allow three steps; the search needs more than that.
    for _ in 0..200 {
        h.jump_rounds(1, 3);
        if h.ops()[0].state == "revealed" {
            break;
        }
    }
    let r = &h.ops()[0];
    assert_eq!(r.state, "revealed");
    assert_eq!(r.reveal_block, Some(r.predicted_reveal_block.unwrap() + 13));
    let gone: i64 = h
        .db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sign_log WHERE event = 'reveal_check' AND detail LIKE 'gone%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(gone, 1, "the search never restarted");
    assert_eq!(
        h.counter("sign_abandoned_steps"),
        0,
        "each round released its claim"
    );
}

#[test]
fn v4_storage_upgrades_with_its_signing_journal_intact() {
    const V4_ONLY: crate::sql::Schema = crate::sql::Schema {
        family: "chain_journal",
        migrations: {
            const M: &[crate::sql::Migration] = CHAIN_JOURNAL_SCHEMA.migrations;
            let (v4, _) = M.split_at(4);
            v4
        },
    };
    let db = Sqlite::open();
    tx(&db, |sql| migrate(sql, &V4_ONLY)).unwrap();
    db.conn
        .execute_batch(
            "UPDATE signer SET state = 'active', reason = NULL, nonce_floor = 4, runtime_spec = 486, runtime_tx = 1;
             INSERT INTO runtime_cache VALUES (486, 1, 'aa', '00', 8334588, '0x01', 5);
             INSERT INTO commit_ops (op_id, epoch, target, base_op, state, created_at_ms, updated_at_ms)
                VALUES ('op', 11, 't', 'b', 'committed', 1, 1);",
        )
        .unwrap();
    let report = tx(&db, |sql| migrate(sql, &CHAIN_JOURNAL_SCHEMA)).unwrap();
    assert_eq!((report.from, report.to), (4, 12));
    let s = tx(&db, signer).unwrap();
    assert_eq!(
        (s.state.as_str(), s.nonce_floor, s.runtime, s.runtime_block),
        ("active", 4, Some((486, 1)), Some(8334588))
    );
    assert_eq!((s.rpc_failures, s.backoff_until_ms), (0, None));
    let state: String = db
        .conn
        .query_row("SELECT state FROM commit_ops WHERE op_id = 'op'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(state, "committed");
}

#[test]
fn op_ids_are_per_epoch_and_stable() {
    let a = commit_op_id("slopninja:weights:v1:00", 6);
    assert_eq!(a, commit_op_id("slopninja:weights:v1:00", 6));
    assert_ne!(a, commit_op_id("slopninja:weights:v1:00", 7));
    assert_ne!(a, commit_op_id("slopninja:weights:v1:01", 6));
    assert!(a.starts_with(COMMIT_OP_PREFIX));
}

#[test]
fn status_is_content_free() {
    let mut h = H::new();
    h.resume();
    h.until(80, committed);
    let json = serde_json::to_string(&h.status()).unwrap();
    let ext = &h.chain.included[0].1;
    assert!(!json.contains(&hex::encode(ext)), "no signed bytes");
    assert!(!json.contains(&ALICE_SEED[2..]), "no secret");
    let ct = &h.chain.queue.values().next().unwrap()[0].ciphertext;
    assert!(!json.contains(&hex::encode(ct)), "no ciphertext");
}

#[test]
fn a_late_drand_pulse_is_found_by_searching_the_queue() {
    let mut h = H::new();
    h.chain.reveal_delay = 13;
    h.resume();
    h.until(80, committed);
    h.until(400, |h| h.ops()[0].state == "revealed");
    let r = &h.ops()[0];
    let predicted = r.predicted_reveal_block.unwrap();
    assert_eq!(r.reveal_block, Some(predicted + 13));
    assert_eq!(r.reveal_block, h.chain.reveals.first().copied());
    assert_eq!(r.reveal_weights_match, Some(true));
}

#[test]
fn a_commit_dropped_without_a_reveal_event_is_reported() {
    let mut h = H::new();
    h.chain.reveal_drop = true;
    h.resume();
    h.until(80, committed);
    h.until(400, |h| h.ops()[0].state == "failed");
    assert_eq!(h.ops()[0].detail.as_deref(), Some("reveal_dropped"));
}

/// A reward-v1 launch bundle: the burn row under a launch authorization.
fn launch_bundle_json(issued_epoch: u64) -> String {
    let chain = slop_ninja_protocol::ChainBinding {
        genesis: slop_ninja_protocol::TESTNET_GENESIS,
        netuid: NETUID,
        mechanism: 0,
    };
    let auth = slop_ninja_protocol::LaunchAuthorization::new(&chain, 1, issued_epoch).unwrap();
    let b = target::BundleV2 {
        schema: target::BUNDLE_SCHEMA_V2.into(),
        shortcut: "T12".into(),
        source: None,
        authorization: target::AuthorizationHex {
            kind: "launch".into(),
            authorization: hex::encode(sign(auth, &fx::authorization_key()).unwrap().to_bytes().unwrap()),
            renewal: None,
            carrier: None,
        },
        receipt_chunks: Vec::new(),
        carrier_receipt_chunks: Vec::new(),
    };
    serde_json::to_string(&b).unwrap()
}

/// The coordinator checks bundles against the target it holds active, never
/// one rebuilt from the bundle: once a cohort target has activated, the
/// launch bundle is refused at install and at every later re-verification,
/// and the held record never moves backwards.
#[test]
fn a_reward_v1_launch_bundle_is_refused_once_a_cohort_target_has_activated() {
    let h = H::new();
    tx(&h.db, |sql| install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(1), T0)).unwrap().unwrap();
    assert!(tx(&h.db, |sql| active_target(sql, &h.cfg, &h.trusted)).unwrap().unwrap().is_ok());
    // A cohort target activates at epoch 50 (the record a checked closure leaves).
    let activated = slop_ninja_protocol::ActiveTarget::restore(
        &slop_ninja_protocol::ChainBinding { genesis: slop_ninja_protocol::TESTNET_GENESIS, netuid: NETUID, mechanism: 0 },
        slop_ninja_protocol::Digest([0x5a; 32]),
        Some(50),
    )
    .unwrap();
    let launch = tx(&h.db, |sql| held_target(sql, &h.cfg)).unwrap();
    assert!(launch.is_launch_burn());
    tx(&h.db, |sql| record_held(sql, &launch, &activated, T0)).unwrap().unwrap();
    // The installed launch bundle no longer verifies, so nothing is signed.
    match tx(&h.db, |sql| active_target(sql, &h.cfg, &h.trusted)).unwrap().unwrap() {
        Err(refused) => assert_eq!(refused.code, "bundle_unverified"),
        Ok(_) => panic!("the launch bundle must not verify after an activation"),
    }
    // Installing it again is refused before anything is stored.
    let again = tx(&h.db, |sql| install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(60), T0)).unwrap().unwrap_err();
    assert_eq!(again.code, "bundle_unverified");
    assert!(again.detail.contains("launch authorization after activation"), "{}", again.detail);
    // The held record only moves forward.
    let held = tx(&h.db, |sql| held_target(sql, &h.cfg)).unwrap();
    assert_eq!(held, activated);
    assert_eq!(tx(&h.db, |sql| record_held(sql, &held, &launch, T0)).unwrap().unwrap_err().code, "held_target_regressed");
}

fn chain_601() -> slop_ninja_protocol::ChainBinding {
    slop_ninja_protocol::ChainBinding {
        genesis: slop_ninja_protocol::TESTNET_GENESIS,
        netuid: NETUID,
        mechanism: 0,
    }
}

fn logged(h: &H, event: &str) -> Vec<Option<String>> {
    h.db.conn
        .prepare("SELECT detail FROM sign_log WHERE event = ?1 ORDER BY seq")
        .unwrap()
        .query_map([event], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// A chain record restarted under the same trust roots (a named eval
/// authority, R_0 with no predecessor): the held cohort target refuses the
/// restarted launch bundle until the operator restarts it, with signing
/// paused, in one transaction that installs that bundle. A refused restart
/// changes nothing. Every obligation of the earlier target survives: its
/// committed operation is read back and revealed, nothing is sent twice, no
/// epoch takes a second operation, and no bundle of the earlier record
/// installs again.
#[test]
fn a_held_target_restart_installs_the_restarted_launch_bundle_and_keeps_every_obligation() {
    let mut h = H::new();
    tx(&h.db, |sql| {
        install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(1), T0)
    })
    .unwrap()
    .unwrap();
    h.resume();
    h.until(80, committed);
    let first = h.ops()[0].clone();
    assert_eq!(first.state, "committed");
    // The earlier chain's cohort target activated (the record a checked
    // closure leaves), the epoch before the restarted record's first: the
    // restarted chain's launch bundle is refused.
    let held_at = first.epoch - 1;
    let activated = slop_ninja_protocol::ActiveTarget::restore(
        &chain_601(),
        slop_ninja_protocol::Digest([0x5a; 32]),
        Some(held_at),
    )
    .unwrap();
    let launch = tx(&h.db, |sql| held_target(sql, &h.cfg)).unwrap();
    tx(&h.db, |sql| record_held(sql, &launch, &activated, T0))
        .unwrap()
        .unwrap();
    let from = first.epoch;
    let restarted = launch_bundle_json(from);
    let refused = tx(&h.db, |sql| {
        install_target(sql, &h.cfg, &h.trusted, &restarted, T0)
    })
    .unwrap()
    .unwrap_err();
    assert!(
        refused
            .detail
            .contains("launch authorization after activation"),
        "{}",
        refused.detail
    );
    let before = |h: &H| {
        (
            tx(&h.db, |sql| held_target(sql, &h.cfg)).unwrap(),
            tx(&h.db, |sql| {
                Ok::<_, StoreError>(
                    sql.exec(
                        "SELECT target, active FROM sign_targets ORDER BY target",
                        &[],
                    )?
                    .iter()
                    .map(|r| (text(r, 0).unwrap().to_owned(), opt_u(r, 1)))
                    .collect::<Vec<_>>(),
                )
            })
            .unwrap(),
            h.counter("held_target_resets"),
        )
    };
    let unchanged = before(&h);
    // Refused while signing is active, and it changes nothing: never a probe.
    let r = tx(&h.db, |sql| {
        restart_target(sql, &h.cfg, &h.trusted, &restarted, from, T0)
    })
    .unwrap()
    .unwrap_err();
    assert_eq!(r.code, "signing_active");
    assert_eq!(before(&h), unchanged);
    tx(&h.db, |sql| pause(sql, "operator_hold", T0)).unwrap();
    // A launch bundle issued before the restarted record's first epoch, or
    // anything but a launch bundle, is refused, and nothing changes.
    let r = tx(&h.db, |sql| {
        restart_target(sql, &h.cfg, &h.trusted, &restarted, from + 1, T0)
    })
    .unwrap()
    .unwrap_err();
    assert_eq!(r.code, "bundle_before_restarted_record");
    let r = tx(&h.db, |sql| {
        restart_target(sql, &h.cfg, &h.trusted, "{}", from, T0)
    })
    .unwrap()
    .unwrap_err();
    assert_eq!(r.code, "bundle_malformed");
    // A floor at or below the held target's activation (a low or mistyped
    // from_epoch) would let the earlier record's bundles back in: refused,
    // even with a launch bundle issued at it.
    for (low, bundle) in [(0, "{}".to_owned()), (held_at, launch_bundle_json(held_at))] {
        let r = tx(&h.db, |sql| {
            restart_target(sql, &h.cfg, &h.trusted, &bundle, low, T0)
        })
        .unwrap()
        .unwrap_err();
        assert_eq!(r.code, "restart_before_held_record", "{low}");
    }
    assert_eq!(before(&h), unchanged);
    // Paused, with the restarted record's launch bundle: one transaction.
    let r = tx(&h.db, |sql| {
        restart_target(sql, &h.cfg, &h.trusted, &restarted, from, T0)
    })
    .unwrap()
    .unwrap();
    assert_eq!(
        r.previous,
        Some((
            slop_ninja_protocol::Digest([0x5a; 32]).to_string(),
            Some(held_at)
        ))
    );
    assert_eq!(r.from_epoch, from);
    assert!(r.replaced.is_some());
    assert!(
        tx(&h.db, |sql| held_target(sql, &h.cfg))
            .unwrap()
            .is_launch_burn()
    );
    let active = tx(&h.db, |sql| active_target(sql, &h.cfg, &h.trusted))
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(active.digest, r.installed);
    // The record set aside is kept in the history, never deleted.
    let history = tx(&h.db, |sql| {
        Ok::<_, StoreError>(
            sql.exec(
                "SELECT held_target, held_activated_epoch, from_epoch, launch_issued_epoch FROM sign_held_target_resets",
                &[],
            )?
            .iter()
            .map(|r| (opt_t(r, 0), opt_u(r, 1), opt_u(r, 2), opt_u(r, 3)))
            .collect::<Vec<_>>(),
        )
    })
    .unwrap();
    assert_eq!(
        history,
        vec![(
            Some(slop_ninja_protocol::Digest([0x5a; 32]).to_string()),
            Some(held_at),
            Some(from),
            Some(from)
        )]
    );
    assert_eq!(
        logged(&h, "held_target_reset"),
        vec![Some(format!(
            "{}@{held_at} from {from}",
            slop_ninja_protocol::Digest([0x5a; 32])
        ))]
    );
    assert_eq!(h.counter("held_target_resets"), 1);
    // The earlier record's launch bundle, signed by the same keys, never
    // installs again after the restart.
    let old = tx(&h.db, |sql| {
        install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(1), T0)
    })
    .unwrap()
    .unwrap_err();
    assert_eq!(old.code, "bundle_before_restarted_record");
    // A T12 (v1) bundle carries no epoch to hold to the floor: none installs
    // after a restart.
    let v1 = tx(&h.db, |sql| {
        install_target(sql, &h.cfg, &h.trusted, &bundle_json(), T0)
    })
    .unwrap()
    .unwrap_err();
    assert_eq!(v1.code, "v1_after_restart");
    // The earlier commit is still revealed while paused.
    h.until(400, |h| h.ops()[0].state == "revealed");
    assert_eq!(h.ops().len(), 1, "nothing new is prepared while paused");
    h.resume();
    h.until(400, |h| h.ops().len() == 2 && committed(h));
    let ops = h.ops();
    assert!(
        ops[1].epoch > ops[0].epoch,
        "the restarted target's first commit takes a later epoch"
    );
    let epochs: std::collections::BTreeSet<u64> = ops.iter().map(|o| o.epoch).collect();
    assert_eq!(epochs.len(), ops.len(), "one operation per epoch");
    let attempts: usize = ops.iter().map(|o| o.attempts.len()).sum();
    assert_eq!(
        (attempts, h.chain.accepted.len()),
        (2, 2),
        "nothing is sent twice"
    );
    // A later restart cannot start before this one's record; one at or after
    // it, with nothing activated, sets aside no held record.
    tx(&h.db, |sql| pause(sql, "operator_hold", T0)).unwrap();
    let r = tx(&h.db, |sql| {
        restart_target(
            sql,
            &h.cfg,
            &h.trusted,
            &launch_bundle_json(from - 1),
            from - 1,
            T0,
        )
    })
    .unwrap()
    .unwrap_err();
    assert_eq!(r.code, "restart_regressed");
    let r = tx(&h.db, |sql| {
        restart_target(
            sql,
            &h.cfg,
            &h.trusted,
            &launch_bundle_json(from + 2),
            from + 2,
            T0,
        )
    })
    .unwrap()
    .unwrap();
    assert_eq!(r.previous, None);
}

/// The floor a restart sets: launch bundles issued, and cohort bundles
/// opened, at or after the restarted record's first epoch.
#[test]
fn only_the_restarted_records_bundles_follow_a_restart() {
    assert_eq!(restart_floor_ok(67, Some(67), None), Ok(()));
    assert_eq!(restart_floor_ok(67, Some(70), None), Ok(()));
    assert!(restart_floor_ok(67, Some(66), None).is_err());
    assert_eq!(restart_floor_ok(67, None, Some(67)), Ok(()));
    assert!(
        restart_floor_ok(67, None, Some(63)).is_err(),
        "the earlier record's cohort 12"
    );
    assert!(restart_floor_ok(67, None, None).is_err());
    assert!(restart_floor_ok(67, Some(70), Some(70)).is_err());
}

/// With the launch burn row held (no cohort target activated, so no
/// activation bounds the floor), a second restart may not reuse the first
/// one's floor: every restart's first epoch must exceed every epoch an
/// installed bundle was issued or opened at, the first restart's own launch
/// bundle included. So that record's bundles never install again after the
/// second restart, nor the original record's.
#[test]
fn a_second_restart_never_reuses_an_earlier_restarts_floor() {
    let h = H::new();
    tx(&h.db, |sql| {
        install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(1), T0)
    })
    .unwrap()
    .unwrap();
    assert_eq!(tx(&h.db, record_epoch).unwrap(), Some(1));
    tx(&h.db, |sql| pause(sql, "operator_hold", T0)).unwrap();
    let first = launch_bundle_json(10);
    tx(&h.db, |sql| {
        restart_target(sql, &h.cfg, &h.trusted, &first, 10, T0)
    })
    .unwrap()
    .unwrap();
    assert_eq!(tx(&h.db, record_epoch).unwrap(), Some(10));
    // The first restart's floor again (or below it), even with a launch
    // bundle issued later: refused, nothing changes.
    for floor in [10, 9] {
        let r = tx(&h.db, |sql| {
            restart_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(12), floor, T0)
        })
        .unwrap()
        .unwrap_err();
        assert_eq!(r.code, "restart_regressed", "{floor}");
    }
    assert_eq!(h.counter("held_target_resets"), 1);
    let second = launch_bundle_json(12);
    tx(&h.db, |sql| {
        restart_target(sql, &h.cfg, &h.trusted, &second, 11, T0)
    })
    .unwrap()
    .unwrap();
    assert_eq!(tx(&h.db, restart_floor).unwrap(), Some(11));
    assert_eq!(tx(&h.db, record_epoch).unwrap(), Some(12));
    for (issued, record) in [
        (10, "the first restarted record"),
        (1, "the original record"),
    ] {
        let r = tx(&h.db, |sql| {
            install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(issued), T0)
        })
        .unwrap()
        .unwrap_err();
        assert_eq!(r.code, "bundle_before_restarted_record", "{record}");
    }
    // A third restart must exceed the second's own launch bundle (issued 12),
    // not only its floor (11).
    let r = tx(&h.db, |sql| {
        restart_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(12), 12, T0)
    })
    .unwrap()
    .unwrap_err();
    assert_eq!(r.code, "restart_regressed");
    tx(&h.db, |sql| {
        restart_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(13), 13, T0)
    })
    .unwrap()
    .unwrap();
}

/// A restart while an attempt is uncertain leaves it to finalized state: it
/// is never sent again before its era expires, and the restarted target
/// signs only an epoch the earlier one never took.
#[test]
fn a_held_target_restart_with_an_uncertain_attempt_never_resends_it() {
    let mut h = H::new();
    tx(&h.db, |sql| {
        install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(1), T0)
    })
    .unwrap()
    .unwrap();
    h.resume();
    h.send.set(Send::Lose);
    h.until(40, |h| {
        h.ops()
            .first()
            .is_some_and(|o| o.attempts.first().is_some_and(|a| a.state == "uncertain"))
    });
    let op = h.ops()[0].clone();
    h.send.set(Send::Accept);
    tx(&h.db, |sql| pause(sql, "operator_hold", T0)).unwrap();
    tx(&h.db, |sql| {
        restart_target(
            sql,
            &h.cfg,
            &h.trusted,
            &launch_bundle_json(op.epoch),
            op.epoch,
            T0,
        )
    })
    .unwrap()
    .unwrap();
    h.resume();
    let expiry = op.attempts[0].expiry;
    while h.chain.head + 1 < expiry {
        h.block();
        assert_eq!(
            h.ops()[0].attempts.len(),
            1,
            "re-signed before expiry at {}",
            h.chain.head
        );
    }
    h.until(400, committed);
    let ops = h.ops();
    let epochs: std::collections::BTreeSet<u64> = ops.iter().map(|o| o.epoch).collect();
    assert_eq!(epochs.len(), ops.len(), "one operation per epoch");
    assert_eq!(ops[0].attempts[0].state, "expired");
    assert_eq!(
        h.chain.accepted.len(),
        1,
        "only one submission ever reached the pool"
    );
}

#[test]
fn a_reward_v1_launch_bundle_commits_the_burn_row_to_the_owner_uid_each_epoch() {
    let mut h = H::new();
    let digest = tx(&h.db, |sql| install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(1), T0))
        .unwrap()
        .unwrap();
    let launch = slop_ninja_protocol::CalculationTargetV2::launch_burn(slop_ninja_protocol::ChainBinding {
        genesis: slop_ninja_protocol::TESTNET_GENESIS,
        netuid: NETUID,
        mechanism: 0,
    });
    assert_eq!(digest, slop_ninja_protocol::Record::digest(&launch).unwrap().to_string());
    h.resume();
    h.until(80, committed);
    let ops = h.ops();
    assert_eq!(ops.len(), 1);
    assert!(ops[0].op_id.starts_with(slop_ninja_protocol::WEIGHT_OPERATION_V2_PREFIX));
    let row: RowSummary = serde_json::from_value(ops[0].row.clone().unwrap()).unwrap();
    assert_eq!(row.rule, "reward-v1");
    assert_eq!(row.entries, vec![(fx::owner().uid, 65535)], "every unit burns through the owner hotkey's UID");
    h.until(200, |h| h.ops().len() == 2 && committed(h));
    let ops = h.ops();
    assert_eq!(ops[1].epoch, ops[0].epoch + 1);
    assert_ne!(ops[0].op_id, ops[1].op_id, "one identity per tempo");
    // Before its issue the launch authorization yields no operation; from
    // its issue on it has no end, its renew-by hint (epoch 540) included.
    tx(&h.db, |sql| install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(500), T0)).unwrap().unwrap();
    let t = tx(&h.db, |sql| active_target(sql, &h.cfg, &h.trusted)).unwrap().unwrap().unwrap();
    assert_eq!(t.op_id(&h.trusted, h.cfg.hotkey, 9).unwrap_err().code, "authorization_refused");
    for epoch in [500, 540, 541, 1_000_000] {
        assert!(t.op_id(&h.trusted, h.cfg.hotkey, epoch).is_ok(), "epoch {epoch}");
    }
}

fn chain_binding() -> slop_ninja_protocol::ChainBinding {
    slop_ninja_protocol::ChainBinding {
        genesis: slop_ninja_protocol::TESTNET_GENESIS,
        netuid: NETUID,
        mechanism: 0,
    }
}

/// A signed revocation of the launch authorization issued at
/// `authorization_issued`, dated `issued_epoch`, as hex.
fn launch_revocation_hex(authorization_issued: u64, issued_epoch: u64) -> String {
    let chain = chain_binding();
    let auth = sign(
        slop_ninja_protocol::LaunchAuthorization::new(&chain, 1, authorization_issued).unwrap(),
        &fx::authorization_key(),
    )
    .unwrap();
    let launch = slop_ninja_protocol::ActiveTarget::launch(&chain).unwrap();
    let checked = slop_ninja_protocol::LaunchAuthorization::check(&auth, &fx::trusted_keys(), &chain, &launch).unwrap();
    let revocation = slop_ninja_protocol::RewardIntentRevocation::new(&checked, 1, issued_epoch).unwrap();
    hex::encode(sign(revocation, &fx::authorization_key()).unwrap().to_bytes().unwrap())
}

/// No authorization expires (simulated): the chain is at epoch 106, long
/// past the launch authorization's renew-by hint (epoch 41), as after a long
/// coordinator silence, and the coordinator still commits the burn row each
/// epoch. A signed revocation stops commits of the revoked target at once
/// (no new operation is signed); a later authorization resumes them.
#[test]
fn the_coordinator_commits_past_the_renew_by_hint_until_a_signed_revocation() {
    let mut h = H::new();
    h.chain.sched.subnet_epoch_index += 100;
    h.chain.snapshot(START - 1, vec![]);
    tx(&h.db, |sql| install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(1), T0)).unwrap().unwrap();
    let view = h.status().authorization.unwrap();
    assert_eq!((view.kind.as_str(), view.issued_epoch, view.renew_by_epoch, view.revoked_by.as_deref()), ("launch", 1, 41, None));
    h.resume();
    h.until(80, committed);
    let first = h.ops()[0].clone();
    assert!(first.epoch > 41 + 60, "the hint passed long ago: epoch {}", first.epoch);
    h.until(400, |h| h.ops().len() == 2 && h.ops()[1].state == "committed");
    let second = h.ops()[1].clone();
    assert_eq!(second.epoch, first.epoch + 1);

    // A forged or foreign revocation is refused and changes nothing.
    let mut forged = hex::decode(launch_revocation_hex(1, second.epoch)).unwrap();
    let last = forged.len() - 1;
    forged[last] ^= 1;
    let refused = tx(&h.db, |sql| install_revocation(sql, &h.cfg, &h.trusted, &hex::encode(&forged), None, T0)).unwrap().unwrap_err();
    assert_eq!(refused.code, "revocation_refused");
    assert!(tx(&h.db, |sql| active_target(sql, &h.cfg, &h.trusted)).unwrap().unwrap().is_ok());

    // The signed revocation withdraws the installed launch authorization.
    let revoked_at = second.epoch;
    let hex_rev = launch_revocation_hex(1, revoked_at);
    let r = tx(&h.db, |sql| install_revocation(sql, &h.cfg, &h.trusted, &hex_rev, None, T0)).unwrap().unwrap();
    assert!(r.new && r.withdraws_installed, "{r:?}");
    let again = tx(&h.db, |sql| install_revocation(sql, &h.cfg, &h.trusted, &hex_rev, None, T0)).unwrap().unwrap();
    assert!(!again.new, "journaled once");
    match tx(&h.db, |sql| active_target(sql, &h.cfg, &h.trusted)).unwrap().unwrap() {
        Err(r) => assert_eq!(r.code, "authorization_revoked"),
        Ok(_) => panic!("the revoked authorization is still active"),
    }
    assert_eq!(h.status().authorization.unwrap().revoked_by.as_deref(), Some(r.revocation.as_str()));
    // Reinstalling the revoked authorization is refused.
    let reinstall = tx(&h.db, |sql| install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(1), T0)).unwrap();
    assert_eq!(reinstall.unwrap_err().code, "authorization_revoked");
    // Three more epochs: nothing of the revoked target is committed.
    h.blocks(3 * 360);
    let after: Vec<_> = h.ops().into_iter().filter(|o| o.epoch > second.epoch).collect();
    assert!(
        after.iter().all(|o| o.state != "committed" && o.attempts.is_empty()),
        "no operation of the revoked target is signed: {after:#?}"
    );
    assert!(h.status().log.iter().any(|l| l["event"] == "authorization_revoked"));

    // A launch authorization issued after the revocation resumes commits.
    let resume_at = h.chain.sched.subnet_epoch_index;
    assert!(resume_at > revoked_at);
    tx(&h.db, |sql| install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(resume_at), T0)).unwrap().unwrap();
    assert!(h.status().authorization.unwrap().revoked_by.is_none());
    h.until(800, |h| h.ops().iter().any(|o| o.epoch >= resume_at && o.state == "committed"));
}

/// Records a preflight's schedule at epoch `epoch`, observed at a block past
/// the finalized cursor, as the finalized epoch index a revocation's issue is
/// bounded by.
fn observe_epoch(h: &H, epoch: u64) {
    tx(&h.db, |sql| {
        sql.exec(
            "INSERT INTO chain_schedule (id, block_number, block_hash, tempo, last_epoch_block, pending_epoch_at,
                subnet_epoch_index, blocks_since_last_step, reveal_period, observed_at_ms)
             VALUES (1, 1000000000, '00', 360, 0, 0, ?, 1, 1, 0)
             ON CONFLICT(id) DO UPDATE SET subnet_epoch_index = excluded.subnet_epoch_index",
            &[crate::sql::bind_u64(epoch)?],
        )
    })
    .unwrap();
}

/// A signed revocation of an arbitrary `target`, dated `issued_epoch`, under
/// `key`.
fn revocation_of_target(target: u8, issued_epoch: u64, key: &slop_ninja_protocol::SigningKey) -> Vec<u8> {
    let revocation = slop_ninja_protocol::RewardIntentRevocation {
        chain: chain_binding(),
        target: slop_ninja_protocol::Digest([target; 32]),
        authorization: slop_ninja_protocol::Digest([target ^ 0xff; 32]),
        checkpoint_generation: 1,
        issued_epoch,
    };
    sign(revocation, key).unwrap().to_bytes().unwrap()
}

/// The pointer names the installed authorization and the revocation set by
/// content; it is rewritten only when its bytes change, and the write is
/// recorded once.
#[test]
fn the_authorization_pointer_names_the_installed_authorization_and_the_revocation_set() {
    let h = H::new();
    observe_epoch(&h, 10);
    // A T12 v1 bundle has no reward-v1 authorization: no pointer.
    assert!(tx(&h.db, |sql| pointer_plan(sql, &h.cfg, &h.trusted)).unwrap().is_none());
    tx(&h.db, |sql| install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(1), T0)).unwrap().unwrap();
    let plan = tx(&h.db, |sql| pointer_plan(sql, &h.cfg, &h.trusted)).unwrap().unwrap();
    assert_eq!(plan.written, None);
    let pointer: slop_ninja_admission_wire::json::AuthorizationPointer = serde_json::from_str(&plan.body).unwrap();
    assert_eq!(pointer.kind, "launch");
    assert_eq!(pointer.schema, slop_ninja_admission_wire::json::AUTHORIZATION_POINTER_SCHEMA);
    assert_eq!(pointer.chain.binding(), Some(chain_binding()));
    assert!(pointer.revocations.is_none() && pointer.renewal.is_none());
    assert!(!plan.body.contains("revocation"), "an absent revocation set is not written");
    assert_eq!(plan.objects.len(), 1);
    let o = &plan.objects[0];
    assert_eq!(*o, pointer.authorization, "the plan names objects, never carries them");
    let bytes = tx(&h.db, |sql| pointer_object(sql, &h.cfg, &h.trusted, &o.sha256)).unwrap().unwrap();
    assert_eq!(o.path, format!("authorizations/{}.signed", hex::encode(sha2::Sha256::digest(&bytes))));
    assert_eq!(pointer.authorization.bytes, bytes.len() as u64);
    assert_eq!(tx(&h.db, |sql| pointer_object(sql, &h.cfg, &h.trusted, &"0".repeat(64))).unwrap(), None);
    slop_ninja_protocol::Signed::<slop_ninja_protocol::LaunchAuthorization>::open(&bytes, &h.trusted).unwrap();
    // The receipts contract the retention hold and the publisher read accepts
    // what this writes, within its read bounds; a launch hold names no cohort.
    use slop_ninja_admission_wire::receipts as contract;
    assert_eq!(super::authority::POINTER_KEY, contract::AUTHORIZATION_POINTER_KEY);
    assert!(plan.body.len() as u64 <= contract::AUTHORIZATION_POINTER_BOUND);
    assert!(bytes.len() as u64 <= contract::AUTHORIZATION_OBJECT_BOUND);
    let checked = contract::parse_pointer(plan.body.as_bytes(), &chain_binding()).unwrap();
    assert_eq!(
        crate::eval::retention::hold_of(&checked.kind, &checked.authorization, &bytes),
        Ok(crate::eval::retention::Hold::Launch)
    );
    assert_eq!(plan.sha256, hex::encode(sha2::Sha256::digest(plan.body.as_bytes())));
    // Recorded once; the same plan reports it written. Every pass is
    // recorded: a failure keeps the last confirmation and shows on
    // `/status` until a later pass confirms the pointer again.
    let view = |h: &H| h.status().authorization.unwrap();
    assert!(!view(&h).pointer_current && view(&h).pointer_confirmed_age_ms.is_none());
    let confirmed = PointerOutcome { sha256: Some(plan.sha256.clone()), error: None };
    assert!(tx(&h.db, |sql| pointer_outcome(sql, &confirmed, T0)).unwrap());
    assert!(!tx(&h.db, |sql| pointer_outcome(sql, &confirmed, T0 + 1)).unwrap());
    assert!(view(&h).pointer_current && view(&h).pointer_error.is_none());
    let failed = PointerOutcome { sha256: None, error: Some("object_put".into()) };
    for _ in 0..3 {
        assert!(!tx(&h.db, |sql| pointer_outcome(sql, &failed, T0 + 2)).unwrap());
    }
    let v = view(&h);
    assert!(!v.pointer_current);
    assert_eq!(v.pointer_error.as_deref(), Some("object_put"));
    assert!(v.pointer_confirmed_age_ms.is_some(), "a failure keeps the last confirmation");
    assert_eq!(logged(&h, "authorization_pointer_failed").len(), 1, "a repeated failure is logged once");
    let odd = PointerOutcome { sha256: None, error: Some("not a code!".into()) };
    tx(&h.db, |sql| pointer_outcome(sql, &odd, T0 + 3)).unwrap();
    assert_eq!(view(&h).pointer_error.as_deref(), Some("outcome_malformed"));
    tx(&h.db, |sql| pointer_outcome(sql, &confirmed, T0 + 4)).unwrap();
    assert!(view(&h).pointer_current && view(&h).pointer_error.is_none());
    assert_eq!(logged(&h, "authorization_pointer_recovered").len(), 1);
    let again = tx(&h.db, |sql| pointer_plan(sql, &h.cfg, &h.trusted)).unwrap().unwrap();
    assert_eq!(again.written.as_deref(), Some(plan.sha256.as_str()));
    assert_eq!(again.body, plan.body, "the pointer does not change with time");
    // A revocation changes the pointer: it names the revocation set.
    let hex_rev = launch_revocation_hex(1, 5);
    tx(&h.db, |sql| install_revocation(sql, &h.cfg, &h.trusted, &hex_rev, None, T0)).unwrap().unwrap();
    let revoked = tx(&h.db, |sql| pointer_plan(sql, &h.cfg, &h.trusted)).unwrap().unwrap();
    assert_ne!(revoked.sha256, plan.sha256);
    let pointer: slop_ninja_admission_wire::json::AuthorizationPointer = serde_json::from_str(&revoked.body).unwrap();
    let r = pointer.revocations.unwrap();
    assert!(revoked.objects.contains(&r));
    let set_bytes = tx(&h.db, |sql| pointer_object(sql, &h.cfg, &h.trusted, &r.sha256)).unwrap().unwrap();
    assert_eq!(hex::encode(set_bytes), hex_rev);
    assert!(revoked.body.len() as u64 <= contract::AUTHORIZATION_POINTER_BOUND);
    assert_eq!(hex_rev.len() as u64 / 2, contract::revocation_len(&chain_binding()));
    contract::parse_pointer(revoked.body.as_bytes(), &chain_binding()).unwrap();
    assert_eq!(h.status().authorization.unwrap().revocations, 1);

    // Revocations of other targets join the set, oldest first; a later one
    // of a target replaces its earlier one, which it covers; one a rotated
    // trust root no longer verifies is left out. Every earlier revocation
    // stays in the set however many later ones of other targets follow.
    let other = fx::package_key();
    for (target, epoch) in [(0x11, 6), (0x22, 7), (0x11, 9)] {
        let hex_other = hex::encode(revocation_of_target(target, epoch, &fx::authorization_key()));
        tx(&h.db, |sql| install_revocation(sql, &h.cfg, &h.trusted, &hex_other, None, T0)).unwrap().unwrap();
    }
    let stale = revocation_of_target(0x33, 8, &other);
    tx(&h.db, |sql| {
        sql.exec(
            "INSERT INTO sign_revocations (digest, target, authorization, issued_epoch, signed_hex, at_ms)
             VALUES ('stale', ?, 'x', 8, ?, 0)",
            &[slop_ninja_protocol::Digest([0x33; 32]).to_string().into(), hex::encode(&stale).into()],
        )
    })
    .unwrap();
    let set_plan = tx(&h.db, |sql| pointer_plan(sql, &h.cfg, &h.trusted)).unwrap().unwrap();
    let pointer: slop_ninja_admission_wire::json::AuthorizationPointer = serde_json::from_str(&set_plan.body).unwrap();
    let r = pointer.revocations.unwrap();
    let set = tx(&h.db, |sql| pointer_object(sql, &h.cfg, &h.trusted, &r.sha256)).unwrap().unwrap();
    assert_eq!(r.bytes, set.len() as u64);
    let entries: Vec<_> = contract::revocation_set_entries(&set, &chain_binding())
        .unwrap()
        .map(|e| slop_ninja_protocol::Signed::<slop_ninja_protocol::RewardIntentRevocation>::open(e, &h.trusted).unwrap())
        .map(|r| (r.target, r.issued_epoch))
        .collect();
    let burn = slop_ninja_protocol::CalculationTargetV2::launch_burn(chain_binding()).digest().unwrap();
    assert_eq!(
        entries,
        vec![
            (burn, 5),
            (slop_ninja_protocol::Digest([0x22; 32]), 7),
            (slop_ninja_protocol::Digest([0x11; 32]), 9)
        ]
    );
    assert!(set_plan.body.len() as u64 <= contract::AUTHORIZATION_POINTER_BOUND);
}

/// A revocation's issue epoch is bounded by the finalized epoch index plus
/// one, read from chain state; none is accepted before any is observed.
#[test]
fn a_revocation_dated_past_the_finalized_epoch_is_refused() {
    let h = H::new();
    tx(&h.db, |sql| install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(1), T0)).unwrap().unwrap();
    let unknown = tx(&h.db, |sql| install_revocation(sql, &h.cfg, &h.trusted, &launch_revocation_hex(1, 5), None, T0)).unwrap();
    assert_eq!(unknown.unwrap_err().code, "revocation_epoch_unknown");
    observe_epoch(&h, 20);
    for epoch in [22, 1_000_000] {
        let far = tx(&h.db, |sql| install_revocation(sql, &h.cfg, &h.trusted, &launch_revocation_hex(1, epoch), None, T0)).unwrap();
        assert_eq!(far.unwrap_err().code, "revocation_epoch_future", "epoch {epoch}");
    }
    assert_eq!(h.status().authorization.unwrap().revocations, 0, "a refusal journals nothing");
    let ok = tx(&h.db, |sql| install_revocation(sql, &h.cfg, &h.trusted, &launch_revocation_hex(1, 21), None, T0)).unwrap().unwrap();
    assert!(ok.new && ok.withdraws_installed);
    assert_eq!(super::authority::finalized_epoch(&h.db).unwrap(), Some(20));
    // A schedule observed before the cursor is advanced to it.
    let cursor = crate::journal::cursor(&h.db).unwrap().map_or(0, |c| c.number);
    tx(&h.db, |sql| {
        sql.exec(
            "UPDATE chain_schedule SET block_number = ?, last_epoch_block = ?, blocks_since_last_step = 0",
            &[crate::sql::bind_u64(cursor.saturating_sub(3 * 361))?, crate::sql::bind_u64(cursor.saturating_sub(3 * 361))?],
        )
    })
    .unwrap();
    if cursor > 3 * 361 {
        assert!(super::authority::finalized_epoch(&h.db).unwrap() > Some(20));
    }
    // The set capacity follows from the set's bound: whole records in what
    // the revoke route reads whole.
    let len = slop_ninja_admission_wire::receipts::revocation_len(&chain_binding()) as usize;
    let capacity = super::authority::revocation_set_capacity(len);
    assert_eq!(capacity, super::authority::REVOCATION_SET_BYTES / len);
    assert!(capacity * len <= crate::arena_routes::WORKER_CONTROL_BYTES);
}

/// A journal that never ran a signing preflight (paused since it was
/// created) takes the Worker's RPC reading of the finalized epoch index;
/// the later of the two sources bounds the issue epoch.
#[test]
fn a_revocation_without_a_preflight_uses_the_observed_epoch() {
    let h = H::new();
    tx(&h.db, |sql| install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(1), T0)).unwrap().unwrap();
    let far = tx(&h.db, |sql| install_revocation(sql, &h.cfg, &h.trusted, &launch_revocation_hex(1, 32), Some(30), T0)).unwrap();
    assert_eq!(far.unwrap_err().code, "revocation_epoch_future");
    let ok = tx(&h.db, |sql| install_revocation(sql, &h.cfg, &h.trusted, &launch_revocation_hex(1, 31), Some(30), T0)).unwrap().unwrap();
    assert!(ok.new && ok.withdraws_installed);
    // A schedule that lags the reading does not lower it.
    observe_epoch(&h, 10);
    let ok = tx(&h.db, |sql| install_revocation(sql, &h.cfg, &h.trusted, &launch_revocation_hex(1, 31), Some(30), T0)).unwrap().unwrap();
    assert!(!ok.new);
}

/// Journals `count` revocations of distinct targets under the trusted key,
/// directly (as `install_revocation` would store them).
fn fill_revocation_set(h: &H, count: usize) {
    let key = fx::authorization_key();
    tx(&h.db, |sql| {
        for i in 0..count {
            let mut target = [0u8; 32];
            target[..8].copy_from_slice(&(i as u64).to_le_bytes());
            target[31] = 0xee;
            let revocation = slop_ninja_protocol::RewardIntentRevocation {
                chain: chain_binding(),
                target: slop_ninja_protocol::Digest(target),
                authorization: slop_ninja_protocol::Digest([1; 32]),
                checkpoint_generation: 1,
                issued_epoch: 5,
            };
            let bytes = sign(revocation, &key).unwrap().to_bytes().unwrap();
            sql.exec(
                "INSERT INTO sign_revocations (digest, target, authorization, issued_epoch, signed_hex, at_ms)
                 VALUES (?, ?, 'x', 5, ?, 0)",
                &[format!("d{i}").into(), slop_ninja_protocol::Digest(target).to_string().into(), hex::encode(bytes).into()],
            )?;
        }
        Ok::<(), crate::sql::StoreError>(())
    })
    .unwrap();
}

/// Arena witness (`--features arena-witness`): at full revocation-set
/// capacity the journal builds the plan and the set within its footprint,
/// and the operator's revoke route (the smaller of the two routes that run
/// the pointer pass) holds the plan reply and the set within its own.
#[cfg(feature = "arena-witness")]
#[test]
fn arena_witness_the_pointer_pass_fits_at_full_revocation_capacity() {
    use crate::arena_routes::{JOURNAL_CONTROL_BYTES, R_JOURNAL_FETCH, R_OPERATOR, R_SCHEDULED, WORKER_CONTROL_BYTES, CONTINUATION_CONTROL_BYTES};
    use slop_ninja_arena::{Mode, admit_current_block, set_mode};
    set_mode(Mode::Enforce);
    let h = H::new();
    tx(&h.db, |sql| install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(1), T0)).unwrap().unwrap();
    let len = slop_ninja_admission_wire::receipts::revocation_len(&chain_binding()) as usize;
    let capacity = super::authority::revocation_set_capacity(len);
    fill_revocation_set(&h, capacity);
    // Warm the database outside the witnessed scopes, as a live journal is.
    let warm = tx(&h.db, |sql| pointer_plan(sql, &h.cfg, &h.trusted)).unwrap().unwrap();
    let set_ref = warm.objects.last().unwrap().clone();
    assert_eq!(set_ref.bytes as usize, capacity * len);
    drop(warm);
    let reply = crate::arena_witness::witness(R_JOURNAL_FETCH.route(), JOURNAL_CONTROL_BYTES, || {
        let plan = tx(&h.db, |sql| pointer_plan(sql, &h.cfg, &h.trusted)).unwrap().unwrap();
        serde_json::to_vec(&Some(plan)).unwrap().len()
    });
    let set_len = crate::arena_witness::witness(R_JOURNAL_FETCH.route(), JOURNAL_CONTROL_BYTES, || {
        tx(&h.db, |sql| pointer_object(sql, &h.cfg, &h.trusted, &set_ref.sha256)).unwrap().unwrap().len()
    });
    assert_eq!(set_len, set_ref.bytes as usize);
    // The Worker side: the plan reply and the set arrive as admitted blocks
    // of their exact lengths; the parse and the digest check run in the
    // route's control footprint.
    let plan_bytes = serde_json::to_vec(&tx(&h.db, |sql| pointer_plan(sql, &h.cfg, &h.trusted)).unwrap()).unwrap();
    assert_eq!(plan_bytes.len(), reply);
    let set = tx(&h.db, |sql| pointer_object(sql, &h.cfg, &h.trusted, &set_ref.sha256)).unwrap().unwrap();
    for (route, control) in [(R_OPERATOR, WORKER_CONTROL_BYTES), (R_SCHEDULED, CONTINUATION_CONTROL_BYTES)] {
        crate::arena_witness::witness(route.route(), control, || {
            assert_eq!(admit_current_block(plan_bytes.len(), 1), Ok(true));
            let mut raw = Vec::with_capacity(plan_bytes.len());
            raw.extend_from_slice(&plan_bytes);
            let plan: Option<PointerPlan> = serde_json::from_slice(&raw).unwrap();
            let plan = plan.unwrap();
            assert_eq!(admit_current_block(set.len(), 1), Ok(true));
            let mut copy = Vec::with_capacity(set.len());
            copy.extend_from_slice(&set);
            assert_eq!(crate::chain::sha256_hex(&copy), plan.objects.last().unwrap().sha256);
            (plan.body.len(), copy.len())
        });
    }
}

/// Only entries the set would carry count against its capacity: a
/// revocation of a target already carried adds nothing, one of a target
/// whose latest is under a rotated key adds one; at capacity a new target
/// is refused and a later revocation of a carried one is still accepted.
#[test]
fn the_revocation_set_capacity_counts_carried_entries() {
    let h = H::new();
    observe_epoch(&h, 1_000);
    tx(&h.db, |sql| install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(1), T0)).unwrap().unwrap();
    let len = slop_ninja_admission_wire::receipts::revocation_len(&chain_binding()) as usize;
    let capacity = super::authority::revocation_set_capacity(len);
    // Fill the set to one below capacity directly (each signed under the
    // trusted key), plus one stale entry under a rotated key.
    let key = fx::authorization_key();
    fill_revocation_set(&h, capacity - 1);
    tx(&h.db, |sql| {
        sql.exec(
            "INSERT INTO sign_revocations (digest, target, authorization, issued_epoch, signed_hex, at_ms)
             VALUES ('stale', ?, 'x', 5, ?, 0)",
            &[
                slop_ninja_protocol::Digest([0x33; 32]).to_string().into(),
                hex::encode(revocation_of_target(0x33, 5, &fx::package_key())).into(),
            ],
        )
    })
    .unwrap();
    // A target whose latest is not carried (rotated key) takes the last place.
    let replace = hex::encode(revocation_of_target(0x33, 6, &key));
    tx(&h.db, |sql| install_revocation(sql, &h.cfg, &h.trusted, &replace, None, T0)).unwrap().unwrap();
    let set = tx(&h.db, |sql| super::authority::revocation_set(sql, &h.cfg, &h.trusted)).unwrap().unwrap();
    assert_eq!(set.len(), capacity * len);
    // Full: a new target is refused, and nothing is journaled.
    let new_target = hex::encode(revocation_of_target(0x44, 6, &key));
    let full = tx(&h.db, |sql| install_revocation(sql, &h.cfg, &h.trusted, &new_target, None, T0)).unwrap();
    assert_eq!(full.unwrap_err().code, "revocation_set_full");
    // A later revocation of a carried target replaces its entry: accepted.
    let later = hex::encode(revocation_of_target(0x33, 7, &key));
    assert!(tx(&h.db, |sql| install_revocation(sql, &h.cfg, &h.trusted, &later, None, T0)).unwrap().unwrap().new);
    // The installed target is still withdrawn by a successor (supersession).
}

/// A journaled revocation is not verified again: one signed under a trust
/// root rotated since (here a key the coordinator does not trust) for an
/// unrelated target never stops the installed target's commits, and one of
/// the installed target still withdraws it.
#[test]
fn a_journaled_revocation_under_a_rotated_key_stops_only_its_own_target() {
    let mut h = H::new();
    h.chain.snapshot(START - 1, vec![]);
    tx(&h.db, |sql| install_target(sql, &h.cfg, &h.trusted, &launch_bundle_json(1), T0)).unwrap().unwrap();
    let journal = |h: &H, digest: &str, target: slop_ninja_protocol::Digest, bytes: &[u8]| {
        tx(&h.db, |sql| {
            sql.exec(
                "INSERT INTO sign_revocations (digest, target, authorization, issued_epoch, signed_hex, at_ms)
                 VALUES (?, ?, 'x', 1000000, ?, 0)",
                &[digest.into(), target.to_string().into(), hex::encode(bytes).into()],
            )
        })
        .unwrap();
    };
    let rotated = fx::package_key();
    journal(&h, "old", slop_ninja_protocol::Digest([0x44; 32]), &revocation_of_target(0x44, 1_000_000, &rotated));
    assert!(tx(&h.db, |sql| active_target(sql, &h.cfg, &h.trusted)).unwrap().unwrap().is_ok());
    h.resume();
    h.until(80, committed);
    // One of the installed target, journaled under the old key, withdraws it.
    let burn = slop_ninja_protocol::CalculationTargetV2::launch_burn(chain_binding()).digest().unwrap();
    let mut record = slop_ninja_protocol::Signed::<slop_ninja_protocol::RewardIntentRevocation>::from_bytes(
        &revocation_of_target(0x44, 1_000_000, &rotated),
    )
    .unwrap()
    .record;
    record.target = burn;
    journal(&h, "own", burn, &sign(record, &rotated).unwrap().to_bytes().unwrap());
    match tx(&h.db, |sql| active_target(sql, &h.cfg, &h.trusted)).unwrap().unwrap() {
        Err(r) => assert_eq!(r.code, "authorization_revoked"),
        Ok(_) => panic!("the revoked authorization is still active"),
    }
}

#[test]
fn a_step_that_does_not_fit_the_budget_is_never_claimed() {
    // The first step an unbounded wakeup would get, and its worst case.
    let mut probe = H::new();
    probe.resume();
    probe.chain.produce();
    probe.confirm_to(probe.chain.head);
    let first = probe.plan("probe").expect("a first step");
    let worst = step_worst_ms(&first, crate::liveness::TIMEOUT_MS);

    let mut h = H::new();
    h.resume();
    h.chain.produce();
    h.confirm_to(h.chain.head);
    let short = tx(&h.db, |sql| plan_within(sql, &h.ctx(), "delivery:a", Some(worst - 1), h.now.get())).unwrap();
    assert_eq!(short, None, "a step longer than the budget is not handed out");
    assert_eq!(h.counter("sign_budget_declines"), 1);
    // The claim was released at once with nothing pending: the next wakeup
    // plans without waiting out a lease and abandons nothing.
    let fits = tx(&h.db, |sql| plan_within(sql, &h.ctx(), "tick:b", Some(worst), h.now.get())).unwrap();
    assert_eq!(fits.as_ref().map(std::mem::discriminant), Some(std::mem::discriminant(&first)), "{fits:?}");
    assert_eq!(h.counter("sign_claim_held_skips"), 0);
    assert_eq!(h.counter("sign_abandoned_steps"), 0);

    // The step costs the Worker charges are the journal's: a metadata fetch
    // or a submission never fits what a delivery's 30 s `waitUntil` leaves
    // after its first journal call, so those steps stay for the cron.
    let t = crate::liveness::TIMEOUT_MS;
    let after_call = slop_ninja_arena::limits::WAIT_UNTIL_MS - t;
    let claim = ClaimToken { op_id: SIGNER_CLAIM.into(), generation: 1 };
    let at = BlockRef { number: 1, hash: Id([1; 32]) };
    let metadata = SignPlan::Metadata { claim: claim.clone(), at, spec_version: 1, tx_version: 1, purpose: RuntimeFor::Head };
    let submit = SignPlan::Submit { claim: claim.clone(), op_id: "op".into(), attempt: 1, extrinsic: "0x00".into(), drill: None };
    let version = SignPlan::RuntimeVersion { claim, at, purpose: RuntimeFor::Head };
    assert!(step_worst_ms(&metadata, t) > after_call);
    assert!(step_worst_ms(&submit, t) > after_call);
    assert!(step_worst_ms(&version, t) <= after_call, "a one-call step fits a delivery");
}
