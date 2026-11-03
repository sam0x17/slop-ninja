//! Native helpers: check a model directory or an object file, build the
//! object from a directory, and write the scorer's parts layout.
//!
//! A directory is checked by streaming the object it would pack into, so the
//! directory and object paths run the same checks.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use slop_ninja_protocol::{ArtifactFile, ArtifactHeader, Digest, Record};

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

/// Checks a model directory as the object it packs into (spec v1).
pub fn check_dir(dir: &Path) -> Result<ObjectReport, Refusal> {
    check_dir_for(dir, Version::V1)
}

/// Checks a model directory as the object it packs into, under `version`.
pub fn check_dir_for(dir: &Path, version: Version) -> Result<ObjectReport, Refusal> {
    let header = header_for_dir(dir)?;
    let mut check = ObjectCheck::for_spec(version);
    stream_object(dir, &header, |b| check.update(b))?;
    check.finish()
}

/// Checks an object file (spec v1).
pub fn check_object(path: &Path) -> Result<ObjectReport, Refusal> {
    check_object_for(path, Version::V1)
}

/// Checks an object file under `version`.
pub fn check_object_for(path: &Path, version: Version) -> Result<ObjectReport, Refusal> {
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

/// Checks `dir` and writes its object to `out` (created new), under spec v1.
pub fn pack(dir: &Path, out: &Path) -> Result<ObjectReport, Refusal> {
    pack_for(dir, out, Version::V1)
}

/// Checks `dir` under `version` and writes its object to `out` (created new).
pub fn pack_for(dir: &Path, out: &Path, version: Version) -> Result<ObjectReport, Refusal> {
    let header = header_for_dir(dir)?;
    let tmp = out.with_extension("packing");
    let mut w = fs::File::create_new(&tmp).map_err(|e| io(&tmp, e))?;
    let mut check = ObjectCheck::for_spec(version);
    let result = stream_object(dir, &header, |b| {
        check.update(b)?;
        w.write_all(b).map_err(|e| io(&tmp, e))
    })
    .and_then(|()| check.finish());
    match result {
        Ok(report) => {
            w.sync_all().map_err(|e| io(&tmp, e))?;
            drop(w);
            fs::rename(&tmp, out).map_err(|e| io(out, e))?;
            Ok(report)
        }
        Err(e) => {
            drop(w);
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Writes the scorer's layout for a checked object or directory:
/// `out/blobs/<part sha256>` and `out/artifact-files.json`.
pub fn write_parts(dir: &Path, report: &ObjectReport, out: &Path) -> Result<PathBuf, Refusal> {
    let blobs = out.join("blobs");
    fs::create_dir_all(&blobs).map_err(|e| io(&blobs, e))?;
    for f in &report.parts_manifest.files {
        let path = dir.join(&f.name);
        let mut src = fs::File::open(&path).map_err(|e| io(&path, e))?;
        for p in &f.parts {
            let mut buf = Vec::with_capacity(p.bytes as usize);
            (&mut src)
                .take(p.bytes)
                .read_to_end(&mut buf)
                .map_err(|e| io(&path, e))?;
            if buf.len() as u64 != p.bytes || crate::sha256_hex(&buf) != p.sha256 {
                return Err(Refusal::Object(format!(
                    "{} changed after the check",
                    f.name
                )));
            }
            let dest = blobs.join(&p.sha256);
            if !dest.exists() {
                let tmp = blobs.join(format!("{}.tmp", p.sha256));
                fs::write(&tmp, &buf).map_err(|e| io(&tmp, e))?;
                fs::rename(&tmp, &dest).map_err(|e| io(&dest, e))?;
            }
        }
    }
    let manifest = out.join("artifact-files.json");
    fs::write(&manifest, report.parts_manifest.to_bytes()).map_err(|e| io(&manifest, e))?;
    Ok(manifest)
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
