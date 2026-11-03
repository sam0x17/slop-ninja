//! ICSI Meeting Corpus adapter over the original MRT transcripts
//! (`ICSI_original_transcripts.zip`, `transcripts/*.mrt`).
//!
//! * **Rights.** CC BY 4.0 for the corpus and its annotations (licence
//!   page). Train, Train-mining and evaluation.
//! * **Human transcription.** The ICSI hand transcription (MRT XML).
//! * **Normalisation** (`NORMALIZATION`). Segments without a participant
//!   (non-speech) are skipped; `DigitTask` segments (read digit strings)
//!   break; `Pause`, `Comment`, `VocalSound` and `NonVocalSound` are removed
//!   and counted; `Emphasis` and `Pronounce` keep their text (the canonical
//!   form); `Uncertain` keeps the transcriber's best guess unless it is the
//!   unintelligible symbol `@@` (break); `Foreign` breaks; spelled acronyms
//!   (`I_B_M`) are written without underscores.
//! * **Groups.** Family and unit: one meeting (`icsi_meeting:<session>`).
//!   Bucket and split group: the component of meetings sharing a
//!   participant. ICSI's participants recur across meeting series, so the
//!   whole corpus is one component and one bucket (recorded).
use crate::{
    adapter::Run,
    ami::{latin1, underscores},
    record::Doc,
    spoken::{self, Chunk, Removed, Use},
    text::{self, Excerpt},
    xmldoc,
};
use anyhow::{Context, Result};
use roxmltree::Node;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::Read,
    path::Path,
};

pub const COLLECTION: &str = "icsi_meeting_corpus_transcripts";
pub const LICENSE: &str = "CC-BY-4.0";
pub const NORMALIZATION: &str = "icsi_mrt_v1:skip_participantless;break_digit_task_foreign_unintelligible;drop_pause_comment_vocalsound_nonvocalsound;keep_emphasis_pronounce_uncertain_text;letter_acronym_underscores_joined";

pub struct Meeting {
    pub session: String,
    pub date: Option<String>,
    pub participants: Vec<String>,
    pub chunks: Vec<Chunk>,
}

fn walk(n: Node, out: &mut String, rm: &mut Removed, brk: &mut Option<&'static str>) {
    for c in n.children() {
        if c.is_text() {
            out.push_str(c.text().unwrap_or(""));
            out.push(' ');
            continue;
        }
        if !c.is_element() {
            continue;
        }
        match c.tag_name().name() {
            "Emphasis" => {
                rm.add("emphasis_tag");
                walk(c, out, rm, brk)
            }
            "Pronounce" => {
                rm.add("pronounce_tag");
                walk(c, out, rm, brk)
            }
            "Uncertain" => {
                let mut inner = String::new();
                walk(c, &mut inner, rm, brk);
                if inner.contains("@@") || inner.trim().is_empty() {
                    *brk = brk.or(Some("unintelligible"));
                } else {
                    rm.add("uncertain_tag");
                    out.push_str(&inner);
                }
            }
            "Foreign" => *brk = brk.or(Some("foreign_language")),
            "Pause" => rm.add("pause"),
            "Comment" => rm.add("transcriber_comment"),
            "VocalSound" => rm.add("vocalsound"),
            "NonVocalSound" => rm.add("nonvocalsound"),
            _ => *brk = brk.or(Some("unknown_transcription_element")),
        }
    }
}

/// Render one segment's content.
pub fn segment(seg: Node, speaker: &str) -> Chunk {
    if seg.attribute("DigitTask") == Some("true") {
        return Chunk::brk(speaker, "digit_task_read_speech");
    }
    let mut rm = Removed::default();
    let mut s = String::new();
    let mut brk = None;
    walk(seg, &mut s, &mut rm, &mut brk);
    if let Some(r) = brk {
        return Chunk::brk(speaker, r);
    }
    if s.contains("@@") {
        return Chunk::brk(speaker, "unintelligible");
    }
    let mut w = Vec::new();
    for t in s.split_whitespace() {
        let (x, u) = underscores(t);
        if u {
            rm.add("acronym_underscores");
        }
        w.push(x);
    }
    Chunk::speech(speaker, w.join(" "), rm.take())
}

