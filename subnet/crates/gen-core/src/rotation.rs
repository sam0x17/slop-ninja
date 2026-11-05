//! The per-source seeded rotation (`generation_rotation_v1`).
//!
//! A **stream** is `(manifest version, kind, domain)`: mixed and model-only
//! documents of a pool domain have separate streams, so both kinds get the
//! whole spread. A stream's state ([`StreamState`]) is durable in the cohort
//! object, advanced inside the draw transaction that creates the documents
//! (a draw is applied once, so a duplicate wakeup cannot advance a deck
//! twice), and carried across cohorts: it is never reset by time.
//!
//! Decks of a stream:
//! 1. the **lane deck** (frontier, Workers AI), reshuffled every pair: a
//!    stream's documents are half frontier, half Workers AI, within one;
//! 2. per lane, **model blocks**: an epoch is `C` blocks of `M` draws (`M`
//!    the lane's models, `C` the kind's secondary cells: mixed = span classes
//!    x 2 intensities, model-only = scratch lengths). Block `b` is a fresh
//!    seeded permutation of the models, and model `m` gets cell `(b + o_m) mod
//!    C`, `o_m` a seeded per-model offset fixed for the epoch: every block uses
//!    each model once, every epoch gives each model each cell once, and every
//!    block spreads the cells within one of each other;
//! 3. per model, an **effort deck** over its usable levels, advanced only
//!    when that model is drawn;
//! 4. per intensity, an **operation deck** (mixed only);
//! 5. a separate **fallback deck** over the Workers AI models, used only when a
//!    frontier draw moves to Workers AI (a paused vendor), so fallbacks are
//!    themselves spread and never disturb the main Workers AI deck.
//!
//! Permutations are ranked by `H(rotation_seed, stream, deck, epoch, ...)`
//! with the deployment's private rotation seed (not a cohort seed: streams
//! span cohorts). Avoidance (a reused parent's earlier models and
//! operations) swaps the deck's next entry with the nearest later one not
//! avoided, within the current block or round, so every deck stays a
//! permutation and the counts above still hold. The voice is not a deck: it
//! is a seeded random draw per document ([`crate::voice`]).

use crate::hash::{h, h64, permutation};
use crate::manifest::{Intensity, Manifest, Refused};
use crate::vendor::{DocKind, Effort, Lane, Vendor};
use crate::voice::{Category, Library, VoiceChoice};
use crate::ROTATION_VERSION;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The deployment's rotation seed, from its private seed.
pub fn rotation_seed(private_seed: &[u8; 32]) -> [u8; 32] {
    h(&[private_seed, ROTATION_VERSION.as_bytes()])
}

/// The stream key of `(manifest version, kind, domain)`.
pub fn stream_key(manifest_version: u32, kind: DocKind, domain: &str) -> String {
    format!("{ROTATION_VERSION}/v{manifest_version}/{}/{domain}", kind.code())
}

/// A deck of `n` entries: the current round's permutation and position.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deck {
    pub epoch: u64,
    pub pos: u32,
    /// The current round's order (empty: not yet drawn this round).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<u16>,
}

/// Swaps `order[pos]` with the nearest later entry `avoid` does not refuse
/// (none: keep it), and returns the entry now at `pos`.
fn take_forward(order: &mut [u16], pos: usize, avoid: &dyn Fn(u16) -> bool) -> u16 {
    let n = order.len();
    if let Some(&v) = order.get(pos)
        && avoid(v)
        && let Some(j) = (pos + 1..n).find(|&j| order.get(j).is_some_and(|&x| !avoid(x)))
    {
        order.swap(pos, j);
    }
    order.get(pos).copied().unwrap_or(0)
}

impl Deck {
    /// The next entry of a deck of `n` (`n >= 1`), and the position it came
    /// from `(epoch, pos)`.
    fn next(&mut self, seed: &[u8; 32], label: &[&[u8]], n: usize, avoid: &dyn Fn(u16) -> bool) -> (u16, (u64, u32)) {
        if self.order.len() != n {
            let e = self.epoch.to_be_bytes();
            let mut parts = label.to_vec();
            parts.push(&e);
            self.order = permutation(seed, &parts, n);
            self.pos = 0;
        }
        let at = (self.epoch, self.pos);
        let v = take_forward(&mut self.order, self.pos as usize, avoid);
        self.pos += 1;
        if self.pos as usize >= n {
            self.epoch += 1;
            self.pos = 0;
            self.order.clear();
        }
        (v, at)
    }
}

