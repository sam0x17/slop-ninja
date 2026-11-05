//! The coordinator's current calculation target and its row.
//!
//! Testnet shortcut T12: until cohort control (M4) publishes packages, the
//! operator installs the same signed bundle file the native validator reads
//! (`slop_ninja.fixture_package/v1`). Verification is the native validator's,
//! over the shared protocol crate: every signed record is authenticated
//! before it is decoded, the package is bound to its manifest, the target to
//! the package and the receipt index to the package's root and count, and
//! every stored receipt object is checked against its index entry.
//!
//! The row comes from the frozen reward rule the package names. A
//! `testnet-reward-v0` package (T8) goes through [`derive_row`]; a `reward-v1`
//! bundle ([`BUNDLE_SCHEMA_V2`]) is verified by the protocol crate's
//! `target_bundle`, which recomputes the package's outcome and checks the
//! closure and the authorization, and its row comes from
//! `slop_ninja_protocol::reward_v1::derive_row` against a snapshot that also
//! carries the finalized subnet owner hotkey.

use serde::{Deserialize, Serialize};
use slop_ninja_protocol::{
    AccountId, CalculationTarget, ChainBinding, CohortManifest, Digest, NeuronIdentity,
    ReceiptIndex, Record, RegistrationSnapshot, RegistrationSnapshotV2, RewardRule, ScorePackage,
    Signed, TrustedKeys, WeightRow, WeightRowV2,
};

pub const BUNDLE_SCHEMA: &str = "slop_ninja.fixture_package/v1";
/// Largest bundle the operator route accepts.
pub const MAX_BUNDLE_BYTES: usize = 1 << 20;

/// One package as the native validator stores it. Every field is exact hex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    pub schema: String,
    /// Always `T12`.
    pub shortcut: String,
    pub manifest: String,
    pub package: String,
    pub target: String,
    pub receipt_index: String,
    pub receipts: Vec<String>,
}

/// A refusal with a stable code. Details carry no private data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Refusal {
    pub code: &'static str,
    pub detail: String,
}

impl Refusal {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

fn protocol(code: &'static str) -> impl Fn(slop_ninja_protocol::Error) -> Refusal {
    move |e| Refusal::new(code, format!("{e} ({})", e.code()))
}

fn unhex(field: &str, value: &str) -> Result<Vec<u8>, Refusal> {
    hex::decode(value).map_err(|_| Refusal::new("bundle_malformed", format!("{field} is not hex")))
}

fn key(name: &str, value: &str) -> Result<[u8; 32], Refusal> {
    hex::decode(value.trim_start_matches("0x"))
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| Refusal::new("trusted_keys_invalid", format!("{name} is not 32-byte hex")))
}

/// The three application public keys (not chain keys), all distinct.
pub fn trusted_keys(
    package: &str,
    receipt: &str,
    authorization: &str,
) -> Result<TrustedKeys, Refusal> {
    TrustedKeys::new(
        key("package", package)?,
        key("receipt", receipt)?,
        key("authorization", authorization)?,
    )
    .map_err(protocol("trusted_keys_invalid"))
}

/// Authenticated, mutually bound records of one package.
#[derive(Clone, Debug)]
pub struct VerifiedTarget {
    pub manifest: CohortManifest,
    pub package: ScorePackage,
    pub target: CalculationTarget,
    pub target_digest: Digest,
    pub receipt_count: u64,
}

impl VerifiedTarget {
    pub fn chain(&self) -> &ChainBinding {
        &self.target.chain
    }
}

pub fn parse(bytes: &[u8]) -> Result<Bundle, Refusal> {
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(Refusal::new("bundle_too_large", "bundle exceeds 1 MiB"));
    }
    let bundle: Bundle = serde_json::from_slice(bytes)
        .map_err(|e| Refusal::new("bundle_malformed", e.to_string()))?;
    if bundle.schema != BUNDLE_SCHEMA || bundle.shortcut != "T12" {
        return Err(Refusal::new(
            "bundle_schema",
            "unsupported schema or shortcut label",
        ));
    }
    Ok(bundle)
}

