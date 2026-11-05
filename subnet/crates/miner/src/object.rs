//! Packing artifact objects and reading their headers.
//!
//! The object is the protocol's `u32_le(len(H)) || H || files`
//! (`slop_ninja_protocol::artifact`). [`pack_dir`] packs a model directory
//! under the *A* artifact spec's file table (six fixed names and roles, the
//! spec's output contract); [`pack`] packs any `ROLE:PATH` list.
//!
//! **Hash once.** Packing streams each file exactly once into the object
//! while it hashes those same bytes
//! (`slop_ninja_artifact_check::fs::write_files_once`): the header's length
//! is fixed before any digest is known, so room for it is reserved, every
//! file is written in place and the header goes in last. That pack hash is
//! the only SHA-256 anyone computes over the model's bytes: the commitment
//! binds the header's digests, and R2 verifies each uploaded file against
//! them as it arrives. No later step reads the files back to hash them:
//! [`read_header`] reads only the length prefix and the header (a range of at
//! most `4 + MAX_ARTIFACT_HEADER_LEN` bytes) and compares the file's length
//! on disk with the header's. The header bytes themselves (a file table of at
//! most 1 MiB, never a model file) are hashed for `header_sha256`, the digest
//! the upload envelope names.
//!
//! The object must stay secret until its cohort's scoring starts, so it is
//! written with mode 0600 like the openings.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use slop_ninja_protocol::{ArtifactFile, ArtifactHeader, Digest, FileRole, VerifiedHeader};

/// Role names as the command line spells them.
pub const ROLES: &[(&str, FileRole)] = &[
    ("graph", FileRole::Graph),
    ("weights", FileRole::Weights),
    ("tokenizer", FileRole::Tokenizer),
    ("calibration", FileRole::Calibration),
    ("output-schema", FileRole::OutputSchema),
    ("reference-vectors", FileRole::ReferenceVectors),
    ("owner-metadata", FileRole::OwnerMetadata),
    ("publisher-signature", FileRole::PublisherSignature),
];

pub fn parse_role(name: &str) -> Result<FileRole> {
    ROLES
        .iter()
        .find(|(text, _)| *text == name)
        .map(|(_, role)| *role)
        .with_context(|| {
            let known: Vec<&str> = ROLES.iter().map(|(text, _)| *text).collect();
            format!(
                "unknown file role {name:?}; expected one of {}",
                known.join(", ")
            )
        })
}

pub fn role_name(role: FileRole) -> &'static str {
    ROLES
        .iter()
        .find(|(_, r)| *r == role)
        .map(|(text, _)| *text)
        .expect("every role is named")
}

/// One file to pack: its name inside the object, its role and its path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackInput {
    pub name: String,
    pub role: FileRole,
    pub path: PathBuf,
}

impl PackInput {
    /// Parses `ROLE:RELATIVE_PATH` under `root`. The object name is the
    /// relative path with `/` separators.
    pub fn parse(spec: &str, root: &Path) -> Result<Self> {
        let (role, name) = spec
            .split_once(':')
            .with_context(|| format!("expected ROLE:PATH, got {spec:?}"))?;
        let role = parse_role(role)?;
        ensure!(
            !name.is_empty() && !name.starts_with('/') && !name.contains('\\'),
            "file {name:?} must be a relative path with / separators"
        );
        Ok(Self {
            name: name.to_owned(),
            role,
            path: root.join(name),
        })
    }
}

/// A local artifact object, known by its header alone: what `commit`,
/// `upload` and `check` need, read without hashing any model byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectHeader {
    pub header: ArtifactHeader,
    /// The header bytes `H`.
    pub bytes: Vec<u8>,
    /// SHA-256 of `H`: the upload envelope's `header_sha256` and the digest
    /// R2 verifies the header object against.
    pub sha256: Digest,
    /// `len(H)`.
    pub len: u32,
    /// The derived inference-content digest the commitment binds.
    pub content: Digest,
    /// The object's whole length: the prefix, `H` and every file.
    pub object_len: u64,
}

impl ObjectHeader {
    /// From the header bytes `H` (hashing them: a file table, never a model
    /// file).
    pub fn from_header_bytes(bytes: Vec<u8>) -> Result<Self> {
        let verified = VerifiedHeader::from_bytes(&bytes)
            .map_err(|error| anyhow::anyhow!("invalid artifact header: {error:?}"))?;
        let files_len = verified
            .header
            .files_len()
            .map_err(|error| anyhow::anyhow!("invalid artifact header: {error:?}"))?;
        let object_len = (4 + u64::from(verified.len))
            .checked_add(files_len)
            .context("artifact length overflows")?;
        Ok(Self {
            header: verified.header,
            bytes,
            sha256: verified.sha256,
            len: verified.len,
            content: verified.content,
            object_len,
        })
    }

    /// Where the files start: after the prefix and `H`.
    pub fn files_at(&self) -> u64 {
        4 + u64::from(self.len)
    }

