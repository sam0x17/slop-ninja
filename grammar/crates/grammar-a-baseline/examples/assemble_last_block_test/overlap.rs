//! Generated-Test overlap audit; it never changes fitting/selection populations.
use super::*;
fn normalized(s: &str) -> String {
    sha(s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .as_bytes())
}
fn shingles(s: &str) -> BTreeSet<String> {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .windows(50)
        .map(|w| sha(w.join(" ").as_bytes()))
        .collect()
}
fn visit(
    v: &Value,
    prose: &mut impl FnMut(&str) -> Result<()>,
    prompt: &mut impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    match v {
        Value::Array(a) => {
            for r in a {
                visit(r, prose, prompt)?;
            }
        }
        Value::Object(m) => {
            if let Some(t) = m.get("text").and_then(Value::as_str) {
                prose(t)?;
            }
            if let Some(messages) = m.get("messages").and_then(Value::as_array) {
                for r in messages {
                    if let Some(t) = r["content"].as_str() {
                        prompt(t)?;
                    }
                }
            }
            for (k, r) in m {
                if k != "text" {
                    visit(r, prose, prompt)?;
                }
            }
        }
        _ => {}
    };
    Ok(())
}
pub(super) fn check(
    rows: &[Value],
    refs: &[Value],
    prompts: &BTreeMap<(String, u64), Value>,
    admission: &Value,
    inputs: &mut Vec<Value>,
) -> Result<Value> {
    let mut exact = BTreeMap::<String, BTreeSet<String>>::new();
    let mut norm = exact.clone();
    let mut sh = exact.clone();
    let mut route = BTreeMap::new();
    for r in rows.iter().chain(refs) {
        let t = str_at(r, "text")?;
        let g = str_at(r, "source_group")?.to_owned();
        route.insert(str_at(r, "id")?.to_owned(), g.clone());
        exact
            .entry(sha(t.as_bytes()))
            .or_default()
            .insert(g.clone());
        norm.entry(normalized(t)).or_default().insert(g.clone());
        for h in shingles(t) {
            sh.entry(h).or_default().insert(g.clone());
        }
    }
    let mut scratch = BTreeMap::<String, BTreeSet<String>>::new();
    for ((id, p), v) in prompts {
        if *p == 0 {
            scratch
                .entry(sha(str_at(&v["messages"][0], "content")?.as_bytes()))
                .or_default()
                .insert(route.get(id).context("scratch parent absent")?.clone());
        }
    }
    let mut prose_conflicts = BTreeSet::new();
    let mut prompt_conflicts = BTreeSet::new();
    let mut count = 0usize;
    let mut prompt_count = 0usize;
    let mut check_text = |t: &str| -> Result<()> {
        count += 1;
        for groups in [exact.get(&sha(t.as_bytes())), norm.get(&normalized(t))]
            .into_iter()
            .flatten()
        {
            prose_conflicts.extend(groups.iter().cloned());
        }
        for h in shingles(t) {
            if let Some(groups) = sh.get(&h) {
                prose_conflicts.extend(groups.iter().cloned());
            }
        }
        Ok(())
    };
    let mut check_prompt = |t: &str| -> Result<()> {
        prompt_count += 1;
        if let Some(groups) = scratch.get(&sha(t.as_bytes())) {
            prompt_conflicts.extend(groups.iter().cloned());
        }
        Ok(())
    };
    let protected = array(admission, "protected_inputs")?;
    ensure!(!protected.is_empty(), "empty overlap provenance");
    let mut visited = BTreeSet::new();
    for binding in protected {
        let p = Path::new(str_at(binding, "path")?);
        ensure!(
            seal::sha(p)? == binding["sha256"],
            "protected input changed"
        );
        inputs.push(binding.clone());
        if binding["metadata_only"] == true {
            continue;
        }
        if !visited.insert(format!("{}:{}", p.display(), binding["pointer"])) {
            continue;
        }
        let format = if binding["scope"] == "consumed_Test_or_current_nonTest" {
            "jsonl"
        } else {
            str_at(binding, "format")?
        };
        match format {
            "jsonl" => {
                for r in records(&bytes(p)?)? {
                    visit(&r, &mut check_text, &mut check_prompt)?;
                }
            }
            "json" => {
                let v: Value = serde_json::from_slice(&bytes(p)?)?;
                let v = if binding["pointer"].is_null() {
                    &v
                } else {
                    v.pointer(str_at(binding, "pointer")?)
                        .context("protected pointer")?
                };
                visit(v, &mut check_text, &mut check_prompt)?;
            }
            _ => anyhow::bail!("unsupported protected format"),
        }
    }
    let conflicting_families = prose_conflicts
        .union(&prompt_conflicts)
        .cloned()
        .collect::<BTreeSet<_>>();
    Ok(
        json!({"complete":true,"protected_text_occurrences_scanned":count,
        "protected_prompt_occurrences_scanned":prompt_count,"protected_file_views":visited.len(),
        "requested_rows_scanned":rows.len(),"references_scanned":refs.len(),
        "method":"exact_and_whitespace_normalized_text_50_word_sequences_and_scratch_prompts_v1",
        "conflicting_families":conflicting_families,"prose_conflicting_families":prose_conflicts,
        "scratch_prompt_conflicting_families":prompt_conflicts,
        "collision_policy":"sealed_plan_entire_conflicting_source_family_exclusion_no_replacement"}),
    )
}
/// Match the established Test policy: conflicting work/reference text stops;
/// unchanged or duplicate generated text within a work is retained as an
/// explicit exclusion, with its original record and generation evidence.
pub(super) fn within_test(
    rows: Vec<Value>,
    refs: &[Value],
) -> Result<(Vec<Value>, Vec<Value>, Vec<Value>)> {
    let mut groups = BTreeMap::<String, BTreeSet<String>>::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut roots = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut refhash = BTreeSet::new();
    for r in rows.iter().chain(refs) {
        ensure!(
            ids.insert(str_at(r, "id")?.to_owned()),
            "duplicate query/reference id"
        );
        let h = normalized(str_at(r, "text")?);
        groups
            .entry(h.clone())
            .or_default()
            .insert(str_at(r, "source_group")?.to_owned());
        if r["purpose"] == "reference" {
            ensure!(refhash.insert(h), "duplicate Test reference");
        } else {
            *counts.entry(h.clone()).or_default() += 1;
            if r["origin"] == "human_only" {
                ensure!(roots.insert(h), "duplicate human excerpts");
            }
        }
    }
    ensure!(
        groups.values().all(|g| g.len() == 1),
        "text collision across Test works"
    );
    ensure!(
        counts.keys().all(|h| !refhash.contains(h)),
        "query/reference overlap"
    );
    let (mut kept, mut exclusions, mut raw) = (Vec::new(), Vec::new(), Vec::new());
    for r in rows {
        let h = normalized(str_at(&r, "text")?);
        if r["origin"] != "human_only" && (counts[&h] > 1 || roots.contains(&h)) {
            exclusions.push(json!({"id":r["id"],"source_group":r["source_group"],"text_sha256":r["text_sha256"],
                "normalized_text_sha256":h,"reason":"within_Test_family_generated_duplicate_or_no_change",
                "generation_evidence_retained":true}));
            raw.push(r);
        } else {
            kept.push(r);
        }
    }
    Ok((kept, exclusions, raw))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn row(id: &str, group: &str, t: &str, origin: &str) -> Value {
        json!({"id":id,"source_group":group,"text":t,"text_sha256":sha(t.as_bytes()),"origin":origin,"purpose":"query"})
    }
    #[test]
    fn duplicate_splices_are_accounted_and_cross_work_conflicts_stop() {
        let h = row("human", "a", "Synthetic prose.", "human_only");
        let g = row("rewrite", "a", "Synthetic   prose.", "mixed");
        let (kept, excluded, raw) = within_test(vec![h.clone(), g], &[]).unwrap();
        assert_eq!((kept.len(), excluded.len(), raw.len()), (1, 1, 1));
        assert!(
            within_test(
                vec![
                    h.clone(),
                    row("other", "b", "Synthetic prose.", "model_only")
                ],
                &[]
            )
            .is_err()
        );
        let mut reference = row("reference", "a", "Synthetic prose.", "human_only");
        reference["purpose"] = json!("reference");
        assert!(within_test(vec![h], &[reference]).is_err());
    }
    #[test]
    fn nested_prose_and_request_scans_are_separate() {
        let (mut texts, mut prompts) = (Vec::new(), Vec::new());
        visit(&json!({"text":"Synthetic prose.","generation":{"request":{"messages":[{"content":"Write a scene."}]}},"title":"metadata"}),
            &mut|t|{texts.push(t.to_owned());Ok(())},&mut|t|{prompts.push(t.to_owned());Ok(())}).unwrap();
        assert_eq!(texts, vec!["Synthetic prose."]);
        assert_eq!(prompts, vec!["Write a scene."]);
    }
}
