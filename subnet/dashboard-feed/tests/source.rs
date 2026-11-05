//! The feed is read-only by construction: its sources call no R2 write,
//! delete, list or multipart method, make no outbound fetch and read no
//! variable, secret or other binding.

const SOURCES: &[(&str, &str)] = &[
    ("lib.rs", include_str!("../src/lib.rs")),
    ("cf.rs", include_str!("../src/cf.rs")),
    ("contract.rs", include_str!("../src/contract.rs")),
];

#[test]
fn sources_never_write_delete_list_or_fetch() {
    for (name, src) in SOURCES {
        for forbidden in [
            ".put(",
            ".delete(",
            ".list(",
            "multipart",
            "Fetch::",
            "fetch_request",
            ".service(",
            ".secret(",
            ".var(",
            ".durable_object(",
            ".kv(",
            ".d1(",
            "Queue",
            "scheduled",
            "send_email",
            "custom_metadata",
            "Access-Control",
            "set-cookie",
        ] {
            assert!(!src.contains(forbidden), "{name} contains {forbidden:?}");
        }
    }
}

#[test]
fn wrangler_config_has_no_route_and_one_binding() {
    let w = include_str!("../wrangler.toml");
    let active: Vec<&str> = w.lines().map(str::trim).filter(|l| !l.starts_with('#')).collect();
    for forbidden in ["routes", "route =", "custom_domain", "[[env.testnet.services]]", "kv_namespaces", "durable_objects", "[vars]", "account_id", "zone"] {
        assert!(!active.iter().any(|l| l.contains(forbidden)), "wrangler.toml has {forbidden:?}");
    }
    // The only var is the testnet marker, in the testnet environment.
    let vars = active.iter().position(|l| *l == "[env.testnet.vars]").expect("the testnet vars table");
    let table: Vec<&&str> = active[vars + 1..].iter().take_while(|l| !l.starts_with('[')).filter(|l| !l.is_empty()).collect();
    assert_eq!(table, [&"NETWORK = \"testnet\""]);
    assert_eq!(active.iter().filter(|l| l.ends_with("vars]")).count(), 1);
    assert_eq!(active.iter().filter(|l| l.starts_with("workers_dev = false")).count(), 2);
    assert_eq!(active.iter().filter(|l| l.starts_with("preview_urls = false")).count(), 2);
    assert_eq!(active.iter().filter(|l| l.starts_with("[[env.testnet.r2_buckets]]")).count(), 1);
    assert!(active.contains(&"binding = \"DASHBOARD_TESTNET\""));
    assert!(active.contains(&"bucket_name = \"dashboard-bucket-testnet\""));
    assert!(active.contains(&"crons = []"));
    assert!(!active.iter().any(|l| l.starts_with("crons = [\"")));
}
