//! Protocol fixtures only. These invented strings have no production-history
//! evidentiary value and must never support detector performance claims.

use slop_ninja_detector::dataset::{
    Evidence, Origin, OriginRecord, RECORD_SCHEMA, Rights, Source, Split, assign_splits,
    read_records, sha256, validate_records, write_records,
};

fn fixture(id: &str, group: &str, text: &str) -> OriginRecord {
    OriginRecord {
        schema: RECORD_SCHEMA.into(),
        id: id.into(),
        source_group: group.into(),
        split: None,
        origin: Origin::HumanOnly,
        evidence: Evidence::SyntheticFixture,
        evidence_notes:
            "Invented protocol fixture; origin field is a test label, not provenance evidence."
                .into(),
        text: text.into(),
        text_sha256: sha256(text),
        source: Source {
            collection: "synthetic-contract-fixture".into(),
            url: format!("fixture://contract/{id}"),
            version: "fixture-v1".into(),
            published_at: None,
            author_ids: Vec::new(),
            raw_path: String::new(),
            raw_sha256: sha256(text),
            extraction: "original-fixture-v1".into(),
        },
        rights: Rights {
            license: "Synthetic-Original".into(),
            evidence_url: "fixture://contract/original-notice".into(),
            evidence_sha256: sha256("original synthetic protocol fixture"),
            attribution: "Slop Ninja synthetic protocol fixture".into(),
            commercial_training: true,
            model_release: true,
            external_evaluation: false,
            redistribute_text: true,
            share_alike: None,
        },
        parent_id: None,
        generation: None,
    }
}

#[test]
fn manifest_roundtrip_rejects_text_and_hash_tampering_without_overwriting() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("records.jsonl");
    let original = fixture("one", "one", "The violet jar contains a paper moon.");
    write_records(&path, std::slice::from_ref(&original)).unwrap();
    let restored = read_records(&path).unwrap();
    assert_eq!(restored[0].text_sha256, original.text_sha256);
    assert!(write_records(&path, std::slice::from_ref(&original)).is_err());

    let mut altered_text = original.clone();
    altered_text.text.push_str(" A blue spoon waits nearby.");
    assert!(
        altered_text
            .validate()
            .unwrap_err()
            .to_string()
            .contains("text hash mismatch")
    );
    let mut altered_hash = original;
    altered_hash.text_sha256 = sha256("different payload");
    let invalid_path = temporary.path().join("invalid.jsonl");
    std::fs::write(&invalid_path, serde_json::to_string(&altered_hash).unwrap()).unwrap();
    assert!(read_records(&invalid_path).is_err());
}

#[test]
fn admission_rejects_unknown_evidence_unapproved_rights_and_inconsistent_origin() {
    let original = fixture(
        "one",
        "one",
        "An amber kite rests beside the empty cabinet.",
    );
    let mut unknown = original.clone();
    unknown.evidence = Evidence::Unknown;
    assert!(unknown.validate().is_err());

    let mut unsupported = original.clone();
    unsupported.rights.license = "CC-BY-NC-4.0".into();
    assert!(
        unsupported
            .validate()
            .unwrap_err()
            .to_string()
            .contains("license requires a separate admission policy")
    );
    let mut no_training_grant = original.clone();
    no_training_grant.rights.commercial_training = false;
    assert!(no_training_grant.validate().is_err());
    let mut no_release_grant = original.clone();
    no_release_grant.rights.model_release = false;
    assert!(no_release_grant.validate().is_err());

    let mut inconsistent = original;
    inconsistent.evidence = Evidence::HistoricalProxy;
    inconsistent.origin = Origin::ModelOnly;
    assert!(inconsistent.validate().is_err());
}

#[test]
fn normalized_duplicates_cannot_cross_partitions() {
    let mut first = fixture("first", "family-a", "The café has a violet window.");
    let mut second = fixture("second", "family-b", "THE CAFE\u{301} HAS A VIOLET WINDOW!");
    assert_ne!(first.text_sha256, second.text_sha256);
    first.split = Some(Split::Train);
    second.split = Some(Split::Test);
    let error = validate_records(&[first, second]).unwrap_err().to_string();
    assert!(error.contains("duplicate crosses splits"), "{error}");
}

