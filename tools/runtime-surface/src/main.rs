//! Offline extraction and comparison of the Subtensor runtime surface that
//! Slop Ninja's subnet participants use.
//!
//! The tool never contacts a chain. It reads metadata blobs that were fetched
//! separately (`Metadata_metadata_at_version` for V15 and V16) and writes a
//! compact JSON surface: pallet and call indices, argument types, the storage,
//! events and constants Slop Ninja reads, error tables, runtime APIs and the
//! signed/transaction extension lists. Two surfaces can then be diffed.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use blake2::{Blake2b, digest::consts::U32};
use codec::{Decode, Encode};
use frame_metadata::{RuntimeMetadata, RuntimeMetadataPrefixed, v15, v16};
use scale_info::{PortableRegistry, TypeDef, form::PortableForm};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

mod upgrade;

const SURFACE_SCHEMA: &str = "slop_ninja.runtime_surface.v1";
const DIFF_SCHEMA: &str = "slop_ninja.runtime_surface_diff.v1";
const NEEDED_SCHEMA: &str = "slop_ninja.runtime_needed.v1";
const PROVENANCE_SCHEMA: &str = "slop_ninja.runtime_provenance.v1";
const META_MAGIC: &[u8; 4] = b"meta";

/// Pallets whose error variants and constant values are listed; other
/// pallets record digests.
const FOCUS_PALLETS: &[&str] = &[
    "SubtensorModule",
    "AdminUtils",
    "Commitments",
    "Drand",
    "MevShield",
];

/// A call Slop Ninja's owner, coordinator, validators or miners submit.
struct NeededCall {
    pallet: &'static str,
    call: &'static str,
    purpose: &'static str,
    /// Dispatch origin read from Subtensor v486 (`72323e13`) source, which
    /// metadata does not carry.
    origin: &'static str,
}

const fn needed(
    pallet: &'static str,
    call: &'static str,
    purpose: &'static str,
    origin: &'static str,
) -> NeededCall {
    NeededCall {
        pallet,
        call,
        purpose,
        origin,
    }
}

const SIGNED_HOTKEY: &str = "signed hotkey";
const SIGNED_COLDKEY: &str = "signed coldkey";
const OWNER_LIMITED: &str = "subnet owner coldkey (owner rate limit, admin freeze window) or root";
const OWNER_LIMITED_ROOT_ON_NETUID_0: &str =
    "subnet owner coldkey (owner rate limit, admin freeze window) or root; root only on netuid 0";
const ROOT: &str = "root";

const NEEDED_CALLS: &[NeededCall] = &[
    needed(
        "SubtensorModule",
        "commit_timelocked_weights",
        "timelocked (CRv4) weight commit",
        SIGNED_HOTKEY,
    ),
    needed(
        "SubtensorModule",
        "commit_timelocked_mechanism_weights",
        "timelocked commit with mechanism id",
        SIGNED_HOTKEY,
    ),
    needed(
        "SubtensorModule",
        "commit_crv3_weights",
        "legacy CRv3 commit name",
        SIGNED_HOTKEY,
    ),
    needed(
        "SubtensorModule",
        "set_weights",
        "plaintext weights; refused while commit-reveal is on",
        SIGNED_HOTKEY,
    ),
    needed(
        "SubtensorModule",
        "set_mechanism_weights",
        "plaintext weights with mechanism id",
        SIGNED_HOTKEY,
    ),
    needed(
        "SubtensorModule",
        "commit_weights",
        "hash commit-reveal (CRv2) commit",
        SIGNED_HOTKEY,
    ),
    needed(
        "SubtensorModule",
        "reveal_weights",
        "hash commit-reveal (CRv2) reveal",
        SIGNED_HOTKEY,
    ),
    needed(
        "SubtensorModule",
        "register",
        "legacy alias of burned_register; PoW arguments ignored",
        "signed coldkey (pays burn)",
    ),
    needed(
        "SubtensorModule",
        "pow_register",
        "owner-enabled PoW registration without burn",
        SIGNED_COLDKEY,
    ),
    needed(
        "SubtensorModule",
        "burned_register",
        "burn registration of miners and validators",
        SIGNED_COLDKEY,
    ),
    needed(
        "SubtensorModule",
        "register_network",
        "subnet registration",
        SIGNED_COLDKEY,
    ),
    needed(
        "SubtensorModule",
        "register_network_with_identity",
        "subnet registration with identity",
        SIGNED_COLDKEY,
    ),
    needed(
        "SubtensorModule",
        "start_call",
        "owner start of subnet emissions",
        "signed subnet owner coldkey",
    ),
    needed(
        "SubtensorModule",
        "serve_axon",
        "axon announcement",
        SIGNED_HOTKEY,
    ),
    needed(
        "SubtensorModule",
        "serve_axon_tls",
        "axon announcement with certificate",
        SIGNED_HOTKEY,
    ),
    needed(
        "SubtensorModule",
        "add_stake",
        "validator stake",
        SIGNED_COLDKEY,
    ),
    needed(
        "AdminUtils",
        "sudo_set_commit_reveal_weights_enabled",
        "CommitRevealWeightsEnabled",
        OWNER_LIMITED,
    ),
    needed(
        "AdminUtils",
        "sudo_set_commit_reveal_weights_interval",
        "RevealPeriodEpochs",
        OWNER_LIMITED,
    ),
    needed(
        "AdminUtils",
        "sudo_set_weights_version_key",
        "WeightsVersionKey",
        OWNER_LIMITED,
    ),
    needed(
        "AdminUtils",
        "sudo_set_activity_cutoff",
        "ActivityCutoff",
        OWNER_LIMITED,
    ),
    needed(
        "AdminUtils",
        "sudo_set_tempo",
        "Tempo; owner values bounded to MinTempo..MaxTempo",
        OWNER_LIMITED,
    ),
    needed(
        "AdminUtils",
        "sudo_set_commit_reveal_version",
        "CommitRevealWeightsVersion",
        ROOT,
    ),
    needed(
        "AdminUtils",
        "sudo_set_weights_set_rate_limit",
        "WeightsSetRateLimit",
        ROOT,
    ),
    needed(
        "AdminUtils",
        "sudo_set_network_registration_allowed",
        "NetworkRegistrationAllowed (burned registration); disabling requires NetworkPowRegistrationAllowed",
        OWNER_LIMITED_ROOT_ON_NETUID_0,
    ),
    needed(
        "AdminUtils",
        "sudo_set_network_pow_registration_allowed",
        "NetworkPowRegistrationAllowed; disabling requires NetworkRegistrationAllowed; refused on netuid 0",
        OWNER_LIMITED,
    ),
    needed(
        "AdminUtils",
        "sudo_set_epoch_consensus",
        "SubnetEpochConsensus (Yuma or Null); sets the timelocked commit bound",
        OWNER_LIMITED,
    ),
    needed(
        "Commitments",
        "set_commitment",
        "model-artifact admission digest",
        "signed hotkey registered on the subnet",
    ),
];

