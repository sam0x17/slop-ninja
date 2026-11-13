//! Main-text extraction with dom_smoothie (a Readability port that builds
//! for `wasm32-unknown-unknown`), on stripped HTML that already passed the
//! depth and element bounds ([`crate::strip::shape`]).
//!
//! The output is the article's paragraphs joined by a blank line, with
//! whitespace collapsed inside each (the detect Worker then applies
//! `canon_v1`).

use crate::bound::MAX_ELEMENTS;
use dom_smoothie::{Config, Readability, ReadabilityError, TextMode};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extracted {
    pub text: String,
    pub words: usize,
    pub title: Option<String>,
    pub byline: Option<String>,
    pub site_name: Option<String>,
    pub lang: Option<String>,
    pub published: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtractError {
    TooManyElements,
    /// No readable main text.
    NoContent,
}

/// Extracts the main text of `html`, a page at `url`.
pub fn extract(html: &str, url: &str) -> Result<Extracted, ExtractError> {
    extract_with(html, url, MAX_ELEMENTS)
}

/// [`extract`] with another element bound (0: none; measurements only).
pub fn extract_with(html: &str, url: &str, max_elements: usize) -> Result<Extracted, ExtractError> {
    // JSON-LD is read by the strip pass (its contents never reach the DOM).
    let cfg = Config { max_elements_to_parse: max_elements, text_mode: TextMode::Formatted, disable_json_ld: true, ..Config::default() };
    let mut r = Readability::new(html, Some(url), Some(cfg)).map_err(|_| ExtractError::NoContent)?;
    let a = match r.parse() {
        Ok(a) => a,
        Err(ReadabilityError::TooManyElements(..)) => return Err(ExtractError::TooManyElements),
        Err(_) => return Err(ExtractError::NoContent),
    };
    let text = paragraphs(&a.text_content);
    if text.is_empty() {
        return Err(ExtractError::NoContent);
    }
    let words = text.split_whitespace().count();
    let nonempty = |s: Option<String>| s.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    Ok(Extracted {
        text,
        words,
        title: nonempty(Some(a.title)),
        byline: nonempty(a.byline),
        site_name: nonempty(a.site_name),
        lang: nonempty(a.lang),
        published: nonempty(a.published_time),
    })
}

/// A fixed synthetic page that exercises the strip pass, the shape check and
/// the extractor: common elements, a custom element and attribute (the
/// parser interns unknown names in a global table), JSON-LD and metadata.
const WARM_DOC: &str = "<!doctype html><html lang=\"en\"><head><title>Warm</title>\
    <meta name=\"robots\" content=\"index\"><meta property=\"og:title\" content=\"Warm\">\
    <link rel=\"canonical\" href=\"https://example.com/warm\">\
    <script type=\"application/ld+json\">{\"@type\":\"Article\",\"author\":{\"name\":\"W\"}}</script></head>\
    <body class=\"page\"><nav><a href=\"/\">Home</a></nav><main id=\"main\"><article class=\"post-content entry\">\
    <h1>Warm</h1><p class=\"byline\">By W</p><x-warm data-x-warm=\"1\">custom</x-warm>\
    <p>The harbour keeps its boats, and the evening light settles on the water while the town goes quiet.</p>\
    <figure><img src=\"/a.png\" alt=\"\"><figcaption>Caption</figcaption></figure>\
    <blockquote><p>Quoted words in a block.</p></blockquote><ul><li>One</li><li>Two</li></ul>\
    <table><tr><td>Cell</td></tr></table><pre><code>code</code></pre>\
    <p>Another paragraph with <a href=\"/x\">a link</a>, <em>emphasis</em> and <strong>weight</strong>.<br>Line two.</p>\
    </article><div class=\"comments sidebar\"><p>Comment</p></div></main><footer>Foot</footer></body></html>";

/// A long page with every structure the extractor treats specially (lists
/// with paragraphs, layout and data tables, figures, forms, fonts and line
/// breaks, every heading level, a byline, a dialog, hidden and lazy parts,
/// `javascript:` links), so its first-pass paths and their lazy statics run.
fn warm_long() -> String {
    let para = "<p>The harbour keeps its boats, and the evening light settles on the water while the town goes quiet \
                and the ferry waits by the old stone pier for the last of the travellers.</p>";
    format!(
        "<!doctype html><html lang=\"en\"><head><title>Warm page</title><meta name=\"description\" content=\"d\">\
         <link rel=\"icon\" href=\"/f.ico\"></head><body>\
         <div role=\"dialog\" aria-modal=\"true\"><p>Notice</p></div><div aria-hidden=\"true\">hidden</div>\
         <div class=\"page\"><div id=\"content\" class=\"article-body\">\
         <h1>Warm</h1><h2>Two</h2><h3>Three</h3><h4>Four</h4><h5>Five</h5><h6>Six</h6>\
         <div class=\"byline author\"><a rel=\"author\" href=\"/w\">W</a></div>\
         {p}{p}{p}<p><a href=\"javascript:void(0)\">js</a> <font>font</font> one<br><br><br>two</p>{p}{p}\
         <ul><li><p>{p}</p></li><li>two</li></ul><ol><li>one</li></ol>\
         <table role=\"presentation\"><tr><td>{p}</td></tr></table>\
         <table><caption>Data</caption><thead><tr><th>H</th></tr></thead><tbody><tr><td>1</td><td>2</td></tr></tbody></table>\
         <figure class=\"lazy-image\"><figcaption>Caption</figcaption></figure>\
         <form><fieldset><legend>L</legend>{p}</fieldset></form>\
         <blockquote>{p}</blockquote><pre><code>code</code></pre>\
         <div class=\"share\">Share</div><div><span>{p}</span></div>{p}{p}{p}\
         </div><div class=\"comments sidebar\"><p>Comment</p></div></div></body></html>",
        p = para
    )
}

/// Runs the strip pass, the shape check and the extractor once on
/// [`WARM_DOC`], so the extraction stack's lazy statics are initialized in
/// the scope current when it runs (the Worker's statics home), never in a
/// request's scope.
pub fn warm_up() {
    // The short page takes the extractor's retry paths, the long one its
    // first-pass success path; each initializes different statics.
    let long = warm_long();
    for doc in [WARM_DOC, long.as_str()] {
        let mut s = crate::strip::Stripper::new(64 * 1024, None);
        s.write(doc.as_bytes());
        if let Ok(st) = s.finish() {
            let _ = crate::strip::shape(&st.html, crate::bound::DEPTH_BOUND, MAX_ELEMENTS);
            let _ = extract(&st.html, "https://example.com/warm");
        }
    }
}

/// Paragraphs (runs of non-blank lines) joined by one blank line, with
/// whitespace inside each collapsed to single spaces.
pub fn paragraphs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut para_open = false;
    for line in text.lines() {
        let mut words = line.split_whitespace().peekable();
        if words.peek().is_none() {
            para_open = false;
            continue;
        }
        if para_open {
            out.push(' ');
        } else if !out.is_empty() {
            out.push_str("\n\n");
        }
        for (k, w) in words.enumerate() {
            if k > 0 {
                out.push(' ');
            }
            out.push_str(w);
        }
        para_open = true;
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A synthetic article of `paras` paragraphs of `words` words each.
    pub(crate) fn article(paras: usize, words: usize) -> String {
        let mut body = String::new();
        for p in 0..paras {
            body.push_str("<p>");
            for w in 0..words {
                body.push_str(["the ", "quiet ", "harbour ", "keeps ", "its ", "boats, ", "and ", "evening ", "light. "][(p + w) % 9]);
            }
            body.push_str("</p>\n");
        }
        format!(
            "<!doctype html><html lang=\"en\"><head><title>A synthetic article</title></head><body>\
             <div class=\"menu\"><a href=\"/\">Home</a> <a href=\"/about\">About</a></div>\
             <article class=\"post-content\"><h1>A synthetic article</h1>{body}</article>\
             <div class=\"comments\">Comments are closed.</div></body></html>"
        )
    }

    #[test]
    fn extracts_the_article_as_paragraphs() {
        let e = extract(&article(6, 60), "https://example.com/a").unwrap();
        let paras: Vec<&str> = e.text.split("\n\n").collect();
        assert!(paras.len() >= 6, "{paras:?}");
        assert!(e.text.contains("quiet harbour keeps its boats"));
        assert!(!e.text.contains("Comments are closed"));
        assert!(!e.text.contains("  ") && !e.text.contains("\n\n\n"));
        assert!(e.words >= 360);
        assert_eq!(e.lang.as_deref(), Some("en"));
    }

    #[test]
    fn empty_pages_have_no_content() {
        assert_eq!(extract("<html><body></body></html>", "https://example.com/").unwrap_err(), ExtractError::NoContent);
    }

    #[test]
    fn paragraph_joining() {
        assert_eq!(paragraphs("  a  b \n c\n\n\n d\te \n"), "a b c\n\nd e");
        assert_eq!(paragraphs(""), "");
    }

    /// The extractor survives nesting [`crate::bound::SURVIVED_DEPTH`] deep
    /// on a 1 MiB thread stack (the wasm32 stack's size; native frames are
    /// larger), so [`crate::bound::DEPTH_BOUND`], a quarter of it, keeps a
    /// fourfold margin. Each depth runs in a child process, since a stack
    /// overflow aborts the process.
    #[test]
    fn the_depth_bound_fits_the_stack() {
        const CHILD: &str = "SLOP_NINJA_FETCH_DEPTH_CHILD";
        if let Ok(d) = std::env::var(CHILD) {
            let depth: usize = d.parse().unwrap();
            let h = std::thread::Builder::new()
                .stack_size(1 << 20)
                .spawn(move || {
                    let html = format!(
                        "<html><body>{}<p>{}</p>{}</body></html>",
                        "<div class=\"content\">".repeat(depth),
                        "word ".repeat(400),
                        "</div>".repeat(depth)
                    );
                    let s = crate::strip::shape(&html, usize::MAX, usize::MAX).unwrap();
                    assert!(s.depth >= depth);
                    let _ = extract(&html, "https://example.com/deep");
                })
                .unwrap();
            h.join().unwrap();
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let run = |depth: usize| {
            std::process::Command::new(&exe)
                .args(["--exact", "extract::tests::the_depth_bound_fits_the_stack", "--test-threads=1"])
                .env(CHILD, depth.to_string())
                .output()
                .unwrap()
                .status
                .success()
        };
        assert!(run(crate::bound::SURVIVED_DEPTH), "the extractor must survive SURVIVED_DEPTH on a 1 MiB stack");
        if std::env::var("SLOP_NINJA_FETCH_DEPTH_PROBE").is_ok() {
            // Measurement only: the first depth that fails.
            let mut d = crate::bound::SURVIVED_DEPTH;
            while d <= 1 << 16 && run(d) {
                d *= 2;
            }
            eprintln!("depth probe: survives {}, fails at or below {d}", d / 2);
        }
    }
}
