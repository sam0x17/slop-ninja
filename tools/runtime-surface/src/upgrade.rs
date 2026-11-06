//! Scan of chain storage for an announced runtime upgrade.
//!
//! Subtensor upgrades are root calls of `System` (`set_code`,
//! `set_code_without_checks`, `authorize_upgrade`,
//! `authorize_upgrade_without_checks`, `apply_authorized_upgrade`). A direct
//! sudo `set_code` is not visible before its block. An upgrade becomes visible
//! early only when it is
//! - authorized (`System.AuthorizedUpgrade` holds the code hash),
//! - scheduled (a `Scheduler.Agenda` entry whose call, inline or by preimage,
//!   contains one of those calls, at any nesting: sudo, batch, proxy), or
//! - noted as a preimage (`Preimage.PreimageFor` holds such a call).
//!
//! The tool never contacts a chain. The caller fetches the raw storage values
//! (hex, as `state_getStorage` and `state_queryStorageAt` return them) into a
//! JSON file and passes the V15 metadata of the runtime that wrote them. Every
//! type is resolved by name from that metadata: pallet and call indices, the
//! agenda and preimage value types and `RuntimeCall` itself. Nothing here
//! assumes a runtime version.

use anyhow::{Context, Result, bail, ensure};
use blake2::{Blake2b, digest::consts::U32};
use frame_metadata::{RuntimeMetadata, v15};
use scale_info::{PortableRegistry, TypeDef, TypeDefPrimitive, form::PortableForm};
use serde_json::{Value, json};
use sha2::Digest;

const SCAN_SCHEMA: &str = "slop_ninja.runtime_upgrade_scan.v1";

/// The `System` calls that replace or authorize the runtime code.
const UPGRADE_CALLS: &[&str] = &[
    "set_code",
    "set_code_without_checks",
    "authorize_upgrade",
    "authorize_upgrade_without_checks",
    "apply_authorized_upgrade",
];

/// `twox128(pallet) ++ twox128(item)`: the key prefix of a storage item.
pub fn storage_prefix(pallet: &str, item: &str) -> String {
    let mut out = twox_128(pallet.as_bytes()).to_vec();
    out.extend_from_slice(&twox_128(item.as_bytes()));
    format!("0x{}", hex::encode(out))
}

fn twox_128(data: &[u8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&xxh64(data, 0).to_le_bytes());
    out[8..].copy_from_slice(&xxh64(data, 1).to_le_bytes());
    out
}

const P1: u64 = 11_400_714_785_074_694_791;
const P2: u64 = 14_029_467_366_897_019_727;
const P3: u64 = 1_609_587_929_392_839_161;
const P4: u64 = 9_650_029_242_287_828_579;
const P5: u64 = 2_870_177_450_012_600_261;

fn round(acc: u64, lane: u64) -> u64 {
    acc.wrapping_add(lane.wrapping_mul(P2))
        .rotate_left(31)
        .wrapping_mul(P1)
}

fn merge(acc: u64, lane: u64) -> u64 {
    (acc ^ round(0, lane)).wrapping_mul(P1).wrapping_add(P4)
}

fn le64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().expect("eight bytes"))
}

/// XXH64, as `sp_core::hashing::twox_*` uses it.
fn xxh64(data: &[u8], seed: u64) -> u64 {
    let mut rest = data;
    let mut h = if data.len() >= 32 {
        let mut v = [
            seed.wrapping_add(P1).wrapping_add(P2),
            seed.wrapping_add(P2),
            seed,
            seed.wrapping_sub(P1),
        ];
        while rest.len() >= 32 {
            for (i, lane) in v.iter_mut().enumerate() {
                *lane = round(*lane, le64(&rest[i * 8..]));
            }
            rest = &rest[32..];
        }
        let mut h = v[0]
            .rotate_left(1)
            .wrapping_add(v[1].rotate_left(7))
            .wrapping_add(v[2].rotate_left(12))
            .wrapping_add(v[3].rotate_left(18));
        for lane in v {
            h = merge(h, lane);
        }
        h
    } else {
        seed.wrapping_add(P5)
    };
    h = h.wrapping_add(data.len() as u64);
    while rest.len() >= 8 {
        h = (h ^ round(0, le64(rest)))
            .rotate_left(27)
            .wrapping_mul(P1)
            .wrapping_add(P4);
        rest = &rest[8..];
    }
    if rest.len() >= 4 {
        let w = u64::from(u32::from_le_bytes(
            rest[..4].try_into().expect("four bytes"),
        ));
        h = (h ^ w.wrapping_mul(P1))
            .rotate_left(23)
            .wrapping_mul(P2)
            .wrapping_add(P3);
        rest = &rest[4..];
    }
    for &b in rest {
        h = (h ^ u64::from(b).wrapping_mul(P5))
            .rotate_left(11)
            .wrapping_mul(P1);
    }
    h ^= h >> 33;
    h = h.wrapping_mul(P2);
    h ^= h >> 29;
    h = h.wrapping_mul(P3);
    h ^ (h >> 32)
}

