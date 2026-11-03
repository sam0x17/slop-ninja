//! Metadata-only Project Gutenberg selection plan.
//!
//! Reads the read-only catalog inventory, one RDF record per candidate (from
//! the bulk RDF feed) and the prior-snapshot index. It never opens book text.
//! Each candidate receives exactly one role:
//!
//! - `excluded`: a rights, metadata, prior-overlap or prior-writer conflict;
//! - `reserved_evaluation`: the writer hashes into the reserved range, so every
//!   work of that writer is held for Dev/CAL/Test reservation and its prose is
//!   never downloaded by a Train run;
//! - `held_family_bucket_reserved`: a Train writer whose work family hashes
//!   into the reserved family range used by the other source runners; not
//!   downloaded, so family-level reservations stay valid too;
//! - `train`: eligible for Train download and adaptation.
//!
//! The split unit follows the protocol: sources sharing a known contributor
//! are one group. A PG writer is identified by both the name key the adapter
//! writes (`author:<author_key>`) and the PG agent id; a prior writer under
//! either identity excludes the work.
use crate::{adapter, gutenberg, io, protected::Index, text};
use anyhow::{Context, Result, ensure};
use roxmltree::Document;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

pub const PLAN_SCHEMA: &str = "slop_ninja_a_pool_gutenberg_plan_v1";
const PG: &str = "http://www.gutenberg.org/2009/pgterms/";
const DC: &str = "http://purl.org/dc/terms/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RELATORS: &str = "http://id.loc.gov/vocabulary/relators/";
/// Relator roles whose holder may have written the English prose itself.
const PROSE_ROLES: &[&str] = &["trl", "aut", "com", "ctb"];

/// One agent of an RDF record: role, PG agent id and name.
#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    pub role: String,
    pub agent_id: Option<String>,
    pub name: String,
}

pub fn agents(xml: &str) -> Result<Vec<Agent>> {
    let doc = Document::parse(xml).context("RDF XML")?;
    let ebook = doc.descendants().find(|n| n.has_tag_name((PG, "ebook"))).context("pgterms:ebook")?;
    let mut out = Vec::new();
    for c in ebook.children().filter(|c| c.is_element()) {
        let role = if c.has_tag_name((DC, "creator")) {
            "creator".to_owned()
        } else if c.tag_name().namespace() == Some(RELATORS) {
            c.tag_name().name().to_owned()
        } else {
            continue;
        };
        let Some(agent) = c.descendants().find(|d| d.has_tag_name((PG, "agent"))) else { continue };
        let name = agent
            .descendants()
            .find(|d| d.has_tag_name((PG, "name")))
            .and_then(|d| d.text())
            .map(|t| t.trim().to_owned())
            .unwrap_or_default();
        out.push(Agent { role, agent_id: agent.attribute((RDF, "about")).map(str::to_owned), name });
    }
    Ok(out)
}

pub fn rdf_path(root: &Path, pg: u64) -> PathBuf {
    root.join(format!("cache/epub/{pg}/pg{pg}.rdf"))
}

pub struct Options<'a> {
    pub inventory: &'a Path,
    pub rdf_root: &'a Path,
    pub index: &'a Path,
    pub extra_writers: Option<&'a Path>,
    pub reserve: (u32, u32),
    pub out: &'a Path,
}

fn in_range(b: u32, r: (u32, u32)) -> bool {
    r.0 <= b && b < r.1
}

/// The writer-bucket key, identical to the adapter's `writer_key`.
pub fn writer_key(author_key: &str) -> String {
    format!("author:{author_key}")
}

