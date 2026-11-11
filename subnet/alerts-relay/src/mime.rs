//! Plain-text RFC 5322 messages for the `send_email` binding's raw
//! `EmailMessage` path.
//!
//! Every message carries `From`, `To`, `Subject`, `Date` and `Message-ID`,
//! with `MIME-Version`, a `text/plain; charset=utf-8` body in
//! quoted-printable (so no line passes 76 characters whatever an event's
//! detail holds) and `Auto-Submitted: auto-generated` (RFC 3834: no
//! auto-replies). Header values are ASCII built from rule names, subjects and
//! counts only; details and any figure appear only in the body. The relay
//! sets a deterministic `Message-ID` (derived from the event ids), so a
//! message it had to send again carries the same id unless Cloudflare
//! replaces it: Cloudflare's header reference lists `Date` and `Message-ID`
//! as platform-generated for its structured API, and the deploy drill
//! compares the delivered headers with these.

use sha2::{Digest, Sha256};
use slop_ninja_alerts_wire::{Event, EventKind};

/// RFC 5322's recommended line length, which the subject keeps to.
pub const SUBJECT_LINE: usize = 78;
/// RFC 5321's path limit for an address.
pub const MAX_ADDRESS: usize = 254;
const QP_LINE: usize = 76;

/// The `From` header without the network and the address.
const FROM_FIXED: &str = "From: slop-ninja  alerts <>";
/// The `To` header without the address.
const TO_FIXED: &str = "To: <>";
/// The `Date` header (RFC 5322, four-digit year).
const DATE_LINE: usize = "Date: Mon, 02 Nov 2026 13:45:07 +0000".len();
/// The `Message-ID` header without its hash and the sender's domain.
const MESSAGE_ID_FIXED: &str = "Message-ID: <.alerts@>";
const MESSAGE_ID_HASH: usize = 32;
/// The headers whose values never vary.
const FIXED_HEADERS: [&str; 4] = [
    "MIME-Version: 1.0",
    "Content-Type: text/plain; charset=utf-8",
    "Content-Transfer-Encoding: quoted-printable",
    "Auto-Submitted: auto-generated",
];
/// From, To, Subject, Date, Message-ID and the fixed ones.
const HEADER_LINES: usize = 5 + FIXED_HEADERS.len();

/// The header block's bound for a network name of `network` bytes, with
/// everything around the encoded body: the nine headers (each address at
/// its 254-byte limit, the sender's domain at most 252 of them, the subject
/// line at 78), their line breaks, the blank line and the final CRLF. Every
/// header stays within RFC 5322's 998 characters.
pub const fn header_bound(network: usize) -> usize {
    let mut fixed = 0;
    let mut i = 0;
    while i < FIXED_HEADERS.len() {
        fixed += FIXED_HEADERS[i].len();
        i += 1;
    }
    FROM_FIXED.len()
        + network
        + MAX_ADDRESS
        + TO_FIXED.len()
        + MAX_ADDRESS
        + SUBJECT_LINE
        + DATE_LINE
        + MESSAGE_ID_FIXED.len()
        + MESSAGE_ID_HASH
        + (MAX_ADDRESS - 2)
        + fixed
        + 2 * (HEADER_LINES - 1)
        + 4
        + 2
}

/// The header block's bound for the network the configuration admits.
pub const HEADER_BYTES: usize = header_bound(crate::config::NETWORK.len());

/// A checked mailbox: ASCII, one `@`, a dot-atom local part and a domain of
/// LDH labels. Anything else (display names, comments, routes, whitespace,
/// line breaks) is refused.
pub fn valid_address(a: &str) -> bool {
    if a.is_empty() || a.len() > MAX_ADDRESS || !a.is_ascii() {
        return false;
    }
    let Some((local, domain)) = a.split_once('@') else {
        return false;
    };
    let atext = |b: u8| b.is_ascii_alphanumeric() || b"!#$%&'*+-/=?^_`{|}~".contains(&b);
    let local_ok = !local.is_empty()
        && local
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(atext));
    let domain_ok = domain.contains('.')
        && domain.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    local_ok && domain_ok
}

/// The sender and recipient, read from the deploy-time values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Addresses {
    pub from: String,
    pub to: String,
}

