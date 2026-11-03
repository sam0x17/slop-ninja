use serde_json::json;
use slop_ninja_detector::wikipedia::{screen_revision, screen_revision_in_word_range};

#[test]
fn longer_range_retains_historical_and_notice_guards() {
    let text = "The traveler reached the station and waited for the train. ".repeat(70);
    let review = json!({"page_id":17,"revision_id":29});
    let raw = json!({"query":{"pages":[{"pageid":17,"revisions":[{
        "revid":29,"timestamp":"2012-01-01T00:00:00Z","slots":{"main":{"content":text}}
    }]}]}});
    let rendered = json!({"parse":{"revid":29,"text":format!("<div class=\"mw-parser-output\"><p>{text}</p></div>")}});
    let cutoff = "2013-06-01T00:00:00Z";
    assert!(screen_revision(&review, &raw, &rendered, &text, cutoff).is_err());
    screen_revision_in_word_range(&review, &raw, &rendered, &text, cutoff, 650..=1000).unwrap();
    let mut changed = raw.clone();
    changed["query"]["pages"][0]["revisions"][0]["timestamp"] = json!("2026-01-01T00:00:00Z");
    assert!(
        screen_revision_in_word_range(&review, &changed, &rendered, &text, cutoff, 650..=1000)
            .is_err()
    );
    changed = raw.clone();
    changed["query"]["pages"][0]["revisions"][0]["slots"]["main"]["content"] =
        json!(format!("{text}\n{{{{copyvio}}}}"));
    assert!(
        screen_revision_in_word_range(&review, &changed, &rendered, &text, cutoff, 650..=1000)
            .is_err()
    );
    let absent = json!({"parse":{"revid":29,"text":"<p>A different passage.</p>"}});
    assert!(
        screen_revision_in_word_range(&review, &raw, &absent, &text, cutoff, 650..=1000).is_err()
    );
    let quoted = format!("{text}\"one two three four five six seven eight\"");
    changed["query"]["pages"][0]["revisions"][0]["slots"]["main"]["content"] = json!(quoted);
    let quoted_render = json!({"parse":{"revid":29,"text":format!("<div class=\"mw-parser-output\"><p>{quoted}</p></div>")}});
    assert!(
        screen_revision_in_word_range(
            &review,
            &changed,
            &quoted_render,
            &quoted,
            cutoff,
            650..=1000
        )
        .is_err()
    );
}
