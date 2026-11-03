//! Slop Ninja *A* artifact spec (`slop_ninja_a_artifact_spec_v1`, `_v2` and
//! `_v3`, [`spec::Version`]) and its admission checks. Spec v2 adds the span
//! class and heads v2 token heads; spec v3 adds the span tower class and heads
//! v3. Everything below holds for every version unless a version is named.
//!
//! One artifact format: the protocol **artifact object**
//! ([`slop_ninja_protocol::artifact_object`]), which a miner commits to by
//! its inference-content digest and uploads. The scorer's parts manifest
//! (`slop_ninja_a_artifact_files_v1`) is derived from the object by an exact,
//! deterministic mapping ([`mapping`]), so the coordinator never needs a
//! second upload format.
//!
//! The checks run before any runtime opens a file:
//!
//! - **File table** ([`spec::ROLES`]): exactly six files with fixed names and
//!   roles. Any other file is undeclared and refused. Every length is checked
//!   from the header against the runtime that reads the file: the graph must
//!   have a served class's exact length, the tokenizer the pinned length,
//!   `heads.json` at most the heads runtime's limit, and every file at most
//!   the scorer's 64 parts.
//! - **Tokenizer**: byte-identical to the pinned tokenizer.
//! - **Role schemas** ([`roles`]): the canonical JSON layout of each file
//!   ([`canonical`]), typed schemas for the heads, contract, calibration and
//!   export manifest, the pinned output contract and calibration policy, the
//!   scorer's load guard evaluated with its own predicates ([`guard`]), and
//!   further cross-file bindings. The heads are also parsed by the scorer's
//!   own heads runtime.
//! - **Graph** ([`onnx`]): a streaming protobuf pre-parse that refuses custom
//!   operator domains, external data, unknown fields and operators outside
//!   the allow-list, counts parameters against the bound, and computes the
//!   structure digest that must match an allow-listed, served graph class.
//!
//! [`text`] records the canonical text profile as the scorer implements it.
//!
//! Everything here is pure computation over bytes, with every bound taken from
//! the versioned spec. It keeps no global state, so it builds for
//! `wasm32-unknown-unknown`; filesystem helpers sit behind the `native`
//! feature.

pub mod canonical;
pub mod guard;
pub mod mapping;
pub mod onnx;
pub mod roles;
pub mod spec;
pub mod text;

#[cfg(feature = "native")]
pub mod fs;

// The scorer's heads runtime, compiled from the same sources the scorer
// compiles. `spec::HEADS_RUNTIME_SHA256` pins their digests. They belong to
// `grammar-a-baseline`, so this crate's formatter skips them.
#[rustfmt::skip]
#[allow(dead_code, clippy::all, unused_imports)]
#[path = "../../../../grammar/crates/grammar-a-baseline/src/full_a.rs"]
mod full_a;
#[rustfmt::skip]
#[allow(dead_code, clippy::all, unused_imports)]
#[path = "../../../../grammar/crates/grammar-a-baseline/src/full_a_spans.rs"]
mod full_a_spans;
#[rustfmt::skip]
#[allow(dead_code, clippy::all, unused_imports)]
#[path = "../../../../grammar/crates/grammar-a-baseline/src/involvement_calibration.rs"]
mod involvement_calibration;
#[rustfmt::skip]
#[allow(dead_code, clippy::all, unused_imports)]
#[path = "../../../../grammar/crates/grammar-a-baseline/src/shared.rs"]
mod shared;
// Spec v3: the span tower runtime, which reads heads v3 and hands the pinned
// runtime above heads v2. `spec::TOWER_RUNTIME_SHA256` pins its digest.
#[rustfmt::skip]
#[allow(dead_code, clippy::all, unused_imports)]
#[path = "../../../../grammar/crates/grammar-a-baseline/src/full_a_tower.rs"]
mod full_a_tower;

/// The span tower runtime source compiled into this crate (spec v3).
pub const TOWER_RUNTIME_SOURCE: &[u8] =
    include_bytes!("../../../../grammar/crates/grammar-a-baseline/src/full_a_tower.rs");