/// A decoded SCALE value: enough structure to find calls and their arguments.
#[derive(Debug, Clone, PartialEq)]
enum Val {
    Bytes(Vec<u8>),
    Num(u128),
    Other,
    Seq(Vec<Val>),
    Comp(Vec<(Option<String>, Val)>),
    /// `ty` is the last segment of the enum's type path (`RuntimeCall`, `Call`, `Option`, ...).
    Var {
        ty: String,
        name: String,
        fields: Vec<(Option<String>, Val)>,
    },
}

/// Generic decoder over a metadata type registry.
struct Dec<'a> {
    reg: &'a PortableRegistry,
    /// Nesting bound: the registry's type count plus the input's length. A value can only nest
    /// deeper through a recursive type, and every level of a valid recursive value consumes
    /// input, so a deeper nesting is malformed input.
    depth_bound: usize,
}

fn compact(input: &mut &[u8]) -> Result<u128> {
    let first = *input.first().context("compact: end of input")?;
    let (value, len) = match first & 3 {
        0 => (u128::from(first >> 2), 1),
        1 => {
            ensure!(input.len() >= 2, "compact: end of input");
            (u128::from(u16::from_le_bytes([input[0], input[1]]) >> 2), 2)
        }
        2 => {
            ensure!(input.len() >= 4, "compact: end of input");
            (
                u128::from(u32::from_le_bytes(input[..4].try_into().expect("four")) >> 2),
                4,
            )
        }
        _ => {
            let n = usize::from(first >> 2) + 4;
            ensure!(n <= 16, "compact: wider than u128");
            ensure!(input.len() > n, "compact: end of input");
            let mut b = [0u8; 16];
            b[..n].copy_from_slice(&input[1..=n]);
            (u128::from_le_bytes(b), n + 1)
        }
    };
    *input = &input[len..];
    Ok(value)
}

fn take<'b>(input: &mut &'b [u8], n: usize) -> Result<&'b [u8]> {
    ensure!(input.len() >= n, "end of input");
    let (head, tail) = input.split_at(n);
    *input = tail;
    Ok(head)
}

impl<'a> Dec<'a> {
    fn new(reg: &'a PortableRegistry, input_len: usize) -> Self {
        Self {
            reg,
            depth_bound: reg.types.len() + input_len,
        }
    }

    fn is_u8(&self, id: u32) -> bool {
        matches!(
            self.reg.resolve(id).map(|t| &t.type_def),
            Some(TypeDef::Primitive(TypeDefPrimitive::U8))
        )
    }

    fn fields(
        &self,
        fields: &[scale_info::Field<PortableForm>],
        input: &mut &[u8],
        depth: usize,
    ) -> Result<Vec<(Option<String>, Val)>> {
        fields
            .iter()
            .map(|f| Ok((f.name.clone(), self.value(f.ty.id, input, depth + 1)?)))
            .collect()
    }

    fn value(&self, id: u32, input: &mut &[u8], depth: usize) -> Result<Val> {
        ensure!(
            depth <= self.depth_bound,
            "value nests deeper than its input allows"
        );
        let ty = self.reg.resolve(id).context("unknown type id")?;
        Ok(match &ty.type_def {
            TypeDef::Composite(c) => Val::Comp(self.fields(&c.fields, input, depth)?),
            TypeDef::Variant(v) => {
                let index = take(input, 1)?[0];
                let var = v
                    .variants
                    .iter()
                    .find(|var| var.index == index)
                    .with_context(|| format!("no variant {index} in {}", ty.path))?;
                Val::Var {
                    ty: ty.path.segments.last().cloned().unwrap_or_default(),
                    name: var.name.clone(),
                    fields: self.fields(&var.fields, input, depth)?,
                }
            }
            TypeDef::Sequence(s) => {
                let len = usize::try_from(compact(input)?).context("sequence length")?;
                if self.is_u8(s.type_param.id) {
                    Val::Bytes(take(input, len)?.to_vec())
                } else {
                    ensure!(len <= input.len(), "sequence longer than its input");
                    let mut items = Vec::with_capacity(len);
                    for _ in 0..len {
                        items.push(self.value(s.type_param.id, input, depth + 1)?);
                    }
                    Val::Seq(items)
                }
            }
            TypeDef::Array(a) => {
                let len = a.len as usize;
                if self.is_u8(a.type_param.id) {
                    Val::Bytes(take(input, len)?.to_vec())
                } else {
                    let mut items = Vec::with_capacity(len.min(input.len()));
                    for _ in 0..len {
                        items.push(self.value(a.type_param.id, input, depth + 1)?);
                    }
                    Val::Seq(items)
                }
            }
            TypeDef::Tuple(t) => {
                let mut items = Vec::with_capacity(t.fields.len());
                for f in &t.fields {
                    items.push((None, self.value(f.id, input, depth + 1)?));
                }
                Val::Comp(items)
            }
            TypeDef::Primitive(p) => {
                let width = match p {
                    TypeDefPrimitive::Bool | TypeDefPrimitive::U8 | TypeDefPrimitive::I8 => 1,
                    TypeDefPrimitive::U16 | TypeDefPrimitive::I16 => 2,
                    TypeDefPrimitive::U32 | TypeDefPrimitive::I32 | TypeDefPrimitive::Char => 4,
                    TypeDefPrimitive::U64 | TypeDefPrimitive::I64 => 8,
                    TypeDefPrimitive::U128 | TypeDefPrimitive::I128 => 16,
                    TypeDefPrimitive::U256 | TypeDefPrimitive::I256 => 32,
                    TypeDefPrimitive::Str => {
                        let len = usize::try_from(compact(input)?).context("string length")?;
                        take(input, len)?;
                        return Ok(Val::Other);
                    }
                };
                let bytes = take(input, width)?;
                if width <= 16 {
                    let mut b = [0u8; 16];
                    b[..width].copy_from_slice(bytes);
                    Val::Num(u128::from_le_bytes(b))
                } else {
                    Val::Bytes(bytes.to_vec())
                }
            }
            TypeDef::Compact(_) => Val::Num(compact(input)?),
            TypeDef::BitSequence(_) => bail!("bit sequences are not decoded"),
        })
    }

