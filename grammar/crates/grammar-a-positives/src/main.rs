use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_a_positives::{CODE_ID, briefs, diverse, enron_half, finalize, form_post, generation, library, mirror_regen, order, overlap, pilot, plan, renorm, span_ops, split_copy, stratify, text, values};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Parser)]
#[command(name = "slop_ninja_a_positives")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build the mechanical request plan for Train, Dev-iter, Dev-select and CAL.
    Plan {
        #[arg(long)]
        train: PathBuf,
        #[arg(long)]
        dev_iter: PathBuf,
        #[arg(long)]
        dev_select: PathBuf,
        #[arg(long)]
        cal: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Build the Test plan (primary cycle with OLMo-2 instruct in slots 0-6
    /// and the OLMo-2 base robustness slice). Refuses unless the written seal
    /// exists and its SHA-256 matches --seal-sha256.
    PlanTest {
        /// Train documents (IDF source only: the domain's Train pool).
        #[arg(long)]
        train: PathBuf,
        /// Sealed-then-acquired Test human documents.
        #[arg(long)]
        test: PathBuf,
        #[arg(long)]
        seal: PathBuf,
        #[arg(long)]
        seal_sha256: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Re-plan an existing plan directory from its recorded inputs and
    /// compare items, plan failures and request files byte for byte.
    ReplanCheck {
        /// Existing plan directory (plan.json schema v1, v2 or v3).
        #[arg(long)]
        plan: PathBuf,
        /// Plan v2 only: style library and fix-slop bundle used for it.
        #[arg(long)]
        library: Option<PathBuf>,
        #[arg(long)]
        fix_slop: Option<PathBuf>,
        /// Scratch directory for the new plan (must not exist).
        #[arg(long)]
        out: PathBuf,
    },
    /// Run one generator's pending requests against a running llama-server.
    Generate {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        generator: String,
        #[arg(long)]
        config: PathBuf,
        /// Root holding one run directory per generator.
        #[arg(long)]
        runs: PathBuf,
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        tokenizer: PathBuf,
        #[arg(long, default_value_t = 4)]
        workers: usize,
        #[arg(long)]
        limit: Option<usize>,
        /// Comma-separated splits to include (default: all the config allows).
        #[arg(long)]
        splits: Option<String>,
        /// Further run roots whose accepted texts are dedup priors only.
        #[arg(long)]
        extra_runs: Vec<PathBuf>,
        /// Comma-separated item cohorts to run (default: all).
        #[arg(long)]
        cohorts: Option<String>,
        /// Item-id list (one per line) that restricts pending requests to the
        /// listed items and runs them in list order; `{GEN}` in the path is
        /// replaced by the generator key. Every listed item must have a
        /// request in the selected splits/cohorts.
        #[arg(long)]
        request_order: Option<PathBuf>,
        /// Report the selection without contacting the server or generating.
        #[arg(long)]
        dry_run: bool,
        /// Form profile in the acceptance path: `cc_cohort_profile_v2`
        /// (protocol A2 §6.2) or `raw` (the default: no post-processing).
        #[arg(long)]
        profile: Option<String>,
        /// Round-2 population id of a `cc_cohort_profile_v2` run (its seed and
        /// prior); the run selects exactly one split.
        #[arg(long)]
        profile_population: Option<String>,
    },
    /// Write per-generator Train item-order files (domain/cohort/bin stratified) for `generate --request-order`.
    StratifyTrain {
        #[arg(long)]
        plan: PathBuf,
        /// Run root of `plan` (pending/accepted state).
        #[arg(long)]
        runs: PathBuf,
        /// Earlier PLAN_DIR=RUNS_DIR pairs whose accepted Train items count as existing positives.
        #[arg(long)]
        prior: Vec<String>,
        /// Frozen style partitions (JSONL with id and partition).
        #[arg(long)]
        partitions: PathBuf,
        /// GENERATOR=BUDGET, repeated.
        #[arg(long = "generator")]
        generators: Vec<String>,
        /// GENERATOR, repeated: a target of v_fam accepted Train items (D7;
        /// protocol A2 section 6.4), v_fam read from --values (S0-7); the
        /// budget is the target less the generator's existing accepted Train
        /// items (this plan's runs and --prior). Only Train lines of every
        /// plan and journal are parsed.
        #[arg(long = "target")]
        targets: Vec<String>,
        /// VALUES-R2.json, whose S0-7 (v_fam) --target reads.
        #[arg(long)]
        values: Option<PathBuf>,
        /// S0-10 (B5b part 2): write the base-cohort stratification's
        /// register entry (values.S0-10) of this --target selection to this
        /// new file, and no order file; a later A9 step writes it into
        /// VALUES-R2.json.
        #[arg(long)]
        write_stratification: Option<PathBuf>,
        /// D7's step (B5b part 2): per-domain quotas from --values' S0-10
        /// (refused while absent, or when its inputs differ from this run's).
        #[arg(long)]
        stratification: bool,
        /// Per-domain floor of new items (bounded by availability); 60 when
        /// absent (the tool's default, not a protocol value). S0-10's
        /// modes take floor 0 and refuse any other value (amendment 27).
        #[arg(long)]
        floor: Option<usize>,
        /// COHORT=SHARE list; items of unlisted cohorts are not selected.
        #[arg(long)]
        cohort_shares: String,
        /// Comma-separated domains placed early in each order file.
        #[arg(long, default_value = "")]
        priority_domains: String,
        /// DOMAIN=MULTIPLIER list applied to Train human counts for quotas.
        #[arg(long, default_value = "")]
        domain_weights: String,
        #[arg(long)]
        seed: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Assemble accepted responses into per-split records and accounting.
    Finalize {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        runs: PathBuf,
        #[arg(long)]
        configs: PathBuf,
        #[arg(long)]
        tokenizer: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Re-verdict one batch of a plan's journals under profile v2, with no
    /// model call (protocol A2 §4.3; B4): a local plan's split (`--split`)
    /// or a spliced plan's population (`--population`). Writes a new
    /// directory holding the batch's binding and prior record, the
    /// re-verdict journals (resumable under `generate --profile`), the
    /// per-item re-verdict records and the regeneration plan keyed by
    /// request id.
    Renorm {
        #[arg(long)]
        plan: PathBuf,
        /// Root holding one run directory per generator.
        #[arg(long)]
        runs: PathBuf,
        /// Local plans: the split whose items form the batch.
        #[arg(long)]
        split: Option<String>,
        /// Spliced plans: the plan population whose items form the batch.
        #[arg(long)]
        population: Option<String>,
        /// `cc_cohort_profile_v2` (the only profile renorm applies).
        #[arg(long)]
        profile: String,
        /// Round-2 population id: the prior's seed and population.
        #[arg(long)]
        profile_population: String,
        /// Local plans: the A tokenizer of the token limit.
        #[arg(long)]
        tokenizer: Option<PathBuf>,
        /// Local plans: R0's records of the batch (one item id per line);
        /// the regeneration plan covers only them.
        #[arg(long)]
        records: Option<PathBuf>,
        /// New output directory.
        #[arg(long)]
        out: PathBuf,
    },
    /// Re-verdict and normalise RAID, stage-3 legacy and hosted-capture Train
    /// rows under profile v2 (RAID humans, recorded or recovered parents or
    /// layout donors), with no model call; failures are dropped with a
    /// receipt.
    RenormRows {
        /// Bundle rows: only lines whose `split` is train are parsed.
        #[arg(long)]
        rows: PathBuf,
        /// Layout donors (bundle rows; `human_only` with weight > 0); the
        /// rows file when omitted.
        #[arg(long)]
        donors: Option<PathBuf>,
        /// The RAID pool documents the assembler read (needed when the rows
        /// hold RAID rows): the RAID humans and their attack names.
        #[arg(long)]
        raid_documents: Option<PathBuf>,
        /// Stop after the plan: write only `ROWS-PLAN.json` (counts by
        /// class and layout source or drop reason; no layout or verdict).
        #[arg(long)]
        plan_only: bool,
        #[arg(long)]
        profile: String,
        #[arg(long)]
        profile_population: String,
        /// New output directory.
        #[arg(long)]
        out: PathBuf,
    },
    /// Invent and review style briefs with one inventor model (running llama-server).
    Briefs {
        #[arg(long)]
        config: PathBuf,
        /// Train documents (anchor passages; Train only).
        #[arg(long)]
        train: PathBuf,
        /// Library root holding one journal directory per inventor.
        #[arg(long)]
        root: PathBuf,
        #[arg(long, default_value_t = 56)]
        target: usize,
        #[arg(long, default_value_t = 170)]
        max_draws: usize,
        #[arg(long, default_value_t = 4)]
        workers: usize,
    },
    /// Compile accepted/rejected briefs, split partitions and diversity report.
    BriefLibrary {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Build plan v2 (protocol primary on evaluation splits plus diverse cohorts).
    PlanV2 {
        #[arg(long)]
        train: PathBuf,
        #[arg(long)]
        dev_iter: PathBuf,
        #[arg(long)]
        dev_select: PathBuf,
        #[arg(long)]
        cal: PathBuf,
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        fix_slop: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Build plan v3: the additive exact-span supervision cohorts
    /// (prefix continuations and interior-run rewrites), or with
    /// --base-generators the Train base-model cohort. Not run by default.
    PlanV3 {
        #[arg(long)]
        train: PathBuf,
        /// Optional inputs: only the given ones are read (hashed and loaded,
        /// here and by `generate`/`finalize` on the plan); a planned split
        /// needs its input. Dev-iter is read only as a planned split and is
        /// refused in base-model mode.
        #[arg(long)]
        dev_iter: Option<PathBuf>,
        /// Refused before any file is opened: old Dev-select is never read,
        /// scored or parsed (protocol A2 section 2.4), and no v3 split uses it.
        #[arg(long)]
        dev_select: Option<PathBuf>,
        /// CAL humans, read only as `generate`'s overlap extras (dedup).
        #[arg(long)]
        cal: Option<PathBuf>,
        /// Comma-separated splits to plan: train, optionally dev_iter (supplementary, ungated).
        #[arg(long, default_value = "train")]
        splits: String,
        /// A human gets a prefix continuation when rank(k_prefix) mod N == 0 (default 2; instruct mode only).
        #[arg(long)]
        prefix_every: Option<usize>,
        /// A human gets a middle rewrite when rank(k_middle) mod N == 0 (default 2; instruct mode only).
        #[arg(long)]
        middle_every: Option<usize>,
        /// Base-model mode: comma-separated Train base families (granite_base,
        /// smollm3_base, ernie_base). Plans one `train_base_v3` item per Train
        /// human (raw completion, Tier C continuation and splice rules) and no
        /// instruct cohort.
        #[arg(long, default_value = "")]
        base_generators: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Library v2 session: cross-family review of pending candidates, then timed invention (running llama-server).
    LibrarySession {
        #[arg(long)]
        config: PathBuf,
        /// Train documents (anchor passages; Train only).
        #[arg(long)]
        train: PathBuf,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        round: u64,
        #[arg(long, default_value_t = 9.0)]
        minutes: f64,
        #[arg(long, default_value_t = 4)]
        workers: usize,
        /// Goal for INVENTED voice prompts (accepted + 0.7 x pending).
        #[arg(long, default_value_t = 7000)]
        voice_goal: usize,
        /// Goal for INVENTED briefs (accepted + 0.7 x pending).
        #[arg(long, default_value_t = 6000)]
        brief_goal: usize,
        #[arg(long)]
        review_only: bool,
        /// Human-derived sample (sample.jsonl from human-sample); this inventor describes its assigned rows.
        #[arg(long)]
        hd_sample: Option<PathBuf>,
    },
    /// Build the human-derived description sample from Train and Train-mining.
    HumanSample {
        #[arg(long)]
        train: PathBuf,
        #[arg(long)]
        train_mining: PathBuf,
        /// Comma-separated describer inventor keys (rotation order).
        #[arg(long)]
        describers: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Compile library v2: accepted/rejected, partitions, breadth and author conflicts.
    LibraryCompile {
        #[arg(long)]
        root: PathBuf,
        /// Evaluation reservation manifests (metadata only) as split=path.
        #[arg(long)]
        eval_manifest: Vec<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Style breadth of accepted outputs (function-word/punctuation profiles) by style kind, generator and cohort.
    OutputBreadth {
        /// Run roots as label=path (each holds one directory per generator).
        #[arg(long)]
        runs: Vec<String>,
        /// Plan directories whose items.jsonl label the runs (any order).
        #[arg(long)]
        plan: Vec<PathBuf>,
        /// Train human documents, profiled per genre as a reference.
        #[arg(long)]
        humans: Option<PathBuf>,
        #[arg(long, default_value_t = 1500)]
        cap: usize,
        #[arg(long)]
        out: PathBuf,
    },
    /// Records/minute per generator and attempt accounting from journals.
    Rates {
        #[arg(long)]
        runs: PathBuf,
    },
    /// Write a one-split copy of a local plan (B5b; ruling R2): every line of
    /// its items, request and plan-failure files is selected by the split read
    /// from its bytes before any parse, and copied byte for byte. `dev_select`
    /// and `test` are refused as the copy's split. The source's CAL input is
    /// kept as generate's overlap extra (R0's screen; amendment 24).
    SplitCopy {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        split: String,
        /// New output directory.
        #[arg(long)]
        out: PathBuf,
    },
    /// D4's plan (B5b; ruling R6): renorm's regenerate_mirror rows as new
    /// requests at mirror_target_words_v2 with their remaining attempts, in
    /// renorm's batch, with D4's request-order files.
    MirrorPlan {
        /// A plan whose files hold renorm's split only (a split copy).
        #[arg(long)]
        plan: PathBuf,
        /// Renorm's output directory for the split.
        #[arg(long)]
        renorm: PathBuf,
        #[arg(long)]
        split: String,
        /// VALUES-R2.json, whose S0-6 (r05) the new requests' target reads.
        #[arg(long)]
        values: PathBuf,
        /// New output directory.
        #[arg(long)]
        out: PathBuf,
    },
    /// Keyed pilot prefix of a D-step (B5b; V22, S0-11): pilot and bulk
    /// request-order files and PILOT.json. A spliced plan (D8) takes
    /// --population and --renorm: its rows are renorm's regenerate items.
    Pilot {
        #[arg(long)]
        plan: PathBuf,
        /// The D-step's `generate --request-order` files, with `{GEN}`
        /// (default: every item of the plan). Local plans only.
        #[arg(long)]
        request_order: Option<String>,
        /// D8: the spliced plan population.
        #[arg(long)]
        population: Option<String>,
        /// D8: renorm's spliced output for the population.
        #[arg(long)]
        renorm: Option<PathBuf>,
        /// The D-step's name (D4 ... D7).
        #[arg(long)]
        step: String,
        /// New output directory.
        #[arg(long)]
        out: PathBuf,
    },
    /// The Train-only planner for new mirrors (B5b part 2 item 1): one mirror
    /// per Train human of --train at mirror_target_words_v2 (r05 from
    /// --values' S0-6), the seven Train instruct families. With --topup it is
    /// D5's local part: --train is the top-up plan's parents.jsonl and the
    /// family quotas are its own. There is no --dev-select.
    TrainMirrorPlan {
        /// The Train input (frozen_split train only: a line of another split,
        /// or of none, refuses the plan from its bytes, before any parse).
        #[arg(long)]
        train: PathBuf,
        /// VALUES-R2.json, whose S0-6 (r05) the requests' length reads; with
        /// --topup also its S0-8, which must be the top-up plan's.
        #[arg(long)]
        values: PathBuf,
        /// An R0 local plan (v1 or v2, or a one-split copy of one) whose
        /// recorded CAL input the plan keeps as generate's overlap extra
        /// (required; the CAL file is not opened here).
        #[arg(long)]
        cal_from: PathBuf,
        /// D5: the top-up plan directory (topup-plan's output).
        #[arg(long)]
        topup: Option<PathBuf>,
        /// New output directory.
        #[arg(long)]
        out: PathBuf,
    },
    /// D6's plan (B5b part 2 item 3): one mirror per document of the Enron
    /// Train half (enron-half's output, checked against S0-9), local only.
    EnronPlan {
        /// enron-half's Train file.
        #[arg(long)]
        train_half: PathBuf,
        /// VALUES-R2.json (S0-9's halves, S0-6's r05).
        #[arg(long)]
        values: PathBuf,
        /// An R0 local plan (v1 or v2, or a one-split copy of one) whose
        /// recorded CAL input the plan keeps as generate's overlap extra
        /// (required; the CAL file is not opened here).
        #[arg(long)]
        cal_from: PathBuf,
        /// New output directory.
        #[arg(long)]
        out: PathBuf,
    },
    /// The domain-supervision top-up plan from Train-mining against S0-8
    /// (B5b part 2 item 2): added humans, D5's mirror parents and family
    /// quotas, and Stage 0-H's frontier label allocation with its check.
    TopupPlan {
        /// VALUES-R2.json, whose S0-8 the targets are.
        #[arg(long)]
        values: PathBuf,
        /// Train-mining documents (frozen_split train_mining).
        #[arg(long)]
        train_mining: PathBuf,
        /// Train-mining's split manifest (id, split, domain): S0-8's domains.
        #[arg(long)]
        train_mining_manifest: PathBuf,
        /// R0's Train humans (required): R0 Train ids are never added again
        /// as rule-4 humans, and they are the second parent source of D5's
        /// mirrors.
        #[arg(long)]
        train: PathBuf,
        /// R0's Train split manifest (id, split, domain).
        #[arg(long)]
        train_manifest: PathBuf,
        /// Generator configs (key, family) of the seven Train instruct families.
        #[arg(long)]
        configs: PathBuf,
        /// The frontier collector's plan config whose models' labels the
        /// allocation splits over.
        #[arg(long)]
        frontier_config: PathBuf,
        /// New output directory.
        #[arg(long)]
        out: PathBuf,
    },
    /// Re-checks a top-up plan's frontier allocation against S0-8 (amendment
    /// 5 item 5.3; exit 0 only when every domain passes).
    TopupCheck {
        #[arg(long)]
        topup: PathBuf,
        #[arg(long)]
        values: PathBuf,
    },
    /// The Enron Train half (V23, S0-9): its writer keys' documents as a Train
    /// input file; the diagnostic half's lines are skipped unparsed.
    EnronHalf {
        /// The enron-v1 pool directory S0-9 binds (its SHA256SUMS); its
        /// documents.jsonl is read, checked against that SHA256SUMS.
        #[arg(long)]
        pool: PathBuf,
        /// The repository root, against which S0-9's input paths resolve.
        #[arg(long)]
        root: PathBuf,
        /// VALUES-R2.json, whose S0-9 halves, counts and words the run must equal.
        #[arg(long)]
        values: PathBuf,
        /// New output file (mode 600).
        #[arg(long)]
        out: PathBuf,
    },
}

fn read_jsonl(path: &Path) -> Result<Vec<Value>> {
    let f = BufReader::new(fs::File::open(path).with_context(|| path.display().to_string())?);
    let mut v = Vec::new();
    for l in f.lines() {
        let l = l?;
        if !l.trim().is_empty() {
            v.push(serde_json::from_str(&l)?);
        }
    }
    Ok(v)
}

use grammar_a_positives::{write_json, write_jsonl};

fn file_sha(p: &Path) -> Result<String> {
    Ok(text::sha_hex(&fs::read(p)?))
}

/// The non-blank lines of JSONL bytes, parsed (as `read_jsonl` reads a file).
fn jsonl_bytes(b: &[u8]) -> Result<Vec<Value>> {
    let mut v = Vec::new();
    for l in b.split(|&x| x == b'\n') {
        if !l.iter().all(u8::is_ascii_whitespace) {
            v.push(serde_json::from_slice(l)?);
        }
    }
    Ok(v)
}

/// `generate` and `finalize` read every input their plan records
/// (`load_humans`); a plan that records an old Dev-select input they would
/// read (R0's plans v1 and v2) is refused before any is opened (protocol A2
/// §2.4; amendment 20 item 20.1): run on a one-split copy (`split-copy`).
fn refuse_old_dev_select_inputs(meta: &Value) -> Result<()> {
    for sp in plan::ALL_SPLITS {
        if !meta["inputs"][sp].is_null() && span_ops::plan_reads_input(meta["schema"].as_str(), sp) {
            ensure!(sp != "dev_select", "the plan records a dev_select input, which generate and finalize would read: old Dev-select is never read, scored or parsed (protocol A2 section 2.4); run on a one-split copy (split-copy)");
        }
    }
    Ok(())
}

/// A Train-only mirror plan's Train rows take `plan::train_only_domain_of`
/// (enron-v1's Train half is D6's); every other plan's take `domain_of`,
/// which refuses enron-v1 (B5b part 2; amendment 27).
fn train_only_plan(plan_meta: &Value) -> bool {
    plan_meta["schema"] == grammar_a_positives::train_mirror::TRAIN_MIRROR_SCHEMA
}

fn load_humans(plan_meta: &Value) -> Result<(Vec<plan::Human>, HashMap<String, (String, Value)>)> {
    let mut hs = Vec::new();
    let mut rows = HashMap::new();
    let train_only = train_only_plan(plan_meta);
    for split in plan::ALL_SPLITS {
        let inp = &plan_meta["inputs"][split];
        if inp.is_null() || !span_ops::plan_reads_input(plan_meta["schema"].as_str(), split) {
            continue;
        }
        let p = PathBuf::from(inp["path"].as_str().context("input path")?);
        ensure!(file_sha(&p)? == inp["sha256"], "input {split} changed since planning");
        for row in read_jsonl(&p)? {
            let h = plan::human_from_in(&row, split, train_only)?;
            let mut meta = row.clone();
            meta.as_object_mut().unwrap().remove("text");
            ensure!(rows.insert(h.id.clone(), (h.text.clone(), meta)).is_none(), "duplicate human id {}", h.id);
            hs.push(h);
        }
    }
    Ok((hs, rows))
}

/// Profile v2's acceptance step for a `generate` run of one split under
/// population `pop`: the prior of the parents of the plan's items in that
/// split (`items.jsonl`, bound by `items_sha256`), read from that split's
/// input alone; the generator's items with several requests in that split
/// (each request's span checked against the request file); and every
/// request id of the split's items, of every generator (the batch's
/// requests, for the binding).
fn form_for_split(
    meta: &Value,
    pdir: &Path,
    want: &[String],
    pop: &str,
    generator: &str,
    reqs: &[plan::Request],
) -> Result<(form_post::FormPost, Vec<form_post::ItemSpec>, Vec<String>)> {
    ensure!(want.len() == 1, "a profile run selects exactly one split (--splits), so its population and prior are one batch's; selected {want:?}");
    let split = want[0].as_str();
    let items_path = pdir.join("items.jsonl");
    // one split, read from the lines' bytes before any parse (ruling R2)
    let (ib, isplit) = split_copy::one_split_file(&items_path, "split", split_copy::OneSplit::Rows)?;
    ensure!(meta["items_sha256"] == text::sha_hex(&ib).as_str(), "items.jsonl changed since planning");
    ensure!(isplit.as_deref() == Some(split), "the plan's items are of split {isplit:?}, not the run's {split}");
    let spans: HashMap<&str, Option<(usize, usize)>> = reqs.iter().map(|r| (r.request_id.as_str(), r.source_span)).collect();
    let mut ids: BTreeSet<String> = BTreeSet::new();
    let mut specs: Vec<form_post::ItemSpec> = Vec::new();
    let mut batch: Vec<String> = Vec::new();
    for v in jsonl_bytes(&ib)? {
        let it: plan::Item = serde_json::from_value(v)?;
        ids.insert(it.parent_id.clone());
        batch.extend(it.requests.iter().cloned());
        if it.generator == generator && it.requests.len() > 1 {
            ensure!(it.kind != "mirror" && it.source_spans.len() == it.requests.len(), "item {}: {} requests, {} source spans", it.item_id, it.requests.len(), it.source_spans.len());
            let mut parts = Vec::new();
            for (rid, sp) in it.requests.iter().zip(&it.source_spans) {
                ensure!(spans.get(rid.as_str()) == Some(&Some(*sp)), "item {}: request {rid} is not its span {sp:?} in the request file", it.item_id);
                parts.push((rid.clone(), *sp, None));
            }
            specs.push(form_post::ItemSpec { item_id: it.item_id.clone(), parts });
        }
    }
    let inp = &meta["inputs"][split];
    ensure!(!inp.is_null() && span_ops::plan_reads_input(meta["schema"].as_str(), split), "the plan reads no {split} input");
    let p = PathBuf::from(inp["path"].as_str().context("input path")?);
    ensure!(file_sha(&p)? == inp["sha256"], "input {split} changed since planning");
    let mut parents: Vec<plan::Human> = Vec::new();
    let train_only = train_only_plan(meta);
    for row in read_jsonl(&p)? {
        let h = plan::human_from_in(&row, split, train_only)?;
        if ids.contains(&h.id) {
            parents.push(h);
        }
    }
    ensure!(parents.len() == ids.len(), "split {split}: {} item parents, {} rows of its input match them", ids.len(), parents.len());
    let f = form_post::FormPost::new(pop, parents.iter().map(|h| (h.id.as_str(), h.domain, h.text.as_str())))?;
    Ok((f, specs, batch))
}

/// The batch binding of a `generate` run: one file per split under the runs
/// directory. With the profile off every selected split must be unbound;
/// with it on, the one split's binding must hold this run's record, or be
/// absent with no request of the split judged yet.
fn check_generate_binding(runs: &Path, want: &[String], form: Option<&form_post::FormPost>, batch: &[String], all: &HashMap<String, generation::ReqState>) -> Result<()> {
    match form {
        None => {
            for s in want {
                form_post::check_binding(&form_post::binding_path(runs, &format!("split-{s}")), None, false)?;
            }
            Ok(())
        }
        Some(f) => {
            let earlier = batch.iter().any(|id| all.get(id).is_some_and(|st| !st.intents.is_empty() || !st.responses.is_empty()));
            form_post::check_binding(&form_post::binding_path(runs, &format!("split-{}", want[0])), Some(&f.record()), earlier)
        }
    }
}

fn run_dirs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut v = Vec::new();
    if root.exists() {
        for e in fs::read_dir(root)? {
            let p = e?.path();
            if p.is_dir() {
                v.push(p);
            }
        }
    }
    v.sort();
    Ok(v)
}

/// Accepted Train item ids per (generator, domain, cohort, stratum), with
/// `stratify::Item`'s cohort and stratum.
type StratumAcc = BTreeMap<(String, String, String, String), BTreeSet<String>>;

/// The Train lines of a plan's `items.jsonl`, each selected by the split read
/// from its bytes before any parse (amendment 15 item 15.4's reading): lines
/// of other splits are skipped unparsed and counted in `skipped`, and a line
/// whose split cannot be read refuses the file. R0's plans hold old
/// Dev-select lines, which are never parsed (protocol A2 §2.4; B5b).
fn train_items(plan_dir: &Path, skipped: &mut split_copy::Skipped) -> Result<Vec<Value>> {
    let p = plan_dir.join("items.jsonl");
    let b = fs::read(&p).with_context(|| p.display().to_string())?;
    let (sel, _) = split_copy::select_lines(&b, "split", "train", &p.display().to_string(), &format!("{}:items", plan_dir.display()), false, skipped)?;
    jsonl_bytes(&sel)
}

/// The Train journal states of a runs root (`generation::load_states_of_split`;
/// `ids`: the Train items' requests, for journal lines that name no split).
fn train_states(runs: &Path, ids: &HashSet<String>, skipped: &mut split_copy::Skipped) -> Result<HashMap<String, generation::ReqState>> {
    let (st, sk) = generation::load_states_of_split(&run_dirs(runs)?, "train", ids)?;
    let e = skipped.entry(format!("{}:journals", runs.display())).or_default();
    for (k, n) in sk.other_splits {
        *e.entry(k).or_default() += n;
    }
    if sk.unreadable > 0 {
        *e.entry("no_readable_split".into()).or_default() += sk.unreadable;
    }
    Ok(st)
}

/// The request ids of plan items.
fn request_ids(items: &[Value]) -> HashSet<String> {
    items.iter().flat_map(|v| v["requests"].as_array().into_iter().flatten().filter_map(|r| r.as_str().map(String::from))).collect()
}

/// Accepted Train items of one plan/run pair, per (generator, domain) and per
/// (generator, domain, cohort, stratum). Only Train lines of the plan and
/// its journals are parsed.
fn accepted_train(plan_dir: &Path, runs: &Path, acc: &mut BTreeMap<(String, String), BTreeSet<String>>, by_stratum: &mut StratumAcc, skipped: &mut split_copy::Skipped) -> Result<usize> {
    let items = train_items(plan_dir, skipped)?;
    let states = train_states(runs, &request_ids(&items), skipped)?;
    let mut n = 0;
    for v in items {
        if v["split"] != "train" {
            continue;
        }
        let ok = v["requests"].as_array().is_some_and(|rs| rs.iter().any(|r| r.as_str().and_then(|r| states.get(r)).is_some_and(|s| s.accepted().is_some())));
        if ok {
            let k = (v["generator"].as_str().unwrap_or("").to_owned(), v["domain"].as_str().unwrap_or("").to_owned());
            if acc.entry(k).or_default().insert(v["item_id"].as_str().unwrap_or("").to_owned()) {
                n += 1;
            }
            if let Some(it) = stratify::Item::from_plan(&v) {
                by_stratum.entry((it.generator, it.domain, it.cohort, it.stratum)).or_default().insert(it.item_id);
            }
        }
    }
    Ok(n)
}

/// `stratify-train`'s S0-10 mode (B5b part 2 item 5; `stratify::S0_10_RULE`).
#[derive(Clone, Copy)]
enum StratMode<'a> {
    /// Part 1's selection (budgets and v_fam targets).
    Off,
    /// Write S0-10's register entry of the --target selection to this file.
    Write(&'a Path),
    /// D7's step: the per-domain quotas of --values' S0-10.
    Frozen,
}

#[allow(clippy::too_many_arguments)]
fn stratify_train(
    pdir: &Path,
    runs: &Path,
    prior: &[String],
    partitions: &Path,
    generators: &[String],
    targets: &[String],
    values: Option<&Path>,
    mode: StratMode,
    floor: usize,
    cohort_shares: &str,
    priority_domains: &str,
    domain_weights: &str,
    seed: &str,
    out: &Path,
) -> Result<()> {
    let meta: Value = serde_json::from_slice(&fs::read(pdir.join("plan.json"))?)?;
    let humans: BTreeMap<String, f64> = meta["humans"]["train"]
        .as_object()
        .context("plan humans.train")?
        .iter()
        .map(|(k, v)| (k.clone(), v.as_f64().unwrap_or(0.0)))
        .collect();
    let mut dweights = BTreeMap::new();
    for kv in domain_weights.split(',').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').context("DOMAIN=MULTIPLIER")?;
        ensure!(humans.contains_key(k), "weighted domain {k} has no Train humans in the plan");
        dweights.insert(k.to_owned(), v.parse::<f64>()?);
    }
    let quota_humans: BTreeMap<String, f64> =
        humans.iter().map(|(d, h)| (d.clone(), h * dweights.get(d).copied().unwrap_or(1.0))).collect();
    let shares: Vec<(String, f64)> = cohort_shares
        .split(',')
        .map(|kv| {
            let (k, v) = kv.split_once('=').context("COHORT=SHARE")?;
            Ok((k.to_owned(), v.parse::<f64>()?))
        })
        .collect::<Result<_>>()?;
    let gens: Vec<(String, usize)> = generators
        .iter()
        .map(|kv| {
            let (k, v) = kv.split_once('=').context("GENERATOR=BUDGET")?;
            Ok((k.to_owned(), v.parse::<usize>()?))
        })
        .collect::<Result<_>>()?;
    ensure!(!gens.is_empty() || !targets.is_empty(), "no generators");
    let mut target_rec = serde_json::Map::new();
    let prio: BTreeSet<String> = priority_domains.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
    for d in &prio {
        ensure!(humans.contains_key(d), "priority domain {d} has no Train humans in the plan");
    }
    let mut parts = HashMap::new();
    for v in read_jsonl(partitions)? {
        parts.insert(v["id"].as_str().context("partition id")?.to_owned(), v["partition"].as_str().context("partition")?.to_owned());
    }
    // existing accepted Train positives (Train lines only, read from bytes)
    let mut skipped = split_copy::Skipped::new();
    let mut acc: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    let mut acc_strata = StratumAcc::new();
    let mut prior_rec = Vec::new();
    let n0 = accepted_train(pdir, runs, &mut acc, &mut acc_strata, &mut skipped)?;
    prior_rec.push(json!({"plan":pdir.display().to_string(),"runs":runs.display().to_string(),"accepted_train_items":n0}));
    for pr in prior {
        let (pp, rr) = pr.split_once('=').context("PLAN_DIR=RUNS_DIR")?;
        let n = accepted_train(Path::new(pp), Path::new(rr), &mut acc, &mut acc_strata, &mut skipped)?;
        prior_rec.push(json!({"plan":pp,"runs":rr,"accepted_train_items":n,
            "plan_sha256":file_sha(&Path::new(pp).join("plan.json"))?}));
    }
    // a target of v_fam accepted rows per listed generator, v_fam read from
    // VALUES-R2.json (S0-7; protocol A2 sections 2.8 and 6.4): the budget is
    // the target less the generator's existing accepted Train items (B5b: D7)
    if !matches!(mode, StratMode::Off) {
        ensure!(!targets.is_empty() && gens.is_empty(), "S0-10 is the base cohort's stratification at v_fam: --target generators only, no --generator budget");
        for k in targets {
            ensure!(plan::TRAIN_BASE.contains(&k.as_str()), "--target {k}: S0-10 stratifies the Train base families ({:?})", plan::TRAIN_BASE);
        }
        if let StratMode::Write(p) = mode {
            ensure!(!p.exists(), "{} exists: S0-10's entry is written once", p.display());
            let set: BTreeSet<&str> = targets.iter().map(String::as_str).collect();
            ensure!(set.len() == targets.len() && set == plan::TRAIN_BASE.iter().copied().collect::<BTreeSet<_>>(),
                "S0-10 stratifies every Train base family once ({:?}; protocol A2 section 6.4)", plan::TRAIN_BASE);
        }
        // S0-10's fixed options (amendment 27): by Train human counts (the
        // floor is fixed where the options are read)
        ensure!(dweights.is_empty(), "S0-10 stratifies by the base plan's Train human counts (protocol A2 sections 6.4 and 12.6; amendment 27): --domain-weights is refused");
        ensure!(prio.is_empty(), "S0-10's order has no priority domains (amendment 27): --priority-domains is refused");
        ensure!(shares.len() == 1 && shares[0].0 == span_ops::COHORT_BASE && shares[0].1 == 1.0, "S0-10's cohort shares are {}=1 (amendment 27)", span_ops::COHORT_BASE);
        ensure!(seed == stratify::S0_10_SEED, "S0-10's seed is {} (amendment 27)", stratify::S0_10_SEED);
    }
    let mut gens = gens;
    let mut values_rec = Value::Null;
    let mut values_open: Option<values::Values> = None;
    if !targets.is_empty() {
        let vp = values.context("--target reads v_fam from --values VALUES-R2.json (S0-7)")?;
        let vv = values::Values::open(vp)?;
        let (t, rec) = values::v_fam(&vv)?;
        values_open = Some(vv);
        values_rec = rec;
        for k in targets {
            ensure!(!k.contains('='), "--target {k}: a generator name; its target is S0-7's v_fam, read from --values");
            ensure!(!gens.iter().any(|(g, _)| g == k), "generator {k} has both a budget and a target");
            let have: usize = acc.iter().filter(|((g, _), _)| g == k).map(|(_, s)| s.len()).sum();
            target_rec.insert(k.to_owned(), json!({"target_accepted": t, "existing_accepted_train": have, "budget": t.saturating_sub(have)}));
            gens.push((k.to_owned(), t.saturating_sub(have)));
        }
    }
    ensure!(!gens.is_empty(), "no generators");
    // candidates: Train, listed cohort, Train-partition styles, not all requests done
    let titems = train_items(pdir, &mut split_copy::Skipped::new())?;
    let states = train_states(runs, &request_ids(&titems), &mut split_copy::Skipped::new())?;
    let mut cands: BTreeMap<String, Vec<stratify::Item>> = BTreeMap::new();
    let mut excluded: BTreeMap<String, BTreeMap<&str, usize>> = BTreeMap::new();
    for v in titems {
        if v["split"] != "train" {
            continue;
        }
        let Some(it) = stratify::Item::from_plan(&v) else { continue };
        if !gens.iter().any(|(g, _)| *g == it.generator) {
            continue;
        }
        let why = if !shares.iter().any(|(c, _)| *c == it.cohort) {
            Some("cohort_not_listed")
        } else if !stratify::styles_train(&it, &parts) {
            Some("style_not_train_partition")
        } else if it.requests.iter().all(|r| states.get(r).is_some_and(|s| s.done())) {
            Some("already_done")
        } else {
            None
        };
        match why {
            Some(w) => *excluded.entry(it.generator.clone()).or_default().entry(w).or_default() += 1,
            None => cands.entry(it.generator.clone()).or_default().push(it),
        }
    }
    let balanced: BTreeSet<String> = stratify::BALANCED_COHORTS.iter().map(|c| c.to_string()).collect();
    let params_of = |g: &str, budget: usize| -> stratify::Params {
        let credit: BTreeMap<(String, String, String), usize> = acc_strata
            .iter()
            .filter(|((gg, _, c, _), _)| gg == g && balanced.contains(c))
            .map(|((_, d, c, s), ids)| ((d.clone(), c.clone(), s.clone()), ids.len()))
            .collect();
        stratify::Params {
            budget,
            floor,
            cohort_shares: shares.clone(),
            priority_domains: prio.clone(),
            seed: format!("{seed}:{g}"),
            balanced_cohorts: balanced.clone(),
            stratum_credit: credit,
        }
    };
    let existing_of = |g: &str| -> BTreeMap<String, usize> { acc.iter().filter(|((gg, _), _)| gg == g).map(|((_, d), s)| (d.clone(), s.len())).collect() };
    let prior_inputs: Vec<Value> = prior_rec.iter().skip(1).map(|r| json!({"plan": r["plan"], "plan_sha256": r["plan_sha256"]})).collect();
    let s0_10_inputs = json!({"plan": {"plan_sha256": file_sha(&pdir.join("plan.json"))?, "items_sha256": file_sha(&pdir.join("items.jsonl"))?},
        "v_fam": values_rec["value"], "prior_plans": prior_inputs, "partitions_sha256": file_sha(partitions)?, "floor": floor, "seed": seed,
        "cohort_shares": shares.iter().map(|(c, s)| json!([c, s])).collect::<Vec<_>>(), "domain_weights": dweights, "train_humans": humans});
    // S0-10: write its entry, or read it for D7's step (B5b part 2 item 5)
    let mut fixed: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut strat_rec = Value::Null;
    match mode {
        StratMode::Off => {}
        StratMode::Write(p) => {
            let mut tg = serde_json::Map::new();
            let mut ex = serde_json::Map::new();
            let mut av = serde_json::Map::new();
            let mut pq = serde_json::Map::new();
            for (g, budget) in &gens {
                let cs = cands.get(g).map(Vec::as_slice).unwrap_or(&[]);
                let existing = existing_of(g);
                let sel = stratify::select(cs, &quota_humans, &existing, &params_of(g, *budget))?;
                let mut t: BTreeMap<String, usize> = sel.quota.iter().map(|(d, q)| (d.clone(), q + existing.get(d).copied().unwrap_or(0))).collect();
                for (d, n) in &existing {
                    t.entry(d.clone()).or_insert(*n);
                }
                tg.insert(g.clone(), json!(t));
                ex.insert(g.clone(), json!(existing));
                av.insert(g.clone(), json!(sel.available));
                pq.insert(g.clone(), json!(sel.quota));
            }
            let mut value = s0_10_inputs.clone();
            value["schema"] = json!(stratify::S0_10_SCHEMA);
            value["rule"] = json!(stratify::S0_10_RULE);
            value["targets"] = json!(tg);
            value["existing_accepted_train"] = json!(ex);
            value["available"] = json!(av);
            value["first_pass_quota"] = json!(pq);
            value["v_fam_record"] = values_rec.clone();
            let (id, name) = values::S0_10_ID;
            let entry = json!({"id": id, "name": name, "value": value});
            let mut b = serde_json::to_vec_pretty(&entry)?;
            b.push(b'\n');
            let h = split_copy::write_new(p, &b)?;
            println!("{}", serde_json::to_string(&json!({"s0_10_entry": p.display().to_string(), "sha256": h, "targets": entry["value"]["targets"]}))?);
            return Ok(());
        }
        StratMode::Frozen => {
            let vv = values_open.as_ref().context("--stratification reads S0-10 from --values")?;
            let (st, rec) = values::base_stratification(vv)?;
            for k in ["plan", "v_fam", "prior_plans", "partitions_sha256", "floor", "seed", "cohort_shares", "domain_weights", "train_humans"] {
                ensure!(st[k] == s0_10_inputs[k], "S0-10's {k} differs from this run's: D7 follows the frozen stratification (protocol A2 section 12.6)");
            }
            let tg = st["targets"].as_object().context("S0-10 targets")?;
            ensure!(tg.keys().cloned().collect::<BTreeSet<_>>() == gens.iter().map(|(g, _)| g.clone()).collect::<BTreeSet<_>>(), "S0-10 stratifies other generators than --target names");
            for (g, budget) in gens.iter_mut() {
                let existing = existing_of(g);
                let mut q = BTreeMap::new();
                for (d, t) in tg[g.as_str()].as_object().context("S0-10 targets of a generator")? {
                    let t = t.as_u64().context("an S0-10 target")? as usize;
                    q.insert(d.clone(), t.saturating_sub(existing.get(d).copied().unwrap_or(0)));
                }
                *budget = q.values().sum();
                fixed.insert(g.clone(), q);
            }
            strat_rec = json!({"record": rec, "rule": stratify::S0_10_RULE, "quota_this_pass": fixed});
        }
    }
    fs::create_dir_all(out)?;
    let mut table = serde_json::Map::new();
    for (g, budget) in &gens {
        let cs = cands.get(g).map(Vec::as_slice).unwrap_or(&[]);
        let existing: BTreeMap<String, usize> =
            acc.iter().filter(|((gg, _), _)| gg == g).map(|((_, d), s)| (d.clone(), s.len())).collect();
        let balanced: BTreeSet<String> = stratify::BALANCED_COHORTS.iter().map(|c| c.to_string()).collect();
        let credit: BTreeMap<(String, String, String), usize> = acc_strata
            .iter()
            .filter(|((gg, _, c, _), _)| gg == g && balanced.contains(c))
            .map(|((_, d, c, s), ids)| ((d.clone(), c.clone(), s.clone()), ids.len()))
            .collect();
        let p = stratify::Params {
            budget: *budget,
            floor,
            cohort_shares: shares.clone(),
            priority_domains: prio.clone(),
            seed: format!("{seed}:{g}"),
            balanced_cohorts: balanced,
            stratum_credit: credit.clone(),
        };
        let sel = stratify::select_with(cs, &quota_humans, &existing, &p, fixed.get(g))?;
        let by_id: HashMap<&str, &stratify::Item> = cs.iter().map(|i| (i.item_id.as_str(), i)).collect();
        let mut dom: BTreeMap<&str, usize> = BTreeMap::new();
        let mut coh: BTreeMap<String, usize> = BTreeMap::new();
        let mut strat: BTreeMap<String, usize> = BTreeMap::new();
        let mut dom_strat: BTreeMap<String, usize> = BTreeMap::new();
        let credit_rec: BTreeMap<String, usize> = credit.iter().map(|((d, c, s), n)| (format!("{d}|{c}|{s}"), *n)).collect();
        let mut sk: BTreeMap<&str, usize> = BTreeMap::new();
        let mut nreq = 0;
        for id in &sel.order {
            let it = by_id[id.as_str()];
            *dom.entry(&it.domain).or_default() += 1;
            *coh.entry(it.cohort.clone()).or_default() += 1;
            *strat.entry(format!("{}|{}", it.cohort, it.stratum)).or_default() += 1;
            if p.balanced_cohorts.contains(&it.cohort) {
                *dom_strat.entry(format!("{}|{}|{}", it.domain, it.cohort, it.stratum)).or_default() += 1;
            }
            *sk.entry(&it.style_kind).or_default() += 1;
            nreq += it.requests.len();
        }
        let mut body = format!(
            "# slop_ninja_a_train_strat_v1 generator={g} budget={budget} floor={floor} seed={seed} items={}\n",
            sel.order.len()
        );
        for id in &sel.order {
            body.push_str(id);
            body.push('\n');
        }
        let path = out.join(format!("{g}.txt"));
        let tmp = out.join(format!(".{g}.txt.tmp"));
        fs::write(&tmp, &body)?;
        fs::rename(&tmp, &path)?;
        table.insert(g.clone(), json!({"budget":budget,"items":sel.order.len(),"requests":nreq,"order_file":path.display().to_string(),
            "order_sha256":text::sha_hex(body.as_bytes()),"domains":dom,"quota":sel.quota,"available":sel.available,
            "existing_accepted_train":sel.existing,"cohorts":coh,"cohort_bins":strat,"style_kinds":sk,"excluded":excluded.get(g),
            "balanced_kinds":{"selected":dom_strat,"accepted_credit":credit_rec}}));
    }
    let rec = json!({"schema":"slop_ninja_a_train_strat_v1","code":CODE_ID,"plan":pdir.display().to_string(),
        "plan_sha256":file_sha(&pdir.join("plan.json"))?,"items_sha256":file_sha(&pdir.join("items.jsonl"))?,
        "partitions":partitions.display().to_string(),"partitions_sha256":file_sha(partitions)?,"existing_sources":prior_rec,
        "floor":floor,"cohort_shares":shares.iter().map(|(c, s)| json!([c, s])).collect::<Vec<_>>(),"priority_domains":prio,
        "seed":seed,"train_humans":humans,"domain_weights":dweights,"targets":target_rec,"target_values":values_rec,
        "lines_of_other_splits_skipped_unparsed":skipped,
        "policy":"per-generator domain quotas water-filled so existing accepted plus new Train items track the Train human mix, floor per domain bounded by availability; cohort shares and share-bin rotation inside a domain (in the balanced cohorts, train_base_v3, the kind with the fewest accepted plus selected items in the domain first, so kinds stay 1:1 across batches); voice ladders whole; styles only from the frozen Train partition; domains interleaved so any prefix is stratified, priority domains front-loaded",
        "generators":table});
    let mut rec = rec;
    if !strat_rec.is_null() {
        // D7's step only: with the S0-10 options off the record's bytes are part 1's
        rec["stratification"] = strat_rec;
    }
    write_json(&out.join("selection.json"), &rec)?;
    println!("{}", serde_json::to_string(&rec)?);
    Ok(())
}

fn run_plan_v1(train: &Path, dev_iter: &Path, dev_select: &Path, cal: &Path, out: &Path) -> Result<()> {
            fs::create_dir_all(out.join("requests"))?;
            let mut inputs = serde_json::Map::new();
            for (split, p) in [("train", &train), ("dev_iter", &dev_iter), ("dev_select", &dev_select), ("cal", &cal)] {
                inputs.insert(split.into(), json!({"path":fs::canonicalize(p)?.display().to_string(),"sha256":file_sha(p)?}));
            }
            let meta0 = json!({"inputs":inputs});
            let (humans, _) = load_humans(&meta0)?;
            let stop = text::stopwords();
            let mut idf: HashMap<&'static str, text::Idf> = HashMap::new();
            for d in plan::DOMAINS {
                idf.insert(d, text::Idf::default());
            }
            for h in humans.iter().filter(|h| h.split == "train") {
                idf.get_mut(h.domain).unwrap().add(&h.text, &stop);
            }
            let p = plan::build(&humans, &idf)?;
            let items_sha = write_jsonl(&out.join("items.jsonl"), &p.items)?;
            let fail_sha = write_jsonl(&out.join("plan-failures.jsonl"), &p.failures)?;
            let mut by_gen: BTreeMap<String, Vec<&plan::Request>> = BTreeMap::new();
            for r in &p.requests {
                by_gen.entry(r.generator.clone()).or_default().push(r);
            }
            let mut req_files = serde_json::Map::new();
            let order = |s: &str| plan::SPLITS.iter().position(|x| *x == s).unwrap();
            for (g, mut v) in by_gen {
                // eval splits first so a partial run still covers evaluation
                v.sort_by_key(|r| (if r.split == "train" { 1 } else { 0 }, order(&r.split)));
                let rows: Vec<&plan::Request> = v;
                let sha = write_jsonl(&out.join("requests").join(format!("{g}.jsonl")), &rows)?;
                req_files.insert(g, json!({"requests":rows.len(),"sha256":sha}));
            }
            let mut counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
            for it in &p.items {
                *counts.entry(it.split.clone()).or_default().entry(format!("{}:{}", it.cohort, it.generator)).or_default() += 1;
            }
            let mut humans_by: BTreeMap<String, BTreeMap<&str, usize>> = BTreeMap::new();
            for h in &humans {
                *humans_by.entry(h.split.clone()).or_default().entry(h.domain).or_default() += 1;
            }
            let idf_meta: BTreeMap<&str, Value> =
                idf.iter().map(|(d, t)| (*d, json!({"train_documents":t.n_docs,"types":t.df.len(),"sha256":t.digest()}))).collect();
            let meta = json!({"schema":plan::PLAN_SCHEMA,"code":CODE_ID,"inputs":meta0["inputs"],
                "producer_sha256":{"plan.rs":text::sha_hex(include_bytes!("plan.rs")),"text.rs":text::sha_hex(include_bytes!("text.rs"))},
                "key_prefix":plan::PLAN_KEY_PREFIX,"splitter":text::SPLITTER_ID,"terms":text::TERMS_ID,"stopwords_sha256":text::sha_hex(text::STOPWORDS_V1.join("\n").as_bytes()),
                "idf":idf_meta,"humans":humans_by,"items":p.items.len(),"items_sha256":items_sha,"requests":p.requests.len(),
                "request_files":req_files,"plan_failures":p.failures.len(),"plan_failures_sha256":fail_sha,"item_counts":counts,
                "rules":{
                    "evaluation":"protocol-v1.json positive_plan verbatim: 21-slot generator cycle, bins R1-R6 by rank(k_mixed_bin) mod 6, two-span when rank(k_multi_span) mod 3 == 0 in R2-R4, decoding profile by rank(k_decoding) mod 2, seed = first 4 bytes of request SHA-256 big-endian, second attempt seed+1",
                    "train_primary":"one mirror and one mixed request per human, same templates and bins; generator = 8-cycle over Train instruct families (Qwen3.6, Phi-4-mini, Mistral-7B, Gemma-3-4b, Granite, SmolLM3, ERNIE, gpt-oss-20b) by rank mod 8; decoding profile rank(k_decoding) mod 4 over A(0.7,0.9) B(1.0,0.95) C(0.8,0.92) D(0.9,1.0)",
                    "train_low_share":"Train-only supplementary cohort: rank(k_train_low) mod 4 == 0; one whole-sentence run closest to 5% (R0), generator rank(k_train_low_gen) mod 8",
                    "train_base_robustness":"Train-only robustness cohort: rank(k_train_base) mod 4 == 0; continuation if floor(rank/4) mod 2 == 0 else splice; generator floor(rank/8) mod 3 over Granite/SmolLM3/ERNIE base; temperature 0.8 top_p 0.95 max 1024; 12-word copy screen against removed human text",
                    "requests_below_10_percent":"not made in primary cohorts; realised shares below 10% are reported in (0,10)"
                }});
            write_json(&out.join("plan.json"), &meta)?;
            println!("{}", serde_json::to_string(&json!({"items":p.items.len(),"requests":p.requests.len(),"plan_failures":p.failures.len(),"request_files":meta["request_files"]}))?);
            Ok(())
}

fn run_plan_v2(train: &Path, dev_iter: &Path, dev_select: &Path, cal: &Path, library: &Path, fix_slop: &Path, out: &Path) -> Result<()> {
            fs::create_dir_all(out.join("requests"))?;
            let mut inputs = serde_json::Map::new();
            for (split, p) in [("train", &train), ("dev_iter", &dev_iter), ("dev_select", &dev_select), ("cal", &cal)] {
                inputs.insert(split.into(), json!({"path":fs::canonicalize(p)?.display().to_string(),"sha256":file_sha(p)?}));
            }
            let meta0 = json!({"inputs":inputs});
            let (humans, _) = load_humans(&meta0)?;
            let stop = text::stopwords();
            let mut idf: HashMap<&'static str, text::Idf> = HashMap::new();
            for d in plan::DOMAINS {
                idf.insert(d, text::Idf::default());
            }
            for h in humans.iter().filter(|h| h.split == "train") {
                idf.get_mut(h.domain).unwrap().add(&h.text, &stop);
            }
            let mut lib = Vec::new();
            let mut lib_files = serde_json::Map::new();
            for f in ["voices.accepted.jsonl", "briefs.accepted.jsonl"] {
                let p = library.join(f);
                lib_files.insert(f.into(), json!({"sha256":file_sha(&p)?}));
                for a in read_jsonl(&p)? {
                    let kind = a["type"].as_str().context("type")?.to_owned();
                    lib.push(diverse::Brief {
                        id: a["id"].as_str().context("id")?.into(),
                        genre: if kind == "voice" { "any".into() } else { a["genre"].as_str().context("genre")?.into() },
                        kind,
                        inventor: a["inventor"].as_str().context("inventor")?.into(),
                        instruction: a["text"].as_str().context("text")?.into(),
                        partition: a["partition"].as_str().context("partition")?.into(),
                        axis: a["axis"].as_str().unwrap_or("other").into(),
                        imitated_author: a["imitated_author"].as_str().map(String::from),
                        rewrite_safe: a["rewrite_safe"] == true,
                        eval_author_conflict: a["eval_author_conflicts"].as_array().is_some_and(|v| !v.is_empty()),
                        origin: a["origin"].as_str().unwrap_or("invented").into(),
                        source_family: a["source"]["source_group"].as_str().map(String::from),
                        cluster: a["cluster"].as_str().context("cluster")?.into(),
                    });
                }
            }
            let fs_bundle = diverse::load_fix_slop(&fix_slop)?;
            let p = diverse::build_v2(&diverse::V2Inputs { humans: &humans, idf: &idf, briefs: &lib, fix_slop: &fs_bundle })?;
            let items_sha = write_jsonl(&out.join("items.jsonl"), &p.items)?;
            let fail_sha = write_jsonl(&out.join("plan-failures.jsonl"), &p.failures)?;
            let mut by_gen: BTreeMap<String, Vec<&plan::Request>> = BTreeMap::new();
            for r in &p.requests {
                by_gen.entry(r.generator.clone()).or_default().push(r);
            }
            let order = |s: &str| plan::SPLITS.iter().position(|x| *x == s).unwrap();
            let mut req_files = serde_json::Map::new();
            for (g, mut v) in by_gen {
                v.sort_by_key(|r| (if r.split == "train" { 1 } else { 0 }, order(&r.split)));
                let sha = write_jsonl(&out.join("requests").join(format!("{g}.jsonl")), &v)?;
                req_files.insert(g, json!({"requests":v.len(),"sha256":sha}));
            }
            // coverage tables
            let mut cov: BTreeMap<String, BTreeMap<String, BTreeMap<String, usize>>> = BTreeMap::new();
            let mut bump = |split: &str, dim: &str, val: String| {
                *cov.entry(split.into()).or_default().entry(dim.into()).or_default().entry(val).or_default() += 1;
            };
            for it in &p.items {
                bump(&it.split, "cohort", it.cohort.clone());
                bump(&it.split, &format!("generator:{}", it.cohort), it.generator.clone());
                if let Some(st) = &it.style {
                    let sk = st["style_kind"].as_str().unwrap_or("-").to_owned();
                    bump(&it.split, &format!("style_kind:{}", it.cohort), sk.clone());
                    bump(&it.split, &format!("default_voice:{}", it.cohort), st["default_voice"].to_string());
                    if let Some(a) = st["voice_axis"].as_str() {
                        bump(&it.split, "voice_axis", a.into());
                        bump(&it.split, "voice_inventor", st["voice_inventor"].as_str().unwrap_or("?").into());
                    }
                    if let Some(i) = st["brief_inventor"].as_str() {
                        bump(&it.split, "brief_inventor", i.into());
                        bump(&it.split, "brief_genre", st["brief_genre"].as_str().unwrap_or("?").into());
                    }
                    if let Some(m) = st["modifier"]["type"].as_str() {
                        bump(&it.split, "voice_plus_modifier", m.into());
                    }
                    if st["voice_imitated_author"].is_string() {
                        bump(&it.split, "literary_imitation_items", it.cohort.clone());
                    }
                    if let Some(dt) = st["document_type"].as_str() {
                        bump(&it.split, "mirror_document_type", dt.into());
                        bump(&it.split, "mirror_generator_x_document_type", format!("{}|{}", it.generator, dt));
                        bump(&it.split, "mirror_document_type_x_style_kind", format!("{}|{}", dt, sk));
                    }
                    if let Some(op) = &it.operation {
                        bump(&it.split, "rewrite_operation_x_style_kind", format!("{}|{}", op, sk));
                    }
                    bump(&it.split, &format!("generator_x_style_kind:{}", it.cohort), format!("{}|{}", it.generator, sk));
                }
                if let Some(op) = &it.operation {
                    bump(&it.split, "rewrite_operation", op.clone());
                    bump(&it.split, &format!("rewrite_bin:{}", it.cohort), it.bin.clone().unwrap_or_default());
                    bump(&it.split, "rewrite_generator_x_operation", format!("{}|{}", it.generator, op));
                    bump(&it.split, "rewrite_operation_x_bin", format!("{}|{}", op, it.bin.clone().unwrap_or_default()));
                    bump(&it.split, "rewrite_generator_x_operation_x_bin", format!("{}|{}|{}", it.generator, op, it.bin.clone().unwrap_or_default()));
                }
                if it.cohort == "train_base_v2" {
                    bump(&it.split, "base_generator_x_kind", format!("{}|{}", it.generator, it.kind));
                }
            }
            let mut humans_by: BTreeMap<String, BTreeMap<&str, usize>> = BTreeMap::new();
            let mut human_genres: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
            for h in &humans {
                *humans_by.entry(h.split.clone()).or_default().entry(h.domain).or_default() += 1;
                *human_genres.entry(h.split.clone()).or_default().entry(h.genre.clone()).or_default() += 1;
            }
            // factorial checks: voice must be independent of generator
            let mut factorial = serde_json::Map::new();
            for split in plan::SPLITS {
                let styled: Vec<&plan::Item> = p.items.iter().filter(|it| it.split == split && it.style.as_ref().is_some_and(|s| s["style_kind"].is_string() && s["style_policy"].is_string() && s["style_kind"] != "base_checkpoint")).collect();
                let lab = |it: &plan::Item, k: &str| it.style.as_ref().unwrap()[k].as_str().unwrap_or("-").to_owned();
                let mut f = serde_json::Map::new();
                let sk: Vec<(String, String)> = styled.iter().map(|it| (it.generator.clone(), lab(it, "style_kind"))).collect();
                f.insert("generator_x_style_kind".into(), diverse::factorial_v(&sk, &format!("{split}:sk")));
                let ax: Vec<(String, String)> = styled.iter().filter(|it| it.style.as_ref().unwrap()["voice_axis"].is_string()).map(|it| (it.generator.clone(), lab(it, "voice_axis"))).collect();
                f.insert("generator_x_voice_axis".into(), diverse::factorial_v(&ax, &format!("{split}:ax")));
                let vc: Vec<(String, String)> = styled.iter().filter(|it| it.style.as_ref().unwrap()["voice_cluster"].is_string()).map(|it| (it.generator.clone(), lab(it, "voice_cluster"))).collect();
                f.insert("generator_x_voice_cluster".into(), diverse::factorial_v(&vc, &format!("{split}:vc")));
                let bc: Vec<(String, String)> = styled.iter().filter(|it| it.style.as_ref().unwrap()["brief_cluster"].is_string()).map(|it| (it.generator.clone(), lab(it, "brief_cluster"))).collect();
                f.insert("generator_x_brief_cluster".into(), diverse::factorial_v(&bc, &format!("{split}:bc")));
                let ladders: std::collections::BTreeSet<String> = styled.iter().filter_map(|it| it.style.as_ref().unwrap()["ladder_id"].as_str().map(String::from)).collect();
                let crosses: std::collections::BTreeSet<String> = styled.iter().filter_map(|it| it.style.as_ref().unwrap()["cross_id"].as_str().map(String::from)).collect();
                let vclusters: std::collections::BTreeSet<String> = vc.iter().map(|x| x.1.clone()).collect();
                let mut gens_per_vc: BTreeMap<&str, std::collections::BTreeSet<&str>> = BTreeMap::new();
                for (g, c) in &vc {
                    gens_per_vc.entry(c.as_str()).or_default().insert(g.as_str());
                }
                let multi = gens_per_vc.values().filter(|s| s.len() >= 2).count();
                f.insert("voice_ladders".into(), json!(ladders.len()));
                f.insert("cross_generator_groups".into(), json!(crosses.len()));
                f.insert("voice_clusters_used".into(), json!(vclusters.len()));
                f.insert("voice_clusters_used_by_two_or_more_generators".into(), json!(multi));
                factorial.insert(split.into(), Value::Object(f));
            }
            let meta = json!({"schema":diverse::PLAN_V2_SCHEMA,"code":CODE_ID,"inputs":meta0["inputs"],
                "producer_sha256":{"plan.rs":text::sha_hex(include_bytes!("plan.rs")),"diverse.rs":text::sha_hex(include_bytes!("diverse.rs")),"text.rs":text::sha_hex(include_bytes!("text.rs"))},
                "library":{"path":fs::canonicalize(&library)?.display().to_string(),"files":lib_files,"items":lib.len(),"policy":library::POLICY},"style_policy":diverse::STYLE_POLICY,
                "fix_slop":{"revision":diverse::FIX_SLOP_REVISION,"files":diverse::FIX_SLOP_FILES.iter().map(|(n,h)| json!({"path":n,"sha256":h})).collect::<Vec<_>>()},
                "key_prefix_v2":diverse::V2_KEY_PREFIX,"key_prefix_protocol":plan::PLAN_KEY_PREFIX,"splitter":text::SPLITTER_ID,"terms":text::TERMS_ID,
                "humans":humans_by,"items":p.items.len(),"items_sha256":items_sha,"requests":p.requests.len(),"request_files":req_files,
                "plan_failures":p.failures.len(),"plan_failures_sha256":fail_sha,"coverage":cov,"factorial":factorial,"human_genres":human_genres,
                "rules":{
                    "evaluation_primary":"protocol-v1.1 positive_plan verbatim via plan::build (identical item and request ids to plan v1); the gated evaluation population",
                    "evaluation_diverse":"supplementary, not gated: one diverse mirror and one diverse rewrite per evaluation human under the split's generator cycle (Dev: 7 families; CAL: 21-slot cycle with GLM-4 in slots 0-6); voices and briefs from the split's disjoint library partition",
                    "train":"one diverse mirror and one diverse rewrite per Train human; generator rank mod 8 over Train instruct families; base cohort rank(k_base) mod 4 == 0, continuation/splice over Granite/SmolLM3/ERNIE base (default voice)",
                    "mirror":"document type rank(k_mirror_doc) mod 72: 0-35 native (two phrasings), 36-71 the 36 off-domain types; length = human word count; topic = title plus 8 TF-IDF key terms; style kind from rank(k_mirror_none) and rank(k_mirror_kind)",
                    "rewrite":"operation rank(k_mixed_op) mod 17 over weighted slots paraphrase x3, expand x2, condense x2, continue x2, humanize x2 (3 phrasings), casual, simplify, polish, formalize, fix_slop (pinned bundle), spelling (British/American); bin rank(k_mixed_bin) mod 7 over R0 (0,10) target 5%, R1-R6 protocol spans; two spans when rank(k_multi_span) mod 3 == 0 in R2-R4 except continue; continue replaces a document-final run (R5 keeps the first sentence; R6 falls back to paraphrase); voice/brief only for paraphrase, expand, condense, continue, casual, humanize, simplify (others are operation_only)",
                    "style":"style-capable items: none when rank(k_none) mod 4 == 0 (25%); else rank(k_kind) mod 20: 0-7 short voice prompt (40%), 8-10 voice plus one modifier (15%: format constraint only for mirrors and full rewrites, else mood or audience), 11-12 persona combination voice x setting x mood (10%), 13-19 free-form genre brief (35%); wrapper phrasing (5), style position (mirrors: before or after the task), modifier and setting from per-item keyed hashes; rewrites use only rewrite_safe library items; Train never uses items whose imitated author matches an evaluation PG author",
                    "decoding":"per item from SHA-256(v2 prefix:decoding:item_id): temperature U[0.5, t_max(generator)] (1.2 Qwen/ERNIE/gpt-oss, 1.1 Mistral/GLM/Gemma, 1.0 Phi/SmolLM3/Granite), top_p U[0.85,1], min_p U[0,0.1] floored at 0.05 when temperature > 1, repeat_penalty U[1,1.15], top_k 0; base temperature U[0.6,1.0], max 1024",
                    "library_assignment":"round-robin within (split partition, kind, genre, rewrite) lists in key order; voices interleaved by axis, briefs by inventor; a human-derived item is never applied to a parent of its own source family; ladder rungs take items from clusters not yet used in that ladder (relaxations counted in plan failures)",
                    "voice_ladders":"Train and Dev-iter humans with rank(k_ladder) mod 8 == 0: three extra rewrite rungs (same spans, generator, decoding and operation, or paraphrase when the base operation carries no voice) and one extra mirror rung (same document type, generator and decoding), rung style kinds rotated over voice, persona_combo, brief, voice_plus",
                    "cross_generator":"Train humans with rank(k_cross) mod 12 == 0: the base rewrite (same spans, operation and exact style) under the generators 3 and 5 places further in the 8-family Train cycle, decoding sampled per generator",
                    "selection":"no detector or Pangram score selects any brief, item or output"
                }});
            write_json(&out.join("plan.json"), &meta)?;
            println!("{}", serde_json::to_string(&json!({"items":p.items.len(),"requests":p.requests.len(),"plan_failures":p.failures.len(),"request_files":meta["request_files"]}))?);
            Ok(())
}

/// Plan v3 (`plan-v3`). Instruct mode builds the exact-span supervision
/// cohorts. Base-model mode (non-empty `base_generators`) builds only the
/// Train base cohort. The arguments are checked before any file is opened
/// (`span_ops::check_plan_v3_args`: `--dev-select` is refused, and base-model
/// mode reads Train and CAL only); then only the given inputs are read.
#[allow(clippy::too_many_arguments)]
fn run_plan_v3(
    train: &Path,
    dev_iter: Option<&Path>,
    dev_select: Option<&Path>,
    cal: Option<&Path>,
    splits: &[String],
    prefix_every: Option<usize>,
    middle_every: Option<usize>,
    base_generators: &[String],
    out: &Path,
) -> Result<()> {
    let given = [("train", Some(train)), ("dev_iter", dev_iter), ("dev_select", dev_select), ("cal", cal)];
    let names: Vec<&str> = given.iter().filter(|(_, p)| p.is_some()).map(|(n, _)| *n).collect();
    let gens = span_ops::check_plan_v3_args(&names, splits, prefix_every, middle_every, base_generators)?;
    let base = !gens.is_empty();
    fs::create_dir_all(out.join("requests"))?;
    let mut inputs = serde_json::Map::new();
    for (split, p) in given {
        if let Some(p) = p {
            inputs.insert(split.into(), json!({"path":fs::canonicalize(p)?.display().to_string(),"sha256":file_sha(p)?}));
        }
    }
    let meta0 = json!({"inputs":inputs});
    let (humans, _) = load_humans(&meta0)?;
    if base {
        return write_plan_v3_base(&humans, &gens, &meta0, splits, out);
    }
    let (prefix_every, middle_every) = (prefix_every.unwrap_or(2), middle_every.unwrap_or(2));
    let stop = text::stopwords();
    let mut idf: HashMap<&'static str, text::Idf> = HashMap::new();
    for d in plan::DOMAINS {
        idf.insert(d, text::Idf::default());
    }
    for h in humans.iter().filter(|h| h.split == "train") {
        idf.get_mut(h.domain).unwrap().add(&h.text, &stop);
    }
    let p = span_ops::build_v3(&span_ops::V3Inputs { humans: &humans, idf: &idf, splits, prefix_every, middle_every })?;
    let items_sha = write_jsonl(&out.join("items.jsonl"), &p.items)?;
    let fail_sha = write_jsonl(&out.join("plan-failures.jsonl"), &p.failures)?;
    let mut by_gen: BTreeMap<String, Vec<&plan::Request>> = BTreeMap::new();
    for r in &p.requests {
        by_gen.entry(r.generator.clone()).or_default().push(r);
    }
    let mut req_files = serde_json::Map::new();
    for (g, v) in by_gen {
        let sha = write_jsonl(&out.join("requests").join(format!("{g}.jsonl")), &v)?;
        req_files.insert(g, json!({"requests":v.len(),"sha256":sha}));
    }
    let mut cov: BTreeMap<String, BTreeMap<String, BTreeMap<String, usize>>> = BTreeMap::new();
    let mut bump = |split: &str, dim: &str, val: String| {
        *cov.entry(split.into()).or_default().entry(dim.into()).or_default().entry(val).or_default() += 1;
    };
    for it in &p.items {
        let op = it.operation.clone().unwrap_or_default();
        let st = it.style.clone().unwrap_or_default();
        bump(&it.split, "cohort", it.cohort.clone());
        bump(&it.split, "operation", op.clone());
        bump(&it.split, &format!("generator:{}", it.cohort), it.generator.clone());
        bump(&it.split, &format!("domain:{}", it.cohort), it.domain.clone());
        let fr = it.requested_fraction.unwrap_or(0.0);
        bump(&it.split, &format!("replaced_fraction_decile:{op}"), format!("{}", ((fr * 10.0).floor() as i64).min(9)));
        if let Some(k) = st["kept_prefix_bp"].as_u64() {
            bump(&it.split, "kept_prefix_decile", format!("{}", (k / 1000).min(9)));
        }
    }
    let mut fails: BTreeMap<String, usize> = BTreeMap::new();
    for f in &p.failures {
        *fails.entry(format!("{}:{}", f["cohort"].as_str().unwrap_or("?"), f["reason"].as_str().unwrap_or("?"))).or_default() += 1;
    }
    let mut humans_by: BTreeMap<String, BTreeMap<&str, usize>> = BTreeMap::new();
    for h in humans.iter().filter(|h| splits.contains(&h.split)) {
        *humans_by.entry(h.split.clone()).or_default().entry(h.domain).or_default() += 1;
    }
    let meta = json!({"schema":span_ops::PLAN_V3_SCHEMA,"code":CODE_ID,"rules_id":span_ops::RULES_ID,"inputs":meta0["inputs"],
        "producer_sha256":{"plan.rs":text::sha_hex(include_bytes!("plan.rs")),"diverse.rs":text::sha_hex(include_bytes!("diverse.rs")),
            "span_ops.rs":text::sha_hex(include_bytes!("span_ops.rs")),"text.rs":text::sha_hex(include_bytes!("text.rs"))},
        "key_prefix_v3":span_ops::V3_KEY_PREFIX,"splitter":text::SPLITTER_ID,"terms":text::TERMS_ID,"planned_splits":splits,
        "prefix_every":prefix_every,"middle_every":middle_every,"humans":humans_by,"items":p.items.len(),"items_sha256":items_sha,
        "requests":p.requests.len(),"request_files":req_files,"plan_failures":p.failures.len(),"plan_failures_by_reason":fails,
        "plan_failures_sha256":fail_sha,"coverage":cov,
        "attribution":"prefix-continuation and interior-span regeneration with exact word labels follow an idea from the MIT-licensed It's AI validator (https://github.com/It-s-AI/llm-detection); selection, prompts, screens and records are this planner's own",
        "rules":{
            "scope":"additive plan: only the two cohorts below, keyed under the v3 prefix so item and request ids never collide with plans v1/v2; Train by default, Dev-iter only when requested and then supplementary and ungated; sealed splits refused",
            "prefix_continuation":"humans with rank(k_prefix) mod prefix_every == 0; target kept-prefix fraction drawn per item uniformly in [30%, 70%] of words (SHA-256 v3 prefix:prefix_bp:item_id); the whole-sentence boundary closest to the target inside [30%, 70%] (ties shorter); the model writes the remaining words' count; no boundary in range is a recorded plan failure",
            "middle_rewrite":"humans with rank(k_middle) mod middle_every == 0; one interior whole-sentence run with at least one sentence and 10% of the words kept on each side; run size target drawn in [15%, 50%] and midpoint target in [30%, 70%]; mode by rank(k_middle_mode) mod 2: middle_rewrite shows the run (rewrite in context), middle_infill hides it and gives up to 6 TF-IDF key terms of the run",
            "generators":"Train: rank(k_*_gen) mod 8 over the Train instruct families; Dev-iter: mod 7 over the Dev families; decoding sampled per item with the plan-v2 sampler",
            "acceptance":"plan-v2 checks plus: no 12-word window shared with the kept human context; for prefix_continue and middle_infill also none with the removed run (recalled human text would be mislabelled as model text); middle_rewrite records but allows windows of the shown run",
            "labels":"realised final-word share and exact per-word labels (share_evidence.word_label_runs) from the assembled replacement span; never the requested fraction",
            "selection":"no detector or Pangram score selects any item or output"
        }});
    write_json(&out.join("plan.json"), &meta)?;
    println!("{}", serde_json::to_string(&json!({"items":p.items.len(),"requests":p.requests.len(),"plan_failures":p.failures.len(),"request_files":meta["request_files"]}))?);
    Ok(())
}

/// Base-model mode of plan v3: writes the `train_base_v3` items, requests
/// (one file per base family), plan failures and `plan.json`.
fn write_plan_v3_base(humans: &[plan::Human], gens: &[&'static str], meta0: &Value, splits: &[String], out: &Path) -> Result<()> {
    let p = span_ops::build_v3_base(humans, gens)?;
    let items_sha = write_jsonl(&out.join("items.jsonl"), &p.items)?;
    let fail_sha = write_jsonl(&out.join("plan-failures.jsonl"), &p.failures)?;
    let mut by_gen: BTreeMap<String, Vec<&plan::Request>> = BTreeMap::new();
    for r in &p.requests {
        by_gen.entry(r.generator.clone()).or_default().push(r);
    }
    let mut req_files = serde_json::Map::new();
    for (g, v) in by_gen {
        let sha = write_jsonl(&out.join("requests").join(format!("{g}.jsonl")), &v)?;
        req_files.insert(g, json!({"requests":v.len(),"sha256":sha}));
    }
    let mut cov: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut bump = |dim: &str, val: String| {
        *cov.entry(dim.into()).or_default().entry(val).or_default() += 1;
    };
    for it in &p.items {
        bump("generator", it.generator.clone());
        bump("kind", it.kind.clone());
        bump("generator_x_kind", format!("{}|{}", it.generator, it.kind));
        bump("domain", it.domain.clone());
        bump("domain_x_generator_x_kind", format!("{}|{}|{}", it.domain, it.generator, it.kind));
        let fr = it.requested_fraction.unwrap_or(0.0);
        bump(&format!("replaced_fraction_decile:{}", it.kind), format!("{}", ((fr * 10.0).floor() as i64).min(9)));
    }
    let mut fails: BTreeMap<String, usize> = BTreeMap::new();
    for f in &p.failures {
        let k = format!("{}:{}:{}", f["cohort"].as_str().unwrap_or("?"), f["kind"].as_str().unwrap_or("?"), f["reason"].as_str().unwrap_or("?"));
        *fails.entry(k).or_default() += 1;
    }
    let mut humans_by: BTreeMap<String, BTreeMap<&str, usize>> = BTreeMap::new();
    for h in humans.iter().filter(|h| h.split == "train") {
        *humans_by.entry(h.split.clone()).or_default().entry(h.domain).or_default() += 1;
    }
    let meta = json!({"schema":span_ops::PLAN_V3_SCHEMA,"code":CODE_ID,"rules_id":span_ops::BASE_RULES_ID,"mode":"base_model","inputs":meta0["inputs"],
        "producer_sha256":{"plan.rs":text::sha_hex(include_bytes!("plan.rs")),"span_ops.rs":text::sha_hex(include_bytes!("span_ops.rs")),
            "text.rs":text::sha_hex(include_bytes!("text.rs"))},
        "key_prefix_v3":span_ops::V3_KEY_PREFIX,"key_prefix_r2_plan":span_ops::R2_PLAN_KEY_PREFIX,"base_key":span_ops::BASE_KEY,
        "splitter":text::SPLITTER_ID,"planned_splits":splits,"base_generators":gens,"prefix_every":null,"middle_every":null,
        "cohort":span_ops::COHORT_BASE,"humans":humans_by,"items":p.items.len(),"items_sha256":items_sha,"requests":p.requests.len(),
        "request_files":req_files,"plan_failures":p.failures.len(),"plan_failures_by_reason":fails,"plan_failures_sha256":fail_sha,
        "coverage":{"train":cov},
        "rules":{
            "scope":"base-model mode of plan v3 (round-2 build B6): only the Train base cohort train_base_v3 on the listed Train base families; no instruct cohort; Train only (no Dev, CAL or Test base positives); item ids from the v3 prefix with the cohort name, so they never collide with the instruct cohorts or plans v1/v2",
            "humans":"every Train human gets one assignment; rank = rank of SHA-256('slop_ninja_a_comp_r2_plan_v1:base_cohort:' + id) within (train, domain) (protocol A2 V21, unkeyed); generator = base_generators[rank mod G] in Train base order; continuation when floor(rank / G) mod 2 == 0, else splice, so the kinds are 1:1 (to within one) in every domain x generator cell; the per-family target (v_fam accepted rows, counting the family's existing base rows) is reached by stratify-train passes, not here",
            "inputs":"Train, and CAL only as generate's overlap extras; Dev-iter and Dev-select refused",
            "continuation":"Tier C robustness rule: prefix = shortest whole-sentence prefix with >= 30% of words; the base model continues it on /completion with the verbatim prefix and no template; output cut at the last sentence end keeping the document within the human word count (budget = removed words)",
            "splice":"Tier C robustness rule: prefix and suffix = shortest whole-sentence prefix and suffix with >= 25% of words each; the middle is generated from the verbatim prefix on /completion and cut at the last sentence end not exceeding the removed words; the human suffix follows unchanged",
            "plan_failures":"a passage with no such prefix (and suffix) plan is a recorded failure; no other kind, generator or text is substituted",
            "decoding":"Tier C robustness decoding (plan::robustness_decoding): temperature 0.8, top_p 0.95, top_k 0, min_p 0, repeat_penalty 1.0, max_tokens 1024; seed = first four bytes of the request SHA-256 big-endian; second attempt seed + 1",
            "acceptance":"the generate completion path: content cut to the budget at a sentence end; words within [0.5, 1.5] of the removed words; continuation >= 20 words; not byte-identical to the replaced span; no 12-word window shared with the removed human text (Train-side screen); duplicate and prior-overlap screens; assembled document <= 2048 A-tokenizer tokens; at most two attempts; failures terminal",
            "labels":"exact spans: finalize writes share_evidence.word_label_runs for this cohort (span_ops::COHORTS); the realised final-word share, never the requested fraction",
            "selection":"no detector or Pangram score selects any item or output"
        }});
    write_json(&out.join("plan.json"), &meta)?;
    println!("{}", serde_json::to_string(&json!({"items":p.items.len(),"requests":p.requests.len(),"plan_failures":p.failures.len(),"request_files":meta["request_files"]}))?);
    Ok(())
}

/// Test plan schema (protocol-v1.json `positive_plan`, Test cycle).
const TEST_PLAN_SCHEMA: &str = "slop_ninja_a_comp_positive_plan_test_v1";

fn run_plan_test(train: &Path, test: &Path, seal: &Path, seal_sha256: &str, out: &Path) -> Result<()> {
    ensure!(seal.is_file(), "seal not written: {} (Test is sealed until SEAL.json exists)", seal.display());
    let seal_sha = file_sha(seal)?;
    ensure!(seal_sha == seal_sha256.to_ascii_lowercase(), "seal sha256 {seal_sha} does not match --seal-sha256 {seal_sha256}");
    ensure!(!out.join("plan.json").exists(), "Test plan already written at {}", out.display());
    fs::create_dir_all(out.join("requests"))?;
    let mut inputs = serde_json::Map::new();
    inputs.insert("test".into(), json!({"path":fs::canonicalize(test)?.display().to_string(),"sha256":file_sha(test)?}));
    let meta0 = json!({"inputs":inputs});
    let (humans, _) = load_humans(&meta0)?;
    ensure!(!humans.is_empty(), "no Test humans");
    // IDF from each evaluation domain's Train pool (Train rows mapped to evaluation domains by collection).
    let stop = text::stopwords();
    let mut idf: HashMap<&'static str, text::Idf> = plan::EVAL_DOMAINS.iter().map(|d| (*d, text::Idf::default())).collect();
    let mut idf_rows = 0usize;
    for row in read_jsonl(train)? {
        let h = plan::human_from(&row, "train")?;
        if let Some(d) = plan::eval_domain_of(row["collection"].as_str().context("collection")?) {
            idf.get_mut(d).unwrap().add(&h.text, &stop);
            idf_rows += 1;
        }
    }
    let p = plan::build_test(&humans, &idf)?;
    let items_sha = write_jsonl(&out.join("items.jsonl"), &p.items)?;
    let fail_sha = write_jsonl(&out.join("plan-failures.jsonl"), &p.failures)?;
    let mut by_gen: BTreeMap<String, Vec<&plan::Request>> = BTreeMap::new();
    for r in &p.requests {
        by_gen.entry(r.generator.clone()).or_default().push(r);
    }
    let mut req_files = serde_json::Map::new();
    for (g, v) in by_gen {
        let sha = write_jsonl(&out.join("requests").join(format!("{g}.jsonl")), &v)?;
        req_files.insert(g, json!({"requests":v.len(),"sha256":sha}));
    }
    // Quotas: assignments (planned items plus plan failures) against the protocol rule.
    let mut n_by: BTreeMap<&str, usize> = BTreeMap::new();
    for h in &humans {
        *n_by.entry(h.domain).or_default() += 1;
    }
    let mut assigned: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    let mut bins: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut kinds: BTreeMap<(String, String), usize> = BTreeMap::new();
    for it in &p.items {
        *assigned.entry((it.domain.clone(), it.cohort.clone(), it.generator.clone())).or_default() += 1;
        if let Some(b) = &it.bin {
            *bins.entry((it.domain.clone(), b.clone())).or_default() += 1;
        }
        if it.cohort == plan::TEST_ROBUSTNESS_COHORT {
            *kinds.entry((it.domain.clone(), it.kind.clone())).or_default() += 1;
        }
    }
    let dom_of: HashMap<&str, &str> = humans.iter().map(|h| (h.id.as_str(), h.domain)).collect();
    for f in &p.failures {
        let d = dom_of[f["parent_id"].as_str().context("parent_id")?].to_owned();
        let c = f["cohort"].as_str().context("cohort")?.to_owned();
        let g = f["generator"].as_str().context("generator")?.to_owned();
        if let Some(b) = f["bin"].as_str() {
            *bins.entry((d.clone(), b.to_owned())).or_default() += 1;
        }
        if let Some(k) = f["kind"].as_str() {
            *kinds.entry((d.clone(), k.to_owned())).or_default() += 1;
        }
        *assigned.entry((d, c, g)).or_default() += 1;
    }
    let mut quotas = serde_json::Map::new();
    let mut tot = [0usize; 7];
    for (d, &n) in &n_by {
        let (olmo, robust, frontier) = plan::test_quota(n);
        let get = |c: &str, g: &str| assigned.get(&(d.to_string(), c.to_string(), g.to_string())).copied().unwrap_or(0);
        let mirror: usize = assigned.iter().filter(|((dd, c, _), _)| dd == d && c == "primary_mirror").map(|(_, v)| v).sum();
        let mixed: usize = assigned.iter().filter(|((dd, c, _), _)| dd == d && c == "primary_mixed").map(|(_, v)| v).sum();
        let om = get("primary_mirror", plan::TEST_ONLY_INSTRUCT);
        let ox = get("primary_mixed", plan::TEST_ONLY_INSTRUCT);
        let rb = get(plan::TEST_ROBUSTNESS_COHORT, plan::TEST_ONLY_BASE);
        ensure!(mirror == n && mixed == n, "{d}: {mirror} mirror / {mixed} mixed assignments for {n} humans");
        ensure!(om == olmo && ox == olmo, "{d}: OLMo-2 instruct assignments {om}/{ox}, rule {olmo}");
        ensure!(rb == robust, "{d}: robustness assignments {rb}, rule {robust}");
        let bin_counts: BTreeMap<&str, usize> = ["R1", "R2", "R3", "R4", "R5", "R6"]
            .iter()
            .map(|b| (*b, bins.get(&(d.to_string(), b.to_string())).copied().unwrap_or(0)))
            .collect();
        let kind_counts: BTreeMap<&str, usize> = ["continuation", "splice"]
            .iter()
            .map(|k| (*k, kinds.get(&(d.to_string(), k.to_string())).copied().unwrap_or(0)))
            .collect();
        let gens: BTreeMap<String, [usize; 2]> = {
            let mut m: BTreeMap<String, [usize; 2]> = BTreeMap::new();
            for ((dd, c, g), v) in &assigned {
                if dd == d && (c == "primary_mirror" || c == "primary_mixed") {
                    m.entry(g.clone()).or_default()[usize::from(c == "primary_mixed")] += v;
                }
            }
            m
        };
        quotas.insert((*d).into(), json!({"humans":n,"mirror":mirror,"mixed":mixed,"olmo2_instruct":[om, ox],"robustness":rb,
            "frontier_rule_ceil_n_over_4":frontier,"mixed_bins":bin_counts,"robustness_kinds":kind_counts,"generators_mirror_mixed":gens}));
        for (k, v) in [n, mirror, mixed, om, ox, rb, frontier].into_iter().enumerate() {
            tot[k] += v;
        }
    }
    quotas.insert("total".into(), json!({"humans":tot[0],"mirror":tot[1],"mixed":tot[2],"olmo2_instruct":[tot[3], tot[4]],
        "robustness":tot[5],"frontier_rule_ceil_n_over_4":tot[6]}));
    let idf_meta: BTreeMap<&str, Value> =
        idf.iter().map(|(d, t)| (*d, json!({"train_documents":t.n_docs,"types":t.df.len(),"sha256":t.digest()}))).collect();
    let meta = json!({"schema":TEST_PLAN_SCHEMA,"code":CODE_ID,"inputs":meta0["inputs"],
        "idf_source":{"train":{"path":fs::canonicalize(train)?.display().to_string(),"sha256":file_sha(train)?,"rows_in_evaluation_domains":idf_rows}},
        "seal":{"path":fs::canonicalize(seal)?.display().to_string(),"sha256":seal_sha},
        "producer_sha256":{"plan.rs":text::sha_hex(include_bytes!("plan.rs")),"text.rs":text::sha_hex(include_bytes!("text.rs"))},
        "key_prefix":plan::PLAN_KEY_PREFIX,"splitter":text::SPLITTER_ID,"terms":text::TERMS_ID,"stopwords_sha256":text::sha_hex(text::STOPWORDS_V1.join("\n").as_bytes()),
        "idf":idf_meta,"humans":n_by,"items":p.items.len(),"items_sha256":items_sha,"requests":p.requests.len(),
        "request_files":req_files,"plan_failures":p.failures.len(),"plan_failures_sha256":fail_sha,"quotas":quotas,
        "rules":{
            "generator_cycle":"slot = rank(k_mirror_gen) mod 21 and rank(k_mixed_gen) mod 21 within (test, domain); slots 0-6 OLMo-2-13B-Instruct (olmo2_instruct), 7-20 Qwen3.6, Phi-4-mini, Mistral-7B-v0.3, Gemma-3-4b, Granite 4.0 H-Tiny, SmolLM3-3B, ERNIE-4.5 two slots each",
            "mixed":"bin [R1..R6][rank(k_mixed_bin) mod 6]; R1-R4 one whole-sentence run closest to the bin midpoint, shorter on ties, anchored by SHA-256(id:bin)[0] mod 3; two runs of half the target at start and end when rank(k_multi_span) mod 3 == 0 in R2-R4 and they fit; R5 keeps one sentence; R6 full rewrite; one request per span",
            "mirror":"protocol mirror template with the protocol register of the evaluation domain; title sentence omitted without a title; eight TF-IDF key terms with IDF from the evaluation domain's Train pool",
            "decoding":"profile rank(k_decoding) mod 2: A temperature 0.7 top_p 0.9, B temperature 1.0 top_p 0.95; top_k 0, min_p 0, repeat_penalty 1.0, max_tokens 1536; seed = first four bytes of the request SHA-256 big-endian; second attempt seed + 1",
            "robustness":"rank(k_robust) mod 2 == 0; continuation when floor(rank(k_robust) / 2) mod 2 == 0 else splice; OLMo-2-13B base (olmo2_base) on /completion with the verbatim prefix and no template; continuation prefix = shortest whole-sentence prefix with >= 30% of words, output cut at the last sentence end keeping the document within the human word count; splice prefix and suffix = shortest whole-sentence prefix/suffix with >= 25%, middle cut at the last sentence end not exceeding the removed words; temperature 0.8 top_p 0.95 max_tokens 1024; population outside primary",
            "acceptance":"trimmed content non-empty; no chat-template special tokens; words within [0.5, 1.5] of the target; not byte-identical to the replaced span; not a duplicate (generated texts of every run root and the prior index plus Test humans, the parent excluded for span requests); assembled document <= 2048 A-tokenizer tokens; continuation >= 20 words; at most two attempts, the second with seed + 1; failures retained as terminal, no substitute generator, bin or text",
            "frontier":"not planned here: the frontier collector's Test plan covers rank(k_frontier) mod 4 == 0"
        }});
    write_json(&out.join("plan.json"), &meta)?;
    println!("{}", serde_json::to_string(&json!({"items":p.items.len(),"requests":p.requests.len(),"plan_failures":p.failures.len(),
        "request_files":meta["request_files"],"quotas_total":meta["quotas"]["total"]}))?);
    Ok(())
}

/// Re-plan an existing v1, v2 or v3 plan from its recorded inputs into `out`
/// and compare items, plan failures and every request file by SHA-256; on a
/// mismatch, list the first differing items by id and field. A v3 plan is
/// re-planned with its recorded splits, rates and base generators (none in
/// plans written before base-model mode), from the inputs it recorded only.
fn replan_check(pdir: &Path, library: Option<&Path>, fix_slop: Option<&Path>, out: &Path) -> Result<bool> {
    ensure!(!out.exists(), "scratch output {} must not exist", out.display());
    let meta: Value = serde_json::from_slice(&fs::read(pdir.join("plan.json"))?)?;
    ensure!(meta.get("split_copy").is_none() && meta.get("mirror_regeneration").is_none(),
        "{} is a split copy or a D4 plan: replan-check re-plans from every split's input; check its source plan", pdir.display());
    let input = |sp: &str| -> Result<PathBuf> {
        let i = &meta["inputs"][sp];
        let p = PathBuf::from(i["path"].as_str().with_context(|| format!("input {sp}"))?);
        ensure!(file_sha(&p)? == i["sha256"], "input {sp} changed since planning");
        Ok(p)
    };
    let all4 = || -> Result<(PathBuf, PathBuf, PathBuf, PathBuf)> { Ok((input("train")?, input("dev_iter")?, input("dev_select")?, input("cal")?)) };
    match meta["schema"].as_str() {
        Some(plan::PLAN_SCHEMA) => {
            let (tr, di, ds, ca) = all4()?;
            run_plan_v1(&tr, &di, &ds, &ca, out)?
        }
        Some(diverse::PLAN_V2_SCHEMA) => {
            let (tr, di, ds, ca) = all4()?;
            let lib = library.map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(meta["library"]["path"].as_str().unwrap_or_default()));
            for (f, v) in meta["library"]["files"].as_object().context("library files")? {
                ensure!(file_sha(&lib.join(f))? == v["sha256"], "library file {f} changed since planning");
            }
            run_plan_v2(&tr, &di, &ds, &ca, &lib, fix_slop.context("--fix-slop is required for a v2 plan")?, out)?
        }
        Some(span_ops::PLAN_V3_SCHEMA) => {
            let opt = |sp: &str| -> Result<Option<PathBuf>> { if meta["inputs"][sp].is_null() { Ok(None) } else { input(sp).map(Some) } };
            let strs = |k: &str| -> Result<Vec<String>> {
                match &meta[k] {
                    Value::Null => Ok(Vec::new()),
                    v => v.as_array().with_context(|| format!("plan.json {k}"))?.iter().map(|x| x.as_str().map(String::from).with_context(|| format!("plan.json {k}"))).collect(),
                }
            };
            let splits = strs("planned_splits")?;
            let base = strs("base_generators")?;
            let rate = |k: &str| -> Result<Option<usize>> {
                if base.is_empty() { Ok(Some(meta[k].as_u64().with_context(|| format!("plan.json {k}"))? as usize)) } else { Ok(None) }
            };
            // A recorded dev_select input (plans written before plan-v3
            // refused it) is never opened: no v3 item uses it (section 2.4).
            let (di, ca) = (opt("dev_iter")?, opt("cal")?);
            run_plan_v3(&input("train")?, di.as_deref(), None, ca.as_deref(), &splits, rate("prefix_every")?, rate("middle_every")?, &base, out)?
        }
        s => anyhow::bail!("unknown plan schema {s:?}"),
    }
    let new: Value = serde_json::from_slice(&fs::read(out.join("plan.json"))?)?;
    let mut checks = serde_json::Map::new();
    let mut ok = true;
    for k in ["items_sha256", "plan_failures_sha256", "items", "requests", "plan_failures"] {
        let same = meta[k] == new[k];
        ok &= same;
        checks.insert(k.into(), json!({"existing":meta[k],"replanned":new[k],"identical":same}));
    }
    let same_files = meta["request_files"] == new["request_files"];
    ok &= same_files;
    checks.insert("request_files".into(), json!({"identical":same_files,"generators":meta["request_files"].as_object().map(|m| m.len())}));
    // Field comparison (needed when an older build serialised fewer fields):
    // every field of an existing row must be equal in the re-planned row;
    // fields the older build did not write are listed with their values' kinds.
    let mut field_report = serde_json::Map::new();
    let mut fields_ok = true;
    let mut cmp = |name: String, old_p: &Path, new_p: &Path, key: &str| -> Result<()> {
        if file_sha(old_p)? == file_sha(new_p)? {
            field_report.insert(name, json!({"bytes_identical":true}));
            return Ok(());
        }
        let old: HashMap<String, Value> =
            read_jsonl(old_p)?.into_iter().map(|v| (v[key].as_str().unwrap_or_default().to_owned(), v)).collect();
        let mut seen = 0usize;
        let mut diffs = Vec::new();
        let mut new_only: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
        for v in read_jsonl(new_p)? {
            let id = v[key].as_str().unwrap_or_default().to_owned();
            let Some(o) = old.get(&id) else {
                diffs.push(json!({"id":id,"diff":"replanned_only"}));
                continue;
            };
            seen += 1;
            let (om, nm) = (o.as_object().context("row")?, v.as_object().context("row")?);
            let changed: Vec<&String> = om.keys().filter(|k| om.get(k.as_str()) != nm.get(k.as_str())).collect();
            if !changed.is_empty() && diffs.len() < 20 {
                diffs.push(json!({"id":id,"fields":changed}));
            }
            for (k, x) in nm {
                if !om.contains_key(k) {
                    let kind = if x.is_null() { "null" } else if x.is_string() { "string" } else { "other" };
                    *new_only.entry(k.clone()).or_default().entry(kind.into()).or_default() += 1;
                }
            }
        }
        let missing = old.len() - seen;
        let same = diffs.is_empty() && missing == 0;
        fields_ok &= same;
        field_report.insert(name, json!({"bytes_identical":false,"rows":old.len(),"existing_fields_identical":same,
            "existing_only_rows":missing,"first_diffs":diffs,"fields_not_in_existing_rows":new_only}));
        Ok(())
    };
    cmp("items.jsonl".into(), &pdir.join("items.jsonl"), &out.join("items.jsonl"), "item_id")?;
    cmp("plan-failures.jsonl".into(), &pdir.join("plan-failures.jsonl"), &out.join("plan-failures.jsonl"), "item_id")?;
    for g in meta["request_files"].as_object().context("request_files")?.keys() {
        let f = format!("requests/{g}.jsonl");
        cmp(f.clone(), &pdir.join(&f), &out.join(&f), "request_id")?;
    }
    let not_read: Vec<&str> =
        plan::ALL_SPLITS.iter().copied().filter(|sp| !meta["inputs"][*sp].is_null() && !span_ops::plan_reads_input(meta["schema"].as_str(), sp)).collect();
    let rec = json!({"schema":"slop_ninja_a_comp_replan_check_v1","code":CODE_ID,"plan":pdir.display().to_string(),
        "plan_schema":meta["schema"],"plan_json_sha256":file_sha(&pdir.join("plan.json"))?,"scratch":out.display().to_string(),
        "recorded_inputs_not_read":not_read,
        "identical":ok,"existing_fields_identical":fields_ok,"checks":checks,"files":field_report,
        "note":"plan.json itself records producer source hashes and is expected to differ; items, plan failures and request files must be byte-identical, or, for a plan written by an older build that serialised fewer fields, every existing field of every row identical"});
    write_json(&out.join("replan-check.json"), &rec)?;
    println!("{}", serde_json::to_string(&rec)?);
    Ok(ok || fields_ok)
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Plan { train, dev_iter, dev_select, cal, out } => run_plan_v1(&train, &dev_iter, &dev_select, &cal, &out)?,
        Cmd::PlanTest { train, test, seal, seal_sha256, out } => run_plan_test(&train, &test, &seal, &seal_sha256, &out)?,
        Cmd::ReplanCheck { plan: pdir, library, fix_slop, out } => {
            if !replan_check(&pdir, library.as_deref(), fix_slop.as_deref(), &out)? {
                std::process::exit(3);
            }
        }
        Cmd::StratifyTrain { plan: pdir, runs, prior, partitions, generators, targets, values, write_stratification, stratification, floor, cohort_shares, priority_domains, domain_weights, seed, out } => {
            let mode = match (write_stratification.as_deref(), stratification) {
                (None, false) => StratMode::Off,
                (Some(p), false) => StratMode::Write(p),
                (None, true) => StratMode::Frozen,
                (Some(_), true) => anyhow::bail!("--write-stratification fixes S0-10 and --stratification reads it: not both"),
            };
            let floor = match mode {
                StratMode::Off => floor.unwrap_or(stratify::DEFAULT_FLOOR),
                _ => {
                    ensure!(matches!(floor, None | Some(stratify::S0_10_FLOOR)), "S0-10's floor is {} (amendment 27: stratified by Train human counts): --floor {} is refused", stratify::S0_10_FLOOR, floor.unwrap_or_default());
                    stratify::S0_10_FLOOR
                }
            };
            stratify_train(&pdir, &runs, &prior, &partitions, &generators, &targets, values.as_deref(), mode, floor, &cohort_shares, &priority_domains, &domain_weights, &seed, &out)?;
        }
        Cmd::Generate {
            plan: pdir,
            generator,
            config,
            runs,
            index,
            tokenizer,
            workers,
            limit,
            splits,
            extra_runs,
            cohorts,
            request_order,
            dry_run,
            profile,
            profile_population,
        } => {
            let population = form_post::profile_population(profile.as_deref(), profile_population.as_deref())?;
            let meta: Value = serde_json::from_slice(&fs::read(pdir.join("plan.json"))?)?;
            // a plan recording an old Dev-select input it would read is refused
            // before any plan file is read (B5b review finding 3)
            refuse_old_dev_select_inputs(&meta)?;
            let cfg_bytes = fs::read(&config)?;
            let cfg: Value = serde_json::from_slice(&cfg_bytes)?;
            ensure!(cfg["key"] == generator.as_str(), "config key mismatch");
            let allowed: Vec<String> =
                cfg["allowed_splits"].as_array().context("allowed_splits")?.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            let want: Vec<String> = match &splits {
                Some(s) => s.split(',').map(String::from).collect(),
                None => allowed.clone(),
            };
            // one split per request file, never dev_select or test, read from
            // the lines' bytes before any parse (ruling R2; amendment 20 item
            // 20.1; B5b review finding 3)
            let (rb, _) = split_copy::one_split_file(&pdir.join("requests").join(format!("{generator}.jsonl")), "split", split_copy::OneSplit::Rows)?;
            ensure!(meta["request_files"][&generator]["sha256"] == text::sha_hex(&rb).as_str(), "request file changed");
            let reqs: Vec<plan::Request> = jsonl_bytes(&rb)?.into_iter().map(serde_json::from_value).collect::<Result<_, _>>()?;
            for r in &reqs {
                ensure!(allowed.contains(&r.split), "generator {generator} is not allowed on split {}", r.split);
            }
            let order_file = match &request_order {
                Some(p) => {
                    let path = PathBuf::from(p.to_string_lossy().replace(order::GEN_PLACEHOLDER, &generator));
                    let bytes = fs::read(&path).with_context(|| path.display().to_string())?;
                    let ids = order::parse_order(std::str::from_utf8(&bytes)?)?;
                    Some((path, text::sha_hex(&bytes), ids))
                }
                None => None,
            };
            let order_rec = order_file.as_ref().map(|(p, h, ids)| json!({"path":p.display().to_string(),"sha256":h,"items":ids.len()}));
            let want_cohorts: Option<Vec<String>> = cohorts.as_ref().map(|c| c.split(',').map(String::from).collect());
            let (form, specs, batch) = match &population {
                Some(pop) => {
                    let (f, specs, batch) = form_for_split(&meta, &pdir, &want, pop, &generator, &reqs)?;
                    (Some(f), specs, batch)
                }
                None => (None, Vec::new(), Vec::new()),
            };
            let form_rec = form.as_ref().map(|f| f.record());
            let out = runs.join(&generator);
            if dry_run {
                let mut run_dirs = Vec::new();
                if runs.exists() {
                    for e in fs::read_dir(&runs)? {
                        let p = e?.path();
                        if p.is_dir() {
                            run_dirs.push(p);
                        }
                    }
                }
                let all = generation::load_states(&run_dirs)?;
                check_generate_binding(&runs, &want, form.as_ref(), &batch, &all)?;
                let form = form.map(|f| f.with_items(specs, &all)).transpose()?;
                let mut pending = order::select_pending(
                    reqs,
                    &want,
                    want_cohorts.as_deref(),
                    order_file.as_ref().map(|(_, _, ids)| ids.as_slice()),
                    &all,
                )?;
                let total_pending = pending.len();
                if let Some(l) = limit {
                    pending.truncate(l);
                }
                let mut items: Vec<&str> = Vec::new();
                for (r, _) in &pending {
                    if items.last() != Some(&r.item_id.as_str()) {
                        items.push(&r.item_id);
                    }
                }
                let mut rec = json!({"generator":generator,"dry_run":true,"pending_before":total_pending,
                    "would_run":pending.len(),"would_run_items":items.len(),"splits":want,"cohorts":cohorts,"request_order":order_rec,
                    "item_order":items});
                if let (Some(r), Some(f)) = (form_rec, &form) {
                    rec["profile"] = r;
                    rec["profile_prior"] = f.prior_record();
                    rec["profile_items"] = f.items_record();
                    rec["profile_binding"] = json!(form_post::binding_path(&runs, &format!("split-{}", want[0])).display().to_string());
                }
                println!("{}", serde_json::to_string(&rec)?);
                return Ok(());
            }
            // all journal states so far, every generator; the batch binding
            // is checked before anything is written
            let mut run_dirs = Vec::new();
            if runs.exists() {
                for e in fs::read_dir(&runs)? {
                    let p = e?.path();
                    if p.is_dir() {
                        run_dirs.push(p);
                    }
                }
            }
            let all = generation::load_states(&run_dirs)?;
            check_generate_binding(&runs, &want, form.as_ref(), &batch, &all)?;
            fs::create_dir_all(&out)?;
            // server identity
            let endpoint = cfg["endpoint"].as_str().context("endpoint")?.trim_end_matches('/').to_owned();
            let client = generation::client()?;
            let props: Value = client.get(format!("{endpoint}/props")).send()?.error_for_status()?.json()?;
            ensure!(props["model_path"] == cfg["model_path"], "server model path mismatch: {}", props["model_path"]);
            let models: Value = client.get(format!("{endpoint}/v1/models")).send()?.error_for_status()?.json()?;
            ensure!(
                models["data"].as_array().is_some_and(|a| a.iter().any(|m| m["id"] == cfg["alias"])),
                "server alias mismatch"
            );
            let t0 = generation::now_ms();
            write_json(&out.join(format!("server-{t0}.json")), &json!({"props":props,"models":models,"config":cfg,
                "config_sha256":text::sha_hex(&cfg_bytes),"chat_template_sha256":props["chat_template"].as_str().map(|t| text::sha_hex(t.as_bytes()))}))?;
            // parents and overlap
            let (humans, _) = load_humans(&meta)?;
            let mut parents = HashMap::new();
            let mut ov = overlap::Overlap::new(overlap::PriorIndex::load(&index)?);
            for h in &humans {
                if h.split == "cal" || h.split == "test" {
                    ov.add_extra(&h.text);
                }
                parents.insert(h.id.clone(), h.text.clone());
            }
            // the binding and the prior record, before the first request
            if let Some(f) = &form {
                form_post::write_binding(&form_post::binding_path(&runs, &format!("split-{}", want[0])), &runs, f)?;
            }
            let form = form.map(|f| f.with_items(specs, &all)).transpose()?;
            let items_rec = form.as_ref().map(|f| f.items_record());
            // all accepted generated text so far, every generator
            let mut preloaded = 0;
            for st in all.values() {
                if let Some(v) = st.accepted() {
                    ov.add_generated(v["verdict"]["text"].as_str().unwrap_or(""));
                    preloaded += 1;
                }
            }
            for root in &extra_runs {
                let mut dirs = Vec::new();
                for e in fs::read_dir(root)? {
                    let p = e?.path();
                    if p.is_dir() {
                        dirs.push(p);
                    }
                }
                for st in generation::load_states(&dirs)?.values() {
                    if let Some(v) = st.accepted() {
                        ov.add_generated(v["verdict"]["text"].as_str().unwrap_or(""));
                        preloaded += 1;
                    }
                }
            }
            let mut tok = tokenizers::Tokenizer::from_file(&tokenizer).map_err(|e| anyhow::anyhow!(e.to_string()))?;
            tok.with_truncation(None).map_err(|e| anyhow::anyhow!(e.to_string()))?;
            tok.with_padding(None);
            let mut pending = order::select_pending(
                reqs,
                &want,
                want_cohorts.as_deref(),
                order_file.as_ref().map(|(_, _, ids)| ids.as_slice()),
                &all,
            )?;
            let total_pending = pending.len();
            if let Some(l) = limit {
                pending.truncate(l);
            }
            let n = pending.len();
            eprintln!("{generator}: {total_pending} pending, running {n}, preloaded {preloaded} generated texts");
            let ((post, doc_words), vf) = form_post::generate_wiring(form)?;
            let ctx = Arc::new(generation::Ctx {
                endpoint,
                alias: cfg["alias"].as_str().unwrap().to_owned(),
                config: cfg.clone(),
                tokenizer: tok,
                overlap: ov,
                parents,
                client,
                post,
                doc_words,
            });
            let stats = generation::run_with(ctx.clone(), pending, &out, workers, vf)?;
            let mut rec = json!({"generator":generator,"t0":t0,"t_end":generation::now_ms(),"pending_before":total_pending,"ran":n,"stats":stats,
                "index_sha256":ctx.overlap.prior.sha256,"tokenizer_sha256":file_sha(&tokenizer)?,"plan_sha256":file_sha(&pdir.join("plan.json"))?,
                "binary":std::env::current_exe()?.display().to_string(),"code":CODE_ID,"splits":want,"cohorts":cohorts,"journal_sync_policy":generation::JOURNAL_SYNC_POLICY});
            if let Some(o) = order_rec {
                rec["request_order"] = o;
            }
            if let Some(f) = form_rec {
                rec["profile"] = f;
                rec["profile_items"] = items_rec.unwrap_or(Value::Null);
            }
            write_json(&out.join(format!("run-{t0}.json")), &rec)?;
            println!("{}", serde_json::to_string(&rec)?);
        }
        Cmd::Finalize { plan: pdir, runs, configs, tokenizer, out } => {
            let meta: Value = serde_json::from_slice(&fs::read(pdir.join("plan.json"))?)?;
            refuse_old_dev_select_inputs(&meta)?;
            // every plan file finalize parses holds one split, the same one, and
            // never dev_select or test, read from the lines' bytes before any
            // parse (ruling R2; B5b's reading for finalize, which D4 runs on the
            // same copy; B5b review findings 3 and 9)
            let (ib, isplit) = split_copy::one_split_file(&pdir.join("items.jsonl"), "split", split_copy::OneSplit::Rows)?;
            let mut one: Option<String> = isplit;
            let mut same = |s: Option<String>, what: &str| -> Result<()> {
                match (&one, s) {
                    (_, None) => Ok(()),
                    (None, Some(x)) => {
                        one = Some(x);
                        Ok(())
                    }
                    (Some(a), Some(b)) => {
                        ensure!(*a == b, "{what} holds split {b}, items.jsonl split {a}: finalize reads one split (ruling R2)");
                        Ok(())
                    }
                }
            };
            let mut req_bytes = Vec::new();
            let mut rpaths: Vec<PathBuf> = fs::read_dir(pdir.join("requests"))?.map(|e| e.map(|e| e.path())).collect::<Result<_, _>>()?;
            rpaths.sort();
            for p in rpaths {
                let (b, s) = split_copy::one_split_file(&p, "split", split_copy::OneSplit::Rows)?;
                same(s, &p.display().to_string())?;
                req_bytes.push(b);
            }
            let (fb, fsplit) = split_copy::one_split_file(&pdir.join("plan-failures.jsonl"), "split", split_copy::OneSplit::PlanFailures)?;
            same(fsplit, "plan-failures.jsonl")?;
            let items: Vec<plan::Item> = jsonl_bytes(&ib)?.into_iter().map(serde_json::from_value).collect::<Result<_, _>>()?;
            let mut requests = HashMap::new();
            for b in &req_bytes {
                for r in jsonl_bytes(b)? {
                    let r: plan::Request = serde_json::from_value(r)?;
                    requests.insert(r.request_id.clone(), r);
                }
            }
            let plan_failures = jsonl_bytes(&fb)?;
            let (_, rows) = load_humans(&meta)?;
            let mut generators = HashMap::new();
            for e in fs::read_dir(&configs)? {
                let p = e?.path();
                if p.extension().is_some_and(|x| x == "json") {
                    let v: Value = serde_json::from_slice(&fs::read(&p)?)?;
                    let k = v["key"].as_str().context("key")?.to_owned();
                    let mut g = v.clone();
                    g["config_sha256"] = json!(file_sha(&p)?);
                    generators.insert(k, g);
                }
            }
            let mut run_dirs = Vec::new();
            for e in fs::read_dir(&runs)? {
                let p = e?.path();
                if p.is_dir() {
                    run_dirs.push(p);
                }
            }
            let states = generation::load_states(&run_dirs)?;
            let mut tok = tokenizers::Tokenizer::from_file(&tokenizer).map_err(|e| anyhow::anyhow!(e.to_string()))?;
            tok.with_truncation(None).map_err(|e| anyhow::anyhow!(e.to_string()))?;
            tok.with_padding(None);
            let plan_sha = file_sha(&pdir.join("plan.json"))?;
            let o = finalize::finalize(&items, &requests, &states, &rows, &generators, &plan_failures, &tok, &plan_sha)?;
            let mut files = serde_json::Map::new();
            for split in plan::ALL_SPLITS.iter().copied().filter(|sp| !meta["inputs"][*sp].is_null() && span_ops::plan_reads_input(meta["schema"].as_str(), sp)) {
                let d = out.join(split.replace('_', "-"));
                fs::create_dir_all(&d)?;
                let empty = Vec::new();
                let r = o.records.get(split).unwrap_or(&empty);
                let f = o.failures.get(split).unwrap_or(&empty);
                let x = o.exclusions.get(split).unwrap_or(&empty);
                let s1 = write_jsonl(&d.join("records.jsonl"), r)?;
                let s2 = write_jsonl(&d.join("failures.jsonl"), f)?;
                let s3 = write_jsonl(&d.join("exclusions.jsonl"), x)?;
                let sm = json!({"split":split,"records":r.len(),"failures":f.len(),"exclusions":x.len(),
                    "counts":o.summary["counts"][split],"records_sha256":s1,"failures_sha256":s2,"exclusions_sha256":s3,
                    "plan_sha256":plan_sha,"code":CODE_ID});
                write_json(&d.join("summary.json"), &sm)?;
                files.insert(split.into(), json!({"records":r.len(),"failures":f.len(),"exclusions":x.len(),"records_sha256":s1}));
            }
            let tok_sha = file_sha(&tokenizer)?;
            let s = json!({"schema":"slop_ninja_a_comp_positives_summary_v1","code":CODE_ID,"plan_sha256":plan_sha,"tokenizer_sha256":tok_sha,
                "splits":files,"noop_splice_control":o.summary["noop_splice_control"],"counts":o.summary["counts"],
                "detector_or_pangram_calls":0});
            write_json(&out.join("summary.json"), &s)?;
            println!("{}", serde_json::to_string(&json!({"splits":files,"noop":o.summary["noop_splice_control"]}))?);
        }
        Cmd::Renorm { plan: pdir, runs, split, population, profile, profile_population, tokenizer, records, out } => {
            let pop = form_post::profile_population(Some(&profile), Some(&profile_population))?.context("renorm applies profile cc_cohort_profile_v2")?;
            if let Some(s) = &split {
                ensure!(!renorm::refused_split(s), "split {s} is refused: renorm never reads, scores or parses old Dev-select or the consumed Test (protocol A2 section 2.4)");
            }
            let spliced = serde_json::from_slice::<Value>(&fs::read(pdir.join("plan.json"))?)?["schema"] == grammar_a_positives::splice::PLAN_SCHEMA;
            let s = match (spliced, split, population, tokenizer, records) {
                (false, Some(split), None, Some(tok), records) => renorm::run_local(&pdir, &split, &pop, &tok, records.as_deref(), &runs, &out)?,
                (true, None, Some(population), None, None) => renorm::run_batch(&renorm::spliced_batch(&pdir, &population, &pop)?, &runs, &out)?,
                (false, ..) => anyhow::bail!("a local plan's renorm takes --split and --tokenizer, not --population"),
                (true, ..) => anyhow::bail!("a spliced plan's renorm takes --population, not --split, --tokenizer or --records"),
            };
            println!("{}", serde_json::to_string(&json!({"batch": s["batch"], "counts": s["counts"], "prior_sha256": s["prior_sha256"]}))?);
        }
        Cmd::RenormRows { rows, donors, raid_documents, plan_only, profile, profile_population, out } => {
            let pop = form_post::profile_population(Some(&profile), Some(&profile_population))?.context("renorm-rows applies profile cc_cohort_profile_v2")?;
            let s = renorm::renorm_rows(&rows, donors.as_deref(), raid_documents.as_deref(), &pop, &out, plan_only)?;
            println!("{}", serde_json::to_string(&json!({"counts": s["counts"], "planned_by_class": s["planned_by_class"], "prior_sha256": s["prior_sha256"]}))?);
        }
        Cmd::Briefs { config, train, root, target, max_draws, workers } => {
            let cfg_bytes = fs::read(&config)?;
            let cfg: Value = serde_json::from_slice(&cfg_bytes)?;
            let key = cfg["key"].as_str().context("key")?.to_owned();
            ensure!(cfg["kind"] == "instruct" && key != plan::CAL_ONLY, "inventors must be Train-family instruct models");
            let endpoint = cfg["endpoint"].as_str().context("endpoint")?.trim_end_matches('/').to_owned();
            let client = generation::client()?;
            let props: Value = client.get(format!("{endpoint}/props")).send()?.error_for_status()?.json()?;
            ensure!(props["model_path"] == cfg["model_path"], "server model path mismatch");
            let mut anchors: HashMap<String, Vec<(String, String)>> = HashMap::new();
            for row in read_jsonl(&train)? {
                let h = plan::human_from(&row, "train")?;
                anchors.entry(h.genre.clone()).or_default().push((h.id, h.text));
            }
            for v in anchors.values_mut() {
                v.sort();
            }
            fs::create_dir_all(root.join(&key))?;
            let t0 = generation::now_ms();
            write_json(&root.join(&key).join(format!("server-{t0}.json")), &json!({"props":props,"config":cfg,
                "config_sha256":text::sha_hex(&cfg_bytes),"train_sha256":file_sha(&train)?,"policy":briefs::LIBRARY_POLICY,
                "chat_template_sha256":props["chat_template"].as_str().map(|t| text::sha_hex(t.as_bytes()))}))?;
            let inv = Arc::new(briefs::Inventor { key: key.clone(), endpoint, alias: cfg["alias"].as_str().context("alias")?.into(), config: cfg.clone(), client });
            let genres: Vec<String> = briefs::GENRES.iter().map(|g| g.0.to_string()).collect();
            let s = briefs::run(inv, Arc::new(anchors), &root, target, max_draws, workers, &genres)?;
            write_json(&root.join(&key).join(format!("run-{t0}.json")), &s)?;
            println!("{}", serde_json::to_string(&s)?);
        }
        Cmd::BriefLibrary { root, out } => {
            fs::create_dir_all(&out)?;
            let (results, _) = briefs::load_journals(&root)?;
            let mut accepted = Vec::new();
            let mut rejected = Vec::new();
            let mut counts: BTreeMap<String, BTreeMap<String, BTreeMap<String, usize>>> = BTreeMap::new();
            let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
            for r in &results {
                let g = r["genre"].as_str().unwrap_or("?").to_owned();
                let inv = r["inventor"].as_str().unwrap_or("?").to_owned();
                let acc = r["accepted"] == true;
                *counts.entry(g.clone()).or_default().entry(inv.clone()).or_default().entry(if acc { "accepted".into() } else { "rejected".into() }).or_default() += 1;
                for x in r["reasons"].as_array().into_iter().flatten() {
                    let k = x.as_str().unwrap_or("?").split(':').take(2).collect::<Vec<_>>().join(":");
                    *reasons.entry(k).or_default() += 1;
                }
                if acc {
                    let id = r["draw_id"].as_str().unwrap().to_owned();
                    let h = text::sha_hex(format!("{}:partition:{id}", briefs::LIBRARY_POLICY).as_bytes());
                    let u = u64::from_str_radix(&h[..8], 16).unwrap() % 100;
                    let part = if u < 82 { "train" } else if u < 88 { "dev_iter" } else if u < 94 { "dev_select" } else { "cal" };
                    accepted.push(json!({"brief_id":id,"genre":g,"inventor":inv,"partition":part,
                        "style_name":r["candidate"]["style_name"],"instruction":r["candidate"]["instruction"].as_str().unwrap_or("").trim(),
                        "rationale":r["candidate"]["rationale"],"review":r["review"],"dimensions":r["dimensions"],"anchor_id":r["anchor_id"],
                        "attempt":r["attempt"],"json_extracted":r["json_extracted"],"policy":r["policy"],"recent_seen":r["recent_seen"]}));
                } else {
                    rejected.push(r.clone());
                }
            }
            let ins_of = |f: &dyn Fn(&Value) -> bool| -> Vec<String> {
                accepted.iter().filter(|a| f(a)).map(|a| a["instruction"].as_str().unwrap_or("").to_owned()).collect()
            };
            let mut div = serde_json::Map::new();
            let all: Vec<String> = ins_of(&|_| true);
            div.insert("all".into(), briefs::diversity(&all.iter().map(String::as_str).collect::<Vec<_>>()));
            let mut by_genre = serde_json::Map::new();
            for (g, _) in briefs::GENRES {
                let v = ins_of(&|a: &Value| a["genre"] == g);
                by_genre.insert(g.into(), briefs::diversity(&v.iter().map(String::as_str).collect::<Vec<_>>()));
            }
            div.insert("by_genre".into(), Value::Object(by_genre));
            let invs: std::collections::BTreeSet<String> = accepted.iter().map(|a| a["inventor"].as_str().unwrap_or("?").to_owned()).collect();
            let mut by_inv = serde_json::Map::new();
            for i in &invs {
                let v = ins_of(&|a: &Value| a["inventor"] == i.as_str());
                by_inv.insert(i.clone(), briefs::diversity(&v.iter().map(String::as_str).collect::<Vec<_>>()));
            }
            div.insert("by_inventor".into(), Value::Object(by_inv));
            let mut parts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
            for a in &accepted {
                *parts.entry(a["partition"].as_str().unwrap().into()).or_default().entry(a["genre"].as_str().unwrap().into()).or_default() += 1;
            }
            let s1 = write_jsonl(&out.join("accepted.jsonl"), &accepted)?;
            let s2 = write_jsonl(&out.join("rejected.jsonl"), &rejected)?;
            let summary = json!({"schema":"slop_ninja_a_comp_style_brief_library_v1","policy":briefs::LIBRARY_POLICY,"accepted":accepted.len(),
                "rejected":rejected.len(),"accepted_sha256":s1,"rejected_sha256":s2,"counts_by_genre_inventor":counts,
                "rejection_reasons":reasons,"partitions":parts,"lexical_diversity":div,
                "partition_rule":"sha256(policy:partition:brief_id) first 8 hex mod 100: <82 train, <88 dev_iter, <94 dev_select, else cal (disjoint brief sets per split)",
                "review":"separate temperature-0 call to the same inventor model; automatic screening only, not human verification; style compliance and fidelity of generated text unverified",
                "dedup":"exact name, exact instruction and 4-gram Jaccard > 0.35 against every accepted brief of the genre (all inventors), plus last-eight-accepted contrast in both calls",
                "detector_or_pangram_calls":0});
            write_json(&out.join("summary.json"), &summary)?;
            println!("{}", serde_json::to_string(&json!({"accepted":accepted.len(),"rejected":rejected.len(),"partitions":parts}))?);
        }
        Cmd::PlanV2 { train, dev_iter, dev_select, cal, library, fix_slop, out } => {
            run_plan_v2(&train, &dev_iter, &dev_select, &cal, &library, &fix_slop, &out)?
        }
        Cmd::PlanV3 { train, dev_iter, dev_select, cal, splits, prefix_every, middle_every, base_generators, out } => {
            let splits: Vec<String> = splits.split(',').map(|x| x.trim().to_owned()).filter(|x| !x.is_empty()).collect();
            let base: Vec<String> = base_generators.split(',').map(|x| x.trim().to_owned()).filter(|x| !x.is_empty()).collect();
            run_plan_v3(&train, dev_iter.as_deref(), dev_select.as_deref(), cal.as_deref(), &splits, prefix_every, middle_every, &base, &out)?
        }
        Cmd::LibrarySession { config, train, root, round, minutes, workers, voice_goal, brief_goal, review_only, hd_sample } => {
            let cfg_bytes = fs::read(&config)?;
            let cfg: Value = serde_json::from_slice(&cfg_bytes)?;
            let key = cfg["key"].as_str().context("key")?.to_owned();
            ensure!(cfg["kind"] == "instruct" && key != plan::CAL_ONLY, "inventors must be Train-family instruct models");
            let endpoint = cfg["endpoint"].as_str().context("endpoint")?.trim_end_matches('/').to_owned();
            let client = generation::client()?;
            let props: Value = client.get(format!("{endpoint}/props")).send()?.error_for_status()?.json()?;
            ensure!(props["model_path"] == cfg["model_path"], "server model path mismatch");
            let mut anchors: HashMap<String, Vec<String>> = HashMap::new();
            let mut rows: Vec<(String, String, String)> = Vec::new();
            for row in read_jsonl(&train)? {
                let h = plan::human_from(&row, "train")?;
                rows.push((h.genre.clone(), h.id.clone(), h.text.clone()));
            }
            rows.sort();
            for (g, _, t) in rows {
                anchors.entry(g).or_default().push(t);
            }
            fs::create_dir_all(root.join(&key))?;
            let t0 = generation::now_ms();
            write_json(&root.join(&key).join(format!("server-{t0}.json")), &json!({"props":props,"config":cfg,
                "config_sha256":text::sha_hex(&cfg_bytes),"train_sha256":file_sha(&train)?,"policy":library::POLICY,"round":round,"journal_sync_policy":generation::JOURNAL_SYNC_POLICY,
                "chat_template_sha256":props["chat_template"].as_str().map(|t| text::sha_hex(t.as_bytes()))}))?;
            let inv = Arc::new(library::Inventor { key: key.clone(), family: cfg["family"].as_str().context("family")?.into(), endpoint,
                alias: cfg["alias"].as_str().context("alias")?.into(), config: cfg.clone(), client });
            let hd = match &hd_sample {
                Some(p) => read_jsonl(p)?,
                None => Vec::new(),
            };
            let a = library::SessionArgs { round, minutes, workers, voice_goal, brief_goal, review_only, hd };
            let s = library::session(inv, Arc::new(anchors), &root, &a)?;
            write_json(&root.join(&key).join(format!("session-{t0}.json")), &s)?;
            println!("{}", serde_json::to_string(&s)?);
        }
        Cmd::HumanSample { train, train_mining, describers, out } => {
            fs::create_dir_all(&out)?;
            let ds: Vec<String> = describers.split(',').map(String::from).collect();
            let (rows, mut summary) = grammar_a_positives::human_derived::build_sample(&[("train".into(), train.as_path()), ("train_mining".into(), train_mining.as_path())], &ds)?;
            let sha = write_jsonl(&out.join("sample.jsonl"), &rows)?;
            summary["sample_sha256"] = json!(sha);
            summary["inputs"] = json!({"train":{"path":train.display().to_string(),"sha256":file_sha(&train)?},
                "train_mining":{"path":train_mining.display().to_string(),"sha256":file_sha(&train_mining)?}});
            write_json(&out.join("sample-summary.json"), &summary)?;
            println!("{}", serde_json::to_string(&json!({"selected":summary["selected"],"strata":summary["strata"]}))?);
        }
        Cmd::LibraryCompile { root, eval_manifest, out } => {
            let mut ms = Vec::new();
            for x in &eval_manifest {
                let (s, p) = x.split_once('=').context("split=path")?;
                ms.push((s.to_owned(), PathBuf::from(p)));
            }
            let authors = library::load_eval_authors(&ms)?;
            let s = library::compile(&root, &authors, &out)?;
            let brief = json!({"accepted_voice_prompts":s["accepted_voice_prompts"],"accepted_briefs":s["accepted_briefs"],
                "pending":s["pending_unreviewed"],"rejected":s["rejected"],
                "voice_rounds":s["breadth"]["voice_provisional"]["per_round"],"brief_rounds":s["breadth"]["brief_provisional"]["per_round"],
                "voice_clusters":s["breadth"]["voice_accepted"]["clusters"],"brief_clusters":s["breadth"]["brief_accepted"]["clusters"]});
            println!("{}", serde_json::to_string(&brief)?);
        }
        Cmd::OutputBreadth { runs, plan: plans, humans, cap, out } => {
            use grammar_a_positives::breadth;
            let mut items: HashMap<String, (String, String, String)> = HashMap::new();
            for pd in &plans {
                for it in read_jsonl(&pd.join("items.jsonl"))? {
                    let sk = it["style"]["style_kind"].as_str().unwrap_or("fixed_template_v1").to_owned();
                    items.insert(it["item_id"].as_str().unwrap_or("").into(), (sk, it["cohort"].as_str().unwrap_or("").into(), it["split"].as_str().unwrap_or("").into()));
                }
            }
            let mut groups: BTreeMap<String, Vec<Vec<f32>>> = BTreeMap::new();
            for spec in &runs {
                let (label, root) = spec.split_once('=').context("label=path")?;
                let mut dirs = Vec::new();
                for e in fs::read_dir(root)? {
                    let p = e?.path();
                    if p.is_dir() {
                        dirs.push(p);
                    }
                }
                for st in generation::load_states(&dirs)?.values() {
                    let Some(v) = st.accepted() else { continue };
                    let t = v["verdict"]["text"].as_str().unwrap_or("");
                    if text::words(t) < 40 {
                        continue;
                    }
                    let pr = breadth::profile(t);
                    let g = v["generator"].as_str().unwrap_or("?");
                    let (sk, cohort, _) = items.get(v["item_id"].as_str().unwrap_or("")).cloned().unwrap_or(("unplanned".into(), "?".into(), "?".into()));
                    for k in [format!("{label}|all"), format!("{label}|style_kind:{sk}"), format!("{label}|generator:{g}"), format!("{label}|cohort:{cohort}")] {
                        groups.entry(k).or_default().push(pr.clone());
                    }
                }
            }
            if let Some(h) = &humans {
                for row in read_jsonl(h)? {
                    let hu = plan::human_from(&row, "train")?;
                    groups.entry(format!("human_train|genre:{}", hu.genre)).or_default().push(breadth::profile(&hu.text));
                    groups.entry("human_train|all".into()).or_default().push(breadth::profile(&hu.text));
                }
            }
            let rep = breadth::report(&groups, cap);
            let v = json!({"schema":"slop_ninja_a_comp_output_breadth_v1","method":"function-word, punctuation, layout, casing and sentence-length profile (unit-normalised); per group a keyed sample of at most cap documents; dispersion = 1 - mean pairwise cosine; leader clusters at cosine 0.90/0.95; outputs under 40 words skipped","cap":cap,"groups":rep,"detector_or_pangram_calls":0});
            write_json(&out, &v)?;
            println!("{}", serde_json::to_string(&json!({"groups":groups.len()}))?);
        }
        Cmd::SplitCopy { plan: pdir, split, out } => {
            let r = split_copy::split_copy(&pdir, &split, &out)?;
            println!("{}", serde_json::to_string(&json!({"split": r["split"], "copied_lines": r["copied_lines"], "skipped": r["lines_of_other_splits_skipped_unparsed"], "plan_sha256": r["plan_sha256"]}))?);
        }
        Cmd::MirrorPlan { plan: pdir, renorm: rdir, split, values, out } => {
            let r = mirror_regen::mirror_plan(&pdir, &rdir, &split, &values::Values::open(&values)?, &out)?;
            println!("{}", serde_json::to_string(&json!({"regenerated_mirrors": r["regenerated_mirrors"], "rows_by_action": r["rows_by_action"], "d4_orders": r["d4_orders"], "plan_sha256": r["plan_sha256"]}))?);
        }
        Cmd::Pilot { plan: pdir, request_order, population, renorm, step, out } => {
            let spliced = serde_json::from_slice::<Value>(&fs::read(pdir.join("plan.json"))?)?["schema"] == grammar_a_positives::splice::PLAN_SCHEMA;
            let r = match (spliced, population, renorm) {
                (false, None, None) => pilot::run(&pdir, request_order.as_deref(), &step, &out)?,
                (true, Some(pop), Some(rn)) => {
                    ensure!(request_order.is_none(), "a spliced plan's pilot reads its rows from renorm's regeneration plan, not --request-order");
                    pilot::run_spliced(&pdir, &pop, &rn, &step, &out)?
                }
                (false, ..) => anyhow::bail!("a local plan's pilot takes no --population or --renorm"),
                (true, ..) => anyhow::bail!("a spliced plan's pilot (D8) takes --population and --renorm (renorm's spliced output)"),
            };
            println!("{}", serde_json::to_string(&json!({"rows": r["rows"], "pilot_rows": r["pilot_rows"], "bulk_rows": r["bulk_rows"], "pilot_sha256": r["pilot_sha256"]}))?);
        }
        Cmd::TrainMirrorPlan { train, values, cal_from, topup, out } => {
            let vals = values::Values::open(&values)?;
            let (cohort, groups, source, train_sha256) = match &topup {
                Some(t) => {
                    let (g, src, sha) = grammar_a_positives::topup::groups_of(t, &train, &vals)?;
                    (grammar_a_positives::train_mirror::COHORT_TOPUP_MIRROR, Some(g), src, Some(sha))
                }
                None => (grammar_a_positives::train_mirror::COHORT_TRAIN_MIRROR, None, json!({"step": null}), None),
            };
            let spec = grammar_a_positives::train_mirror::Spec { cohort, groups, source, train_sha256 };
            let r = grammar_a_positives::train_mirror::run(&train, &cal_from, &vals, &spec, &out)?;
            println!("{}", serde_json::to_string(&json!({"cohort": r["cohort"], "items": r["items"], "plan_sha256": r["plan_sha256"], "values": r["values"]}))?);
        }
        Cmd::EnronPlan { train_half, values, cal_from, out } => {
            let vals = values::Values::open(&values)?;
            let r = enron_half::plan_d6(&train_half, &vals, &cal_from, &out)?;
            println!("{}", serde_json::to_string(&json!({"cohort": r["cohort"], "items": r["items"], "plan_sha256": r["plan_sha256"]}))?);
        }
        Cmd::TopupPlan { values, train_mining, train_mining_manifest, train, train_manifest, configs, frontier_config, out } => {
            let vals = values::Values::open(&values)?;
            let tr = (train.as_path(), train_manifest.as_path());
            let inp = grammar_a_positives::topup::Inputs { values: &vals, train_mining: &train_mining, train_mining_manifest: &train_mining_manifest, train: tr,
                configs: &configs, frontier_config: &frontier_config };
            let r = grammar_a_positives::topup::run(&inp, &out)?;
            println!("{}", serde_json::to_string(&json!({"totals": r["totals"], "topup_sha256": r["topup_sha256"]}))?);
        }
        Cmd::TopupCheck { topup, values } => {
            let r = grammar_a_positives::topup::check(&topup, &values::Values::open(&values)?)?;
            println!("{}", serde_json::to_string(&r)?);
        }
        Cmd::EnronHalf { pool, root, values, out } => {
            let r = enron_half::run(&pool, &root, &values, &out)?;
            println!("{}", serde_json::to_string(&json!({"train_half": r["train_half"], "diagnostic_half": r["diagnostic_half"], "documents": r["documents"],
                "checks": r["checks"], "out_sha256": r["out_sha256"]}))?);
        }
        Cmd::Rates { runs } => {
            let mut outm = serde_json::Map::new();
            for e in fs::read_dir(&runs)? {
                let p = e?.path();
                if !p.is_dir() {
                    continue;
                }
                let g = p.file_name().unwrap().to_string_lossy().to_string();
                let mut per_run: Vec<Value> = Vec::new();
                for f in fs::read_dir(&p)? {
                    let f = f?.path();
                    let name = f.file_name().unwrap().to_string_lossy().to_string();
                    if name.starts_with("run-") && name.ends_with(".json") {
                        per_run.push(serde_json::from_slice(&fs::read(&f)?)?);
                    }
                }
                let st = generation::load_states(&[p.clone()])?;
                let mut acc = 0;
                let mut term = 0;
                let mut attempts = 0;
                let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
                for s in st.values() {
                    attempts += s.responses.len();
                    if s.accepted().is_some() {
                        acc += 1;
                    } else if s.done() {
                        term += 1;
                    }
                    for v in s.responses.values() {
                        for r in v["verdict"]["reasons"].as_array().into_iter().flatten() {
                            *reasons.entry(r.as_str().unwrap_or("?").into()).or_default() += 1;
                        }
                    }
                }
                outm.insert(g, json!({"requests_seen":st.len(),"accepted":acc,"terminal_failed":term,"responses":attempts,
                    "rejection_reasons":reasons,"runs":per_run.iter().map(|r| json!({"t0":r["t0"],"ran":r["ran"],"stats":r["stats"],"splits":r["splits"]})).collect::<Vec<_>>()}));
            }
            println!("{}", serde_json::to_string_pretty(&Value::Object(outm))?);
        }
    }
    Ok(())
}
