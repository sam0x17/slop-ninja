//! Contributor-closure components of an admitted human pool.
//!
//! Reads text-free metadata rows (JSONL with `id`, `source_group`, optional
//! `writer_key`, optional `pool_admission.split_component`) and merges source
//! groups by transitive closure over shared writer keys and dedup split
//! components, as the A protocol requires for split and bootstrap groups.
//!
//! `slop_ninja_pool_components METADATA.jsonl --out DIR` writes
//! `components.jsonl` (one row per source group with its component key) and
//! `summary.json` (size distribution, largest components, per-prefix counts).
//! It never reads text.
//!
//! `--writer-alias ALIASES.jsonl` maps `writer_key` values to a canonical
//! contributor (rows `{"writer_key", "canonical"}`) before merging, for
//! example per-site Stack Exchange users to their network account.
//! `slop_ninja_pool_components se-accounts --site SITE < Users.xml` writes
//! that map from a dump's `Users.xml`, reading only `Id` and `AccountId`.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

struct Uf(Vec<usize>);
impl Uf {
    fn find(&mut self, mut x: usize) -> usize {
        while self.0[x] != x {
            self.0[x] = self.0[self.0[x]];
            x = self.0[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            let (lo, hi) = if a < b { (a, b) } else { (b, a) };
            self.0[hi] = lo;
        }
    }
}

#[derive(Default)]
struct Nodes {
    ids: HashMap<String, usize>,
    names: Vec<String>,
}
impl Nodes {
    fn get(&mut self, k: &str) -> usize {
        if let Some(&i) = self.ids.get(k) {
            return i;
        }
        let i = self.names.len();
        self.ids.insert(k.to_owned(), i);
        self.names.push(k.to_owned());
        i
    }
}

/// Site or source prefix of a family key: everything before the last `:`-separated
/// two fields (for example `stackexchange:pets.stackexchange.com`).
fn prefix(family: &str) -> String {
    let parts: Vec<&str> = family.split(':').collect();
    if parts.len() > 2 { parts[..parts.len() - 2].join(":") } else { parts[0].to_owned() }
}

pub fn load_aliases(p: &Path) -> Result<HashMap<String, String>> {
    let mut m = HashMap::new();
    for line in BufReader::new(std::fs::File::open(p).with_context(|| format!("open {}", p.display()))?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).context("alias row")?;
        let (Some(w), Some(c)) = (v["writer_key"].as_str(), v["canonical"].as_str()) else { bail!("alias row needs writer_key and canonical") };
        if let Some(prev) = m.insert(w.to_owned(), c.to_owned()) {
            if prev != c {
                bail!("conflicting aliases for {w}");
            }
        }
    }
    Ok(m)
}

