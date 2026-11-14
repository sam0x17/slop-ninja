//! Source rules of the store: what its code and config never do.

use std::fs;

fn src(name: &str) -> String {
    fs::read_to_string(format!("{}/src/{name}", env!("CARGO_MANIFEST_DIR"))).expect(name)
}

const STORE_FILES: &[&str] = &[
    "canon.rs",
    "cases.rs",
    "cf_cases.rs",
    "cf_paid.rs",
    "claims.rs",
    "excerpt.rs",
    "host.rs",
    "index_gates.rs",
    "indexnow.rs",
    "reputation.rs",
    "url_flow.rs",
    "x_flow.rs",
    "cf_routes.rs",
    "cf_store.rs",
    "cron.rs",
    "db.rs",
    "display.rs",
    "doc.rs",
    "handoff.rs",
    "keys.rs",
    "lookup.rs",
    "manage.rs",
    "ops.rs",
    "page.rs",
    "paid.rs",
    "queue.rs",
    "reads.rs",
    "rerun.rs",
    "share.rs",
    "sitemap.rs",
    "slug.rs",
    "store.rs",
    "store_ops.rs",
];

/// Strips `//` comments, so the rules read code only.
fn code(s: &str) -> String {
    s.lines().map(|l| l.split("//").next().unwrap_or("")).collect::<Vec<_>>().join("\n")
}

#[test]
fn the_retired_x_source_rule_is_gone() {
    let mut all = String::new();
    for f in STORE_FILES {
        all.push_str(&src(f));
    }
    all.push_str(
        &fs::read_to_string(format!("{}/migrations/0001_detect_store.sql", env!("CARGO_MANIFEST_DIR"))).unwrap(),
    );
    all.push_str(&fs::read_to_string(format!("{}/wrangler.toml", env!("CARGO_MANIFEST_DIR"))).unwrap());
    for banned in ["text_withheld", "X_DEVELOPER", "ADV16", "x_source_rule", "XSourceRule"] {
        assert!(!all.contains(banned), "{banned}");
    }
}

#[test]
fn nothing_in_the_store_writes_to_the_console_or_panics_on_input() {
    for f in STORE_FILES {
        let s = code(&src(f));
        for banned in [
            "console_log",
            "console_error",
            "println!",
            "eprintln!",
            "dbg!",
            "web_sys::console",
            ".unwrap()",
            ".expect(",
        ] {
            // Tests live below `#[cfg(test)]`; only the code above it counts.
            let body = s.split("#[cfg(test)]").next().unwrap_or("");
            assert!(!body.contains(banned), "{f} uses {banned}");
        }
    }
}

/// The text of `pub const <name>` in `file`.
fn statement(file: &str, name: &str) -> String {
    let s = src(file);
    let at = s.find(&format!("pub const {name}: &str")).unwrap_or_else(|| panic!("{name}"));
    let rest = &s[at..];
    rest[..rest.find("\";").unwrap()].to_owned()
}

#[test]
fn every_owner_statement_matches_the_owner() {
    for (file, name, clause) in [
        ("manage.rs", "DELETE_BY_OWNER", "WHERE id_h = ?1 AND owner_h = ?2"),
        ("manage.rs", "RELEASE", "WHERE id_h = ?1 AND owner_h = ?2"),
        ("share.rs", "CLEAR_OWNER", "WHERE id_h = ?1 AND owner_h = ?2"),
        ("manage.rs", "CLAIM", "AND owner_h IS NULL"),
        ("queue.rs", "SELECT_OWNED_DELETABLE", "WHERE owner_h = ?1"),
        ("queue.rs", "RELEASE_OWNED", "WHERE owner_h = ?1"),
    ] {
        assert!(statement(file, name).contains(clause), "{name}");
    }
}

#[test]
fn blob_columns_are_read_as_hex() {
    // A SELECT or RETURNING of an id or hash column goes through hex().
    for f in STORE_FILES {
        // A subquery's own column (`IN (SELECT id_h …)`) returns nothing.
        let s = code(&src(f)).replace("IN (SELECT id_h ", "IN (subquery ");
        for col in ["id_h", "owner_h", "manage_h", "copy_of", "superseded_by", "det_id_h"] {
            for bad in [format!("SELECT {col} "), format!("SELECT {col},"), format!("RETURNING {col}")] {
                assert!(!s.contains(&bad), "{f}: {bad}");
            }
        }
    }
}

#[test]
fn the_consumer_takes_one_message_per_invocation() {
    let w = fs::read_to_string(format!("{}/wrangler.toml", env!("CARGO_MANIFEST_DIR"))).unwrap();
    for env in ["testnet", "staging"] {
        let consumers: Vec<&str> = w.split(&format!("[[env.{env}.queues.consumers]]")).skip(1).collect();
        assert_eq!(consumers.len(), 1, "{env}");
        let block = consumers[0].split("\n[").next().unwrap();
        assert!(block.lines().any(|l| l.trim() == "max_batch_size = 1"), "{env}: max_batch_size = 1");
        assert!(block.contains("dead_letter_queue = "), "{env}: a dead-letter queue");
        assert_eq!(w.matches(&format!("[[env.{env}.d1_databases]]")).count(), 1, "{env}: one store");
    }
    // The store's bindings exist, and the D1 id is a placeholder.
    for b in [
        "binding = \"DETECT_STORE\"",
        "binding = \"DETECT_DOCS\"",
        "binding = \"DETECT_JOBS\"",
        "binding = \"ACCOUNT\"",
        "binding = \"ALERTS_RELAY\"",
    ] {
        assert!(w.contains(b), "{b}");
    }
    // The store's id is the placeholder the deploy wrappers replace.
    assert!(w.contains("database_id = \"detect-store-id-testnet\""));
}

#[test]
fn the_store_names_no_origin() {
    // Every origin comes from config: no host of the site in the store's
    // code.
    for f in STORE_FILES {
        let s = code(&src(f));
        assert!(!s.contains("slop.ninja"), "{f}");
    }
}

/// Messages go as JSON, the account Worker's encoding (a flattened message
/// through the typed send would arrive as a JS `Map`, which reads as `{}`).
#[test]
fn queue_messages_go_as_json() {
    let s = code(&src("cf_store.rs"));
    assert!(s.contains("QueueContentType::Json") && s.contains("send_raw_batch("));
    assert!(!s.contains("q.send_batch(") && !s.contains("q.send("), "no typed send");
}