/// A lane's model blocks (module docs, item 2).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockDeck {
    pub epoch: u64,
    pub block: u32,
    pub pos: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<u16>,
    /// Per model index, its cell offset for the epoch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offsets: Vec<u16>,
}

/// Where a block draw came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockPos {
    pub epoch: u64,
    pub block: u32,
    pub pos: u32,
}

impl BlockDeck {
    /// The next (model index, secondary cell) of `m` models and `c` cells.
    fn next(&mut self, seed: &[u8; 32], label: &[u8], m: usize, c: usize, avoid: &dyn Fn(u16) -> bool) -> (u16, u16, BlockPos) {
        let e = self.epoch.to_be_bytes();
        if self.offsets.len() != m {
            let rank = permutation(seed, &[label, b"offsets", &e], m);
            let mut offsets = vec![0u16; m];
            for (k, model) in rank.iter().enumerate() {
                if let Some(o) = offsets.get_mut(*model as usize) {
                    *o = (k % c) as u16;
                }
            }
            self.offsets = offsets;
        }
        if self.order.len() != m {
            let b = self.block.to_be_bytes();
            self.order = permutation(seed, &[label, b"block", &e, &b], m);
            self.pos = 0;
        }
        let at = BlockPos { epoch: self.epoch, block: self.block, pos: self.pos };
        let model = take_forward(&mut self.order, self.pos as usize, avoid);
        let off = self.offsets.get(model as usize).copied().unwrap_or(0) as usize;
        let cell = ((self.block as usize + off) % c) as u16;
        self.pos += 1;
        if self.pos as usize >= m {
            self.pos = 0;
            self.block += 1;
            self.order.clear();
            if self.block as usize >= c {
                self.block = 0;
                self.epoch += 1;
                self.offsets.clear();
            }
        }
        (model, cell, at)
    }
}

/// One stream's durable state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamState {
    /// Assignments made so far.
    pub draws: u64,
    pub lane: Deck,
    pub frontier: BlockDeck,
    pub workers_ai: BlockDeck,
    pub fallback: BlockDeck,
    /// Per model id.
    #[serde(default)]
    pub effort: BTreeMap<String, Deck>,
    /// Per intensity code.
    #[serde(default)]
    pub operation: BTreeMap<String, Deck>,
    /// Per Workers AI model id, the effort deck of fallback draws (separate
    /// from the main effort decks, so fallbacks spread over every level
    /// equally and never shift the main decks).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fallback_effort: BTreeMap<String, Deck>,
}

/// Why a draw moved from the frontier lane to Workers AI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackCause {
    /// The vendor answered with its spend-cap or quota response.
    SpendCap,
    /// The vendor's key did not resolve or was refused.
    KeyUnavailable,
    /// The item reached the plan's consecutive-outage bound.
    Outage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneFallback {
    pub from: Vendor,
    pub cause: FallbackCause,
    /// The model and effort the rotation drew (and counts).
    pub drawn_model: String,
    pub drawn_effort: Effort,
}

/// Deck positions an assignment came from (audit only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Positions {
    pub draw: u64,
    pub lane: (u64, u32),
    pub block: BlockPos,
    pub effort: (u64, u32),
}

/// One generated document's cell: the only input its generation uses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenAssignment {
    pub rotation_version: String,
    pub manifest_version: u32,
    pub stream: String,
    pub kind: DocKind,
    /// The lane the rotation drew (frontier stays frontier in the counts
    /// even when [`fallback`](Self::fallback) moved the call).
    pub lane: Lane,
    pub model: String,
    pub effort: Effort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_percent: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intensity: Option<Intensity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scratch_words: Option<u32>,
    pub voice: VoiceChoice,
    pub voice_library_version: u32,
    pub template: String,
    pub positions: Positions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<LaneFallback>,
}