pub fn meeting(mrt: &str) -> Result<Meeting> {
    let prepared = xmldoc::prepare(mrt);
    let doc = xmldoc::document(&prepared)?;
    let root = doc.root_element();
    let session = if root.has_tag_name("Meeting") { root.attribute("Session").unwrap_or("").to_owned() } else { String::new() };
    let date = root.attribute("DateTimeStamp").and_then(|d| (d.len() >= 10 && crate::record::valid_date(&d[..10])).then(|| d[..10].to_owned()));
    let participants = doc
        .descendants()
        .filter(|n| n.has_tag_name("Participant") && n.parent().is_some_and(|p| p.has_tag_name("Participants")))
        .filter_map(|n| n.attribute("Name").map(str::to_owned))
        .collect();
    let mut chunks = Vec::new();
    for seg in doc.descendants().filter(|n| n.has_tag_name("Segment")) {
        let Some(p) = seg.attribute("Participant") else { continue };
        chunks.push(segment(seg, &format!("icsi_participant:{p}")));
    }
    Ok(Meeting { session, date, participants, chunks })
}

/// Component key per session from shared participants.
pub fn groups(ms: &[(String, Vec<String>)]) -> HashMap<String, String> {
    let as_ami: Vec<crate::ami::Meeting> = ms
        .iter()
        .map(|(s, ps)| crate::ami::Meeting { id: s.clone(), kind: String::new(), date: None, agents: ps.iter().enumerate().map(|(i, p)| (format!("{i:03}"), p.clone())).collect() })
        .collect();
    crate::ami::groups(&as_ami).into_iter().map(|(k, v)| (k, v.replacen("ami_participant_component:", "icsi_participant_component:", 1))).collect()
}

