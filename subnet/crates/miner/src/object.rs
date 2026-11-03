//! Packing and verifying artifact objects.
//!
//! The object is the protocol's `u32_le(len(H)) || H || files`
//! (`slop_ninja_protocol::artifact`). [`pack_dir`] packs a model directory
//! under the *A* artifact spec's file table (six fixed names and roles, the
//! spec's output contract); [`pack`] packs any `ROLE:PATH` list. Packing
//! streams every file twice, once to hash it for the header and once to copy
//! it, and refuses a file that changed in between. The written object is then
//! verified by streaming it through the protocol's `ArtifactReader`, so the
//! object on disk is exactly what the coordinator will read. Nothing is held
//! in memory beyond one I/O buffer and the header.
//!
//! The object must stay secret until its cohort closes, so it is written with
//! mode 0600 like the openings.

use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest as _, Sha256};
use slop_ninja_protocol::{
    ArtifactFile, ArtifactHeader, ArtifactReader, Digest, FileRole, MAX_ARTIFACT_HEADER_LEN,
    Record, VerifiedArtifact,
};

/// Size of the streaming read buffer. It bounds memory, not object size.
const IO_BUFFER: usize = 1 << 20;

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

fn hash_file(path: &Path) -> Result<(Digest, u64)> {
    let mut reader = BufReader::with_capacity(
        IO_BUFFER,
        File::open(path).with_context(|| format!("open {}", path.display()))?,
    );
    let mut hash = Sha256::new();
    let mut len = 0u64;
    let mut buffer = vec![0u8; IO_BUFFER];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
        len += read as u64;
    }
    Ok((Digest(hash.finalize().into()), len))
}

/// The header for `inputs`, sorted by name as the protocol requires.
pub fn header_for(
    contract: &str,
    inputs: &[PackInput],
) -> Result<(ArtifactHeader, Vec<PackInput>)> {
    let mut sorted = inputs.to_vec();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for pair in sorted.windows(2) {
        ensure!(
            pair[0].name != pair[1].name,
            "file {:?} is listed twice",
            pair[0].name
        );
    }
    let mut files = Vec::with_capacity(sorted.len());
    for input in &sorted {
        let (sha256, len) = hash_file(&input.path)?;
        ensure!(len > 0, "file {} is empty", input.path.display());
        files.push(ArtifactFile {
            name: input.name.clone(),
            role: input.role,
            sha256,
            len,
        });
    }
    let header = ArtifactHeader {
        contract: contract.to_owned(),
        files,
    };
    header
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid artifact header: {error:?}"))?;
    Ok((header, sorted))
}

/// Writes the artifact object for `inputs` to `out` (which must not exist)
/// and returns it verified.
pub fn pack(contract: &str, inputs: &[PackInput], out: &Path) -> Result<VerifiedArtifact> {
    ensure!(!inputs.is_empty(), "nothing to pack");
    if out.exists() {
        bail!("{} exists; choose another output path", out.display());
    }
    let (header, sorted) = header_for(contract, inputs)?;
    let paths: Vec<PathBuf> = sorted.into_iter().map(|input| input.path).collect();
    write_object(&header, &paths, out)
}

/// Packs a model directory under the artifact spec's file table: exactly the
/// spec's six files, with their fixed roles and the spec's output contract. A
/// directory with any other entry, or without one of the files, is refused
/// with the artifact-check code (`undeclared_file`, `missing_file`).
pub fn pack_dir(dir: &Path, out: &Path) -> Result<VerifiedArtifact> {
    if out.exists() {
        bail!("{} exists; choose another output path", out.display());
    }
    let header = slop_ninja_artifact_check::fs::header_for_dir(dir)
        .map_err(|refused| anyhow::anyhow!("refused {}: {refused}", refused.code()))?;
    header
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid artifact header: {error:?}"))?;
    let paths: Vec<PathBuf> = header.files.iter().map(|f| dir.join(&f.name)).collect();
    write_object(&header, &paths, out)
}

