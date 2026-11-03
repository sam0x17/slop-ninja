//! Freeze nested Train-family selections and loss weights from numeric inputs.
//! prepare_source_size_curve NUMERIC_BUNDLE EXTENSION_DATASET DESIGN INIT NEW_OUT
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};
const STRATA: [&str; 5] = [
    "cmu-books-historical",
    "hc3-wiki-historical",
    "plos_one",
    "wikinews",
    "project_gutenberg_historical_novel",
];
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn rows(b: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(b)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn write(p: &Path, v: &Value) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(p)?;
    serde_json::to_writer_pretty(&mut f, v)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn rank(g: &str) -> String {
    sha(format!("slop_ninja_a_source_size_curve_v1\0{g}").as_bytes())
}
fn selected(groups: &BTreeMap<String, String>, limit: Option<usize>) -> Result<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    for stratum in STRATA {
        let mut g = groups
            .iter()
            .filter(|(_, v)| v.as_str() == stratum)
            .map(|(g, _)| g.clone())
            .collect::<Vec<_>>();
        g.sort_by_key(|g| (rank(g), g.clone()));
        let count = limit.unwrap_or(g.len());
        ensure!(
            count > 0 && g.len() >= count,
            "stratum shortfall: {stratum}"
        );
        result.extend(g.into_iter().take(count));
    }
    Ok(result)
}
fn weights(
    rows: &[Value],
    n: usize,
    groups: &BTreeMap<String, String>,
    selection: &BTreeSet<String>,
) -> Result<Vec<Value>> {
    let mut cells = BTreeMap::<(String, u64), usize>::new();
    let mut families = BTreeMap::<String, BTreeSet<String>>::new();
    let mut origins = BTreeMap::<String, BTreeSet<u64>>::new();
    let mut labels = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        ensure!(
            r["frozen_split"] == "train" && r["purpose"] == "query",
            "only Train queries"
        );
        let g = s(r, "source_group")?;
        let origin = r["targets"]["origin"].as_u64().context("origin label")?;
        ensure!(origin < 3, "origin range");
        let stratum = if i < n {
            "original"
        } else {
            groups.get(g).context("missing group stratum")?
        };
        let chosen = i < n || selection.contains(g);
        labels.push((stratum.to_owned(), chosen));
        if chosen {
            *cells.entry((g.into(), origin)).or_default() += 1;
            families.entry(stratum.into()).or_default().insert(g.into());
            origins.entry(g.into()).or_default().insert(origin);
        }
    }
    rows.iter().zip(labels).map(|(r,(stratum,chosen))|{let g=s(r,"source_group")?;let weight=if chosen{let origin=r["targets"]["origin"].as_u64().unwrap();let mass=if stratum=="original"{0.5}else{0.1};mass/(families[&stratum].len()*origins[g].len()*cells[&(g.into(),origin)]) as f64}else{0.0};Ok(json!({"id":r["id"],"source_group":g,"stratum":stratum,"selected":chosen,"weight":weight}))}).collect()
}
fn verify_extension_families(
    numeric: &[Value],
    raw_families: &BTreeMap<String, String>,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    for r in numeric {
        let id = s(r, "id")?;
        ensure!(seen.insert(id), "duplicate numeric extension ID");
        ensure!(
            raw_families.get(id).map(String::as_str) == Some(s(r, "source_group")?),
            "numeric extension source family differs from admitted record"
        );
    }
    ensure!(seen.len() == raw_families.len(), "matched extension roster");
    Ok(())
}
fn run(root: &Path, dataset: &Path, design: &Path, init: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let db = fs::read(dataset)?;
    let dm: Value = serde_json::from_slice(&db)?;
    ensure!(
        dm["complete"] == true
            && dm["train_extension_only"] == true
            && dm["test_text_opened"] == false
            && dm["test_ids"] == json!([]),
        "Train-only dataset"
    );
    let raw = fs::read(
        dataset
            .parent()
            .context("dataset parent")?
            .join("records.jsonl"),
    )?;
    ensure!(dm["records_sha256"] == sha(&raw), "dataset changed");
    let mut groups = BTreeMap::new();
    let mut raw_families = BTreeMap::new();
    for r in rows(&raw)? {
        ensure!(
            r["frozen_split"] == "train"
                && r["purpose"] == "query"
                && raw_families
                    .insert(s(&r, "id")?.to_owned(), s(&r, "source_group")?.to_owned())
                    .is_none(),
            "dataset scope/unique ID"
        );
        if r["parent_id"].is_null() {
            ensure!(r["origin"] == "human_only", "expected historical root");
            let stratum = s(&r["metadata"], "source_collection")?;
            ensure!(STRATA.contains(&stratum), "unqualified source stratum");
            ensure!(
                groups
                    .insert(s(&r, "source_group")?.to_owned(), stratum.to_owned())
                    .is_none(),
                "multiple roots for new family"
            );
        }
    }
    let mut manifests = BTreeMap::new();
    let mut parents = BTreeMap::new();
    let mut train = None;
    let mut original_count = None;
    for mode in ["text", "combined"] {
        let dir = root.join(mode);
        let mb = fs::read(dir.join("manifest.json"))?;
        let m: Value = serde_json::from_slice(&mb)?;
        let c = read(&dir.join("contract.json"))?;
        ensure!(
            m["final_test_opened"] == false
                && c["ablation"] == mode
                && c["input_dimension"] == 2588
                && m["training_extension"]["extension_dataset_manifest_sha256"] == sha(&db),
            "numeric dataset binding"
        );
        let split = m["splits"]
            .as_array()
            .context("splits")?
            .iter()
            .find(|r| r["split"] == "train")
            .context("Train split")?;
        let tb = fs::read(dir.join("train.jsonl"))?;
        ensure!(split["sha256"] == sha(&tb), "numeric Train changed");
        let rr = rows(&tb)?;
        let n = m["training_extension"]["original_partitions"]["train"]["rows"]
            .as_u64()
            .context("original count")? as usize;
        ensure!(n > 0 && n < rr.len(), "original prefix bounds");
        verify_extension_families(&rr[n..], &raw_families)?;
        let original = rr[..n]
            .iter()
            .map(|r| s(r, "source_group"))
            .collect::<Result<BTreeSet<_>>>()?;
        ensure!(
            original.iter().all(|g| !groups.contains_key(*g)),
            "original/additional family overlap"
        );
        if let Some(previous) = &train {
            let pp: &Vec<Value> = previous;
            ensure!(
                pp.len() == rr.len()
                    && pp.iter().zip(&rr).all(|(a, b)| a["id"] == b["id"]
                        && a["source_group"] == b["source_group"]
                        && a["targets"] == b["targets"]),
                "unmatched modality supervision"
            );
            ensure!(original_count == Some(n), "different original prefix");
        } else {
            train = Some(rr);
            original_count = Some(n);
        }
        manifests.insert(mode, sha(&mb));
        parents.insert(
            mode,
            m["training_extension"]["parent_manifest_sha256"].clone(),
        );
    }
    let rows = train.context("no Train")?;
    let n = original_count.unwrap();
    let design_bytes = fs::read(design)?;
    let init_sha = sha(&fs::read(init)?);
    fs::create_dir(out)?;
    for (level, limit) in [("12", Some(12)), ("24", Some(24)), ("all", None)] {
        let selection = selected(&groups, limit)?;
        let rr = weights(&rows, n, &groups, &selection)?;
        let p = json!({"schema":"slop_ninja_a_source_size_sampling_v1","level":level,"selected_added_families":selection,"original_rows":n,"total_train_rows":rows.len(),"rows":rr,"source_strata":STRATA,"original_cohort_mass":0.5,"added_stratum_mass":0.1,"within_stratum":"family_origin_balanced","rank_domain":"slop_ninja_a_source_size_curve_v1","apply_to":"train_only","normalization_fitted":false,"profiles_changed":false,"final_test_opened":false,"epochs":250,"seeds":[17,29,41],"allowed_modes":if level=="all"{vec!["text","combined"]}else{vec!["text"]},"numeric_manifest_sha256":manifests,"parent_manifest_sha256":parents,"extension_dataset_manifest_sha256":sha(&db),"initialization_sha256":init_sha,"design_protocol_sha256":sha(&design_bytes),"implementation_sha256":sha(include_bytes!("prepare_source_size_curve.rs"))});
        write(&out.join(format!("{level}.json")), &p)?;
    }
    write(
        &out.join("manifest.json"),
        &json!({"schema":"slop_ninja_source_size_curve_plan_v1","complete":true,"levels":["12","24","all"],"requested_fits":12,"selected_by":"metadata_only_fixed_hash_order","final_test_opened":false,"design_protocol_sha256":sha(&design_bytes),"dataset_manifest_sha256":sha(&db)}),
    )?;
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 5,
        "NUMERIC_BUNDLE EXTENSION_DATASET DESIGN INIT NEW_OUT"
    );
    run(
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
        Path::new(&a[4]),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descendant_cannot_move_between_existing_families() {
        let families = BTreeMap::from([
            ("root-a".into(), "family-a".into()),
            ("rewrite-a".into(), "family-a".into()),
            ("root-b".into(), "family-b".into()),
        ]);
        let mut numeric = vec![
            json!({"id":"root-a","source_group":"family-a"}),
            json!({"id":"rewrite-a","source_group":"family-a"}),
            json!({"id":"root-b","source_group":"family-b"}),
        ];
        verify_extension_families(&numeric, &families).unwrap();
        // Preserve the ID roster, row count and both existing families.
        numeric[1]["source_group"] = json!("family-b");
        assert!(verify_extension_families(&numeric, &families).is_err());
    }
    #[test]
    fn nested_deterministic_selection_and_shortfall() {
        let groups = STRATA
            .into_iter()
            .flat_map(|s| (0..30).map(move |i| (format!("{s}:{i}"), s.to_owned())))
            .collect();
        let small = selected(&groups, Some(12)).unwrap();
        let medium = selected(&groups, Some(24)).unwrap();
        let all = selected(&groups, None).unwrap();
        assert_eq!((small.len(), medium.len(), all.len()), (60, 120, 150));
        assert!(small.is_subset(&medium) && medium.is_subset(&all));
        assert!(selected(&groups, Some(31)).is_err());
    }
    #[test]
    fn mass_independent_of_rewrite_counts() {
        let mut rr = Vec::new();
        let mut groups = BTreeMap::new();
        for (j, stratum) in std::iter::once("original").chain(STRATA).enumerate() {
            let group = format!("family{j}");
            if j > 0 {
                groups.insert(group.clone(), stratum.into());
            }
            for i in 0..=j {
                rr.push(json!({"id":format!("{j}:{i}"),"source_group":group,"frozen_split":"train","purpose":"query","targets":{"origin":i%3}}));
            }
        }
        let selected = groups.keys().cloned().collect();
        let w = weights(&rr, 1, &groups, &selected).unwrap();
        let sum: f64 = w.iter().map(|r| r["weight"].as_f64().unwrap()).sum();
        assert!((sum - 1.).abs() < 1e-12);
        for stratum in STRATA {
            let mass: f64 = w
                .iter()
                .filter(|r| r["stratum"] == stratum)
                .map(|r| r["weight"].as_f64().unwrap())
                .sum();
            assert!((mass - 0.1).abs() < 1e-12);
        }
        assert_eq!(w[0]["weight"], 0.5);
    }
}
