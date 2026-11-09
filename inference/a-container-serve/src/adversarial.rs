//! The adversarial artifact set: artifacts derived from an admitted base artifact
//! (R0 or any other), each built to break the scorer in one way, with the
//! outcome the scorer must give. `slop_ninja_a_adversarial` writes and runs them;
//! `docs/testnet/scorer-threat-model.md` explains each case.
//!
//! Two kinds of case:
//! - **Model cases** write a model directory: the base's files, hard-linked when
//!   possible, with one file changed. Graph changes keep the graph's length, so
//!   the length check alone does not refuse them.
//! - **Manifest cases** write only a files manifest (`artifact-files.json`) whose
//!   declared lengths are the attack; the scorer refuses it before any file byte
//!   arrives, so no file is written.
//!
//! The JSON cases change only `manifest.json`, which no other file binds, in its
//! canonical layout and far inside the byte budget: their attack is the memory a
//! parse holds, sized from the base's own value cost and the spec's value budget.
//!
//! An artifact that loads and then fails every document cannot be built from
//! an admitted graph by changing weights alone: the LayerNormalizations
//! normalize away the overflow the warm-up document would avoid. The engine
//! tests build one from a synthetic graph instead (`tests/engine.rs`).
//!
//! The outcomes: `Refused(code)` is an admission refusal before ONNX Runtime
//! opens the graph (the load fails and the failure is the artifact's);
//! `LoadFails(detail)` is a load that fails after ONNX Runtime opened the graph
//! (the warm-up document), also the artifact's; `EveryDocument(status)` means
//! the artifact loads and every document gets that per-document status
//! (attributed to the artifact); `Bounded` means the artifact loads and scores,
//! and the case is there to measure time against the base.
use crate::artifact::{FileEntry, FilesManifest, SCHEMA};
use anyhow::{Context, Result, bail, ensure};
use std::fs;
use std::ops::Range;
use std::path::Path;

