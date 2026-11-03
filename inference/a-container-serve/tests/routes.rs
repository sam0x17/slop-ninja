//! `a-scorer`'s routes, through the built binary on loopback: the single-use
//! binding, the manifest check before any part, the clean work directory, core
//! dumps off, and that no document text or id reaches standard error,
//! `/healthz` or a refusal.
//!
//! The binding tests need no model: the parts of a manifest that passes the
//! spec's file table are made present by writing files under their digests in
//! the work directory, so the load binds and then fails at assembly. The
//! scoring test needs R0's files (`A_R0_MODEL_DIR`) and ONNX Runtime 1.31.1
//! (`A_ORT_LIB`); without them it prints a notice and passes. Nothing leaves
//! the host.
use a_container_serve::artifact::{ArtifactFile, ArtifactFiles, Part, SCHEMA};
use serde_json::{Value, json};
use slop_ninja_artifact_check::{mapping, spec};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A text and an id that must never be echoed outside a result's `id`.
const SENTINEL_TEXT: &str = "zyxwvut sentinel-text-4f1c quokka marmalade";
const SENTINEL_ID: &str = "sentinel-id-93be";

/// A test's work directory, with its `.stderr` and `.parts` siblings; all
/// three are removed when it drops, also when the test fails.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let s = Scratch(std::env::temp_dir().join(format!("a-routes-{tag}-{}", std::process::id())));
        s.clear();
        std::fs::create_dir_all(&s.0).unwrap();
        s
    }

    fn clear(&self) {
        let _ = std::fs::remove_dir_all(&self.0);
        let _ = std::fs::remove_dir_all(self.0.with_extension("parts"));
        let _ = std::fs::remove_file(self.0.with_extension("stderr"));
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        self.clear();
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

struct Scorer {
    child: Child,
    port: u16,
    stderr: PathBuf,
}

impl Drop for Scorer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Scorer {
    fn start(work: &Path, lib: Option<&str>) -> Scorer {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let stderr = work.with_extension("stderr");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_a-scorer"));
        cmd.env("A_HOST", "127.0.0.1")
            .env("A_PORT", port.to_string())
            .env("A_SESSIONS", "1")
            .env("A_WORK_DIR", work)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr).unwrap());
        match lib {
            Some(l) => cmd.env("A_ORT_LIB", l),
            None => cmd.env("A_ORT_LIB", "/nonexistent/libonnxruntime.so"),
        };
        // Start the child with core dumps as allowed as this environment lets
        // them be, so the scorer's own limit is what the test sees.
        // SAFETY: getrlimit/setrlimit between fork and exec touch only this
        // process's limits and allocate nothing.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                let mut l = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
                if libc::getrlimit(libc::RLIMIT_CORE, &mut l) == 0 {
                    l.rlim_cur = l.rlim_max;
                    libc::setrlimit(libc::RLIMIT_CORE, &l);
                }
                Ok(())
            });
        }
        let child = cmd.spawn().unwrap();
        let s = Scorer { child, port, stderr };
        let t = Instant::now();
        while s.try_http("GET", "/healthz", b"").is_none() {
            assert!(t.elapsed() < Duration::from_secs(30), "a-scorer did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        s
    }

    fn try_http(&self, method: &str, path: &str, body: &[u8]) -> Option<(u16, Vec<u8>)> {
        let mut c = TcpStream::connect(("127.0.0.1", self.port)).ok()?;
        c.set_read_timeout(Some(Duration::from_secs(120))).ok()?;
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        c.write_all(head.as_bytes()).ok()?;
        c.write_all(body).ok()?;
        let mut out = Vec::new();
        c.read_to_end(&mut out).ok()?;
        let split = out.windows(4).position(|w| w == b"\r\n\r\n")?;
        let status: u16 = std::str::from_utf8(&out[..split]).ok()?.split(' ').nth(1)?.parse().ok()?;
        Some((status, out[split + 4..].to_vec()))
    }

    fn http(&self, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
        let (code, b) = self.try_http(method, path, body).expect("request");
        (code, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    fn health(&self) -> Value {
        self.http("GET", "/healthz", b"").1
    }

    fn wait_for(&self, state: &str) -> Value {
        let t = Instant::now();
        loop {
            let h = self.health();
            if h["state"] == state {
                return h;
            }
            assert!(h["state"] != "failed", "load failed: {}", h["error"]);
            assert!(t.elapsed() < Duration::from_secs(300), "no {state} state: {h}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    fn assert_no_sentinel(&self, also: &[&Value]) {
        let health = self.health().to_string();
        let err = self.stderr();
        for s in [SENTINEL_TEXT, SENTINEL_ID, "sentinel"] {
            assert!(!err.contains(s), "standard error carries {s:?}: {err}");
            assert!(!health.contains(s), "/healthz carries {s:?}: {health}");
            for v in also {
                assert!(!v.to_string().contains(s), "a response carries {s:?}: {v}");
            }
        }
    }
}

/// A manifest that passes the spec's file table: the plain class's graph
/// length, the pinned tokenizer and small JSON files, each with placeholder
/// part digests derived from `seed`. `calibration_bytes` sets the calibration
/// file's declared length.
fn manifest(seed: u8, calibration_bytes: u64, profile_id: &str) -> (Vec<u8>, String) {
    manifest_with_graph(seed, spec::GRAPHS[0].bytes, calibration_bytes, profile_id)
}

/// [`manifest`] with a graph of `graph_bytes`.
fn manifest_with_graph(seed: u8, graph_bytes: u64, calibration_bytes: u64, profile_id: &str) -> (Vec<u8>, String) {
    let mut files = Vec::new();
    // Placeholder digests: file k is `k * 1000`, its parts `k * 1000 + 1 + i` (at most 64 parts a file).
    for (k, r) in (0u64..).zip(&spec::ROLES) {
        let bytes = match r.name {
            spec::GRAPH_FILE => graph_bytes,
            "tokenizer.json" => spec::TOKENIZER_BYTES,
            "calibration.json" => calibration_bytes,
            _ => 1000,
        };
        let sha = match r.name {
            "tokenizer.json" => spec::TOKENIZER_SHA256.to_string(),
            _ => format!("{seed:02x}{:062x}", k * 1000),
        };
        let parts = (0..bytes.div_ceil(spec::PART_BYTES))
            .map(|i| Part {
                sha256: format!("{seed:02x}{:062x}", k * 1000 + 1 + i),
                bytes: spec::PART_BYTES.min(bytes - i * spec::PART_BYTES),
            })
            .collect();
        files.push(ArtifactFile { name: r.name.into(), role: r.scorer_role.into(), bytes, sha256: sha, parts });
    }
    let m = ArtifactFiles {
        schema: SCHEMA.into(),
        profile_id: profile_id.into(),
        contract: spec::CONTRACT_ID.into(),
        graph: spec::GRAPH_FILE.into(),
        files,
    };
    let bytes = serde_json::to_vec(&m).unwrap();
    let package = a_container_serve::profile::sha256_hex(&bytes);
    (bytes, package)
}

fn profile_id() -> String {
    mapping::profile_id_for(&spec::GRAPHS[0], a_container_serve::engine::NEWEST_SPEC)
}

fn score_body(package: &str, cohort: Option<&str>) -> Vec<u8> {
    let mut b = json!({"package": package, "spans": true,
        "docs": [{"id": SENTINEL_ID, "text": SENTINEL_TEXT}, {"id": "plain-2", "text": "Short text for the scorer."}]});
    if let Some(c) = cohort {
        b["cohort"] = json!(c);
    }
    serde_json::to_vec(&b).unwrap()
}

#[test]
fn the_scorer_starts_with_a_clean_work_directory_and_no_core_dumps() {
    let work = Scratch::new("start");
    std::fs::create_dir_all(work.join("blobs")).unwrap();
    std::fs::create_dir_all(work.join("files/old-package")).unwrap();
    std::fs::write(work.join("blobs/leftover"), b"x").unwrap();
    std::fs::write(work.join("files/old-package/heads.json"), b"{}").unwrap();
    std::fs::write(work.join("unrelated.txt"), b"kept").unwrap();
    let s = Scorer::start(&work, None);
    assert!(!work.join("blobs/leftover").exists() && !work.join("files").exists());
    assert!(work.join("blobs").is_dir(), "blobs/ is recreated");
    assert!(work.join("unrelated.txt").exists(), "only blobs/ and files/ are cleared");
    let limits = std::fs::read_to_string(format!("/proc/{}/limits", s.child.id())).unwrap();
    let core = limits.lines().find(|l| l.starts_with("Max core file size")).unwrap();
    assert_eq!(core.split_whitespace().skip(4).take(2).collect::<Vec<_>>(), ["0", "0"], "{core}");
    let mut mine = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: getrlimit writes one initialised struct.
    if unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut mine) } == 0 && mine.rlim_max == 0 {
        eprintln!("notice: core dumps are already off here; the scorer's own limit is not distinguished");
    }
    let h = s.health();
    assert_eq!(h["state"], "empty");
    assert!(h["build"]["limits"]["memory_limit"].as_u64().is_some_and(|m| m > 0), "{h}");
    drop(s);
}

#[test]
fn manifests_are_refused_before_any_part_is_read() {
    let work = Scratch::new("manifest");
    let s = Scorer::start(&work, None);
    let load = |m: &[u8], p: &str| s.http("POST", &format!("/v1/load?package={p}"), m);
    // Past the JSON budget, from the declared lengths.
    let (m, p) = manifest(1, 300 << 20, &profile_id());
    let (code, v) = load(&m, &p);
    assert_eq!(code, 422, "{v}");
    assert!(v["error"].as_str().unwrap().starts_with("refused json_budget"), "{v}");
    // A free-text profile id.
    let (m, p) = manifest(2, 1000, "notes: /home/example/notes");
    let (code, v) = load(&m, &p);
    assert_eq!(code, 422, "{v}");
    assert!(v["error"].as_str().unwrap().starts_with("refused profile_id"), "{v}");
    assert!(!v.to_string().contains("/home/example"), "the refusal echoes the profile id: {v}");
    // A manifest that is not the package's bytes.
    let (m, _) = manifest(3, 1000, &profile_id());
    assert_eq!(load(&m, &"0".repeat(64)).0, 422);
    // A valid manifest without its parts: refused, and the instance stays unbound.
    let (m, p) = manifest(4, 1000, &profile_id());
    let (code, v) = load(&m, &p);
    assert_eq!((code, v["error"].as_str()), (409, Some("missing_parts")), "{v}");
    assert_eq!(s.health()["state"], "empty");
    // Bad queries, and specs the scorer does not admit by: 400, not the artifact's refusal.
    assert_eq!(s.http("POST", "/v1/load?package=ab&cohort=c.1", &m).0, 400);
    assert_eq!(s.http("POST", "/v1/load?package=ab&cohort=c1&cohort=c2", &m).0, 400);
    for bad in [spec::Version::V1.spec_sha256(), "0".repeat(64), "v3".into()] {
        let (code, v) = s.http("POST", &format!("/v1/load?package={p}&spec={bad}"), &m);
        assert_eq!((code, v["error"].as_str()), (400, Some("spec")), "{bad}: {v}");
    }
    assert_eq!(s.health()["state"], "empty");
    drop(s);
}

#[test]
fn a_load_admits_by_the_spec_it_names_and_binds_it() {
    let work = Scratch::new("spec");
    let s = Scorer::start(&work, None);
    let (v2, v3) = (spec::Version::V2.spec_sha256(), spec::Version::V3.spec_sha256());
    // A tower-class manifest (spec v3 only): a load that names spec v2, or no
    // spec, refuses it before any part is read; one that names spec v3 checks it.
    let tower = mapping::profile_id_for(&spec::TOWER_GRAPH, spec::Version::V3);
    let (m, p) = manifest_with_graph(7, spec::TOWER_GRAPH.bytes, 1000, &tower);
    for q in [format!("package={p}"), format!("package={p}&cohort=c1"), format!("package={p}&cohort=c1&spec={v2}")] {
        let (code, v) = s.http("POST", &format!("/v1/load?{q}"), &m);
        assert_eq!(code, 422, "{q}: {v}");
        assert!(v["error"].as_str().unwrap().starts_with("refused graph_size"), "{q}: {v}");
    }
    let (code, v) = s.http("POST", &format!("/v1/load?package={p}&cohort=c1&spec={v3}"), &m);
    assert_eq!((code, v["error"].as_str()), (409, Some("missing_parts")), "{v}");
    assert_eq!(s.health()["state"], "empty");
    // With the parts present the load binds, and the spec is part of the binding.
    let parsed: ArtifactFiles = serde_json::from_slice(&m).unwrap();
    for part in parsed.parts() {
        std::fs::write(work.join("blobs").join(&part.sha256), b"placeholder").unwrap();
    }
    assert_eq!(s.http("POST", &format!("/v1/load?package={p}&cohort=c1&spec={v3}"), &m).0, 202);
    let h = s.wait_for("failed");
    assert_eq!((h["cohort"].as_str(), h["spec"].as_str()), (Some("c1"), Some(spec::Version::V3.id())), "{h}");
    assert_eq!(s.http("POST", &format!("/v1/load?package={p}&cohort=c1&spec={v3}"), &m).0, 200);
    for q in [format!("package={p}&cohort=c1"), format!("package={p}&cohort=c1&spec={v2}")] {
        let (code, v) = s.http("POST", &format!("/v1/load?{q}"), &m);
        assert_eq!((code, v["error"].as_str()), (409, Some("instance_bound")), "{q}: {v}");
    }
    // Score requests name the package and cohort only.
    let (code, v) = s.http("POST", "/v1/score", &score_body(&p, Some("c1")));
    assert_eq!((code, v["error"].as_str()), (409, Some("package_not_ready")), "{v}");
    // `/healthz` lists the specs a load may name and the one a load naming none gets.
    let limits = &h["build"]["limits"];
    let named: Vec<&str> = limits["specs"].as_array().unwrap().iter().map(|x| x["sha256"].as_str().unwrap()).collect();
    assert_eq!(named, [v2.as_str(), v3.as_str()], "{limits}");
    assert_eq!(limits["unnamed_load_spec"], spec::Version::V2.id());
    drop(s);
}

#[test]
fn a_bound_instance_refuses_every_other_package_cohort_and_part() {
    let work = Scratch::new("bound");
    let s = Scorer::start(&work, None);
    let (m, p) = manifest(5, 1000, &profile_id());
    let parsed: ArtifactFiles = serde_json::from_slice(&m).unwrap();
    // The parts are present (placeholders under their digests), so the load binds;
    // assembly then fails on the first part's digest.
    for part in parsed.parts() {
        std::fs::write(work.join("blobs").join(&part.sha256), b"placeholder").unwrap();
    }
    let bound_part = parsed.parts()[0].sha256.clone();
    let (code, v) = s.http("POST", &format!("/v1/load?package={p}&cohort=c1"), &m);
    assert_eq!(code, 202, "{v}");
    let h = s.wait_for("failed");
    assert_eq!((h["package"].as_str(), h["cohort"].as_str()), (Some(p.as_str()), Some("c1")));
    // Loads: the same binding answers; anything else is refused.
    assert_eq!(s.http("POST", &format!("/v1/load?package={p}&cohort=c1"), &m).0, 200);
    for q in [format!("package={p}&cohort=c2"), format!("package={p}")] {
        let (code, v) = s.http("POST", &format!("/v1/load?{q}"), &m);
        assert_eq!((code, v["error"].as_str()), (409, Some("instance_bound")), "{q}: {v}");
    }
    let (m2, p2) = manifest(6, 1000, &profile_id());
    let (code, v) = s.http("POST", &format!("/v1/load?package={p2}&cohort=c1"), &m2);
    assert_eq!((code, v["error"].as_str()), (409, Some("instance_bound")), "{v}");
    // A refused manifest on a bound instance is still `instance_bound`.
    let (code, _) = s.http("POST", &format!("/v1/load?package={p2}"), b"not json");
    assert_eq!(code, 409);
    // Parts: a part of the bound package counts as present; any other is refused.
    assert_eq!(s.http("HEAD", &format!("/v1/blobs/{bound_part}"), b"").0, 200);
    assert_eq!(s.http("PUT", &format!("/v1/blobs/{bound_part}"), b"x").0, 201);
    let junk = a_container_serve::profile::sha256_hex(b"junk");
    let (code, v) = s.http("PUT", &format!("/v1/blobs/{junk}"), b"junk");
    assert_eq!((code, v["error"].as_str()), (409, Some("instance_bound")), "{v}");
    assert!(!work.join("blobs").join(&junk).exists());
    // Scores: another cohort or package is `instance_bound`; the bound one is not ready.
    let mut responses = Vec::new();
    for (pkg, cohort, want) in [
        (p.as_str(), Some("c2"), "instance_bound"),
        (p.as_str(), None, "instance_bound"),
        (p2.as_str(), Some("c1"), "instance_bound"),
        (p.as_str(), Some("c1"), "package_not_ready"),
    ] {
        let (code, v) = s.http("POST", "/v1/score", &score_body(pkg, cohort));
        assert_eq!((code, v["error"].as_str()), (409, Some(want)), "{v}");
        responses.push(v);
    }
    let refs: Vec<&Value> = responses.iter().chain([&h]).collect();
    s.assert_no_sentinel(&refs);
    drop(s);
}

/// The per-document result keys of a scored document without spans emitted.
const RESULT_KEYS: [&str; 7] = ["id", "ms", "output_sha256", "prediction", "status", "text_sha256", "token_count"];

#[test]
fn r0_scores_through_the_routes_without_echoing_text_and_stays_bound() {
    let (Some(r0), Some(lib)) = (
        std::env::var_os("A_R0_MODEL_DIR").map(PathBuf::from),
        std::env::var("A_ORT_LIB").ok().filter(|v| !v.is_empty()),
    ) else {
        eprintln!("notice: A_R0_MODEL_DIR or A_ORT_LIB is unset; the R0 route test passes without running");
        return;
    };
    let work = Scratch::new("r0");
    let parts = work.with_extension("parts");
    let report = slop_ninja_artifact_check::fs::check_dir_for(&r0, a_container_serve::engine::UNNAMED_LOAD_SPEC).unwrap();
    let manifest_path = slop_ninja_artifact_check::fs::write_parts(&r0, &report, &parts).unwrap();
    let m = std::fs::read(&manifest_path).unwrap();
    let p = a_container_serve::profile::sha256_hex(&m);
    let s = Scorer::start(&work, Some(&lib));
    let parsed: ArtifactFiles = serde_json::from_slice(&m).unwrap();
    for part in parsed.parts() {
        let bytes = std::fs::read(parts.join("blobs").join(&part.sha256)).unwrap();
        assert_eq!(s.http("PUT", &format!("/v1/blobs/{}", part.sha256), &bytes).0, 201);
    }
    std::fs::remove_dir_all(&parts).unwrap();
    assert_eq!(s.http("POST", &format!("/v1/load?package={p}&cohort=testnet-routes-1"), &m).0, 202);
    let ready = s.wait_for("ready");
    let (code, v) = s.http("POST", "/v1/score", &score_body(&p, Some("testnet-routes-1")));
    assert_eq!(code, 200, "{v}");
    let results = v["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    for r in results {
        assert_eq!(r["status"], "ok", "{r}");
        // The content-free schema, exactly: R0 has no token head, so asking for
        // spans adds nothing.
        let keys: Vec<&str> = r.as_object().unwrap().keys().map(String::as_str).collect();
        let mut want = RESULT_KEYS.to_vec();
        want.sort();
        let mut got = keys.clone();
        got.sort();
        assert_eq!(got, want, "{r}");
        let mut rest = r.clone();
        rest.as_object_mut().unwrap().remove("id");
        assert!(!rest.to_string().contains("sentinel"), "a result carries input text or its id: {r}");
    }
    assert_eq!(results[0]["id"], SENTINEL_ID);
    assert_eq!(results[0]["text_sha256"], a_container_serve::profile::sha256_hex(SENTINEL_TEXT.as_bytes()));
    // Ready and bound: another cohort, no cohort or another package is refused.
    let other = "ab".repeat(32);
    for (pkg, cohort) in [(p.as_str(), Some("testnet-routes-2")), (p.as_str(), None), (other.as_str(), Some("testnet-routes-1"))] {
        let (code, v) = s.http("POST", "/v1/score", &score_body(pkg, cohort));
        assert_eq!((code, v["error"].as_str()), (409, Some("instance_bound")), "{v}");
    }
    let mut stripped = v.clone();
    for r in stripped["results"].as_array_mut().unwrap() {
        r.as_object_mut().unwrap().remove("id");
    }
    s.assert_no_sentinel(&[&stripped, &ready]);
    drop(s);

    // The local scorer's standard error summary is content-free too.
    let docs = work.join("docs.jsonl");
    let out = work.join("out.jsonl");
    std::fs::write(&docs, format!("{}\n", json!({"id": SENTINEL_ID, "text": SENTINEL_TEXT, "involved": true, "share": 0.5}))).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_slop_ninja_a_local_score"))
        .args(["--model-dir"])
        .arg(&r0)
        .arg("--docs")
        .arg(&docs)
        .arg("--out")
        .arg(&out)
        .args(["--spans", "--sessions", "1", "--ort-lib", &lib])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!err.contains("sentinel"), "the local summary carries input text or ids: {err}");
    let line: Value = serde_json::from_str(std::fs::read_to_string(&out).unwrap().trim()).unwrap();
    let mut rest = line.clone();
    rest.as_object_mut().unwrap().remove("id");
    assert!(!rest.to_string().contains("sentinel"), "{line}");
}
