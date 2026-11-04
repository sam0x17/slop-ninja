use super::*;
use sha2::{Digest, Sha256};

/// Synthetic edge cases with the scorer's counts (tokenizers 0.22.2, onig,
/// `encode(text, true)`), written by `reference golden`.
const GOLDEN: &str = include_str!("../table/golden-v1.json");

fn none(_: usize, _: usize) -> bool {
    true
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn the_table_is_the_pinned_tokenizers_and_parses() {
    assert_eq!(hex(&Sha256::digest(TABLE)), TABLE_SHA256);
    let t = Table::parse(TABLE).expect("table parses");
    assert_eq!(hex(t.source_sha256), SOURCE_TOKENIZER_SHA256);
    assert_eq!(frame_tokens(), Ok(2));
    // A truncated or extended table is refused, never read past its end.
    assert!(Table::parse(&TABLE[..TABLE.len() - 1]).is_none());
    let mut longer = TABLE.to_vec();
    longer.push(0);
    assert!(Table::parse(&longer).is_none());
    assert!(Table::parse(b"SNTOK001").is_none());
}

#[test]
fn every_golden_case_counts_as_the_scorer_counts() {
    let g: serde_json::Value = serde_json::from_str(GOLDEN).expect("golden json");
    assert_eq!(g["tokenizer_sha256"], SOURCE_TOKENIZER_SHA256);
    let cases = g["cases"].as_array().expect("cases");
    assert!(cases.len() > 300);
    let mut bad = Vec::new();
    for c in cases {
        let text = c["text"].as_str().expect("text");
        let want = c["tokens"].as_u64().expect("tokens");
        let got = count(text, &mut none).expect("count");
        if got != want {
            bad.push((text.escape_unicode().to_string(), want, got));
        }
        for limit in [want.saturating_sub(1), want, want + 1] {
            assert_eq!(
                fits(text, limit, &mut none),
                Ok(want <= limit),
                "fits({limit}) of a {want}-token case"
            );
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} cases differ: {:?}",
        bad.len(),
        cases.len(),
        &bad[..bad.len().min(5)]
    );
}

#[test]
fn the_frame_alone_and_fixed_cases() {
    assert_eq!(count("", &mut none), Ok(2));
    // Added tokens are one token each, special ones included; [MASK] takes
    // the whitespace before it with it.
    assert_eq!(count("[SEP]", &mut none), Ok(3));
    assert_eq!(count("[MASK]", &mut none), count("   [MASK]", &mut none));
}

#[test]
fn fits_stops_past_the_limit_and_short_nfc_text_needs_no_scratch() {
    let long = "word ".repeat(3000);
    let n = count(&long, &mut none).expect("count");
    assert!(n > 2048);
    assert_eq!(fits(&long, 2048, &mut none), Ok(false));
    assert_eq!(fits(&long, n, &mut none), Ok(true));
    let mut asked = 0usize;
    let short = "A short sentence that is already in NFC.";
    assert_eq!(
        fits(short, 2048, &mut |_, _| {
            asked += 1;
            true
        }),
        Ok(true)
    );
    assert_eq!(asked, 0, "the byte bound answers without tokenizing");
}

#[test]
fn scratch_is_offered_first_and_bounded_by_the_longest_pre_token() {
    // Pre-tokens of at most 64 bytes: one symbol buffer and one queue, each
    // offered once, at their exact layouts.
    let mut blocks = Vec::new();
    let text = "Plain words, then one more sentence. ".repeat(40);
    count(&text, &mut |b, a| {
        blocks.push((b, a));
        true
    })
    .expect("count");
    assert_eq!(
        blocks,
        vec![(64 * size_of::<Sym>(), align_of::<Sym>()), (64 * 3 * 8, 8)]
    );
    // A pre-token of 1,001 bytes (a space and 1,000 letters) grows the
    // buffers once, to its length.
    let mut blocks = Vec::new();
    count(&format!("short {}", "x".repeat(1000)), &mut |b, a| {
        blocks.push((b, a));
        true
    })
    .expect("count");
    assert_eq!(blocks.last(), Some(&(1001 * 3 * 8, 8)));
    assert!(blocks.iter().map(|b| b.0).sum::<usize>() <= 2 * 1001 * (size_of::<Sym>() + 24));
    // A refusal ends the count cleanly.
    assert_eq!(
        count("refused", &mut |_, _| false),
        Err(Refused::Scratch(64 * size_of::<Sym>()))
    );
    // A piece that is not NFC is normalized into one offered block.
    let mut blocks = Vec::new();
    count("A\u{30a}ngstr\u{f6}m", &mut |b, a| {
        blocks.push((b, a));
        true
    })
    .expect("count");
    assert_eq!(blocks.first(), Some(&("\u{c5}ngstr\u{f6}m".len(), 1)));
}

#[test]
fn no_input_panics() {
    // Lone combining marks, every whitespace kind at the end, a run of each
    // added token's first byte, and the largest scalar values.
    for s in [
        "\u{301}",
        "\u{301}\u{301}x",
        " \u{3000}",
        "\t",
        "|||",
        "[[[[",
        "<<|",
        "\u{10ffff}",
        "'",
        "' ",
        "''s",
        " '",
    ] {
        assert!(count(s, &mut none).is_ok());
    }
}
