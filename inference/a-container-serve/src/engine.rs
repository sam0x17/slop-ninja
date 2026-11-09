//! Model engine shared by the batch scorer and the local scorer: admission by
//! the artifact spec, the load guards, the f32 banded ONNX encoder and the f64
//! heads, for one artifact directory.
//!
//! It implements the same graph contract as `a-container-serve` (CLS...SEP, no
//! truncation, at most 2,048 tokens, padded to a multiple of 64 with a masked pad,
//! one inter-op thread, graph optimisation level 3, the `h` output feeds the
//! document heads) and repeats the release runner's export, contract and
//! calibration guards. Nothing here logs text, token ids or document ids.
//!
//! **Admission first.** [`Model::load`] runs the artifact spec's checks
//! (`slop_ninja_artifact_check`, the spec the load names: the cohort's, spec
//! v2 or v3 ([`admits_by`])) over the whole directory before any guard reads a
//! file and before ONNX Runtime opens the graph: the file table, the tokenizer
//! byte-match, the role schemas, the graph pre-parse (custom domains, external
//! data, operator allow-list) and the graph class. Anything the checker
//! refuses never reaches ONNX Runtime.
//!
//! **Span heads.** Which token states the heads read is declared by the export
//! schema and the heads themselves (`load_guard.rs`, `full_a_tower.rs`), never
//! chosen by a runtime version:
//! - R0's form (export v2, heads v1, no span head) reads `h` only and keeps the
//!   release guard ([`guard`], unchanged and pinned by spec v1);
//! - a span head, or a heads v2 token head (S0+), reads the encoder's final-norm
//!   token states, graph output `token_states`;
//! - a heads v3 token head (spans-S2 S2c) reads the span tower's normed states,
//!   graph output `span_tower_token_states`, which only the tower class
//!   (`a-tower-f32band-v1`, spec v3) outputs. ONNX Runtime computes every
//!   output, so a tower-class artifact pays for the tower on every document.
//!
//! The load refuses a graph that lacks an output its heads read. A document's
//! sentences and spans are computed only when a score request asks for them.
use crate::full_a_tower::{SpanTowerHeads, TokenInput, TokenStates};
use crate::load_guard;
use crate::profile::sha256_hex as sha;
use anyhow::{Context, Result, anyhow, ensure};
use ort::session::{Session, builder::GraphOptimizationLevel};
use ort::value::Tensor;
use serde_json::{Value, json};
use slop_ninja_artifact_check::spec::Version;
use slop_ninja_protocol::ArtifactHeader;
use std::fs;
use std::path::Path;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Instant;
use tokenizers::Tokenizer;

pub const BLOCK: usize = 64;
pub const MAX_TOKENS: usize = 2048;
/// Width of `h` and of each token state (the contract's raw dimension).
pub const WIDTH: usize = 768;
/// Graph output with the encoder's final-norm token states, `[batch, seq, 768]`.
pub const TOKEN_STATES: &str = "token_states";
/// Graph output with the span tower's normed token states, `[batch, seq, 768]`.
pub const TOWER_STATES: &str = "span_tower_token_states";
/// The default artifact spec of the operator tools. Spec v4 admits exactly
/// what spec v3 admits (its graph class rule is stated as a byte comparison
/// with the class's reference structure stream), and v3 admits every artifact
/// spec v2 admits, which is every artifact spec v1 admits whose JSON role
/// files fit its JSON budget plus the span class, and the tower class; its
/// classes are every class a load can be admitted under, so the memory model
/// and `/healthz` cover them. The adversarial set and the local scorer's
/// default use it. Spec v5 (v4 plus a required token head with the 24-word
/// floor decoder) is admitted by when a load names it, and is not the
/// default.
pub const NEWEST_SPEC: Version = Version::V4;

/// The spec a load that names none is admitted by: spec v2, the spec testnet
/// cohort templates name. A load for a cohort that names another spec names
/// it (`spec=<digest>` on `/v1/load`). A load that names none never admits
/// what its cohort's spec (v2 or v3) refuses. `slop_ninja_a_served_profile
/// pack` packs by it unless `--spec` names another, so a package's
/// `profile_id` names the spec its unnamed load admits it by.
pub const UNNAMED_LOAD_SPEC: Version = Version::V2;

