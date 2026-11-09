//! The offline operator-decision key: what the operator signs offline for a
//! high-stakes operator action, and how the evaluation service and the
//! coordinator verify it. Nothing here signs with that key outside tests:
//! the key never reaches a Worker, a validator or the publisher.
//!
//! **Key format.** An OpenSSH Ed25519 key. The public half is configured in
//! the two Workers' trust configuration as its OpenSSH public-key line
//! (`ssh-ed25519 AAAA... comment`, [`OperatorKey::parse`]); the private half
//! stays on an offline device with a recorded backup. It is made with
//! `ssh-keygen -t ed25519` and signs with `ssh-keygen -Y sign -n
//! slop-ninja-operator-decision` ([`OPERATOR_NAMESPACE`]), so no project
//! tool runs on the offline device.
//!
//! **What is signed.** A [`Statement`]: a short text naming the action, the
//! decision (its id and the SHA-256 of its record), the SHA-256 of the
//! request body it authorizes, and where it is addressed (the service, the
//! authority, the store, the chain binding and the subnet registration). The
//! verifier checks the SSHSIG signature over the statement's exact bytes
//! ([`OperatorKey::verify`]) and then compares every field with the request
//! and with its own identity, so a signature can neither be attached to
//! another body nor be replayed into another store.

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest as _, Sha256, Sha512};

use crate::{Digest, Error, Result};

/// The SSHSIG namespace of an operator decision (`ssh-keygen -Y sign -n`).
pub const OPERATOR_NAMESPACE: &str = "slop-ninja-operator-decision";
/// The first line of every [`Statement`].
pub const STATEMENT_HEADER: &str = "slop_ninja operator decision v1";
/// Longest statement text accepted.
pub const MAX_STATEMENT_LEN: usize = 4096;
/// Longest armored signature accepted.
pub const MAX_ARMORED_SIGNATURE_LEN: usize = 4096;

const SSHSIG_MAGIC: &[u8; 6] = b"SSHSIG";
const SSHSIG_VERSION: u32 = 1;
const ARMOR_BEGIN: &str = "-----BEGIN SSH SIGNATURE-----";
const ARMOR_END: &str = "-----END SSH SIGNATURE-----";
const ED25519: &str = "ssh-ed25519";

/// The operator-decision public key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperatorKey([u8; 32]);

impl OperatorKey {
    pub fn from_bytes(key: [u8; 32]) -> Result<Self> {
        VerifyingKey::from_bytes(&key).map_err(|_| Error::BadPublicKey)?;
        Ok(Self(key))
    }

    /// An OpenSSH public-key line (`ssh-ed25519 <base64> [comment]`) or the
    /// key's 64 hex digits.
    pub fn parse(text: &str) -> Result<Self> {
        let t = text.trim();
        if t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()) {
            let mut key = [0u8; 32];
            for (i, chunk) in t.as_bytes().chunks(2).enumerate() {
                let s = core::str::from_utf8(chunk).map_err(|_| Error::Invalid("operator key"))?;
                key[i] = u8::from_str_radix(s, 16).map_err(|_| Error::Invalid("operator key"))?;
            }
            return Self::from_bytes(key);
        }
        let mut parts = t.split_ascii_whitespace();
        if parts.next() != Some(ED25519) {
            return Err(Error::Invalid("operator key type"));
        }
        let blob = base64_decode(parts.next().ok_or(Error::Invalid("operator key"))?)?;
        let mut r = SshReader(&blob);
        if r.string()? != ED25519.as_bytes() {
            return Err(Error::Invalid("operator key type"));
        }
        let key: [u8; 32] = r.string()?.try_into().map_err(|_| Error::Invalid("operator key"))?;
        r.finish()?;
        Self::from_bytes(key)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The OpenSSH public-key blob (`string "ssh-ed25519" || string key`).
    fn blob(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(51);
        put_string(&mut b, ED25519.as_bytes());
        put_string(&mut b, &self.0);
        b
    }

