// Numeric feature semantics copied from the already frozen preparer pipeline.
// Keep that historical source unchanged; conformance verifies exact numeric x.
const FEATURE_POLICY: &str = "slop_ninja_supervised_a_forest_numeric_v1";
const FIXED: [&str; 3] = [
    "document_availability/projection_available",
    "document_availability/search_exhaustive",
    "document_availability/has_token_opportunities",
];
const POOLS: [&str; 6] = [
    "native_complete",
    "fallback_complete",
    "partial",
    "search_limited",
    "lexical_recovery",
    "unavailable",
];
const TIERS: [&str; 8] = [
    "NativeComplete",
    "LexicalFallbackComplete",
    "Partial",
    "ResourceLimit",
    "BudgetSkipped",
    "TokenFragment",
    "ExecutionFailure",
    "Layout",
];

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn str_at<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    let s = value[key]
        .as_str()
        .with_context(|| format!("missing {key}"))?;
    ensure!(!s.is_empty(), "empty {key}");
    Ok(s)
}

fn boolean(value: &Value, key: &str) -> Result<bool> {
    value[key]
        .as_bool()
        .with_context(|| format!("missing boolean {key}"))
}

fn counter(value: &Value, key: &str) -> Result<u64> {
    value[key]
        .as_u64()
        .with_context(|| format!("missing unsigned counter {key}"))
}

fn number(value: &Value) -> Result<f64> {
    let n = value.as_f64().context("missing numeric measurement")?;
    ensure!(n.is_finite() && n >= 0.0, "invalid nonnegative measurement");
    Ok(n)
}

fn digest_at<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    let s = str_at(value, key)?;
    ensure!(
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid SHA256 in {key}"
    );
    Ok(s)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

// Only these exported names can become learned coordinates. Pool prefixes are
// preserved verbatim; identities, source/grammar provenance and labels cannot
// become coordinates. Structural signatures are supplied by the bound exporter.
fn validate_key(key: &str) -> Result<()> {
    if matches!(
        key,
        "availability/unresolved_token"
            | "availability/no_real_reading_token"
            | "availability/forced_window"
    ) || key
        .strip_prefix("availability/window/")
        .is_some_and(|t| TIERS.contains(&t))
    {
        return Ok(());
    }
    let (pool, rest) = key.split_once('/').context("unpooled structural key")?;
    let (kind, signature) = rest
        .split_once('/')
        .context("missing structural signature")?;
    ensure!(
        POOLS.contains(&pool)
            && matches!(kind, "symbol" | "production" | "leaf")
            && !signature.is_empty(),
        "unsupported structural coordinate"
    );
    // Match the exporter's structural projection shape. Arbitrary named fields
    // such as lexical IDs, text, author or review evidence cannot enter a key.
    let signature: Value = serde_json::from_str(signature).context("non-JSON grammar signature")?;
    match kind {
        "symbol" => validate_symbol(&signature)?,
        "production" => {
            let tuple = signature.as_array().context("production tuple")?;
            ensure!(tuple.len() == 3, "production signature fields");
            validate_symbol(&tuple[0])?;
            let children = tuple[1].as_array().context("production children")?;
            for child in children {
                validate_symbol(child)?;
            }
            ensure!(
                tuple[2].is_null() || tuple[2].as_u64().is_some_and(|i| i < children.len() as u64),
                "production head child"
            );
        }
        "leaf" => {
            let tuple = signature.as_array().context("leaf grammar tuple")?;
            ensure!(tuple.len() == 4, "leaf grammar signature fields");
            ensure!(
                matches!(tuple[0].as_str(), Some("inventory" | "fallback")),
                "leaf marker"
            );
            ensure!(
                tuple[1]
                    .as_str()
                    .is_some_and(|s| !s.is_empty()
                        && s.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_')),
                "leaf POS must be a grammatical tag"
            );
            validate_symbol(&tuple[2])?;
            ensure!(
                tuple[3].is_null()
                    || tuple[3].as_str().is_some_and(
                        |s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphabetic())
                    ),
                "leaf morphology kind"
            );
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn validate_symbol(value: &Value) -> Result<()> {
    let symbol = value.as_object().context("grammar symbol object")?;
    ensure!(
        symbol.len() == 2 && symbol.contains_key("category") && symbol.contains_key("features"),
        "non-structural symbol fields"
    );
    let category = str_at(value, "category")?;
    ensure!(
        category
            .bytes()
            .all(|b| b.is_ascii_alphabetic() || b == b'_'),
        "invalid grammar category"
    );
    let keys = [
        "agreement",
        "count",
        "proper",
        "form",
        "tense",
        "degree",
        "voice",
        "case",
        "person",
        "number",
        "aspect",
        "mood",
        "polarity",
    ];
    let values = [
        "first_singular",
        "second",
        "third_singular",
        "plural",
        "singular",
        "mass",
        "yes",
        "no",
        "any",
        "finite",
        "bare",
        "present_participle",
        "past_participle",
        "perfect_participle",
        "passive_participle",
        "gerund",
        "present",
        "past",
        "future",
        "positive",
        "comparative",
        "superlative",
        "active",
        "passive",
        "nominative",
        "accusative",
        "genitive",
        "first",
        "third",
        "perfect",
        "progressive",
        "indicative",
        "subjunctive",
        "imperative",
        "negative",
    ];
    for (key, value) in value["features"]
        .as_object()
        .context("symbol feature map")?
    {
        ensure!(
            keys.contains(&key.as_str()) && value.as_str().is_some_and(|v| values.contains(&v)),
            "symbol feature outside frozen grammatical vocabulary"
        );
    }
    Ok(())
}

fn document_scope(key: &str) -> bool {
    FIXED.contains(&key)
        || key.starts_with("availability/window/")
        || key == "availability/forced_window"
}

struct Measurements {
    tokens: f64,
    counts: BTreeMap<String, f64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ablation {
    All,
    Structure,
    LeafGrammar,
    Availability,
}

impl Ablation {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "all" => Ok(Self::All),
            "structure" => Ok(Self::Structure),
            "leaf_grammar" => Ok(Self::LeafGrammar),
            "availability" => Ok(Self::Availability),
            _ => anyhow::bail!("ablation must be all, structure, leaf_grammar or availability"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Structure => "structure",
            Self::LeafGrammar => "leaf_grammar",
            Self::Availability => "availability",
        }
    }

    fn includes(self, key: &str) -> bool {
        let availability = FIXED.contains(&key) || key.starts_with("availability/");
        let leaf_grammar = key.split('/').nth(1) == Some("leaf");
        match self {
            Self::All => true,
            Self::Structure => !availability && !leaf_grammar,
            Self::LeafGrammar => leaf_grammar,
            Self::Availability => availability,
        }
    }
}

fn measurement(row: &Measurements, key: &str) -> Result<(f64, f64)> {
    let count = row.counts.get(key).copied().unwrap_or(0.0);
    let denominator = if document_scope(key) { 1.0 } else { row.tokens };
    if denominator == 0.0 {
        ensure!(count == 0.0, "count without token opportunities");
        return Ok((0.0, 0.0));
    }
    let value = (count / denominator).ln_1p();
    ensure!(value.is_finite(), "nonfinite transformed measurement");
    Ok((value, 1.0))
}
