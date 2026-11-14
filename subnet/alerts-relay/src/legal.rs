//! The legal stream (`slop_ninja.detect.legal_notice/v1`): removal notices
//! from the detect Worker, delivered by email to the legal mailbox's
//! forward target.
//!
//! - **Input.** The detect Worker's queue consumer posts one notice per case
//!   to `POST /v1/legal` through its service binding: the case id, its kind,
//!   when it was received, its targets (public slugs, or "private
//!   detection") and the notice as the claimant wrote it, a secret page's
//!   link and slug masked (the detect Worker's sealed copy keeps them, so
//!   none reaches this relay's state or the mailbox). The relay checks
//!   the post's exact shape ([`parse`]) within its bound
//!   ([`LEGAL_BODY_BYTES`]), keeps it in its state object until it is
//!   delivered, and answers 200 once it is kept (or was delivered before),
//!   so the detect Worker's queue acknowledges exactly then.
//! - **Delivery.** The relay's one-minute tick sends what is waiting as one
//!   digest email ([`digest_len`], [`build`]): a flood of notices is at most
//!   one email a minute, each notice is emailed once (its case id is the
//!   event id of the sent-ledger), and a notice is deleted from the relay's
//!   state as soon as its email is sent. A failed send keeps it for the next
//!   tick; notices left waiting longer than [`UNDELIVERED_MS`] withhold the
//!   relay's external dead-man ping, so the operator is paged.
//! - **Recipient.** Only the secret `LEGAL_EMAIL_TO`, from the secret
//!   `ALERT_EMAIL_FROM` (the zone's sender). No tracked file names an
//!   address, and the binding carries no destination list: the relay sends
//!   to that secret alone. Without it the stream is off and a post answers
//!   503, so the detect Worker's retries end in its dead-letter queue, which
//!   raises an alert; the case and its notice stay in the detect store
//!   either way.
//! - **Content.** The email carries the notice (the claimant's name and
//!   contact included: the legal mailbox needs them to review and reply),
//!   never an excerpt of a detection's text. The relay's feed carries
//!   nothing of the legal stream, and its logs only counts.

use serde::Deserialize;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use crate::mime::{self, Addresses, Message};

/// The route.
pub const LEGAL_PATH: &str = "/v1/legal";
/// The post's schema.
pub const LEGAL_SCHEMA: &str = "slop_ninja.detect.legal_notice/v1";
/// The subject's prefix: the stream sends to the forward target of the
/// legal mailbox, so the subject names the mailbox it stands for.
pub const SUBJECT_PREFIX: &str = "[legal]";

/// The detect Worker's notice bounds (`subnet/detect-worker/src/cases.rs`;
/// a test checks they agree): the removal form's field bytes, and the
/// notice's JSON with every byte escaped to at most two.
pub const NOTICE_FIELD_BYTES: u64 = 256 * 1024;
pub const MAX_FIELDS: u64 = 9;
pub const MAX_FIELD_NAME: u64 = 16;
pub const NOTICE_JSON_BYTES: u64 =
    2 * NOTICE_FIELD_BYTES + MAX_FIELDS * (MAX_FIELD_NAME + 8) + 4096;

/// A post's bound: the notice's JSON bound, rounded up to the next power of
/// two (1 MiB), which leaves about half a megabyte for the envelope and the
/// case's targets (a target is at most "private detection" and its quotes).
pub const LEGAL_BODY_BYTES: u64 = NOTICE_JSON_BYTES.next_power_of_two();
const _: () = assert!(LEGAL_BODY_BYTES >= NOTICE_JSON_BYTES + 256 * 1024);

/// A case id: `td-` and 8 characters of the slug alphabet.
const CASE_ID_PREFIX: &str = "td-";
const SLUG_ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
/// The case kinds the form's tracks name.
pub const KINDS: &[&str] = &[
    "copyright",
    "author",
    "personal_data",
    "wrong_result",
    "illegal",
    "counter_notice",
    "court_order",
    "other_legal",
];
/// A secret target's stand-in.
pub const PRIVATE_TARGET: &str = "private detection";

