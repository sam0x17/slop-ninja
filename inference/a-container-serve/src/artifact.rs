//! Artifact files manifest v2 and the placement of an artifact's files.
//!
//! An artifact is the six files the server loads (the f32 ONNX graph, heads,
//! contract, calibration, export manifest and tokenizer). The manifest
//! (`slop_ninja_a_artifact_files_v2`, defined by the artifact spec crate)
//! lists each file with its length and SHA-256, which the object store
//! verified when the file was uploaded, and where it is stored. Its exact
//! bytes are the package: their SHA-256 (a small record's) is the package
//! digest an artifact record commits to, and the scorer refuses a manifest
//! whose bytes hash to anything else.
//!
//! The coordinator writes each file into the container in fixed chunks of
//! [`MAX_RANGE_BYTES`] at their offsets ([`Placement`]); no single transfer
//! carries a whole graph. **Nothing here hashes a file**: the bytes arrive
//! only from the coordinator's own lane over the container port (Internet is
//! off), read from storage with the length and entity tag the manifest
//! names, and every chunk is checked against its exact offset and length.
pub use slop_ninja_artifact_check::files_manifest::{FILES_SCHEMA_V2 as SCHEMA, FileEntry, FilesManifest, MAX_MANIFEST_BYTES};
use anyhow::{Context, Result, anyhow, ensure};
use slop_ninja_artifact_check::spec;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// One chunk, and so the largest transfer: the spec's scorer mapping part
/// size (64 MiB).
pub const MAX_RANGE_BYTES: u64 = spec::PART_BYTES;
/// Most chunks per file: the spec's per-file bound.
pub const MAX_CHUNKS_PER_FILE: u64 = spec::MAX_PARTS_PER_FILE;

pub fn is_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The files of one bound package as they arrive: each created at its
/// manifest length, and which of its chunks have been written.
#[derive(Debug)]
pub struct Placement {
    dir: PathBuf,
    files: Vec<(String, u64, Vec<bool>)>,
}

impl Placement {
    /// Creates `dir` and every file of `m` at its declared length (sparse).
    pub fn create(dir: &Path, m: &FilesManifest) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let mut files = Vec::with_capacity(m.files.len());
        for f in &m.files {
            ensure!(
                !f.name.is_empty() && !f.name.contains('/') && f.name != "." && f.name != "..",
                "artifact file name must be a bare file name"
            );
            let chunks = f.bytes.div_ceil(MAX_RANGE_BYTES);
            ensure!(f.bytes > 0 && chunks <= MAX_CHUNKS_PER_FILE, "{}: length", f.name);
            let file = fs::File::create(dir.join(&f.name))?;
            file.set_len(f.bytes)?;
            files.push((f.name.clone(), f.bytes, vec![false; chunks as usize]));
        }
        Ok(Self { dir: dir.to_path_buf(), files })
    }

    /// The directory the files are placed in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The exact length of chunk `offset` of file `index`: offsets are
    /// multiples of [`MAX_RANGE_BYTES`] inside the file.
    pub fn chunk_len(&self, index: usize, offset: u64) -> Option<u64> {
        let (_, len, _) = self.files.get(index)?;
        (offset % MAX_RANGE_BYTES == 0 && offset < *len).then(|| (len - offset).min(MAX_RANGE_BYTES))
    }

    /// Where chunk `offset` of file `index` goes: the file's path and the
    /// chunk's exact length.
    pub fn target(&self, index: usize, offset: u64) -> Option<(PathBuf, u64)> {
        let want = self.chunk_len(index, offset)?;
        let (name, _, _) = self.files.get(index)?;
        Some((self.dir.join(name), want))
    }

    /// Records chunk `offset` of file `index` as written.
    pub fn mark(&mut self, index: usize, offset: u64) {
        if let Some((_, _, done)) = self.files.get_mut(index)
            && let Some(c) = done.get_mut((offset / MAX_RANGE_BYTES) as usize)
        {
            *c = true;
        }
    }

    /// Writes chunk `offset` of file `index` from `body`: exactly its length,
    /// no more and no less. Writing a chunk again (a retried transfer)
    /// overwrites it. Returns the bytes written.
    pub fn write(&mut self, index: usize, offset: u64, body: &mut dyn Read) -> Result<u64> {
        let (path, want) = self.target(index, offset).ok_or_else(|| anyhow!("no such chunk"))?;
        let n = write_chunk(&path, offset, want, body)?;
        self.mark(index, offset);
        Ok(n)
    }

    /// The chunks not yet written, as `(file index, offset)`.
    pub fn missing(&self) -> Vec<(usize, u64)> {
        self.files
            .iter()
            .enumerate()
            .flat_map(|(i, (_, _, done))| {
                done.iter().enumerate().filter(|(_, d)| !**d).map(move |(c, _)| (i, c as u64 * MAX_RANGE_BYTES))
            })
            .collect()
    }

    /// Whether every chunk of every file is written.
    pub fn complete(&self) -> bool {
        self.files.iter().all(|(_, _, done)| done.iter().all(|d| *d))
    }
}

