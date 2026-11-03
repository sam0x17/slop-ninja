//! The scorer's load guard for artifacts that read token states: heads with a
//! span head or a spans-v1 token head, in export v2, v3 or v4. R0's form (export
//! v2, heads v1, no span head) keeps the release guard, `engine.rs` `guard`,
//! unchanged.
//!
//! The guard is pure: `serde_json::Value`s of the four JSON role files in, a
//! refusal message out. It reads no file and holds no state, so the scorer
//! (`engine.rs`) and the artifact checker (`subnet/crates/artifact-check`,
//! spec v2) compile this one file through `#[path]`, and spec v2 pins its
//! SHA-256. Each file is read through the top-level keys listed for it below;
//! a caller may pass a view that keeps only those keys.
//!
//! The export, heads and calibration schemas pair as the release runtime pairs
//! them (`inference/a-v6-cpu/src/bin/full_a.rs`), keyed off the export's
//! declared schema only, never a runtime version:
//!
//! | Export | Heads | Calibration | Token head reads |
//! |---|---|---|---|
//! | `slop_ninja_full_a_export_v2` | `slop_ninja_full_a_heads_v1` | v2 | no token head |
//! | `slop_ninja_full_a_export_v3` | `slop_ninja_full_a_heads_v2` | v3 | the encoder's final-norm token states |
//! | `slop_ninja_full_a_export_v4` | `slop_ninja_full_a_heads_v3` | v3 | the span tower's normed token states |
//!
//! A span head (the token-span involvement head, any heads schema) reads the
//! encoder's final-norm token states. [`Facts`] says which graph outputs the
//! heads read; the engine refuses a graph that lacks one.
use serde_json::Value;

pub const EXPORT_V2: &str = "slop_ninja_full_a_export_v2";
pub const EXPORT_V3: &str = "slop_ninja_full_a_export_v3";
pub const EXPORT_V4: &str = "slop_ninja_full_a_export_v4";
pub const HEADS_V1: &str = "slop_ninja_full_a_heads_v1";
pub const HEADS_V2: &str = "slop_ninja_full_a_heads_v2";
pub const HEADS_V3: &str = "slop_ninja_full_a_heads_v3";
pub const CALIBRATION_V2: &str = "slop_ninja_full_a_calibration_v2";
pub const CALIBRATION_V3: &str = "slop_ninja_full_a_calibration_v3";
pub const CONTRACT: &str = "slop_ninja_full_a_contract_v2";
/// The contract's token limit (`engine.rs` `MAX_TOKENS`), with its type.
const MAX_TOKENS: usize = 2048;

/// Top-level keys the guard reads from `manifest.json`.
pub const MANIFEST_KEYS: &[&str] = &[
    "schema",
    "complete",
    "calibrated",
    "arm",
    "seed",
    "selected_checkpoint_sha256",
    "token_head",
    "span_emission",
    "span_tower",
];
/// Top-level keys the guard reads from `contract.json`.
pub const CONTRACT_KEYS: &[&str] = &[
    "schema",
    "input_mode",
    "raw_dimension",
    "maximum_tokens_including_specials",
    "token_truncation",
    "human_profiles",
    "ai_profiles",
];
/// Top-level keys the guard reads from `heads.json`.
pub const HEADS_KEYS: &[&str] = &[
    "schema",
    "base_temperature",
    "contract_sha256",
    "involvement_calibration",
    "profiles",
    "thresholds",
    "span_head",
    "token_head",
    "span_emission",
    "span_tower",
];
/// Top-level keys the guard reads from `calibration.json`.
pub const CALIBRATION_KEYS: &[&str] = &[
    "schema",
    "complete",
    "fit_split",
    "arm",
    "seed",
    "selected_checkpoint_sha256",
    "contract_sha256",
    "involvement",
    "roles",
    "spans",
];

/// Export schemas, from the manifest's declared `schema`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Export {
    V2,
    V3,
    V4,
}

impl Export {
    pub fn of(schema: &Value) -> Option<Self> {
        match schema.as_str()? {
            EXPORT_V2 => Some(Export::V2),
            EXPORT_V3 => Some(Export::V3),
            EXPORT_V4 => Some(Export::V4),
            _ => None,
        }
    }

    pub fn heads_schema(self) -> &'static str {
        match self {
            Export::V2 => HEADS_V1,
            Export::V3 => HEADS_V2,
            Export::V4 => HEADS_V3,
        }
    }

    pub fn calibration_schema(self) -> &'static str {
        match self {
            Export::V2 => CALIBRATION_V2,
            Export::V3 | Export::V4 => CALIBRATION_V3,
        }
    }

    /// Only export v4 declares a span tower.
    pub fn span_tower(self) -> bool {
        self == Export::V4
    }
}

