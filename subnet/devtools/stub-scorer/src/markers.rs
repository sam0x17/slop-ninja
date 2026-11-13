//! The markers a fixture text carries to choose the stub's answer.
//!
//! A marker is one whitespace-free token `sn-stub:<key>` or
//! `sn-stub:<key>=<value>` anywhere in the text; a trailing `.`, `,`, `;`
//! or `:` is not part of it, so a marker may end a sentence. Unknown keys and
//! bad values are ignored. With no marker the answer is the `weak` band.
//!
//! | Marker | Effect |
//! |---|---|
//! | `band=very_strong\|strong\|unclear\|weak` | the document confidence and share of that band ([`Band`]) |
//! | `conf=<0..100>`, `share=<0..100>` | the exact `ai_confidence_percent` or `ai_share_percent` |
//! | `spans=auto\|none\|off\|bad\|short` | passages: `auto` (default) one passage over every word in a strong band, or the marked regions; `none` an empty list; `off` `span_status` unsupported; `bad` a passage whose byte offsets disagree with its characters; `short` a passage under 24 words |
//! | `span-begin`, `span-end` | a passage region: from the first word after `span-begin` to the last word before the next `span-end` (or the end) |
//! | `span_conf=<0..100>` | each passage's `ai_confidence_percent` (default 99) |
//! | `status=ok\|unsupported_length\|empty_text\|no_word_tokens\|error_tokenize\|error_model` | the document's status |
//! | `http=<status>` | the whole reply answers this status (400, 404, 409, 413, 422, 500, 503) |
//! | `delay_ms=<n>` | the reply waits `n` ms first (at most 300,000) |
//! | `package=other` | the reply names another package |
//! | `reply=garbage\|two\|none` | the reply is not JSON, holds two results, or holds none |

/// The token every marker starts with.
pub const PREFIX: &str = "sn-stub:";

/// The longest delay a marker may ask for.
pub const MAX_DELAY_MS: u64 = 300_000;

/// A document's band, as the detect Worker cuts it: the page contract's
/// band codes, so a `band=` marker takes exactly the page's wire strings.
pub use slop_ninja_detect_page::Band;

/// `(ai_confidence_percent, ai_share_percent)` of a band: each well inside
/// its band of the detect Worker's cuts (99.988..., 90.909..., 50), and
/// `very_strong` above every short-text level.
pub fn band_percents(band: Band) -> (f64, f64) {
    match band {
        Band::VeryStrong => (99.995, 88.0),
        Band::Strong => (95.0, 61.5),
        Band::Unclear => (70.0, 30.0),
        Band::Weak => (12.5, 3.0),
    }
}

/// What the passages do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpanMode {
    #[default]
    Auto,
    None,
    Off,
    Bad,
    Short,
}

/// A document's status other than `ok`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocStatus {
    Ok,
    UnsupportedLength,
    EmptyText,
    NoWordTokens,
    ErrorTokenize,
    ErrorModel,
}

impl DocStatus {
    fn parse(s: &str) -> Option<DocStatus> {
        Some(match s {
            "ok" => DocStatus::Ok,
            "unsupported_length" => DocStatus::UnsupportedLength,
            "empty_text" => DocStatus::EmptyText,
            "no_word_tokens" => DocStatus::NoWordTokens,
            "error_tokenize" => DocStatus::ErrorTokenize,
            "error_model" => DocStatus::ErrorModel,
            _ => return None,
        })
    }
}

/// A fault in the reply's form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyFault {
    Garbage,
    Two,
    None,
}

/// Everything the markers of one text ask for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Directives {
    pub band: Option<Band>,
    pub conf: Option<f64>,
    pub share: Option<f64>,
    pub spans: SpanMode,
    pub span_conf: Option<f64>,
    pub status: Option<DocStatus>,
    pub http: Option<u16>,
    pub delay_ms: Option<u64>,
    pub package_other: bool,
    pub reply: Option<ReplyFault>,
    /// Character ranges `[begin, end)` of the `span-begin` and `span-end`
    /// marker tokens, in text order.
    pub regions: Vec<Region>,
}

/// One marked passage region, as character offsets of its two markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    /// The character just after the `span-begin` marker.
    pub after_begin: usize,
    /// The character where the closing `span-end` marker starts (the text's
    /// length when there is none).
    pub before_end: usize,
}

impl Directives {
    /// `(ai_confidence_percent, ai_share_percent)` for this text.
    pub fn percents(&self) -> (f64, f64) {
        let (c, s) = band_percents(self.band.unwrap_or(Band::Weak));
        (self.conf.unwrap_or(c), self.share.unwrap_or(s))
    }
}

fn percent(v: &str) -> Option<f64> {
    v.parse::<f64>().ok().filter(|p| p.is_finite() && (0.0..=100.0).contains(p))
}

