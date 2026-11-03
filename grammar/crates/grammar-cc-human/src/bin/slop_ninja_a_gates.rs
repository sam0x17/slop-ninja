//! `slop_ninja_a_gates`: the data-gate tool for A bundles (protocol A2 build
//! B12). Steps:
//! * `gates`: G0a, G0b or G0c per block and per Tier domain (the layout and
//!   typography, era-only and word-count-only adversaries, with MWL and the
//!   character-class block as diagnostics), in Brier units, bounded at
//!   0.05 × B0_ref when B0_ref is given;
//! * `audit`: the layout audit `r2_layout_audit_v1` (counts only), with
//!   amendment 10 item 10.12's and amendment 13 item 13.10's "before"
//!   figures and the source-level stop invariants (amendment 14);
//! * `sigma`: σ, δ and w from three seeds by the cohort-mix method (the
//!   stratified bootstrap SE, B = 2,000, seed 20261030; amendment 12 items
//!   12.3 and 12.4), with the Kish ESS and the named weight field (σ′ of
//!   item 1.13 at n = 1,000; σ_C at n = 4,000);
//! * `pilot-gate`: `r2_pilot_gate_v1` on one D-item's keyed pilot, with the
//!   before scope computed on R0's Train rows (amendment 12 item 12.1);
//! * `paired-test`: the pilot gate's paired child − parent tests alone,
//!   reported without a verdict (G0a's Stage-1 layout-audit report).
//!
//! `slop_ninja_cc_era gates` is unchanged. No model, detector or network
//! call. Outputs hold counts, statistics and digests, never text.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_cc_human::a_gates::{self, AdvConfig, Block, Gate, WeightField};
use grammar_cc_human::{layout_audit, pilot_gate};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "slop_ninja_a_gates")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args, Clone)]
struct Adv {
    /// Fold seed (the CC gates' default).
    #[arg(long, default_value = a_gates::DEFAULT_FOLD_SEED)]
    seed: String,
    #[arg(long, default_value_t = 5)]
    folds: usize,
    #[arg(long, default_value_t = 5)]
    cv_repeats: usize,
    /// Bootstrap draws for the adversary gains' reported interval.
    #[arg(long, default_value_t = 1000)]
    draws: usize,
}

