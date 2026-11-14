//! The op envelope's refusals: wrong target, wrong environment, expired, not
//! yet valid, reused nonce, bad signature, unknown op and malformed bodies,
//! plus the canonical encoding's golden bytes.

use ed25519_dalek::SigningKey;
use serde_json::{Map, Value, json};
use slop_ninja_signed_op::*;

const NOW: u64 = 1_763_000_000_000;

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn op(target: Target, env: Env, name: &str, nonce: u8) -> Op {
    let mut args = Map::new();
    args.insert("name".into(), json!("DETECT_STORE"));
    args.insert("state".into(), json!("off"));
    args.insert("reason".into(), json!("drill"));
    Op { target, env, issued_ms: NOW, expires_ms: NOW + 60_000, nonce: Nonce([nonce; 16]), op: name.into(), args }
}

fn body(op: &Op, k: &SigningKey) -> Vec<u8> {
    serde_json::to_vec(&sign(op, k).unwrap()).unwrap()
}

fn detect(k: &SigningKey, env: Env) -> Verifier {
    Verifier::new(OperatorKey::from_verifying_key(k.verifying_key()), env, &[Target::Detect, Target::Edge])
}

#[test]
fn a_good_op_verifies_once() {
    let k = key(7);
    let v = detect(&k, Env::Staging);
    let o = op(Target::Detect, Env::Staging, "set_switch", 1);
    let b = body(&o, &k);
    let mut ledger = MemoryLedger::default();
    let got = v.verify_and_claim(&b, NOW + 1, &mut ledger).unwrap();
    assert_eq!(got.op, o);
    assert_eq!(got.actor(), format!("operator:{}", OperatorKey::from_verifying_key(k.verifying_key()).fingerprint()));
    assert_eq!(v.verify_and_claim(&b, NOW + 2, &mut ledger), Err(Refusal::ReusedNonce));
    // The same nonce under another target or environment is another nonce_h.
    let mut other = o.clone();
    other.env = Env::Production;
    assert_ne!(other.nonce_h(), o.nonce_h());
}

#[test]
fn wrong_target_is_refused() {
    let k = key(7);
    let detect = detect(&k, Env::Production);
    let account =
        Verifier::new(OperatorKey::from_verifying_key(k.verifying_key()), Env::Production, &[Target::Account]);
    let to_account = body(&op(Target::Account, Env::Production, "set_switch", 2), &k);
    assert_eq!(detect.verify(&to_account, NOW).unwrap_err(), Refusal::WrongTarget);
    let to_detect = body(&op(Target::Detect, Env::Production, "set_switch", 3), &k);
    assert_eq!(account.verify(&to_detect, NOW).unwrap_err(), Refusal::WrongTarget);
    // The edge target reaches detect for edge_coverage only.
    let edge = body(&op(Target::Edge, Env::Production, "edge_coverage", 4), &k);
    assert!(detect.verify(&edge, NOW).is_ok());
    let edge_other = body(&op(Target::Edge, Env::Production, "purge", 5), &k);
    assert_eq!(detect.verify(&edge_other, NOW).unwrap_err(), Refusal::UnknownOp);
    // An account op addressed to detect.
    let misfiled = body(&op(Target::Detect, Env::Production, "grant_credit", 6), &k);
    assert_eq!(detect.verify(&misfiled, NOW).unwrap_err(), Refusal::UnknownOp);
    // The X sign-in purge reaches the account Worker, and only it.
    let purge = body(&op(Target::Account, Env::Production, "x_signin_purge", 10), &k);
    assert_eq!(account.verify(&purge, NOW).unwrap().op.op, "x_signin_purge");
    let purge_to_detect = body(&op(Target::Detect, Env::Production, "x_signin_purge", 11), &k);
    assert_eq!(detect.verify(&purge_to_detect, NOW).unwrap_err(), Refusal::UnknownOp);
}

#[test]
fn wrong_env_is_refused() {
    let k = key(7);
    let staging_op = body(&op(Target::Detect, Env::Staging, "status", 8), &k);
    assert_eq!(detect(&k, Env::Production).verify(&staging_op, NOW).unwrap_err(), Refusal::WrongEnv);
    // With separate keys per environment the signature fails first.
    let prod_key = key(9);
    assert_eq!(detect(&prod_key, Env::Production).verify(&staging_op, NOW).unwrap_err(), Refusal::BadSignature);
}

