//! Fixed public categories; never emit identities, source aliases or prose.
use super::*;
fn role(m: &Value, contract: &Value, name: &str) -> Result<String> {
    match m[format!("{name}_role_present")].as_bool() {
        Some(false) => Ok("absent".into()),
        None => Ok("presence_unknown".into()),
        Some(true) => {
            let identity = &m[format!("{name}_identity")];
            if identity.is_null() {
                return Ok("identity_unknown".into());
            }
            let profiles = b::array(contract, &format!("{name}_profiles"))?;
            Ok(if profiles.contains(identity) {
                "known"
            } else {
                "withheld"
            }
            .into())
        }
    }
}
pub(super) fn categories(
    row: &Value,
    m: &Value,
    contract: &Value,
) -> Result<Vec<(&'static str, String)>> {
    let origin = b::count(&row["targets"], "origin")?;
    let history = match (origin, m["generation"]["mode"].as_str()) {
        (0, None) => "human_source_proxy",
        (1, Some("from_scratch")) => "from_scratch",
        (2, Some("controlled_rewrite"))
            if m["share_evidence"]["full_rewrite_is_mixed_history"] == true =>
        {
            "full_rewrite_mixed_history"
        }
        (2, Some("controlled_rewrite")) => "partial_rewrite_mixed_history",
        _ => "unavailable",
    };
    let share = match row["targets"]["share"].as_f64() {
        None => "unknown",
        Some(x) => {
            ensure!((0.0..=1.).contains(&x), "share stratum range");
            if x == 0. {
                "0"
            } else if x == 1. {
                "100"
            } else if x <= 0.25 {
                "(0,25]"
            } else if x <= 0.5 {
                "(25,50]"
            } else if x <= 0.75 {
                "(50,75]"
            } else {
                "(75,100)"
            }
        }
    };
    let position = if origin == 0 || origin == 1 {
        "not_applicable"
    } else if history == "full_rewrite_mixed_history" {
        "full_document"
    } else {
        match m["generation"]["position"].as_str() {
            Some("start") => "start",
            Some("middle") => "middle",
            Some("end") => "end",
            _ => "unavailable",
        }
    };
    let mut generator = role(m, contract, "ai")?;
    if generator == "known" {
        let index = b::array(contract, "ai_profiles")?
            .iter()
            .position(|p| p == &m["ai_identity"])
            .context("known generator")?;
        generator = format!("known_generator_{}", index + 1);
    }
    Ok(vec![
        ("generator", generator),
        ("production_history", history.into()),
        ("realized_share_percent", share.into()),
        ("replacement_position", position.into()),
        ("writer_status", role(m, contract, "human")?),
    ])
}
pub(super) fn report(
    rows: &[Value],
    metadata: &Pred,
    contract: &Value,
    left: &[&Pred],
    v6: &[&Pred],
    pg: &[&Pred],
) -> Result<Value> {
    let mut groups = BTreeMap::<(&str, String), Vec<Value>>::new();
    for row in rows {
        let id = b::text(row, "id")?;
        let m = &metadata.get(id).context("strata metadata missing")?["metadata"];
        ensure!(
            m["id"] == row["id"] && m["source_group"] == row["source_group"],
            "strata family differs"
        );
        for cat in categories(row, m, contract)? {
            groups.entry(cat).or_default().push(row.clone());
        }
    }
    let mut out = Vec::new();
    for ((axis, category), subset) in groups {
        out.push(json!({"axis":axis,"category":category,"documents":subset.len(),"source_families":metrics::families(&subset),
            "selected_minus_frozen_v6":metrics::comparison("selected_candidate","frozen_v6",&subset,left,v6)?,
            "selected_minus_CAL_mapped_pangram":metrics::comparison("selected_candidate","CAL_mapped_pangram_proxy",&subset,left,pg)?}));
    }
    Ok(
        json!({"scope":"selected model and comparators; common support across all three fixed seeds",
        "metric":"involvement_brier_difference","generator_numbering":"frozen selected contract profile order; identities withheld",
        "axes":["generator","production_history","realized_share_percent","replacement_position","writer_status"],
        "all_head_stratified_metrics_available":false,"groups":out}),
    )
}
