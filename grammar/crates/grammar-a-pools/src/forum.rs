//! Informal-discussion human pools: Reddit posts from Webis-TLDR-17, Ubuntu
//! IRC channel logs and Wikipedia talk-page comments from WikiConv.
//!
//! * **Rights.** These sources are admitted under the classifier-training
//!   policy of 2026-10-27: lawful public access, the licence or
//!   terms recorded per row, exact text in ignored storage, no
//!   redistribution, one removable cohort per source. The admitted and
//!   excluded sources are listed in `docs/a-competitive-protocol.md`.
//! * **Groups.** The source family is one post, comment, utterance or
//!   speaker-day. The bucket, split group and `writer_key` is an opaque
//!   contributor key (`<prefix>:<20 hex>` of a domain-separated SHA-256 of
//!   the account name, or of channel and nick for IRC). Raw account names are
//!   never written to metadata.
//! * **Paragraphs.** The shared screens apply, relaxed for informal writing:
//!   no eight-word minimum and no sentence-final punctuation requirement.
//!   Quoted replies (`>`), URLs, residual markup, lists, code and long
//!   quotations still break runs. Excerpts are whole paragraphs (whole
//!   messages for IRC) in 150..=450 whitespace words.
//! * **Shortcut control.** Platform boilerplate is removed or breaks runs:
//!   TL;DR summary paragraphs (every Webis post has one by construction),
//!   Wikipedia signatures (stripped from the final paragraph and counted,
//!   otherwise a break), IRC timestamps and nick prefixes (not part of the
//!   message text), bot accounts.
use crate::{
    adapter::Run,
    io,
    record::{self, Doc},
    text::{self, Excerpt, Para, Screen},
    wikitext,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

pub const ADMISSION_BASIS: &str = "classifier-training use, user decision 2026-10-27";
pub const CONTRIBUTOR_DOMAIN: &str = "slop_ninja_a_forums_contributor_v1";

pub const REDDIT_COLLECTION: &str = "reddit_webis_tldr17_historical";
pub const IRC_COLLECTION: &str = "ubuntu_irc_historical";
pub const WIKI_TALK_COLLECTION: &str = "wikipedia_talk_wikiconv_historical";

/// Webis-TLDR-17 compilation (CC BY 4.0); post text is Reddit user content.
pub const WEBIS_LICENSE: &str = "CC-BY-4.0-compilation+reddit-user-content";
/// Ubuntu IRC logs, stated to be public domain by the Common Pile release.
pub const IRC_LICENSE: &str = "public-domain-stated-ubuntu-irc-logs";
/// Wikipedia talk comments (CC BY-SA 3.0); the WikiConv compilation is CC0.
pub const WIKI_TALK_LICENSE: &str = "CC-BY-SA-3.0";

pub const INFORMAL_METHOD: &str = "informal_screen_v1:no_minimum_words_no_sentence_end_requirement;quoted_reply_url_markup_non_ascii_long_quotation_break";

/// Opaque contributor key: never the raw account name.
pub fn contributor_key(prefix: &str, raw: &str) -> String {
    let h = text::sha_hex(format!("{CONTRIBUTOR_DOMAIN}\0{prefix}\0{raw}").as_bytes());
    format!("{prefix}:{}", &h[..20])
}

/// Prose screen for informal paragraphs and messages.
pub fn informal_reason(t: &str, screen: Screen) -> Option<&'static str> {
    if t.trim().is_empty() {
        return Some("empty");
    }
    if t.contains(text::GAP) {
        return Some("removed_inline_content");
    }
    if let Some(r) = text::damaged(t) {
        return Some(r);
    }
    if t.trim_start().starts_with('>') {
        return Some("quoted_reply");
    }
    if text::has_url(t) {
        return Some("url");
    }
    for m in ["{{", "}}", "[[", "]]", "{|", "|}", "''", "__"] {
        if t.contains(m) {
            return Some("residual_markup");
        }
    }
    if screen.angle_brackets && (t.contains('<') || t.contains('>')) {
        return Some("residual_markup");
    }
    if !t.chars().any(char::is_alphabetic) {
        return Some("no_letters");
    }
    if text::ascii_letter_ratio(t) < 0.9 {
        return Some("non_ascii_letters");
    }
    if screen.quotes && text::long_quotation(t) {
        return Some("long_quotation");
    }
    None
}

pub fn informal(t: String, screen: Screen) -> Para {
    let reason = informal_reason(&t, screen);
    Para { prose: reason.is_none(), reason, text: t }
}

/// Re-screen paragraphs classified by a shared renderer: prose paragraphs
/// and those rejected only for length or a missing sentence end are
/// screened again with the informal rule; structural breaks stay.
pub fn rescreen(paras: Vec<Para>, screen: Screen) -> Vec<Para> {
    paras
        .into_iter()
        .map(|p| match p.reason {
            None | Some("short_paragraph") | Some("no_sentence_end") => informal(p.text, screen),
            _ => p,
        })
        .collect()
}

/// Quotation that spans paragraphs: a paragraph with an unbalanced quote
/// (an odd number of straight double quotes, or unequal curly opening and
/// closing quotes) opens a quotation that runs until the paragraph that
/// closes it; every paragraph inside it, both ends included, becomes a
/// `multi_paragraph_quotation` break. Quoted spans inside one paragraph are
/// handled by the long-quotation screen.
pub fn mark_quote_spans(paras: Vec<Para>) -> Vec<Para> {
    let mut open = false;
    paras
        .into_iter()
        .map(|p| {
            let unbalanced = p.text.matches('"').count() % 2 == 1 || p.text.matches('“').count() != p.text.matches('”').count();
            let inside = open || unbalanced;
            if unbalanced {
                open = !open;
            }
            if inside && p.prose { Para::non_prose(p.text, "multi_paragraph_quotation") } else { p }
        })
        .collect()
}

/// `text::paragraph_run` with a chosen joiner (IRC messages join by one
/// newline). With `"\n\n"` it is identical to `paragraph_run`.
pub fn paragraph_run_joined(paras: &[Para], joiner: &str) -> Option<Excerpt> {
    let all_prose = paras.iter().all(|p| p.prose);
    let mut i = 0;
    while i < paras.len() {
        if !paras[i].prose {
            i += 1;
            continue;
        }
        let (mut n, mut j) = (0, i);
        while j < paras.len() && paras[j].prose {
            n += text::words(&paras[j].text);
            j += 1;
            if n >= text::TARGET_WORDS {
                break;
            }
        }
        let run_ended = j == paras.len() || !paras[j].prose;
        if (text::MIN_WORDS..=text::MAX_WORDS).contains(&n) && (n >= text::TARGET_WORDS || run_ended) {
            let t = paras[i..j].iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(joiner);
            return Some(Excerpt { words: text::words(&t), text: t, start: i, end: j, complete_unit: all_prose && i == 0 && j == paras.len() });
        }
        i = if n > text::MAX_WORDS { i + 1 } else { j };
    }
    None
}

fn tally(breaks: &mut BTreeMap<&'static str, u64>, paras: &[Para]) {
    for p in paras {
        if let Some(r) = p.reason {
            *breaks.entry(r).or_default() += 1;
        }
    }
}