#[test]
fn the_validity_window() {
    let k = key(7);
    let v = detect(&k, Env::Staging);
    let o = op(Target::Detect, Env::Staging, "status", 10);
    let b = body(&o, &k);
    assert!(v.verify(&b, o.expires_ms - 1).is_ok());
    assert_eq!(v.verify(&b, o.expires_ms).unwrap_err(), Refusal::Expired);
    assert_eq!(v.verify(&b, o.expires_ms + 86_400_000).unwrap_err(), Refusal::Expired);
    // Issued within the skew of now: accepted; beyond it: not yet valid.
    let skew = OP_POLICY_V1.clock_skew_ms;
    assert!(v.verify(&b, NOW - skew).is_ok());
    assert_eq!(v.verify(&b, NOW - skew - 1).unwrap_err(), Refusal::NotYetValid);
    // A window longer than the policy, or empty, is malformed.
    let mut long = op(Target::Detect, Env::Staging, "status", 11);
    long.expires_ms = long.issued_ms + OP_POLICY_V1.max_validity_ms + 1;
    assert_eq!(v.verify(&body(&long, &k), NOW).unwrap_err(), Refusal::Malformed);
    let mut empty = op(Target::Detect, Env::Staging, "status", 12);
    empty.expires_ms = empty.issued_ms;
    assert_eq!(v.verify(&body(&empty, &k), NOW).unwrap_err(), Refusal::Malformed);
}

#[test]
fn bad_signatures_are_refused() {
    let k = key(7);
    let v = detect(&k, Env::Staging);
    let o = op(Target::Detect, Env::Staging, "set_switch", 13);
    // Signed by another key.
    assert_eq!(v.verify(&body(&o, &key(8)), NOW).unwrap_err(), Refusal::BadSignature);
    // Another key's signature presented under this key's fingerprint.
    let mut w = sign(&o, &key(8)).unwrap();
    w.key = OperatorKey::from_verifying_key(k.verifying_key()).fingerprint();
    assert_eq!(v.verify(&serde_json::to_vec(&w).unwrap(), NOW).unwrap_err(), Refusal::BadSignature);
    // Every signed field is bound: change one after signing.
    let good = sign(&o, &k).unwrap();
    let mut edits: Vec<Wire> = Vec::new();
    let mut e = good.clone();
    e.args = json!({"name": "DETECT_STORE", "state": "on", "reason": "drill"});
    edits.push(e);
    let mut e = good.clone();
    e.op = "status".into();
    edits.push(e);
    let mut e = good.clone();
    e.expires_ms += 1;
    edits.push(e);
    let mut e = good.clone();
    e.issued_ms -= 1;
    edits.push(e);
    let mut e = good.clone();
    e.nonce = Nonce([14; 16]).hex();
    edits.push(e);
    let mut e = good.clone();
    let mut sig = hex::decode(&e.sig).unwrap();
    sig[0] ^= 1;
    e.sig = hex::encode(sig);
    edits.push(e);
    for e in edits {
        assert_eq!(v.verify(&serde_json::to_vec(&e).unwrap(), NOW).unwrap_err(), Refusal::BadSignature, "{e:?}");
    }
    assert!(v.verify(&serde_json::to_vec(&good).unwrap(), NOW).is_ok());
}

#[test]
fn malformed_bodies_are_refused() {
    let k = key(7);
    let v = detect(&k, Env::Staging);
    let good = serde_json::to_value(sign(&op(Target::Detect, Env::Staging, "status", 15), &k).unwrap()).unwrap();
    let with = |f: &str, x: Value| {
        let mut w = good.clone();
        w[f] = x;
        serde_json::to_vec(&w).unwrap()
    };
    for b in [
        with("v", json!(2)),
        with("target", json!("Detect")),
        with("env", json!("prod")),
        with("nonce", json!("ABCDEF00ABCDEF00ABCDEF00ABCDEF00")),
        with("nonce", json!("00")),
        with("op", json!("Set-Switch")),
        with("args", json!([1, 2])),
        with("args", json!({"ratio": 0.5})),
        with("sig", json!("zz")),
        with("key", json!("not-a-key")),
        with("extra", json!(1)),
        b"{".to_vec(),
        b"null".to_vec(),
    ] {
        assert_eq!(v.verify(&b, NOW).unwrap_err(), Refusal::Malformed, "{}", String::from_utf8_lossy(&b));
    }
    // Nesting past the bound.
    let mut deep = json!(1);
    for _ in 0..(MAX_ARGS_DEPTH + 1) {
        deep = json!([deep]);
    }
    assert_eq!(v.verify(&with("args", json!({ "x": deep })), NOW).unwrap_err(), Refusal::Malformed);
}