    /// The object range `(offset, len)` of the first file with `sha256`.
    pub fn file_range(&self, sha256: &Digest) -> Option<(u64, u64)> {
        let mut offset = self.files_at();
        for f in &self.header.files {
            if f.sha256 == *sha256 {
                return Some((offset, f.len));
            }
            offset += f.len;
        }
        None
    }

    /// The distinct files `(sha256, len)` in header order: one R2 object
    /// each (two entries with one digest share one object).
    pub fn distinct_files(&self) -> Vec<&ArtifactFile> {
        let mut out: Vec<&ArtifactFile> = Vec::with_capacity(self.header.files.len());
        for f in &self.header.files {
            if !out.iter().any(|o| o.sha256 == f.sha256) {
                out.push(f);
            }
        }
        out
    }

    /// The header as the protocol's verified header.
    pub fn verified(&self) -> VerifiedHeader {
        VerifiedHeader {
            sha256: self.sha256,
            len: self.len,
            header: self.header.clone(),
            content: self.content,
        }
    }
}

/// Writes the artifact object for `inputs` to `out` (which must not exist),
/// mode 0600, hashing each file once while writing it.
pub fn pack(contract: &str, inputs: &[PackInput], out: &Path) -> Result<ObjectHeader> {
    ensure!(!inputs.is_empty(), "nothing to pack");
    let mut names: Vec<&str> = inputs.iter().map(|i| i.name.as_str()).collect();
    names.sort_unstable();
    if let Some(pair) = names.windows(2).find(|pair| pair[0] == pair[1]) {
        anyhow::bail!("file {:?} is listed twice", pair[0]);
    }
    let files: Vec<(String, FileRole, PathBuf)> = inputs
        .iter()
        .map(|i| (i.name.clone(), i.role, i.path.clone()))
        .collect();
    let (_, bytes) = slop_ninja_artifact_check::fs::write_files_once(contract, &files, out)
        .map_err(|refused| anyhow::anyhow!("{refused}"))?;
    ObjectHeader::from_header_bytes(bytes)
}

/// Packs a model directory under the artifact spec's file table: exactly the
/// spec's six files, with their fixed roles and the spec's output contract. A
/// directory with any other entry, or without one of the files, is refused
/// with the artifact-check code (`undeclared_file`, `missing_file`).
pub fn pack_dir(dir: &Path, out: &Path) -> Result<ObjectHeader> {
    let (_, bytes) = slop_ninja_artifact_check::fs::write_object_once(dir, out)
        .map_err(|refused| anyhow::anyhow!("refused {}: {refused}", refused.code()))?;
    ObjectHeader::from_header_bytes(bytes)
}

/// Reads an object file's header (the length prefix and `H`, a range read)
/// and checks the file's length on disk against it. Reads no file body.
pub fn read_header(path: &Path) -> Result<ObjectHeader> {
    read_header_checked(path)?.map_err(anyhow::Error::msg)
}

