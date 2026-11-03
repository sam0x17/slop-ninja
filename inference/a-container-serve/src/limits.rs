//! The scorer's process and memory limits. Each comes from the artifact spec
//! (`slop_ninja_artifact_check`, the version the load names), the output
//! contract or a recorded measurement; none is tuned.
//!
//! - **Per artifact, before any part is read:** the parts manifest must list
//!   the load's spec's file table with the scorer's roles, the graph must have
//!   a class's exact length that spec serves, the tokenizer the pinned digest
//!   and length, the heads at most the heads runtime's limit, and the four JSON
//!   role files together at most the spec's JSON budget ([`check_manifest`]).
//! - **Per artifact, after assembly and before ONNX Runtime:** the whole
//!   admission check by the same spec (`engine::admit`).
//! - **Per instance:** one artifact, one cohort and one spec (`a_scorer.rs`);
//!   parts are deleted once assembled, so the disk holds one copy of one
//!   artifact.
//! - **Per process:** no core dumps ([`no_core_dumps`]); a panic aborts.
//! - **Memory:** with the graph's length fixed by its class, the JSON files'
//!   bytes and value cost bounded by the spec for that class, a document
//!   bounded by the contract's 2,048 tokens and request bodies bounded here,
//!   what a load of a class can use is bounded by [`memory_bound`]. A load is
//!   refused as a platform fault when the container's memory limit (its cgroup
//!   limit, else the VM's memory) is below its class's bound. The bound of a
//!   class is the same under every spec a load may name: they share the JSON
//!   budget, and each bounds a class's value cost by the class's own value.
use crate::artifact::ArtifactFiles;
use crate::engine::{NEWEST_SPEC, UNNAMED_LOAD_SPEC, admits_by};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use slop_ninja_artifact_check::spec::{GraphClass, Version};
use slop_ninja_artifact_check::{mapping, spec};

/// The spec versions a load may name ([`admits_by`]), oldest first.
pub fn load_specs() -> impl Iterator<Item = Version> {
    Version::ALL.into_iter().filter(|v| admits_by(*v))
}

/// The bound on the four JSON role files together: the largest of the
/// bounds of the specs a load may name (each defines one).
pub fn json_budget() -> u64 {
    load_specs().filter_map(Version::max_json_bytes).max().unwrap_or(spec::MAX_JSON_BYTES_V2)
}

/// Largest score request body.
pub const MAX_SCORE_BODY: usize = 8 << 20;
/// Most documents per score request.
pub const MAX_DOCS: usize = 128;
/// Most requests handled at once; more get 503.
pub const MAX_INFLIGHT: usize = 16;

/// Whether `id` is a parts manifest `profile_id` a packer writes: the spec's
/// `<spec id>/<graph class id>` for a version and class the tools define
/// (`slop_ninja_artifact_check`, `slop_ninja_miner pack`), or the served
/// profile's id, which `slop_ninja_a_served_profile pack` keeps for R0's own
/// files. Free text is refused: published manifests must not carry it.
pub fn known_profile_id(id: &str) -> bool {
    let spec_form = spec::Version::ALL
        .iter()
        .any(|v| v.classes().iter().any(|g| id == mapping::profile_id_for(g, *v)));
    spec_form || crate::profile::ServedProfile::release_candidate().is_ok_and(|p| p.profile_id == id)
}

/// Checks a parts manifest against spec `version`'s file table before any
/// part is read: names, scorer roles, the contract and graph names, every
/// declared length (the graph's at a class that version serves), the JSON
/// budget and the profile id. A refusal reads `refused <code>: <detail>`.
pub fn check_manifest(m: &ArtifactFiles, version: Version) -> Result<()> {
    ensure!(m.contract == spec::CONTRACT_ID, "refused contract_id: {:?} is not {}", m.contract, spec::CONTRACT_ID);
    ensure!(m.graph == spec::GRAPH_FILE, "refused undeclared_file: graph {:?}", m.graph);
    for f in &m.files {
        let role = spec::role_by_name(&f.name).ok_or_else(|| anyhow!("refused undeclared_file: {:?}", f.name))?;
        ensure!(f.role == role.scorer_role, "refused file_role: {} must have scorer role {}", f.name, role.scorer_role);
    }
    let listing: Vec<(&str, &str, u64)> = m.files.iter().map(|f| (f.name.as_str(), f.sha256.as_str(), f.bytes)).collect();
    mapping::check_listing(&m.contract, &listing, version).map_err(|e| anyhow!("refused {}: {e}", e.code()))?;
    // Last, so each length refusal keeps its own code; the id is not echoed.
    ensure!(known_profile_id(&m.profile_id), "refused profile_id: not a packer's profile id");
    Ok(())
}

