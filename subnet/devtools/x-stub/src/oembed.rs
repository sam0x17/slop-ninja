//! The stub's oEmbed answers and its switches, as pure functions of the
//! state and the call.
//!
//! Every post is synthetic: ids are whatever the test names, and the text,
//! display name and handle are the stub's own. The bodies follow the shape
//! X documents for its oEmbed endpoint (a `rich` object whose `html` is a
//! `blockquote.twitter-tweet` with the post's text in its first `<p>`, then
//! the author line and the date link), until the staging check records the
//! real structure.
//!
//! What a post answers ([`Answer`]) is chosen per id by `POST
//! /__x/switch {"id": ..., "answer": ...}`, for every id by `{"all": ...}`,
//! and for one id from a given time by `{"id": ..., "gone_at_ms": ...}`; an
//! id with no switch answers [`Answer::Plain`]. The two control posts answer
//! present (`LIVE_CONTROL`) and JSON 404 (`GONE_CONTROL`) unless a switch
//! says otherwise. `POST /__x/reset` clears the switches and the
//! counters; `GET /__x/state` reads them.

use serde::{Deserialize, Serialize};
use serde_json::json;
use slop_ninja_dev_http::{Call, Reply, escape};
use std::collections::BTreeMap;

/// oEmbed's real host.
pub const HOST: &str = "publish.x.com";
/// oEmbed's path.
pub const PATH: &str = "/oembed";
/// The stub's synthetic author.
pub const AUTHOR_NAME: &str = "Dev Poster";
pub const AUTHOR_HANDLE: &str = "devposter";
/// The size of an [`Answer::Oversize`] body: far past any oEmbed answer.
pub const OVERSIZE_BYTES: usize = 256 * 1024;
/// The longest delay a switch may ask for.
pub const MAX_DELAY_MS: u64 = 120_000;

/// One post's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// A plain post the embed shows in full (30-odd words, with a stub
    /// scorer marker).
    Plain,
    /// A reply: the text starts with two @mention anchors.
    Reply,
    /// A post with a trailing media link (`pic.x.com/...`).
    Media,
    /// A quote post: a trailing link to the quoted post.
    Quote,
    /// A long post the embed cut: the text ends in "…".
    Long,
    /// A post under 24 words.
    Short,
    /// oEmbed's JSON 404 (a deleted or unknown post).
    Gone,
    /// oEmbed's JSON 403 (a protected or withheld post).
    Forbidden,
    /// An HTML 403 (X's edge refusing the caller).
    Html403,
    /// 429.
    Busy,
    /// 503.
    Error,
    /// A plain post after a delay (`{"slow_ms": n}` on the switch).
    Slow,
    /// A body far past any oEmbed answer.
    Oversize,
    /// Markup the text rule does not know: a `<span>` inside the `<p>`.
    Changed,
}

/// The stub's state: switches and counters.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Per-id answers.
    pub answers: BTreeMap<String, Answer>,
    /// One answer for every id, controls included.
    pub all: Option<Answer>,
    /// Ids that answer [`Answer::Gone`] from the given time.
    pub gone_at_ms: BTreeMap<String, u64>,
    /// The delay of [`Answer::Slow`].
    pub slow_ms: u64,
    /// oEmbed calls per id.
    pub calls: BTreeMap<String, u64>,
    /// Every oEmbed call, malformed ones included.
    pub total: u64,
    /// Calls answering now, and the most at once.
    pub in_flight: u64,
    pub high_water: u64,
}

/// The two control posts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Controls {
    pub live: Option<String>,
    pub gone: Option<String>,
}