/// Streams `header` and the files at `paths` (in header order) into `out`,
/// mode 0600, and verifies the result.
fn write_object(
    header: &ArtifactHeader,
    paths: &[PathBuf],
    out: &Path,
) -> Result<VerifiedArtifact> {
    ensure!(
        paths.len() == header.files.len(),
        "one path per header file"
    );
    let header_bytes = header
        .to_bytes()
        .map_err(|error| anyhow::anyhow!("encode header: {error:?}"))?;
    let header_len = u32::try_from(header_bytes.len())
        .ok()
        .filter(|&len| len <= MAX_ARTIFACT_HEADER_LEN)
        .context("artifact header exceeds the protocol's header bound")?;

    let partial = partial_path(out);
    let result = (|| -> Result<()> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)
            .with_context(|| format!("create {}", partial.display()))?;
        let mut writer = BufWriter::with_capacity(IO_BUFFER, file);
        writer.write_all(&header_len.to_le_bytes())?;
        writer.write_all(&header_bytes)?;
        let mut buffer = vec![0u8; IO_BUFFER];
        for (path, entry) in paths.iter().zip(&header.files) {
            let mut reader = File::open(path)?;
            let mut hash = Sha256::new();
            let mut len = 0u64;
            loop {
                let read = reader.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hash.update(&buffer[..read]);
                len += read as u64;
                writer.write_all(&buffer[..read])?;
            }
            ensure!(
                len == entry.len && Digest(hash.finalize().into()) == entry.sha256,
                "{} changed while it was packed",
                path.display()
            );
        }
        let file = writer.into_inner().map_err(|error| error.into_error())?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&partial);
        return Err(error);
    }
    let verified = match verify_file(&partial) {
        Ok(verified) => verified,
        Err(error) => {
            let _ = fs::remove_file(&partial);
            return Err(error);
        }
    };
    ensure!(
        verified.header == *header,
        "packed header did not read back"
    );
    fs::rename(&partial, out).with_context(|| format!("rename to {}", out.display()))?;
    Ok(verified)
}

fn partial_path(out: &Path) -> PathBuf {
    let mut name = out
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".partial");
    out.with_file_name(name)
}

/// Streams an object file through the protocol's `ArtifactReader`.
pub fn verify_file(path: &Path) -> Result<VerifiedArtifact> {
    verify_file_checked(path)?.map_err(anyhow::Error::msg)
}

/// Like [`verify_file`], but separates the two failures: the outer error is
/// a file that cannot be read; the inner one an object that does not parse
/// or verify (a refusal, with its message).
pub fn verify_file_checked(path: &Path) -> Result<std::result::Result<VerifiedArtifact, String>> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader = ArtifactReader::new();
    let mut buffer = vec![0u8; IO_BUFFER];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        if let Err(error) = reader.update(&buffer[..read]) {
            return Ok(Err(format!("artifact object is invalid: {error:?}")));
        }
    }
    Ok(reader
        .finish()
        .map_err(|error| format!("artifact object is invalid: {error:?}")))
}

/// Reads exactly `len` bytes at `offset`.
pub fn read_range(path: &Path, offset: u64, len: u64) -> Result<Vec<u8>> {
    use std::io::{Seek, SeekFrom};
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    file.seek(SeekFrom::Start(offset))?;
    let len = usize::try_from(len).context("part does not fit in memory on this platform")?;
    let mut out = vec![0u8; len];
    file.read_exact(&mut out)
        .with_context(|| format!("read {len} bytes at {offset} of {}", path.display()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn pack_matches_the_protocol_object_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let graph: Vec<u8> = (0..(3 * IO_BUFFER as u32 / 2)).map(|i| i as u8).collect();
        write(dir.path(), "g/model.onnx", &graph);
        write(dir.path(), "tokenizer.json", b"{}");
        let inputs = vec![
            PackInput::parse("tokenizer:tokenizer.json", dir.path()).unwrap(),
            PackInput::parse("graph:g/model.onnx", dir.path()).unwrap(),
        ];
        let out = dir.path().join("a.sna");
        let verified = pack("c-1", &inputs, &out).unwrap();
        let expected = slop_ninja_protocol::artifact_object(
            "c-1",
            &[
                ("g/model.onnx", FileRole::Graph, &graph),
                ("tokenizer.json", FileRole::Tokenizer, b"{}"),
            ],
        )
        .unwrap();
        assert_eq!(fs::read(&out).unwrap(), expected);
        assert_eq!(verified, VerifiedArtifact::from_bytes(&expected).unwrap());
        assert!(!partial_path(&out).exists());
        // The object is private until its cohort closes.
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&out).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Never overwrites.
        assert!(pack("c-1", &inputs, &out).is_err());
        // Reads ranges exactly.
        assert_eq!(read_range(&out, 4, 4).unwrap(), expected[4..8].to_vec());
        assert!(read_range(&out, expected.len() as u64 - 1, 2).is_err());
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
        assert!(!out.exists() && !partial_path(&out).exists());
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
        let verified = pack_dir(&model, &out).unwrap();
        assert_eq!(verified.header.contract, spec::CONTRACT_ID);
        assert_eq!(verified.header.files.len(), spec::ROLES.len());
        for file in &verified.header.files {
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
    fn verify_refuses_truncated_and_trailing_objects() {
        let object =
            slop_ninja_protocol::artifact_object("c", &[("m", FileRole::Graph, b"graph")]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("o");
        fs::write(&path, &object[..object.len() - 1]).unwrap();
        assert!(verify_file(&path).is_err());
        let mut longer = object.clone();
        longer.push(0);
        fs::write(&path, &longer).unwrap();
        assert!(verify_file(&path).is_err());
        fs::write(&path, &object).unwrap();
        assert_eq!(verify_file(&path).unwrap().len, object.len() as u64);
    }
}
