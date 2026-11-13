//! The fixture catalogue: one host per scenario under [`DOMAIN`], each
//! answering pure functions of the request. Every text is synthetic.
//!
//! | Host | What it serves |
//! |---|---|
//! | `plain` | `robots.txt` allowing all; the articles |
//! | `robots-disallow` | `Disallow: /` for every agent |
//! | `robots-agent` | a `SlopNinjaBot` group that disallows `/article/` but allows `/article/weak`, and an open `*` group |
//! | `robots-wildcards` | `*` and `$` patterns, and an `Allow` that out-matches a `Disallow` |
//! | `robots-404`, `robots-500` | `robots.txt` answering 404 (no rules) or 500 (disallow all) |
//! | `robots-redirect`, `robots-loop` | `robots.txt` redirecting to `plain`'s, or in a loop |
//! | `robots-big` | a `robots.txt` over 500 KiB whose only rule comes after the first 500 KiB |
//! | `robots-slow` | `robots.txt` after 20 s |
//! | `noindex` | `/meta`, `/meta-none`, `/meta-bot` (a `SlopNinjaBot` meta), `/header`, `/header-bot`, `/index` (none) |
//! | `redirects` | `/hop/<n>` (n hops, then an article), `/loop`, `/status/<code>`, `/to-http`, `/to-loopback`, `/to-private`, `/to-ip6`, `/to-other-host`, `/relative`, `/no-location`, `/meta-refresh` |
//! | `deep` | `/nest/<depth>`: the article inside `<depth>` nested `div`s |
//! | `large` | `/bytes/<n>`: an HTML page of exactly n bytes; `/words/<n>`: an article of n words |
//! | `charset` | `/latin1-header`, `/latin1-meta`, `/cp1252-meta`, `/utf8-bom`, `/utf16le-bom`, `/undeclared-latin1`, `/mislabelled` |
//! | `types` | `/pdf`, `/json`, `/png`, `/plain-text`, `/xhtml`, `/no-type` |
//! | `walls` | `/login`, `/paywall`, `/challenge` (403 with `cf-mitigated: challenge`) |
//! | `slow` | `/delay/<ms>`: the article after the delay |
//! | `status` | `/code/<n>`: that status |
//!
//! Every host also serves the articles: `/article/<band>` (`weak`,
//! `unclear`, `strong`, `very_strong`: the stub scorer's band marker in the
//! first paragraph; `none`: no marker), `/article/short` (40 words) and
//! `/canonical` (an article whose canonical link names `plain`'s).

use slop_ninja_detect_page::Band;
use slop_ninja_dev_http::{Body, Call, Reply};

/// The fixture domain: under the reserved `.test` top-level domain, so a
/// fetch that escapes the dev build resolves nowhere.
pub const DOMAIN: &str = "fixtures.test";

/// The fetch Worker's product token.
pub const BOT: &str = "SlopNinjaBot";

/// The longest delay a fixture waits.
pub const MAX_DELAY_MS: u64 = 120_000;
/// The largest generated page.
pub const MAX_BYTES: u64 = 256 << 20;
/// The deepest nesting.
pub const MAX_DEPTH: usize = 100_000;

/// Synthetic prose: plain English, enough function words for the English
/// screen, no quotation of any source.
pub const PARAGRAPHS: &[&str] = &[
    "The town council met on a wet Tuesday evening to talk about the old footbridge over the river. Most of the people in the room had crossed it every day for years, and they wanted to know whether it would be repaired or replaced before the winter floods came.",
    "An engineer from the county explained that the main beams were still sound, but the deck boards had worn thin and some of the railings were loose. She said that a repair would take about six weeks and that the bridge would be closed for most of that time.",
    "Several shop owners on the far bank were worried about the closure. They said that many of their customers walked across the river from the station, and that six weeks without the bridge would cost them a good part of their trade for the season.",
    "The council asked whether the work could be done in two stages, so that one half of the bridge could stay open while the other half was rebuilt. The engineer agreed to look at the idea, though she warned that it might add a week or two to the job.",
    "A teacher from the primary school pointed out that the children used the bridge on their way to the playing fields. She asked for a safe crossing to be marked on the road bridge further down the river while the footbridge was closed, and the council agreed at once.",
    "By the end of the evening there was a rough plan. The repair would start in early spring, after the worst of the floods had passed, and it would be done in two halves. Notices would go up in the shops and at the station a month before the work began.",
    "Some people were still not happy. One man said that the bridge had been patched too many times already and that the town should save up for a new one. Others replied that a new bridge would take years to plan and that the repair was the only thing that could be done now.",
    "The minutes of the meeting were posted on the noticeboard outside the library the next morning, and a short survey was left at the front desk so that anyone who had not been able to attend could still give their view on the plan before it was agreed.",
];

