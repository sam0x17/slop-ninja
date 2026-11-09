//! The versioned spec values. Every bound the checks apply is here; none is
//! tuned. Each comes from a schema, an allow-list or a measured reference
//! (R0), and [`spec_json`] serializes all of them so the spec has one digest.
//!
//! Three versions are defined ([`Version`]). Spec v1 is unchanged: its values,
//! record and digest are the ones published first. Spec v2 keeps every v1
//! value and adds what serving span heads needs: the span class is served,
//! heads v2 with a spans-v1 token head (export v3, calibration v3) are
//! admitted, the scorer's load guard for those forms is a shared source pinned
//! by digest, and the JSON role files share one budget ([`MAX_JSON_BYTES_V2`]).
//! Spec v3 keeps every v2 value for the plain and span classes and adds the
//! span tower class ([`TOWER_GRAPH`]): heads v3 whose token head reads the
//! tower's normed states (export v4, calibration v3), with its own parameter
//! and JSON value bounds, so that v3 admits every artifact v2 admits.
//! Spec v4 keeps every v3 value and states the graph class rule as a byte
//! comparison of the graph's structure stream with the class's pinned
//! reference stream ([`reference_structure`]) instead of a digest of it, so
//! nothing hashes a graph byte. It admits exactly what v3 admits. Every
//! version's check runs the comparison: each class's v1 to v3 digest is its
//! reference stream's digest.

use serde_json::{Value, json};
use slop_ninja_protocol::FileRole;

use crate::canonical::Layout;
use crate::onnx::{GraphPolicy, attr_type, dtype};

/// Spec identifier of spec v1. A change to any value below is a new version.
pub const SPEC_ID: &str = "slop_ninja_a_artifact_spec_v1";

/// Spec identifier of spec v2.
pub const SPEC_ID_V2: &str = "slop_ninja_a_artifact_spec_v2";

/// Spec identifier of spec v3.
pub const SPEC_ID_V3: &str = "slop_ninja_a_artifact_spec_v3";

/// Spec identifier of spec v4.
pub const SPEC_ID_V4: &str = "slop_ninja_a_artifact_spec_v4";

/// A spec version. The checks take the version they apply; the functions
/// without a version (`spec_json`, `spec_sha256`, `ObjectCheck::new`,
/// `fs::check_dir`, `mapping::check_header`, `roles::check_roles`,
/// [`graph_class`], [`graph_class_by_len`]) are spec v1 (the last two also
/// spec v2, which defines the same classes). Versions are ordered by age.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Version {
    V1,
    V2,
    V3,
    V4,
}

impl Version {
    /// The newest version.
    pub const LATEST: Version = Version::V4;
    pub const ALL: [Version; 4] = [Version::V1, Version::V2, Version::V3, Version::V4];

    pub fn id(self) -> &'static str {
        match self {
            Version::V1 => SPEC_ID,
            Version::V2 => SPEC_ID_V2,
            Version::V3 => SPEC_ID_V3,
            Version::V4 => SPEC_ID_V4,
        }
    }

    /// The graph classes this version defines, in record order: spec v1 and
    /// v2 the two classes of [`GRAPHS`], spec v3 and v4 also [`TOWER_GRAPH`].
    pub fn classes(self) -> &'static [GraphClass] {
        match self {
            Version::V1 | Version::V2 => &GRAPHS,
            Version::V3 | Version::V4 => &GRAPHS_V3,
        }
    }

    /// The class of this version whose spec v1 to v3 structure digest is
    /// `structure_sha256`, if any. A record lookup: no check computes the
    /// digest of a graph (see [`reference_structure`]).
    pub fn graph_class(self, structure_sha256: &str) -> Option<&'static GraphClass> {
        self.classes()
            .iter()
            .find(|g| g.structure_sha256 == structure_sha256)
    }

    /// The class of this version whose exact length is `len`, if any.
    pub fn graph_class_by_len(self, len: u64) -> Option<&'static GraphClass> {
        self.classes().iter().find(|g| g.bytes == len)
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.id() == id)
    }

    /// The version a cohort names by id and digest, when this build defines it.
    pub fn named(id: &str, sha256: &str) -> Option<Self> {
        Self::from_id(id).filter(|v| v.spec_sha256() == sha256)
    }

    /// Whether the scorer serves `class` under this version. Spec v1 serves
    /// the plain class only; spec v2 and later serve every class they define.
    pub fn serves(self, class: &GraphClass) -> bool {
        self.classes().contains(class)
            && match self {
                Version::V1 => class.served,
                Version::V2 | Version::V3 | Version::V4 => true,
            }
    }

    /// The spec-level bound on the sum of the JSON role files' lengths, if any.
    pub fn max_json_bytes(self) -> Option<u64> {
        match self {
            Version::V1 => None,
            Version::V2 | Version::V3 | Version::V4 => Some(MAX_JSON_BYTES_V2),
        }
    }

    /// The spec-level bound on the sum of the JSON role files' value costs
    /// ([`crate::canonical::VALUE_COST_ID`]) when the graph class is not
    /// known, if any: the smallest of the version's class bounds.
    pub fn max_json_value_bytes(self) -> Option<u64> {
        match self {
            Version::V1 => None,
            Version::V2 | Version::V3 | Version::V4 => {
                self.classes().iter().map(|g| g.max_json_value_bytes).min()
            }
        }
    }

    /// The bound on the JSON role files' summed value cost for an artifact of
    /// `class`, if any. Spec v2 bounds every class by [`MAX_JSON_VALUE_BYTES_V2`];
    /// spec v3 and v4 bound each class by its own value, which is v2's for the
    /// plain and span classes.
    pub fn max_json_value_bytes_for(self, class: &GraphClass) -> Option<u64> {
        match self {
            Version::V1 => None,
            Version::V2 | Version::V3 | Version::V4 => Some(class.max_json_value_bytes),
        }
    }

    /// Whether this version admits heads v3 on the span tower class (spec v3
    /// and later).
    pub fn has_tower(self) -> bool {
        self >= Version::V3
    }

    pub fn spec_json(self) -> Value {
        match self {
            Version::V1 => spec_json(),
            Version::V2 => spec_json_v2(),
            Version::V3 => spec_json_v3(),
            Version::V4 => spec_json_v4(),
        }
    }

    pub fn spec_sha256(self) -> String {
        crate::sha256_hex(&serde_json::to_vec(&self.spec_json()).expect("spec serializes"))
    }
}