impl GenAssignment {
    /// The lane the call runs on (Workers AI after a fallback).
    pub fn call_lane(&self) -> Lane {
        if self.fallback.is_some() { Lane::WorkersAi } else { self.lane }
    }

    /// The model and effort the rotation drew (before any fallback).
    pub fn drawn(&self) -> (&str, Effort) {
        match &self.fallback {
            Some(f) => (&f.drawn_model, f.drawn_effort),
            None => (&self.model, self.effort),
        }
    }
}

/// What a draw must avoid: a reused parent's earlier models, voices and
/// operations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Avoid {
    pub models: Vec<String>,
    pub voices: Vec<String>,
    pub operations: Vec<String>,
}

/// The template of a kind and voice.
pub fn template_for(kind: DocKind, voice: &VoiceChoice) -> &'static str {
    match (kind, voice.category()) {
        (DocKind::Mixed, None) => "rewrite_v2_plain",
        (DocKind::Mixed, Some(Category::MatchSurrounding)) => "rewrite_v2_match_context",
        (DocKind::Mixed, Some(_)) => "rewrite_v2_voiced",
        (DocKind::ModelOnly, None) => "scratch_v2_plain",
        (DocKind::ModelOnly, Some(_)) => "scratch_v2_voiced",
    }
}

/// The deployment-wide pause of a vendor, as the draw sees it.
pub type Paused<'a> = &'a dyn Fn(Vendor) -> Option<FallbackCause>;

fn effort_of(state: &mut StreamState, seed: &[u8; 32], stream: &str, model: &crate::manifest::ModelEntry) -> (Effort, (u64, u32)) {
    let levels = model.usable_efforts();
    let deck = state.effort.entry(model.id.clone()).or_default();
    let (i, at) = deck.next(seed, &[stream.as_bytes(), b"effort", model.id.as_bytes()], levels.len().max(1), &|_| false);
    (levels.get(i as usize).copied().unwrap_or(Effort::Default), at)
}

/// Assigns the next document of `stream` (`kind`, its domain), advancing
/// `state`. `doc_key` is the document's private key (the voice draw's input);
/// `paused` names a paused vendor's cause: its draws move to the Workers AI
/// fallback deck, recorded, while the main decks advance as usual.
#[allow(clippy::too_many_arguments)]
pub fn assign(
    state: &mut StreamState,
    m: &Manifest,
    lib: &Library,
    split: crate::voice::VoiceSplit,
    seed: &[u8; 32],
    stream: &str,
    kind: DocKind,
    doc_key: &str,
    avoid: &Avoid,
    paused: Paused<'_>,
) -> Result<GenAssignment, Refused> {
    let c = m.secondary_cells(kind);
    let draw = state.draws;
    let (li, lane_at) = state.lane.next(seed, &[stream.as_bytes(), b"lane"], Lane::ALL.len(), &|_| false);
    let lane = *Lane::ALL.get(li as usize).ok_or(Refused("rotation lane"))?;
    let models = m.lane_models(lane);
    if models.is_empty() || c == 0 {
        return Err(Refused("rotation (a lane without models or a kind without cells)"));
    }
    let avoid_model = |i: u16| models.get(i as usize).is_some_and(|x| avoid.models.contains(&x.id));
    let deck = match lane {
        Lane::Frontier => &mut state.frontier,
        Lane::WorkersAi => &mut state.workers_ai,
    };
    let label = [stream.as_bytes(), b"/", lane.code().as_bytes()].concat();
    let (mi, cell, block_at) = deck.next(seed, &label, models.len(), c, &avoid_model);
    let model = *models.get(mi as usize).ok_or(Refused("rotation model"))?;
    let (effort, effort_at) = effort_of(state, seed, stream, model);
    let (mut span_percent, mut intensity, mut operation, mut scratch_words) = (None, None, None, None);
    match kind {
        DocKind::Mixed => {
            let cell = cell as usize;
            span_percent = m.span_classes_percent.get(cell / Intensity::ALL.len()).copied();
            let i = *Intensity::ALL.get(cell % Intensity::ALL.len()).ok_or(Refused("rotation intensity"))?;
            intensity = Some(i);
            let ops = m.operations.of(i);
            let avoid_op = |k: u16| ops.get(k as usize).is_some_and(|o| avoid.operations.contains(&o.id));
            let d = state.operation.entry(i.code().to_owned()).or_default();
            let (oi, _) = d.next(seed, &[stream.as_bytes(), b"operation", i.code().as_bytes()], ops.len().max(1), &avoid_op);
            operation = ops.get(oi as usize).map(|o| o.id.clone());
        }
        DocKind::ModelOnly => scratch_words = m.scratch_words.get(cell as usize).copied(),
    }
    let voice = lib.draw(seed, doc_key, kind, &avoid.voices, split);
    let mut a = GenAssignment {
        rotation_version: ROTATION_VERSION.into(),
        manifest_version: m.version,
        stream: stream.into(),
        kind,
        lane,
        model: model.id.clone(),
        effort,
        span_percent,
        intensity,
        operation,
        scratch_words,
        template: template_for(kind, &voice).into(),
        voice,
        voice_library_version: lib.header.version,
        positions: Positions { draw, lane: lane_at, block: block_at, effort: effort_at },
        fallback: None,
    };
    state.draws += 1;
    if let Some(cause) = paused(model.vendor) {
        fallback(state, m, seed, &mut a, avoid, model.vendor, cause)?;
    }
    Ok(a)
}

