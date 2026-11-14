//! Rule `x_post_v1` on the free check's input: is the pasted text only the
//! link to a post on X?
//!
//! The free check (`/api/detect/v1/score`) applies it to the envelope's text
//! before the length floor. A match scores nothing and takes no ticket: it
//! answers `x_post_link` with the post's canonical URL while the URL checks
//! and X post checks are both on, and `x_off` otherwise. Anything that does
//! not match is ordinary pasted text, which is always the safe reading: a
//! lone link that is not a post is then simply too short to score.
//!
//! The rule, exactly:
//! - the input, trimmed of surrounding whitespace, is one URL and nothing
//!   else (no whitespace or control character inside it);
//! - scheme `https`, `http` (either in any case) or none, which reads as
//!   `https`;
//! - host `x.com` or `twitter.com`, bare or with `www.` or `mobile.`, in any
//!   case, with an optional trailing dot; no userinfo, and no port other than
//!   the scheme's default written as itself;
//! - path `/<handle>/status/<id>`, `/<handle>/statuses/<id>` or
//!   `/i/web/status/<id>` (`/i/status/<id>` is the first form), with handle
//!   `[A-Za-z0-9_]{1,15}`; further segments (`/photo/1`, `/analytics`) and a
//!   trailing slash are ignored;
//! - `<id>`: 1 to 19 ASCII digits, no leading zero, at most 2^63 - 1;
//! - the query and fragment are dropped.
//!
//! Not a post: every other path on an X host (profiles, search, lists),
//! `t.co` links and mirrors such as `fxtwitter.com`. The canonical form,
//! `https://x.com/i/status/<id>`, names no handle (handles change); the page
//! contract's `PostUrl` builds it from the id.
//!
//! The URL forms are also listed in `tests/fixtures/x-post-v1.json` with the
//! id each one reads as (or none), so the URL route's own reading of a post
//! link can be held to the same table.

/// The hosts of X post links (before a trailing dot is removed).
const HOSTS: &[&str] = &["x.com", "twitter.com", "www.x.com", "www.twitter.com", "mobile.x.com", "mobile.twitter.com"];

/// The longest handle X allows.
pub const MAX_HANDLE: usize = 15;

/// The post id of `input` when it is only a link to a post on X.
pub fn x_post_v1(input: &str) -> Option<u64> {
    let s = input.trim();
    if s.is_empty() || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (rest, default_port) = scheme(s)?;
    let cut = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, after) = rest.split_at(cut);
    if authority.contains('@') {
        return None;
    }
    let host = match authority.rsplit_once(':') {
        Some((h, port)) if port == default_port => h,
        Some(_) => return None,
        None => authority,
    };
    let host = host.strip_suffix('.').unwrap_or(host);
    if !HOSTS.iter().any(|h| h.eq_ignore_ascii_case(host)) {
        return None;
    }
    let path = after.split(['?', '#']).next().unwrap_or("");
    let mut seg = path.strip_prefix('/')?.split('/');
    let (first, second) = (seg.next()?, seg.next()?);
    let id = match (first, second) {
        ("i", "web") => match (seg.next()?, seg.next()?) {
            ("status", id) => id,
            _ => return None,
        },
        (handle, "status" | "statuses") if is_handle(handle) => seg.next()?,
        _ => return None,
    };
    post_id(id)
}

/// The rest after the scheme and the default port of the scheme: `https`,
/// `http` or none (read as `https`).
fn scheme(s: &str) -> Option<(&str, &'static str)> {
    let after = |p: &str| s.get(..p.len()).filter(|h| h.eq_ignore_ascii_case(p)).and_then(|_| s.get(p.len()..));
    if let Some(rest) = after("https://") {
        Some((rest, "443"))
    } else if let Some(rest) = after("http://") {
        Some((rest, "80"))
    } else if s.contains("://") || s.starts_with('/') {
        None
    } else {
        Some((s, "443"))
    }
}

