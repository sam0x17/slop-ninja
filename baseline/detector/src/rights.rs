//! Explicit release obligations for the Wikipedia-derived corpus lane.
use crate::dataset::{OriginRecord, Rights, sha256};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const POLICY: &str = "slop_ninja_wikipedia_share_alike_v1";
pub const LICENSE: &str = "CC-BY-SA-4.0";
pub const LICENSE_URL: &str = "https://creativecommons.org/licenses/by-sa/4.0/";

/// Preserve the original Apache lane; add only the individually reviewed Phi-4 grant.
pub fn generator_admitted(license: &str, revision: &str, url: &str, grant_hash: &str) -> bool {
    license == "Apache-2.0"
        || (license == "MIT"
            && revision == "mlx-community/phi-4-4bit@fc0f8f23d369dc29b55cad1d65cb5bf0dcbee910"
            && url
                == "https://huggingface.co/microsoft/phi-4/resolve/2db69c1c3e91a05d2c64a3185acfbaf36f744e25/LICENSE"
            && grant_hash == "c49419617a6070bcb197cfe272f7007fdec3e790dbb529cb995473bd69c0bd51")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShareAlike {
    pub policy: String,
    pub source_title: String,
    pub article_url: String,
    pub history_url: String,
    pub source_license: String,
    pub source_license_url: String,
    pub review_sha256: String,
    pub notices: Vec<String>,
    pub changes: String,
}

pub fn validate(rights: &Rights) -> Result<()> {
    let is_sa = matches!(rights.license.as_str(), "CC-BY-SA-3.0" | "CC-BY-SA-4.0");
    ensure!(
        is_sa == rights.share_alike.is_some(),
        "ShareAlike requires explicit release obligations"
    );
    if let Some(sa) = &rights.share_alike {
        ensure!(
            sa.policy == POLICY
                && !sa.source_title.trim().is_empty()
                && sa.source_license == "CC-BY-SA-3.0"
                && sa.source_license_url == "https://creativecommons.org/licenses/by-sa/3.0/"
                && !sa.changes.trim().is_empty()
                && rights.redistribute_text,
            "Incomplete or unsupported ShareAlike policy"
        );
        for url in [&sa.article_url, &sa.history_url] {
            let url = reqwest::Url::parse(url)?;
            ensure!(
                url.scheme() == "https"
                    && url.host_str() == Some("en.wikipedia.org")
                    && url.path() == "/w/index.php",
                "Expected Wikipedia attribution URL"
            );
        }
        ensure!(
            sa.review_sha256.len() == 64
                && sa.review_sha256.bytes().all(|b| b.is_ascii_hexdigit())
                && !sa.notices.is_empty()
                && sa.notices.iter().all(|n| !n.trim().is_empty()),
            "Missing reviewed source notices"
        );
    }
    Ok(())
}

/// Text-free notices travel with every exported shard and trained bundle.
pub fn release_manifest(records: &[OriginRecord], partitions: &Value) -> Value {
    let notices: Vec<_> = records
        .iter()
        .filter_map(|r| notice(&r.id, &r.text_sha256, &r.rights))
        .collect();
    manifest_for_notices(notices, partitions)
}

pub fn notice(id: &str, text_sha256: &str, rights: &Rights) -> Option<Value> {
    rights.share_alike.as_ref().map(|sa| {
        json!({"record_id":id,"text_sha256":text_sha256,
        "attribution":rights.attribution,"text_license":rights.license,"obligations":sa})
    })
}

pub fn manifest_for_notices(notices: Vec<Value>, partitions: &Value) -> Value {
    let content = json!({"schema":"slop_ninja_data_release_obligations_v1",
        "partitions":partitions,"share_alike_notices":notices,
        "model_release_license":if notices.is_empty() { None } else { Some(LICENSE) },
        "model_release_license_url":if notices.is_empty() { None } else { Some(LICENSE_URL) },
        "policy":"For this corpus lane, distribute fine-tuned weights under CC-BY-SA-4.0 as a project choice; retain base-model notices and this attribution manifest. Code retains its own license."});
    json!({"content_sha256":sha256(serde_json::to_vec(&content).expect("JSON value")),"content":content})
}
