//! Coordinator-private, chunked receipt indexes. This format is not activated
//! by defining it: manifests and every producer/consumer must select it together.
//!
//! A v2 signed commitment binds the manifest, total receipt count and an ordered
//! digest of bounded index chunks. Each chunk binds its position and exact
//! entries; each entry binds the retained receipt bytes. Receipts remain private.
//! V1 indexes and their roots keep their existing encoding and meaning.

use sha2::{Digest as _, Sha256};

use crate::codec::canonical_struct;
use crate::{
    AttemptReceipt, Canonical, Decoder, Digest, Encoder, Error, KeyRole, ReceiptIndexEntry, Record,
    Result, SignedStream, TrustedKeys, object_sha256,
};

/// V1 chunk records fit one 64 KiB storage/read page. This is a versioned
/// per-object working-set bound, not a cap on cohort receipts.
pub const CHUNK_BYTES: usize = 64 << 10;
const CHUNK_KIND: &str = "receipt-index-chunk";
const ENTRY_BYTES: usize = 32 + 1 + 32 + 4;
const CHUNK_HEADER_BYTES: usize = 4 + CHUNK_KIND.len() + 2 + 32 + 8 + 4;
/// Derived from the record byte budget and fixed entry encoding.
pub const CHUNK_ENTRIES: usize = (CHUNK_BYTES - CHUNK_HEADER_BYTES) / ENTRY_BYTES;
const CHUNKS_DOMAIN: &str = "slop_ninja.protocol/receipt-index-chunks/v1";

/// The complete v2 record is fixed width. Readers refuse any other length,
/// even when a trusted key signed it.
pub const COMMITMENT_BYTES: usize = 4 + "receipt-index".len() + 2 + 32 + 8 + 32;

/// Canonical chunks are full except for the final chunk. Entries are strictly
/// increasing by attempt ID across the entire index, including chunk boundaries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptIndexChunk {
    pub cohort_manifest: Digest,
    pub ordinal: u64,
    pub entries: Vec<ReceiptIndexEntry>,
}

impl Canonical for ReceiptIndexChunk {
    fn encode_to(&self, e: &mut Encoder) {
        self.cohort_manifest.encode_to(e);
        e.u64(self.ordinal);
        self.entries.encode_to(e);
    }

    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        let cohort_manifest = Digest::decode_from(d)?;
        let ordinal = d.u64()?;
        // Reject an oversized count before allocating or decoding entries.
        let count = d.len_prefix("receipt chunk entries", CHUNK_ENTRIES as u32)?;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            entries.push(ReceiptIndexEntry::decode_from(d)?);
        }
        Ok(Self {
            cohort_manifest,
            ordinal,
            entries,
        })
    }
}

impl Record for ReceiptIndexChunk {
    const KIND: &'static str = CHUNK_KIND;
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = None;

    fn validate(&self) -> Result<()> {
        if self.entries.is_empty() || self.entries.len() > CHUNK_ENTRIES {
            return Err(Error::OutOfRange("receipt chunk entries"));
        }
        for pair in self.entries.windows(2) {
            ordered(pair[0].attempt, pair[1].attempt)?;
        }
        Ok(())
    }
}

fn ordered(previous: Digest, next: Digest) -> Result<()> {
    match previous.cmp(&next) {
        core::cmp::Ordering::Less => Ok(()),
        core::cmp::Ordering::Equal => Err(Error::Duplicate("receipt attempt")),
        core::cmp::Ordering::Greater => Err(Error::Unsorted("receipt attempt")),
    }
}

canonical_struct! {
    /// The signed v2 receipt index has constant size. Its record digest is the
    /// v2 receipt root. It never claims to be the digest of a flat v1 index.
    pub struct ReceiptIndexCommitment {
        pub cohort_manifest: Digest,
        pub receipt_count: u64,
        pub chunks_digest: Digest,
    }
}

impl Record for ReceiptIndexCommitment {
    const KIND: &'static str = "receipt-index";
    const VERSION: u16 = 2;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Receipt);

    fn validate(&self) -> Result<()> {
        if self.receipt_count == 0
            && self.chunks_digest
                != ReceiptIndexBuilder::new(self.cohort_manifest)
                    .finish()
                    .chunks_digest
        {
            return Err(Error::Mismatch("empty receipt index"));
        }
        Ok(())
    }
}

impl ReceiptIndexCommitment {
    /// Rebuilds the commitment from an already verified, ordered chunk ledger.
    /// Checks the number of chunk digests without retaining them. The caller
    /// must have verified each chunk's manifest, ordinal, entries and boundary
    /// order before writing that ledger; hashes alone cannot establish those.
    pub fn from_verified_chunk_digests(
        cohort_manifest: Digest,
        receipt_count: u64,
        digests: impl IntoIterator<Item = Result<Digest>>,
    ) -> Result<Self> {
        let mut hash = Sha256::new();
        hash.update((CHUNKS_DOMAIN.len() as u32).to_le_bytes());
        hash.update(CHUNKS_DOMAIN.as_bytes());
        hash.update(cohort_manifest.0);
        let expected = receipt_count.div_ceil(CHUNK_ENTRIES as u64);
        let mut count = 0_u64;
        for digest in digests {
            if count == expected {
                return Err(Error::Mismatch("extra receipt chunk digest"));
            }
            hash.update(digest?.0);
            count += 1;
        }
        if count != expected {
            return Err(Error::Mismatch("missing receipt chunk digest"));
        }
        Ok(Self {
            cohort_manifest,
            receipt_count,
            chunks_digest: Digest(hash.finalize().into()),
        })
    }

