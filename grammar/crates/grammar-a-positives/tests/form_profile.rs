//! The form profile in the generators' acceptance path (B1b), on synthetic
//! text against a local mock server (no model, detector or network call):
//! - profile off: `generate`'s `Ctx` (no `post`, no `doc_words`) and the
//!   spliced generator's `splice::verdict_fn` give the same journal bytes as
//!   before the profile existed (the four clock fields aside), pinned by a
//!   digest recorded from the pre-profile code;
//! - profile v2: the layout, form, echo and window steps fire in both
//!   generators' acceptance, the accepted text is `grammar-a-form`'s, and the
//!   records hold ids and digests, never text;
//! - a step that is not profile v2's (B1c): the journals are the profile-off
//!   journals plus the step's record, with no `raw_text`;
//! - items with several spans or edits: the window and the form screen read
//!   the final document when the item's last request is judged, which
//!   regenerates within its attempt limit; earlier requests get their own
//!   edit's rules only; resumed runs read the siblings from the journal;
//! - the binaries: flag checks, the dry run's prior and population, the
//!   batch binding (prior record and profile record, refusals), the spliced
//!   prior's population scope, and the item check's wiring.
use grammar_a_positives::generation::{self, Ctx};
use grammar_a_positives::overlap::{Overlap, PriorIndex};
use grammar_a_positives::plan::{self, Request};
use grammar_a_positives::splice::{self, Edit};
use grammar_a_positives::text;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;

/// Synthetic vocabularies (no corpus text).
const SYN: [&str; 20] = [
    "amber", "basalt", "cobalt", "delta", "ember", "fjord", "garnet", "harbor", "indigo", "juniper", "kestrel", "lantern", "meadow",
    "nectar", "orchid", "pebble", "quartz", "river", "saffron", "timber",
];
const ALT: [&str; 20] = [
    "acorn", "bramble", "canyon", "dune", "estuary", "fern", "glacier", "heather", "inlet", "jasper", "knoll", "lichen", "marsh",
    "nettle", "oasis", "prairie", "quarry", "ridge", "sorrel", "thicket",
];

/// `n` sentences of `len` words drawn by a seeded LCG, each capitalised and
/// ending with a full stop, joined by single spaces.
fn prose(vocab: &[&str], seed: u64, n: usize, len: usize) -> String {
    let mut st = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut out: Vec<String> = Vec::new();
    for _ in 0..n {
        let ws: Vec<&str> = (0..len)
            .map(|_| {
                st = st.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                vocab[((st >> 33) as usize) % vocab.len()]
            })
            .collect();
        let s = ws.join(" ");
        out.push(format!("{}{}.", s[..1].to_uppercase(), &s[1..]));
    }
    out.join(" ")
}

/// P1: three paragraphs, about 200 words, curly double quotes in the majority.
fn parent_one() -> String {
    let a = format!("{} The keeper called it \u{201C}amber harbor\u{201D} again. {}", prose(&SYN, 1, 3, 11), prose(&SYN, 2, 2, 10));
    let b = prose(&SYN, 3, 6, 11);
    let c = format!("{} They named the shore \u{201C}quiet fjord\u{201D} later. {}", prose(&SYN, 4, 2, 12), prose(&SYN, 5, 3, 11));
    format!("{a}\n\n{b}\n\n{c}")
}

/// P2: one paragraph of about 160 words.
fn parent_two() -> String {
    prose(&SYN, 7, 14, 12)
}

fn span_of(p: &str, sentence: usize) -> (usize, usize) {
    let s = text::sentences(p)[sentence];
    (s.start, s.end)
}

fn request(id: &str, parent: &str, span: Option<(usize, usize)>, target: usize) -> Request {
    Request {
        request_id: id.into(),
        item_id: format!("item-{id}"),
        parent_id: parent.into(),
        split: "train".into(),
        generator: "g".into(),
        endpoint: "chat".into(),
        template_id: "t".into(),
        prompt: format!("prompt {id}"),
        span_index: 0,
        source_span: span,
        target_words: target,
        cut_budget: None,
        decoding: plan::profile("A", 0.7, 0.9, 1536),
        request_sha256: text::sha_hex(id.as_bytes()),
        seed: 7,
        op: None,
        cohort: None,
    }
}

fn edit(id: &str, kind: &str, span: (usize, usize), sep: Option<&str>, source_words: usize) -> Edit {
    let (lo, hi) = if kind == "insert" { (5.0, 40.0) } else { (source_words as f64 * 0.5, source_words as f64 * 1.5) };
    Edit {
        edit_index: 0,
        component: kind.into(),
        kind: kind.into(),
        operation: kind.into(),
        family: "f".into(),
        operation_requested: kind.into(),
        generator: "g".into(),
        parent_span: span,
        insert_sep: sep.map(String::from),
        sentence_range: (0, 1),
        context_included: false,
        context_spans: Vec::new(),
        style: Value::Null,
        template_id: "t".into(),
        source_words,
        target_words: source_words.max(10),
        len_lo: lo,
        len_hi: hi,
        min_change: None,
        start_rule: "sentence_start".into(),
        end_rule: "terminal".into(),
        newline_ok: false,
        request_id: id.into(),
    }
}

/// The synthetic battery: the parents, `generate`'s (local) requests with
/// their replies, and the spliced edits with theirs.
struct Battery {
    parents: HashMap<String, String>,
    local: Vec<(Request, String)>,
    spliced: Vec<(Request, Edit, String)>,
}

fn battery() -> Battery {
    let p1 = parent_one();
    let p2 = parent_two();
    let w1 = text::words(&p1);
    let w2 = text::words(&p2);
    assert!((150..=450).contains(&w1) && (150..=450).contains(&w2), "{w1} {w2}");
    let s_rewrite = span_of(&p1, 1);
    let s_echo = span_of(&p1, 7);
    let s_list = span_of(&p1, 9);
    let s_quote = span_of(&p1, 12);
    let sw = |sp: (usize, usize)| text::words(&p1[sp.0..sp.1]);
    let local = vec![
        (request("m1", "p1", None, w1), prose(&ALT, 11, 18, 11)),
        (request("m2", "p1", None, w1), p1.clone()),
        (request("s1", "p1", Some(s_rewrite), sw(s_rewrite)), prose(&ALT, 12, 1, sw(s_rewrite))),
        (request("s2", "p1", Some(s_echo), sw(s_echo)), p1[s_echo.0..s_echo.1].to_string()),
        (request("s3", "p1", Some(s_list), sw(s_list)), "- Acorn bramble canyon dune estuary fern.\n- Glacier heather inlet jasper knoll lichen.".into()),
        (request("s4", "p1", Some(s_quote), sw(s_quote)), "The ranger said \"marsh nettle\" and walked on past the oasis ridge.".into()),
        (request("m3", "p2", None, w2), prose(&ALT, 13, 9, 11)),
    ];
    let e_run = span_of(&p1, 14);
    let e_fold = span_of(&p1, 10);
    // the end of P1's first paragraph (sentences 0-5)
    let para_end = span_of(&p1, 5).1;
    let mut spliced = Vec::new();
    for (id, kind, span, sep, reply) in [
        ("e1", "run", e_run, None, prose(&ALT, 21, 1, sw(e_run))),
        ("e2", "insert", (para_end, para_end), Some(" "), prose(&ALT, 22, 1, 12)),
        ("e3", "run", e_fold, None, p1[e_fold.0..e_fold.1].replacen(' ', "  ", 1)),
    ] {
        let words = if kind == "insert" { 0 } else { sw(span) };
        let mut r = request(id, "p1", Some(span), words.max(10));
        r.item_id = format!("spliced-{id}");
        spliced.push((r, edit(id, kind, span, sep, words), reply));
    }
    Battery { parents: [("p1".to_string(), p1), ("p2".to_string(), p2)].into(), local, spliced }
}

fn read_http(s: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let len = head
        .lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.trim().parse::<usize>().ok()))
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Some((head.lines().next()?.to_owned(), buf[head_end..head_end + len].to_vec()))
}

/// A chat server that answers each prompt with its fixed reply until QUIT.
fn serve(replies: HashMap<String, String>) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let h = std::thread::spawn(move || {
        for s in listener.incoming() {
            let mut s = s.unwrap();
            let Some((line, body)) = read_http(&mut s) else { continue };
            if line.starts_with("QUIT") {
                break;
            }
            let b: Value = serde_json::from_slice(&body).unwrap();
            let reply = &replies[b["messages"][0]["content"].as_str().unwrap()];
            let out = serde_json::to_vec(&json!({"choices":[{"message":{"content":reply},"finish_reason":"stop"}],"usage":{},"timings":{}})).unwrap();
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
            s.write_all(head.as_bytes()).unwrap();
            s.write_all(&out).unwrap();
        }
    });
    (format!("http://{addr}"), h)
}

fn quit(endpoint: &str) {
    TcpStream::connect(endpoint.trim_start_matches("http://")).unwrap().write_all(b"QUIT / HTTP/1.1\r\n\r\n").unwrap();
}

fn tokenizer() -> tokenizers::Tokenizer {
    r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#
        .parse()
        .unwrap()
}

fn ctx(endpoint: &str, parents: &HashMap<String, String>, post: Option<Arc<dyn generation::PostProcess>>, doc_words: Option<(usize, usize)>) -> Arc<Ctx> {
    Arc::new(Ctx {
        endpoint: endpoint.into(),
        alias: "m".into(),
        config: json!({}),
        tokenizer: tokenizer(),
        overlap: Overlap::new(PriorIndex::empty()),
        parents: parents.clone(),
        client: generation::client().unwrap(),
        post,
        doc_words,
    })
}

/// The journal lines of one run directory, without the clock fields.
fn journal(dir: &Path) -> Vec<Value> {
    let files = generation::journal_files(dir).unwrap();
    assert_eq!(files.len(), 1);
    std::fs::read_to_string(&files[0])
        .unwrap()
        .lines()
        .map(|l| {
            let mut v: Value = serde_json::from_str(l).unwrap();
            for k in ["t", "t_start", "t_end", "elapsed_ms"] {
                v.as_object_mut().unwrap().remove(k);
            }
            v
        })
        .collect()
}

