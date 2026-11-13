//! Freshness and politeness read from response headers (RFC 9111, RFC 9110).
//!
//! A source counts as fresh until the time its own response allowed:
//! `Cache-Control: max-age`, else `Expires` minus `Date`, else RFC 9111
//! section 4.2.2's heuristic of 10% of the time since `Last-Modified`;
//! `no-store` and `no-cache` make it stale at once. With none of these the
//! answer states no lifetime and the detect Worker uses the robots cache
//! period.

use crate::net::Head;
use std::time::UNIX_EPOCH;

fn http_date_ms(v: &str) -> Option<u64> {
    let t = httpdate::parse_http_date(v.trim()).ok()?;
    let d = t.duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(d.as_millis()).ok()
}

/// The response's freshness lifetime in ms, if it states one.
pub fn fresh_ms(head: &Head, now_ms: u64) -> Option<u64> {
    let cc = head.all("cache-control").collect::<Vec<_>>().join(",");
    let mut max_age = None;
    for d in cc.split(',') {
        let d = d.trim().to_ascii_lowercase();
        if d == "no-store" || d == "no-cache" {
            return Some(0);
        }
        if let Some(v) = d.strip_prefix("max-age=") {
            max_age = v.trim_matches('"').parse::<u64>().ok();
        }
    }
    if let Some(s) = max_age {
        return Some(s.saturating_mul(1000));
    }
    let date = head.get("date").and_then(http_date_ms).unwrap_or(now_ms);
    if let Some(exp) = head.get("expires") {
        // An invalid Expires means already expired (RFC 9111 section 5.3).
        return Some(http_date_ms(exp).map_or(0, |e| e.saturating_sub(date)));
    }
    let lm = head.get("last-modified").and_then(http_date_ms)?;
    Some(date.saturating_sub(lm) / 10)
}

/// A `Retry-After` (delta-seconds or an HTTP date) in ms from now.
pub fn retry_after_ms(head: &Head, now_ms: u64) -> Option<u64> {
    let v = head.get("retry-after")?.trim();
    if let Ok(s) = v.parse::<u64>() {
        return Some(s.saturating_mul(1000));
    }
    http_date_ms(v).map(|t| t.saturating_sub(now_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(h: &[(&str, &str)]) -> Head {
        Head { status: 200, headers: h.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect() }
    }

    #[test]
    fn lifetimes() {
        let now = 1_700_000_000_000;
        assert_eq!(fresh_ms(&head(&[("cache-control", "public, max-age=600")]), now), Some(600_000));
        assert_eq!(fresh_ms(&head(&[("cache-control", "no-cache"), ("expires", "Wed, 21 Oct 2099 07:28:00 GMT")]), now), Some(0));
        assert_eq!(
            fresh_ms(&head(&[("date", "Wed, 21 Oct 2015 07:28:00 GMT"), ("expires", "Wed, 21 Oct 2015 08:28:00 GMT")]), now),
            Some(3_600_000)
        );
        assert_eq!(fresh_ms(&head(&[("expires", "0")]), now), Some(0));
        assert_eq!(
            fresh_ms(&head(&[("date", "Wed, 21 Oct 2015 07:28:00 GMT"), ("last-modified", "Sun, 11 Oct 2015 07:28:00 GMT")]), now),
            Some(86_400_000)
        );
        assert_eq!(fresh_ms(&head(&[]), now), None);
    }

    #[test]
    fn retry_after() {
        assert_eq!(retry_after_ms(&head(&[("retry-after", "120")]), 0), Some(120_000));
        let now = http_date_ms("Wed, 21 Oct 2015 07:28:00 GMT").unwrap();
        assert_eq!(retry_after_ms(&head(&[("retry-after", "Wed, 21 Oct 2015 07:29:00 GMT")]), now), Some(60_000));
        assert_eq!(retry_after_ms(&head(&[]), 0), None);
    }
}
