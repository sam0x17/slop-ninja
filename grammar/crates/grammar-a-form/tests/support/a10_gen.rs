//! Synthetic text generators for the amendment 10 regression tests: the
//! independent checker's probe generators (`round2/a7check/a7check.rs`) and
//! the adversarial generators of the design-B prototype, copied so their
//! seeded streams give the same documents. Vocabulary only, no corpus text.
#![allow(dead_code)]

pub const VOCAB: &[&str] = &[
    "a", "an", "the", "of", "to", "in", "on", "and", "it", "we", "was", "for", "that", "with", "they", "this", "from", "have", "were", "would",
    "about", "there", "their", "which", "could", "other", "after", "first", "never", "these", "people", "before", "should", "around", "through",
    "village", "morning", "council", "evening", "building", "together", "questions", "different", "afterwards", "committee", "river", "road",
    "school", "bridge", "garden", "market", "winter", "summer", "letter", "report", "station", "harbour", "meeting", "weather", "office",
];
pub const PROPER: &[&str] = &["London", "Monday", "Houston", "Friday", "Enron", "Texas", "I", "March", "Smith", "Dana"];

pub struct Lcg(pub u64);
impl Lcg {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A sentence of 8-20 vocabulary words; `proper` > 0 makes one word in
/// `proper` a capitalised name, `comma` > 0 puts a comma after one word in
/// `comma` (never the last). With both 0 it is the checker's `sentence`.
pub fn sentence_v(r: &mut Lcg, end: bool, proper: u64, comma: u64) -> String {
    let n = 8 + r.below(13) as usize;
    let mut w: Vec<String> = (0..n)
        .map(|_| {
            let mut s = if proper > 0 && r.below(proper) == 0 { PROPER[r.below(PROPER.len() as u64) as usize].to_string() } else { VOCAB[r.below(VOCAB.len() as u64) as usize].to_string() };
            if comma > 0 && r.below(comma) == 0 {
                s.push(',');
            }
            s
        })
        .collect();
    let mut f = w[0].chars();
    w[0] = f.next().unwrap().to_uppercase().collect::<String>() + f.as_str();
    let last = w.len() - 1;
    w[last] = w[last].trim_end_matches(',').to_string();
    let mut s = w.join(" ");
    if end {
        s.push('.');
    }
    s
}

pub fn sentence(r: &mut Lcg, end: bool) -> String {
    sentence_v(r, end, 0, 0)
}

/// 3-6 sentences; the last one punctuated when `last_punct`.
pub fn paragraph(r: &mut Lcg, last_punct: bool) -> String {
    paragraph_v(r, last_punct, 0, 0)
}

pub fn paragraph_v(r: &mut Lcg, last_punct: bool, proper: u64, comma: u64) -> String {
    let k = 3 + r.below(4) as usize;
    (0..k).map(|i| sentence_v(r, i + 1 < k || last_punct, proper, comma)).collect::<Vec<_>>().join(" ")
}

/// Greedy wrap at `w` columns; the first line carries `indent` spaces that
/// count toward the column.
pub fn wrap_cols(p: &str, w: usize, indent: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut limit = w - indent;
    for word in p.split_whitespace() {
        let wl = word.chars().count();
        if cur.is_empty() {
            cur.push_str(word);
        } else if cur.chars().count() + 1 + wl <= limit {
            cur.push(' ');
            cur.push_str(word);
        } else {
            out.push(std::mem::take(&mut cur));
            cur.push_str(word);
            limit = w;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if indent > 0 && !out.is_empty() {
        out[0] = format!("{}{}", " ".repeat(indent), out[0]);
    }
    out
}

/// The checker's P1 paragraphs (seed 20261101): 60 documents of 5.
pub fn p1_paragraphs() -> Vec<Vec<String>> {
    let mut r = Lcg(20261101);
    (0..60).map(|_| (0..5).map(|_| paragraph(&mut r, true)).collect()).collect()
}

/// The checker's P1 documents: wrapped at 64 with a first-line indent,
/// paragraphs joined by `sep`.
pub fn p1(indent: usize, sep: &str) -> Vec<String> {
    p1_paragraphs().iter().map(|ps| ps.iter().map(|p| wrap_cols(p, 64, indent).join("\n")).collect::<Vec<_>>().join(sep)).collect()
}

/// P4: every paragraph's last sentence unpunctuated (seed 7).
pub fn p4() -> Vec<String> {
    let mut r = Lcg(7);
    (0..60).map(|_| (0..4).map(|_| wrap_cols(&paragraph(&mut r, false), 64, 0).join("\n")).collect::<Vec<_>>().join("\n\n")).collect()
}

/// P4b: only the first paragraph punctuated (seed 8).
pub fn p4b() -> Vec<String> {
    let mut r = Lcg(8);
    (0..60).map(|_| (0..4).map(|k| wrap_cols(&paragraph(&mut r, k == 0), 64, 0).join("\n")).collect::<Vec<_>>().join("\n\n")).collect()
}

/// P4c: only the last of four paragraphs unpunctuated (seed 10).
pub fn p4c() -> Vec<String> {
    let mut r = Lcg(10);
    (0..60).map(|_| (0..4).map(|k| wrap_cols(&paragraph(&mut r, k < 3), 64, 0).join("\n")).collect::<Vec<_>>().join("\n\n")).collect()
}

/// P1b: indent 2 at width 72 (seed 12).
pub fn p1b() -> Vec<String> {
    let mut r = Lcg(12);
    (0..60).map(|_| (0..5).map(|_| wrap_cols(&paragraph(&mut r, true), 72, 2).join("\n")).collect::<Vec<_>>().join("\n\n")).collect()
}

/// P5: a block wrapped at 40, then three long single-newline paragraphs
/// (seed 9); with the checker's mirror child after each document.
pub fn p5() -> Vec<(String, String)> {
    let mut r = Lcg(9);
    let mut docs = Vec::new();
    for _ in 0..40 {
        let a = wrap_cols(&paragraph(&mut r, true), 40, 0).join("\n");
        let b = (0..3).map(|_| paragraph(&mut r, true)).collect::<Vec<_>>().join("\n");
        docs.push(format!("{a}\n\n{b}"));
    }
    docs.into_iter().map(|d| (d, (0..4).map(|_| sentence(&mut r, true)).collect::<Vec<_>>().join(" "))).collect()
}

/// P2/P3: an email with a greeting and a sign-off around three paragraphs
/// wrapped at 66 (seed 11), and a mirror child carrying its own greeting and
/// sign-off: prose (even i) or a lead-in and a list (odd i).
pub fn p2() -> Vec<(String, String)> {
    let mut r = Lcg(11);
    let mut out = Vec::new();
    for i in 0..40 {
        let body = (0..3).map(|_| wrap_cols(&paragraph(&mut r, true), 66, 0).join("\n")).collect::<Vec<_>>().join("\n\n");
        let parent = format!("Hi Bob,\n\n{body}\n\nThanks,\nSam\n");
        let prose = (0..3).map(|_| paragraph(&mut r, true)).collect::<Vec<_>>().join("\n\n");
        let child = if i % 2 == 0 {
            format!("Hello Bob,\n\n{prose}\n\nBest regards,\nSam")
        } else {
            format!(
                "Hello Bob,\n\n{}\nHere is what we still need before the meeting:\n- the signed contract from the legal team\n- two printed copies of the budget\n- the booking for the large room\n\nBest regards,\nSam",
                paragraph(&mut r, true)
            )
        };
        out.push((parent, child));
    }
    out
}

/// Design B's adversarial generators (seeds 101-113).
pub fn adversarial(name: &str) -> Vec<String> {
    match name {
        // A1: the wrapper counted a 4-space indent, then admission trimmed it.
        "A1" => {
            let mut r = Lcg(101);
            (0..60).map(|_| (0..5).map(|_| trimmed(wrap_cols(&paragraph(&mut r, true), 64, 4))).collect::<Vec<_>>().join("\n\n")).collect()
        }
        "A1b" => {
            let mut r = Lcg(102);
            (0..60).map(|_| (0..5).map(|_| trimmed(wrap_cols(&paragraph_v(&mut r, true, 6, 0), 64, 4))).collect::<Vec<_>>().join("\n\n")).collect()
        }
        // A2: a 4-space indent added after wrapping at 64.
        "A2" => {
            let mut r = Lcg(103);
            (0..60)
                .map(|_| {
                    (0..5)
                        .map(|_| {
                            let mut l = wrap_cols(&paragraph(&mut r, true), 64, 0);
                            l[0] = format!("    {}", l[0]);
                            l.join("\n")
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
                .collect()
        }
        // A2b: a tab indent the wrapper expanded to 8 columns.
        "A2b" => {
            let mut r = Lcg(104);
            (0..60)
                .map(|_| {
                    (0..5)
                        .map(|_| {
                            let mut l = wrap_cols(&paragraph(&mut r, true), 72, 8);
                            l[0] = format!("\t{}", l[0].trim_start());
                            l.join("\n")
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
                .collect()
        }
        // A3: single-newline paragraphs wrapped at 64, last sentences unpunctuated.
        "A3" => {
            let mut r = Lcg(105);
            (0..60).map(|_| (0..4).map(|_| wrap_cols(&paragraph(&mut r, false), 64, 0).join("\n")).collect::<Vec<_>>().join("\n")).collect()
        }
        "A3b" => {
            let mut r = Lcg(106);
            (0..60).map(|_| (0..4).map(|_| wrap_cols(&paragraph_v(&mut r, false, 6, 0), 64, 0).join("\n")).collect::<Vec<_>>().join("\n")).collect()
        }
        // A4: a title line directly above a wrapped paragraph.
        "A4" => {
            let mut r = Lcg(107);
            (0..60)
                .map(|_| {
                    let p = (0..3).map(|_| wrap_cols(&paragraph(&mut r, true), 64, 0).join("\n")).collect::<Vec<_>>().join("\n\n");
                    format!("Notes From the Village Meeting\n{p}")
                })
                .collect()
        }
        // A5: wrapped prose with commas; A5b: two-sentence paragraphs with commas.
        "A5" => {
            let mut r = Lcg(108);
            (0..60).map(|_| (0..5).map(|_| wrap_cols(&paragraph_v(&mut r, true, 0, 8), 64, 0).join("\n")).collect::<Vec<_>>().join("\n\n")).collect()
        }
        "A5b" => {
            let mut r = Lcg(109);
            (0..60)
                .map(|_| {
                    (0..8)
                        .map(|_| {
                            let s = sentence_v(&mut r, true, 0, 6) + " " + &sentence_v(&mut r, true, 0, 6);
                            wrap_cols(&s, 72, 0).join("\n")
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
                .collect()
        }
        // A6: a block wrapped at 40 and one wrapped at 72.
        "A6" => {
            let mut r = Lcg(110);
            (0..40).map(|_| format!("{}\n\n{}", wrap_cols(&paragraph(&mut r, true), 40, 0).join("\n"), wrap_cols(&paragraph(&mut r, true), 72, 0).join("\n"))).collect()
        }
        // A7: a wrapped block, a short one-line block and a long unwrapped one.
        "A7" => {
            let mut r = Lcg(111);
            (0..40)
                .map(|_| format!("{}\n\nSee you all at the meeting on Friday morning.\n\n{}", wrap_cols(&paragraph(&mut r, true), 66, 0).join("\n"), paragraph(&mut r, true)))
                .collect()
        }
        // A8: single-newline paragraphs with a 4-space indent, unpunctuated (A8) or punctuated (A8b).
        "A8" => {
            let mut r = Lcg(112);
            (0..60).map(|_| (0..4).map(|_| wrap_cols(&paragraph(&mut r, false), 64, 4).join("\n")).collect::<Vec<_>>().join("\n")).collect()
        }
        "A8b" => {
            let mut r = Lcg(113);
            (0..60).map(|_| (0..4).map(|_| wrap_cols(&paragraph(&mut r, true), 64, 4).join("\n")).collect::<Vec<_>>().join("\n")).collect()
        }
        _ => panic!("unknown generator {name}"),
    }
}

fn trimmed(mut l: Vec<String>) -> String {
    l[0] = l[0].trim_start().to_string();
    l.join("\n")
}

/// Design B's hand cases (synthetic).
pub const CASES: &[(&str, &str)] = &[
    ("verse_uneven", "The lamps are lit along the quay,\n  the boats are drawn up high,\nthe nets are hung to dry tonight,\n  and gulls go wheeling by.\n\nThe tide comes in across the sand,\n  the wind is soft and low,\nthe harbour bell rings out at nine,\n  and home the sailors go."),
    ("verse_even", "The woods are quiet, dark and deep,\nthe path runs on beneath the hill,\nand all the birds have gone to sleep.\n\nThe moon comes up above the mill,\nthe gate is shut, the yard is still,\nand no one walks the lane tonight."),
    ("verse_modern_unpunct", "we walked out past the station wall\nand down the road to where the river\nbends around the old stone bridge\nand nobody said a word at all"),
    ("titled", "A Walk by the River\nWe walked along the river path until the light began to fade behind the hills.\nThe dogs ran ahead of us and waited at every gate along the way home."),
    ("lead_in", "Here is what we agreed at the meeting:\nWe will move the budget item to the top of the agenda next week.\nThe venue will provide a projector for the afternoon session."),
    ("signed", "The report is attached and the figures for the third quarter are in the second table of it.\n\nSam Smith\nProject Manager, Gas Trading\nNorth Office"),
    ("signoff_long", "The report is attached and the figures for the third quarter are in the second table of it.\n\nThanks for all your help on this,\nJohn Smith"),
    ("unmarked_list", "Bring the signed contract from legal\nTwo printed copies of the budget sheet\nThe booking for the large room upstairs"),
    ("address", "Mr John Smith\n1400 Smith Street\nHouston Texas 77002"),
    ("hand_break", "Thanks for all of\nthe help with the budget."),
    ("prose_then_list", "The club met on Monday and agreed the plan for the spring walks.\nEveryone who comes should bring the following items with them.\n- a waterproof coat\n- sturdy boots\n- a packed lunch\n- the club map"),
    ("raid_short_last", "The museum reopened last week after two years of repairs to the roof and the main hall. \nVisitors queued from early morning, and the staff handed out maps and small guides at the door. \nThe new gallery holds the old maps of the harbour, which had been kept in storage since the flood. \nMost people stayed for an hour or more, and the cafe ran out of sandwiches well before noon.\nWorth a visit!"),
    ("email_greeting", "Hi Dana,\n\nThanks for sending the draft agenda over so quickly. I read through\nit on the train this morning and I think the order works, although\nthe budget item should probably move up so that the finance people\ncan leave early if they need to.\n\nI also spoke to the venue about the projector. They can provide one\nfor the afternoon session, but we would need to bring our own cable\nfor the laptops. Could you check whether the office still has the\nspare adapters from last year?\n\nThanks,\nSam\n"),
    ("hyphen_wrap", "The committee met on Monday to discuss the long-\nterm plan for the harbour and the new bridge, and agreed\nthat the work should start in the spring of next year."),
    ("unpunct_no_periods", "yeah i looked at the numbers again this morning and i think we\nare fine for now but we should talk to the people in the office\nabout the meeting before friday"),
    ("lower_greeting_inherit", "hi bob\nthe report is ready and i will send it over to you this afternoon.\n\nThe committee met on Monday to discuss the plan for the harbour and\nthe new bridge, and agreed that the work should start in the spring\nof next year when the weather is better for the building work."),
    ("indented_wrapped", "  We were glad to hear that the hall is free on the\n  first Saturday of the month, and we would like to\n  book it for the whole afternoon if that is still\n  possible. Please let us know the cost by Friday."),
];
