//! Web Bot Auth: every outbound request of SlopNinjaBot carries an HTTP
//! Message Signature (RFC 9421) made with the Worker secret
//! `WEB_BOT_AUTH_KEY`, an Ed25519 key separate from every other key (risk
//! F5), in the form the Web Bot Auth architecture draft and Cloudflare's
//! verified-bot check read:
//!
//! ```text
//! Signature-Agent: "https://slop.ninja"
//! Signature-Input: sig1=("@authority" "signature-agent");created=<s>;expires=<s>;keyid="<thumbprint>";alg="ed25519";nonce="<b64>";tag="web-bot-auth"
//! Signature: sig1=:<base64 Ed25519 signature>:
//! ```
//!
//! The signed base is RFC 9421 section 2.5's: one line per covered
//! component, then `"@signature-params"`. `keyid` is the key's JWK
//! Thumbprint (RFC 7638, SHA-256, base64url). The public key is published
//! as a JWK set in the site's `/.well-known/http-message-signatures-directory`
//! ([`Signer::directory`]), at the origin the `Signature-Agent` names (the
//! deployment's `SITE_ORIGIN`). Rotation: install the new secret and publish
//! both keys in the directory until the old one is retired.
//!
//! The secret holds a private JWK: `{"kty":"OKP","crv":"Ed25519","d":"...","x":"..."}`
//! (`x` optional; checked when present).

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer as _, SigningKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// The signature label and tag.
pub const LABEL: &str = "sig1";
pub const TAG: &str = "web-bot-auth";
/// How long one signature is valid.
pub const VALIDITY_S: u64 = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyError {
    /// Not a JSON private OKP Ed25519 JWK.
    Shape,
    /// `x` does not match the key derived from `d`.
    Mismatch,
    /// The signature agent origin is not an `https` origin.
    Agent,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    crv: String,
    d: String,
    #[serde(default)]
    x: Option<String>,
}

/// The signing key and its identifiers.
pub struct Signer {
    key: SigningKey,
    x: String,
    keyid: String,
    /// The `Signature-Agent` header's value: the origin as an sf-string.
    agent: String,
}

impl Signer {
    /// The key in `jwk` (the secret's value) for requests that name
    /// `agent_origin` (`https://<site>`, no path) as their signature agent.
    pub fn from_jwk(jwk: &str, agent_origin: &str) -> Result<Signer, KeyError> {
        let mut j: Jwk = serde_json::from_str(jwk.trim()).map_err(|_| KeyError::Shape)?;
        let d_text = Zeroizing::new(std::mem::take(&mut j.d));
        if j.kty != "OKP" || j.crv != "Ed25519" {
            return Err(KeyError::Shape);
        }
        let d = Zeroizing::new(URL_SAFE_NO_PAD.decode(d_text.trim().as_bytes()).map_err(|_| KeyError::Shape)?);
        let seed: Zeroizing<[u8; 32]> = Zeroizing::new(d.as_slice().try_into().map_err(|_| KeyError::Shape)?);
        let key = SigningKey::from_bytes(&seed);
        let x = URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes());
        if j.x.as_deref().is_some_and(|given| given.trim() != x) {
            return Err(KeyError::Mismatch);
        }
        let agent_origin = agent_origin.trim().trim_end_matches('/');
        let ok = agent_origin
            .strip_prefix("https://")
            .is_some_and(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'));
        if !ok {
            return Err(KeyError::Agent);
        }
        Ok(Signer { keyid: thumbprint(&x), key, x, agent: format!("\"{agent_origin}\"") })
    }

    /// The JWK Thumbprint (RFC 7638) that names the key.
    pub fn keyid(&self) -> &str {
        &self.keyid
    }

    /// The public JWK set for the signatures directory.
    pub fn directory(&self) -> String {
        format!("{{\"keys\":[{{\"kty\":\"OKP\",\"crv\":\"Ed25519\",\"kid\":\"{}\",\"x\":\"{}\",\"use\":\"sig\"}}]}}", self.keyid, self.x)
    }

    /// The `@signature-params` value.
    fn params(&self, created: u64, nonce_b64: &str) -> String {
        format!(
            "(\"@authority\" \"signature-agent\");created={created};expires={};keyid=\"{}\";alg=\"ed25519\";nonce=\"{nonce_b64}\";tag=\"{TAG}\"",
            created + VALIDITY_S,
            self.keyid
        )
    }

