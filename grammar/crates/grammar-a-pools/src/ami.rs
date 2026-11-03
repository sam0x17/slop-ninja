//! AMI Meeting Corpus adapter over the manual annotations release
//! (`ami_public_manual_1.6.2.zip`, NXT XML).
//!
//! * **Rights.** CC BY 4.0 for the corpus and its annotations (licence page
//!   and the archive's `LICENCE.txt`). Train, Train-mining and evaluation.
//! * **Human transcription.** The manual orthographic transcription
//!   (`words/`), produced by human transcribers; word timings come from
//!   forced alignment and are not used for text. The automatic (ASR)
//!   annotation release is never read.
//! * **Turns.** Each speaker's `segments/` file points at word ranges of its
//!   `words/` file; segments of all speakers are ordered by
//!   `transcriber_start` and consecutive segments of one speaker form a turn.
//! * **Normalisation** (`NORMALIZATION`). `vocalsound` and `disfmarker`
//!   elements are removed and counted; `gap` and `transformerror` make the
//!   segment a break; punctuation tokens attach to the preceding word;
//!   truncated words (`trunc="true"`) end with `-`; letter-by-letter
//!   acronyms (`X_M_L`) are written without underscores.
//! * **Groups.** Family and unit: one meeting (`ami_meeting:<id>`). Bucket
//!   and split group: the connected component of meetings that share a
//!   participant (`global_name`), keyed by its smallest participant id, so no
//!   participant spans splits.
use crate::{
    adapter::Run,
    record::Doc,
    spoken::{self, Chunk, Removed, Use},
    text::{self, Excerpt},
    xmldoc,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::Read,
    path::Path,
};

pub const COLLECTION: &str = "ami_meeting_corpus_manual_transcripts";
pub const LICENSE: &str = "CC-BY-4.0";
pub const NORMALIZATION: &str = "ami_nxt_words_v1:drop_vocalsound_disfmarker;break_gap_transformerror;attach_punctuation;trunc_hyphen;letter_acronym_underscores_joined";

pub fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

/// `X_M_L` -> `XML`; mixed forms (`X_ray`) -> `X-ray`.
pub fn underscores(w: &str) -> (String, bool) {
    if !w.contains('_') {
        return (w.to_owned(), false);
    }
    let parts: Vec<&str> = w.split('_').filter(|p| !p.is_empty()).collect();
    if parts.iter().all(|p| p.chars().filter(|c| c.is_alphanumeric()).count() <= 1 || p.len() <= 2 && p.ends_with('s')) {
        (parts.concat(), true)
    } else {
        (parts.join("-"), true)
    }
}

pub struct Meeting {
    pub id: String,
    pub kind: String,
    pub date: Option<String>,
    /// nxt agent letter -> global participant id
    pub agents: BTreeMap<String, String>,
}

/// `meetings.xml` -> meetings.
pub fn meetings(xml: &str) -> Result<Vec<Meeting>> {
    let prepared = xmldoc::prepare(xml);
    let doc = xmldoc::document(&prepared)?;
    let mut out = Vec::new();
    for m in doc.descendants().filter(|n| n.has_tag_name("meeting")) {
        let id = m.attribute("observation").unwrap_or("").to_owned();
        let date = m.attribute("dateOnly").and_then(|d| {
            let p: Vec<&str> = d.split('-').collect();
            // dd-mm-yyyy, or dd-mm-yy for recordings of 2004-2006
            let y = match p.get(2).map(|y| y.len()) {
                Some(4) => p[2].to_owned(),
                Some(2) => format!("20{}", p[2]),
                _ => return None,
            };
            (p.len() == 3).then(|| format!("{y}-{:0>2}-{:0>2}", p[1], p[0])).filter(|d| crate::record::valid_date(d))
        });
        let agents = m
            .children()
            .filter(|n| n.has_tag_name("speaker"))
            .filter_map(|s| Some((s.attribute("nxt_agent")?.to_owned(), s.attribute("global_name")?.to_owned())))
            .collect();
        out.push(Meeting { id, kind: m.attribute("type").unwrap_or("").to_owned(), date, agents });
    }
    Ok(out)
}