const NEEDED_STORAGE: &[(&str, &str)] = &[
    ("SubtensorModule", "TimelockedWeightCommits"),
    ("SubtensorModule", "CRV3WeightCommitsV2"),
    ("SubtensorModule", "WeightCommits"),
    ("SubtensorModule", "RevealPeriodEpochs"),
    ("SubtensorModule", "CommitRevealWeightsEnabled"),
    ("SubtensorModule", "CommitRevealWeightsVersion"),
    ("SubtensorModule", "Tempo"),
    ("SubtensorModule", "Weights"),
    ("SubtensorModule", "WeightsSetRateLimit"),
    ("SubtensorModule", "WeightsVersionKey"),
    ("SubtensorModule", "LastUpdate"),
    ("SubtensorModule", "Uids"),
    ("SubtensorModule", "Keys"),
    ("SubtensorModule", "BlockAtRegistration"),
    ("SubtensorModule", "Incentive"),
    ("SubtensorModule", "ActivityCutoff"),
    ("SubtensorModule", "SubnetOwner"),
    ("SubtensorModule", "SubnetOwnerHotkey"),
    ("SubtensorModule", "FirstEmissionBlockNumber"),
    ("SubtensorModule", "NetworkRegistrationAllowed"),
    ("SubtensorModule", "NetworkPowRegistrationAllowed"),
    ("SubtensorModule", "SubnetEpochConsensus"),
    ("SubtensorModule", "Axons"),
    ("SubtensorModule", "MechanismCountCurrent"),
    ("SubtensorModule", "LastMechansimStepBlock"),
    ("SubtensorModule", "TransactionKeyLastBlock"),
    ("Commitments", "CommitmentOf"),
    ("Commitments", "LastCommitment"),
    ("Commitments", "MaxSpace"),
    ("Commitments", "UsedSpaceOf"),
    ("Drand", "LastStoredRound"),
    ("Drand", "Pulses"),
    // Read by name by the coordinator (signing preflight, rows, roster), the eval and upload
    // Workers and rtao (2026-11-06 runtime-watch code-derived list).
    ("System", "Account"),
    ("System", "Events"),
    ("System", "LastRuntimeUpgrade"),
    ("Timestamp", "Now"),
    ("SubtensorModule", "NetworksAdded"),
    ("SubtensorModule", "PendingEpochAt"),
    ("SubtensorModule", "BlocksSinceLastStep"),
    ("SubtensorModule", "LastEpochBlock"),
    ("SubtensorModule", "SubnetEpochIndex"),
    ("SubtensorModule", "SubnetworkN"),
    ("SubtensorModule", "MinAllowedWeights"),
    ("SubtensorModule", "MaxWeightsLimit"),
    ("SubtensorModule", "Owner"),
];

const NEEDED_EVENTS: &[(&str, &str)] = &[
    ("SubtensorModule", "TimelockedWeightsCommitted"),
    ("SubtensorModule", "TimelockedWeightsRevealed"),
    ("SubtensorModule", "CRV3WeightsCommitted"),
    ("SubtensorModule", "CRV3WeightsRevealed"),
    ("SubtensorModule", "WeightsSet"),
    ("SubtensorModule", "NeuronRegistered"),
    ("SubtensorModule", "BulkNeuronsRegistered"),
    ("SubtensorModule", "NetworkAdded"),
    ("SubtensorModule", "FirstEmissionBlockNumberSet"),
    ("SubtensorModule", "AxonServed"),
    ("SubtensorModule", "CommitRevealEnabled"),
    ("SubtensorModule", "CommitRevealPeriodsSet"),
    ("Commitments", "Commitment"),
    ("System", "ExtrinsicSuccess"),
    ("System", "ExtrinsicFailed"),
];

/// Runtime API methods the participants call (`state_call` or rtao's runtime API reads).
const NEEDED_RUNTIME_APIS: &[&str] = &[
    "Core.version",
    "Metadata.metadata_at_version",
    "Metadata.metadata_versions",
    "SubnetInfoRuntimeApi.get_selective_mechagraph",
    "SubnetInfoRuntimeApi.get_selective_metagraph",
    "TaggedTransactionQueue.validate_transaction",
    "AccountNonceApi.account_nonce",
];