///  ->  rows of a Stack Exchange , as alias rows.
pub fn se_accounts<R: BufRead, W: Write>(site: &str, users: R, out: &mut W) -> Result<(u64, u64)> {
    use quick_xml::{Reader, events::Event};
    let mut reader = Reader::from_reader(users);
    let mut buf = Vec::new();
    let (mut rows, mut mapped) = (0u64, 0u64);
    loop {
        match reader.read_event_into(&mut buf).context("Users.xml")? {
            Event::Empty(e) | Event::Start(e) if e.local_name().as_ref() == b"row" => {
                rows += 1;
                let (mut id, mut acc) = (None, None);
                for a in e.attributes() {
                    let a = a?;
                    match a.key.as_ref() {
                        b"Id" => id = Some(String::from_utf8(a.value.to_vec())?),
                        b"AccountId" => acc = Some(String::from_utf8(a.value.to_vec())?),
                        _ => {}
                    }
                }
                if let (Some(id), Some(acc)) = (id, acc) {
                    if id.parse::<i64>().is_err() || acc.parse::<i64>().is_err() {
                        bail!("non-numeric user or account id");
                    }
                    // The Community bot (negative ids) owns no admitted posts and has no person behind it.
                    if !id.starts_with('-') && !acc.starts_with('-') {
                        writeln!(out, "{}", json!({"writer_key": format!("stackexchange:{site}:user:{id}"),
                                                  "canonical": format!("stackexchange_network:account:{acc}")}))?;
                        mapped += 1;
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    out.flush()?;
    Ok((rows, mapped))
}

pub fn run(meta: &Path, out: &Path) -> Result<Value> {
    run_with(meta, out, None)
}

pub fn run_with(meta: &Path, out: &Path, alias_path: Option<&Path>) -> Result<Value> {
    if out.exists() {
        bail!("output exists: {}", out.display());
    }
    let aliases = match alias_path {
        Some(p) => load_aliases(p)?,
        None => HashMap::new(),
    };
    let mut aliased = 0u64;
    let mut nodes = Nodes::default();
    let mut family_docs: BTreeMap<usize, u64> = BTreeMap::new();
    let mut edges: Vec<(usize, usize)> = Vec::new();
    let mut is_family: Vec<bool> = Vec::new();
    let mut rows = 0u64;
    for line in BufReader::new(std::fs::File::open(meta).with_context(|| format!("open {}", meta.display()))?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).context("metadata row")?;
        if v.get("text").is_some() {
            bail!("metadata row carries text; refuse to read text");
        }
        rows += 1;
        let fam = v["source_group"].as_str().context("source_group")?;
        let f = nodes.get(&format!("f\0{fam}"));
        if is_family.len() <= f {
            is_family.resize(f + 1, false);
        }
        is_family[f] = true;
        *family_docs.entry(f).or_default() += 1;
        if let Some(w) = v["writer_key"].as_str() {
            let w = match aliases.get(w) {
                Some(c) => {
                    aliased += 1;
                    c.as_str()
                }
                None => w,
            };
            let n = nodes.get(&format!("w\0{w}"));
            edges.push((f, n));
        }
        if let Some(c) = v["pool_admission"]["split_component"].as_str() {
            let n = nodes.get(&format!("c\0{c}"));
            edges.push((f, n));
        }
    }
    is_family.resize(nodes.names.len(), false);
    let mut uf = Uf((0..nodes.names.len()).collect());
    for (a, b) in edges {
        uf.union(a, b);
    }
    // Component key: smallest family key among member families.
    let mut comp: BTreeMap<usize, (String, u64, u64, u64)> = BTreeMap::new(); // key, families, docs, writers
    for i in 0..nodes.names.len() {
        let r = uf.find(i);
        let e = comp.entry(r).or_insert_with(|| (String::new(), 0, 0, 0));
        let name = &nodes.names[i];
        if is_family[i] {
            let k = &name[2..];
            if e.0.is_empty() || k < e.0.as_str() {
                e.0 = k.to_owned();
            }
            e.1 += 1;
            e.2 += family_docs[&i];
        } else if name.starts_with("w\0") {
            e.3 += 1;
        }
    }
    std::fs::create_dir_all(out)?;
    let mut w = BufWriter::new(std::fs::File::create(out.join("components.jsonl"))?);
    for (&f, &docs) in &family_docs {
        let r = uf.find(f);
        writeln!(w, "{}", json!({"source_group": &nodes.names[f][2..], "documents": docs, "contributor_component": comp[&r].0}))?;
    }
    w.flush()?;
    let mut sizes: Vec<(u64, u64, u64, String)> = comp.values().filter(|c| c.1 > 0).map(|c| (c.1, c.2, c.3, c.0.clone())).collect();
    sizes.sort_by(|a, b| b.cmp(a));
    let total_fam: u64 = sizes.iter().map(|s| s.0).sum();
    let total_docs: u64 = sizes.iter().map(|s| s.1).sum();
    let mut hist: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for s in &sizes {
        let b = match s.0 { 1 => "1", 2..=5 => "2-5", 6..=50 => "6-50", 51..=1000 => "51-1000", _ => ">1000" };
        let e = hist.entry(b).or_default();
        e.0 += 1;
        e.1 += s.1;
    }
    // Per prefix: families, docs, components and largest component share.
    let mut per: BTreeMap<String, (u64, u64, BTreeMap<String, u64>)> = BTreeMap::new();
    for (&f, &docs) in &family_docs {
        let r = uf.find(f);
        let e = per.entry(prefix(&nodes.names[f][2..])).or_default();
        e.0 += 1;
        e.1 += docs;
        *e.2.entry(comp[&r].0.clone()).or_default() += 1;
    }
    let per_json: serde_json::Map<String, Value> = per
        .into_iter()
        .map(|(k, (fams, docs, comps))| {
            let largest = comps.values().copied().max().unwrap_or(0);
            let singletons = comps.values().filter(|&&n| n == 1).count();
            (k, json!({"families": fams, "documents": docs, "components": comps.len(),
                       "largest_component_families": largest, "singleton_components": singletons}))
        })
        .collect();
    let summary = json!({
        "schema": "slop_ninja_a_pool_contributor_components_v1",
        "input": meta.display().to_string(),
        "rows": rows,
        "families": total_fam,
        "documents": total_docs,
        "components": sizes.len(),
        "merge_rule": "transitive closure over shared writer_key (after aliases) and dedup split_component",
        "writer_aliases": alias_path.map(|p| p.display().to_string()),
        "writer_alias_entries": aliases.len(),
        "rows_with_aliased_writer": aliased,
        "histogram_by_component_families": hist.iter().map(|(k, (n, d))| (k.to_string(), json!({"components": n, "documents": d}))).collect::<serde_json::Map<_, _>>(),
        "largest_components": sizes.iter().take(10).map(|s| json!({"families": s.0, "documents": s.1, "writers": s.2,
            "family_share": s.0 as f64 / total_fam.max(1) as f64})).collect::<Vec<_>>(),
        "per_prefix": per_json,
    });
    std::fs::write(out.join("summary.json"), serde_json::to_string_pretty(&summary)? + "\n")?;
    Ok(summary)
}

fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.as_slice() {
        [meta, flag, out] if flag == "--out" => {
            println!("{}", serde_json::to_string_pretty(&run(Path::new(meta), Path::new(out))?)?);
            Ok(())
        }
        [meta, flag, out, af, aliases] if flag == "--out" && af == "--writer-alias" => {
            println!("{}", serde_json::to_string_pretty(&run_with(Path::new(meta), Path::new(out), Some(Path::new(aliases)))?)?);
            Ok(())
        }
        [cmd, sf, site] if cmd == "se-accounts" && sf == "--site" => {
            let (rows, mapped) = se_accounts(site, std::io::stdin().lock(), &mut BufWriter::new(std::io::stdout().lock()))?;
            eprintln!("{}", json!({"site": site, "user_rows": rows, "mapped": mapped}));
            Ok(())
        }
        _ => bail!("usage: slop_ninja_pool_components METADATA.jsonl --out DIR [--writer-alias ALIASES.jsonl] | se-accounts --site SITE < Users.xml"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writers_and_components_merge_transitively() {
        let d = std::env::temp_dir().join(format!("pool-components-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let m = d.join("m.jsonl");
        let rows = [
            json!({"id": "1", "source_group": "se:s:question:1", "writer_key": "se:s:user:a"}),
            json!({"id": "2", "source_group": "se:s:question:1", "writer_key": "se:s:user:b"}),
            json!({"id": "3", "source_group": "se:s:question:2", "writer_key": "se:s:user:b"}),
            json!({"id": "4", "source_group": "se:s:question:3", "writer_key": "se:s:user:c",
                   "pool_admission": {"split_component": "se:s:question:3"}}),
            json!({"id": "5", "source_group": "se:s:question:4", "writer_key": "se:s:user:d",
                   "pool_admission": {"split_component": "se:s:question:3"}}),
            json!({"id": "6", "source_group": "se:s:question:5"}),
        ];
        std::fs::write(&m, rows.iter().map(|r| r.to_string() + "\n").collect::<String>()).unwrap();
        let s = run(&m, &d.join("out")).unwrap();
        assert_eq!(s["families"], 5);
        assert_eq!(s["components"], 3);
        assert_eq!(s["largest_components"][0]["families"], 2);
        assert_eq!(s["per_prefix"]["se:s"]["singleton_components"], 1);
        let c = std::fs::read_to_string(d.join("out/components.jsonl")).unwrap();
        assert!(c.contains(r#""contributor_component":"se:s:question:1""#));
        assert!(run(&m, &d.join("out")).is_err());
        let t = d.join("t.jsonl");
        std::fs::write(&t, json!({"id": "1", "source_group": "g", "text": "x"}).to_string()).unwrap();
        assert!(run(&t, &d.join("out2")).is_err());
        // Aliases merge two per-site users into one network account.
        let a = d.join("a.jsonl");
        std::fs::write(&a, [json!({"writer_key": "se:s:user:c", "canonical": "net:9"}), json!({"writer_key": "se:s:user:a", "canonical": "net:9"})]
            .iter().map(|r| r.to_string() + "\n").collect::<String>()).unwrap();
        let s = run_with(&m, &d.join("out3"), Some(&a)).unwrap();
        assert_eq!(s["components"], 2);
        assert_eq!(s["rows_with_aliased_writer"], 2);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn se_accounts_reads_only_ids() {
        let x = r#"<?xml version="1.0"?><users><row Id="-1" AccountId="-1" DisplayName="Synthetic" AboutMe="x" /><row Id="5" AccountId="12" /><row Id="6" /></users>"#;
        let mut out = Vec::new();
        assert_eq!(se_accounts("s.example", x.as_bytes(), &mut out).unwrap(), (3, 1));
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains(r#""writer_key":"stackexchange:s.example:user:5""#) && s.contains("stackexchange_network:account:12"));
        assert!(!s.contains("Synthetic") && !s.contains("AboutMe"));
    }
}