/// Whether the scorer admits by `v`: a version that bounds the JSON role
/// files' bytes and value cost, which the memory model needs (spec v2 to v4;
/// spec v1 bounds neither).
pub fn admits_by(v: Version) -> bool {
    v.max_json_bytes().is_some() && v.max_json_value_bytes().is_some()
}

/// A tool's `--spec` value: `v2` to `v5` or a full spec id, of a spec the
/// scorer admits by ([`admits_by`]).
pub fn spec_arg(id: &str) -> Result<Version> {
    match id {
        "v2" => Ok(Version::V2),
        "v3" => Ok(Version::V3),
        "v4" => Ok(Version::V4),
        "v5" => Ok(Version::V5),
        id => Version::from_id(id)
            .filter(|v| admits_by(*v))
            .with_context(|| {
                format!("--spec {id}: not a spec the scorer admits by (v2 to v5 or a full spec id)")
            }),
    }
}

/// The decoder load fact: the span decoder's rule and minimum length in words
/// (`span_emission.decoder` of the heads), or null for an artifact without a
/// token head. The span gate's structural conditions read it.
pub fn decoder_fact(heads: &Value) -> Value {
    let d = &heads["span_emission"]["decoder"];
    match (d["rule"].as_str(), d["min_words"].as_u64()) {
        (Some(rule), Some(min_words)) => json!({"rule": rule, "min_words": min_words}),
        _ => Value::Null,
    }
}

static ORT: OnceLock<std::result::Result<(), String>> = OnceLock::new();

/// Initialise ONNX Runtime once per process from `lib`.
pub fn init_ort(lib: &str) -> Result<()> {
    ORT.get_or_init(|| ort::init_from(lib).commit().map(|_| ()).map_err(|e| format!("{e}")))
        .clone()
        .map_err(|e| anyhow!("onnxruntime init: {e}"))
}

/// The release runner's export, contract and calibration guards (`full_a.rs` guard,
/// `guard_calibration`), minus the parent safetensors check: the encoder is the graph.
/// Returns the heads bytes and a content-free summary.
pub fn guard(dir: &Path) -> Result<(Vec<u8>, Value)> {
    let hb = fs::read(dir.join("heads.json"))?;
    let cb = fs::read(dir.join("contract.json"))?;
    let w: Value = serde_json::from_slice(&hb)?;
    let c: Value = serde_json::from_slice(&cb)?;
    let m: Value = serde_json::from_slice(&fs::read(dir.join("manifest.json"))?)?;
    let cal: Value = serde_json::from_slice(&fs::read(dir.join("calibration.json"))?)?;
    let ch = sha(&cb);
    ensure!(
        m["schema"] == "slop_ninja_full_a_export_v2" && m["complete"] == true && m["calibrated"] == true,
        "incomplete or uncalibrated export"
    );
    ensure!(
        c["schema"] == "slop_ninja_full_a_contract_v2"
            && c["input_mode"] == "text"
            && c["raw_dimension"] == 768
            && c["maximum_tokens_including_specials"] == MAX_TOKENS
            && c["token_truncation"] == false,
        "input meaning differs"
    );
    let t = w["base_temperature"].as_f64().context("base temperature")?;
    ensure!(w["contract_sha256"] == ch.as_str() && t.is_finite() && t > 0., "heads/contract differ");
    ensure!(
        cal["schema"] == "slop_ninja_full_a_calibration_v2"
            && cal["complete"] == true
            && cal["fit_split"] == "cal"
            && cal["arm"] == m["arm"]
            && cal["seed"] == m["seed"]
            && cal["selected_checkpoint_sha256"] == m["selected_checkpoint_sha256"]
            && cal["contract_sha256"] == ch.as_str()
            && cal["involvement"]["selected"] == w["involvement_calibration"],
        "calibration/checkpoint/involvement map mismatch"
    );
    for role in ["human", "ai"] {
        let r = &cal["roles"][role];
        let empty = r["roster_empty"] == true
            && w["profiles"][role].as_array().is_some_and(|a| a.is_empty())
            && w["thresholds"][role] == 1.0;
        ensure!(
            empty || (r["qualified"] == true && r["threshold"] == w["thresholds"][role]),
            "unqualified or changed role threshold"
        );
        ensure!(w["profiles"][role] == c[format!("{role}_profiles")], "profile roster differs");
    }
    ensure!(w["span_head"].is_null(), "span-head arms need token states; not served here");
    Ok((hb, json!({"arm": m["arm"], "seed": m["seed"], "base_temperature": t,
        "calibration": cal["involvement"]["selected"]["method"], "contract_sha256": ch})))
}

