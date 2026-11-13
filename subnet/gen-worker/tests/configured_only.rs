//! Lint: the generation Worker reaches a vendor only at its configured
//! endpoint. Its tracked configuration binds no vendor key (no Secrets
//! Store binding, no key variable), and its non-test source has no other
//! route: no Secrets Store read, no vendor API URL, no direct-route target.
//! The behaviour itself (a vendor without a valid endpoint secret is
//! unavailable and nothing is sent) is tested in `src/route.rs` and
//! `src/drive.rs`; this keeps the removed route from coming back unseen.
//!
//! Comments in the configuration are ignored. Run with `cargo test --test
//! configured_only`.

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The configuration without its comments.
fn config() -> String {
    read("wrangler.toml")
        .lines()
        .map(|l| match l.find('#') {
            Some(at) => &l[..at],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A source file up to its test modules.
fn code(rel: &str) -> String {
    let text = read(rel);
    let end = text.find("#[cfg(test)]").unwrap_or(text.len());
    text[..end].to_owned()
}

#[test]
fn the_configuration_binds_no_vendor_key() {
    let c = config();
    for absent in ["secrets_store_secrets", "store_id", "SECRETS_STORE_ID", "API_KEY", "[vars]", "vars =", "[env.testnet.vars]"] {
        assert!(!c.contains(absent), "wrangler.toml names {absent}");
    }
    // The endpoint secrets are Worker secrets, never tracked values.
    for name in ["GEN_OPENAI_ENDPOINT", "GEN_ANTHROPIC_ENDPOINT"] {
        assert!(!c.contains(name), "wrangler.toml names {name}");
    }
}

#[test]
fn the_source_has_no_route_but_the_configured_endpoint() {
    for rel in ["src/cf.rs", "src/drive.rs", "src/route.rs", "src/routes.rs", "src/lib.rs"] {
        let c = code(rel);
        for absent in ["secret_store(", "Vendor::endpoint", ".endpoint()", "Route::Direct", "api.openai.com", "api.anthropic.com", "API_KEY"] {
            assert!(!c.contains(absent), "{rel} has {absent}");
        }
    }
    // The only secrets read are the two endpoint secrets.
    let cf = code("src/cf.rs");
    assert_eq!(cf.matches("env.secret(").count(), 1, "one endpoint secret read");
    assert!(cf.contains("route::secret_name(v)"));
    assert!(code("src/route.rs").contains("pub const ROUTE: Route = Route::ConfiguredEndpointV1;"));
}
