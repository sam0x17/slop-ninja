//! Dataset authorization and targets, deliberately separate from measurements.
use super::{PARTS, counter, digest_at, number, sha, str_at};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Dataset {
    pub records_sha256: String,
    pub non_test_ids: BTreeSet<String>,
    pub test_ids: BTreeSet<String>,
    pub split_assignments: BTreeMap<String, String>,
    test_mode: bool,
    references_only: bool,
}

fn id_set(value: &Value, key: &str) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for value in value[key]
        .as_array()
        .with_context(|| format!("missing {key} array"))?
    {
        let id = value
            .as_str()
            .context("manifest IDs must be strings, never records")?;
        ensure!(
            !id.is_empty() && ids.insert(id.to_owned()),
            "empty or duplicate manifest ID"
        );
    }
    Ok(ids)
}

// Unknown metadata fields, especially text and evidence, are skipped by serde.
#[derive(Deserialize)]
struct Header {
    id: String,
    metadata: MetadataHeader,
}

#[derive(Deserialize)]
struct MetadataHeader {
    id: String,
    source_group: String,
    frozen_split: String,
    purpose: String,
}

impl Dataset {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        Self::parse_mode(bytes, false, false)
    }

    pub fn parse_test(bytes: &[u8]) -> Result<Self> {
        Self::parse_mode(bytes, true, false)
    }

    pub fn parse_references(bytes: &[u8]) -> Result<Self> {
        Self::parse_mode(bytes, false, true)
    }

    // Only the selection-gated gallery route calls this explicitly separate mode.
    pub fn parse_final_references(bytes: &[u8]) -> Result<Self> {
        let d = Self::parse_mode(bytes, true, true)?;
        ensure!(
            d.non_test_ids.is_empty() && d.split_assignments.values().all(|p| p == "test"),
            "final reference scope must contain only Test families and IDs"
        );
        Ok(d)
    }

    fn parse_mode(bytes: &[u8], test_mode: bool, references_only: bool) -> Result<Self> {
        let value: Value = serde_json::from_slice(bytes)?;
        ensure!(
            value["schema"] == "slop_ninja_a_supervised_dataset_v1",
            "dataset schema"
        );
        ensure!(
            value["reference_transform_only"].as_bool().unwrap_or(false) == references_only,
            "reference-only manifest cannot enter fitting or Test transformation"
        );
        ensure!(
            value["final_test_reference_transform_only"].as_bool().unwrap_or(false)
                == (test_mode && references_only),
            "final-Test reference manifest requires its separate scope"
        );
        let non_test_ids = id_set(&value, "non_test_ids")?;
        let test_ids = id_set(&value, "test_ids")?;
        ensure!(
            if test_mode {
                !test_ids.is_empty()
            } else {
                !non_test_ids.is_empty()
            },
            "empty selected ID allow-list"
        );
        ensure!(
            non_test_ids.is_disjoint(&test_ids),
            "Test ID in non-Test allow-list"
        );
        let assignments = value["split_assignments"]
            .as_object()
            .context("split_assignments map")?;
        let mut split_assignments = BTreeMap::new();
        for (group, split) in assignments {
            let split = split
                .as_str()
                .context("split assignment must be a string")?;
            ensure!(
                !group.is_empty() && (PARTS.contains(&split) || split == "test"),
                "invalid frozen split assignment"
            );
            split_assignments.insert(group.clone(), split.to_owned());
        }
        ensure!(!split_assignments.is_empty(), "empty split assignments");
        ensure!(
            has_evidence(&value["sources"]) || has_evidence(&value["provenance"]),
            "dataset requires sources/provenance metadata"
        );
        Ok(Self {
            records_sha256: digest_at(&value, "records_sha256")?.to_owned(),
            non_test_ids,
            test_ids,
            split_assignments,
            test_mode,
            references_only,
        })
    }

    pub fn authorize_id(&self, id: &str) -> Result<()> {
        if self.test_mode {
            ensure!(
                !self.non_test_ids.contains(id) && self.test_ids.contains(id),
                "export ID absent from explicit Test allow-list"
            );
            return Ok(());
        }
        ensure!(!self.test_ids.contains(id), "reserved Test ID in export");
        ensure!(
            self.non_test_ids.contains(id),
            "export ID absent from non-Test allow-list"
        );
        Ok(())
    }

    pub fn require_exact_ids<'a>(&self, ids: impl Iterator<Item = &'a str>) -> Result<()> {
        let ids: BTreeSet<_> = ids.map(str::to_owned).collect();
        ensure!(
            ids == if self.test_mode {
                &self.test_ids
            } else {
                &self.non_test_ids
            }
            .clone(),
            "export population differs from manifest selected allow-list"
        );
        Ok(())
    }

    fn validate_header(&self, id: &str, metadata: &MetadataHeader) -> Result<()> {
        self.authorize_id(id)?;
        ensure!(id == metadata.id, "row/metadata ID mismatch");
        ensure!(
            if self.test_mode {
                metadata.frozen_split == "test"
            } else {
                PARTS.contains(&metadata.frozen_split.as_str())
            },
            "reserved Test or unknown split in metadata"
        );
        ensure!(
            self.split_assignments.get(&metadata.source_group) == Some(&metadata.frozen_split),
            "metadata source-family split differs from frozen manifest"
        );
        ensure!(
            matches!(metadata.purpose.as_str(), "query" | "reference"),
            "unknown purpose"
        );
        if self.references_only {
            ensure!(
                metadata.purpose == "reference"
                    && if self.test_mode {
                        metadata.frozen_split == "test"
                    } else {
                        matches!(metadata.frozen_split.as_str(), "development" | "calibration")
                    },
                "reference partition differs from explicit transformation scope"
            );
        } else {
            ensure!(
                metadata.purpose != "reference" || metadata.frozen_split == "train",
                "reference must be Train"
            );
        }
        Ok(())
    }

    pub fn supervision(&self, metadata: &Value) -> Result<Supervision> {
        Supervision::parse_mode(metadata, self.references_only, self.test_mode && self.references_only)
    }

    pub fn check_headers(&self, bytes: &[u8]) -> Result<()> {
        let mut ids = BTreeSet::new();
        for line in std::str::from_utf8(bytes)?.lines() {
            let header: Header = serde_json::from_str(line).context("invalid export row header")?;
            self.validate_header(&header.id, &header.metadata)?;
            ensure!(ids.insert(header.id), "duplicate row ID");
        }
        self.require_exact_ids(ids.iter().map(String::as_str))
    }

    pub fn validate_metadata(&self, id: &str, metadata: &Value) -> Result<()> {
        self.validate_header(id, &serde_json::from_value(metadata.clone())?)?;
        let text = metadata["text"].as_str().context("metadata text")?;
        // Optional producer bindings are verified as well as mandatory receipt
        // source hashes. The entire metadata is bound by records_sha256 upstream.
        for key in ["source_sha256", "text_sha256"] {
            if metadata.get(key).is_some() {
                ensure!(
                    digest_at(metadata, key)? == sha(text.as_bytes()),
                    "metadata text hash mismatch"
                );
            }
        }
        Ok(())
    }
}

