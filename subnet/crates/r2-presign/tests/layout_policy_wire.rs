//! The object layout, the URL policy and the signer messages.

use slop_ninja_r2_presign::layout::{self, MAX_KEY_LEN, ObjectId, ObjectKind};
use slop_ninja_r2_presign::policy::{self, PolicyError, valid_secs};
use slop_ninja_r2_presign::wire::{self, Entry, Op, ReplyEntry, Status};

#[test]
fn keys_round_trip_and_are_bounded() {
    let id = ObjectId { cohort: u64::MAX, commitment: [0xff; 32], kind: ObjectKind::File([0xee; 32]) };
    let key = id.key();
    assert_eq!(key.len(), MAX_KEY_LEN);
    assert!(!key.overflowed());
    assert_eq!(ObjectId::parse(key.as_str()), Some(id));
    let h = ObjectId { cohort: 7, commitment: [1; 32], kind: ObjectKind::Header };
    assert_eq!(h.key().as_str(), format!("admission/v2/c7/{}/h", "01".repeat(32)));
    assert_eq!(ObjectId::parse(h.key().as_str()), Some(h));
    for bad in [
        "admission/v2/c07/0101010101010101010101010101010101010101010101010101010101010101/h",
        "admission/v2/c7/0101010101010101010101010101010101010101010101010101010101010101/x",
        "admission/v2/c7/0101010101010101010101010101010101010101010101010101010101010101/h/",
        "admission/v2/c7/0101010101010101010101010101010101010101010101010101010101010101/f/00",
        "admission/v1/c7/0101010101010101010101010101010101010101010101010101010101010101/h",
        "admission/v2/c7/0101010101010101010101010101010101010101010101010101010101010101/f/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "health/c7/0101010101010101010101010101010101010101010101010101010101010101",
    ] {
        assert_eq!(ObjectId::parse(bad), None, "{bad}");
    }
    assert!(layout::health_key(7, &[1; 32]).as_str().starts_with(layout::HEALTH_PREFIX));
    assert!(!layout::health_key(7, &[1; 32]).as_str().starts_with(layout::ADMISSION_PREFIX));
    assert_eq!(&layout::canary_body(&[9; 32])[32..], &[9; 32]);
}

/// Reference staging (`slop_ninja_reference_staging_v1`): one key per file digest, outside the event and
/// canary prefixes, parsed back only in its canonical form.
#[test]
fn reference_keys_are_digest_keys_outside_the_admission_prefixes() {
    let key = layout::reference_key(&[0xab; 32]);
    assert_eq!(key.as_str(), format!("reference/v2/files/{}", "ab".repeat(32)));
    assert!(!key.overflowed());
    assert!(!key.as_str().starts_with(layout::ADMISSION_PREFIX));
    assert!(!key.as_str().starts_with(layout::HEALTH_PREFIX));
    assert_eq!(layout::parse_reference_key(key.as_str()), Some([0xab; 32]));
    assert_eq!(ObjectId::parse(key.as_str()), None);
    for bad in [
        "reference/v2/files/",
        "reference/v2/files/ab",
        "reference/v1/files/abababababababababababababababababababababababababababababababab",
        "reference/v2/files/ABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABAB",
        "reference/v2/files/abababababababababababababababababababababababababababababababab/",
    ] {
        assert_eq!(layout::parse_reference_key(bad), None, "{bad}");
    }
}

/// The policy's values (design section 3.3).
#[test]
fn url_policy_v1_values() {
    assert_eq!(policy::POLICY_ID, "slop_ninja_upload_url_policy_v1");
    // The largest spec v3 object.
    assert_eq!(valid_secs(954_576_228), Ok(8_537));
    assert_eq!(policy::MAX_L_AT_B_MIN, 75_487_500_000);
    assert_eq!(valid_secs(75_487_500_000), Ok(604_800));
    assert_eq!(valid_secs(75_487_500_001), Err(PolicyError::BeyondPolicy));
    assert_eq!(valid_secs(0), Err(PolicyError::Empty));
    assert_eq!(valid_secs(1), Ok(901));
    for l in [1u64, 999, 125_000, 954_576_228, 5_363_466_240] {
        let v = u64::from(valid_secs(l).unwrap());
        assert!((policy::SKEW_SECS + 1..=policy::R2_MAX_VALID_SECS).contains(&v), "{l} {v}");
    }
}

#[test]
fn signer_messages_round_trip() {
    let entries = [
        Entry { op: Op::Header, cohort: 5, commitment: [1; 32], sha256: [2; 32], len: 900, valid_secs: 901 },
        Entry { op: Op::File, cohort: 5, commitment: [1; 32], sha256: [3; 32], len: 681_508_964, valid_secs: 8_537 },
        Entry { op: Op::Reference, cohort: 0, commitment: [0; 32], sha256: [4; 32], len: 681_508_964, valid_secs: 8_537 },
    ];
    let mut buf = [0u8; wire::request_len(3)];
    assert_eq!(wire::encode_request(&entries, &mut buf), Some(buf.len()));
    let back: Vec<Entry> = wire::decode_request(&buf, 7).unwrap().map(Option::unwrap).collect();
    assert_eq!(back, entries);
    assert!(wire::decode_request(&buf, 1).is_none(), "more entries than the bound");
    assert!(wire::decode_request(&buf[..buf.len() - 1], 7).is_none());
    let mut bad = buf;
    bad[0] = 9;
    assert!(wire::decode_request(&bad, 7).is_none());
    let mut zero = [0u8; 2];
    zero[0] = wire::VERSION;
    assert!(wire::decode_request(&zero, 7).is_none());
    let mut bad_op = buf;
    bad_op[2] = 99;
    assert_eq!(wire::decode_request(&bad_op, 7).unwrap().next(), Some(None));

    let mut out = vec![0u8; wire::reply_max(2)];
    let mut at = wire::encode_reply_head(&mut out, 2).unwrap();
    let e1 = ReplyEntry { status: Status::Ok, signed_at: 10, valid_secs: 901, content_length: 900, checksum: "AAAA", url: "https://x/y" };
    let e2 = ReplyEntry { status: Status::Length, signed_at: 0, valid_secs: 0, content_length: 0, checksum: "", url: "" };
    at = wire::encode_reply_entry(&mut out, at, &e1).unwrap();
    at = wire::encode_reply_entry(&mut out, at, &e2).unwrap();
    let got: Vec<ReplyEntry<'_>> = wire::decode_reply(&out[..at], 2).unwrap().map(Option::unwrap).collect();
    assert_eq!(got, vec![e1, e2]);
    assert!(wire::decode_reply(&out[..at], 1).is_none());
    assert!(wire::decode_reply(&out[..at - 1], 2).is_none());
    assert!(wire::decode_reply(&out[..at + 1], 2).is_none(), "trailing byte");
}