/// Disable core dumps, so a crash never writes model weights or document text
/// to disk.
pub fn no_core_dumps() -> Result<()> {
    let zero = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: setrlimit reads one initialised struct and changes this process's limit only.
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_CORE, &zero) };
    ensure!(rc == 0, "setrlimit(RLIMIT_CORE, 0) failed");
    Ok(())
}

/// The cgroup v2 memory limit of this container, when one is set and visible.
pub fn cgroup_memory_max() -> Option<u64> {
    std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok()?.trim().parse().ok()
}

/// The memory this kernel manages (`MemTotal` in `/proc/meminfo`). In a
/// Cloudflare Container, a microVM with no visible cgroup limit, it is the
/// instance's memory.
pub fn mem_total() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kib: u64 = info.lines().find_map(|l| l.strip_prefix("MemTotal:"))?.trim().strip_suffix("kB")?.trim().parse().ok()?;
    kib.checked_mul(1024)
}

/// The memory limit a load must fit: the cgroup limit when one is visible
/// and numeric, else the kernel's memory, whichever is smaller when both are
/// known. `None` only when neither can be read.
pub fn memory_limit() -> Option<u64> {
    match (cgroup_memory_max(), mem_total()) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Memory model `slop_ninja_a_scorer_memory_v3` (recorded in the scorer's
/// README): `slop_ninja_a_scorer_memory_v2` per graph class. The session and
/// base terms are measured: peak resident memory of `slop_ninja_a_local_score`
/// loading an artifact and scoring documents of up to 2,048 tokens, with ONNX
/// Runtime 1.31.1 and one intra-op thread per session. R0 (plain class, graph
/// 601,028,257 bytes) on November 1, 2026: 1,565,851,648 bytes with one session
/// and 5,185,441,792 with four. The S2c span package (tower class, graph
/// 681,508,964 bytes, spans requested) on November 2, 2026, on the container
/// parity requests, 16 documents of 2,040 to 2,048 tokens, the package's 215
/// synthetic reference cases and a cohort-sized stream of 1,500 synthetic
/// documents of 8 to 2,048 tokens, in a random order and in ascending length:
/// at most 1,716,305,920 bytes with one session and 6,651,670,528 with four.
/// The peak depends on the documents a session has served and their order,
/// not only on their number; the tower term covers four-session instances that
/// serve up to 1,500 documents (the one-session peak was measured on at most
/// 215 documents). The JSON terms come from the spec: its byte budget and the
/// class's value budget (a bound on what any parse of the files holds, checked
/// before the first parse).
pub mod measured {
    /// Process baseline: the one-session peak minus one session (R0).
    pub const BASE_BYTES: u64 = 359_321_600;
    /// Resident bytes per session per graph byte of the plain and span
    /// classes: (four-session peak minus one-session peak) / 3 / graph bytes
    /// = 1,206,530,048 / 601,028,257 (R0).
    pub const SESSION_PER_GRAPH_BYTE: f64 = 2.0075;
    /// The same for the tower class, with the S2c span package: the smallest
    /// coefficient with which [`BASE_BYTES`] and the sessions cover every
    /// measured peak, set by four sessions on the 1,500-document stream in
    /// ascending length, (6,651,670,528 - 359,321,600) / 4 / 681,508,964 =
    /// 2.30824, rounded up.
    pub const TOWER_SESSION_PER_GRAPH_BYTE: f64 = 2.3083;
    /// What a standard-4 instance left the scorer process, from its `/info`
    /// on October 29, 2026 (four sessions, R0 loaded): free memory 7,111,452
    /// KiB plus the process's resident set 4,683,932 KiB. `MemTotal` read
    /// 12,508,404 KiB; no cgroup memory limit was visible.
    pub const STANDARD_4_PROCESS_BYTES: u64 = (7_111_452 + 4_683_932) * 1024;
}

/// Raw copies of the JSON role files a load holds at once: the admission
/// buffers and the guards' reads.
pub const JSON_BYTE_COPIES: u64 = 2;
/// Parses of the JSON role files live at once, each bounded by the files'
/// value cost: admission's typed schemas while its guard views are built,
/// and the heads runtime the model keeps.
pub const JSON_VALUE_COPIES: u64 = 3;
/// The sessions the deployed scorer runs (`A_SESSIONS` default), with which
/// the spec's value budgets were derived.
pub const REFERENCE_SESSIONS: usize = 4;

/// The classes the scorer serves: the newest spec's, which include every
/// class an older spec a load may name serves (a test checks it).
pub fn served_classes() -> impl Iterator<Item = &'static GraphClass> {
    NEWEST_SPEC.classes().iter().filter(|g| NEWEST_SPEC.serves(g))
}

/// The class of a graph of `len` bytes that spec `version` serves, if any.
pub fn class_of_len_for(version: Version, len: u64) -> Option<&'static GraphClass> {
    version.graph_class_by_len(len).filter(|g| version.serves(g))
}

