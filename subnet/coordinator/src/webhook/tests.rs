use super::*;
use crate::testing::{context, finalized_body, header, id, receiver, sign};

const NOW: u64 = 1_793_000_000;

/// Computed independently from the receiver contract framing with
/// `openssl dgst -sha256 -mac HMAC` over hand-assembled bytes: secret
/// 0x42 * 32, key `pilot-v1`, timestamp NOW, subscription/event/delivery
/// 0x11/0x22/0x33 * 32, attempt 2, body below.
const KNOWN_MAC: &str = "8f894a1b55cb8e4575b07c232b69741110eab2390a965443bbebf047030b44f5";

fn challenge_body() -> Vec<u8> {
    format!(
        "{{\"type\":\"challenge\",\"subscription\":\"0x{}\",\"challenge\":\"{}\"}}",
        "11".repeat(32),
        "ab".repeat(32)
    )
    .into_bytes()
}

#[test]
fn mac_matches_independent_vector() {
    let fields = SignedFields {
        version: 1,
        key_id: "pilot-v1".into(),
        timestamp: NOW,
        context: context(0x22, 0x33),
        attempt: 2,
    };
    let mac = receiver().key.mac(&fields, &challenge_body());
    assert_eq!(hex::encode(mac), KNOWN_MAC);
}

#[test]
fn accepts_valid_signature_and_challenge() {
    let raw = challenge_body();
    let h = sign(context(0x22, 0x33), 2, NOW, &raw);
    let config = receiver();
    let signed = authenticate(&config, Some(&h), &raw, NOW).unwrap();
    assert_eq!(signed.attempt, 2);
    assert_eq!(
        parse_authenticated(&config, &signed, &raw).unwrap(),
        Delivery::Challenge {
            challenge: "ab".repeat(32)
        }
    );
}

#[test]
fn rejects_any_modified_input() {
    let config = receiver();
    let raw = finalized_body(0x22, 10, 0xa0, 0x9f);
    let good = sign(context(0x22, 0x33), 1, NOW, &raw);
    assert!(authenticate(&config, Some(&good), &raw, NOW).is_ok());

    let mut tampered = raw.clone();
    let last = tampered.len() - 2;
    tampered[last] ^= 1;
    assert_eq!(
        authenticate(&config, Some(&good), &tampered, NOW),
        Err(AuthError::Mac)
    );
    let mut longer = raw.clone();
    longer.push(b' ');
    assert_eq!(
        authenticate(&config, Some(&good), &longer, NOW),
        Err(AuthError::Mac)
    );

    // A field change without re-signing breaks the MAC, since every field is framed.
    let fields = |attempt, event| SignedFields {
        version: 1,
        key_id: "pilot-v1".into(),
        timestamp: NOW,
        context: context(event, 0x33),
        attempt,
    };
    let mac = Id(config.key.mac(&fields(1, 0x22), &raw));
    let replayed_attempt = header(&fields(2, 0x22), &mac);
    assert_eq!(
        authenticate(&config, Some(&replayed_attempt), &raw, NOW),
        Err(AuthError::Mac)
    );
    let other_event = header(&fields(1, 0x23), &mac);
    assert_eq!(
        authenticate(&config, Some(&other_event), &raw, NOW),
        Err(AuthError::Mac)
    );

    let wrong_key = ReceiverConfig {
        key: SigningKey::from_hex("pilot-v1", &"43".repeat(32)).unwrap(),
        ..receiver()
    };
    assert_eq!(
        authenticate(&wrong_key, Some(&good), &raw, NOW),
        Err(AuthError::Mac)
    );
}