impl Addresses {
    pub fn new(from: &str, to: &str) -> Option<Self> {
        let (from, to) = (from.trim(), to.trim());
        (valid_address(from) && valid_address(to)).then(|| Self {
            from: from.to_owned(),
            to: to.to_owned(),
        })
    }

    fn domain(&self) -> &str {
        self.from.rsplit('@').next().unwrap_or("invalid")
    }
}

/// The two senders a message can name.
pub const SOURCE_PUBLISHER: &str = "the publisher";
pub const SOURCE_RELAY: &str = "the alerts relay";

/// Where a message's events came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Source<'a> {
    /// "the publisher" or "the alerts relay".
    pub name: &'a str,
    /// The rule set that raised the events.
    pub rules: &'a str,
}

/// One message ready for `EmailMessage::new(from, to, raw)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub from: String,
    pub to: String,
    pub subject: String,
    pub message_id: String,
    pub raw: String,
}

// ------------------------------------------------------------------ dates

/// Days since 1970-01-01 to (year, month, day) (Howard Hinnant's civil
/// calendar algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// (year, month) to days since 1970-01-01 of its first day.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

struct Clock {
    y: i64,
    mo: u32,
    d: u32,
    h: u64,
    mi: u64,
    s: u64,
    weekday: usize,
}

fn clock(ms: u64) -> Clock {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, mo, d) = civil(days);
    Clock {
        y,
        mo,
        d,
        h: rem / 3600,
        mi: rem % 3600 / 60,
        s: rem % 60,
        // 1970-01-01 was a Thursday.
        weekday: ((days + 4).rem_euclid(7)) as usize,
    }
}

/// RFC 3339 UTC, to the second: `2026-11-02T13:45:07Z`.
pub fn rfc3339(ms: u64) -> String {
    let c = clock(ms);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        c.y, c.mo, c.d, c.h, c.mi, c.s
    )
}

/// Parses `YYYY-MM-DDTHH:MM:SSZ` to milliseconds since the epoch.
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u64> {
        let part = &s[r];
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    let days = days_from_civil(y as i64, mo as u32, d as u32);
    if civil(days) != (y as i64, mo as u32, d as u32) || days < 0 {
        return None;
    }
    Some((days as u64 * 86_400 + h * 3600 + mi * 60 + sec) * 1000)
}

/// RFC 5322 date: `Mon, 02 Nov 2026 13:45:07 +0000`.
pub fn rfc5322_date(ms: u64) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let c = clock(ms);
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} +0000",
        DAYS[c.weekday],
        c.d,
        MONTHS[(c.mo - 1) as usize],
        c.y,
        c.h,
        c.mi,
        c.s
    )
}

// ------------------------------------------------------------ encodings

/// Quoted-printable (RFC 2045 section 6.7) of `text`, whose lines end in
/// `\n`: CRLF line ends, soft breaks before 76 characters, `=` and every
/// byte outside printable ASCII encoded, and whitespace before a line end
/// encoded.
pub fn quoted_printable(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 2);
    quoted_printable_into(&mut out, text);
    out
}

/// [`quoted_printable`], appended to `out` (which the caller sized), so a
/// message is encoded straight into its buffer.
pub fn quoted_printable_into(out: &mut String, text: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for (n, line) in text.split('\n').enumerate() {
        if n > 0 {
            out.push_str("\r\n");
        }
        let bytes = line.as_bytes();
        let mut col = 0usize;
        for (i, &b) in bytes.iter().enumerate() {
            let last = i + 1 == bytes.len();
            let literal =
                (b'!'..=b'~').contains(&b) && b != b'=' || (matches!(b, b' ' | b'\t') && !last);
            let width = if literal { 1 } else { 3 };
            // Leave room for the soft break's '='.
            if col + width > QP_LINE - 1 {
                out.push_str("=\r\n");
                col = 0;
            }
            if literal {
                out.push(b as char);
            } else {
                out.push('=');
                out.push(HEX[usize::from(b >> 4)] as char);
                out.push(HEX[usize::from(b & 15)] as char);
            }
            col += width;
        }
    }
}

/// Header-safe text: printable ASCII, anything else as `?`.
fn header_text(s: &str) -> String {
    s.chars()
        .map(|c| if (' '..='~').contains(&c) { c } else { '?' })
        .collect()
}