fn has_evidence(value: &Value) -> bool {
    match value {
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => false,
    }
}

#[derive(Clone, Debug)]
pub(super) struct Role {
    pub present: Option<bool>,
    pub identity: Option<String>,
}

fn role(metadata: &Value, name: &str, expected_present: bool) -> Result<Role> {
    let present = match metadata
        .get(format!("{name}_role_present"))
        .context("missing role presence")?
    {
        Value::Null => None,
        Value::Bool(present) => Some(*present),
        _ => anyhow::bail!("role presence must be bool or null"),
    };
    ensure!(
        present.is_none_or(|p| p == expected_present),
        "role presence contradicts origin"
    );
    let identity = match metadata
        .get(format!("{name}_identity"))
        .context("missing role identity")?
    {
        Value::Null => None,
        Value::String(identity) if !identity.trim().is_empty() => Some(identity.clone()),
        _ => anyhow::bail!("role identity must be nonempty string or null"),
    };
    if let Some(identity) = &identity {
        ensure!(
            !matches!(identity.as_str(), "other_human" | "other_ai"),
            "fallback is not an enrolled identity"
        );
    }
    Ok(Role { present, identity })
}

#[derive(Clone, Debug)]
pub(super) struct Supervision {
    pub origin: usize,
    pub share: Option<f64>,
    pub human: Role,
    pub ai: Role,
    pub purpose: String,
    pub exposed: bool,
    pub exposure_was_unspecified: bool,
}

impl Supervision {
    pub fn parse(metadata: &Value) -> Result<Self> {
        Self::parse_mode(metadata, false, false)
    }

