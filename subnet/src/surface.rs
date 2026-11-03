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
    CallDescriptor, RuntimeHandle, StorageSurface,
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
    /// rtao's compiled descriptor no longer matches this call, and rtao
    /// composes it by name against live metadata instead (D-088).
    UpstreamComposedByName,
    /// rtao's compiled descriptors no longer match these storage items or
    /// constants, and rtao reads them by name against live metadata (D-089).
    UpstreamReadByName { items: Vec<String> },
    /// The stored `CommitRevealWeightsVersion` differs from the payload
    /// version rtao encodes. rtao follows the stored tag under runtime code it
    /// reviewed or saw reveal its payload, and otherwise pauses timelocked
    /// commits for the mechanism and records the refusal as the attempt's
    /// outcome; either way it decides per attempt (D-086).
    CommitRevealTagChanged { stored: u16 },
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
/// descriptor for the call (its preferred composer while the shape matches).
pub struct Operation {
    pub expected: ExpectedCall,
    /// The rtao descriptor its scheduler composes through while the live
    /// shape matches; otherwise rtao composes the call by name.
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
/// `Ok` carries the adaptations it needs (empty when unchanged). This module's
/// own by-name classification runs first. rtao then composes the call through
/// its compiled descriptor while the live shape matches, and otherwise by name
/// against live metadata ([`Adaptation::UpstreamComposedByName`]). Only a call
/// that neither can compose is deferred, as `upstream_compose_refused`, with
/// rtao's own verdict ([`rtao_neuron::weights::ValidatorSurface`]).
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
    let upstream = rtao_neuron::weights::ValidatorSurface::check(runtime);
    let prefix = format!("{id}: ");
    if let Some(line) = upstream
        .incompatible
        .iter()
        .find(|line| line.starts_with(&prefix))
    {
        return Err(Deferral::new(
            "upstream_compose_refused",
            format!(
                "live metadata is encodable by name here, but rtao composes this call neither through its compiled descriptor nor by name: {}",
                &line[prefix.len()..]
            ),
        ));
    }
    let mut adaptations = resolution.adaptations;
    if !runtime.surface_report().shape_matches(&id) {
        adaptations.push(Adaptation::UpstreamComposedByName);
    }
    Ok(adaptations)
}

/// Storage items rtao's weight preflight, scheduler and reveal watcher read:
/// rtao's own lists, so this check never drifts from what it reads. Each read
/// uses the compiled descriptor while its shape matches and is otherwise
/// resolved by name against live metadata, or refused for that item alone
/// when its meaning changed (D-089).
pub fn upstream_storage() -> Vec<StorageSurface> {
    rtao_neuron::weights::PREFLIGHT_STORAGE
        .iter()
        .chain(&rtao_neuron::weights::RUN_PATH_STORAGE)
        .copied()
        .collect()
}

/// rtao's verdict on the storage and constants its validator run path reads.
/// `Ok` lists the items it reads by name ([`Adaptation::UpstreamReadByName`],
/// empty when every compiled descriptor matches). An item that neither the
/// compiled descriptor nor the name-resolved read accepts is reported as
/// `upstream_storage_refused`: rtao's finalized observation and recovery stop
/// until it supports the item.
pub fn evaluate_upstream_storage(runtime: &RuntimeHandle) -> Result<Vec<Adaptation>, Deferral> {
    let upstream = rtao_neuron::weights::ValidatorSurface::check(runtime);
    let refused: Vec<&str> = upstream
        .incompatible
        .iter()
        .filter(|line| line.starts_with("storage:") || line.starts_with("constant:"))
        .map(|line| line.split(": ").next().unwrap_or(line))
        .collect();
    if !refused.is_empty() {
        return Err(Deferral::new(
            "upstream_storage_refused",
            format!(
                "rtao reads {} neither through its compiled descriptor nor by name against live metadata, so finalized recovery and weight preparation stop until rtao supports the live item",
                refused.join(", ")
            ),
        ));
    }
    let report = runtime.surface_report();
    let items: Vec<String> = upstream_storage()
        .iter()
        .map(|surface| format!("storage:{}.{}", surface.pallet, surface.name))
        .chain(
            rtao_neuron::weights::RUN_PATH_CONSTANTS
                .iter()
                .map(|constant| format!("constant:{}.{}", constant.pallet, constant.name)),
        )
        .filter(|id| !report.shape_matches(id))
        .collect();
    Ok(if items.is_empty() {
        Vec::new()
    } else {
        vec![Adaptation::UpstreamReadByName { items }]
    })
}