/// Types that wrap whole runtime enums are recorded by name only.
const OPAQUE_PATHS: &[&str] = &[
    "RuntimeCall",
    "RuntimeEvent",
    "RuntimeOrigin",
    "OriginCaller",
];

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match refs.as_slice() {
        ["unwrap", input, output] => unwrap_rpc(Path::new(input), Path::new(output)),
        ["surface", label, v15_path, v16_path] => {
            let v16 = if *v16_path == "-" {
                None
            } else {
                Some(fs::read(v16_path)?)
            };
            let surface = surface(label, &fs::read(v15_path)?, v16.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&surface)?);
            Ok(())
        }
        ["provenance", network, endpoint, dir] => {
            let out = provenance(network, endpoint, Path::new(dir))?;
            println!("{}", serde_json::to_string_pretty(&out)?);
            Ok(())
        }
        ["needed", surface, provenance, diffs @ ..] => {
            let read = |p: &str| -> Result<Value> { Ok(serde_json::from_slice(&fs::read(p)?)?) };
            let diffs = diffs.iter().map(|p| read(p)).collect::<Result<Vec<_>>>()?;
            let out = needed_table(&read(surface)?, &read(provenance)?, &diffs)?;
            println!("{}", serde_json::to_string_pretty(&out)?);
            Ok(())
        }
        ["upgrade-scan", v15_path, input] => {
            let input: Value = serde_json::from_slice(&fs::read(input)?)?;
            let out = upgrade::scan(&fs::read(v15_path)?, &input)?;
            println!("{}", serde_json::to_string_pretty(&out)?);
            Ok(())
        }
        ["storage-prefix", pallet, item] => {
            println!("{}", upgrade::storage_prefix(pallet, item));
            Ok(())
        }
        ["diff", old, new] => {
            let old: Value = serde_json::from_slice(&fs::read(old)?)?;
            let new: Value = serde_json::from_slice(&fs::read(new)?)?;
            println!("{}", serde_json::to_string_pretty(&diff(&old, &new)?)?);
            Ok(())
        }
        _ => bail!(
            "usage: slop_ninja_runtime_surface unwrap RPC.json OUT.scale\n       \
             slop_ninja_runtime_surface surface LABEL V15.scale V16.scale|-\n       \
             slop_ninja_runtime_surface diff OLD.json NEW.json\n       \
             slop_ninja_runtime_surface provenance NETWORK ENDPOINT RPC_DIR\n       \
             slop_ninja_runtime_surface needed SURFACE.json PROVENANCE.json [DIFF.json...]\n       \
             slop_ninja_runtime_surface upgrade-scan V15.scale STORAGE.json\n       \
             slop_ninja_runtime_surface storage-prefix PALLET ITEM"
        ),
    }
}

/// Converts a saved JSON-RPC response into raw prefixed metadata bytes. Accepts
/// a `state_call` `Option<OpaqueMetadata>` envelope or a `state_getMetadata`
/// result.
fn unwrap_rpc(input: &Path, output: &Path) -> Result<()> {
    let response: Value = serde_json::from_slice(&fs::read(input)?)?;
    let text = response["result"]
        .as_str()
        .context("RPC response has no string result")?;
    let bytes = hex::decode(text.trim_start_matches("0x"))?;
    let raw = raw_metadata(&bytes)?;
    fs::write(output, &raw)?;
    eprintln!("{} bytes sha256 {}", raw.len(), sha256_hex(&raw));
    Ok(())
}

fn raw_metadata(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.starts_with(META_MAGIC) {
        return Ok(bytes.to_vec());
    }
    let option = Option::<Vec<u8>>::decode(&mut &*bytes).context("not an Option envelope")?;
    ensure!(
        option.encode() == bytes,
        "metadata envelope is not canonical"
    );
    let raw = option.context("runtime returned None for the metadata version")?;
    ensure!(raw.starts_with(META_MAGIC), "metadata magic is missing");
    Ok(raw)
}

fn decode(bytes: &[u8]) -> Result<RuntimeMetadata> {
    ensure!(bytes.starts_with(META_MAGIC), "metadata magic is missing");
    let prefixed = RuntimeMetadataPrefixed::decode(&mut &*bytes)?;
    Ok(prefixed.1)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn digest(text: &str) -> String {
    sha256_hex(text.as_bytes())[..16].to_owned()
}

#[derive(Clone, Copy)]
struct Types<'a>(&'a PortableRegistry);