fn digest(lines: &[Value]) -> String {
    let mut b = Vec::new();
    for v in lines {
        b.extend(serde_json::to_vec(v).unwrap());
        b.push(b'\n');
    }
    text::sha_hex(&b)
}

/// Runs the battery with profile off exactly as `generate` and
/// `slop_ninja_a_spliced generate` built their contexts before the profile:
/// no `post`, no `doc_words`, `splice::verdict_fn`.
fn run_off(b: &Battery, endpoint: &str) -> (Vec<Value>, Vec<Value>) {
    let d1 = tempfile::tempdir().unwrap();
    generation::run(ctx(endpoint, &b.parents, None, None), b.local.iter().map(|(r, _)| (r.clone(), 0)).collect(), d1.path(), 1).unwrap();
    let d2 = tempfile::tempdir().unwrap();
    let edits: HashMap<String, Edit> = b.spliced.iter().map(|(_, e, _)| (e.request_id.clone(), e.clone())).collect();
    generation::run_with(ctx(endpoint, &b.parents, None, None), b.spliced.iter().map(|(r, _, _)| (r.clone(), 0)).collect(), d2.path(), 1, splice::verdict_fn(edits))
        .unwrap();
    (journal(d1.path()), journal(d2.path()))
}

fn replies(b: &Battery) -> HashMap<String, String> {
    b.local.iter().map(|(r, t)| (r.prompt.clone(), t.clone())).chain(b.spliced.iter().map(|(r, _, t)| (r.prompt.clone(), t.clone()))).collect()
}

/// The digest of the profile-off journals, recorded by this harness from
/// origin/main `46b1d878` (before the profile was wired).
const OFF_GOLDEN: &str = "8e440ed18b246b1944d63fed77dca2f1655fd8f648e1b6d29ea99547824ed716";

#[test]
fn profile_off_journals_are_the_pre_profile_bytes() {
    let b = battery();
    let (endpoint, server) = serve(replies(&b));
    let (g, s) = run_off(&b, &endpoint);
    quit(&endpoint);
    server.join().unwrap();
    let all: Vec<Value> = g.iter().chain(&s).cloned().collect();
    let d = digest(&all);
    eprintln!("OFF_DIGEST {d} lines {}", all.len());
    // the battery's own expectations, profile off
    let accepted = |lines: &[Value], id: &str| lines.iter().any(|v| v["kind"] == "response" && v["request_id"] == id && v["verdict"]["accepted"] == true);
    for id in ["m1", "m2", "s1", "s3", "s4", "m3"] {
        assert!(accepted(&g, id), "{id}");
    }
    assert!(!accepted(&g, "s2"));
    for id in ["e1", "e2", "e3"] {
        assert!(accepted(&s, id), "{id}");
    }
    assert_eq!(d, OFF_GOLDEN);
}
// ---------------------------------------------------------------- profile v2 (B1b)

use grammar_a_form::profile::{EditV2, child_v2, item_v2};
use grammar_a_positives::form_post::{self, FormPost};

fn form(b: &Battery, population: &str) -> FormPost {
    let mut ps: Vec<(&str, &str)> = b.parents.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    ps.sort();
    FormPost::new(population, ps.into_iter().map(|(id, t)| (id, "synthetic_domain", t))).unwrap().with_items(Vec::new(), &HashMap::new()).unwrap()
}

fn verdicts(lines: &[Value]) -> HashMap<String, Vec<Value>> {
    let mut m: HashMap<String, Vec<Value>> = HashMap::new();
    for v in lines.iter().filter(|v| v["kind"] == "response") {
        m.entry(v["request_id"].as_str().unwrap().to_string()).or_default().push(v["verdict"].clone());
    }
    m
}