/// The stub scorer's band marker for an article: a band code of the page
/// contract ([`Band`]); anything else (`none`) carries no marker.
fn band_marker(band: &str) -> Option<String> {
    Band::parse(band).map(|b| format!("sn-stub:band={b}"))
}

/// `n` words of the synthetic prose.
pub fn prose_words(n: usize) -> String {
    let mut out: Vec<&str> = Vec::with_capacity(n);
    'outer: loop {
        for p in PARAGRAPHS {
            for w in p.split_whitespace() {
                if out.len() == n {
                    break 'outer;
                }
                out.push(w);
            }
        }
        if n == 0 {
            break;
        }
    }
    out.join(" ")
}

fn article_html(title: &str, marker: Option<&str>, paragraphs: &[String], head_extra: &str) -> String {
    let mut body = String::new();
    for (i, p) in paragraphs.iter().enumerate() {
        match (i, marker) {
            (0, Some(m)) => body.push_str(&format!("<p>{p} {m}</p>\n")),
            _ => body.push_str(&format!("<p>{p}</p>\n")),
        }
    }
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>{title}</title>{head_extra}</head>\n\
         <body><header><nav><a href=\"/\">Home</a></nav></header>\n<main><article><h1>{title}</h1>\n{body}</article></main>\n\
         <footer><p>Fixture page for local tests.</p></footer></body></html>\n"
    )
}

fn paragraphs() -> Vec<String> {
    PARAGRAPHS.iter().map(|p| (*p).to_owned()).collect()
}

/// The article a band names.
pub fn article(band: &str, head_extra: &str) -> String {
    article_html("The footbridge repair", band_marker(band).as_deref(), &paragraphs(), head_extra)
}

fn html(status: u16, s: String) -> Reply {
    Reply::html(status, &s)
}

fn robots(body: &str) -> Reply {
    Reply::text(200, body)
}

/// The scenario label of a fixture host, if it is one.
pub fn label(host: &str) -> Option<&str> {
    host.strip_suffix(DOMAIN)?.strip_suffix('.')
}

fn redirect(status: u16, to: &str) -> Reply {
    Reply::redirect(status, to)
}

fn host_url(label: &str, path: &str) -> String {
    format!("https://{label}.{DOMAIN}{path}")
}

/// Bytes of a Latin-1 string (every char below U+0100).
fn latin1(s: &str) -> Vec<u8> {
    s.chars().map(|c| u32::from(c).min(0xFF) as u8).collect()
}

const LATIN1_TEXT: &str = "Caf\u{e9} cr\u{e8}me and the na\u{ef}ve fa\u{e7}ade of the old station were all part of the plan that the council agreed on.";