impl Adv {
    fn config(&self) -> AdvConfig {
        AdvConfig { seed: self.seed.clone(), folds: self.folds, cv_repeats: self.cv_repeats, draws: self.draws }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// G0a, G0b or G0c on A rows (gates.json).
    Gates {
        #[arg(long)]
        rows: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// The CC pilot pool for the era probe (needed for the era adversary).
        #[arg(long)]
        pool: Option<PathBuf>,
        /// B0_ref of the T block (decimal, or f64 bits as 0x…); absent: Brier units only.
        #[arg(long)]
        b0_ref_t: Option<String>,
        /// B0_ref of the C block (decimal, or f64 bits as 0x…).
        #[arg(long)]
        b0_ref_c: Option<String>,
        /// G0a (default), G0b or G0c.
        #[arg(long, default_value = "G0a")]
        gate: String,
        #[arg(long, default_value = a_gates::DEFAULT_PROBE_SEED)]
        probe_seed: String,
        #[arg(long, default_value_t = 3000)]
        per_domain_class: usize,
        #[command(flatten)]
        adv: Adv,
    },
    /// The layout audit r2_layout_audit_v1 (layout-audit.json).
    Audit {
        #[arg(long)]
        rows: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// σ, δ and w from three seeds (sigma.json).
    Sigma {
        #[arg(long)]
        rows: PathBuf,
        /// NAME=predictions.jsonl, exactly three times: seeds 17, 29, 41 in order.
        #[arg(long = "pred", required = true)]
        preds: Vec<String>,
        #[arg(long)]
        out: PathBuf,
        /// Batch size; repeat for several (1000 for σ′, 4000 for σ_C).
        #[arg(long = "n", default_values_t = [1000usize])]
        ns: Vec<usize>,
        /// Origin mix human_only,mixed,model_only.
        #[arg(long, default_value = "0.20,0.55,0.25")]
        mix: String,
        /// Bootstrap replicates (pinned: 2000).
        #[arg(long, default_value_t = a_gates::SIGMA_DRAWS)]
        draws: usize,
        /// Bootstrap seed (pinned: 20261030).
        #[arg(long, default_value_t = a_gates::SIGMA_SEED)]
        seed: u64,
        /// Restrict to one block (T or C).
        #[arg(long)]
        block: Option<String>,
        /// The row-weight field (e.g. cohort_weight); every row must carry it.
        /// Absent: `weight`, 1 when a row has none.
        #[arg(long)]
        weight_field: Option<String>,
        /// The manifest's σ_m on its 1e-18 grid (for σ_eff = max(σ_m, σ′)).
        #[arg(long)]
        sigma_m_scaled: Option<u64>,
    },
    /// r2_pilot_gate_v1 on one D-item's keyed pilot (pilot-gate.json).
    PilotGate {
        /// Pilot children (JSONL: id, origin, parent_id, text, source or
        /// source_kind, spans; domain and source_group default to the parent's).
        #[arg(long)]
        pilot: PathBuf,
        /// Rows holding the parents (id, text; domain and source_group checked when present).
        #[arg(long)]
        parents: PathBuf,
        /// A rows holding the humans of the pilot's domains (R0's raw Train at Stage 0).
        #[arg(long)]
        humans: PathBuf,
        /// R0's Train rows holding the before scopes' positives.
        #[arg(long)]
        before_rows: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[command(flatten)]
        adv: Adv,
    },
    /// The paired child - parent tests alone, without a verdict (paired-test.json).
    PairedTest {
        #[arg(long)]
        pilot: PathBuf,
        #[arg(long)]
        parents: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

fn parse_b0(s: Option<String>) -> Result<Option<f64>> {
    s.map(|s| match s.strip_prefix("0x") {
        Some(h) => Ok(f64::from_bits(u64::from_str_radix(h, 16).context("B0_ref bits")?)),
        None => s.parse::<f64>().context("B0_ref"),
    })
    .transpose()
}

fn main() -> Result<()> {
    let out = match Cli::parse().cmd {
        Cmd::Gates { rows, out, pool, b0_ref_t, b0_ref_c, gate, probe_seed, per_domain_class, adv } => a_gates::gates(&a_gates::GatesArgs {
            rows,
            pool,
            b0_ref_t: parse_b0(b0_ref_t)?,
            b0_ref_c: parse_b0(b0_ref_c)?,
            gate: Gate::parse(&gate)?,
            out,
            cfg: adv.config(),
            probe_seed,
            per_domain_class,
        })?,
        Cmd::Audit { rows, out } => layout_audit::audit(&layout_audit::AuditArgs { rows, out })?,
        Cmd::Sigma { rows, preds, out, ns, mix, draws, seed, block, weight_field, sigma_m_scaled } => {
            let preds = preds
                .iter()
                .map(|s| {
                    let (k, v) = s.split_once('=').context("--pred NAME=PATH")?;
                    Ok((k.to_string(), PathBuf::from(v)))
                })
                .collect::<Result<Vec<_>>>()?;
            let m: Vec<f64> = mix.split(',').map(|x| x.trim().parse::<f64>().context("--mix")).collect::<Result<_>>()?;
            ensure!(m.len() == 3, "--mix takes three shares");
            a_gates::sigma(&a_gates::SigmaArgs {
                rows,
                preds,
                block: block.as_deref().map(Block::parse).transpose()?,
                weight_field: WeightField::parse(weight_field.as_deref())?,
                opts: a_gates::SigmaOpts { ns, mix: [m[0], m[1], m[2]], draws, seed, sigma_m_scaled },
                out,
            })?
        }
        Cmd::PilotGate { pilot, parents, humans, before_rows, out, adv } => {
            pilot_gate::pilot_gate(&pilot_gate::PilotArgs { pilot, parents, humans, before_rows, cfg: adv.config(), out })?
        }
        Cmd::PairedTest { pilot, parents, out } => pilot_gate::paired_test(&pilot_gate::PairedArgs { pilot, parents, out })?,
    };
    println!("{out}");
    Ok(())
}