/// [`guard`] with the header's digest of `contract.json` (lowercase hex),
/// which the object store verified at upload, instead of hashing the file:
/// the scorer's load path. Its body is [`guard`]'s line for line apart from
/// the signature and the digest's source (a test compares the two sources),
/// so [`guard`], pinned by every artifact spec (`SCORER_GUARD_SHA256`), keeps
/// its digest until a spec version moves the pin to this function.
pub fn guard_verified(dir: &Path, contract_sha256: &str) -> Result<(Vec<u8>, Value)> {
    let hb = fs::read(dir.join("heads.json"))?;
    let cb = fs::read(dir.join("contract.json"))?;
    let w: Value = serde_json::from_slice(&hb)?;
    let c: Value = serde_json::from_slice(&cb)?;
    let m: Value = serde_json::from_slice(&fs::read(dir.join("manifest.json"))?)?;
    let cal: Value = serde_json::from_slice(&fs::read(dir.join("calibration.json"))?)?;
    let ch = contract_sha256.to_owned();
    ensure!(
        m["schema"] == "slop_ninja_full_a_export_v2" && m["complete"] == true && m["calibrated"] == true,
        "incomplete or uncalibrated export"
    );
    ensure!(
        c["schema"] == "slop_ninja_full_a_contract_v2"
            && c["input_mode"] == "text"
            && c["raw_dimension"] == 768
            && c["maximum_tokens_including_specials"] == MAX_TOKENS
            && c["token_truncation"] == false,
        "input meaning differs"
    );
    let t = w["base_temperature"].as_f64().context("base temperature")?;
    ensure!(w["contract_sha256"] == ch.as_str() && t.is_finite() && t > 0., "heads/contract differ");
    ensure!(
        cal["schema"] == "slop_ninja_full_a_calibration_v2"
            && cal["complete"] == true
            && cal["fit_split"] == "cal"
            && cal["arm"] == m["arm"]
            && cal["seed"] == m["seed"]
            && cal["selected_checkpoint_sha256"] == m["selected_checkpoint_sha256"]
            && cal["contract_sha256"] == ch.as_str()
            && cal["involvement"]["selected"] == w["involvement_calibration"],
        "calibration/checkpoint/involvement map mismatch"
    );
    for role in ["human", "ai"] {
        let r = &cal["roles"][role];
        let empty = r["roster_empty"] == true
            && w["profiles"][role].as_array().is_some_and(|a| a.is_empty())
            && w["thresholds"][role] == 1.0;
        ensure!(
            empty || (r["qualified"] == true && r["threshold"] == w["thresholds"][role]),
            "unqualified or changed role threshold"
        );
        ensure!(w["profiles"][role] == c[format!("{role}_profiles")], "profile roster differs");
    }
    ensure!(w["span_head"].is_null(), "span-head arms need token states; not served here");
    Ok((hb, json!({"arm": m["arm"], "seed": m["seed"], "base_temperature": t,
        "calibration": cal["involvement"]["selected"]["method"], "contract_sha256": ch})))
}

/// Runs spec `spec`'s admission checks on the files of `header` in `dir`
/// (the header's digests are the store-verified ones; no file is hashed),
/// reading the graph once and the JSON role files within the spec's JSON
/// budget. Returns a content-free summary, which names the spec. A refusal
/// reads `refused <code>: <detail>`; a spec the scorer does not admit by
/// ([`admits_by`]) is an error before any file is read.
pub fn admit(dir: &Path, header: &ArtifactHeader, spec: Version) -> Result<Value> {
    ensure!(admits_by(spec), "the scorer does not admit by {}", spec.id());
    let r = slop_ninja_artifact_check::files::check_files_for(dir, header, spec)
        .map_err(|e| anyhow!("refused {}: {e}", e.code()))?;
    Ok(json!({"spec": r.spec, "graph_class": r.graph_class, "content_sha256": r.content_sha256,
        "parameters": r.parameters}))
}