/// Verifies `bundle` for this chain and subnet. Every refusal happens before
/// any row exists.
pub fn verify(
    bundle: &Bundle,
    keys: &TrustedKeys,
    genesis: &[u8; 32],
    netuid: u16,
) -> Result<VerifiedTarget, Refusal> {
    let manifest = Signed::<CohortManifest>::open(&unhex("manifest", &bundle.manifest)?, keys)
        .map_err(protocol("manifest_unverified"))?;
    let package = Signed::<ScorePackage>::open(&unhex("package", &bundle.package)?, keys)
        .map_err(protocol("package_unverified"))?;
    let target = Signed::<CalculationTarget>::open(&unhex("target", &bundle.target)?, keys)
        .map_err(protocol("target_unverified"))?;
    let index = Signed::<ReceiptIndex>::open(&unhex("receipt_index", &bundle.receipt_index)?, keys)
        .map_err(protocol("receipt_index_unverified"))?;
    if package.chain.genesis.0 != *genesis || package.chain.netuid != netuid {
        return Err(Refusal::new(
            "package_chain_mismatch",
            "the package names another chain or subnet",
        ));
    }
    package
        .check_manifest(&manifest)
        .map_err(protocol("package_manifest_mismatch"))?;
    target
        .check_package(&package)
        .map_err(protocol("target_package_mismatch"))?;
    package
        .check_receipts(&index)
        .map_err(protocol("receipt_index_mismatch"))?;
    if bundle.receipts.len() as u64 != index.count() {
        return Err(Refusal::new(
            "receipts_incomplete",
            format!(
                "{} objects for {} entries",
                bundle.receipts.len(),
                index.count()
            ),
        ));
    }
    let baseline = manifest.baseline_artifact;
    for (position, object) in bundle.receipts.iter().enumerate() {
        let receipt = index
            .verify_object(position, &unhex("receipts", object)?)
            .map_err(protocol("receipt_object_mismatch"))?;
        if receipt.artifact != baseline
            && manifest.artifacts.binary_search(&receipt.artifact).is_err()
        {
            return Err(Refusal::new(
                "receipt_artifact_unknown",
                "a receipt names an artifact outside the manifest",
            ));
        }
    }
    let target_digest = target.digest().map_err(protocol("target_unverified"))?;
    Ok(VerifiedTarget {
        receipt_count: index.count(),
        manifest,
        package,
        target,
        target_digest,
    })
}

/// The coordinator's own finalized registrations as read from chain storage.
pub fn snapshot(
    chain: &ChainBinding,
    block: u64,
    block_hash: [u8; 32],
    mut neurons: Vec<(u16, [u8; 32], u64)>,
) -> Result<RegistrationSnapshot, Refusal> {
    neurons.sort_by_key(|n| n.0);
    let snapshot = RegistrationSnapshot {
        chain: chain.clone(),
        block,
        block_hash: Digest(block_hash),
        neurons: neurons
            .into_iter()
            .map(|(uid, hotkey, registration_block)| NeuronIdentity {
                uid,
                hotkey: AccountId(hotkey),
                registration_block,
            })
            .collect(),
    };
    snapshot
        .validate()
        .map_err(protocol("registration_snapshot_invalid"))?;
    Ok(snapshot)
}

/// The single dispatch point from a package's frozen reward rule to its row
/// calculation. The rule is pinned by the manifest, so a comparison never
/// changes rule midway.
pub fn derive_row(
    verified: &VerifiedTarget,
    snapshot: &RegistrationSnapshot,
) -> Result<WeightRow, Refusal> {
    match verified.package.reward_rule {
        RewardRule::TestnetRewardV0 => {
            slop_ninja_protocol::reward::derive_row(&verified.target, &verified.package, snapshot)
                .map_err(protocol("row_refused"))
        }
        // Version-1 packages carry only `testnet-reward-v0`; `reward-v1` rows
        // come from the version-2 records of a reward-v1 bundle ([`verify_v2`]).
        RewardRule::RewardV2 => Err(Refusal::new(
            "row_refused",
            "reward-v1 rows come from a reward-v1 bundle",
        )),
    }
}

