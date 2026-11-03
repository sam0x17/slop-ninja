//! Stack Exchange data-dump adapter (`Posts.xml`, optional `Users.xml`).
use crate::{
    adapter::Run,
    html,
    record::{self, Doc},
    text::{self, Screen},
};
use anyhow::{Context, Result};
use quick_xml::{Reader, events::Event};
use serde_json::json;
use std::{collections::BTreeMap, io::BufRead};

/// Every `<row .../>` of a dump file as an attribute map.
pub fn rows<R: BufRead>(r: R, mut f: impl FnMut(BTreeMap<String, String>) -> Result<bool>) -> Result<()> {
    let mut reader = Reader::from_reader(r);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf).context("Stack Exchange XML")? {
            Event::Empty(e) | Event::Start(e) if e.local_name().as_ref() == b"row" => {
                let mut m = BTreeMap::new();
                for a in e.attributes() {
                    let a = a?;
                    let k = String::from_utf8(a.key.as_ref().to_vec())?;
                    let v = quick_xml::escape::unescape(std::str::from_utf8(&a.value)?)?.into_owned();
                    m.insert(k, v);
                }
                if !f(m)? {
                    return Ok(());
                }
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
        buf.clear();
    }
}

pub fn user_names<R: BufRead>(r: R) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    rows(r, |m| {
        if let (Some(id), Some(name)) = (m.get("Id"), m.get("DisplayName")) {
            out.insert(id.clone(), name.clone());
        }
        Ok(true)
    })?;
    Ok(out)
}

/// Licence of a post version, from the dump's `ContentLicense` when present,
/// otherwise from Stack Exchange's published contribution-date schedule.
pub fn post_license(content_license: Option<&str>, version_date: &str) -> Option<String> {
    if let Some(l) = content_license {
        let l = l.trim().to_uppercase().replace(' ', "-");
        return match l.as_str() {
            "CC-BY-SA-2.5" | "CC-BY-SA-3.0" | "CC-BY-SA-4.0" => Some(l),
            _ => None,
        };
    }
    let d = version_date.get(..10)?;
    Some(if d < "2011-04-08" {
        "CC-BY-SA-2.5"
    } else if d < "2018-05-02" {
        "CC-BY-SA-3.0"
    } else {
        "CC-BY-SA-4.0"
    }
    .into())
}

pub struct Site<'a> {
    pub domain: &'a str,
    pub dump: &'a str,
    pub dump_sha256: Option<&'a str>,
    pub dump_url: Option<&'a str>,
}