/// The header of a local model directory, by hashing each file once: for
/// operator tools that load a directory they built (`slop_ninja_a_local_score`,
/// `slop_ninja_a_adversarial`). The scorer's load path never calls it: its
/// header comes from the files manifest, whose digests the object store
/// verified at upload.
pub fn header_of_dir(dir: &Path) -> Result<ArtifactHeader> {
    slop_ninja_artifact_check::fs::header_for_dir(dir).map_err(|e| anyhow!("refused {}: {e}", e.code()))
}

/// What the heads read from the graph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Needs {
    /// The encoder's final-norm token states ([`TOKEN_STATES`]).
    pub token_states: bool,
    /// The span tower's normed token states ([`TOWER_STATES`]).
    pub tower_states: bool,
}

impl Needs {
    /// From the heads' own declarations: a span head or a final-norm token head
    /// reads [`TOKEN_STATES`]; a span-tower token head reads [`TOWER_STATES`].
    pub fn of(heads: &SpanTowerHeads) -> Self {
        Needs {
            token_states: heads.has_span_head() || heads.token_input() == Some(TokenInput::FinalNorm),
            tower_states: heads.token_input() == Some(TokenInput::SpanTower),
        }
    }

    /// The graph outputs these needs name, after `h`.
    pub fn outputs(self) -> Vec<&'static str> {
        let mut v = vec!["h"];
        if self.token_states {
            v.push(TOKEN_STATES);
        }
        if self.tower_states {
            v.push(TOWER_STATES);
        }
        v
    }
}

/// The load guard for any admitted artifact, keyed off the declared schemas:
/// R0's form takes the release [`guard`]; every other form takes
/// `load_guard::check`. `contract_sha256` is the header's digest of
/// `contract.json`. Returns the heads bytes, a content-free summary and, for
/// the other forms, the guard's facts.
pub fn guard_any(dir: &Path, contract_sha256: &str) -> Result<(Vec<u8>, Value, Option<load_guard::Facts>)> {
    let m: Value = serde_json::from_slice(&fs::read(dir.join("manifest.json"))?)?;
    let hb = fs::read(dir.join("heads.json"))?;
    let w: Value = serde_json::from_slice(&hb)?;
    if load_guard::takes_release_guard(&m, &w) {
        drop((m, w, hb));
        let (hb, meta) = guard_verified(dir, contract_sha256)?;
        return Ok((hb, meta, None));
    }
    let c: Value = serde_json::from_slice(&fs::read(dir.join("contract.json"))?)?;
    let cal: Value = serde_json::from_slice(&fs::read(dir.join("calibration.json"))?)?;
    let ch = contract_sha256.to_owned();
    let facts = load_guard::check(&m, &c, &w, &cal, &ch).map_err(|e| anyhow!("{e}"))?;
    let meta = json!({"arm": m["arm"], "seed": m["seed"], "base_temperature": w["base_temperature"],
        "calibration": cal["involvement"]["selected"]["method"], "contract_sha256": ch,
        "export": m["schema"], "heads_schema": w["schema"], "span_head": facts.span_head,
        "token_head": facts.token_head, "span_tower": facts.tower_states, "decoder": decoder_fact(&w)});
    Ok((hb, meta, Some(facts)))
}

/// Span-operator content positions: every position except the CLS/SEP frame.
pub fn content_mask(n: usize) -> Vec<bool> {
    (0..n).map(|i| i != 0 && i + 1 != n).collect()
}

/// `slop_ninja_word_token_mask_v1`: content positions whose character offsets
/// (code points of the exact text) cover at least one character that is not
/// Python whitespace. The release runtime's rule
/// (`inference/a-v6-cpu/src/full_encoder.rs` `word_mask`); the caller decides
/// what an empty mask means.
pub fn word_mask(text: &str, offsets: &[(usize, usize)]) -> Result<Vec<bool>> {
    let chars = text.chars().collect::<Vec<_>>();
    let content = content_mask(offsets.len());
    let mut out = Vec::with_capacity(offsets.len());
    for (keep, &(a, b)) in content.iter().zip(offsets) {
        ensure!(a <= b && b <= chars.len(), "token offset out of range");
        out.push(*keep && chars[a..b].iter().any(|c| !crate::full_a_spans::is_space(*c)));
    }
    Ok(out)
}

pub struct Model {
    pub tokenizer: Tokenizer,
    pub heads: SpanTowerHeads,
    pub needs: Needs,
    pool: Mutex<Vec<Session>>,
    free: Condvar,
    pub load: Value,
}