impl Types<'_> {
    fn ty(&self, id: u32) -> &scale_info::Type<PortableForm> {
        self.0.resolve(id).expect("metadata type id resolves")
    }

    /// Short readable type name, for example `BoundedVec<u8>`.
    fn name(&self, id: u32) -> String {
        let ty = self.ty(id);
        let params: Vec<String> = ty
            .type_params
            .iter()
            .filter_map(|p| p.ty.map(|t| self.name(t.id)))
            .collect();
        let generic = |base: &str| {
            if params.is_empty() {
                base.to_owned()
            } else {
                format!("{base}<{}>", params.join(", "))
            }
        };
        match &ty.type_def {
            TypeDef::Composite(c) if ty.path.segments.is_empty() => {
                let fields: Vec<String> = c
                    .fields
                    .iter()
                    .map(|f| match &f.name {
                        Some(n) => format!("{n}: {}", self.name(f.ty.id)),
                        None => self.name(f.ty.id),
                    })
                    .collect();
                format!("{{{}}}", fields.join(", "))
            }
            TypeDef::Composite(_) | TypeDef::Variant(_) => {
                generic(ty.path.segments.last().map_or("?", String::as_str))
            }
            TypeDef::Sequence(s) => format!("Vec<{}>", self.name(s.type_param.id)),
            TypeDef::Array(a) => format!("[{}; {}]", self.name(a.type_param.id), a.len),
            TypeDef::Tuple(t) => {
                let items: Vec<String> = t.fields.iter().map(|f| self.name(f.id)).collect();
                format!("({})", items.join(", "))
            }
            TypeDef::Primitive(p) => format!("{p:?}").to_lowercase(),
            TypeDef::Compact(c) => format!("Compact<{}>", self.name(c.type_param.id)),
            TypeDef::BitSequence(_) => "BitVec".to_owned(),
        }
    }

    /// Canonical structural encoding shape. Two types with equal shapes encode
    /// identically; runtime-wide enums stop at their path.
    fn shape(&self, id: u32, stack: &mut Vec<u32>) -> String {
        let ty = self.ty(id);
        let path = ty.path.segments.join("::");
        if ty
            .path
            .segments
            .last()
            .is_some_and(|last| OPAQUE_PATHS.contains(&last.as_str()))
        {
            return format!("opaque {path}");
        }
        if stack.contains(&id) {
            return format!("rec {path}");
        }
        stack.push(id);
        let shape = match &ty.type_def {
            TypeDef::Composite(c) => {
                let fields: Vec<String> = c
                    .fields
                    .iter()
                    .map(|f| {
                        format!(
                            "{}:{}",
                            f.name.as_deref().unwrap_or("_"),
                            self.shape(f.ty.id, stack)
                        )
                    })
                    .collect();
                format!("{path}{{{}}}", fields.join(","))
            }
            TypeDef::Variant(v) => {
                let variants: Vec<String> = v
                    .variants
                    .iter()
                    .map(|var| {
                        let fields: Vec<String> = var
                            .fields
                            .iter()
                            .map(|f| self.shape(f.ty.id, stack))
                            .collect();
                        format!("{}#{}({})", var.name, var.index, fields.join(","))
                    })
                    .collect();
                format!("{path}<{}>", variants.join("|"))
            }
            TypeDef::Sequence(s) => format!("[{}]", self.shape(s.type_param.id, stack)),
            TypeDef::Array(a) => format!("[{};{}]", self.shape(a.type_param.id, stack), a.len),
            TypeDef::Tuple(t) => {
                let items: Vec<String> = t.fields.iter().map(|f| self.shape(f.id, stack)).collect();
                format!("({})", items.join(","))
            }
            TypeDef::Primitive(p) => format!("{p:?}").to_lowercase(),
            TypeDef::Compact(c) => format!("compact({})", self.shape(c.type_param.id, stack)),
            TypeDef::BitSequence(b) => format!(
                "bits({},{})",
                self.shape(b.bit_store_type.id, stack),
                self.shape(b.bit_order_type.id, stack)
            ),
        };
        stack.pop();
        shape
    }

    fn shape_digest(&self, id: u32) -> String {
        digest(&self.shape(id, &mut Vec::new()))
    }

    /// Type name plus a structural digest; the full shape is included when short.
    fn describe(&self, id: u32) -> Value {
        let shape = self.shape(id, &mut Vec::new());
        let mut out = json!({"type": self.name(id), "shape_sha256_16": digest(&shape)});
        if shape.len() <= 240 {
            out["shape"] = json!(shape);
        }
        out
    }

    fn variants(&self, id: u32) -> &[scale_info::Variant<PortableForm>] {
        match &self.ty(id).type_def {
            TypeDef::Variant(v) => &v.variants,
            _ => &[],
        }
    }
}

fn needed_call(pallet: &str, call: &str) -> Option<&'static NeededCall> {
    NEEDED_CALLS
        .iter()
        .find(|n| n.pallet == pallet && n.call == call)
}

