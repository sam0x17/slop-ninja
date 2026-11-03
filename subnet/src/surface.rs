//! Live-metadata call resolution, type checking and adaptive encoding.
//!
//! Every lookup here is by pallet, call and argument name in the metadata the
//! chain serves now. Pallet and call indices come from that metadata, so moved
//! indices need no code change. No runtime spec or transaction version decides
//! anything in this module; a runtime upgrade only changes what the live
//! metadata says, and each operation is judged on its own live types.
//!
//! A difference is adapted only when it is mechanically safe: a wider unsigned
//! or signed integer, or a new trailing `Option` argument that receives `None`.
//! Anything else (an absent pallet or call, a removed, renamed or reordered
//! argument, a changed type, a new required argument) defers that one operation
//! with a diagnostic. An encoded call is decoded again against the same live
//! metadata before it is returned, so a payload that does not type-check is
//! never produced.
//!
//! All traversal is bounded and fallible. Malformed, recursive or hostile
//! metadata yields a [`Deferral`], never a panic.

use std::fmt;

use rtao_core::codec::Value;
use rtao_core::runtime::{Runtime, TypeSpec};
use rtao_runtime::{
    CallDescriptor, RuntimeHandle, StorageSurface, SurfaceClass, SurfaceDescriptor,
};
use scale_info::{TypeDef, TypeDefPrimitive};
use serde::{Deserialize, Serialize};

/// Deepest type nesting followed before deferring.
const MAX_DEPTH: usize = 48;
/// Most type nodes visited for one call before deferring.
const MAX_NODES: usize = 4096;
/// Longest diagnostic retained, in bytes.
const MAX_DETAIL: usize = 512;

/// Wire-relevant structure of one SCALE type, independent of type IDs, paths
/// and documentation. Single-field wrappers are transparent because they encode
/// exactly like their field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    Bool,
    Char,
    Str,
    /// Unsigned integer of this many bits.
    Uint(u16),
    /// Signed integer of this many bits.
    Int(u16),
    Compact(Box<Ty>),
    Seq(Box<Ty>),
    Array(u32, Box<Ty>),
    Tuple(Vec<Ty>),
    Option(Box<Ty>),
    /// Named fields in declaration order.
    Struct(Vec<(String, Ty)>),
    /// Variants as (name, index, field types), in index order.
    Enum(Vec<(String, u8, Vec<Ty>)>),
    BitSequence,
}

impl Ty {
    pub fn seq(inner: Ty) -> Self {
        Self::Seq(Box::new(inner))
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool => f.write_str("bool"),
            Self::Char => f.write_str("char"),
            Self::Str => f.write_str("str"),
            Self::Uint(bits) => write!(f, "u{bits}"),
            Self::Int(bits) => write!(f, "i{bits}"),
            Self::Compact(inner) => write!(f, "Compact<{inner}>"),
            Self::Seq(inner) => write!(f, "Vec<{inner}>"),
            Self::Array(len, inner) => write!(f, "[{inner}; {len}]"),
            Self::Tuple(items) => {
                f.write_str("(")?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str(")")
            }
            Self::Option(inner) => write!(f, "Option<{inner}>"),
            Self::Struct(fields) => {
                f.write_str("{")?;
                for (index, (name, ty)) in fields.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{name}: {ty}")?;
                }
                f.write_str("}")
            }
            Self::Enum(variants) => write!(f, "enum({} variants)", variants.len()),
            Self::BitSequence => f.write_str("BitVec"),
        }
    }
}

/// Why one operation cannot be encoded against the live metadata right now.
/// It is re-evaluated on every later block and metadata change.
// Readiness is advisory and recomputable, so its records decode tolerantly:
// fields added by a later build are ignored rather than refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deferral {
    /// Stable reason token.
    pub code: String,
    /// Operator-readable cause, without secret material or payload values.
    pub detail: String,
}

impl Deferral {
    pub(crate) fn new(code: &str, detail: impl Into<String>) -> Self {
        let mut detail = detail.into();
        if detail.len() > MAX_DETAIL {
            let mut end = MAX_DETAIL;
            while !detail.is_char_boundary(end) {
                end -= 1;
            }
            detail.truncate(end);
        }
        Self {
            code: code.into(),
            detail,
        }
    }
}