/// Group key per meeting: component of meetings sharing a participant.
pub fn groups(ms: &[Meeting]) -> HashMap<String, String> {
    let mut parent: HashMap<String, String> = HashMap::new();
    fn find(p: &mut HashMap<String, String>, x: &str) -> String {
        let mut r = x.to_owned();
        while let Some(n) = p.get(&r).filter(|n| **n != r) {
            r = n.clone();
        }
        p.insert(x.to_owned(), r.clone());
        r
    }
    for m in ms {
        let ps: Vec<&String> = m.agents.values().collect();
        for p in &ps {
            parent.entry((*p).clone()).or_insert_with(|| (*p).clone());
        }
        for w in ps.windows(2) {
            let (a, b) = (find(&mut parent, w[0]), find(&mut parent, w[1]));
            if a != b {
                let (lo, hi) = if a < b { (a, b) } else { (b, a) };
                parent.insert(hi, lo);
            }
        }
    }
    let mut members: HashMap<String, BTreeSet<String>> = HashMap::new();
    for m in ms {
        if let Some(p) = m.agents.values().next() {
            let r = find(&mut parent, p);
            members.entry(r).or_default().extend(m.agents.values().cloned());
        }
    }
    let mut out = HashMap::new();
    for m in ms {
        if let Some(p) = m.agents.values().next() {
            let r = find(&mut parent, p);
            let min = members[&r].iter().next().cloned().unwrap_or_default();
            out.insert(m.id.clone(), format!("ami_participant_component:{min}"));
        }
    }
    out
}

/// One speaker's word elements in file order: (nite id, element, text, attrs).
pub struct Words {
    pub ids: HashMap<String, usize>,
    pub items: Vec<(String, String, bool, bool)>, // (tag, text, punc, trunc)
}

pub fn words(xml: &str) -> Result<Words> {
    let prepared = xmldoc::prepare(xml);
    let doc = xmldoc::document(&prepared)?;
    let mut ids = HashMap::new();
    let mut items = Vec::new();
    for n in doc.root_element().children().filter(|n| n.is_element()) {
        let id = n.attributes().find(|a| a.name() == "id").map(|a| a.value().to_owned()).unwrap_or_default();
        ids.insert(id, items.len());
        items.push((
            n.tag_name().name().to_owned(),
            n.text().unwrap_or("").to_owned(),
            n.attribute("punc") == Some("true"),
            n.attribute("trunc") == Some("true"),
        ));
    }
    Ok(Words { ids, items })
}

/// Segments: (transcriber_start, first word id, last word id).
pub fn segments(xml: &str) -> Result<Vec<(f64, String, String)>> {
    let prepared = xmldoc::prepare(xml);
    let doc = xmldoc::document(&prepared)?;
    let mut out = Vec::new();
    for s in doc.descendants().filter(|n| n.has_tag_name("segment")) {
        let start: f64 = s.attribute("transcriber_start").and_then(|v| v.parse().ok()).unwrap_or(f64::NAN);
        for c in s.children().filter(|n| n.is_element()) {
            let Some(h) = c.attribute("href") else { continue };
            let Some((_, frag)) = h.split_once('#') else { continue };
            let ids: Vec<String> = frag.split("..").map(|p| p.trim().trim_start_matches("id(").trim_end_matches(')').to_owned()).collect();
            if let (Some(a), Some(b)) = (ids.first(), ids.last()) {
                out.push((start, a.clone(), b.clone()));
            }
        }
    }
    Ok(out)
}

/// Render word items `[a, b]` into a chunk.
pub fn render(items: &[(String, String, bool, bool)], speaker: &str) -> Chunk {
    let mut rm = Removed::default();
    let mut s = String::new();
    for (tag, t, punc, trunc) in items {
        match tag.as_str() {
            "w" => {
                let (w, u) = underscores(t.trim());
                if u {
                    rm.add("acronym_underscores");
                }
                if *punc {
                    s.push_str(&w);
                } else {
                    if !s.is_empty() {
                        s.push(' ');
                    }
                    s.push_str(&w);
                    if *trunc {
                        s.push('-');
                    }
                }
            }
            "vocalsound" => rm.add("vocalsound"),
            "disfmarker" => rm.add("disfmarker"),
            "gap" => return Chunk::brk(speaker, "gap_untranscribed_or_unintelligible"),
            "transformerror" => return Chunk::brk(speaker, "transform_error"),
            _ => return Chunk::brk(speaker, "unknown_transcription_element"),
        }
    }
    Chunk::speech(speaker, s, rm.take())
}