/// What the scorer must do with a case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    /// Refused with this admission code, before ONNX Runtime opens anything.
    Refused(&'static str),
    /// Admitted, but the load fails with an error containing this text.
    LoadFails(&'static str),
    /// Loads; every document gets this `status:reason`.
    EveryDocument(&'static str),
    /// Loads and scores; time is measured against the base.
    Bounded,
}

#[derive(Clone, Copy, Debug)]
pub struct Case {
    pub name: &'static str,
    pub expect: Expect,
    pub manifest_only: bool,
    pub what: &'static str,
}

impl Case {
    /// What the scorer must do with this case written from `base`. Only
    /// `json-objects-in-budget` depends on the base's form: its array replaces
    /// the export manifest's `token_head`, which an export v2 does not have
    /// (refused `schema`), while an export v3 or v4 takes any JSON value there
    /// and its load guard refuses the array as unequal to the heads' token head
    /// (`binding`). Either way the array is parsed within the value budget first.
    pub fn expect_for(&self, base: &Path) -> Result<Expect> {
        if self.name != "json-objects-in-budget" {
            return Ok(self.expect);
        }
        let m: serde_json::Value = serde_json::from_slice(&fs::read(base.join("manifest.json"))?)?;
        Ok(match m.get("token_head") {
            Some(t) if !t.is_null() => Expect::Refused("binding"),
            _ => self.expect,
        })
    }
}

pub const CASES: &[Case] = &[
    Case {
        name: "nonfinite-outputs",
        expect: Expect::LoadFails("invalid h"),
        manifest_only: false,
        what: "the weight of the head's final LayerNormalization, which produces h, set to 3.0e38: finite in the file, so admitted, but h overflows, already on the load's warm-up document",
    },
    Case {
        name: "saturated-weights",
        expect: Expect::Bounded,
        manifest_only: false,
        what: "every FLOAT initializer value set to 3.0e38: admitted; the LayerNormalizations keep h finite, so it loads and scores garbage",
    },
    Case {
        name: "subnormal-weights",
        expect: Expect::Refused("graph_subnormal_initializer"),
        manifest_only: false,
        what: "every FLOAT initializer value set to 1e-39, a float32 subnormal: refused by the initializer value rule (slop_ninja_onnx_no_subnormal_initializers_v1), which removes the constant lever on arithmetic speed on CPUs without flush-to-zero",
    },
    Case {
        name: "custom-domain",
        expect: Expect::Refused("graph_custom_domain"),
        manifest_only: false,
        what: "the first Add node moved to a custom operator domain, at the graph's own length",
    },
    Case {
        name: "operator-not-allowed",
        expect: Expect::Refused("graph_op_not_allowed"),
        manifest_only: false,
        what: "the first Add node changed to Abs, at the graph's own length",
    },
    Case {
        name: "external-data",
        expect: Expect::Refused("graph_external_data"),
        manifest_only: false,
        what: "the first initializer's data_type field rewritten as data_location = EXTERNAL, at the graph's own length",
    },
    Case {
        name: "json-value-budget",
        expect: Expect::Refused("json_value_budget"),
        manifest_only: false,
        what: "manifest.json with an array of small objects ({\"a\":0}, 8 bytes each, a map node each when parsed) whose value cost passes the spec's value budget, at about 20 MB",
    },
    Case {
        name: "json-objects-in-budget",
        expect: Expect::Refused("schema"),
        manifest_only: false,
        what: "the same array, under the value budget, as the export manifest's token_head, which the schema parses as a JSON value: the parse is bounded by the value budget, then the schema refuses it (a base with a token head: its load guard, `binding`)",
    },
    Case {
        name: "padded-provenance",
        expect: Expect::Bounded,
        manifest_only: false,
        what: "manifest.json's free-text limits list padded to the longest list of one-letter strings whose price, its strings and its capacity's slots, stays inside the value budget (2^23 strings, 34 MB, for R0): admitted; loads and scores within the memory model",
    },
    Case {
        name: "never-loads",
        expect: Expect::Refused("binding"),
        manifest_only: false,
        what: "heads.json bound to another contract digest (one hex digit changed): the load guard refuses it",
    },
    Case {
        name: "oversize-protobuf",
        expect: Expect::Refused("graph_size"),
        manifest_only: true,
        what: "a graph declared at 2 GiB + 1 bytes, past protobuf's 2 GiB message limit",
    },
    Case {
        name: "oversize-heads",
        expect: Expect::Refused("file_size"),
        manifest_only: true,
        what: "heads.json declared one byte past the heads runtime's 64 MiB limit",
    },
    Case {
        name: "oversize-json",
        expect: Expect::Refused("json_budget"),
        manifest_only: true,
        what: "calibration.json declared at 300 MiB: each file within its own bound, the four JSON files past the spec's JSON budget",
    },
];

pub fn case(name: &str) -> Option<&'static Case> {
    CASES.iter().find(|c| c.name == name)
}

// ------------------------------------------------------------------ protobuf

fn varint(b: &[u8], at: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*at).context("truncated varint")?;
        *at += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
    }
    bail!("varint too long")
}

/// One field of a message: number, wire type, and the value's byte range
/// (for a varint, the varint's bytes), plus the tag's offset.
struct Field {
    number: u64,
    wire: u8,
    tag_at: usize,
    value: Range<usize>,
}

fn fields(b: &[u8], range: Range<usize>) -> Result<Vec<Field>> {
    let mut out = Vec::new();
    let mut at = range.start;
    while at < range.end {
        let tag_at = at;
        let key = varint(b, &mut at)?;
        let (number, wire) = (key >> 3, (key & 7) as u8);
        let start = at;
        match wire {
            0 => {
                varint(b, &mut at)?;
            }
            1 => at += 8,
            5 => at += 4,
            2 => {
                let len = varint(b, &mut at)? as usize;
                let start = at;
                at = start.checked_add(len).context("length overflow")?;
                ensure!(at <= range.end, "field past its message");
                out.push(Field { number, wire, tag_at, value: start..at });
                continue;
            }
            _ => bail!("unsupported wire type {wire}"),
        }
        ensure!(at <= range.end, "field past its message");
        out.push(Field { number, wire, tag_at, value: start..at });
    }
    Ok(out)
}

