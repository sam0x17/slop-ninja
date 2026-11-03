//! Read a JSON array of source strings; emit explicit views without running NLP.
use anyhow::Result;
use std::io::{self, Read};

fn main() -> Result<()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let sources: Vec<String> = serde_json::from_str(&input)?;
    let views = sources
        .iter()
        .map(|source| grammar_markdown::render(source))
        .collect::<Result<Vec<_>>>()?;
    serde_json::to_writer_pretty(io::stdout().lock(), &views)?;
    Ok(())
}