impl fmt::Display for Deferral {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

/// One mechanically safe difference applied while encoding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Adaptation {
    /// The live integer type is wider; every expected value still fits.
    Widened {
        argument: String,
        from: String,
        to: String,
    },
    /// A new trailing optional argument, encoded as `None`.
    TrailingOptionNone { argument: String },
}

/// An argument the application supplies, with the type it was written for.
#[derive(Clone, Debug)]
pub struct ExpectedArg {
    pub name: &'static str,
    pub ty: Ty,
}

/// A call the application signs, named the way runtime metadata names it.
#[derive(Clone, Debug)]
pub struct ExpectedCall {
    pub pallet: &'static str,
    pub call: &'static str,
    pub args: Vec<ExpectedArg>,
}

impl ExpectedCall {
    /// `Pallet.call`, the operation identity used in diagnostics and records.
    pub fn id(&self) -> String {
        format!("{}.{}", self.pallet, self.call)
    }
}

/// Where the call lives now and what adaptation its arguments need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub pallet_index: u8,
    pub call_index: u8,
    pub adaptations: Vec<Adaptation>,
    /// Names of new trailing optional arguments, encoded as `None`.
    trailing_none: Vec<String>,
}

/// Bytes composed by name against live metadata and decoded back successfully.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedCall {
    pub bytes: Vec<u8>,
    pub resolution: Resolution,
}

struct Walk {
    nodes: usize,
    stack: Vec<u32>,
}

/// Lower one live type into its wire structure, within fixed bounds.
pub fn lower(runtime: &Runtime, id: u32) -> Result<Ty, Deferral> {
    lower_at(
        runtime,
        id,
        &mut Walk {
            nodes: 0,
            stack: Vec::new(),
        },
        0,
    )
}

