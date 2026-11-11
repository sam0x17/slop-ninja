//! The `reward-v3` golden report: the hand-worked q-only examples under
//! both band rules, the computed cohort finalized under `reward-v3` (on its
//! `quant-v1` grid and on a `quant-v2` grid), package walk scenarios, ties,
//! promotion and the rule's record tag. It is compared byte for byte against
//! `fixtures/golden-reward-v3.txt` on native targets and inside a
//! `wasm32-unknown-unknown` build.

use std::fmt::Write as _;

use crate::codec::{Canonical, Decoder, Encoder};
use crate::fixtures_reward_v1::{self as fx, FIXTURE_QUANT, Spec, bp};
use crate::quant;
use crate::reward_v1::{Placement, QEntrant, QInput, QOutcome, q_cutoff, q_outcome};
use crate::{
    CutoffMode, QuantBlock, QuantParamsV2, Record, Result, RewardRule, RewardV1Params, SCORE_SCALE,
    ScorePackageV2,
};

/// One hundredth of a score.
pub const HUNDREDTH: u64 = SCORE_SCALE / 100;

/// The hand-worked examples' parameters: `w = 0.01`, `sigma = 0.02`, so
/// `delta = 0.04` and `W_q = 0.01`.
pub fn example_params() -> RewardV1Params {
    RewardV1Params::new(SCORE_SCALE / 50, crate::fixtures_quant::TESTNET_QUANT)
}

/// A q-only roster entry: its `q` in hundredths and whether it is gated.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub q: Option<u64>,
    pub gated: bool,
}

pub fn gated(hundredths: u64) -> Entry {
    Entry { q: Some(hundredths * HUNDREDTH), gated: true }
}

/// The labels each rule's walk publishes, with the near-copy relation
/// `near(head, other)` (identical content is a near relation here): the
/// walk over the entrants at or above the `reward-v2` cutoff in q-rank order
/// (`reward-v2`), or over every gated entrant in roster order (`reward-v3`);
/// each cluster labelled by its first member in roster order.
pub fn walk_labels(
    params: &RewardV1Params,
    width: u64,
    reference_q: u64,
    predecessor_q: Option<u64>,
    entries: &[Entry],
    near: impl Fn(usize, usize) -> bool,
    placement: Placement,
) -> Result<Vec<QEntrant>> {
    let bare: Vec<QEntrant> = entries.iter().map(|e| QEntrant { q: e.q, gated: e.gated, cluster: None }).collect();
    let order: Vec<usize> = match placement {
        Placement::QRankHead => {
            let input = QInput { params, width, reference_q, predecessor_q, entrants: &bare, placement };
            q_cutoff(&input)?.2
        }
        Placement::RosterFirst => (0..entries.len()).filter(|&i| entries[i].gated && entries[i].q.is_some()).collect(),
    };
    let mut head_of: Vec<Option<usize>> = vec![None; entries.len()];
    for (pos, &h) in order.iter().enumerate() {
        if head_of[h].is_some() {
            continue;
        }
        head_of[h] = Some(h);
        for &j in &order[pos + 1..] {
            if head_of[j].is_none() && near(h, j) {
                head_of[j] = Some(h);
            }
        }
    }
    let mut out = bare;
    for (i, head) in head_of.iter().enumerate() {
        if let Some(h) = head {
            let first = (0..entries.len()).find(|&k| head_of[k] == Some(*h)).expect("member");
            out[i].cluster = Some(first as u16);
        }
    }
    Ok(out)
}

/// The q-only outcome of a hand-worked example under `placement`.
pub fn example_outcome(
    entries: &[Entry],
    reference: u64,
    predecessor: Option<u64>,
    near: impl Fn(usize, usize) -> bool,
    placement: Placement,
) -> Result<QOutcome> {
    let params = example_params();
    let (w, r, p) = (HUNDREDTH, reference * HUNDREDTH, predecessor.map(|p| p * HUNDREDTH));
    let entrants = walk_labels(&params, w, r, p, entries, near, placement)?;
    q_outcome(&QInput { params: &params, width: w, reference_q: r, predecessor_q: p, entrants: &entrants, placement })
}

