//! Reproduce the preface extraction with the source book's reference labels.
//! Usage: cargo run --example render_preface -- INPUT.tex OUTPUT.txt
use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};
use unslop::util::digest;

fn reference(label: &str) -> Result<&'static str> {
    // Verified against preview/labels.tex; the book calls chapters sections too.
    match label {
        "sec:1.7" => Ok("section 1.7"),
        "sec:architectural-convergence" => Ok("section 2.7.4"),
        "sec:ise" => Ok("section 4.4"),
        "sec:1.4" => Ok("section 1.4"),
        "sec:sealed-implementation" => Ok("section 1.5.4"),
        "app:contributions" => Ok("appendix A"),
        "sec:2.11" => Ok("section 2.11"),
        _ => bail!("unknown reference label: {label:?}"),
    }
}

fn prepare(source: &str) -> Result<String> {
    // Match Python Path.read_text's universal newline handling without changing
    // the original source bytes used for the provenance hash.
    let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
    let tex = if normalized.starts_with("\\chapter*") {
        // Python splitlines recognizes these separators and omits a trailing
        // empty line. Preserve that behavior before removing the three headers.
        let line_end = Regex::new(r"[\n\u{b}\u{c}\u{1c}-\u{1e}\u{85}\u{2028}\u{2029}]")?;
        let mut lines: Vec<_> = line_end.split(&normalized).collect();
        if lines.last() == Some(&"") {
            lines.pop();
        }
        lines.into_iter().skip(3).collect::<Vec<_>>().join("\n")
    } else {
        normalized
    };
    let refs = Regex::new(r"\\(Cref|cref)\{([^}]*)\}")?;
    let mut rendered = String::with_capacity(tex.len());
    let mut cursor = 0;
    for capture in refs.captures_iter(&tex) {
        let whole = capture.get(0).context("reference match missing")?;
        rendered.push_str(&tex[cursor..whole.start()]);
        let labels = capture[2]
            .split(',')
            .map(reference)
            .collect::<Result<Vec<_>>>()?;
        let mut label = labels.join(" and ");
        if &capture[1] == "Cref" {
            label[..1].make_ascii_uppercase();
        }
        rendered.push_str(&label);
        cursor = whole.end();
    }
    rendered.push_str(&tex[cursor..]);
    let unresolved = Regex::new(r"\\(?:[Cc]ref|(?:page|eq|auto|v)?ref)\b")?;
    ensure!(
        !unresolved.is_match(&rendered),
        "unresolved or unsupported reference command"
    );
    Ok(rendered)
}

fn render(prepared: &str) -> Result<(String, String)> {
    let version = Command::new("pandoc")
        .arg("--version")
        .output()
        .context("run pandoc --version")?;
    ensure!(version.status.success(), "pandoc --version failed");
    ensure!(version.stderr.is_empty(), "pandoc --version wrote stderr");
    let version = String::from_utf8(version.stdout).context("Pandoc version is not UTF-8")?;
    let version = version.lines().next().context("empty Pandoc version")?;

    let mut child = Command::new("pandoc")
        .args(["-f", "latex", "-t", "plain", "--wrap=none"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run pandoc")?;
    child
        .stdin
        .take()
        .context("missing Pandoc stdin")?
        .write_all(prepared.as_bytes())
        .context("write LaTeX to Pandoc")?;
    let result = child.wait_with_output().context("wait for Pandoc")?;
    ensure!(
        result.status.success(),
        "Pandoc failed ({}): {}",
        result.status,
        String::from_utf8_lossy(&result.stderr)
    );
    ensure!(
        result.stderr.is_empty(),
        "Pandoc wrote stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = String::from_utf8(result.stdout).context("Pandoc output is not UTF-8")?;
    ensure!(!text.trim().is_empty(), "Pandoc produced empty prose");
    Ok((text, version.to_owned()))
}

fn save_exact(path: &Path, text: &str) -> Result<bool> {
    match fs::read(path) {
        Ok(existing) => {
            ensure!(
                existing == text.as_bytes(),
                "{} already contains different text; choose a new output path",
                path.display()
            );
            return Ok(false);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("read existing output"),
    }
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(text.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path)
        .with_context(|| format!("save {} without overwriting", path.display()))?;
    Ok(true)
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 2,
        "usage: render_preface INPUT.tex OUTPUT.txt"
    );
    let input = Path::new(&args[0]);
    let output = Path::new(&args[1]);
    if output.exists() {
        ensure!(
            input.canonicalize()? != output.canonicalize()?,
            "input and output must be different files"
        );
    }
    let source =
        fs::read_to_string(input).with_context(|| format!("read {} as UTF-8", input.display()))?;
    let prepared = prepare(&source)?;
    let (text, version) = render(&prepared)?;
    let created = save_exact(output, &text)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "source_path": input,
            "output_path": output,
            "source_sha256": digest(&source),
            "output_sha256": digest(&text),
            "pandoc_version": version,
            "reference_label_policy": "book-section-labels-v1",
            "stripped_three_header_lines": source.starts_with("\\chapter*"),
            "created": created
        }))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_case_multiple_labels_and_book_section_names() {
        assert_eq!(
            prepare(r"See \Cref{sec:1.7,sec:ise}; also \cref{app:contributions,sec:sealed-implementation}.").unwrap(),
            "See Section 1.7 and section 4.4; also appendix A and section 1.5.4."
        );
        assert_eq!(
            prepare(r"\Cref{sec:architectural-convergence,sec:1.4,sec:2.11}").unwrap(),
            "Section 2.7.4 and section 1.4 and section 2.11"
        );
    }

    #[test]
    fn rejects_unknown_empty_and_unresolved_references() {
        for input in [
            r"\Cref{sec:unknown}",
            r"\cref{sec:1.7, sec:ise}",
            r"\Cref{}",
            r"\cref{sec:1.7",
            r"\ref{sec:1.7}",
        ] {
            assert!(prepare(input).is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn matches_legacy_header_and_newline_handling() {
        assert_eq!(
            prepare("\\chapter*{Preface}\r\nheader two\r\nheader three\r\n\r\nBody.\r\n").unwrap(),
            "\nBody."
        );
        assert_eq!(prepare("Body.\r\nNext.\r").unwrap(), "Body.\nNext.\n");
    }

    #[test]
    fn preserves_existing_output_when_a_revision_differs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("prose.txt");
        assert!(save_exact(&path, "Exact prose.\n").unwrap());
        assert!(!save_exact(&path, "Exact prose.\n").unwrap());
        assert!(save_exact(&path, "Different prose.\n").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"Exact prose.\n");
    }
}