pub fn unix_date(secs: i64) -> String {
    io::utc_from_unix(secs)
}

fn forum_rights(license: &str, license_url: Option<&str>, attribution: String, evidence_url: &str, cohort: &str, evaluation_use: &str, notes: &str) -> Value {
    let mut r = record::rights(license, attribution, evidence_url, notes);
    if let Some(u) = license_url {
        r["license_url"] = json!(u);
    }
    r["admission_decision"] = json!(ADMISSION_BASIS);
    r["admission_basis"] = json!(ADMISSION_BASIS);
    r["cohort_id"] = json!(cohort);
    r["removable_cohort"] = json!(true);
    r["redistribute_text"] = json!(false);
    r["evaluation_use"] = json!(evaluation_use);
    if evaluation_use == "train_only" {
        r["external_evaluation"] = json!(false);
    }
    r
}

// ---------------------------------------------------------------- Reddit

fn list_marker(l: &str) -> bool {
    if l.starts_with("* ") || l.starts_with("- ") || l.starts_with("+ ") {
        return true;
    }
    let d = l.bytes().take_while(u8::is_ascii_digit).count();
    d > 0 && d <= 3 && (l[d..].starts_with(". ") || l[d..].starts_with(") "))
}

fn rule_line(l: &str) -> bool {
    let t: String = l.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3 && (t.chars().all(|c| c == '*') || t.chars().all(|c| c == '-') || t.chars().all(|c| c == '_'))
}

