//! Append-only zstd pack files holding content-addressed blobs.
//!
//! Every blob is one independent zstd frame, so a pack is also a valid zstd
//! stream. The SQLite `blob` table is the index (SHA-256 -> pack, offset,
//! compressed and raw length) and `pack.committed_len` is the durable end of
//! each pack. Bytes beyond it are an interrupted write and are truncated on
//! open, so a crash between the pack write and the database commit is safe.

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const PACK_LIMIT: u64 = 1 << 30;
pub const LEVEL: i32 = 6;

pub struct Packs {
    dir: PathBuf,
    cur_id: i64,
    cur: File,
    cur_len: u64,
    dirty: bool,
}

pub fn pack_path(dir: &Path, id: i64) -> PathBuf {
    dir.join(format!("pack-{id:06}.zst"))
}

impl Packs {
    /// Open the pack directory, truncating every unsealed pack to its committed length.
    pub fn open(db: &Connection, dir: &Path) -> Result<Packs> {
        fs::create_dir_all(dir)?;
        let mut open: Vec<(i64, u64)> = Vec::new();
        {
            let mut st = db.prepare("SELECT pack_id, committed_len FROM pack WHERE sealed = 0 ORDER BY pack_id")?;
            for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as u64)))? {
                open.push(r?);
            }
        }
        ensure!(open.len() <= 1, "more than one unsealed pack");
        let (cur_id, cur_len) = match open.first() {
            Some(x) => *x,
            None => {
                let next: i64 = db.query_row("SELECT COALESCE(MAX(pack_id), 0) + 1 FROM pack", [], |r| r.get(0))?;
                db.execute("INSERT INTO pack(pack_id, committed_len, sealed) VALUES (?1, 0, 0)", [next])?;
                (next, 0)
            }
        };
        let path = pack_path(dir, cur_id);
        let cur = OpenOptions::new().create(true).read(true).write(true).truncate(false).open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        let actual = cur.metadata()?.len();
        ensure!(actual >= cur_len, "pack {} is shorter ({actual}) than its committed length {cur_len}", path.display());
        if actual > cur_len {
            cur.set_len(cur_len)?;
            cur.sync_all()?;
        }
        Ok(Packs { dir: dir.to_path_buf(), cur_id, cur, cur_len, dirty: false })
    }

    pub fn has(db: &Connection, sha: &[u8; 32]) -> Result<bool> {
        Ok(db.prepare_cached("SELECT 1 FROM blob WHERE sha256 = ?1")?.query_row([&sha[..]], |_| Ok(())).optional()?.is_some())
    }

    /// Store `bytes` (already known to hash to `sha`) unless present. `zframe` is
    /// its zstd frame when the caller compressed it in parallel. Returns true when new.
    pub fn put(&mut self, db: &Connection, sha: &[u8; 32], bytes: &[u8], zframe: Option<Vec<u8>>) -> Result<bool> {
        if Self::has(db, sha)? {
            return Ok(false);
        }
        let z = match zframe {
            Some(z) => z,
            None => zstd::bulk::compress(bytes, LEVEL)?,
        };
        if self.cur_len > 0 && self.cur_len + z.len() as u64 > PACK_LIMIT {
            self.rotate(db)?;
        }
        self.cur.seek(SeekFrom::Start(self.cur_len))?;
        self.cur.write_all(&z)?;
        db.prepare_cached("INSERT INTO blob(sha256, pack_id, offset, zlen, len) VALUES (?1, ?2, ?3, ?4, ?5)")?
            .execute(params![&sha[..], self.cur_id, self.cur_len as i64, z.len() as i64, bytes.len() as i64])?;
        self.cur_len += z.len() as u64;
        self.dirty = true;
        Ok(true)
    }

    fn rotate(&mut self, db: &Connection) -> Result<()> {
        self.cur.sync_all()?;
        db.execute("UPDATE pack SET committed_len = ?2, sealed = 1 WHERE pack_id = ?1", params![self.cur_id, self.cur_len as i64])?;
        let next = self.cur_id + 1;
        db.execute("INSERT INTO pack(pack_id, committed_len, sealed) VALUES (?1, 0, 0)", [next])?;
        let path = pack_path(&self.dir, next);
        self.cur = OpenOptions::new().create(true).read(true).write(true).truncate(true).open(&path)?;
        self.cur_id = next;
        self.cur_len = 0;
        Ok(())
    }

    /// Make pack bytes durable and record the committed length. Call inside the
    /// transaction that also holds the blob rows, just before COMMIT.
    pub fn sync(&mut self, db: &Connection) -> Result<()> {
        if self.dirty {
            self.cur.sync_data()?;
            self.dirty = false;
        }
        db.prepare_cached("UPDATE pack SET committed_len = ?2 WHERE pack_id = ?1")?
            .execute(params![self.cur_id, self.cur_len as i64])?;
        Ok(())
    }
}

/// Read one blob and verify its SHA-256.
pub fn get(db: &Connection, dir: &Path, sha: &[u8; 32]) -> Result<Vec<u8>> {
    let (pack, off, zlen, len): (i64, i64, i64, i64) = db
        .prepare_cached("SELECT pack_id, offset, zlen, len FROM blob WHERE sha256 = ?1")?
        .query_row([&sha[..]], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .with_context(|| format!("blob {} not in store", hex::encode(sha)))?;
    let mut f = File::open(pack_path(dir, pack))?;
    f.seek(SeekFrom::Start(off as u64))?;
    let mut z = vec![0u8; zlen as usize];
    f.read_exact(&mut z)?;
    let raw = zstd::bulk::decompress(&z, len as usize)?;
    ensure!(raw.len() == len as usize && crate::skeleton::sha256(&raw) == *sha, "blob {} fails its digest", hex::encode(sha));
    Ok(raw)
}
