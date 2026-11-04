//! Domain-separated Ed25519 signatures over record digests.
//!
//! Application signing keys are distinct from chain hotkeys and from each
//! other: a package key cannot sign a receipt index, and a receipt key cannot
//! sign a score package. The signing payload is
//! `str("slop_ninja.protocol.signature/v1") || u8(role) || str(record domain) || record digest`.

use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};

use crate::codec::{Decoder, Encoder};
use crate::records::Record;
use crate::{Digest, Error, Result, domain_hash};

const SIGNATURE_DOMAIN: &str = "slop_ninja.protocol.signature/v1";
const ENVELOPE_KIND: &str = "signed";
const ENVELOPE_VERSION: u16 = 1;
/// Largest signed record accepted in one envelope: [`Signed::to_bytes`]
/// refuses to encode a longer record and the decoder refuses to read one.
pub const MAX_SIGNED_RECORD_LEN: u32 = 64 << 20;

/// Purpose of an application signing key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRole {
    /// Signs cohort manifests, score packages and calculation targets.
    Package,
    /// Signs receipt indexes.
    Receipt,
    /// Signs validator authorization records: reward-intent authorizations
    /// and their renewals.
    Authorization,
}

impl KeyRole {
    fn tag(self) -> u8 {
        match self {
            Self::Package => 1,
            Self::Receipt => 2,
            Self::Authorization => 3,
        }
    }
}

/// Public keys a verifier accepts, one per role. Rotation replaces this set
/// under a new authorization; records keep the key that signed them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedKeys {
    package: [u8; 32],
    receipt: [u8; 32],
    authorization: [u8; 32],
}

impl TrustedKeys {
    pub fn new(package: [u8; 32], receipt: [u8; 32], authorization: [u8; 32]) -> Result<Self> {
        if package == receipt || package == authorization || receipt == authorization {
            return Err(Error::DuplicateTrustedKey);
        }
        Ok(Self {
            package,
            receipt,
            authorization,
        })
    }
    pub fn key(&self, role: KeyRole) -> &[u8; 32] {
        match role {
            KeyRole::Package => &self.package,
            KeyRole::Receipt => &self.receipt,
            KeyRole::Authorization => &self.authorization,
        }
    }
}

/// Bytes a key holder signs for `record`.
pub fn signing_payload<R: Record>(record: &R) -> Result<Vec<u8>> {
    payload_for_digest::<R>(&record.digest()?)
}

fn payload_for_digest<R: Record>(digest: &Digest) -> Result<Vec<u8>> {
    let role = R::SIGNER.ok_or(Error::WrongRole)?;
    let mut e = Encoder::new();
    e.str(SIGNATURE_DOMAIN);
    e.u8(role.tag());
    e.str(&R::domain());
    e.raw(&digest.0);
    Ok(e.into_bytes())
}

/// Checks that `public_key` is trusted for `R`'s role and that `signature` is
/// valid for `digest` under strict (non-malleable) Ed25519 verification.
fn verify_digest<R: Record>(
    keys: &TrustedKeys,
    digest: &Digest,
    public_key: &[u8; 32],
    signature: &[u8; 64],
) -> Result<()> {
    let role = R::SIGNER.ok_or(Error::WrongRole)?;
    if keys.key(role) != public_key {
        return Err(Error::UntrustedKey);
    }
    let key = VerifyingKey::from_bytes(public_key).map_err(|_| Error::BadPublicKey)?;
    key.verify_strict(
        &payload_for_digest::<R>(digest)?,
        &Signature::from_bytes(signature),
    )
    .map_err(|_| Error::BadSignature)
}

/// An envelope split into its undecoded record bytes, key and signature.
struct Envelope<'a> {
    record: &'a [u8],
    public_key: [u8; 32],
    signature: [u8; 64],
}

impl<'a> Envelope<'a> {
    fn parse<R: Record>(bytes: &'a [u8]) -> Result<Self> {
        let role = R::SIGNER.ok_or(Error::WrongRole)?;
        let mut d = Decoder::new(bytes);
        if d.str()? != ENVELOPE_KIND {
            return Err(Error::WrongKind {
                expected: ENVELOPE_KIND,
            });
        }
        let version = d.u16()?;
        if version != ENVELOPE_VERSION {
            return Err(Error::WrongVersion {
                kind: ENVELOPE_KIND,
                version,
            });
        }
        if d.u8()? != role.tag() {
            return Err(Error::WrongRole);
        }
        let len = d.len_prefix("signed record", MAX_SIGNED_RECORD_LEN)?;
        let record = d.take(len)?;
        let public_key = d.bytes32()?;
        let signature = d.bytes64()?;
        d.finish()?;
        Ok(Self {
            record,
            public_key,
            signature,
        })
    }
}

