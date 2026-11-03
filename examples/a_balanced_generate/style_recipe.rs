//! Versioned instruction diversity. The same source/span/style cells go to every model.
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::path::Path;

pub const STYLES: &[(&str, &str)] = &[
    ("plain", ""),
    (
        "british",
        "Pretend to be British: use idiomatic British English spelling and phrasing. Keep the original setting and speaker facts; do not invent nationality or relocate events.",
    ),
    (
        "conversational",
        "Use an informal, conversational style with natural contractions and varied sentence lengths.",
    ),
    (
        "academic",
        "Use a formal academic register with precise wording. Do not invent citations, evidence, or technical claims.",
    ),
    (
        "journalistic",
        "Use a clear, restrained newspaper style. Preserve the original claims and perspective; do not invent reporting, quotations, or sources.",
    ),
    (
        "terse",
        "Use spare, direct prose with short sentences and concrete verbs, while retaining all the information.",
    ),
    (
        "ornate",
        "Use an elaborate literary style with flowing sentences and rich but faithful descriptions. Do not add events or factual details.",
    ),
    (
        "hemingway",
        "Write in the style of Ernest Hemingway: concrete language, economical sentences, and restrained emotion. Preserve the passage's facts and perspective.",
    ),
    (
        "sound_human",
        "Make this sound as though a human wrote it, not an AI. Avoid common AI writing tells and formulaic phrasing. Vary rhythm naturally without inserting deliberate errors or inventing personal experiences.",
    ),
    (
        "fix_slop",
        "Apply the pinned fix-slop editing skill below, including its phrase and construction checks. Its original scope is technical prose; use applicable guidance while preserving this passage's genre and meaning. Return only the edited passage, not an audit or explanation.",
    ),
];

const FILES: &[(&str, &str)] = &[
    (
        "SKILL.md",
        "d0bc56baf7144958ff5cafd271bc790680cd7531ba30bbeee36cb49789a8e538",
    ),
    (
        "references/phrases.md",
        "321bc05f5b23dd7593ffafe0f433266e4600d4fe472f20267d8cf88e8cc921e1",
    ),
    (
        "references/constructions.md",
        "8dbefaeb4359c045dc721bcd7679e11e4bfb75ff17c838a73b9fa0be6cf18cb2",
    ),
    (
        "references/greps.txt",
        "910fcd2d58168a777edb8763d434c9a36a6eeb464eb27676873a4fdf23fab940",
    ),
];

pub fn load_skill(root: &Path) -> Result<String> {
    let mut bundle = String::new();
    for (name, hash) in FILES {
        let bytes = std::fs::read(root.join(name))?;
        ensure!(
            super::sha(&bytes) == *hash,
            "pinned fix-slop file differs: {name}"
        );
        bundle.push_str(&format!(
            "\n\nFILE: {name}\n{}",
            std::str::from_utf8(&bytes)?
        ));
    }
    Ok(bundle)
}

pub fn contract() -> Value {
    json!({"schema":"slop_ninja_instruction_matrix_v1",
        "styles":STYLES.iter().map(|(id,instruction)|json!({"id":id,"instruction":instruction})).collect::<Vec<_>>(),
        "requested_source_span_percentages":[40,100],
        "crossing":"every source x every style x every percentage x every available model",
        "fix_slop":{"repository":"https://github.com/sam0x17/fix-slop",
            "revision":"78555d2f173db50b8ca03a0cb0961cac4abee0e0",
            "files":FILES.iter().map(|(path,sha256)|json!({"path":path,"sha256":sha256})).collect::<Vec<_>>()},
        "scope":"Train-family collection; no fitting or model selection in this acquisition",
        "acceptance":"retain all completed responses independent of detector scores; style compliance and fidelity are unverified",
        "identity":"actual generator configuration and response, never inferred from requested style"})
}

pub fn prompt(style: &str, text: &str, skill: &str) -> Result<String> {
    let (_, instruction) = STYLES
        .iter()
        .find(|(id, _)| *id == style)
        .ok_or_else(|| anyhow::anyhow!("unknown instruction style"))?;
    let extra = if style == "fix_slop" { skill } else { "" };
    Ok(format!(
        "Rewrite the following English passage in your own words while preserving its meaning, facts, perspective, and approximate length. Return only replacement prose, retaining paragraph breaks where useful, with no title, preface, analysis, quotation delimiters, or explanation.\n{instruction}{extra}\n\nPASSAGE TO REWRITE:\n{text}"
    ))
}

pub fn cases(enabled: bool, percentages: &[usize]) -> Vec<(&'static str, usize)> {
    if enabled {
        STYLES
            .iter()
            .flat_map(|(style, _)| [(*style, 40), (*style, 100)])
            .collect()
    } else {
        percentages.iter().map(|&p| ("", p)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_cross_has_no_missing_or_duplicate_cells() {
        let cells = cases(true, &[]);
        assert_eq!(cells.len(), 20);
        assert_eq!(
            cells
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            20
        );
        for (style, _) in STYLES {
            assert!(cells.contains(&(*style, 40)) && cells.contains(&(*style, 100)));
        }
        assert_eq!(
            cases(false, &[0, 20, 60, 100]),
            vec![("", 0), ("", 20), ("", 60), ("", 100)]
        );
    }
    #[test]
    fn plain_has_no_style_directive_and_skill_is_only_in_skill_cell() {
        let plain = prompt("plain", "Synthetic source passage.", "SECRET_SKILL_MARKER").unwrap();
        assert!(!plain.contains("SECRET_SKILL_MARKER"));
        assert!(!plain.contains("British"));
        assert!(
            prompt(
                "fix_slop",
                "Synthetic source passage.",
                "SECRET_SKILL_MARKER"
            )
            .unwrap()
            .contains("SECRET_SKILL_MARKER")
        );
        assert!(prompt("made_up", "text", "").is_err());
    }
}
