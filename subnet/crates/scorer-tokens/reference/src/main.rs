//! The reference side of `slop-ninja-scorer-tokens`, run natively with the
//! scorer's own tokenizers build (0.22.2, onig):
//!
//! - `table <tokenizer.json> <out.bin>`: derives the compiled-in table. It
//!   refuses a tokenizer that uses anything the counter does not implement.
//!   The letter, number and whitespace classes are read off the scorer's
//!   pre-tokenizer itself, one probe per code point and class.
//! - `golden <tokenizer.json> <out.json>`: synthetic edge cases with the
//!   scorer's counts, for the crate's own tests (no onig needed there).
//! - `sweep <tokenizer.json> <docs> <seed>`: every code point in context, then
//!   `docs` seeded synthetic documents (short, and long around the limit),
//!   compared count for count and on `fits` at the artifact spec's limit
//!   (`MAX_TOKENS_INCLUDING_SPECIALS`, 2,048).
//! - `parity <tokenizer.json> <file.jsonl>...`: the `text` field of every
//!   line, compared the same way. Prints counts only, never text.
//!
//! Every command prints one JSON summary; a disagreement exits non-zero.

use sha2::{Digest, Sha256};
use slop_ninja_scorer_tokens as sct;
use std::collections::{BTreeMap, HashMap};
use std::io::BufRead;
use tokenizers::Tokenizer;
use tokenizers::tokenizer::{OffsetReferential, OffsetType, PreTokenizedString, PreTokenizer};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// The scorer's limit, read from the artifact spec (the coordinator's check
/// uses the same constant).
const LIMIT: u64 = slop_ninja_artifact_check::text::MAX_TOKENS_INCLUDING_SPECIALS;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let r = match a.get(1).map(String::as_str) {
        Some("table") if a.len() == 4 => table(&a[2], &a[3]),
        Some("golden") if a.len() == 4 => golden(&a[2], &a[3]),
        Some("sweep") if a.len() == 5 => sweep(&a[2], a[3].parse().unwrap_or(0), a[4].parse().unwrap_or(1)),
        Some("parity") if a.len() >= 4 => parity(&a[2], &a[3..]),
        _ => Err("usage: table|golden <tokenizer.json> <out> | sweep <tokenizer.json> <docs> <seed> | parity <tokenizer.json> <jsonl>...".into()),
    };
    match r {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    }
}

fn load(path: &str) -> Res<(Vec<u8>, Tokenizer)> {
    let bytes = std::fs::read(path)?;
    let tok = Tokenizer::from_bytes(&bytes).map_err(|e| format!("tokenizer: {e}"))?;
    Ok((bytes, tok))
}

fn reference(tok: &Tokenizer, text: &str) -> Res<u64> {
    Ok(tok
        .encode(text, true)
        .map_err(|e| format!("encode: {e}"))?
        .get_ids()
        .len() as u64)
}

fn ours(text: &str) -> Res<u64> {
    sct::count(text, &mut |_, _| true).map_err(|e| format!("count: {e:?}").into())
}

fn ours_fits(text: &str) -> Res<bool> {
    sct::fits(text, LIMIT, &mut |_, _| true).map_err(|e| format!("fits: {e:?}").into())
}

// ------------------------------------------------------------------ table

/// GPT-2's byte-level alphabet: byte -> char.
fn bytes_char() -> [char; 256] {
    let mut bs: Vec<u32> = (u32::from(b'!')..=u32::from(b'~'))
        .chain(0xa1..=0xac)
        .chain(0xae..=0xff)
        .collect();
    let mut cs = bs.clone();
    let mut n = 0;
    for b in 0..256u32 {
        if !bs.contains(&b) {
            bs.push(b);
            cs.push(256 + n);
            n += 1;
        }
    }
    let mut out = ['\0'; 256];
    for (b, c) in bs.iter().zip(&cs) {
        out[*b as usize] = char::from_u32(*c).unwrap_or('\0');
    }
    out
}

fn need(ok: bool, what: &str) -> Res<()> {
    if ok {
        Ok(())
    } else {
        Err(format!("unsupported tokenizer: {what}").into())
    }
}

fn pieces(pre: &impl PreTokenizer, s: &str) -> Res<usize> {
    let mut p = PreTokenizedString::from(s);
    pre.pre_tokenize(&mut p)
        .map_err(|e| format!("pre-tokenize: {e}"))?;
    Ok(p.get_splits(OffsetReferential::Original, OffsetType::Byte)
        .len())
}