fn table_rule(l: &str) -> bool {
    let t = l.trim();
    t.contains('-') && t.contains('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// Replace `[text](target)` with `text`.
fn md_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("](") {
        let before = &rest[..i];
        let after = &rest[i + 2..];
        match (before.rfind('['), after.find(')')) {
            (Some(o), Some(c)) => {
                out.push_str(&before[..o]);
                out.push_str(&before[o + 1..]);
                rest = &after[c + 1..];
            }
            _ => {
                out.push_str(&rest[..i + 2]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Render Reddit markdown inline markup conservatively: link text is kept,
/// balanced emphasis and inline-code markers are removed, and
/// strikethrough, superscript or unbalanced markers leave a gap.
pub fn md_inline(s: &str) -> String {
    const ESC: [(&str, char); 6] = [("\\*", '\u{E010}'), ("\\_", '\u{E011}'), ("\\`", '\u{E012}'), ("\\^", '\u{E013}'), ("\\~", '\u{E014}'), ("\\#", '#')];
    let mut t = s.to_owned();
    for (e, c) in ESC {
        t = t.replace(e, &c.to_string());
    }
    t = md_links(&t);
    if t.contains("~~") || t.contains('^') {
        t.push(text::GAP);
    }
    for m in ["***", "**", "__", "*", "`"] {
        let n = t.matches(m).count();
        if n > 0 {
            if n % 2 == 0 {
                t = t.replace(m, "");
            } else {
                t.push(text::GAP);
            }
        }
    }
    for (e, c) in ESC {
        t = t.replace(c, &e[1..]);
    }
    t
}

pub fn tldr(t: &str) -> bool {
    let f = text::folded(t);
    f.contains("tl dr") || f.split(' ').any(|w| w == "tldr" || w == "tldr's")
}

fn md_block(lines: &[&str]) -> Para {
    let first = lines[0].trim_start();
    let joined = text::squash(&lines.join(" "));
    if lines.iter().any(|l| l.trim_start().starts_with('>')) {
        return Para::non_prose(joined, "quotation");
    }
    if lines.iter().any(|l| l.starts_with("    ") || l.starts_with('\t') || l.contains("```")) {
        return Para::non_prose(joined, "code_block");
    }
    if first.starts_with('#') {
        return Para::non_prose(joined, "heading");
    }
    if lines.iter().any(|l| list_marker(l.trim_start())) {
        return Para::non_prose(joined, "list_item");
    }
    if lines.iter().any(|l| table_rule(l)) {
        return Para::non_prose(joined, "table");
    }
    if lines.iter().any(|l| rule_line(l)) {
        return Para::non_prose(joined, "rule");
    }
    let p = informal(md_inline(&joined), Screen::XML);
    if tldr(&p.text) {
        return Para::non_prose(p.text, "tldr_summary");
    }
    p
}

/// Reddit markdown body -> paragraphs (blank-line separated blocks; single
/// newlines join, as Reddit renders them).
pub fn reddit_paragraphs(body: &str) -> Vec<Para> {
    let body = wikitext::decode_entities(body);
    let mut out = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    for line in body.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if !block.is_empty() {
                out.push(md_block(&block));
                block.clear();
            }
        } else {
            block.push(line);
        }
    }
    out
}

pub fn bot_account(name: &str) -> bool {
    let l = name.to_lowercase();
    l == "automoderator" || l.ends_with("bot") || l.ends_with("_bot") || l.contains("bot_") || l.contains("-bot")
}

/// Webis-TLDR-17 zip (JSON lines). Every post predates the corpus
/// publication (2017-11-07); the posts come from the 2006-2016 Reddit dump.
pub fn webis(zip_path: &Path, zip_sha: Option<&str>, run: &mut Run) -> Result<Value> {
    let f = std::fs::File::open(zip_path).with_context(|| format!("open {}", zip_path.display()))?;
    let mut z = zip::ZipArchive::new(f)?;
    let mut names: Vec<String> = (0..z.len()).filter_map(|i| z.by_index(i).ok().map(|e| e.name().to_owned())).filter(|n| n.ends_with(".json") || n.ends_with(".jsonl")).collect();
    names.sort();
    anyhow::ensure!(!names.is_empty(), "no .json entries in {}", zip_path.display());
    let method = format!("reddit_markdown_blocks_v1;tldr_paragraph_break;{INFORMAL_METHOD};{}", text::PARAGRAPH_RUN_METHOD);
    let mut breaks = BTreeMap::new();
    let mut entries = Vec::new();
    let mut by_sub: BTreeMap<String, u64> = BTreeMap::new();
    for name in &names {
        let e = z.by_name(name)?;
        let mut r = io::HashingReader::new(e);
        let mut rows = 0u64;
        {
            let br = BufReader::with_capacity(1 << 20, &mut r);
            for line in br.lines() {
                if run.full() {
                    break;
                }
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                rows += 1;
                run.units_seen += 1;
                let v: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => {
                        run.count("unparsable_row");
                        continue;
                    }
                };
                let s = |k: &str| v[k].as_str().unwrap_or("").to_owned();
                let id = s("id");
                if id.is_empty() {
                    run.count("row_without_id");
                    continue;
                }
                let unit = format!("reddit_post:{id}");
                let author = s("author");
                if author.is_empty() || author == "[deleted]" {
                    run.count("deleted_or_missing_author");
                    continue;
                }
                let wk = contributor_key("reddit_author", &author);
                if !run.sampled_meta_grouped(&wk, &unit, || json!({"post_id": id, "subreddit": v["subreddit"], "has_title": !v["title"].is_null()}))? {
                    continue;
                }
                if bot_account(&author) {
                    run.reject(&unit, "bot_account")?;
                    continue;
                }
                let body = s("body");
                if matches!(body.trim(), "[deleted]" | "[removed]" | "") {
                    run.reject(&unit, "deleted_or_removed_body")?;
                    continue;
                }
                let paras = mark_quote_spans(reddit_paragraphs(&body));
                tally(&mut breaks, &paras);
                let Some(ex) = text::paragraph_run(&paras) else {
                    run.reject(&unit, "no_paragraph_run_within_bounds")?;
                    continue;
                };
                if let Some(r) = text::english_screen(&ex.text) {
                    run.reject(&unit, r)?;
                    continue;
                }
                let sub = s("subreddit");
                let kind = if v["title"].is_null() { "comment" } else { "submission" };
                let doc = Doc {
                    id: format!("pool-reddit-{id}"),
                    collection: REDDIT_COLLECTION,
                    domain: "forum_reddit",
                    source_group: unit.clone(),
                    writer_key: Some(wk.clone()),
                    evidence: "historical_proxy",
                    excerpt: &ex,
                    extraction_method: &method,
                    unit: json!({"kind": "reddit_post_body", "post_id": id, "post_kind_inferred": kind, "body_sha256": text::sha_hex(body.as_bytes()),
                                 "zip_entry": name, "span_basis": "rendered_paragraph_index_half_open"}),
                    provenance: json!({"post_id": id, "subreddit": sub, "subreddit_id": v["subreddit_id"], "post_kind_inferred": kind,
                                       "corpus": "Webis-TLDR-17", "corpus_doi": "10.5281/zenodo.1043504", "zip_sha256": zip_sha,
                                       "source_url": format!("https://www.reddit.com/r/{sub}/comments/{id}/"),
                                       "post_period": "2006-2016 (corpus description)"}),
                    date: Some("2017-11-07".into()),
                    date_basis: "upper_bound_corpus_publication_2017-11-07;posts_2006_2016",
                    cutoff_policy: "dated_before_cutoff",
                    rights: forum_rights(
                        WEBIS_LICENSE,
                        Some("https://creativecommons.org/licenses/by/4.0/"),
                        format!("Reddit post {id} in r/{sub}, via Webis-TLDR-17 (Syed, Voelske, Potthast and Stein 2017; CC BY 4.0); author account withheld (opaque writer_key)"),
                        "https://zenodo.org/records/1043504",
                        "forums-reddit-webis-tldr17-v1",
                        "train_or_evaluation",
                        "Compilation licence CC BY 4.0; post text is Reddit user content under Reddit's terms, admitted for classifier-training use; never redistributed.",
                    ),
                    aliases: vec![],
                };
                if run.accept(&unit, &doc)? {
                    *by_sub.entry(sub).or_default() += 1;
                }
            }
        }
        let (sha, bytes) = r.finish();
        entries.push(json!({"entry": name, "rows": rows, "decompressed_sha256": sha, "decompressed_bytes": bytes}));
    }
    let mut subs: Vec<(String, u64)> = by_sub.into_iter().collect();
    subs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(json!({"entries": entries, "paragraph_breaks": breaks, "subreddits_with_candidates": subs.len(),
              "top_subreddits": subs.iter().take(50).map(|(s, n)| json!([s, n])).collect::<Vec<_>>(),
              "grouping": "unit and source family: post; bucket, split group and writer_key: opaque Reddit author key",
              "limits": ["post dates are bounded by the corpus (2006-2016) rather than read per row",
                         "every Webis post contains a TL;DR by selection; TL;DR paragraphs break runs, so excerpts usually precede it",
                         "comment and submission are inferred from the title field",
                         "bot screen is name based"]}))
}

// ---------------------------------------------------------------- Ubuntu IRC

#[derive(Debug, PartialEq)]
pub struct Msg {
    pub minute: u32,
    pub text: String,
}

/// Nick folded for grouping: mode prefixes and trailing reconnect marks
/// (`_`, backquote) removed, lowercased.
pub fn fold_nick(n: &str) -> String {
    n.trim_start_matches(['@', '+', '%', '~', '&']).trim_end_matches(['_', '`']).to_lowercase()
}

pub fn irc_bot(nick: &str) -> bool {
    const KNOWN: &[&str] = &["meetingology", "ubuntulog", "launchpad", "ubottu", "ubotu", "lubotu", "kubotu", "jenkins", "checkbox", "irker", "mup"];
    nick.contains("bot") || KNOWN.iter().any(|k| nick.starts_with(k))
}

/// Parse `[HH:MM] <nick> message` lines into per-nick message lists.
/// Actions (`* nick ...`), status lines and other forms are counted.
pub fn irc_messages(log: &str, counts: &mut BTreeMap<&'static str, u64>) -> Vec<(String, Vec<Msg>)> {
    let mut order: Vec<String> = Vec::new();
    let mut by: HashMap<String, Vec<Msg>> = HashMap::new();
    for line in log.lines() {
        let parsed = (|| {
            let rest = line.strip_prefix('[')?;
            let (hm, rest) = rest.split_once("] ")?;
            let (h, m) = hm.split_once(':')?;
            let minute = h.parse::<u32>().ok()? * 60 + m.get(..2)?.parse::<u32>().ok()?;
            let rest = rest.strip_prefix('<')?;
            let (nick, msg) = rest.split_once("> ")?;
            Some((minute, nick, msg))
        })();
        let Some((minute, nick, msg)) = parsed else {
            *counts.entry(if line.contains("] * ") || line.contains("]  * ") { "action_line" } else if line.starts_with("===") { "status_line" } else { "other_line" }).or_default() += 1;
            continue;
        };
        let k = fold_nick(nick);
        if k.is_empty() {
            continue;
        }
        *counts.entry("message_line").or_default() += 1;
        if !by.contains_key(&k) {
            order.push(k.clone());
        }
        by.entry(k).or_default().push(Msg { minute, text: text::squash(msg) });
    }
    order.into_iter().map(|k| {
        let v = by.remove(&k).unwrap_or_default();
        (k, v)
    }).collect()
}

pub const IRC_GAP_MINUTES: u32 = 30;

/// One nick's messages as paragraphs: screened messages, with a break
/// where more than `IRC_GAP_MINUTES` pass between consecutive messages.
pub fn irc_units(msgs: &[Msg]) -> Vec<Para> {
    // Angle brackets break: pasted chat lines (`<nick> ...`) are other
    // speakers' text.
    let screen = Screen { quotes: true, angle_brackets: true };
    let mut out = Vec::new();
    let mut last: Option<u32> = None;
    for m in msgs {
        if let Some(l) = last {
            if m.minute < l || m.minute - l > IRC_GAP_MINUTES {
                out.push(Para::non_prose(String::new(), "time_gap"));
            }
        }
        last = Some(m.minute);
        let t = m.text.trim();
        let nonspace = t.chars().filter(|c| !c.is_whitespace()).count();
        let letters = t.chars().filter(|c| c.is_alphabetic()).count();
        if t.starts_with('!') {
            out.push(Para::non_prose(t, "bot_command"));
        } else if nonspace > 0 && (letters as f64) < 0.5 * nonspace as f64 {
            out.push(Para::non_prose(t, "symbol_heavy"));
        } else {
            out.push(informal(t.to_owned(), screen));
        }
    }
    out
}

/// `YYYY/MM/DD` from an irclogs.ubuntu.com URL.
pub fn irc_url_date(url: &str) -> Option<String> {
    let rest = url.split("irclogs.ubuntu.com/").nth(1)?;
    let mut p = rest.split('/');
    let (y, m, d) = (p.next()?, p.next()?, p.next()?);
    let s = format!("{y}-{m}-{d}");
    record::valid_date(&s).then_some(s)
}

pub fn ubuntu_irc(shards: &[PathBuf], dataset_revision: &str, run: &mut Run) -> Result<Value> {
    let method = format!("irc_log_nick_messages_v1:one_nick_one_channel_day;messages_in_order;break_on_{IRC_GAP_MINUTES}_minute_gap;{INFORMAL_METHOD};first_consecutive_whole_messages_until_220_words_or_run_end;150..450_whitespace_words;messages_joined_by_newline_v1");
    let mut counts: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut breaks = BTreeMap::new();
    let mut files = Vec::new();
    let mut by_year: BTreeMap<String, u64> = BTreeMap::new();
    let mut channels: BTreeMap<String, u64> = BTreeMap::new();
    for path in shards {
        let (sha, bytes) = io::file_sha(path)?;
        let gz = flate2::read::MultiGzDecoder::new(std::fs::File::open(path)?);
        let mut hr = io::HashingReader::new(gz);
        let mut logs = 0u64;
        {
            let br = BufReader::with_capacity(1 << 20, &mut hr);
            for line in br.lines() {
                if run.full() {
                    break;
                }
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                logs += 1;
                let v: Value = serde_json::from_str(&line).with_context(|| format!("row in {}", path.display()))?;
                let url = v["metadata"]["url"].as_str().unwrap_or("").to_owned();
                let channel = v["metadata"]["channel"].as_str().unwrap_or("").to_owned();
                let Some(date) = irc_url_date(&url) else {
                    run.count("log_without_url_date");
                    continue;
                };
                if v["created"].as_str().is_some_and(|c| c.get(..10) != Some(date.as_str())) {
                    run.count("log_created_differs_from_url_date");
                    continue;
                }
                if !record::before_cutoff(&date) {
                    run.count("log_not_before_2022_11_30");
                    continue;
                }
                if channel.is_empty() {
                    run.count("log_without_channel");
                    continue;
                }
                let log = v["text"].as_str().unwrap_or("");
                let log_sha = text::sha_hex(log.as_bytes());
                for (nick, msgs) in irc_messages(log, &mut counts) {
                    run.units_seen += 1;
                    let words: usize = msgs.iter().map(|m| text::words(&m.text)).sum();
                    if words < text::MIN_WORDS {
                        run.count("speaker_day_under_150_words");
                        continue;
                    }
                    let wk = contributor_key("ubuntu_irc_speaker", &format!("{}\0{nick}", channel.to_lowercase()));
                    let unit = format!("ubuntu_irc:{}:{date}:{}", channel.to_lowercase(), &wk["ubuntu_irc_speaker:".len()..]);
                    if !run.sampled_meta_grouped(&wk, &unit, || json!({"channel": channel, "date": date, "messages": msgs.len()}))? {
                        continue;
                    }
                    if irc_bot(&nick) {
                        run.reject(&unit, "bot_nick")?;
                        continue;
                    }
                    let paras = irc_units(&msgs);
                    tally(&mut breaks, &paras);
                    let Some(ex) = paragraph_run_joined(&paras, "\n") else {
                        run.reject(&unit, "no_message_run_within_bounds")?;
                        continue;
                    };
                    if let Some(r) = text::english_screen(&ex.text) {
                        run.reject(&unit, r)?;
                        continue;
                    }
                    let doc = Doc {
                        id: format!("pool-irc-{}", &text::sha_hex(unit.as_bytes())[..24]),
                        collection: IRC_COLLECTION,
                        domain: "chat_irc",
                        source_group: unit.clone(),
                        writer_key: Some(wk.clone()),
                        evidence: "historical_proxy",
                        excerpt: &ex,
                        extraction_method: &method,
                        unit: json!({"kind": "irc_speaker_channel_day", "log_id": v["id"], "log_text_sha256": log_sha, "speaker_messages": msgs.len(),
                                     "span_basis": "speaker_message_index_half_open_with_gap_breaks", "shard": path.file_name().and_then(|n| n.to_str())}),
                        provenance: json!({"channel": channel, "date": date, "source_url": url, "dataset": "common-pile/ubuntu_irc (raw)",
                                           "dataset_revision": dataset_revision, "log_id": v["id"], "upstream_license_field": v["metadata"]["license"]}),
                        date: Some(date.clone()),
                        date_basis: "irclogs_url_date",
                        cutoff_policy: "dated_before_cutoff",
                        rights: forum_rights(
                            IRC_LICENSE,
                            None,
                            format!("Ubuntu IRC log {channel} {date}, {url}; speaker nick withheld (opaque writer_key)"),
                            "https://huggingface.co/datasets/common-pile/ubuntu_irc",
                            "forums-ubuntu-irc-v1",
                            "train_or_evaluation",
                            "Common Pile states the Ubuntu IRC logs were released into the public domain; irclogs.ubuntu.com shows no licence page. Nicks and timestamps are not part of the excerpt text; addressed nicks inside messages remain.",
                        ),
                        aliases: vec![],
                    };
                    if run.accept(&unit, &doc)? {
                        *by_year.entry(date[..4].to_owned()).or_default() += 1;
                        *channels.entry(channel.to_lowercase()).or_default() += 1;
                    }
                }
            }
        }
        let (dsha, dbytes) = hr.finish();
        files.push(json!({"file": path.file_name().and_then(|n| n.to_str()), "sha256": sha, "bytes": bytes, "logs": logs,
                          "decompressed_sha256": dsha, "decompressed_bytes": dbytes}));
    }
    let mut ch: Vec<(String, u64)> = channels.into_iter().collect();
    ch.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(json!({"files": files, "line_counts": counts, "message_breaks": breaks, "candidates_by_year": by_year, "channels_with_candidates": ch.len(),
              "top_channels": ch.iter().take(40).map(|(c, n)| json!([c, n])).collect::<Vec<_>>(),
              "grouping": "unit and source family: speaker-channel-day; bucket, split group and writer_key: opaque channel+nick key",
              "nick_folding": "mode prefixes and trailing _ or backquote removed, lowercased",
              "limits": ["a document is one speaker's messages in one channel-day, not a contiguous conversation",
                         "nick identity is not authenticated; one person may use several nicks and a nick may be shared",
                         "bot screen is name based", "messages addressed to other users keep the addressed nick"]}))
}

// ---------------------------------------------------------------- WikiConv

#[derive(Deserialize)]
struct Utt {
    id: String,
    #[serde(default)]
    conversation_id: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    speaker: Option<String>,
    #[serde(default)]
    timestamp: Option<f64>,
    #[serde(default, rename = "reply-to")]
    reply_to: Option<String>,
    meta: UttMeta,
}

#[derive(Deserialize)]
struct UttMeta {
    #[serde(default)]
    is_section_header: Option<bool>,
    #[serde(default)]
    rev_id: Option<String>,
    #[serde(default)]
    original: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    modification: Option<Vec<serde::de::IgnoredAny>>,
    #[serde(default)]
    deletion: Option<Vec<serde::de::IgnoredAny>>,
    #[serde(default)]
    restoration: Option<Vec<serde::de::IgnoredAny>>,
}

#[derive(Debug, PartialEq)]
pub enum Sig {
    None,
    Stripped(String, usize),
    Unstrippable,
}

fn last_sentence_end(t: &str) -> Option<usize> {
    t.char_indices().filter(|(_, c)| matches!(c, '.' | '!' | '?')).map(|(i, _)| i).last()
}

/// Strip a trailing Wikipedia signature (`... sentence. -- Name 12:00, 7 June
/// 2003 (UTC)` or `... sentence. --Name`) from a paragraph. The signature is
/// everything after the last sentence-final mark, at most 16 words with a
/// clock time for `(UTC)` signatures and at most 6 words starting with a
/// dash or tilde otherwise.
pub fn strip_signature(p: &str) -> Sig {
    let t = p.trim_end();
    let utc = t.ends_with("(UTC)");
    let head = if utc { &t[..t.len() - 5] } else { t };
    let Some(i) = last_sentence_end(head) else {
        return if utc { Sig::Unstrippable } else { Sig::None };
    };
    let mut cut = i + 1;
    while let Some(c) = t[cut..].chars().next().filter(|c| matches!(c, '"' | '\'' | '”' | '’' | ')' | ']')) {
        cut += c.len_utf8();
    }
    let tail = t[cut..].trim();
    if tail.is_empty() {
        return Sig::None;
    }
    let n = text::words(tail);
    if utc {
        let clock = tail.as_bytes().windows(3).any(|w| w[0].is_ascii_digit() && w[1] == b':' && w[2].is_ascii_digit());
        if n <= 16 && clock { Sig::Stripped(t[..cut].trim_end().to_owned(), n) } else { Sig::Unstrippable }
    } else if n <= 6 && (tail.starts_with("--") || tail.starts_with('—') || tail.starts_with('–') || tail.starts_with('~')) {
        Sig::Stripped(t[..cut].trim_end().to_owned(), n)
    } else {
        Sig::None
    }
}

/// Substituted template messages (welcomes, warnings, notices): not the
/// posting user's own writing.
pub const TALK_TEMPLATE_PHRASES: &[&str] = &[
    "welcome to wikipedia", "hope you enjoy editing", "enjoy editing here", "four tildes", "three tildes", "sign your name on talk pages",
    "sign your posts on talk pages", "here are a few good links", "here are some links", "you might find these links helpful",
    "thank you for experimenting", "your test worked", "please do not add nonsense", "you have been blocked", "you may be blocked",
    "will be blocked from editing", "if you continue to vandalize", "considered vandalism", "speedy deletion", "has been nominated for deletion",
    "proposed deletion", "articles for deletion", "this is an automated message", "this message was left by", "image copyright problem",
    "copyright problems", "fair use rationale", "unspecified source", "orphaned non-free", "disambiguation link notification",
    "your recent edit", "your edit to", "the welcome committee", "helpme", "new users log",
    "articles for creation", "your submission at", "your article submission", "has been reviewed", "you are invited to join",
    "teahouse", "you've got mail", "you have new messages", "this is an automated notice", "a tag has been placed",
    "dispute resolution noticeboard", "has been listed at", "please stop your disruptive editing", "edit warring",
    "the signpost", "newsletter", "to opt out of", "delivered by", "mass message", "barnstar",
];

pub fn talk_template(body_lower: &str) -> Option<&'static str> {
    TALK_TEMPLATE_PHRASES.iter().copied().find(|p| body_lower.contains(p))
}

/// Visible holes left by WikiConv's upstream removal of links and
/// templates: a space before punctuation, leading punctuation, pipes or
/// empty parentheses, and the shared MediaWiki residue screen.
pub fn talk_gap(t: &str) -> Option<&'static str> {
    let b = t.as_bytes();
    if t.trim_start().starts_with([',', '.', ';', ':', ')', '|']) {
        return Some("leading_punctuation_gap");
    }
    for i in 1..b.len() {
        if b[i - 1] == b' ' && matches!(b[i], b'.' | b',' | b';' | b':' | b')') && (i + 1 == b.len() || !b[i + 1].is_ascii_alphanumeric()) {
            return Some("space_before_punctuation_gap");
        }
    }
    if !text::markup_residue(t).is_empty() {
        return Some("mediawiki_rendering_residue");
    }
    None
}

/// Render one WikiConv comment: conservative wikitext paragraphs, section
/// boundaries as breaks, the informal screen, a trailing signature stripped
/// from the last paragraph, any other `(UTC)` signature a break.
pub fn talk_paragraphs(wikitext_src: &str, sig_words: &mut usize, stripped: &mut bool) -> Vec<Para> {
    let mut paras = Vec::new();
    for (k, s) in wikitext::render(wikitext_src, Screen::MARKUP).into_iter().enumerate() {
        if k > 0 {
            paras.push(Para::non_prose(String::new(), "section_boundary"));
        }
        paras.extend(s.paras);
    }
    let mut paras: Vec<Para> = rescreen(paras, Screen::MARKUP)
        .into_iter()
        .map(|p| match (p.prose, talk_gap(&p.text)) {
            (true, Some(r)) => Para::non_prose(p.text, r),
            _ => p,
        })
        .collect();
    let last = paras.iter().rposition(|p| !p.text.trim().is_empty());
    for (k, p) in paras.iter_mut().enumerate() {
        if Some(k) == last {
            match strip_signature(&p.text) {
                Sig::Stripped(t, n) => {
                    *sig_words = n;
                    *stripped = true;
                    *p = if t.contains("(UTC)") {
                        Para::non_prose(t, "embedded_signature")
                    } else {
                        let q = informal(t, Screen::MARKUP);
                        match (q.prose, talk_gap(&q.text)) {
                            (true, Some(r)) => Para::non_prose(q.text, r),
                            _ => q,
                        }
                    };
                }
                Sig::Unstrippable => *p = Para::non_prose(std::mem::take(&mut p.text), "unstrippable_signature"),
                Sig::None if p.text.contains("(UTC)") => *p = Para::non_prose(std::mem::take(&mut p.text), "embedded_signature"),
                Sig::None => {}
            }
        } else if p.text.contains("(UTC)") {
            *p = Para::non_prose(std::mem::take(&mut p.text), "embedded_signature");
        }
    }
    paras
}

pub fn wikiconv(zips: &[PathBuf], run: &mut Run) -> Result<Value> {
    let method = format!("wikiconv_utterance_text_v1;{};section_breaks;{INFORMAL_METHOD};trailing_signature_stripped;{}", wikitext::RENDER_METHOD, text::PARAGRAPH_RUN_METHOD);
    let mut breaks = BTreeMap::new();
    let mut files = Vec::new();
    let mut by_year: BTreeMap<String, u64> = BTreeMap::new();
    let (mut sig_stripped, mut sig_words_total, mut header_flagged, mut ip_speakers) = (0u64, 0u64, 0u64, 0u64);
    for path in zips {
        let (sha, bytes) = io::file_sha(path)?;
        let corpus = path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("").to_owned();
        let f = std::fs::File::open(path)?;
        let mut z = zip::ZipArchive::new(f)?;
        let e = z.by_name("utterances.jsonl").with_context(|| format!("utterances.jsonl in {}", path.display()))?;
        let mut hr = io::HashingReader::new(e);
        let mut rows = 0u64;
        {
            let br = BufReader::with_capacity(1 << 20, &mut hr);
            for line in br.lines() {
                if run.full() {
                    break;
                }
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                rows += 1;
                run.units_seen += 1;
                let u: Utt = match serde_json::from_str(&line) {
                    Ok(u) => u,
                    Err(_) => {
                        run.count("unparsable_utterance");
                        continue;
                    }
                };
                let body = u.text.as_deref().unwrap_or("");
                if text::words(body) < text::MIN_WORDS {
                    run.count("utterance_under_150_words_before_rendering");
                    continue;
                }
                let Some(speaker) = u.speaker.as_deref().filter(|s| !s.is_empty()) else {
                    run.count("no_speaker");
                    continue;
                };
                let unit = format!("wikiconv_utterance:{}", u.id);
                let wk = contributor_key("wikipedia_talk_user", speaker);
                let ts = u.timestamp.unwrap_or(0.0) as i64;
                let date = unix_date(ts);
                if !run.sampled_meta_grouped(&wk, &unit, || json!({"utterance_id": u.id, "rev_id": u.meta.rev_id, "timestamp": ts, "corpus": corpus}))? {
                    continue;
                }
                if bot_account(speaker) || speaker.to_lowercase().contains("bot") {
                    run.reject(&unit, "bot_account")?;
                    continue;
                }
                let edited = u.meta.original.is_some()
                    || u.meta.modification.as_ref().is_some_and(|v| !v.is_empty())
                    || u.meta.deletion.as_ref().is_some_and(|v| !v.is_empty())
                    || u.meta.restoration.as_ref().is_some_and(|v| !v.is_empty());
                if edited {
                    run.reject(&unit, "modified_deleted_or_restored_utterance")?;
                    continue;
                }
                if ts <= 0 || !record::before_cutoff(&date) {
                    run.reject(&unit, "not_dated_before_2022_11_30")?;
                    continue;
                }
                if let Some(t) = talk_template(&body.to_lowercase()) {
                    run.reject(&unit, &format!("template_message:{}", t.replace(' ', "_")))?;
                    continue;
                }
                let (mut sw, mut stripped) = (0usize, false);
                let paras = mark_quote_spans(talk_paragraphs(body, &mut sw, &mut stripped));
                tally(&mut breaks, &paras);
                let Some(ex) = text::paragraph_run(&paras) else {
                    run.reject(&unit, "no_paragraph_run_within_bounds")?;
                    continue;
                };
                if let Some(r) = text::english_screen(&ex.text) {
                    run.reject(&unit, r)?;
                    continue;
                }
                let sig_in_excerpt = stripped && ex.end == paras.iter().rposition(|p| !p.text.trim().is_empty()).map_or(0, |k| k + 1);
                let ip = speaker.split('.').count() == 4 && speaker.split('.').all(|x| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit()))
                    || (speaker.contains(':') && speaker.chars().all(|c| c.is_ascii_hexdigit() || c == ':'));
                let rev = u.meta.rev_id.clone().unwrap_or_default();
                let url = format!("https://en.wikipedia.org/w/index.php?oldid={rev}&diff=prev");
                let doc = Doc {
                    id: format!("pool-wikitalk-{}", u.id.replace('.', "-")),
                    collection: WIKI_TALK_COLLECTION,
                    domain: "discussion_wikipedia_talk",
                    source_group: unit.clone(),
                    writer_key: Some(wk.clone()),
                    evidence: "historical_proxy",
                    excerpt: &ex,
                    extraction_method: &method,
                    unit: json!({"kind": "wikiconv_utterance", "utterance_id": u.id, "text_sha256": text::sha_hex(body.as_bytes()),
                                 "trailing_signature_stripped_from_excerpt": sig_in_excerpt, "signature_words_stripped": if sig_in_excerpt { sw } else { 0 },
                                 "is_section_header_flag": u.meta.is_section_header, "span_basis": "rendered_paragraph_index_half_open"}),
                    provenance: json!({"utterance_id": u.id, "conversation_id": u.conversation_id, "reply_to": u.reply_to, "rev_id": rev,
                                       "timestamp": ts, "source_url": url, "corpus": format!("convokit {corpus}"), "corpus_sha256": sha,
                                       "upstream": "WikiConv (Hua et al. 2018), 2018-07-01 English Wikipedia dumps", "anonymous_ip_editor": ip}),
                    date: Some(date.clone()),
                    date_basis: "wikiconv_utterance_timestamp_unix_utc",
                    cutoff_policy: "dated_before_cutoff",
                    rights: forum_rights(
                        WIKI_TALK_LICENSE,
                        None,
                        format!("Wikipedia talk-page comment, revision {rev} ({url}); contributor in page history; CC BY-SA 3.0; via WikiConv (CC0 compilation); wikitext rendered, signature removed"),
                        "https://github.com/conversationai/wikidetox/tree/master/wikiconv",
                        "forums-wikipedia-talk-wikiconv-v1",
                        "train_or_evaluation",
                        "Comment text CC BY-SA 3.0 (Wikipedia); WikiConv dataset CC0 1.0. Only unmodified, undeleted, unrestored utterances.",
                    ),
                    aliases: vec![],
                };
                if run.accept(&unit, &doc)? {
                    *by_year.entry(date[..4].to_owned()).or_default() += 1;
                    if sig_in_excerpt {
                        sig_stripped += 1;
                        sig_words_total += sw as u64;
                    }
                    if u.meta.is_section_header == Some(true) {
                        header_flagged += 1;
                    }
                    if ip {
                        ip_speakers += 1;
                    }
                }
            }
        }
        let (dsha, dbytes) = hr.finish();
        files.push(json!({"file": format!("{corpus}/{}", path.file_name().and_then(|n| n.to_str()).unwrap_or("")), "sha256": sha, "bytes": bytes,
                          "utterances": rows, "utterances_jsonl_sha256": dsha, "utterances_jsonl_bytes": dbytes}));
    }
    Ok(json!({"files": files, "paragraph_breaks": breaks, "candidates_by_year": by_year,
              "candidates_with_stripped_signature": sig_stripped, "signature_words_stripped_total": sig_words_total,
              "candidates_flagged_is_section_header": header_flagged, "candidates_by_ip_editors": ip_speakers,
              "grouping": "unit and source family: utterance; bucket, split group and writer_key: opaque Wikipedia user key",
              "limits": ["WikiConv text is its 2018 reconstruction of each comment, already partly rendered",
                         "the is_section_header flag is set on most utterances and is not used as a screen",
                         "signature stripping is heuristic (text after the last sentence-final mark)",
                         "IP editors are grouped by address, which is not a stable person"]}))
}

