//! Lossless storage only: no corpus parsing, split changes, or model calls.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

const MAX_BYTES: u64 = 512 * 1024 * 1024;
const MANIFEST: &str = "datasets/compression-manifest.json";

fn eligible(path: &str) -> bool {
    path.starts_with("datasets/")
        && (path.ends_with(".jsonl")
            || path.ends_with("/data_rights.json")
            || path.ends_with("/evidence-index.json"))
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn bounded(mut reader: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "payload exceeds 512 MiB safety limit"
    );
    Ok(bytes)
}

fn compress(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut writer = zstd::stream::Encoder::new(Vec::new(), 9)?;
    writer.include_checksum(true)?;
    writer.set_pledged_src_size(Some(bytes.len() as u64))?;
    writer.write_all(bytes)?;
    Ok(writer.finish()?)
}

fn decode(bytes: &[u8]) -> Result<Vec<u8>> {
    bounded(zstd::stream::Decoder::new(bytes)?)
}

fn atomic_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().context("no parent")?)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path)
        .with_context(|| format!("refuse overwrite: {}", path.display()))?;
    Ok(())
}

fn inventory(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        ensure!(
            !ty.is_symlink(),
            "dataset symlink rejected: {}",
            entry.path().display()
        );
        if ty.is_dir() {
            inventory(root, &entry.path(), out)?;
        } else if ty.is_file() {
            let path = entry.path();
            out.push(
                path.strip_prefix(root)?
                    .to_str()
                    .context("non-UTF8 path")?
                    .replace('\\', "/"),
            );
        }
    }
    Ok(())
}

fn paths(root: &Path) -> Result<Vec<String>> {
    ensure!(
        !fs::symlink_metadata(root.join("datasets"))?
            .file_type()
            .is_symlink(),
        "datasets symlink rejected"
    );
    let mut out = Vec::new();
    inventory(root, &root.join("datasets"), &mut out)?;
    out.sort();
    Ok(out)
}

fn entry(path: &str, packed: &[u8], raw: &[u8]) -> Value {
    json!({"path":path, "decoded_path":path.strip_suffix(".zst").unwrap(),
        "compressed_bytes":packed.len(), "decoded_bytes":raw.len(),
        "compressed_sha256":sha(packed), "decoded_sha256":sha(raw)})
}

fn prefix_valid(prefix: &str) -> bool {
    prefix.starts_with("datasets/")
        && !prefix.contains('\\')
        && Path::new(prefix)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        && !prefix
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == "..")
}