pub struct Graph {
    pub h: Vec<f32>,
    /// The first `n` rows of [`TOKEN_STATES`] (`n x 768`, row-major), when asked for.
    pub token_states: Option<Vec<f32>>,
    /// The first `n` rows of [`TOWER_STATES`], when asked for.
    pub tower_states: Option<Vec<f32>>,
}

/// Runs the graph on one document and reads `h`.
pub fn forward(s: &mut Session, ids: &[u32]) -> Result<Graph> {
    forward_with(s, ids, Needs::default())
}

fn states(o: &ort::session::SessionOutputs<'_>, name: &str, n: usize, m: usize) -> Result<Vec<f32>> {
    let (shape, d) = o.get(name).with_context(|| format!("graph output {name} absent"))?.try_extract_tensor::<f32>()?;
    ensure!(
        shape[..] == [1, m as i64, WIDTH as i64] && d.len() == m * WIDTH,
        "invalid {name} shape"
    );
    let rows = d[..n * WIDTH].to_vec();
    ensure!(rows.iter().all(|v| v.is_finite()), "invalid {name}");
    Ok(rows)
}

/// Runs the graph on one document and reads `h` plus the token states `want` names.
pub fn forward_with(s: &mut Session, ids: &[u32], want: Needs) -> Result<Graph> {
    let n = ids.len();
    ensure!(n > 0 && n <= MAX_TOKENS, "over-limit input ({n} tokens)");
    let m = n.div_ceil(BLOCK) * BLOCK;
    let mut x = vec![0i64; m];
    let mut mask = vec![0i64; m];
    for (i, &t) in ids.iter().enumerate() {
        x[i] = t as i64;
        mask[i] = 1;
    }
    let xt = Tensor::from_array(([1usize, m], x.into_boxed_slice()))?;
    let mt = Tensor::from_array(([1usize, m], mask.into_boxed_slice()))?;
    let o = s.run(ort::inputs!["input_ids" => xt, "attention_mask" => mt])?;
    let (_, d) = o["h"].try_extract_tensor::<f32>()?;
    let h = d.to_vec();
    ensure!(h.len() == WIDTH && h.iter().all(|v| v.is_finite()), "invalid h");
    let token_states = if want.token_states { Some(states(&o, TOKEN_STATES, n, m)?) } else { None };
    let tower_states = if want.tower_states { Some(states(&o, TOWER_STATES, n, m)?) } else { None };
    Ok(Graph { h, token_states, tower_states })
}

fn widen(v: &[f32]) -> Vec<f64> {
    v.iter().map(|&x| x as f64).collect()
}

impl Model {
    /// Admit the files of `header` in `dir` by `spec` and load them with
    /// `sessions` sessions of `threads` intra-op threads. Admission
    /// ([`admit`]) runs before any guard and before ONNX Runtime opens the
    /// graph. The header's digests are the object store's; nothing on this
    /// path hashes a file.
    pub fn load(
        dir: &Path,
        header: &ArtifactHeader,
        spec: Version,
        graph: &str,
        sessions: usize,
        threads: usize,
        lib: &str,
    ) -> Result<Self> {
        let t = Instant::now();
        let admission = admit(dir, header, spec)?;
        let admit_s = t.elapsed().as_secs_f64();
        let contract = header
            .files
            .iter()
            .find(|f| f.name == "contract.json")
            .map(|f| hex::encode(f.sha256.0))
            .context("contract.json in the header")?;
        let mut model = Self::load_without_admission(dir, &contract, graph, sessions, threads, lib)?;
        model.load["admission"] = admission;
        model.load["admission_s"] = json!(admit_s);
        Ok(model)
    }