    /// The fingerprint `ssh-keygen -l` prints: `SHA256:` and the unpadded
    /// base64 of the blob's SHA-256.
    pub fn fingerprint(&self) -> String {
        let d = Sha256::digest(self.blob());
        format!("SHA256:{}", base64_encode(&d).trim_end_matches('='))
    }

    /// The OpenSSH public-key line, with `comment`.
    pub fn to_openssh(&self, comment: &str) -> String {
        format!("{ED25519} {} {comment}", base64_encode(&self.blob())).trim_end().to_owned()
    }

    /// Verifies an armored SSHSIG signature (`ssh-keygen -Y sign -n
    /// slop-ninja-operator-decision`) over `message` by this key: the
    /// signature names this key and the operator namespace, hashes with
    /// SHA-512 or SHA-256, and verifies under strict Ed25519 rules.
    pub fn verify(&self, message: &[u8], armored: &str) -> Result<()> {
        if armored.len() > MAX_ARMORED_SIGNATURE_LEN {
            return Err(Error::Invalid("operator signature length"));
        }
        let blob = dearmor(armored)?;
        let mut r = SshReader(&blob);
        if r.take(6)? != SSHSIG_MAGIC || r.u32()? != SSHSIG_VERSION {
            return Err(Error::Invalid("operator signature format"));
        }
        if r.string()? != self.blob().as_slice() {
            return Err(Error::UntrustedKey);
        }
        let namespace = r.string()?;
        if namespace != OPERATOR_NAMESPACE.as_bytes() {
            return Err(Error::Invalid("operator signature namespace"));
        }
        let reserved = r.string()?.to_vec();
        let hash_alg = r.string()?.to_vec();
        let sig_blob = r.string()?;
        r.finish()?;
        let mut s = SshReader(sig_blob);
        if s.string()? != ED25519.as_bytes() {
            return Err(Error::Invalid("operator signature type"));
        }
        let sig: [u8; 64] = s.string()?.try_into().map_err(|_| Error::Invalid("operator signature"))?;
        s.finish()?;
        let hashed = match hash_alg.as_slice() {
            b"sha512" => Sha512::digest(message).to_vec(),
            b"sha256" => Sha256::digest(message).to_vec(),
            _ => return Err(Error::Invalid("operator signature hash")),
        };
        let signed = signed_data(&reserved, &hash_alg, &hashed);
        VerifyingKey::from_bytes(&self.0)
            .map_err(|_| Error::BadPublicKey)?
            .verify_strict(&signed, &Signature::from_bytes(&sig))
            .map_err(|_| Error::BadSignature)
    }
}

/// The bytes an SSHSIG signature covers.
fn signed_data(reserved: &[u8], hash_alg: &[u8], hashed: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(128);
    d.extend_from_slice(SSHSIG_MAGIC);
    put_string(&mut d, OPERATOR_NAMESPACE.as_bytes());
    put_string(&mut d, reserved);
    put_string(&mut d, hash_alg);
    put_string(&mut d, hashed);
    d
}

/// Signs `message` as `ssh-keygen -Y sign -n slop-ninja-operator-decision`
/// would (SHA-512), armored. Tests and drills only: the real key signs on its
/// offline device with `ssh-keygen`.
#[cfg(any(test, feature = "fixtures"))]
pub fn sign_sshsig(key: &ed25519_dalek::SigningKey, message: &[u8]) -> String {
    use ed25519_dalek::Signer as _;
    let public = OperatorKey(key.verifying_key().to_bytes());
    let hashed = Sha512::digest(message).to_vec();
    let sig = key.sign(&signed_data(b"", b"sha512", &hashed));
    let mut sig_blob = Vec::new();
    put_string(&mut sig_blob, ED25519.as_bytes());
    put_string(&mut sig_blob, &sig.to_bytes());
    let mut blob = Vec::new();
    blob.extend_from_slice(SSHSIG_MAGIC);
    blob.extend_from_slice(&SSHSIG_VERSION.to_be_bytes());
    put_string(&mut blob, &public.blob());
    put_string(&mut blob, OPERATOR_NAMESPACE.as_bytes());
    put_string(&mut blob, b"");
    put_string(&mut blob, b"sha512");
    put_string(&mut blob, &sig_blob);
    let b64 = base64_encode(&blob);
    let mut out = String::from(ARMOR_BEGIN);
    out.push('\n');
    for line in b64.as_bytes().chunks(70) {
        out.push_str(core::str::from_utf8(line).unwrap_or_default());
        out.push('\n');
    }
    out.push_str(ARMOR_END);
    out.push('\n');
    out
}