/// Writes exactly `want` bytes of `body` into `path` at `offset`, refusing a
/// body of any other length; no hash.
pub fn write_chunk(path: &Path, offset: u64, want: u64, body: &mut dyn Read) -> Result<u64> {
    let mut w = fs::OpenOptions::new().write(true).open(path)?;
    w.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        ensure!(total <= want, "chunk longer than {want} bytes");
        w.write_all(&buf[..n])?;
    }
    ensure!(total == want, "chunk of {total} bytes; {want} expected");
    w.sync_all()?;
    Ok(total)
}

/// Lays out a model directory as a reference package for tests and tools:
/// `out/files/<sha256>` and `out/artifact-files.json`, with no admission
/// check (adversarial cases and engine tests lay out artifacts the spec
/// refuses). The digests come from hashing the directory once (the packer's
/// own hash, operator side). Returns the manifest bytes.
pub fn pack(model_dir: &Path, out: &Path, profile_id: &str) -> Result<Vec<u8>> {
    let header = slop_ninja_artifact_check::fs::header_for_dir(model_dir).map_err(|e| anyhow!("{e}"))?;
    let files = out.join("files");
    fs::create_dir_all(&files)?;
    let mut entries = Vec::new();
    for role in &spec::ROLES {
        let f = header.files.iter().find(|f| f.name == role.name).context("file table")?;
        let sha = hex::encode(f.sha256.0);
        let dest = files.join(&sha);
        if !dest.exists() {
            fs::copy(model_dir.join(&f.name), &dest)?;
        }
        entries.push(FileEntry {
            name: f.name.clone(),
            role: role.scorer_role.into(),
            bytes: f.len,
            sha256: sha.clone(),
            bucket: "eval".into(),
            key: slop_ninja_artifact_check::fs::reference_file_key(&sha),
            etag: None,
        });
    }
    let m = FilesManifest {
        schema: SCHEMA.into(),
        profile_id: profile_id.into(),
        contract: header.contract.clone(),
        graph: spec::GRAPH_FILE.into(),
        files: entries,
    };
    let bytes = m.to_bytes();
    fs::write(out.join("artifact-files.json"), &bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("a-artifact-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn manifest(lens: [u64; 6]) -> FilesManifest {
        FilesManifest {
            schema: SCHEMA.into(),
            profile_id: "p".into(),
            contract: spec::CONTRACT_ID.into(),
            graph: spec::GRAPH_FILE.into(),
            files: spec::ROLES
                .iter()
                .zip(lens)
                .map(|(r, bytes)| FileEntry {
                    name: r.name.into(),
                    role: r.scorer_role.into(),
                    bytes,
                    sha256: "ab".repeat(32),
                    bucket: "eval".into(),
                    key: "k".into(),
                    etag: None,
                })
                .collect(),
        }
    }

    #[test]
    fn chunks_are_written_at_exact_offsets_and_lengths() {
        let dir = scratch("place");
        let big = MAX_RANGE_BYTES + 5;
        let mut p = Placement::create(&dir, &manifest([big, 3, 3, 3, 3, 3])).unwrap();
        assert_eq!(p.missing().len(), 7);
        assert_eq!(p.chunk_len(0, MAX_RANGE_BYTES), Some(5));
        assert_eq!(p.chunk_len(0, 1), None, "unaligned offset");
        assert_eq!(p.chunk_len(0, big), None, "past the end");
        // Short and long bodies are refused and leave the chunk missing.
        assert!(p.write(1, 0, &mut &b"ab"[..]).is_err());
        assert!(p.write(1, 0, &mut &b"abcd"[..]).is_err());
        assert!(p.missing().contains(&(1, 0)));
        assert_eq!(p.write(0, MAX_RANGE_BYTES, &mut &b"12345"[..]).unwrap(), 5);
        // Writing again overwrites.
        p.write(0, MAX_RANGE_BYTES, &mut &b"54321"[..]).unwrap();
        for i in 1..6 {
            p.write(i, 0, &mut &b"xyz"[..]).unwrap();
        }
        assert!(!p.complete());
        assert_eq!(p.missing(), vec![(0, 0)]);
        let first = vec![7u8; MAX_RANGE_BYTES as usize];
        p.write(0, 0, &mut first.as_slice()).unwrap();
        assert!(p.complete());
        let graph = fs::read(dir.join(spec::GRAPH_FILE)).unwrap();
        assert_eq!(graph.len() as u64, big);
        assert_eq!(&graph[MAX_RANGE_BYTES as usize..], b"54321");
    }

    #[test]
    fn a_file_past_the_chunk_bound_or_a_path_name_is_refused() {
        let dir = scratch("bounds");
        let too_big = MAX_CHUNKS_PER_FILE * MAX_RANGE_BYTES + 1;
        assert!(Placement::create(&dir, &manifest([too_big, 3, 3, 3, 3, 3])).is_err());
        let mut m = manifest([3; 6]);
        m.files[1].name = "../heads.json".into();
        assert!(Placement::create(&dir, &m).is_err());
    }
}
