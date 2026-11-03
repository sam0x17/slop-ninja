//! Private, immutable, checksum-verified research snapshots. No source deletion.
mod remote;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};

const SCHEMA: &str = "slop_ninja_private_artifact_snapshot_v1";
const CHUNK: usize = 64 * 1024 * 1024;
const MANIFEST_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    roots: Vec<Source>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    name: String,
    path: PathBuf,
    kind: String,
    /// Paths relative to a `tree` root that are skipped, with their subtrees.
    /// Every entry must match an existing path, so a typo cannot silently widen a snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    exclude: Vec<String>,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
struct Blob {
    sha256: String,
    bytes: u64,
    raw_sha256: String,
    raw_bytes: u64,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: String,
    offset: u64,
    bytes: u64,
    sha256: String,
    executable: bool,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: String,
    chunk_bytes: usize,
    codec: String,
    total_bytes: u64,
    files: Vec<Entry>,
    blobs: Vec<Blob>,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn hash_file(path: &Path) -> Result<String> {
    let mut h = Sha256::new();
    std::io::copy(&mut File::open(path)?, &mut h)?;
    Ok(format!("{:x}", h.finalize()))
}
fn safe_hash(s: &str) -> Result<()> {
    ensure!(
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid SHA256"
    );
    Ok(())
}
fn relative(s: &str) -> Result<()> {
    ensure!(
        !s.is_empty()
            && !s.contains(['\\', '\0', ':'])
            && !s.split('/').any(|x| x.is_empty() || x == "." || x == ".."),
        "invalid relative path"
    );
    ensure!(
        Path::new(s)
            .components()
            .all(|c| matches!(c, Component::Normal(_))),
        "invalid relative path"
    );
    Ok(())
}
fn no_symlinks(path: &Path) -> Result<()> {
    for part in path.ancestors() {
        if part.as_os_str().is_empty() {
            continue;
        }
        ensure!(
            !fs::symlink_metadata(part)
                .with_context(|| format!("inspect {}", part.display()))?
                .file_type()
                .is_symlink(),
            "symlink rejected: {}",
            part.display()
        );
    }
    Ok(())
}
fn private_dir(path: &Path) -> Result<()> {
    let mut b = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(path)?;
    Ok(())
}
fn new_file(path: &Path) -> Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    Ok(o.open(path)?)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = new_file(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    File::open(path.parent().context("parent")?)?.sync_all()?;
    Ok(())
}
fn read_manifest(path: &Path) -> Result<(Manifest, Vec<u8>)> {
    no_symlinks(path)?;
    ensure!(
        fs::metadata(path)?.len() <= MANIFEST_LIMIT,
        "manifest too large"
    );
    let bytes = fs::read(path)?;
    let m: Manifest = serde_json::from_slice(&bytes)?;
    validate(&m)?;
    Ok((m, bytes))
}
fn validate(m: &Manifest) -> Result<()> {
    ensure!(
        m.schema == SCHEMA && m.chunk_bytes == CHUNK && m.codec == "zstd3",
        "unsupported snapshot schema"
    );
    ensure!(!m.files.is_empty(), "empty snapshot");
    let mut paths = BTreeSet::new();
    let mut end = 0u64;
    for e in &m.files {
        relative(&e.path)?;
        safe_hash(&e.sha256)?;
        ensure!(paths.insert(e.path.as_str()), "duplicate file");
        ensure!(e.offset == end, "file ranges must be contiguous");
        end = end.checked_add(e.bytes).context("file size overflow")?;
    }
    for p in &paths {
        for a in Path::new(p).ancestors().skip(1) {
            ensure!(
                !paths.contains(a.to_str().context("UTF-8 path")?),
                "file is an ancestor of another file"
            );
        }
    }
    ensure!(end == m.total_bytes, "file total differs");
    end = 0;
    for (i, b) in m.blobs.iter().enumerate() {
        safe_hash(&b.sha256)?;
        safe_hash(&b.raw_sha256)?;
        ensure!(
            b.bytes > 0 && b.bytes <= (2 * CHUNK) as u64,
            "invalid stored chunk size"
        );
        ensure!(
            b.raw_bytes > 0 && b.raw_bytes <= CHUNK as u64,
            "invalid decoded chunk size"
        );
        ensure!(
            i + 1 == m.blobs.len() || b.raw_bytes == CHUNK as u64,
            "short interior chunk"
        );
        end = end.checked_add(b.raw_bytes).context("blob size overflow")?;
    }
    ensure!(end == m.total_bytes, "blob total differs");
    Ok(())
}

struct Input {
    source: PathBuf,
    expected: Option<String>,
}
fn add(
    inputs: &mut BTreeMap<String, Input>,
    name: String,
    path: PathBuf,
    expected: Option<String>,
) -> Result<()> {
    relative(&name)?;
    no_symlinks(&path)?;
    ensure!(
        fs::metadata(&path)?.is_file(),
        "regular file required: {}",
        path.display()
    );
    if let Some(x) = &expected {
        safe_hash(x)?;
    }
    if let Some(old) = inputs.get(&name) {
        ensure!(
            old.source == path && old.expected == expected,
            "conflicting source path"
        );
    } else {
        inputs.insert(
            name,
            Input {
                source: path,
                expected,
            },
        );
    }
    Ok(())
}
fn tree(
    inputs: &mut BTreeMap<String, Input>,
    prefix: &str,
    path: &Path,
    rel: &str,
    exclude: &BTreeSet<String>,
    hits: &mut BTreeSet<String>,
) -> Result<()> {
    no_symlinks(path)?;
    ensure!(fs::metadata(path)?.is_dir(), "tree directory required");
    for e in fs::read_dir(path)? {
        let e = e?;
        let file = e.file_name();
        let file = file.to_str().context("UTF-8 path")?;
        let r = if rel.is_empty() {
            file.to_string()
        } else {
            format!("{rel}/{file}")
        };
        // Excluded entries are never opened or followed.
        if exclude.contains(&r) {
            hits.insert(r);
            continue;
        }
        let name = format!("{prefix}/{file}");
        let ty = e.file_type()?;
        ensure!(!ty.is_symlink(), "symlink rejected: {}", e.path().display());
        if ty.is_dir() {
            tree(inputs, &name, &e.path(), &r, exclude, hits)?;
        } else {
            add(inputs, name, e.path(), None)?;
        }
    }
    Ok(())
}
fn check_roots(roots: &[Source]) -> Result<()> {
    let mut names = BTreeSet::new();
    for s in roots {
        relative(&s.name)?;
        ensure!(names.insert(s.name.as_str()), "duplicate root name");
        // The CFG staging copy is named after its root.
        ensure!(
            s.kind != "cfg_archive" || !s.name.contains('/'),
            "nested root names are allowed only for tree and file roots"
        );
        ensure!(
            s.exclude.is_empty() || s.kind == "tree",
            "exclude is allowed only for tree roots"
        );
        let mut seen = BTreeSet::new();
        for x in &s.exclude {
            relative(x)?;
            ensure!(seen.insert(x), "duplicate exclude path");
        }
    }
    // A root nested inside another root's logical path would restore as a file/directory clash.
    for a in &names {
        for b in &names {
            ensure!(
                !b.starts_with(&format!("{a}/")),
                "root name {b} is nested inside root name {a}"
            );
        }
    }
    Ok(())
}
fn cfg_inputs(inputs: &mut BTreeMap<String, Input>, root: &Source, staging: &Path) -> Result<()> {
    let format: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path.join("format.json"))?)?;
    ensure!(
        format["schema"] == "slop_ninja_cfg_parse_archive_v1",
        "unsupported CFG archive schema"
    );
    let src_db = root.path.join("index.sqlite");
    no_symlinks(&src_db)?;
    let db = Connection::open_with_flags(&src_db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_secs(10))?;
    let copy = staging.join(format!("{}.sqlite", root.name));
    let mut snapshot = Connection::open(&copy)?;
    {
        let backup = rusqlite::backup::Backup::new(&db, &mut snapshot)?;
        backup.run_to_completion(256, Duration::from_millis(10), None)?;
    }
    ensure!(
        snapshot.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))? == "ok",
        "SQLite snapshot integrity failure"
    );
    // A standalone database, never a bare copy of a live WAL-mode source.
    snapshot.execute_batch("PRAGMA journal_mode=DELETE;")?;
    let rows = snapshot.prepare("SELECT windows_sha256,compressed_sha256,record_sha256 FROM parses ORDER BY text_sha256")?
        .query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let n = rows.len();
    for (window, compressed, record) in rows {
        safe_hash(&window)?;
        safe_hash(&record)?;
        let p = format!("parses/{window}.cfg.zst");
        add(
            inputs,
            format!("{}/{p}", root.name),
            root.path.join(p),
            Some(compressed),
        )?;
        let p = format!("records/{record}.json.zst");
        let source = root.path.join(&p);
        no_symlinks(&source)?;
        let decoder = zstd::stream::read::Decoder::new(File::open(&source)?)?;
        let mut raw_hash = Sha256::new();
        let n = std::io::copy(&mut decoder.take(CHUNK as u64 + 1), &mut raw_hash)?;
        ensure!(
            n <= CHUNK as u64 && format!("{:x}", raw_hash.finalize()) == record,
            "CFG record hash/size mismatch"
        );
        add(
            inputs,
            format!("{}/{p}", root.name),
            root.path.join(p),
            None,
        )?;
    }
    let rows = snapshot
        .prepare("SELECT DISTINCT raw_sha256,compressed_sha256 FROM files ORDER BY raw_sha256")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (raw, compressed) in rows {
        safe_hash(&raw)?;
        let p = format!("files/{raw}.zst");
        add(
            inputs,
            format!("{}/{p}", root.name),
            root.path.join(p),
            Some(compressed),
        )?;
    }
    drop(snapshot);
    add(inputs, format!("{}/index.sqlite", root.name), copy, None)?;
    for p in ["format.json", "profile.json"] {
        add(
            inputs,
            format!("{}/{p}", root.name),
            root.path.join(p),
            None,
        )?;
    }
    eprintln!("consistent CFG snapshot: {n} document bindings");
    Ok(())
}
fn flush_blob(out: &Path, buf: &mut Vec<u8>, blobs: &mut Vec<Blob>) -> Result<()> {
    if buf.is_empty() {
        return Ok(());
    }
    let raw_sha256 = digest(buf);
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3)?;
    encoder.include_checksum(true)?;
    encoder.set_pledged_src_size(Some(buf.len() as u64))?;
    encoder.write_all(buf)?;
    let compressed = encoder.finish()?;
    let sha256 = digest(&compressed);
    let p = out.join("blobs").join(&sha256);
    if p.exists() {
        ensure!(hash_file(&p)? == sha256, "local chunk corruption");
    } else {
        write_new(&p, &compressed)?;
    }
    blobs.push(Blob {
        sha256,
        bytes: compressed.len() as u64,
        raw_sha256,
        raw_bytes: buf.len() as u64,
    });
    buf.clear();
    Ok(())
}
fn prepare(spec: &Path, out: &Path) -> Result<()> {
    let spec: Spec = serde_json::from_slice(&fs::read(spec)?)?;
    ensure!(!spec.roots.is_empty(), "no sources");
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    no_symlinks(parent)?;
    // Refuse recursive snapshots or any overlap between the output and source trees.
    let output_abs = parent
        .canonicalize()?
        .join(out.file_name().context("snapshot name")?);
    check_roots(&spec.roots)?;
    for s in &spec.roots {
        no_symlinks(&s.path)?;
        let p = s.path.canonicalize()?;
        ensure!(
            !output_abs.starts_with(&p) && !p.starts_with(&output_abs),
            "source overlaps snapshot"
        );
    }
    private_dir(out)?;
    private_dir(&out.join("blobs"))?;
    let staging = tempfile::tempdir_in(out)?;
    let mut inputs = BTreeMap::new();
    for root in &spec.roots {
        match root.kind.as_str() {
            "tree" => {
                let exclude: BTreeSet<String> = root.exclude.iter().cloned().collect();
                let mut hits = BTreeSet::new();
                tree(&mut inputs, &root.name, &root.path, "", &exclude, &mut hits)?;
                if let Some(x) = exclude.difference(&hits).next() {
                    bail!("exclude path not found under {}: {x}", root.name);
                }
            }
            "file" => add(&mut inputs, root.name.clone(), root.path.clone(), None)?,
            "cfg_archive" => cfg_inputs(&mut inputs, root, staging.path())?,
            _ => bail!("unsupported source kind"),
        }
    }
    let mut m = Manifest {
        schema: SCHEMA.into(),
        chunk_bytes: CHUNK,
        codec: "zstd3".into(),
        total_bytes: 0,
        files: vec![],
        blobs: vec![],
    };
    let mut buf = Vec::with_capacity(CHUNK);
    let mut block = vec![0u8; 1024 * 1024];
    for (name, input) in inputs {
        let before = fs::metadata(&input.source)?;
        let mut f = File::open(&input.source)?;
        let mut h = Sha256::new();
        let mut bytes = 0u64;
        loop {
            let n = f.read(&mut block[..(CHUNK - buf.len()).min(1024 * 1024)])?;
            if n == 0 {
                break;
            }
            h.update(&block[..n]);
            buf.extend_from_slice(&block[..n]);
            bytes += n as u64;
            if buf.len() == CHUNK {
                flush_blob(out, &mut buf, &mut m.blobs)?;
            }
        }
        let after = fs::metadata(&input.source)?;
        ensure!(
            bytes == before.len()
                && before.len() == after.len()
                && before.modified()? == after.modified()?,
            "source changed during snapshot: {name}"
        );
        let sha256 = format!("{:x}", h.finalize());
        if let Some(expected) = input.expected {
            ensure!(sha256 == expected, "source checksum mismatch: {name}");
        }
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            before.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = false;
        m.files.push(Entry {
            path: name,
            offset: m.total_bytes,
            bytes,
            sha256,
            executable,
        });
        m.total_bytes = m
            .total_bytes
            .checked_add(bytes)
            .context("snapshot overflow")?;
        if m.files.len().is_multiple_of(1000) {
            eprintln!("snapshot: {} files, {} bytes", m.files.len(), m.total_bytes);
        }
    }
    flush_blob(out, &mut buf, &mut m.blobs)?;
    validate(&m)?;
    let bytes = serde_json::to_vec_pretty(&m)?;
    ensure!(bytes.len() as u64 <= MANIFEST_LIMIT, "manifest too large");
    // Manifest publication is the local completion marker.
    write_new(&out.join("manifest.json"), &bytes)?;
    println!(
        "{}",
        serde_json::json!({"manifest_sha256":digest(&bytes),"files":m.files.len(),"bytes":m.total_bytes,"compressed_bytes":m.blobs.iter().map(|b|b.bytes).sum::<u64>(),"chunks":m.blobs.len()})
    );
    Ok(())
}
fn verify_blobs(root: &Path, m: &Manifest) -> Result<()> {
    for b in &m.blobs {
        let p = root.join("blobs").join(&b.sha256);
        no_symlinks(&p)?;
        ensure!(
            fs::metadata(&p)?.len() == b.bytes && hash_file(&p)? == b.sha256,
            "local blob checksum mismatch"
        );
    }
    Ok(())
}
fn restore(root: &Path, out: &Path) -> Result<()> {
    let (m, _) = read_manifest(&root.join("manifest.json"))?;
    verify_blobs(root, &m)?;
    no_symlinks(out.parent().context("restore parent")?)?;
    private_dir(out)?;
    let mut cached: Option<(usize, Vec<u8>)> = None;
    // The destination must be new; partial restores are retained, never overwritten.
    for e in &m.files {
        let p = out.join(&e.path);
        let mut b = fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(p.parent().context("file parent")?)?;
        no_symlinks(p.parent().context("file parent")?)?;
        let mut f = new_file(&p)?;
        let mut h = Sha256::new();
        let mut pos = e.offset;
        let mut left = e.bytes;
        while left > 0 {
            let i = (pos / CHUNK as u64) as usize;
            if cached.as_ref().map(|c| c.0) != Some(i) {
                cached = None;
                let blob = &m.blobs[i];
                let decoder = zstd::stream::read::Decoder::new(File::open(
                    root.join("blobs").join(&blob.sha256),
                )?)?;
                let mut bytes = Vec::with_capacity(blob.raw_bytes as usize);
                decoder.take(blob.raw_bytes + 1).read_to_end(&mut bytes)?;
                ensure!(
                    bytes.len() as u64 == blob.raw_bytes && digest(&bytes) == blob.raw_sha256,
                    "decoded blob checksum mismatch"
                );
                cached.replace((i, bytes));
            }
            let offset = (pos % CHUNK as u64) as usize;
            let n = left.min(m.blobs[i].raw_bytes - offset as u64) as usize;
            let bytes = &cached.as_ref().context("decoded chunk")?.1[offset..offset + n];
            f.write_all(bytes)?;
            h.update(bytes);
            pos += n as u64;
            left -= n as u64;
        }
        ensure!(
            format!("{:x}", h.finalize()) == e.sha256,
            "restored checksum mismatch: {}",
            e.path
        );
        #[cfg(unix)]
        if e.executable {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(fs::Permissions::from_mode(0o700))?;
        }
        f.sync_all()?;
    }
    File::open(out)?.sync_all()?;
    println!(
        "restored and verified {} files ({} bytes)",
        m.files.len(),
        m.total_bytes
    );
    Ok(())
}
fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["prepare", spec, out] => prepare(Path::new(spec), Path::new(out)),
        ["verify", root] => {
            let (m, b) = read_manifest(&Path::new(root).join("manifest.json"))?;
            verify_blobs(Path::new(root), &m)?;
            println!("verified manifest {}", digest(&b));
            Ok(())
        }
        ["restore", root, out] => restore(Path::new(root), Path::new(out)),
        ["upload", root, account, bucket] => remote::upload(Path::new(root), account, bucket, None),
        ["upload", root, account, bucket, prefix] => {
            remote::upload(Path::new(root), account, bucket, Some(prefix))
        }
        ["fetch", account, bucket, sha, out] => {
            remote::fetch(account, bucket, sha, Path::new(out), None)
        }
        ["fetch", account, bucket, sha, out, prefix] => {
            remote::fetch(account, bucket, sha, Path::new(out), Some(prefix))
        }
        ["put-object", account, bucket, key, file] => {
            remote::put_object(account, bucket, key, Path::new(file))
        }
        ["get-object", account, bucket, key, sha, out] => {
            remote::get_object(account, bucket, key, sha, Path::new(out))
        }
        _ => bail!(
            "usage: slop_ninja_artifact_store prepare SPEC NEW_SNAPSHOT | verify SNAPSHOT | restore SNAPSHOT NEW_DIRECTORY | upload SNAPSHOT ACCOUNT BUCKET [KEY_PREFIX] | fetch ACCOUNT BUCKET MANIFEST_SHA SNAPSHOT_CACHE [KEY_PREFIX] | put-object ACCOUNT BUCKET KEY FILE | get-object ACCOUNT BUCKET KEY SHA256 NEW_FILE"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_and_corruption() -> Result<()> {
        let t = tempfile::tempdir()?;
        let src = t.path().join("src");
        fs::create_dir(&src)?;
        fs::write(src.join("empty"), b"")?;
        // One file crosses a 64 MiB chunk boundary; another starts inside that chunk.
        let mut f = File::create(src.join("large"))?;
        f.write_all(&vec![37; CHUNK + 7])?;
        f.sync_all()?;
        fs::write(src.join("tail"), b"synthetic\n\0bytes")?;
        let spec = t.path().join("spec.json");
        fs::write(
            &spec,
            serde_json::to_vec(&Spec {
                roots: vec![Source {
                    name: "data".into(),
                    path: src.clone(),
                    kind: "tree".into(),
                    exclude: vec![],
                }],
            })?,
        )?;
        let snap = t.path().join("snapshot");
        prepare(&spec, &snap)?;
        let restored = t.path().join("restored");
        restore(&snap, &restored)?;
        for p in ["empty", "large", "tail"] {
            assert_eq!(
                hash_file(&src.join(p))?,
                hash_file(&restored.join("data").join(p))?
            );
        }
        assert!(restore(&snap, &restored).is_err());
        let (mut m, _) = read_manifest(&snap.join("manifest.json"))?;
        m.files[0].path = "../escape".into();
        assert!(validate(&m).is_err());
        let (m, _) = read_manifest(&snap.join("manifest.json"))?;
        fs::write(snap.join("blobs").join(&m.blobs[0].sha256), b"bad")?;
        assert!(restore(&snap, &t.path().join("bad")).is_err());
        Ok(())
    }
    #[test]
    fn live_wal_snapshot_follows_committed_index() -> Result<()> {
        let t = tempfile::tempdir()?;
        let src = t.path().join("archive");
        fs::create_dir(&src)?;
        fs::create_dir(src.join("parses"))?;
        fs::create_dir(src.join("records"))?;
        let mut db = Connection::open(src.join("index.sqlite"))?;
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE parses(text_sha256 TEXT,windows_sha256 TEXT,compressed_sha256 TEXT,record_sha256 TEXT); CREATE TABLE files(raw_sha256 TEXT, compressed_sha256 TEXT);")?;
        let window = digest(b"window");
        let record = digest(b"record");
        let compressed = digest(b"compressed");
        fs::write(src.join(format!("parses/{window}.cfg.zst")), b"compressed")?;
        fs::write(
            src.join(format!("records/{record}.json.zst")),
            zstd::stream::encode_all(&b"record"[..], 3)?,
        )?;
        fs::write(
            src.join("format.json"),
            br#"{"schema":"slop_ninja_cfg_parse_archive_v1"}"#,
        )?;
        fs::write(src.join("profile.json"), b"{}")?;
        db.execute(
            "INSERT INTO parses VALUES ('text',?1,?2,?3)",
            rusqlite::params![window, compressed, record],
        )?;
        let tx = db.transaction()?;
        tx.execute(
            "INSERT INTO parses VALUES ('uncommitted','bad','bad','bad')",
            [],
        )?;
        let staging = t.path().join("staging");
        fs::create_dir(&staging)?;
        let mut inputs = BTreeMap::new();
        cfg_inputs(
            &mut inputs,
            &Source {
                name: "cfg".into(),
                path: src,
                kind: "cfg_archive".into(),
                exclude: vec![],
            },
            &staging,
        )?;
        assert_eq!(inputs.len(), 5);
        let copied = Connection::open(staging.join("cfg.sqlite"))?;
        assert_eq!(
            copied.query_row("SELECT count(*) FROM parses", [], |r| r.get::<_, u64>(0))?,
            1
        );
        tx.rollback()?;
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn reject_symlink_sources() -> Result<()> {
        let t = tempfile::tempdir()?;
        fs::write(t.path().join("original"), b"synthetic")?;
        std::os::unix::fs::symlink(t.path().join("original"), t.path().join("link"))?;
        assert!(
            add(
                &mut BTreeMap::new(),
                "x".into(),
                t.path().join("link"),
                None
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn exclusions_and_nested_names() -> Result<()> {
        let t = tempfile::tempdir()?;
        let src = t.path().join("src");
        fs::create_dir_all(src.join("keep/sub"))?;
        fs::create_dir_all(src.join("sealed/test"))?;
        fs::write(src.join("keep/sub/a.json"), b"synthetic a")?;
        fs::write(src.join("keep/b.json"), b"synthetic b")?;
        fs::write(src.join("sealed/test/prose.jsonl"), b"must not be read")?;
        fs::write(src.join("skip.bin"), b"skip")?;
        let root = |exclude: Vec<&str>| Source {
            name: "restricted/example-v1".into(),
            path: src.clone(),
            kind: "tree".into(),
            exclude: exclude.into_iter().map(String::from).collect(),
        };
        let spec = t.path().join("spec.json");
        fs::write(
            &spec,
            serde_json::to_vec(&Spec {
                roots: vec![root(vec!["sealed", "skip.bin"])],
            })?,
        )?;
        let snap = t.path().join("snapshot");
        prepare(&spec, &snap)?;
        let (m, _) = read_manifest(&snap.join("manifest.json"))?;
        let paths: Vec<&str> = m.files.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "restricted/example-v1/keep/b.json",
                "restricted/example-v1/keep/sub/a.json"
            ]
        );
        // A missing exclude path fails closed instead of silently widening the snapshot.
        fs::write(
            &spec,
            serde_json::to_vec(&Spec {
                roots: vec![root(vec!["sealed", "typo"])],
            })?,
        )?;
        assert!(prepare(&spec, &t.path().join("snapshot-typo")).is_err());
        // Nested logical names may not overlap.
        let mut inner = root(vec![]);
        inner.name = "restricted/example-v1/keep".into();
        assert!(check_roots(&[root(vec![]), inner]).is_err());
        assert!(check_roots(&[root(vec!["../x"])]).is_err());
        Ok(())
    }
}
