//! Index of every registered prior snapshot: text hashes, 50-word
//! sequences, source-family keys and work aliases.
//!
//! Every JSON string of at least 20 whitespace words anywhere in a
//! registered file (record text, prompts, replies, nested provenance) is
//! indexed, so source text embedded in a rewrite prompt is protected too.
use crate::{io, text};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub const REGISTRY_SCHEMA: &str = "slop_ninja_a_pool_prior_registry_v1";
pub const INDEX_SCHEMA: &str = "slop_ninja_a_pool_protected_index_v1";
pub const METHOD: &str = "exact_whitespace_normalized_and_folded_text_hashes;whitespace_and_folded_50_word_sequences;family_keys;pg_ids;title_aliases_v1";

const FAMILY_KEYS: &[&str] = &[
    "source_group", "underlying_work_key", "family_key", "source_family", "family", "source_family_key",
    "parent_source_group", "work_key",
];
const TITLE_KEYS: &[&str] = &[
    "title", "source_title", "expected_title", "catalog_title", "item_title", "canonical_work_title",
];
const ALIAS_KEYS: &[&str] = &["work_aliases", "title_aliases", "underlying_work_aliases", "aliases"];
const KINDS: [&str; 5] = ["exact", "normalized", "folded", "shingles_ws", "shingles_folded"];

/// One registry entry: a file, or a directory walked for matching names.
#[derive(Debug, Clone)]
pub struct Entry {
    pub label: String,
    pub scope: String,
    pub path: PathBuf,
    pub names: Vec<String>,
    pub exclude_dirs: Vec<String>,
    pub exclude_names: Vec<String>,
    pub max_bytes: u64,
    pub required: bool,
}

/// Shell-style name glob: `*` matches any run of characters (including
/// none) anywhere in the pattern, `?` matches one character, and every other
/// character matches itself. `pg*.txt`, `*vectors*.jsonl` and `export-*` all
/// work; a pattern without wildcards is an exact name.
pub fn matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut i, mut j) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while j < n.len() {
        if i < p.len() && (p[i] == '?' || (p[i] != '*' && p[i] == n[j])) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some((i, j));
            i += 1;
        } else if let Some((si, sj)) = star {
            // Backtrack: let the last star absorb one more character.
            i = si + 1;
            j = sj + 1;
            star = Some((si, sj + 1));
        } else {
            return false;
        }
    }
    p[i..].iter().all(|&c| c == '*')
}

/// Matcher semantics recorded in every index built by this code.
pub const NAME_MATCHER: &str = "shell_glob_star_anywhere_and_question_mark_v2";

pub fn load_registry(path: &Path, root: &Path) -> Result<(Vec<Entry>, String)> {
    let bytes = fs::read(path).with_context(|| format!("registry {}", path.display()))?;
    let v: Value = serde_json::from_slice(&bytes)?;
    ensure!(v["schema"] == REGISTRY_SCHEMA, "registry schema");
    let strings = |e: &Value, k: &str| -> Vec<String> {
        e[k].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default()
    };
    let mut out = Vec::new();
    for e in v["entries"].as_array().context("registry entries")? {
        let p = e["path"].as_str().context("entry path")?;
        let path = if Path::new(p).is_absolute() { PathBuf::from(p) } else { root.join(p) };
        out.push(Entry {
            label: e["label"].as_str().context("entry label")?.to_owned(),
            scope: e["scope"].as_str().unwrap_or("prior").to_owned(),
            path,
            names: strings(e, "names"),
            exclude_dirs: strings(e, "exclude_dirs"),
            exclude_names: strings(e, "exclude_names"),
            max_bytes: e["max_bytes"].as_u64().unwrap_or(2 << 30),
            required: e["required"].as_bool().unwrap_or(true),
        });
    }
    Ok((out, text::sha_hex(&bytes)))
}

/// Files an entry selects, sorted for determinism.
pub fn entry_files(e: &Entry) -> Result<Vec<PathBuf>> {
    if !e.path.exists() {
        ensure!(!e.required, "required protected input missing: {}", e.path.display());
        return Ok(Vec::new());
    }
    if e.path.is_file() {
        return Ok(vec![e.path.clone()]);
    }
    let mut out = Vec::new();
    let mut stack = vec![e.path.clone()];
    while let Some(d) = stack.pop() {
        for x in fs::read_dir(&d)? {
            let x = x?;
            let name = x.file_name().to_string_lossy().into_owned();
            let ft = x.file_type()?;
            if ft.is_dir() {
                if !e.exclude_dirs.iter().any(|p| matches(p, &name)) {
                    stack.push(x.path());
                }
            } else if ft.is_file()
                && e.names.iter().any(|p| matches(p, &name))
                && !e.exclude_names.iter().any(|p| matches(p, &name))
            {
                out.push(x.path());
            }
        }
    }
    out.sort();
    Ok(out)
}