/// Builds the surface from V15 (calls, storage, extensions with additional
/// signed data) and cross-checks the call table against V16. Without V16 (a
/// runtime that does not serve it, or a test with V15 fixtures only) the
/// runtime APIs come from V15, without API versions, and the transaction
/// extension sections are empty; compare such a surface only with another one
/// built the same way.
fn surface(label: &str, v15_bytes: &[u8], v16_bytes: Option<&[u8]>) -> Result<Value> {
    let RuntimeMetadata::V15(m15) = decode(v15_bytes)? else {
        bail!("first blob is not V15 metadata");
    };
    let m16 = match v16_bytes {
        Some(b) => {
            let RuntimeMetadata::V16(m16) = decode(b)? else {
                bail!("second blob is not V16 metadata");
            };
            Some(m16)
        }
        None => None,
    };
    let types = Types(&m15.types);

    let mut pallets = Map::new();
    let mut calls = Map::new();
    let mut storage = Map::new();
    let mut storage_digests = Map::new();
    let mut events = Map::new();
    let mut event_digests = Map::new();
    let mut errors = Map::new();
    let mut constants = Map::new();
    for pallet in &m15.pallets {
        let focus = FOCUS_PALLETS.contains(&pallet.name.as_str());
        let call_count = pallet
            .calls
            .as_ref()
            .map_or(0, |c| types.variants(c.ty.id).len());
        pallets.insert(
            pallet.name.clone(),
            json!({"index": pallet.index, "calls": call_count}),
        );
        if let Some(c) = &pallet.calls {
            for var in types.variants(c.ty.id) {
                let args: Vec<Value> = var
                    .fields
                    .iter()
                    .map(|f| json!([f.name.as_deref().unwrap_or("_"), types.name(f.ty.id)]))
                    .collect();
                let shape: Vec<String> = var
                    .fields
                    .iter()
                    .map(|f| types.shape(f.ty.id, &mut Vec::new()))
                    .collect();
                let mut entry = json!({
                    "index": [pallet.index, var.index],
                    "args": args,
                    "args_sha256_16": digest(&shape.join(",")),
                });
                if let Some(need) = needed_call(&pallet.name, &var.name) {
                    entry["needed"] = json!(need.purpose);
                    entry["origin"] = json!(need.origin);
                    entry["arg_shapes"] =
                        var.fields.iter().map(|f| types.describe(f.ty.id)).collect();
                }
                calls.insert(format!("{}.{}", pallet.name, var.name), entry);
            }
        }
        if let Some(s) = &pallet.storage {
            for entry in &s.entries {
                let key = format!("{}.{}", pallet.name, entry.name);
                let (shape, detail) = storage_entry(&types, entry);
                let needed = NEEDED_STORAGE
                    .iter()
                    .any(|(p, n)| *p == pallet.name && *n == entry.name);
                storage_digests.insert(key.clone(), json!(digest(&shape)));
                if needed {
                    storage.insert(key, detail);
                }
            }
        }
        if let Some(e) = &pallet.event {
            for var in types.variants(e.ty.id) {
                let key = format!("{}.{}", pallet.name, var.name);
                let fields: Vec<Value> = var
                    .fields
                    .iter()
                    .map(|f| json!([f.name.as_deref().unwrap_or("_"), types.name(f.ty.id)]))
                    .collect();
                let shape: Vec<String> = var
                    .fields
                    .iter()
                    .map(|f| types.shape(f.ty.id, &mut Vec::new()))
                    .collect();
                let shape = format!("#{}({})", var.index, shape.join(","));
                event_digests.insert(key.clone(), json!(digest(&shape)));
                if NEEDED_EVENTS
                    .iter()
                    .any(|(p, n)| *p == pallet.name && *n == var.name)
                {
                    events.insert(
                        key,
                        json!({"index": [pallet.index, var.index], "fields": fields,
                               "fields_sha256_16": digest(&shape)}),
                    );
                }
            }
        }
        if let Some(e) = &pallet.error {
            let names: Vec<&str> = types
                .variants(e.ty.id)
                .iter()
                .map(|v| v.name.as_str())
                .collect();
            let mut entry = json!({"sha256_16": digest(&names.join(","))});
            if focus {
                entry["variants"] = json!(names);
            }
            errors.insert(pallet.name.clone(), entry);
        }
        for c in &pallet.constants {
            // Account-id values and other pallets' values are digested.
            let ty = types.name(c.ty.id);
            let value = if focus && ty != "AccountId32" {
                json!({"type": ty, "value": format!("0x{}", hex::encode(&c.value))})
            } else {
                json!({"type": ty, "value_sha256_16": &sha256_hex(&c.value)[..16]})
            };
            constants.insert(format!("{}.{}", pallet.name, c.name), value);
        }
    }

    for need in NEEDED_CALLS {
        let key = format!("{}.{}", need.pallet, need.call);
        if !calls.contains_key(&key) {
            calls.insert(key, json!({"absent": true}));
        }
    }

    if let Some(m16) = &m16 {
        cross_check_v16(&m15, m16)?;
    }

    let ext = &m15.extrinsic;
    let signed_extensions: Vec<Value> = ext
        .signed_extensions
        .iter()
        .map(|e| {
            json!({
                "identifier": e.identifier,
                "explicit": types.describe(e.ty.id),
                "additional_signed": types.describe(e.additional_signed.id),
            })
        })
        .collect();
    let (by_version, tx_extensions, apis) = match &m16 {
        Some(m16) => v16_sections(m16),
        None => (Map::new(), Vec::new(), v15_apis(&m15)),
    };
    let digest_of = |b: Option<&[u8]>, f: fn(&[u8]) -> String| b.map(f);

    Ok(json!({
        "schema": SURFACE_SCHEMA,
        "label": label,
        "metadata": {
            "v15_sha256": sha256_hex(v15_bytes),
            // rtao `compat.json` records this `blake2b-256-raw` metadata hash.
            "v15_blake2b_256": hex::encode(Blake2b::<U32>::digest(v15_bytes)),
            "v15_bytes": v15_bytes.len(),
            "v16_sha256": digest_of(v16_bytes, sha256_hex),
            "v16_blake2b_256": digest_of(v16_bytes, |b| hex::encode(Blake2b::<U32>::digest(b))),
            "v16_bytes": v16_bytes.map(<[u8]>::len),
        },
        "extrinsic": {
            "v15_version": ext.version,
            "v16_versions": m16.as_ref().map(|m| m.extrinsic.versions.clone()),
            "address": types.describe(ext.address_ty.id),
            "signature": types.describe(ext.signature_ty.id),
            "extra": types.describe(ext.extra_ty.id),
            "call_type": types.name(ext.call_ty.id),
        },
        "signed_extensions": signed_extensions,
        "transaction_extensions_by_version": by_version,
        "transaction_extensions": tx_extensions,
        "pallets": pallets,
        "calls": calls,
        "storage": storage,
        "storage_digests": storage_digests,
        "events": events,
        "event_digests": event_digests,
        "errors": errors,
        "constants": constants,
        "runtime_apis": apis,
    }))
}

/// Runtime APIs from V15 (no API versions there).
fn v15_apis(m15: &v15::RuntimeMetadataV15) -> Map<String, Value> {
    let types = Types(&m15.types);
    let mut apis = Map::new();
    for api in &m15.apis {
        for method in &api.methods {
            let inputs: Vec<Value> = method
                .inputs
                .iter()
                .map(|i| json!([i.name, types.name(i.ty.id)]))
                .collect();
            let mut shape: Vec<String> = method
                .inputs
                .iter()
                .map(|i| types.shape(i.ty.id, &mut Vec::new()))
                .collect();
            shape.push(types.shape(method.output.id, &mut Vec::new()));
            apis.insert(
                format!("{}.{}", api.name, method.name),
                json!({"api_version": null, "inputs": inputs,
                       "output": types.name(method.output.id),
                       "sha256_16": digest(&shape.join("->"))}),
            );
        }
    }
    apis
}

