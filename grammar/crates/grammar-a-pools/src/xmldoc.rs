//! Shared helpers for per-document XML sources (PLOS JATS, Federal Register).
use crate::{
    text::{GAP, Para, Screen, squash},
    wikitext,
};
use anyhow::{Context, Result};
use roxmltree::{Document, Node, ParsingOptions};

/// Rewrite HTML named entities that XML does not predefine as numeric
/// references, so documents without their DTD still parse. Unknown names
/// are left for the parser to reject.
pub fn numeric_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let end = tail
            .char_indices()
            .take(12)
            .find(|&(_, c)| c == ';')
            .map(|(k, _)| k);
        match end {
            Some(k) if !tail.starts_with('#') && !["amp", "lt", "gt", "quot", "apos"].contains(&&tail[..k]) => {
                match wikitext::entity(&tail[..k]) {
                    Some(c) => {
                        out.push_str(&format!("&#{};", c as u32));
                        rest = &tail[k + 1..];
                    }
                    None => {
                        out.push('&');
                        rest = tail;
                    }
                }
            }
            _ => {
                out.push('&');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Strip an internal/external DOCTYPE so roxmltree never resolves it.
pub fn without_doctype(s: &str) -> String {
    if let Some(i) = s.find("<!DOCTYPE") {
        // Internal subsets ("[...]") end at "]>"; plain declarations at ">".
        let rest = &s[i..];
        let end = match (rest.find('['), rest.find('>')) {
            (Some(b), Some(g)) if b < g => rest.find("]>").map(|k| k + 2),
            (_, Some(g)) => Some(g + 1),
            _ => None,
        };
        if let Some(e) = end {
            return format!("{}{}", &s[..i], &s[i + e..]);
        }
    }
    s.to_owned()
}

/// XML documents from a directory tree, a single file or a `.zip` archive,
/// visited in sorted name order.
pub enum Source {
    Files(Vec<std::path::PathBuf>),
    Zip(std::path::PathBuf),
}

impl Source {
    pub fn open(input: &std::path::Path) -> Result<Self> {
        if input.is_file() && input.extension().is_some_and(|e| e == "zip") {
            return Ok(Source::Zip(input.to_owned()));
        }
        if input.is_file() {
            return Ok(Source::Files(vec![input.to_owned()]));
        }
        let mut out = Vec::new();
        let mut stack = vec![input.to_owned()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).with_context(|| format!("read {}", d.display()))? {
                let e = e?;
                let p = e.path();
                if e.file_type()?.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "xml") {
                    out.push(p);
                }
            }
        }
        out.sort();
        anyhow::ensure!(!out.is_empty(), "no .xml files under {}", input.display());
        Ok(Source::Files(out))
    }

    pub fn describe(&self) -> serde_json::Value {
        match self {
            Source::Files(f) => serde_json::json!({"kind": "xml_files", "files": f.len()}),
            Source::Zip(p) => {
                let h = crate::io::file_sha(p).ok();
                serde_json::json!({"kind": "zip_archive", "path": p, "sha256": h.as_ref().map(|x| &x.0), "bytes": h.map(|x| x.1)})
            }
        }
    }

    /// Call `f(name, bytes)` per document (`bytes` is `None` if unreadable
    /// or over `limit`); stop when it returns false.
    pub fn each(&self, limit: u64, mut f: impl FnMut(&str, Option<&[u8]>) -> Result<bool>) -> Result<()> {
        match self {
            Source::Files(files) => {
                for p in files {
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let b = crate::io::read_all(p, limit).ok();
                    if !f(&name, b.as_deref())? {
                        break;
                    }
                }
            }
            Source::Zip(p) => {
                let mut z = zip::ZipArchive::new(std::fs::File::open(p)?).context("zip archive")?;
                let mut names: Vec<(String, usize)> = (0..z.len())
                    .filter_map(|i| z.by_index_raw(i).ok().map(|e| (e.name().to_owned(), i)))
                    .filter(|(n, _)| n.ends_with(".xml"))
                    .collect();
                names.sort();
                for (name, i) in names {
                    let base = name.rsplit('/').next().unwrap_or(&name).to_owned();
                    let mut b = Vec::new();
                    let ok = z
                        .by_index(i)
                        .ok()
                        .and_then(|e| {
                            use std::io::Read;
                            (e.size() <= limit).then_some(e)?.take(limit + 1).read_to_end(&mut b).ok()
                        })
                        .is_some();
                    if !f(&base, ok.then_some(b.as_slice()))? {
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Source text made parseable without its DTD.
pub fn prepare(xml: &str) -> String {
    numeric_entities(&without_doctype(xml))
}

pub fn document(prepared: &str) -> Result<Document<'_>> {
    Document::parse_with_options(prepared, ParsingOptions { allow_dtd: false, ..Default::default() }).context("XML document")
}

/// Render the text of a paragraph element. Elements in `gap` leave a gap
/// (their paragraph will be non-prose); elements in `drop` are skipped
/// silently; everything else contributes its text.
pub fn paragraph(node: Node, gap: &[&str], drop: &[&str], screen: Screen) -> Para {
    fn walk(n: Node, gap: &[&str], drop: &[&str], out: &mut String) {
        for c in n.children() {
            if c.is_text() {
                out.push_str(c.text().unwrap_or(""));
            } else if c.is_element() {
                let name = c.tag_name().name();
                if drop.contains(&name) {
                    continue;
                }
                if gap.contains(&name) {
                    out.push(GAP);
                    continue;
                }
                walk(c, gap, drop, out);
            }
        }
    }
    let mut s = String::new();
    walk(node, gap, drop, &mut s);
    Para::classify(squash(&s), screen)
}

pub fn text_of(n: Node) -> String {
    squash(&n.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn entities_and_doctype() {
        assert_eq!(numeric_entities("a&nbsp;b &amp; &mdash; &#233; &bogus;"), "a&#32;b &amp; &#8212; &#233; &bogus;");
        assert_eq!(without_doctype("<?xml version=\"1.0\"?><!DOCTYPE article PUBLIC \"x\" \"y.dtd\"><article/>"), "<?xml version=\"1.0\"?><article/>");
    }
}