#[derive(Default)]
pub struct Builder {
    pub keys: [Vec<u64>; 5],
    pub families: BTreeSet<String>,
    pub pg_ids: BTreeSet<u64>,
    pub titles: BTreeSet<String>,
    pub strings_indexed: u64,
    compact_at: [usize; 5],
    probe: Option<Arc<Probe>>,
    /// Bounded-memory mode: a key vector reaching `spill_keys` is sorted,
    /// deduplicated and written to a sorted run file under `spill_dir`;
    /// runs are merged at the end. The key files are byte-identical to the
    /// in-memory build (sorted unique keys).
    spill_keys: usize,
    spill_dir: Option<PathBuf>,
    runs: [Vec<PathBuf>; 5],
    run_seq: u64,
    builder_id: usize,
}

/// Write sorted unique keys (little-endian) to `path` from an in-memory
/// sorted slice merged with sorted run files, dropping duplicates; returns
/// (sha256 of the bytes, keys written). Streams: memory is the read
/// buffers only.
fn merge_keys(path: &Path, mem: &[u64], runs: &[PathBuf]) -> Result<(String, usize)> {
    use sha2::{Digest, Sha256};
    use std::{cmp::Reverse, collections::BinaryHeap, io::Read};
    let mut readers: Vec<BufReader<fs::File>> = runs.iter().map(|r| fs::File::open(r).map(|f| BufReader::with_capacity(1 << 22, f))).collect::<std::io::Result<_>>()?;
    let next = |r: &mut BufReader<fs::File>| -> Result<Option<u64>> {
        let mut b = [0u8; 8];
        match r.read_exact(&mut b) {
            Ok(()) => Ok(Some(u64::from_le_bytes(b))),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e.into()),
        }
    };
    let mut heap = BinaryHeap::new();
    for (i, r) in readers.iter_mut().enumerate() {
        if let Some(k) = next(r)? {
            heap.push(Reverse((k, i)));
        }
    }
    let src_mem = readers.len();
    let mut mi = 0usize;
    if mi < mem.len() {
        heap.push(Reverse((mem[mi], src_mem)));
        mi += 1;
    }
    let f = fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    let mut w = std::io::BufWriter::with_capacity(1 << 22, f);
    let mut h = Sha256::new();
    let (mut last, mut n) = (None::<u64>, 0usize);
    while let Some(Reverse((k, i))) = heap.pop() {
        if last != Some(k) {
            let b = k.to_le_bytes();
            w.write_all(&b)?;
            h.update(b);
            last = Some(k);
            n += 1;
        }
        let nk = if i == src_mem {
            let x = mem.get(mi).copied();
            mi += 1;
            x
        } else {
            next(&mut readers[i])?
        };
        if let Some(k2) = nk {
            ensure!(k2 >= k, "unsorted run");
            heap.push(Reverse((k2, i)));
        }
    }
    let f = w.into_inner().map_err(|e| e.into_error())?;
    f.sync_all()?;
    Ok((hex::encode(h.finalize()), n))
}

/// The text keys of a fixed candidate set. A probe-filtered index stores
/// only registered keys that also occur here, so it answers
/// `text_conflicts` exactly as the full index would for those candidates
/// (and only for them) while its size follows the candidates, not the
/// registered snapshots.
pub struct Probe {
    keys: [Vec<u64>; 5],
    pub files: Vec<Value>,
    pub texts: u64,
}

impl Probe {
    pub fn from_texts<'a>(texts: impl IntoIterator<Item = &'a str>) -> Self {
        let mut keys: [Vec<u64>; 5] = Default::default();
        let mut n = 0;
        for t in texts {
            n += 1;
            let [a, b, c] = text::text_keys(t);
            keys[0].push(a);
            keys[1].push(b);
            keys[2].push(c);
            keys[3].extend(text::shingles_ws(t));
            keys[4].extend(text::shingles_folded(t));
        }
        for v in keys.iter_mut() {
            v.sort_unstable();
            v.dedup();
        }
        Probe { keys, files: Vec::new(), texts: n }
    }
    /// The `text` field of every row of `<dir>/candidates.jsonl`.
    pub fn from_candidate_dirs(dirs: &[PathBuf]) -> Result<Self> {
        let mut texts = Vec::new();
        let mut files = Vec::new();
        for d in dirs {
            let p = d.join("candidates.jsonl");
            let (sha, bytes) = io::file_sha(&p)?;
            let mut rows = 0u64;
            for r in io::jsonl_rows(&p)? {
                let r = r?;
                let t = r["text"].as_str().with_context(|| format!("{}: row without text", p.display()))?;
                texts.push(t.to_owned());
                rows += 1;
            }
            files.push(json!({"path": p, "sha256": sha, "bytes": bytes, "rows": rows}));
        }
        let mut pr = Probe::from_texts(texts.iter().map(String::as_str));
        pr.files = files;
        Ok(pr)
    }
    fn has(&self, kind: usize, k: u64) -> bool {
        self.keys[kind].binary_search(&k).is_ok()
    }
    pub fn key_counts(&self) -> Vec<usize> {
        self.keys.iter().map(Vec::len).collect()
    }
}