/// What an operator signs offline for one action ([`Statement::to_text`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    /// The action, `<service>.<name>` (for example `eval.abandon`).
    pub action: String,
    /// The decision id the request carries.
    pub decision: String,
    /// SHA-256 of the decision record, when the request names one.
    pub decision_sha256: Option<Digest>,
    /// SHA-256 of the request body it authorizes (the service's canonical
    /// form, without `preview` and `authorization`).
    pub body_sha256: Digest,
    /// `eval`, `coordinator` or `publisher`.
    pub service: String,
    /// The eval authority, or the coordinator deployment's name.
    pub authority: String,
    /// The store it is addressed to (an eval instance, or `journal`).
    pub store: String,
    pub genesis: Digest,
    pub netuid: u16,
    pub mechanism: u8,
    /// The subnet registration block, when the signer names one; a verifier
    /// that knows its own compares them.
    pub registration: Option<u64>,
}

fn field_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.:/@".contains(&b))
}

impl Statement {
    pub fn validate(&self) -> Result<()> {
        for (v, what) in [
            (&self.action, "statement action"),
            (&self.decision, "statement decision"),
            (&self.service, "statement service"),
            (&self.authority, "statement authority"),
            (&self.store, "statement store"),
        ] {
            if !field_ok(v) {
                return Err(Error::Invalid(what));
            }
        }
        Ok(())
    }

    /// The exact bytes signed: one `name: value` line per field after the
    /// header, in this order, each line ending in `\n`.
    pub fn to_text(&self) -> Result<String> {
        self.validate()?;
        let opt = |d: &Option<Digest>| d.map_or("none".to_owned(), |d| d.hex());
        Ok(format!(
            "{STATEMENT_HEADER}\naction: {}\ndecision: {}\ndecision_sha256: {}\nbody_sha256: {}\nservice: {}\nauthority: {}\nstore: {}\ngenesis: {}\nnetuid: {}\nmechanism: {}\nregistration: {}\n",
            self.action,
            self.decision,
            opt(&self.decision_sha256),
            self.body_sha256.hex(),
            self.service,
            self.authority,
            self.store,
            self.genesis.hex(),
            self.netuid,
            self.mechanism,
            self.registration.map_or("none".to_owned(), |r| r.to_string()),
        ))
    }

