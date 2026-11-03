//! Hash-bound local experiment I/O. No annotation or model invocation here.
use crate::verbnet_resource::{Resource, XmlSource};
use anyhow::{Context, Result, ensure};
use grammar_core::{
    edits,
    syntax::{self, Document},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Component, Path},
};

pub type Parsed = std::result::Result<Document, String>;
pub fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .with_context(|| format!("Missing string {key}"))
}
pub fn rows<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    v[key]
        .as_array()
        .with_context(|| format!("Missing array {key}"))
}
pub fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}
pub fn safe(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && Path::new(name)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "Expected normal relative path"
    );
    Ok(())
}
pub fn checked(path: &Path, sha: &str) -> Result<Vec<u8>> {
    ensure!(
        sha.len() == 64
            && sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "Invalid SHA256"
    );
    let b = fs::read(path).with_context(|| format!("Read {}", path.display()))?;
    ensure!(hash(&b) == sha, "Hash differs: {}", path.display());
    Ok(b)
}
pub fn bytes(repo: &Path, binding: &Value) -> Result<Vec<u8>> {
    checked(&repo.join(text(binding, "path")?), text(binding, "sha256")?)
}
pub fn bound(repo: &Path, binding: &Value) -> Result<Value> {
    Ok(serde_json::from_slice(&bytes(repo, binding)?)?)
}
pub fn binding(repo: &Path, name: &str) -> Result<Value> {
    safe(name)?;
    Ok(json!({"path":name,"sha256":hash(&fs::read(repo.join(name))?)}))
}
pub fn rebase(prefix: &str, binding: &Value) -> Result<Value> {
    safe(text(binding, "path")?)?;
    let mut b = binding.clone();
    b["path"] = json!(format!("{prefix}/{}", text(binding, "path")?));
    Ok(b)
}
pub fn output(out: &Path, name: &str, value: &Value) -> Result<Value> {
    safe(name)?;
    let path = out.join(name);
    fs::create_dir_all(path.parent().unwrap())?;
    let mut b = serde_json::to_vec_pretty(value)?;
    b.push(b'\n');
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?
        .write_all(&b)?;
    Ok(json!({"path":name,"sha256":hash(&b)}))
}
pub fn index(report: &Value) -> Result<BTreeMap<String, Value>> {
    let mut map = BTreeMap::new();
    for b in rows(report, "artifacts")? {
        let name = text(b, "path")?;
        safe(name)?;
        ensure!(
            map.insert(name.into(), b.clone()).is_none(),
            "Duplicate artifact path"
        );
    }
    Ok(map)
}
pub fn verify_parser(repo: &Path, parser: &Value) -> Result<()> {
    bytes(repo, &parser["parser_python"])?;
    bytes(repo, &parser["environment_manifest"])?;
    ensure!(
        !rows(parser, "asset_bindings")?.is_empty(),
        "Missing parser assets"
    );
    for b in rows(parser, "asset_bindings")? {
        bytes(repo, b)?;
    }
    Ok(())
}
pub fn resource(repo: &Path, binding: &Value) -> Result<Resource> {
    let m = bound(repo, binding)?;
    ensure!(
        m["schema"] == "slopninja-verbnet-resource-manifest-v1" && m["version"] == "3.4",
        "Resource identity differs"
    );
    for k in ["archive", "license", "readme"] {
        bytes(repo, &m[k])?;
    }
    for b in rows(&m, "supporting_files")? {
        bytes(repo, b)?;
    }
    let sources = rows(&m, "files")?
        .iter()
        .map(|b| {
            Ok(XmlSource {
                path: text(b, "path")?.into(),
                sha256: text(b, "sha256")?.into(),
                xml: String::from_utf8(bytes(repo, b)?)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Resource::from_sources(sources)
}
pub fn validate_doc(value: &Value, identity: &str, sha: &str) -> Result<Document> {
    let doc: Document = serde_json::from_value(value.clone())?;
    syntax::validate(&doc)?;
    ensure!(
        doc.parser_identity == identity && edits::digest(&doc.text) == sha,
        "Document text/parser binding differs"
    );
    Ok(doc)
}
pub fn document(repo: &Path, binding: &Value, identity: &str, sha: &str) -> Result<Parsed> {
    let v = bound(repo, binding)?;
    match text(&v, "status")? {
        "ok" => Ok(Ok(validate_doc(&v["document"], identity, sha)?)),
        "error" | "process_error" => {
            ensure!(
                edits::digest(text(&v, "text")?) == sha,
                "Failed document source differs"
            );
            Ok(Err(text(&v, "error")?.into()))
        }
        _ => anyhow::bail!("Unknown document wrapper status"),
    }
}
