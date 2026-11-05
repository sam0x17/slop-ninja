use slop_ninja_protocol::fixtures_reward_v1 as fx;
use slop_ninja_protocol::submission::SubmissionUpload;
use slop_ninja_protocol::{AccountId, Encoder, TESTNET_GENESIS, domain_hash, object_sha256};

use super::*;

/// The pinned request message of the example scope (computed independently
/// with Python's hashlib, 2026-11-04; the v1 path's value reproduced first).
const REQUEST_MESSAGE_PIN: &str = "6f1a3bb2058ba6e384a8c39ade8248d9ec8a81d9edab91b67f5a841d63be3640";

const PROTOCOL_GOLDEN: &str = include_str!("../../../protocol/fixtures/golden-reward-v1.txt");
const CONTRACT_DOC: &str = include_str!("../../../../../docs/testnet/admission-api.md");

fn golden(key: &str) -> &'static str {
    PROTOCOL_GOLDEN
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .unwrap_or_else(|| panic!("golden line {key}"))
}

fn chain601() -> ChainBinding {
    ChainBinding {
        genesis: TESTNET_GENESIS,
        netuid: 601,
        mechanism: 0,
    }
}

#[test]
fn request_message_is_the_protocol_domain_hash_of_the_encoded_scope() {
    let chain = chain601();
    let hotkey = AccountId([0x22; 32]);
    let content = object_sha256(b"part bytes");
    let path = path::urls("ab");
    let scope = RequestScope {
        chain: &chain,
        hotkey: &hotkey,
        valid_until: 8_340_372,
        method: "PUT",
        path: &path,
        content_sha256: &content,
    };
    let mut e = Encoder::new();
    e.raw(&chain.genesis.0);
    e.u16(chain.netuid);
    e.u8(chain.mechanism);
    e.raw(&hotkey.0);
    e.u64(8_340_372);
    e.str("PUT");
    e.str(&path);
    e.raw(&content.0);
    let expected = domain_hash(REQUEST_DOMAIN, &e.into_bytes());
    assert_eq!(request_message(&scope), expected.0);
    // Pinned so a change of the message is a visible contract change.
    assert_eq!(
        hex::encode(request_message(&scope)),
        REQUEST_MESSAGE_PIN
    );
}

#[test]
fn request_message_binds_every_field() {
    let chain = chain601();
    let hotkey = AccountId([0x22; 32]);
    let content = empty_sha256();
    let base = RequestScope {
        chain: &chain,
        hotkey: &hotkey,
        valid_until: 10,
        method: "POST",
        path: path::SESSIONS,
        content_sha256: &content,
    };
    let reference = request_message(&base);
    let other_chain = ChainBinding {
        netuid: 602,
        ..chain.clone()
    };
    let other_hotkey = AccountId([0x23; 32]);
    let other_content = object_sha256(b"x");
    let variants = [
        RequestScope {
            chain: &other_chain,
            ..base
        },
        RequestScope {
            hotkey: &other_hotkey,
            ..base
        },
        RequestScope {
            valid_until: 11,
            ..base
        },
        RequestScope {
            method: "PUT",
            ..base
        },
        RequestScope {
            path: path::COHORT,
            ..base
        },
        RequestScope {
            content_sha256: &other_content,
            ..base
        },
    ];
    for variant in variants {
        assert_ne!(request_message(&variant), reference);
    }
    // Length prefixes keep the method/path boundary unambiguous.
    let split_a = RequestScope {
        method: "PO",
        path: "ST/admission",
        ..base
    };
    let split_b = RequestScope {
        method: "POST",
        path: "/admission",
        ..base
    };
    assert_ne!(request_message(&split_a), request_message(&split_b));
}

#[test]
fn empty_sha256_is_the_digest_of_nothing() {
    assert_eq!(empty_sha256(), object_sha256(b""));
}

