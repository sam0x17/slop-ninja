//! Stage CMU Book Summaries without executing an upstream loader or admitting rows.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::json;
use slop_ninja_detector::{acquire::Collector, dataset::sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    output_dir: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output_dir.exists(), "Use a new staging directory");
    let mut collector = Collector::new(&args.output_dir)?;
    let mut captures = Vec::new();
    for url in [
        "https://www.cs.cmu.edu/~dbamman/booksummaries.html",
        "https://creativecommons.org/licenses/by-sa/3.0/us/legalcode",
        "https://www.cs.cmu.edu/~dbamman/data/booksummaries.tar.gz",
    ] {
        captures.push(collector.get(&url.parse()?)?);
    }
    let archive = args.output_dir.join(
        captures[2].metadata["raw_path"]
            .as_str()
            .context("Archive path")?,
    );
    let listing = Command::new("tar").arg("-tzf").arg(&archive).output()?;
    ensure!(listing.status.success(), "Cannot inspect CMU archive");
    let members = String::from_utf8(listing.stdout)?;
    let member = members
        .lines()
        .find(|p| *p == "booksummaries/booksummaries.txt")
        .context("Expected summary member missing")?;
    // Extract one named member to stdout; no archive paths are written to disk.
    let mut child = Command::new("tar")
        .arg("-xOzf")
        .arg(&archive)
        .arg(member)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .context("Missing tar stdout")?
        .take(150_000_001)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 150_000_000 {
        let _ = child.kill();
    }
    let status = child.wait()?;
    ensure!(
        bytes.len() <= 150_000_000 && status.success(),
        "Summary extraction failed or exceeded bound"
    );
    let text = std::str::from_utf8(&bytes)?;
    let mut ids = BTreeSet::new();
    let mut bounded = 0;
    let mut empty_summaries = 0;
    let mut rows = 0;
    for line in text.lines() {
        let fields: Vec<_> = line.splitn(7, '\t').collect();
        ensure!(
            fields.len() == 7 && fields[0].parse::<u64>().is_ok(),
            "Unexpected CMU row shape"
        );
        ids.insert(fields[0]);
        rows += 1;
        let words = grammar_core::features::words(fields[6]).len();
        bounded += usize::from((80..=500).contains(&words));
        empty_summaries += usize::from(words == 0);
    }
    fs::write(args.output_dir.join("booksummaries.txt"), &bytes)?;
    fs::write(
        args.output_dir.join("archive-members.txt"),
        members.as_bytes(),
    )?;
    let report = json!({"schema":"slop_ninja_cmu_books_staging_v1",
        "captures":captures.iter().map(|c| &c.metadata).collect::<Vec<_>>(),
        "rows":rows,"unique_wikipedia_page_ids":ids.len(),"rows_with_80_to_500_lexical_words":bounded,
        "empty_summaries":empty_summaries,"text_sha256":sha256(&bytes),"text_bytes":bytes.len(),
        "collection_license":"CC-BY-SA-3.0-US",
        "attribution":"David Bamman and Noah Smith (2013), New Alignment Methods for Discriminative Book Summarization; Wikipedia contributors; aligned Freebase metadata.",
        "origin_note":"Book publication date is not summary composition date. Book author is not summary author.",
        "admission":"Staging only; historical summary matching, source notices and the ported US license require a source-specific admission extension.",
        "training_records":0,"pangram_calls":0});
    fs::write(
        args.output_dir.join("staging.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