pub fn adapt(zip_path: &Path, run: &mut Run) -> Result<Value> {
    let (zsha, zbytes) = crate::io::file_sha(zip_path)?;
    run.input(json!({"role": "icsi_original_mrt_transcripts_zip", "path": zip_path, "sha256": zsha, "bytes": zbytes}));
    let mut z = zip::ZipArchive::new(std::fs::File::open(zip_path)?).context("ICSI zip")?;
    let mut names: Vec<String> = z.file_names().filter(|n| n.starts_with("transcripts/") && n.ends_with(".mrt")).map(str::to_owned).collect();
    names.sort();
    let mut parsed = Vec::new();
    for n in &names {
        let mut b = Vec::new();
        z.by_name(n)?.read_to_end(&mut b)?;
        let m = meeting(&latin1(&b)).with_context(|| n.clone())?;
        if m.session.is_empty() {
            // preambles.mrt: concatenated preambles under <Meetings>, no transcript
            run.count("non_meeting_mrt_file");
            continue;
        }
        parsed.push((n.clone(), text::sha_hex(&b), m));
    }
    let groups = groups(&parsed.iter().map(|(_, _, m)| (m.session.clone(), m.participants.clone())).collect::<Vec<_>>());
    let comps: BTreeSet<&String> = groups.values().collect();
    let method = format!("icsi_mrt_segments_in_file_order;{};{};{}", spoken::RENDERING, spoken::RUN_METHOD, NORMALIZATION);
    let mut breaks: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut removed_total: BTreeMap<&'static str, u64> = BTreeMap::new();
    for (name, sha, m) in parsed {
        run.units_seen += 1;
        if run.full() {
            break;
        }
        let unit = format!("icsi_meeting:{}", m.session);
        let Some(group) = groups.get(&m.session).cloned() else {
            run.reject(&unit, "no_participants")?;
            continue;
        };
        let meta = || json!({"session": m.session, "date": m.date, "participants": m.participants, "file": name, "file_sha256": sha, "group_key": group});
        if !run.sampled_meta_grouped(&group, &unit, meta)? {
            continue;
        }
        let Some(date) = m.date.clone() else {
            run.reject(&unit, "no_meeting_date")?;
            continue;
        };
        let turns = spoken::turns(m.chunks);
        for t in &turns {
            if let Some(r) = t.brk {
                *breaks.entry(r).or_default() += 1;
            }
        }
        let wins = spoken::windows(&turns, 0, None);
        if wins.is_empty() {
            run.reject(&unit, "no_turn_run_within_bounds")?;
            continue;
        }
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
                id: format!("pool-icsi-{}-w{}", m.session, w.index),
                collection: COLLECTION,
                domain: "spoken_meeting",
                source_group: unit.clone(),
                writer_key: Some(group.clone()),
                evidence: "historical_proxy",
                excerpt: &ex,
                extraction_method: &method,
                unit: json!({"kind": "meeting_transcript_turn_run", "span_basis": "merged_turn_index_half_open", "session": m.session, "window_index": w.index,
                             "turns": w.end - w.start, "speakers": w.labels.iter().map(|(l, s)| json!({"label": l, "speaker_key": s})).collect::<Vec<_>>(),
                             "normalization": spoken::normalization(w, NORMALIZATION), "rendering": spoken::RENDERING,
                             "short_form_unit": "speaker turns rendered as paragraphs (closest faithful unit for multi-party speech)"}),
                provenance: json!({"session": m.session, "series": m.session.get(..3), "group_key": group, "file": name, "file_sha256": sha,
                                   "transcription": "ICSI hand transcription (MRT)", "archive": "ICSI_original_transcripts.zip", "archive_sha256": zsha,
                                   "source_url": "https://groups.inf.ed.ac.uk/ami/icsi/download/"}),
                date: Some(date.clone()),
                date_basis: "icsi_mrt_DateTimeStamp_recording_date",
                cutoff_policy: "dated_before_cutoff",
                rights: spoken::rights(LICENSE, "https://creativecommons.org/licenses/by/4.0/",
                    format!("ICSI Meeting Corpus, meeting {}; International Computer Science Institute; CC BY 4.0; transcript rendered as speaker turns, non-lexical markup removed", m.session),
                    "https://groups.inf.ed.ac.uk/ami/icsi/license.shtml",
                    "CC BY 4.0 (licence page; read 2026-10-27)",
                    Use::Any, "spoken_icsi_v1", "Natural research-group meetings; participants recur across meeting series."),
                aliases: vec![],
            };
            run.accept(&wid, &doc)?;
        }
    }
    Ok(json!({"files": names.len(), "participant_components": comps.len(), "turn_breaks": breaks, "removed_markup_in_candidates": removed_total,
              "normalization": NORMALIZATION,
              "limits": ["participants recur across all meeting series, so the corpus forms one participant component and one bucket",
                         "turn order follows segment order; overlapping speech is serialised"]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mrt_segments_normalise_and_break() {
        let mrt = r#"<?xml version="1.0" encoding="iso-8859-1"?>
<!DOCTYPE Meeting SYSTEM "http://example.invalid/Meeting1.dtd">
<Meeting Session="Bxx001" DateTimeStamp="2001-02-01-1500"><Preamble><Participants><Participant Name="me001"/><Participant Name="fe002"/></Participants></Preamble>
<Transcript>
<Segment StartTime="0" EndTime="1" Participant="me001">O_K. So, <Emphasis> which </Emphasis> tool <Pause/> do we <VocalSound Description="laugh"/> use?</Segment>
<Segment StartTime="1" EndTime="2"><NonVocalSound Description="door"/></Segment>
<Segment StartTime="2" EndTime="3" Participant="fe002"><Uncertain> I mean </Uncertain> the I_B_M one . <Comment Description="whispered"/></Segment>
<Segment StartTime="3" EndTime="4" Participant="fe002"><Uncertain NumSyllables="2"> @@ </Uncertain> yes.</Segment>
<Segment StartTime="4" EndTime="5" Participant="me001" DigitTask="true">one two three</Segment>
<Segment StartTime="5" EndTime="6" Participant="me001"><Foreign Language="de"> Nein. </Foreign></Segment>
</Transcript></Meeting>"#;
        let m = meeting(mrt).unwrap();
        assert_eq!(m.session, "Bxx001");
        assert_eq!(m.date.as_deref(), Some("2001-02-01"));
        assert_eq!(m.participants, ["me001", "fe002"]);
        assert_eq!(m.chunks.len(), 5);
        assert_eq!(m.chunks[0].text, "OK. So, which tool do we use?");
        assert_eq!(m.chunks[0].removed.get("pause"), Some(&1));
        assert_eq!(m.chunks[1].text, "I mean the IBM one .");
        assert_eq!(m.chunks[2].brk, Some("unintelligible"));
        assert_eq!(m.chunks[3].brk, Some("digit_task_read_speech"));
        assert_eq!(m.chunks[4].brk, Some("foreign_language"));
        let t = spoken::turns(m.chunks);
        assert_eq!(t[1].text, "I mean the IBM one.");
        let g = groups(&[("a".into(), vec!["x".into(), "y".into()]), ("b".into(), vec!["y".into()]), ("c".into(), vec!["z".into()])]);
        assert_eq!(g["a"], "icsi_participant_component:x");
        assert_eq!(g["b"], g["a"]);
        assert_eq!(g["c"], "icsi_participant_component:z");
    }
}
