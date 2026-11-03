//! Pass 3: dedup against corpus-v1, host closure and within-pool dedup.
//!
//! **corpus-v1** (opened read-only, never written): every unique text is
//! read from its pack and digest-checked. A candidate is excluded when any
//! corpus text matches it by
//! 1. exact bytes, normalised-whitespace or folded hash;
//! 2. a shared folded 50-word window;
//! 3. a shared folded 12-word window (every window on both sides, no
//!    sampling); or
//! 4. MinHash over folded 5-word shingles (128 permutations, LSH 16 bands
//!    of 8 rows), verified at Jaccard >= 0.5.
//!
//! **Host closure.** Registered domains of corpus-v1 FineWeb records
//! (`fineweb_site:`, `fineweb_blog:` and the `fineweb_page` writer key)
//! whose texts sit in an evaluation role (test, CAL, dev, diagnostic,
//! reserved) exclude the candidate; domains seen in held or Train roles are
//! flagged and counted, not excluded.
//!
//! **Within the pool.** Candidates are visited in
//! `rank(seed, "cc_human_pool_order_v1", id)` order; the first is kept and
//! any later one sharing a whole-text hash, a 12-word window or a verified
//! MinHash match with a kept candidate is excluded. Excluded pairs join
//! their groups into one split component.
use crate::{
    candidates::{self, Candidate},
    funnel::Funnel,
    shingle::{self, MH_BANDS, MinHasher, Packed},
    store,
};
use anyhow::{Context, Result, ensure};
use grammar_a_pools::{psl::Psl, splits::rank, text};
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

pub const DEDUP_METHOD: &str = "corpus_v1_exact_normalized_folded_hash+folded_50w_window+folded_12w_window_full+minhash_5w_128p_16x8_jaccard_0.5;host_closure_eval_roles;pool_rank_order_same_screens_v1";
pub const POOL_SCOPE: &str = "cc_human_pool_order_v1";

const K_EXACT: u8 = 1;
const K_NORM: u8 = 2;
const K_FOLD: u8 = 4;
const K_W50: u8 = 8;
const K_W12: u8 = 16;
const K_MH: u8 = 32;

pub fn kind_names(k: u8) -> Vec<&'static str> {
    let mut v = Vec::new();
    for (b, n) in [
        (K_EXACT, "exact"),
        (K_NORM, "normalized"),
        (K_FOLD, "folded"),
        (K_W50, "shared_50w"),
        (K_W12, "shared_12w"),
        (K_MH, "minhash_j05"),
    ] {
        if k & b != 0 {
            v.push(n);
        }
    }
    v
}

/// The first (strongest) screen in the order the module lists them.
pub fn first_kind(k: u8) -> &'static str {
    kind_names(k).first().copied().unwrap_or("none")
}

struct Slim {
    stem: usize,
    id: String,
    text: String,
    registered_domain: String,
    group: String,
    domain: String,
    page_words: usize,
    words: usize,
    whole: [u64; 3],
    bands: [u64; MH_BANDS],
}

fn whole_keys(s: &str) -> [u64; 3] {
    text::text_keys(s)
}

struct Feat {
    w12: Vec<u64>,
    w50: Vec<u64>,
    sh5: Vec<u64>,
}