fn reasons(v: &Value) -> Vec<String> {
    v["reasons"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect()
}

/// No record names a sentence of either parent or of any reply.
fn assert_no_text(b: &Battery, rec: &Value) {
    let s = rec.to_string();
    for t in b.parents.values().chain(b.local.iter().map(|(_, t)| t)).chain(b.spliced.iter().map(|(_, _, t)| t)) {
        for sent in text::sentences(t) {
            let x = &t[sent.start..sent.end];
            if text::words(x) >= 4 {
                assert!(!s.contains(x), "record holds text: {x:?}");
            }
        }
    }
}

#[test]
fn profile_v2_layout_form_echo_and_window_fire_in_both_generators() {
    let b = battery();
    let (endpoint, server) = serve(replies(&b));
    // generate, under profile v2
    let fp = form(&b, "train2");
    let ((post, doc_words), vf) = form_post::generate_wiring(Some(fp.clone())).unwrap();
    // the step applies the window itself, on the item's final document
    assert_eq!(doc_words, None);
    let d1 = tempfile::tempdir().unwrap();
    generation::run_with(ctx(&endpoint, &b.parents, post, doc_words), b.local.iter().map(|(r, _)| (r.clone(), 0)).collect(), d1.path(), 1, vf).unwrap();
    // the spliced generator, under profile v2
    let sp = form(&b, "span_select2");
    let edits: HashMap<String, Edit> = b.spliced.iter().map(|(_, e, _)| (e.request_id.clone(), e.clone())).collect();
    let ((post, doc_words), vf) = form_post::spliced_wiring(Some(sp.clone()), edits).unwrap();
    let d2 = tempfile::tempdir().unwrap();
    generation::run_with(ctx(&endpoint, &b.parents, post, doc_words), b.spliced.iter().map(|(r, _, _)| (r.clone(), 0)).collect(), d2.path(), 1, vf).unwrap();
    quit(&endpoint);
    server.join().unwrap();

    let g = verdicts(&journal(d1.path()));
    let accepted = |id: &str| g[id].iter().find(|v| v["accepted"] == true).cloned();
    // accepted texts are grammar-a-form's child_v2 output for the item's conventions
    for (r, reply) in &b.local {
        let p = &b.parents[&r.parent_id];
        let (conv, _, _) = fp.conventions(&r.parent_id, p, &r.item_id).unwrap();
        let want = child_v2(p, r.source_span, &conv, reply.trim());
        match accepted(&r.request_id) {
            Some(v) => {
                assert!(want.reasons.is_empty(), "{} {:?}", r.request_id, want.reasons);
                assert_eq!(v["text"].as_str(), Some(want.text.as_str()), "{}", r.request_id);
                assert_eq!(v["diag"]["post"]["prior_sha256"].as_str(), Some(fp.prior_sha256()));
                assert_eq!(v["diag"]["post"]["profile"], "cc_cohort_profile_v2");
                assert_eq!(v["diag"]["post"]["population"], "train2");
                assert_eq!(v["diag"]["post"]["echo"]["id"], "cc_echo_screen_v1");
                // the verdict keeps the cleaned response the step read
                assert_eq!(v["raw_text"].as_str(), Some(reply.trim()));
            }
            None => assert_eq!(g[&r.request_id].len(), generation::ATTEMPTS as usize, "{}: retried within the attempt limit", r.request_id),
        }
        for v in &g[&r.request_id] {
            assert_no_text(&b, &v["diag"]["post"]);
        }
    }
    for id in ["m1", "s1", "s4"] {
        assert!(accepted(id).is_some(), "{id}: {:?}", g[id]);
    }
    // the layout transfer: a single-paragraph mirror takes P1's three paragraphs
    let m1 = accepted("m1").unwrap();
    assert_eq!(m1["text"].as_str().unwrap().matches("\n\n").count(), 2);
    // conventions: ASCII quotes become the parent's curly ones
    let s4 = accepted("s4").unwrap()["text"].as_str().unwrap().to_string();
    assert!(s4.contains('\u{201C}') && s4.contains('\u{201D}') && !s4.contains('"'), "{s4}");
    // echo: a mirror equal to its parent, and a span equal to its source
    let m2 = reasons(&g["m2"][0]);
    assert!(m2.contains(&"echo_mirror_byte_identical".to_string()) && m2.contains(&"echo_mirror_rendered_byte_identical".to_string()), "{m2:?}");
    let s2 = reasons(&g["s2"][0]);
    for r in ["byte_identical_to_replaced_span", "echo_span_byte_identical", "echo_span_rendered_byte_identical"] {
        assert!(s2.contains(&r.to_string()), "{r} {s2:?}");
    }
    // form: generated list lines in a prose source
    assert!(reasons(&g["s3"][0]).contains(&"form_line_list_like".to_string()), "{:?}", g["s3"][0]);
    // window: a mirror of P2 below 150 words
    assert_eq!(reasons(&g["m3"][0]), vec!["final_document_below_150_words".to_string()]);
    assert_eq!(g["m3"][0]["diag"]["post"]["length_window"]["words"].as_u64(), Some(text::words(&b.local[6].1) as u64));
    assert_eq!(g["m3"][0]["diag"]["post"]["length_window"]["document"], "the laid-out mirror");
    // a span's window reads the parent with the laid-out span
    let s1 = accepted("s1").unwrap();
    let p1 = &b.parents["p1"];
    let s1_doc = generation::assemble(p1, &[b.local[2].0.source_span.unwrap()], &[s1["text"].as_str().unwrap()]).unwrap().0;
    assert_eq!(s1["diag"]["post"]["length_window"]["words"].as_u64(), Some(text::words(&s1_doc) as u64));
    assert!(g.values().flatten().all(|v| v["diag"].get("final_document_words").is_none()));

    let s = verdicts(&journal(d2.path()));
    for (r, e, reply) in &b.spliced {
        let p = &b.parents[&r.parent_id];
        let (conv, _, _) = sp.conventions(&r.parent_id, p, &r.item_id).unwrap();
        let want = item_v2(p, &conv, &[EditV2 { span: e.parent_span, insert_sep: e.insert_sep.as_deref(), raw: reply.trim() }]);
        let got = s[&r.request_id].iter().find(|v| v["accepted"] == true);
        if r.request_id == "e3" {
            assert!(got.is_none());
            let why = reasons(&s["e3"][0]);
            assert!(why.contains(&"echo_span_whitespace_folded_identical".to_string()) && why.contains(&"echo_span_rendered_byte_identical".to_string()), "{why:?}");
        } else {
            let v = got.unwrap_or_else(|| panic!("{}: {:?}", r.request_id, s[&r.request_id]));
            assert!(want.reasons.is_empty(), "{:?}", want.reasons);
            assert_eq!(v["text"].as_str(), Some(want.reps[0].as_str()));
            assert_eq!(v["diag"]["post"]["population"], "span_select2");
            assert_eq!(v["diag"]["post"]["edits"][0]["insertion"], json!(e.kind == "insert"));
            let final_words = text::words(p) - e.source_words + text::words(&want.reps[0]);
            assert_eq!(v["diag"]["post"]["length_window"]["words"].as_u64(), Some(final_words as u64));
            assert_eq!(v["raw_text"].as_str(), Some(reply.trim()));
        }
        for v in &s[&r.request_id] {
            assert_no_text(&b, &v["diag"]["post"]);
        }
    }
}

fn chat(reply: &str) -> Value {
    json!({"choices":[{"message":{"content":reply},"finish_reason":"stop"}]})
}

#[test]
fn profile_off_wiring_is_the_pre_profile_wiring() {
    let b = battery();
    let c = ctx("http://127.0.0.1:9", &b.parents, None, None);
    // generate: no post, no window, and generation::verdict, response for response
    let ((post, doc_words), vf) = form_post::generate_wiring(None).unwrap();
    assert!(post.is_none() && doc_words.is_none());
    for (r, reply) in &b.local {
        assert_eq!(vf(&c, r, 200, &chat(reply)), generation::verdict(&c, r, 200, &chat(reply)));
    }
    let edits: HashMap<String, Edit> = b.spliced.iter().map(|(_, e, _)| (e.request_id.clone(), e.clone())).collect();
    let ((post, doc_words), vf) = form_post::spliced_wiring(None, edits.clone()).unwrap();
    assert!(post.is_none() && doc_words.is_none());
    // the off verdict function is splice::verdict_fn's, response for response
    let base = splice::verdict_fn(edits);
    for (r, _, reply) in &b.spliced {
        assert_eq!(vf(&c, r, 200, &chat(reply)), base(&c, r, 200, &chat(reply)));
    }
}

// ---------------------------------------------------------------- B1c

/// A post step that is not profile v2's (as a v1 normaliser is not): it
/// keeps the text and adds only its record.
struct Identity;

impl generation::PostProcess for Identity {
    fn apply(&self, _r: &Request, _parent: Option<&str>, clean: &str) -> generation::PostOut {
        generation::PostOut { text: clean.to_string(), reasons: Vec::new(), record: json!({"step": "identity"}) }
    }
    fn is_profile_v2(&self) -> bool {
        false
    }
}

#[test]
fn a_step_that_is_not_profile_v2_adds_its_record_and_no_raw_text() {
    // B1c (BUILD-PLAN 10.3, ruling R1): both generators' journals under a
    // step that is not profile v2's are the profile-off journals with the
    // step's record and nothing else; no verdict carries raw_text
    let b = battery();
    let (endpoint, server) = serve(replies(&b));
    let (g_off, s_off) = run_off(&b, &endpoint);
    let post: Option<Arc<dyn generation::PostProcess>> = Some(Arc::new(Identity));
    let d1 = tempfile::tempdir().unwrap();
    generation::run(ctx(&endpoint, &b.parents, post.clone(), None), b.local.iter().map(|(r, _)| (r.clone(), 0)).collect(), d1.path(), 1).unwrap();
    let d2 = tempfile::tempdir().unwrap();
    let edits: HashMap<String, Edit> = b.spliced.iter().map(|(_, e, _)| (e.request_id.clone(), e.clone())).collect();
    generation::run_with(ctx(&endpoint, &b.parents, post, None), b.spliced.iter().map(|(r, _, _)| (r.clone(), 0)).collect(), d2.path(), 1, form_post::spliced_verdict_fn(edits))
        .unwrap();
    quit(&endpoint);
    server.join().unwrap();
    let (g, s) = (journal(d1.path()), journal(d2.path()));
    assert_eq!((g.len(), s.len()), (g_off.len(), s_off.len()));
    let (mut responses, mut stepped) = (0, 0);
    for (on, off) in g.iter().chain(&s).zip(g_off.iter().chain(&s_off)) {
        let mut x = on.clone();
        if x["kind"] == "response" {
            responses += 1;
            assert!(x["verdict"].get("raw_text").is_none(), "{x}");
            if x["verdict"]["diag"]["post"] == json!({"step": "identity"}) {
                stepped += 1;
                x["verdict"]["diag"].as_object_mut().unwrap().remove("post");
            }
        }
        assert_eq!(&x, off);
    }
    // the step ran on every response of the battery
    assert!(responses >= 10 && stepped == responses, "{stepped} of {responses}");
}

#[test]
fn profile_v2_window_rejects_a_spliced_edit_that_leaves_the_window() {
    // 155 words; shortening one 12-word sentence to 6 words leaves 149
    let p3 = format!("{} {}", prose(&SYN, 9, 12, 12), prose(&SYN, 10, 1, 11));
    assert_eq!(text::words(&p3), 155);
    let parents: HashMap<String, String> = [("p3".to_string(), p3.clone())].into();
    let span = span_of(&p3, 4);
    let e = edit("e4", "run", span, None, 12);
    let mut r = request("e4", "p3", Some(span), 12);
    r.item_id = "spliced-e4".into();
    let parsed = json!({"choices":[{"message":{"content": prose(&ALT, 31, 1, 6)},"finish_reason":"stop"}]});
    let edits: HashMap<String, Edit> = [("e4".to_string(), e)].into();
    let ((post, doc_words), vf) = form_post::spliced_wiring(None, edits.clone()).unwrap();
    let off = vf(&ctx("http://127.0.0.1:9", &parents, post, doc_words), &r, 200, &parsed);
    assert_eq!(off["accepted"], true, "{off}");
    let fp = FormPost::new("span_select2", [("p3", "synthetic_domain", p3.as_str())]).unwrap().with_items(Vec::new(), &HashMap::new()).unwrap();
    let ((post, doc_words), vf) = form_post::spliced_wiring(Some(fp), edits).unwrap();
    let on = vf(&ctx("http://127.0.0.1:9", &parents, post, doc_words), &r, 200, &parsed);
    assert_eq!(on["accepted"], false);
    assert_eq!(reasons(&on), vec!["final_document_below_150_words".to_string()]);
    assert_eq!(on["diag"]["post"]["length_window"]["words"].as_u64(), Some(149));
}

#[test]
fn profile_v2_window_reads_the_parent_with_a_local_span() {
    // a parent inside the window whose final document falls below it, and
    // a parent below the window whose final document is inside it
    let p_in = format!("{} {}", prose(&SYN, 41, 12, 12), prose(&SYN, 42, 1, 8));
    assert_eq!(text::words(&p_in), 152);
    let p_out = format!("{} {}", prose(&SYN, 44, 11, 12), prose(&SYN, 45, 2, 8));
    assert_eq!(text::words(&p_out), 148);
    let parents: HashMap<String, String> = [("pi".to_string(), p_in.clone()), ("po".to_string(), p_out.clone())].into();
    let fp = FormPost::new("train2", [("pi", "synthetic_domain", p_in.as_str()), ("po", "synthetic_domain", p_out.as_str())])
        .unwrap()
        .with_items(Vec::new(), &HashMap::new())
        .unwrap();
    let ((post, doc_words), vf) = form_post::generate_wiring(Some(fp)).unwrap();
    let c = ctx("http://127.0.0.1:9", &parents, post, doc_words);
    let r1 = request("w1", "pi", Some(span_of(&p_in, 3)), 12);
    let v1 = vf(&c, &r1, 200, &chat(&prose(&ALT, 43, 1, 7)));
    assert_eq!(reasons(&v1), vec!["final_document_below_150_words".to_string()]);
    assert_eq!(v1["diag"]["post"]["length_window"]["words"].as_u64(), Some(147));
    let r2 = request("w2", "po", Some(span_of(&p_out, 11)), 8);
    let v2 = vf(&c, &r2, 200, &chat(&prose(&ALT, 46, 1, 11)));
    assert_eq!(v2["accepted"], true, "{v2}");
    assert_eq!(v2["diag"]["post"]["length_window"]["words"].as_u64(), Some(151));
}

// ------------------------------------------------- items with several requests

/// A chat server that answers the k-th call with a prompt's k-th reply (the
/// last one once they run out), until QUIT.
fn serve_seq(replies: HashMap<String, Vec<String>>) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let h = std::thread::spawn(move || {
        let mut calls: HashMap<String, usize> = HashMap::new();
        for s in listener.incoming() {
            let mut s = s.unwrap();
            let Some((line, body)) = read_http(&mut s) else { continue };
            if line.starts_with("QUIT") {
                break;
            }
            let b: Value = serde_json::from_slice(&body).unwrap();
            let prompt = b["messages"][0]["content"].as_str().unwrap().to_string();
            let k = calls.entry(prompt.clone()).or_default();
            let list = &replies[&prompt];
            let reply = &list[(*k).min(list.len() - 1)];
            *k += 1;
            let out = serde_json::to_vec(&json!({"choices":[{"message":{"content":reply},"finish_reason":"stop"}],"usage":{},"timings":{}})).unwrap();
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
            s.write_all(head.as_bytes()).unwrap();
            s.write_all(&out).unwrap();
        }
    });
    (format!("http://{addr}"), h)
}

/// Every journal line of a run directory (all files), without the clock fields.
fn journal_all(dir: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    for f in generation::journal_files(dir).unwrap() {
        for l in std::fs::read_to_string(&f).unwrap().lines() {
            let mut v: Value = serde_json::from_str(l).unwrap();
            for k in ["t", "t_start", "t_end", "elapsed_ms"] {
                v.as_object_mut().unwrap().remove(k);
            }
            out.push(v);
        }
    }
    out
}