fn read_member(z: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<Option<Vec<u8>>> {
    match z.by_name(name) {
        Ok(mut e) => {
            let mut b = Vec::new();
            e.read_to_end(&mut b)?;
            Ok(Some(b))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn adapt(zip_path: &Path, run: &mut Run) -> Result<Value> {
    let (zsha, zbytes) = crate::io::file_sha(zip_path)?;
    run.input(json!({"role": "ami_manual_annotations_zip", "path": zip_path, "sha256": zsha, "bytes": zbytes}));
    let mut z = zip::ZipArchive::new(std::fs::File::open(zip_path)?).context("AMI zip")?;
    let mxml = latin1(&read_member(&mut z, "corpusResources/meetings.xml")?.context("meetings.xml")?);
    let ms = meetings(&mxml)?;
    let groups = groups(&ms);
    let method = format!("ami_nxt_segments_ordered_by_transcriber_start;{};{};{}", spoken::RENDERING, spoken::RUN_METHOD, NORMALIZATION);
    let mut breaks: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut removed_total: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut by_kind: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut group_sizes: BTreeMap<String, u64> = BTreeMap::new();
    for g in groups.values() {
        *group_sizes.entry(g.clone()).or_default() += 1;
    }
    for m in &ms {
        run.units_seen += 1;
        if run.full() {
            break;
        }
        let unit = format!("ami_meeting:{}", m.id);
        let Some(group) = groups.get(&m.id).cloned() else {
            run.reject(&unit, "no_participants")?;
            continue;
        };
        let meta = || json!({"meeting": m.id, "type": m.kind, "date": m.date, "participants": m.agents.values().collect::<Vec<_>>(), "group_key": group});
        if !run.sampled_meta_grouped(&group, &unit, meta)? {
            continue;
        }
        let Some(date) = m.date.clone() else {
            run.reject(&unit, "no_meeting_date")?;
            continue;
        };
        // Collect segments of every agent.
        let mut chunks: Vec<(f64, usize, Chunk)> = Vec::new();
        let mut missing = false;
        for (agent, global) in &m.agents {
            let wname = format!("words/{}.{agent}.words.xml", m.id);
            let sname = format!("segments/{}.{agent}.segments.xml", m.id);
            let (Some(wb), Some(sb)) = (read_member(&mut z, &wname)?, read_member(&mut z, &sname)?) else {
                missing = true;
                continue;
            };
            let w = words(&latin1(&wb)).with_context(|| wname.clone())?;
            for (k, (start, a, b)) in segments(&latin1(&sb)).with_context(|| sname.clone())?.into_iter().enumerate() {
                let (Some(&ia), Some(&ib)) = (w.ids.get(&a), w.ids.get(&b)) else {
                    chunks.push((start, k, Chunk::brk(format!("ami_participant:{global}"), "segment_word_range_unresolved")));
                    continue;
                };
                let (lo, hi) = (ia.min(ib), ia.max(ib));
                chunks.push((start, k, render(&w.items[lo..=hi], &format!("ami_participant:{global}"))));
            }
        }
        if chunks.is_empty() {
            run.reject(&unit, if missing { "no_word_files" } else { "no_segments" })?;
            continue;
        }
        chunks.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.2.speaker.cmp(&y.2.speaker)).then(x.1.cmp(&y.1)));
        let turns = spoken::turns(chunks.into_iter().map(|c| c.2).collect());
        for t in &turns {
            if let Some(r) = t.brk {
                *breaks.entry(r).or_default() += 1;
            }
        }
        let wins = spoken::windows(&turns, 0, None);
        by_kind.entry(m.kind.clone()).or_default().0 += 1;
        if wins.is_empty() {
            run.reject(&unit, "no_turn_run_within_bounds")?;
            continue;
        }
        let mut kept = 0;
        for w in &wins {
            let wid = format!("{unit}:w{}", w.index);
            if let Some(r) = text::english_screen(&w.text) {
                run.reject(&wid, r)?;
                continue;
            }
            for (k, v) in &w.removed {
                *removed_total.entry(k).or_default() += *v as u64;
            }
            let ex = Excerpt { text: w.text.clone(), start: w.start, end: w.end, words: w.words, complete_unit: w.complete_unit };
            let doc = Doc {
                id: format!("pool-ami-{}-w{}", m.id, w.index),
                collection: COLLECTION,
                domain: "spoken_meeting",
                source_group: unit.clone(),
                writer_key: Some(group.clone()),
                evidence: "historical_proxy",
                excerpt: &ex,
                extraction_method: &method,
                unit: json!({"kind": "meeting_transcript_turn_run", "span_basis": "merged_turn_index_half_open", "meeting": m.id, "window_index": w.index,
                             "turns": w.end - w.start, "speakers": w.labels.iter().map(|(l, s)| json!({"label": l, "speaker_key": s})).collect::<Vec<_>>(),
                             "normalization": spoken::normalization(w, NORMALIZATION), "rendering": spoken::RENDERING,
                             "short_form_unit": "speaker turns rendered as paragraphs (closest faithful unit for multi-party speech)"}),
                provenance: json!({"meeting": m.id, "meeting_type": m.kind, "group_key": group, "group_meetings": group_sizes.get(&group),
                                   "transcription": "AMI manual orthographic transcription (human); forced-alignment timings unused",
                                   "archive": "ami_public_manual_1.6.2.zip", "archive_sha256": zsha,
                                   "source_url": "https://groups.inf.ed.ac.uk/ami/download/", "speech_mode": if m.kind == "scenario" { "role_played_design_scenario_spontaneous_speech" } else { "natural_meeting_spontaneous_speech" }}),
                date: Some(date.clone()),
                date_basis: "ami_meetings_xml_dateOnly_recording_date;two_digit_years_read_as_20yy",
                cutoff_policy: "dated_before_cutoff",
                rights: spoken::rights(LICENSE, "https://creativecommons.org/licenses/by/4.0/",
                    format!("AMI Meeting Corpus, meeting {}; AMI Consortium; CC BY 4.0; manual transcription rendered as speaker turns, non-lexical markup removed", m.id),
                    "https://groups.inf.ed.ac.uk/ami/corpus/license.shtml",
                    "CC BY 4.0 (licence page and LICENCE.txt in the annotation archive; read 2026-10-27)",
                    Use::Any, "spoken_ami_v1", "Two thirds of AMI meetings are role-played design scenarios; speech is spontaneous; many participants are non-native speakers."),
                aliases: vec![],
            };
            if run.accept(&wid, &doc)? {
                kept += 1;
            }
        }
        if kept > 0 {
            by_kind.entry(m.kind.clone()).or_default().1 += 1;
        }
    }
    Ok(json!({"meetings": ms.len(), "participant_components": group_sizes.len(), "largest_component_meetings": group_sizes.values().max(),
              "turn_breaks": breaks, "removed_markup_in_candidates": removed_total,
              "meetings_by_type_screened_with_candidate": by_kind.into_iter().map(|(k, (a, b))| (k, json!({"screened": a, "with_candidate": b}))).collect::<serde_json::Map<_, _>>(),
              "normalization": NORMALIZATION,
              "limits": ["scenario meetings are role play", "turn order follows transcriber segment starts; overlapping speech is serialised",
                         "non-native English is common (participants.xml records native language; not used for selection)"]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_words_markup_and_breaks() {
        let xml = r#"<?xml version="1.0" encoding="ISO-8859-1"?><nite:root xmlns:nite="http://nite.sourceforge.net/">
<w nite:id="m.A.words0">So</w><vocalsound nite:id="m.A.words1" type="laugh"/><w nite:id="m.A.words2" trunc="true">th</w>
<w nite:id="m.A.words3">the</w><w nite:id="m.A.words4">X_M_L</w><w nite:id="m.A.words5">file</w><disfmarker nite:id="m.A.words6"/>
<w nite:id="m.A.words7" punc="true">,</w><w nite:id="m.A.words8">don&#39;t</w><w nite:id="m.A.words9" punc="true">.</w>
<gap nite:id="m.A.words10"/></nite:root>"#;
        let w = words(xml).unwrap();
        let c = render(&w.items[0..10], "p");
        assert_eq!(c.text, "So th- the XML file, don't.");
        assert_eq!(c.removed.get("vocalsound"), Some(&1));
        assert_eq!(c.removed.get("disfmarker"), Some(&1));
        assert_eq!(render(&w.items[8..11], "p").brk, Some("gap_untranscribed_or_unintelligible"));
        assert_eq!(underscores("X_ray").0, "X-ray");
        assert_eq!(underscores("T_V_s").0, "TVs");
        let s = r#"<nite:root xmlns:nite="http://nite.sourceforge.net/"><segment nite:id="s1" transcriber_start="1.5"><nite:child href="m.A.words.xml#id(m.A.words0)..id(m.A.words5)"/></segment><segment nite:id="s2" transcriber_start="3"><nite:child href="m.A.words.xml#id(m.A.words8)"/></segment></nite:root>"#;
        let seg = segments(s).unwrap();
        assert_eq!(seg[0], (1.5, "m.A.words0".to_owned(), "m.A.words5".to_owned()));
        assert_eq!(seg[1].1, seg[1].2);
    }
    #[test]
    fn meetings_dates_and_participant_components() {
        let xml = r#"<nite:root xmlns:nite="http://nite.sourceforge.net/">
<meeting observation="X1a" type="scenario" dateOnly="9-3-2005"><speaker nxt_agent="A" global_name="P2"/><speaker nxt_agent="B" global_name="P1"/></meeting>
<meeting observation="X1b" type="scenario" dateOnly="10-03-05"><speaker nxt_agent="A" global_name="P1"/><speaker nxt_agent="B" global_name="P3"/></meeting>
<meeting observation="Y1" type="non-scenario" dateOnly="1-1-2006"><speaker nxt_agent="A" global_name="Q9"/></meeting></nite:root>"#;
        let ms = meetings(xml).unwrap();
        assert_eq!(ms[0].date.as_deref(), Some("2005-03-09"));
        assert_eq!(ms[1].date.as_deref(), Some("2005-03-10"));
        let g = groups(&ms);
        assert_eq!(g["X1a"], "ami_participant_component:P1");
        assert_eq!(g["X1b"], g["X1a"]);
        assert_eq!(g["Y1"], "ami_participant_component:Q9");
    }
}
