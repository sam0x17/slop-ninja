//! Which coordinator the client may upload to.
//!
//! An upload sends the opening, with its secret nonce, and the whole artifact
//! before the cohort closes, so the destination must come from an
//! authenticated source. The public status snapshot is unsigned, so its
//! `coordinator` field never chooses the destination by itself. The client
//! uploads only to:
//!
//! - an origin listed in `admission_origins` of the release trust file
//!   (`testnet-trust.json`, schema `slop_ninja_testnet_trust_roots_v1`), which
//!   ships with the release and is covered by its signature; or
//! - the origin the miner names with `--coordinator`, when no trust file pins
//!   any origin.
//!
//! When a trust file pins origins, every other origin is refused, including
//! one named with `--coordinator`. When it pins exactly one and the miner
//! names none (no `--coordinator`, no `--public`), the client uses that one.
//!
//! The artifact's bytes go straight to R2 under presigned URLs the
//! coordinator mints. A trust file may also pin the one R2 S3 host those URLs
//! may name, by name (`upload_host`, `<account>.r2.cloudflarestorage.com`)
//! or by the SHA-256 of that name (`upload_host_sha256`, lowercase hex: the
//! release trust file's form, decision D1, so the account id itself is not
//! in the tracked file; every presigned URL still shows it to its miner),
//! and the bucket they may name (`upload_bucket`). Without a host pin the
//! client accepts any R2 S3 host (`*.r2.cloudflarestorage.com`) in a URL
//! from the authenticated coordinator. Only the pin stops a compromised
//! coordinator front from pointing uploads at another account's bucket with
//! the same name and key layout (skeptic K12); the release trust file
//! carries both pins.

use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use url::{Origin, Url};

use crate::client::base_url;

/// The trust file schema this client reads.
pub const TRUST_SCHEMA: &str = "slop_ninja_testnet_trust_roots_v1";

#[derive(Deserialize)]
struct TrustFile {
    schema: String,
    #[serde(default)]
    admission_origins: Option<Vec<String>>,
    #[serde(default)]
    upload_host: Option<String>,
    #[serde(default)]
    upload_host_sha256: Option<String>,
    #[serde(default)]
    upload_bucket: Option<String>,
}

/// The coordinator origins a trust file pins (empty when it pins none), and
/// the R2 S3 host it pins for uploads, if any.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trust {
    origins: Vec<Origin>,
    upload_host: Option<String>,
    upload_host_sha256: Option<[u8; 32]>,
    upload_bucket: Option<String>,
}

/// Where the coordinator URL came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Pinned by the trust file.
    Pinned,
    /// Named with `--coordinator`; no trust file pins an origin.
    Named,
}

impl Source {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Pinned => "pinned by the trust file",
            Self::Named => "named with --coordinator; no trust file pins an origin",
        }
    }
}