/// The graph's initializer messages (`ModelProto.graph.initializer`).
fn initializers(b: &[u8]) -> Result<Vec<Range<usize>>> {
    let graph = fields(b, 0..b.len())?
        .into_iter()
        .find(|f| f.number == 7 && f.wire == 2)
        .context("no graph")?;
    Ok(fields(b, graph.value)?.into_iter().filter(|f| f.number == 5 && f.wire == 2).map(|f| f.value).collect())
}

/// The `raw_data` payload of every FLOAT initializer.
pub fn float_initializer_payloads(b: &[u8]) -> Result<Vec<Range<usize>>> {
    let mut out = Vec::new();
    for t in initializers(b)? {
        let fs = fields(b, t)?;
        let float = fs.iter().any(|f| f.number == 2 && f.wire == 0 && b[f.value.clone()] == [1]);
        if float && let Some(raw) = fs.iter().find(|f| f.number == 9 && f.wire == 2) {
            out.push(raw.value.clone());
        }
    }
    Ok(out)
}

/// The initializer the class names `model.encoder.head.norm.weight`: the weight
/// of the LayerNormalization whose output is `h`. Initializer names are part
/// of the structure stream, so every admitted plain or span graph has it.
pub const HEAD_NORM_WEIGHT: &str = "model.encoder.head.norm.weight";

/// Every value of the FLOAT initializer named `name` set to `v`, in place.
pub fn fill_initializer(b: &mut [u8], name: &str, v: f32) -> Result<usize> {
    for t in initializers(b)? {
        let fs = fields(b, t)?;
        let named = fs.iter().any(|f| f.number == 8 && f.wire == 2 && &b[f.value.clone()] == name.as_bytes());
        let float = fs.iter().any(|f| f.number == 2 && f.wire == 0 && b[f.value.clone()] == [1]);
        if named && float && let Some(raw) = fs.iter().find(|f| f.number == 9 && f.wire == 2) {
            let le = v.to_le_bytes();
            let r = raw.value.clone();
            ensure!(r.len() % 4 == 0, "float payload length");
            for chunk in b[r.clone()].chunks_exact_mut(4) {
                chunk.copy_from_slice(&le);
            }
            return Ok(r.len() / 4);
        }
    }
    bail!("no FLOAT initializer {name}")
}

/// Every FLOAT initializer value set to `v`, in place.
pub fn fill_float_initializers(b: &mut [u8], v: f32) -> Result<usize> {
    let ranges = float_initializer_payloads(b)?;
    let le = v.to_le_bytes();
    let mut n = 0;
    for r in ranges {
        ensure!(r.len() % 4 == 0, "float payload length");
        for chunk in b[r].chunks_exact_mut(4) {
            chunk.copy_from_slice(&le);
            n += 1;
        }
    }
    Ok(n)
}

/// The first initializer's `data_type` field rewritten as `data_location` (field
/// 14) with value 1 (EXTERNAL): same length, external data declared.
pub fn declare_external_data(b: &mut [u8]) -> Result<()> {
    let first = initializers(b)?.into_iter().next().context("no initializer")?;
    let dt = fields(b, first)?.into_iter().find(|f| f.number == 2 && f.wire == 0).context("no data_type")?;
    ensure!(dt.value.start == dt.tag_at + 1 && dt.value.len() == 1, "data_type layout");
    b[dt.tag_at] = (14 << 3) as u8; // field 14, varint
    b[dt.value.start] = 1;
    Ok(())
}

/// The first `Add` node's `op_type` field patched: `Abs` (same field) or moved to
/// the `domain` field (a custom domain "Add").
pub fn patch_first_add(b: &mut [u8], to_domain: bool) -> Result<()> {
    let at = b.windows(5).position(|w| w == b"\x22\x03Add").context("no Add node")?;
    if to_domain {
        b[at] = 0x3a; // field 7 (domain), length-delimited
    } else {
        b[at + 2..at + 5].copy_from_slice(b"Abs");
    }
    Ok(())
}

// --------------------------------------------------------------------- write

const GRAPH: &str = "a-fp32band.onnx";
const FILES: [(&str, &str); 6] = [
    (GRAPH, "encoder_graph"),
    ("heads.json", "heads"),
    ("contract.json", "contract"),
    ("calibration.json", "calibration"),
    ("manifest.json", "export_manifest"),
    ("tokenizer.json", "tokenizer"),
];