/// Notices left waiting this long withhold the external dead-man ping: a
/// quarter hour is fifteen ticks of failed sends.
pub const UNDELIVERED_MS: u64 = 15 * 60 * 1000;
/// How long the sent-ledger keeps a case id (a redelivery of an old
/// message after that would send its notice again, which errs the safe
/// way): the detect queue's retries end within a day.
pub const SENT_RETAIN_MS: u64 = 30 * 86_400_000;

/// One post.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegalNotice {
    pub schema: String,
    pub case_id: String,
    pub kind: String,
    pub received_at: u64,
    pub targets: Vec<String>,
    /// The notice as the claimant wrote it (a secret page's link and slug
    /// masked by the detect Worker): a JSON object, kept as its bytes (never
    /// parsed into a map, so it holds no more memory than its length).
    pub notice: Box<RawValue>,
}

/// Why a post was refused (content-free).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Body,
    Schema,
    CaseId,
    Kind,
    Target,
    Notice,
}

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Refusal::Body => "body",
            Refusal::Schema => "schema",
            Refusal::CaseId => "case_id",
            Refusal::Kind => "kind",
            Refusal::Target => "target",
            Refusal::Notice => "notice",
        }
    }
}

/// Whether `s` is a case id.
pub fn valid_case_id(s: &str) -> bool {
    s.strip_prefix(CASE_ID_PREFIX)
        .is_some_and(|r| r.len() == 8 && r.bytes().all(|b| SLUG_ALPHABET.contains(&b)))
}

fn valid_target(s: &str) -> bool {
    s == PRIVATE_TARGET || (s.len() == 7 && s.bytes().all(|b| SLUG_ALPHABET.contains(&b)))
}

/// Parses a post (the caller has bounded its length).
pub fn parse(body: &[u8]) -> Result<LegalNotice, Refusal> {
    let n: LegalNotice = serde_json::from_slice(body).map_err(|_| Refusal::Body)?;
    if n.schema != LEGAL_SCHEMA {
        return Err(Refusal::Schema);
    }
    if !valid_case_id(&n.case_id) {
        return Err(Refusal::CaseId);
    }
    if !KINDS.contains(&n.kind.as_str()) {
        return Err(Refusal::Kind);
    }
    if n.targets.is_empty() || !n.targets.iter().all(|t| valid_target(t)) {
        return Err(Refusal::Target);
    }
    if !n.notice.get().starts_with('{') {
        return Err(Refusal::Notice);
    }
    Ok(n)
}

/// The fixed text around one notice in a digest (its lines and labels),
/// besides the post's own bytes, at most.
pub const NOTICE_FIXED_BYTES: u64 = 512;
/// The digest's own fixed text (its opening and closing lines).
pub const DIGEST_FIXED_BYTES: u64 = 512;
/// A digest's text, at most: one post at its bound with its fixed text, or
/// as many smaller ones as fit in the same bytes.
pub const DIGEST_TEXT_BYTES: u64 = LEGAL_BODY_BYTES + NOTICE_FIXED_BYTES + DIGEST_FIXED_BYTES;

/// How many of the waiting posts (their byte lengths, oldest first) the
/// next digest takes: as many as fit [`DIGEST_TEXT_BYTES`], and always the
/// first.
pub fn digest_len(lens: &[u64]) -> usize {
    let mut total = DIGEST_FIXED_BYTES;
    let mut n = 0;
    for l in lens {
        let next = total + l + NOTICE_FIXED_BYTES;
        if n > 0 && next > DIGEST_TEXT_BYTES {
            break;
        }
        total = next;
        n += 1;
    }
    n
}

/// The digest's subject: the prefix, then the case or the count (ASCII,
/// within [`mime::SUBJECT_LINE`] with its header name).
pub fn subject(notices: &[LegalNotice]) -> String {
    match notices {
        [one] => format!("{SUBJECT_PREFIX} Removal request {}", one.case_id),
        many => format!("{SUBJECT_PREFIX} {} removal requests", many.len()),
    }
}