/// The session id of the protocol fixture's upload envelope is the golden
/// `submission.upload.digest`.
#[test]
fn session_id_is_the_upload_record_digest() {
    let spec = fx::models()[0].spec.clone();
    let payload = hex::decode(golden("submission.chain_payload")).unwrap();
    let cohort = u64::from_le_bytes(payload[4..12].try_into().unwrap());
    let opening = fx::opening(&fx::chain(), cohort, &spec);
    assert_eq!(opening.chain_payload().as_slice(), payload.as_slice());
    let upload = SubmissionUpload::new(opening, &fx::header_bytes(&fx::artifact_bytes(&spec))).unwrap();
    assert_eq!(
        session_id(&upload).unwrap().hex(),
        golden("submission.upload.digest")
    );
}

#[test]
fn routes_parse_without_copying_and_round_trip_through_the_writers() {
    let id = "ab".repeat(32);
    let cases: Vec<(&str, String, Route<'_>)> = vec![
        ("GET", path::COHORT.into(), Route::ReadCohort),
        ("POST", path::SESSIONS.into(), Route::CreateSession),
        ("GET", path::session(&id), Route::ReadSession { session: &id }),
        ("POST", path::urls(&id), Route::Urls { session: &id }),
        ("POST", path::complete(&id), Route::Complete { session: &id }),
        ("POST", path::repair(&id), Route::Repair { session: &id }),
        ("GET", path::hotkey(7, "5Fhot"), Route::ReadHotkey { cohort: 7, hotkey: "5Fhot" }),
    ];
    for (method, route, expected) in &cases {
        assert_eq!(Route::parse(method, route), Some(*expected), "{method} {route}");
        assert_eq!(expected.signed(), *expected != Route::ReadCohort);
    }
    // The parsed segments borrow the request path.
    let text = path::session(&id);
    let Some(Route::ReadSession { session }) = Route::parse("GET", &text) else {
        panic!()
    };
    assert!(std::ptr::eq(session.as_ptr(), text[text.len() - 64..].as_ptr()));
    // The writers fill a caller's buffer.
    let mut buffer = heapless_like::Buffer::default();
    path::write_urls(&mut buffer, &id).unwrap();
    assert_eq!(buffer.as_str(), path::urls(&id));
    for (method, bad) in [
        ("GET", "/admission/v2/sessions".to_owned()),
        ("PUT", path::session(&id)),
        ("GET", path::session(&id.to_uppercase())),
        ("GET", path::session("abcd")),
        // Version 1's part and attempt routes are gone: no upload byte
        // reaches a Worker.
        ("PUT", format!("{}/parts/1", path::session(&id))),
        ("DELETE", format!("{}/attempts/1", path::session(&id))),
        ("POST", format!("{}/attempts/2", path::session(&id))),
        ("GET", path::urls(&id)),
        ("PUT", path::urls(&id)),
        ("POST", format!("{}/extra", path::complete(&id))),
        ("GET", "/admission/v2/cohorts/7/hotkeys/".to_owned()),
        ("GET", "/admission/v2/cohorts/x/hotkeys/5F".to_owned()),
        // The hotkey segment is bounded by the SS58 shape.
        ("GET", path::hotkey(7, &"5".repeat(SS58_ACCOUNT_MAX_CHARS + 1))),
        ("GET", path::hotkey(7, "5F0l")),
        ("GET", path::hotkey(7, "5F%20")),
        ("GET", "/admission/v1/cohort".to_owned()),
        ("POST", path::COHORT.to_owned()),
    ] {
        assert_eq!(Route::parse(method, &bad), None, "{method} {bad}");
    }
}

/// A fixed-capacity `fmt::Write` sink, as an arena-backed writer would be.
mod heapless_like {
    pub struct Buffer {
        bytes: [u8; 160],
        len: usize,
    }
    impl Default for Buffer {
        fn default() -> Self {
            Self {
                bytes: [0; 160],
                len: 0,
            }
        }
    }
    impl Buffer {
        pub fn as_str(&self) -> &str {
            core::str::from_utf8(&self.bytes[..self.len]).unwrap()
        }
    }
    impl core::fmt::Write for Buffer {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let end = self.len + s.len();
            if end > self.bytes.len() {
                return Err(core::fmt::Error);
            }
            self.bytes[self.len..end].copy_from_slice(s.as_bytes());
            self.len = end;
            Ok(())
        }
    }
}