/// Output contract an artifact implements: the protocol header's `contract`,
/// the scorer manifest's `contract` and `contract.json`'s `schema`.
pub const CONTRACT_ID: &str = "slop_ninja_full_a_contract_v2";

/// The scorer's parts manifest schema.
pub const PARTS_SCHEMA: &str = "slop_ninja_a_artifact_files_v1";

/// Part size of the parts mapping: the scorer's `MAX_PART_BYTES`
/// (`inference/a-container-serve/src/artifact.rs`), the size R0's deployed
/// package was packed with.
pub const PART_BYTES: u64 = 64 << 20;

/// Most parts per file: the scorer's `MAX_PARTS_PER_FILE` (`artifact.rs`).
pub const MAX_PARTS_PER_FILE: u64 = 64;

/// Largest file the scorer can load: [`MAX_PARTS_PER_FILE`] parts of
/// [`PART_BYTES`]. The scorer also refuses an empty file.
pub const MAX_FILE_BYTES: u64 = MAX_PARTS_PER_FILE * PART_BYTES;

/// Largest parts manifest: the scorer's `MAX_MANIFEST_BYTES` (`artifact.rs`).
pub const MAX_MANIFEST_BYTES: u64 = 1 << 20;

/// Largest `heads.json`: the heads runtime refuses a larger file
/// (`full_a.rs` `FullAHeads::from_json`, "research size limit").
pub const HEADS_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Largest number of parameters of the plain and span classes (every class of
/// spec v1 and v2): `FLOAT` elements in the graph's initializers and constant
/// attributes plus the head weights in `heads.json`. R0 has 150,058,763 graph
/// and 244,069 head parameters.
pub const MAX_PARAMETERS: u64 = 160_000_000;

/// Head parameters of the pinned document heads ([`heads`]): the architecture
/// fixes them, so every admitted artifact has R0's 244,069.
pub const DOCUMENT_HEAD_PARAMETERS: u64 = 244_069;

/// Widest token head the arm definition admits (`token_head.hidden`, the
/// trainer's 1..=4096).
pub const TOKEN_HEAD_MAX_HIDDEN: u64 = 4096;

/// Parameters of a token head with `hidden` units on 768-wide token states: a
/// 768 x hidden layer with its bias, then a hidden x 1 output with its bias.
pub const fn token_head_parameters(hidden: u64) -> u64 {
    heads::EMBEDDING * hidden + hidden + hidden + 1
}

/// Spec v3: the tower class's graph parameters, fixed by its structure (the
/// qualified export's count; the export test recounts it).
pub const TOWER_GRAPH_PARAMETERS: u64 = 170_151_195;

/// Spec v3: the tower class's parameter bound, derived from its structure: its
/// graph parameters plus the largest heads its form admits, the pinned
/// document heads and the widest token head (heads v3 has no span head).
/// S2c has 170,151,195 graph and 441,190 head parameters.
pub const TOWER_MAX_PARAMETERS: u64 = TOWER_GRAPH_PARAMETERS
    + DOCUMENT_HEAD_PARAMETERS
    + token_head_parameters(TOKEN_HEAD_MAX_HIDDEN);

/// One file of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoleSpec {
    /// The file name, in the object and in the scorer's model directory.
    pub name: &'static str,
    /// Role in the protocol object header.
    pub file_role: FileRole,
    /// Role string in the scorer's parts manifest.
    pub scorer_role: &'static str,
    /// Canonical JSON layout of a JSON role file (see [`crate::canonical`]).
    pub layout: Option<Layout>,
    /// Largest length, from a runtime that reads the file.
    pub max_bytes: u64,
}

/// Graph file name.
pub const GRAPH_FILE: &str = "a-fp32band.onnx";

/// The file table, in the scorer's file order. An artifact has exactly these
/// files. `manifest.json` is the export record: the scorer's load guard reads
/// it, but no prediction depends on it, so it is excluded from inference
/// content identity (owner metadata) like the protocol's other non-inference
/// roles.
pub const ROLES: [RoleSpec; 6] = [
    RoleSpec {
        name: GRAPH_FILE,
        file_role: FileRole::Graph,
        scorer_role: "encoder_graph",
        layout: None,
        max_bytes: MAX_FILE_BYTES,
    },
    RoleSpec {
        name: "heads.json",
        file_role: FileRole::Weights,
        scorer_role: "heads",
        layout: Some(Layout::PythonCompact),
        max_bytes: HEADS_MAX_BYTES,
    },
    RoleSpec {
        name: "contract.json",
        file_role: FileRole::OutputSchema,
        scorer_role: "contract",
        layout: Some(Layout::SerdePretty),
        max_bytes: MAX_FILE_BYTES,
    },
    RoleSpec {
        name: "calibration.json",
        file_role: FileRole::Calibration,
        scorer_role: "calibration",
        layout: Some(Layout::SerdePretty),
        max_bytes: MAX_FILE_BYTES,
    },
    RoleSpec {
        name: "manifest.json",
        file_role: FileRole::OwnerMetadata,
        scorer_role: "export_manifest",
        layout: Some(Layout::PythonCompact),
        max_bytes: MAX_FILE_BYTES,
    },
    RoleSpec {
        name: "tokenizer.json",
        file_role: FileRole::Tokenizer,
        scorer_role: "tokenizer",
        layout: None,
        max_bytes: TOKENIZER_BYTES,
    },
];

pub fn role_by_name(name: &str) -> Option<&'static RoleSpec> {
    ROLES.iter().find(|r| r.name == name)
}

/// The pinned tokenizer: the V6 classifier `tokenizer.json`, shipped as the
/// release `runtime-base/tokenizer.json`.
pub const TOKENIZER_SHA256: &str =
    "6c8aaa9a542084f2457eab775d4eeb51f92a70c0fd9de28d5edb0ddec3c08d30";