/// A local two-span item of parent `pid`: its two requests (in item order,
/// one item id) and its spec.
fn two_span_item(pid: &str, p: &str, a: usize, b: usize, target: usize) -> (Request, Request, form_post::ItemSpec) {
    let (sa, sb) = (span_of(p, a), span_of(p, b));
    let mut ra = request("ra", pid, Some(sa), target);
    let mut rb = request("rb", pid, Some(sb), target);
    ra.item_id = "item-two".into();
    rb.item_id = "item-two".into();
    rb.span_index = 1;
    let spec = form_post::ItemSpec { item_id: "item-two".into(), parts: vec![("ra".into(), sa, None), ("rb".into(), sb, None)] };
    (ra, rb, spec)
}

/// Runs `pending` under profile v2 (`generate`'s wiring) with the item
/// `spec` bound and the journal states of `dir` seeded; returns the
/// verdicts by request id.
fn run_items(parents: &HashMap<String, String>, spec: &form_post::ItemSpec, pending: Vec<Request>, replies: HashMap<String, Vec<String>>, dir: &Path, workers: usize) -> HashMap<String, Vec<Value>> {
    let mut ps: Vec<(&str, &str)> = parents.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    ps.sort();
    let states = generation::load_states(&[dir.to_path_buf()]).unwrap();
    let fp = FormPost::new("train2", ps.into_iter().map(|(id, t)| (id, "synthetic_domain", t))).unwrap().with_items(vec![spec.clone()], &states).unwrap();
    let ((post, doc_words), vf) = form_post::generate_wiring(Some(fp)).unwrap();
    let (endpoint, server) = serve_seq(replies);
    generation::run_with(ctx(&endpoint, parents, post, doc_words), pending.into_iter().map(|r| (r, 0)).collect(), dir, workers, vf).unwrap();
    quit(&endpoint);
    server.join().unwrap();
    verdicts(&journal_all(dir))
}

#[test]
fn item_check_reads_the_final_document_at_the_upper_bound() {
    // 445 words; each span grows by 4 words: each partial document holds 449
    // words, the final one 453, which only the last request can see
    let p = format!("{} {}", prose(&SYN, 51, 36, 12), prose(&SYN, 52, 1, 13));
    assert_eq!(text::words(&p), 445);
    let parents: HashMap<String, String> = [("pw".to_string(), p.clone())].into();
    let (ra, rb, spec) = two_span_item("pw", &p, 2, 30, 12);
    let replies: HashMap<String, Vec<String>> = [
        (ra.prompt.clone(), vec![prose(&ALT, 53, 1, 16)]),
        (rb.prompt.clone(), vec![prose(&ALT, 54, 1, 16), prose(&ALT, 55, 1, 12)]),
    ]
    .into();
    let dir = tempfile::tempdir().unwrap();
    let v = run_items(&parents, &spec, vec![ra.clone(), rb.clone()], replies, dir.path(), 1);
    // the first request: its own edit's rules only, no window
    assert_eq!(v["ra"].len(), 1);
    let a = &v["ra"][0];
    assert_eq!(a["accepted"], true, "{a}");
    assert_eq!(a["diag"]["post"]["item_check"]["last"], false);
    assert_eq!(a["diag"]["post"]["item_check"]["pending"], json!(["rb"]));
    assert!(a["diag"]["post"]["length_window"]["words"].is_null());
    // the last request: the final document is over 450 words, so it regenerates, then passes at 449
    assert_eq!(v["rb"].len(), 2);
    let (b0, b1) = (&v["rb"][0], &v["rb"][1]);
    assert_eq!(reasons(b0), vec!["final_document_above_450_words".to_string()], "{b0}");
    assert_eq!(b0["diag"]["post"]["item_check"]["last"], true);
    assert_eq!(b0["diag"]["post"]["length_window"]["words"].as_u64(), Some(453));
    assert_eq!(b1["accepted"], true, "{b1}");
    assert_eq!(b1["diag"]["post"]["length_window"]["words"].as_u64(), Some(449));
    assert_eq!(b1["diag"]["post"]["item_check"]["siblings"][0]["text_is_item_v2s"], true);
    // the accepted texts assemble to that final document, as finalize assembles it
    let fin = generation::assemble(&p, &[ra.source_span.unwrap(), rb.source_span.unwrap()], &[a["text"].as_str().unwrap(), b1["text"].as_str().unwrap()]).unwrap().0;
    assert_eq!(text::words(&fin), 449);
    // the per-request reading would have accepted b's first attempt: its partial document is in the window
    let partial = generation::assemble(&p, &[rb.source_span.unwrap()], &[prose(&ALT, 54, 1, 16).as_str()]).unwrap().0;
    assert_eq!(text::words(&partial), 449);
}

#[test]
fn item_check_accepts_a_span_whose_partial_document_leaves_the_window() {
    // 152 words; the first span shrinks by 4 words and the second grows by
    // 4: the first partial document has 148 words, the final one 152
    let p = format!("{} {}", prose(&SYN, 61, 12, 12), prose(&SYN, 62, 1, 8));
    assert_eq!(text::words(&p), 152);
    let parents: HashMap<String, String> = [("pv".to_string(), p.clone())].into();
    let (ra, rb, spec) = two_span_item("pv", &p, 1, 5, 12);
    let (rep_a, rep_b) = (prose(&ALT, 63, 1, 8), prose(&ALT, 64, 1, 16));
    let partial = generation::assemble(&p, &[ra.source_span.unwrap()], &[rep_a.as_str()]).unwrap().0;
    assert_eq!(text::words(&partial), 148);
    let replies: HashMap<String, Vec<String>> = [(ra.prompt.clone(), vec![rep_a]), (rb.prompt.clone(), vec![rep_b])].into();
    let dir = tempfile::tempdir().unwrap();
    let v = run_items(&parents, &spec, vec![ra, rb], replies, dir.path(), 1);
    assert_eq!(v["ra"][0]["accepted"], true, "{}", v["ra"][0]);
    assert_eq!(v["ra"][0]["diag"]["post"]["length_window"]["document"], "deferred to the item's last request");
    assert_eq!(v["rb"][0]["accepted"], true, "{}", v["rb"][0]);
    assert_eq!(v["rb"][0]["diag"]["post"]["length_window"]["words"].as_u64(), Some(152));
}

#[test]
fn item_check_resumes_from_the_journal_and_serialises_workers() {
    let p = format!("{} {}", prose(&SYN, 71, 14, 12), prose(&SYN, 72, 1, 10));
    let parents: HashMap<String, String> = [("pr".to_string(), p.clone())].into();
    let (ra, rb, spec) = two_span_item("pr", &p, 2, 9, 12);
    let replies = || -> HashMap<String, Vec<String>> { [(ra.prompt.clone(), vec![prose(&ALT, 73, 1, 12)]), (rb.prompt.clone(), vec![prose(&ALT, 74, 1, 12)])].into() };
    // run 1 judges only the first request; run 2, the second, which reads the first from the journal
    let dir = tempfile::tempdir().unwrap();
    let v = run_items(&parents, &spec, vec![ra.clone()], replies(), dir.path(), 1);
    assert_eq!(v["ra"][0]["diag"]["post"]["item_check"]["last"], false);
    let v = run_items(&parents, &spec, vec![rb.clone()], replies(), dir.path(), 1);
    let b = &v["rb"][0];
    assert_eq!(b["accepted"], true, "{b}");
    assert_eq!(b["diag"]["post"]["item_check"]["last"], true);
    let sib = &b["diag"]["post"]["item_check"]["siblings"][0];
    assert_eq!((sib["request_id"].as_str(), sib["text_is_item_v2s"].as_bool()), (Some("ra"), Some(true)));
    assert_eq!(sib["raw_response_sha256"].as_str(), Some(text::sha_hex(prose(&ALT, 73, 1, 12).as_bytes()).as_str()));
    // two workers: the item's requests are judged one at a time, so exactly one is the last
    let dir = tempfile::tempdir().unwrap();
    let v = run_items(&parents, &spec, vec![ra.clone(), rb.clone()], replies(), dir.path(), 2);
    let last: Vec<bool> = ["ra", "rb"].iter().map(|id| v[*id][0]["diag"]["post"]["item_check"]["last"].as_bool().unwrap()).collect();
    assert_eq!(last.iter().filter(|x| **x).count(), 1, "{last:?}");
    // a sibling accepted without its raw text cannot be judged as a whole: the item fails closed
    let mut st = generation::ReqState::default();
    st.intents.push(0);
    st.responses.insert(0, json!({"verdict": {"accepted": true, "text": prose(&ALT, 73, 1, 12)}}));
    let states: HashMap<String, generation::ReqState> = [("ra".to_string(), st)].into();
    let fp = FormPost::new("train2", [("pr", "synthetic_domain", p.as_str())]).unwrap().with_items(vec![spec.clone()], &states).unwrap();
    let o = grammar_a_positives::generation::PostProcess::apply(&fp, &rb, Some(&p), &prose(&ALT, 74, 1, 12));
    assert!(o.reasons.contains(&"item_sibling_raw_missing".to_string()), "{:?}", o.reasons);
    assert_eq!(o.record["item_check"]["raw_missing"], json!(["ra"]));
}

/// The spliced two-edit parent: three paragraphs; the second holds the two
/// edited sentences (1 and 3). The third paragraph ends with a Greek word,
/// so the script is the parent's (the spliced generator rejects a script
/// absent from the parent). `greek` puts three seven-letter Greek words in
/// the second paragraph's sentence 3.
fn spliced_pair_parent(greek: bool) -> String {
    let mid: Vec<String> = (0..6)
        .map(|k| if greek && k == 3 { "Saffron αβγδεζη timber θικλμνξ hall οπρστυφ stood quiet past dusk.".to_string() } else { prose(&SYN, 80 + k, 1, 11) })
        .collect();
    format!("{}\n\n{}\n\n{} The keepers wrote ωμεγα on the quay.", prose(&SYN, 91, 5, 11), mid.join(" "), prose(&SYN, 92, 5, 11))
}