fn is_handle(h: &str) -> bool {
    (1..=MAX_HANDLE).contains(&h.len()) && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// 1 to 19 digits without a leading zero, at most 2^63 - 1.
fn post_id(id: &str) -> Option<u64> {
    let ok = (1..=19).contains(&id.len()) && !id.starts_with('0') && id.bytes().all(|b| b.is_ascii_digit());
    if !ok {
        return None;
    }
    id.parse::<u64>().ok().filter(|v| *v <= i64::MAX as u64)
}

/// X's time-based post ids count milliseconds from this epoch (2010-11-04).
pub const X_EPOCH_MS: u64 = 1_288_834_974_657;
/// X's longest documented edit window (one hour) plus 60 seconds.
pub const X_EDIT_WINDOW_MS: u64 = 3_600_000 + 60_000;

/// Rule `x_age_v1`: when post `id` was created, read from the id itself
/// (`(id >> 22) + X_EPOCH_MS`). An id from before X's time-based ids gives a
/// time at their start, so it counts as old.
pub fn x_created_ms(id: u64) -> u64 {
    (id >> 22).saturating_add(X_EPOCH_MS)
}

/// Rule `x_age_v1`: `Some(after_ms)` when post `id` is still inside its edit
/// window at `now` (a creation time in the future counts as young), with
/// the time from which it can be checked; `None` when it can be checked
/// now. The free embed shows no edit history, so a post is scored only once
/// it can no longer change.
pub fn x_too_new(id: u64, now: u64) -> Option<u64> {
    let after = x_created_ms(id).saturating_add(X_EDIT_WINDOW_MS);
    (now < after).then_some(after)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: u64 = 1_790_000_000_000_000_000;
    /// Userinfo's separator, written apart so no test line reads as an address.
    const AT: char = '@';

    #[test]
    fn every_host_scheme_and_path_form_reads_the_id() {
        let schemes = ["https://", "http://", "HTTPS://", "Http://", ""];
        let hosts = ["x.com", "twitter.com", "www.x.com", "www.twitter.com", "mobile.x.com", "mobile.twitter.com"];
        let paths = [
            format!("/someone/status/{ID}"),
            format!("/someone/statuses/{ID}"),
            format!("/i/status/{ID}"),
            format!("/i/web/status/{ID}"),
            format!("/A_b_9/status/{ID}/photo/1"),
            format!("/someone/status/{ID}/video/1"),
            format!("/someone/status/{ID}/analytics"),
            format!("/someone/status/{ID}/"),
        ];
        let tails = ["", "?s=20&t=abc", "?ref_src=twsrc%5Etfw", "#m", "?s=20#frag"];
        for s in schemes {
            for h in hosts {
                for host in [h.to_owned(), h.to_uppercase(), format!("{h}.")] {
                    for p in &paths {
                        for t in tails {
                            let url = format!("{s}{host}{p}{t}");
                            assert_eq!(x_post_v1(&url), Some(ID), "{url}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_and_inner_text_is_paste() {
        assert_eq!(x_post_v1(&format!("  \n\thttps://x.com/a/status/{ID}\n ")), Some(ID));
        assert_eq!(x_post_v1(&format!("\u{3000}x.com/a/status/{ID}\u{a0}")), Some(ID));
        // A link plus any other text is ordinary pasted text.
        for s in [
            format!("look at this https://x.com/a/status/{ID}"),
            format!("https://x.com/a/status/{ID} wow"),
            format!("https://x.com/a/status/{ID}\nhttps://x.com/a/status/{ID}"),
            format!("https://x.com/a/status/{ID}\u{0}"),
        ] {
            assert_eq!(x_post_v1(&s), None, "{s:?}");
        }
    }

    #[test]
    fn ports_userinfo_and_other_schemes() {
        assert_eq!(x_post_v1(&format!("https://x.com:443/a/status/{ID}")), Some(ID));
        assert_eq!(x_post_v1(&format!("http://x.com:80/a/status/{ID}")), Some(ID));
        assert_eq!(x_post_v1(&format!("x.com:443/a/status/{ID}")), Some(ID));
        for s in [
            format!("https://x.com:80/a/status/{ID}"),
            format!("http://x.com:443/a/status/{ID}"),
            format!("https://x.com:8443/a/status/{ID}"),
            format!("https://x.com:/a/status/{ID}"),
            format!("https://x.com:0443/a/status/{ID}"),
            format!("https://user{AT}x.com/a/status/{ID}"),
            format!("https://user:pw{AT}x.com/a/status/{ID}"),
            format!("ftp://x.com/a/status/{ID}"),
            format!("//x.com/a/status/{ID}"),
            format!("/a/status/{ID}"),
            format!("https:/x.com/a/status/{ID}"),
            format!("https:\\\\x.com\\a\\status\\{ID}"),
        ] {
            assert_eq!(x_post_v1(&s), None, "{s}");
        }
    }

    #[test]
    fn hosts_that_are_not_post_hosts() {
        for h in [
            "t.co",
            "pbs.twimg.com",
            "twimg.com",
            "fxtwitter.com",
            "vxtwitter.com",
            "fixupx.com",
            "nitter.net",
            "notx.com",
            "x.com.evil.example",
            "api.x.com",
            "publish.x.com",
            "m.twitter.com",
            "www.mobile.x.com",
            "x.co",
            "xn--x-com.example",
            "x.com..",
        ] {
            assert_eq!(x_post_v1(&format!("https://{h}/a/status/{ID}")), None, "{h}");
        }
    }

    #[test]
    fn paths_that_are_not_posts() {
        for p in [
            "".to_owned(),
            "/".to_owned(),
            "/someone".to_owned(),
            "/someone/".to_owned(),
            "/someone/likes".to_owned(),
            "/search?q=a".to_owned(),
            "/i/lists/123".to_owned(),
            "/someone/lists".to_owned(),
            "/someone/status".to_owned(),
            "/someone/status/".to_owned(),
            format!("/someone/STATUS/{ID}"),
            format!("/someone//status/{ID}"),
            format!("//someone/status/{ID}"),
            format!("/i/web/statuses/{ID}"),
            format!("/i/web/{ID}"),
            format!("/i/web/status/x/{ID}"),
            format!("/a-b/status/{ID}"),
            format!("/a.b/status/{ID}"),
            format!("/%61/status/{ID}"),
            format!("/{}/status/{ID}", "a".repeat(16)),
            format!("/someone/status/{ID}%20"),
        ] {
            let url = format!("https://x.com{p}");
            assert_eq!(x_post_v1(&url), None, "{url}");
        }
        assert_eq!(x_post_v1(&format!("https://x.com/{}/status/{ID}", "a".repeat(15))), Some(ID));
        assert_eq!(x_post_v1(&format!("https://x.com/_/status/{ID}")), Some(ID));
    }

    #[test]
    fn id_bounds() {
        let url = |id: &str| format!("https://x.com/a/status/{id}");
        assert_eq!(x_post_v1(&url("1")), Some(1));
        assert_eq!(x_post_v1(&url("9223372036854775807")), Some(i64::MAX as u64));
        for id in
            ["0", "01", "9223372036854775808", "18446744073709551615", "12345678901234567890", "12a", "-1", "+1", ""]
        {
            assert_eq!(x_post_v1(&url(id)), None, "{id}");
        }
    }

    #[test]
    fn the_shared_fixture_holds() {
        #[derive(serde::Deserialize)]
        struct Case {
            input: String,
            id: Option<u64>,
        }
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/x-post-v1.json");
        let cases: Vec<Case> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(cases.len() >= 40);
        for c in &cases {
            assert_eq!(x_post_v1(&c.input), c.id, "{:?}", c.input);
        }
        // The canonical URL the free check answers with.
        let pu = slop_ninja_detect_page::PostUrl::new(ID).unwrap();
        assert_eq!(serde_json::to_value(pu).unwrap(), format!("https://x.com/i/status/{ID}"));
    }

    #[test]
    fn x_age_v1_reads_the_time_from_the_id() {
        // An id created at exactly t: (t - epoch) << 22.
        let at = |t: u64| (t - X_EPOCH_MS) << 22;
        let t = 1_795_169_040_000;
        let id = at(t);
        assert_eq!(x_created_ms(id), t);
        // The low 22 bits (worker and sequence) do not move the time.
        assert_eq!(x_created_ms(id | 0x3f_ffff), t);
        // Exactly one hour plus 60 seconds after creation it can be checked.
        assert_eq!(x_too_new(id, t + X_EDIT_WINDOW_MS - 1), Some(t + X_EDIT_WINDOW_MS));
        assert_eq!(x_too_new(id, t + X_EDIT_WINDOW_MS), None);
        assert_eq!(x_too_new(id, t), Some(t + X_EDIT_WINDOW_MS));
        // A creation time in the future counts as young.
        assert_eq!(x_too_new(at(t + 10_000), t), Some(t + 10_000 + X_EDIT_WINDOW_MS));
        // A pre-snowflake id (before 2010) reads as the epoch: old.
        assert_eq!(x_created_ms(20), X_EPOCH_MS);
        assert_eq!(x_too_new(20, t), None);
        // The largest id never overflows.
        assert!(x_too_new(i64::MAX as u64, t).is_some());
    }
}
