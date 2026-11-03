//! Fixed marginal, permutation and inventory projections for a TRAIN diagnostic.
use crate::{
    function_word_roles::Event,
    predicate_operators::{
        CarrierMorphology, CarrierPosition, FiniteCarrier, FiniteSignature, OperatorRole,
        OperatorSequence, PredicateEvent, PredicateObservation,
    },
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const SEEDS: [u64; 5] = [0, 17, 29, 101, 1009];
pub const VIEW_NAMES: [&str; 10] = [
    "function_lemma_pos",
    "function_role_context",
    "function_joint",
    "function_shuffle_0",
    "function_shuffle_17",
    "function_shuffle_29",
    "function_shuffle_101",
    "function_shuffle_1009",
    "operator_inventory",
    "operator_sequence",
];
pub type Counts = BTreeMap<String, u64>;

fn add(counts: &mut Counts, key: String) -> Result<()> {
    let n = counts.entry(key).or_default();
    *n = n.checked_add(1).context("Projection count overflow")?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CarrierEvidence {
    pos: String,
    tag: String,
    morphology: CarrierMorphology,
    verb_form_fin: bool,
    finite_tag: bool,
    other_verb_forms: Vec<String>,
}

impl From<&FiniteCarrier> for CarrierEvidence {
    fn from(value: &FiniteCarrier) -> Self {
        Self {
            pos: value.pos.clone(),
            tag: value.tag.clone(),
            morphology: value.morphology.clone(),
            verb_form_fin: value.verb_form_fin,
            finite_tag: value.finite_tag,
            other_verb_forms: value.other_verb_forms.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InventoryOperator {
    role: OperatorRole,
    lemma: Option<String>,
    finite: Option<CarrierEvidence>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InventoryEntry {
    operator: InventoryOperator,
    count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Cardinality {
    None,
    One,
    Multiple,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PredicateInventory {
    incoming_dependency: String,
    head_pos: String,
    finite_state: Cardinality,
    head_finite: Option<CarrierEvidence>,
    /// Sorted by the compact serialized operator item. Multiplicity is retained.
    operators: Vec<InventoryEntry>,
}

/// Remove side, order, paths and parent identities. Attach finite evidence to
/// its operator before deleting the source ordinal; HEAD remains separate.
pub fn inventory(event: &PredicateEvent) -> Result<PredicateInventory> {
    let operators = match &event.operators {
        OperatorSequence::None => &[][..],
        OperatorSequence::Present { items } => {
            ensure!(!items.is_empty(), "Empty PRESENT operator sequence");
            items.as_slice()
        }
    };
    let (finite_state, carriers): (_, &[FiniteCarrier]) = match &event.finite {
        FiniteSignature::None => (Cardinality::None, &[]),
        FiniteSignature::One { carrier } => (Cardinality::One, std::slice::from_ref(carrier)),
        FiniteSignature::Multiple { carriers } => {
            ensure!(carriers.len() > 1, "MULTIPLE requires multiple carriers");
            (Cardinality::Multiple, carriers)
        }
    };
    let mut head_finite = None;
    let mut operator_finite = BTreeMap::new();
    for carrier in carriers {
        ensure!(
            carrier.verb_form_fin || carrier.finite_tag,
            "Carrier lacks finite evidence"
        );
        match carrier.position {
            CarrierPosition::Head => {
                ensure!(
                    head_finite
                        .replace(CarrierEvidence::from(carrier))
                        .is_none(),
                    "Repeated HEAD finite carrier"
                );
            }
            CarrierPosition::Operator { sequence_index } => {
                let op = operators
                    .get(sequence_index)
                    .context("Finite operator ordinal out of range")?;
                ensure!(
                    !matches!(op.role, OperatorRole::HeadAux | OperatorRole::Neg),
                    "Invalid operator finite-carrier role"
                );
                ensure!(
                    operator_finite
                        .insert(sequence_index, CarrierEvidence::from(carrier))
                        .is_none(),
                    "Repeated operator finite carrier"
                );
            }
        }
    }
    let mut bag = BTreeMap::<String, InventoryEntry>::new();
    for (index, op) in operators.iter().enumerate() {
        let item = InventoryOperator {
            role: op.role,
            lemma: op.lemma.clone(),
            finite: operator_finite.remove(&index),
        };
        let key = serde_json::to_string(&item)?;
        if let Some(entry) = bag.get_mut(&key) {
            entry.count += 1;
        } else {
            bag.insert(
                key,
                InventoryEntry {
                    operator: item,
                    count: 1,
                },
            );
        }
    }
    Ok(PredicateInventory {
        incoming_dependency: event.incoming_dependency.clone(),
        head_pos: event.head_pos.clone(),
        finite_state,
        head_finite,
        operators: bag.into_values().collect(),
    })
}

pub fn marginal_counts(events: &[Event]) -> Result<(Counts, Counts)> {
    let mut lexical = Counts::new();
    let mut roles = Counts::new();
    for event in events {
        add(
            &mut lexical,
            serde_json::to_string(&(&event.lemma, &event.own_pos))?,
        )?;
        add(
            &mut roles,
            serde_json::to_string(&(
                &event.own_pos,
                &event.incoming_relation,
                &event.head_pos,
                event.side,
            ))?,
        )?;
    }
    Ok((lexical, roles))
}

#[derive(Clone, Copy)]
pub struct Random(pub u64);
impl Random {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Rejection avoids modulo bias; the interval has a multiple-of-bound size.
    pub fn bounded(&mut self, bound: u64) -> u64 {
        assert!(bound > 0 && self.0 != 0);
        // A nonzero xorshift state visits 1..=u64::MAX, not all 2^64 words.
        let limit = u64::MAX - u64::MAX % bound;
        loop {
            let value = self.next() - 1;
            if value < limit {
                return value % bound;
            }
        }
    }
}

pub fn permutation(
    events: &[Event],
    post_id: &str,
    seed: u64,
) -> Result<(Vec<Option<String>>, u64)> {
    ensure!(SEEDS.contains(&seed), "Unregistered shuffle seed");
    let encoded = serde_json::to_vec(&("slopninja-function-role-permutation-v1", seed, post_id))?;
    let digest = Sha256::digest(encoded);
    let mut state = u64::from_le_bytes(digest[..8].try_into().unwrap());
    if state == 0 {
        state = 0x6a09e667f3bcc909;
    }
    let mut random = Random(state);
    let mut groups = BTreeMap::<&str, Vec<usize>>::new();
    ensure!(
        events
            .windows(2)
            .all(|pair| pair[0].token_index < pair[1].token_index),
        "Function events not in unique source-token order"
    );
    for (index, event) in events.iter().enumerate() {
        groups.entry(&event.own_pos).or_default().push(index);
    }
    let mut lemmas = events
        .iter()
        .map(|event| event.lemma.clone())
        .collect::<Vec<_>>();
    for indices in groups.values() {
        for upper in (1..indices.len()).rev() {
            let other = random.bounded((upper + 1) as u64) as usize;
            lemmas.swap(indices[upper], indices[other]);
        }
    }
    let changed = events
        .iter()
        .zip(&lemmas)
        .filter(|(event, lemma)| &event.lemma != *lemma)
        .count() as u64;
    Ok((lemmas, changed))
}

#[derive(Serialize)]
pub struct Projection {
    pub views: BTreeMap<String, Counts>,
    pub opportunities: BTreeMap<String, u64>,
    pub shuffle_changed_lemma_events: BTreeMap<u64, u64>,
    pub shuffle_joint_counts_changed: BTreeMap<u64, bool>,
}

pub fn project(
    post_id: &str,
    functions: &[Event],
    predicates: &[PredicateObservation],
) -> Result<Projection> {
    ensure!(
        !functions.is_empty() && !predicates.is_empty(),
        "Required family has zero opportunities; complete diagnostic must stop"
    );
    let (lemma, role) = marginal_counts(functions)?;
    let mut views = BTreeMap::from([
        ("function_lemma_pos".into(), lemma.clone()),
        ("function_role_context".into(), role.clone()),
    ]);
    let mut original = Counts::new();
    for event in functions {
        add(&mut original, event.key()?)?;
    }
    views.insert("function_joint".into(), original.clone());
    let mut changes = BTreeMap::new();
    let mut count_changes = BTreeMap::new();
    for seed in SEEDS {
        let (lemmas, changed) = permutation(functions, post_id, seed)?;
        let mut joint = Counts::new();
        let mut shuffled_lexical = Counts::new();
        for (event, value) in functions.iter().zip(&lemmas) {
            add(
                &mut shuffled_lexical,
                serde_json::to_string(&(value, &event.own_pos))?,
            )?;
            add(
                &mut joint,
                serde_json::to_string(&(
                    value,
                    &event.own_pos,
                    &event.incoming_relation,
                    &event.head_pos,
                    event.side,
                ))?,
            )?;
        }
        ensure!(
            shuffled_lexical == lemma,
            "Permutation changed lemma/POS marginal"
        );
        // Nonlemma event fields are never modified, so the role marginal is exact.
        changes.insert(seed, changed);
        count_changes.insert(seed, joint != original);
        views.insert(format!("function_shuffle_{seed}"), joint);
    }
    let mut inventory_counts = Counts::new();
    let mut sequences = Counts::new();
    for observation in predicates {
        add(
            &mut inventory_counts,
            serde_json::to_string(&inventory(&observation.event)?)?,
        )?;
        add(&mut sequences, serde_json::to_string(&observation.event)?)?;
    }
    views.insert("operator_inventory".into(), inventory_counts);
    views.insert("operator_sequence".into(), sequences);
    let opportunities = views
        .iter()
        .map(|(name, counts)| (name.clone(), counts.values().sum()))
        .collect();
    Ok(Projection {
        views,
        opportunities,
        shuffle_changed_lemma_events: changes,
        shuffle_joint_counts_changed: count_changes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::predicate_operators::{AttachmentParent, Operator, Side};
    fn function(index: usize, lemma: Option<&str>, pos: &str, dep: &str) -> Event {
        Event {
            token_index: index,
            head_index: 9,
            token_byte_span: [index, index + 1],
            head_byte_span: [9, 10],
            lemma: lemma.map(str::to_owned),
            own_pos: pos.into(),
            incoming_relation: dep.into(),
            head_pos: "VERB".into(),
            side: crate::function_word_roles::Side::L,
        }
    }
    fn event() -> PredicateEvent {
        PredicateEvent {
            incoming_dependency: "ROOT".into(),
            head_pos: "VERB".into(),
            operators: OperatorSequence::Present {
                items: vec![
                    Operator {
                        role: OperatorRole::Aux,
                        lemma: Some("may".into()),
                        side: Side::Left,
                        attachment_parent: AttachmentParent::Head,
                        attachment_path: vec![OperatorRole::Aux],
                    },
                    Operator {
                        role: OperatorRole::Neg,
                        lemma: Some("not".into()),
                        side: Side::Left,
                        attachment_parent: AttachmentParent::Operator { sequence_index: 0 },
                        attachment_path: vec![OperatorRole::Aux, OperatorRole::Neg],
                    },
                ],
            },
            finite: FiniteSignature::One {
                carrier: FiniteCarrier {
                    position: CarrierPosition::Operator { sequence_index: 0 },
                    pos: "AUX".into(),
                    tag: "MD".into(),
                    morphology: CarrierMorphology {
                        verb_form: None,
                        tense: None,
                        mood: None,
                        person: None,
                        number: None,
                    },
                    verb_form_fin: false,
                    finite_tag: true,
                    other_verb_forms: vec![],
                },
            },
        }
    }
    #[test]
    fn permutations_are_post_stable_and_preserve_null_pos_and_role_marginals() {
        let events = vec![
            function(0, Some("she"), "PRON", "nsubj"),
            function(1, None, "PRON", "obj"),
            function(2, Some("they"), "PRON", "obl"),
            function(3, Some("can"), "AUX", "aux"),
        ];
        let original = marginal_counts(&events).unwrap();
        for seed in SEEDS {
            let (lemmas, changed) = permutation(&events, "synthetic-post", seed).unwrap();
            assert_eq!(
                (lemmas.clone(), changed),
                permutation(&events, "synthetic-post", seed).unwrap()
            );
            let mut shuffled = events.clone();
            for (event, value) in shuffled.iter_mut().zip(lemmas) {
                event.lemma = value;
            }
            assert_eq!(marginal_counts(&shuffled).unwrap(), original);
            assert_eq!(shuffled[3].lemma, events[3].lemma);
        }
        let projected = project(
            "synthetic-post",
            &events,
            &[PredicateObservation {
                head_index: 9,
                event: event(),
            }],
        )
        .unwrap();
        assert_eq!(projected.views.len(), 10);
        assert_eq!(projected.opportunities["function_joint"], 4);
        assert_eq!(projected.opportunities["operator_inventory"], 1);
    }
    #[test]
    fn inventory_removes_order_parent_side_and_finite_ordinal_without_losing_assignment() {
        let a = event();
        let mut b = a.clone();
        let OperatorSequence::Present { items } = &mut b.operators else {
            panic!()
        };
        items.swap(0, 1);
        for op in items {
            op.side = Side::Right;
            op.attachment_parent = AttachmentParent::Head;
            op.attachment_path = vec![op.role];
        }
        let FiniteSignature::One { carrier } = &mut b.finite else {
            panic!()
        };
        carrier.position = CarrierPosition::Operator { sequence_index: 1 };
        let left = serde_json::to_string(&inventory(&a).unwrap()).unwrap();
        assert_eq!(
            left,
            serde_json::to_string(&inventory(&b).unwrap()).unwrap()
        );
        assert_ne!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
        for forbidden in [
            "sequence_index",
            "attachment_parent",
            "attachment_path",
            "side",
            "position",
        ] {
            assert!(!left.contains(forbidden));
        }
        let OperatorSequence::Present { items } = &mut b.operators else {
            panic!()
        };
        items[1].lemma = Some("must".into());
        assert_ne!(
            left,
            serde_json::to_string(&inventory(&b).unwrap()).unwrap()
        );
    }
    #[test]
    fn inventory_retains_duplicates_missing_lemmas_and_head_finite_evidence() {
        let mut a = event();
        let OperatorSequence::Present { items } = &mut a.operators else {
            panic!()
        };
        items.push(items[1].clone());
        let projected = inventory(&a).unwrap();
        assert!(projected.operators.iter().any(|entry| entry.count == 2));
        let mut b = a.clone();
        let OperatorSequence::Present { items } = &mut b.operators else {
            panic!()
        };
        items[2].lemma = None;
        assert_ne!(
            serde_json::to_string(&inventory(&a).unwrap()).unwrap(),
            serde_json::to_string(&inventory(&b).unwrap()).unwrap()
        );
        let FiniteSignature::One { carrier } = &mut b.finite else {
            panic!()
        };
        carrier.position = CarrierPosition::Head;
        let projected = inventory(&b).unwrap();
        assert!(projected.head_finite.is_some());
        assert!(
            projected
                .operators
                .iter()
                .all(|entry| entry.operator.finite.is_none())
        );
    }
    #[test]
    fn malformed_carrier_mappings_and_unavailable_views_fail() {
        let mut a = event();
        let FiniteSignature::One { carrier } = &mut a.finite else {
            panic!()
        };
        carrier.position = CarrierPosition::Operator { sequence_index: 99 };
        assert!(inventory(&a).is_err());
        let FiniteSignature::One { carrier } = &mut a.finite else {
            panic!()
        };
        carrier.position = CarrierPosition::Operator { sequence_index: 1 };
        assert!(inventory(&a).is_err());
        let mut a = event();
        let FiniteSignature::One { carrier } = &a.finite else {
            panic!()
        };
        a.finite = FiniteSignature::Multiple {
            carriers: vec![carrier.clone(), carrier.clone()],
        };
        assert!(inventory(&a).is_err());
        assert!(project("missing", &[], &[]).is_err());
        let mut events = vec![
            function(1, None, "PRON", "obj"),
            function(0, None, "PRON", "nsubj"),
        ];
        assert!(permutation(&events, "x", 0).is_err());
        events.reverse();
        assert!(permutation(&events, "x", 999).is_err());
    }
}