    /// Decodes one whole value: every input byte must be consumed.
    fn whole(&self, id: u32, bytes: &[u8]) -> Result<Val> {
        let mut input = bytes;
        let v = self.value(id, &mut input, 0)?;
        ensure!(
            input.is_empty(),
            "{} bytes left after the value",
            input.len()
        );
        Ok(v)
    }
}

/// One upgrade call found inside a decoded `RuntimeCall`.
struct Found {
    path: Vec<String>,
    call: String,
    code_hash: Option<String>,
    code_bytes: Option<usize>,
}

/// A newtype's inner value (`BoundedVec<T>` around `Vec<T>`, `H256` around `[u8; 32]`), through
/// any depth of single-field composites.
fn unwrap_newtype(v: &Val) -> &Val {
    match v {
        Val::Comp(f) if f.len() == 1 => unwrap_newtype(&f[0].1),
        other => other,
    }
}

fn field<'v>(fields: &'v [(Option<String>, Val)], name: &str) -> Option<&'v Val> {
    fields
        .iter()
        .find(|(n, _)| n.as_deref() == Some(name))
        .map(|(_, v)| v)
}

/// Walks a decoded value; every `RuntimeCall` level adds `Pallet.call` to the path.
fn find_upgrades(v: &Val, path: &mut Vec<String>, out: &mut Vec<Found>) {
    match v {
        Val::Var { ty, name, fields } if ty == "RuntimeCall" => {
            let inner = fields.first().map(|(_, f)| f);
            let call = match inner {
                Some(Val::Var { name: c, .. }) => c.clone(),
                _ => "?".to_owned(),
            };
            path.push(format!("{name}.{call}"));
            if name == "System" && UPGRADE_CALLS.contains(&call.as_str()) {
                let args = match inner {
                    Some(Val::Var { fields, .. }) => fields.as_slice(),
                    _ => &[],
                };
                let code = field(args, "code").map(unwrap_newtype);
                let hash = field(args, "code_hash").map(unwrap_newtype);
                let (code_hash, code_bytes) = match (code, hash) {
                    (Some(Val::Bytes(code)), _) => (
                        Some(format!("0x{}", hex::encode(Blake2b::<U32>::digest(code)))),
                        Some(code.len()),
                    ),
                    (_, Some(Val::Bytes(h))) => (Some(format!("0x{}", hex::encode(h))), None),
                    _ => (None, None),
                };
                out.push(Found {
                    path: path.clone(),
                    call: format!("System.{call}"),
                    code_hash,
                    code_bytes,
                });
            }
            for (_, f) in fields {
                find_upgrades(f, path, out);
            }
            path.pop();
        }
        Val::Var { fields, .. } | Val::Comp(fields) => {
            for (_, f) in fields {
                find_upgrades(f, path, out);
            }
        }
        Val::Seq(items) => {
            for item in items {
                find_upgrades(item, path, out);
            }
        }
        Val::Bytes(_) | Val::Num(_) | Val::Other => {}
    }
}