/// The V16-only sections: transaction extensions by version, their shapes and the
/// versioned runtime APIs.
fn v16_sections(
    m16: &v16::RuntimeMetadataV16,
) -> (Map<String, Value>, Vec<Value>, Map<String, Value>) {
    let types16 = Types(&m16.types);
    let mut by_version = Map::new();
    for (version, ids) in &m16.extrinsic.transaction_extensions_by_version {
        let names: Vec<&str> = ids
            .iter()
            .map(|i| {
                m16.extrinsic.transaction_extensions[i.0 as usize]
                    .identifier
                    .as_str()
            })
            .collect();
        by_version.insert(version.to_string(), json!(names));
    }
    let tx_extensions: Vec<Value> = m16
        .extrinsic
        .transaction_extensions
        .iter()
        .map(|e| {
            json!({
                "identifier": e.identifier,
                "explicit_sha256_16": types16.shape_digest(e.ty.id),
                "implicit_sha256_16": types16.shape_digest(e.implicit.id),
            })
        })
        .collect();

    let mut apis = Map::new();
    for api in &m16.apis {
        for method in &api.methods {
            let inputs: Vec<Value> = method
                .inputs
                .iter()
                .map(|i| json!([i.name, types16.name(i.ty.id)]))
                .collect();
            let mut shape: Vec<String> = method
                .inputs
                .iter()
                .map(|i| types16.shape(i.ty.id, &mut Vec::new()))
                .collect();
            shape.push(types16.shape(method.output.id, &mut Vec::new()));
            apis.insert(
                format!("{}.{}", api.name, method.name),
                json!({"api_version": api.version.0, "inputs": inputs,
                       "output": types16.name(method.output.id),
                       "sha256_16": digest(&shape.join("->"))}),
            );
        }
    }

    (by_version, tx_extensions, apis)
}

fn storage_entry(
    types: &Types<'_>,
    entry: &v15::StorageEntryMetadata<PortableForm>,
) -> (String, Value) {
    let modifier = format!("{:?}", entry.modifier);
    let default = format!("0x{}", hex::encode(&entry.default));
    match &entry.ty {
        v15::StorageEntryType::Plain(ty) => {
            let shape = format!(
                "{modifier} plain {} = {default}",
                types.shape(ty.id, &mut Vec::new())
            );
            (
                shape,
                json!({"modifier": modifier, "kind": "plain", "value": types.describe(ty.id),
                       "default": default}),
            )
        }
        v15::StorageEntryType::Map {
            hashers,
            key,
            value,
        } => {
            let hashers: Vec<String> = hashers.iter().map(|h| format!("{h:?}")).collect();
            let shape = format!(
                "{modifier} map {} {} -> {} = {default}",
                hashers.join("+"),
                types.shape(key.id, &mut Vec::new()),
                types.shape(value.id, &mut Vec::new())
            );
            (
                shape,
                json!({"modifier": modifier, "kind": "map", "hashers": hashers,
                       "key": types.describe(key.id), "value": types.describe(value.id),
                       "default": default}),
            )
        }
    }
}

/// V15 and V16 from one runtime must agree on pallet indices and call shapes.
fn cross_check_v16(m15: &v15::RuntimeMetadataV15, m16: &v16::RuntimeMetadataV16) -> Result<()> {
    let t15 = Types(&m15.types);
    let t16 = Types(&m16.types);
    let table15: BTreeSet<String> = m15
        .pallets
        .iter()
        .flat_map(|p| {
            let calls = p.calls.as_ref().map_or(&[][..], |c| t15.variants(c.ty.id));
            calls
                .iter()
                .map(move |v| call_row(&t15, &p.name, p.index, v))
        })
        .collect();
    let table16: BTreeSet<String> = m16
        .pallets
        .iter()
        .flat_map(|p| {
            let calls = p.calls.as_ref().map_or(&[][..], |c| t16.variants(c.ty.id));
            calls
                .iter()
                .map(move |v| call_row(&t16, &p.name, p.index, v))
        })
        .collect();
    ensure!(table15 == table16, "V15 and V16 call tables differ");
    Ok(())
}

fn call_row(
    types: &Types<'_>,
    pallet: &str,
    index: u8,
    var: &scale_info::Variant<PortableForm>,
) -> String {
    let shape: Vec<String> = var
        .fields
        .iter()
        .map(|f| types.shape(f.ty.id, &mut Vec::new()))
        .collect();
    format!(
        "{pallet}#{index}.{}#{}({})",
        var.name,
        var.index,
        shape.join(",")
    )
}

/// Compares two surfaces section by section and summarises the needed items.
fn diff(old: &Value, new: &Value) -> Result<Value> {
    for side in [old, new] {
        ensure!(
            side["schema"] == SURFACE_SCHEMA,
            "input is not a runtime surface"
        );
    }
    let mut sections = Map::new();
    for section in [
        "pallets",
        "calls",
        "storage",
        "storage_digests",
        "events",
        "event_digests",
        "errors",
        "constants",
        "runtime_apis",
        "transaction_extensions_by_version",
    ] {
        let a = old[section]
            .as_object()
            .context("section is not an object")?;
        let b = new[section]
            .as_object()
            .context("section is not an object")?;
        sections.insert(section.to_owned(), diff_maps(a, b));
    }
    for section in ["extrinsic", "signed_extensions", "transaction_extensions"] {
        if old[section] != new[section] {
            sections.insert(
                section.to_owned(),
                json!({"old": old[section], "new": new[section]}),
            );
        } else {
            sections.insert(section.to_owned(), json!("unchanged"));
        }
    }

    let mut needed = Map::new();
    for need in NEEDED_CALLS {
        let key = format!("{}.{}", need.pallet, need.call);
        needed.insert(
            format!("call {key}"),
            json!(status(&old["calls"][&key], &new["calls"][&key])),
        );
    }
    for (pallet, entry) in NEEDED_STORAGE {
        let key = format!("{pallet}.{entry}");
        needed.insert(
            format!("storage {key}"),
            json!(status(
                &old["storage_digests"][&key],
                &new["storage_digests"][&key]
            )),
        );
    }
    for (pallet, event) in NEEDED_EVENTS {
        let key = format!("{pallet}.{event}");
        needed.insert(
            format!("event {key}"),
            json!(status(
                &old["event_digests"][&key],
                &new["event_digests"][&key]
            )),
        );
    }
    for api in NEEDED_RUNTIME_APIS {
        needed.insert(
            format!("runtime_api {api}"),
            json!(status(
                &old["runtime_apis"][*api],
                &new["runtime_apis"][*api]
            )),
        );
    }
    // What every signed extrinsic encodes besides its call.
    for section in [
        "extrinsic",
        "signed_extensions",
        "transaction_extensions",
        "transaction_extensions_by_version",
    ] {
        let state = if old[section] == new[section] {
            "unchanged"
        } else {
            "changed"
        };
        needed.insert(format!("encoding {section}"), json!(state));
    }
    // Items that need a look: changed or removed (added and absent ones never break a read).
    let attention: Vec<&String> = needed
        .iter()
        .filter(|(_, v)| *v == "changed" || *v == "removed")
        .map(|(k, _)| k)
        .collect();
    Ok(json!({
        "schema": DIFF_SCHEMA,
        "old": {"label": old["label"], "metadata": old["metadata"]},
        "new": {"label": new["label"], "metadata": new["metadata"]},
        "needed_attention": attention,
        "needed": needed,
        "sections": sections,
    }))
}