    fn parse_mode(metadata: &Value, references_only: bool, final_references: bool) -> Result<Self> {
        let origin = match str_at(metadata, "origin")? {
            "human_only" => 0,
            "model_only" => 1,
            "mixed" => 2,
            _ => anyhow::bail!("unsupported origin label"),
        };
        ensure!(
            has_evidence(&metadata["evidence"]),
            "missing origin evidence"
        );
        let share_evidence = &metadata["share_evidence"];
        ensure!(
            share_evidence.is_object(),
            "share_evidence must be an object"
        );
        let share = match metadata
            .get("share_target")
            .context("missing explicit share_target")?
        {
            Value::Null => None,
            value => {
                let n = number(value)?;
                ensure!(n <= 1.0, "share_target outside [0,1]");
                ensure!(origin != 0 || n == 0.0, "human-only share must be 0");
                ensure!(origin != 1 || n == 1.0, "model-only share must be 1");
                Some(n)
            }
        };
        if share_evidence["schema"] == "slop_ninja_realized_span_share_v1" {
            validate_realized_share(origin, share, metadata)?;
        } else {
            ensure!(
                origin != 2 || share.is_none(),
                "mixed share requires realized span evidence"
            );
        }
        let (exposed, exposure_was_unspecified) = match metadata.get("exposed_to_grammar") {
            None | Some(Value::Null) => (true, true),
            Some(Value::Bool(value)) => (*value, false),
            _ => anyhow::bail!("exposed_to_grammar must be bool or unspecified"),
        };
        let purpose = str_at(metadata, "purpose")?;
        ensure!(matches!(purpose, "query" | "reference"), "unknown purpose");
        if references_only {
            ensure!(
                purpose == "reference"
                    && if final_references {
                        metadata["frozen_split"] == "test"
                    } else {
                        matches!(str_at(metadata, "frozen_split")?, "development" | "calibration")
                    },
                "partition reference purpose/split"
            );
        } else {
            ensure!(
                purpose != "reference" || metadata["frozen_split"] == "train",
                "reference must be Train"
            );
        }
        Ok(Self {
            origin,
            share,
            human: role(metadata, "human", origin != 1)?,
            ai: role(metadata, "ai", origin != 0)?,
            purpose: purpose.into(),
            exposed,
            exposure_was_unspecified,
        })
    }
}

fn byte_span(value: &Value, text: &str) -> Result<(usize, usize)> {
    let span = value.as_array().context("byte span must be a pair")?;
    ensure!(span.len() == 2, "byte span must be a pair");
    let start = span[0].as_u64().context("byte span start")?;
    let end = span[1].as_u64().context("byte span end")?;
    ensure!(
        start <= end && end <= text.len() as u64,
        "byte span outside text"
    );
    let (start, end) = (start as usize, end as usize);
    ensure!(
        text.is_char_boundary(start) && text.is_char_boundary(end),
        "byte span splits UTF-8 character"
    );
    Ok((start, end))
}