    /// The headers of one request to `authority` (the URL's host, with its
    /// port when not the default), signed at `created` (Unix seconds) with
    /// `nonce` (random bytes).
    pub fn headers(&self, authority: &str, created: u64, nonce: &[u8]) -> [(&'static str, String); 3] {
        let params = self.params(created, &STANDARD.encode(nonce));
        let base = signature_base(authority, &self.agent, &params);
        let sig = self.key.sign(base.as_bytes());
        [
            ("signature-agent", self.agent.clone()),
            ("signature-input", format!("{LABEL}={params}")),
            ("signature", format!("{LABEL}=:{}:", STANDARD.encode(sig.to_bytes()))),
        ]
    }
}

/// RFC 9421 section 2.5's signature base for the two covered components.
pub fn signature_base(authority: &str, agent_header: &str, params: &str) -> String {
    format!("\"@authority\": {}\n\"signature-agent\": {agent_header}\n\"@signature-params\": {params}", authority.to_ascii_lowercase())
}

/// RFC 7638's thumbprint of an Ed25519 public key (`x`, base64url).
pub fn thumbprint(x: &str) -> String {
    let canonical = format!("{{\"crv\":\"Ed25519\",\"kty\":\"OKP\",\"x\":\"{x}\"}}");
    URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    /// RFC 8032 section 7.1, TEST 1: the Ed25519 public key (the key RFC 8037
    /// appendix A uses for its JWK examples), and the SHA-256 JWK Thumbprint
    /// RFC 8037 appendix A.3 gives for it, in hex.
    const PUBLIC_HEX: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    const THUMBPRINT_HEX: &str = "90facafea9b1556698540f70c0117a22ea37bd5cf3ed3c47093c1707282b4b89";

    fn b64url_of_hex(h: &str) -> String {
        let bytes: Vec<u8> = (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect();
        URL_SAFE_NO_PAD.encode(bytes)
    }

    /// A fixed test seed (no published key), as the secret holds it.
    fn d() -> String {
        URL_SAFE_NO_PAD.encode([7u8; 32])
    }

    fn x() -> String {
        b64url_of_hex(PUBLIC_HEX)
    }

    fn kid() -> String {
        b64url_of_hex(THUMBPRINT_HEX)
    }

    /// The test key as the secret holds it.
    pub(crate) fn test_jwk() -> String {
        jwk(&d(), None)
    }

    fn jwk(d: &str, x: Option<&str>) -> String {
        match x {
            Some(x) => format!(r#"{{"kty":"OKP","crv":"Ed25519","d":"{d}","x":"{x}"}}"#),
            None => format!(r#"{{"kty":"OKP","crv":"Ed25519","d":"{d}"}}"#),
        }
    }

    #[test]
    fn the_rfc_8037_thumbprint() {
        assert_eq!(thumbprint(&x()), kid());
    }

    #[test]
    fn the_key_and_its_identifiers() {
        let public_x = URL_SAFE_NO_PAD.encode(SigningKey::from_bytes(&[7u8; 32]).verifying_key().as_bytes());
        let s = Signer::from_jwk(&jwk(&d(), Some(&public_x)), "https://slop.ninja").unwrap();
        assert_eq!(s.x, public_x);
        assert_eq!(s.keyid(), thumbprint(&public_x));
        assert!(s.directory().contains(&public_x) && !s.directory().contains(&d()));
    }

    #[test]
    fn signatures_verify_over_the_rfc_9421_base() {
        let s = Signer::from_jwk(&jwk(&d(), None), "https://slop.ninja/").unwrap();
        let h = s.headers("Example.COM", 1_735_689_600, &[7u8; 32]);
        assert_eq!(h[0], ("signature-agent", "\"https://slop.ninja\"".to_owned()));
        let input = &h[1].1;
        assert!(input.starts_with("sig1=(\"@authority\" \"signature-agent\");created=1735689600;expires=1735689900;keyid=\""));
        assert!(input.ends_with(";alg=\"ed25519\";nonce=\"BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=\";tag=\"web-bot-auth\""));
        let params = input.strip_prefix("sig1=").unwrap();
        let base = signature_base("example.com", "\"https://slop.ninja\"", params);
        assert_eq!(base.lines().next(), Some("\"@authority\": example.com"));
        let sig_b64 = h[2].1.strip_prefix("sig1=:").unwrap().strip_suffix(':').unwrap();
        let sig = Signature::from_slice(&STANDARD.decode(sig_b64).unwrap()).unwrap();
        let pk: VerifyingKey = SigningKey::from_bytes(&[7u8; 32]).verifying_key();
        pk.verify(base.as_bytes(), &sig).unwrap();
        // Another authority does not verify.
        let other = signature_base("example.org", "\"https://slop.ninja\"", params);
        assert!(pk.verify(other.as_bytes(), &sig).is_err());
    }

    #[test]
    fn bad_keys_are_refused() {
        assert_eq!(Signer::from_jwk("not json", "https://slop.ninja").err(), Some(KeyError::Shape));
        assert_eq!(Signer::from_jwk(&jwk("AAAA", None), "https://slop.ninja").err(), Some(KeyError::Shape));
        let rsa = test_jwk().replace("\"OKP\"", "\"RSA\"");
        assert_eq!(Signer::from_jwk(&rsa, "https://slop.ninja").err(), Some(KeyError::Shape));
        assert_eq!(Signer::from_jwk(&jwk(&d(), Some(&kid())), "https://slop.ninja").err(), Some(KeyError::Mismatch));
        for agent in ["http://slop.ninja", "https://", "<SITE_ORIGIN>", "https://slop.ninja/path"] {
            assert_eq!(Signer::from_jwk(&jwk(&d(), None), agent).err(), Some(KeyError::Agent), "{agent}");
        }
    }
}