/// The markers in `text`.
pub fn parse(text: &str) -> Directives {
    let mut d = Directives::default();
    let mut open: Option<usize> = None;
    // Character index of each byte offset where a marker starts.
    let mut chars_seen = 0usize;
    let mut last_byte = 0usize;
    for (at, _) in text.match_indices(PREFIX) {
        chars_seen += text[last_byte..at].chars().count();
        last_byte = at;
        let rest = &text[at + PREFIX.len()..];
        let len = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        let token = rest[..len].trim_end_matches(['.', ',', ';', ':']);
        let marker_chars = PREFIX.chars().count() + rest[..len].chars().count();
        let (key, value) = token.split_once('=').unwrap_or((token, ""));
        match key {
            "band" => d.band = Band::parse(value).or(d.band),
            "conf" => d.conf = percent(value).or(d.conf),
            "share" => d.share = percent(value).or(d.share),
            "span_conf" => d.span_conf = percent(value).or(d.span_conf),
            "spans" => {
                d.spans = match value {
                    "auto" => SpanMode::Auto,
                    "none" => SpanMode::None,
                    "off" => SpanMode::Off,
                    "bad" => SpanMode::Bad,
                    "short" => SpanMode::Short,
                    _ => d.spans,
                }
            }
            "status" => d.status = DocStatus::parse(value).or(d.status),
            "http" => d.http = value.parse::<u16>().ok().filter(|s| (100..=599).contains(s)).or(d.http),
            "delay_ms" => d.delay_ms = value.parse::<u64>().ok().map(|v| v.min(MAX_DELAY_MS)).or(d.delay_ms),
            "package" => d.package_other |= value == "other",
            "reply" => {
                d.reply = match value {
                    "garbage" => Some(ReplyFault::Garbage),
                    "two" => Some(ReplyFault::Two),
                    "none" => Some(ReplyFault::None),
                    _ => d.reply,
                }
            }
            "span-begin" => {
                if let Some(b) = open.take() {
                    d.regions.push(Region { after_begin: b, before_end: chars_seen });
                }
                open = Some(chars_seen + marker_chars);
            }
            "span-end" => {
                if let Some(b) = open.take() {
                    d.regions.push(Region { after_begin: b, before_end: chars_seen });
                }
            }
            _ => {}
        }
    }
    if let Some(b) = open {
        d.regions.push(Region { after_begin: b, before_end: text.chars().count() });
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_set_band_values_and_faults() {
        let d = parse("Intro sn-stub:band=strong text sn-stub:share=12.5, more sn-stub:http=503.");
        assert_eq!(d.band, Some(Band::Strong));
        assert_eq!(d.percents(), (95.0, 12.5));
        assert_eq!(d.http, Some(503));
        let d = parse("sn-stub:conf=90.9090909090909 sn-stub:spans=bad sn-stub:delay_ms=999999 sn-stub:package=other sn-stub:reply=two");
        assert_eq!(d.percents().0, 90.9090909090909);
        assert_eq!(d.spans, SpanMode::Bad);
        assert_eq!(d.delay_ms, Some(MAX_DELAY_MS));
        assert!(d.package_other);
        assert_eq!(d.reply, Some(ReplyFault::Two));
    }

    #[test]
    fn no_marker_is_weak_and_bad_values_are_ignored() {
        let d = parse("A plain text with no markers at all.");
        assert_eq!(d, Directives::default());
        assert_eq!(d.percents(), band_percents(Band::Weak));
        let d = parse("sn-stub:band=enormous sn-stub:conf=101 sn-stub:http=99 sn-stub:spans=maybe sn-stub:what");
        assert_eq!(d, Directives::default());
    }

    #[test]
    fn regions_are_character_offsets_between_markers() {
        let t = "é one sn-stub:span-begin two three sn-stub:span-end four sn-stub:span-begin five";
        let d = parse(t);
        assert_eq!(d.regions.len(), 2);
        let chars: Vec<char> = t.chars().collect();
        let s = |r: Region| chars[r.after_begin..r.before_end].iter().collect::<String>();
        assert_eq!(s(d.regions[0]).trim(), "two three");
        assert_eq!(s(d.regions[1]).trim(), "five");
    }

    #[test]
    fn band_values_sit_inside_the_detect_workers_cuts() {
        // The detect Worker's cuts (policy.rs): very strong 99.988..., strong
        // 90.909..., unclear 50; the 24-99-word passage level 96.377....
        assert!(band_percents(Band::VeryStrong).0 >= 99.98820476527482);
        assert!((90.9090909090909..99.98820476527482).contains(&band_percents(Band::Strong).0));
        assert!((50.0..90.9090909090909).contains(&band_percents(Band::Unclear).0));
        assert!(band_percents(Band::Weak).0 < 50.0);
        for &b in Band::ALL {
            let (c, s) = band_percents(b);
            assert!(s <= c, "share never above confidence");
            // Every band code of the page contract is a marker value.
            assert_eq!(parse(&format!("sn-stub:band={b}.")).band, Some(b));
        }
    }
}
