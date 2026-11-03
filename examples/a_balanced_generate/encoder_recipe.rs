//! Genre-matched, source-independent prompts for the encoder comparison.
use super::*;
pub(super) fn prompt(source: &Value) -> Result<String> {
    let id = source["id"].as_str().context("routing id")?;
    let d = Sha256::digest(format!("encoder-stage3-v1:{id}").as_bytes());
    let genre = match source["metadata"]["acquisition_stratum"].as_str() {
        Some("fiction") => "an original fictional scene",
        Some("science_nature") => {
            "a natural-history observation with concrete details and cautious interpretation"
        }
        Some("travel") => "a first-person travel sketch describing an imagined journey",
        Some("history_biography") => {
            "a biographical account of an invented historical person, with events and context"
        }
        Some("essays_ideas") => "a reflective essay developing an idea with specific observations",
        _ => anyhow::bail!("unqualified acquisition stratum"),
    };
    let topic = [
        "seasonal change",
        "local customs",
        "careful observation",
        "unexpected cooperation",
        "a difficult journey",
        "an old craft",
        "an unfamiliar landscape",
        "changes in daily life",
    ][d[0] as usize % 8];
    let place = [
        "a coastal settlement",
        "an inland village",
        "a large city",
        "a forest valley",
        "a mountain district",
        "a farming region",
        "an island community",
        "a riverside town",
    ][d[1] as usize % 8];
    let time = [
        "early spring",
        "midsummer",
        "late autumn",
        "midwinter",
        "the morning",
        "the afternoon",
        "dusk",
        "a rainy day",
    ][d[2] as usize % 8];
    let detail = [
        "a stone bridge",
        "a small orchard",
        "a winding path",
        "an old workshop",
        "a sheltered garden",
        "a local market",
        "a public library",
        "a modest inn",
    ][d[3] as usize % 8];
    let development = [
        "an initial misunderstanding",
        "an unexpected discovery",
        "a gradual change",
        "a practical difficulty",
        "a revealing comparison",
        "a disputed explanation",
        "an interruption",
        "a remembered lesson",
    ][d[4] as usize % 8];
    let emphasis = [
        "uncertainty",
        "patient effort",
        "conflicting perspectives",
        "small observations",
        "local knowledge",
        "changing expectations",
        "unintended consequences",
        "a tentative conclusion",
    ][d[5] as usize % 8];
    Ok(format!(
        "Write {genre} in English, about {topic} in {place} during {time}. Include {detail} and {development}, with attention to {emphasis}. Aim for 220 words of flowing prose. Return only the prose, without a title, preface, analysis, or explanation of the task. Do not use a byline."
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_controls_genre_but_private_routing_and_prose_do_not_enter_prompt() {
        let v = json!({"id":"synthetic-route-9","text":"Synthetic source words.","human_identity":"synthetic_writer","metadata":{"acquisition_stratum":"travel"}});
        let p = prompt(&v).unwrap();
        assert!(p.contains("travel sketch"));
        for k in ["id", "text", "human_identity"] {
            assert!(!p.contains(v[k].as_str().unwrap()));
        }
        let mut changed = v.clone();
        changed["text"] = json!("Different synthetic text.");
        assert_eq!(p, prompt(&changed).unwrap());
        changed["metadata"]["acquisition_stratum"] = json!("unreviewed");
        assert!(prompt(&changed).is_err());
    }
}