#[test]
fn enforces_header_fields() {
    let config = receiver();
    let raw = challenge_body();
    let ctx = context(0x22, 0x33);
    assert_eq!(
        authenticate(&config, None, &raw, NOW),
        Err(AuthError::MissingSignature)
    );

    let mut value: serde_json::Value = serde_json::from_str(&sign(ctx, 1, NOW, &raw)).unwrap();
    let with = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut v = value.clone();
        edit(&mut v);
        authenticate(&config, Some(&v.to_string()), &raw, NOW)
    };
    assert_eq!(with(&|v| v["version"] = 2.into()), Err(AuthError::Version));
    assert_eq!(
        with(&|v| v["key_id"] = "pilot-v2".into()),
        Err(AuthError::KeyId)
    );
    assert_eq!(
        with(&|v| v["context"]["subscription"] = id(0x12).to_hex().into()),
        Err(AuthError::Subscription)
    );
    assert_eq!(with(&|v| v["attempt"] = 0.into()), Err(AuthError::Attempt));
    assert_eq!(
        with(&|v| v["timestamp"] = serde_json::json!(NOW)),
        Err(AuthError::MalformedSignature)
    );
    assert_eq!(
        with(&|v| v["timestamp"] = "01793000000".into()),
        Err(AuthError::MalformedSignature)
    );
    assert_eq!(
        with(&|v| v["mac"] = v["mac"]
            .as_str()
            .unwrap()
            .to_uppercase()
            .replace("0X", "0x")
            .into()),
        Err(AuthError::MalformedSignature)
    );
    assert_eq!(
        with(&|v| v["extra"] = 1.into()),
        Err(AuthError::MalformedSignature)
    );
    value["attempt"] = 70000.into();
    assert_eq!(
        authenticate(&config, Some(&value.to_string()), &raw, NOW),
        Err(AuthError::MalformedSignature)
    );
}

#[test]
fn enforces_timestamp_skew_both_ways() {
    let config = receiver();
    let raw = challenge_body();
    let ctx = context(0x22, 0x33);
    for (signed_at, ok) in [
        (NOW - 300, true),
        (NOW + 300, true),
        (NOW - 301, false),
        (NOW + 301, false),
    ] {
        let h = sign(ctx, 1, signed_at, &raw);
        let result = authenticate(&config, Some(&h), &raw, NOW);
        assert_eq!(result.is_ok(), ok, "signed_at {signed_at}");
        if !ok {
            assert_eq!(result, Err(AuthError::Skew));
        }
    }
}

#[test]
fn bounds_body_size() {
    let config = receiver();
    let raw = vec![b' '; MAX_BODY_BYTES + 1];
    let h = sign(context(0x22, 0x33), 1, NOW, &raw);
    assert_eq!(
        authenticate(&config, Some(&h), &raw, NOW),
        Err(AuthError::BodyTooLarge)
    );
}

fn parse(raw: &[u8], event: u8) -> Result<Delivery, BodyError> {
    let config = receiver();
    let h = sign(context(event, 0x33), 3, NOW, raw);
    let signed = authenticate(&config, Some(&h), raw, NOW).unwrap();
    parse_authenticated(&config, &signed, raw)
}

#[test]
fn parses_finalized_block() {
    let raw = finalized_body(0x22, 10, 0xa0, 0x9f);
    let Delivery::Finalized(n) = parse(&raw, 0x22).unwrap() else {
        panic!("expected block");
    };
    assert_eq!(n.header.number, 10);
    assert_eq!(n.header.hash, id(0xa0));
    assert_eq!(n.header.parent_hash, id(0x9f));
    assert_eq!(n.delivery_id, id(0x33));
    assert_eq!(n.attempt, 3);
    assert_eq!(n.journal_position, 7);
    assert_eq!(n.state_readiness, StateReadiness::HeaderOnly);
    assert_eq!(n.body_sha256, Id(Sha256::digest(&raw).into()));
}

fn edited(edit: impl Fn(&mut serde_json::Value)) -> Vec<u8> {
    let mut v: serde_json::Value =
        serde_json::from_slice(&finalized_body(0x22, 10, 0xa0, 0x9f)).unwrap();
    edit(&mut v);
    v.to_string().into_bytes()
}

