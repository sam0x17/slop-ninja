//! spans-v1: check the runtime sentence splitter copy against the sealed
//! splitter on every text of a trainer bundle (inline `text` or `text_ref`),
//! skipping Dev-select and Test rows before their text is read. Prints counts
//! only; no text is written.
//! usage: splitter_equivalence BUNDLE_DIR OUT.json
use anyhow::{Context, Result, ensure};
use grammar_a_baseline::full_a_spans;
use grammar_a_positives::text;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 2, "BUNDLE_DIR OUT.json");
    let root = Path::new(&a[0]);
    ensure!(!Path::new(&a[1]).exists(), "new output required");
    let mut files: BTreeMap<PathBuf, File> = BTreeMap::new();
    let mut counts: BTreeMap<String, [u64; 4]> = BTreeMap::new(); // rows, equal, sentences, words-outside
    for line in BufReader::new(File::open(root.join("rows.jsonl"))?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line)?;
        let split = v["split"].as_str().context("split")?.to_owned();
        if matches!(split.as_str(), "dev_select" | "development_select" | "dev-select" | "test") {
            continue;
        }
        let t = if let Some(t) = v["text"].as_str() {
            t.to_owned()
        } else {
            let r = &v["text_ref"];
            let p = PathBuf::from(r["path"].as_str().context("text_ref path")?);
            let p = if p.is_absolute() { p } else { root.join(p) };
            let f = match files.get_mut(&p) {
                Some(f) => f,
                None => files.entry(p.clone()).or_insert(File::open(&p)?),
            };
            f.seek(SeekFrom::Start(r["offset"].as_u64().context("offset")?))?;
            let mut b = vec![0; r["length"].as_u64().context("length")? as usize];
            f.read_exact(&mut b)?;
            String::from_utf8(b)?
        };
        let x = text::sentences(&t)
            .iter()
            .map(|s| (s.start, s.end, s.punctuated))
            .collect::<Vec<_>>();
        let y = full_a_spans::sentences(&t)
            .iter()
            .map(|s| (s.start, s.end, s.punctuated))
            .collect::<Vec<_>>();
        let words = full_a_spans::words(&t);
        let ok = full_a_spans::sentence_word_ranges(&full_a_spans::sentences(&t), &words).is_ok();
        let c = counts.entry(split).or_default();
        c[0] += 1;
        c[1] += u64::from(x == y);
        c[2] += x.len() as u64;
        c[3] += u64::from(!ok);
    }
    let report = json!({"schema":"slop_ninja_a_spans_splitter_equivalence_v1","splitter":full_a_spans::SPLITTER_ID,
        "by_split":counts.iter().map(|(k, c)| (k.clone(), json!({"rows":c[0],"equal":c[1],"sentences":c[2],
            "rows_with_a_word_outside_every_sentence":c[3]}))).collect::<serde_json::Map<_, _>>(),
        "all_equal":counts.values().all(|c| c[0] == c[1]),"all_words_in_sentences":counts.values().all(|c| c[3] == 0)});
    std::fs::write(&a[1], serde_json::to_vec_pretty(&report)?)?;
    println!("{}", json!({"all_equal":report["all_equal"],"all_words_in_sentences":report["all_words_in_sentences"]}));
    ensure!(report["all_equal"] == true, "splitter copies differ");
    Ok(())
}