/// Replies of 11 words with four seven-letter Greek words each.
const GREEK_REPLY: &str = "Acorn αβγδεζη bramble θικλμνξ canyon οπρστυφ dune χψωαβγδ estuary fern glacier.";
const GREEK_REPLY_B: &str = "Heather ζηθικλμ inlet νξοπρστ jasper υφχψωαβ knoll γδεζηθι lichen marsh nettle.";

/// A spliced two-edit run's verdicts and the two edits' parent spans.
type PairRun = (HashMap<String, Vec<Value>>, (usize, usize), (usize, usize));

/// The ASCII-letter share of the paragraph of `doc` holding byte `at`.
fn para_ratio(doc: &str, at: usize) -> f64 {
    let a = doc[..at].rfind("\n\n").map(|i| i + 2).unwrap_or(0);
    let b = doc[at..].find("\n\n").map(|i| at + i).unwrap_or(doc.len());
    grammar_a_form::text::ascii_letter_ratio(&doc[a..b])
}

/// Runs a spliced two-edit item of `p` (sentences 7 and 9: the second
/// paragraph's 1 and 3) under profile v2 with per-call replies.
fn run_spliced_pair(p: &str, replies_a: Vec<String>, replies_b: Vec<String>) -> PairRun {
    let parents: HashMap<String, String> = [("ps".to_string(), p.to_string())].into();
    let (sa, sb) = (span_of(p, 6), span_of(p, 8));
    let mut edits = HashMap::new();
    let mut reqs = Vec::new();
    for (i, (id, sp)) in [("xa", sa), ("xb", sb)].into_iter().enumerate() {
        let mut e = edit(id, "run", sp, None, 11);
        e.edit_index = i;
        let mut r = request(id, "ps", Some(sp), 11);
        r.item_id = "spliced-pair".into();
        r.span_index = i;
        edits.insert(id.to_string(), e);
        reqs.push(r);
    }
    let spec = form_post::ItemSpec { item_id: "spliced-pair".into(), parts: vec![("xa".into(), sa, None), ("xb".into(), sb, None)] };
    let fp = FormPost::new("span_select2", [("ps", "synthetic_domain", p)]).unwrap().with_items(vec![spec], &HashMap::new()).unwrap();
    let ((post, doc_words), vf) = form_post::spliced_wiring(Some(fp), edits).unwrap();
    let replies: HashMap<String, Vec<String>> = [(reqs[0].prompt.clone(), replies_a), (reqs[1].prompt.clone(), replies_b)].into();
    let (endpoint, server) = serve_seq(replies);
    let dir = tempfile::tempdir().unwrap();
    generation::run_with(ctx(&endpoint, &parents, post, doc_words), reqs.into_iter().map(|r| (r, 0)).collect(), dir.path(), 1, vf).unwrap();
    quit(&endpoint);
    server.join().unwrap();
    (verdicts(&journal_all(dir.path())), sa, sb)
}

#[test]
fn item_check_screens_the_assembled_spliced_item() {
    // each edit alone keeps the paragraph's ASCII-letter share at or above
    // 0.9; together they take it below, which only the last edit can see
    let p = spliced_pair_parent(false);
    let plain_b = "Bramble canyon dune estuary fern glacier heather inlet jasper knoll lichen.".to_string();
    let (v, sa, sb) = run_spliced_pair(&p, vec![GREEK_REPLY.into()], vec![GREEK_REPLY_B.into(), plain_b.clone()]);
    let one = profile_doc(&p, &[(sa, GREEK_REPLY)]);
    let both = profile_doc(&p, &[(sa, GREEK_REPLY), (sb, GREEK_REPLY_B)]);
    assert!(para_ratio(&one, sa.0) >= 0.9 && para_ratio(&both, sa.0) < 0.9, "{} {}", para_ratio(&one, sa.0), para_ratio(&both, sa.0));
    let a = &v["xa"][0];
    assert_eq!(a["accepted"], true, "{a}");
    assert_eq!(a["diag"]["post"]["item_check"]["deferred_reasons"], json!([]));
    assert_eq!(v["xb"].len(), 2);
    assert_eq!(reasons(&v["xb"][0]), vec!["form_paragraph_non_ascii_letters".to_string()], "{}", v["xb"][0]);
    assert_eq!(v["xb"][0]["diag"]["post"]["item_check"]["last"], true);
    assert_eq!(v["xb"][1]["accepted"], true, "{}", v["xb"][1]);
}

#[test]
fn item_check_does_not_reject_an_edit_on_its_partial_document() {
    // the second paragraph's sentence 3 holds Greek words: the parent
    // passes the screen; the first edit alone takes the paragraph below 0.9;
    // the second edit replaces the Greek sentence, so the item passes
    let p = spliced_pair_parent(true);
    let plain_b = "Bramble canyon dune estuary fern glacier heather inlet jasper knoll lichen.".to_string();
    assert!(para_ratio(&p, span_of(&p, 6).0) >= 0.9);
    let (v, sa, sb) = run_spliced_pair(&p, vec![GREEK_REPLY.into()], vec![plain_b.clone()]);
    let one = profile_doc(&p, &[(sa, GREEK_REPLY)]);
    let both = profile_doc(&p, &[(sa, GREEK_REPLY), (sb, plain_b.as_str())]);
    assert!(para_ratio(&one, sa.0) < 0.9 && para_ratio(&both, sa.0) >= 0.9, "{} {}", para_ratio(&one, sa.0), para_ratio(&both, sa.0));
    let a = &v["xa"][0];
    assert_eq!(a["accepted"], true, "{a}");
    assert_eq!(a["diag"]["post"]["item_check"]["deferred_reasons"], json!(["form_paragraph_non_ascii_letters"]));
    let b = &v["xb"][0];
    assert_eq!(b["accepted"], true, "{b}");
    assert_eq!(b["diag"]["post"]["item_check"]["last"], true);
}

/// `p` with each span replaced (spans ascending).
fn profile_doc(p: &str, edits: &[((usize, usize), &str)]) -> String {
    let spans: Vec<(usize, usize)> = edits.iter().map(|e| e.0).collect();
    let reps: Vec<&str> = edits.iter().map(|e| e.1).collect();
    generation::assemble(p, &spans, &reps).unwrap().0
}

const BIN: &str = env!("CARGO_BIN_EXE_slop_ninja_a_positives");

fn write_humans(path: &Path, split: &str) {
    let mut out = String::new();
    for (d, coll) in [("fw", "fineweb_cc_historical"), ("wp", "wikipedia_enwiki_historical")] {
        for k in 0..8u64 {
            let t = format!("{}\n\n{}", prose(&SYN, 100 + k, 6, 11), prose(&SYN, 200 + k, 7, 11));
            let row = json!({"id":format!("{split}-{d}-{k}"),"frozen_split":split,"origin":"human_only","text":t,
                "text_sha256":text::sha_hex(t.as_bytes()),"word_count":text::words(&t),"collection":coll,
                "provenance":{"title":format!("synthetic {k}")},"source_group":format!("g-{d}-{k}"),"domain":"web"});
            out.push_str(&serde_json::to_string(&row).unwrap());
            out.push('\n');
        }
    }
    std::fs::write(path, out).unwrap();
}