pub const TOKENIZER_BYTES: u64 = 3_583_228;

/// One allow-listed graph class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphClass {
    pub id: &'static str,
    /// The spec v1 to v3 structure digest (see [`crate::onnx`]): SHA-256 of
    /// the digest tag and the class's reference structure stream. Recorded,
    /// never computed by a check.
    pub structure_sha256: &'static str,
    /// Length of the class's reference structure stream
    /// ([`reference_structure`]).
    pub structure_bytes: u64,
    /// SHA-256 of the reference structure stream file as shipped, which the
    /// spec v4 record pins. Recorded, never computed by a check.
    pub structure_stream_sha256: &'static str,
    /// Exact file length. The structure stream keeps every byte except the
    /// initializer float payloads, whose lengths it also keeps, so a class
    /// fixes the length of its graph file.
    pub bytes: u64,
    /// Whether the scorer serves the class. Admission refuses an unserved
    /// class (`graph_class_not_served`), so every admitted artifact can be
    /// scored. Serving a class is a spec change.
    pub served: bool,
    /// Graph outputs, in order.
    pub outputs: &'static [&'static str],
    /// Whether the graph outputs the encoder's final-norm token states
    /// (`token_states`). Spec v1 calls this `span_head`: `heads.json` carries a
    /// span head.
    pub span_head: bool,
    /// Spec v3: whether the graph outputs the span tower's normed token states
    /// (`span_tower_token_states`), which a heads v3 token head reads. Not in
    /// the spec v1 and v2 records.
    pub tower_states: bool,
    /// Largest graph plus head parameters of an artifact of this class
    /// ([`MAX_PARAMETERS`], or [`TOWER_MAX_PARAMETERS`]). In the spec v3 record
    /// only.
    pub max_parameters: u64,
    /// Largest summed value cost of the JSON role files of an artifact of this
    /// class under spec v2 and v3 ([`MAX_JSON_VALUE_BYTES_V2`], or
    /// [`MAX_JSON_VALUE_BYTES_TOWER`]). In the spec v3 record only.
    pub max_json_value_bytes: u64,
}

const PLAIN_OUTPUTS: &[&str] = &[
    "origin_logits",
    "involvement_logit",
    "conditional_mixed_share",
    "ai_logits",
    "human_logits",
    "h",
];
const SPAN_OUTPUTS: &[&str] = &[
    "origin_logits",
    "involvement_logit",
    "conditional_mixed_share",
    "ai_logits",
    "human_logits",
    "h",
    "token_states",
];
const TOWER_OUTPUTS: &[&str] = &[
    "origin_logits",
    "involvement_logit",
    "conditional_mixed_share",
    "ai_logits",
    "human_logits",
    "h",
    "span_tower_token_states",
];

/// The allow-listed graphs. Both are the banded f32 ModernBERT-base *A*
/// graph exported by `grammar/crates/grammar-eval/python/export_a_onnx.py`
/// under its pinned environment: `plain` is R0's graph; `span` adds the
/// final-norm token states the span head reads.
pub const GRAPHS: [GraphClass; 2] = [
    GraphClass {
        id: "a-plain-f32band-v1",
        structure_sha256: PLAIN_STRUCTURE_SHA256,
        structure_bytes: PLAIN_STRUCTURE_BYTES,
        structure_stream_sha256: PLAIN_STRUCTURE_STREAM_SHA256,
        bytes: 601_028_257,
        served: true,
        outputs: PLAIN_OUTPUTS,
        span_head: false,
        tower_states: false,
        max_parameters: MAX_PARAMETERS,
        max_json_value_bytes: MAX_JSON_VALUE_BYTES_V2,
    },
    // Not served under spec v1; spec v2 and v3 serve it.
    GraphClass {
        id: "a-span-f32band-v1",
        structure_sha256: SPAN_STRUCTURE_SHA256,
        structure_bytes: SPAN_STRUCTURE_BYTES,
        structure_stream_sha256: SPAN_STRUCTURE_STREAM_SHA256,
        bytes: 601_028_403,
        served: false,
        outputs: SPAN_OUTPUTS,
        span_head: true,
        tower_states: false,
        max_parameters: MAX_PARAMETERS,
        max_json_value_bytes: MAX_JSON_VALUE_BYTES_V2,
    },
];

/// Spec v3: the span tower class, the tower export of `export_a_onnx.py`
/// (`--variant tower`) under its pinned environment. It is the plain graph plus
/// the span tower its heads v3 declare ([`tower::declaration`]), outputting
/// the tower's normed token states. It is outside [`GRAPHS`], whose two
/// classes the spec v1 and v2 records list.
pub const TOWER_GRAPH: GraphClass = GraphClass {
    id: "a-tower-f32band-v1",
    structure_sha256: TOWER_STRUCTURE_SHA256,
    structure_bytes: TOWER_STRUCTURE_BYTES,
    structure_stream_sha256: TOWER_STRUCTURE_STREAM_SHA256,
    bytes: 681_508_964,
    served: true,
    outputs: TOWER_OUTPUTS,
    span_head: false,
    tower_states: true,
    max_parameters: TOWER_MAX_PARAMETERS,
    max_json_value_bytes: MAX_JSON_VALUE_BYTES_TOWER,
};

/// Spec v3's classes, in record order.
pub const GRAPHS_V3: [GraphClass; 3] = [GRAPHS[0], GRAPHS[1], TOWER_GRAPH];

/// R0's graph (`a-fp32band.onnx`, SHA-256 `08848d95…ca4cb7`); the plain
/// export of R0's release has the same structure.
pub const PLAIN_STRUCTURE_SHA256: &str =
    "b5440ec244c6dc519b961560b9cc69201652ec617c7da90ba0bb36fd4c42a731";
/// The span-head export of R0's weights.
pub const SPAN_STRUCTURE_SHA256: &str =
    "3a35507f1233803d5dc5355594d9ec8c0f292d52ac3d6f06df61c259441c2f6a";
