//! Native helpers: check a model directory or an object file, build the
//! object from a directory, and write the scorer's files layout v2 for an
//! operator-staged reference package.
//!
//! A directory's header is built by hashing each file once (the pack hash,
//! [`header_for_dir`]); every check after that reads the files against that
//! header and hashes nothing ([`crate::files`]).

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use slop_ninja_protocol::{ArtifactFile, ArtifactHeader, Digest, Record};

use crate::files::{FilesReport, check_files_for, check_object_files};
use crate::files_manifest::FilesManifest;
use crate::mapping::{ObjectCheck, ObjectReport};
use crate::spec::Version;
use crate::{Refusal, spec};

const CHUNK: usize = 1 << 20;

fn io(path: &Path, e: std::io::Error) -> Refusal {
    Refusal::Object(format!("{}: {e}", path.display()))
}

fn hash_file(path: &Path) -> Result<(Digest, u64), Refusal> {
    let mut f = fs::File::open(path).map_err(|e| io(path, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(|e| io(path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total += n as u64;
    }
    Ok((Digest(h.finalize().into()), total))
}

/// The header of the object `dir` packs into. Refuses a directory with any
/// entry outside the file table, or without one of its files.
pub fn header_for_dir(dir: &Path) -> Result<ArtifactHeader, Refusal> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| io(dir, e))? {
        let entry = entry.map_err(|e| io(dir, e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type().map_err(|e| io(dir, e))?;
        if !kind.is_file() || spec::role_by_name(&name).is_none() {
            return Err(Refusal::UndeclaredFile(name));
        }
        names.push(name);
    }
    for role in &spec::ROLES {
        if !names.iter().any(|n| n == role.name) {
            return Err(Refusal::MissingFile(role.name));
        }
    }
    names.sort();
    let mut files = Vec::new();
    for name in names {
        let role = spec::role_by_name(&name).expect("checked");
        let (sha256, len) = hash_file(&dir.join(&name))?;
        files.push(ArtifactFile {
            name,
            role: role.file_role,
            sha256,
            len,
        });
    }
    Ok(ArtifactHeader {
        contract: spec::CONTRACT_ID.into(),
        files,
    })
}

/// Streams the object for `header` and the files in `dir` into `sink`.
pub fn stream_object(
    dir: &Path,
    header: &ArtifactHeader,
    mut sink: impl FnMut(&[u8]) -> Result<(), Refusal>,
) -> Result<(), Refusal> {
    let bytes = header
        .to_bytes()
        .map_err(|e| Refusal::Object(e.to_string()))?;
    let len = u32::try_from(bytes.len()).map_err(|_| Refusal::Object("header length".into()))?;
    sink(&len.to_le_bytes())?;
    sink(&bytes)?;
    let mut buf = vec![0u8; CHUNK];
    for f in &header.files {
        let path = dir.join(&f.name);
        let mut file = fs::File::open(&path).map_err(|e| io(&path, e))?;
        loop {
            let n = file.read(&mut buf).map_err(|e| io(&path, e))?;
            if n == 0 {
                break;
            }
            sink(&buf[..n])?;
        }
    }
    Ok(())
}

/// Checks a model directory (spec v1).
pub fn check_dir(dir: &Path) -> Result<FilesReport, Refusal> {
    check_dir_for(dir, Version::V1)
}

/// Checks a model directory under `version`: its header (one hash per
/// file), then the files against it (no hash).
pub fn check_dir_for(dir: &Path, version: Version) -> Result<FilesReport, Refusal> {
    let header = header_for_dir(dir)?;
    check_files_for(dir, &header, version)
}

/// Checks a model directory under `version` and returns its header too.
pub fn check_dir_with_header(dir: &Path, version: Version) -> Result<(ArtifactHeader, FilesReport), Refusal> {
    let header = header_for_dir(dir)?;
    let report = check_files_for(dir, &header, version)?;
    Ok((header, report))
}

/// Checks an object file in place (spec v1), hashing nothing.
pub fn check_object(path: &Path) -> Result<FilesReport, Refusal> {
    check_object_for(path, Version::V1)
}

/// Checks an object file in place under `version`, hashing nothing
/// ([`check_object_files`]).
pub fn check_object_for(path: &Path, version: Version) -> Result<FilesReport, Refusal> {
    check_object_files(path, version)
}

/// Verifies a whole object file the way a reader that trusts nothing does:
/// the protocol reader re-hashes the object and every file. A tool for a
/// downloaded object; admission and scoring never call it.
pub fn verify_object_for(path: &Path, version: Version) -> Result<ObjectReport, Refusal> {
    let mut f = fs::File::open(path).map_err(|e| io(path, e))?;
    let mut check = ObjectCheck::for_spec(version);
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = f.read(&mut buf).map_err(|e| io(path, e))?;
        if n == 0 {
            break;
        }
        check.update(&buf[..n])?;
    }
    check.finish()
}

/// Writes `dir`'s object to `out` (created new) under spec v1.
pub fn pack(dir: &Path, out: &Path) -> Result<FilesReport, Refusal> {
    pack_for(dir, out, Version::V1)
}

/// The spec's file table for `dir`: every entry must be one of the spec's
/// six files, and each must be present. Returns `(name, role, path)` sorted
/// by name. Reads the directory listing only.
pub fn spec_files_in(dir: &Path) -> Result<Vec<(String, slop_ninja_protocol::FileRole, PathBuf)>, Refusal> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| io(dir, e))? {
        let entry = entry.map_err(|e| io(dir, e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type().map_err(|e| io(dir, e))?;
        if !kind.is_file() || spec::role_by_name(&name).is_none() {
            return Err(Refusal::UndeclaredFile(name));
        }
        names.push(name);
    }
    for role in &spec::ROLES {
        if !names.iter().any(|n| n == role.name) {
            return Err(Refusal::MissingFile(role.name));
        }
    }
    names.sort();
    Ok(names
        .into_iter()
        .map(|name| {
            let role = spec::role_by_name(&name).expect("checked").file_role;
            let path = dir.join(&name);
            (name, role, path)
        })
        .collect())
}

/// Writes the object for `dir` to `out` (created new, mode 0600 on Unix)
/// under the spec's file table and contract, hashing each file exactly
/// once ([`write_files_once`]). Returns the header and its bytes.
pub fn write_object_once(dir: &Path, out: &Path) -> Result<(ArtifactHeader, Vec<u8>), Refusal> {
    write_files_once(spec::CONTRACT_ID, &spec_files_in(dir)?, out)
}

/// Writes the artifact object for `files` (`(name, role, path)`, any order)
/// under `contract` to `out`, hashing each file exactly once, in the same
/// pass that writes it: the header's encoded length is fixed by the names,
/// roles and contract (each digest and length has a fixed size), so room for
/// it is reserved first, each file is streamed once into place while it is
/// hashed, and the header is written last. The digests are of exactly the
/// bytes written, so no read-back is needed and nothing re-hashes them: the
/// header is the only record of them, and R2 verifies each uploaded file
/// against it.
///
/// `out` must not exist; the object is written to a sibling temporary file
/// (mode 0600 on Unix: the object stays private until its cohort closes),
/// synced, and renamed into place. An empty file, a duplicate name or any
/// invalid header is refused before a byte is written. Returns the header
/// and its bytes.
pub fn write_files_once(
    contract: &str,
    files: &[(String, slop_ninja_protocol::FileRole, PathBuf)],
    out: &Path,
) -> Result<(ArtifactHeader, Vec<u8>), Refusal> {
    use std::io::{Seek, SeekFrom};
    if out.exists() {
        return Err(Refusal::Object(format!("{} exists; choose another output path", out.display())));
    }
    let mut sorted: Vec<&(String, slop_ninja_protocol::FileRole, PathBuf)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, _, path) in &sorted {
        let len = fs::metadata(path).map_err(|e| io(path, e))?.len();
        if len == 0 {
            return Err(Refusal::Object(format!("{name}: the file {} is empty", path.display())));
        }
    }
    // Every digest and length encodes at a fixed size, so this placeholder
    // has the final header's exact length; encoding it also validates the
    // names, roles and contract before anything is written.
    let placeholder = ArtifactHeader {
        contract: contract.into(),
        files: sorted
            .iter()
            .map(|(name, role, _)| ArtifactFile { name: name.clone(), role: *role, sha256: Digest([0; 32]), len: 1 })
            .collect(),
    };
    let header_len = placeholder.to_bytes().map_err(|e| Refusal::Object(e.to_string()))?.len();
    if header_len > slop_ninja_protocol::MAX_ARTIFACT_HEADER_LEN as usize {
        return Err(Refusal::Object("artifact header length".into()));
    }
    let mut tmp_name = out.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    tmp_name.push(".packing");
    let tmp = out.with_file_name(tmp_name);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut w = options.open(&tmp).map_err(|e| io(&tmp, e))?;
    let result = (|| -> Result<(ArtifactHeader, Vec<u8>), Refusal> {
        w.write_all(&vec![0u8; 4 + header_len]).map_err(|e| io(&tmp, e))?;
        let mut entries = Vec::with_capacity(sorted.len());
        let mut buf = vec![0u8; CHUNK];
        for (name, role, path) in &sorted {
            let mut src = fs::File::open(path).map_err(|e| io(path, e))?;
            // The one hash of this file: the pack hash, over exactly the
            // bytes written.
            let mut h = Sha256::new();
            let mut len = 0u64;
            loop {
                let n = src.read(&mut buf).map_err(|e| io(path, e))?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
                w.write_all(&buf[..n]).map_err(|e| io(&tmp, e))?;
                len += n as u64;
            }
            entries.push(ArtifactFile { name: name.clone(), role: *role, sha256: Digest(h.finalize().into()), len });
        }
        let header = ArtifactHeader { contract: contract.into(), files: entries };
        let bytes = header.to_bytes().map_err(|e| Refusal::Object(e.to_string()))?;
        if bytes.len() != header_len {
            return Err(Refusal::Object("header length changed while packing".into()));
        }
        w.seek(SeekFrom::Start(0)).map_err(|e| io(&tmp, e))?;
        w.write_all(&(header_len as u32).to_le_bytes()).map_err(|e| io(&tmp, e))?;
        w.write_all(&bytes).map_err(|e| io(&tmp, e))?;
        w.sync_all().map_err(|e| io(&tmp, e))?;
        Ok((header, bytes))
    })();
    drop(w);
    let finished = result.and_then(|v| {
        if out.exists() {
            return Err(Refusal::Object(format!("{} exists; choose another output path", out.display())));
        }
        fs::rename(&tmp, out).map_err(|e| io(out, e))?;
        Ok(v)
    });
    if finished.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    finished
}

/// Writes the object for `dir` to `out` (created new) with
/// [`write_object_once`], then checks it in place under `version` (no
/// hash). A refused object is removed.
pub fn pack_for(dir: &Path, out: &Path, version: Version) -> Result<FilesReport, Refusal> {
    write_object_once(dir, out)?;
    check_object_files(out, version).inspect_err(|_| {
        let _ = fs::remove_file(out);
    })
}

pub use crate::files_manifest::{REFERENCE_BUCKET, REFERENCE_FILES_PREFIX, reference_file_key};

/// Writes the scorer's files layout v2 for an operator-staged reference
/// package from a checked directory and its header: `out/files/<sha256>`
/// (hard links, or copies) and `out/artifact-files.json`, a files manifest
/// naming [`REFERENCE_BUCKET`] and [`reference_file_key`] with no entity
/// tags. That manifest is the staging request, not yet the package: the
/// eval Worker's commit route (`POST /eval/reference/v2/commit`) heads each
/// staged file, pins its entity tag, and writes the pinned manifest, whose
/// digest is the package the template names. Returns the manifest path and
/// the unpinned manifest's digest.
pub fn write_files(
    dir: &Path,
    header: &ArtifactHeader,
    report: &FilesReport,
    version: Version,
    out: &Path,
) -> Result<(PathBuf, String), Refusal> {
    let files = out.join("files");
    fs::create_dir_all(&files).map_err(|e| io(&files, e))?;
    for f in &header.files {
        let sha = hex::encode(f.sha256.0);
        let (from, to) = (dir.join(&f.name), files.join(&sha));
        if !to.exists() && fs::hard_link(&from, &to).is_err() {
            fs::copy(&from, &to).map_err(|e| io(&from, e))?;
        }
    }
    let m = FilesManifest::for_header(header, version, &report.profile_id, |_, sha| {
        (REFERENCE_BUCKET.into(), reference_file_key(sha), None)
    })?;
    let bytes = m.to_bytes();
    let manifest = out.join("artifact-files.json");
    fs::write(&manifest, &bytes).map_err(|e| io(&manifest, e))?;
    Ok((manifest, FilesManifest::package_of(&bytes)))
}

/// Writes `dir`'s artifact into `out` (created new) with each JSON role file
/// re-encoded in its canonical layout, and every digest that bound the old
/// bytes rebound to the new ones: any string equal to the old contract's
/// SHA-256 in the heads, the calibration and the export manifest (the heads'
/// and calibration's `contract_sha256` and the fit binding's among them), and
/// any string equal to the old heads' or calibration's SHA-256 in the export
/// manifest. The graph and the tokenizer are hard-linked (or copied).
/// Values are unchanged; only their spelling is. For an exporter whose writers
/// use other layouts (for example a one-space indent).
pub fn canonicalize(dir: &Path, out: &Path) -> Result<(), Refusal> {
    use crate::canonical;
    use serde_json::Value;
    let read = |name: &str| -> Result<(Vec<u8>, Value), Refusal> {
        let path = dir.join(name);
        let b = fs::read(&path).map_err(|e| io(&path, e))?;
        let v = serde_json::from_slice(&b).map_err(|e| Refusal::Object(format!("{name}: {e}")))?;
        Ok((b, v))
    };
    let layout = |name: &str| {
        spec::role_by_name(name)
            .and_then(|r| r.layout)
            .expect("JSON role")
    };
    fn rebind(v: &mut Value, old: &str, new: &str) {
        match v {
            Value::String(s) if s == old => *s = new.to_string(),
            Value::Array(a) => a.iter_mut().for_each(|x| rebind(x, old, new)),
            Value::Object(o) => o.values_mut().for_each(|x| rebind(x, old, new)),
            _ => {}
        }
    }
    fs::create_dir(out).map_err(|e| io(out, e))?;
    let (cb, contract) = read("contract.json")?;
    let cb2 = canonical::encode(&contract, layout("contract.json"));
    let (old_c, new_c) = (crate::sha256_hex(&cb), crate::sha256_hex(&cb2));
    let (hb, mut heads) = read("heads.json")?;
    rebind(&mut heads, &old_c, &new_c);
    let hb2 = canonical::encode(&heads, layout("heads.json"));
    let (calb, mut cal) = read("calibration.json")?;
    rebind(&mut cal, &old_c, &new_c);
    let calb2 = canonical::encode(&cal, layout("calibration.json"));
    let (_, mut m) = read("manifest.json")?;
    rebind(&mut m, &old_c, &new_c);
    rebind(&mut m, &crate::sha256_hex(&hb), &crate::sha256_hex(&hb2));
    rebind(
        &mut m,
        &crate::sha256_hex(&calb),
        &crate::sha256_hex(&calb2),
    );
    let mb2 = canonical::encode(&m, layout("manifest.json"));
    for (name, bytes) in [
        ("contract.json", &cb2),
        ("heads.json", &hb2),
        ("calibration.json", &calb2),
        ("manifest.json", &mb2),
    ] {
        let path = out.join(name);
        fs::write(&path, bytes).map_err(|e| io(&path, e))?;
    }
    for name in [spec::GRAPH_FILE, "tokenizer.json"] {
        let (from, to) = (dir.join(name), out.join(name));
        if fs::hard_link(&from, &to).is_err() {
            fs::copy(&from, &to).map_err(|e| io(&from, e))?;
        }
    }
    Ok(())
}