#[test]
fn share_alike_requires_notices_and_descendants_cannot_drop_them() {
    use slop_ninja_detector::rights::{POLICY, ShareAlike};
    let mut parent = fixture(
        "parent-sa",
        "source-sa",
        "A synthetic violet window stands beside the painted clock.",
    );
    parent.evidence = Evidence::HistoricalProxy;
    parent.rights.license = "CC-BY-SA-3.0".into();
    assert!(parent.validate().is_err());
    parent.rights.share_alike = Some(ShareAlike {
        policy: POLICY.into(),
        source_title: "Synthetic source contract".into(),
        article_url: "https://en.wikipedia.org/w/index.php?oldid=1".into(),
        history_url: "https://en.wikipedia.org/w/index.php?curid=1&action=history".into(),
        source_license: "CC-BY-SA-3.0".into(),
        source_license_url: "https://creativecommons.org/licenses/by-sa/3.0/".into(),
        review_sha256: sha256("synthetic review"),
        notices: vec!["Synthetic notice".into()],
        changes: "Synthetic fixture, no real source text.".into(),
    });
    parent.validate().unwrap();
    let mut child = parent.clone();
    child.id = "child-sa".into();
    child.parent_id = Some(parent.id.clone());
    validate_records(&[parent.clone(), child.clone()]).unwrap();
    child.rights.share_alike.as_mut().unwrap().notices.clear();
    assert!(validate_records(&[parent.clone(), child.clone()]).is_err());
    child.rights.share_alike = None;
    child.rights.license = "CC-BY-4.0".into();
    assert!(validate_records(&[parent, child]).is_err());
}

#[test]
fn parents_must_exist_and_share_group_and_partition() {
    let parent = fixture(
        "parent",
        "one-family",
        "A silver pear floats above the toy bridge.",
    );
    let mut child = fixture(
        "child",
        "other-family",
        "The toy bridge stands beneath a silver pear.",
    );
    child.parent_id = Some(parent.id.clone());
    assert!(validate_records(&[parent.clone(), child.clone()]).is_err());

    child.source_group = parent.source_group.clone();
    validate_records(&[parent.clone(), child.clone()]).unwrap();
    child.split = Some(Split::Test);
    assert!(validate_records(&[parent.clone(), child.clone()]).is_err());
    child.split = None;
    child.parent_id = Some("absent-parent".into());
    assert!(
        validate_records(&[parent, child])
            .unwrap_err()
            .to_string()
            .contains("missing parent")
    );
}

#[test]
fn transitive_duplicate_families_share_a_frozen_order_independent_split() {
    // A links to B through one text; a separate B text links B to C.
    let mut records = vec![
        fixture(
            "a-one",
            "a",
            "The copper lantern shines through a painted door.",
        ),
        fixture(
            "b-one",
            "b",
            "THE COPPER LANTERN SHINES THROUGH A PAINTED DOOR!",
        ),
        fixture(
            "b-two",
            "b",
            "A paper whale circles the tiny wooden island.",
        ),
        fixture(
            "c-one",
            "c",
            "A PAPER WHALE CIRCLES THE TINY WOODEN ISLAND!",
        ),
    ];
    let mut reversed = records.iter().rev().cloned().collect::<Vec<_>>();
    let report = assign_splits(&mut records, "contract-seed").unwrap();
    assert_eq!(report.merged_duplicate_groups, 2);
    assert!(
        records
            .iter()
            .all(|record| record.split == records[0].split)
    );
    assign_splits(&mut reversed, "contract-seed").unwrap();
    for record in &records {
        assert_eq!(
            record.split,
            reversed
                .iter()
                .find(|other| other.id == record.id)
                .unwrap()
                .split
        );
    }
    let before = serde_json::to_vec(&records).unwrap();
    assert!(
        assign_splits(&mut records, "replacement-seed")
            .unwrap_err()
            .to_string()
            .contains("Refusing to reshuffle")
    );
    assert_eq!(serde_json::to_vec(&records).unwrap(), before);
}

#[test]
fn cyclic_ancestry_is_rejected_even_when_all_groups_and_splits_match() {
    let mut a = fixture("a", "cycle-family", "A striped bowl holds a glass acorn.");
    let mut b = fixture(
        "b",
        "cycle-family",
        "The glass acorn rolls across a folded map.",
    );
    let mut c = fixture("c", "cycle-family", "A folded map covers the striped bowl.");
    a.parent_id = Some("b".into());
    b.parent_id = Some("c".into());
    c.parent_id = Some("a".into());
    assert!(
        validate_records(&[a, b, c])
            .unwrap_err()
            .to_string()
            .contains("cyclic lineage")
    );
}