/// Spec v3: the tower export of the S2c span tower (R0's encoder and document
/// heads, seed-17 S2c-lr3e-5 tower).
pub const TOWER_STRUCTURE_SHA256: &str =
    "0b1feeb0b90d79d77999993958fdbfd0b033a6d1d9c29d6b08605451839e3b9c";

/// The reference structure streams, shipped with the spec in
/// `subnet/crates/artifact-check/structure/` and compiled into every build
/// that runs the graph check (the scorer and the miner's `check`):
/// `slop_ninja_artifact_check structure FILE OUT` writes a graph's stream.
/// R0's graph gives the plain class's, the span-head export of R0's weights
/// the span class's, and S2c's tower export the tower class's.
pub const PLAIN_STRUCTURE_BYTES: u64 = 1_623_305;
pub const PLAIN_STRUCTURE_STREAM_SHA256: &str =
    "69b149ce63a044efa0de648bd34f8d179f694e89e8ef9bd2ad4dd828faa692a3";
pub const SPAN_STRUCTURE_BYTES: u64 = 1_623_451;
pub const SPAN_STRUCTURE_STREAM_SHA256: &str =
    "e8172eedeb643e33eeea850cb4c138907f9fe258f92071a0cf1dfcb4caf6a68b";
pub const TOWER_STRUCTURE_BYTES: u64 = 1_860_300;
pub const TOWER_STRUCTURE_STREAM_SHA256: &str =
    "55abdd78664fa52446f3beeeef16e8f4f7a82334f0809992a4c8731030865fbc";

/// The pinned reference structure stream of `class`: a graph is of the class
/// when its structure stream ([`crate::onnx`]) equals this byte for byte.
/// `None` for a class this build does not define.
pub fn reference_structure(class: &GraphClass) -> Option<&'static [u8]> {
    let stream: &'static [u8] = match class.id {
        "a-plain-f32band-v1" => include_bytes!("../structure/a-plain-f32band-v1.structure"),
        "a-span-f32band-v1" => include_bytes!("../structure/a-span-f32band-v1.structure"),
        "a-tower-f32band-v1" => include_bytes!("../structure/a-tower-f32band-v1.structure"),
        _ => return None,
    };
    Some(stream)
}

/// The spec v1 and v2 class with this structure digest
/// ([`Version::graph_class`] for any version).
pub fn graph_class(structure_sha256: &str) -> Option<&'static GraphClass> {
    Version::V2.graph_class(structure_sha256)
}

/// The spec v1 and v2 class whose exact length is `len`, if any
/// ([`Version::graph_class_by_len`] for any version).
pub fn graph_class_by_len(len: u64) -> Option<&'static GraphClass> {
    Version::V2.graph_class_by_len(len)
}

/// Operator domains: the allow-listed graphs use the default domain only.
pub const DOMAINS: &[&str] = &[""];

/// Operators of the allow-listed graphs (ONNX default domain, opset 17).
pub const OPS: &[&str] = &[
    "Add",
    "Cast",
    "Concat",
    "Constant",
    "ConstantOfShape",
    "Cos",
    "Div",
    "Equal",
    "Erf",
    "Expand",
    "Gather",
    "Gemm",
    "LayerNormalization",
    "LogSoftmax",
    "MatMul",
    "Mul",
    "Neg",
    "Pad",
    "Range",
    "ReduceLogSumExp",
    "ReduceSum",
    "Reshape",
    "Shape",
    "Sigmoid",
    "Sin",
    "Slice",
    "Softmax",
    "Split",
    "Squeeze",
    "Sub",
    "Tanh",
    "Transpose",
    "Unsqueeze",
    "Where",
];

/// Element types of the allow-listed graphs.
pub const DTYPES: &[i32] = &[dtype::FLOAT, dtype::INT64];

/// Attribute types of the allow-listed graphs.
pub const ATTR_TYPES: &[i64] = &[
    attr_type::FLOAT,
    attr_type::INT,
    attr_type::STRING,
    attr_type::TENSOR,
    attr_type::INTS,
];

/// The graph walk's policy for the plain and span classes.
pub const GRAPH_POLICY: GraphPolicy<'static> = GraphPolicy {
    domains: DOMAINS,
    ops: OPS,
    dtypes: DTYPES,
    attr_types: ATTR_TYPES,
    max_parameters: MAX_PARAMETERS,
    refuse_subnormal: true,
};

/// The graph walk's policy for a graph of `class`: [`GRAPH_POLICY`] with the
/// class's parameter bound.
pub const fn graph_policy(class: &GraphClass) -> GraphPolicy<'static> {
    GraphPolicy {
        max_parameters: class.max_parameters,
        ..GRAPH_POLICY
    }
}

/// Heads runtime sources compiled into the heads check
/// (`grammar/crates/grammar-a-baseline/src`): the files as they are on main.
/// The scorer compiles the same four files (`full_a.rs` reads `full_a_spans`)
/// and checks them at start against its served profile
/// (`heads_runtime_sha256`), which pins these digests; `tests/spec.rs` checks
/// that the two agree.
pub const HEADS_RUNTIME_SHA256: [(&str, &str); 4] = [
    (
        "full_a.rs",
        "90a387169d38487615c30cb03b8e81f6ef2a76abdc783b0afc1da7b5dc286369",
    ),
    (
        "full_a_spans.rs",
        "70cf814b378ef12acdab25d37d9c1e0f49810a7d649a981d90a2ee16bf6a8b5f",
    ),
    (
        "involvement_calibration.rs",
        "0893526927b09ef22038e1f1c03376546b2c35781366618e5b828d0223196f7e",
    ),
    (
        "shared.rs",
        "ef1db317e5559e00d7f0757c170f76bb0d972328e8968e87e2d75177874e5043",
    ),
];