/// Public row summary kept in the journal and on `/status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowSummary {
    pub digest: String,
    pub rule: String,
    pub registration_block: u64,
    /// The canonical row (a `reward-v1` row sums to 65,535).
    pub entries: Vec<(u16, u16)>,
    /// `reward-v1`: the committed form, which rtao and the chain both keep
    /// (`WeightRowV2::chain_values`: the row max-upscaled to 65,535). Absent
    /// for a `testnet-reward-v0` row, whose largest entry is already 65,535.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<Vec<(u16, u16)>>,
    /// `reward-v1`: the hotkey the finalized snapshot read at each entry's
    /// UID, in entry order (hex), for the signed coordinator row validators
    /// copy (`signing::rows`). Absent for a row derived by an older build,
    /// which then gets no row record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkeys: Option<Vec<String>>,
    /// `reward-v1`: the registration block the snapshot read at each
    /// entry's UID, in entry order, for the same record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_blocks: Option<Vec<u64>>,
    /// `reward-v1`: the snapshot's owner hotkey (hex): the entry paying it
    /// is the burn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_hotkey: Option<String>,
}

impl RowSummary {
    /// The values committed on chain and compared with a reveal.
    pub fn committed(&self) -> &[(u16, u16)] {
        self.chain.as_deref().unwrap_or(&self.entries)
    }
}

pub fn summary(row: &WeightRow) -> Result<RowSummary, Refusal> {
    Ok(RowSummary {
        digest: row.digest().map_err(protocol("row_refused"))?.to_string(),
        rule: row.reward_rule.name().to_owned(),
        registration_block: row.registration_block,
        entries: row.entries.iter().map(|e| (e.uid, e.weight)).collect(),
        chain: None,
        hotkeys: None,
        registration_blocks: None,
        owner_hotkey: None,
    })
}


// ------------------------------------------------------------- reward-v1

/// The reward-v1 bundle (T12 shortcut, testnet): the active target's records
/// and the authorization that lets this validator commit it. The operator
/// writes it with `eval_verify` from the receipts bucket.
pub const BUNDLE_SCHEMA_V2: &str = "slop_ninja.reward_v1_bundle/v1";
/// Largest reward-v1 bundle the coordinator stores: one Durable Object SQLite
/// value holds at most 2 MB (fan-out LIMITS.md, Durable Object storage).
pub const MAX_BUNDLE_V2_BYTES: usize = 2_000_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceHex {
    pub manifest: String,
    pub package: String,
    pub target: String,
    pub closure: String,
    /// The signed receipt index; the coordinator's own bundle may omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_index: Option<String>,
    pub published_contents: String,
    pub fingerprint_index: String,
}

/// A later no-target cohort that carries the target: its signed manifest and
/// package, its signed receipt index (the coordinator's own form may omit
/// it), and its published index as of its manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarrierHex {
    pub manifest: String,
    pub package: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_index: Option<String>,
    pub published_contents: String,
    pub fingerprint_index: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationHex {
    /// `launch` or `cohort`.
    pub kind: String,
    pub authorization: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renewal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier: Option<CarrierHex>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleV2 {
    pub schema: String,
    /// Always `T12`.
    pub shortcut: String,
    /// `None`: the launch burn row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceHex>,
    pub authorization: AuthorizationHex,
    /// Receipt chunk objects (hex), when the holder retains them inline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receipt_chunks: Vec<String>,
    /// The carrying cohort's receipt chunk objects (hex).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carrier_receipt_chunks: Vec<String>,
}

pub fn parse_v2(bytes: &[u8]) -> Result<BundleV2, Refusal> {
    if bytes.len() > MAX_BUNDLE_V2_BYTES {
        return Err(Refusal::new("bundle_too_large", "a reward-v1 bundle exceeds one stored value"));
    }
    let b: BundleV2 = serde_json::from_slice(bytes).map_err(|e| Refusal::new("bundle_malformed", e.to_string()))?;
    if b.schema != BUNDLE_SCHEMA_V2 || b.shortcut != "T12" {
        return Err(Refusal::new("bundle_schema", "unsupported schema or shortcut label"));
    }
    Ok(b)
}

/// Whether bundle bytes are the reward-v1 form.
pub fn is_v2(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| v.get("schema").and_then(|s| s.as_str()).map(|s| s == BUNDLE_SCHEMA_V2))
        .unwrap_or(false)
}