/// Pairwise near-copy groups: entrants in the same group are within `d`.
pub fn groups<'a>(groups: &'a [&'a [usize]]) -> impl Fn(usize, usize) -> bool + 'a {
    move |a, b| groups.iter().any(|g| g.contains(&a) && g.contains(&b))
}

struct Lines(String);

impl Lines {
    fn line(&mut self, key: &str, value: impl core::fmt::Display) {
        let _ = writeln!(self.0, "{key}={value}");
    }

    fn outcome(&mut self, key: &str, o: &QOutcome) {
        let clusters: Vec<String> = o
            .clusters
            .iter()
            .map(|c| format!("{}:{:?}@k{}", c.head, c.members, c.band_rank))
            .collect();
        self.line(
            key,
            format_args!(
                "{:?} cutoff={} clusters=[{}] units={:?}",
                o.mode,
                o.cutoff.map_or("none".into(), |c| c.to_string()),
                clusters.join(" "),
                o.units
            ),
        );
    }

    fn package(&mut self, key: &str, p: &ScorePackageV2) -> Result<()> {
        let heads: Vec<String> = p
            .entrants
            .iter()
            .map(|e| match e.cluster_head {
                Some(h) => format!("{:02x}>{:02x}:{}", e.identity.hotkey.0[0], h.0[0], e.units),
                None => format!("{:02x}:-:{}", e.identity.hotkey.0[0], e.units),
            })
            .collect();
        self.line(
            key,
            format_args!(
                "{:?} cutoff={} promoted={} entrants={}",
                p.mode,
                p.quant.cutoff().map_or("none".into(), |c| c.get().to_string()),
                p.promoted.map_or("none".into(), |h| format!("{:02x}", h.0[0])),
                heads.join(",")
            ),
        );
        Ok(())
    }
}

/// `spec` with more attributable failures than the fixtures' `k_n`.
fn withheld(mut spec: Spec) -> Spec {
    spec.failed = fx::CASES;
    spec
}

pub fn golden_report() -> Result<String> {
    let mut r = Lines(String::new());
    r.line("schema", "slop_ninja.protocol.golden/reward-v3");
    let rules = [(Placement::QRankHead, "v2"), (Placement::RosterFirst, "v3")];
    // E1: late lucky copies. A1 (0), A2 (2) and A3 (3) are within d.
    let e1 = [gated(90), gated(92), gated(95), gated(93), gated(91)];
    let e1_near: &[&[usize]] = &[&[0, 2, 3]];
    for (placement, tag) in rules {
        r.outcome(&format!("e1.{tag}"), &example_outcome(&e1, 90, None, groups(e1_near), placement)?);
    }
    // E1 with A1 at 0.93: party A's cluster is retained at 0.93.
    let e1b = [gated(93), gated(92), gated(95), gated(93), gated(91)];
    for (placement, tag) in rules {
        r.outcome(&format!("e1b.{tag}"), &example_outcome(&e1b, 90, None, groups(e1_near), placement)?);
    }
    // E2: mode flip, with and without a predecessor.
    let e2 = [gated(90), gated(92), gated(89)];
    let e2_near: &[&[usize]] = &[&[0, 1]];
    for (placement, tag) in rules {
        r.outcome(&format!("e2.{tag}"), &example_outcome(&e2, 90, Some(88), groups(e2_near), placement)?);
        r.outcome(&format!("e2k0.{tag}"), &example_outcome(&e2, 90, None, groups(e2_near), placement)?);
    }
    // E3: the fallback's max comes from heads.
    let e3 = [gated(82), gated(90), gated(85)];
    for (placement, tag) in rules {
        r.outcome(&format!("e3.{tag}"), &example_outcome(&e3, 90, Some(80), groups(e2_near), placement)?);
    }
    // E4: no chaining in roster order (A~B, B~C, not A~C).
    let e4 = [gated(95), gated(94), gated(93)];
    let chain = |a: usize, b: usize| a.abs_diff(b) == 1;
    r.outcome("e4.abc.v3", &example_outcome(&e4, 90, None, chain, Placement::RosterFirst)?);
    let e4r = [gated(93), gated(94), gated(95)];
    r.outcome("e4.cba.v3", &example_outcome(&e4r, 90, None, chain, Placement::RosterFirst)?);
    // q_Rk = 1: no target, membership still published.
    let top = [gated(100), gated(99)];
    for (placement, tag) in rules {
        let o = example_outcome(&top, 100, Some(99), groups(&[&[0, 1]]), placement)?;
        r.outcome(&format!("top.{tag}"), &o);
    }
    // Ties: equal head q in one band, and a lone cluster below a crowded
    // band, placed by its head.
    let ties = [gated(93), gated(93), gated(96), gated(93), gated(91)];
    for (placement, tag) in rules {
        r.outcome(&format!("ties.{tag}"), &example_outcome(&ties, 90, None, groups(&[&[1, 2]]), placement)?);
    }
    // The computed cohort under reward-v3, on its quant-v1 grid and on a
    // quant-v2 grid.
    for (name, quant) in [
        ("quant_v1", QuantBlock::V1(FIXTURE_QUANT)),
        (
            "quant_v2",
            QuantBlock::V2(QuantParamsV2 {
                noise_target_h_milli: 70,
                rho_max_milli: 500,
                headroom_pub_ppb: 150_000_000,
                identify_bits: FIXTURE_QUANT.identify_bits,
                budget_factor: FIXTURE_QUANT.budget_factor,
                min_success_max_failed: fx::CASES - 1,
            }),
        ),
    ] {
        let (m2, p2) = fx::computed_package(RewardRule::RewardV2, quant)?;
        let (m3, p3) = fx::computed_package(RewardRule::RewardV3, quant)?;
        r.package(&format!("computed.{name}.v2"), &p2)?;
        r.package(&format!("computed.{name}.v3"), &p3)?;
        let result = fx::result_of(&p3)?;
        r.line(&format!("computed.{name}.v3.manifest"), m3.digest()?);
        r.line(&format!("computed.{name}.v3.package"), p3.digest()?);
        r.line(&format!("computed.{name}.v3.result"), result.digest()?);
        r.line(&format!("computed.{name}.v2.manifest"), m2.digest()?);
        let same = p2.entrants.iter().zip(&p3.entrants).filter(|(a, b)| a.units == b.units).count();
        r.line(&format!("computed.{name}.units_equal"), format_args!("{same}/{}", p3.entrants.len()));
    }
    // Package walk scenarios at 1,000 cases (R_k 0.91, R_k-1 0.90).
    let scenarios: Vec<(&str, Vec<Spec>)> = vec![
        // Identical content under two hotkeys: roster-first placement, one draw.
        ("twins", vec![Spec::new(0x41, bp(9500)), Spec::new(0x42, bp(9300)), Spec::new(0x43, bp(9600)).copy_of(0x41)]),
        // The roster-first copy fails a gate: the next copy heads.
        (
            "first_copy_gated_out",
            vec![
                Spec::new(0x44, bp(9500)).failing(crate::Gate::Share, "share.mae_above_threshold"),
                Spec::new(0x45, bp(9400)).near(0x44),
                Spec::new(0x46, bp(9300)),
            ],
        ),
        // The roster-first copy is withheld: the next copy heads.
        (
            "first_copy_withheld",
            vec![
                withheld(Spec::new(0x47, bp(9500))),
                Spec::new(0x48, bp(9400)).near(0x47),
                Spec::new(0x49, bp(9300)),
            ],
        ),
        // A member above its head's band, and a member below the cutoff,
        // both paid with the cluster.
        (
            "members_outside_band",
            vec![
                Spec::new(0x4a, bp(9400)),
                Spec::new(0x4b, bp(9800)).near(0x4a),
                Spec::new(0x4c, bp(9000)).near(0x4a),
                Spec::new(0x4d, bp(9600)),
            ],
        ),
    ];
    for (name, specs) in &scenarios {
        for (rule, tag) in [(RewardRule::RewardV2, "v2"), (RewardRule::RewardV3, "v3")] {
            let p = fx::example_under(rule, bp(9100), Some(bp(9000)), specs)?;
            r.package(&format!("scenario.{name}.{tag}"), &p)?;
        }
    }
    // Promotion: a retained head meets the margin while a later member has
    // a higher exact score; the head is promoted and the member never
    // audited.
    let promo = vec![Spec::new(0x51, bp(9700)), Spec::new(0x52, bp(9900)).near(0x51)];
    for (rule, tag) in [(RewardRule::RewardV2, "v2"), (RewardRule::RewardV3, "v3")] {
        let p = fx::example_under(rule, bp(9100), Some(bp(9000)), &promo)?;
        let audited: Vec<String> = p.promotion_audits.iter().map(|a| format!("{:02x}", a.head.0[0])).collect();
        r.package(&format!("promotion.{tag}"), &p)?;
        r.line(&format!("promotion.{tag}.audited"), audited.join(","));
    }
    // The record tag.
    let mut e = Encoder::new();
    RewardRule::RewardV3.encode_to(&mut e);
    let bytes = e.into_bytes();
    r.line("reward_rule.v3", format_args!("tag={} name={}", bytes[0], RewardRule::RewardV3.name()));
    r.line(
        "reward_rule.tag5",
        format_args!("{:?}", RewardRule::decode_from(&mut Decoder::new(&[5])).map(|_| ())),
    );
    r.line(
        "launch_burn.rule",
        crate::CalculationTargetV2::launch_burn(fx::chain()).reward_rule.name(),
    );
    let _ = (quant::QUANT_RULE, CutoffMode::NoTarget);
    Ok(r.0)
}