fn feats(s: &str) -> Feat {
    let t = shingle::token_hashes(s);
    Feat {
        w12: shingle::sorted_unique(shingle::windows(&t, shingle::W12)),
        w50: shingle::sorted_unique(shingle::windows(&t, shingle::W50)),
        sh5: shingle::sorted_unique(shingle::windows(&t, shingle::MH_SHINGLE)),
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CorpusHit {
    pub kinds: u8,
    /// First matching corpus text per screen: (text sha256, first_source, jaccard).
    pub evidence: BTreeMap<String, (String, String, f64)>,
}

pub struct CorpusText {
    sha: [u8; 32],
    source: u32,
    pack: i64,
    offset: u64,
    zlen: usize,
    len: usize,
}

pub fn open_ro(store_dir: &Path) -> Result<Connection> {
    let p = store_dir.join("corpus.sqlite");
    let c = Connection::open_with_flags(
        &p,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open {} read-only", p.display()))?;
    c.busy_timeout(std::time::Duration::from_secs(60))?;
    Ok(c)
}

pub fn corpus_texts(db: &Connection) -> Result<(Vec<CorpusText>, Vec<String>)> {
    let mut sources: Vec<String> = Vec::new();
    let mut sid: HashMap<String, u32> = HashMap::new();
    let mut out = Vec::new();
    let mut st = db.prepare(
        "SELECT t.text_sha256, t.first_source, b.pack_id, b.offset, b.zlen, b.len FROM text t JOIN blob b ON b.sha256 = unhex(t.text_sha256)",
    )?;
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        let src: String = r.get(1)?;
        let id = *sid.entry(src.clone()).or_insert_with(|| {
            sources.push(src);
            (sources.len() - 1) as u32
        });
        let hexsha: String = r.get(0)?;
        let mut sha = [0u8; 32];
        hex::decode_to_slice(&hexsha, &mut sha).with_context(|| format!("text sha {hexsha}"))?;
        out.push(CorpusText {
            sha,
            source: id,
            pack: r.get(2)?,
            offset: r.get::<_, i64>(3)? as u64,
            zlen: r.get::<_, i64>(4)? as usize,
            len: r.get::<_, i64>(5)? as usize,
        });
    }
    out.sort_by_key(|t| (t.pack, t.offset));
    Ok((out, sources))
}

/// Role slot of a corpus-v1 split name: 0 evaluation (test, CAL, dev,
/// diagnostic, reserved), 1 held, 2 Train (train, train_mining), 3 none.
pub fn role_slot(split: &str) -> usize {
    let s = split.to_lowercase();
    if ["test", "cal", "dev", "diagnostic", "reserved"].iter().any(|p| s.starts_with(p)) {
        0
    } else if s.starts_with("held") {
        1
    } else if s.starts_with("train") {
        2
    } else {
        3
    }
}

/// Registered domains of corpus-v1 FineWeb records (`fineweb_site:` and
/// `fineweb_blog:` groups, the `fineweb_page` writer key) with the number of
/// view memberships of their texts per split name. The query takes minutes,
/// so the result is cached in `cache` (domain, split, count rows) and keyed
/// by the store database's size and modification time.
pub fn site_roles(db: &Connection, psl: &Psl, store_dir: &Path, cache: &Path) -> Result<HashMap<String, BTreeMap<String, u64>>> {
    let meta = std::fs::metadata(store_dir.join("corpus.sqlite"))?;
    let stamp = format!("{}:{:?}", meta.len(), meta.modified().ok());
    if cache.exists() {
        let raw = zstd::stream::decode_all(File::open(cache)?)?;
        let t = String::from_utf8(raw)?;
        let mut lines = t.lines();
        if lines.next() == Some(stamp.as_str()) {
            let mut out: HashMap<String, BTreeMap<String, u64>> = HashMap::new();
            for l in lines {
                let f: Vec<&str> = l.split('\t').collect();
                ensure!(f.len() == 3, "bad site-roles cache line");
                out.entry(f[0].to_owned()).or_default().insert(f[1].to_owned(), f[2].parse()?);
            }
            return Ok(out);
        }
    }
    let mut out: HashMap<String, BTreeMap<String, u64>> = HashMap::new();
    let mut st = db.prepare(
        "SELECT r.source_group, r.writer_key, vm.split FROM record r INDEXED BY record_group \
         LEFT JOIN view_member vm ON vm.text_sha256 = r.text_sha256 \
         WHERE r.source_group >= 'fineweb_' AND r.source_group < 'fineweb`'",
    )?;
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        let sg: Option<String> = r.get(0)?;
        let wk: Option<String> = r.get(1)?;
        let split: Option<String> = r.get(2)?;
        let mut dom = None;
        for k in [sg.as_deref(), wk.as_deref()].into_iter().flatten() {
            if let Some(d) = k.strip_prefix("fineweb_site:") {
                dom = Some(d.to_owned());
            } else if let Some(h) = k.strip_prefix("fineweb_blog:") {
                dom = psl.registered_domain(h);
            }
            if dom.is_some() {
                break;
            }
        }
        let Some(d) = dom else { continue };
        *out.entry(d).or_default().entry(split.unwrap_or_default()).or_default() += 1;
    }
    let mut t = format!("{stamp}\n");
    let mut keys: Vec<&String> = out.keys().collect();
    keys.sort();
    for d in keys {
        for (s, n) in &out[d] {
            t.push_str(&format!("{d}\t{s}\t{n}\n"));
        }
    }
    if let Some(p) = cache.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(cache, zstd::bulk::compress(t.as_bytes(), 6)?)?;
    Ok(out)
}

/// Per-domain counts by role slot (see `role_slot`).
pub fn slots(m: &BTreeMap<String, u64>) -> [u64; 4] {
    let mut v = [0u64; 4];
    for (s, n) in m {
        v[role_slot(s)] += n;
    }
    v
}

fn read_text(packs: &Path, files: &Mutex<HashMap<i64, std::sync::Arc<File>>>, t: &CorpusText) -> Result<Vec<u8>> {
    let f = {
        let mut m = files.lock().unwrap();
        match m.get(&t.pack) {
            Some(f) => f.clone(),
            None => {
                let p = packs.join(format!("pack-{:06}.zst", t.pack));
                let f = std::sync::Arc::new(File::open(&p).with_context(|| format!("open {}", p.display()))?);
                m.insert(t.pack, f.clone());
                f
            }
        }
    };
    let mut z = vec![0u8; t.zlen];
    f.read_exact_at(&mut z, t.offset)?;
    let raw = zstd::bulk::decompress(&z, t.len)?;
    ensure!(
        raw.len() == t.len && text::sha_hex(&raw) == hex::encode(t.sha),
        "corpus blob {} fails its digest",
        hex::encode(t.sha)
    );
    Ok(raw)
}

pub struct Inputs {
    pub root: PathBuf,
    pub store: PathBuf,
    pub psl: Psl,
    pub pool_seed: String,
}

pub fn run(inp: &Inputs) -> Result<Value> {
    let t0 = Instant::now();
    let root = &inp.root;
    let stems: Vec<String> = store::files_with_suffix(&root.join("funnel"), ".candidates.json")?
        .iter()
        .map(|p| {
            let s = p.strip_prefix(root.join("funnel")).unwrap().to_string_lossy().into_owned();
            s.trim_end_matches(".candidates.json").to_owned()
        })
        .collect();
    ensure!(!stems.is_empty(), "no candidate files; run candidates first");
    // Load candidates.
    let mh = MinHasher::new();
    let mut slim: Vec<Slim> = Vec::new();
    for (si, st) in stems.iter().enumerate() {
        let rows: Vec<Candidate> = store::rows(&candidates::candidates_path(root, st))?;
        for c in rows {
            slim.push(Slim {
                stem: si,
                whole: whole_keys(&c.text),
                bands: [0; MH_BANDS],
                id: c.id,
                text: c.text,
                registered_domain: c.registered_domain,
                group: c.group,
                domain: c.domain,
                page_words: c.page_words,
                words: c.words,
            });
        }
    }
    ensure!(slim.len() as u64 <= shingle::ID_MASK, "too many candidates for packed ids");
    eprintln!("dedup: {} candidates from {} files", slim.len(), stems.len());
    // Candidate indexes, built in chunks so only one chunk of window
    // vectors is alive next to the packed array.
    let mut w12_keys: Vec<u64> = Vec::new();
    let mut band_keys = Vec::new();
    for start in (0..slim.len()).step_by(20_000) {
        let end = (start + 20_000).min(slim.len());
        let built: Vec<(Vec<u64>, [u64; MH_BANDS])> = slim[start..end]
            .par_iter()
            .enumerate()
            .map(|(k, c)| {
                let i = start + k;
                let f = feats(&c.text);
                let bands = mh.signature(&f.sh5).map(|s| MinHasher::bands(&s)).unwrap_or([0; MH_BANDS]);
                (f.w12.iter().map(|h| Packed::pack(*h, i as u32)).collect(), bands)
            })
            .collect();
        for (k, (keys, b)) in built.into_iter().enumerate() {
            let i = start + k;
            w12_keys.extend(keys);
            slim[i].bands = b;
            for (j, x) in b.iter().enumerate() {
                if *x != 0 {
                    band_keys.push(Packed::pack(shingle::splitmix(*x ^ j as u64), i as u32));
                }
            }
        }
    }
    w12_keys.shrink_to_fit();
    let w12 = Packed::build(w12_keys);
    let bands = Packed::build(band_keys);
    let mut whole_keys_v = Vec::new();
    for (i, c) in slim.iter().enumerate() {
        for k in c.whole {
            whole_keys_v.push(Packed::pack(k, i as u32));
        }
    }
    let whole = Packed::build(whole_keys_v);
    let t_index = t0.elapsed().as_secs_f64();
    eprintln!(
        "dedup: indexes built: {} w12 keys, {} band keys ({:.0}s)",
        w12.keys.len(),
        bands.keys.len(),
        t0.elapsed().as_secs_f64()
    );

    // Stream corpus-v1.
    let db = open_ro(&inp.store)?;
    let (texts, sources) = corpus_texts(&db)?;
    eprintln!("dedup: {} corpus texts listed ({:.0}s)", texts.len(), t0.elapsed().as_secs_f64());
    let packs = inp.store.join("packs");
    let files: Mutex<HashMap<i64, std::sync::Arc<File>>> = Mutex::new(HashMap::new());
    let hits: Mutex<HashMap<u32, CorpusHit>> = Mutex::new(HashMap::new());
    let done = AtomicU64::new(0);
    let corpus_bytes = AtomicU64::new(0);
    let corpus_windows = AtomicU64::new(0);
    let chunks: Vec<&[CorpusText]> = texts.chunks(2048).collect();
    let n_chunks = chunks.len();
    chunks.par_iter().try_for_each(|chunk| -> Result<()> {
        let mut local: Vec<(u32, u8, usize, f64)> = Vec::new();
        let mut ids = Vec::new();
        for (k, t) in chunk.iter().enumerate() {
            let raw = read_text(&packs, &files, t)?;
            corpus_bytes.fetch_add(raw.len() as u64, Ordering::Relaxed);
            let s = String::from_utf8_lossy(&raw);
            let wk = whole_keys(&s);
            for (j, key) in wk.iter().enumerate() {
                ids.clear();
                whole.ids(*key, &mut ids);
                for &c in &ids {
                    if slim[c as usize].whole[j] == *key {
                        local.push((c, [K_EXACT, K_NORM, K_FOLD][j], k, 1.0));
                    }
                }
            }
            let toks = shingle::token_hashes(&s);
            let win = shingle::windows(&toks, shingle::W12);
            corpus_windows.fetch_add(win.len() as u64, Ordering::Relaxed);
            let mut w12_cands: HashMap<u32, ()> = HashMap::new();
            let mut cand_cache: HashMap<u32, Feat> = HashMap::new();
            for h in &win {
                ids.clear();
                w12.ids(*h, &mut ids);
                for &c in &ids {
                    if w12_cands.contains_key(&c) {
                        continue;
                    }
                    let f = cand_cache.entry(c).or_insert_with(|| feats(&slim[c as usize].text));
                    if f.w12.binary_search(h).is_ok() {
                        w12_cands.insert(c, ());
                    }
                }
            }
            let mut corpus_feat: Option<Feat> = None;
            if !w12_cands.is_empty() {
                let cf = corpus_feat.get_or_insert_with(|| feats(&s));
                for c in w12_cands.keys() {
                    local.push((*c, K_W12, k, 1.0));
                    let f = cand_cache.entry(*c).or_insert_with(|| feats(&slim[*c as usize].text));
                    if shingle::intersects(&f.w50, &cf.w50) {
                        local.push((*c, K_W50, k, 1.0));
                    }
                }
            }
            let sh5 = match &corpus_feat {
                Some(f) => f.sh5.clone(),
                None => shingle::sorted_unique(shingle::windows(&toks, shingle::MH_SHINGLE)),
            };
            if let Some(sig) = mh.signature(&sh5) {
                let b = MinHasher::bands(&sig);
                let mut seen: HashSet<u32> = HashSet::new();
                for (j, x) in b.iter().enumerate() {
                    let key = shingle::splitmix(*x ^ j as u64);
                    ids.clear();
                    bands.ids(key, &mut ids);
                    for &c in &ids {
                        if slim[c as usize].bands[j] != *x || !seen.insert(c) {
                            continue;
                        }
                        let f = cand_cache.entry(c).or_insert_with(|| feats(&slim[c as usize].text));
                        let jac = shingle::jaccard(&f.sh5, &sh5);
                        if jac >= shingle::MH_VERIFY_JACCARD {
                            local.push((c, K_MH, k, jac));
                        }
                    }
                }
            }
        }
        if !local.is_empty() {
            let mut h = hits.lock().unwrap();
            for (c, kind, k, jac) in local {
                let e = h.entry(c).or_default();
                e.kinds |= kind;
                let t = &chunk[k];
                e.evidence
                    .entry(kind_names(kind)[0].to_owned())
                    .or_insert((hex::encode(t.sha), sources[t.source as usize].clone(), jac));
            }
        }
        let d = done.fetch_add(1, Ordering::Relaxed) + 1;
        if d.is_multiple_of(200) {
            eprintln!("dedup: corpus chunks {d}/{n_chunks} ({:.0}s)", t0.elapsed().as_secs_f64());
        }
        Ok(())
    })?;
    let corpus_hits = hits.into_inner().unwrap();
    let t_corpus = t0.elapsed().as_secs_f64();
    eprintln!(
        "dedup: corpus stream done, {} candidates hit ({:.0}s)",
        corpus_hits.len(),
        t0.elapsed().as_secs_f64()
    );

    // Host closure.
    let raw_roles = site_roles(&db, &inp.psl, &inp.store, &root.join("dedup/site-roles-v1.tsv.zst"))?;
    let roles: HashMap<String, [u64; 4]> = raw_roles.iter().map(|(d, m)| (d.clone(), slots(m))).collect();
    let t_roles = t0.elapsed().as_secs_f64();
    eprintln!(
        "dedup: {} corpus-v1 FineWeb registered domains with roles ({:.0}s)",
        roles.len(),
        t0.elapsed().as_secs_f64()
    );

    // Within-pool dedup over survivors of the corpus and host screens.
    let n = slim.len();
    let mut order: Vec<u32> = (0..n as u32).collect();
    order.sort_by_key(|&i| (rank(&inp.pool_seed, POOL_SCOPE, &slim[i as usize].id), i));
    let mut kept = vec![false; n];
    let mut pool_excl: HashMap<u32, (u32, u8, f64)> = HashMap::new();
    let mut host_excl: HashSet<u32> = HashSet::new();
    let mut host_train_flag: HashSet<u32> = HashSet::new();
    let mut host_held_flag: HashSet<u32> = HashSet::new();
    let mut host_excl_domains: HashMap<String, u64> = HashMap::new();
    let mut kept_whole: HashMap<u64, u32> = HashMap::new();
    let mut parent: HashMap<String, String> = HashMap::new();
    fn find(p: &mut HashMap<String, String>, x: &str) -> String {
        let mut cur = x.to_owned();
        while let Some(n) = p.get(&cur) {
            if *n == cur {
                break;
            }
            cur = n.clone();
        }
        cur
    }
    let mut ids = Vec::new();
    for &c in &order {
        let ci = c as usize;
        if corpus_hits.contains_key(&c) {
            continue;
        }
        let r = roles.get(&slim[ci].registered_domain);
        if r.is_some_and(|r| r[0] > 0) {
            host_excl.insert(c);
            *host_excl_domains.entry(slim[ci].registered_domain.clone()).or_default() += 1;
            continue;
        }
        if r.is_some_and(|r| r[1] > 0) {
            host_held_flag.insert(c);
        }
        if r.is_some_and(|r| r[2] > 0) {
            host_train_flag.insert(c);
        }
        let mut by: Option<(u32, u8, f64)> = None;
        for (j, key) in slim[ci].whole.iter().enumerate() {
            if let Some(&k) = kept_whole.get(key) {
                by = Some((k, [K_EXACT, K_NORM, K_FOLD][j], 1.0));
                break;
            }
        }
        let f = feats(&slim[ci].text);
        if by.is_none() {
            'w: for h in &f.w12 {
                ids.clear();
                w12.ids(*h, &mut ids);
                for &k in &ids {
                    if k != c && kept[k as usize] && feats(&slim[k as usize].text).w12.binary_search(h).is_ok() {
                        by = Some((k, K_W12, 1.0));
                        break 'w;
                    }
                }
            }
        }
        if by.is_none() {
            'b: for (j, x) in slim[ci].bands.iter().enumerate() {
                if *x == 0 {
                    continue;
                }
                ids.clear();
                bands.ids(shingle::splitmix(*x ^ j as u64), &mut ids);
                for &k in &ids {
                    if k != c && kept[k as usize] && slim[k as usize].bands[j] == *x {
                        let jac = shingle::jaccard(&f.sh5, &feats(&slim[k as usize].text).sh5);
                        if jac >= shingle::MH_VERIFY_JACCARD {
                            by = Some((k, K_MH, jac));
                            break 'b;
                        }
                    }
                }
            }
        }
        match by {
            Some(b) => {
                let (ga, gb) = (find(&mut parent, &slim[ci].group), find(&mut parent, &slim[b.0 as usize].group));
                if ga != gb {
                    parent.insert(ga, gb);
                }
                pool_excl.insert(c, b);
            }
            None => {
                kept[ci] = true;
                for key in slim[ci].whole {
                    kept_whole.entry(key).or_insert(c);
                }
            }
        }
    }
    let groups: HashSet<&str> = slim.iter().map(|c| c.group.as_str()).collect();
    let mut comp_size: HashMap<String, usize> = HashMap::new();
    for g in &groups {
        *comp_size.entry(find(&mut parent, g)).or_default() += 1;
    }
    let multi_group_components = comp_size.values().filter(|&&s| s > 1).count();
    let t_pool = t0.elapsed().as_secs_f64();
    eprintln!("dedup: pool dedup done ({:.0}s)", t0.elapsed().as_secs_f64());

    // Write annotated candidates and per-file funnels.
    let mut by_first: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut by_kind: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut by_source: BTreeMap<String, u64> = BTreeMap::new();
    let mut pool_reasons: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut idx = 0usize;
    let mut admitted = store::ZWriter::create(&root.join("pool/admitted.jsonl.zst"))?;
    for (si, st) in stems.iter().enumerate() {
        let rows: Vec<Candidate> = store::rows(&candidates::candidates_path(root, st))?;
        let mut out = store::ZWriter::create(&root.join("dedup").join(format!("{st}.jsonl.zst")))?;
        let mut f = Funnel::default();
        for c in rows {
            let i = idx as u32;
            idx += 1;
            debug_assert_eq!(slim[i as usize].stem, si);
            let s = &slim[i as usize];
            let ch = corpus_hits.get(&i);
            let mut outcome = json!({"corpus_v1": ch.map(|h| json!({"kinds": kind_names(h.kinds), "evidence": h.evidence}))});
            let stage_fail = if let Some(h) = ch {
                *by_first
                    .entry(first_kind(h.kinds).to_owned())
                    .or_default()
                    .entry(s.domain.clone())
                    .or_default() += 1;
                for k in kind_names(h.kinds) {
                    *by_kind.entry(k.to_owned()).or_default().entry(s.domain.clone()).or_default() += 1;
                }
                if let Some((_, src, _)) = h.evidence.values().next() {
                    *by_source.entry(src.clone()).or_default() += 1;
                }
                f.reject("corpus_v1_dedup", first_kind(h.kinds), &s.domain);
                Some("corpus_v1_dedup")
            } else if host_excl.contains(&i) {
                f.pass("corpus_v1_dedup", &s.domain, s.page_words, s.words);
                f.reject("host_closure", "registered_domain_in_corpus_v1_evaluation_role", &s.domain);
                outcome["host_closure"] = json!("excluded_evaluation_role_domain");
                Some("host_closure")
            } else if let Some((k, kind, jac)) = pool_excl.get(&i) {
                f.pass("corpus_v1_dedup", &s.domain, s.page_words, s.words);
                f.pass("host_closure", &s.domain, s.page_words, s.words);
                let kn = first_kind(*kind);
                f.reject("pool_dedup", kn, &s.domain);
                *pool_reasons.entry(kn.to_owned()).or_default().entry(s.domain.clone()).or_default() += 1;
                outcome["pool"] = json!({"excluded_by": slim[*k as usize].id, "screen": kn, "jaccard": jac});
                Some("pool_dedup")
            } else {
                for stg in ["corpus_v1_dedup", "host_closure", "pool_dedup"] {
                    f.pass(stg, &s.domain, s.page_words, s.words);
                }
                None
            };
            outcome["host_train_role_domain"] = json!(host_train_flag.contains(&i));
            outcome["host_held_role_domain"] = json!(host_held_flag.contains(&i));
            outcome["admitted"] = json!(stage_fail.is_none());
            let mut v = serde_json::to_value(&c)?;
            v["dedup"] = outcome;
            if stage_fail.is_none() {
                admitted.write(&v)?;
            }
            out.write(&v)?;
        }
        out.finish()?;
        store::write_json(
            &root.join("funnel").join(format!("{st}.dedup.json")),
            &json!({"schema": "slop_ninja_cc_human_dedup_funnel_v1", "stem": st, "funnel": f}),
        )?;
    }
    let (adm_sha, adm_rows) = admitted.finish()?;
    let train_flag_domains: usize = roles.values().filter(|r| r[0] == 0 && r[2] > 0).count();
    let mut top_excl: Vec<(&String, &u64)> = host_excl_domains.iter().collect();
    top_excl.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let excl_sizes: Vec<u64> = top_excl.iter().map(|(_, n)| **n).collect();
    let report = json!({
        "schema": "slop_ninja_cc_human_dedup_report_v1", "method": DEDUP_METHOD, "pool_seed": inp.pool_seed,
        "corpus_store": inp.store, "corpus_texts": texts.len(), "corpus_bytes_read": corpus_bytes.load(Ordering::Relaxed),
        "corpus_12w_windows": corpus_windows.load(Ordering::Relaxed), "candidates": n,
        "candidate_12w_keys": w12.keys.len(),
        "corpus_v1_excluded": corpus_hits.len(), "corpus_v1_first_screen_by_domain": by_first,
        "corpus_v1_every_screen_by_domain": by_kind, "corpus_v1_first_evidence_source": by_source,
        "host_closure": {"corpus_v1_fineweb_domains": roles.len(), "evaluation_role_domains": roles.values().filter(|r| r[0] > 0).count(),
                         "held_role_domains": roles.values().filter(|r| r[0] == 0 && r[1] > 0).count(),
                         "train_only_domains": train_flag_domains, "excluded_candidates": host_excl.len(),
                         "excluded_domains": host_excl_domains.len(),
                         "excluded_candidates_per_domain_desc": excl_sizes,
                         "flagged_held_role_candidates": host_held_flag.len(),
                         "flagged_train_role_candidates": host_train_flag.len(),
                         "role_slots": "evaluation = test*, cal*, dev*, diagnostic*, reserved*; held = held*; train = train*"},
        "pool_dedup": {"excluded": pool_excl.len(), "by_screen_and_domain": pool_reasons, "kept": kept.iter().filter(|k| **k).count(),
                       "groups": groups.len(), "multi_group_components": multi_group_components},
        "admitted": adm_rows, "admitted_sha256": adm_sha, "seconds": t0.elapsed().as_secs_f64(),
        "phase_seconds": {"load_and_index": t_index, "corpus_stream": t_corpus - t_index, "host_roles_single_thread": t_roles - t_corpus,
                          "pool_single_thread": t_pool - t_roles, "write": t0.elapsed().as_secs_f64() - t_pool},
    });
    store::write_json(&root.join("dedup/report.json"), &report)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidates::Candidate;

    fn cand(id: &str, domain: &str, text: &str) -> Candidate {
        Candidate {
            schema: candidates::CANDIDATE_SCHEMA.into(),
            id: id.into(),
            crawl: "CC-MAIN-2019-04".into(),
            wet_path: "crawl-data/CC-MAIN-2019-04/x.warc.wet.gz".into(),
            wet_sha256: String::new(),
            member_offset: 0,
            member_length: 0,
            record_id: format!("<urn:uuid:{id}>"),
            refers_to: None,
            warc_date: "2019-01-20T00:00:00Z".into(),
            url: format!("https://www.{domain}/p"),
            host: format!("www.{domain}"),
            registered_domain: domain.into(),
            block_digest: None,
            cld2: None,
            page_sha256: String::new(),
            page_words: 400,
            domain: "general_web".into(),
            domain_rule: String::new(),
            cues: Default::default(),
            page_domain: None,
            page_domain_rule: None,
            group: format!("cc_site:{domain}"),
            group_rule: "registered_domain".into(),
            era: "E1".into(),
            line_start: 0,
            line_end: 1,
            valid_runs: 1,
            run_rank: String::new(),
            words: text::words(text),
            chars: text.chars().count(),
            text: text.into(),
            text_sha256: text::sha_hex(text.as_bytes()),
            normalized_sha256: String::new(),
            folded_sha256: String::new(),
            text_redacted: None,
            redacted_emails: 0,
            redacted_phones: 0,
            lang: json!({}),
            rights_basis: String::new(),
            rights_class: String::new(),
        }
    }

    fn words(seed: u64, n: usize) -> String {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = shingle::splitmix(x);
                format!("w{}", x % 50_000)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn corpus_pool_and_host_screens() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join("store");
        std::fs::create_dir_all(store_dir.join("packs")).unwrap();
        let corpus_a = words(1, 200); // exact copy of candidate a
        let corpus_b = format!("{} {}", words(2, 30), words(3, 100)); // shares 30 words with c
        let mut pack = Vec::new();
        let db = Connection::open(store_dir.join("corpus.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE text (text_sha256 TEXT PRIMARY KEY, first_source TEXT NOT NULL);
             CREATE TABLE blob (sha256 BLOB PRIMARY KEY, pack_id INTEGER, offset INTEGER, zlen INTEGER, len INTEGER);
             CREATE TABLE record (source_group TEXT, writer_key TEXT, text_sha256 TEXT);
             CREATE INDEX record_group ON record(source_group);
             CREATE TABLE view_member (text_sha256 TEXT, split TEXT);",
        )
        .unwrap();
        for (t, src) in [(&corpus_a, "src_a"), (&corpus_b, "src_b")] {
            let z = zstd::bulk::compress(t.as_bytes(), 3).unwrap();
            let sha = sha2::Sha256::digest(t.as_bytes());
            db.execute("INSERT INTO text VALUES (?1, ?2)", rusqlite::params![hex::encode(sha), src])
                .unwrap();
            db.execute(
                "INSERT INTO blob VALUES (?1, 1, ?2, ?3, ?4)",
                rusqlite::params![&sha[..], pack.len() as i64, z.len() as i64, t.len() as i64],
            )
            .unwrap();
            pack.extend_from_slice(&z);
        }
        db.execute("INSERT INTO record VALUES ('fineweb_site:evalsite.com', NULL, 'e1')", [])
            .unwrap();
        db.execute("INSERT INTO view_member VALUES ('e1', 'test_consumed')", []).unwrap();
        db.execute("INSERT INTO record VALUES ('fineweb_page:x', 'fineweb_site:trainsite.com', 't1')", [])
            .unwrap();
        db.execute("INSERT INTO view_member VALUES ('t1', 'train')", []).unwrap();
        drop(db);
        std::fs::write(store_dir.join("packs/pack-000001.zst"), &pack).unwrap();
        use sha2::Digest;
        let root = dir.path().join("root");
        let st = "CC-MAIN-2019-04/x";
        let shared = words(9, 200);
        let rows = vec![
            cand("a", "one.com", &corpus_a),
            cand("c", "two.com", &format!("{} {}", words(2, 30), words(4, 150))),
            cand("d", "evalsite.com", &words(5, 200)),
            cand("e", "trainsite.com", &words(6, 200)),
            cand("f", "three.com", &shared),
            cand("g", "four.com", &shared),
            cand("h", "five.com", &words(7, 200)),
        ];
        let mut w = store::ZWriter::create(&candidates::candidates_path(&root, st)).unwrap();
        for r in &rows {
            w.write(r).unwrap();
        }
        w.finish().unwrap();
        store::write_json(&candidates::funnel_path(&root, st), &json!({})).unwrap();
        let list = "// c\ncom\n";
        let rep = run(&Inputs {
            root: root.clone(),
            store: store_dir.clone(),
            psl: Psl::parse(list),
            pool_seed: "t".into(),
        })
        .unwrap();
        assert!(root.join("dedup/site-roles-v1.tsv.zst").exists());
        let db = open_ro(&store_dir).unwrap();
        let cached = site_roles(&db, &Psl::parse(list), &store_dir, &root.join("dedup/site-roles-v1.tsv.zst")).unwrap();
        assert_eq!(slots(&cached["evalsite.com"]), [1, 0, 0, 0]);
        assert_eq!(role_slot("held"), 1);
        assert_eq!(rep["corpus_v1_excluded"], 2, "{rep:#}");
        assert_eq!(rep["host_closure"]["excluded_candidates"], 1);
        assert_eq!(rep["host_closure"]["flagged_train_role_candidates"], 1);
        assert_eq!(rep["pool_dedup"]["excluded"], 1);
        assert_eq!(rep["admitted"], 3);
        let out: Vec<Value> = store::rows(&root.join("dedup").join(format!("{st}.jsonl.zst"))).unwrap();
        let by: HashMap<&str, &Value> = out.iter().map(|v| (v["id"].as_str().unwrap(), &v["dedup"])).collect();
        assert_eq!(by["a"]["corpus_v1"]["kinds"][0], "exact");
        assert!(by["c"]["corpus_v1"]["kinds"].as_array().unwrap().iter().any(|k| k == "shared_12w"));
        assert!(!by["c"]["corpus_v1"]["kinds"].as_array().unwrap().iter().any(|k| k == "shared_50w"));
        assert_eq!(by["d"]["host_closure"], "excluded_evaluation_role_domain");
        assert_eq!(by["e"]["admitted"], true);
        assert_eq!(by["e"]["host_train_role_domain"], true);
        assert_eq!(by["h"]["admitted"], true);
        assert!(by["f"]["admitted"] != by["g"]["admitted"]);
    }
}