    /// Parses a statement written by [`Statement::to_text`], and only that
    /// form: the text must equal the re-encoding of what it parses to.
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > MAX_STATEMENT_LEN {
            return Err(Error::Invalid("statement length"));
        }
        let mut lines = text.split('\n');
        if lines.next() != Some(STATEMENT_HEADER) {
            return Err(Error::Invalid("statement header"));
        }
        let mut next = |name: &str| -> Result<&str> {
            let line = lines.next().ok_or(Error::Invalid("statement field"))?;
            line.strip_prefix(name)
                .and_then(|r| r.strip_prefix(": "))
                .ok_or(Error::Invalid("statement field"))
        };
        let digest = |v: &str| -> Result<Digest> { parse_hex32(v).map(Digest) };
        let action = next("action")?.to_owned();
        let decision = next("decision")?.to_owned();
        let decision_sha256 = match next("decision_sha256")? {
            "none" => None,
            v => Some(digest(v)?),
        };
        let body_sha256 = digest(next("body_sha256")?)?;
        let service = next("service")?.to_owned();
        let authority = next("authority")?.to_owned();
        let store = next("store")?.to_owned();
        let genesis = digest(next("genesis")?)?;
        let netuid = next("netuid")?.parse().map_err(|_| Error::Invalid("statement netuid"))?;
        let mechanism = next("mechanism")?.parse().map_err(|_| Error::Invalid("statement mechanism"))?;
        let registration = match next("registration")? {
            "none" => None,
            v => Some(v.parse().map_err(|_| Error::Invalid("statement registration"))?),
        };
        let s = Self {
            action,
            decision,
            decision_sha256,
            body_sha256,
            service,
            authority,
            store,
            genesis,
            netuid,
            mechanism,
            registration,
        };
        if s.to_text()? != text {
            return Err(Error::Invalid("statement not canonical"));
        }
        Ok(s)
    }

    /// SHA-256 of the statement's text.
    pub fn digest(&self) -> Result<Digest> {
        Ok(Digest(Sha256::digest(self.to_text()?.as_bytes()).into()))
    }
}

fn parse_hex32(v: &str) -> Result<[u8; 32]> {
    if v.len() != 64 || !v.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(Error::Invalid("statement digest"));
    }
    let mut out = [0u8; 32];
    for (i, chunk) in v.as_bytes().chunks(2).enumerate() {
        let s = core::str::from_utf8(chunk).map_err(|_| Error::Invalid("statement digest"))?;
        out[i] = u8::from_str_radix(s, 16).map_err(|_| Error::Invalid("statement digest"))?;
    }
    Ok(out)
}

fn put_string(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
    out.extend_from_slice(s);
}

struct SshReader<'a>(&'a [u8]);

impl<'a> SshReader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(Error::UnexpectedEnd);
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn string(&mut self) -> Result<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    fn finish(&self) -> Result<()> {
        if self.0.is_empty() { Ok(()) } else { Err(Error::TrailingBytes) }
    }
}

fn dearmor(armored: &str) -> Result<Vec<u8>> {
    let t = armored.trim();
    let body = t
        .strip_prefix(ARMOR_BEGIN)
        .and_then(|r| r.strip_suffix(ARMOR_END))
        .ok_or(Error::Invalid("operator signature armor"))?;
    let b64: String = body.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    base64_decode(&b64)
}

