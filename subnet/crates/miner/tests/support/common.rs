//! Helpers shared by the tests that run against the mock.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rtao_primitives::CryptoType;
use rtao_signer::KeypairSigner;
use slop_ninja_miner::client::{AdmissionClient, RequestSigner, RtaoSigner, base_url, http_client};
use slop_ninja_miner::direct::{Progress, PutPolicy};
use slop_ninja_miner::object::ObjectHeader;
use slop_ninja_miner::opening::{OsNonce, Store};
use slop_ninja_miner::upload::UploadOptions;
use slop_ninja_miner::{commit, object};
use slop_ninja_protocol::{ChainBinding, NeuronIdentity, TESTNET_GENESIS};

use super::mock::{Mock, MockConfig};

pub const CONTRACT: &str = slop_ninja_artifact_check::spec::CONTRACT_ID;

pub fn chain601() -> ChainBinding {
    ChainBinding {
        genesis: TESTNET_GENESIS,
        netuid: 601,
        mechanism: 0,
    }
}

pub fn config() -> MockConfig {
    MockConfig {
        chain: chain601(),
        cohort: 7,
        opened_epoch: 23_169,
        open_block: 1_900,
        admission_min: 1,
        output_contract: CONTRACT.into(),
        roster_cap: 40,
        max_object_len: 1 << 20,
        request_validity_blocks: 360,
    }
}

pub fn dev_signer(uri: &str) -> Arc<RtaoSigner> {
    let signer = KeypairSigner::from_uri(uri, CryptoType::Sr25519).expect("dev key");
    Arc::new(RtaoSigner(Arc::new(signer)))
}

pub fn identity(signer: &RtaoSigner, uid: u16) -> NeuronIdentity {
    NeuronIdentity {
        uid,
        hotkey: signer.hotkey(),
        registration_block: 1_000 + u64::from(uid),
    }
}

/// A synthetic artifact object (no real model): `family` changes the
/// inference content, `owner` only the owner metadata.
pub fn packed(dir: &Path, family: u8, owner: &str) -> (PathBuf, ObjectHeader) {
    let src = dir.join(format!("src-{family}-{owner_len}", owner_len = owner.len()));
    std::fs::create_dir_all(src.join("tok")).unwrap();
    let graph: Vec<u8> = (0..3_000u32).map(|i| (i as u8) ^ family).collect();
    std::fs::write(src.join("model.onnx"), &graph).unwrap();
    std::fs::write(
        src.join("tok/tokenizer.json"),
        format!("{{\"family\":{family}}}"),
    )
    .unwrap();
    std::fs::write(src.join("owner.json"), owner).unwrap();
    let inputs: Vec<object::PackInput> = [
        "graph:model.onnx",
        "tokenizer:tok/tokenizer.json",
        "owner-metadata:owner.json",
    ]
    .iter()
    .map(|spec| object::PackInput::parse(spec, &src).unwrap())
    .collect();
    let out = dir.join(format!("artifact-{family}-{}.sna", owner.len()));
    let verified = object::pack(CONTRACT, &inputs, &out).unwrap();
    (out, verified)
}

/// The put policy that sends PUTs to the mock's R2 stub (URLs still name
/// the mock's S3 host and are checked in full).
pub fn stub_policy(mock: &Mock) -> PutPolicy {
    PutPolicy::default()
        .with_endpoint_override(&mock.r2_url())
        .unwrap()
}

pub async fn client(mock: &Mock, signer: Arc<dyn RequestSigner>) -> AdmissionClient {
    let http = http_client(Duration::from_secs(30)).unwrap();
    let base = base_url(&mock.url()).unwrap();
    AdmissionClient::connect(http, base, signer, Some(&chain601()))
        .await
        .unwrap()
        .0
        .with_put_policy(stub_policy(mock))
}

/// Records every progress event (test only).
#[derive(Default)]
pub struct Recorder {
    pub events: std::sync::Mutex<Vec<String>>,
    pub bytes: std::sync::atomic::AtomicU64,
}

impl Progress for Recorder {
    fn plan(&self, objects: usize, bytes: u64) {
        self.events
            .lock()
            .unwrap()
            .push(format!("plan {objects} {bytes}"));
    }
    fn start(&self, name: &str, len: u64, attempt: u32) {
        self.events
            .lock()
            .unwrap()
            .push(format!("start {name} {len} {attempt}"));
    }
    fn advance(&self, bytes: u64) {
        self.bytes
            .fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
    }
    fn finish(&self, name: &str, ok: bool, why: &str) {
        self.events
            .lock()
            .unwrap()
            .push(format!("finish {name} {ok} {why}"));
    }
}

pub fn quick() -> UploadOptions {
    UploadOptions {
        attempts: 3,
        backoff: Duration::from_millis(10),
        wait: Duration::from_secs(5),
        poll: Duration::from_millis(10),
        stop_after_objects: None,
        ..UploadOptions::default()
    }
}

pub fn once() -> UploadOptions {
    UploadOptions {
        attempts: 1,
        ..quick()
    }
}

/// A registered hotkey with a finalized commitment for cohort 7 at `block`.
pub struct Entrant {
    pub signer: Arc<RtaoSigner>,
    pub identity: NeuronIdentity,
    pub prepared: commit::Prepared,
}

pub fn entrant(
    mock: &Mock,
    store: &Store,
    uri: &str,
    uid: u16,
    artifact: &ObjectHeader,
    block: u64,
) -> Entrant {
    let signer = dev_signer(uri);
    let identity = identity(&signer, uid);
    mock.register(identity);
    let prepared = commit::prepare(
        store,
        &chain601(),
        mock.with(|s| s.config.cohort),
        &identity,
        artifact,
        &mut OsNonce,
        true,
    )
    .unwrap();
    mock.commit(identity.hotkey, block, 0, &prepared.payload);
    Entrant {
        signer,
        identity,
        prepared,
    }
}