pub fn adapt<R: BufRead>(posts: R, site: &Site, users: &BTreeMap<String, String>, run: &mut Run) -> Result<()> {
    let method = format!("{};{}", html::RENDER_METHOD, text::PARAGRAPH_RUN_METHOD);
    let collection = "stackexchange_historical";
    rows(posts, |m| {
        run.units_seen += 1;
        let id = m.get("Id").cloned().unwrap_or_default();
        let unit = format!("stackexchange:{}:post:{id}", site.domain);
        let kind = m.get("PostTypeId").map(String::as_str);
        if !matches!(kind, Some("1" | "2")) {
            run.count("not_question_or_answer");
            return Ok(true);
        }
        // Sample whole threads so a question and its answers stay together.
        let thread = if kind == Some("1") { id.clone() } else { m.get("ParentId").cloned().unwrap_or_default() };
        let meta = || {
            json!({"site": site.domain, "post_id": id, "post_type": if kind == Some("1") { "question" } else { "answer" },
                   "thread_id": thread, "owner_user_id": m.get("OwnerUserId"), "last_editor_user_id": m.get("LastEditorUserId"),
                   "creation_date": m.get("CreationDate"), "last_edit_date": m.get("LastEditDate"),
                   "community_owned": m.contains_key("CommunityOwnedDate"), "content_license": m.get("ContentLicense"),
                   "body_html_sha256": m.get("Body").map(|b| text::sha_hex(b.as_bytes())),
                   "dump": site.dump, "dump_sha256": site.dump_sha256})
        };
        if !run.sampled_meta(&format!("stackexchange:{}:question:{thread}", site.domain), meta)? {
            return Ok(true);
        }
        let qid = match kind {
            Some("1") => id.clone(),
            Some("2") => match m.get("ParentId") {
                Some(p) => p.clone(),
                None => return run.reject(&unit, "answer_without_parent").map(|_| true),
            },
            _ => return run.reject(&unit, "not_question_or_answer").map(|_| true),
        };
        let created = m.get("CreationDate").cloned().unwrap_or_default();
        let edited = m.get("LastEditDate").cloned();
        let version = edited.clone().unwrap_or_else(|| created.clone());
        let reason = if !record::before_cutoff(&created) {
            Some("created_not_before_2022_11_30")
        } else if !record::before_cutoff(&version) {
            Some("edited_not_before_2022_11_30")
        } else if m.contains_key("CommunityOwnedDate") {
            Some("community_wiki_multiple_authors")
        } else if m.get("OwnerUserId").is_none_or(|u| u.is_empty() || u == "-1") {
            Some("no_owner_user")
        } else {
            None
        };
        if let Some(r) = reason {
            return run.reject(&unit, r).map(|_| true);
        }
        let owner = m["OwnerUserId"].clone();
        let Some(license) = post_license(m.get("ContentLicense").map(String::as_str), &version) else {
            return run.reject(&unit, "license_not_admitted").map(|_| true);
        };
        let body = m.get("Body").map(String::as_str).unwrap_or("");
        let paras = html::render(body, Screen::XML);
        let Some(ex) = text::paragraph_run(&paras) else {
            return run.reject(&unit, "no_paragraph_run_within_bounds").map(|_| true);
        };
        let path = if kind == Some("1") { "q" } else { "a" };
        let url = format!("https://{}/{path}/{id}", site.domain);
        let user_url = format!("https://{}/users/{owner}", site.domain);
        let name = users.get(&owner).cloned().or_else(|| m.get("OwnerDisplayName").cloned());
        let edited_by_other = m.get("LastEditorUserId").is_some_and(|e| *e != owner);
        let doc = Doc {
            id: format!("pool-stackexchange-{}-{id}", site.domain),
            collection,
            domain: "qa_forum",
            source_group: format!("stackexchange:{}:question:{qid}", site.domain),
            writer_key: Some(format!("stackexchange:{}:user:{owner}", site.domain)),
            evidence: "historical_proxy",
            excerpt: &ex,
            extraction_method: &method,
            unit: json!({"kind": if kind == Some("1") {"question_body"} else {"answer_body"},
                         "body_html_sha256": text::sha_hex(body.as_bytes()),
                         "edited_by_another_user": edited_by_other}),
            provenance: json!({"source_url":url,"site":site.domain,"post_id":id,"question_id":qid,
                               "post_type": if kind == Some("1") {"question"} else {"answer"},
                               "owner_user_id":owner,"owner_display_name":name,"creation_date":created,
                               "last_edit_date":edited,"score":m.get("Score"),"dump":site.dump,
                               "dump_url":site.dump_url,"dump_sha256":site.dump_sha256,
                               "content_license_attribute":m.get("ContentLicense")}),
            date: Some(version.clone()),
            date_basis: "post_version_date_last_edit_or_creation",
            cutoff_policy: "dated_before_cutoff",
            rights: record::rights(
                &license,
                format!("{} ({user_url}); {url}; {}", name.as_deref().unwrap_or("Stack Exchange user"), license.replace('-', " ").replacen("CC BY SA", "CC BY-SA", 1)),
                "https://stackoverflow.com/help/licensing",
                "Single owning author unless edited_by_another_user; code blocks, quotations, lists and headings break paragraph runs.",
            ),
            aliases: vec![],
        };
        run.accept(&unit, &doc)?;
        Ok(!run.full())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn esc(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }
    fn body(n: usize) -> String {
        format!("<p>{} done.</p>\n<p>{} done.</p>", vec!["alpha"; n].join(" "), vec!["beta"; n].join(" "))
    }

    #[test]
    fn licence_schedule_and_attribute() {
        assert_eq!(post_license(None, "2010-01-01T00:00:00").as_deref(), Some("CC-BY-SA-2.5"));
        assert_eq!(post_license(None, "2015-01-01T00:00:00").as_deref(), Some("CC-BY-SA-3.0"));
        assert_eq!(post_license(None, "2019-01-01T00:00:00").as_deref(), Some("CC-BY-SA-4.0"));
        assert_eq!(post_license(Some("CC BY-SA 4.0"), "2010-01-01").as_deref(), Some("CC-BY-SA-4.0"));
        assert_eq!(post_license(Some("CC BY-NC 4.0"), "2010-01-01"), None);
    }

    #[test]
    fn adapter_groups_answers_with_questions_and_screens_dates() {
        let rows = [
            format!(r#"<row Id="1" PostTypeId="1" CreationDate="2012-01-01T00:00:00.000" OwnerUserId="5" Body="{}" />"#, esc(&body(110))),
            format!(r#"<row Id="2" PostTypeId="2" ParentId="1" CreationDate="2012-01-02T00:00:00.000" OwnerUserId="6" Body="{}" />"#, esc(&body(120))),
            format!(r#"<row Id="3" PostTypeId="2" ParentId="1" CreationDate="2012-01-02T00:00:00.000" LastEditDate="2023-01-01T00:00:00.000" OwnerUserId="7" Body="{}" />"#, esc(&body(120))),
            format!(r#"<row Id="4" PostTypeId="1" CreationDate="2012-01-01T00:00:00.000" OwnerUserId="5" CommunityOwnedDate="2012-01-01T00:00:00.000" Body="{}" />"#, esc(&body(120))),
            format!(r#"<row Id="5" PostTypeId="1" CreationDate="2012-01-01T00:00:00.000" Body="{}" />"#, esc(&body(120))),
        ];
        let xml = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<posts>\n{}\n</posts>", rows.join("\n"));
        let users = user_names(r#"<users><row Id="5" DisplayName="Synthetic A" /></users>"#.as_bytes()).unwrap();
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("se");
        let mut run = Run::start(&out, "stackexchange", "stackexchange_historical", json!({}), None).unwrap();
        let site = Site { domain: "example.stackexchange.com", dump: "synthetic", dump_sha256: None, dump_url: None };
        adapt(xml.as_bytes(), &site, &users, &mut run).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["candidates"], 2);
        assert_eq!(rep["rejection_reasons"]["edited_not_before_2022_11_30"], 1);
        assert_eq!(rep["rejection_reasons"]["community_wiki_multiple_authors"], 1);
        assert_eq!(rep["rejection_reasons"]["no_owner_user"], 1);
        let rows: Vec<Value> = crate::io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert!(rows.iter().all(|r| r["source_group"] == "stackexchange:example.stackexchange.com:question:1"));
        assert_eq!(rows[0]["writer_key"], "stackexchange:example.stackexchange.com:user:5");
        assert_eq!(rows[0]["provenance"]["owner_display_name"], "Synthetic A");
        assert_eq!(rows[0]["rights"]["license"], "CC-BY-SA-3.0");
        assert_eq!(rows[0]["word_count"], 222);
    }

    #[test]
    fn metadata_only_lists_reserved_threads_without_rendering_and_selection_filters() {
        let rows = [
            format!(r#"<row Id="1" PostTypeId="1" CreationDate="2012-01-01T00:00:00.000" OwnerUserId="5" LastEditorUserId="8" Body="{}" />"#, esc(&body(110))),
            format!(r#"<row Id="2" PostTypeId="2" ParentId="1" CreationDate="2012-01-02T00:00:00.000" OwnerUserId="6" Body="{}" />"#, esc(&body(120))),
            format!(r#"<row Id="9" PostTypeId="1" CreationDate="2012-01-01T00:00:00.000" OwnerUserId="7" Body="{}" />"#, esc(&body(120))),
        ];
        let xml = format!("<posts>\n{}\n</posts>", rows.join("\n"));
        let site = Site { domain: "example.stackexchange.com", dump: "synthetic", dump_sha256: None, dump_url: None };
        let d = tempfile::tempdir().unwrap();
        // Metadata only over every bucket: three rows, no candidate, no text.
        let out = d.path().join("meta");
        let mut run = Run::start_sampled(&out, "stackexchange", "stackexchange_historical", json!({}), None, Some((0, 1000))).unwrap();
        run.enable_metadata_only().unwrap();
        adapt(xml.as_bytes(), &site, &BTreeMap::new(), &mut run).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["candidates"], 0);
        assert_eq!(rep["rejections"], 0);
        assert_eq!(rep["metadata_only"]["rows"], 3);
        let raw = std::fs::read_to_string(out.join("reserved-metadata.jsonl")).unwrap();
        assert!(!raw.contains("alpha") && !raw.contains("beta"), "metadata rows must not carry prose");
        let meta: Vec<Value> = crate::io::jsonl_rows(&out.join("reserved-metadata.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert_eq!(meta[0]["family_key"], "stackexchange:example.stackexchange.com:question:1");
        assert_eq!(meta[0]["last_editor_user_id"], "8");
        assert_eq!(meta[1]["thread_id"], "1");
        assert_eq!(meta[1]["post_type"], "answer");
        assert_eq!(meta[2]["bucket"], crate::adapter::bucket("stackexchange:example.stackexchange.com:question:9"));
        assert!(meta.iter().all(|m| m.get("text").is_none() && m["prose_opened"] == false));
        // A family selection renders only the listed thread.
        let out2 = d.path().join("sel");
        let mut run = Run::start(&out2, "stackexchange", "stackexchange_historical", json!({}), None).unwrap();
        run.select_families(["stackexchange:example.stackexchange.com:question:9".to_owned()].into_iter().collect());
        adapt(xml.as_bytes(), &site, &BTreeMap::new(), &mut run).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["candidates"], 1);
        assert_eq!(rep["counted_without_ledger_rows"]["family_not_in_selection"], 2);
        assert_eq!(rep["family_selection_size"], 1);
    }
}