#[test]
fn generate_profile_flags_and_dry_run_prior() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (tr, di) = (d.join("train.jsonl"), d.join("dev-iter.jsonl"));
    write_humans(&tr, "train");
    write_humans(&di, "dev_iter");
    let pdir = d.join("plan");
    let o = std::process::Command::new(BIN)
        .args(["plan-v3", "--train", tr.to_str().unwrap(), "--dev-iter", di.to_str().unwrap(), "--splits", "train,dev_iter", "--out", pdir.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let meta: Value = serde_json::from_slice(&std::fs::read(pdir.join("plan.json")).unwrap()).unwrap();
    let generator = meta["request_files"].as_object().unwrap().keys().next().unwrap().clone();
    let cfg = d.join("cfg.json");
    std::fs::write(&cfg, serde_json::to_vec(&json!({"key": generator, "allowed_splits": ["train", "dev_iter"]})).unwrap()).unwrap();
    // the dry run opens neither the index nor the tokenizer
    let missing = d.join("missing");
    let runs = d.join("runs");
    let run = |extra: &[&str]| {
        std::process::Command::new(BIN)
            .args(["generate", "--plan", pdir.to_str().unwrap(), "--generator", generator.as_str(), "--config", cfg.to_str().unwrap()])
            .args(["--runs", runs.to_str().unwrap(), "--index", missing.to_str().unwrap(), "--tokenizer", missing.to_str().unwrap(), "--dry-run"])
            .args(extra)
            .output()
            .unwrap()
    };
    let err = |o: &std::process::Output| String::from_utf8_lossy(&o.stderr).into_owned();
    // profile off: no profile in the dry run, and `raw` is the same run
    let off = run(&["--splits", "train"]);
    assert!(off.status.success(), "{}", err(&off));
    let raw = run(&["--splits", "train", "--profile", "raw"]);
    assert_eq!(off.stdout, raw.stdout);
    let v: Value = serde_json::from_slice(&off.stdout).unwrap();
    assert!(v.get("profile").is_none());
    // refusals
    for (extra, want) in [
        (&["--splits", "train", "--profile", "cc_cohort_profile_v1"][..], "is not wired"),
        (&["--splits", "train", "--profile", "cc_era_parent_norm_v1"][..], "is not wired"),
        (&["--splits", "train", "--profile", "nope"][..], "unknown profile"),
        (&["--splits", "train", "--profile", "cc_cohort_profile_v2"][..], "needs --profile-population"),
        (&["--splits", "train", "--profile-population", "train2"][..], "needs --profile cc_cohort_profile_v2"),
        (&["--splits", "train", "--profile", "cc_cohort_profile_v2", "--profile-population", "train"][..], "not a round-2 population id"),
        (&["--splits", "train,dev_iter", "--profile", "cc_cohort_profile_v2", "--profile-population", "train2"][..], "exactly one split"),
        (&["--profile", "cc_cohort_profile_v2", "--profile-population", "train2"][..], "exactly one split"),
    ] {
        let o = run(extra);
        assert!(!o.status.success() && err(&o).contains(want), "{extra:?}: {}", err(&o));
    }
    // profile v2: the dry run names the prior of the split's item parents
    let on = run(&["--splits", "train", "--profile", "cc_cohort_profile_v2", "--profile-population", "train2"]);
    assert!(on.status.success(), "{}", err(&on));
    let v: Value = serde_json::from_slice(&on.stdout).unwrap();
    let mut ids: Vec<String> = Vec::new();
    for l in std::fs::read_to_string(pdir.join("items.jsonl")).unwrap().lines() {
        let it: Value = serde_json::from_str(l).unwrap();
        if it["split"] == "train" {
            ids.push(it["parent_id"].as_str().unwrap().to_string());
        }
    }
    ids.sort();
    ids.dedup();
    let p = &v["profile"];
    assert_eq!(p["profile"], "cc_cohort_profile_v2");
    assert_eq!(p["population"], "train2");
    assert_eq!(p["prior_parents"].as_u64(), Some(ids.len() as u64));
    assert_eq!(p["seed"].as_str(), Some(grammar_a_form::profile::norm_prior_seed("train2").as_str()));
    // the dry run prints the full prior record: its bytes hash to the digest every record names
    assert_eq!(v["profile_prior"]["population"], "train2");
    assert_eq!(text::sha_hex(&serde_json::to_vec(&v["profile_prior"]).unwrap()), p["prior_sha256"].as_str().unwrap());
    assert_eq!(v["profile_prior"]["parents"].as_u64(), Some(ids.len() as u64));
    assert!(v["profile_binding"].as_str().unwrap().ends_with("form-profile-split-train.json"));
    // a dry run writes nothing
    assert!(!runs.exists());
    let again: Value = serde_json::from_slice(&run(&["--splits", "train", "--profile", "cc_cohort_profile_v2", "--profile-population", "train2"]).stdout).unwrap();
    assert_eq!(again["profile"]["prior_sha256"], p["prior_sha256"]);
    let other: Value = serde_json::from_slice(&run(&["--splits", "dev_iter", "--profile", "cc_cohort_profile_v2", "--profile-population", "deviter2_t"]).stdout).unwrap();
    assert_ne!(other["profile"]["prior_sha256"], p["prior_sha256"]);
    // the run's population names its seed and its prior record
    assert_eq!(other["profile"]["population"], "deviter2_t");
    assert_eq!(other["profile"]["seed"].as_str(), Some(grammar_a_form::profile::norm_prior_seed("deviter2_t").as_str()));
    assert_eq!(other["profile_prior"]["population"], "deviter2_t");
    // the dry run's other fields are the profile-off run's
    let mut a = v.clone();
    for k in ["profile", "profile_prior", "profile_items", "profile_binding"] {
        a.as_object_mut().unwrap().remove(k);
    }
    assert_eq!(a, serde_json::from_slice::<Value>(&off.stdout).unwrap());
}

/// A llama-server stand-in: `/props`, `/v1/models`, and one fixed chat reply.
fn serve_llama(reply: &str) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let reply = reply.to_string();
    let h = std::thread::spawn(move || {
        for s in listener.incoming() {
            let mut s = s.unwrap();
            let Some((line, _)) = read_http(&mut s) else { continue };
            let out = if line.starts_with("QUIT") {
                break;
            } else if line.starts_with("GET /props") {
                json!({"model_path": "/models/m.gguf", "chat_template": "t"})
            } else if line.starts_with("GET /v1/models") {
                json!({"data": [{"id": "m"}]})
            } else {
                json!({"choices":[{"message":{"content":reply},"finish_reason":"stop"}],"usage":{},"timings":{}})
            };
            let out = serde_json::to_vec(&out).unwrap();
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
            s.write_all(head.as_bytes()).unwrap();
            s.write_all(&out).unwrap();
        }
    });
    (format!("http://{addr}"), h)
}

/// A llama-server stand-in: `/props`, `/v1/models`, and a chat reply per
/// prompt (`replies`, else `default`).
fn serve_llama_map(replies: HashMap<String, String>, default: &str) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let default = default.to_string();
    let h = std::thread::spawn(move || {
        for s in listener.incoming() {
            let mut s = s.unwrap();
            let Some((line, body)) = read_http(&mut s) else { continue };
            let out = if line.starts_with("QUIT") {
                break;
            } else if line.starts_with("GET /props") {
                json!({"model_path": "/models/m.gguf", "chat_template": "t"})
            } else if line.starts_with("GET /v1/models") {
                json!({"data": [{"id": "m"}]})
            } else {
                let b: Value = serde_json::from_slice(&body).unwrap();
                let prompt = b["messages"][0]["content"].as_str().unwrap_or("");
                let reply = replies.get(prompt).cloned().unwrap_or_else(|| default.clone());
                json!({"choices":[{"message":{"content":reply},"finish_reason":"stop"}],"usage":{},"timings":{}})
            };
            let out = serde_json::to_vec(&out).unwrap();
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
            s.write_all(head.as_bytes()).unwrap();
            s.write_all(&out).unwrap();
        }
    });
    (format!("http://{addr}"), h)
}

/// An empty protected index and a tokenizer file for real `generate` runs.
fn index_and_tokenizer(d: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let index = d.join("index");
    std::fs::create_dir_all(&index).unwrap();
    let mut files = serde_json::Map::new();
    for k in ["exact", "normalized", "folded", "shingles_ws", "shingles_folded"] {
        std::fs::write(index.join(format!("{k}.u64")), b"").unwrap();
        files.insert(k.into(), json!({"sha256": text::sha_hex(b"")}));
    }
    std::fs::write(index.join("index.json"), serde_json::to_vec(&json!({"schema": "slop_ninja_a_pool_protected_index_v1", "key_files": files})).unwrap()).unwrap();
    let tok = d.join("tok.json");
    tokenizer().save(&tok, false).unwrap();
    (index, tok)
}

/// Responses of every journal under `dir`.
fn responses_in(dir: &Path) -> Vec<Value> {
    journal_all(dir).into_iter().filter(|v| v["kind"] == "response").collect()
}

/// The single `run-*.json` record of `dir`.
fn run_record(dir: &Path) -> Value {
    let recs: Vec<std::path::PathBuf> =
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.file_name().unwrap().to_str().unwrap().starts_with("run-")).collect();
    assert_eq!(recs.len(), 1, "{recs:?}");
    serde_json::from_slice(&std::fs::read(&recs[0]).unwrap()).unwrap()
}