/// The digest's `Message-ID`, from its case ids (a digest sent again after
/// an unknown outcome carries the same one).
pub fn message_id(notices: &[LegalNotice], domain: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"slop_ninja.legal_digest_message_id/v1");
    for n in notices {
        h.update((n.case_id.len() as u64).to_be_bytes());
        h.update(n.case_id.as_bytes());
    }
    let d = hex::encode(h.finalize());
    format!("<{}.legal@{}>", &d[..32], domain)
}

/// The digest's plain text: for each notice its case, kind, received time,
/// targets, the operator's command to open the case, and the notice exactly
/// as it came (JSON, which escapes every control character, so a
/// claimant's text cannot forge a line). Its length is at most the posts'
/// bytes plus the fixed text.
pub fn text(notices: &[LegalNotice]) -> String {
    let mut t = String::new();
    t.push_str(
        "Removal requests received by the SlopNinja detector. Each case's status page is the\n",
    );
    t.push_str("claimant's; no email was sent to them. Reply from the legal mailbox.\n");
    for n in notices {
        t.push_str("\n==== Case ");
        t.push_str(&n.case_id);
        t.push_str(" (");
        t.push_str(&n.kind);
        t.push_str("), received ");
        t.push_str(&mime::rfc3339(n.received_at));
        t.push_str("\nPages: ");
        t.push_str(&n.targets.join(", "));
        t.push_str("\nOpen it: detect-op case_show --case ");
        t.push_str(&n.case_id);
        t.push_str("\n\nThe notice as written (JSON):\n");
        t.push_str(n.notice.get());
        t.push('\n');
    }
    t.push_str(
        "\n-- \nSent by the private alerts relay's legal stream, at most one digest a minute.\n",
    );
    t
}