// ---------------------------------------------------------------- contributor cap

/// Keep at most `max` candidates per `writer_key`, ranked by
/// SHA-256(seed, id); writes a new adapter-shaped directory that `dedup`
/// accepts as a source.
pub fn cap_contributors(input: &Path, out: &Path, max: usize, seed: &str) -> Result<Value> {
    cap_and_sample(input, out, max, seed, None)
}

/// `cap_contributors` after an optional seeded document sample: a candidate
/// is eligible iff (first 8 bytes of SHA-256("sample" NUL seed NUL id)) mod
/// 1000 < `per_mille`.
pub fn cap_and_sample(input: &Path, out: &Path, max: usize, seed: &str, per_mille: Option<u64>) -> Result<Value> {
    anyhow::ensure!(per_mille.is_none_or(|k| (1..=1000).contains(&k)), "per_mille must be 1..=1000");
    let sampled = |id: &str| per_mille.is_none_or(|k| text::h64(format!("sample\0{seed}\0{id}").as_bytes()) % 1000 < k);
    let rep: Value = serde_json::from_slice(&std::fs::read(input.join("report.json"))?)?;
    anyhow::ensure!(rep["schema"] == crate::adapter::REPORT_SCHEMA && rep["complete"] == true, "incomplete adapter report");
    let (sha, _) = io::file_sha(&input.join("candidates.jsonl"))?;
    anyhow::ensure!(rep["candidates_sha256"] == sha.as_str(), "candidates changed since adapter report");
    let mut ranks: HashMap<String, Vec<(u64, String)>> = HashMap::new();
    for row in io::jsonl_rows(&input.join("candidates.jsonl"))? {
        let row = row?;
        let id = row["id"].as_str().context("id")?.to_owned();
        if !sampled(&id) {
            continue;
        }
        let w = row["writer_key"].as_str().unwrap_or(row["source_group"].as_str().unwrap_or("")).to_owned();
        ranks.entry(w).or_default().push((text::h64(format!("{seed}\0{id}").as_bytes()), id));
    }
    let mut keep = std::collections::HashSet::new();
    let mut sizes: Vec<usize> = Vec::new();
    for v in ranks.values_mut() {
        v.sort();
        sizes.push(v.len());
        for (_, id) in v.iter().take(max) {
            keep.insert(id.clone());
        }
    }
    io::fresh_dir(out)?;
    let mut cw = io::JsonlWriter::create(&out.join("candidates.jsonl"))?;
    let mut rw = io::JsonlWriter::create(&out.join("rejections.jsonl"))?;
    let mut words = Vec::new();
    let mut removed: BTreeMap<&str, u64> = BTreeMap::new();
    for row in io::jsonl_rows(&input.join("candidates.jsonl"))? {
        let row = row?;
        let id = row["id"].as_str().unwrap_or("");
        if keep.contains(id) {
            words.push(row["word_count"].as_u64().unwrap_or(0));
            cw.write(&row)?;
        } else {
            let reason = if sampled(id) { "contributor_cap" } else { "document_sample" };
            *removed.entry(reason).or_default() += 1;
            rw.write(&json!({"unit_id": row["source_group"], "reason": reason}))?;
        }
    }
    let (csha, crows) = cw.finish()?;
    let (rsha, rrows) = rw.finish()?;
    sizes.sort_unstable();
    words.sort_unstable();
    let mut report = rep.clone();
    let mut reasons = rep["rejection_reasons"].clone();
    for (k, v) in &removed {
        reasons[*k] = json!(v);
    }
    report["candidates"] = json!(crows);
    report["candidates_sha256"] = json!(csha);
    report["rejections_sha256_cap_stage"] = json!(rsha);
    report["rejection_reasons"] = reasons;
    report["finished_utc"] = json!(io::now_utc());
    report["candidate_word_counts"] = if words.is_empty() { json!(null) } else { json!({"min": words[0], "median": words[words.len() / 2], "max": words[words.len() - 1]}) };
    report["contributor_cap"] = json!({"input_dir": input, "input_candidates_sha256": sha, "max_per_contributor": max, "seed": seed,
        "document_sample_per_mille": per_mille, "removed_by_reason": removed,
        "rule": "optional document sample (SHA-256(\"sample\" NUL seed NUL id) mod 1000 < per_mille), then per writer_key keep the max lowest SHA-256(seed NUL id) ranks", "contributors": sizes.len(),
        "contributors_over_cap": sizes.iter().filter(|&&n| n > max).count(), "largest_contributor_before_cap": sizes.last(),
        "removed": rrows, "kept": crows, "adapter_rejections_file": "../input rejections.jsonl (unchanged)"});
    io::write_json(&out.join("report.json"), &report)?;
    Ok(report["contributor_cap"].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(t: &str) -> Para {
        informal(t.to_owned(), Screen::XML)
    }

    #[test]
    fn informal_screen_keeps_casual_text_and_breaks_quotes_and_urls() {
        assert!(para("lol yeah that happened to me too").prose);
        assert_eq!(para("> someone else said this whole sentence here.").reason, Some("quoted_reply"));
        assert_eq!(para("see https://example.org for the details of it").reason, Some("url"));
        assert_eq!(para(":) :)").reason, Some("no_letters"));
        let r = rescreen(vec![Para::classify("ok thanks", Screen::XML), Para::non_prose("x", "list_item")], Screen::XML);
        assert!(r[0].prose);
        assert_eq!(r[1].reason, Some("list_item"));
        assert_ne!(contributor_key("wikipedia_talk_user", "alice"), contributor_key("reddit_author", "alice"));
        assert!(!contributor_key("wikipedia_talk_user", "alice").contains("alice"));
    }

    #[test]
    fn joined_run_matches_paragraph_run() {
        let w = |n: usize| (0..n).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ");
        let ps = vec![Para::non_prose("", "list_item"), para(&w(100)), para(&w(90)), para(&w(60)), para(&w(10))];
        let a = text::paragraph_run(&ps).unwrap();
        let b = paragraph_run_joined(&ps, "\n\n").unwrap();
        assert_eq!(a, b);
        assert_eq!((a.start, a.end, a.words), (1, 4, 250));
        let c = paragraph_run_joined(&ps, "\n").unwrap();
        assert_eq!(c.text.matches('\n').count(), 2);
    }

    #[test]
    fn reddit_markdown_blocks_and_inline() {
        let body = "So I tried **really** hard to fix my *old* bike this weekend.\nIt went [badly](http://x.y) in the end.\n\n&gt; quoted from the parent comment above\n\n* a list item\n* another\n\n    code here\n\n# Heading\n\nI ~~never~~ did that.\n\nTL;DR: bike is still broken.\n\nEDIT: thanks everyone";
        let p = reddit_paragraphs(body);
        let r: Vec<_> = p.iter().map(|p| p.reason).collect();
        assert_eq!(r, vec![None, Some("quotation"), Some("list_item"), Some("code_block"), Some("heading"), Some("removed_inline_content"), Some("tldr_summary"), None]);
        assert_eq!(p[0].text, "So I tried really hard to fix my old bike this weekend. It went badly in the end.");
        assert_eq!(md_inline("2 \\* 3 is six"), "2 * 3 is six");
        assert!(md_inline("an *unbalanced marker").contains(text::GAP));
        assert!(tldr("tl;dr: nothing") && tldr("TLDR it broke") && !tldr("the toddler slept"));
        assert!(bot_account("AutoModerator") && bot_account("some_bot") && !bot_account("abbott_fan"));
    }

    #[test]
    fn irc_parsing_gaps_and_bots() {
        let log = "[10:00] <Alice_> hi all, quick question about my wifi card\n[10:01]  * bob waves\n[10:02] <bob> alice: which chipset is it\n[10:03] <alice> !info firmware\n[10:50] <alice> back again, it works now\n=== carol is now known as dave\n";
        let mut c = BTreeMap::new();
        let m = irc_messages(log, &mut c);
        assert_eq!(m[0].0, "alice");
        assert_eq!(m[0].1.len(), 3);
        assert_eq!(m[1].0, "bob");
        assert_eq!(c["action_line"], 1);
        assert_eq!(c["status_line"], 1);
        let u = irc_units(&m[0].1);
        let r: Vec<_> = u.iter().map(|p| p.reason).collect();
        assert_eq!(r, vec![None, Some("bot_command"), Some("time_gap"), None]);
        let pasted = irc_units(&[Msg { minute: 1, text: "6:02 AM <someone> my wifi is down again".into() }]);
        assert_eq!(pasted[0].reason, Some("residual_markup"));
        assert!(irc_bot("ubottu") && irc_bot("lubotu3") && irc_bot("meetingology") && !irc_bot("alice"));
        assert_eq!(irc_url_date("https://irclogs.ubuntu.com/2015/04/17/%23ubuntu-ir.txt").as_deref(), Some("2015-04-17"));
    }

    #[test]
    fn signatures_are_stripped_or_break() {
        assert_eq!(strip_signature("I agree with the merge. Example User (talk) 03:12, 7 June 2003 (UTC)"), Sig::Stripped("I agree with the merge.".into(), 8));
        assert_eq!(strip_signature("Fine by me! --Someone"), Sig::Stripped("Fine by me!".into(), 1));
        assert_eq!(strip_signature("no punctuation at all 03:12, 7 June 2003 (UTC)"), Sig::Unstrippable);
        assert_eq!(strip_signature("A plain closing sentence."), Sig::None);
        assert_eq!(strip_signature("A sentence. and then an informal trailing clause"), Sig::None);
        let (mut w, mut s) = (0, false);
        let p = talk_paragraphs("First we should check the ''sources'' again.\n\nSecond point, signed. Writer 10:00, 1 May 2010 (UTC)", &mut w, &mut s);
        assert!(s && w == 6);
        assert_eq!(p.last().unwrap().text, "Second point, signed.");
        assert_eq!(p[0].text, "First we should check the sources again.");
        let (mut w, mut s) = (0, false);
        let (mut w2, mut s2) = (0, false);
        let q = talk_paragraphs("First remark. 04:00 Sep 12, 2002 (UTC) A reply from someone else follows here", &mut w2, &mut s2);
        assert_eq!(q[0].reason, Some("embedded_signature"));
        let p = talk_paragraphs("Old comment. Writer 10:00, 1 May 2010 (UTC)\n\nNew reply below it.", &mut w, &mut s);
        assert_eq!(p[0].reason, Some("embedded_signature"));
        assert!(!s && w == 0);
        assert_eq!(talk_gap("Hello, , welcome aboard."), Some("space_before_punctuation_gap"));
        assert_eq!(talk_gap(", you changed the quotes."), Some("leading_punctuation_gap"));
        assert_eq!(talk_gap("A clean sentence, with commas; and a colon: here."), None);
        assert_eq!(talk_gap("Fine (see ) here."), Some("space_before_punctuation_gap"));
        assert_eq!(talk_template("hello and welcome to wikipedia! here are some links"), Some("welcome to wikipedia"));
        let (mut w, mut s) = (0, false);
        let p = talk_paragraphs("You can ask me on . I will answer soon.", &mut w, &mut s);
        assert_eq!(p[0].reason, Some("space_before_punctuation_gap"));
    }

    #[test]
    fn cap_keeps_at_most_n_per_writer() {
        let d = tempfile::tempdir().unwrap();
        let inp = d.path().join("in");
        std::fs::create_dir_all(&inp).unwrap();
        let mut rows = String::new();
        for i in 0..5 {
            let w = if i < 4 { "w:a" } else { "w:b" };
            rows.push_str(&format!("{}\n", json!({"id": format!("d{i}"), "writer_key": w, "source_group": format!("f{i}"), "word_count": 200})));
        }
        std::fs::write(inp.join("candidates.jsonl"), &rows).unwrap();
        let rep = json!({"schema": crate::adapter::REPORT_SCHEMA, "complete": true, "candidates_sha256": text::sha_hex(rows.as_bytes()), "rejection_reasons": {}});
        std::fs::write(inp.join("report.json"), rep.to_string()).unwrap();
        let out = d.path().join("out");
        let r = cap_contributors(&inp, &out, 2, "s").unwrap();
        assert_eq!(r["kept"], 3);
        assert_eq!(r["removed"], 2);
        let again = cap_contributors(&inp, &d.path().join("out2"), 2, "s").unwrap();
        assert_eq!(std::fs::read(out.join("candidates.jsonl")).unwrap(), std::fs::read(d.path().join("out2/candidates.jsonl")).unwrap());
        assert_eq!(again["kept"], 3);
        let s = cap_and_sample(&inp, &d.path().join("out3"), 10, "s", Some(500)).unwrap();
        assert_eq!(s["kept"].as_u64().unwrap() + s["removed"].as_u64().unwrap(), 5);
        assert!(cap_and_sample(&inp, &d.path().join("out4"), 10, "s", Some(0)).is_err());
    }

    #[test]
    fn quotations_across_paragraphs_break() {
        let ps = vec![para("You should read this:"), para("\"Not fun is zero on the scale, he wrote."), para("Anti-fun is negative.\""), para("My own view follows here.")];
        let r: Vec<_> = mark_quote_spans(ps).iter().map(|p| p.reason).collect();
        assert_eq!(r, vec![None, Some("multi_paragraph_quotation"), Some("multi_paragraph_quotation"), None]);
        let r: Vec<_> = mark_quote_spans(vec![para("He said \"no\" twice."), para("Fine.")]).iter().map(|p| p.reason).collect();
        assert_eq!(r, vec![None, None]);
    }
}