fn link_or_copy(from: &Path, to: &Path) -> Result<()> {
    if fs::hard_link(from, to).is_err() {
        fs::copy(from, to).with_context(|| format!("copy {}", from.display()))?;
    }
    Ok(())
}

/// Writes `case` derived from the model directory `base` into `out` (created new).
pub fn write_case(base: &Path, out: &Path, case: &Case) -> Result<()> {
    fs::create_dir(out).with_context(|| format!("{} must be new", out.display()))?;
    if case.manifest_only {
        let m = oversize_manifest(base, case.name)?;
        let mut bytes = serde_json::to_vec_pretty(&m)?;
        bytes.push(b'\n');
        fs::write(out.join("artifact-files.json"), bytes)?;
        return Ok(());
    }
    let changed = match case.name {
        "never-loads" => "heads.json",
        "json-value-budget" | "json-objects-in-budget" | "padded-provenance" => "manifest.json",
        _ => GRAPH,
    };
    for (name, _) in FILES {
        if name != changed {
            link_or_copy(&base.join(name), &out.join(name))?;
        }
    }
    let mut b = fs::read(base.join(changed))?;
    match case.name {
        "nonfinite-outputs" => {
            fill_initializer(&mut b, HEAD_NORM_WEIGHT, 3.0e38)?;
        }
        "saturated-weights" => {
            fill_float_initializers(&mut b, 3.0e38)?;
        }
        "subnormal-weights" => {
            fill_float_initializers(&mut b, 1e-39)?;
        }
        "custom-domain" => patch_first_add(&mut b, true)?,
        "operator-not-allowed" => patch_first_add(&mut b, false)?,
        "external-data" => declare_external_data(&mut b)?,
        "never-loads" => {
            let key = b"\"contract_sha256\":\"";
            let at = b.windows(key.len()).position(|w| w == key).context("heads contract digest")? + key.len();
            b[at] = if b[at] == b'0' { b'1' } else { b'0' };
        }
        "json-value-budget" | "json-objects-in-budget" | "padded-provenance" => {
            b = json_case(base, case.name)?;
        }
        other => bail!("no writer for {other}"),
    }
    fs::write(out.join(changed), b)?;
    Ok(())
}

const JSON_FILES: [&str; 4] = ["heads.json", "contract.json", "calibration.json", "manifest.json"];

/// The longest list whose price stays under `room`: `text` per element plus
/// `slot` per slot of its capacity, which the value cost takes as the next
/// power of two of its length. For each capacity, largest first, the most
/// elements that fit, if that is more than half of it (so that capacity is
/// the list's own).
fn padded_len(room: u64, text: u64, slot: u64) -> Option<u64> {
    (2..u64::BITS).rev().map(|k| 1u64 << k).find_map(|cap| {
        let rest = room.checked_sub(slot.checked_mul(cap)?)?;
        let n = (rest.checked_sub(1)? / text).min(cap);
        (n > cap / 2).then_some(n)
    })
}

/// The served graph class of the base artifact's graph, by its length.
fn base_class(base: &Path) -> Result<&'static slop_ninja_artifact_check::spec::GraphClass> {
    let len = fs::metadata(base.join(GRAPH))?.len();
    crate::limits::class_of_len(len).with_context(|| format!("base graph of {len} bytes has no served class"))
}

/// The value cost of `base`'s JSON role file `name`.
fn value_cost_of(base: &Path, name: &'static str) -> Result<u64> {
    let bytes = fs::read(base.join(name))?;
    slop_ninja_artifact_check::check_layout_cost(name, &bytes).map_err(|e| anyhow::anyhow!("base {name}: {e}"))
}

