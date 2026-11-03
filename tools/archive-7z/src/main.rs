//! Stream one member of a 7z archive to stdout.
//!
//! `slop_ninja_7z_extract list ARCHIVE` prints one JSON line per member.
//! `slop_ninja_7z_extract cat ARCHIVE MEMBER [--receipt PATH]` writes the
//! member's exact bytes to stdout. Members before it are decoded and
//! discarded, so solid blocks stay aligned; the archive's CRC-32 is checked
//! as the member is read, and the optional receipt records the member's
//! size, CRC and SHA-256. The tool never writes anywhere else.
use anyhow::{Context, Result, bail};
use sevenz_rust2::{ArchiveReader, Password};
use sha2::{Digest, Sha256};
use std::io::{self, Write};
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["list", archive] => list(Path::new(archive), &mut io::stdout().lock()),
        ["cat", archive, member] => cat(Path::new(archive), member, None, &mut io::stdout().lock()),
        ["cat", archive, member, "--receipt", r] => {
            cat(Path::new(archive), member, Some(Path::new(r)), &mut io::stdout().lock())
        }
        _ => bail!("usage: slop_ninja_7z_extract list ARCHIVE | cat ARCHIVE MEMBER [--receipt PATH]"),
    }
}

fn open(archive: &Path) -> Result<ArchiveReader<std::fs::File>> {
    ArchiveReader::open(archive, Password::empty()).with_context(|| format!("open 7z {}", archive.display()))
}

pub fn list(archive: &Path, out: &mut impl Write) -> Result<()> {
    let r = open(archive)?;
    for f in &r.archive().files {
        let row = serde_json::json!({"name": f.name, "size": f.size, "has_crc": f.has_crc,
            "crc32": if f.has_crc { Some(format!("{:08x}", f.crc)) } else { None }, "is_directory": f.is_directory});
        writeln!(out, "{row}")?;
    }
    Ok(())
}

/// Write `member` to `out`; error when it is absent, duplicated or fails CRC.
pub fn cat(archive: &Path, member: &str, receipt: Option<&Path>, out: &mut impl Write) -> Result<()> {
    let mut r = open(archive)?;
    let matches = r.archive().files.iter().filter(|f| f.name == member).count();
    if matches != 1 {
        bail!("member {member:?} occurs {matches} times in {}", archive.display());
    }
    let mut found: Option<(u64, Option<u32>, String)> = None;
    let mut write_err: Option<io::Error> = None;
    let mut buf = vec![0u8; 1 << 20];
    r.for_each_entries(|entry, rd| {
        if entry.name != member {
            io::copy(rd, &mut io::sink())?;
            return Ok(true);
        }
        let mut h = Sha256::new();
        let mut n = 0u64;
        loop {
            let k = rd.read(&mut buf)?;
            if k == 0 {
                break;
            }
            h.update(&buf[..k]);
            n += k as u64;
            if let Err(e) = out.write_all(&buf[..k]) {
                write_err = Some(e);
                return Ok(false);
            }
        }
        found = Some((n, entry.has_crc.then_some(entry.crc as u32), hex::encode(h.finalize())));
        Ok(false)
    })
    .with_context(|| format!("decode {member:?} from {}", archive.display()))?;
    if let Some(e) = write_err {
        return Err(e).context("write member to output");
    }
    out.flush()?;
    let Some((size, crc, sha)) = found else { bail!("member {member:?} not decoded") };
    let declared = r.archive().files.iter().find(|f| f.name == member).map(|f| f.size).unwrap_or(0);
    if size != declared {
        bail!("member {member:?} decoded {size} bytes, archive declares {declared}");
    }
    if let Some(p) = receipt {
        let row = serde_json::json!({"schema": "slop_ninja_7z_member_receipt_v1", "archive": archive.display().to_string(),
            "member": member, "bytes": size, "crc32_verified": crc.map(|c| format!("{c:08x}")), "sha256": sha});
        std::fs::write(p, format!("{row}\n")).with_context(|| format!("write receipt {}", p.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter};

    fn archive(dir: &Path, solid: bool) -> std::path::PathBuf {
        let p = dir.join("t.7z");
        let mut w = ArchiveWriter::create(&p).unwrap();
        let files: Vec<(&str, Vec<u8>)> = vec![
            ("Users.xml", b"<users><row Id=\"1\" /></users>".to_vec()),
            ("Posts.xml", (0..50_000).map(|i| (i % 251) as u8).collect()),
            ("Tags.xml", b"<tags />".to_vec()),
        ];
        if solid {
            let entries = files.iter().map(|(n, _)| ArchiveEntry::new_file(n)).collect();
            let readers = files.iter().map(|(_, b)| sevenz_rust2::SourceReader::new(b.as_slice())).collect();
            w.push_archive_entries(entries, readers).unwrap();
        } else {
            for (n, b) in &files {
                w.push_archive_entry(ArchiveEntry::new_file(n), Some(b.as_slice())).unwrap();
            }
        }
        w.finish().unwrap();
        p
    }

    #[test]
    fn streams_exact_member_from_solid_and_non_solid_archives() {
        for solid in [true, false] {
            let d = tempfile::tempdir().unwrap();
            let p = archive(d.path(), solid);
            let mut out = Vec::new();
            let rc = d.path().join("r.json");
            cat(&p, "Posts.xml", Some(&rc), &mut out).unwrap();
            let want: Vec<u8> = (0..50_000).map(|i| (i % 251) as u8).collect();
            assert_eq!(out, want);
            let r: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&rc).unwrap()).unwrap();
            assert_eq!(r["bytes"], 50_000);
            assert_eq!(r["sha256"], hex::encode(Sha256::digest(&want)));
            let mut out = Vec::new();
            cat(&p, "Tags.xml", None, &mut out).unwrap();
            assert_eq!(out, b"<tags />");
            let mut l = Vec::new();
            list(&p, &mut l).unwrap();
            assert_eq!(String::from_utf8(l).unwrap().lines().count(), 3);
        }
    }

    #[test]
    fn missing_member_fails() {
        let d = tempfile::tempdir().unwrap();
        let p = archive(d.path(), true);
        assert!(cat(&p, "Comments.xml", None, &mut Vec::new()).is_err());
    }

    #[test]
    fn corrupted_archive_fails() {
        let d = tempfile::tempdir().unwrap();
        let p = archive(d.path(), true);
        let mut b = std::fs::read(&p).unwrap();
        let mid = 32 + (b.len() - 32) / 3;
        b[mid] ^= 0xff;
        std::fs::write(&p, b).unwrap();
        let r = cat(&p, "Posts.xml", None, &mut Vec::new());
        assert!(r.is_err());
    }
}