fn status(old: &Value, new: &Value) -> &'static str {
    let present = |v: &Value| !v.is_null() && v.get("absent").is_none();
    match (present(old), present(new)) {
        (false, false) => "absent",
        (false, true) => "added",
        (true, false) => "removed",
        (true, true) => {
            // Compare the encoding-relevant fields only.
            let strip = |v: &Value| match v {
                Value::Object(m) => {
                    let mut m = m.clone();
                    m.remove("needed");
                    m.remove("origin");
                    Value::Object(m)
                }
                other => other.clone(),
            };
            if strip(old) == strip(new) {
                "unchanged"
            } else {
                "changed"
            }
        }
    }
}

fn diff_maps(old: &Map<String, Value>, new: &Map<String, Value>) -> Value {
    let added: Vec<&String> = new.keys().filter(|k| !old.contains_key(*k)).collect();
    let removed: Vec<&String> = old.keys().filter(|k| !new.contains_key(*k)).collect();
    let mut changed = Map::new();
    for (key, value) in old {
        if let Some(other) = new.get(key)
            && other != value
        {
            changed.insert(key.clone(), json!({"old": value, "new": other}));
        }
    }
    json!({"added": added, "removed": removed, "changed": changed})
}

/// Public chain identity of a saved metadata fetch. `RPC_DIR` holds the raw
/// responses of `chain_getFinalizedHead`, `chain_getBlockHash(0)`,
/// `chain_getHeader`, `state_getRuntimeVersion`, `state_getStorageHash(:code)`
/// and `state_getMetadata`, each at the finalized head.
fn provenance(network: &str, endpoint: &str, dir: &Path) -> Result<Value> {
    let result = |name: &str| -> Result<Value> {
        let response: Value = serde_json::from_slice(&fs::read(dir.join(name))?)?;
        ensure!(response.get("error").is_none(), "{name} holds an RPC error");
        Ok(response["result"].clone())
    };
    let header = result("header.json")?;
    let number = header["number"].as_str().context("header number")?;
    let number = u64::from_str_radix(number.trim_start_matches("0x"), 16)?;
    let version = result("runtime_version.json")?;
    let v14 = result("state_getMetadata.json")?;
    let v14 = raw_metadata(&hex::decode(
        v14.as_str()
            .context("metadata hex")?
            .trim_start_matches("0x"),
    )?)?;
    Ok(json!({
        "schema": PROVENANCE_SCHEMA,
        "network": network,
        "endpoint": endpoint,
        "fetched_at": fs::read_to_string(dir.join("fetched_at.txt"))?.trim(),
        "genesis": result("genesis.json")?,
        "finalized_head": result("finalized_head.json")?,
        "finalized_number": number,
        "spec_name": version["specName"],
        "spec_version": version["specVersion"],
        "transaction_version": version["transactionVersion"],
        "code_storage_hash": result("code_hash.json")?,
        "state_get_metadata_v14_sha256": sha256_hex(&v14),
    }))
}