fn pg_from_url(s: &str) -> Option<u64> {
    let l = s.to_lowercase();
    if !l.contains("gutenberg.org") {
        return None;
    }
    for marker in ["/ebooks/", "/epub/", "/files/"] {
        if let Some(i) = l.find(marker) {
            let n: String = l[i + marker.len()..].chars().take_while(char::is_ascii_digit).collect();
            if let Ok(v) = n.parse() {
                return Some(v);
            }
        }
    }
    None
}

impl Builder {
    pub fn add_text(&mut self, s: &str) {
        if text::words(s) < text::MIN_INDEXED_WORDS {
            return;
        }
        self.strings_indexed += 1;
        let [a, b, c] = text::text_keys(s);
        if let Some(p) = self.probe.clone() {
            for (i, k) in [a, b, c].into_iter().enumerate() {
                if p.has(i, k) {
                    self.keys[i].push(k);
                }
            }
            self.keys[3].extend(text::shingles_ws(s).into_iter().filter(|&k| p.has(3, k)));
            self.keys[4].extend(text::shingles_folded(s).into_iter().filter(|&k| p.has(4, k)));
        } else {
            self.keys[0].push(a);
            self.keys[1].push(b);
            self.keys[2].push(c);
            self.keys[3].extend(text::shingles_ws(s));
            self.keys[4].extend(text::shingles_folded(s));
        }
        if self.spill_keys > 0 {
            self.spill_full();
            return;
        }
        // Snapshots repeat the same texts many times; compact as we go.
        for (v, at) in self.keys.iter_mut().zip(self.compact_at.iter_mut()) {
            if v.len() > (*at).max(1 << 26) {
                v.sort_unstable();
                v.dedup();
                *at = 2 * v.len();
            }
        }
    }
    /// Bounded-memory mode: sort, deduplicate and spill any key vector at
    /// `spill_keys`; keep at most eight runs per kind by merging them.
    fn spill_full(&mut self) {
        let lim = self.spill_keys;
        for i in 0..self.keys.len() {
            if self.keys[i].len() < lim {
                // Reserve the bound once, so the vector never doubles past it.
                let v = &mut self.keys[i];
                if v.len() > lim / 4 && v.capacity() < lim + (1 << 20) {
                    v.reserve_exact(lim + (1 << 20) - v.len());
                }
                continue;
            }
            self.keys[i].sort_unstable();
            self.keys[i].dedup();
            if self.keys[i].len() < lim / 2 {
                continue;
            }
            let dir = self.spill_dir.clone().expect("spill directory");
            self.run_seq += 1;
            let p = dir.join(format!("{}-b{}-{:06}.run", KINDS[i], self.builder_id, self.run_seq));
            merge_keys(&p, &self.keys[i], &[]).expect("write key run");
            self.keys[i].clear();
            self.runs[i].push(p);
            if self.runs[i].len() >= 8 {
                self.run_seq += 1;
                let m = dir.join(format!("{}-b{}-{:06}.run", KINDS[i], self.builder_id, self.run_seq));
                merge_keys(&m, &[], &self.runs[i]).expect("merge key runs");
                for r in self.runs[i].drain(..) {
                    let _ = fs::remove_file(r);
                }
                self.runs[i].push(m);
            }
        }
    }
    fn add_title(&mut self, t: &str) {
        for a in text::title_aliases(t) {
            self.titles.insert(a);
        }
    }
    fn add_alias(&mut self, a: &str) {
        if let Some(n) = a.strip_prefix("pg:").and_then(|n| n.parse().ok()) {
            self.pg_ids.insert(n);
        } else if a.starts_with("title:") {
            self.titles.insert(a.to_owned());
        } else if a.starts_with("author_work_v") {
            self.families.insert(a.to_owned());
        } else {
            self.add_title(a);
        }
    }
    pub fn visit(&mut self, v: &Value, key: Option<&str>) {
        match v {
            Value::String(s) => {
                self.add_text(s);
                if let Some(k) = key {
                    if FAMILY_KEYS.contains(&k) {
                        self.families.insert(s.clone());
                    } else if TITLE_KEYS.contains(&k) && text::words(s) <= 40 {
                        self.add_title(s);
                    } else if ALIAS_KEYS.contains(&k) {
                        self.add_alias(s);
                    } else if k == "pg_id" {
                        if let Ok(n) = s.parse() {
                            self.pg_ids.insert(n);
                        }
                    }
                }
                if let Some(n) = pg_from_url(s) {
                    self.pg_ids.insert(n);
                }
            }
            Value::Number(n) if key == Some("pg_id") => {
                if let Some(n) = n.as_u64() {
                    self.pg_ids.insert(n);
                }
            }
            Value::Array(a) => {
                for x in a {
                    self.visit(x, key);
                }
            }
            Value::Object(m) => {
                for (k, x) in m {
                    self.visit(x, Some(k));
                }
            }
            _ => {}
        }
    }
    fn file(&mut self, p: &Path, max_bytes: u64) -> Result<(u64, &'static str)> {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let base = name.trim_end_matches(".zst");
        if base.ends_with(".jsonl") {
            let r = BufReader::with_capacity(1 << 20, io::open(p)?);
            let mut rows = 0u64;
            let mut unparsed = false;
            for (i, line) in r.split(b'\n').enumerate() {
                let line = line.with_context(|| format!("{}:{}", p.display(), i + 1))?;
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                match serde_json::from_slice::<Value>(&line) {
                    Ok(v) => self.visit(&v, None),
                    // Never skip: an unparsable line is still protected as text.
                    Err(_) => {
                        self.add_text(&String::from_utf8_lossy(&line));
                        unparsed = true;
                    }
                }
                rows += 1;
            }
            Ok((rows, if unparsed { "jsonl_with_lines_indexed_as_text" } else { "jsonl" }))
        } else if base.ends_with(".json") {
            let b = io::read_all(p, max_bytes)?;
            match serde_json::from_slice::<Value>(&b) {
                Ok(v) => {
                    self.visit(&v, None);
                    Ok((1, "json"))
                }
                Err(_) => {
                    self.add_text(&String::from_utf8_lossy(&b));
                    Ok((1, "json_unparsed_indexed_as_text"))
                }
            }
        } else {
            // Plain text, for example a retained excerpt file.
            let b = io::read_all(p, max_bytes)?;
            self.add_text(&String::from_utf8_lossy(&b));
            Ok((1, "text"))
        }
    }
}