/// The pinned output contract: the `contract.json` fields that define what
/// the model reads and returns. Training-provenance fields (normalization fit,
/// bundle and parent digests) are typed but free.
pub mod contract {
    pub const INPUT_MODE: &str = "text";
    pub const RAW_DIMENSION: u64 = 768;
    pub const MAX_TOKENS_INCLUDING_SPECIALS: u64 = 2048;
    pub const ORIGIN_ORDER: [&str; 3] = ["human_only", "model_only", "mixed"];
    pub const SPAN_COORDINATES: &str = "utf8_byte_offsets_in_exact_final_text";
    pub const SPAN_LABEL_POLICY: &str = "slop_ninja_token_span_label_v1";
    pub const ZERO_CFG_COORDINATES: u64 = 1820;
    pub const AI_ROSTER_RULE: &str =
        "sorted generator labels of Train rows with a positive cohort weight";
    pub const NORMALIZATION_SCHEMA: &str = "slop_ninja_v6_train_family_normalization_v1";
    /// The human roster is empty.
    pub const HUMAN_PROFILES: usize = 0;
    /// SHA-256 of the compact JSON array of the 33 AI profile names, in
    /// contract order (R0's `ai_profiles`).
    pub const AI_PROFILES_SHA256: &str =
        "2e1c6f39d0f313959a43c6478843934633fb812ad0b109c4245faeb6ee37fa55";
    pub const AI_PROFILES: usize = 33;
}

/// Pinned heads architecture (`heads.json` `architecture`). The graph holds
/// the same document heads, so its structure fixes these sizes.
pub mod heads {
    pub const SCHEMA: &str = "slop_ninja_full_a_heads_v1";
    pub const DIMENSION: u64 = 2588;
    pub const RAW_TEXT_DIMENSION: u64 = 768;
    pub const HIDDEN: u64 = 64;
    pub const EMBEDDING: u64 = 768;
}

/// Pinned calibration map (`heads.json` `involvement_calibration` and
/// `calibration.json` `involvement.selected`).
pub mod calibration {
    pub const SCHEMA: &str = "slop_ninja_full_a_calibration_v2";
    pub const FIT_SPLIT: &str = "cal";
    pub const METHOD: &str = "isotonic_v1_clipped";
    pub const ALGORITHM: &str = "exact_score_tie_aggregation_then_weighted_pool_adjacent_violators";
    pub const INTERPOLATION: &str =
        "linear_between_adjacent_unique_CAL_score_knots; constant_at_each_end_outside_CAL_support";
    pub const KNOT_KEYS: [&str; 5] = [
        "involvement",
        "rows",
        "score",
        "weight",
        "weighted_positives",
    ];
    pub const MAP_KEYS: [&str; 11] = [
        "algorithm",
        "bound",
        "hi",
        "input",
        "interpolation",
        "knots",
        "lo",
        "method",
        "target",
        "temperature",
        "temperature_rule",
    ];
}

/// The pinned calibration policy (`calibration.json`
/// `involvement.report.policy`): the policy under which the pinned map is
/// selected.
pub mod calibration_policy {
    pub const SCHEMA: &str = "slop_ninja_involvement_calibration_policy_v3";
    pub const PRIMARY: &str = "isotonic_v1_clipped";
    pub const FALLBACK: &str = "origin_temperature_v1";
    pub const SELECTION: &str = "isotonic_v1_clipped_unless_recorded_mechanical_failure_nonfinite_value_empty_class_or_fit_error";
    pub const BOUND: &str = "lo=1/(2*CAL human documents), hi=1-1/(2*CAL positive documents); every PAV level and every output clamped to [lo,hi]";
    pub const TEMPERATURE_RULE: &str = "one scalar T on the three origin logits; balanced-weighted three-class NLL on CAL; T=exp(0.01k), integer k=-300..300; nearest 1 on an exact tie";
}

/// Pinned export manifest schema.
pub const EXPORT_SCHEMA: &str = "slop_ninja_full_a_export_v2";
pub const FIT_BINDING_SCHEMA: &str = "slop_ninja_full_a_fit_binding_v2";

/// Spec v2: the export manifest that carries a heads v2 token head.
pub const EXPORT_SCHEMA_V3: &str = "slop_ninja_full_a_export_v3";
/// Spec v2: heads with the spans-v1 token head and span emission spec.
pub const HEADS_SCHEMA_V2: &str = "slop_ninja_full_a_heads_v2";
/// Spec v2: the calibration record of an export v3 (calibration v2 plus the
/// exported span emission spec).
pub const CALIBRATION_SCHEMA_V3: &str = "slop_ninja_full_a_calibration_v3";

/// Spec v2: largest sum of the four JSON role files' lengths. It is four
/// times the heads runtime's limit ([`HEADS_MAX_BYTES`]), the only JSON bound
/// a runtime sets, so every artifact whose JSON files each fit that limit is
/// admitted. Admission refuses a larger header before any file byte
/// (`json_budget`), and the scorer buffers and parses the JSON files within
/// it. Spec v1 has no such bound (only the caller's budget).
pub const MAX_JSON_BYTES_V2: u64 = 4 * HEADS_MAX_BYTES;

/// Spec v2: largest sum of the four JSON role files' value costs
/// ([`crate::canonical::VALUE_COST_ID`]): an upper bound on the heap a
/// `serde_json::Value` of each file holds, computed in the layout check's one
/// pass over the bytes, before any parse. Admission refuses a larger sum
/// (`json_value_budget`) before any role file is parsed, so every parse of an
/// artifact's JSON, admitted or refused, is bounded by it. The byte budget
/// alone does not bound memory: a small object costs a map node of hundreds of
/// bytes for a few text bytes.
///
/// Derived from the scorer's memory model `slop_ninja_a_scorer_memory_v2`
/// (`inference/a-container-serve/src/limits.rs`, whose test recomputes it):
/// what a standard-4 instance left the scorer process on October 29, 2026
/// (free memory plus the process's resident set, 12,078,473,216 bytes, with
/// four sessions loaded), less the measured process base (359,321,600), four
/// sessions at the largest served graph (4,826,258,077), the in-flight request
/// bodies (268,435,456) and two copies of the JSON byte budget (536,870,912),
/// divided among the three parses of the JSON that can be live at once.
/// R0's four files cost 118,493,491, 5.8% of it ([`crate::canonical::check_cost`]).
pub const MAX_JSON_VALUE_BYTES_V2: u64 = 2_029_195_723;