fn validate_realized_share(origin: usize, share: Option<f64>, metadata: &Value) -> Result<()> {
    let evidence = &metadata["share_evidence"];
    ensure!(
        evidence["word_policy"] == "unicode_split_whitespace_v1",
        "unsupported share word policy"
    );
    let text = metadata["text"].as_str().context("final text")?;
    let source = evidence["source_text"]
        .as_str()
        .context("share source_text")?;
    let replacement = str_at(evidence, "replacement")?;
    ensure!(
        replacement.trim() == replacement,
        "replacement layout differs from generator contract"
    );
    ensure!(
        digest_at(evidence, "source_sha256")? == sha(source.as_bytes()),
        "share source digest mismatch"
    );
    ensure!(
        digest_at(evidence, "final_sha256")? == sha(text.as_bytes()),
        "share final digest mismatch"
    );
    let (start, end) = byte_span(&evidence["source_byte_span"], source)?;
    let final_span = byte_span(&evidence["final_byte_span"], text)?;
    let mode = evidence
        .get("mode")
        .or_else(|| metadata["generation"].get("mode"))
        .and_then(Value::as_str)
        .context("realized share generation mode")?;
    if let Some(generation_mode) = metadata["generation"].get("mode") {
        ensure!(generation_mode == mode, "conflicting generation modes");
    }
    match mode {
        "from_scratch" => {
            ensure!(
                origin == 1 && (start, end) == (0, 0),
                "from-scratch origin/source span"
            );
            ensure!(
                text == replacement && final_span == (0, text.len()),
                "from-scratch assembly mismatch"
            );
        }
        "controlled_rewrite" => {
            ensure!(
                origin == 2 && start < end,
                "rewrite must have mixed origin and a nonempty source span"
            );
            ensure!(
                start == 0 || source[..start].ends_with(char::is_whitespace),
                "left token boundary"
            );
            ensure!(
                end == source.len() || source[end..].starts_with(char::is_whitespace),
                "right token boundary"
            );
            ensure!(
                text == format!("{}{}{}", &source[..start], replacement, &source[end..]),
                "exact rewrite assembly mismatch"
            );
            ensure!(
                final_span == (start, start + replacement.len()),
                "replacement final span mismatch"
            );
        }
        _ => anyhow::bail!("unsupported realized share generation mode"),
    }
    ensure!(
        &text[final_span.0..final_span.1] == replacement,
        "replacement does not equal final span"
    );
    let numerator = replacement.split_whitespace().count() as u64;
    let denominator = text.split_whitespace().count() as u64;
    ensure!(
        denominator > 0 && numerator > 0 && numerator <= denominator,
        "realized share word accounting"
    );
    ensure!(
        counter(evidence, "replacement_words")? == numerator
            && counter(evidence, "final_words")? == denominator,
        "realized share word counts differ from exact text"
    );
    let realized = number(&evidence["realized_share"])?;
    let expected = numerator as f64 / denominator as f64;
    ensure!(
        realized <= 1.0 && (realized - expected).abs() <= 1e-12,
        "realized share differs from measured word fraction"
    );
    ensure!(
        share.is_none_or(|share| (share - expected).abs() <= 1e-12),
        "share_target differs from measured word fraction"
    );
    Ok(())
}

pub(super) struct Profiles {
    pub human: Vec<String>,
    pub ai: Vec<String>,
}

impl Profiles {
    pub fn from_contract(contract: &Value) -> Result<Self> {
        let read = |key: &str| -> Result<Vec<String>> {
            let profiles = contract[key]
                .as_array()
                .context("frozen role profiles")?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .context("frozen identity must be nonempty string")
                })
                .collect::<Result<Vec<_>>>()?;
            ensure!(
                profiles.len() != 1 && profiles.windows(2).all(|pair| pair[0] < pair[1]),
                "frozen profiles must be sorted, unique and have zero or at least two identities"
            );
            Ok(profiles)
        };
        Ok(Self {
            human: read("human_profiles")?,
            ai: read("ai_profiles")?,
        })
    }

    // The caller supplies only eligible Train queries, never references.
    pub fn fit<'a>(train: impl Iterator<Item = &'a Supervision>) -> Self {
        let mut human = BTreeSet::new();
        let mut ai = BTreeSet::new();
        for row in train {
            for (role, ids) in [(&row.human, &mut human), (&row.ai, &mut ai)] {
                if role.present == Some(true) {
                    if let Some(identity) = &role.identity {
                        ids.insert(identity.clone());
                    }
                }
            }
        }
        let qualify = |ids: BTreeSet<String>| {
            if ids.len() >= 2 {
                ids.into_iter().collect()
            } else {
                Vec::new()
            }
        };
        Self {
            human: qualify(human),
            ai: qualify(ai),
        }
    }

    fn index(profiles: &[String], role: &Role) -> Option<usize> {
        role.identity
            .as_ref()
            .and_then(|identity| profiles.binary_search(identity).ok())
    }

    pub fn roles(&self, row: &Supervision) -> Value {
        let truth = |profiles: &[String], role: &Role| {
            json!({
                "present":role.present,"identity":role.identity,
                "known":Self::index(profiles, role).is_some()
            })
        };
        json!({"human":truth(&self.human, &row.human),"ai":truth(&self.ai, &row.ai)})
    }

    pub fn targets(&self, row: &Supervision) -> Value {
        if row.purpose == "reference" {
            return json!({"origin":null,"ai_involvement":null,"share":null,"human":null,"ai":null});
        }
        let target = |profiles: &[String], role: &Role| {
            if role.present == Some(true) {
                Self::index(profiles, role)
            } else {
                None
            }
        };
        json!({"origin":row.origin,"ai_involvement":row.origin != 0,"share":row.share,
            "human":target(&self.human, &row.human),"ai":target(&self.ai, &row.ai)})
    }
}

pub(super) fn without_text(metadata: &Value) -> Value {
    let mut compact = metadata.clone();
    if let Some(object) = compact.as_object_mut() {
        object.remove("text");
    }
    compact
}