fn ranges(cps: &[u32]) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &c in cps {
        match out.last_mut() {
            Some(last) if last.1 + 1 == c => last.1 = c,
            _ => out.push((c, c)),
        }
    }
    out
}

fn table(path: &str, out: &str) -> Res<bool> {
    let (bytes, tok) = load(path)?;
    let j: serde_json::Value = serde_json::from_slice(&bytes)?;
    // What the counter implements, and nothing else.
    need(
        j["normalizer"] == serde_json::json!({"type": "NFC"}),
        "normalizer other than NFC",
    )?;
    let pt = &j["pre_tokenizer"];
    need(
        pt["type"] == "ByteLevel" && pt["add_prefix_space"] == false && pt["use_regex"] == true,
        "pre-tokenizer",
    )?;
    let pp = &j["post_processor"];
    need(pp["type"] == "TemplateProcessing", "post-processor")?;
    let single = pp["single"].as_array().ok_or("single template")?;
    need(
        single
            .iter()
            .filter(|x| x.get("Sequence").is_some())
            .count()
            == 1,
        "single template sequence",
    )?;
    let specials = single
        .iter()
        .filter(|x| x.get("SpecialToken").is_some())
        .count() as u32;
    need(
        j["truncation"].is_null() && j["padding"].is_null(),
        "truncation or padding",
    )?;
    let m = &j["model"];
    need(m["type"] == "BPE", "model type")?;
    need(
        m["dropout"].is_null() && m["unk_token"].is_null(),
        "dropout or unk token",
    )?;
    need(
        m["continuing_subword_prefix"].is_null() && m["end_of_word_suffix"].is_null(),
        "subword affixes",
    )?;
    need(
        m["byte_fallback"] == false && m["ignore_merges"] == false,
        "byte fallback or ignore_merges",
    )?;
    let vocab: HashMap<String, u32> = serde_json::from_value(m["vocab"].clone())?;
    need(
        vocab.values().all(|&v| v < 0xffff),
        "vocabulary ids past u16",
    )?;
    let bc = bytes_char();
    let mut byte_ids = [0xffffu16; 256];
    for (b, c) in bc.iter().enumerate() {
        if let Some(&id) = vocab.get(&c.to_string()) {
            byte_ids[b] = id as u16;
        }
    }
    // Merges as tokenizers loads them: rank = position, a later duplicate
    // replaces an earlier one, the new id is the concatenation's.
    let mut merges: HashMap<(u32, u32), (u32, u32)> = HashMap::new();
    for (rank, e) in m["merges"].as_array().ok_or("merges")?.iter().enumerate() {
        let (a, b) = match e {
            serde_json::Value::String(s) => s
                .split_once(' ')
                .map(|(a, b)| (a.to_owned(), b.to_owned()))
                .ok_or("merge")?,
            serde_json::Value::Array(v) if v.len() == 2 => (
                v[0].as_str().ok_or("merge")?.to_owned(),
                v[1].as_str().ok_or("merge")?.to_owned(),
            ),
            _ => return Err("merge entry".into()),
        };
        let (Some(&ia), Some(&ib), Some(&nid)) =
            (vocab.get(&a), vocab.get(&b), vocab.get(&format!("{a}{b}")))
        else {
            return Err(format!("merge {rank} names a token outside the vocabulary").into());
        };
        need(rank < 0xffff, "merge rank past u16")?;
        merges.insert((ia, ib), (rank as u32, nid));
    }
    let mut merges: Vec<_> = merges.into_iter().collect();
    merges.sort_unstable();
    // Added tokens, split by `normalized` as AddedVocabulary does.
    let norm = tok.get_normalizer().ok_or("normalizer")?;
    let (mut raw, mut normd) = (Vec::new(), Vec::new());
    for t in j["added_tokens"].as_array().ok_or("added tokens")? {
        let content = t["content"].as_str().ok_or("content")?;
        need(
            !content.is_empty() && content.len() < 0xffff,
            "added token length",
        )?;
        need(
            t["single_word"] == false && t["rstrip"] == false,
            "single_word or rstrip",
        )?;
        let flags = u8::from(t["lstrip"] == true);
        if t["normalized"] == true {
            let n = tokenizers::NormalizedString::from(content);
            let mut n2 = n.clone();
            tokenizers::Normalizer::normalize(norm, &mut n2).map_err(|e| format!("{e}"))?;
            need(
                n2.get() == content,
                "normalized added token changes under the normalizer",
            )?;
            normd.push((content.to_owned(), flags));
        } else {
            raw.push((content.to_owned(), flags));
        }
    }
    // The classes, from the scorer's pre-tokenizer: c is a letter when "a"+c
    // is one pre-token, a number when "1"+c is, whitespace when "\t"+c is,
    // and in the remaining class when "!"+c is. Exactly one must hold.
    let pre = tok.get_pre_tokenizer().ok_or("pre-tokenizer")?;
    let (mut letters, mut numbers, mut spaces) = (Vec::new(), Vec::new(), Vec::new());
    let mut other = 0u64;
    for cp in 0..=0x10ffffu32 {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        let probe = |p: char| -> Res<bool> { Ok(pieces(pre, &format!("{p}{c}"))? == 1) };
        let k = [probe('a')?, probe('1')?, probe('\t')?, probe('!')?];
        if k.iter().filter(|x| **x).count() != 1 {
            return Err(format!("code point U+{cp:04X} is in {k:?} classes").into());
        }
        match k {
            [true, ..] => letters.push(cp),
            [_, true, ..] => numbers.push(cp),
            [_, _, true, _] => spaces.push(cp),
            _ => other += 1,
        }
    }
    let (l, n, s) = (ranges(&letters), ranges(&numbers), ranges(&spaces));
    let list = |v: &[(String, u8)]| -> Vec<u8> {
        let mut o = Vec::new();
        for (c, f) in v {
            o.extend_from_slice(&(c.len() as u16).to_le_bytes());
            o.push(*f);
            o.extend_from_slice(c.as_bytes());
        }
        o
    };
    let (raw_b, norm_b) = (list(&raw), list(&normd));
    let mut b = Vec::new();
    b.extend_from_slice(b"SNTOK001");
    b.extend_from_slice(&Sha256::digest(&bytes));
    for x in [
        specials,
        merges.len() as u32,
        raw.len() as u32,
        raw_b.len() as u32,
        normd.len() as u32,
        norm_b.len() as u32,
    ] {
        b.extend_from_slice(&x.to_le_bytes());
    }
    for x in [l.len() as u32, n.len() as u32, s.len() as u32] {
        b.extend_from_slice(&x.to_le_bytes());
    }
    for id in byte_ids {
        b.extend_from_slice(&id.to_le_bytes());
    }
    for ((a, bb), (rank, nid)) in &merges {
        b.extend_from_slice(&((a << 16) | bb).to_le_bytes());
        b.extend_from_slice(&(*rank as u16).to_le_bytes());
        b.extend_from_slice(&(*nid as u16).to_le_bytes());
    }
    b.extend_from_slice(&raw_b);
    b.extend_from_slice(&norm_b);
    for r in [&l, &n, &s] {
        for (lo, hi) in r.iter() {
            b.extend_from_slice(&lo.to_le_bytes());
            b.extend_from_slice(&hi.to_le_bytes());
        }
    }
    std::fs::write(out, &b)?;
    println!(
        "{}",
        serde_json::json!({"table": out, "bytes": b.len(), "sha256": hex(&Sha256::digest(&b)),
            "tokenizer_sha256": hex(&Sha256::digest(&bytes)), "specials": specials, "merges": merges.len(),
            "missing_byte_ids": byte_ids.iter().filter(|x| **x == 0xffff).count(),
            "raw_added": raw.len(), "normalized_added": normd.len(),
            "letters": [letters.len(), l.len()], "numbers": [numbers.len(), n.len()],
            "spaces": [spaces.len(), s.len()], "other": other})
    );
    Ok(true)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// --------------------------------------------------------------- synthetic

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Edge-case atoms: contractions, whitespace runs and kinds, added and
/// special tokens (with lstrip), NFC-sensitive sequences (including ones
/// whose composition or ordering differs after Unicode 9), scripts without
/// spaces, numbers of every kind, invisible characters and emoji.
const ATOMS: &[&str] = &[
    "the",
    "The",
    "word",
    " a",
    " ",
    "  ",
    "   ",
    "    ",
    "\n",
    "\n\n",
    "\n\n\n",
    "\t",
    " \n ",
    "\r\n",
    " \t ",
    "'s",
    "'S",
    "'ll",
    "'re",
    "'ve",
    "'m",
    "'d",
    "'t",
    "''",
    "don't",
    "it's",
    "O'Neil",
    "'x",
    "1234",
    "3.14",
    "١٢٣",
    "Ⅻ",
    "½",
    "²",
    "e\u{301}",
    "\u{212b}",
    "\u{1100}\u{1161}\u{11a8}",
    "한국어",
    "日本語のテキストです",
    "中文",
    "😀",
    "👩\u{200d}👩\u{200d}👧",
    "[CLS]",
    "[SEP]",
    "[MASK]",
    " [MASK]",
    "  [MASK]",
    "\n[MASK]",
    "|||IP_ADDRESS|||",
    "|||EMAIL_ADDRESS|||",
    "|||PHONE_NUMBER|||",
    "<|endoftext|>",
    "<|padding|>",
    "[unused3]",
    "[unused42]",
    "[UNK]",
    "[PAD]",
    "---",
    "...",
    "?!",
    "!!!",
    "\u{a0}",
    "\u{3000}",
    "\u{2028}",
    "\u{2029}",
    "\u{85}",
    "\u{200b}",
    "\u{feff}",
    "\u{1c}",
    "\u{1f}",
    "\u{ad}",
    "ﬁ",
    "\u{fb2c}",
    "\u{1d160}",
    "\u{11099}\u{110ba}",
    "\u{0958}",
    "\u{0344}",
    "\u{0345}\u{0301}a",
    "a\u{0323}\u{0307}",
    "a\u{0307}\u{0323}",
    "\u{1e9b}\u{0323}",
    "ǅ",
    "ß",
    "İ",
    "ﬀ",
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "https://example.com/a_b-c?d=e&f=1",
    "\u{0301}",
    "\u{1fae8}",
    "\u{11f04}\u{11f41}",
    "\u{1e4d0}",
    "\u{0cf3}",
    "\u{1e030}",
    "\u{0897}a\u{0301}",
    "\u{1f1fa}\u{1f1f8}",
    "Ω",
    "\u{2126}",
    "Å",
    "\u{0041}\u{030a}",
    "ḱṷṓn",
    "Ελληνικά",
    "русский",
    "עברית",
    "العربية",
    "हिन्दी",
    "ไทย",
    "\u{0}",
    "\u{7f}",
    "a\u{30fc}",
    "    \n    ",
    "                                ",
    "=====",
    "`code`",
    "\\",
    "\"quoted\"",
    "\u{201c}curly\u{201d}",
    "\u{2019}s",
    "x\u{2019}ll",
    "12,345.67",
    "$100",
    "#tag",
    "@name",
];

fn synthetic(r: &mut Rng, atoms: usize) -> String {
    let mut s = String::new();
    for _ in 0..atoms {
        match r.below(4) {
            0 => {
                let n = 1 + r.below(9);
                for _ in 0..n {
                    s.push((b'a' + r.below(26) as u8) as char);
                }
            }
            1 => s.push(' '),
            _ => s.push_str(ATOMS[r.below(ATOMS.len())]),
        }
    }
    s
}

/// A long document of `words` English-like words with sentence breaks.
fn long_doc(r: &mut Rng, words: usize) -> String {
    let lex = [
        "the", "council", "approved", "a", "plan", "on", "Monday", "after", "debate", "of",
        "costs", "river", "in", "northern", "England", "rises", "near", "hills", "and", "flows",
        "east", "toward", "sea", "botanist",
    ];
    let mut s = String::new();
    for i in 0..words {
        if i > 0 {
            s.push(if r.below(12) == 0 { '\n' } else { ' ' });
        }
        s.push_str(lex[r.below(lex.len())]);
        if r.below(9) == 0 {
            s.push_str(ATOMS[r.below(ATOMS.len())]);
        }
        if r.below(10) == 0 {
            s.push('.');
        }
    }
    s
}

#[derive(Default)]
struct Tally {
    docs: u64,
    mismatches: u64,
    fits_disagree: u64,
    over_limit: u64,
    max_tokens: u64,
    examples: Vec<serde_json::Value>,
}

impl Tally {
    fn check(&mut self, tok: &Tokenizer, text: &str, show: bool) -> Res<()> {
        let (want, got) = (reference(tok, text)?, ours(text)?);
        self.docs += 1;
        self.max_tokens = self.max_tokens.max(want);
        self.over_limit += u64::from(want > LIMIT);
        if want != got {
            self.mismatches += 1;
            if show && self.examples.len() < 20 {
                self.examples
                    .push(serde_json::json!({"text": text, "scorer": want, "ours": got}));
            }
        }
        if ours_fits(text)? != (want <= LIMIT) {
            self.fits_disagree += 1;
        }
        Ok(())
    }
    fn json(&self) -> serde_json::Value {
        serde_json::json!({"docs": self.docs, "mismatches": self.mismatches, "fits_disagree": self.fits_disagree,
            "over_2048": self.over_limit, "max_tokens": self.max_tokens, "examples": self.examples})
    }
    fn clean(&self) -> bool {
        self.mismatches == 0 && self.fits_disagree == 0
    }
}

fn source_matches(bytes: &[u8]) -> Res<()> {
    need(
        hex(&Sha256::digest(bytes)) == sct::SOURCE_TOKENIZER_SHA256,
        "not the table's source tokenizer",
    )
}

fn golden(path: &str, out: &str) -> Res<bool> {
    let (bytes, tok) = load(path)?;
    source_matches(&bytes)?;
    let mut cases: Vec<String> = ATOMS.iter().map(|a| (*a).to_owned()).collect();
    cases.extend(ATOMS.iter().map(|a| format!("x {a} y{a}{a}z")));
    cases.push(String::new());
    cases.push(" ".repeat(100));
    cases.push("\n".repeat(50));
    cases.push("a".repeat(600));
    cases.push("[MASK]".repeat(5) + "   [MASK] x  \u{3000}[MASK]");
    let mut r = Rng(20261104);
    for k in 0..200 {
        cases.push(synthetic(&mut r, 5 + k % 60));
    }
    let mut rows = Vec::new();
    for c in &cases {
        rows.push(serde_json::json!({"text": c, "tokens": reference(&tok, c)?}));
    }
    let g = serde_json::json!({"schema": "slop_ninja_scorer_tokens_golden_v1", "tokenizer_sha256": sct::SOURCE_TOKENIZER_SHA256,
        "scorer": "tokenizers 0.22.2 (onig), Tokenizer::encode(text, true)", "cases": rows});
    std::fs::write(out, serde_json::to_vec_pretty(&g)?)?;
    println!(
        "{}",
        serde_json::json!({"golden": out, "cases": cases.len()})
    );
    Ok(true)
}

fn sweep(path: &str, docs: usize, seed: u64) -> Res<bool> {
    let (bytes, tok) = load(path)?;
    source_matches(&bytes)?;
    let mut cp = Tally::default();
    let mut batch = String::new();
    let mut in_batch = 0;
    let mut bad_batches = Vec::new();
    let flush = |batch: &mut String, cp: &mut Tally, bad: &mut Vec<String>| -> Res<()> {
        let before = cp.mismatches;
        cp.check(&tok, batch, false)?;
        if cp.mismatches > before && bad.len() < 8 {
            bad.push(
                batch
                    .chars()
                    .take(64)
                    .collect::<String>()
                    .escape_unicode()
                    .to_string(),
            );
        }
        batch.clear();
        Ok(())
    };
    for c in (0..=0x10ffffu32).filter_map(char::from_u32) {
        batch.push_str(&format!("a{c}b {c}{c}x '{c}s 1{c}2  {c}\n{c}"));
        in_batch += 1;
        if in_batch == 64 {
            flush(&mut batch, &mut cp, &mut bad_batches)?;
            in_batch = 0;
        }
    }
    if !batch.is_empty() {
        flush(&mut batch, &mut cp, &mut bad_batches)?;
    }
    let mut short = Tally::default();
    let mut long = Tally::default();
    let mut r = Rng(seed);
    for _ in 0..docs {
        let atoms = 1 + r.below(120);
        short.check(&tok, &synthetic(&mut r, atoms), true)?;
    }
    for _ in 0..docs / 10 {
        let words = 1200 + r.below(900);
        long.check(&tok, &long_doc(&mut r, words), false)?;
    }
    let ok = cp.clean() && short.clean() && long.clean();
    println!(
        "{}",
        serde_json::json!({"code_points": cp.json(), "bad_batches": bad_batches, "synthetic": short.json(),
            "long": long.json(), "seed": seed, "clean": ok})
    );
    Ok(ok)
}

fn parity(path: &str, files: &[String]) -> Res<bool> {
    let (bytes, tok) = load(path)?;
    source_matches(&bytes)?;
    let mut per: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let mut ok = true;
    for f in files {
        let mut t = Tally::default();
        for line in std::io::BufReader::new(std::fs::File::open(f)?).lines() {
            let v: serde_json::Value = serde_json::from_str(&line?)?;
            if let Some(text) = v.get("text").and_then(|x| x.as_str()) {
                t.check(&tok, text, false)?;
            }
        }
        ok &= t.clean();
        // The pool directory and the shard name: shard names repeat across pools.
        let path = std::path::Path::new(f);
        let name = [path.parent().and_then(|p| p.file_name()), path.file_name()]
            .iter()
            .map(|x| {
                x.map(|x| x.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join("/");
        per.insert(name, t.json());
    }
    println!("{}", serde_json::json!({"files": per, "clean": ok}));
    Ok(ok)
}