/// One line of body text: line breaks inside a field become spaces.
fn body_text(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

/// The subject: content-free (the kind, the rule and the subject, or only
/// counts), within [`SUBJECT_LINE`] with its header name.
pub fn subject(events: &[Event], network: &str) -> String {
    let prefix = format!("[slop-ninja {}]", header_text(network));
    let fits = |s: &str| "Subject: ".len() + s.len() <= SUBJECT_LINE;
    if let [e] = events {
        let full = format!(
            "{prefix} {} {} {}",
            e.state.tag(),
            header_text(&e.rule),
            header_text(&e.subject)
        );
        if fits(&full) {
            return full;
        }
        let short = format!("{prefix} {} {}", e.state.tag(), header_text(&e.rule));
        if fits(&short) {
            return short;
        }
    }
    let count = |k: EventKind| events.iter().filter(|e| e.state == k).count();
    let parts: Vec<String> = [
        (EventKind::Firing, "firing"),
        (EventKind::Reminder, "still firing"),
        (EventKind::Resolved, "resolved"),
    ]
    .into_iter()
    .filter_map(|(k, name)| match count(k) {
        0 => None,
        n => Some(format!("{n} {name}")),
    })
    .collect();
    format!("{prefix} {} alerts: {}", events.len(), parts.join(", "))
}

/// A `Message-ID` from the sorted event ids and the sender's domain.
pub fn message_id(events: &[Event], domain: &str) -> String {
    let mut ids: Vec<&str> = events.iter().map(|e| e.id.as_str()).collect();
    ids.sort_unstable();
    let mut h = Sha256::new();
    h.update(b"slop_ninja.alert_message_id/v1");
    for id in ids {
        h.update((id.len() as u64).to_be_bytes());
        h.update(id.as_bytes());
    }
    let d = hex::encode(h.finalize());
    format!("<{}.alerts@{}>", &d[..32], domain)
}

// The fixed text of a message, in pieces, so its bounds below are their
// lengths and cannot drift from the text.
const HEAD: [&str; 4] = ["SlopNinja ", " alerts from ", " (rules ", ").\n\n"];
const FOOTER: &str = "-- \nSent by the private alerts relay (email slop_ninja.alerts_relay.email/v1) \
     for the rules on its email list: one email when a condition fires and one when it resolves, \
     within a per-subject interval and a daily limit. Reminders and every other alert go to the \
     private alert feed only. The relay does not read replies.\n";
/// Around one event: `[`, its tag, `] `, its rule, ` `, its subject, a
/// line break and indent, its detail, a line break.
const EVENT_HEAD: [&str; 4] = ["[", "] ", " ", "\n  "];
const EVENT_LINE_END: &str = "\n";
const UNCONFIRMED: &str =
    "  (its firing was never confirmed to the sender: the firing email may not have gone out)\n";
/// A reminder is emailed only as the first email on record of its episode
/// ([`crate::email`]): its firing was deferred by the limits or came while
/// email was off, or the episode's row was swept while its sender was
/// silent (its firing may have been emailed before). A resolution never
/// carries it, so an event carries at most one of the two notes.
const FIRST_EMAIL: &str =
    "  (first email on record: its firing was deferred, came while email was off, or expired)\n";
const NOTE_BYTES: usize = max(UNCONFIRMED.len(), FIRST_EMAIL.len());
/// Then `  since `, the time, `; event `, the id's prefix, a blank line.
const EVENT_TAIL: [&str; 3] = ["  since ", "; event ", "\n\n"];
const ID_PREFIX: usize = 12;
/// [`rfc3339`] of the largest millisecond count (a test checks it).
pub const RFC3339_MAX_BYTES: usize = 25;
const LONGEST_TAG: usize = "STILL FIRING".len();

const fn total(pieces: &[&str]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < pieces.len() {
        n += pieces[i].len();
        i += 1;
    }
    n
}

const fn max(a: usize, b: usize) -> usize {
    if a > b { a } else { b }
}

/// The fixed text one event adds besides its rule, subject and detail: the
/// pieces around it, the longest tag, the longer note, the longest time
/// and the id's prefix.
pub const EVENT_FIXED_BYTES: usize = total(&EVENT_HEAD)
    + LONGEST_TAG
    + EVENT_LINE_END.len()
    + NOTE_BYTES
    + total(&EVENT_TAIL)
    + RFC3339_MAX_BYTES
    + ID_PREFIX;

/// The fixed text of a message besides its network and rule set: the head,
/// the longer source name and the footer.
pub const MESSAGE_FIXED_BYTES: usize =
    total(&HEAD) + max(SOURCE_PUBLISHER.len(), SOURCE_RELAY.len()) + FOOTER.len();

/// The plain text of a message: the source, each event (kind, rule and
/// subject, its detail, since when, and its id's prefix) and how the relay
/// sends. Its length is at most the fields' and the network's and rule
/// set's, plus [`MESSAGE_FIXED_BYTES`] and [`EVENT_FIXED_BYTES`] per event.
pub fn text(events: &[Event], network: &str, source: Source<'_>) -> String {
    // One allocation: the fields and the fixed text around them.
    let fields: usize = events
        .iter()
        .map(|e| e.rule.len() + e.subject.len() + e.detail.len())
        .sum();
    let mut t = String::with_capacity(
        fields
            + events.len() * EVENT_FIXED_BYTES
            + MESSAGE_FIXED_BYTES
            + network.len()
            + source.rules.len(),
    );
    for (piece, field) in HEAD.iter().zip([network, source.name, source.rules, ""]) {
        t.push_str(piece);
        t.push_str(&body_text(field));
    }
    for e in events {
        for (piece, field) in EVENT_HEAD
            .iter()
            .zip([e.state.tag(), &e.rule, &e.subject, &e.detail])
        {
            t.push_str(piece);
            t.push_str(&body_text(field));
        }
        t.push_str(EVENT_LINE_END);
        if e.unconfirmed() {
            t.push_str(UNCONFIRMED);
        } else if e.state == EventKind::Reminder {
            t.push_str(FIRST_EMAIL);
        }
        t.push_str(EVENT_TAIL[0]);
        t.push_str(&rfc3339(e.since_ms));
        t.push_str(EVENT_TAIL[1]);
        t.push_str(&e.id[..e.id.len().min(ID_PREFIX)]);
        t.push_str(EVENT_TAIL[2]);
    }
    t.push_str(FOOTER);
    t
}

/// Builds the message for `events` (never empty).
pub fn build(
    addrs: &Addresses,
    events: &[Event],
    network: &str,
    source: Source<'_>,
    now_ms: u64,
) -> Message {
    let subject = subject(events, network);
    let message_id = message_id(events, addrs.domain());
    let text = text(events, network, source);
    assemble(addrs, network, subject, message_id, text, now_ms)
}

// ---------------------------------------------------------------- drills

/// The drill message's fixed text, in pieces (its bound below is their
/// length).
const DRILL_HEAD: [&str; 3] = [
    "This is a drill of the SlopNinja ",
    " alerts relay. It needs no action.\n\n\
     It checks that the relay's email reaches this mailbox. Drill ",
    ", sent ",
];
const DRILL_FOOTER: &str = ".\n\n-- \nSent once per drill id by the private alerts relay, because its \
     deploy-time setting DRILL_ID was set. The relay does not read replies.\n";

/// The drill message's text bound besides its network and drill id.
pub const DRILL_FIXED_BYTES: usize = total(&DRILL_HEAD) + RFC3339_MAX_BYTES + DRILL_FOOTER.len();

/// The drill's subject: `SlopNinja <network> alert drill`.
pub fn drill_subject(network: &str) -> String {
    format!("SlopNinja {} alert drill", header_text(network))
}

/// The drill's plain text: what it is, that it needs no action, its id
/// and when it was sent.
pub fn drill_text(network: &str, drill: &str, now_ms: u64) -> String {
    let mut t = String::with_capacity(DRILL_FIXED_BYTES + network.len() + drill.len());
    for (piece, field) in
        DRILL_HEAD
            .iter()
            .zip([body_text(network), body_text(drill), rfc3339(now_ms)])
    {
        t.push_str(piece);
        t.push_str(&field);
    }
    t.push_str(DRILL_FOOTER);
    t
}

/// A drill's `Message-ID`, from its id alone (a drill sent again after an
/// unknown outcome carries the same one).
pub fn drill_message_id(drill: &str, domain: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"slop_ninja.alert_drill_message_id/v1");
    h.update((drill.len() as u64).to_be_bytes());
    h.update(drill.as_bytes());
    let d = hex::encode(h.finalize());
    format!("<{}.alerts@{}>", &d[..32], domain)
}