/// The `Bounded<RuntimeCall>` payloads inside a decoded agenda value: inline call bytes, and
/// preimage hashes (`Lookup`, `Legacy`), each with the index of its agenda slot.
fn bounded_calls(
    v: &Val,
    slot: &mut Option<usize>,
    inline: &mut Vec<(usize, Vec<u8>)>,
    refs: &mut Vec<(usize, Vec<u8>)>,
) {
    match v {
        Val::Var { ty, name, fields } if ty == "Bounded" => {
            let at = slot.unwrap_or(0);
            match (
                name.as_str(),
                fields.first().map(|(_, f)| unwrap_newtype(f)),
            ) {
                ("Inline", Some(Val::Bytes(b))) => inline.push((at, b.clone())),
                ("Lookup" | "Legacy", _) => {
                    if let Some(Val::Bytes(h)) = field(fields, "hash").map(unwrap_newtype) {
                        refs.push((at, h.clone()));
                    }
                }
                _ => {}
            }
        }
        Val::Var { fields, .. } | Val::Comp(fields) => {
            for (_, f) in fields {
                bounded_calls(f, slot, inline, refs);
            }
        }
        Val::Seq(items) => {
            for (i, item) in items.iter().enumerate() {
                if slot.is_none() {
                    // The agenda value itself: one slot per item.
                    let mut s = Some(i);
                    bounded_calls(item, &mut s, inline, refs);
                } else {
                    bounded_calls(item, slot, inline, refs);
                }
            }
        }
        Val::Bytes(_) | Val::Num(_) | Val::Other => {}
    }
}

/// Whether a type can hold a `RuntimeCall` (a wrapper such as `Sudo.sudo` or `Utility.batch`).
fn reaches_call(reg: &PortableRegistry, id: u32, seen: &mut Vec<u32>) -> bool {
    if seen.contains(&id) {
        return false;
    }
    seen.push(id);
    let Some(ty) = reg.resolve(id) else {
        return false;
    };
    if ty.path.segments.last().is_some_and(|s| s == "RuntimeCall") {
        return true;
    }
    match &ty.type_def {
        TypeDef::Composite(c) => c.fields.iter().any(|f| reaches_call(reg, f.ty.id, seen)),
        TypeDef::Variant(v) => v
            .variants
            .iter()
            .any(|var| var.fields.iter().any(|f| reaches_call(reg, f.ty.id, seen))),
        TypeDef::Sequence(q) => reaches_call(reg, q.type_param.id, seen),
        TypeDef::Array(a) => reaches_call(reg, a.type_param.id, seen),
        TypeDef::Tuple(t) => t.fields.iter().any(|f| reaches_call(reg, f.id, seen)),
        TypeDef::Compact(c) => reaches_call(reg, c.type_param.id, seen),
        TypeDef::Primitive(_) | TypeDef::BitSequence(_) => false,
    }
}

/// What a call that does not decode under this metadata is, read from its first two bytes (the
/// pallet and call indices): its name (None when no call has those indices), whether it is a
/// `System` upgrade call and whether its arguments can carry other calls. A stored call written
/// under an older runtime keeps its indices but may no longer decode.
fn classify(m: &v15::RuntimeMetadataV15, bytes: &[u8]) -> (Option<String>, bool, bool) {
    let found = bytes.first().and_then(|p| {
        let pallet = m.pallets.iter().find(|x| x.index == *p)?;
        let ty = pallet.calls.as_ref()?.ty.id;
        let TypeDef::Variant(v) = &m.types.resolve(ty)?.type_def else {
            return None;
        };
        let var = v
            .variants
            .iter()
            .find(|var| Some(&var.index) == bytes.get(1))?;
        Some((pallet.name.clone(), var))
    });
    match found {
        None => (None, false, false),
        Some((pallet, var)) => {
            let upgrade = pallet == "System" && UPGRADE_CALLS.contains(&var.name.as_str());
            let wraps = var
                .fields
                .iter()
                .any(|f| reaches_call(&m.types, f.ty.id, &mut Vec::new()));
            (Some(format!("{pallet}.{}", var.name)), upgrade, wraps)
        }
    }
}

struct Meta<'a> {
    m: &'a v15::RuntimeMetadataV15,
}

impl Meta<'_> {
    fn storage(
        &self,
        pallet: &str,
        item: &str,
    ) -> Option<&v15::StorageEntryMetadata<PortableForm>> {
        self.m
            .pallets
            .iter()
            .find(|p| p.name == pallet)?
            .storage
            .as_ref()?
            .entries
            .iter()
            .find(|e| e.name == item)
    }

    /// The value type id of a storage item, and for a map its hashers and key type.
    fn value_ty(
        &self,
        pallet: &str,
        item: &str,
    ) -> Option<(u32, Vec<v15::StorageHasher>, Option<u32>)> {
        Some(match &self.storage(pallet, item)?.ty {
            v15::StorageEntryType::Plain(t) => (t.id, Vec::new(), None),
            v15::StorageEntryType::Map {
                hashers,
                key,
                value,
            } => (value.id, hashers.clone(), Some(key.id)),
        })
    }
}

fn hex_bytes(v: &Value) -> Result<Vec<u8>> {
    let s = v.as_str().context("expected a hex string")?;
    Ok(hex::decode(s.trim_start_matches("0x"))?)
}

