//! Separate reference-only scope; query transformations retain their gates.
use super::*;
use grammar_a_baseline::{shared::SharedInput, warm_shared::NumericSharedModel};
use std::{ffi::OsString, path::PathBuf};

mod admission;
mod encode;
#[allow(dead_code)]
#[path = "../final_test_binding/mod.rs"]
mod final_test_binding;
mod project;
#[cfg(test)]
mod tests;
#[allow(dead_code, unused_imports)]
#[path = "../prepare_frozen_text_evaluation.rs"]
mod text_projection;

type Rows = BTreeMap<String, Value>;
type Files = BTreeMap<String, Vec<u8>>;
fn read(path: &Path) -> Result<(Vec<u8>, Value)> {
    let bytes = fs::read(path)?;
    let value = serde_json::from_slice(&bytes)?;
    Ok((bytes, value))
}
fn array<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    v[key].as_array().with_context(|| format!("array {key}"))
}
fn records(bytes: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(bytes)?
        .lines()
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}
fn rows(bytes: &[u8]) -> Result<Rows> {
    let mut out = Rows::new();
    for r in records(bytes)? {
        ensure!(
            out.insert(str_at(&r, "id")?.to_owned(), r).is_none(),
            "duplicate reference ID"
        );
    }
    Ok(out)
}
fn jsonl(values: impl IntoIterator<Item = Value>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for v in values {
        serde_json::to_writer(&mut out, &v)?;
        out.push(b'\n');
    }
    Ok(out)
}
fn pretty(v: &Value) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(v)?)
}
fn ids(v: &Value, key: &str) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for id in array(v, key)? {
        let id = id.as_str().context("reference ID string")?;
        ensure!(
            !id.is_empty() && ids.insert(id.to_owned()),
            "empty/duplicate reference ID"
        );
    }
    Ok(ids)
}
fn publish(out: &Path, files: Files) -> Result<()> {
    ensure!(!out.try_exists()?, "new reference output required");
    fs::create_dir(out)?;
    // Publish the completion manifest last. A failed write cannot look complete.
    for (file, bytes) in files
        .iter()
        .filter(|(file, _)| file.as_str() != "manifest.json")
    {
        write_new(&out.join(file), bytes)?;
    }
    if let Some(bytes) = files.get("manifest.json") {
        write_new(&out.join("manifest.json"), bytes)?;
    }
    fs::File::open(out)?.sync_all()?;
    Ok(())
}
fn new_output(selection: &Value, output: &Path) -> Result<()> {
    ensure!(!output.try_exists()?, "new output required");
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let output = parent
        .canonicalize()?
        .join(output.file_name().context("output filename")?);
    for c in array(selection, "candidates")? {
        let dir = Path::new(str_at(c, "numeric_path")?).canonicalize()?;
        ensure!(
            !output.starts_with(&dir),
            "output inside frozen numeric parent"
        );
        for run in array(c, "runs")? {
            let fit = Path::new(str_at(run, "model_path")?).canonicalize()?;
            ensure!(!output.starts_with(fit), "output inside frozen fit");
        }
    }
    Ok(())
}
fn selection(path: &Path) -> Result<(Vec<u8>, Value)> {
    let validated = final_test_binding::validate_selection_receipt(path)?;
    let (bytes, value) = read(path)?;
    ensure!(value == validated, "selection changed during validation");
    Ok((bytes, value))
}
fn candidate<'a>(selection: &'a Value, name: &str) -> Result<&'a Value> {
    let rows = array(selection, "candidates")?
        .iter()
        .filter(|c| c["candidate"] == name)
        .collect::<Vec<_>>();
    ensure!(rows.len() == 1, "unknown/ambiguous frozen candidate");
    Ok(rows[0])
}
fn run_for<'a>(candidate: &'a Value, seed: u64) -> Result<&'a Value> {
    ensure!([17, 29, 41].contains(&seed), "predeclared seed required");
    let rows = array(candidate, "runs")?
        .iter()
        .filter(|r| r["seed"] == seed)
        .collect::<Vec<_>>();
    ensure!(rows.len() == 1, "frozen run absent/ambiguous");
    Ok(rows[0])
}
pub(super) fn cli(args: &[OsString]) -> Result<()> {
    match args.first().and_then(|v| v.to_str()) {
        Some("select") if args.len() == 5 => {
            let (sb, s) = selection(Path::new(&args[1]))?;
            new_output(&s, Path::new(&args[4]))?;
            admission::select(
                &s,
                &sb,
                Path::new(&args[2]),
                Path::new(&args[3]),
                Path::new(&args[4]),
            )
        }
        Some("project") if args.len() == 8 => {
            let (sb, s) = selection(Path::new(&args[1]))?;
            new_output(&s, Path::new(&args[7]))?;
            project::project(
                &s,
                &sb,
                args[2].to_str().context("candidate")?,
                Path::new(&args[3]),
                Path::new(&args[4]),
                Path::new(&args[5]),
                Path::new(&args[6]),
                Path::new(&args[7]),
            )
        }
        Some("encode") if args.len() == 6 => {
            let (sb, s) = selection(Path::new(&args[1]))?;
            new_output(&s, Path::new(&args[5]))?;
            encode::encode(
                &s,
                &sb,
                args[2].to_str().context("candidate")?,
                args[3].to_str().context("seed")?.parse()?,
                Path::new(&args[4]),
                Path::new(&args[5]),
            )
        }
        _ => anyhow::bail!(
            "select SELECTION DATASET_MANIFEST REFERENCES NEW_OUT | project SELECTION CANDIDATE REF_RECORDS REF_MANIFEST CFG_EXPORT EMBEDDINGS_OR_DASH NEW_OUT | encode SELECTION CANDIDATE SEED REF_NUMERIC NEW_GALLERY.json"
        ),
    }
}