/// Builds the drill message for `drill` (a deploy-time `DRILL_ID`).
pub fn build_drill(addrs: &Addresses, network: &str, drill: &str, now_ms: u64) -> Message {
    let subject = drill_subject(network);
    let message_id = drill_message_id(drill, addrs.domain());
    let text = drill_text(network, drill, now_ms);
    assemble(addrs, network, subject, message_id, text, now_ms)
}

/// The headers and the encoded body of one message.
fn assemble(
    addrs: &Addresses,
    network: &str,
    subject: String,
    message_id: String,
    text: String,
    now_ms: u64,
) -> Message {
    let mut raw = String::with_capacity(crate::arena::message_bound(text.len() as u64) as usize);
    for line in [
        format!(
            "From: slop-ninja {} alerts <{}>",
            header_text(network),
            addrs.from
        ),
        format!("To: <{}>", addrs.to),
        format!("Subject: {subject}"),
        format!("Date: {}", rfc5322_date(now_ms)),
        format!("Message-ID: {message_id}"),
    ]
    .iter()
    .map(String::as_str)
    .chain(FIXED_HEADERS)
    {
        raw.push_str(line);
        raw.push_str("\r\n");
    }
    raw.push_str("\r\n");
    quoted_printable_into(&mut raw, &text);
    drop(text);
    raw.push_str("\r\n");
    Message {
        from: addrs.from.clone(),
        to: addrs.to.clone(),
        subject,
        message_id,
        raw,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn addrs() -> Addresses {
        Addresses::new("alerts@example.invalid", "ops@example.invalid").unwrap()
    }

    /// The publisher rule set the test posts carry.
    pub const RULES: &str = "slop_ninja.publisher.alerts/v3";

    /// An event under [`RULES`]; a resolution or reminder as if notified.
    pub fn ev(state: EventKind, rule: &str, subject: &str, detail: &str) -> Event {
        let notified = (state != EventKind::Firing).then_some(1_793_000_060_000);
        Event::new(
            RULES,
            state,
            rule,
            subject,
            detail,
            1_793_000_000_000,
            notified,
        )
    }

    /// Decodes quoted-printable back to `\n`-separated text.
    pub fn qp_decode(s: &str) -> String {
        let joined = s.replace("=\r\n", "");
        let mut out = Vec::new();
        let b = joined.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'=' {
                out.push(u8::from_str_radix(&joined[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else if b[i] == b'\r' && b.get(i + 1) == Some(&b'\n') {
                out.push(b'\n');
                i += 2;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn addresses_are_checked() {
        assert!(valid_address("operations@sub.example.invalid"));
        assert!(valid_address("a.b+c@example.invalid"));
        for bad in [
            "",
            "no-at",
            "two@@example.invalid",
            "a@b",
            "a b@example.invalid",
            "a@example.invalid\r\nBcc: x@example.invalid",
            "Name <a@example.invalid>",
            "a@-x.invalid",
            ".a@example.invalid",
            "é@example.invalid",
        ] {
            assert!(!valid_address(bad), "{bad:?}");
        }
        assert!(Addresses::new(" a@example.invalid\n", "b@example.invalid").is_some());
        assert!(Addresses::new("a@example.invalid", "b").is_none());
    }

    #[test]
    fn dates() {
        // 2026-11-02 13:45:07 UTC was a Monday.
        let ms = 1_793_627_107_000;
        assert_eq!(rfc3339(ms), "2026-11-02T13:45:07Z");
        assert_eq!(rfc5322_date(ms), "Mon, 02 Nov 2026 13:45:07 +0000");
        assert_eq!(rfc5322_date(0), "Thu, 01 Jan 1970 00:00:00 +0000");
        assert_eq!(rfc3339(951_782_400_000), "2000-02-29T00:00:00Z");
        assert_eq!(parse_rfc3339("2026-11-02T13:45:07Z"), Some(ms));
        assert_eq!(parse_rfc3339("2000-02-29T00:00:00Z"), Some(951_782_400_000));
        for bad in [
            "2026-02-30T00:00:00Z",
            "2026-11-02 13:45:07Z",
            "2026-11-02T24:00:00Z",
            "x",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn quoted_printable_round_trips_and_keeps_lines_short() {
        let text = format!(
            "plain line\nequals = sign and é and tab\tend \n{}\ntrailing tab\t\n",
            "x".repeat(300)
        );
        let qp = quoted_printable(&text);
        assert!(qp.is_ascii());
        for line in qp.split("\r\n") {
            assert!(line.len() <= QP_LINE, "{} chars: {line}", line.len());
            assert!(!line.ends_with(' ') && !line.ends_with('\t'));
        }
        assert_eq!(qp_decode(&qp), text);
        assert!(qp.contains("=3D") && qp.contains("=C3=A9"));
    }

    #[test]
    fn subjects_are_content_free_and_short() {
        let one = [ev(
            EventKind::Firing,
            "commit_gap",
            "coordinator",
            "last commit at epoch 1, now 4",
        )];
        assert_eq!(
            subject(&one, "testnet"),
            "[slop-ninja testnet] FIRING commit_gap coordinator"
        );
        let long = [ev(
            EventKind::Reminder,
            "publication_refused",
            &"c".repeat(200),
            "d",
        )];
        assert_eq!(
            subject(&long, "testnet"),
            "[slop-ninja testnet] STILL FIRING publication_refused"
        );
        let many = [
            ev(EventKind::Firing, "a", "x", "$123.45 spent"),
            ev(EventKind::Firing, "b", "y", "d"),
            ev(EventKind::Resolved, "c", "z", "d"),
        ];
        let s = subject(&many, "testnet");
        assert_eq!(s, "[slop-ninja testnet] 3 alerts: 2 firing, 1 resolved");
        for s in [subject(&one, "testnet"), subject(&long, "testnet"), s] {
            assert!("Subject: ".len() + s.len() <= SUBJECT_LINE);
            assert!(!s.contains('$'));
        }
        let odd = [ev(EventKind::Firing, "r", "a\r\nBcc: x", "d")];
        assert!(!subject(&odd, "testnet").contains('\r'));
    }

    #[test]
    fn messages_carry_the_required_headers() {
        let events = [
            ev(
                EventKind::Firing,
                "commit_gap",
                "coordinator",
                "last commit at epoch 1, now 4",
            ),
            ev(
                EventKind::Resolved,
                "watermark_stalled",
                "eval_worker",
                "multi\nline = detail",
            ),
        ];
        let src = Source {
            name: "the publisher",
            rules: RULES,
        };
        let m = build(&addrs(), &events, "testnet", src, 1_793_627_107_000);
        let (head, body) = m.raw.split_once("\r\n\r\n").unwrap();
        let headers: Vec<&str> = head.split("\r\n").collect();
        for name in [
            "From: ",
            "To: ",
            "Subject: ",
            "Date: ",
            "Message-ID: ",
            "MIME-Version: 1.0",
        ] {
            assert_eq!(
                headers.iter().filter(|h| h.starts_with(name)).count(),
                1,
                "{name}"
            );
        }
        assert!(headers.contains(&"From: slop-ninja testnet alerts <alerts@example.invalid>"));
        assert!(headers.contains(&"To: <ops@example.invalid>"));
        assert!(headers.contains(&"Date: Mon, 02 Nov 2026 13:45:07 +0000"));
        assert!(headers.contains(&"Content-Transfer-Encoding: quoted-printable"));
        assert!(headers.contains(&"Auto-Submitted: auto-generated"));
        assert!(head.len() <= HEADER_BYTES && head.is_ascii());
        assert!(headers.iter().all(|h| h.len() <= 998));
        assert!(m.message_id.starts_with('<') && m.message_id.ends_with("@example.invalid>"));
        // The same events in another order give the same Message-ID.
        let swapped = [events[1].clone(), events[0].clone()];
        assert_eq!(
            build(&addrs(), &swapped, "testnet", src, 5).message_id,
            m.message_id
        );
        let text = qp_decode(body);
        assert!(
            text.contains("[FIRING] commit_gap coordinator\n  last commit at epoch 1, now 4\n")
        );
        assert!(text.contains("multi line = detail"));
        assert!(text.contains(&format!("event {}", &events[0].id[..12])));
        assert!(text.contains("since 2026-10-26T"));
        assert!(text.contains("The relay does not read replies."));
        assert!(
            !text.contains("may not have gone out"),
            "both resolutions were notified"
        );
        assert!(m.raw.len() as u64 <= crate::arena::message_bound(text.len() as u64));
        // An unconfirmed resolution says its firing was never confirmed.
        let z = Event::new(RULES, EventKind::Resolved, "r", "s", "d", 5, None);
        let m = build(&addrs(), &[z], "testnet", src, 5);
        let text = qp_decode(m.raw.split_once("\r\n\r\n").unwrap().1);
        assert!(text.contains("may not have gone out"));
    }

    /// The header bound holds for the longest addresses the configuration
    /// accepts (254 bytes, the sender's domain 252 of them) and the longest
    /// subject: nine headers, each within RFC 5322's 998 characters.
    #[test]
    fn header_bound_holds_for_the_longest_addresses() {
        let local = "a".repeat(64);
        let domain = format!("{}.{}.{}", "b".repeat(63), "c".repeat(63), "d".repeat(61));
        let long = format!("{local}@{domain}");
        assert_eq!(long.len(), MAX_ADDRESS);
        let addrs = Addresses::new(&long, &long).unwrap();
        let src = Source {
            name: "the publisher",
            rules: RULES,
        };
        let one = [ev(EventKind::Reminder, &"r".repeat(60), "s", "d")];
        let many = [
            ev(EventKind::Firing, "a", "x", "d"),
            ev(EventKind::Reminder, "b", "y", "d"),
            ev(EventKind::Resolved, "c", "z", "d"),
        ];
        for events in [&one[..], &many[..]] {
            // The last second of year 9999: the longest date line.
            let m = build(
                &addrs,
                events,
                crate::config::NETWORK,
                src,
                253_402_300_799_000,
            );
            let (head, body) = m.raw.split_once("\r\n\r\n").unwrap();
            let lines: Vec<&str> = head.split("\r\n").collect();
            assert_eq!(lines.len(), HEADER_LINES);
            assert!(lines.iter().all(|h| h.len() <= 998));
            assert!(
                head.len() + 6 <= HEADER_BYTES,
                "{} > {HEADER_BYTES}",
                head.len() + 6
            );
            assert_eq!(m.raw.len() - body.len(), head.len() + 4);
            let text = qp_decode(body.trim_end_matches("\r\n"));
            assert!(m.raw.len() as u64 <= crate::arena::message_bound(text.len() as u64));
        }
        // The bound is exact for the From, To and Message-ID lines at their
        // limits (the subject and date lines are checked by length above).
        assert_eq!(HEADER_BYTES, 1120);
    }

    /// A post at its bound (many events, or one event of the largest size)
    /// builds a message within the budgeted bound.
    #[test]
    fn largest_posts_fit_the_message_bound() {
        use slop_ninja_alerts_wire::{POST_EVENTS_BYTES, post_bound};
        let src = Source {
            name: SOURCE_RELAY,
            rules: RULES,
        };
        let mut big = ev(
            EventKind::Firing,
            "r",
            "s",
            &"\u{e9}=".repeat(POST_EVENTS_BYTES),
        );
        assert!(big.fit(POST_EVENTS_BYTES));
        let mut many = Vec::new();
        let mut size = 0;
        while size + 120 < post_bound() {
            let e = ev(EventKind::Reminder, "", "", "");
            size += e.wire_len() + 1;
            many.push(e);
        }
        for events in [vec![big], many] {
            let t = text(&events, "testnet", src);
            assert!(t.len() as u64 <= crate::arena::text_bound(), "{}", t.len());
            let m = build(&addrs(), &events, "testnet", src, 0);
            assert!(m.raw.len() as u64 <= crate::arena::message_bound(crate::arena::text_bound()));
        }
    }

    /// The drill message: the subject the operator looks for, the required
    /// headers, a body that says it is a drill needing no action, a
    /// Message-ID from the drill id alone, and a text within its bound
    /// (and the budgeted message bound) for the longest id.
    #[test]
    fn drill_messages() {
        let m = build_drill(&addrs(), "testnet", "20261102-a", 1_793_627_107_000);
        assert_eq!(m.subject, "SlopNinja testnet alert drill");
        let (head, body) = m.raw.split_once("\r\n\r\n").unwrap();
        let headers: Vec<&str> = head.split("\r\n").collect();
        assert_eq!(headers.len(), HEADER_LINES);
        for name in [
            "From: ",
            "To: ",
            "Date: ",
            "Message-ID: ",
            "MIME-Version: 1.0",
        ] {
            assert_eq!(
                headers.iter().filter(|h| h.starts_with(name)).count(),
                1,
                "{name}"
            );
        }
        assert!(headers.contains(&"Subject: SlopNinja testnet alert drill"));
        assert!(headers.contains(&"Date: Mon, 02 Nov 2026 13:45:07 +0000"));
        assert!(headers.contains(&"Auto-Submitted: auto-generated"));
        assert!(head.len() <= HEADER_BYTES && head.is_ascii());
        let text = qp_decode(body);
        assert!(text.starts_with(
            "This is a drill of the SlopNinja testnet alerts relay. It needs no action."
        ));
        assert!(text.contains("Drill 20261102-a, sent 2026-11-02T13:45:07Z."));
        assert!(!text.contains('@'), "no address in the body");
        assert_eq!(
            build_drill(&addrs(), "testnet", "20261102-a", 5).message_id,
            m.message_id
        );
        assert_ne!(
            build_drill(&addrs(), "testnet", "20261102-b", 5).message_id,
            m.message_id
        );
        assert!(m.message_id.ends_with(".alerts@example.invalid>"));
        let longest = "d".repeat(slop_ninja_alerts_wire::MAX_STRING_BYTES);
        let t = drill_text(crate::config::NETWORK, &longest, u64::MAX);
        assert_eq!(
            t.len(),
            DRILL_FIXED_BYTES + crate::config::NETWORK.len() + longest.len()
        );
        assert!(t.len() as u64 <= crate::arena::text_bound());
        let m = build_drill(
            &addrs(),
            crate::config::NETWORK,
            &longest,
            253_402_300_799_000,
        );
        assert!(m.raw.len() as u64 <= crate::arena::message_bound(crate::arena::text_bound()));
    }

    /// The fixed text is exactly what the pieces add: an unconfirmed event
    /// at the latest time, with the longest tag and the longer source,
    /// reaches the per-event and per-message bounds exactly.
    #[test]
    fn fixed_text_bounds_are_exact() {
        assert_eq!(rfc3339(u64::MAX).len(), RFC3339_MAX_BYTES);
        let src = Source {
            name: SOURCE_RELAY,
            rules: RULES,
        };
        let mut e = ev(EventKind::Reminder, "rule", "subject", "detail");
        e.since_ms = u64::MAX;
        let one = text(std::slice::from_ref(&e), "testnet", src);
        let none = text(&[], "testnet", src);
        assert_eq!(
            none.len(),
            MESSAGE_FIXED_BYTES + "testnet".len() + RULES.len()
        );
        // The reminder's tag is the longest; it carries the first-email
        // note.
        assert_eq!(
            one.len() - none.len(),
            EVENT_FIXED_BYTES - (NOTE_BYTES - FIRST_EMAIL.len())
                + "rule".len()
                + "subject".len()
                + "detail".len()
        );
        assert!(one.contains("first email on record: its firing was deferred"));
        let mut z = ev(EventKind::Resolved, "rule", "subject", "detail");
        z.notified_ms = None;
        z.since_ms = u64::MAX;
        assert!(z.unconfirmed());
        let unconfirmed = text(std::slice::from_ref(&z), "testnet", src);
        assert_eq!(
            unconfirmed.len() - none.len(),
            EVENT_FIXED_BYTES - (LONGEST_TAG - "RESOLVED".len()) - (NOTE_BYTES - UNCONFIRMED.len())
                + "rule".len()
                + "subject".len()
                + "detail".len()
        );
        assert!(unconfirmed.contains("may not have gone out"));
    }
}
