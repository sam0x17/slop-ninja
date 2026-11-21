//! One resumable index upload per root step. SQL transactions end before
//! object I/O; only verified readback advances the durable cursor.

use super::store::Upload;
use slop_ninja_protocol::receipt_chunks::ReceiptIndexCommitment;

pub(crate) trait Io {
    fn prepare(&mut self) -> Result<Option<Upload>, &'static str>;
    fn finish(&mut self) -> Result<ReceiptIndexCommitment, &'static str>;
    fn confirm(&mut self, ordinal: u64, bytes: Vec<u8>) -> Result<(), &'static str>;
    async fn read(&mut self, key: &str, stage: &'static str) -> Result<Option<Vec<u8>>, &'static str>;
    async fn put(&mut self, key: &str, bytes: Vec<u8>) -> Result<(), &'static str>;
}

pub(crate) async fn step(io: &mut impl Io) -> Result<Option<ReceiptIndexCommitment>, &'static str> {
    if upload_page(io).await? {
        Ok(None)
    } else {
        io.finish().map(Some)
    }
}

/// A page scope can reconcile one upload, even if another invocation has
/// advanced the cursor. Only a finalization scope can enter finish().
pub(crate) async fn upload_page(io: &mut impl Io) -> Result<bool, &'static str> {
    let Some(upload) = io.prepare()? else {
        return Ok(false);
    };
    let stored = match io.read(&upload.key, "receipt index read").await? {
        Some(bytes) => bytes,
        None => {
            io.put(&upload.key, upload.bytes).await?;
            io.read(&upload.key, "receipt index readback").await?
                .ok_or("receipt index readback missing")?
        }
    };
    io.confirm(upload.ordinal, stored)?;
    Ok(true)
}