/// Builds the digest for `notices` (never empty).
pub fn build(addrs: &Addresses, network: &str, notices: &[LegalNotice], now_ms: u64) -> Message {
    mime::assemble_message(
        addrs,
        network,
        subject(notices),
        message_id(notices, addrs.domain()),
        text(notices),
        now_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::Value;

    fn post(case_id: &str, notice: Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema": LEGAL_SCHEMA,
            "case_id": case_id,
            "kind": "copyright",
            "received_at": 1_795_169_040_000u64,
            "targets": ["k7m2xq9", PRIVATE_TARGET],
            "notice": notice,
        }))
        .unwrap()
    }

    #[test]
    fn posts_are_checked_to_their_exact_shape() {
        let n = parse(&post(
            "td-8k2m4q7x",
            serde_json::json!({"fields": {"name": "A"}}),
        ))
        .unwrap();
        assert_eq!(n.targets.len(), 2);
        for (body, want) in [
            (post("td-8k2m4q7", serde_json::json!({})), Refusal::CaseId),
            (post("td-8K2M4Q7X", serde_json::json!({})), Refusal::CaseId),
            (
                post("td-8k2m4q7x", serde_json::json!("text")),
                Refusal::Notice,
            ),
            (b"{}".to_vec(), Refusal::Body),
        ] {
            assert_eq!(parse(&body).err(), Some(want));
        }
        let mut v: Value =
            serde_json::from_slice(&post("td-8k2m4q7x", serde_json::json!({}))).unwrap();
        v["schema"] = "slop_ninja.publisher.alert/v3".into();
        assert_eq!(
            parse(&serde_json::to_vec(&v).unwrap()).err(),
            Some(Refusal::Schema)
        );
        v["schema"] = LEGAL_SCHEMA.into();
        v["kind"] = "x_request".into();
        assert_eq!(
            parse(&serde_json::to_vec(&v).unwrap()).err(),
            Some(Refusal::Kind)
        );
        v["kind"] = "copyright".into();
        v["targets"] = serde_json::json!(["https://site.example/d/k7m2xq9"]);
        assert_eq!(
            parse(&serde_json::to_vec(&v).unwrap()).err(),
            Some(Refusal::Target)
        );
        v["targets"] = serde_json::json!(["k7m2xq9"]);
        v["extra"] = 1.into();
        assert_eq!(
            parse(&serde_json::to_vec(&v).unwrap()).err(),
            Some(Refusal::Body)
        );
    }

    #[test]
    fn a_digest_takes_what_fits_and_always_the_first() {
        assert_eq!(digest_len(&[]), 0);
        assert_eq!(digest_len(&[LEGAL_BODY_BYTES, 10]), 1);
        assert_eq!(digest_len(&[10, LEGAL_BODY_BYTES]), 1);
        assert_eq!(digest_len(&[1000; 50]), 50);
        let small = 2048;
        let n = digest_len(&vec![small; 10_000]);
        assert!(
            n > 100
                && (n as u64) * (small + NOTICE_FIXED_BYTES) + DIGEST_FIXED_BYTES
                    <= DIGEST_TEXT_BYTES
        );
    }

    #[test]
    fn the_digest_names_the_case_and_carries_the_notice_on_its_own_lines() {
        let a = parse(&post(
            "td-8k2m4q7x",
            serde_json::json!({"v": 1, "track": "copyright", "target": "https://site.example/d/k7m2xq9",
                               "fields": {"name": "Claimant\nSubject: forged", "work": "My essay"}}),
        ))
        .unwrap();
        let mut b = a.clone();
        b.case_id = "td-0000aaaa".into();
        let addrs = Addresses::new(
            "sender@mail.example.invalid",
            "legal-forward@example.invalid",
        )
        .unwrap();
        let m = build(
            &addrs,
            "testnet",
            std::slice::from_ref(&a),
            1_795_169_100_000,
        );
        assert_eq!(m.to, "legal-forward@example.invalid");
        assert!(m.subject.starts_with("[legal] ") && m.subject.contains("td-8k2m4q7x"));
        assert!(m.subject.is_ascii() && "Subject: ".len() + m.subject.len() <= mime::SUBJECT_LINE);
        assert!(m.raw.contains("To: <legal-forward@example.invalid>"));
        let t = text(std::slice::from_ref(&a));
        assert!(
            t.contains(r#""name":"Claimant\nSubject: forged""#) && !t.contains("\nSubject: forged"),
            "{t}"
        );
        assert!(t.contains("detect-op case_show --case td-8k2m4q7x"));
        assert!(t.contains("Pages: k7m2xq9, private detection"));
        // The text stays within the posts' bytes and the fixed text.
        let body = post("td-8k2m4q7x", serde_json::from_str(a.notice.get()).unwrap()).len() as u64;
        assert!(
            (text(std::slice::from_ref(&a)).len() as u64)
                <= body + NOTICE_FIXED_BYTES + DIGEST_FIXED_BYTES
        );
        let two = build(&addrs, "testnet", &[a.clone(), b.clone()], 1);
        assert!(two.subject.ends_with("2 removal requests"));
        assert_ne!(two.message_id, m.message_id);
        assert_eq!(
            build(&addrs, "testnet", &[a, b], 9).message_id,
            two.message_id
        );
    }

    #[test]
    fn no_tracked_setting_names_an_address() {
        // Δ2: the relay's recipients are secrets; the binding carries no
        // destination list, and no setting line holds an address.
        let w =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/wrangler.toml")).unwrap();
        for line in w.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
            assert!(!line.contains('@'), "{line}");
            assert!(!line.contains("destination_address"), "{line}");
        }
    }

    #[test]
    fn the_bounds_are_the_detect_workers() {
        let cases = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../detect-worker/src/cases.rs"
        ))
        .unwrap();
        for line in [
            "pub const NOTICE_FIELD_BYTES: usize = 256 * 1024;",
            "pub const MAX_FIELDS: usize = 9;",
            "pub const MAX_FIELD_NAME: usize = 16;",
            "pub const NOTICE_JSON_BYTES: usize = 2 * NOTICE_FIELD_BYTES + MAX_FIELDS * (MAX_FIELD_NAME + 8) + 4096;",
            "pub const CASE_ID_PREFIX: &str = \"td-\";",
            "pub const CASE_ID_CHARS: usize = 8;",
        ] {
            assert!(cases.contains(line), "detect-worker cases.rs: {line}");
        }
        let queue = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../detect-worker/src/queue.rs"
        ))
        .unwrap();
        assert!(
            queue.contains("pub const LEGAL_SCHEMA: &str = \"slop_ninja.detect.legal_notice/v1\";")
        );
        assert!(queue.contains("\"private detection\""));
    }
}