/// The served class of a graph of `len` bytes under the newest spec, if any.
pub fn class_of_len(len: u64) -> Option<&'static GraphClass> {
    class_of_len_for(NEWEST_SPEC, len)
}

/// The largest graph any served class has.
fn max_graph_bytes() -> u64 {
    served_classes().map(|g| g.bytes).max().unwrap_or(0)
}

/// Request bodies in flight: each held once raw and once parsed.
pub fn requests_bytes() -> u64 {
    (MAX_INFLIGHT * 2 * MAX_SCORE_BODY) as u64
}

/// The spec's bound on the JSON files' summed value cost for `class`, the
/// same under every spec a load may name (a test checks it).
pub fn json_value_budget(class: &GraphClass) -> u64 {
    NEWEST_SPEC.max_json_value_bytes_for(class).unwrap_or(class.max_json_value_bytes)
}

/// Measured resident bytes per session per graph byte of `class`.
pub fn session_per_graph_byte(class: &GraphClass) -> f64 {
    if class.tower_states { measured::TOWER_SESSION_PER_GRAPH_BYTE } else { measured::SESSION_PER_GRAPH_BYTE }
}

fn sessions_bytes(class: &GraphClass, sessions: usize) -> u64 {
    (sessions as f64 * class.bytes as f64 * session_per_graph_byte(class)).ceil() as u64
}

/// What a load of `class` with `sessions` sessions can use at most, for any
/// artifact of that class the spec admits or refuses after assembly: the
/// measured model at the class's graph, the request bodies, the JSON byte
/// budget and the class's JSON value budget, each times its copies.
pub fn memory_bound(class: &GraphClass, sessions: usize) -> u64 {
    measured::BASE_BYTES
        + sessions_bytes(class, sessions)
        + requests_bytes()
        + JSON_BYTE_COPIES * json_budget()
        + JSON_VALUE_COPIES * json_value_budget(class)
}

/// The largest [`memory_bound`] of a served class: what a container needs to
/// load every admitted artifact.
pub fn max_memory_bound(sessions: usize) -> u64 {
    served_classes().map(|g| memory_bound(g, sessions)).max().unwrap_or(0)
}

/// The value budget the reference deployment holds for `class`: what a
/// standard-4 instance leaves the process, less every other term of
/// [`memory_bound`] at [`REFERENCE_SESSIONS`], divided among the value copies.
/// Spec v2's `MAX_JSON_VALUE_BYTES_V2` is this figure at the span class (the
/// larger of the plain and span graphs, which share it), and spec v3's
/// `MAX_JSON_VALUE_BYTES_TOWER` at the tower class.
pub fn reference_value_budget(class: &GraphClass) -> u64 {
    let rest = measured::BASE_BYTES
        + sessions_bytes(class, REFERENCE_SESSIONS)
        + requests_bytes()
        + JSON_BYTE_COPIES * json_budget();
    (measured::STANDARD_4_PROCESS_BYTES - rest) / JSON_VALUE_COPIES
}