    pub fn chunk_count(&self) -> u64 {
        self.receipt_count.div_ceil(CHUNK_ENTRIES as u64)
    }

    /// Binding checked by the coordinator's target installation path. A caller
    /// must authenticate the commitment first; this method does not verify a
    /// signature or the retained receipt objects.
    pub fn binding(&self) -> Result<crate::ReceiptIndexBinding> {
        Ok(crate::ReceiptIndexBinding {
            cohort_manifest: self.cohort_manifest,
            root: self.digest()?,
            count: self.receipt_count,
        })
    }
}

/// Authenticates a v2 commitment from arbitrarily split transport bytes while
/// retaining at most the fixed record and signed-envelope framing. This reader
/// is selected only for a format that the caller already permits. It never
/// retries a rejected signature as a legacy v1 index.
pub struct ReceiptCommitmentStream(SignedStream<ReceiptIndexCommitment>);

impl Default for ReceiptCommitmentStream {
    fn default() -> Self {
        Self::new()
    }
}

impl ReceiptCommitmentStream {
    pub fn new() -> Self {
        Self(SignedStream::new(COMMITMENT_BYTES))
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    pub fn finish(self, keys: &TrustedKeys) -> Result<ReceiptIndexCommitment> {
        let record = self.0.finish(keys)?;
        if record.len != COMMITMENT_BYTES || record.prefix.len() != COMMITMENT_BYTES {
            return Err(Error::Mismatch("receipt commitment length"));
        }
        let commitment = ReceiptIndexCommitment::from_bytes(&record.prefix)?;
        if commitment.digest()? != record.digest {
            return Err(Error::Mismatch("receipt commitment digest"));
        }
        Ok(commitment)
    }
}

/// Constant-size accumulator over index chunks. `push` checks index structure;
/// it does not establish that the receipt objects exist. Before installing a
/// target, use `push_verified` for every chunk and `verify` against the signed
/// commitment, or retain equivalent durable evidence of those checks.
pub struct ReceiptIndexBuilder {
    cohort_manifest: Digest,
    hash: Sha256,
    count: u64,
    chunks: u64,
    last: Option<Digest>,
    short_tail: bool,
}

impl ReceiptIndexBuilder {
    pub fn new(cohort_manifest: Digest) -> Self {
        let mut hash = Sha256::new();
        hash.update((CHUNKS_DOMAIN.len() as u32).to_le_bytes());
        hash.update(CHUNKS_DOMAIN.as_bytes());
        hash.update(cohort_manifest.0);
        Self {
            cohort_manifest,
            hash,
            count: 0,
            chunks: 0,
            last: None,
            short_tail: false,
        }
    }

    /// Failed pushes leave the accumulator unchanged.
    pub fn push(&mut self, chunk: &ReceiptIndexChunk) -> Result<()> {
        chunk.validate()?;
        if chunk.cohort_manifest != self.cohort_manifest || chunk.ordinal != self.chunks {
            return Err(Error::Mismatch("receipt chunk position"));
        }
        if self.short_tail {
            return Err(Error::Invalid("receipt chunk after short tail"));
        }
        if let Some(last) = self.last {
            ordered(last, chunk.entries[0].attempt)?;
        }
        let count = self
            .count
            .checked_add(chunk.entries.len() as u64)
            .ok_or(Error::Overflow("receipt count"))?;
        let chunks = self
            .chunks
            .checked_add(1)
            .ok_or(Error::Overflow("receipt chunks"))?;
        let digest = chunk.digest()?;
        self.hash.update(digest.0);
        self.count = count;
        self.chunks = chunks;
        self.last = chunk.entries.last().map(|entry| entry.attempt);
        self.short_tail = chunk.entries.len() < CHUNK_ENTRIES;
        Ok(())
    }

    /// Verifies exactly one object for each entry, in entry order, then commits
    /// the chunk to the accumulator. A missing, extra or mismatched object fails
    /// without advancing. The caller can release each chunk after this call.
    pub fn push_verified<'a>(
        &mut self,
        chunk: &ReceiptIndexChunk,
        objects: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<()> {
        chunk.validate()?;
        let mut objects = objects.into_iter();
        for entry in &chunk.entries {
            let bytes = objects
                .next()
                .ok_or(Error::Mismatch("missing receipt object"))?;
            if bytes.len() as u64 != u64::from(entry.object_len)
                || object_sha256(bytes) != entry.object_sha256
            {
                return Err(Error::Mismatch("receipt object digest"));
            }
            let receipt = AttemptReceipt::from_bytes(bytes)?;
            if receipt.attempt != entry.attempt || receipt.outcome != entry.outcome {
                return Err(Error::Mismatch("receipt object entry"));
            }
        }
        if objects.next().is_some() {
            return Err(Error::Mismatch("extra receipt object"));
        }
        self.push(chunk)
    }

    pub fn finish(self) -> ReceiptIndexCommitment {
        ReceiptIndexCommitment {
            cohort_manifest: self.cohort_manifest,
            receipt_count: self.count,
            chunks_digest: Digest(self.hash.finalize().into()),
        }
    }

    pub fn verify(self, expected: &ReceiptIndexCommitment) -> Result<()> {
        expected.validate()?;
        if self.finish() != *expected {
            return Err(Error::Mismatch("receipt index commitment"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
