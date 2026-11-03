//! Keys from the frozen pre-V6 source ledger, including retired Test roots.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use slop_ninja_detector::dataset::sha256;
use std::{collections::BTreeSet, fs, path::Path};

pub const HISTORY_SHA256: &str = "62a4f08c839fdbc05700a224187933910bfa6cf1d94aabecdb6a367527822e8f";
pub const FRESH_SHA256: &str = "195a632721f370f84b5ab3bc8c71a5e7f620fdb1c3a4796e04c639cef2887ad4";
pub struct LongReview {
    pub policy: &'static str,
    pub words: [usize; 2],
    pub domain: &'static str,
    pub version: u8,
}
pub fn long_review(extended: bool) -> LongReview {
    if extended {
        LongReview {
            policy: include_str!("../../../LONG_DOCUMENT_DATA_V2.md"),
            words: [1200, 1600],
            domain: "slop-ninja-cmu-long-review-v2",
            version: 2,
        }
    } else {
        LongReview {
            policy: include_str!("../../../LONG_DOCUMENT_DATA_V1.md"),
            words: [650, 1000],
            domain: "slop-ninja-cmu-long-review-v1",
            version: 1,
        }
    }
}
pub struct History {
    pub groups: BTreeSet<String>,
    pub exact: BTreeSet<String>,
    pub lexical: BTreeSet<String>,
}
pub fn load(path: &Path) -> Result<History> {
    let bytes = fs::read(path)?;
    ensure!(
        sha256(bytes.as_slice()) == HISTORY_SHA256,
        "Changed historical source ledger"
    );
    let ledger: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        ledger["schema"] == "slop-ninja-v6-canonical-source-ledger-v1",
        "Wrong ledger schema"
    );
    let keys = |name: &str| -> Result<BTreeSet<String>> {
        Ok(ledger["indexes"][name]
            .as_object()
            .context("Missing historical index")?
            .keys()
            .cloned()
            .collect())
    };
    let mut groups = keys("canonical_work")?;
    groups.extend(keys("source_group")?);
    Ok(History {
        groups,
        exact: keys("exact_text_sha256")?,
        lexical: keys("lexical_text_sha256")?,
    })
}