/// Whether a container with memory limit `max` can load an artifact of
/// `class` with `sessions` sessions. An unknown limit passes; the scorer
/// reads the cgroup limit or the VM's memory, so it is unknown only when
/// neither file can be read.
pub fn memory_fits(max: Option<u64>, class: &GraphClass, sessions: usize) -> bool {
    max.is_none_or(|m| memory_bound(class, sessions) <= m)
}

/// Whether a container with memory limit `max` can load the artifact a
/// parts manifest checked by spec `version` ([`check_manifest`]) lists: its
/// graph's class's bound. A manifest whose graph has no class that version
/// serves does not fit.
pub fn manifest_fits(m: &ArtifactFiles, version: Version, max: Option<u64>, sessions: usize) -> bool {
    m.files
        .iter()
        .find(|f| f.name == m.graph)
        .and_then(|f| class_of_len_for(version, f.bytes))
        .is_some_and(|c| memory_fits(max, c, sessions))
}

/// The limits, for `/healthz` and `/info`: `specs` lists the specs a load may
/// name (by id and digest) and `unnamed_load_spec` the one a load naming none
/// is admitted by; `memory_bound_bytes` is the largest class bound, and
/// `classes` lists each served class's own.
pub fn report(sessions: usize) -> Value {
    let specs: Vec<Value> = load_specs().map(|v| json!({"id": v.id(), "sha256": v.spec_sha256()})).collect();
    let classes: serde_json::Map<String, Value> = served_classes()
        .map(|g| {
            (g.id.to_string(), json!({"graph_bytes": g.bytes, "json_value_budget_bytes": json_value_budget(g),
                "memory_bound_bytes": memory_bound(g, sessions)}))
        })
        .collect();
    json!({"specs": specs, "unnamed_load_spec": UNNAMED_LOAD_SPEC.id(), "json_budget_bytes": json_budget(),
        "part_bytes_max": spec::PART_BYTES, "manifest_bytes_max": spec::MAX_MANIFEST_BYTES,
        "graph_bytes_max": max_graph_bytes(), "tokens_max": crate::engine::MAX_TOKENS,
        "score_body_bytes_max": MAX_SCORE_BODY, "docs_max": MAX_DOCS, "inflight_max": MAX_INFLIGHT,
        "memory_bound_bytes": max_memory_bound(sessions), "classes": classes,
        "cgroup_memory_max": cgroup_memory_max(), "mem_total": mem_total(), "memory_limit": memory_limit(),
        "memory_model": "slop_ninja_a_scorer_memory_v3", "core_dumps": false})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bound_covers_the_measured_peaks_and_fits_the_deployed_instance() {
        let plain = &spec::GRAPHS[0];
        // The measured peaks with R0 (one and four sessions) are inside the bound,
        // and so are the S2c span package's without the JSON and request terms.
        assert!(memory_bound(plain, 1) >= 1_565_851_648 && memory_bound(plain, 4) >= 5_185_441_792);
        let tower = &spec::TOWER_GRAPH;
        let peaks = [(1, 1_716_305_920), (4, 5_663_686_656), (4, 5_944_213_504), (4, 6_305_533_952),
            (4, 6_416_629_760), (4, 6_651_670_528)];
        for (sessions, peak) in peaks {
            assert!(measured::BASE_BYTES + sessions_bytes(tower, sessions) >= peak, "{sessions} sessions");
        }
        // The deployed configuration, four sessions on standard-4, fits what
        // the instance left the process, and so its MemTotal, for every class.
        for g in served_classes() {
            assert!(memory_bound(g, REFERENCE_SESSIONS) <= measured::STANDARD_4_PROCESS_BYTES, "{}", g.id);
            assert!(memory_fits(Some(12_508_404 * 1024), g, REFERENCE_SESSIONS), "{}", g.id);
            // standard-3 (MemTotal 8,380,140 KiB, measured the same way) does
            // not hold four sessions.
            assert!(!memory_fits(Some(8_380_140 * 1024), g, REFERENCE_SESSIONS), "{}", g.id);
            assert!(memory_fits(None, g, 8));
        }
        assert_eq!(max_memory_bound(REFERENCE_SESSIONS), served_classes().map(|g| memory_bound(g, 4)).max().unwrap());
        assert_eq!(json_budget(), 4 * 64 * 1024 * 1024);
        // Every class the scorer admits by is served, the tower class included.
        assert_eq!(served_classes().count(), NEWEST_SPEC.classes().len());
        assert_eq!(class_of_len(spec::TOWER_GRAPH.bytes), Some(&spec::TOWER_GRAPH));
    }

    #[test]
    fn the_spec_value_budgets_are_the_reference_remainders() {
        // Spec v2's budget, at the larger of the plain and span graphs, which share it.
        assert_eq!(
            spec::MAX_JSON_VALUE_BYTES_V2,
            reference_value_budget(&spec::GRAPHS[1]),
            "a memory model term changed: re-derive spec::MAX_JSON_VALUE_BYTES_V2 (a spec v2 change)"
        );
        assert!(spec::GRAPHS[0].bytes < spec::GRAPHS[1].bytes);
        assert!(spec::MAX_JSON_VALUE_BYTES_V2 <= reference_value_budget(&spec::GRAPHS[0]));
        // Spec v3's tower budget, at the tower graph and its own coefficient.
        assert_eq!(
            spec::MAX_JSON_VALUE_BYTES_TOWER,
            reference_value_budget(&spec::TOWER_GRAPH),
            "a memory model term changed: re-derive spec::MAX_JSON_VALUE_BYTES_TOWER (a spec v3 change)"
        );
        for g in served_classes() {
            assert_eq!(json_value_budget(g), g.max_json_value_bytes);
        }
    }

    #[test]
    fn every_spec_a_load_may_name_has_the_newest_specs_class_bounds() {
        // Spec v2 and v3; spec v1 bounds no JSON file.
        assert_eq!(load_specs().collect::<Vec<_>>(), [Version::V2, Version::V3]);
        assert!(admits_by(UNNAMED_LOAD_SPEC) && NEWEST_SPEC == Version::LATEST);
        for v in load_specs() {
            assert_eq!(v.max_json_bytes(), Some(json_budget()), "{}", v.id());
            for g in v.classes().iter().filter(|g| v.serves(g)) {
                // Each class it serves is a served class with the same length and
                // value budget, so its memory bound is the newest spec's.
                assert!(served_classes().any(|s| s == g), "{} {}", v.id(), g.id);
                assert_eq!(v.max_json_value_bytes_for(g), Some(json_value_budget(g)), "{} {}", v.id(), g.id);
                assert_eq!(class_of_len_for(v, g.bytes), Some(g));
            }
        }
        // The tower class is served only under spec v3.
        assert_eq!(class_of_len_for(Version::V2, spec::TOWER_GRAPH.bytes), None);
        assert_eq!(class_of_len_for(Version::V3, spec::TOWER_GRAPH.bytes), Some(&spec::TOWER_GRAPH));
        let r = report(REFERENCE_SESSIONS);
        assert_eq!(r["specs"].as_array().map(Vec::len), Some(2));
        assert_eq!(r["unnamed_load_spec"], UNNAMED_LOAD_SPEC.id());
    }

    #[test]
    fn this_host_reports_a_limit() {
        // Linux always has /proc/meminfo, so the limit is never unknown here.
        assert!(mem_total().is_some_and(|m| m > 0));
        assert!(memory_limit().is_some());
    }

    #[test]
    fn profile_ids_are_the_packers_only() {
        for v in spec::Version::ALL {
            for g in v.classes() {
                assert!(known_profile_id(&mapping::profile_id_for(g, v)));
            }
        }
        // The tower class is a spec v3 class only.
        for v in [spec::Version::V1, spec::Version::V2] {
            assert!(!known_profile_id(&mapping::profile_id_for(&spec::TOWER_GRAPH, v)));
        }
        let served = crate::profile::ServedProfile::release_candidate().unwrap().profile_id;
        assert!(known_profile_id(&served));
        for bad in ["", "adversarial/x", "testnet-rehearsal-variant:soup", "slop_ninja_a_artifact_spec_v2/"] {
            assert!(!known_profile_id(bad), "{bad}");
        }
    }
}