/// The answer for a call.
pub fn page(call: &Call) -> Reply {
    let Some(label) = label(&call.host) else {
        return Reply::text(421, "not a fixture host");
    };
    let path = call.path.as_str();
    if call.method != "GET" && call.method != "HEAD" {
        return Reply::text(405, "GET only").with_header("allow", "GET, HEAD");
    }
    if path == "/robots.txt" {
        return robots_txt(label);
    }
    if let Some(r) = common(label, path) {
        return r;
    }
    let tail = |p: &str| path.strip_prefix(p).and_then(|n| n.parse::<u64>().ok());
    match (label, path) {
        ("noindex", "/meta") => html(200, article("strong", "<meta name=\"robots\" content=\"noindex\">")),
        ("noindex", "/meta-none") => html(200, article("strong", "<meta name=\"robots\" content=\"none\">")),
        ("noindex", "/meta-bot") => html(200, article("strong", &format!("<meta name=\"{BOT}\" content=\"noindex, nofollow\">"))),
        ("noindex", "/header") => html(200, article("strong", "")).with_header("x-robots-tag", "noindex"),
        ("noindex", "/header-bot") => html(200, article("strong", "")).with_header("x-robots-tag", &format!("{BOT}: noindex")),
        ("noindex", "/index") => html(200, article("strong", "<meta name=\"robots\" content=\"index, follow\">")),
        ("redirects", p) if p.starts_with("/hop/") => match tail("/hop/") {
            Some(0) => redirect(302, "/article/strong"),
            Some(n) => redirect(302, &format!("/hop/{}", n - 1)),
            None => Reply::text(404, "hop count"),
        },
        ("redirects", "/loop") => redirect(302, "/loop-b"),
        ("redirects", "/loop-b") => redirect(302, "/loop"),
        ("redirects", p) if p.starts_with("/status/") => match tail("/status/") {
            Some(s @ (301 | 302 | 303 | 307 | 308)) => redirect(s as u16, "/article/strong"),
            _ => Reply::text(404, "redirect status"),
        },
        ("redirects", "/to-http") => redirect(302, &format!("http://plain.{DOMAIN}/article/strong")),
        ("redirects", "/to-loopback") => redirect(302, "http://127.0.0.1/"),
        ("redirects", "/to-private") => redirect(302, "http://10.0.0.1/"),
        ("redirects", "/to-ip6") => redirect(302, "http://[::1]/"),
        ("redirects", "/to-other-host") => redirect(302, &host_url("plain", "/article/strong")),
        ("redirects", "/relative") => redirect(302, "../article/strong"),
        ("redirects", "/no-location") => Reply::new(302),
        ("redirects", "/meta-refresh") => html(200, article("none", "<meta http-equiv=\"refresh\" content=\"0; url=/article/strong\">")),
        ("deep", p) if p.starts_with("/nest/") => match tail("/nest/") {
            Some(d) => {
                let d = (d as usize).min(MAX_DEPTH);
                let inner = article("strong", "");
                let (head, rest) = inner.split_once("<main>").unwrap_or(("", &inner));
                html(200, format!("{head}{}<main>{rest}{}", "<div>".repeat(d), "</div>".repeat(d)))
            }
            None => Reply::text(404, "depth"),
        },
        ("large", p) if p.starts_with("/bytes/") => match tail("/bytes/") {
            Some(n) => {
                let n = n.min(MAX_BYTES);
                let mut r = Reply::new(200).with_header("content-type", "text/html; charset=utf-8");
                let head = format!("<!doctype html><html><head><title>Large</title></head><body><p>{} sn-stub:band=strong</p><p>", prose_words(200));
                r.body = Body::Sized { head: head.into_bytes(), unit: b"filler text ".to_vec(), len: n };
                r
            }
            None => Reply::text(404, "size"),
        },
        ("large", p) if p.starts_with("/words/") => match tail("/words/") {
            Some(n) => {
                let words = prose_words((n as usize).min(200_000));
                let paras: Vec<String> = words.split(". ").map(|s| format!("{s}.")).collect::<Vec<_>>().chunks(4).map(|c| c.join(" ")).collect();
                html(200, article_html("A long fixture", Some("sn-stub:band=strong"), &paras, ""))
            }
            None => Reply::text(404, "words"),
        },
        ("charset", "/latin1-header") => Reply::new(200)
            .with_header("content-type", "text/html; charset=ISO-8859-1")
            .with_body(latin1(&article_html("Latin-1", None, &[LATIN1_TEXT.to_owned(), PARAGRAPHS[0].to_owned()], ""))),
        ("charset", "/latin1-meta") => Reply::new(200).with_header("content-type", "text/html").with_body(latin1(
            &article_html("Latin-1 meta", None, &[LATIN1_TEXT.to_owned(), PARAGRAPHS[0].to_owned()], "").replace("<meta charset=\"utf-8\">", "<meta charset=\"iso-8859-1\">"),
        )),
        ("charset", "/cp1252-meta") => {
            let page = article_html("Windows-1252", None, &["QUOTE_OPENsmart quotesQUOTE_CLOSE and an EM dash".to_owned(), PARAGRAPHS[1].to_owned()], "")
                .replace("<meta charset=\"utf-8\">", "<meta http-equiv=\"Content-Type\" content=\"text/html; charset=windows-1252\">");
            let bytes = page.replace("QUOTE_OPEN", "\u{1}").replace("QUOTE_CLOSE", "\u{2}").replace("EM", "\u{3}").into_bytes();
            let bytes = bytes.into_iter().map(|b| match b { 1 => 0x93, 2 => 0x94, 3 => 0x97, b => b }).collect();
            Reply::new(200).with_header("content-type", "text/html").with_body(bytes)
        }
        ("charset", "/utf8-bom") => {
            let mut b = vec![0xEF, 0xBB, 0xBF];
            b.extend_from_slice(article_html("UTF-8 BOM", None, &[LATIN1_TEXT.to_owned(), PARAGRAPHS[2].to_owned()], "").replace("<meta charset=\"utf-8\">", "").as_bytes());
            Reply::new(200).with_header("content-type", "text/html").with_body(b)
        }
        ("charset", "/utf16le-bom") => {
            let page = article_html("UTF-16", None, &[LATIN1_TEXT.to_owned(), PARAGRAPHS[3].to_owned()], "").replace("<meta charset=\"utf-8\">", "");
            let mut b = vec![0xFF, 0xFE];
            for u in page.encode_utf16() {
                b.extend_from_slice(&u.to_le_bytes());
            }
            Reply::new(200).with_header("content-type", "text/html").with_body(b)
        }
        ("charset", "/undeclared-latin1") => Reply::new(200).with_header("content-type", "text/html").with_body(latin1(
            &article_html("Undeclared", None, &[LATIN1_TEXT.to_owned(), PARAGRAPHS[4].to_owned()], "").replace("<meta charset=\"utf-8\">", ""),
        )),
        ("charset", "/mislabelled") => Reply::new(200)
            .with_header("content-type", "text/html; charset=utf-8")
            .with_body(latin1(&article_html("Mislabelled", None, &[LATIN1_TEXT.to_owned(), PARAGRAPHS[5].to_owned()], ""))),
        ("types", "/pdf") => Reply::new(200).with_header("content-type", "application/pdf").with_body(b"%PDF-1.4\n%stub\n".to_vec()),
        ("types", "/json") => Reply::json(200, &serde_json::json!({"text": PARAGRAPHS[0]})),
        ("types", "/png") => Reply::new(200).with_header("content-type", "image/png").with_body(vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        ("types", "/plain-text") => Reply::text(200, &PARAGRAPHS.join("\n\n")),
        ("types", "/xhtml") => Reply::new(200).with_header("content-type", "application/xhtml+xml").with_body(article("strong", "").into_bytes()),
        ("types", "/no-type") => Reply::new(200).with_body(article("strong", "").into_bytes()),
        ("walls", "/login") => html(200, article_html("Sign in to continue", None, &["Sign in to read this article. Log in with your account or create one to continue reading.".to_owned()], "")
            .replace("<main>", "<main><form action=\"/login\" method=\"post\"><input type=\"email\" name=\"email\"><input type=\"password\" name=\"password\"><button>Log in</button></form>")),
        ("walls", "/paywall") => html(200, article_html("Subscribe to keep reading", None, &[PARAGRAPHS[0].to_owned(), "Subscribe to continue reading. You have reached your limit of free articles this month.".to_owned()], "")),
        ("walls", "/challenge") => html(403, "<!doctype html><title>Just a moment...</title><p>Checking your browser before accessing the site.</p>".to_owned())
            .with_header("cf-mitigated", "challenge"),
        ("slow", p) if p.starts_with("/delay/") => match tail("/delay/") {
            Some(ms) => html(200, article("strong", "")).with_delay(ms.min(MAX_DELAY_MS)),
            None => Reply::text(404, "delay"),
        },
        ("status", p) if p.starts_with("/code/") => match tail("/code/") {
            Some(s @ 200..=599) => Reply::text(s as u16, "status fixture"),
            _ => Reply::text(404, "code"),
        },
        _ => Reply::text(404, "no such fixture"),
    }
}

/// Pages every host serves.
fn common(label: &str, path: &str) -> Option<Reply> {
    if let Some(band) = path.strip_prefix("/article/") {
        return Some(match band {
            b if b == "none" || Band::parse(b).is_some() => html(200, article(b, "")),
            "short" => html(200, article_html("A short note", Some("sn-stub:band=very_strong"), &[prose_words(40)], "")),
            _ => Reply::text(404, "no such article"),
        });
    }
    match path {
        "/" => Some(html(
            200,
            format!(
                "<!doctype html><html><head><meta charset=\"utf-8\"><title>{label}</title></head><body><h1>{label}.{DOMAIN}</h1>\
                 <ul><li><a href=\"/article/strong\">strong</a></li><li><a href=\"/article/weak\">weak</a></li></ul></body></html>"
            ),
        )),
        "/canonical" => Some(html(200, article("strong", &format!("<link rel=\"canonical\" href=\"{}\">", host_url("plain", "/article/strong"))))),
        _ => None,
    }
}

fn robots_txt(label: &str) -> Reply {
    match label {
        "robots-disallow" => robots("User-agent: *\nDisallow: /\n"),
        "robots-agent" => robots(&format!(
            "# A group for the fetch Worker's product token.\nUser-agent: {BOT}\nDisallow: /article/\nAllow: /article/weak\n\nUser-agent: *\nAllow: /\n"
        )),
        "robots-wildcards" => robots(&format!(
            "User-agent: {BOT}\nDisallow: /*.pdf$\nDisallow: /article/*_strong\nDisallow: /article/\nAllow: /article/strong\nAllow: /$\n"
        )),
        "robots-404" => Reply::text(404, "not found"),
        "robots-500" => Reply::text(500, "server error"),
        "robots-redirect" => redirect(301, &host_url("plain", "/robots.txt")),
        "robots-loop" => redirect(302, "/robots.txt"),
        "robots-big" => {
            let head = "# Padding before the only rule, which sits past the first 500 KiB.\n";
            let pad_line = "# padding padding padding padding padding padding padding padding\n";
            let mut s = String::from(head);
            while s.len() < 512 * 1024 + 1 {
                s.push_str(pad_line);
            }
            s.push_str("User-agent: *\nDisallow: /\n");
            robots(&s)
        }
        "robots-slow" => robots("User-agent: *\nAllow: /\n").with_delay(20_000),
        _ => robots("User-agent: *\nAllow: /\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(host: &str, path: &str) -> Reply {
        page(&Call::new("GET", path).host(host))
    }

    fn h(label: &str) -> String {
        format!("{label}.{DOMAIN}")
    }

    #[test]
    fn hosts_route_by_label_and_only_fixture_hosts_answer() {
        assert_eq!(get("example.com", "/").status, 421);
        assert_eq!(get(DOMAIN, "/").status, 421);
        assert_eq!(get(&h("plain"), "/robots.txt").body_text(), "User-agent: *\nAllow: /\n");
        assert_eq!(get(&h("robots-disallow"), "/robots.txt").body_text(), "User-agent: *\nDisallow: /\n");
        assert_eq!(get(&h("robots-404"), "/robots.txt").status, 404);
        assert_eq!(get(&h("robots-500"), "/robots.txt").status, 500);
        assert_eq!(get(&h("robots-redirect"), "/robots.txt").header("location"), Some("https://plain.fixtures.test/robots.txt"));
        let big = get(&h("robots-big"), "/robots.txt").body_text();
        assert!(big.len() > 512 * 1024 && big.find("Disallow: /").is_some_and(|i| i > 500 * 1024));
        assert_eq!(get(&h("robots-slow"), "/robots.txt").delay_ms, 20_000);
        assert!(get(&h("robots-agent"), "/robots.txt").body_text().contains("User-agent: SlopNinjaBot"));
        assert_eq!(page(&Call::new("POST", "/").host(&h("plain"))).status, 405);
    }

    #[test]
    fn articles_carry_the_stub_scorer_band() {
        let a = get(&h("plain"), "/article/strong").body_text();
        assert!(a.contains("sn-stub:band=strong") && a.contains("<article>"));
        assert!(!get(&h("plain"), "/article/none").body_text().contains("sn-stub:"));
        // One article per band code of the page contract.
        for b in Band::ALL {
            assert!(get(&h("plain"), &format!("/article/{b}")).body_text().contains(&format!("sn-stub:band={b}")), "{b}");
        }
        assert_eq!(get(&h("plain"), "/article/enormous").status, 404);
        let words: usize = PARAGRAPHS.iter().map(|p| p.split_whitespace().count()).sum();
        assert!(words >= 300, "{words} words: inside the measured range");
        assert_eq!(prose_words(40).split_whitespace().count(), 40);
        assert_eq!(prose_words(1000).split_whitespace().count(), 1000);
        assert!(get(&h("deep"), "/canonical").body_text().contains("rel=\"canonical\" href=\"https://plain.fixtures.test/article/strong\""));
    }

    #[test]
    fn noindex_and_redirect_fixtures() {
        assert!(get(&h("noindex"), "/meta").body_text().contains("content=\"noindex\""));
        assert_eq!(get(&h("noindex"), "/header").header("x-robots-tag"), Some("noindex"));
        assert_eq!(get(&h("noindex"), "/header-bot").header("x-robots-tag"), Some("SlopNinjaBot: noindex"));
        assert_eq!(get(&h("redirects"), "/hop/3").header("location"), Some("/hop/2"));
        assert_eq!(get(&h("redirects"), "/hop/0").header("location"), Some("/article/strong"));
        assert_eq!(get(&h("redirects"), "/status/308").status, 308);
        assert_eq!(get(&h("redirects"), "/to-http").header("location"), Some("http://plain.fixtures.test/article/strong"));
        assert_eq!(get(&h("redirects"), "/to-loopback").header("location"), Some("http://127.0.0.1/"));
        assert!(get(&h("redirects"), "/no-location").header("location").is_none());
    }

    #[test]
    fn deep_large_charset_and_type_fixtures() {
        let d = get(&h("deep"), "/nest/500").body_text();
        assert_eq!(d.matches("<div>").count(), 500);
        let big = get(&h("large"), "/bytes/2621440");
        assert_eq!(big.body.len(), 2_621_440);
        let small = get(&h("large"), "/bytes/100").body.to_vec();
        assert_eq!(small.len(), 100);
        assert!(get(&h("large"), "/words/1500").body_text().contains("sn-stub:band=strong"));
        let l1 = get(&h("charset"), "/latin1-header");
        assert_eq!(l1.header("content-type"), Some("text/html; charset=ISO-8859-1"));
        let b = l1.body.to_vec();
        assert!(b.windows(4).any(|w| w == b"Caf\xe9") && std::str::from_utf8(&b).is_err());
        assert!(get(&h("charset"), "/cp1252-meta").body.to_vec().contains(&0x93));
        assert_eq!(&get(&h("charset"), "/utf16le-bom").body.to_vec()[..2], &[0xFF, 0xFE]);
        assert_eq!(&get(&h("charset"), "/utf8-bom").body.to_vec()[..3], &[0xEF, 0xBB, 0xBF]);
        assert_eq!(get(&h("types"), "/pdf").header("content-type"), Some("application/pdf"));
        assert!(get(&h("types"), "/no-type").header("content-type").is_none());
        assert_eq!(get(&h("walls"), "/challenge").header("cf-mitigated"), Some("challenge"));
        assert_eq!(get(&h("slow"), "/delay/999999").delay_ms, MAX_DELAY_MS);
        assert_eq!(get(&h("status"), "/code/503").status, 503);
    }
}
