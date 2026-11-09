//! The pools' English screen on the shared fixture of synthetic texts that
//! the detect Worker's port (`subnet/detect-worker/src/english.rs`) is
//! checked against: the recorded verdicts and shares must be this
//! function's, bit for bit.
//!
//! To record the fixture again after a deliberate change to the screen, run
//! this test with `SLOP_NINJA_WRITE_ENGLISH_FIXTURE=1`; it rewrites the
//! verdicts and shares in place, keeping the names and texts.

use grammar_a_pools::text::{english_screen, english_stopword_share, latin_letter_share};
use serde_json::{Value, json};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../subnet/detect-worker/tests/fixtures/english-screen.json");

fn record(text: &str) -> (Value, Value, Value) {
    (
        json!(english_screen(text)),
        json!(latin_letter_share(text)),
        json!(english_stopword_share(text)),
    )
}

#[test]
fn the_fixture_holds_the_pools_verdicts() {
    let raw = std::fs::read_to_string(FIXTURE).expect("fixture");
    let mut v: Value = serde_json::from_str(&raw).expect("fixture json");
    let write = std::env::var("SLOP_NINJA_WRITE_ENGLISH_FIXTURE").as_deref() == Ok("1");
    let cases = v["cases"].as_array_mut().expect("cases");
    assert!(cases.len() >= 12);
    for c in cases.iter_mut() {
        let text = c["text"].as_str().expect("text").to_owned();
        let (screen, latin, stop) = record(&text);
        if write {
            c["screen"] = screen;
            c["latin_letter_share"] = latin;
            c["english_stopword_share"] = stop;
        } else {
            assert_eq!(c["screen"], screen, "{}", c["name"]);
            let bits = |x: &Value| x.as_f64().map(f64::to_bits);
            assert_eq!(bits(&c["latin_letter_share"]), bits(&latin), "{}", c["name"]);
            assert_eq!(bits(&c["english_stopword_share"]), bits(&stop), "{}", c["name"]);
        }
    }
    if write {
        std::fs::write(FIXTURE, serde_json::to_string_pretty(&v).expect("json") + "\n").expect("write fixture");
    }
}
