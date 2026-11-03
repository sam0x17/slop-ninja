//! Source plan: which inputs each source adapter reads, in ingestion order.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const PLAN_SCHEMA: &str = "slop_ninja_corpus_plan_v1";

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub schema: String,
    /// Absolute paths no source may read (sealed Test material).
    #[serde(default)]
    pub forbidden_prefixes: Vec<String>,
    /// Exceptions inside a forbidden prefix (read-only, non-Test material).
    #[serde(default)]
    pub allowed_prefixes: Vec<String>,
    pub sources: Vec<SourceSpec>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct SourceSpec {
    pub id: String,
    pub adapter: String,
    pub priority: i64,
    pub root: String,
    pub inputs: Vec<InputSpec>,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct InputSpec {
    /// Relative path or glob (`*` within a component, `**` for any depth) under the root.
    pub pattern: String,
    pub role: String,
    /// jsonl | json | raw (default: from the extension, `.zst` stripped).
    #[serde(default)]
    pub format: Option<String>,
    /// Patterns (same syntax) to leave out.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// An input that may legitimately be absent.
    #[serde(default)]
    pub optional: bool,
}

pub fn load(path: &Path) -> Result<Plan> {
    let p: Plan = serde_json::from_slice(&std::fs::read(path)?).with_context(|| format!("parse {}", path.display()))?;
    ensure!(p.schema == PLAN_SCHEMA, "plan schema {}", p.schema);
    let mut ids = std::collections::HashSet::new();
    for s in &p.sources {
        ensure!(ids.insert(s.id.clone()), "duplicate source id {}", s.id);
    }
    Ok(p)
}

fn match_component(pat: &str, s: &str) -> bool {
    // `*` matches any run of characters; no other metacharacters.
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == s;
    }
    let mut rest = s;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            if !rest.starts_with(part) {
                return false;
            }
            rest = &rest[part.len()..];
        } else if i == parts.len() - 1 {
            return rest.ends_with(part) && rest.len() >= part.len();
        } else if let Some(k) = rest.find(part) {
            rest = &rest[k + part.len()..];
        } else {
            return false;
        }
    }
    true
}

pub fn glob_match(pat: &str, rel: &str) -> bool {
    fn go(p: &[&str], s: &[&str]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(&"**") => (0..=s.len()).any(|k| go(&p[1..], &s[k..])),
            Some(c) => !s.is_empty() && match_component(c, s[0]) && go(&p[1..], &s[1..]),
        }
    }
    let p: Vec<&str> = pat.split('/').collect();
    let s: Vec<&str> = rel.split('/').collect();
    go(&p, &s)
}

fn literal_prefix(pat: &str) -> String {
    let mut out = Vec::new();
    for c in pat.split('/') {
        if c.contains('*') {
            break;
        }
        out.push(c);
    }
    out.join("/")
}

/// Resolve an input to sorted (relative path, absolute path) pairs. Symlinked
/// directories are not followed; regular files only.
pub fn resolve(root: &Path, input: &InputSpec) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    if !input.pattern.contains('*') {
        let p = root.join(&input.pattern);
        if p.is_file() {
            out.push((input.pattern.clone(), p));
        } else if !input.optional {
            bail!("missing input {}", p.display());
        }
        return Ok(out);
    }
    let prefix = literal_prefix(&input.pattern);
    let start = if prefix.is_empty() { root.to_path_buf() } else { root.join(&prefix) };
    if !start.is_dir() {
        if input.optional {
            return Ok(out);
        }
        bail!("missing input directory {}", start.display());
    }
    let mut stack = vec![start];
    while let Some(d) = stack.pop() {
        let mut ents: Vec<_> = std::fs::read_dir(&d)?.collect::<std::io::Result<_>>()?;
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            let ft = e.file_type()?;
            let p = e.path();
            let rel = p.strip_prefix(root)?.to_string_lossy().to_string();
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file()
                && glob_match(&input.pattern, &rel)
                && !input.exclude.iter().any(|x| glob_match(x, &rel))
            {
                out.push((rel, p));
            }
        }
    }
    out.sort();
    if out.is_empty() && !input.optional {
        bail!("input {} under {} matched no file", input.pattern, root.display());
    }
    Ok(out)
}

pub fn format_of(input: &InputSpec, rel: &str) -> Result<crate::archive::Format> {
    use crate::archive::Format;
    let name = rel.strip_suffix(".zst").unwrap_or(rel);
    let f = match input.format.as_deref() {
        Some("jsonl") => Format::Jsonl,
        Some("json") => Format::Json,
        Some("raw") => Format::Raw,
        Some(x) => bail!("unknown format {x}"),
        None if name.ends_with(".jsonl") => Format::Jsonl,
        None if name.ends_with(".json") => Format::Json,
        None => Format::Raw,
    };
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::glob_match;
    #[test]
    fn globs() {
        assert!(glob_match("a/**/records.jsonl", "a/x/y/records.jsonl"));
        assert!(glob_match("a/**/records.jsonl", "a/records.jsonl"));
        assert!(glob_match("runs-v2/*/journal-*.jsonl", "runs-v2/qwen36/journal-1-w0.jsonl"));
        assert!(!glob_match("runs-v2/*/journal-*.jsonl", "runs-v2/qwen36/server-1.json"));
        assert!(glob_match("*", "x"));
        assert!(!glob_match("a/*", "a/b/c"));
    }
}