#[test]
fn rejects_invalid_bodies() {
    assert_eq!(
        parse(&finalized_body(0x22, 10, 0xa0, 0x9f), 0x24),
        Err(BodyError::EventId)
    );
    let cases: Vec<(Vec<u8>, BodyError)> = vec![
        (
            edited(|v| v["event"]["version"] = 2.into()),
            BodyError::EventVersion,
        ),
        (
            edited(|v| v["event"]["chain"] = id(1).to_hex().into()),
            BodyError::Chain,
        ),
        (
            edited(|v| v["event"]["journal_position"] = "0".into()),
            BodyError::JournalPosition,
        ),
        (
            edited(|v| v["event"]["kind"]["type"] = "best".into()),
            BodyError::UnsupportedKind,
        ),
        (
            edited(|v| v["event"]["kind"] = serde_json::json!({"type": "reorg"})),
            BodyError::UnsupportedKind,
        ),
        (
            edited(|v| v["event"]["kind"]["block"]["number"] = serde_json::json!(10)),
            BodyError::Malformed,
        ),
        (
            edited(|v| v["event"]["kind"]["block"]["number"] = (1u64 << 53).to_string().into()),
            BodyError::StorageRange,
        ),
        (
            edited(|v| v["event"]["kind"]["block"]["parent_hash"] = id(0xa0).to_hex().into()),
            BodyError::Block,
        ),
        (
            edited(|v| v["event"]["kind"]["block"]["number"] = "0".into()),
            BodyError::Block,
        ),
        (
            edited(|v| v["event"]["kind"]["block"]["hash"] = Id::ZERO.to_hex().into()),
            BodyError::Block,
        ),
        (
            edited(|v| v["event"]["unknown"] = 1.into()),
            BodyError::Malformed,
        ),
        (edited(|v| v["unknown"] = 1.into()), BodyError::Malformed),
        (b"[]".to_vec(), BodyError::Malformed),
        (b"not json".to_vec(), BodyError::Malformed),
    ];
    for (raw, expected) in cases {
        assert_eq!(
            parse(&raw, 0x22),
            Err(expected),
            "{}",
            String::from_utf8_lossy(&raw)
        );
    }
    // Genesis has a zero parent.
    assert!(parse(&finalized_body(0x22, 0, 0xa0, 0), 0x22).is_ok());
}

#[test]
fn rejects_invalid_challenges() {
    let body = |sub: &str, challenge: &str| {
        serde_json::json!({"type": "challenge", "subscription": sub, "challenge": challenge})
            .to_string()
            .into_bytes()
    };
    let ours = id(0x11).to_hex();
    assert_eq!(
        parse(&body(&id(0x12).to_hex(), "abc"), 0x22),
        Err(BodyError::ChallengeSubscription)
    );
    assert_eq!(
        parse(&body(&ours, ""), 0x22),
        Err(BodyError::ChallengeValue)
    );
    assert_eq!(
        parse(&body(&ours, "a\r\nb"), 0x22),
        Err(BodyError::ChallengeValue)
    );
    assert_eq!(
        parse(&body(&ours, &"a".repeat(257)), 0x22),
        Err(BodyError::ChallengeValue)
    );
    let wrong_type = serde_json::json!({"type": "ping", "subscription": ours, "challenge": "x"});
    assert_eq!(
        parse(wrong_type.to_string().as_bytes(), 0x22),
        Err(BodyError::Malformed)
    );
}

#[test]
fn precheck_refuses_every_header_authentication_would_refuse_before_the_body() {
    // The receiver checks the header before it admits any body byte: every
    // header field authentication refuses is refused here with the same
    // code, and only the MAC (which needs the body) is left to `authenticate`.
    let config = receiver();
    let raw = challenge_body();
    let ctx = context(0x22, 0x33);
    let good = sign(ctx, 1, NOW, &raw);
    assert_eq!(precheck(&config, Some(&good), NOW), Ok(()));
    assert_eq!(precheck(&config, None, NOW), Err(AuthError::MissingSignature));
    let value: serde_json::Value = serde_json::from_str(&good).unwrap();
    let with = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut v = value.clone();
        edit(&mut v);
        let h = v.to_string();
        let pre = precheck(&config, Some(&h), NOW);
        assert_eq!(pre, authenticate(&config, Some(&h), &raw, NOW).map(|_| ()), "{h}");
        pre
    };
    assert_eq!(with(&|v| v["version"] = 2.into()), Err(AuthError::Version));
    assert_eq!(with(&|v| v["key_id"] = "pilot-v2".into()), Err(AuthError::KeyId));
    assert_eq!(with(&|v| v["context"]["subscription"] = id(0x12).to_hex().into()), Err(AuthError::Subscription));
    assert_eq!(with(&|v| v["attempt"] = 0.into()), Err(AuthError::Attempt));
    assert_eq!(with(&|v| v["timestamp"] = (NOW - 301).to_string().into()), Err(AuthError::Skew));
    assert_eq!(with(&|v| v["extra"] = 1.into()), Err(AuthError::MalformedSignature));
    assert_eq!(precheck(&config, Some(&"x".repeat(MAX_SIGNATURE_HEADER_BYTES + 1)), NOW), Err(AuthError::MalformedSignature));
    // A well-formed header with a wrong MAC passes the precheck; the body
    // read and authentication refuse it.
    let forged = sign(ctx, 1, NOW, b"another body");
    assert_eq!(precheck(&config, Some(&forged), NOW), Ok(()));
    assert_eq!(authenticate(&config, Some(&forged), &raw, NOW), Err(AuthError::Mac));
}