/// Read a key file in bounded extra memory, checking its SHA-256.
fn read_keys(path: &Path, want: &str) -> Result<Vec<u64>> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let len = fs::metadata(path)?.len();
    ensure!(len % 8 == 0, "key file length {len}");
    let mut out = Vec::with_capacity((len / 8) as usize);
    let mut r = BufReader::with_capacity(1 << 22, fs::File::open(path)?);
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 22];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let mut n = n;
        while n % 8 != 0 {
            let m = r.read(&mut buf[n..n + (8 - n % 8)])?;
            ensure!(m > 0, "truncated key file");
            n += m;
        }
        h.update(&buf[..n]);
        out.extend(buf[..n].chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().expect("8 bytes"))));
    }
    ensure!(hex::encode(h.finalize()) == want, "changed (sha256)");
    Ok(out)
}

fn write_keys(path: &Path, keys: &[u64]) -> Result<String> {
    let mut b = Vec::with_capacity(keys.len() * 8);
    for k in keys {
        b.extend_from_slice(&k.to_le_bytes());
    }
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    f.write_all(&b)?;
    f.sync_all()?;
    Ok(text::sha_hex(&b))
}

/// Build the index into a fresh directory.
pub fn build(registry: &Path, root: &Path, out: &Path) -> Result<Value> {
    build_with(registry, root, out, None, 1)
}

/// Build the index, optionally filtered to a probe's keys (see `Probe`),
/// scanning files with `threads` workers. Without a probe and with one
/// thread this is exactly `build`.
pub fn build_with(registry: &Path, root: &Path, out: &Path, probe: Option<Probe>, threads: usize) -> Result<Value> {
    build_bounded(registry, root, out, probe, threads, 0, None)
}