/// Signs with a deterministic Ed25519 key. Key custody is the caller's.
pub fn sign<R: Record>(record: R, key: &SigningKey) -> Result<Signed<R>> {
    let signature = key.sign(&signing_payload(&record)?);
    Ok(Signed {
        record,
        public_key: key.verifying_key().to_bytes(),
        signature: signature.to_bytes(),
    })
}

/// A record with its signer's public key and signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signed<R> {
    pub record: R,
    pub public_key: [u8; 32],
    pub signature: [u8; 64],
}

/// A signed envelope for `R` over raw record bytes that need not decode as
/// `R` (a record a newer schema wrote), for tests of readers that must tell
/// a valid signature from a record they cannot decode.
#[cfg(any(test, feature = "fixtures"))]
pub fn sign_raw<R: Record>(record: &[u8], key: &SigningKey) -> Result<Vec<u8>> {
    let role = R::SIGNER.ok_or(Error::WrongRole)?;
    let signature = key.sign(&payload_for_digest::<R>(&domain_hash(&R::domain(), record))?);
    let mut e = Encoder::new();
    e.str(ENVELOPE_KIND);
    e.u16(ENVELOPE_VERSION);
    e.u8(role.tag());
    e.len_prefix(record.len());
    e.raw(record);
    e.raw(&key.verifying_key().to_bytes());
    e.raw(&signature.to_bytes());
    Ok(e.into_bytes())
}

impl<R: Record> Signed<R> {
    /// `str("signed") || u16(1) || u8(role) || u32 len || record bytes || key || signature`.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let role = R::SIGNER.ok_or(Error::WrongRole)?;
        let record = self.record.to_bytes()?;
        if record.len() > MAX_SIGNED_RECORD_LEN as usize {
            return Err(Error::LengthLimit {
                field: "signed record",
                len: record.len() as u64,
            });
        }
        let mut e = Encoder::new();
        e.str(ENVELOPE_KIND);
        e.u16(ENVELOPE_VERSION);
        e.u8(role.tag());
        e.len_prefix(record.len());
        e.raw(&record);
        e.raw(&self.public_key);
        e.raw(&self.signature);
        Ok(e.into_bytes())
    }

    /// Strict structural decode. Call [`Signed::verify`] before trusting it,
    /// or use [`Signed::open`], which authenticates before decoding.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let envelope = Envelope::parse::<R>(bytes)?;
        Ok(Self {
            record: R::from_bytes(envelope.record)?,
            public_key: envelope.public_key,
            signature: envelope.signature,
        })
    }

    /// Checks the signer is the trusted key for this record's role and the
    /// signature is valid under strict (non-malleable) Ed25519 verification.
    pub fn verify(&self, keys: &TrustedKeys) -> Result<&R> {
        verify_digest::<R>(
            keys,
            &self.record.digest()?,
            &self.public_key,
            &self.signature,
        )?;
        Ok(&self.record)
    }

    /// Verifies the signature over the raw record bytes, then decodes them, so
    /// unauthenticated bytes are never decoded. Decoding is canonical, so the
    /// digest of the raw bytes equals the digest of every record they decode to.
    pub fn open(bytes: &[u8], keys: &TrustedKeys) -> Result<R> {
        R::from_bytes(Self::open_raw(bytes, keys)?)
    }

    /// Verifies the signature over the raw record bytes and returns those
    /// bytes undecoded, for a reader that walks a large record in place
    /// instead of decoding it into memory. The bytes are authenticated, not
    /// validated: the caller applies the record's canonical decoding rules.
    pub fn open_raw<'b>(bytes: &'b [u8], keys: &TrustedKeys) -> Result<&'b [u8]> {
        let envelope = Envelope::parse::<R>(bytes)?;
        let digest = domain_hash(&R::domain(), envelope.record);
        verify_digest::<R>(keys, &digest, &envelope.public_key, &envelope.signature)?;
        Ok(envelope.record)
    }
}

/// The envelope's fixed header: `str("signed") || u16(1) || u8(role) || u32 len`.
const STREAM_HEAD_LEN: usize = 4 + ENVELOPE_KIND.len() + 2 + 1 + 4;
/// The envelope's trailer: the public key and the signature.
const STREAM_TAIL_LEN: usize = 32 + 64;

/// What [`SignedStream::finish`] authenticated: the record's digest (equal
/// to [`Record::digest`] of the record it decodes to), its first bytes and
/// its length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamedRecord {
    pub digest: Digest,
    pub prefix: Vec<u8>,
    pub len: usize,
}

