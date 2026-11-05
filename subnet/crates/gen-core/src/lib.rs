//! Generation v2 (`slop_ninja_generation_plan_v2`) for evaluation cohorts.
//!
//! Half of a cohort's generated documents come from frontier models (OpenAI
//! and Anthropic), half from a spread of Workers AI models. Every choice for a
//! document (lane, model, effort, span class, edit intensity, operation,
//! voice profile, template) comes from a durable per-source rotation and a
//! seeded voice draw, and is recorded privately with every attempt.
//!
//! - [`vendor`]: lanes, vendors, effort levels and the documented effort set
//!   of each model.
//! - [`manifest`]: the versioned generator manifest (a private object bound by
//!   digest in the plan): models, prices, templates, registers, bounds and the
//!   measured figures of each (model, effort) cell.
//! - [`voice`]: the versioned voice library and the per-document voice draw.
//! - [`rotation`]: the per-(kind, domain) decks and the assignment of a cell.
//! - [`request`]: prompts from templates and the vendor request bodies.
//! - [`sse`]: the bounded stream reader of both vendors' event grammars.
//! - [`outcome`]: HTTP and stream outcome classes (content-free).
//! - [`pause`]: vendor pause, fallback to Workers AI and resume.
//! - [`spend`]: cost per call from vendor usage, and per-cohort totals.
//! - [`record`]: the private per-document generation record.
//! - [`measure`]: the compiled synthetic measurement set.
//! - [`plan`]: the plan field and its admission checks.
//! - [`iface`]: the internal interface of the generation Worker and its bounds.
//!
//! Nothing here performs I/O, reads a key, or keeps vendor message text.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)
)]

pub mod hash;
pub mod iface;
pub mod manifest;
pub mod measure;
pub mod outcome;
pub mod pause;
pub mod plan;
pub mod record;
pub mod request;
pub mod rotation;
pub mod spend;
pub mod sse;
pub mod vendor;
pub mod voice;

/// The plan field's schema.
pub const GENERATION_PLAN_SCHEMA: &str = "slop_ninja_generation_plan_v2";
/// The generator manifest's schema.
pub const MANIFEST_SCHEMA: &str = "slop_ninja_generator_manifest_v1";
/// The rotation this build implements (bound into the plan and the records).
pub const ROTATION_VERSION: &str = "generation_rotation_v1";
/// The voice library's schema.
pub const VOICE_LIBRARY_SCHEMA: &str = "eval_voice_library_v1";
/// The stream bound rule (section 9.1 of the design).
pub const STREAM_BOUND: &str = "stream_bound_v1";
// The voice splits this build implements are [`voice::VoiceSplit`]; the
// plan names one (`generation.voice_split`).
/// The generation record's schema.
pub const RECORD_SCHEMA: &str = "slop_ninja_generation_record_v1";
