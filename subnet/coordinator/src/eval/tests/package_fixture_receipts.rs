//! Receipt-format oracle for synthetic package fixtures. This is native setup,
//! not a measurement of the Worker's packaging memory or CPU use.

use super::*;
use crate::eval::receipt_index::{self, IndexScan};
use slop_ninja_protocol::receipt_chunks::{ReceiptIndexBuilder, ReceiptIndexChunk};
use slop_ninja_protocol::{AttemptReceipt, Digest, ReceiptIndex, ReceiptIndexBinding, ScorePackageV2, Signed};

pub(super) struct FixtureIndex {
    binding: ReceiptIndexBinding,
    record: Vec<u8>,
    pub(super) signed: Vec<u8>,
    pub(super) objects: BTreeMap<String, Vec<u8>>,
    legacy: Option<ReceiptIndex>,
}

impl FixtureIndex {
    pub(super) fn count(&self) -> u64 { self.binding.count }
    pub(super) fn digest(&self) -> Digest { self.binding.root }
    pub(super) fn record_bytes(&self) -> &[u8] { &self.record }
    pub(super) fn check_package(&self, package: &ScorePackageV2) { self.binding.check(package).unwrap(); }

    pub(super) fn verify_objects(&self, bundles: &[Vec<u8>]) -> u64 {
        if let Some(index) = &self.legacy {
            return slop_ninja_protocol::target_bundle::verify_receipt_chunks(index,
                &bundles.iter().map(Vec::as_slice).collect::<Vec<_>>()).unwrap().len() as u64;
        }
        // The native oracle can index borrowed bundle slices. The Worker never
        // constructs this map; its lane verification and durable journal remain
        // the production evidence path.
        let mut receipts_by_id = BTreeMap::new();
        for bundle in bundles {
            for bytes in receipts::decode_chunk(bundle).unwrap() {
                let receipt = AttemptReceipt::from_bytes(bytes).unwrap();
                assert!(receipts_by_id.insert(receipt.attempt, bytes).is_none(), "duplicate receipt");
            }
        }
        assert_eq!(receipts_by_id.len() as u64, self.count());
        let mut verified = ReceiptIndexBuilder::new(self.binding.cohort_manifest);
        for bytes in self.objects.values() {
            let chunk = ReceiptIndexChunk::from_bytes(bytes).unwrap();
            let stored: Vec<&[u8]> = chunk.entries.iter()
                .map(|entry| receipts_by_id.remove(&entry.attempt).expect("receipt object missing")).collect();
            verified.push_verified(&chunk, stored).unwrap();
        }
        assert!(receipts_by_id.is_empty(), "unindexed receipt object");
        let commitment = verified.finish();
        assert_eq!(commitment.binding().unwrap(), self.binding);
        self.count()
    }
}

pub(super) fn fixture_index(db: &Sqlite, seq: i64) -> (FixtureIndex, Value) {
    let cohort = engine::cohort(db, seq).unwrap();
    let key = slop_ninja_protocol::fixtures::receipt_key();
    let keys = slop_ninja_protocol::fixtures::trusted_keys();
    if cohort.plan.receipt_index_format.is_none() {
        let (index, map) = engine::receipt_index(db, seq).unwrap();
        let binding = ReceiptIndexBinding { cohort_manifest: index.cohort_manifest,
            root: index.digest().unwrap(), count: index.count() };
        let signed = crate::eval::package::signed_bytes(index.clone(), &key).unwrap();
        assert_eq!(Signed::<ReceiptIndex>::open(&signed, &keys).unwrap(), index);
        return (FixtureIndex { binding, record: index.to_bytes().unwrap(), signed,
            objects: BTreeMap::new(), legacy: Some(index) }, map);
    }
    let scan = IndexScan::open(db, seq).unwrap();
    let mut builder = ReceiptIndexBuilder::new(scan.manifest());
    let mut after = None;
    let mut ordinal = 0;
    let mut objects = BTreeMap::new();
    while let Some(chunk) = scan.page(db, ordinal, after).unwrap() {
        let bytes = chunk.to_bytes().unwrap();
        let key = format!("{}/receipts/index-v2/{ordinal:016x}-{}.bin",
            engine::receipt_prefix(&cohort.label), lane_logic::sha_hex(&bytes));
        assert!(objects.insert(key, bytes).is_none());
        builder.push(&chunk).unwrap();
        after = Some(chunk.entries.last().unwrap().attempt);
        ordinal += 1;
    }
    let index = builder.finish();
    scan.check_complete(db, &index).unwrap();
    let map = receipt_index::chunk_map(db, seq, &index).unwrap();
    let signed = crate::eval::package::signed_bytes(index.clone(), &key).unwrap();
    assert_eq!(Signed::<slop_ninja_protocol::receipt_chunks::ReceiptIndexCommitment>::open(&signed, &keys).unwrap(), index);
    (FixtureIndex { binding: index.binding().unwrap(), record: index.to_bytes().unwrap(),
        signed, objects, legacy: None }, map)
}