#[cfg(unix)]
fn mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn generate_binary_wires_the_profile_into_its_context() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (tr, di) = (d.join("train.jsonl"), d.join("dev-iter.jsonl"));
    write_humans(&tr, "train");
    write_humans(&di, "dev_iter");
    let pdir = d.join("plan");
    let o = std::process::Command::new(BIN)
        .args(["plan-v3", "--train", tr.to_str().unwrap(), "--dev-iter", di.to_str().unwrap(), "--splits", "train,dev_iter", "--out", pdir.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let meta: Value = serde_json::from_slice(&std::fs::read(pdir.join("plan.json")).unwrap()).unwrap();
    let generator = meta["request_files"].as_object().unwrap().keys().next().unwrap().clone();
    let (endpoint, server) = serve_llama_map(HashMap::new(), "Acorn bramble canyon dune estuary fern glacier heather inlet jasper.");
    let cfg = d.join("cfg.json");
    std::fs::write(
        &cfg,
        serde_json::to_vec(&json!({"key": generator, "allowed_splits": ["train", "dev_iter"], "endpoint": endpoint, "model_path": "/models/m.gguf", "alias": "m"})).unwrap(),
    )
    .unwrap();
    let (index, tok) = index_and_tokenizer(d);
    let gen_cmd = |runs: &Path, extra: &[&str]| {
        let mut cmd = std::process::Command::new(BIN);
        cmd.args(["generate", "--plan", pdir.to_str().unwrap(), "--generator", generator.as_str(), "--config", cfg.to_str().unwrap()])
            .args(["--runs", runs.to_str().unwrap(), "--index", index.to_str().unwrap(), "--tokenizer", tok.to_str().unwrap()])
            .args(["--limit", "2", "--workers", "1"])
            .args(extra);
        cmd
    };
    let err = |o: &std::process::Output| String::from_utf8_lossy(&o.stderr).into_owned();
    let on_flags = ["--splits", "train", "--profile", "cc_cohort_profile_v2", "--profile-population", "train2"];
    let mut prior = None;
    let mut journals: HashMap<&str, Vec<Value>> = HashMap::new();
    for (name, extra) in [("off", &["--splits", "train"][..]), ("raw", &["--splits", "train", "--profile", "raw"][..]), ("on", &on_flags[..])] {
        let runs = d.join(format!("runs-{name}"));
        if name == "on" {
            let v: Value = serde_json::from_slice(&gen_cmd(&runs, extra).arg("--dry-run").output().unwrap().stdout).unwrap();
            prior = v["profile"]["prior_sha256"].as_str().map(String::from);
            assert!(prior.is_some(), "{v}");
        }
        let o = gen_cmd(&runs, extra).output().unwrap();
        assert!(o.status.success(), "{name}: {}", err(&o));
        let out = runs.join(&generator);
        let lines = journal(&out);
        let responses: Vec<&Value> = lines.iter().filter(|v| v["kind"] == "response").collect();
        assert!(!responses.is_empty());
        let run_rec = run_record(&out);
        for v in &responses {
            if name == "on" {
                assert_eq!(v["verdict"]["diag"]["post"]["profile"], "cc_cohort_profile_v2", "{}", v["verdict"]);
                assert_eq!(v["verdict"]["diag"]["post"]["prior_sha256"].as_str(), prior.as_deref());
                assert_eq!(v["verdict"]["diag"]["post"]["population"], "train2");
                assert!(v["verdict"]["raw_text"].is_string());
            } else {
                assert!(v["verdict"]["diag"].get("post").is_none(), "{}", v["verdict"]);
                assert!(v["verdict"]["diag"].get("final_document_words").is_none());
                assert!(v["verdict"].get("raw_text").is_none());
            }
        }
        if name == "on" {
            assert_eq!(run_rec["profile"]["prior_sha256"].as_str(), prior.as_deref());
            assert_eq!(run_rec["profile"]["population"], "train2");
            // the binding and the prior record, written before the first request, mode 600
            let binding = runs.join("form-profile-split-train.json");
            let b: Value = serde_json::from_slice(&std::fs::read(&binding).unwrap()).unwrap();
            assert_eq!(b["profile"], run_rec["profile"]);
            let pr = runs.join(format!("form-prior-{}.json", prior.as_deref().unwrap()));
            assert_eq!(text::sha_hex(&std::fs::read(&pr).unwrap()), prior.as_deref().unwrap());
            #[cfg(unix)]
            assert_eq!((mode(&binding), mode(&pr)), (0o600, 0o600));
        } else {
            assert!(run_rec.get("profile").is_none());
            assert!(!runs.join("form-profile-split-train.json").exists());
        }
        journals.insert(name, lines);
    }
    // `--profile raw` is the profile-off run, byte for byte (the clock fields aside)
    assert_eq!(journals["off"], journals["raw"]);
    // one batch, one profile record: refusals before any request
    let (on_runs, off_runs) = (d.join("runs-on"), d.join("runs-off"));
    let before = responses_in(&on_runs.join(&generator)).len();
    for (runs, extra, want) in [
        (&on_runs, &["--splits", "train"][..], "profile off"),
        (&on_runs, &["--splits", "train", "--profile", "cc_cohort_profile_v2", "--profile-population", "deviter2_t"][..], "another profile record"),
        (&off_runs, &on_flags[..], "judged with the profile off"),
    ] {
        let o = gen_cmd(runs, extra).output().unwrap();
        assert!(!o.status.success() && err(&o).contains(want), "{extra:?}: {}", err(&o));
        let o = gen_cmd(runs, extra).arg("--dry-run").output().unwrap();
        assert!(!o.status.success() && err(&o).contains(want), "dry {extra:?}: {}", err(&o));
    }
    assert_eq!(responses_in(&on_runs.join(&generator)).len(), before);
    // the same record resumes the batch
    let o = gen_cmd(&on_runs, &on_flags).output().unwrap();
    assert!(o.status.success(), "{}", err(&o));
    // another split is another batch: a deviter2_t run names its population and seed in every record
    let dev = d.join("runs-dev");
    let o = gen_cmd(&dev, &["--splits", "dev_iter", "--profile", "cc_cohort_profile_v2", "--profile-population", "deviter2_t"]).output().unwrap();
    assert!(o.status.success(), "{}", err(&o));
    let rec = run_record(&dev.join(&generator));
    assert_eq!(rec["profile"]["population"], "deviter2_t");
    assert_eq!(rec["profile"]["seed"].as_str(), Some(grammar_a_form::profile::norm_prior_seed("deviter2_t").as_str()));
    let resp = responses_in(&dev.join(&generator));
    assert!(!resp.is_empty());
    for v in &resp {
        assert_eq!(v["split"], "dev_iter");
        assert_eq!(v["verdict"]["diag"]["post"]["population"], "deviter2_t");
        assert_eq!(v["verdict"]["diag"]["post"]["prior_sha256"], rec["profile"]["prior_sha256"]);
    }
    quit(&endpoint);
    server.join().unwrap();
}

#[test]
fn generate_binary_judges_an_item_with_two_spans_on_its_final_document() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let tr = d.join("train.jsonl");
    write_humans(&tr, "train");
    let pdir = d.join("plan");
    let o = std::process::Command::new(BIN).args(["plan-v3", "--train", tr.to_str().unwrap(), "--out", pdir.to_str().unwrap()]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let mut meta: Value = serde_json::from_slice(&std::fs::read(pdir.join("plan.json")).unwrap()).unwrap();
    let generator = meta["request_files"].as_object().unwrap().keys().next().unwrap().clone();
    // make one span item an item with two spans: a second request on another sentence of its parent
    let humans: HashMap<String, String> = std::fs::read_to_string(&tr)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .map(|v| (v["id"].as_str().unwrap().to_string(), v["text"].as_str().unwrap().to_string()))
        .collect();
    let rf = pdir.join("requests").join(format!("{generator}.jsonl"));
    let mut reqs: Vec<Value> = std::fs::read_to_string(&rf).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let mut items: Vec<Value> = std::fs::read_to_string(pdir.join("items.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let k = items
        .iter()
        .position(|it| it["generator"] == generator.as_str() && it["split"] == "train" && it["requests"].as_array().unwrap().len() == 1 && it["source_spans"].as_array().unwrap().len() == 1)
        .expect("a single-span item");
    let rid = items[k]["requests"][0].as_str().unwrap().to_string();
    let s1: (usize, usize) = serde_json::from_value(items[k]["source_spans"][0].clone()).unwrap();
    let parent = &humans[items[k]["parent_id"].as_str().unwrap()];
    let s2 = text::sentences(parent).iter().map(|x| (x.start, x.end)).find(|&(a, b)| b <= s1.0 || a >= s1.1).unwrap();
    let r1 = reqs.iter().position(|r| r["request_id"] == rid.as_str()).unwrap();
    let mut second = reqs[r1].clone();
    let rid2 = format!("{rid}-second");
    second["request_id"] = json!(rid2);
    second["source_span"] = json!([s2.0, s2.1]);
    second["prompt"] = json!(format!("{} (second span)", reqs[r1]["prompt"].as_str().unwrap()));
    second["target_words"] = json!(text::words(&parent[s2.0..s2.1]));
    let (ids, spans) = if s2.0 > s1.0 { (vec![rid.clone(), rid2.clone()], vec![s1, s2]) } else { (vec![rid2.clone(), rid.clone()], vec![s2, s1]) };
    second["span_index"] = json!(ids.iter().position(|x| *x == rid2).unwrap());
    reqs[r1]["span_index"] = json!(ids.iter().position(|x| *x == rid).unwrap());
    reqs.insert(r1 + 1, second);
    items[k]["requests"] = json!(ids);
    items[k]["source_spans"] = json!(spans);
    items[k]["multi_span"] = json!(true);
    let lines = |rows: &[Value]| -> Vec<u8> {
        let mut out = Vec::new();
        for r in rows {
            out.extend(serde_json::to_vec(r).unwrap());
            out.push(b'\n');
        }
        out
    };
    std::fs::write(&rf, lines(&reqs)).unwrap();
    std::fs::write(pdir.join("items.jsonl"), lines(&items)).unwrap();
    meta["request_files"][&generator]["sha256"] = json!(text::sha_hex(&std::fs::read(&rf).unwrap()));
    meta["items_sha256"] = json!(text::sha_hex(&std::fs::read(pdir.join("items.jsonl")).unwrap()));
    std::fs::write(pdir.join("plan.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
    // distinct replies of each request's target length
    let replies: HashMap<String, String> = reqs
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let n = r["target_words"].as_u64().unwrap() as usize;
            (r["prompt"].as_str().unwrap().to_string(), prose(&ALT, 300 + i as u64, n.div_ceil(11).max(1), n.clamp(4, 11)))
        })
        .collect();
    let (endpoint, server) = serve_llama_map(replies, "Acorn bramble canyon dune estuary fern glacier heather inlet jasper.");
    let cfg = d.join("cfg.json");
    std::fs::write(&cfg, serde_json::to_vec(&json!({"key": generator, "allowed_splits": ["train"], "endpoint": endpoint, "model_path": "/models/m.gguf", "alias": "m"})).unwrap())
        .unwrap();
    let (index, tok) = index_and_tokenizer(d);
    let runs = d.join("runs");
    let gen_cmd = || {
        let mut cmd = std::process::Command::new(BIN);
        cmd.args(["generate", "--plan", pdir.to_str().unwrap(), "--generator", generator.as_str(), "--config", cfg.to_str().unwrap()])
            .args(["--runs", runs.to_str().unwrap(), "--index", index.to_str().unwrap(), "--tokenizer", tok.to_str().unwrap()])
            .args(["--splits", "train", "--workers", "1", "--profile", "cc_cohort_profile_v2", "--profile-population", "train2"]);
        cmd
    };
    let dry: Value = serde_json::from_slice(&gen_cmd().arg("--dry-run").output().unwrap().stdout).unwrap();
    assert_eq!(dry["profile_items"], json!({"items": 1, "requests": 2, "accepted_at_start": 0}), "{dry}");
    let o = gen_cmd().output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let resp = responses_in(&runs.join(&generator));
    let first = |id: &str| resp.iter().find(|v| v["request_id"] == id).unwrap_or_else(|| panic!("{id}"))["verdict"].clone();
    let (v0, v1) = (first(&ids[0]), first(&ids[1]));
    // whichever request is judged first gets its own edit's rules; the other, the item's final document
    let (early, last, last_idx) = if v0["diag"]["post"]["item_check"]["last"] == true { (v1, v0, 0) } else { (v0, v1, 1) };
    assert_eq!(early["accepted"], true, "{early}");
    assert_eq!(early["diag"]["post"]["item_check"]["last"], false);
    let lc = &last["diag"]["post"];
    assert_eq!(lc["item_check"]["last"], true, "{lc}");
    let mut reps = [early["text"].as_str().unwrap(), early["text"].as_str().unwrap()];
    reps[last_idx] = last["text"].as_str().unwrap();
    let fin = generation::assemble(parent, &spans, &reps).unwrap().0;
    assert_eq!(lc["length_window"]["words"].as_u64(), Some(text::words(&fin) as u64));
    assert_eq!(lc["length_window"]["document"], "the parent with all of the item's spans or edits (its final document)");
    let rec = run_record(&runs.join(&generator));
    assert_eq!(rec["profile_items"], json!({"items": 1, "requests": 2, "accepted_at_start": 0}));
    quit(&endpoint);
    server.join().unwrap();
}

const SPLICED_BIN: &str = env!("CARGO_BIN_EXE_slop_ninja_a_spliced");

/// A minimal spliced plan directory that `slop_ninja_a_spliced generate`
/// reads: `plan.json` (request file, items, parents and tokenizer digests),
/// `items.jsonl`, `parents.jsonl` and one request file. Population `train`
/// holds the battery's three one-edit items of P1 and a two-edit item of P1
/// (order 3); population `dev_iter_span_eval` holds one item of P2.
fn spliced_plan(d: &Path, b: &Battery, endpoint: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let pdir = d.join("spliced-plan");
    std::fs::create_dir_all(pdir.join("requests")).unwrap();
    let write = |p: &Path, rows: Vec<Value>| -> String {
        let mut out = Vec::new();
        for r in rows {
            out.extend(serde_json::to_vec(&r).unwrap());
            out.push(b'\n');
        }
        std::fs::write(p, &out).unwrap();
        text::sha_hex(&out)
    };
    let p1 = &b.parents["p1"];
    let p2 = &b.parents["p2"];
    let parent = |id: &str, t: &str, split: &str| {
        json!({"id": id, "split": split, "domain": "synthetic_domain", "genre": null, "source_group": format!("g-{id}"), "writer_key": null,
            "cohort_id": "c", "provenance_sha256": null, "text": t, "text_sha256": text::sha_hex(t.as_bytes()), "a_tokens": 0, "words": text::words(t)})
    };
    let parents_sha = write(&pdir.join("parents.jsonl"), vec![parent("p1", p1, "train"), parent("p2", p2, "dev_iter")]);
    let item = |id: &str, pop: &str, pid: &str, t: &str, order: usize, edits: Vec<Value>| {
        json!({"item_id": id, "population": pop, "split": if pop == "train" { "train" } else { "dev_iter" }, "domain": "synthetic_domain", "parent_id": pid,
            "parent_text_sha256": text::sha_hex(t.as_bytes()), "source_group": format!("g-{pid}"), "writer_key": null, "variant": 0, "order": order,
            "order_t": 0.5, "policy": "p", "policy_requested": "p", "policy_fallback_steps": 0, "generators": ["g"], "two_generators": false,
            "voice_scope": "document", "span_half": null, "edits": edits})
    };
    let mut items: Vec<Value> =
        b.spliced.iter().enumerate().map(|(i, (r, e, _))| item(&r.item_id, "train", "p1", p1, i, vec![serde_json::to_value(e).unwrap()])).collect();
    let mut reqs: Vec<Value> = b.spliced.iter().map(|(r, _, _)| serde_json::to_value(r).unwrap()).collect();
    // the two-edit item of P1 and the other population's item of P2
    let mut pair = Vec::new();
    for (i, (id, sent)) in [("e5", 2), ("e6", 4)].into_iter().enumerate() {
        let sp = span_of(p1, sent);
        let mut e = edit(id, "run", sp, None, text::words(&p1[sp.0..sp.1]));
        e.edit_index = i;
        pair.push(serde_json::to_value(&e).unwrap());
        let mut r = request(id, "p1", Some(sp), 12);
        r.item_id = "spliced-pair".into();
        r.span_index = i;
        reqs.push(serde_json::to_value(&r).unwrap());
    }
    items.push(item("spliced-pair", "train", "p1", p1, 3, pair));
    let sp = span_of(p2, 1);
    let mut r = request("e7", "p2", Some(sp), 12);
    r.item_id = "spliced-dev".into();
    r.split = "dev_iter".into();
    reqs.push(serde_json::to_value(&r).unwrap());
    items.push(item("spliced-dev", "dev_iter_span_eval", "p2", p2, 0, vec![serde_json::to_value(edit("e7", "run", sp, None, 12)).unwrap()]));
    let items_sha = write(&pdir.join("items.jsonl"), items);
    let req_sha = write(&pdir.join("requests/g.jsonl"), reqs);
    let tok = d.join("spliced-tok.json");
    tokenizer().save(&tok, false).unwrap();
    let tok_sha = text::sha_hex(&std::fs::read(&tok).unwrap());
    std::fs::write(
        pdir.join("plan.json"),
        serde_json::to_vec(&json!({"request_files": {"g": {"sha256": req_sha}}, "items": {"sha256": items_sha}, "parents": {"sha256": parents_sha},
            "inputs": {"tokenizer": {"path": tok, "sha256": tok_sha}}}))
        .unwrap(),
    )
    .unwrap();
    let cfg = d.join("spliced-cfg.json");
    std::fs::write(&cfg, serde_json::to_vec(&json!({"key": "g", "allowed_splits": ["train"], "endpoint": endpoint, "model_path": "/models/m.gguf", "alias": "m"})).unwrap())
        .unwrap();
    (pdir, cfg)
}

#[test]
fn spliced_binary_wires_the_profile_into_its_context() {
    let b = battery();
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // e1 is accepted under both settings; its post record exists only under the profile
    let reply = b.spliced[0].2.clone();
    let (endpoint, server) = serve_llama(&reply);
    let (pdir, cfg) = spliced_plan(d, &b, &endpoint);
    let (index, _) = index_and_tokenizer(d);
    let on_flags = ["--profile", "cc_cohort_profile_v2", "--profile-population", "span_select2"];
    let cmd = |runs: &Path, extra: &[&str], to: &str, dry: bool| {
        let mut c = std::process::Command::new(SPLICED_BIN);
        c.args(["generate", "--plan", pdir.to_str().unwrap(), "--population", "train", "--generator", "g", "--config", cfg.to_str().unwrap()])
            .args(["--runs", runs.to_str().unwrap(), "--index", index.to_str().unwrap(), "--workers", "1", "--to", to])
            .args(extra);
        if dry {
            c.arg("--dry-run");
        }
        c.output().unwrap()
    };
    let err = |o: &std::process::Output| String::from_utf8_lossy(&o.stderr).into_owned();
    let mut journals: HashMap<&str, Vec<Value>> = HashMap::new();
    for (name, extra) in [("off", &[][..]), ("raw", &["--profile", "raw"][..]), ("on", &on_flags[..])] {
        let runs = d.join(format!("spliced-runs-{name}"));
        let dry: Value = serde_json::from_slice(&cmd(&runs, extra, "1", true).stdout).unwrap();
        let o = cmd(&runs, extra, "1", false);
        assert!(o.status.success(), "{name}: {}", err(&o));
        let lines = journal(&runs.join("g"));
        let responses: Vec<&Value> = lines.iter().filter(|v| v["kind"] == "response").collect();
        assert_eq!(responses.len(), 1);
        let v = &responses[0]["verdict"];
        assert_eq!(v["accepted"], true, "{v}");
        let binding = runs.join("form-profile-population-train.json");
        if name == "on" {
            let prior = dry["profile"]["prior_sha256"].as_str().unwrap();
            assert_eq!(v["diag"]["post"]["prior_sha256"].as_str(), Some(prior));
            assert_eq!(v["diag"]["post"]["population"], "span_select2");
            assert!(v["diag"]["post"]["length_window"]["words"].is_u64());
            assert_eq!(v["raw_text"].as_str(), Some(reply.as_str()));
            // the prior holds the run population's parents only: P1, not the other population's P2
            let only_p1 = FormPost::new("span_select2", [("p1", "synthetic_domain", b.parents["p1"].as_str())]).unwrap();
            assert_eq!(prior, only_p1.prior_sha256());
            assert_eq!(dry["profile"]["prior_parents"].as_u64(), Some(1));
            assert_eq!(dry["profile_prior"]["parents"].as_u64(), Some(1));
            assert_eq!(text::sha_hex(&serde_json::to_vec(&dry["profile_prior"]).unwrap()), prior);
            // the binding and the prior record, mode 600
            let bv: Value = serde_json::from_slice(&std::fs::read(&binding).unwrap()).unwrap();
            assert_eq!(bv["profile"], dry["profile"]);
            let pr = runs.join(format!("form-prior-{prior}.json"));
            assert_eq!(text::sha_hex(&std::fs::read(&pr).unwrap()), prior);
            #[cfg(unix)]
            assert_eq!((mode(&binding), mode(&pr)), (0o600, 0o600));
        } else {
            assert!(dry.get("profile").is_none());
            assert!(v["diag"].get("post").is_none() && v["diag"].get("final_document_words").is_none(), "{v}");
            assert!(!binding.exists());
        }
        journals.insert(name, lines);
    }
    assert_eq!(journals["off"], journals["raw"]);
    let (on_runs, off_runs) = (d.join("spliced-runs-on"), d.join("spliced-runs-off"));
    // the item check covers the two-edit item in the window
    let dry: Value = serde_json::from_slice(&cmd(&on_runs, &on_flags, "10", true).stdout).unwrap();
    assert_eq!(dry["profile_items"], json!({"items": 1, "requests": 2, "accepted_at_start": 0}), "{dry}");
    // one batch, one profile record
    for (runs, extra, want) in [
        (&on_runs, &[][..], "profile off"),
        (&on_runs, &["--profile", "cc_cohort_profile_v2", "--profile-population", "span_test2"][..], "another profile record"),
        (&off_runs, &on_flags[..], "judged with the profile off"),
    ] {
        let o = cmd(runs, extra, "2", false);
        assert!(!o.status.success() && err(&o).contains(want), "{extra:?}: {}", err(&o));
    }
    // a run holds the batch's lock: a second v2 run of the population is refused while it is held
    let held = std::fs::File::open(on_runs.join("form-profile-population-train.json")).unwrap();
    held.lock().unwrap();
    let o = cmd(&on_runs, &on_flags, "2", false);
    assert!(!o.status.success() && err(&o).contains("another profile v2 run"), "{}", err(&o));
    held.unlock().unwrap();
    let o = cmd(&on_runs, &on_flags, "2", false);
    assert!(o.status.success(), "{}", err(&o));
    quit(&endpoint);
    server.join().unwrap();
}