/// What a call asks of the stub.
#[derive(Debug, PartialEq)]
pub enum Route {
    /// An oEmbed request for this post id.
    Oembed(String),
    /// An oEmbed request that is not of the form the fetch Worker sends.
    BadOembed(&'static str),
    Switch,
    Reset,
    State,
    NotFound,
}

/// The id a well-formed request names: `url=https://twitter.com/i/status/<id>`
/// with `omit_script=1` and `dnt=true`, GET only.
pub fn route(call: &Call) -> Route {
    let path = match call.path.strip_prefix("/publish.x.com") {
        Some(p) => p,
        None if call.host == HOST => call.path.as_str(),
        None => call.path.as_str(),
    };
    match (call.method.as_str(), path) {
        ("POST", "/__x/switch") => return Route::Switch,
        ("POST", "/__x/reset") => return Route::Reset,
        ("GET", "/__x/state") => return Route::State,
        _ => {}
    }
    if path != PATH {
        return Route::NotFound;
    }
    if call.method != "GET" {
        return Route::BadOembed("GET only");
    }
    if call.q("omit_script") != Some("1") || call.q("dnt") != Some("true") {
        return Route::BadOembed("omit_script=1 and dnt=true are required");
    }
    for banned in ["cookie", "authorization", "referer"] {
        if call.h(banned).is_some() {
            return Route::BadOembed("the request carries a cookie, a key or a referrer");
        }
    }
    let Some(id) = call.q("url").and_then(|u| u.strip_prefix("https://twitter.com/i/status/")) else {
        return Route::BadOembed("url must be https://twitter.com/i/status/<id>");
    };
    if id.is_empty() || id.len() > 19 || !id.bytes().all(|b| b.is_ascii_digit()) || id.starts_with('0') {
        return Route::BadOembed("not a post id");
    }
    Route::Oembed(id.to_owned())
}

/// A switch body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Switch {
    pub id: Option<String>,
    pub answer: Option<Answer>,
    pub all: Option<Answer>,
    pub gone_at_ms: Option<u64>,
    pub slow_ms: Option<u64>,
    /// Clears the `all` answer.
    #[serde(default)]
    pub clear_all: bool,
}

impl State {
    /// Applies a switch; `Err` names what is wrong with it.
    pub fn switch(&mut self, s: Switch) -> Result<(), String> {
        if let Some(ms) = s.slow_ms {
            if ms > MAX_DELAY_MS {
                return Err(format!("slow_ms over {MAX_DELAY_MS}"));
            }
            self.slow_ms = ms;
        }
        if s.clear_all {
            self.all = None;
        }
        if let Some(a) = s.all {
            self.all = Some(a);
        }
        match (s.id, s.answer, s.gone_at_ms) {
            (None, None, None) => Ok(()),
            (Some(id), answer, gone) => {
                if let Some(a) = answer {
                    self.answers.insert(id.clone(), a);
                }
                if let Some(t) = gone {
                    self.gone_at_ms.insert(id, t);
                }
                Ok(())
            }
            _ => Err("an answer or gone_at_ms needs an id".into()),
        }
    }

    /// The answer `id` gets now.
    pub fn answer_of(&self, id: &str, controls: &Controls, now_ms: u64) -> Answer {
        if let Some(a) = &self.all {
            return a.clone();
        }
        if self.gone_at_ms.get(id).is_some_and(|t| now_ms >= *t) {
            return Answer::Gone;
        }
        if let Some(a) = self.answers.get(id) {
            return a.clone();
        }
        if controls.gone.as_deref() == Some(id) {
            return Answer::Gone;
        }
        Answer::Plain
    }

    /// Counts one oEmbed call starting (`id` when it named one).
    pub fn begin(&mut self, id: Option<&str>) {
        self.total += 1;
        if let Some(id) = id {
            *self.calls.entry(id.to_owned()).or_default() += 1;
        }
        self.in_flight += 1;
        self.high_water = self.high_water.max(self.in_flight);
    }

    /// Counts one oEmbed call answered.
    pub fn end(&mut self) {
        self.in_flight = self.in_flight.saturating_sub(1);
    }