#[test]
fn canonical_bytes_are_fixed() {
    let o = Op {
        target: Target::Account,
        env: Env::Production,
        issued_ms: 1,
        expires_ms: 2,
        nonce: Nonce([0xab; 16]),
        op: "reset_quota".into(),
        args: serde_json::from_str(r#"{"z":[true,null,-3],"kind":"email","a":{"y":"é\n","b":7}}"#).unwrap(),
    };
    let bytes = o.signed_bytes().unwrap();
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        "slop_ninja_signed_op_v1\n{\"args\":{\"a\":{\"b\":7,\"y\":\"\u{e9}\\n\"},\"kind\":\"email\",\"z\":[true,null,-3]},\
\"env\":\"production\",\"expires_ms\":2,\"issued_ms\":1,\"nonce\":\"abababababababababababababababab\",\
\"op\":\"reset_quota\",\"target\":\"account\",\"v\":1}"
    );
}

#[test]
fn keys_and_codes() {
    let k = key(7);
    let pk = OperatorKey::from_verifying_key(k.verifying_key());
    assert_eq!(OperatorKey::parse(&pk.hex()), Some(pk));
    assert_eq!(OperatorKey::parse(&pk.hex().to_uppercase()), None);
    assert_eq!(pk.fingerprint().len(), 16);
    assert_ne!(pk.fingerprint(), OperatorKey::from_verifying_key(key(8).verifying_key()).fingerprint());
    let codes: std::collections::HashSet<&str> = Refusal::ALL.iter().map(|r| r.code()).collect();
    assert_eq!(codes.len(), Refusal::ALL.len());
    for (t, o) in CATALOG {
        assert!(o.len() <= MAX_OP_NAME && o.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{o}");
        assert!(in_catalog(*t, o));
    }
    assert!(in_catalog(Target::Detect, "set_switch") && in_catalog(Target::Account, "set_switch"));
    assert!(!in_catalog(Target::Edge, "set_switch"));
    // X's class purge and the removal by hash are detect ops only; the
    // withdrawn oEmbed trust reset is no op at all.
    assert!(CATALOG.iter().all(|(_, o)| *o != "x_oembed_trust"));
    for o in ["x_purge", "x_remove"] {
        assert!(in_catalog(Target::Detect, o), "{o}");
        assert!(!in_catalog(Target::Account, o) && !in_catalog(Target::Edge, o), "{o}");
    }
    // The X sign-in purge is an account op only: the account Worker holds
    // the identities it deletes.
    assert!(in_catalog(Target::Account, "x_signin_purge"));
    assert!(!in_catalog(Target::Detect, "x_signin_purge") && !in_catalog(Target::Edge, "x_signin_purge"));
    // Every entry is listed once.
    let entries: std::collections::HashSet<_> = CATALOG.iter().collect();
    assert_eq!(entries.len(), CATALOG.len());
}

#[test]
fn a_parsed_envelope_verifies_as_its_bytes_do() {
    // A route whose body wraps the envelope parses it first and checks it
    // with verify_wire: the same answers as verify on the bare bytes.
    let k = key(7);
    let v = detect(&k, Env::Staging);
    let o = op(Target::Detect, Env::Staging, "restore", 3);
    let w = sign(&o, &k).unwrap();
    let bytes = serde_json::to_vec(&w).unwrap();
    assert_eq!(v.verify_wire(&w, NOW + 1), v.verify(&bytes, NOW + 1));
    assert_eq!(v.verify_wire(&w, NOW + 1).unwrap().op, o);
    let mut bad = w.clone();
    bad.args = json!({"case_id": "td-other"});
    assert_eq!(v.verify_wire(&bad, NOW + 1), Err(Refusal::BadSignature));
    assert_eq!(v.verify_wire(&w, NOW + 60_000), Err(Refusal::Expired));
    let mut env = w.clone();
    env.env = "production".into();
    assert_eq!(v.verify_wire(&env, NOW + 1), Err(Refusal::BadSignature));
}