fn lower_at(runtime: &Runtime, id: u32, walk: &mut Walk, depth: usize) -> Result<Ty, Deferral> {
    if depth > MAX_DEPTH {
        return Err(Deferral::new(
            "type_unresolvable",
            "type nesting exceeds the bound",
        ));
    }
    walk.nodes += 1;
    if walk.nodes > MAX_NODES {
        return Err(Deferral::new(
            "type_unresolvable",
            "type graph exceeds the bound",
        ));
    }
    if walk.stack.contains(&id) {
        return Err(Deferral::new(
            "type_unresolvable",
            format!("type {id} is recursive"),
        ));
    }
    let ty = runtime
        .resolve(id)
        .map_err(|error| Deferral::new("type_unresolvable", error.to_string()))?;
    walk.stack.push(id);
    let next = depth + 1;
    let lowered = match &ty.type_def {
        TypeDef::Primitive(primitive) => Ok(match primitive {
            TypeDefPrimitive::Bool => Ty::Bool,
            TypeDefPrimitive::Char => Ty::Char,
            TypeDefPrimitive::Str => Ty::Str,
            TypeDefPrimitive::U8 => Ty::Uint(8),
            TypeDefPrimitive::U16 => Ty::Uint(16),
            TypeDefPrimitive::U32 => Ty::Uint(32),
            TypeDefPrimitive::U64 => Ty::Uint(64),
            TypeDefPrimitive::U128 => Ty::Uint(128),
            TypeDefPrimitive::U256 => Ty::Uint(256),
            TypeDefPrimitive::I8 => Ty::Int(8),
            TypeDefPrimitive::I16 => Ty::Int(16),
            TypeDefPrimitive::I32 => Ty::Int(32),
            TypeDefPrimitive::I64 => Ty::Int(64),
            TypeDefPrimitive::I128 => Ty::Int(128),
            TypeDefPrimitive::I256 => Ty::Int(256),
        }),
        TypeDef::Compact(compact) => lower_at(runtime, compact.type_param.id, walk, next)
            .map(|inner| Ty::Compact(Box::new(inner))),
        TypeDef::Sequence(sequence) => {
            lower_at(runtime, sequence.type_param.id, walk, next).map(Ty::seq)
        }
        TypeDef::Array(array) => lower_at(runtime, array.type_param.id, walk, next)
            .map(|inner| Ty::Array(array.len, Box::new(inner))),
        TypeDef::Tuple(tuple) => match tuple.fields.as_slice() {
            [only] => lower_at(runtime, only.id, walk, next),
            fields => fields
                .iter()
                .map(|field| lower_at(runtime, field.id, walk, next))
                .collect::<Result<Vec<_>, _>>()
                .map(Ty::Tuple),
        },
        TypeDef::Composite(composite) => match composite.fields.as_slice() {
            [only] => lower_at(runtime, only.ty.id, walk, next),
            fields if !fields.is_empty() && fields.iter().all(|field| field.name.is_some()) => {
                fields
                    .iter()
                    .map(|field| {
                        lower_at(runtime, field.ty.id, walk, next)
                            .map(|ty| (field.name.clone().unwrap_or_default(), ty))
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(Ty::Struct)
            }
            fields => fields
                .iter()
                .map(|field| lower_at(runtime, field.ty.id, walk, next))
                .collect::<Result<Vec<_>, _>>()
                .map(Ty::Tuple),
        },
        TypeDef::Variant(variant) => {
            let is_option = ty.path.segments.last().map(String::as_str) == Some("Option")
                && matches!(
                    variant.variants.as_slice(),
                    [none, some]
                        if none.name == "None" && none.index == 0 && none.fields.is_empty()
                            && some.name == "Some" && some.index == 1 && some.fields.len() == 1
                );
            if is_option {
                match variant.variants.get(1).and_then(|some| some.fields.first()) {
                    Some(field) => lower_at(runtime, field.ty.id, walk, next)
                        .map(|inner| Ty::Option(Box::new(inner))),
                    None => Err(Deferral::new("type_unresolvable", "malformed Option")),
                }
            } else {
                let mut variants = Vec::with_capacity(variant.variants.len());
                let mut result = Ok(());
                for item in &variant.variants {
                    match item
                        .fields
                        .iter()
                        .map(|field| lower_at(runtime, field.ty.id, walk, next))
                        .collect::<Result<Vec<_>, _>>()
                    {
                        Ok(fields) => variants.push((item.name.clone(), item.index, fields)),
                        Err(error) => {
                            result = Err(error);
                            break;
                        }
                    }
                }
                result.map(|()| {
                    variants.sort_by_key(|(_, index, _)| *index);
                    Ty::Enum(variants)
                })
            }
        }
        TypeDef::BitSequence(_) => Ok(Ty::BitSequence),
    };
    walk.stack.pop();
    lowered
}

/// Accept the live type when it equals the expected one, or differs only by
/// integer widening (recursively). Every widening is recorded.
fn fits(
    expected: &Ty,
    live: &Ty,
    argument: &str,
    adaptations: &mut Vec<Adaptation>,
) -> Result<(), Deferral> {
    if expected == live {
        return Ok(());
    }
    let changed = || {
        Deferral::new(
            "argument_type_changed",
            format!("argument {argument:?} changed from {expected} to {live}"),
        )
    };
    match (expected, live) {
        (Ty::Uint(from), Ty::Uint(to)) | (Ty::Int(from), Ty::Int(to)) if to > from => {
            adaptations.push(Adaptation::Widened {
                argument: argument.into(),
                from: expected.to_string(),
                to: live.to_string(),
            });
            Ok(())
        }
        (Ty::Compact(a), Ty::Compact(b))
        | (Ty::Seq(a), Ty::Seq(b))
        | (Ty::Option(a), Ty::Option(b)) => fits(a, b, argument, adaptations),
        (Ty::Array(n, a), Ty::Array(m, b)) if n == m => fits(a, b, argument, adaptations),
        (Ty::Tuple(a), Ty::Tuple(b)) if a.len() == b.len() => a
            .iter()
            .zip(b)
            .try_for_each(|(a, b)| fits(a, b, argument, adaptations)),
        (Ty::Struct(a), Ty::Struct(b))
            if a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.0 == y.0) =>
        {
            a.iter()
                .zip(b)
                .try_for_each(|(x, y)| fits(&x.1, &y.1, argument, adaptations))
        }
        _ => Err(changed()),
    }
}

/// Resolve a call by name in live metadata and classify its argument list.
pub fn resolve(runtime: &Runtime, expected: &ExpectedCall) -> Result<Resolution, Deferral> {
    let pallet = runtime.pallet(expected.pallet).ok_or_else(|| {
        Deferral::new(
            "pallet_absent",
            format!("pallet {} is absent from live metadata", expected.pallet),
        )
    })?;
    let calls = pallet.calls_type.ok_or_else(|| {
        Deferral::new(
            "call_absent",
            format!("pallet {} has no calls", expected.pallet),
        )
    })?;
    let calls = runtime
        .resolve(calls)
        .map_err(|error| Deferral::new("type_unresolvable", error.to_string()))?;
    let TypeDef::Variant(calls) = &calls.type_def else {
        return Err(Deferral::new(
            "type_unresolvable",
            "the call type is not an enum",
        ));
    };
    let call = calls
        .variants
        .iter()
        .find(|call| call.name == expected.call)
        .ok_or_else(|| {
            Deferral::new(
                "call_absent",
                format!("call {} is absent from live metadata", expected.id()),
            )
        })?;
    let mut adaptations = Vec::new();
    let mut trailing_none = Vec::new();
    let mut walk = Walk {
        nodes: 0,
        stack: Vec::new(),
    };
    for (position, want) in expected.args.iter().enumerate() {
        let Some(field) = call.fields.get(position) else {
            return Err(Deferral::new(
                "argument_removed",
                format!("argument {:?} is absent", want.name),
            ));
        };
        let name = field.name.as_deref().unwrap_or_default();
        if name != want.name {
            let code = if call
                .fields
                .iter()
                .any(|field| field.name.as_deref() == Some(want.name))
            {
                "argument_reordered"
            } else {
                "argument_renamed_or_removed"
            };
            return Err(Deferral::new(
                code,
                format!(
                    "position {position} expected {:?}, found {name:?}",
                    want.name
                ),
            ));
        }
        let live = lower_at(runtime, field.ty.id, &mut walk, 0)?;
        fits(&want.ty, &live, want.name, &mut adaptations)?;
    }
    for field in call.fields.iter().skip(expected.args.len()) {
        let name = field.name.clone().unwrap_or_default();
        match lower_at(runtime, field.ty.id, &mut walk, 0)? {
            Ty::Option(_) if !name.is_empty() => {
                adaptations.push(Adaptation::TrailingOptionNone {
                    argument: name.clone(),
                });
                trailing_none.push(name);
            }
            other => {
                return Err(Deferral::new(
                    "required_argument_added",
                    format!("new argument {name:?} of type {other} has no safe default"),
                ));
            }
        }
    }
    Ok(Resolution {
        pallet_index: pallet.index,
        call_index: call.index,
        adaptations,
        trailing_none,
    })
}

/// Encode positional values for an expected call against live metadata, then
/// decode the bytes back with the same metadata as a type check.
pub fn encode(
    runtime: &Runtime,
    expected: &ExpectedCall,
    values: Vec<Value>,
) -> Result<EncodedCall, Deferral> {
    if values.len() != expected.args.len() {
        return Err(Deferral::new(
            "encoding_failed",
            format!(
                "{} values supplied for {} arguments",
                values.len(),
                expected.args.len()
            ),
        ));
    }
    let resolution = resolve(runtime, expected)?;
    let mut params: Vec<(Value, Value)> = expected
        .args
        .iter()
        .zip(values)
        .map(|(arg, value)| (Value::str(arg.name), value))
        .collect();
    params.extend(
        resolution
            .trailing_none
            .iter()
            .map(|name| (Value::str(name.clone()), Value::Null)),
    );
    let bytes = runtime
        .compose_call(expected.pallet, expected.call, &Value::Dict(params))
        .map_err(|error| Deferral::new("encoding_failed", error.to_string()))?;
    let decoded = decode_named(runtime, &bytes)?;
    if decoded.0 != expected.pallet || decoded.1 != expected.call {
        return Err(Deferral::new(
            "decode_check_failed",
            format!(
                "encoded bytes decode as {}.{}, not {}",
                decoded.0,
                decoded.1,
                expected.id()
            ),
        ));
    }
    Ok(EncodedCall { bytes, resolution })
}

/// Argument values decoded by name, in live declaration order.
pub type NamedArgs = Vec<(String, Value)>;

/// Decode a complete call by live metadata into (pallet, call, named values).
/// Trailing bytes, unknown indices and malformed values are refused.
pub fn decode_named(
    runtime: &Runtime,
    bytes: &[u8],
) -> Result<(String, String, NamedArgs), Deferral> {
    let value = runtime
        .decode_spec(&TypeSpec::Call, bytes, true)
        .map_err(|error| Deferral::new("decode_check_failed", error.to_string()))?;
    let field = |record: &Value, name: &str| -> Option<Value> {
        match record {
            Value::Dict(entries) => entries
                .iter()
                .find(|(key, _)| matches!(key, Value::Str(key) if key == name))
                .map(|(_, value)| value.clone()),
            _ => None,
        }
    };
    let text = |value: Option<Value>| match value {
        Some(Value::Str(text)) => Some(text),
        _ => None,
    };
    let malformed = || Deferral::new("decode_check_failed", "decoded call has no name");
    let pallet = text(field(&value, "call_module")).ok_or_else(malformed)?;
    let call = text(field(&value, "call_function")).ok_or_else(malformed)?;
    let mut args = Vec::new();
    if let Some(Value::List(items)) = field(&value, "call_args") {
        for item in &items {
            let name = text(field(item, "name")).ok_or_else(malformed)?;
            let value = field(item, "value").ok_or_else(malformed)?;
            args.push((name, value));
        }
    }
    Ok((pallet, call, args))
}

/// One signed operation this application performs, with rtao's compiled
/// descriptor for the path that currently composes it.
pub struct Operation {
    pub expected: ExpectedCall,
    /// The rtao descriptor its scheduler composes through at this pin.
    pub upstream: &'static CallDescriptor,
}

/// The weight calls the native adapter signs through rtao's scheduler.
pub fn weight_operations() -> Vec<Operation> {
    use rtao_runtime::calls::subtensor_module as calls;
    vec![
        Operation {
            expected: ExpectedCall {
                pallet: "SubtensorModule",
                call: "commit_timelocked_mechanism_weights",
                args: vec![
                    ExpectedArg {
                        name: "netuid",
                        ty: Ty::Uint(16),
                    },
                    ExpectedArg {
                        name: "mecid",
                        ty: Ty::Uint(8),
                    },
                    ExpectedArg {
                        name: "commit",
                        ty: Ty::seq(Ty::Uint(8)),
                    },
                    ExpectedArg {
                        name: "reveal_round",
                        ty: Ty::Uint(64),
                    },
                    ExpectedArg {
                        name: "commit_reveal_version",
                        ty: Ty::Uint(16),
                    },
                ],
            },
            upstream: &calls::COMMIT_TIMELOCKED_MECHANISM_WEIGHTS,
        },
        Operation {
            expected: ExpectedCall {
                pallet: "SubtensorModule",
                call: "set_mechanism_weights",
                args: vec![
                    ExpectedArg {
                        name: "netuid",
                        ty: Ty::Uint(16),
                    },
                    ExpectedArg {
                        name: "mecid",
                        ty: Ty::Uint(8),
                    },
                    ExpectedArg {
                        name: "dests",
                        ty: Ty::seq(Ty::Uint(16)),
                    },
                    ExpectedArg {
                        name: "weights",
                        ty: Ty::seq(Ty::Uint(16)),
                    },
                    ExpectedArg {
                        name: "version_key",
                        ty: Ty::Uint(64),
                    },
                ],
            },
            upstream: &calls::SET_MECHANISM_WEIGHTS,
        },
    ]
}

/// Evaluate one operation against live metadata.
///
/// `Ok` carries the adaptations it needs (empty when unchanged). The rtao path
/// that composes weight calls at this pin uses its compiled descriptor, which
/// refuses moved indices and any argument shape change; that refusal is
/// reported as `upstream_descriptor_refused` after this module's own
/// classification has passed, so the diagnostic says which side would refuse.
pub fn evaluate(
    runtime: &RuntimeHandle,
    operation: &Operation,
) -> Result<Vec<Adaptation>, Deferral> {
    runtime
        .signing_status()
        .map_err(|error| Deferral::new("signing_unavailable", error.to_string()))?;
    let resolution = resolve(runtime.core(), &operation.expected)?;
    let id = format!(
        "call:{}.{}",
        operation.upstream.pallet, operation.upstream.name
    );
    let report =
        rtao_runtime::verify_surface_with(runtime, &[SurfaceDescriptor::Call(operation.upstream)]);
    let breaking = report
        .changes
        .iter()
        .find(|change| change.class == SurfaceClass::Breaking);
    if breaking.is_some() || !report.shape_matches(&id) {
        let detail = breaking.map_or_else(
            || {
                report
                    .changes
                    .iter()
                    .map(|change| change.detail.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            },
            |change| change.detail.clone(),
        );
        return Err(Deferral::new(
            "upstream_descriptor_refused",
            format!(
                "live metadata is encodable by name, but the pinned rtao weight path composes only its compiled descriptor: {detail}"
            ),
        ));
    }
    Ok(resolution.adaptations)
}

/// Storage items rtao's weight scheduler, weight preflight and reveal watcher
/// read through compiled typed descriptors at this pin. rtao's typed read
/// refuses the whole read (`SurfaceChanged`) when any one of them no longer
/// matches live metadata, with no by-name fallback, so finalized recovery,
/// reveal tracking and every weight operation stop together.
pub fn upstream_storage() -> Vec<StorageSurface> {
    use rtao_runtime::storage::{drand, subtensor_module as st};
    vec![
        st::UIDS.surface(),
        st::LAST_UPDATE.surface(),
        st::WEIGHTS_SET_RATE_LIMIT.surface(),
        st::COMMIT_REVEAL_WEIGHTS_ENABLED.surface(),
        st::COMMIT_REVEAL_WEIGHTS_VERSION.surface(),
        st::REVEAL_PERIOD_EPOCHS.surface(),
        st::WEIGHTS_VERSION_KEY.surface(),
        st::MIN_ALLOWED_WEIGHTS.surface(),
        st::MAX_WEIGHTS_LIMIT.surface(),
        st::SUBNETWORK_N.surface(),
        st::SUBNET_EPOCH_CONSENSUS.surface(),
        st::VALIDATOR_PERMIT.surface(),
        st::TEMPO.surface(),
        st::LAST_EPOCH_BLOCK.surface(),
        st::PENDING_EPOCH_AT.surface(),
        st::SUBNET_EPOCH_INDEX.surface(),
        st::BLOCKS_SINCE_LAST_STEP.surface(),
        st::MECHANISM_COUNT_CURRENT.surface(),
        st::ACTIVITY_CUTOFF_FACTOR_MILLI.surface(),
        st::STAKE_THRESHOLD.surface(),
        st::SUBNET_OWNER_HOTKEY.surface(),
        st::TIMELOCKED_WEIGHT_COMMITS.surface(),
        drand::LAST_STORED_ROUND.surface(),
    ]
}

/// Check rtao's compiled storage descriptors against live metadata with the
/// same test its typed reads apply (`shape_matches`). A refusal names every
/// item that no longer matches; it is reported as `upstream_storage_refused`.
pub fn evaluate_upstream_storage(runtime: &RuntimeHandle) -> Result<(), Deferral> {
    let surfaces = upstream_storage();
    let descriptors: Vec<_> = surfaces
        .iter()
        .map(|surface| SurfaceDescriptor::Storage(*surface))
        .collect();
    let report = rtao_runtime::verify_surface_with(runtime, &descriptors);
    let refused: Vec<String> = surfaces
        .iter()
        .map(|surface| format!("{}.{}", surface.pallet, surface.name))
        .filter(|item| !report.shape_matches(&format!("storage:{item}")))
        .collect();
    if refused.is_empty() {
        return Ok(());
    }
    Err(Deferral::new(
        "upstream_storage_refused",
        format!(
            "the pinned rtao weight scheduler and reveal watcher read {} only through compiled descriptors that no longer match live metadata, so finalized recovery and weight preparation fail until rtao reads them by name",
            refused.join(", ")
        ),
    ))
}