/// Spec v2: the token head the span class serves (`full_a_spans.rs`):
/// spans-v1 rule and label policy, on the encoder's final-norm token states.
pub const TOKEN_HEAD_INPUT_V2: &str = "final_norm_token_states";

/// Spec v2: `heads.json` supervision keys of heads v2 (heads v1 keys plus the
/// exporter's `token_head` flag).
pub const SUPERVISION_KEYS_V2: [&str; 6] =
    ["ai", "human", "origin", "share", "token", "token_head"];

/// Spec v3: the export manifest of a span tower artifact (heads v3).
pub const EXPORT_SCHEMA_V4: &str = "slop_ninja_full_a_export_v4";
/// Spec v3: heads v2 plus the span tower declaration; the token head reads the
/// tower's normed token states.
pub const HEADS_SCHEMA_V3: &str = "slop_ninja_full_a_heads_v3";
/// Spec v3: the token head input of heads v3.
pub const TOKEN_HEAD_INPUT_V3: &str = "span_tower_final_norm_token_states";

/// Spec v3: SHA-256 of the span tower runtime
/// (`grammar/crates/grammar-a-baseline/src/full_a_tower.rs`), which reads
/// heads v3 and which this crate and the scorer compile. It equals the
/// scorer's `TOWER_RUNTIME_SHA256` (`inference/a-container-serve/src/lib.rs`);
/// `tests/spec_v3.rs` checks both.
pub const TOWER_RUNTIME_SHA256: &str =
    "bdb83f4ae65b06272572075865fc559f75f13081d019d3a63878ba67057434b1";

/// Spec v3: the span tower the tower class's graph holds, as heads v3 declare
/// it (`span_tower`) and the export manifest repeats it
/// (`span_tower.declaration`): copies of encoder layers 18 to 21 of the 22
/// and of the final norm, each layer keeping its attention kind, read from the
/// hidden state entering layer 18 with the encoder's masks and positions.
pub mod tower {
    use serde_json::{Value, json};

    pub const RULE: &str = "slop_ninja_s2_span_tower_v1: token_head(final_norm_copy(layers_copy[L-K..L-1](h_in(L-K))))";
    pub const ENCODER_LAYERS: u64 = 22;
    pub const SOURCE_LAYER: u64 = 18;
    pub const LAYERS: [u64; 4] = [18, 19, 20, 21];
    pub const ATTENTION: [&str; 4] = ["global", "local", "local", "global"];
    pub const GLOBAL_ATTN_EVERY_N_LAYERS: u64 = 3;
    pub const LOCAL_ATTENTION: u64 = 128;
    pub const CAPTURE: &str = "hidden_state_entering_source_layer";
    pub const WEIGHTS: &str = "tower.safetensors";
    pub const TENSOR_PREFIX: &str = "span_tower.";
    /// Tensors of the tower's weights file: six per copied layer and the norm.
    pub const TENSORS: u64 = 6 * LAYERS.len() as u64 + 1;
    /// Width of the encoder's hidden states, and of each copied norm.
    pub const HIDDEN: u64 = 768;
    /// Width of one half of a copied layer's gated MLP.
    pub const INTERMEDIATE: u64 = 1152;
    /// Parameters of one copied encoder layer, whose six tensors carry no
    /// bias: the attention norm, `Wqkv`, the attention output, the MLP norm,
    /// the MLP input (both gate halves) and the MLP output.
    pub const LAYER_PARAMETERS: u64 = HIDDEN
        + HIDDEN * 3 * HIDDEN
        + HIDDEN * HIDDEN
        + HIDDEN
        + HIDDEN * 2 * INTERMEDIATE
        + INTERMEDIATE * HIDDEN;
    /// Parameters of the tower's weights file: the copied layers and the
    /// final norm copy (S2c's 20,060,928).
    pub const PARAMETERS: u64 = LAYER_PARAMETERS * LAYERS.len() as u64 + HIDDEN;
    /// The arm's `s2` mechanism of a span tower fit, and the frozen document
    /// model it trains with.
    pub const MECHANISM: &str = "span_tower";
    pub const FREEZE: &str = "encoder_and_document_heads";
    /// The trainer's bound on the arm's `s2.layers`.
    pub const MAX_ARM_LAYERS: u64 = 64;

    /// The declaration, exactly as heads v3 carry it.
    pub fn declaration() -> Value {
        json!({
            "rule": RULE, "encoder_layers": ENCODER_LAYERS, "source_layer": SOURCE_LAYER,
            "layers": LAYERS, "attention": ATTENTION,
            "global_attn_every_n_layers": GLOBAL_ATTN_EVERY_N_LAYERS,
            "local_attention": LOCAL_ATTENTION, "capture": CAPTURE,
            "masks_and_positions": "encoder", "final_norm": "copy",
            "weights": WEIGHTS, "tensor_prefix": TENSOR_PREFIX,
        })
    }
}

/// Spec v3: largest sum of the four JSON role files' value costs for an
/// artifact of the tower class. Derived from the scorer's memory model
/// `slop_ninja_a_scorer_memory_v3` (`inference/a-container-serve/src/limits.rs`,
/// whose test recomputes it) as [`MAX_JSON_VALUE_BYTES_V2`] is, at the tower
/// graph's length and its own measured resident bytes per session: what a
/// standard-4 instance left the scorer process, less the process base, four
/// sessions of the tower graph, the in-flight request bodies and two copies of
/// the JSON byte budget, divided among the three parses that can be live at
/// once.
pub const MAX_JSON_VALUE_BYTES_TOWER: u64 = 1_540_445_560;

/// Spec v2: SHA-256 of the scorer's shared load guard
/// (`inference/a-container-serve/src/load_guard.rs`), which this crate
/// compiles for every form the release guard does not take.
pub const SCORER_LOAD_GUARD_SHA256: &str =
    "355475f1582a7dc07d806fb4aaba524dcb8f86cec18992fa43351a701daff194";

/// SHA-256 of the scorer's `guard` function source
/// (`inference/a-container-serve/src/engine.rs`, from `pub fn guard(` to the
/// first line that is a lone `}`). [`crate::guard`] mirrors that source; a
/// test fails when the scorer's guard changes.
pub const SCORER_GUARD_SHA256: &str =
    "270bc28b64e0b1e0c533e753f5274764964775ec7da77030839ba9376bb1492e";