/// Compact reference table of the items Slop Ninja needs, with their status
/// against each older surface.
fn needed_table(surface: &Value, provenance: &Value, diffs: &[Value]) -> Result<Value> {
    ensure!(
        surface["schema"] == SURFACE_SCHEMA,
        "input is not a runtime surface"
    );
    ensure!(
        provenance["schema"] == PROVENANCE_SCHEMA,
        "input is not provenance"
    );
    let mut comparisons = Map::new();
    for d in diffs {
        ensure!(d["schema"] == DIFF_SCHEMA, "input is not a surface diff");
        ensure!(
            d["new"]["metadata"] == surface["metadata"],
            "diff does not end at this surface"
        );
        let label = d["old"]["label"].as_str().context("diff label")?;
        let mut changed = Map::new();
        if let Some(sections) = d["sections"].as_object() {
            for (name, section) in sections {
                let is_empty = section == "unchanged"
                    || ["added", "removed"]
                        .iter()
                        .all(|k| section[k].as_array().is_some_and(Vec::is_empty))
                        && section["changed"].as_object().is_some_and(Map::is_empty);
                if !is_empty {
                    let summary = match section.get("changed").and_then(Value::as_object) {
                        Some(c) => json!({"added": section["added"], "removed": section["removed"],
                                          "changed": c.keys().collect::<Vec<_>>()}),
                        None => json!("changed"),
                    };
                    changed.insert(name.clone(), summary);
                }
            }
        }
        comparisons.insert(
            label.to_owned(),
            json!({"metadata": d["old"]["metadata"], "needed": d["needed"], "changed_sections": changed}),
        );
    }
    let mut calls = Map::new();
    for need in NEEDED_CALLS {
        let key = format!("{}.{}", need.pallet, need.call);
        let c = &surface["calls"][&key];
        let row = if c.get("absent").is_some() {
            json!({"absent": true, "purpose": need.purpose})
        } else {
            json!({"index": c["index"], "args": c["args"], "args_sha256_16": c["args_sha256_16"],
                   "origin": need.origin, "purpose": need.purpose})
        };
        calls.insert(key, row);
    }
    let extensions: Vec<Value> = surface["signed_extensions"]
        .as_array()
        .context("signed extensions")?
        .iter()
        .map(|e| {
            json!([
                e["identifier"],
                e["explicit"]["shape_sha256_16"],
                e["additional_signed"]["type"],
                e["additional_signed"]["shape_sha256_16"]
            ])
        })
        .collect();
    Ok(json!({
        "schema": NEEDED_SCHEMA,
        "runtime": provenance,
        "metadata": surface["metadata"],
        "pallets": surface["pallets"],
        "extrinsic": {
            "versions": surface["extrinsic"]["v16_versions"],
            "address": surface["extrinsic"]["address"]["type"],
            "signature": surface["extrinsic"]["signature"]["type"],
            "signed_extensions_columns": ["identifier", "explicit_shape_sha256_16",
                                          "additional_signed", "additional_shape_sha256_16"],
            "signed_extensions": extensions,
            "transaction_extensions_by_version": surface["transaction_extensions_by_version"],
        },
        "calls": calls,
        "storage": surface["storage"],
        "events": surface["events"],
        "comparisons": comparisons,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(calls: Value) -> Value {
        let mut s = json!({"schema": SURFACE_SCHEMA, "label": "x", "metadata": {}});
        for section in [
            "pallets",
            "storage",
            "storage_digests",
            "events",
            "event_digests",
            "errors",
            "constants",
            "runtime_apis",
            "transaction_extensions_by_version",
        ] {
            s[section] = json!({});
        }
        s["calls"] = calls;
        s["extrinsic"] = json!({});
        s["signed_extensions"] = json!([]);
        s["transaction_extensions"] = json!([]);
        s
    }

    #[test]
    fn diff_reports_needed_call_changes() {
        let old = sample(json!({
            "SubtensorModule.set_weights": {"index": [7, 0], "args": [["netuid", "u16"]]},
            "SubtensorModule.serve_axon": {"index": [7, 4], "args": []},
        }));
        let new = sample(json!({
            "SubtensorModule.set_weights": {"index": [7, 0], "args": [["netuid", "NetUid"]],
                                            "needed": "ignored in status"},
            "SubtensorModule.start_call": {"index": [7, 92], "args": []},
        }));
        let d = diff(&old, &new).unwrap();
        assert_eq!(d["needed"]["call SubtensorModule.set_weights"], "changed");
        assert_eq!(d["needed"]["call SubtensorModule.serve_axon"], "removed");
        assert_eq!(d["needed"]["call SubtensorModule.start_call"], "added");
        assert_eq!(d["needed"]["call SubtensorModule.register"], "absent");
        let attention: Vec<&str> = d["needed_attention"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(attention.contains(&"call SubtensorModule.set_weights"));
        assert!(attention.contains(&"call SubtensorModule.serve_axon"));
        assert!(!attention.contains(&"call SubtensorModule.start_call"));
        assert_eq!(d["needed"]["encoding signed_extensions"], "unchanged");
        assert_eq!(
            d["sections"]["calls"]["removed"][0],
            "SubtensorModule.serve_axon"
        );
        assert_eq!(d["sections"]["extrinsic"], "unchanged");
    }

    #[test]
    fn a_v15_only_surface_diffs_the_recorded_runtimes() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../subnet/fixtures");
        let a = fs::read(format!("{dir}/runtime-486/testnet-486.v15.scale")).unwrap();
        let b = fs::read(format!("{dir}/runtime-487/pr3294-f3654266.v15.scale")).unwrap();
        let a = surface("486", &a, None).unwrap();
        let b = surface("487", &b, None).unwrap();
        assert_eq!(a["metadata"]["v16_bytes"], Value::Null);
        assert!(a["runtime_apis"].get("Core.version").is_some());
        let d = diff(&a, &b).unwrap();
        for item in [
            "runtime_api Core.version",
            "runtime_api Metadata.metadata_at_version",
            "storage System.Events",
            "storage SubtensorModule.SubnetEpochIndex",
            "event System.ExtrinsicSuccess",
            "call SubtensorModule.commit_timelocked_mechanism_weights",
            "encoding signed_extensions",
        ] {
            assert_eq!(d["needed"][item], "unchanged", "{item}");
        }
    }

    #[test]
    fn needed_status_ignores_annotations() {
        let a = json!({"index": [7, 0], "needed": "a"});
        let b = json!({"index": [7, 0], "needed": "b"});
        assert_eq!(status(&a, &b), "unchanged");
        assert_eq!(status(&json!({"absent": true}), &Value::Null), "absent");
    }

    #[test]
    fn raw_metadata_unwraps_option_envelope() {
        let raw = b"meta\x0f".to_vec();
        let envelope = Some(raw.clone()).encode();
        assert_eq!(raw_metadata(&envelope).unwrap(), raw);
        assert_eq!(raw_metadata(&raw).unwrap(), raw);
        assert!(raw_metadata(&None::<Vec<u8>>.encode()).is_err());
    }

    #[test]
    fn committed_486_surface_lists_needed_calls() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../subnet/fixtures/runtime-486/surface-testnet-486.json"
        );
        let surface: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(surface["schema"], SURFACE_SCHEMA);
        for need in NEEDED_CALLS {
            let key = format!("{}.{}", need.pallet, need.call);
            assert!(surface["calls"].get(key).is_some());
        }
    }
}