/// Moves `a` to Workers AI from the stream's fallback deck, keeping its
/// secondary cell, voice, operation and template, and recording the move
/// (a pending item of a paused vendor, or an item past its outage bound).
/// An assignment already on Workers AI is left as it is.
#[allow(clippy::too_many_arguments)]
pub fn fallback(
    state: &mut StreamState,
    m: &Manifest,
    seed: &[u8; 32],
    a: &mut GenAssignment,
    avoid: &Avoid,
    from: Vendor,
    cause: FallbackCause,
) -> Result<(), Refused> {
    if a.call_lane() == Lane::WorkersAi {
        return Ok(());
    }
    let models = m.lane_models(Lane::WorkersAi);
    let avoid_model = |i: u16| models.get(i as usize).is_some_and(|x| avoid.models.contains(&x.id));
    let label = [a.stream.as_bytes(), b"/fallback"].concat();
    // The fallback deck spreads models only: one cell per block.
    let (mi, _, _) = state.fallback.next(seed, &label, models.len(), 1, &avoid_model);
    let model = *models.get(mi as usize).ok_or(Refused("rotation fallback model"))?;
    let levels = model.usable_efforts();
    let deck = state.fallback_effort.entry(model.id.clone()).or_default();
    let (k, _) = deck.next(seed, &[a.stream.as_bytes(), b"fallback-effort", model.id.as_bytes()], levels.len().max(1), &|_| false);
    let why = LaneFallback { from, cause, drawn_model: std::mem::replace(&mut a.model, model.id.clone()), drawn_effort: a.effort };
    a.effort = levels.get(k as usize).copied().unwrap_or(Effort::Default);
    a.fallback = Some(why);
    Ok(())
}