    /// The counters and switches as the state route answers them.
    pub fn view(&self) -> serde_json::Value {
        json!({
            "calls": self.calls,
            "total": self.total,
            "in_flight": self.in_flight,
            "high_water": self.high_water,
            "answers": self.answers,
            "all": self.all,
            "gone_at_ms": self.gone_at_ms,
            "slow_ms": self.slow_ms,
        })
    }
}

/// The synthetic text of a post (before markup): one sentence naming its id
/// and the stub scorer's strong marker, about thirty words.
pub fn post_text(id: &str) -> String {
    format!(
        "Post {id} from the dev session: the town library will stay open late on Thursday evenings this winter, \
         and the reading room on the first floor gets new lamps next week. sn-stub:band=strong"
    )
}

fn mention(handle: &str) -> String {
    format!("<a href=\"https://twitter.com/{handle}?ref_src=twsrc%5Etfw\">@{handle}</a>")
}

fn link(shown: &str) -> String {
    format!("<a href=\"https://t.co/abcDEF1234\">{shown}</a>")
}

/// The first `<p>`'s inner markup for a post.
pub fn quote_markup(id: &str, a: &Answer) -> String {
    let text = escape(&post_text(id));
    match a {
        Answer::Reply => format!("{} {} {text}", mention("devfriend"), mention("devneighbour")),
        Answer::Media => format!("{text} {}", link("pic.x.com/abcDEF1234")),
        Answer::Quote => format!("{text} {}", link("https://t.co/abcDEF1234")),
        Answer::Long => {
            let cut: String = text.chars().take(200).collect();
            format!("{}\u{2026}", cut.trim_end())
        }
        Answer::Short => format!("Post {id}: a short note. sn-stub:band=strong"),
        Answer::Changed => format!("<span class=\"tweet-text\">{text}</span>"),
        _ => {
            // A line break and character references, as the embed writes them.
            text.replacen(": ", ":<br><br>", 1).replacen(", and", " &amp; then, and", 1)
        }
    }
}

/// oEmbed's success object for a post.
pub fn success(id: &str, a: &Answer) -> serde_json::Value {
    let p = quote_markup(id, a);
    let html = format!(
        "<blockquote class=\"twitter-tweet\" data-dnt=\"true\"><p lang=\"en\" dir=\"ltr\">{p}</p>&mdash; {AUTHOR_NAME} \
         (@{AUTHOR_HANDLE}) <a href=\"https://twitter.com/{AUTHOR_HANDLE}/status/{id}?ref_src=twsrc%5Etfw\">November 1, \
         2026</a></blockquote>\n"
    );
    json!({
        "url": format!("https://twitter.com/{AUTHOR_HANDLE}/status/{id}"),
        "author_name": AUTHOR_NAME,
        "author_url": format!("https://twitter.com/{AUTHOR_HANDLE}"),
        "html": html,
        "width": 550,
        "height": null,
        "type": "rich",
        "cache_age": "3153600000",
        "provider_name": "Twitter",
        "provider_url": "https://twitter.com",
        "version": "1.0",
    })
}

/// The reply for an oEmbed call of `id` answering `a`.
pub fn reply(id: &str, a: &Answer, slow_ms: u64) -> Reply {
    let json_error = |status: u16| {
        Reply::json(status, &json!({"errors": [{"message": "x-stub: the embed does not show this post"}]}))
    };
    match a {
        Answer::Gone => json_error(404),
        Answer::Forbidden => json_error(403),
        Answer::Html403 => {
            Reply::html(403, "<!doctype html><title>Forbidden</title><p>x-stub: refused at the edge.</p>")
        }
        Answer::Busy => Reply::json(429, &json!({"errors": [{"message": "x-stub: too many requests"}]})),
        Answer::Error => Reply::html(503, "<!doctype html><title>Over capacity</title><p>x-stub: try again later.</p>"),
        Answer::Oversize => {
            let mut v = success(id, &Answer::Plain);
            v["padding"] = json!("x".repeat(OVERSIZE_BYTES));
            Reply::json(200, &v)
        }
        Answer::Slow => Reply::json(200, &success(id, &Answer::Plain)).with_delay(slow_ms.min(MAX_DELAY_MS)),
        a => Reply::json(200, &success(id, a)),
    }
    .with_header("cache-control", "must-revalidate, max-age=3153600000")
}