/// `build_with` in bounded memory when `spill_keys > 0`: key vectors spill
/// sorted runs to `spill_dir` (default `<out>.spill`, removed at the end);
/// the written index is byte-identical.
pub fn build_bounded(registry: &Path, root: &Path, out: &Path, probe: Option<Probe>, threads: usize, spill_keys: usize, spill_dir: Option<&Path>) -> Result<Value> {
    io::fresh_dir(out)?;
    let spill_dir: Option<PathBuf> = if spill_keys > 0 {
        ensure!(spill_keys >= 1 << 16, "spill_keys too small");
        let d = spill_dir.map(Path::to_owned).unwrap_or_else(|| out.with_extension("spill"));
        io::fresh_dir(&d)?;
        Some(d)
    } else {
        None
    };
    let (entries, registry_sha) = load_registry(registry, root)?;
    let probe = probe.map(Arc::new);
    let mut missing = Vec::new();
    let mut listed_vanished = Vec::new();
    let mut seen = BTreeSet::new();
    let mut work: Vec<(usize, PathBuf)> = Vec::new();
    for (ei, e) in entries.iter().enumerate() {
        let list = entry_files(e)?;
        if list.is_empty() {
            missing.push(json!({"label": e.label, "path": e.path, "required": e.required}));
        }
        for p in list {
            let canon = match fs::canonicalize(&p) {
                Ok(c) => c,
                Err(_) if !e.required => {
                    listed_vanished.push(json!({"label": e.label, "path": p, "vanished_during_scan": true}));
                    continue;
                }
                Err(err) => return Err(err).with_context(|| format!("protected input {}", p.display())),
            };
            if seen.insert(canon) {
                work.push((ei, p));
            }
        }
    }
    let next = Mutex::new(0usize);
    let slots: Mutex<Vec<Option<Result<Value>>>> = Mutex::new((0..work.len()).map(|_| None).collect());
    let builders: Mutex<Vec<Builder>> = Mutex::new(Vec::new());
    let builder_ids = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..threads.max(1) {
            sc.spawn(|| {
                let builder_id = builder_ids.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut b = Builder { probe: probe.clone(), spill_keys, spill_dir: spill_dir.clone(), builder_id, ..Default::default() };
                loop {
                    let i = {
                        let mut n = next.lock().unwrap();
                        let i = *n;
                        *n += 1;
                        i
                    };
                    if i >= work.len() {
                        break;
                    }
                    let (ei, p) = &work[i];
                    let e = &entries[*ei];
                    if !e.required && fs::metadata(p).is_err() {
                        slots.lock().unwrap()[i] = Some(Ok(json!({"label": e.label, "path": p, "vanished_during_scan": true})));
                        continue;
                    }
                    let r = (|| -> Result<Value> {
                        let (sha, bytes) = io::file_sha(p)?;
                        let before = b.strings_indexed;
                        let (records, format) = b.file(p, e.max_bytes).with_context(|| format!("protected input {}", p.display()))?;
                        Ok(json!({"label": e.label, "scope": e.scope, "path": p, "sha256": sha, "bytes": bytes,
                                  "format": format, "records": records, "strings_indexed": b.strings_indexed - before}))
                    })();
                    slots.lock().unwrap()[i] = Some(r);
                }
                builders.lock().unwrap().push(b);
            });
        }
    });
    let mut files = Vec::new();
    let mut vanished = listed_vanished;
    for s in slots.into_inner().unwrap() {
        let v = s.context("protected input not scanned")??;
        if v["vanished_during_scan"] == true {
            vanished.push(v);
        } else {
            files.push(v);
        }
    }
    let mut b = Builder::default();
    for w in builders.into_inner().unwrap() {
        for (k, v) in b.keys.iter_mut().zip(w.keys) {
            if k.is_empty() {
                *k = v;
            } else {
                k.extend(v);
            }
        }
        for (r, wr) in b.runs.iter_mut().zip(w.runs) {
            r.extend(wr);
        }
        b.families.extend(w.families);
        b.pg_ids.extend(w.pg_ids);
        b.titles.extend(w.titles);
        b.strings_indexed += w.strings_indexed;
    }
    let mut kinds = serde_json::Map::new();
    for (i, k) in KINDS.iter().enumerate() {
        let v = &mut b.keys[i];
        v.sort_unstable();
        v.dedup();
        if b.runs[i].is_empty() {
            let sha = write_keys(&out.join(format!("{k}.u64")), v)?;
            kinds.insert((*k).into(), json!({"keys": v.len(), "sha256": sha}));
        } else {
            let (sha, n) = merge_keys(&out.join(format!("{k}.u64")), v, &b.runs[i])?;
            kinds.insert((*k).into(), json!({"keys": n, "sha256": sha}));
            for r in &b.runs[i] {
                fs::remove_file(r)?;
            }
        }
        *v = Vec::new();
    }
    if let Some(d) = &spill_dir {
        fs::remove_dir(d).with_context(|| format!("spill directory {} not empty", d.display()))?;
    }
    let index = json!({
        "schema": INDEX_SCHEMA,
        "method": METHOD,
        "built_utc": io::now_utc(),
        "registry_sha256": registry_sha,
        "registry_path": registry,
        "name_matcher": NAME_MATCHER,
        "files": files,
        "files_scanned": files.len(),
        "entries_without_files": missing,
        "strings_indexed": b.strings_indexed,
        "key_files": kinds,
        "families": b.families,
        "pg_ids": b.pg_ids,
        "title_aliases": b.titles,
        "key_encoding": "first 8 bytes of SHA-256, big-endian value, stored little-endian, sorted unique",
        "limits": [
            "Only registered files are protected; unregistered copies elsewhere are not.",
            "64-bit key collisions can only exclude a candidate, never admit one.",
            "Title aliases are title-only, as in the stage-3 catalog inventory; they deliberately over-exclude.",
            "This is not a pretraining or bibliographic exposure audit."
        ],
    });
    let mut index = index;
    if !vanished.is_empty() {
        index["files_vanished_during_scan"] = json!(vanished);
    }
    if let Some(p) = &probe {
        index["probe_filter"] = json!({
            "rule": "only registered text keys that also occur in the probe candidates are stored; family keys, PG ids and title aliases are complete",
            "valid_for": "deduplicating exactly the probe candidates; not a general prior index",
            "probe_files": p.files, "probe_texts": p.texts, "probe_key_counts": p.key_counts(), "threads": threads,
        });
    }
    io::write_json(&out.join("index.json"), &index)?;
    Ok(index)
}

