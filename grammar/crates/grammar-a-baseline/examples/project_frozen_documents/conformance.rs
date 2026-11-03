//! Explicit opt-in non-Test comparison, run by the task-owned parent script.
use super::*;

#[test]
#[ignore = "requires explicit non-Test numeric directory and projected file; never autodiscovers data"]
fn projected_non_test_inputs_match_prepared_vectors_exactly() -> Result<()> {
    let root =
        std::env::var_os("SLOP_NINJA_PROJECT_FROZEN_NUMERIC").context("set numeric directory")?;
    let projected_path =
        std::env::var_os("SLOP_NINJA_PROJECTED_INPUTS").context("set projected file")?;
    let expected_rows: usize = std::env::var("SLOP_NINJA_PROJECT_EXPECTED_ROWS")
        .context("set expected rows")?
        .parse()?;
    let expected_references: usize = std::env::var("SLOP_NINJA_PROJECT_EXPECTED_REFERENCES")
        .context("set expected reference count")?
        .parse()?;
    let root = Path::new(&root);
    let manifest: Value = serde_json::from_slice(&fs::read(root.join("manifest.json"))?)?;
    ensure!(
        manifest["final_test_opened"] == false,
        "conformance requires the original non-Test numeric dataset"
    );
    let contract_bytes = fs::read(root.join("contract.json"))?;
    ensure!(
        sha(&contract_bytes) == digest_at(&manifest, "contract_file_sha256")?,
        "prepared contract file changed"
    );
    let frozen = Frozen::parse(&contract_bytes)?;
    ensure!(
        manifest["contract_sha256"] == frozen.hash,
        "prepared semantic contract hash mismatch"
    );
    let bindings = manifest["splits"]
        .as_array()
        .context("prepared split bindings")?;
    ensure!(
        bindings.len() == 4
            && bindings.iter().all(|b| matches!(
                b["split"].as_str(),
                Some("train" | "development" | "calibration" | "reference")
            )),
        "only Train/Development/Calibration/reference bindings are allowed"
    );
    let mut expected = BTreeMap::<String, Value>::new();
    let mut references = 0usize;
    for part in ["train", "development", "calibration", "reference"] {
        let matches = bindings
            .iter()
            .filter(|b| b["split"] == part)
            .collect::<Vec<_>>();
        ensure!(matches.len() == 1, "exactly one {part} binding required");
        let binding = matches[0];
        let bytes = fs::read(root.join(format!("{part}.jsonl")))?;
        ensure!(
            sha(&bytes) == digest_at(binding, "sha256")?,
            "changed prepared {part}"
        );
        let mut count = 0u64;
        for line in std::str::from_utf8(&bytes)?.lines() {
            let row: Value = serde_json::from_str(line)?;
            let split = if part == "reference" { "train" } else { part };
            let purpose = if part == "reference" {
                "reference"
            } else {
                "query"
            };
            ensure!(
                row["frozen_split"] == split && row["purpose"] == purpose,
                "non-Test conformance split/purpose mismatch"
            );
            ensure!(
                row["input"]["contract_sha256"] == frozen.hash,
                "prepared input contract mismatch"
            );
            ensure!(
                expected
                    .insert(str_at(&row, "id")?.into(), row["input"].clone())
                    .is_none(),
                "duplicate prepared ID"
            );
            count += 1;
            references += usize::from(part == "reference");
        }
        ensure!(
            counter(binding, "rows")? == count,
            "prepared split count mismatch"
        );
    }
    ensure!(
        expected.len() == expected_rows && references == expected_references,
        "unexpected conformance population/reference count"
    );
    let projected_bytes = fs::read(Path::new(&projected_path))?;
    let mut seen = BTreeSet::new();
    let mut coordinates = 0usize;
    for line in std::str::from_utf8(&projected_bytes)?.lines() {
        let row: Value = serde_json::from_str(line)?;
        let id = str_at(&row, "id")?;
        ensure!(seen.insert(id.to_owned()), "duplicate projected ID");
        let target = expected
            .get(id)
            .context("projected ID outside non-Test prepared population")?;
        ensure!(
            row.as_object().context("projected object")?.len() == 2 && row.get("input").is_some(),
            "extra projected output fields"
        );
        let input = &row["input"];
        ensure!(
            input.as_object().context("projected input")?.len() == 2
                && input["contract_sha256"] == frozen.hash,
            "projected input contract/shape mismatch"
        );
        let values = input["x"].as_array().context("projected x")?;
        let expected_values = target["x"].as_array().context("prepared x")?;
        ensure!(
            values.len() == frozen.keys.len() * 2 && values.len() == expected_values.len(),
            "projected dimension mismatch"
        );
        for (index, (a, b)) in values.iter().zip(expected_values).enumerate() {
            let a = a.as_f64().context("projected numeric coordinate")?;
            let b = b.as_f64().context("prepared numeric coordinate")?;
            ensure!(
                a.is_finite() && b.is_finite() && a.to_bits() == b.to_bits(),
                "numeric mismatch for {id} coordinate {index}"
            );
            coordinates += 1;
        }
    }
    ensure!(
        seen.len() == expected.len(),
        "projected population incomplete"
    );
    println!(
        "{}",
        json!({"schema":"slop_ninja_frozen_projection_conformance_v1","documents":seen.len(),
        "reference_rows":references,"coordinates_compared":coordinates,"comparison":"exact_f64_bits",
        "contract_sha256":frozen.hash,"projected_sha256":sha(&projected_bytes),"final_test_opened":false})
    );
    Ok(())
}