/// Like [`read_header`], but separates the two failures: the outer error is
/// a file that cannot be read; the inner one an object whose header does not
/// decode or validate, or whose length is not its header's (a refusal, with
/// its message).
pub fn read_header_checked(path: &Path) -> Result<std::result::Result<ObjectHeader, String>> {
    let on_disk = fs::metadata(path)
        .with_context(|| format!("open {}", path.display()))?
        .len();
    let bytes = match slop_ninja_artifact_check::files::object_header(path) {
        Ok((_, bytes)) => bytes,
        Err(refused) => return Ok(Err(format!("artifact object is invalid: {refused}"))),
    };
    let object = match ObjectHeader::from_header_bytes(bytes) {
        Ok(object) => object,
        Err(error) => return Ok(Err(format!("artifact object is invalid: {error:#}"))),
    };
    if object.object_len != on_disk {
        return Ok(Err(format!(
            "artifact object is invalid: {} is {on_disk} bytes; its header describes {}",
            path.display(),
            object.object_len
        )));
    }
    Ok(Ok(object))
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_protocol::{Record, VerifiedArtifact};

    fn write(dir: &Path, name: &str, bytes: &[u8]) {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn roles_parse_and_name_each_other() {
        for (name, role) in ROLES {
            assert_eq!(parse_role(name).unwrap(), *role);
            assert_eq!(role_name(*role), *name);
        }
        assert!(parse_role("onnx").is_err());
    }

    #[test]
    fn pack_inputs_are_relative_role_paths() {
        let root = Path::new("/r");
        let input = PackInput::parse("graph:sub/model.onnx", root).unwrap();
        assert_eq!(input.name, "sub/model.onnx");
        assert_eq!(input.role, FileRole::Graph);
        assert_eq!(input.path, root.join("sub/model.onnx"));
        for bad in ["model.onnx", "graph:", "graph:/abs", "graph:a\\b", "nope:x"] {
            assert!(PackInput::parse(bad, root).is_err(), "{bad}");
        }
    }

    #[test]
    fn pack_matches_the_protocol_object() {
        let dir = tempfile::tempdir().unwrap();
        let graph: Vec<u8> = (0..(3u32 << 19)).map(|i| i as u8).collect();
        write(dir.path(), "g/model.onnx", &graph);
        write(dir.path(), "tokenizer.json", b"{}");
        let inputs = vec![
            PackInput::parse("tokenizer:tokenizer.json", dir.path()).unwrap(),
            PackInput::parse("graph:g/model.onnx", dir.path()).unwrap(),
        ];
        let out = dir.path().join("a.sna");
        let packed = pack("c-1", &inputs, &out).unwrap();
        let expected = slop_ninja_protocol::artifact_object(
            "c-1",
            &[
                ("g/model.onnx", FileRole::Graph, &graph),
                ("tokenizer.json", FileRole::Tokenizer, b"{}"),
            ],
        )
        .unwrap();
        assert_eq!(fs::read(&out).unwrap(), expected);
        // The pack-time digests are the protocol reader's (test only: the
        // reader re-hashes every file).
        let reread = VerifiedArtifact::from_bytes(&expected).unwrap();
        assert_eq!(packed.verified(), reread.verified_header());
        assert_eq!(packed.object_len, expected.len() as u64);
        assert_eq!(read_header(&out).unwrap(), packed);
        assert_eq!(packed.bytes, reread.header.to_bytes().unwrap());
        let (at, len) = packed.file_range(&reread.header.files[1].sha256).unwrap();
        assert_eq!(&expected[at as usize..(at + len) as usize], b"{}");
        // The object is private until its cohort's scoring starts.
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&out).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Never overwrites, and leaves no temporary file.
        assert!(pack("c-1", &inputs, &out).is_err());
        assert_eq!(fs::read(&out).unwrap(), expected);
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["a.sna", "g", "tokenizer.json"]);
    }

    #[test]
    fn pack_refuses_bad_inputs() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a", b"x");
        write(dir.path(), "empty", b"");
        let a = PackInput::parse("graph:a", dir.path()).unwrap();
        let out = dir.path().join("o");
        assert!(pack("c", &[], &out).is_err());
        assert!(
            pack("c", &[a.clone(), a.clone()], &out).is_err(),
            "duplicate name"
        );
        let empty = PackInput::parse("graph:empty", dir.path()).unwrap();
        assert!(pack("c", &[empty], &out).is_err(), "empty file");
        assert!(pack("", &[a], &out).is_err(), "empty contract");
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "no object or temporary file: {names:?}");
    }

    #[test]
    fn pack_dir_uses_the_spec_file_table() {
        use slop_ninja_artifact_check::spec;
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model");
        for role in &spec::ROLES {
            write(&model, role.name, role.name.as_bytes());
        }
        let out = dir.path().join("a.sna");
        let packed = pack_dir(&model, &out).unwrap();
        assert_eq!(packed.header.contract, spec::CONTRACT_ID);
        assert_eq!(packed.header.files.len(), spec::ROLES.len());
        for file in &packed.header.files {
            let role = spec::role_by_name(&file.name).unwrap();
            assert_eq!(file.role, role.file_role, "{}", file.name);
        }
        // An undeclared file, or a missing one, is refused with its code.
        write(&model, "notes.txt", b"x");
        let error = pack_dir(&model, &dir.path().join("b.sna")).unwrap_err();
        assert!(error.to_string().contains("undeclared_file"), "{error}");
        fs::remove_file(model.join("notes.txt")).unwrap();
        fs::remove_file(model.join("heads.json")).unwrap();
        let error = pack_dir(&model, &dir.path().join("c.sna")).unwrap_err();
        assert!(error.to_string().contains("missing_file"), "{error}");
    }

    #[test]
    fn read_header_refuses_truncated_and_trailing_objects() {
        let object =
            slop_ninja_protocol::artifact_object("c", &[("m", FileRole::Graph, b"graph")]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("o");
        fs::write(&path, &object[..object.len() - 1]).unwrap();
        assert!(read_header_checked(&path).unwrap().is_err());
        let mut longer = object.clone();
        longer.push(0);
        fs::write(&path, &longer).unwrap();
        assert!(read_header_checked(&path).unwrap().is_err());
        fs::write(&path, &object[..3]).unwrap();
        assert!(read_header_checked(&path).unwrap().is_err());
        fs::write(&path, &object).unwrap();
        assert_eq!(read_header(&path).unwrap().object_len, object.len() as u64);
        // A missing file is an error, not a refusal.
        assert!(read_header_checked(&dir.path().join("none")).is_err());
    }

    #[test]
    fn distinct_files_share_one_object() {
        let object = slop_ninja_protocol::artifact_object(
            "c",
            &[
                ("a", FileRole::Graph, b"same"),
                ("b", FileRole::OwnerMetadata, b"same"),
                ("c", FileRole::Tokenizer, b"other"),
            ],
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("o");
        fs::write(&path, &object).unwrap();
        let h = read_header(&path).unwrap();
        let distinct: Vec<&str> = h.distinct_files().iter().map(|f| f.name.as_str()).collect();
        assert_eq!(distinct, ["a", "c"]);
    }
}