pub struct Index {
    keys: [Vec<u64>; 5],
    pub families: BTreeSet<String>,
    pub pg_ids: BTreeSet<u64>,
    pub titles: BTreeSet<String>,
    pub sha256: String,
}

impl Index {
    pub fn load(dir: &Path) -> Result<Self> {
        let raw = fs::read(dir.join("index.json"))?;
        let meta: Value = serde_json::from_slice(&raw)?;
        ensure!(meta["schema"] == INDEX_SCHEMA, "index schema");
        let mut keys: [Vec<u64>; 5] = Default::default();
        for (i, k) in KINDS.iter().enumerate() {
            keys[i] = read_keys(&dir.join(format!("{k}.u64")), meta["key_files"][*k]["sha256"].as_str().unwrap_or(""))
                .with_context(|| format!("index key file {k}"))?;
        }
        let strings = |k: &str| -> BTreeSet<String> {
            meta[k].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default()
        };
        Ok(Index {
            keys,
            families: strings("families"),
            pg_ids: meta["pg_ids"].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default(),
            titles: strings("title_aliases"),
            sha256: text::sha_hex(&raw),
        })
    }
    fn has(&self, kind: usize, k: u64) -> bool {
        self.keys[kind].binary_search(&k).is_ok()
    }
    /// Text conflicts of one candidate, as reason labels.
    pub fn text_conflicts(&self, s: &str) -> Vec<&'static str> {
        let mut out = Vec::new();
        let [a, b, c] = text::text_keys(s);
        if self.has(0, a) {
            out.push("matches_prior_exact_text");
        }
        if self.has(1, b) {
            out.push("matches_prior_whitespace_normalized_text");
        }
        if self.has(2, c) {
            out.push("matches_prior_folded_text");
        }
        if text::shingles_ws(s).into_iter().any(|k| self.has(3, k)) {
            out.push("shares_50_word_sequence_with_prior");
        }
        if text::shingles_folded(s).into_iter().any(|k| self.has(4, k)) {
            out.push("shares_folded_50_word_sequence_with_prior");
        }
        out
    }
    /// Family and alias conflicts of one candidate row.
    pub fn family_conflicts(&self, row: &Value) -> Vec<&'static str> {
        let mut out = Vec::new();
        if row["source_group"].as_str().is_some_and(|g| self.families.contains(g)) {
            out.push("family_key_in_prior_snapshot");
        }
        for a in row["aliases"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if a.strip_prefix("pg:").and_then(|n| n.parse().ok()).is_some_and(|n| self.pg_ids.contains(&n)) {
                out.push("pg_id_in_prior_snapshot");
            } else if a.starts_with("title:") && self.titles.contains(a) {
                out.push("title_alias_in_prior_snapshot");
            } else if a.starts_with("author_work_v") && self.families.contains(a) {
                out.push("family_key_in_prior_snapshot");
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    #[test]
    fn probe_filtered_index_answers_like_the_full_index_for_its_candidates() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let words = |seed: &str, n: usize| (0..n).map(|i| format!("{seed}{i}")).collect::<Vec<_>>().join(" ");
        let shared = words("shared", 60);
        let prior_rows = [json!({"text": format!("{} {}", words("pre", 30), shared)}), json!({"text": words("other", 80)}), json!({"source_group": "fam:1", "text": words("x", 25)})];
        std::fs::create_dir_all(root.join("snap")).unwrap();
        std::fs::write(root.join("snap/a.jsonl"), prior_rows.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
        std::fs::write(root.join("snap/b.jsonl"), json!({"text": words("third", 70)}).to_string()).unwrap();
        std::fs::create_dir_all(root.join("concurrent")).unwrap();
        std::os::unix::fs::symlink(root.join("gone.jsonl"), root.join("concurrent/dangling.jsonl")).unwrap();
        let reg = root.join("reg.json");
        std::fs::write(&reg, json!({"schema": REGISTRY_SCHEMA, "entries": [{"label": "s", "path": "snap", "names": ["*.jsonl"]},
            {"label": "c", "path": "concurrent", "names": ["*.jsonl"], "required": false}]}).to_string()).unwrap();
        let cand_hit = format!("{} {}", words("c", 20), shared);
        let cand_exact = words("third", 70);
        let cand_clean = words("fresh", 90);
        let full = build(&reg, root, &root.join("full")).unwrap();
        let probe = Probe::from_texts([cand_hit.as_str(), cand_exact.as_str(), cand_clean.as_str()]);
        let probed = build_with(&reg, root, &root.join("probed"), Some(probe), 3).unwrap();
        assert_eq!(full["files_scanned"], probed["files_scanned"]);
        assert_eq!(full["strings_indexed"], probed["strings_indexed"]);
        assert_eq!(full["families"], probed["families"]);
        assert!(probed["probe_filter"].is_object() && full.get("probe_filter").is_none());
        // A dangling link is not a listed file; it neither aborts nor counts as vanished.
        assert!(probed.get("files_vanished_during_scan").is_none());
        let (fi, pi) = (Index::load(&root.join("full")).unwrap(), Index::load(&root.join("probed")).unwrap());
        for c in [&cand_hit, &cand_exact, &cand_clean] {
            assert_eq!(fi.text_conflicts(c), pi.text_conflicts(c));
        }
        assert!(!pi.text_conflicts(&cand_hit).is_empty());
        assert!(pi.text_conflicts(&cand_exact).contains(&"matches_prior_exact_text"));
        assert!(pi.text_conflicts(&cand_clean).is_empty());
        assert!(probed["key_files"]["shingles_ws"]["keys"].as_u64().unwrap() < full["key_files"]["shingles_ws"]["keys"].as_u64().unwrap());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn prose(seed: &str, n: usize) -> String {
        (0..n).map(|i| format!("{seed}{i}")).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn bounded_build_writes_byte_identical_key_files() {
        let d = tempfile::tempdir().unwrap();
        let snap = d.path().join("snap");
        fs::create_dir_all(&snap).unwrap();
        // Many overlapping texts so runs hold duplicates across spills.
        let mut rows = String::new();
        for i in 0..2000 {
            rows.push_str(&format!("{}\n", json!({"text": format!("{} {}", prose("shared", 60), prose(&format!("u{}x", i % 1500), 80 + i % 50)), "source_group": format!("fam:{i}")})));
        }
        fs::write(snap.join("a.jsonl"), &rows).unwrap();
        fs::write(snap.join("b.jsonl"), &rows).unwrap();
        let reg = d.path().join("registry.json");
        fs::write(&reg, json!({"schema": REGISTRY_SCHEMA, "entries": [{"label": "s", "path": "snap", "names": ["*.jsonl"]}]}).to_string()).unwrap();
        let a = build_with(&reg, d.path(), &d.path().join("ix-mem"), None, 1).unwrap();
        let b = build_bounded(&reg, d.path(), &d.path().join("ix-spill"), None, 1, 1 << 16, None).unwrap();
        assert_eq!(a["key_files"], b["key_files"]);
        for k in KINDS {
            assert_eq!(fs::read(d.path().join(format!("ix-mem/{k}.u64"))).unwrap(), fs::read(d.path().join(format!("ix-spill/{k}.u64"))).unwrap());
        }
        assert!(!d.path().join("ix-spill.spill").exists());
        let c = build_bounded(&reg, d.path(), &d.path().join("ix-spill2"), None, 3, 1 << 16, Some(&d.path().join("sp"))).unwrap();
        assert_eq!(a["key_files"], c["key_files"]);
        // The streaming loader reads both back.
        let ia = Index::load(&d.path().join("ix-mem")).unwrap();
        let ib = Index::load(&d.path().join("ix-spill")).unwrap();
        assert_eq!(ia.keys, ib.keys);
        assert!(ia.keys[4].len() > 1 << 16, "fixture must exceed one spill run");
    }

    #[test]
    fn glob_matches_stars_anywhere() {
        for (p, n, want) in [
            ("pg*.txt", "pg1023.txt", true),
            ("pg*.txt", "pg.txt", true),
            ("pg*.txt", "pg1023.txt.bak", false),
            ("pg*.txt", "xpg1.txt", false),
            ("*vectors*.jsonl", "cfg-vectors-v1.jsonl", true),
            ("*vectors*.jsonl", "vectors.jsonl", true),
            ("*vectors*.jsonl", "cfg-vectors-v1.jsonl.zst", false),
            ("*vectors*.jsonl.zst", "a.vectors.b.jsonl.zst", true),
            ("*.jsonl", "rows.jsonl", true),
            ("*.jsonl", "rows.jsonl.zst", false),
            ("export-*", "export-v2", true),
            ("export-*", "export", false),
            ("*-target", "a-target", true),
            ("*", "anything", true),
            ("a*b*c", "aXbYc", true),
            ("a*b*c", "aXbY", false),
            ("a*a", "aaa", true),
            ("pg?.txt", "pg7.txt", true),
            ("pg?.txt", "pg77.txt", false),
            ("records.jsonl", "records.jsonl", true),
            ("records.jsonl", "records.jsonlx", false),
        ] {
            assert_eq!(matches(p, n), want, "{p} vs {n}");
        }
    }

    #[test]
    fn registry_middle_star_selects_raw_books() {
        let d = tempfile::tempdir().unwrap();
        let snap = d.path().join("snap/downloads");
        fs::create_dir_all(&snap).unwrap();
        let book = prose("b", 70);
        fs::write(snap.join("pg1023.txt"), &book).unwrap();
        fs::write(snap.join("pg1023.txt.headers"), "x").unwrap();
        fs::write(snap.join("notes.txt"), prose("n", 70)).unwrap();
        fs::write(snap.join("cfg-vectors-v1.jsonl"), format!("{}\n", json!({"text": prose("v", 60)}))).unwrap();
        fs::write(snap.join("rows.jsonl"), format!("{}\n", json!({"text": prose("r", 60)}))).unwrap();
        let reg = d.path().join("registry.json");
        fs::write(&reg, json!({"schema": REGISTRY_SCHEMA, "entries": [
            {"label": "s1", "path": "snap", "names": ["*.jsonl", "pg*.txt"], "exclude_names": ["*vectors*.jsonl"]}
        ]}).to_string()).unwrap();
        let out = d.path().join("index");
        let meta = build(&reg, d.path(), &out).unwrap();
        assert_eq!(meta["files_scanned"], 2, "{meta}");
        assert_eq!(meta["name_matcher"], NAME_MATCHER);
        let ix = Index::load(&out).unwrap();
        let excerpt = format!("Intro words here. {} More.", prose("b", 55));
        assert!(ix.text_conflicts(&excerpt).contains(&"shares_50_word_sequence_with_prior"));
        assert!(ix.text_conflicts(&prose("v", 60)).is_empty(), "excluded vectors file was indexed");
        assert!(ix.text_conflicts(&prose("n", 70)).is_empty(), "unregistered txt was indexed");
    }

    #[test]
    fn index_protects_nested_text_prompts_families_and_aliases() {
        let d = tempfile::tempdir().unwrap();
        let snap = d.path().join("snap");
        fs::create_dir_all(snap.join("skipme")).unwrap();
        let human = prose("h", 80);
        let rows = [
            json!({"text": human, "source_group": "wikipedia:page:5", "metadata": {"title": "The Old Book", "aliases": ["pg:77"]}}),
            json!({"generation": {"request": {"messages": [{"content": format!("Rewrite this: {}", prose("p", 60))}]}}, "source": {"source_url": "https://www.gutenberg.org/ebooks/99.txt.utf-8"}}),
        ];
        fs::write(snap.join("records.jsonl"), rows.iter().map(|r| r.to_string() + "\n").collect::<String>()).unwrap();
        let enc = zstd::encode_all(format!("{}\n", json!({"text": prose("z", 30), "pg_id": 12})).as_bytes(), 3).unwrap();
        fs::write(snap.join("other.jsonl.zst"), enc).unwrap();
        fs::write(snap.join("skipme/records.jsonl"), format!("{}\n", json!({"text": prose("s", 60)}))).unwrap();
        fs::write(snap.join("population.json"), json!({"rows": [{"text": prose("j", 25)}]}).to_string()).unwrap();
        let reg = d.path().join("registry.json");
        fs::write(&reg, json!({"schema": REGISTRY_SCHEMA, "entries": [
            {"label": "snap", "path": "snap", "names": ["*.jsonl", "*.jsonl.zst", "population.json"], "exclude_dirs": ["skipme"]},
            {"label": "optional", "path": "absent", "names": ["*.jsonl"], "required": false}
        ]}).to_string()).unwrap();
        let out = d.path().join("index");
        let meta = build(&reg, d.path(), &out).unwrap();
        assert_eq!(meta["files_scanned"], 3);
        let ix = Index::load(&out).unwrap();
        assert!(ix.families.contains("wikipedia:page:5"));
        assert!(ix.pg_ids.contains(&77) && ix.pg_ids.contains(&99) && ix.pg_ids.contains(&12));
        assert!(ix.titles.contains("title:oldbook"));
        assert_eq!(ix.text_conflicts(&human.replace(' ', "  ")), vec![
            "matches_prior_whitespace_normalized_text", "matches_prior_folded_text",
            "shares_50_word_sequence_with_prior", "shares_folded_50_word_sequence_with_prior"]);
        // Source text inside a prompt is protected by its 50-word sequences.
        let embedded = format!("New intro {} and more.", prose("p", 55));
        assert!(ix.text_conflicts(&embedded).contains(&"shares_50_word_sequence_with_prior"));
        // Case and punctuation changes are caught by folded sequences only.
        let shouted = prose("h", 80).to_uppercase().replace(' ', ", ");
        assert_eq!(ix.text_conflicts(&shouted), vec!["matches_prior_folded_text", "shares_folded_50_word_sequence_with_prior"]);
        assert!(ix.text_conflicts(&prose("s", 60)).is_empty(), "excluded directory was indexed");
        assert!(ix.text_conflicts(&prose("j", 25)).contains(&"matches_prior_exact_text"));
        let row = json!({"source_group": "author_work_v2:x:y", "aliases": ["pg:77", "title:newbook"]});
        assert_eq!(ix.family_conflicts(&row), vec!["pg_id_in_prior_snapshot"]);
        assert!(build(&reg, d.path(), &out).is_err(), "index directory must be fresh");
    }
}