/// Verifies a reward-v1 bundle for this chain and subnet, recomputing the
/// package's outcome (and a carrier's), for a holder whose own checked
/// history holds `held` active. Every refusal happens before any row exists.
pub fn verify_v2(
    b: &BundleV2,
    keys: &TrustedKeys,
    genesis: &[u8; 32],
    netuid: u16,
    held: &slop_ninja_protocol::ActiveTarget,
) -> Result<slop_ninja_protocol::VerifiedTarget, Refusal> {
    let chain = ChainBinding { genesis: Digest(*genesis), netuid, mechanism: 0 };
    let src = match &b.source {
        Some(s) => Some((
            unhex("manifest", &s.manifest)?,
            unhex("package", &s.package)?,
            unhex("target", &s.target)?,
            unhex("closure", &s.closure)?,
            s.receipt_index.as_deref().map(|r| unhex("receipt_index", r)).transpose()?,
            unhex("published_contents", &s.published_contents)?,
            unhex("fingerprint_index", &s.fingerprint_index)?,
        )),
        None => None,
    };
    let source = src.as_ref().map(|(m, p, t, c, r, pc, fi)| slop_ninja_protocol::Source {
        manifest: m,
        package: p,
        target: t,
        closure: c,
        receipt_index: r.as_deref(),
        published_contents: pc,
        fingerprint_index: fi,
    });
    let auth = unhex("authorization", &b.authorization.authorization)?;
    let renewal = b.authorization.renewal.as_deref().map(|r| unhex("renewal", r)).transpose()?;
    let carrier = match &b.authorization.carrier {
        Some(c) => Some((
            unhex("carrier manifest", &c.manifest)?,
            unhex("carrier package", &c.package)?,
            c.receipt_index.as_deref().map(|r| unhex("carrier receipt_index", r)).transpose()?,
            unhex("carrier published_contents", &c.published_contents)?,
            unhex("carrier fingerprint_index", &c.fingerprint_index)?,
        )),
        None => None,
    };
    let authorizing = match b.authorization.kind.as_str() {
        "launch" => slop_ninja_protocol::Authorizing::Launch { authorization: &auth, renewal: renewal.as_deref() },
        "cohort" => slop_ninja_protocol::Authorizing::Cohort {
            authorization: &auth,
            renewal: renewal.as_deref(),
            carrier: carrier.as_ref().map(|(m, p, r, pc, fi)| slop_ninja_protocol::Carrier {
                manifest: m,
                package: p,
                receipt_index: r.as_deref(),
                published_contents: pc,
                fingerprint_index: fi,
            }),
        },
        _ => return Err(Refusal::new("bundle_schema", "authorization kind")),
    };
    let v = slop_ninja_protocol::target_bundle::verify(&chain, keys, held, source, authorizing)
        .map_err(protocol("bundle_unverified"))?;
    if !b.receipt_chunks.is_empty() {
        let chunks = b.receipt_chunks.iter().map(|c| unhex("receipt_chunks", c)).collect::<Result<Vec<_>, _>>()?;
        let refs: Vec<&[u8]> = chunks.iter().map(Vec::as_slice).collect();
        v.verify_receipts(&refs).map_err(protocol("receipt_object_mismatch"))?;
    }
    if !b.carrier_receipt_chunks.is_empty() {
        let chunks =
            b.carrier_receipt_chunks.iter().map(|c| unhex("carrier_receipt_chunks", c)).collect::<Result<Vec<_>, _>>()?;
        let refs: Vec<&[u8]> = chunks.iter().map(Vec::as_slice).collect();
        v.verify_carrier_receipts(&refs).map_err(protocol("receipt_object_mismatch"))?;
    }
    Ok(v)
}

/// The coordinator's own finalized registrations and owner hotkey.
pub fn snapshot_v2(
    chain: &ChainBinding,
    block: u64,
    block_hash: [u8; 32],
    owner_hotkey: [u8; 32],
    mut neurons: Vec<(u16, [u8; 32], u64)>,
) -> Result<RegistrationSnapshotV2, Refusal> {
    neurons.sort_by_key(|n| n.0);
    let s = RegistrationSnapshotV2 {
        chain: chain.clone(),
        block,
        block_hash: Digest(block_hash),
        owner_hotkey: AccountId(owner_hotkey),
        neurons: neurons
            .into_iter()
            .map(|(uid, hotkey, registration_block)| NeuronIdentity { uid, hotkey: AccountId(hotkey), registration_block })
            .collect(),
    };
    s.validate().map_err(protocol("registration_snapshot_invalid"))?;
    Ok(s)
}