pub fn run(o: &Options) -> Result<Value> {
    io::fresh_dir(o.out)?;
    let index = Index::load(o.index)?;
    let inventory: Vec<Value> = io::jsonl_rows(o.inventory)?.collect::<Result<_>>()?;
    let (inv_sha, _) = io::file_sha(o.inventory)?;

    // Prior writers: name keys from prior work-family keys, explicit extras,
    // and every agent of every prior PG id present in the feed.
    let mut prior_names: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut prior_agents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for f in &index.families {
        let mut parts = f.splitn(3, ':');
        let (Some(kind), Some(author)) = (parts.next(), parts.next()) else { continue };
        if kind.starts_with("author_work_v") || kind.starts_with("encoder_stage3_work_v") {
            if !author.is_empty() {
                prior_names.entry(author.to_owned()).or_default().insert(format!("family:{f}"));
            }
        }
    }
    let mut extra_sha = None;
    if let Some(p) = o.extra_writers {
        extra_sha = Some(io::file_sha(p)?.0);
        for l in std::fs::read_to_string(p)?.lines() {
            let k = l.trim();
            if !k.is_empty() && !k.starts_with('#') {
                prior_names.entry(k.to_owned()).or_default().insert("extra_writer_list".to_owned());
            }
        }
    }
    let mut prior_pg_without_rdf = Vec::new();
    for &pg in &index.pg_ids {
        let p = rdf_path(o.rdf_root, pg);
        let Ok(b) = io::read_all(&p, 8 * 1024 * 1024) else {
            prior_pg_without_rdf.push(pg);
            continue;
        };
        let Ok(a) = std::str::from_utf8(&b).map_err(anyhow::Error::from).and_then(agents) else {
            prior_pg_without_rdf.push(pg);
            continue;
        };
        for ag in a.iter().filter(|a| a.role == "creator" || PROSE_ROLES.contains(&a.role.as_str())) {
            let k = text::author_key(&ag.name);
            if !k.is_empty() {
                prior_names.entry(k).or_default().insert(format!("pg:{pg}"));
            }
            if let Some(id) = &ag.agent_id {
                prior_agents.entry(id.clone()).or_default().insert(format!("pg:{pg}"));
            }
        }
    }

    let mut plan = io::JsonlWriter::create(&o.out.join("plan.jsonl"))?;
    let mut train_ids = Vec::new();
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut reasons_n: BTreeMap<String, u64> = BTreeMap::new();
    let mut role_writers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut role_families: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut role_strata: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for inv in &inventory {
        let pg = inv["pg_id"].as_u64().context("inventory pg_id")?;
        ensure!(seen.insert(pg), "duplicate inventory pg_id {pg}");
        let stratum = inv["stratum"].as_str().unwrap_or("unknown").to_owned();
        let mut reasons: Vec<String> = Vec::new();
        if inv["catalog_overlap"].as_array().is_some_and(|a| !a.is_empty()) {
            reasons.push("inventory_catalog_overlap".into());
        }
        for a in inv["aliases"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if a.strip_prefix("pg:").and_then(|n| n.parse().ok()).is_some_and(|n: u64| index.pg_ids.contains(&n)) {
                reasons.push("pg_id_in_prior_snapshot".into());
            } else if a.starts_with("title:") && index.titles.contains(a) {
                reasons.push("title_alias_in_prior_snapshot".into());
            }
        }
        let p = rdf_path(o.rdf_root, pg);
        let mut row = json!({"pg_id": pg, "stratum": stratum, "rdf_path": p, "inventory_title": inv["title"],
                             "inventory_author": inv["catalog_author"]});
        let rdf = io::read_all(&p, 8 * 1024 * 1024).ok();
        match rdf.as_deref().map(|b| (text::sha_hex(b), std::str::from_utf8(b))) {
            None => reasons.push("rdf_missing_in_feed".into()),
            Some((_, Err(_))) => reasons.push("rdf_not_utf8".into()),
            Some((sha, Ok(x))) => {
                row["rdf_sha256"] = json!(sha);
                match (gutenberg::parse_rdf(x), agents(x)) {
                    (Ok(cat), Ok(ags)) if cat.pg_id == pg => {
                        if let Some(r) = gutenberg::catalog_screen(&cat) {
                            reasons.push(r.into());
                        }
                        let creator = cat.creators.first().cloned().unwrap_or_default();
                        let author = text::author_key(&creator);
                        let work = text::title_key(&cat.title);
                        let family = format!("author_work_v2:{author}:{work}");
                        if author.is_empty() || work.is_empty() {
                            reasons.push("empty_family_key".into());
                        }
                        if index.families.contains(&family) {
                            reasons.push("family_key_in_prior_snapshot".into());
                        }
                        if text::title_aliases(&cat.title).iter().any(|a| index.titles.contains(a)) {
                            reasons.push("title_alias_in_prior_snapshot".into());
                        }
                        if prior_names.contains_key(&author) {
                            reasons.push("writer_name_in_prior_snapshot".into());
                        }
                        let creator_agents: Vec<&Agent> = ags.iter().filter(|a| a.role == "creator").collect();
                        if creator_agents.iter().any(|a| a.agent_id.as_ref().is_some_and(|i| prior_agents.contains_key(i))) {
                            reasons.push("writer_agent_in_prior_snapshot".into());
                        }
                        let prose_roles: Vec<&Agent> = ags.iter().filter(|a| PROSE_ROLES.contains(&a.role.as_str())).collect();
                        if !prose_roles.is_empty() {
                            reasons.push("other_prose_contributor_role".into());
                        }
                        let wk = writer_key(&author);
                        row["title"] = json!(cat.title);
                        row["catalog_author"] = json!(creator);
                        row["author_key"] = json!(author);
                        row["writer_key"] = json!(wk);
                        row["creator_agent_ids"] = json!(creator_agents.iter().map(|a| a.agent_id.clone()).collect::<Vec<_>>());
                        row["relator_roles"] = json!(ags.iter().filter(|a| a.role != "creator").map(|a| a.role.clone()).collect::<Vec<_>>());
                        row["source_group"] = json!(family);
                        row["family_bucket"] = json!(adapter::bucket(&family));
                        row["writer_bucket"] = json!(adapter::bucket(&wk));
                        row["pg_ebook_release_date"] = json!(cat.issued);
                        row["rights_statement"] = json!(cat.rights);
                        row["languages"] = json!(cat.languages);
                    }
                    (Ok(_), Ok(_)) => reasons.push("rdf_id_mismatch".into()),
                    _ => reasons.push("rdf_unparsed".into()),
                }
            }
        }
        reasons.sort();
        reasons.dedup();
        let role = if !reasons.is_empty() {
            "excluded"
        } else if in_range(row["writer_bucket"].as_u64().unwrap_or(0) as u32, o.reserve) {
            "reserved_evaluation"
        } else if in_range(row["family_bucket"].as_u64().unwrap_or(0) as u32, o.reserve) {
            "held_family_bucket_reserved"
        } else {
            "train"
        };
        if role == "train" {
            train_ids.push(pg);
        }
        for r in &reasons {
            *reasons_n.entry(r.clone()).or_default() += 1;
        }
        *counts.entry(role.to_owned()).or_default() += 1;
        if let Some(w) = row["writer_key"].as_str() {
            role_writers.entry(role.to_owned()).or_default().insert(w.to_owned());
        }
        if let Some(f) = row["source_group"].as_str() {
            role_families.entry(role.to_owned()).or_default().insert(f.to_owned());
        }
        *role_strata.entry(role.to_owned()).or_default().entry(stratum).or_default() += 1;
        row["role"] = json!(role);
        row["exclusion_reasons"] = json!(reasons);
        plan.write(&row)?;
    }
    let (plan_sha, plan_rows) = plan.finish()?;
    let sel: String = train_ids.iter().map(|i| format!("{i}\n")).collect();
    std::fs::write(o.out.join("train-selection.txt"), &sel)?;
    let prior = json!({
        "writer_name_keys": prior_names.iter().map(|(k, v)| json!({"author_key": k, "evidence": v})).collect::<Vec<_>>(),
        "agent_ids": prior_agents.iter().map(|(k, v)| json!({"agent_id": k, "evidence": v})).collect::<Vec<_>>(),
        "prior_pg_ids_without_feed_rdf": prior_pg_without_rdf,
    });
    let prior_sha = io::write_json(&o.out.join("prior-writers.json"), &prior)?;
    let per_role = |m: &BTreeMap<String, BTreeSet<String>>| -> Value {
        m.iter().map(|(k, v)| (k.clone(), json!(v.len()))).collect::<serde_json::Map<_, _>>().into()
    };
    let summary = json!({
        "schema": PLAN_SCHEMA,
        "created_utc": io::now_utc(),
        "metadata_only": true,
        "book_text_opened": false,
        "inputs": {"inventory": o.inventory, "inventory_sha256": inv_sha, "rdf_root": o.rdf_root,
                   "prior_index": o.index, "prior_index_sha256": index.sha256,
                   "extra_writers": o.extra_writers, "extra_writers_sha256": extra_sha},
        "reserve_range": [o.reserve.0, o.reserve.1],
        "reserve_rule": "writer bucket (adapter::bucket of 'author:<author_key>') in range -> reserved_evaluation for every work of that writer; otherwise family bucket in range -> held_family_bucket_reserved (not downloaded); otherwise train",
        "inventory_rows": inventory.len(),
        "plan_rows": plan_rows,
        "plan_sha256": plan_sha,
        "train_selection_sha256": text::sha_hex(sel.as_bytes()),
        "prior_writers_sha256": prior_sha,
        "prior_writer_name_keys": prior_names.len(),
        "prior_agent_ids": prior_agents.len(),
        "prior_pg_ids_without_feed_rdf": prior["prior_pg_ids_without_feed_rdf"].as_array().map(Vec::len),
        "roles": counts,
        "exclusion_reasons": reasons_n,
        "distinct_writers_by_role": per_role(&role_writers),
        "distinct_families_by_role": per_role(&role_families),
        "strata_by_role": role_strata,
        "limits": [
            "Metadata only: the plan reads RDF and the inventory, never book text.",
            "Writer identity is the PG creator name key and agent id; pseudonyms, ghost-writers and editors' interpolations are not resolved.",
            "Works with a translator, compiler, contributor or second-author relator are excluded so a single named writer owns the prose.",
            "Prior-writer protection covers writers of registered prior PG ids and prior author-work family keys only."
        ],
    });
    io::write_json(&o.out.join("summary.json"), &summary)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protected;

    fn rdf(id: u64, title: &str, creator: &str, agent: u64, extra: &str) -> String {
        format!(r#"<?xml version="1.0" encoding="utf-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:pgterms="http://www.gutenberg.org/2009/pgterms/" xmlns:marcrel="http://id.loc.gov/vocabulary/relators/">
<pgterms:ebook rdf:about="ebooks/{id}">
<dcterms:title>{title}</dcterms:title>
<dcterms:creator><pgterms:agent rdf:about="2009/agents/{agent}"><pgterms:name>{creator}</pgterms:name></pgterms:agent></dcterms:creator>
{extra}
<dcterms:language><rdf:Description><rdf:value>en</rdf:value></rdf:Description></dcterms:language>
<dcterms:rights>Public domain in the USA.</dcterms:rights>
<dcterms:issued>2004-01-01</dcterms:issued>
<dcterms:type><rdf:Description><rdf:value>Text</rdf:value></rdf:Description></dcterms:type>
</pgterms:ebook></rdf:RDF>"#)
    }

    #[test]
    fn plan_roles_and_prior_writers() {
        let d = tempfile::tempdir().unwrap();
        let feed = d.path().join("feed");
        let put = |id: u64, x: String| {
            let p = rdf_path(&feed, id);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, x).unwrap();
        };
        // Prior PG id 1 by agent 10 ("Prior, Pat"); 2 is another work by that agent
        // under a different name form; 3 is by a fresh writer; 4 has a translator.
        put(1, rdf(1, "Old Book", "Prior, Pat", 10, ""));
        put(2, rdf(2, "Second Book", "Prior, Patricia", 10, ""));
        for id in 3..60u64 {
            put(id, rdf(id, &format!("Fresh Work {id}"), &format!("Writer{id}, Ann"), 100 + id, ""));
        }
        put(60, rdf(60, "Translated Tale", "Foreign, Fay", 300, r#"<marcrel:trl><pgterms:agent rdf:about="2009/agents/301"><pgterms:name>Trans, Tom</pgterms:name></pgterms:agent></marcrel:trl>"#));
        // Prior snapshot: a registry with one file mentioning pg 1.
        let snap = d.path().join("snap.jsonl");
        let prose = (0..80).map(|i| format!("a{i}")).collect::<Vec<_>>().join(" ");
        std::fs::write(&snap, format!("{}\n", json!({"pg_id": 1, "source_group": "author_work_v2:old_writer:x", "text": prose}))).unwrap();
        let reg = d.path().join("reg.json");
        std::fs::write(&reg, json!({"schema": protected::REGISTRY_SCHEMA, "entries": [{"label": "s", "path": "snap.jsonl"}]}).to_string()).unwrap();
        let idx = d.path().join("idx");
        protected::build(&reg, d.path(), &idx).unwrap();
        let inv = d.path().join("inv.jsonl");
        let rows: String = (1..=60u64)
            .map(|i| format!("{}\n", json!({"pg_id": i, "aliases": [format!("pg:{i}")], "catalog_overlap": [], "stratum": "fiction"})))
            .collect();
        std::fs::write(&inv, rows).unwrap();
        let extra = d.path().join("extra.txt");
        std::fs::write(&extra, "ann_writer3\n").unwrap();
        let out = d.path().join("plan");
        let s = run(&Options { inventory: &inv, rdf_root: &feed, index: &idx, extra_writers: Some(&extra), reserve: (900, 1000), out: &out }).unwrap();
        let plan: Vec<Value> = io::jsonl_rows(&out.join("plan.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        let by = |id: u64| plan.iter().find(|r| r["pg_id"] == id).unwrap().clone();
        assert_eq!(by(1)["role"], "excluded");
        assert!(by(1)["exclusion_reasons"].as_array().unwrap().iter().any(|r| r == "pg_id_in_prior_snapshot"));
        // Same agent, different name form: excluded through the agent id.
        assert_eq!(by(2)["role"], "excluded");
        assert_eq!(by(2)["exclusion_reasons"], json!(["writer_agent_in_prior_snapshot"]));
        assert_eq!(by(3)["exclusion_reasons"], json!(["writer_name_in_prior_snapshot"]));
        assert_eq!(by(60)["exclusion_reasons"], json!(["other_prose_contributor_role"]));
        // The old family key's author is a prior writer name.
        assert!(s["prior_writer_name_keys"].as_u64().unwrap() >= 2);
        for r in plan.iter().filter(|r| r["role"] != "excluded") {
            let wb = r["writer_bucket"].as_u64().unwrap();
            let fb = r["family_bucket"].as_u64().unwrap();
            assert_eq!(wb, adapter::bucket(r["writer_key"].as_str().unwrap()) as u64);
            let want = if wb >= 900 { "reserved_evaluation" } else if fb >= 900 { "held_family_bucket_reserved" } else { "train" };
            assert_eq!(r["role"], want);
        }
        let sel = std::fs::read_to_string(out.join("train-selection.txt")).unwrap();
        assert_eq!(sel.lines().count() as u64, s["roles"]["train"].as_u64().unwrap());
        assert_eq!(s["book_text_opened"], false);
    }
}