/// The standard base64 alphabet: `A`-`Z`, `a`-`z`, `0`-`9`, `+`, `/`.
const B64: [u8; 64] = {
    let mut a = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        a[i] = match i {
            0..=25 => b'A' + i as u8,
            26..=51 => b'a' + (i - 26) as u8,
            52..=61 => b'0' + (i - 52) as u8,
            62 => b'+',
            _ => b'/',
        };
        i += 1;
    }
    a
};

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Standard base64 with padding (the OpenSSH key line and armor form).
fn base64_decode(text: &str) -> Result<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return Err(Error::Invalid("base64"));
    }
    let val = |c: u8| -> Result<u32> {
        B64.iter().position(|&x| x == c).map(|p| p as u32).ok_or(Error::Invalid("base64"))
    };
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let last = bytes.len() / 4 - 1;
    for (i, q) in bytes.chunks(4).enumerate() {
        let pad = q.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && i != last) {
            return Err(Error::Invalid("base64"));
        }
        let mut n = 0u32;
        for (j, &c) in q.iter().enumerate() {
            n = (n << 6) | if j >= 4 - pad { 0 } else { val(c)? };
        }
        let b = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&b[..3 - pad]);
        // Canonical: the bits a padded quantum drops must be zero.
        if (pad == 1 && b[2] != 0) || (pad == 2 && (b[1] != 0 || b[2] != 0)) {
            return Err(Error::Invalid("base64"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn statement() -> Statement {
        Statement {
            action: "eval.abandon".into(),
            decision: "drill-1".into(),
            decision_sha256: Some(Digest([7; 32])),
            body_sha256: Digest([9; 32]),
            service: "eval".into(),
            authority: "r1".into(),
            store: "main".into(),
            genesis: crate::TESTNET_GENESIS,
            netuid: 601,
            mechanism: 0,
            registration: None,
        }
    }

    #[test]
    fn statement_round_trips_and_refuses_other_forms() {
        let s = statement();
        let text = s.to_text().unwrap();
        assert!(text.starts_with("slop_ninja operator decision v1\naction: eval.abandon\n"));
        assert_eq!(Statement::parse(&text).unwrap(), s);
        assert!(Statement::parse(&text.replace("netuid: 601", "netuid: 0601")).is_err());
        assert!(Statement::parse(&format!("{text}extra\n")).is_err());
        assert!(Statement::parse(text.trim_end()).is_err());
        let mut bad = s.clone();
        bad.authority = "r 1".into();
        assert!(bad.to_text().is_err());
    }

    #[test]
    fn sshsig_verifies_only_the_signed_bytes_under_its_key() {
        let key = SigningKey::from_bytes(&[3; 32]);
        let other = SigningKey::from_bytes(&[4; 32]);
        let public = OperatorKey::from_bytes(key.verifying_key().to_bytes()).unwrap();
        let text = statement().to_text().unwrap();
        let sig = sign_sshsig(&key, text.as_bytes());
        public.verify(text.as_bytes(), &sig).unwrap();
        assert!(public.verify(format!("{text} ").as_bytes(), &sig).is_err());
        let foreign = sign_sshsig(&other, text.as_bytes());
        assert_eq!(public.verify(text.as_bytes(), &foreign), Err(Error::UntrustedKey));
        // The OpenSSH line round-trips.
        let line = public.to_openssh("operator test");
        assert_eq!(OperatorKey::parse(&line).unwrap(), public);
        assert_eq!(OperatorKey::parse(&hex(public.bytes())).unwrap(), public);
    }

    /// A signature made by OpenSSH's `ssh-keygen -Y sign -n
    /// slop-ninja-operator-decision` (a throwaway key made for this test;
    /// only its public half is kept) over the bytes `fixture statement\n`.
    #[test]
    fn verifies_an_openssh_signature() {
        let unhex = |h: &str| -> Vec<u8> {
            (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect()
        };
        let pk: [u8; 32] = unhex(OPENSSH_FIXTURE_KEY_HEX).try_into().unwrap();
        let key = OperatorKey::from_bytes(pk).unwrap();
        // The public-key line and the armor, as OpenSSH writes them.
        let line = key.to_openssh("operator fixture");
        assert_eq!(OperatorKey::parse(&line).unwrap(), key);
        let b64 = base64_encode(&unhex(OPENSSH_FIXTURE_SIG_HEX));
        let mut armored = format!("{ARMOR_BEGIN}\n");
        for chunk in b64.as_bytes().chunks(70) {
            armored.push_str(core::str::from_utf8(chunk).unwrap());
            armored.push('\n');
        }
        armored.push_str(ARMOR_END);
        armored.push('\n');
        key.verify(b"fixture statement\n", &armored).unwrap();
        assert!(key.verify(b"fixture statement", &armored).is_err());
        let fp = base64_encode(&unhex(OPENSSH_FIXTURE_FINGERPRINT_HEX));
        assert_eq!(key.fingerprint(), format!("SHA256:{}", fp.trim_end_matches('=')));
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn base64_is_canonical() {
        for n in 0..10 {
            let data: Vec<u8> = (0..n as u8).collect();
            if data.is_empty() {
                continue;
            }
            assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data);
        }
        assert!(base64_decode("AB==").is_err());
        assert!(base64_decode("A===").is_err());
        assert!(base64_decode("QQ=A").is_err());
    }

    include!("operator_fixture.rs");
}
