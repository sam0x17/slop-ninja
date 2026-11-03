//! Read a Pangram public report without an account or a new inference request.
//! This experimental web endpoint is distinct from the documented task API.

use anyhow::{Context, Result, anyhow, ensure};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{io::Read, time::Duration};

use crate::util::digest;

const MAX_BYTES: u64 = 2 * 1024 * 1024;

fn report_url(id: &str) -> Result<String> {
    ensure!(
        id.len() == 36
            && id.bytes().enumerate().all(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    b == b'-'
                } else {
                    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
                }
            }),
        "Expected a lowercase report UUID, not a URL"
    );
    Ok(format!("https://web.pangram.com/api/history/{id}/"))
}

/// A single GET to a fixed provider origin. No API key, cookies, redirects or
/// automatic retry. Retain these raw bytes alongside the verified observation.
pub fn fetch(id: &str) -> Result<Vec<u8>> {
    let url = report_url(id)?;
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(20))
        .build()?;
    let response = client
        .get(url)
        .header("Accept", "application/json")
        .send()
        .map_err(|_| anyhow!("Pangram public report transport failed"))?;
    ensure!(
        response.status().as_u16() == 200,
        "Public report unavailable: HTTP {}",
        response.status()
    );
    let mut bytes = Vec::new();
    response
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("Pangram public report body could not be read"))?;
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "Public report exceeds size limit"
    );
    Ok(bytes)
}

/// Check provider data against a caller's committed text, reported version and
/// time window. Authenticity comes from fetching the provider directly, not
/// from this function accepting a local JSON file. Replay accounting and
/// editorial judgments belong to the caller.
pub fn verify(
    bytes: &[u8],
    id: &str,
    expected_text: &str,
    expected_version: &str,
    not_before: DateTime<Utc>,
    before: DateTime<Utc>,
) -> Result<Value> {
    let url = report_url(id)?;
    ensure!(not_before < before, "Invalid evaluation window");
    ensure!(
        !expected_text.is_empty() && !expected_version.is_empty(),
        "Text and version required"
    );
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "Public report exceeds size limit"
    );
    let report: Value = serde_json::from_slice(bytes)?;
    ensure!(
        report["uuid"] == id && report["is_public"] == true && report["share_access"] == "public",
        "Report identity or public access differs"
    );
    let payload = &report["response_payload"];
    ensure!(
        report["prompt"] == expected_text && payload["text"] == expected_text,
        "Exact text differs; do not silently normalize the committed candidate"
    );
    ensure!(
        payload["version"] == expected_version
            && report["response"]["overall"]["version"] == expected_version,
        "Reported model version differs"
    );
    let timestamp = DateTime::parse_from_rfc3339(
        report["timestamp"]
            .as_str()
            .context("Missing report timestamp")?,
    )?
    .with_timezone(&Utc);
    ensure!(
        timestamp >= not_before && timestamp < before,
        "Report outside evaluation window"
    );
    let fields = ["fraction_ai", "fraction_ai_assisted", "fraction_human"];
    let mut fractions = [0.0; 3];
    for (i, field) in fields.iter().enumerate() {
        let fraction = payload[field]
            .as_f64()
            .context("Missing document fraction")?;
        ensure!(
            fraction.is_finite() && (0.0..=1.0).contains(&fraction),
            "Invalid document fraction"
        );
        ensure!(
            report["response"]["overall"][field].as_f64() == Some(fraction),
            "Public score views disagree"
        );
        fractions[i] = fraction;
    }
    // Float serialization tolerance for a probability partition, not an audit
    // tolerance and never an expansion of the reward threshold.
    ensure!(
        (fractions.iter().sum::<f64>() - 1.0).abs() <= 1e-6,
        "Fractions do not sum to one"
    );
    let combined = fractions[0] + fractions[1];
    Ok(json!({
        "schema":"slopninja-public-pangram-observation-v1",
        "evidence_kind":"provider_hosted_public_record",
        "report_id":id,"report_url":format!("https://www.pangram.com/history/{id}"),
        "retrieval_url":url,"raw_response_sha256":hex::encode(Sha256::digest(bytes)),
        "text_sha256":digest(expected_text),"version":expected_version,"timestamp":timestamp,
        "fraction_ai":fractions[0],"fraction_ai_assisted":fractions[1],"fraction_human":fractions[2],
        "ai_plus_assisted":combined,"below_ten_percent":combined < 0.10,
        "zero_ai_plus_assisted":combined == 0.0,
        "requested_model_selector_verified":false,"offline_cryptographic_proof":false,
        "replay_checked":false,"quality_checked":false
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "00000000-0000-4000-8000-000000000000";

    fn fixture() -> Value {
        let payload = json!({"text":"Synthetic candidate.","version":"4.0","fraction_ai":0.0,"fraction_ai_assisted":0.095,"fraction_human":0.905});
        json!({"uuid":ID,"is_public":true,"share_access":"public","prompt":"Synthetic candidate.","timestamp":"2026-09-09T12:00:00Z","response_payload":payload,"response":{"overall":payload}})
    }

    fn check(value: &Value, text: &str) -> Result<Value> {
        verify(
            &serde_json::to_vec(value)?,
            ID,
            text,
            "4.0",
            "2026-09-09T00:00:00Z".parse()?,
            "2026-09-10T00:00:00Z".parse()?,
        )
    }

    #[test]
    fn bindings_and_public_origin_fail_closed() {
        assert!(check(&fixture(), "Synthetic candidate.").is_ok());
        assert!(check(&fixture(), "Synthetic candidate.\n").is_err());
        for bad in [
            "https://elsewhere.test/report",
            "../private",
            "00000000-0000-4000-8000-000000000000?x",
        ] {
            assert!(report_url(bad).is_err());
        }
        for (field, bad) in [
            ("is_public", json!(false)),
            ("uuid", json!("wrong")),
            ("timestamp", json!("2026-09-08T12:00:00Z")),
        ] {
            let mut value = fixture();
            value[field] = bad;
            assert!(check(&value, "Synthetic candidate.").is_err());
        }
    }

    #[test]
    fn assisted_fraction_counts_and_tolerance_does_not_expand_threshold() {
        let mut value = fixture();
        assert_eq!(
            check(&value, "Synthetic candidate.").unwrap()["below_ten_percent"],
            true
        );
        for combined in [0.10, 0.11] {
            value["response_payload"]["fraction_ai_assisted"] = json!(combined);
            value["response_payload"]["fraction_human"] = json!(1.0 - combined);
            value["response"]["overall"] = value["response_payload"].clone();
            let result = check(&value, "Synthetic candidate.").unwrap();
            assert_eq!(result["below_ten_percent"], false);
            assert_eq!(result["zero_ai_plus_assisted"], false);
        }
        value["response_payload"]["fraction_ai_assisted"] = json!(0.0);
        assert!(check(&value, "Synthetic candidate.").is_err());
    }
}