impl Trust {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        Self::parse(&bytes).with_context(|| format!("trust file {}", path.display()))
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let file: TrustFile = serde_json::from_slice(bytes).context("not a trust file")?;
        ensure!(
            file.schema == TRUST_SCHEMA,
            "unknown trust schema {:?}",
            file.schema
        );
        let mut origins = Vec::new();
        for text in file.admission_origins.unwrap_or_default() {
            let url = base_url(&text)?;
            ensure!(
                url.path() == "/",
                "admission origin {text:?} must be an origin, without a path"
            );
            origins.push(url.origin());
        }
        let upload_host = match file.upload_host {
            None => None,
            Some(host) => {
                let suffix = slop_ninja_r2_presign::check::R2_S3_SUFFIX;
                ensure!(
                    slop_ninja_r2_presign::presign::valid_host(&host)
                        && host.ends_with(suffix)
                        && host.len() > suffix.len(),
                    "upload_host {host:?} must be an R2 S3 host (<account>{suffix})"
                );
                Some(host)
            }
        };
        let upload_host_sha256 = match file.upload_host_sha256 {
            None => None,
            Some(hex_digest) => {
                let mut d = [0u8; 32];
                ensure!(
                    hex_digest.len() == 64
                        && hex_digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        && hex::decode_to_slice(&hex_digest, &mut d).is_ok(),
                    "upload_host_sha256 must be 64 lowercase hex digits"
                );
                Some(d)
            }
        };
        let upload_bucket = match file.upload_bucket {
            None => None,
            Some(bucket) => {
                ensure!(
                    slop_ninja_r2_presign::presign::valid_bucket(&bucket),
                    "upload_bucket {bucket:?} is not an R2 bucket name"
                );
                Some(bucket)
            }
        };
        Ok(Self {
            origins,
            upload_host,
            upload_host_sha256,
            upload_bucket,
        })
    }

    /// The SHA-256 of the R2 S3 host presigned upload URLs must name, when
    /// pinned by digest.
    pub fn upload_host_sha256(&self) -> Option<[u8; 32]> {
        self.upload_host_sha256
    }

    /// The release's own trust roots (`coordinator/testnet-trust.json`,
    /// compiled in): the default upload pin when no trust file pins one
    /// (review round 2, skeptic K12 and decision D1), so a miner run with
    /// `--coordinator` and no trust file still refuses URLs for any other R2
    /// host or bucket.
    pub fn release() -> Self {
        Self::parse(include_bytes!("../../../coordinator/testnet-trust.json")).expect("the release trust file parses")
    }

    /// Whether this trust pins the upload host (by name or digest) or the
    /// bucket.
    pub fn pins_upload(&self) -> bool {
        self.upload_host.is_some() || self.upload_host_sha256.is_some() || self.upload_bucket.is_some()
    }

    /// The bucket presigned upload URLs must name, when pinned.
    pub fn upload_bucket(&self) -> Option<&str> {
        self.upload_bucket.as_deref()
    }

    /// The R2 S3 host presigned upload URLs must name, when pinned.
    pub fn upload_host(&self) -> Option<&str> {
        self.upload_host.as_deref()
    }

    pub fn pins_any(&self) -> bool {
        !self.origins.is_empty()
    }

    pub fn pins(&self, url: &Url) -> bool {
        self.origins.contains(&url.origin())
    }

    /// The one origin the trust file pins, when it pins exactly one: the
    /// destination when neither `--coordinator` nor `--public` names one.
    pub fn single(&self) -> Option<Url> {
        match self.origins.as_slice() {
            [only] => Url::parse(&only.ascii_serialization()).ok(),
            _ => None,
        }
    }

    /// The coordinator to upload to, from `--coordinator` (`named`) or the
    /// unsigned snapshot (`advertised`).
    pub fn choose(&self, named: Option<&str>, advertised: Option<&str>) -> Result<(Url, Source)> {
        if let Some(text) = named {
            let url = base_url(text)?;
            if !self.pins_any() {
                return Ok((url, Source::Named));
            }
            if self.pins(&url) {
                return Ok((url, Source::Pinned));
            }
            bail!(
                "the trust file pins other coordinator origins; refusing {}",
                url.origin().ascii_serialization()
            );
        }
        let Some(text) = advertised else {
            bail!("pass --coordinator, or --public with a trust file that pins the coordinator");
        };
        let url = base_url(text)?;
        if self.pins(&url) {
            return Ok((url, Source::Pinned));
        }
        if self.pins_any() {
            bail!(
                "the public snapshot names coordinator {}, which the trust file does not pin; \
                 refusing it",
                url.origin().ascii_serialization()
            );
        }
        bail!(
            "the public snapshot names coordinator {}, but the snapshot is unsigned and no trust \
             file pins an origin. Pass --coordinator with the origin from the miner guide, or \
             --trust with the release's testnet-trust.json",
            url.origin().ascii_serialization()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trust(origins: &[&str]) -> Trust {
        let json = serde_json::json!({
            "schema": TRUST_SCHEMA,
            "network": "testnet",
            "package_public_key": "00",
            "admission_origins": origins,
        });
        Trust::parse(json.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn a_trust_file_without_origins_pins_nothing() {
        let file = br#"{"schema":"slop_ninja_testnet_trust_roots_v1","netuid":601}"#;
        let trust = Trust::parse(file).unwrap();
        assert!(!trust.pins_any());
        let (url, source) = trust.choose(Some("https://a.example/x"), None).unwrap();
        assert_eq!(
            (url.as_str(), source),
            ("https://a.example/x/", Source::Named)
        );
        // The unsigned snapshot alone never chooses the destination.
        let error = trust.choose(None, Some("https://a.example")).unwrap_err();
        assert!(error.to_string().contains("unsigned"), "{error}");
        assert!(trust.choose(None, None).is_err());
    }

    #[test]
    fn pinned_origins_decide() {
        let trust = trust(&["https://admission.example", "http://127.0.0.1:8080"]);
        let (url, source) = trust
            .choose(None, Some("https://admission.example/base/"))
            .unwrap();
        assert_eq!(
            (url.as_str(), source),
            ("https://admission.example/base/", Source::Pinned)
        );
        assert_eq!(
            trust.choose(Some("http://127.0.0.1:8080"), None).unwrap().1,
            Source::Pinned
        );
        for (named, advertised) in [
            (Some("https://other.example"), None),
            (Some("http://127.0.0.1:8081"), None),
            (None, Some("https://admission.example.evil")),
            (
                Some("http://localhost:8080"),
                Some("https://admission.example"),
            ),
        ] {
            assert!(
                trust.choose(named, advertised).is_err(),
                "{named:?} {advertised:?}"
            );
        }
    }

    #[test]
    fn an_upload_host_pin_must_be_an_r2_s3_host() {
        let parse = |host: &str| {
            let json = serde_json::json!({"schema": TRUST_SCHEMA, "upload_host": host});
            Trust::parse(json.to_string().as_bytes())
        };
        let pinned = parse("acct.r2.cloudflarestorage.com").unwrap();
        assert_eq!(pinned.upload_host(), Some("acct.r2.cloudflarestorage.com"));
        for bad in [
            "r2.cloudflarestorage.com",
            ".r2.cloudflarestorage.com",
            "acct.example.com",
            "ACCT.r2.cloudflarestorage.com",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert_eq!(trust(&[]).upload_host(), None);
    }

    /// Decision D1 and skeptic K12: the release trust file pins the R2 S3
    /// host (by its name's SHA-256, so the tracked file carries no account
    /// id) and the admission bucket.
    #[test]
    fn the_release_trust_file_pins_the_upload_host_and_bucket() {
        let t = Trust::parse(include_bytes!("../../../coordinator/testnet-trust.json")).unwrap();
        assert_eq!(t, Trust::release());
        assert!(t.pins_upload() && t.upload_host_sha256().is_some() && t.upload_bucket().is_some());
        assert!(!Trust::default().pins_upload());
        assert!(t.pins_any());
        assert!(t.upload_host_sha256().is_some(), "the release pins the upload host");
        assert_eq!(t.upload_host(), None, "never the account id itself in the tracked file");
        assert_eq!(t.upload_bucket(), Some("slop-ninja-admission-testnet"));
        let bad = |k: &str, v: &str| {
            let json = serde_json::json!({"schema": TRUST_SCHEMA, k: v});
            Trust::parse(json.to_string().as_bytes()).is_err()
        };
        assert!(bad("upload_host_sha256", "AB"));
        assert!(bad("upload_host_sha256", &"G".repeat(64)));
        assert!(bad("upload_bucket", "Bad_Bucket"));
    }

    #[test]
    fn a_single_pinned_origin_is_the_default() {
        let one = trust(&["https://testnet-admission.slop.ninja"]);
        assert_eq!(
            one.single().unwrap().as_str(),
            "https://testnet-admission.slop.ninja/"
        );
        assert!(one.pins(&one.single().unwrap()));
        assert_eq!(
            trust(&["https://a.example", "https://b.example"]).single(),
            None
        );
        assert_eq!(Trust::default().single(), None);
    }

    #[test]
    fn malformed_trust_files_are_refused() {
        for bad in [
            r#"{"schema":"other"}"#.to_owned(),
            r#"{"schema":"slop_ninja_testnet_trust_roots_v1","admission_origins":["http://example.com"]}"#.to_owned(),
            r#"{"schema":"slop_ninja_testnet_trust_roots_v1","admission_origins":["https://example.com/path"]}"#.to_owned(),
            "not json".to_owned(),
        ] {
            assert!(Trust::parse(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