// The scorer's load guard for the forms the release guard does not take
// (spec v2). It belongs to the scorer, so this crate's formatter skips it;
// `spec::SCORER_LOAD_GUARD_SHA256` pins its digest.
#[rustfmt::skip]
#[allow(dead_code, clippy::all)]
#[path = "../../../../inference/a-container-serve/src/load_guard.rs"]
pub mod load_guard;

/// The scorer's load guard source compiled into this crate.
pub const LOAD_GUARD_SOURCE: &[u8] =
    include_bytes!("../../../../inference/a-container-serve/src/load_guard.rs");

/// The heads runtime sources compiled into this crate, by the names the
/// served profile pins.
pub const HEADS_RUNTIME_SOURCES: [(&str, &[u8]); 4] = [
    (
        "full_a.rs",
        include_bytes!("../../../../grammar/crates/grammar-a-baseline/src/full_a.rs"),
    ),
    (
        "full_a_spans.rs",
        include_bytes!("../../../../grammar/crates/grammar-a-baseline/src/full_a_spans.rs"),
    ),
    (
        "involvement_calibration.rs",
        include_bytes!(
            "../../../../grammar/crates/grammar-a-baseline/src/involvement_calibration.rs"
        ),
    ),
    (
        "shared.rs",
        include_bytes!("../../../../grammar/crates/grammar-a-baseline/src/shared.rs"),
    ),
];

