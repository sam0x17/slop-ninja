//! Optional item-ordered request selection for `generate`.
//!
//! An order file lists plan item ids, one per line (blank lines and `#`
//! comments ignored). With an order, only requests of listed items run, in
//! list order (requests of one item keep their plan order). Without an
//! order, selection is the plan order filtered by split and cohort.
use crate::{generation::ReqState, plan::Request};
use anyhow::{Result, bail, ensure};
use std::collections::{HashMap, HashSet};

/// Placeholder replaced by the generator key in an order-file path.
pub const GEN_PLACEHOLDER: &str = "{GEN}";

/// Parse an order file's text into item ids with their list positions.
pub fn parse_order(text: &str) -> Result<Vec<String>> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let id = line.trim();
        if id.is_empty() || id.starts_with('#') {
            continue;
        }
        ensure!(seen.insert(id.to_owned()), "duplicate item id {id} on line {}", n + 1);
        ids.push(id.to_owned());
    }
    ensure!(!ids.is_empty(), "order file lists no item ids");
    Ok(ids)
}

/// Pending requests (not done) after split, cohort and optional item-order
/// filtering, each with its first unused attempt number.
pub fn select_pending(
    reqs: Vec<Request>,
    splits: &[String],
    cohorts: Option<&[String]>,
    order: Option<&[String]>,
    states: &HashMap<String, ReqState>,
) -> Result<Vec<(Request, u32)>> {
    let kept: Vec<Request> = reqs
        .into_iter()
        .filter(|r| splits.contains(&r.split))
        .filter(|r| cohorts.is_none_or(|w| r.cohort.as_ref().is_some_and(|c| w.contains(c))))
        .collect();
    let kept = match order {
        None => kept,
        Some(ids) => {
            let pos: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
            let present: HashSet<&str> = kept.iter().map(|r| r.item_id.as_str()).collect();
            let missing: Vec<&str> = ids.iter().map(String::as_str).filter(|id| !present.contains(id)).collect();
            if !missing.is_empty() {
                bail!(
                    "{} listed item ids have no request for this generator in the selected splits/cohorts (first: {})",
                    missing.len(),
                    missing[0]
                );
            }
            let mut v: Vec<(usize, usize, Request)> = kept
                .into_iter()
                .enumerate()
                .filter_map(|(i, r)| pos.get(r.item_id.as_str()).map(|&p| (p, i, r)))
                .collect();
            v.sort_by_key(|(p, i, _)| (*p, *i));
            v.into_iter().map(|(_, _, r)| r).collect()
        }
    };
    let mut pending = Vec::new();
    for r in kept {
        let st = states.get(&r.request_id).cloned().unwrap_or_default();
        if !st.done_within(r.attempts()) {
            let first = st.used();
            pending.push((r, first));
        }
    }
    Ok(pending)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Decoding;

    fn req(id: &str, item: &str, split: &str, cohort: &str) -> Request {
        let d: Decoding = serde_json::from_value(serde_json::json!({"profile":"A","temperature":0.7,"top_p":0.9,"top_k":0,
            "min_p":0.0,"repeat_penalty":1.0,"max_tokens":16}))
        .unwrap();
        Request {
            request_id: id.into(),
            item_id: item.into(),
            parent_id: "p".into(),
            split: split.into(),
            generator: "g".into(),
            endpoint: "chat".into(),
            template_id: "t".into(),
            prompt: "x".into(),
            span_index: 0,
            source_span: None,
            target_words: 10,
            cut_budget: None,
            decoding: d,
            request_sha256: "h".into(),
            seed: 1,
            op: None,
            cohort: Some(cohort.into()),
            attempt_limit: None,
        }
    }

    fn reqs() -> Vec<Request> {
        vec![
            req("r1", "a", "train", "c1"),
            req("r2", "b", "train", "c1"),
            req("r3", "b", "train", "c1"),
            req("r4", "c", "train", "c2"),
            req("r5", "d", "cal", "c1"),
        ]
    }

    fn ids(v: &[(Request, u32)]) -> Vec<&str> {
        v.iter().map(|(r, _)| r.request_id.as_str()).collect()
    }

    #[test]
    fn without_order_keeps_plan_order() {
        let s = vec!["train".to_owned()];
        let p = select_pending(reqs(), &s, None, None, &HashMap::new()).unwrap();
        assert_eq!(ids(&p), ["r1", "r2", "r3", "r4"]);
        let c = vec!["c1".to_owned()];
        let p = select_pending(reqs(), &s, Some(&c), None, &HashMap::new()).unwrap();
        assert_eq!(ids(&p), ["r1", "r2", "r3"]);
    }

    #[test]
    fn order_restricts_and_reorders_items() {
        let s = vec!["train".to_owned()];
        let o = parse_order("# comment\nc\n\nb\n").unwrap();
        let p = select_pending(reqs(), &s, None, Some(&o), &HashMap::new()).unwrap();
        assert_eq!(ids(&p), ["r4", "r2", "r3"]);
    }

    #[test]
    fn order_skips_done_requests() {
        let s = vec!["train".to_owned()];
        let o = parse_order("b\na\n").unwrap();
        let mut st = HashMap::new();
        let mut done = ReqState::default();
        done.responses.insert(0, serde_json::json!({"verdict":{"accepted":true}}));
        st.insert("r2".to_owned(), done);
        let p = select_pending(reqs(), &s, None, Some(&o), &st).unwrap();
        assert_eq!(ids(&p), ["r3", "r1"]);
    }

    #[test]
    fn a_carried_attempt_limit_bounds_pending() {
        // B5b (ruling R6): a regenerated mirror's request carries its remaining attempts
        let s = vec!["train".to_owned()];
        let mut rs = reqs();
        rs[0].attempt_limit = Some(1);
        rs[1].attempt_limit = Some(0);
        let mut st = HashMap::new();
        let p = select_pending(rs.clone(), &s, None, None, &st).unwrap();
        // a limit of 0 is never pending; a limit of 1 is pending from attempt 0
        assert_eq!(ids(&p), ["r1", "r3", "r4"]);
        assert_eq!(p[0].1, 0);
        // one attempt used: done under a limit of 1, pending (attempt 1) under R0's two
        let mut one = ReqState::default();
        one.intents.push(0);
        st.insert("r1".to_owned(), one.clone());
        st.insert("r3".to_owned(), one);
        let p = select_pending(rs, &s, None, None, &st).unwrap();
        assert_eq!(p.iter().map(|(r, a)| (r.request_id.as_str(), *a)).collect::<Vec<_>>(), [("r3", 1), ("r4", 0)]);
    }

    #[test]
    fn order_rejects_unknown_or_out_of_split_items() {
        let s = vec!["train".to_owned()];
        assert!(select_pending(reqs(), &s, None, Some(&parse_order("a\nzz\n").unwrap()), &HashMap::new()).is_err());
        assert!(select_pending(reqs(), &s, None, Some(&parse_order("d\n").unwrap()), &HashMap::new()).is_err());
    }

    #[test]
    fn parse_rejects_duplicates_and_empty() {
        assert!(parse_order("a\na\n").is_err());
        assert!(parse_order("# only\n\n").is_err());
    }
}
