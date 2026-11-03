//! Optional spaCy adapter for the parser-independent grammar core.
//!
//! Python emits annotations of the exact source, with Unicode character offsets.
//! This adapter verifies source identity, converts offsets to UTF-8 bytes, and
//! validates all spans and dependency trees before returning the core IR.

use anyhow::{Context, Result, ensure};
use grammar_core::syntax::{Document, Sentence, Token, validate};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub const ANNOTATION_VERSION: &str = "grammar-spacy-annotation-v1";
pub const DEFAULT_MODEL: &str = "en_core_web_sm";

/// Explicit runtime choice. The returned identity includes the actual model assets.
pub struct Parser<'a> {
    pub python: &'a str,
    pub model: &'a str,
}

impl Parser<'_> {
    /// Keep the NLP model loaded while consuming one bounded document at a time.
    /// No input, annotation or failed source is persisted by this transport.
    pub fn stream(&self) -> Result<Stream> {
        self.stream_using(None)
    }

    /// Explicit block-aware profile; preserves text and binds a distinct identity.
    pub fn stream_with_paragraph_boundaries(&self) -> Result<Stream> {
        self.stream_using(Some("--paragraph-boundaries"))
    }

    /// Parse paragraph content without edge whitespace; exact source is retained.
    /// This experimental profile has its own identity and cannot reuse v1 spaces.
    pub fn stream_with_paragraph_content(&self) -> Result<Stream> {
        self.stream_using(Some("--paragraph-content"))
    }

    fn stream_using(&self, profile_argument: Option<&str>) -> Result<Stream> {
        let bridge =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("python/syntax_bridge.py");
        let mut command = Command::new(self.python);
        command.arg(bridge).arg("--stream");
        if let Some(argument) = profile_argument {
            command.arg(argument);
        }
        model_argument(&mut command, Some(self.model))?;
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let input = child.stdin.take().context("Stream stdin missing")?;
        let output = BufReader::new(child.stdout.take().context("Stream stdout missing")?);
        Ok(Stream {
            child,
            input: Some(input),
            output,
            next: 0,
            finished: false,
        })
    }

    pub fn parse(&self, text: &str) -> Result<Document> {
        parse_using(text, self.python, Some(self.model))
    }

    pub fn parse_batch(
        &self,
        texts: &[&str],
    ) -> Result<Vec<std::result::Result<Document, String>>> {
        let bridge =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("python/syntax_bridge.py");
        parse_batch_using(texts, self.python, &bridge, Some(self.model))
    }
}

pub struct Stream {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    next: usize,
    finished: bool,
}

impl Stream {
    /// Outer errors are transport failures; inner errors belong to one document.
    pub fn parse(&mut self, text: &str) -> Result<std::result::Result<Document, String>> {
        let input = self
            .input
            .as_mut()
            .context("Syntax stream already closed")?;
        serde_json::to_writer(&mut *input, text)?;
        input.write_all(b"\n")?;
        input.flush()?;
        let mut line = String::new();
        ensure!(
            self.output.read_line(&mut line)? > 0,
            "Syntax stream ended before its response"
        );
        let mut record: WireBatchRecord =
            serde_json::from_str(&line).context("Invalid syntax stream response")?;
        ensure!(record.index == self.next, "Syntax stream index mismatch");
        self.next += 1;
        record.index = 0;
        Ok(from_batch_wire(&[text], vec![record])?.remove(0))
    }