#[test]
fn accepts_contract_one_and_two_and_refuses_any_other() {
    assert_eq!(CONTRACT_HEADER, "X-Rtao-Webhook-Contract");
    assert_eq!(contract(None), Ok(Contract::Unnamed));
    assert_eq!(contract(Some("rtao-block-webhooks/1")), Ok(Contract::V1));
    assert_eq!(contract(Some("rtao-block-webhooks/2")), Ok(Contract::V2));
    assert_eq!(contract(Some(" rtao-block-webhooks/2\t")), Ok(Contract::V2));
    for other in [
        "",
        "rtao-block-webhooks/3",
        "rtao-block-webhooks/0",
        "rtao-block-webhooks/",
        "RTAO-BLOCK-WEBHOOKS/2",
        "rtao-block-webhooks/2,rtao-block-webhooks/1",
        "rtao-block-webhooks/22",
    ] {
        assert_eq!(contract(Some(other)), Err(BodyError::Contract), "{other:?}");
    }
    assert_eq!((BodyError::Contract.code(), BodyError::Contract.status()), ("webhook_contract", 422));
    // Each contract has its own delivery counter.
    let names = [Contract::Unnamed, Contract::V1, Contract::V2].map(Contract::counter);
    assert_eq!(names, ["deliveries_contract_unnamed", "deliveries_contract_v1", "deliveries_contract_v2"]);
}

#[test]
fn both_contracts_share_the_signature_framing_and_the_body() {
    // Contract 2 changes only when a delivery arrives: the same signed bytes
    // authenticate and parse identically whichever contract names them, and
    // the contract travels to the journal beside the parsed block.
    let raw = finalized_body(0x22, 10, 0xa0, 0x9f);
    let config = receiver();
    let h = sign(context(0x22, 0x33), 1, NOW, &raw);
    let signed = authenticate(&config, Some(&h), &raw, NOW).unwrap();
    let Delivery::Finalized(parsed) = parse_authenticated(&config, &signed, &raw).unwrap() else {
        panic!("expected block");
    };
    assert_eq!(parsed.contract, Contract::Unnamed);
    for (name, expected) in [("rtao-block-webhooks/1", Contract::V1), ("rtao-block-webhooks/2", Contract::V2)] {
        let mut n = parsed.clone();
        n.contract = contract(Some(name)).unwrap();
        assert_eq!(n.contract, expected);
        assert_eq!((&n.header, &n.delivery_id, &n.body_sha256), (&parsed.header, &parsed.delivery_id, &parsed.body_sha256));
        // The Worker-to-journal message carries it, and an older message
        // without the field still reads (as unnamed).
        let wire = serde_json::to_value(&*n).unwrap();
        assert_eq!(wire["contract"], serde_json::json!(if expected == Contract::V1 { "v1" } else { "v2" }));
        let back: BlockNotification = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back, *n);
        let mut old = wire;
        old.as_object_mut().unwrap().remove("contract");
        let back: BlockNotification = serde_json::from_value(old).unwrap();
        assert_eq!(back.contract, Contract::Unnamed);
    }
}
