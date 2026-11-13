//! The security screen of a redirect hop's host (risk F3): a DNS-over-HTTPS
//! query to 1.1.1.1 for Families' malware and phishing resolver
//! (`security.cloudflare-dns.com`, the JSON form). An answer of `0.0.0.0`
//! (or `::`) means the host is blocked. The first hop's host is screened by
//! the detect Worker's `DetectHost` object before any slug exists; the fetch
//! Worker screens every new host a redirect reaches. A screen that cannot
//! answer stops the fetch: no hop goes unscreened.

use crate::bound::DOH_BYTES;
use crate::net::{Headers, Net, NetError, body_within};
use serde::Deserialize;

/// The resolver's JSON endpoint.
pub const SECURITY_DOH: &str = "https://security.cloudflare-dns.com/dns-query";

/// The query URL for `host` (an A-label name: only letters, digits, `-`
/// and `.`, so it needs no escaping).
pub fn doh_url(host: &str) -> String {
    format!("{SECURITY_DOH}?name={host}&type=A")
}

#[derive(Deserialize)]
struct Answer {
    #[serde(rename = "Status")]
    status: u32,
    #[serde(rename = "Answer", default)]
    answer: Vec<Record>,
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: u32,
    #[serde(default)]
    data: String,
}

/// Whether a JSON answer blocks the host; `None` when it is not one.
pub fn blocked(body: &[u8]) -> Option<bool> {
    let a: Answer = serde_json::from_slice(body).ok()?;
    let _ = a.status;
    Some(a.answer.iter().any(|r| (r.kind == 1 && r.data.trim() == "0.0.0.0") || (r.kind == 28 && r.data.trim() == "::")))
}

/// How a screen ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Clear,
    Blocked,
    /// The resolver did not answer usably.
    Unknown,
}

/// Screens `host` before `deadline_ms`.
pub async fn screen<N: Net>(net: &mut N, host: &str, deadline_ms: u64) -> Result<Screen, NetError> {
    if !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') {
        return Ok(Screen::Unknown);
    }
    let headers: Headers = vec![("accept", "application/dns-json".to_owned())];
    let head = net.get(&doh_url(host), &headers, deadline_ms).await?;
    if head.status != 200 {
        net.discard();
        return Ok(Screen::Unknown);
    }
    let Some(body) = body_within(net, DOH_BYTES, deadline_ms).await? else { return Ok(Screen::Unknown) };
    Ok(match blocked(&body) {
        Some(true) => Screen::Blocked,
        Some(false) => Screen::Clear,
        None => Screen::Unknown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_on;
    use crate::net::fake::{FakeNet, Reply};

    #[test]
    fn answers() {
        assert_eq!(blocked(br#"{"Status":0,"Answer":[{"name":"bad.example","type":1,"TTL":60,"data":"0.0.0.0"}]}"#), Some(true));
        assert_eq!(blocked(br#"{"Status":0,"Answer":[{"name":"ok.example","type":1,"TTL":60,"data":"93.184.216.34"}]}"#), Some(false));
        assert_eq!(blocked(br#"{"Status":3}"#), Some(false));
        assert_eq!(blocked(b"<html>"), None);
    }

    #[test]
    fn screens_through_the_resolver() {
        let mut net = FakeNet::new()
            .site(
                &doh_url("bad.example.com"),
                Reply::new(200, &[("content-type", "application/dns-json")], r#"{"Status":0,"Answer":[{"type":1,"data":"0.0.0.0"}]}"#),
            )
            .site(&doh_url("ok.example.com"), Reply::new(200, &[], r#"{"Status":0,"Answer":[{"type":1,"data":"192.0.2.1"}]}"#));
        assert_eq!(block_on(screen(&mut net, "bad.example.com", u64::MAX)), Ok(Screen::Blocked));
        assert_eq!(block_on(screen(&mut net, "ok.example.com", u64::MAX)), Ok(Screen::Clear));
        assert_eq!(block_on(screen(&mut net, "missing.example.com", u64::MAX)), Ok(Screen::Unknown));
        assert_eq!(net.requests[0].1, vec![("accept".to_owned(), "application/dns-json".to_owned())]);
    }
}