/// What the heads read from the graph, from their declarations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts {
    pub export: Export,
    /// The encoder's final-norm token states (graph output `token_states`):
    /// read by a span head, and by a token head of heads v2.
    pub token_states: bool,
    /// The span tower's normed token states (graph output
    /// `span_tower_token_states`): read by the token head of heads v3.
    pub tower_states: bool,
    pub token_head: bool,
    pub span_head: bool,
}

fn ensure(ok: bool, what: &'static str) -> Result<(), &'static str> {
    if ok { Ok(()) } else { Err(what) }
}

/// Whether `release_guard` applies: R0's form, export v2 without a span head.
/// Every other artifact takes [`check`].
pub fn takes_release_guard(m: &Value, w: &Value) -> bool {
    Export::of(&m["schema"]) == Some(Export::V2) && w["span_head"].is_null()
}

/// The load guard for every export the release guard does not take. `ch` is
/// the SHA-256 of the exact `contract.json` bytes. The checks are the release
/// guard's, with the same predicates, plus the schema pairing, the token head
/// and span emission declarations and the span tower declaration.
pub fn check(m: &Value, c: &Value, w: &Value, cal: &Value, ch: &str) -> Result<Facts, &'static str> {
    let export = Export::of(&m["schema"]).ok_or("incomplete or uncalibrated export")?;
    ensure(m["complete"] == true && m["calibrated"] == true, "incomplete or uncalibrated export")?;
    ensure(
        c["schema"] == CONTRACT
            && c["input_mode"] == "text"
            && c["raw_dimension"] == 768
            && c["maximum_tokens_including_specials"] == MAX_TOKENS
            && c["token_truncation"] == false,
        "input meaning differs",
    )?;
    ensure(w["schema"] == export.heads_schema(), "heads schema does not match the export schema")?;
    let t = w["base_temperature"].as_f64().ok_or("base temperature")?;
    ensure(w["contract_sha256"] == ch && t.is_finite() && t > 0., "heads/contract differ")?;
    // Calibration v2 records carry no span emission spec; v3 records carry the exported one.
    let spans = match export {
        Export::V2 => cal.get("spans").is_none(),
        Export::V3 | Export::V4 => cal.get("spans").is_some() && cal["spans"] == w["span_emission"],
    };
    ensure(
        cal["schema"] == export.calibration_schema()
            && cal["complete"] == true
            && cal["fit_split"] == "cal"
            && cal["arm"] == m["arm"]
            && cal["seed"] == m["seed"]
            && cal["selected_checkpoint_sha256"] == m["selected_checkpoint_sha256"]
            && cal["contract_sha256"] == ch
            && cal["involvement"]["selected"] == w["involvement_calibration"]
            && spans,
        "calibration/checkpoint/involvement map mismatch",
    )?;
    for role in ["human", "ai"] {
        let r = &cal["roles"][role];
        let empty = r["roster_empty"] == true
            && w["profiles"][role].as_array().is_some_and(|a| a.is_empty())
            && w["thresholds"][role] == 1.0;
        ensure(
            empty || (r["qualified"] == true && r["threshold"] == w["thresholds"][role]),
            "unqualified or changed role threshold",
        )?;
        ensure(w["profiles"][role] == c[format!("{role}_profiles")], "profile roster differs")?;
    }
    // Heads v1 and export v2 predate the token head: the keys are absent, not null.
    // Later exports declare the token head and its emission spec exactly as the heads do.
    match export {
        Export::V2 => ensure(
            [m, w].iter().all(|v| v.get("token_head").is_none() && v.get("span_emission").is_none()),
            "a v2 export has no token head",
        )?,
        Export::V3 | Export::V4 => ensure(
            m.get("token_head").is_some()
                && m.get("span_emission").is_some()
                && m["token_head"] == w["token_head"]
                && m["span_emission"] == w["span_emission"]
                && w["token_head"].is_null() == w["span_emission"].is_null(),
            "token head declaration",
        )?,
    }
    // Only v4 declares the span tower, and the declaration is the heads' own. The
    // tower's weights belong to the graph, which outputs the tower's states.
    if export.span_tower() {
        ensure(
            w.get("span_tower").is_some_and(|t| !t.is_null())
                && m["span_tower"]["declaration"] == w["span_tower"]
                && !w["token_head"].is_null(),
            "span tower declaration",
        )?;
    } else {
        ensure(m.get("span_tower").is_none() && w.get("span_tower").is_none(), "only a v4 export has a span tower")?;
    }
    let span_head = !w["span_head"].is_null();
    let token_head = !w["token_head"].is_null();
    let facts = Facts {
        export,
        token_states: span_head || (token_head && !export.span_tower()),
        tower_states: export.span_tower(),
        token_head,
        span_head,
    };
    // This guard serves only heads that read token states.
    ensure(facts.token_states || facts.tower_states, "the release guard takes heads that read no token states")?;
    Ok(facts)
}