/// The base's `manifest.json` with a JSON case's change, in its canonical layout.
fn json_case(base: &Path, name: &str) -> Result<Vec<u8>> {
    use serde_json::{Value, json};
    use slop_ninja_artifact_check::canonical::{self, Layout, value_cost as c};
    let mut m: Value = serde_json::from_slice(&fs::read(base.join("manifest.json"))?)?;
    let class = base_class(base)?;
    let budget = crate::limits::json_value_budget(class);
    let base_cost = JSON_FILES.iter().map(|n| value_cost_of(base, n)).sum::<Result<u64>>()?;
    ensure!(base_cost < budget, "the base is past the value budget");
    // `{"a":0}` costs a map node, its key and its number, plus its array slot
    // (one to two slots of capacity).
    let object = c::OBJECT + 2 * (c::TEXT + 1);
    match name {
        "json-value-budget" => {
            let n = budget / object + 1;
            m["diagnostic_train_subset"] = Value::Array(vec![json!({"a": 0}); n as usize]);
        }
        "json-objects-in-budget" => {
            // Leave room for the slots at twice the length, the worst capacity.
            let n = (budget - base_cost) / (object + 2 * c::ARRAY_SLOT);
            m["token_head"] = Value::Array(vec![json!({"a": 0}); n as usize]);
        }
        "padded-provenance" => {
            // The longest list of one-letter strings whose price stays inside
            // the budget: 2^23 for R0 under spec v2's budget.
            let limits = m["limits"].as_array_mut().context("manifest limits list")?;
            let len = padded_len(budget - base_cost, c::TEXT + 1, c::ARRAY_SLOT).context("room")? as usize;
            let n = len.checked_sub(limits.len()).context("limits list length")?;
            limits.extend(std::iter::repeat_n(json!("a"), n));
        }
        other => bail!("no JSON case {other}"),
    }
    let bytes = canonical::encode(&m, Layout::PythonCompact);
    let cost = canonical::check_cost(&bytes, Layout::PythonCompact).map_err(|e| anyhow::anyhow!("{e}"))?;
    let total = base_cost - value_cost_of(base, "manifest.json")? + cost;
    let past = total > budget;
    ensure!(past == (name == "json-value-budget"), "{name}: value cost {total} against the budget {budget}");
    Ok(bytes)
}