/// The whole spec as one JSON value, for its digest and for publication.
pub fn spec_json() -> Value {
    crate::sorted_keys(json!({
        "schema": SPEC_ID,
        "contract": CONTRACT_ID,
        "object": {
            "format": "slop_ninja_protocol artifact object (artifact-header v1)",
            "files": ROLES.iter().map(|r| json!({
                "name": r.name, "file_role": format!("{:?}", r.file_role), "file_role_tag": r.file_role.tag(),
                "scorer_role": r.scorer_role, "json_layout": r.layout.map(Layout::id),
                "max_bytes": r.max_bytes,
            })).collect::<Vec<_>>(),
            "undeclared_files": "refused",
            "empty_files": "refused",
            "json_buffer": "each JSON role file is buffered at exactly its declared length",
        },
        "scorer_mapping": {"schema": PARTS_SCHEMA, "part_bytes": PART_BYTES,
            "max_parts_per_file": MAX_PARTS_PER_FILE, "max_manifest_bytes": MAX_MANIFEST_BYTES,
            "file_order": "file table order", "profile_id": "<spec id>/<graph class id>"},
        "tokenizer": {"sha256": TOKENIZER_SHA256, "bytes": TOKENIZER_BYTES},
        "graph": {
            "structure_digest": crate::onnx::STRUCTURE_DIGEST_ID,
            "free_values": "FLOAT initializer payloads",
            "classes": GRAPHS.iter().map(|g| json!({
                "id": g.id, "structure_sha256": g.structure_sha256, "bytes": g.bytes,
                "served": g.served, "outputs": g.outputs, "span_head": g.span_head,
            })).collect::<Vec<_>>(),
            "domains": DOMAINS, "ops": OPS, "dtypes": DTYPES, "attr_types": ATTR_TYPES,
            "float_values": "finite",
        },
        "max_parameters": MAX_PARAMETERS,
        "heads_runtime_sha256": HEADS_RUNTIME_SHA256.iter().map(|(n, d)| (n.to_string(), json!(d)))
            .collect::<serde_json::Map<_, _>>(),
        "pinned_contract": {
            "input_mode": contract::INPUT_MODE, "raw_dimension": contract::RAW_DIMENSION,
            "maximum_tokens_including_specials": contract::MAX_TOKENS_INCLUDING_SPECIALS,
            "token_truncation": false, "origin_order": contract::ORIGIN_ORDER,
            "span_coordinates": contract::SPAN_COORDINATES, "span_label_policy": contract::SPAN_LABEL_POLICY,
            "zero_cfg_coordinates": contract::ZERO_CFG_COORDINATES, "ai_roster_rule": contract::AI_ROSTER_RULE,
            "human_profiles": contract::HUMAN_PROFILES, "ai_profiles": contract::AI_PROFILES,
            "ai_profiles_sha256": contract::AI_PROFILES_SHA256,
            "normalization_schema": contract::NORMALIZATION_SCHEMA,
        },
        "pinned_heads": {"schema": heads::SCHEMA, "dimension": heads::DIMENSION,
            "raw_text_dimension": heads::RAW_TEXT_DIMENSION, "hidden": heads::HIDDEN,
            "embedding": heads::EMBEDDING},
        "pinned_calibration": {"schema": calibration::SCHEMA, "fit_split": calibration::FIT_SPLIT,
            "method": calibration::METHOD, "algorithm": calibration::ALGORITHM,
            "interpolation": calibration::INTERPOLATION,
            "policy": {"schema": calibration_policy::SCHEMA, "primary": calibration_policy::PRIMARY,
                "fallback": calibration_policy::FALLBACK, "selection": calibration_policy::SELECTION,
                "bound": calibration_policy::BOUND, "temperature_rule": calibration_policy::TEMPERATURE_RULE}},
        "export_schema": EXPORT_SCHEMA,
        "scorer_guard_sha256": SCORER_GUARD_SHA256,
        "text_profile": crate::text::profile_json(),
    }))
}

/// SHA-256 of the compact serialization of [`spec_json`].
pub fn spec_sha256() -> String {
    crate::sha256_hex(&serde_json::to_vec(&spec_json()).expect("spec serializes"))
}

/// Spec v2 as one JSON value: spec v1's record with the v2 changes.
pub fn spec_json_v2() -> Value {
    let mut v = spec_json();
    v["schema"] = json!(SPEC_ID_V2);
    v["scorer_mapping"]["profile_id"] = json!("<spec id>/<graph class id>");
    v["object"]["json_buffer"] = json!(
        "each JSON role file is buffered at exactly its declared length; the four lengths sum to at most max_json_bytes"
    );
    v["max_json_bytes"] = json!(MAX_JSON_BYTES_V2);
    v["max_json_value_bytes"] = json!(MAX_JSON_VALUE_BYTES_V2);
    {
        use crate::canonical::value_cost as c;
        v["json_value_cost"] = json!({
            "id": crate::canonical::VALUE_COST_ID,
            "checked": "in the layout check's pass over each JSON role file, before any parse; the four costs sum to at most max_json_value_bytes",
            "root": c::ROOT,
            "array": {"block": c::ARRAY, "per_capacity_slot": c::ARRAY_SLOT,
                "capacity": format!("max({}, next power of two of the length)", c::ARRAY_MIN_CAPACITY)},
            "object": {"first_entry": c::OBJECT, "per_further_entry": c::OBJECT_ENTRY},
            "text": {"per_nonempty_key_string_or_number": c::TEXT, "plus": "its length in the file (string text between the quotes, number literal)"},
            "scratch": {"per_longest_text_byte": c::SCRATCH_PER_LONGEST_TEXT, "per": "file"},
        });
    }
    if let Some(classes) = v["graph"]["classes"].as_array_mut() {
        for c in classes {
            c["served"] = json!(true);
            let span = c["span_head"] == true;
            if let Some(o) = c.as_object_mut() {
                o.remove("span_head");
            }
            c["token_states"] = json!(span);
        }
    }
    v["graph"]["token_states_rule"] = json!(
        "a class with token_states needs heads that read the encoder's final-norm token states (a span head, or a heads v2 token head); a class without needs heads that read none"
    );
    v["forms"] = json!([
        {"export": EXPORT_SCHEMA, "heads": heads::SCHEMA, "calibration": calibration::SCHEMA,
         "token_head": false, "load_guard": "release guard (scorer_guard_sha256) without a span head, else the shared load guard"},
        {"export": EXPORT_SCHEMA_V3, "heads": HEADS_SCHEMA_V2, "calibration": CALIBRATION_SCHEMA_V3,
         "token_head": {"input": TOKEN_HEAD_INPUT_V2, "rule": crate::full_a_spans::TOKEN_HEAD_RULE,
            "label_policy": crate::full_a_spans::LABEL_POLICY, "feeds_document_heads": false},
         "supervision_keys": SUPERVISION_KEYS_V2, "load_guard": "shared load guard"},
    ]);
    v["scorer_load_guard_sha256"] = json!(SCORER_LOAD_GUARD_SHA256);
    crate::sorted_keys(v)
}