    pub fn finish(mut self) -> Result<()> {
        drop(self.input.take());
        let status = self.child.wait()?;
        self.finished = true;
        ensure!(status.success(), "Syntax stream exited unsuccessfully");
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if !self.finished {
            drop(self.input.take());
            // This is the dedicated child created above, never a shared service.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn model_argument(command: &mut Command, model: Option<&str>) -> Result<()> {
    if let Some(model) = model {
        ensure!(!model.trim().is_empty(), "Empty parser model");
        command.arg(format!("--model={model}"));
    }
    Ok(())
}

#[derive(Deserialize)]
struct WireDocument {
    text: String,
    parser_identity: String,
    tokens: Vec<WireToken>,
    sentences: Vec<WireSentence>,
}

#[derive(Deserialize)]
struct WireToken {
    i: usize,
    start_char: usize,
    end_char: usize,
    text: String,
    lemma: String,
    pos: String,
    tag: String,
    dep: String,
    head: usize,
    sentence: usize,
    morph: BTreeMap<String, Vec<String>>,
    is_punct: bool,
    is_space: bool,
}

#[derive(Deserialize)]
struct WireSentence {
    start_char: usize,
    end_char: usize,
    root: usize,
    token_start: usize,
    token_end: usize,
}

#[derive(Deserialize)]
struct WireBatchRecord {
    index: usize,
    #[serde(flatten)]
    result: WireBatchResult,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum WireBatchResult {
    Ok { annotation: serde_json::Value },
    Error { text: String, error: String },
}

/// Parse the original UTF-8 text without normalization or quote substitutions.
/// The default model is the installed `en_core_web_sm` English dependency parser.
pub fn parse(text: &str, python: &str) -> Result<Document> {
    parse_using(text, python, None)
}

fn parse_using(text: &str, python: &str, model: Option<&str>) -> Result<Document> {
    let bridge = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("python/syntax_bridge.py");
    let mut command = Command::new(python);
    command.arg(bridge);
    model_argument(&mut command, model)?;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Cannot start syntax interpreter {python:?}"))?;
    let input_result = child
        .stdin
        .take()
        .context("Syntax bridge stdin unavailable")?
        .write_all(text.as_bytes());
    let output = child
        .wait_with_output()
        .context("Cannot wait for syntax bridge")?;
    ensure!(
        output.status.success(),
        "Syntax annotation failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    input_result.context("Cannot write text to syntax bridge")?;
    let wire = serde_json::from_slice(&output.stdout)
        .context("Syntax bridge returned invalid annotation JSON")?;
    from_wire(text, wire)
}

/// Parse a caller-bounded batch, loading the installed model once.
///
/// The returned records preserve input order and use exactly the same annotation
/// semantics and identity as [`parse`]. Each document is independently validated;
/// a failed annotation or validation is retained as an error at its input index.
/// Interpreter, transport, response count and ordering failures reject the batch.
/// Empty batches return immediately without starting an interpreter.
pub fn parse_batch(
    texts: &[&str],
    python: &str,
) -> Result<Vec<std::result::Result<Document, String>>> {
    let bridge = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("python/syntax_bridge.py");
    parse_batch_with_bridge(texts, python, &bridge)
}

/// Parse with a caller-supplied copy of the versioned annotation bridge.
/// This lets an executable embed the bridge without depending on its build
/// machine's source directory. Validation and annotation semantics are unchanged.
pub fn parse_batch_with_bridge(
    texts: &[&str],
    python: &str,
    bridge: &std::path::Path,
) -> Result<Vec<std::result::Result<Document, String>>> {
    parse_batch_using(texts, python, bridge, None)
}

fn parse_batch_using(
    texts: &[&str],
    python: &str,
    bridge: &std::path::Path,
    model: Option<&str>,
) -> Result<Vec<std::result::Result<Document, String>>> {
    let mut command = Command::new(python);
    command.arg(bridge).arg("--batch");
    model_argument(&mut command, model)?;
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let input = serde_json::to_vec(texts).context("Cannot encode syntax batch input")?;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Cannot start syntax interpreter {python:?}"))?;
    // The bridge consumes the complete JSON input before loading the model or
    // writing records, so this write cannot block on undrained annotation output.
    let input_result = child
        .stdin
        .take()
        .context("Syntax bridge stdin unavailable")?
        .write_all(&input);
    let output = child
        .wait_with_output()
        .context("Cannot wait for syntax batch bridge")?;
    ensure!(
        output.status.success(),
        "Syntax batch annotation failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    input_result.context("Cannot write texts to syntax batch bridge")?;
    let records = serde_json::from_slice(&output.stdout)
        .context("Syntax bridge returned invalid batch JSON")?;
    from_batch_wire(texts, records)
}

fn from_batch_wire(
    texts: &[&str],
    records: Vec<WireBatchRecord>,
) -> Result<Vec<std::result::Result<Document, String>>> {
    ensure!(
        records.len() == texts.len(),
        "Syntax batch returned {} records for {} inputs",
        records.len(),
        texts.len()
    );
    ensure!(
        records
            .iter()
            .enumerate()
            .all(|(index, record)| record.index == index),
        "Syntax batch returned records out of input order"
    );
    Ok(texts
        .iter()
        .zip(records)
        .map(|(text, record)| {
            let result: Result<Document> = match record.result {
                WireBatchResult::Ok { annotation } => serde_json::from_value(annotation)
                    .context("Syntax batch returned invalid document JSON")
                    .and_then(|wire| from_wire(text, wire)),
                WireBatchResult::Error {
                    text: returned_text,
                    error,
                } => {
                    if returned_text != *text {
                        Err(anyhow::anyhow!(
                            "Syntax bridge changed the exact source text"
                        ))
                    } else {
                        Err(anyhow::anyhow!("Syntax annotation failed: {error}"))
                    }
                }
            };
            result.map_err(|error| format!("{error:#}"))
        })
        .collect())
}

fn from_wire(text: &str, wire: WireDocument) -> Result<Document> {
    ensure!(
        wire.text == text,
        "Syntax bridge changed the exact source text"
    );
    ensure!(
        wire.parser_identity
            .starts_with(&format!("{ANNOTATION_VERSION};"))
            && wire.parser_identity.ends_with(";raw-text-v1"),
        "Syntax bridge returned an unsupported annotation identity"
    );
    // Python string offsets count Unicode scalars, while Rust edits use bytes.
    // The final boundary permits exclusive spans ending at text.len().
    let boundaries: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain([text.len()])
        .collect();
    let byte = |character: usize| -> Result<usize> {
        boundaries.get(character).copied().with_context(|| {
            format!("Syntax character offset {character} exceeds the source length")
        })
    };
    let tokens = wire
        .tokens
        .into_iter()
        .map(|token| {
            Ok(Token {
                i: token.i,
                start_byte: byte(token.start_char)?,
                end_byte: byte(token.end_char)?,
                text: token.text,
                lemma: token.lemma,
                pos: token.pos,
                tag: token.tag,
                dep: token.dep,
                head: token.head,
                sentence: token.sentence,
                morph: token.morph,
                is_punct: token.is_punct,
                is_space: token.is_space,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let sentences = wire
        .sentences
        .into_iter()
        .map(|sentence| {
            Ok(Sentence {
                start_byte: byte(sentence.start_char)?,
                end_byte: byte(sentence.end_char)?,
                root: sentence.root,
                token_start: sentence.token_start,
                token_end: sentence.token_end,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let doc = Document {
        text: wire.text,
        parser_identity: wire.parser_identity,
        tokens,
        sentences,
    };
    validate(&doc)?;
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed_python() -> Option<std::path::PathBuf> {
        let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        std::env::var_os("GRAMMAR_SPACY_PYTHON")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                ["../../.venv/bin/python", "../../../.venv/bin/python"]
                    .iter()
                    .map(|path| crate_dir.join(path))
                    .find(|path| path.exists())
            })
    }

    fn empty_batch_record(index: usize) -> WireBatchRecord {
        WireBatchRecord {
            index,
            result: WireBatchResult::Ok {
                annotation: serde_json::json!({
                    "text": "",
                    "parser_identity": format!("{ANNOTATION_VERSION};synthetic-fixture;raw-text-v1"),
                    "tokens": [],
                    "sentences": [],
                }),
            },
        }
    }

    fn wire(text: &str, items: &[(&str, &str, &str, usize)]) -> WireDocument {
        let mut cursor = 0;
        let mut tokens = Vec::new();
        for (i, &(word, pos, dep, head)) in items.iter().enumerate() {
            let start_byte = text[cursor..].find(word).unwrap() + cursor;
            let end_byte = start_byte + word.len();
            tokens.push(WireToken {
                i,
                start_char: text[..start_byte].chars().count(),
                end_char: text[..end_byte].chars().count(),
                text: word.into(),
                lemma: word.to_lowercase(),
                pos: pos.into(),
                tag: pos.into(),
                dep: dep.into(),
                head,
                sentence: 0,
                morph: BTreeMap::new(),
                is_punct: pos == "PUNCT",
                is_space: pos == "SPACE",
            });
            cursor = end_byte;
        }
        let sentences = if tokens.is_empty() {
            Vec::new()
        } else {
            vec![WireSentence {
                start_char: tokens[0].start_char,
                end_char: tokens.last().unwrap().end_char,
                root: items
                    .iter()
                    .enumerate()
                    .find(|(i, item)| *i == item.3)
                    .unwrap()
                    .0,
                token_start: 0,
                token_end: tokens.len(),
            }]
        };
        WireDocument {
            text: text.into(),
            parser_identity: format!("{ANNOTATION_VERSION};synthetic-fixture;raw-text-v1"),
            tokens,
            sentences,
        }
    }

    #[test]
    fn character_offsets_convert_to_exact_utf8_spans_without_normalization() {
        let text = "cafe\u{301} isn’t 🦊 ordinary.";
        let items = [
            ("cafe\u{301}", "NOUN", "nsubj", 1),
            ("is", "AUX", "ROOT", 1),
            ("n’t", "PART", "neg", 1),
            ("🦊", "PUNCT", "punct", 1),
            ("ordinary", "ADJ", "acomp", 1),
            (".", "PUNCT", "punct", 1),
        ];
        let doc = from_wire(text, wire(text, &items)).unwrap();
        assert_eq!(doc.text.as_bytes(), text.as_bytes());
        assert_eq!(doc.tokens[0].end_byte, 6);
        assert_eq!(doc.tokens[2].text, "n’t");
        assert_eq!(doc.tokens[3].end_byte - doc.tokens[3].start_byte, 4);
        for token in &doc.tokens {
            assert_eq!(&text[token.start_byte..token.end_byte], token.text);
        }
        let mut corrupt = wire(text, &items);
        corrupt.tokens[2].end_char = text.chars().count() + 1;
        assert!(from_wire(text, corrupt).is_err());
        let mut normalized = wire(text, &items);
        normalized.text = "café isn’t 🦊 ordinary.".into();
        assert!(
            from_wire(text, normalized)
                .unwrap_err()
                .to_string()
                .contains("exact source")
        );
    }

    #[test]
    fn incompatible_annotation_identity_and_broken_trees_are_rejected() {
        let text = "Birds fly.";
        let items = [
            ("Birds", "NOUN", "nsubj", 1),
            ("fly", "VERB", "ROOT", 1),
            (".", "PUNCT", "punct", 1),
        ];
        let mut wrong_identity = wire(text, &items);
        wrong_identity.parser_identity = "normalized-parser-v1".into();
        assert!(
            from_wire(text, wrong_identity)
                .unwrap_err()
                .to_string()
                .contains("unsupported annotation identity")
        );
        let mut wrong_head = wire(text, &items);
        wrong_head.tokens[0].head = 3;
        assert!(
            from_wire(text, wrong_head)
                .unwrap_err()
                .to_string()
                .contains("out of bounds")
        );
        let mut wrong_root = wire(text, &items);
        wrong_root.sentences[0].root = 0;
        assert!(from_wire(text, wrong_root).is_err());
    }

    #[test]
    fn missing_interpreter_is_an_explicit_error() {
        assert!(
            parse("Some text.", "/grammar-spacy-nonexistent-python")
                .unwrap_err()
                .to_string()
                .contains("Cannot start syntax interpreter")
        );
        assert!(
            parse_batch(&["Some text."], "/grammar-spacy-nonexistent-python")
                .unwrap_err()
                .to_string()
                .contains("Cannot start syntax interpreter")
        );
        assert!(
            parse_batch(&[], "/grammar-spacy-nonexistent-python")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn batch_requires_one_record_per_input_in_exact_order() {
        assert!(
            from_batch_wire(&["", ""], vec![empty_batch_record(0)])
                .unwrap_err()
                .to_string()
                .contains("1 records for 2 inputs")
        );
        assert!(
            from_batch_wire(
                &["", ""],
                vec![empty_batch_record(1), empty_batch_record(0)]
            )
            .unwrap_err()
            .to_string()
            .contains("out of input order")
        );
        assert!(
            from_batch_wire(
                &["", ""],
                vec![empty_batch_record(0), empty_batch_record(0)]
            )
            .is_err()
        );
    }

    #[test]
    fn batch_retains_each_annotation_and_validation_failure() {
        let mut wrong_identity = empty_batch_record(1);
        let WireBatchResult::Ok { annotation } = &mut wrong_identity.result else {
            panic!()
        };
        annotation["parser_identity"] = serde_json::json!("incompatible");
        let records = vec![
            empty_batch_record(0),
            wrong_identity,
            WireBatchRecord {
                index: 2,
                result: WireBatchResult::Ok {
                    annotation: serde_json::Value::Null,
                },
            },
            WireBatchRecord {
                index: 3,
                result: WireBatchResult::Error {
                    text: "".into(),
                    error: "synthetic parser failure".into(),
                },
            },
            WireBatchRecord {
                index: 4,
                result: WireBatchResult::Error {
                    text: "changed source".into(),
                    error: "synthetic parser failure".into(),
                },
            },
            empty_batch_record(5),
        ];
        let actual = from_batch_wire(&["different source", "", "", "", "", ""], records).unwrap();
        assert!(
            actual[0]
                .as_ref()
                .unwrap_err()
                .contains("exact source text")
        );
        assert!(
            actual[1]
                .as_ref()
                .unwrap_err()
                .contains("unsupported annotation identity")
        );
        assert!(
            actual[2]
                .as_ref()
                .unwrap_err()
                .contains("invalid document JSON")
        );
        assert!(
            actual[3]
                .as_ref()
                .unwrap_err()
                .contains("synthetic parser failure")
        );
        assert!(
            actual[4]
                .as_ref()
                .unwrap_err()
                .contains("exact source text")
        );
        assert_eq!(actual[5].as_ref().unwrap().text, "");
    }

    #[test]
    fn installed_batch_matches_individual_parses_and_continues_after_parser_error() {
        let Some(python) = installed_python() else {
            eprintln!(
                "spaCy integration skipped: set GRAMMAR_SPACY_PYTHON or install a local .venv"
            );
            return;
        };
        let python = python.to_str().unwrap();
        let texts = [
            "\n\ncafe\u{301} isn’t empty. 🦊 We saw birds that flew home.\n\n",
            "We can't stay; they don't know.\tCan you help?",
            "",
            "   ",
        ];
        let actual = parse_batch(&texts, python).unwrap();
        for (text, actual) in texts.iter().zip(actual) {
            assert_eq!(actual.unwrap(), parse(text, python).unwrap());
        }
        // spaCy rejects inputs above its configured maximum before tokenization.
        // An error in one input must not discard the valid document after it.
        let too_long = "x".repeat(1_000_001);
        let actual = parse_batch(&[&too_long, texts[1]], python).unwrap();
        assert!(actual[0].as_ref().unwrap_err().contains("E088"));
        assert_eq!(actual[1].as_ref().unwrap().text, texts[1]);
    }

    #[test]
    fn installed_bridge_rejects_malformed_batch_input() {
        let Some(python) = installed_python() else {
            eprintln!(
                "spaCy integration skipped: set GRAMMAR_SPACY_PYTHON or install a local .venv"
            );
            return;
        };
        let bridge =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("python/syntax_bridge.py");
        for input in ["{}", "[\"text\", 42]"] {
            let mut child = Command::new(&python)
                .arg(&bridge)
                .arg("--batch")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("Batch input must be a JSON array of strings")
            );
        }
    }

    #[test]
    fn model_fingerprint_ignores_install_path_and_detects_asset_changes() {
        let Some(python) = installed_python() else {
            eprintln!("parser fingerprint test skipped: no configured Python interpreter");
            return;
        };
        let bridge =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("python/syntax_bridge.py");
        let script = r#"
import importlib.util
from pathlib import Path
import sys
import tempfile
spec = importlib.util.spec_from_file_location("syntax_bridge", sys.argv[1])
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)
with tempfile.TemporaryDirectory() as directory:
    roots = [Path(directory) / "first", Path(directory) / "second"]
    for root in roots:
        root.mkdir()
        (root / "config.cfg").write_bytes(b"synthetic parser config")
        (root / "weights").write_bytes(b"synthetic parser weights")
    original = bridge._model_files_sha256(roots[0])
    assert original == bridge._model_files_sha256(roots[1])
    (roots[0] / "__pycache__").mkdir()
    (roots[0] / "__pycache__" / "generated.pyc").write_bytes(b"generated cache")
    assert original == bridge._model_files_sha256(roots[0])
    (roots[1] / "weights").write_bytes(b"modified synthetic weights")
    assert original != bridge._model_files_sha256(roots[1])
"#;
        let output = Command::new(python)
            .arg("-c")
            .arg(script)
            .arg(bridge)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn installed_adjunct_fronting_checks_attachment_and_preserves_named_openers() {
        let Some(python) = installed_python() else {
            eprintln!("spaCy integration skipped: set GRAMMAR_SPACY_PYTHON");
            return;
        };
        let cases = [
            (
                "The café, although Mira dislikes it, remains open.",
                Some("Although Mira dislikes it, the café remains open."),
            ),
            (
                "The workshop, as Mira describes it, serves local artists.",
                Some("As Mira describes it, the workshop serves local artists."),
            ),
            (
                "The Café Atlas, although Mira dislikes it, remains open.",
                Some("Although Mira dislikes it, The Café Atlas remains open."),
            ),
            (
                "Mira says the café, although Lee dislikes it, remains open.",
                None,
            ),
            ("The files that Mira checks remain available.", None),
            (
                "\"The café, although Mira dislikes it, remains open.\"",
                None,
            ),
            ("The café,\nalthough Mira dislikes it, remains open.", None),
        ];
        let texts: Vec<_> = cases.iter().map(|(s, _)| *s).collect();
        let parsed = parse_batch(&texts, python.to_str().unwrap()).unwrap();
        for ((source, expected), doc) in cases.iter().zip(parsed) {
            let doc = doc.unwrap();
            let candidates = grammar_core::rules::generate_selected(
                &doc,
                16,
                &["front_parenthetical_adjunct".into()],
            )
            .unwrap();
            if let Some(expected) = expected {
                assert_eq!(candidates.len(), 1, "{source}");
                assert_eq!(&candidates[0].text, expected);
                let reparsed = parse(expected, python.to_str().unwrap()).unwrap();
                assert_eq!(reparsed.sentences.len(), 1);
                grammar_core::features::extract(&reparsed).unwrap();
            } else {
                assert!(candidates.is_empty(), "{source}");
            }
        }
    }

    #[test]
    fn installed_paragraph_profile_separates_headings_without_changing_text() {
        let Some(python) = installed_python() else {
            return;
        };
        let parser = Parser {
            python: python.to_str().unwrap(),
            model: DEFAULT_MODEL,
        };
        let text = "Opening\n\nThe café serves visitors.\n\nMira waits outside.";
        let raw = parser.parse(text).unwrap();
        let mut stream = parser.stream_with_paragraph_boundaries().unwrap();
        let doc = stream.parse(text).unwrap().unwrap();
        stream.finish().unwrap();
        assert_eq!(doc.text, text);
        assert!(doc.parser_identity.contains(";paragraph-documents-v1;"));
        assert_ne!(doc.parser_identity, raw.parser_identity);
        assert_eq!(
            doc.tokens
                .iter()
                .map(|t| (&t.text, t.start_byte, t.end_byte))
                .collect::<Vec<_>>(),
            raw.tokens
                .iter()
                .map(|t| (&t.text, t.start_byte, t.end_byte))
                .collect::<Vec<_>>()
        );
        assert_eq!(doc.sentences.len(), 3);
        for s in doc.sentences {
            assert!(!text[s.start_byte..s.end_byte].trim().contains("\n\n"));
        }
    }

    #[test]
    fn installed_paragraph_content_preserves_offsets_without_layout_sentences() {
        let Some(python) = installed_python() else {
            eprintln!("spaCy integration skipped: set GRAMMAR_SPACY_PYTHON");
            return;
        };
        let parser = Parser {
            python: python.to_str().unwrap(),
            model: DEFAULT_MODEL,
        };
        let mut stream = parser.stream_with_paragraph_content().unwrap();
        let text = " \t\r\n\r\nCafé notes\r\n \t\r\nMira checked the brakes.\n\n \t";
        let doc = stream.parse(text).unwrap().unwrap();
        assert_eq!(doc.text, text);
        assert!(doc.parser_identity.contains(";paragraph-content-v2;"));
        validate(&doc).unwrap();
        assert_eq!(doc.sentences.len(), 2);
        assert!(doc.tokens.iter().all(|t| !t.is_space));
        for word in ["Café", "Mira", "brakes"] {
            let token = doc.tokens.iter().find(|t| t.text == word).unwrap();
            assert_eq!(token.start_byte, text.find(word).unwrap());
            assert_eq!(&text[token.start_byte..token.end_byte], word);
        }
        for text in ["", " \t\r\n \r\n\n"] {
            let empty = stream.parse(text).unwrap().unwrap();
            assert_eq!(empty.text, text);
            assert!(empty.tokens.is_empty() && empty.sentences.is_empty());
            validate(&empty).unwrap();
        }
        stream.finish().unwrap();
    }

    #[test]
    fn installed_stream_matches_batch_and_recovers_from_document_failure() {
        let Some(python) = installed_python() else {
            return;
        };
        let parser = Parser {
            python: python.to_str().unwrap(),
            model: DEFAULT_MODEL,
        };
        let texts = ["\n\nThe café isn’t closed.\n", "Mira waits outside."];
        let expected = parser.parse_batch(&texts).unwrap();
        let mut stream = parser.stream().unwrap();
        assert_eq!(stream.parse(texts[0]).unwrap(), expected[0]);
        // spaCy's document length guard must remain a per-document error.
        assert!(stream.parse(&"x".repeat(1_000_001)).unwrap().is_err());
        assert_eq!(stream.parse(texts[1]).unwrap(), expected[1]);
        stream.finish().unwrap();
    }

    #[test]
    fn explicit_model_preserves_default_annotations_and_rejects_missing_models() {
        let Some(python) = installed_python() else {
            return;
        };
        let python = python.to_str().unwrap();
        let text = "The café serves visitors who arrive early.";
        let expected = parse(text, python).unwrap();
        let parser = Parser {
            python,
            model: DEFAULT_MODEL,
        };
        assert_eq!(parser.parse(text).unwrap(), expected);
        assert_eq!(
            parser.parse_batch(&[text]).unwrap()[0].as_ref().unwrap(),
            &expected
        );
        let missing = Parser {
            python,
            model: "slop_ninja_nonexistent_parser",
        };
        assert!(
            missing
                .parse_batch(&[text])
                .unwrap_err()
                .to_string()
                .contains("Cannot load spaCy model")
        );
        assert!(Parser { python, model: " " }.parse_batch(&[]).is_err());
    }

    #[test]
    fn installed_complete_joins_keep_words_and_reject_ambiguous_boundaries() {
        let Some(python) = installed_python() else {
            return;
        };
        let cases = [
            (
                "The café serves visitors who arrive early. The bakery opens at dawn.",
                Some("The café serves visitors who arrive early; the bakery opens at dawn."),
            ),
            (
                "The café serves visitors who arrive early. I wait outside.",
                Some("The café serves visitors who arrive early; I wait outside."),
            ),
            (
                "The café serves visitors who arrive early. Mira waits outside.",
                Some("The café serves visitors who arrive early; Mira waits outside."),
            ),
            (
                "The café serves visitors who arrive early. Its owner greets them.",
                Some("The café serves visitors who arrive early; its owner greets them."),
            ),
            (
                "The café serves visitors who arrive early. And the bakery opens at dawn.",
                Some("The café serves visitors who arrive early; and the bakery opens at dawn."),
            ),
            (
                "The café does not close. The bakery may open.",
                Some("The café does not close; the bakery may open."),
            ),
            ("The café opens.\n\nThe bakery closes.", None),
            ("If the café opens, Mira visits. The bakery closes.", None),
            ("The café opens. Why does the bakery close?", None),
            ("The café opens. The Hague attracts visitors.", None),
            ("\"The café opens. The bakery closes.\"", None),
        ];
        let texts: Vec<_> = cases.iter().map(|(t, _)| *t).collect();
        let docs = parse_batch(&texts, python.to_str().unwrap()).unwrap();
        let expected_texts: Vec<_> = cases.iter().filter_map(|(_, e)| *e).collect();
        let reparses = parse_batch(&expected_texts, python.to_str().unwrap()).unwrap();
        for doc in reparses {
            let doc = doc.unwrap();
            assert_eq!(doc.sentences.len(), 1);
            grammar_core::features::extract(&doc).unwrap();
        }
        for ((source, expected), doc) in cases.iter().zip(docs) {
            let candidates = grammar_core::rules::generate_selected(
                &doc.unwrap(),
                16,
                &["join_complete_sentences".into()],
            )
            .unwrap();
            if let Some(expected) = expected {
                assert_eq!(candidates.len(), 1, "{source}");
                assert_eq!(&candidates[0].text, expected);
            } else {
                assert!(candidates.is_empty(), "{source}");
            }
        }
    }

    #[test]
    fn installed_complete_boundary_rules_preserve_embedded_material_and_connectors() {
        let Some(python) = installed_python() else {
            return;
        };
        let cases = [
            (
                "The café serves visitors who arrive early, and the bakery opens at dawn.",
                Some("The café serves visitors who arrive early. And the bakery opens at dawn."),
            ),
            (
                "The café serves visitors who arrive early; the bakery opens at dawn.",
                Some("The café serves visitors who arrive early. The bakery opens at dawn."),
            ),
            // The small pipeline tags 'stays' as a noun here. Keep that failed
            // parse as a rejected nomination; the source sentence is grammatical.
            (
                "The café that Mira owns stays open; the bakery serves customers.",
                None,
            ),
            ("Mira says that the café opens and the bakery closes.", None),
            ("If the café opens, Mira visits, and Lee waits.", None),
            ("Mira buys bread and leaves.", None),
            (
                "\"The café stays open; the bakery serves customers.\"",
                None,
            ),
        ];
        let texts: Vec<_> = cases.iter().map(|(text, _)| *text).collect();
        let docs = parse_batch(&texts, python.to_str().unwrap()).unwrap();
        for ((source, expected), doc) in cases.iter().zip(docs) {
            let candidates = grammar_core::rules::generate_selected(
                &doc.unwrap(),
                16,
                &[
                    "split_complete_semicolon_clauses".into(),
                    "split_complete_coordinate_clauses".into(),
                ],
            )
            .unwrap();
            if let Some(expected) = expected {
                assert!(candidates.iter().any(|c| c.text == *expected), "{source}");
                let reparsed = parse(expected, python.to_str().unwrap()).unwrap();
                assert_eq!(reparsed.sentences.len(), 2);
                grammar_core::features::extract(&reparsed).unwrap();
            } else {
                assert!(candidates.is_empty(), "{source}");
            }
        }
    }

    #[test]
    fn installed_parser_preserves_raw_text_and_produces_valid_typed_features() {
        let Some(python) = installed_python() else {
            eprintln!(
                "spaCy integration skipped: set GRAMMAR_SPACY_PYTHON or install a local .venv"
            );
            return;
        };
        let text = "\n\ncafe\u{301} isn’t empty. 🦊 We saw birds that flew home.\n\n";
        let doc = parse(text, python.to_str().unwrap()).unwrap();
        assert_eq!(doc.text, text);
        assert!(doc.parser_identity.starts_with(ANNOTATION_VERSION));
        assert!(doc.parser_identity.contains(";spacy="));
        assert!(doc.parser_identity.contains(";model-files-sha256="));
        assert!(!doc.parser_identity.contains("grammar-rules-v1"));
        assert!(doc.tokens.iter().any(|token| token.text == "cafe\u{301}"));
        assert!(doc.tokens.iter().any(|token| token.text == "n’t"));
        assert!(
            doc.tokens
                .iter()
                .any(|token| token.text == "🦊" && token.end_byte - token.start_byte == 4)
        );
        let result = grammar_core::features::extract(&doc).unwrap();
        let grammar_core::features::Family::Distribution { opportunities, .. } =
            &result.families["clause_head_frame"]
        else {
            panic!()
        };
        assert!(*opportunities >= 3);
        let grammar_core::features::Family::Metrics { opportunities, .. } =
            &result.families["syntax_sentence_load"]
        else {
            panic!()
        };
        assert_eq!(*opportunities, 2);
        for text in ["", "   "] {
            let doc = parse(text, python.to_str().unwrap()).unwrap();
            assert_eq!(doc.text, text);
            let features = grammar_core::features::extract(&doc).unwrap();
            let grammar_core::features::Family::Distribution { opportunities, .. } =
                &features.families["word"]
            else {
                panic!()
            };
            assert_eq!(*opportunities, 0);
        }
    }
}