/// Every answer but the oEmbed call's own delay: the routes that need no
/// waiting, and the oEmbed reply. Counting is the caller's ([`State::begin`],
/// [`State::end`]), around the delay.
pub fn handle(state: &mut State, controls: &Controls, call: &Call, now_ms: u64) -> Reply {
    match route(call) {
        Route::Switch => {
            let parsed = serde_json::from_slice::<Switch>(&call.body).map_err(|e| e.to_string());
            match parsed.and_then(|s| state.switch(s)) {
                Ok(()) => Reply::json(200, &state.view()),
                Err(e) => Reply::json(400, &json!({"error": e})),
            }
        }
        Route::Reset => {
            *state = State::default();
            Reply::json(200, &state.view())
        }
        Route::State => Reply::json(200, &state.view()),
        Route::NotFound => Reply::text(404, "x-stub: no such route"),
        Route::BadOembed(why) => Reply::text(400, &format!("x-stub: {why}")),
        Route::Oembed(id) => {
            let a = state.answer_of(&id, controls, now_ms);
            reply(&id, &a, state.slow_ms)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oembed(id: &str) -> Call {
        Call::new(
            "GET",
            &format!("/publish.x.com/oembed?url=https%3A%2F%2Ftwitter.com%2Fi%2Fstatus%2F{id}&omit_script=1&dnt=true"),
        )
    }

    fn controls() -> Controls {
        Controls { live: Some("1000000000000000001".into()), gone: Some("1000000000000000002".into()) }
    }

    fn html_of(r: &Reply) -> String {
        r.body_json()["html"].as_str().unwrap_or("").to_owned()
    }

    #[test]
    fn requests_route_on_the_real_host_and_path() {
        assert_eq!(route(&oembed("123")), Route::Oembed("123".into()));
        let bound = Call::new("GET", "/oembed?url=https://twitter.com/i/status/9&omit_script=1&dnt=true").host(HOST);
        assert_eq!(route(&bound), Route::Oembed("9".into()));
        assert!(matches!(
            route(&Call::new("GET", "/publish.x.com/oembed?url=https://twitter.com/i/status/9")),
            Route::BadOembed(_)
        ));
        assert!(matches!(route(&oembed("0123")), Route::BadOembed(_)));
        assert!(matches!(route(&oembed("12345678901234567890")), Route::BadOembed(_)));
        assert!(matches!(route(&oembed("12").header("Cookie", "a=b")), Route::BadOembed(_)));
        let handle =
            Call::new("GET", "/publish.x.com/oembed?url=https://twitter.com/a/status/9&omit_script=1&dnt=true");
        assert!(matches!(route(&handle), Route::BadOembed(_)));
        assert_eq!(route(&Call::new("GET", "/publish.x.com/other")), Route::NotFound);
        assert_eq!(route(&Call::new("POST", "/__x/switch")), Route::Switch);
    }

    #[test]
    fn each_answer_has_its_shape() {
        let c = controls();
        let mut s = State::default();
        let r = handle(&mut s, &c, &oembed("42"), 0);
        assert_eq!(r.status, 200);
        assert_eq!(r.header("content-type"), Some("application/json"));
        let v = r.body_json();
        assert_eq!(v["type"], "rich");
        let html = html_of(&r);
        assert!(
            html.starts_with(
                "<blockquote class=\"twitter-tweet\" data-dnt=\"true\"><p lang=\"en\" dir=\"ltr\">Post 42"
            )
        );
        assert!(html.contains("<br>") && html.contains("&amp;"));
        assert!(!html.contains("\u{2026}"));
        for (a, want) in [
            (Answer::Reply, "@devneighbour</a> Post 7"),
            (Answer::Media, ">pic.x.com/abcDEF1234</a></p>"),
            (Answer::Quote, ">https://t.co/abcDEF1234</a></p>"),
            (Answer::Long, "\u{2026}</p>"),
            (Answer::Changed, "<p lang=\"en\" dir=\"ltr\"><span"),
        ] {
            s.switch(Switch {
                id: Some("7".into()),
                answer: Some(a.clone()),
                all: None,
                gone_at_ms: None,
                slow_ms: None,
                clear_all: false,
            })
            .expect("switch");
            let h = html_of(&handle(&mut s, &c, &oembed("7"), 0));
            assert!(h.contains(want), "{a:?}: {h}");
        }
        for (a, status, kind) in [
            (Answer::Gone, 404, "application/json"),
            (Answer::Forbidden, 403, "application/json"),
            (Answer::Html403, 403, "text/html; charset=utf-8"),
            (Answer::Busy, 429, "application/json"),
            (Answer::Error, 503, "text/html; charset=utf-8"),
        ] {
            let r = reply("7", &a, 0);
            assert_eq!((r.status, r.header("content-type")), (status, Some(kind)), "{a:?}");
            if kind == "application/json" {
                assert!(r.body_json().get("html").is_none(), "{a:?}: an error carries no html");
            }
        }
        assert!(reply("7", &Answer::Oversize, 0).body.len() > OVERSIZE_BYTES as u64);
        assert_eq!(reply("7", &Answer::Slow, 2_000).delay_ms, 2_000);
        // The words a post carries: at least 24, under 150.
        let words = post_text("7").split_whitespace().count();
        assert!((24..150).contains(&words), "{words}");
    }

    #[test]
    fn controls_switches_and_counters() {
        let c = controls();
        let mut s = State::default();
        assert_eq!(s.answer_of("1000000000000000001", &c, 0), Answer::Plain);
        assert_eq!(s.answer_of("1000000000000000002", &c, 0), Answer::Gone);
        s.switch(Switch {
            id: None,
            answer: None,
            all: Some(Answer::Plain),
            gone_at_ms: None,
            slow_ms: None,
            clear_all: false,
        })
        .expect("all");
        assert_eq!(
            s.answer_of("1000000000000000002", &c, 0),
            Answer::Plain,
            "present for every id, the gone control included"
        );
        s.switch(Switch { id: None, answer: None, all: None, gone_at_ms: None, slow_ms: None, clear_all: true })
            .expect("clear");
        s.switch(Switch {
            id: Some("5".into()),
            answer: None,
            all: None,
            gone_at_ms: Some(100),
            slow_ms: None,
            clear_all: false,
        })
        .expect("gone at");
        assert_eq!(s.answer_of("5", &c, 99), Answer::Plain);
        assert_eq!(s.answer_of("5", &c, 100), Answer::Gone);
        assert!(
            s.switch(Switch {
                id: None,
                answer: Some(Answer::Busy),
                all: None,
                gone_at_ms: None,
                slow_ms: None,
                clear_all: false
            })
            .is_err()
        );
        assert!(
            s.switch(Switch {
                id: None,
                answer: None,
                all: None,
                gone_at_ms: None,
                slow_ms: Some(MAX_DELAY_MS + 1),
                clear_all: false
            })
            .is_err()
        );
        s.begin(Some("5"));
        s.begin(Some("5"));
        s.end();
        s.begin(None);
        assert_eq!((s.calls["5"], s.total, s.in_flight, s.high_water), (2, 3, 2, 2));
        let r = handle(&mut s, &c, &Call::new("POST", "/__x/reset"), 0);
        assert_eq!(r.status, 200);
        assert_eq!(s, State::default());
        let bad = Call::new("POST", "/__x/switch").body(b"{\"id\":\"1\",\"answer\":\"nope\"}".to_vec());
        assert_eq!(handle(&mut s, &c, &bad, 0).status, 400);
    }
}