/// Spec v3 as one JSON value: spec v2's record with the v3 changes. The plain
/// and span classes keep every v2 value; each class records its own parameter
/// and JSON value bounds.
pub fn spec_json_v3() -> Value {
    let mut v = spec_json_v2();
    v["schema"] = json!(SPEC_ID_V3);
    if let Some(o) = v.as_object_mut() {
        o.remove("max_parameters");
        o.remove("max_json_value_bytes");
    }
    v["json_value_cost"]["checked"] = json!(
        "in the layout check's pass over each JSON role file, before any parse; the four costs sum to at most the graph class's max_json_value_bytes"
    );
    v["graph"]["classes"] = json!(
        GRAPHS_V3
            .iter()
            .map(|g| json!({
                "id": g.id, "structure_sha256": g.structure_sha256, "bytes": g.bytes,
                "served": true, "outputs": g.outputs, "token_states": g.span_head,
                "tower_states": g.tower_states, "max_parameters": g.max_parameters,
                "max_json_value_bytes": g.max_json_value_bytes,
            }))
            .collect::<Vec<_>>()
    );
    if let Some(c) = v["graph"]["classes"]
        .as_array_mut()
        .and_then(|c| c.last_mut())
    {
        c["span_tower"] = tower::declaration();
        c["max_parameters_rule"] = json!(format!(
            "graph parameters ({TOWER_GRAPH_PARAMETERS}, fixed by the structure) + the pinned document heads ({DOCUMENT_HEAD_PARAMETERS}) + the widest token head the arm admits ({TOKEN_HEAD_MAX_HIDDEN} units: {})",
            token_head_parameters(TOKEN_HEAD_MAX_HIDDEN)
        ));
    }
    v["graph"]["token_states_rule"] = json!(
        "a class with token_states needs heads that read the encoder's final-norm token states (a span head, or a heads v2 token head); a class with tower_states needs heads v3 whose span_tower is the class's; a class with neither needs heads that read none"
    );
    if let Some(forms) = v["forms"].as_array_mut() {
        forms.push(json!({
            "export": EXPORT_SCHEMA_V4, "heads": HEADS_SCHEMA_V3, "calibration": CALIBRATION_SCHEMA_V3,
            "graph_class": TOWER_GRAPH.id,
            "token_head": {"input": TOKEN_HEAD_INPUT_V3, "rule": crate::full_a_spans::TOKEN_HEAD_RULE,
                "label_policy": crate::full_a_spans::LABEL_POLICY, "feeds_document_heads": false},
            "supervision_keys": SUPERVISION_KEYS_V2,
            "arm_s2": {"mechanism": tower::MECHANISM, "keys": ["layers", "learning_rate", "mechanism"],
                "layers": "integer, the declaration's depth", "learning_rate": "finite, > 0",
                "freeze": tower::FREEZE, "token_head": "required"},
            "fit_binding_s2": {"mechanism": tower::MECHANISM, "rule": tower::RULE, "module_sha256": "digest"},
            "export_span_tower": {"declaration": "the heads' span_tower", "dtype": "F32", "file": tower::WEIGHTS,
                "tensors": tower::TENSORS, "parameters": tower::PARAMETERS},
            "fit_hashes": "encoder and document heads equal to the init fit's; frozen document check exact",
            "reference_tower_rotary_modules": "the declaration's depth",
            "heads_runtime": "full_a_tower.rs (tower_runtime_sha256), which reads heads v3 and hands the pinned runtime heads v2",
            "load_guard": "shared load guard"}));
    }
    v["tower_runtime_sha256"] = json!(TOWER_RUNTIME_SHA256);
    crate::sorted_keys(v)
}

/// Spec v4 as one JSON value: spec v3's record with the graph class rule
/// stated as a byte comparison of the structure stream with each class's
/// pinned reference stream, which the record pins by length and SHA-256. Every
/// other value is v3's.
pub fn spec_json_v4() -> Value {
    let mut v = spec_json_v3();
    v["schema"] = json!(SPEC_ID_V4);
    if let Some(g) = v["graph"].as_object_mut() {
        g.remove("structure_digest");
    }
    v["graph"]["structure_stream"] = json!({
        "id": crate::onnx::STRUCTURE_STREAM_ID,
        "stream": "the graph file's bytes with the payload of every FLOAT initializer data field (raw_data or float_data of a GraphProto.initializer) removed; each field's tag and length prefix stay",
        "class_rule": "the stream equals the class's reference_structure byte for byte; nothing is hashed",
    });
    if let Some(classes) = v["graph"]["classes"].as_array_mut() {
        for (c, g) in classes.iter_mut().zip(GRAPHS_V3.iter()) {
            if let Some(o) = c.as_object_mut() {
                o.remove("structure_sha256");
            }
            c["reference_structure"] = json!({"bytes": g.structure_bytes, "sha256": g.structure_stream_sha256});
        }
    }
    crate::sorted_keys(v)
}