pub fn summary_v2(row: &WeightRowV2) -> Result<RowSummary, Refusal> {
    Ok(RowSummary {
        digest: row.digest().map_err(protocol("row_refused"))?.to_string(),
        rule: row.reward_rule.name().to_owned(),
        registration_block: row.registration_block,
        entries: row.entries.iter().map(|e| (e.uid, e.weight)).collect(),
        chain: Some(row.chain_values()),
        hotkeys: None,
        registration_blocks: None,
        owner_hotkey: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_protocol::TESTNET_GENESIS;

    #[test]
    fn malformed_and_mislabelled_bundles_are_refused_before_verification() {
        assert_eq!(parse(b"{").unwrap_err().code, "bundle_malformed");
        let wrong = serde_json::json!({
            "schema": BUNDLE_SCHEMA, "shortcut": "T13", "manifest": "", "package": "",
            "target": "", "receipt_index": "", "receipts": []
        });
        assert_eq!(
            parse(wrong.to_string().as_bytes()).unwrap_err().code,
            "bundle_schema"
        );
        let extra = serde_json::json!({
            "schema": BUNDLE_SCHEMA, "shortcut": "T12", "manifest": "", "package": "",
            "target": "", "receipt_index": "", "receipts": [], "row": []
        });
        assert_eq!(
            parse(extra.to_string().as_bytes()).unwrap_err().code,
            "bundle_malformed"
        );
        assert_eq!(
            parse(&vec![b' '; MAX_BUNDLE_BYTES + 1]).unwrap_err().code,
            "bundle_too_large"
        );
    }

    /// A two-UID reward-v1 row is committed and compared with its reveal in
    /// the max-upscaled form the chain stores; the summary keeps the
    /// canonical row and its digest.
    #[test]
    fn a_two_uid_reward_v1_row_is_committed_max_upscaled() {
        let row = WeightRowV2 {
            chain: ChainBinding { genesis: TESTNET_GENESIS, netuid: 601, mechanism: 0 },
            calculation_target: Digest([1; 32]),
            reward_rule: RewardRule::RewardV2,
            registration_block: 9,
            registration: Digest([2; 32]),
            owner_hotkey: AccountId([3; 32]),
            entries: vec![
                slop_ninja_protocol::RowEntry { uid: 33, weight: 35424 },
                slop_ninja_protocol::RowEntry { uid: 34, weight: 30111 },
            ],
        };
        let s = summary_v2(&row).unwrap();
        assert_eq!(s.entries, vec![(33, 35424), (34, 30111)]);
        assert_eq!(s.committed(), &[(33, 65535), (34, 55706)]);
        assert_eq!(s.digest, row.digest().unwrap().to_string());
        // A stored v0 summary without the field commits its entries as they are.
        let old: RowSummary =
            serde_json::from_str(r#"{"digest":"d","rule":"testnet-reward-v0","registration_block":1,"entries":[[2,65535]]}"#)
                .unwrap();
        assert_eq!(old.committed(), &[(2, 65535)]);
    }

    #[test]
    fn trusted_keys_must_be_distinct_32_byte_values() {
        let a = "11".repeat(32);
        let b = "22".repeat(32);
        let c = "33".repeat(32);
        assert!(trusted_keys(&a, &b, &c).is_ok());
        assert_eq!(
            trusted_keys(&a, &a, &c).unwrap_err().code,
            "trusted_keys_invalid"
        );
        assert_eq!(
            trusted_keys("00", &b, &c).unwrap_err().code,
            "trusted_keys_invalid"
        );
    }

    #[test]
    fn a_forged_record_is_refused_before_decoding() {
        let keys = trusted_keys(&"11".repeat(32), &"22".repeat(32), &"33".repeat(32)).unwrap();
        let bundle = Bundle {
            schema: BUNDLE_SCHEMA.into(),
            shortcut: "T12".into(),
            manifest: "00".repeat(200),
            package: String::new(),
            target: String::new(),
            receipt_index: String::new(),
            receipts: vec![],
        };
        let refusal = verify(&bundle, &keys, &TESTNET_GENESIS.0, 601).unwrap_err();
        assert_eq!(refusal.code, "manifest_unverified");
    }
}
