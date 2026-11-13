//! Lint: the R2 S3 credentials and the signer's routes stay where the
//! design puts them (review finding: the placement must not rest on
//! convention alone).
//!
//! - Only the internal signer Worker (`r2-signer`) binds Secrets Store
//!   secrets (the presign and copy key pairs). The public upload Worker, the
//!   eval Worker (coordinator), the publisher and the generation Worker bind
//!   none.
//! - Only the eval Worker (minting) and the publisher (the copy route) bind
//!   the signer Worker; the public upload Worker never does.
//! - The publisher reads the copy route's token as a secret only, never as a
//!   plain var a mis-deploy could expose.
//!
//! Comments in the configs are ignored. Run with `cargo test --test
//! credential_placement`.

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A wrangler.toml without its comments.
fn config(rel: &str) -> String {
    let text = std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    text.lines()
        .map(|l| match l.find('#') {
            Some(at) => &l[..at],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const SIGNER_SERVICE: &str = "\"slop-ninja-r2-signer-testnet\"";

#[test]
fn only_the_signer_binds_the_r2_credentials() {
    assert!(config("r2-signer/wrangler.toml").contains("secrets_store_secrets"), "the signer binds them");
    for rel in ["admission-worker/wrangler.toml", "coordinator/wrangler.toml", "publisher/wrangler.toml", "gen-worker/wrangler.toml"] {
        let c = config(rel);
        assert!(!c.contains("secrets_store_secrets"), "{rel} binds a Secrets Store secret");
        assert!(!c.contains("R2_PRESIGN") && !c.contains("R2_COPY_KEY"), "{rel} names an R2 S3 key");
    }
}

#[test]
fn the_public_upload_worker_never_reaches_the_signer() {
    let c = config("admission-worker/wrangler.toml");
    assert!(!c.contains(SIGNER_SERVICE), "the upload Worker binds the signer");
    assert!(!c.contains("r2-signer"), "the upload Worker names the signer");
    // The signer has no public surface of its own.
    let s = config("r2-signer/wrangler.toml");
    assert!(s.contains("workers_dev = false") && !s.contains("routes") && !s.contains("r2_buckets"));
    // Its two callers.
    assert!(config("coordinator/wrangler.toml").contains(SIGNER_SERVICE));
    assert!(config("publisher/wrangler.toml").contains(SIGNER_SERVICE));
}

#[test]
fn the_publisher_reads_the_copy_token_as_a_secret_only() {
    let cf = std::fs::read_to_string(root().join("publisher/src/cf.rs")).unwrap();
    assert!(cf.contains("env.secret(\"COPY_INTERFACE_TOKEN\")"));
    assert!(!cf.contains("var_or_secret(&env, \"COPY_INTERFACE_TOKEN\")"));
    assert!(!config("publisher/wrangler.toml").contains("COPY_INTERFACE_TOKEN"), "never a tracked var");
}