    /// [`Model::load`] without the admission checks, given the header's
    /// digest of `contract.json`: for engine tests on synthetic graphs only.
    /// The scorer binaries never call it.
    pub fn load_without_admission(
        dir: &Path,
        contract_sha256: &str,
        graph: &str,
        sessions: usize,
        threads: usize,
        lib: &str,
    ) -> Result<Self> {
        let t = Instant::now();
        let (hb, meta, facts) = guard_any(dir, contract_sha256)?;
        let guard_s = t.elapsed().as_secs_f64();
        let t = Instant::now();
        init_ort(lib)?;
        let mut tokenizer =
            Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| anyhow!("tokenizer: {e}"))?;
        tokenizer.with_truncation(None).map_err(|e| anyhow!("{e}"))?;
        tokenizer.with_padding(None);
        let heads = SpanTowerHeads::from_json(&hb)?;
        drop(hb);
        let needs = Needs::of(&heads);
        // The guard's facts and the parsed heads must agree on what the heads read.
        match facts {
            None => ensure!(needs == Needs::default() && !heads.has_token_head(), "R0's form reads no token states"),
            Some(f) => ensure!(
                f.token_states == needs.token_states
                    && f.tower_states == needs.tower_states
                    && f.span_head == heads.has_span_head()
                    && f.token_head == heads.has_token_head(),
                "heads declarations differ from the export's"
            ),
        }
        let support_s = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let mut pool = Vec::new();
        for _ in 0..sessions {
            pool.push(
                Session::builder()?
                    .with_optimization_level(GraphOptimizationLevel::Level3)?
                    .with_intra_threads(threads)?
                    .with_inter_threads(1)?
                    .commit_from_file(dir.join(graph))?,
            );
        }
        let outputs: Vec<String> = pool.first().map(|s| s.outputs.iter().map(|o| o.name.clone()).collect()).unwrap_or_default();
        for want in needs.outputs() {
            ensure!(outputs.iter().any(|o| o == want), "the graph lacks the {want} output its heads read");
        }
        let sessions_s = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let warm: Vec<u32> = std::iter::once(50281).chain((0..62).map(|i| 1000 + i)).chain([50282]).collect();
        for s in pool.iter_mut() {
            forward_with(s, &warm, needs)?;
        }
        Ok(Self {
            tokenizer,
            heads,
            needs,
            pool: Mutex::new(pool),
            free: Condvar::new(),
            load: json!({"guard_s": guard_s, "tokenizer_heads_ort_init_s": support_s,
                "sessions_s": sessions_s, "warmup_s": t.elapsed().as_secs_f64(), "release": meta,
                "graph_outputs_read": needs.outputs()}),
        })
    }

    /// Whether this artifact can emit sentences and spans (a token head).
    pub fn emits_spans(&self) -> bool {
        self.heads.has_token_head()
    }

    /// Score one document's document outputs. The result carries digests, counts
    /// and the prediction only.
    pub fn score(&self, id: &str, text: &str) -> Value {
        self.score_with(id, text, false)
    }

    /// Score one document; with `spans` and a token head, also its sentences and
    /// spans. The document prediction and `output_sha256` do not depend on
    /// `spans`: sentences and spans are added to the prediction after the
    /// document digest, with their own digest `span_output_sha256` and a
    /// `span_status`.
    pub fn score_with(&self, id: &str, text: &str, spans: bool) -> Value {
        self.score_with_sentences(id, text, spans, true)
    }

    /// [`Model::score_with`]; without `sentences`, the reply leaves out the
    /// sentences and keeps the spans. `span_output_sha256` is the same either
    /// way: it is computed over both before the sentences are left out.
    pub fn score_with_sentences(&self, id: &str, text: &str, spans: bool, sentences: bool) -> Value {
        let t0 = Instant::now();
        let text_sha = sha(text.as_bytes());
        if text.trim().is_empty() {
            return json!({"id": id, "status": "unsupported_input", "reason": "empty_text",
                "text_sha256": text_sha, "prediction": null});
        }
        let span_head = self.heads.has_span_head();
        let emit = spans && self.heads.has_token_head();
        // Offsets and the word mask: for a span head (document outputs) and for emission.
        let with_words = span_head || emit;
        let encoded = if with_words {
            self.tokenizer.encode_char_offsets(text, true)
        } else {
            self.tokenizer.encode(text, true)
        };
        let (ids, offsets) = match encoded {
            Ok(enc) => (enc.get_ids().to_vec(), enc.get_offsets().to_vec()),
            Err(_) => {
                return json!({"id": id, "status": "error", "reason": "tokenize", "text_sha256": text_sha,
                    "prediction": null});
            }
        };
        if ids.len() > MAX_TOKENS {
            return json!({"id": id, "status": "unsupported_input", "reason": "unsupported_length",
                "text_sha256": text_sha, "token_count": ids.len(), "prediction": null});
        }
        let words = if with_words {
            match word_mask(text, &offsets) {
                Ok(w) => w,
                Err(_) => {
                    return json!({"id": id, "status": "error", "reason": "tokenize", "text_sha256": text_sha,
                        "token_count": ids.len(), "prediction": null});
                }
            }
        } else {
            Vec::new()
        };
        // A span head's document outputs need a word-bearing token (the release
        // runtime refuses such a request; the text, not the artifact, causes it).
        if span_head && !words.iter().any(|w| *w) {
            return json!({"id": id, "status": "unsupported_input", "reason": "no_word_tokens",
                "text_sha256": text_sha, "token_count": ids.len(), "prediction": null});
        }
        let want = Needs {
            token_states: span_head || (emit && self.needs.token_states),
            tower_states: emit && self.needs.tower_states,
        };
        let mut session = {
            let mut pool = self.pool.lock().unwrap();
            loop {
                if let Some(s) = pool.pop() {
                    break s;
                }
                pool = self.free.wait(pool).unwrap();
            }
        };
        let g = forward_with(&mut session, &ids, want);
        self.pool.lock().unwrap().push(session);
        self.free.notify_one();
        let out = g.and_then(|g| {
            let h: Vec<f64> = widen(&g.h);
            let tokens = g.token_states.as_deref().map(widen).unwrap_or_default();
            let tower = g.tower_states.as_deref().map(widen);
            let p = if span_head {
                self.heads.predict(&h, &tokens, &content_mask(ids.len()), &words)?
            } else {
                self.heads.predict(&h, &[], &[], &[])?
            };
            Ok((p, tokens, tower))
        });
        match out {
            Ok((out, tokens, tower)) => {
                let mut prediction = serde_json::to_value(&out.prediction).unwrap_or(Value::Null);
                let output_sha = sha(&serde_json::to_vec(&prediction).unwrap_or_default());
                let mut r = json!({"id": id, "status": "ok", "text_sha256": text_sha, "token_count": ids.len(),
                    "prediction": prediction.clone(), "output_sha256": output_sha});
                if emit {
                    let states = TokenStates { final_norm: &tokens, span_tower: tower.as_deref() };
                    let emitted = self.heads.token_logits(&states, &words).and_then(|logits| {
                        let logits = logits.context("token head")?;
                        self.heads.emit_spans(text, &offsets, &words, &logits, &out.prediction)
                    });
                    match emitted {
                        Ok(x) => {
                            prediction["sentences"] = serde_json::to_value(&x.sentences).unwrap_or(Value::Null);
                            prediction["spans"] = serde_json::to_value(&x.spans).unwrap_or(Value::Null);
                            r["span_status"] = json!("ok");
                            let both = json!({"sentences": prediction["sentences"], "spans": prediction["spans"]});
                            r["span_output_sha256"] = json!(sha(&serde_json::to_vec(&both).unwrap_or_default()));
                            if !sentences && let Some(p) = prediction.as_object_mut() {
                                p.remove("sentences");
                            }
                        }
                        Err(e) => {
                            prediction["sentences"] = Value::Null;
                            prediction["spans"] = Value::Null;
                            r["span_status"] = json!({"unsupported": e.to_string()});
                        }
                    }
                    r["prediction"] = prediction;
                }
                r["ms"] = json!(t0.elapsed().as_secs_f64() * 1e3);
                r
            }
            Err(_) => json!({"id": id, "status": "error", "reason": "model", "text_sha256": text_sha,
                "token_count": ids.len(), "prediction": null}),
        }
    }
}

#[cfg(test)]
mod decoder_fact_tests {
    use super::*;

    #[test]
    fn the_decoder_fact_is_the_heads_decoder_rule_and_minimum() {
        let h = json!({"span_emission": {"decoder": {"rule": "slop_ninja_span_decoder_v1", "tau_hi": 0.75, "delta": 0.2,
            "min_words": 24, "gap": 2, "snap": 0.8}}});
        assert_eq!(decoder_fact(&h), json!({"rule": "slop_ninja_span_decoder_v1", "min_words": 24}));
        assert_eq!(decoder_fact(&json!({"span_emission": null})), Value::Null);
        assert_eq!(decoder_fact(&json!({})), Value::Null);
        assert_eq!(spec_arg("v5").unwrap(), Version::V5);
        assert!(admits_by(Version::V5));
    }
}