#[test]
fn the_response_limit_is_fixed() {
    assert_eq!(response_limit(), RESPONSE_BASE_BYTES);
}

#[test]
fn codes_round_trip_and_are_unique() {
    let mut seen = std::collections::BTreeSet::new();
    for code in ErrorCode::ALL {
        assert_eq!(ErrorCode::parse(code.as_str()), Some(*code));
        assert!(seen.insert(code.as_str()), "{code} repeated");
    }
    for state in SessionState::ALL {
        assert_eq!(SessionState::parse(state.as_str()), Some(*state));
    }
    assert_eq!(ErrorCode::parse("later_code"), None);
}

/// Every code and state this crate knows is documented in the contract, with
/// the HTTP status the crate reports.
#[test]
fn contract_document_lists_every_code() {
    for code in ErrorCode::ALL {
        let quoted = format!("`{}`", code.as_str());
        assert!(
            CONTRACT_DOC.contains(&quoted),
            "{code} missing from the contract"
        );
        if let Some(status) = code.http_status() {
            let row = format!("| `{}` | {status} |", code.as_str());
            assert!(
                CONTRACT_DOC.contains(&row),
                "{code} has another status in the contract"
            );
        }
    }
    for state in SessionState::ALL {
        assert!(
            CONTRACT_DOC.contains(&format!("`{}`", state.as_str())),
            "{state} missing from the contract"
        );
    }
    for name in [
        header::HOTKEY,
        header::VALID_UNTIL,
        header::CONTENT_SHA256,
        header::SIGNATURE,
        header::COMMITMENT_BLOCK,
        REQUEST_DOMAIN,
        UPLOAD_MEDIA_TYPE,
        SELECTION_EARLIEST_COMMITMENT_V1,
        json::SCHEMA_COHORT,
        json::SCHEMA_SESSION,
        json::SCHEMA_HOTKEY,
        json::SCHEMA_ERROR,
        path::COHORT,
        path::SESSIONS,
        "/urls",
        "/complete",
        "/repair",
        "slop_ninja_upload_url_policy_v1",
        "x-amz-checksum-sha256",
        "if-none-match",
        "repair_needed",
        "last_refusal",
        "artifact_spec",
        "cutoff_block",
    ] {
        assert!(
            CONTRACT_DOC.contains(name),
            "{name} missing from the contract"
        );
    }
}

#[test]
fn header_values_parse_strictly() {
    assert_eq!(parse_valid_until("0"), Some(0));
    assert_eq!(parse_valid_until("8340372"), Some(8_340_372));
    for bad in ["", "01", "+1", "-1", "1 ", "0x10", "18446744073709551616"] {
        assert_eq!(parse_valid_until(bad), None, "{bad:?}");
    }
    assert_eq!(parse_hex32(&"ab".repeat(32)), Some([0xab; 32]));
    assert_eq!(
        parse_hex32(&format!("0x{}", "ab".repeat(32))),
        Some([0xab; 32])
    );
    assert_eq!(parse_hex32("abcd"), None);
    assert_eq!(parse_signature(&"01".repeat(64)), Some([1; 64]));
    assert_eq!(parse_signature(&"01".repeat(63)), None);
}

#[test]
fn json_shapes_round_trip_and_tolerate_new_fields() {
    let chain = json::Chain::from_binding(&chain601());
    assert_eq!(chain.binding(), Some(chain601()));
    let body = json::ErrorBody::new(
        ErrorCode::WrongCohort,
        "the opening names cohort 6",
        json::Detail {
            admitting: Some(7),
            ..Default::default()
        },
    );
    let text = serde_json::to_string(&body).unwrap();
    assert!(text.contains("\"retryable\":false"));
    assert!(!text.contains("missing"));
    let back: json::ErrorBody = serde_json::from_str(&text).unwrap();
    assert_eq!(back, body);
    let extended = r#"{"schema":"slop_ninja.admission.error/v2","later":1,
        "error":{"code":"later_code","message":"m","detail":{"later":true}}}"#;
    let parsed: json::ErrorBody = serde_json::from_str(extended).unwrap();
    assert_eq!(ErrorCode::parse(&parsed.error.code), None);
    assert!(!parsed.error.retryable);
}