fn selected(path: &str, prefix: Option<&str>) -> bool {
    prefix.is_none_or(|p| {
        path == p
            || path.strip_suffix(".zst") == Some(p)
            || path
                .strip_prefix(p)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

fn process(root: &Path, mode: &str, prefix: Option<&str>) -> Result<Value> {
    ensure!(
        prefix.is_none_or(prefix_valid),
        "prefix must be a normal datasets/ path"
    );
    let roster = paths(root)?;
    if mode == "pack" {
        for path in roster.iter().filter(|p| eligible(p) && selected(p, prefix)) {
            let raw = bounded(fs::File::open(root.join(path))?)?;
            let dest = root.join(format!("{path}.zst"));
            if dest.exists() {
                ensure!(
                    decode(&bounded(fs::File::open(&dest)?)?)? == raw,
                    "existing archive differs from source: {path}"
                );
            } else {
                let packed = compress(&raw)?;
                ensure!(
                    decode(&packed)? == raw,
                    "compression round trip differs: {path}"
                );
                atomic_new(&dest, &packed)?;
            }
        }
    }
    let old_manifest: Option<Value> = if root.join(MANIFEST).exists() {
        Some(serde_json::from_slice(&fs::read(root.join(MANIFEST))?)?)
    } else {
        None
    };
    if let Some(m) = &old_manifest {
        ensure!(
            m["schema"] == "slop_ninja_dataset_compression_v1",
            "unknown compression manifest"
        );
    }
    let mut entries = Vec::new();
    for path in paths(root)?
        .iter()
        .filter(|p| p.strip_suffix(".zst").is_some_and(eligible) && selected(p, prefix))
    {
        let packed = bounded(fs::File::open(root.join(path))?)?;
        let raw = decode(&packed).with_context(|| format!("decode {path}"))?;
        let record = entry(path, &packed, &raw);
        if mode != "pack" && mode != "manifest" {
            if let Some(m) = &old_manifest {
                let expected = m["files"]
                    .as_array()
                    .context("manifest files")?
                    .iter()
                    .find(|e| e["path"] == *path)
                    .with_context(|| format!("unmanifested archive: {path}"))?;
                ensure!(expected == &record, "archive identity mismatch: {path}");
            }
        }
        let decoded_path = path.strip_suffix(".zst").unwrap();
        let dest = root.join(decoded_path);
        if dest.exists() {
            ensure!(
                bounded(fs::File::open(&dest)?)? == raw,
                "local decoded file differs; no overwrite performed: {decoded_path}"
            );
        } else if mode == "restore" {
            atomic_new(&dest, &raw)?;
        }
        entries.push(record);
    }
    ensure!(
        !entries.is_empty(),
        "no eligible compressed dataset files found"
    );
    if mode != "pack" && mode != "manifest" {
        if let Some(m) = &old_manifest {
            let expected = m["files"]
                .as_array()
                .context("manifest files")?
                .iter()
                .filter(|e| e["path"].as_str().is_some_and(|p| selected(p, prefix)))
                .count();
            ensure!(
                entries.len() == expected,
                "missing or duplicate manifest archive"
            );
        }
    }
    let report = json!({"schema":"slop_ninja_dataset_compression_v1", "codec":"zstd",
        "decoded_bytes_are_canonical":true, "files":entries});
    if mode == "manifest" {
        ensure!(prefix.is_none(), "manifest covers all datasets");
        let mut bytes = serde_json::to_vec_pretty(&report)?;
        bytes.push(b'\n');
        // Manifest generation is explicit; existing entries and files are never silently replaced.
        atomic_new(&root.join(MANIFEST), &bytes)?;
    }
    Ok(report)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "compress") {
        ensure!(args.len() == 1, "compress takes stdin only");
        let raw = bounded(io::stdin().lock())?;
        io::stdout().lock().write_all(&compress(&raw)?)?;
        return Ok(());
    }
    ensure!(
        (2..=3).contains(&args.len()),
        "usage: slop_ninja_dataset_storage pack|manifest|verify|restore REPO_ROOT [datasets/PREFIX]"
    );
    let mode = args[0].as_str();
    if !matches!(mode, "pack" | "manifest" | "verify" | "restore") {
        bail!("unknown command {mode}");
    }
    let root: PathBuf = fs::canonicalize(&args[1])?;
    let report = process(&root, mode, args.get(2).map(String::as_str))?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_bytes_and_corruption() {
        let raw = b"{\"text\":\"synthetic\"}\r\n\n\x00\xff";
        let mut packed = compress(raw).unwrap();
        assert_eq!(decode(&packed).unwrap(), raw);
        let last = packed.len() - 1;
        packed[last] ^= 1;
        assert!(decode(&packed).is_err());
    }

    #[test]
    fn materialize_and_identity_guards() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("datasets/example");
        fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("train.jsonl");
        fs::write(&raw, b"synthetic\r\n").unwrap();
        process(root.path(), "pack", None).unwrap();
        process(root.path(), "manifest", None).unwrap();
        fs::remove_file(&raw).unwrap();
        process(root.path(), "verify", None).unwrap();
        assert!(!raw.exists());
        process(root.path(), "restore", Some("datasets/example/train.jsonl")).unwrap();
        assert_eq!(fs::read(&raw).unwrap(), b"synthetic\r\n");
        fs::write(&raw, b"local changes").unwrap();
        assert!(process(root.path(), "restore", None).is_err());
        assert_eq!(fs::read(&raw).unwrap(), b"local changes");
        fs::remove_file(&raw).unwrap();
        fs::remove_file(dir.join("train.jsonl.zst")).unwrap();
        assert!(process(root.path(), "verify", None).is_err());
    }

    #[test]
    fn scope_and_paths() {
        assert!(eligible("datasets/example/train.jsonl"));
        assert!(eligible("datasets/example/data_rights.json"));
        assert!(!eligible("data/private/train.jsonl"));
        assert!(!eligible("datasets/example/manifest.json"));
        for bad in [
            "/datasets/a",
            "datasets/../data",
            "datasets/a/./b",
            "datasets//b",
            "data/private",
        ] {
            assert!(!prefix_valid(bad));
        }
        assert!(!selected(
            "datasets/foobar/train.jsonl.zst",
            Some("datasets/foo")
        ));
    }
}
