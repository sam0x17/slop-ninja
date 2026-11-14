//! The stub's ops pass the Workers' own check, and its variants fail it the
//! way the suites expect.

use serde_json::{Map, json};
use slop_ninja_dev_op_stub::{DEFAULT_VALID_MS, Spec, args, key_from_seed, nonce, public, signed};
use slop_ninja_signed_op::{Env, MemoryLedger, Nonce, OperatorKey, Refusal, Target, Verifier};

/// A throwaway test seed, made at run time: 32 bytes of one value.
fn seed() -> String {
    format!("{:02x}", 0x5au8).repeat(32)
}
const NOW: u64 = 1_800_000_000_000;

fn spec(target: Target, env: Env, op: &str, a: Map<String, serde_json::Value>) -> Spec {
    Spec {
        target,
        env,
        op: op.into(),
        args: a,
        issued_ms: NOW,
        valid_ms: DEFAULT_VALID_MS,
        nonce: Nonce([7; 16]),
    }
}

fn verifier(accepts: &[Target]) -> Verifier {
    let key = key_from_seed(&seed()).expect("seed");
    let (hex, _) = public(&key);
    Verifier::new(
        OperatorKey::parse(&hex).expect("var form"),
        Env::Staging,
        accepts,
    )
}

#[test]
fn a_signed_op_verifies_on_its_own_worker() {
    let key = key_from_seed(&format!("  {}\n", seed())).expect("seed with whitespace");
    let (hex, fpr) = public(&key);
    assert_eq!(hex.len(), 64);
    assert!(
        hex.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    let a = args(r#"{"name":"DETECT_URLS","state":"off","reason":"dev"}"#).expect("args");
    let wire = signed(&key, &spec(Target::Detect, Env::Staging, "set_switch", a)).expect("signed");
    assert_eq!(wire.key, fpr);
    let body = serde_json::to_vec(&wire).expect("json");
    let v = verifier(&[Target::Detect, Target::Edge])
        .verify(&body, NOW + 1)
        .expect("verifies");
    assert_eq!(v.op.op, "set_switch");
    assert_eq!(v.op.args["state"], json!("off"));
}

#[test]
fn the_variants_fail_as_the_suites_expect() {
    let key = key_from_seed(&seed()).expect("seed");
    let detect = verifier(&[Target::Detect, Target::Edge]);
    let body = |s: &Spec| serde_json::to_vec(&signed(&key, s).expect("signed")).expect("json");
    let empty = Map::new();
    // Another environment, another target, an op of another target.
    let prod = spec(Target::Detect, Env::Production, "status", empty.clone());
    assert_eq!(
        detect.verify(&body(&prod), NOW).unwrap_err(),
        Refusal::WrongEnv
    );
    let acct = spec(Target::Account, Env::Staging, "reset_quota", empty.clone());
    assert_eq!(
        detect.verify(&body(&acct), NOW).unwrap_err(),
        Refusal::WrongTarget
    );
    // Expired, and issued too far ahead.
    let mut old = spec(Target::Detect, Env::Staging, "status", empty.clone());
    old.issued_ms = NOW - 2 * DEFAULT_VALID_MS;
    assert_eq!(
        detect.verify(&body(&old), NOW).unwrap_err(),
        Refusal::Expired
    );
    // A body changed after signing.
    let mut w = signed(
        &key,
        &spec(Target::Detect, Env::Staging, "status", empty.clone()),
    )
    .expect("signed");
    w.op = "purge".into();
    assert_eq!(
        detect
            .verify(&serde_json::to_vec(&w).expect("json"), NOW)
            .unwrap_err(),
        Refusal::BadSignature
    );
    // Another key.
    let other = key_from_seed(&"11".repeat(32)).expect("seed");
    let wrong = serde_json::to_vec(
        &signed(
            &other,
            &spec(Target::Detect, Env::Staging, "status", empty.clone()),
        )
        .expect("signed"),
    )
    .expect("json");
    assert_eq!(
        detect.verify(&wrong, NOW).unwrap_err(),
        Refusal::BadSignature
    );
    // One nonce twice: the second claim is refused.
    let same = spec(Target::Detect, Env::Staging, "status", empty);
    let mut ledger = MemoryLedger::default();
    assert!(
        detect
            .verify_and_claim(&body(&same), NOW, &mut ledger)
            .is_ok()
    );
    assert_eq!(
        detect
            .verify_and_claim(&body(&same), NOW, &mut ledger)
            .unwrap_err(),
        Refusal::ReusedNonce
    );
}

#[test]
fn bad_inputs_are_refused_before_signing() {
    assert!(key_from_seed("abc").is_err());
    assert!(key_from_seed(&"zz".repeat(32)).is_err());
    assert!(args("[1]").is_err());
    assert!(args("{").is_err());
    assert!(nonce("ABCDEF0123456789ABCDEF0123456789").is_err());
    assert!(nonce(&"0".repeat(32)).is_ok());
    // A fraction in the arguments is not an op's form.
    let key = key_from_seed(&seed()).expect("seed");
    let s = spec(
        Target::Account,
        Env::Staging,
        "grant_credit",
        args(r#"{"tokens":1.5}"#).expect("args"),
    );
    assert!(signed(&key, &s).is_err());
}