/// Authenticates a signed envelope fed in pieces, as [`Signed::open_raw`]
/// does for one held whole. It holds only the envelope's header, the first
/// `keep` bytes of the record, the record's running digest and the trailing
/// key and signature, so a record of any admitted length is checked in
/// constant memory. The record is authenticated, not decoded: the caller
/// reads what it needs from the prefix under the record's own encoding.
pub struct SignedStream<R> {
    head: Vec<u8>,
    record_len: Option<usize>,
    hashed: usize,
    keep: usize,
    prefix: Vec<u8>,
    tail: Vec<u8>,
    trailing: bool,
    malformed: Option<Error>,
    hasher: sha2::Sha256,
    _record: core::marker::PhantomData<fn() -> R>,
}

impl<R: Record> SignedStream<R> {
    pub fn new(keep: usize) -> Self {
        use sha2::Digest as _;
        let domain = R::domain();
        let mut hasher = sha2::Sha256::new();
        hasher.update((domain.len() as u32).to_le_bytes());
        hasher.update(domain.as_bytes());
        Self {
            head: Vec::with_capacity(STREAM_HEAD_LEN),
            record_len: None,
            hashed: 0,
            keep,
            prefix: Vec::new(),
            tail: Vec::with_capacity(STREAM_TAIL_LEN),
            trailing: false,
            malformed: None,
            hasher,
            _record: core::marker::PhantomData,
        }
    }

    /// Feeds the envelope's next bytes.
    pub fn update(&mut self, mut chunk: &[u8]) {
        use sha2::Digest as _;
        if self.malformed.is_some() || self.trailing {
            return;
        }
        if self.record_len.is_none() {
            let n = (STREAM_HEAD_LEN - self.head.len()).min(chunk.len());
            self.head.extend_from_slice(&chunk[..n]);
            chunk = &chunk[n..];
            if self.head.len() < STREAM_HEAD_LEN {
                return;
            }
            match Self::parse_head(&self.head) {
                Ok(len) => self.record_len = Some(len),
                Err(e) => {
                    self.malformed = Some(e);
                    return;
                }
            }
        }
        let record_len = self.record_len.unwrap_or(0);
        if self.hashed < record_len && !chunk.is_empty() {
            let n = (record_len - self.hashed).min(chunk.len());
            let part = &chunk[..n];
            self.hasher.update(part);
            if self.prefix.len() < self.keep {
                let k = (self.keep - self.prefix.len()).min(n);
                self.prefix.extend_from_slice(&part[..k]);
            }
            self.hashed += n;
            chunk = &chunk[n..];
        }
        if !chunk.is_empty() {
            let n = (STREAM_TAIL_LEN - self.tail.len()).min(chunk.len());
            self.tail.extend_from_slice(&chunk[..n]);
            self.trailing = n < chunk.len();
        }
    }

    fn parse_head(head: &[u8]) -> Result<usize> {
        let role = R::SIGNER.ok_or(Error::WrongRole)?;
        let mut d = Decoder::new(head);
        if d.str()? != ENVELOPE_KIND {
            return Err(Error::WrongKind { expected: ENVELOPE_KIND });
        }
        let version = d.u16()?;
        if version != ENVELOPE_VERSION {
            return Err(Error::WrongVersion { kind: ENVELOPE_KIND, version });
        }
        if d.u8()? != role.tag() {
            return Err(Error::WrongRole);
        }
        let len = d.len_prefix("signed record", MAX_SIGNED_RECORD_LEN)?;
        d.finish()?;
        Ok(len)
    }

    /// Verifies the whole envelope arrived (nothing after it) and its
    /// signature is the trusted key's for this record's role.
    pub fn finish(self, keys: &TrustedKeys) -> Result<StreamedRecord> {
        use sha2::Digest as _;
        if let Some(e) = self.malformed {
            return Err(e);
        }
        if self.trailing {
            return Err(Error::TrailingBytes);
        }
        let Some(len) = self.record_len else {
            return Err(Error::UnexpectedEnd);
        };
        if self.hashed != len || self.tail.len() != STREAM_TAIL_LEN {
            return Err(Error::UnexpectedEnd);
        }
        let digest = Digest(self.hasher.finalize().into());
        let public_key: [u8; 32] = self.tail[..32].try_into().map_err(|_| Error::UnexpectedEnd)?;
        let signature: [u8; 64] = self.tail[32..].try_into().map_err(|_| Error::UnexpectedEnd)?;
        verify_digest::<R>(keys, &digest, &public_key, &signature)?;
        Ok(StreamedRecord { digest, prefix: self.prefix, len })
    }
}
