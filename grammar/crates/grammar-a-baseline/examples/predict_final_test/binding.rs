//! Shared frozen admission/output guards for prediction and all-head scoring.
use super::{final_test_binding as frozen, test_binding as b};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::Path};
pub struct Admitted {
    pub selection: Value,
    pub candidate: Value,
    pub run: Value,
    pub fit: Value,
    pub weights: Value,
    pub weight_bytes: Vec<u8>,
    pub contract: Value,
    pub data: b::TestData,
}
pub fn all_heads(w: &Value, c: &Value) -> Result<()> {
    ensure!(
        w["control"] == "joint" && w["architecture"]["dimension"] == c["input_dimension"],
        "complete joint A with frozen dimension required"
    );
    for k in ["origin", "share", "mixed_fractional_share", "human", "ai"] {
        ensure!(
            w["supervision"][k].as_u64().is_some_and(|n| n > 0),
            "missing positive {k} supervision"
        );
    }
    for role in ["human", "ai"] {
        ensure!(
            w["profiles"][role] == c[format!("{role}_profiles")],
            "frozen profile roster differs"
        );
    }
    Ok(())
}
pub fn admit(numeric: &Path, fit: &Path, selection_path: &Path) -> Result<Admitted> {
    let selection = frozen::validate_selection_receipt(selection_path)?;
    let sh = b::sha(selection_path)?;
    let data = b::load_data(numeric, &selection, &sh)?;
    let (candidate, run, f) = b::fit_binding(&selection, &data, fit)?;
    let weight_bytes = fs::read(fit.join("weights.json"))?;
    ensure!(
        b::digest(&weight_bytes) == b::text(&run, "weights_sha256")?,
        "weights changed during admission"
    );
    let weights: Value = serde_json::from_slice(&weight_bytes)?;
    let contract = b::read(&numeric.join("contract.json"))?;
    ensure!(
        b::digest(&serde_json::to_vec(&contract)?) == data.contract_sha,
        "contract changed during admission"
    );
    all_heads(&weights, &contract)?;
    // load_data checks origin/share. Bind role masks/targets to the same retained
    // metadata too; no prose is needed and no unknown identity is invented.
    let audit = b::read(&b::member(
        numeric,
        b::text(&data.manifest["test_transform"], "audit_file")?,
    )?)?;
    let population = b::indexed(b::array(&audit, "rows")?)?;
    for (id, row) in &data.numeric {
        roles(row, &population[id]["metadata"], &contract)?;
    }
    Ok(Admitted {
        selection,
        candidate,
        run,
        fit: f,
        weights,
        weight_bytes,
        contract,
        data,
    })
}
pub fn roles(row: &Value, m: &Value, c: &Value) -> Result<()> {
    for role in ["human", "ai"] {
        let profiles = b::array(c, &format!("{role}_profiles"))?;
        let present = m
            .get(format!("{role}_role_present"))
            .context("explicit role mask")?;
        ensure!(
            present.is_null() || present.is_boolean(),
            "invalid role mask"
        );
        let identity = m
            .get(format!("{role}_identity"))
            .context("explicit nullable identity")?;
        ensure!(
            identity.is_null() || identity.as_str().is_some_and(|s| !s.is_empty()),
            "invalid identity"
        );
        let index = profiles.iter().position(|p| p == identity);
        ensure!(
            row["roles"][role]
                == json!({"present":present,"identity":identity,"known":index.is_some()})
                && row["targets"][role]
                    == if *present == true {
                        json!(index)
                    } else {
                        Value::Null
                    },
            "numeric role/mask differs from retained metadata"
        );
    }
    Ok(())
}
pub fn new_output(path: &Path, selection: &Value, numeric: &Path) -> Result<()> {
    ensure!(!path.try_exists()?, "new output required");
    let dest = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?
        .join(path.file_name().context("output basename")?);
    ensure!(
        !dest.starts_with(numeric.canonicalize()?),
        "output inside numeric Test input bundle"
    );
    for c in b::array(selection, "candidates")? {
        ensure!(
            !dest.starts_with(Path::new(b::text(c, "numeric_path")?).canonicalize()?),
            "output inside frozen numeric parent"
        );
        for r in b::array(c, "runs")? {
            ensure!(
                !dest.starts_with(Path::new(b::text(r, "model_path")?).canonicalize()?),
                "output inside frozen fit"
            );
        }
    }
    Ok(())
}
pub fn unchanged(a: &Admitted, numeric: &Path, fit: &Path, selection: &Path) -> Result<()> {
    ensure!(
        b::sha(selection)? == a.data.selection_sha
            && b::sha(&numeric.join("manifest.json"))? == a.data.manifest_sha
            && b::sha(&numeric.join("test.jsonl"))? == a.data.query_sha
            && b::sha(&numeric.join("contract.json"))?
                == b::text(&a.data.manifest, "contract_file_sha256")?
            && b::sha(&b::member(
                numeric,
                b::text(&a.data.manifest["test_transform"], "audit_file")?
            )?)? == a.data.population_sha
            && b::sha(&numeric.join("test_dataset_manifest.json"))? == a.data.dataset_sha
            && b::sha(&fit.join("weights.json"))? == b::text(&a.run, "weights_sha256")?
            && b::sha(&fit.join("fit.json"))? == b::text(&a.run, "fit_sha256")?,
        "bound input changed during execution"
    );
    Ok(())
}
