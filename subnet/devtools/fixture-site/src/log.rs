//! The fixture site's request log: what the fetch Worker asked for, with
//! the headers a test checks (agent, Web Bot Auth, cookies, referrer).

use serde::{Deserialize, Serialize};
use slop_ninja_dev_http::Call;

/// The most entries kept.
pub const MAX_ENTRIES: usize = 2000;

/// The request headers a log entry keeps.
pub const KEPT_HEADERS: &[&str] =
    &["user-agent", "signature", "signature-input", "signature-agent", "cookie", "referer", "accept", "accept-language", "if-none-match", "if-modified-since"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub at_ms: u64,
    pub method: String,
    pub host: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
}

impl Entry {
    pub fn of(call: &Call, at_ms: u64) -> Entry {
        Entry {
            at_ms,
            method: call.method.clone(),
            host: call.host.clone(),
            path: call.path.clone(),
            headers: call.headers.iter().filter(|(k, _)| KEPT_HEADERS.contains(&k.as_str())).cloned().collect(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Log {
    pub entries: Vec<Entry>,
}

impl Log {
    pub fn push(&mut self, e: Entry) {
        self.entries.push(e);
        if self.entries.len() > MAX_ENTRIES {
            let over = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..over);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_keep_only_the_checked_headers_and_the_log_is_bounded() {
        let c = Call::new("GET", "/robots.txt").host("plain.fixtures.test").header("User-Agent", "SlopNinjaBot/1.0").header("X-Other", "x");
        let e = Entry::of(&c, 5);
        assert_eq!(e.headers, vec![("user-agent".to_owned(), "SlopNinjaBot/1.0".to_owned())]);
        let mut l = Log::default();
        for i in 0..(MAX_ENTRIES + 5) {
            l.push(Entry { at_ms: i as u64, ..e.clone() });
        }
        assert_eq!(l.entries.len(), MAX_ENTRIES);
        assert_eq!(l.entries[0].at_ms, 5);
    }
}