/// The model and effort of retry `attempt` (1-based) of a document after a
/// content outcome: the next model of the call's lane, in an order fixed by
/// the document key, not yet in `tried` (cycling once all were tried), with
/// effort `levels[H(key, attempt) mod E]`, skipping cells `disabled` refuses.
/// Decided from the key and attempt only, never from deck state.
pub fn retry_cell(
    m: &Manifest,
    seed: &[u8; 32],
    a: &GenAssignment,
    doc_key: &str,
    attempt: u32,
    tried: &[String],
    disabled: &dyn Fn(&str, Effort) -> bool,
) -> Option<(String, Effort)> {
    let lane = a.call_lane();
    let models = m.lane_models(lane);
    let order = permutation(seed, &[doc_key.as_bytes(), b"retry", lane.code().as_bytes()], models.len());
    let ordered: Vec<&crate::manifest::ModelEntry> = order.iter().filter_map(|i| models.get(*i as usize).copied()).collect();
    let att = attempt.to_be_bytes();
    let pick = |mdl: &crate::manifest::ModelEntry| -> Option<Effort> {
        let levels = mdl.usable_efforts();
        let n = levels.len();
        let start = (h64(&[seed, b"retry-effort", doc_key.as_bytes(), &att]) % n.max(1) as u64) as usize;
        (0..n).filter_map(|k| levels.get((start + k) % n).copied()).find(|e| !disabled(&mdl.id, *e))
    };
    let fresh = ordered.iter().filter(|x| x.id != a.model && !tried.contains(&x.id));
    for mdl in fresh {
        if let Some(e) = pick(mdl) {
            return Some((mdl.id.clone(), e));
        }
    }
    // Every model was tried: cycle through the order by attempt.
    let n = ordered.len();
    (0..n)
        .filter_map(|k| ordered.get((attempt as usize + k) % n.max(1)))
        .find_map(|mdl| pick(mdl).map(|e| (mdl.id.clone(), e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::fixture::planned;
    use crate::voice::fixture::library;
    use proptest::prelude::*;
    use std::collections::BTreeMap as Map;

    fn none(_: Vendor) -> Option<FallbackCause> {
        None
    }

    fn run(seed: [u8; 32], kind: DocKind, n: usize) -> (StreamState, Vec<GenAssignment>) {
        let m = planned();
        let lib = library();
        let s = stream_key(m.version, kind, "wikipedia");
        let mut st = StreamState::default();
        let out = (0..n)
            .map(|i| assign(&mut st, &m, &lib, crate::voice::VoiceSplit::NoneHalf, &seed, &s, kind, &format!("d{i}"), &Avoid::default(), &none).unwrap())
            .collect();
        (st, out)
    }

    fn counts<K: Ord>(it: impl Iterator<Item = K>) -> Map<K, usize> {
        let mut c = Map::new();
        for k in it {
            *c.entry(k).or_insert(0) += 1;
        }
        c
    }

    fn check_spread(m: &Manifest, kind: DocKind, out: &[GenAssignment]) {
        let lanes = counts(out.iter().map(|a| a.lane));
        let f = lanes.get(&Lane::Frontier).copied().unwrap_or(0);
        let w = lanes.get(&Lane::WorkersAi).copied().unwrap_or(0);
        assert!(f.abs_diff(w) <= 1, "lanes {f} {w}");
        for lane in Lane::ALL {
            let ids: Vec<&str> = m.lane_models(lane).iter().map(|x| x.id.as_str()).collect();
            let mm = ids.len();
            let draws: Vec<&GenAssignment> = out.iter().filter(|a| a.lane == lane).collect();
            let j = draws.len();
            let per = counts(draws.iter().map(|a| a.drawn().0));
            for id in &ids {
                let c = per.get(id).copied().unwrap_or(0);
                assert!(c == j / mm || c == j.div_ceil(mm), "{lane:?} {id} {c} of {j}");
            }
            // Effort levels of each model within one of each other.
            for id in &ids {
                let e = counts(draws.iter().filter(|a| a.drawn().0 == *id).map(|a| a.drawn().1));
                let levels = m.model(id).unwrap().usable_efforts();
                let lo = levels.iter().map(|l| e.get(l).copied().unwrap_or(0)).min().unwrap();
                let hi = levels.iter().map(|l| e.get(l).copied().unwrap_or(0)).max().unwrap();
                assert!(hi - lo <= 1, "{id} efforts {e:?}");
            }
            // Every completed epoch: each model meets each cell exactly once.
            let c = m.secondary_cells(kind);
            let epochs = j / (mm * c);
            for ep in 0..epochs {
                let chunk = &draws[ep * mm * c..(ep + 1) * mm * c];
                let cells = counts(chunk.iter().map(|a| (a.drawn().0.to_owned(), a.span_percent, a.intensity, a.scratch_words)));
                assert_eq!(cells.len(), mm * c, "epoch {ep} of {lane:?}");
                assert!(cells.values().all(|v| *v == 1));
            }
        }
    }

    #[test]
    fn a_stream_spreads_lanes_models_cells_and_efforts() {
        let m = planned();
        for kind in [DocKind::Mixed, DocKind::ModelOnly] {
            let (_, out) = run([3; 32], kind, 2 * 14 * 6 * 2 + 7);
            check_spread(&m, kind, &out);
            assert!(out.iter().all(|a| a.fallback.is_none()));
            if kind == DocKind::Mixed {
                assert!(out.iter().all(|a| a.span_percent.is_some() && a.intensity.is_some() && a.operation.is_some()));
                let ops = counts(out.iter().map(|a| (a.intensity.unwrap(), a.operation.clone().unwrap())));
                assert_eq!(ops.len(), 6);
            } else {
                assert!(out.iter().all(|a| a.scratch_words.is_some() && a.operation.is_none()));
            }
            // Templates follow the voice.
            for a in &out {
                assert_eq!(a.template, template_for(kind, &a.voice));
            }
        }
    }

    #[test]
    fn the_same_state_and_seed_give_the_same_assignments() {
        let (s1, a1) = run([4; 32], DocKind::Mixed, 100);
        let (s2, a2) = run([4; 32], DocKind::Mixed, 100);
        assert_eq!((s1.clone(), a1.clone()), (s2, a2));
        let (_, a3) = run([5; 32], DocKind::Mixed, 100);
        assert_ne!(a1, a3);
        // A replayed draw from the same stored state gives the same result
        // and the same next state: applying it once is the store's rule.
        let m = planned();
        let lib = library();
        let s = stream_key(m.version, DocKind::Mixed, "wikipedia");
        let mut x = s1.clone();
        let mut y = s1.clone();
        let ax = assign(&mut x, &m, &lib, crate::voice::VoiceSplit::NoneHalf, &[4; 32], &s, DocKind::Mixed, "next", &Avoid::default(), &none).unwrap();
        let ay = assign(&mut y, &m, &lib, crate::voice::VoiceSplit::NoneHalf, &[4; 32], &s, DocKind::Mixed, "next", &Avoid::default(), &none).unwrap();
        assert_eq!((ax, x), (ay, y));
        // The state survives a store round trip.
        let back: StreamState = serde_json::from_str(&serde_json::to_string(&s1).unwrap()).unwrap();
        assert_eq!(back, s1);
    }

    #[test]
    fn a_new_manifest_version_starts_new_streams() {
        assert_ne!(stream_key(1, DocKind::Mixed, "wikipedia"), stream_key(2, DocKind::Mixed, "wikipedia"));
        assert_ne!(stream_key(1, DocKind::Mixed, "wikipedia"), stream_key(1, DocKind::ModelOnly, "wikipedia"));
    }

    #[test]
    fn avoidance_swaps_forward_and_keeps_the_counts() {
        let m = planned();
        let lib = library();
        let s = stream_key(m.version, DocKind::Mixed, "fineweb");
        let mut st = StreamState::default();
        let mut out = Vec::new();
        let ids = crate::manifest::fixture::lane_ids(&m, Lane::WorkersAi);
        for i in 0..(2 * 14 * 6 * 3) {
            let avoid = Avoid {
                models: vec![ids[i % ids.len()].clone(), ids[(i + 5) % ids.len()].clone()],
                voices: Vec::new(),
                operations: vec!["smooth".into()],
            };
            let a = assign(&mut st, &m, &lib, crate::voice::VoiceSplit::NoneHalf, &[6; 32], &s, DocKind::Mixed, &format!("d{i}"), &avoid, &none).unwrap();
            out.push((a, avoid));
        }
        let assigned: Vec<GenAssignment> = out.iter().map(|(a, _)| a.clone()).collect();
        check_spread(&m, DocKind::Mixed, &assigned);
        // Avoided where the block still offered another model.
        let avoided = out.iter().filter(|(a, av)| av.models.contains(&a.model)).count();
        assert!(avoided * 10 < out.len(), "{avoided}");
    }

    #[test]
    fn a_paused_vendor_falls_back_to_workers_ai_and_is_recorded() {
        let m = planned();
        let lib = library();
        let s = stream_key(m.version, DocKind::Mixed, "wikipedia");
        let mut st = StreamState::default();
        let paused = |v: Vendor| (v == Vendor::Anthropic).then_some(FallbackCause::SpendCap);
        let out: Vec<GenAssignment> =
            (0..400).map(|i| assign(&mut st, &m, &lib, crate::voice::VoiceSplit::NoneHalf, &[7; 32], &s, DocKind::Mixed, &format!("d{i}"), &Avoid::default(), &paused).unwrap()).collect();
        // The rotation still counts the frontier half; Anthropic's draws ran on Workers AI.
        check_spread(&m, DocKind::Mixed, &out);
        let moved: Vec<&GenAssignment> = out.iter().filter(|a| a.fallback.is_some()).collect();
        assert!(!moved.is_empty());
        for a in &moved {
            assert_eq!(a.lane, Lane::Frontier);
            assert_eq!(a.call_lane(), Lane::WorkersAi);
            assert_eq!(m.model(&a.model).unwrap().vendor, Vendor::WorkersAi);
            let f = a.fallback.as_ref().unwrap();
            assert_eq!((f.from, f.cause), (Vendor::Anthropic, FallbackCause::SpendCap));
            assert_eq!(m.model(&f.drawn_model).unwrap().vendor, Vendor::Anthropic);
        }
        assert!(out.iter().all(|a| m.model(&a.model).unwrap().vendor != Vendor::Anthropic));
        // Fallbacks are spread over the Workers AI models.
        let per = counts(moved.iter().map(|a| a.model.as_str()));
        assert_eq!(per.len(), 14);
        assert!(per.values().max().unwrap() - per.values().min().unwrap() <= 1);
        // Each Workers AI model's fallbacks spread over its levels equally
        // (its own fallback effort deck: within one of each other).
        for model in m.lane_models(Lane::WorkersAi) {
            let levels = model.usable_efforts();
            let efforts = counts(moved.iter().filter(|a| a.model == model.id).map(|a| a.effort.code()));
            let got: Vec<usize> = levels.iter().map(|e| efforts.get(e.code()).copied().unwrap_or(0)).collect();
            assert!(got.iter().max().unwrap() - got.iter().min().unwrap() <= 1, "{} {got:?}", model.id);
        }
    }

    #[test]
    fn retries_stay_in_the_lane_and_skip_disabled_cells() {
        let m = planned();
        let (_, out) = run([8; 32], DocKind::Mixed, 40);
        for a in &out {
            let mut tried = vec![a.model.clone()];
            for attempt in 1..30u32 {
                let (model, effort) = retry_cell(&m, &[8; 32], a, "k", attempt, &tried, &|id, e| id == "gpt-6-astra" && e == Effort::Max).unwrap();
                assert_eq!(m.model(&model).unwrap().lane, a.call_lane());
                assert!(m.model(&model).unwrap().usable_efforts().contains(&effort));
                assert!(!(model == "gpt-6-astra" && effort == Effort::Max));
                let lane_n = m.lane_models(a.call_lane()).len();
                if tried.len() < lane_n {
                    assert!(!tried.contains(&model), "repeat before exhaustion");
                }
                tried.push(model);
            }
            assert_eq!(
                retry_cell(&m, &[8; 32], a, "k", 3, &[], &|_, _| false),
                retry_cell(&m, &[8; 32], a, "k", 3, &[], &|_, _| false)
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]
        /// Per-cohort domain counts of any size (1-document domains
        /// included): the guarantees hold on the stream across cohorts.
        #[test]
        fn small_domains_keep_the_guarantees_across_cohorts(seed in any::<[u8; 32]>(), cohorts in proptest::collection::vec(1usize..9, 1..60)) {
            let m = planned();
            let lib = library();
            let s = stream_key(m.version, DocKind::Mixed, "small");
            let mut stored = serde_json::to_string(&StreamState::default()).unwrap();
            let mut out = Vec::new();
            for (ci, g) in cohorts.iter().enumerate() {
                // Each cohort loads the stored state and stores it back.
                let mut st: StreamState = serde_json::from_str(&stored).unwrap();
                for k in 0..*g {
                    out.push(assign(&mut st, &m, &lib, crate::voice::VoiceSplit::NoneHalf, &seed, &s, DocKind::Mixed, &format!("c{ci}-{k}"), &Avoid::default(), &none).unwrap());
                }
                stored = serde_json::to_string(&st).unwrap();
            }
            check_spread(&m, DocKind::Mixed, &out);
        }
    }
}