/// A files manifest listing `base`'s files with one declared length changed;
/// the scorer refuses the manifest before any file byte arrives.
fn oversize_manifest(base: &Path, name: &str) -> Result<FilesManifest> {
    let (file, bytes) = match name {
        "oversize-protobuf" => (GRAPH, (1u64 << 31) + 1),
        "oversize-heads" => ("heads.json", 64 * 1024 * 1024 + 1),
        "oversize-json" => ("calibration.json", 300 * 1024 * 1024),
        other => bail!("no manifest for {other}"),
    };
    let mut files = Vec::new();
    for (n, role) in FILES {
        let (sha, len) = crate::profile::sha256_file(&base.join(n))?;
        let (sha, len) = if n == file { ("0".repeat(64), bytes) } else { (sha, len) };
        let key = slop_ninja_artifact_check::fs::reference_file_key(&sha);
        files.push(FileEntry { name: n.into(), role: role.into(), bytes: len, sha256: sha, bucket: "eval".into(), key, etag: None });
    }
    let m = FilesManifest {
        schema: SCHEMA.into(),
        profile_id: slop_ninja_artifact_check::mapping::profile_id_for(base_class(base)?, crate::engine::NEWEST_SPEC),
        contract: "slop_ninja_full_a_contract_v2".into(),
        graph: GRAPH.into(),
        files,
    };
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes_field(out: &mut Vec<u8>, field: u64, b: &[u8]) {
        out.push(((field << 3) | 2) as u8);
        out.push(b.len() as u8);
        out.extend_from_slice(b);
    }

    /// A tiny ModelProto: one Add node and two FLOAT initializers.
    fn model() -> Vec<u8> {
        let mut node = Vec::new();
        bytes_field(&mut node, 1, b"x");
        bytes_field(&mut node, 2, b"y");
        bytes_field(&mut node, 4, b"Add");
        let mut graph = Vec::new();
        bytes_field(&mut graph, 1, &node);
        for name in [&b"w"[..], b"v"] {
            let mut t = vec![0x08, 0x02, 0x10, 0x01]; // dims [2], data_type FLOAT
            bytes_field(&mut t, 8, name);
            let raw: Vec<u8> = [1.5f32, -2.0].iter().flat_map(|v| v.to_le_bytes()).collect();
            bytes_field(&mut t, 9, &raw);
            bytes_field(&mut graph, 5, &t);
        }
        let mut m = vec![0x08, 0x08]; // ir_version 8
        bytes_field(&mut m, 7, &graph);
        m
    }

    #[test]
    fn graph_mutations_keep_the_length_and_change_only_their_field() {
        let base = model();
        let mut filled = base.clone();
        assert_eq!(fill_float_initializers(&mut filled, 3.0e38).unwrap(), 4);
        assert_eq!(filled.len(), base.len());
        for r in float_initializer_payloads(&filled).unwrap() {
            assert!(filled[r].chunks_exact(4).all(|c| f32::from_le_bytes(c.try_into().unwrap()) == 3.0e38));
        }
        let mut one = base.clone();
        assert_eq!(fill_initializer(&mut one, "v", 7.0).unwrap(), 2);
        assert_eq!(float_initializer_payloads(&one).unwrap().len(), 2);
        let changed: Vec<usize> = one.iter().zip(&base).enumerate().filter(|(_, (a, b))| a != b).map(|(i, _)| i).collect();
        let v = float_initializer_payloads(&base).unwrap()[1].clone();
        assert!(changed.iter().all(|i| v.contains(i)));
        assert!(fill_initializer(&mut one, "absent", 7.0).is_err());
        let mut ext = base.clone();
        declare_external_data(&mut ext).unwrap();
        assert_eq!(ext.len(), base.len());
        assert_eq!(ext.iter().zip(&base).filter(|(a, b)| a != b).count(), 1);
        assert!(ext.windows(2).any(|w| w == [0x70, 0x01]));
        for to_domain in [true, false] {
            let mut p = base.clone();
            patch_first_add(&mut p, to_domain).unwrap();
            assert_eq!(p.len(), base.len());
            assert_ne!(p, base);
        }
    }

    #[test]
    fn the_in_budget_case_expects_the_refusal_of_the_bases_form() {
        let d = std::env::temp_dir().join(format!("a-adversarial-expect-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let c = case("json-objects-in-budget").unwrap();
        fs::write(d.join("manifest.json"), br#"{"schema":"slop_ninja_full_a_export_v2"}"#).unwrap();
        assert_eq!(c.expect_for(&d).unwrap(), Expect::Refused("schema"));
        fs::write(d.join("manifest.json"), br#"{"token_head":{"input":"x"}}"#).unwrap();
        assert_eq!(c.expect_for(&d).unwrap(), Expect::Refused("binding"));
        // Every other case keeps its own expectation, whatever the base.
        for other in CASES.iter().filter(|o| o.name != c.name) {
            assert_eq!(other.expect_for(&d).unwrap(), other.expect, "{}", other.name);
        }
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn padding_fills_the_room_at_the_lists_capacity() {
        use slop_ninja_artifact_check::canonical::value_cost as c;
        let (text, slot) = (c::TEXT + 1, c::ARRAY_SLOT);
        let price = |n: u64| text * n + slot * n.next_power_of_two();
        // R0 under spec v2's budget: 2^23, a power of two (its base cost 118,493,491).
        let room = slop_ninja_artifact_check::spec::MAX_JSON_VALUE_BYTES_V2 - 118_493_491;
        assert_eq!(padded_len(room, text, slot), Some(1 << 23));
        // A room where the power-of-two list leaves most of it: the list runs on
        // past 2^22, at capacity 2^23, until its price nearly fills the room.
        let room = slot * (1 << 23) + text * 4_300_000;
        let n = padded_len(room, text, slot).unwrap();
        assert!(n > 1 << 22 && price(n) < room && price(n + 1) >= room, "{n}");
        for room in [1, slot * 4, 10_000, 123_456_789] {
            if let Some(n) = padded_len(room, text, slot) {
                assert!(price(n) < room && price(n + 1) >= room, "{room} {n}");
            }
        }
    }

    #[test]
    fn every_case_has_a_writer_and_an_outcome() {
        let mut names: Vec<_> = CASES.iter().map(|c| c.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), CASES.len());
        for c in CASES {
            assert!(case(c.name).is_some());
            if c.manifest_only {
                assert!(matches!(c.expect, Expect::Refused(_)), "{}", c.name);
            }
        }
    }
}