/// Why an artifact is refused. [`Refusal::code`] is a stable identifier for
/// tooling; the `Display` text adds detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The protocol object is malformed or a file does not match its header.
    Object(String),
    /// The header's contract is not the spec's output contract.
    Contract(String),
    /// A file name outside the spec's file table.
    UndeclaredFile(String),
    /// A file of the table is absent.
    MissingFile(&'static str),
    /// A file has the wrong header role.
    Role {
        name: String,
        expected: slop_ninja_protocol::FileRole,
    },
    /// The tokenizer is not byte-identical to the pinned tokenizer.
    Tokenizer,
    /// A declared file length is outside the bound of the runtime that reads
    /// the file (empty, past the scorer's parts, or past the heads runtime).
    FileSize {
        name: String,
        len: u64,
        max: u64,
    },
    /// The JSON role files need more buffer than the caller's budget.
    JsonBudget {
        need: u64,
        budget: u64,
    },
    /// The JSON role files' value costs sum past the budget.
    JsonValueBudget {
        need: u64,
        budget: u64,
    },
    /// A JSON role file is not in its canonical layout.
    JsonLayout {
        file: &'static str,
        detail: String,
    },
    /// A role file does not match its schema.
    Schema {
        file: &'static str,
        detail: String,
    },
    /// Files that must agree do not.
    Binding(String),
    GraphMalformed(String),
    GraphUnknownField {
        message: &'static str,
        field: u64,
    },
    GraphCustomDomain(String),
    GraphExternalData,
    GraphUnsupported(String),
    GraphOpNotAllowed(String),
    GraphDtypeNotAllowed(i32),
    /// The declared graph length is no allow-listed class's exact length.
    GraphSize(u64),
    /// An allow-listed graph class the scorer does not serve yet.
    GraphClassNotServed(&'static str),
    TooManyParameters {
        count: u64,
        max: u64,
    },
    /// The structure digest matches no allow-listed graph class.
    GraphNotAllowed {
        structure_sha256: String,
    },
    /// The heads and the graph class disagree (span head with a plain graph,
    /// or the reverse).
    GraphClass(String),
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::Object(_) => "object",
            Refusal::Contract(_) => "contract_id",
            Refusal::UndeclaredFile(_) => "undeclared_file",
            Refusal::MissingFile(_) => "missing_file",
            Refusal::Role { .. } => "file_role",
            Refusal::Tokenizer => "tokenizer_mismatch",
            Refusal::FileSize { .. } => "file_size",
            Refusal::JsonBudget { .. } => "json_budget",
            Refusal::JsonValueBudget { .. } => "json_value_budget",
            Refusal::JsonLayout { .. } => "json_layout",
            Refusal::Schema { .. } => "schema",
            Refusal::Binding(_) => "binding",
            Refusal::GraphMalformed(_) => "graph_malformed",
            Refusal::GraphUnknownField { .. } => "graph_unknown_field",
            Refusal::GraphCustomDomain(_) => "graph_custom_domain",
            Refusal::GraphExternalData => "graph_external_data",
            Refusal::GraphUnsupported(_) => "graph_unsupported",
            Refusal::GraphOpNotAllowed(_) => "graph_op_not_allowed",
            Refusal::GraphDtypeNotAllowed(_) => "graph_dtype_not_allowed",
            Refusal::GraphSize(_) => "graph_size",
            Refusal::GraphClassNotServed(_) => "graph_class_not_served",
            Refusal::TooManyParameters { .. } => "too_many_parameters",
            Refusal::GraphNotAllowed { .. } => "graph_not_allowed",
            Refusal::GraphClass(_) => "graph_class",
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Object(d) => write!(f, "artifact object: {d}"),
            Refusal::Contract(c) => write!(f, "contract {c:?} is not {}", spec::CONTRACT_ID),
            Refusal::UndeclaredFile(n) => write!(f, "undeclared file {n:?}"),
            Refusal::MissingFile(n) => write!(f, "missing file {n}"),
            Refusal::Role { name, expected } => {
                write!(f, "{name} must have header role {expected:?}")
            }
            Refusal::Tokenizer => write!(
                f,
                "tokenizer.json is not byte-identical to the pinned tokenizer"
            ),
            Refusal::FileSize { name, len, max } => {
                write!(f, "{name}: declared length {len} is outside 1..={max}")
            }
            Refusal::JsonBudget { need, budget } => {
                write!(
                    f,
                    "JSON role files need {need} bytes; the budget is {budget}"
                )
            }
            Refusal::JsonValueBudget { need, budget } => {
                write!(
                    f,
                    "JSON role files have a value cost of {need} bytes; the budget is {budget}"
                )
            }
            Refusal::JsonLayout { file, detail } => {
                write!(f, "{file}: not the canonical JSON layout: {detail}")
            }
            Refusal::Schema { file, detail } => write!(f, "{file}: {detail}"),
            Refusal::Binding(d) => write!(f, "binding: {d}"),
            Refusal::GraphMalformed(d) => write!(f, "graph: malformed protobuf: {d}"),
            Refusal::GraphUnknownField { message, field } => {
                write!(
                    f,
                    "graph: field {field} of {message} is outside the accepted ONNX subset"
                )
            }
            Refusal::GraphCustomDomain(d) => write!(f, "graph: custom operator domain {d:?}"),
            Refusal::GraphExternalData => write!(f, "graph: external tensor data"),
            Refusal::GraphUnsupported(d) => write!(f, "graph: unsupported {d}"),
            Refusal::GraphOpNotAllowed(op) => {
                write!(f, "graph: operator {op:?} is not allow-listed")
            }
            Refusal::GraphDtypeNotAllowed(t) => {
                write!(f, "graph: element type {t} is not allow-listed")
            }
            Refusal::GraphSize(len) => {
                write!(f, "graph length {len} is no allow-listed class's length")
            }
            Refusal::GraphClassNotServed(id) => {
                write!(f, "graph class {id} is allow-listed but not served yet")
            }
            Refusal::TooManyParameters { count, max } => {
                write!(f, "{count} parameters exceed the bound of {max}")
            }
            Refusal::GraphNotAllowed { structure_sha256 } => {
                write!(
                    f,
                    "graph structure {structure_sha256} is not an allow-listed graph class"
                )
            }
            Refusal::GraphClass(d) => write!(f, "graph class: {d}"),
        }
    }
}

impl std::error::Error for Refusal {}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// `value` with every object's keys in sorted order, at every depth.
///
/// A digest over `serde_json` output must not depend on the build. A build
/// that also links rtao unifies `serde_json`'s `preserve_order` feature in,
/// and a `Map` then keeps insertion order instead of sorting its keys, so the
/// records this crate hashes ([`spec::spec_json`], [`text::profile_json`])
/// are sorted explicitly: every build prints and hashes the same bytes.
pub fn sorted_keys(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k, sorted_keys(v)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted_keys).collect()),
        other => other,
    }
}

pub use mapping::{ObjectCheck, ObjectReport, PartsManifest, parts_manifest};
pub use roles::{
    RoleFiles, RolesReport, check_layout, check_layout_cost, check_roles, check_roles_with,
    check_tokenizer,
};
pub use spec::{GraphClass, SPEC_ID, SPEC_ID_V2, Version};