/// The first key component of a map key after its 32-byte prefix, when its hasher keeps the key
/// (`Twox64Concat`, `Blake2_128Concat`, `Identity`).
fn map_key(
    dec: &Dec<'_>,
    hashers: &[v15::StorageHasher],
    key_ty: Option<u32>,
    key: &[u8],
) -> Option<Val> {
    let rest = key.get(32..)?;
    let skip = match hashers.first()? {
        v15::StorageHasher::Twox64Concat => 8,
        v15::StorageHasher::Blake2_128Concat => 16,
        v15::StorageHasher::Identity => 0,
        _ => return None,
    };
    let mut input = rest.get(skip..)?;
    let v = dec.value(key_ty?, &mut input, 0).ok()?;
    match v {
        Val::Comp(items) if hashers.len() > 1 => items.into_iter().next().map(|(_, v)| v),
        other => Some(other),
    }
}

/// Scans fetched storage for announced upgrades. `input` holds `code_hash` (the live `:code`
/// storage hash), `authorized_upgrade` (hex or null), `agenda` and `preimages` (lists of
/// `{key, value}` in hex).
pub fn scan(v15_bytes: &[u8], input: &Value) -> Result<Value> {
    let RuntimeMetadata::V15(m15) = crate::decode(v15_bytes)? else {
        bail!("metadata is not V15");
    };
    let meta = Meta { m: &m15 };
    let call_ty = m15.extrinsic.call_ty.id;
    let current = input["code_hash"].as_str().unwrap_or("").to_lowercase();
    let mut pending = Vec::new();
    let mut undecodable = Vec::new();
    let mut entry = |source: &str, f: Found, extra: Value| {
        let applied = f
            .code_hash
            .as_deref()
            .is_some_and(|h| h.to_lowercase() == current);
        let mut e = json!({
            "source": source, "call": f.call, "path": f.path.join(" > "),
            "code_hash": f.code_hash, "code_bytes": f.code_bytes,
            "is_current_code": applied,
        });
        if let (Value::Object(m), Value::Object(x)) = (&mut e, extra) {
            m.extend(x);
        }
        pending.push(e);
    };

    // System.AuthorizedUpgrade: Option<CodeUpgradeAuthorization { code_hash, check_version }>.
    let authorized = &input["authorized_upgrade"];
    if !authorized.is_null() {
        match meta.value_ty("System", "AuthorizedUpgrade") {
            None => undecodable.push(json!({"source": "authorized_upgrade", "error": "System.AuthorizedUpgrade is not in this metadata"})),
            Some((ty, _, _)) => {
                let bytes = hex_bytes(authorized)?;
                let dec = Dec::new(&m15.types, bytes.len());
                match dec.whole(ty, &bytes) {
                    Ok(v) => {
                        // A plain OptionQuery value is stored without its Option tag.
                        let fields = match &v {
                            Val::Comp(f) => f.clone(),
                            Val::Var { name, fields, .. } if name == "Some" => match fields.first() {
                                Some((_, Val::Comp(f))) => f.clone(),
                                _ => Vec::new(),
                            },
                            _ => Vec::new(),
                        };
                        let code_hash = match field(&fields, "code_hash").map(unwrap_newtype) {
                            Some(Val::Bytes(h)) => Some(format!("0x{}", hex::encode(h))),
                            _ => None,
                        };
                        let check = matches!(field(&fields, "check_version"), Some(Val::Num(1)));
                        entry(
                            "authorized_upgrade",
                            Found { path: vec!["System.AuthorizedUpgrade".into()], call: "System.authorize_upgrade".into(), code_hash, code_bytes: None },
                            json!({"check_version": check}),
                        );
                    }
                    Err(e) => undecodable.push(json!({"source": "authorized_upgrade", "error": e.to_string()})),
                }
            }
        }
    }

    // Preimages first: an agenda entry that names one by hash is reported with it.
    let mut preimage_hashes: Vec<(Vec<u8>, usize)> = Vec::new();
    let mut preimage_found: Vec<(Vec<u8>, Vec<Found>)> = Vec::new();
    let preimages = input["preimages"].as_array().cloned().unwrap_or_default();
    let pre_ty = meta.value_ty("Preimage", "PreimageFor");
    for p in &preimages {
        let key = hex_bytes(&p["key"])?;
        let value = hex_bytes(&p["value"])?;
        // Identity-hashed (H256, u32): the preimage's hash and length follow the prefix.
        let hash = key.get(32..64).map(<[u8]>::to_vec).unwrap_or_default();
        preimage_hashes.push((hash.clone(), value.len()));
        let Some((ty, _, _)) = pre_ty.clone() else {
            undecodable.push(json!({"source": "preimage", "key": p["key"], "error": "Preimage.PreimageFor is not in this metadata"}));
            continue;
        };
        let dec = Dec::new(&m15.types, value.len());
        let bytes = match dec.whole(ty, &value).map(|v| unwrap_newtype(&v).clone()) {
            Ok(Val::Bytes(b)) => b,
            Ok(_) => {
                undecodable.push(json!({"source": "preimage", "key": p["key"],
                                        "error": "Preimage.PreimageFor is not a byte sequence"}));
                continue;
            }
            Err(e) => {
                undecodable
                    .push(json!({"source": "preimage", "key": p["key"], "error": e.to_string()}));
                continue;
            }
        };
        let dec = Dec::new(&m15.types, bytes.len());
        match dec.whole(call_ty, &bytes) {
            Ok(call) => {
                let mut found = Vec::new();
                find_upgrades(&call, &mut Vec::new(), &mut found);
                preimage_found.push((hash, found));
            }
            // A preimage need not be a call (it may be any noted bytes): recorded, and flagged for
            // review only when its indices name an upgrade call or a call that carries calls.
            Err(e) => {
                let (call, upgrade, wraps) = classify(&m15, &bytes);
                if upgrade {
                    entry(
                        "preimage",
                        Found {
                            path: vec![call.clone().unwrap_or_default()],
                            call: call.clone().unwrap_or_default(),
                            code_hash: None,
                            code_bytes: None,
                        },
                        json!({"preimage_hash": format!("0x{}", hex::encode(&hash)), "undecoded": true}),
                    );
                }
                undecodable.push(
                    json!({"source": "preimage", "key": p["key"], "bytes": bytes.len(),
                                        "call_by_index": call, "needs_review": upgrade || wraps,
                                        "error": format!("not a RuntimeCall here: {e}")}),
                );
            }
        }
    }

    let mut scheduled_refs: Vec<(Vec<u8>, Value)> = Vec::new();
    let agenda = input["agenda"].as_array().cloned().unwrap_or_default();
    let mut agenda_entries = 0usize;
    match meta.value_ty("Scheduler", "Agenda") {
        None if !agenda.is_empty() => undecodable.push(
            json!({"source": "scheduler", "error": "Scheduler.Agenda is not in this metadata"}),
        ),
        None => {}
        Some((ty, hashers, key_ty)) => {
            for a in &agenda {
                let key = hex_bytes(&a["key"])?;
                let value = hex_bytes(&a["value"])?;
                let dec = Dec::new(&m15.types, value.len().max(key.len()));
                let block = match map_key(&dec, &hashers, key_ty, &key) {
                    Some(Val::Num(n)) => json!(n),
                    _ => Value::Null,
                };
                let v = match dec.whole(ty, &value) {
                    Ok(v) => v,
                    Err(e) => {
                        undecodable.push(
                            json!({"source": "scheduler", "key": a["key"], "error": e.to_string()}),
                        );
                        continue;
                    }
                };
                if let Val::Seq(items) = unwrap_newtype(&v) {
                    agenda_entries += items.len();
                }
                let (mut inline, mut refs) = (Vec::new(), Vec::new());
                bounded_calls(&v, &mut None, &mut inline, &mut refs);
                for (slot, bytes) in inline {
                    let dec = Dec::new(&m15.types, bytes.len());
                    match dec.whole(call_ty, &bytes) {
                        Ok(call) => {
                            let mut found = Vec::new();
                            find_upgrades(&call, &mut Vec::new(), &mut found);
                            for f in found {
                                entry("scheduler", f, json!({"block": block, "slot": slot}));
                            }
                        }
                        // A scheduled call that does not decode needs review unless its indices
                        // name a call that is neither an upgrade nor a wrapper of calls.
                        Err(e) => {
                            let (call, upgrade, wraps) = classify(&m15, &bytes);
                            if upgrade {
                                entry(
                                    "scheduler",
                                    Found {
                                        path: vec![call.clone().unwrap_or_default()],
                                        call: call.clone().unwrap_or_default(),
                                        code_hash: None,
                                        code_bytes: None,
                                    },
                                    json!({"block": block, "slot": slot, "undecoded": true}),
                                );
                            }
                            undecodable.push(json!({"source": "scheduler", "key": a["key"], "block": block, "slot": slot,
                                                    "call_by_index": call, "needs_review": upgrade || wraps || call.is_none(),
                                                    "error": format!("inline call: {e}")}));
                        }
                    }
                }
                for (slot, hash) in refs {
                    scheduled_refs.push((hash, json!({"block": block, "slot": slot})));
                }
            }
        }
    }

    for (hash, found) in preimage_found {
        let at: Vec<Value> = scheduled_refs
            .iter()
            .filter(|(h, _)| *h == hash)
            .map(|(_, at)| at.clone())
            .collect();
        for f in found {
            entry(
                "preimage",
                f,
                json!({"preimage_hash": format!("0x{}", hex::encode(&hash)), "scheduled_at": at}),
            );
        }
    }
    // A scheduled call by hash whose preimage is not on chain cannot be read: recorded.
    let unresolved: Vec<Value> = scheduled_refs
        .iter()
        .filter(|(h, _)| !preimage_hashes.iter().any(|(p, _)| p == h))
        .map(|(h, at)| json!({"preimage_hash": format!("0x{}", hex::encode(h)), "at": at}))
        .collect();

    // Anything else that could not be read (a value, or an item missing from the metadata) is a
    // gap in the scan.
    for u in &mut undecodable {
        if u.get("needs_review").is_none() {
            u["needs_review"] = json!(true);
        }
    }
    let needs_review =
        undecodable.iter().any(|u| u["needs_review"] == true) || !unresolved.is_empty();
    Ok(json!({
        "schema": SCAN_SCHEMA,
        "needs_review": needs_review,
        "current_code_hash": input["code_hash"],
        "pending": pending,
        "scheduled_by_hash_unresolved": unresolved,
        "undecodable": undecodable,
        "counts": {"agenda_blocks": agenda.len(), "agenda_entries": agenda_entries, "preimages": preimages.len()},
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codec::{Compact, Encode};

    const METADATA: &[u8] =
        include_bytes!("../../../subnet/fixtures/runtime-486/testnet-486.v15.scale");

    #[test]
    fn prefixes_match_known_keys() {
        assert_eq!(
            storage_prefix("System", "Events"),
            "0x26aa394eea5630e07c48ae0c9558cef780d41e5e16056765bc8461851072c9d7"
        );
        assert_eq!(
            storage_prefix("System", "Account"),
            "0x26aa394eea5630e07c48ae0c9558cef7b99d880ec681799c0cf30e8886371da9"
        );
        assert_eq!(
            storage_prefix("Scheduler", "Agenda"),
            "0x3db7a24cfdc9de785974746c14a99df91643f5419718219c95679ddd2d825574"
        );
        // The published empty-input vector, and the four-lane path (36 bytes).
        assert_eq!(xxh64(b"", 0), 0xef46_db37_51d8_e999);
        assert_eq!(
            xxh64(b"abcdefghijklmnopqrstuvwxyz0123456789", 0),
            0x64f2_3ecf_1609_b766
        );
    }

    fn indices(pallet: &str, call: &str) -> (u8, u8) {
        let RuntimeMetadata::V15(m) = crate::decode(METADATA).unwrap() else {
            panic!()
        };
        let p = m.pallets.iter().find(|p| p.name == pallet).unwrap();
        let ty = p.calls.as_ref().unwrap().ty.id;
        let TypeDef::Variant(v) = &m.types.resolve(ty).unwrap().type_def else {
            panic!()
        };
        (
            p.index,
            v.variants.iter().find(|v| v.name == call).unwrap().index,
        )
    }

    fn set_code(code: &[u8]) -> Vec<u8> {
        let (p, c) = indices("System", "set_code");
        let mut out = vec![p, c];
        code.to_vec().encode_to(&mut out);
        out
    }

    fn sudo(inner: &[u8]) -> Vec<u8> {
        let (p, c) = indices("Sudo", "sudo");
        let mut out = vec![p, c];
        out.extend_from_slice(inner);
        out
    }

    fn remark(text: &[u8]) -> Vec<u8> {
        let (p, c) = indices("System", "remark");
        let mut out = vec![p, c];
        text.to_vec().encode_to(&mut out);
        out
    }

    /// One `Option<Scheduled>`: Some, no id, priority, `Bounded::Inline(call)` (variant 1), no
    /// period, origin `system(Root)` (both variant 0).
    fn scheduled_inline(call: &[u8]) -> Vec<u8> {
        let mut out = vec![1u8, 0u8, 63u8, 1u8];
        Compact(call.len() as u32).encode_to(&mut out);
        out.extend_from_slice(call);
        out.extend_from_slice(&[0u8, 0u8, 0u8]);
        out
    }

    fn agenda(entries: &[Vec<u8>]) -> String {
        let mut out = Vec::new();
        Compact(entries.len() as u32).encode_to(&mut out);
        for e in entries {
            out.extend_from_slice(e);
        }
        format!("0x{}", hex::encode(out))
    }

    fn agenda_key(block: u32) -> String {
        let mut k =
            hex::decode(storage_prefix("Scheduler", "Agenda").trim_start_matches("0x")).unwrap();
        k.extend_from_slice(&xxh64(&block.encode(), 0).to_le_bytes());
        k.extend_from_slice(&block.encode());
        format!("0x{}", hex::encode(k))
    }

    #[test]
    fn a_scheduled_set_code_is_found_at_any_nesting() {
        let code = b"\0asm fake runtime".to_vec();
        let input = json!({
            "code_hash": "0x00",
            "authorized_upgrade": null,
            "agenda": [{"key": agenda_key(9_500_000),
                        "value": agenda(&[scheduled_inline(&remark(b"hi")), scheduled_inline(&sudo(&set_code(&code)))])}],
            "preimages": [],
        });
        let out = scan(METADATA, &input).unwrap();
        assert_eq!(out["undecodable"], json!([]), "{out:#}");
        let p = out["pending"].as_array().unwrap();
        assert_eq!(p.len(), 1, "{out:#}");
        assert_eq!(p[0]["source"], "scheduler");
        assert_eq!(p[0]["call"], "System.set_code");
        assert_eq!(p[0]["path"], "Sudo.sudo > System.set_code");
        assert_eq!(p[0]["block"], 9_500_000);
        assert_eq!(p[0]["slot"], 1);
        assert_eq!(p[0]["code_bytes"], code.len());
        assert_eq!(
            p[0]["code_hash"],
            format!("0x{}", hex::encode(Blake2b::<U32>::digest(&code)))
        );
        assert_eq!(out["counts"]["agenda_entries"], 2);
    }

    #[test]
    fn a_preimage_upgrade_and_an_authorization_are_found() {
        let code = b"\0asm another".to_vec();
        let call = set_code(&code);
        let hash = Blake2b::<U32>::digest(&call).to_vec();
        let mut key =
            hex::decode(storage_prefix("Preimage", "PreimageFor").trim_start_matches("0x"))
                .unwrap();
        key.extend_from_slice(&hash);
        key.extend_from_slice(&(call.len() as u32).encode());
        let mut auth = hash.clone();
        auth.push(1);
        let input = json!({
            "code_hash": "0x00",
            "authorized_upgrade": format!("0x{}", hex::encode(&auth)),
            "agenda": [],
            "preimages": [{"key": format!("0x{}", hex::encode(&key)), "value": format!("0x{}", hex::encode(call.encode()))},
                          {"key": format!("0x{}", hex::encode(&key)), "value": format!("0x{}", hex::encode(b"not a call".to_vec().encode()))}],
        });
        let out = scan(METADATA, &input).unwrap();
        let p = out["pending"].as_array().unwrap();
        assert_eq!(p.len(), 2, "{out:#}");
        assert_eq!(p[0]["source"], "authorized_upgrade");
        assert_eq!(p[0]["code_hash"], format!("0x{}", hex::encode(&hash)));
        assert_eq!(p[0]["check_version"], true);
        assert_eq!(p[1]["source"], "preimage");
        assert_eq!(p[1]["call"], "System.set_code");
        // The non-call preimage is recorded, not alerted.
        assert_eq!(out["undecodable"].as_array().unwrap().len(), 1, "{out:#}");
        assert_eq!(out["needs_review"], false, "{out:#}");
    }

    #[test]
    fn a_call_stored_under_an_older_runtime_is_classified_by_its_indices() {
        // A SubtensorModule call whose arguments no longer decode (cut short), a cut System
        // upgrade and a cut Utility.batch.
        let (sp, sc) = indices("SubtensorModule", "burned_register");
        let old = vec![sp, sc, 1];
        let mut cut_upgrade = set_code(b"\0asm code");
        cut_upgrade.truncate(4);
        let (up, uc) = indices("Utility", "batch");
        let cut_batch = vec![up, uc, 8];
        let input = json!({"code_hash": "0x00", "authorized_upgrade": null,
                           "agenda": [{"key": agenda_key(4_941_251), "value": agenda(&[
                               scheduled_inline(&old), scheduled_inline(&cut_upgrade), scheduled_inline(&cut_batch)])}],
                           "preimages": []});
        let out = scan(METADATA, &input).unwrap();
        let u = out["undecodable"].as_array().unwrap();
        assert_eq!(u.len(), 3, "{out:#}");
        assert_eq!(u[0]["call_by_index"], "SubtensorModule.burned_register");
        assert_eq!(u[0]["needs_review"], false);
        assert_eq!(u[1]["call_by_index"], "System.set_code");
        assert_eq!(u[1]["needs_review"], true);
        assert_eq!(u[2]["call_by_index"], "Utility.batch");
        assert_eq!(u[2]["needs_review"], true);
        assert_eq!(out["needs_review"], true);
        let p = out["pending"].as_array().unwrap();
        assert_eq!(p.len(), 1, "{out:#}");
        assert_eq!(p[0]["undecoded"], true);
        assert_eq!(p[0]["block"], 4_941_251);
        // The older call alone is no gap.
        let input = json!({"code_hash": "0x00", "authorized_upgrade": null,
                           "agenda": [{"key": agenda_key(4_941_251), "value": agenda(&[scheduled_inline(&old)])}],
                           "preimages": []});
        let out = scan(METADATA, &input).unwrap();
        assert_eq!(out["needs_review"], false, "{out:#}");
        assert_eq!(out["pending"], json!([]));
    }

    #[test]
    fn the_current_code_is_marked_and_an_empty_chain_has_nothing() {
        let code = b"\0asm live".to_vec();
        let live = format!("0x{}", hex::encode(Blake2b::<U32>::digest(&code)));
        let input = json!({"code_hash": live, "authorized_upgrade": null,
                           "agenda": [{"key": agenda_key(1), "value": agenda(&[scheduled_inline(&set_code(&code))])}],
                           "preimages": []});
        let out = scan(METADATA, &input).unwrap();
        assert_eq!(out["pending"][0]["is_current_code"], true);
        let empty = scan(
            METADATA,
            &json!({"code_hash": live, "authorized_upgrade": null, "agenda": [], "preimages": []}),
        )
        .unwrap();
        assert_eq!(empty["pending"], json!([]));
        assert_eq!(empty["undecodable"], json!([]));
    }
}
