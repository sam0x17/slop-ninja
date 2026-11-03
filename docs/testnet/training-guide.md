# Training an A artifact from public material

This guide trains an *A* artifact for testnet subnet 601 from
[ModernBERT-base](https://huggingface.co/answerdotai/ModernBERT-base) and the
redistributable datasets under [`datasets/`](../../datasets/README.md). It
needs no private data, account or credential. The result is the six-file
artifact of the [artifact spec](artifact-spec.md), packed and checked with
`slop_ninja_miner pack` and `check`. It is step 5 of the
[miner guide](miner-guide.md), which covers everything around it: installing
the client, the wallet and hotkey, registration on subnet 601 (with rtao; no
btcli or Python), the commitment and the upload. The
[landing page](README.md) lists what is live.

The recipe gives you a pipeline that produces an admissible artifact. It is
not a strong detector, and nothing here measures how it would score in a
cohort. Improving the data and the training is the miner's job.

## Where this guide stops today

Steps 1 to 13 run now, offline after the downloads, and end with `check`
exiting 3 (every local check passed; no cohort to compare with). Nothing
after that can run yet: `commit`, `upload` and `next-cohort` need the
coordinator's admission routes, the public status host and the
coordinator's origin pinned in the release trust file, and none of these is
live ([What is live](README.md#what-is-live)). Until then those commands
stop with exit 2 and say what is missing. The miner guide will list the
endpoints when they go live.

## Getting the code

The recipe runs from a checkout of the Slop Ninja repository at a revision
that contains this guide, including its `datasets/` directory. The public
repository is <https://github.com/sam0x17/slop-ninja>. The commands fetch everything else from public sources: Rust crates
(including rtao from GitHub), Python wheels and ModernBERT-base.

## The testnet reference

- The testnet reference is **R0**, scored in every cohort. Its
  [reference package](reference-packages.md) is published under CC BY-SA 4.0
  when public access opens, with a training companion that holds R0's
  encoder. The recipe below starts from ModernBERT-base and public data; you
  can start from R0's encoder instead ([Warm start from R0](#warm-start-from-r0)).
- **Promotion applies.** An entrant that beats the reference by the margin δ
  and passes the promotion checks becomes the next reference, as the
  whitepaper's promotion rule says. Its artifact is published with its cohort,
  like every admitted artifact.
- **One replacement outside promotion.** Separately, Slop Ninja may replace
  R0 at most once with a better reference it trains, only if that model passes
  every required gate, announced in advance on the status page.
- **The burn rule.** Testnet follows the whitepaper's rule: no miner earns,
  and the pool burns, until an entrant beats the reference by δ. The
  [miner guide's Rewards section](miner-guide.md#13-rewards) gives the score,
  δ and the credit rule.
- At most 40 entrants are admitted to a cohort.

Otherwise testnet follows the whitepaper, with one rule the whitepaper does
not state yet:

- **Reused human documents.** While no outside miner has a working artifact,
  testnet cohorts reuse human documents that earlier cohorts already used.
  Fresh human stock is drawn only once outside miners have working artifacts.

Two testnet limits also apply, as the miner guide explains: the gate
thresholds are provisional, frozen in each signed cohort manifest, and Slop
Ninja's validators do not yet commit the launch burn row
([Rewards](miner-guide.md#13-rewards)).

## What the recipe reproduces of R0, and what it does not

R0 was trained with the same trainer, export and calibration code, but from
material that is not public. The recipe reproduces R0's pipeline and artifact
format. It does not reproduce R0's weights, data or scores.

| | R0 | This recipe |
|---|---|---|
| Starting model | the V6 detector, itself fine-tuned from ModernBERT-base; not distributed | ModernBERT-base at revision `8949b909…`, with a new three-label head |
| Train data | 86,575 rows from the project corpus, including private and non-redistributable sources (local open-weight generations, hosted frontier generations, RAID and others) | 4,201 rows from five published snapshots |
| Generators with Train rows | all 33 names of the fixed roster | 2 of the 33 (`qwen3.6`, `phi4`) |
| Arm | `E1aux`: style adversary and generator-contrastive head, which need voice metadata | `E1`: the plain arm (R0's recipe F) |
| Optimizer and schedule | AdamW, encoder 1e-5, heads 3e-4, cosine, 6% warm-up, 2 epochs, batch 32, attribution weight 0.25 | the same |
| Train weighting | the corpus assembler's row weights | `family_origin_balanced` (source groups and origin cells balanced) |
| Seeds and selection | three seeds; Dev-iter early stopping, then a Dev-select choice between finalists | one seed (17); Dev-iter early stopping only |
| Calibration | CAL only, 5,379 documents; origin temperature, then `isotonic_v1_clipped` | the same policy on 1,131 CAL documents (94 human) |
| Export | banded f32 graph, class `a-plain-f32band-v1` | the same exporter and class |
| Tokenizer, contract fields, roster names | pinned by the spec | the same bytes and values |

Both artifacts have the same graph structure, tokenizer and contract schema.
So the checker treats them the same way, but their weights and quality are
unrelated.

## Tested platforms

| OS | Architecture | Runtime | Status |
|---|---|---|---|
| Ubuntu 26.04 | x86_64 | Python 3.12.15, torch 2.9.1+cu128, CUDA 12.8 wheels, NVIDIA RTX 4090 (24 GB, driver 595); Rust 1.99.1 | tested end to end (smoke below); the ONNX export ran on the CPU |
| Other Linux x86_64 distributions | x86_64 | the same wheels | untested; expected to work where the wheels install |
| Linux | aarch64 | any | untested |
| macOS | arm64 or x86_64 | any | untested; the GPU steps need CUDA |
| Windows | any | any | untested |
| CPU-only training | x86_64 | torch 2.9.1 CPU wheels | untested; the trainer refuses bf16 on the CPU and needs FP32 there, so it would be far slower |
| Other GPUs, CUDA versions or driver series | | | untested |

The ONNX export must run in the pinned environment (Python 3.12,
torch 2.9.1, transformers 4.57.6): the graph's structure depends on the
exporter version, and `export_a_onnx.py` refuses other versions.

## Data

The recipe uses the five snapshots that follow the shared supervised record
contract (`slop_ninja_a_supervised_record_v1`). The bundle builder reads only
the files below; it never opens a Test file or a reference file.

| Snapshot | Files read | Trainer split |
|---|---|---|
| [`a-encoder-20261026-v1`](../../datasets/a-encoder-20261026-v1/README.md) | `train.jsonl`, `development.jsonl`, `calibration.jsonl` | train, dev_iter, cal |
| [`a-balanced-20261026-v1`](../../datasets/a-balanced-20261026-v1/README.md) | `train.jsonl`, `development.jsonl`, `calibration.jsonl` | train, dev_iter, cal |
| [`a-supervised-20261026-v1`](../../datasets/a-supervised-20261026-v1/README.md) | `train.jsonl`, `development.jsonl`, `calibration.jsonl` | train, dev_iter, cal |
| [`a-retained-train-20261026-v1`](../../datasets/a-retained-train-20261026-v1/README.md) | `records.jsonl` | train |
| [`a-train-augmentation-20261026-v1`](../../datasets/a-train-augmentation-20261026-v1/README.md) | `records.jsonl` | train |

Each snapshot's own splits are kept: Development becomes the trainer's
`dev_iter` (checkpoint selection) and Calibration becomes `cal` (read only by
the calibrator). No source group appears in two splits. The rule
(`slop_ninja_a_public_bundle_v1`, in
[`public_a_bundle.rs`](../../grammar/crates/grammar-a-baseline/examples/public_a_bundle.rs))
also:

- keeps only `purpose: query` rows;
- requires a measured `share_target`, and one generator identity on every row
  with AI involvement;
- maps generator identities to labels with the table in
  [`recipe.json`](../../grammar/crates/grammar-eval/python/public_a/recipe.json).
  Train labels must be in the fixed roster. Evaluation splits keep other
  generators, so that the calibration can measure rejection of generators
  outside the roster;
- keeps the first copy of an exact text and drops later copies;
- gives every row cohort weight 1.

Result on the current snapshots: 6,137 rows (4,201 Train, 805 Dev-iter,
1,131 CAL) from 473 source groups. Excluded: 80 rows without a measured share,
66 model rows without a single generator identity, 70 Train rows of generators
outside the roster and 24 duplicate texts.

The human rows are historical texts (public-domain books, Wikipedia,
Wikinews, PLOS abstracts) used as proxies for human writing; the snapshots
say so. Most positives are rewrites of those texts by two local open-weight
models. The remaining published snapshots (`detector-v5`, `detector-v6`,
`detector-hosted-v1`, `detector-origin-v1`, `detector-revisions-v1`) use a
different record format and are not read by the recipe. Adding them, or any
other data you have the rights to use, needs your own conversion to the
bundle rows.

Each snapshot's `README.md` and per-row rights records give the terms of its
source texts and generated texts. Four of the five also carry `NOTICE.md` and
a `licenses/` directory (`a-train-augmentation-20261026-v1` also
`LICENSES.md`). `a-supervised-20261026-v1` has no notice files of its own: its
README names the notices that apply to its rows, in
[`detector-v6`](../../datasets/detector-v6/README.md) (with
`LICENSE.fix-slop`), [`detector-origin-v1`](../../datasets/detector-origin-v1/README.md)
and [`detector-revisions-v1`](../../datasets/detector-revisions-v1/README.md),
although the recipe reads no data from those snapshots. The repository's
[`NOTICE`](../../NOTICE) lists attributions across snapshots. Keep all of
these with any copy of the data.

## What you must keep, and what you may change

The checker admits only artifacts that keep these fixed:

- the graph class `a-plain-f32band-v1`: exported by `export_a_onnx.py` in the
  pinned environment, from a ModernBERT-base-shaped encoder with the document
  heads. The span class (spec v2) and the tower class (spec v3) are served
  too, but the recipe trains neither;
- the tokenizer bytes. The parent step rebuilds them from the upstream
  tokenizer and refuses any other bytes;
- the contract's pinned fields, including the 33 AI roster names in
  [`ai-roster.json`](../../grammar/crates/grammar-eval/python/public_a/ai-roster.json)
  (their digest is the spec's pin). Names without Train rows stay in the
  roster unsupervised;
- the calibration policy in
  [`policy.json`](../../grammar/crates/grammar-eval/python/public_a/policy.json)
  and the file schemas (heads v1, export manifest v2, calibration v2).

You may change the data, the weighting, the hyperparameters, the seed, the
number of epochs and the arm, provided the arm has no span or token head and
the protocol keeps its `ai_roster` entry.

The recipe is deterministic on one platform: two runs of the smoke on the
tested host produced byte-identical encoder weights and graph (digests in the
[smoke run](#smoke-run)). Another GPU, driver or library version can change
the weights. So two miners who run the recipe unchanged on the same software
can get identical or nearly identical artifacts. Under the reward rule
(`reward-v1`; the [miner guide's Rewards section](miner-guide.md#13-rewards),
and the whitepaper's section "Benchmark scoring and native rewards",
subsection "From scores to credits"), near-duplicate artifacts share one
credit, and copies of a published artifact earn nothing. Change something
that matters.

### Training data and evaluation text

- The [terms](terms.md) require that you have the rights to what your artifact
  derives from, training data included. They set no other limit on training
  data, and the published snapshots' Test splits are data like any other. If
  you train on a Test split, it no longer gives you a held-out check.
- The whitepaper's rules govern what an artifact carries, not what it saw
  (its section "Benchmark scoring and native rewards", subsection "Lookup
  exposure and the canary"). Admission takes only the allow-listed graph and
  schema-checked files, so an explicit text index or lookup table of text
  fails admission. An artifact later found to carry one has its scores
  invalidated, and their signed correction also addresses any promotion they
  produced.
- Before an entrant becomes the reference, a lookup canary audits it. Two
  flags block its promotion, with no reward penalty: the next qualifying
  entrant is promoted instead.
- Recognition learned in the weights cannot be excluded. Testnet's human
  stock is public text, and cohorts now reuse human documents that earlier
  cohorts used. A testnet score therefore shows that an artifact runs and is
  scored; it is not evidence that it detects AI involvement in unseen text.

## What you need

- The repository checkout described in [Getting the code](#getting-the-code).
- Linux x86_64 and an NVIDIA GPU whose driver runs the CUDA 12.8 wheels. Only
  a 24 GB card was tested (see [Tested platforms](#tested-platforms) and
  [GPU memory](#gpu-memory)).
- Rust 1.89 or later (tested: 1.99.1) and a C compiler;
  [uv](https://docs.astral.sh/uv/), which also fetches Python 3.12 if the host
  lacks it; `curl` and `sha256sum`. No other host Python is needed.
- About 16 GB of free disk: 14 GB in the working directory (6.8 GB of it the
  Python environment, 0.6 GB the ModernBERT-base download, 3.4 GB trainer
  checkpoints), about 1.2 GB of Rust build output in the checkout and 0.13 GB
  of restored datasets. uv's cache (`~/.cache/uv` by default) also holds the
  downloaded wheels; `uv cache clean` removes them after the install.
- Host memory: the largest peak in the smoke was 3.7 GB (training).

### GPU memory

The protocol caps the trainer at a fraction of the GPU's memory
(`cuda_memory_fraction`: 0.45 in `protocol.json`, 0.35 in the smoke), and an
allocation past the cap fails as out of memory. In the smoke on a 24 GB card,
the trainer's peak CUDA allocation was 4.7 GB (batch 32), and the device
showed at most 8.6 GB in use, PyTorch's cache and the CUDA context included.
At the default 0.45 the cap is below that peak on cards under about 11 GB.
On such a card, raise `cuda_memory_fraction` in `config/protocol.json`,
leaving some headroom; the trainer accepts at most 0.95, so a card under
about 5 GB cannot hold the peak at all. Only the 24 GB card was tested. The
checker does not pin the protocol, but the role files record its digest, so
any change to it changes their digests.

## Steps

All commands run from a new working directory outside the repository, with
relative paths. The artifact's JSON files are published after the cohort
closes, so the tools record each local location they write as a portable
path: resolved against the working directory, repository-relative inside the
repository, and only its final component under the ignored `data/` tree or
outside the repository (a user or home directory name is replaced), so from a
working directory outside the repository they record final components. The normalization fit records its run
directories this way, the trainer its init fit and allow-list, and the
release exporter its calibration provenance. The contract keeps the
normalization's run paths, and the fit binds the contract's digest, so they
cannot change after training. `slop_ninja_miner pack` and `check` warn about
any string or key left in a JSON file that reads as or carries a local path,
or names a path under a `data/` tree;
the check reads the start of each string and a few embedded forms, so it is a
check, not a guarantee.

### 1. Build the tools

From the repository root:

```sh
cargo build --release --locked --manifest-path tools/dataset-storage/Cargo.toml
cargo build --release --locked --manifest-path grammar/Cargo.toml -p grammar-a-baseline \
  --example public_a_bundle --example fit_full_a_normalization \
  --example prepare_full_a_contract --example calibrate_full_a
cargo build --release --locked --manifest-path subnet/Cargo.toml \
  -p slop_ninja_miner -p slop-ninja-artifact-check
```

Then set the paths used below, in the shell you run every later step in:

```sh
REPO=$PWD
PYDIR=$REPO/grammar/crates/grammar-eval/python
RECIPE=$PYDIR/public_a
X=$REPO/grammar/target/release/examples
B=$REPO/subnet/target/release
umask 077
mkdir -p ~/a-recipe && cd ~/a-recipe
```

`umask 077` makes every file the later steps write readable only by you. The
trained weights end up in `fit/`, `release/`, `graph/` and `artifact/` as well
as in `artifact.obj`, and all of them must stay private until your cohort
closes: anyone who has them before then can enter the same model.

### 2. Python environment

[`requirements-linux-x86_64-cu128.lock`](../../grammar/crates/grammar-eval/python/public_a/requirements-linux-x86_64-cu128.lock)
pins every package of the tested environment to its version and to the
SHA-256 of each published file of that version; with `--require-hashes`
the install below refuses any file whose digest is not listed.
[`requirements.in`](../../grammar/crates/grammar-eval/python/public_a/requirements.in)
holds its top-level pins. With [uv](https://docs.astral.sh/uv/):

```sh
uv venv --python 3.12 venv
uv pip install --python venv/bin/python --index-strategy unsafe-best-match \
  --index-url https://download.pytorch.org/whl/cu128 --extra-index-url https://pypi.org/simple \
  --require-hashes -r $RECIPE/requirements-linux-x86_64-cu128.lock
PY=$PWD/venv/bin/python
```

### 3. Restore the datasets

The snapshots are stored compressed. Restore the five snapshots and check
them:

```sh
for d in a-encoder-20261026-v1 a-balanced-20261026-v1 a-supervised-20261026-v1 \
         a-retained-train-20261026-v1 a-train-augmentation-20261026-v1; do
  (cd $REPO && tools/dataset-storage/target/release/slop_ninja_dataset_storage restore . datasets/$d \
     && cd datasets/$d && sha256sum -c --quiet SHA256SUMS)
done
```

Restoring a snapshot also restores its Test and reference files; the recipe
does not read them (see [Training data and evaluation
text](#training-data-and-evaluation-text)).

### 4. Download ModernBERT-base

```sh
REV=8949b909ec900327062f0ebf497f51aef5e6f0c8
mkdir -p modernbert-base
for f in config.json model.safetensors tokenizer.json tokenizer_config.json special_tokens_map.json; do
  curl -fL -o modernbert-base/$f https://huggingface.co/answerdotai/ModernBERT-base/resolve/$REV/$f
done
```

ModernBERT-base is licensed under Apache-2.0. The next step checks each file's
SHA-256 against `recipe.json`.

### 5. Build the parent

```sh
$PY $PYDIR/public_a.py parent --upstream modernbert-base --out parent
```

`parent/` holds the encoder as a three-label classifier (its new head comes
from transformers' own initialization under seed 17), the tokenizer
re-serialized to the pinned bytes, `init.json` and a manifest whose
`artifact_id` names the parent in every later record.

### 6. Build the bundle

```sh
$X/public_a_bundle --recipe $RECIPE/recipe.json --repo $REPO --out bundle
```

It prints the row counts and exclusions; `bundle/MANIFEST.json` and
`bundle/exclusions.jsonl` record them.

### 7. Fit the embedding normalization

The heads read the encoder output after a Train-only normalization. It is a
fit of the parent's output over every Train row, with equal weight per source
group:

```sh
mkdir norm
$X/fit_full_a_normalization requests --bundle bundle --out norm/req \
  --shards 1 --sample-keep 1 --sample-modulus 1
CUDA_VISIBLE_DEVICES=0 $PY $PYDIR/public_a.py embed --parent parent \
  --requests norm/req/requests-000.jsonl --output norm/run-000 --device cuda
$X/fit_full_a_normalization fit --plan norm/req/plan.json --run norm/run-000 \
  --out norm/normalization.json
```

Some record fields keep the names of the first parent they served, V6 (for
example `v6_artifact_id`). Here they hold the public parent's identity.

### 8. Write the contract

```sh
mkdir config
$X/prepare_full_a_contract --bundle bundle --normalization norm/normalization.json \
  --parent-artifact-id "$($PY -c 'import json; print(json.load(open("parent/manifest.json"))["artifact_id"])')" \
  --roster $RECIPE/ai-roster.json --out config/contract.json
cp $RECIPE/protocol.json config/protocol.json
```

`--roster` fixes the AI roster to the 33 spec names. The protocol's
`ai_roster` entry (rule `fixed_v1`, with the roster's digest) lets the trainer
accept a roster larger than its Train labels. The trainer and the exporter
read `config/protocol.json`; to run the [smoke](#smoke-run) instead, copy
`$RECIPE/protocol-smoke.json` there in place of `protocol.json`.

### 9. Train

```sh
export CUBLAS_WORKSPACE_CONFIG=:4096:8 CUDA_VISIBLE_DEVICES=0
$PY $PYDIR/public_a.py train --data bundle --config config --parent parent \
  --init parent/init.json --out fit --arm E1 --seed 17 --device cuda
```

The trainer evaluates Dev-iter eight times per epoch, keeps the checkpoint
with the lowest uncalibrated balanced involvement Brier score, and stops
after four evaluations without improvement. It resumes from its last
evaluation if interrupted. The protocol caps the process at 45% of the GPU's
memory (`cuda_memory_fraction`).

`public_a.py` runs the release trainer (`train_full_a.py`) unchanged, except
that it replaces the trainer's loader for the V6 parent and its classifier
initializer with loaders for `parent/`.

### 10. Calibrate on CAL

```sh
$PY $PYDIR/public_a.py raw --data bundle --config config --fit fit --parent parent \
  --init parent/init.json --part cal --out raw-cal --device cuda
$PY $PYDIR/public_a.py select --data bundle --fit fit --out selection.json
$X/calibrate_full_a bundle config fit raw-cal selection.json $RECIPE/policy.json \
  calibration.json cal-calibrated.jsonl
```

The calibrator fits the involvement map and the AI attribution threshold on
CAL alone. The threshold needs CAL positives both of roster generators and of
generators outside the roster; the bundle provides both.

### 11. Write the release

```sh
$PY $PYDIR/public_a.py release --data bundle --config config --fit fit --parent parent \
  --init parent/init.json --calibration calibration.json --out release --device cpu
```

This runs the release exporter (`export_full_a.py`), which also writes
reference predictions, then rewrites its files in the spec's schemas:
heads v1 and export manifest v2, with the Rust calibration v2 unchanged. The
manifest's `limits` record what was dropped and why.

### 12. Export the graph

```sh
CUDA_VISIBLE_DEVICES= $PY $PYDIR/export_a_onnx.py --release-dir release --variant plain \
  --out graph/a-fp32band.onnx --record graph/export.json
```

The record must say `"qualified": true`: the structure digest equals the plain
class digest and every validation output is finite.

### 13. Assemble, pack and check

```sh
$PY $PYDIR/public_a.py assemble --release release --graph graph/a-fp32band.onnx \
  --record graph/export.json --out artifact
$B/slop_ninja_artifact_check check-dir artifact
$B/slop_ninja_miner pack --dir artifact --out artifact.obj
$B/slop_ninja_miner check artifact.obj
```

`slop_ninja_artifact_check check-dir` prints its report and exits 0 when the
directory passes. A refusal prints `refused <code>: <detail>` and exits 2; a
path it cannot read is reported as `refused object`. It exits 64 on a usage
error and 0 for `--help`.

`slop_ninja_miner check` without `--coordinator` exits 3 ("incomplete") when
every local check passes, because it cannot compare the artifact with an open
cohort's contract and object bound. Exit 1 means refused, with the spec's
refusal code; exit 2 means an error such as a missing file, with nothing
decided about the artifact. The [miner guide](miner-guide.md#6-pack-and-check)
lists the codes.

`pack` writes `artifact.obj` with mode 0600. Keep it, and the working
directory's other copies of the weights, private until the cohort closes.
This is where the guide stops today. Once the admission routes are live,
continue with the miner guide from
[step 7](miner-guide.md#7-choose-the-endpoints) (endpoints, commitment and
upload), with a registered hotkey ([steps 3 and 4](miner-guide.md#3-wallet-and-hotkey)).

## Warm start from R0

`R0`'s reference package comes with a training companion: R0's encoder
(`model/encoder.safetensors`, 598,442,828 bytes, SHA-256
`a167c6072e946be3ce23aa06a1f6ea0e0150dd29dafa3fb387980fe90c236927`) and its
classifier config, listed in the package's `training/companion.json` as
64 MiB parts, under the same CC BY-SA 4.0 licence
([public layout](public-layout.md)). To start from R0's encoder instead of
ModernBERT-base, rebuild the file from its parts, check its SHA-256, and pass
it to step 5:

```sh
$PY $PYDIR/public_a.py parent --upstream modernbert-base \
  --encoder r0-companion/model/encoder.safetensors --out parent
```

The parent keeps the upstream tokenizer and classifier config, and takes every
encoder, prediction-head and classifier tensor from R0's encoder, which must
have exactly the parent's tensors, shapes and FP32 type. Its manifest records
the file's digest, so the parent has its own `artifact_id`. Steps 6 to 13 are
unchanged; step 7 fits the normalization to the new parent's output.

- **Copies earn nothing.** An artifact within the near-copy distance of
  either reference fails the public-copy gate unless it scores more than half
  a band above it ([miner guide](miner-guide.md#13-rewards)). A warm start
  must move away from R0 by more than retraining noise.
- **What is served, not how it is trained.** The allow-listed graph classes
  limit what the scorer serves. You may train larger teachers privately and
  distil them into a served class.
- **Attribution.** R0's package `NOTICE.md`, which also covers the
  companion, says how to credit R0 and its base model.

## Smoke run

Run on November 1, 2026, on the maintainers' Linux x86_64 host (the tested
platform above), with the commands above and one difference: step 8 ran
`cp $RECIPE/protocol-smoke.json config/protocol.json` in place of
`cp $RECIPE/protocol.json config/protocol.json`, so the trainer used
[`protocol-smoke.json`](../../grammar/crates/grammar-eval/python/public_a/protocol-smoke.json)
(one epoch, two Dev-iter evaluations, patience 2, GPU memory cap 35%). A
second run on the same host, by a reader following this guide, reproduced
every count, metric and size below. **These numbers check the pipeline. They are a smoke,
not a result:** one short epoch, one seed, public data only, and Dev-iter
both chose the checkpoint and reports it.

| Step | Wall time | Peak host memory | Notes |
|---|---:|---:|---|
| Parent (CPU) | about 1 min | | parent config and tokenizer equal the pinned bytes |
| Bundle | seconds | | 6,137 rows: 4,201 Train, 805 Dev-iter, 1,131 CAL |
| Embed (GPU) | 1 min 41 s | 1.4 GB | 4,201 Train rows, none unsupported |
| Train (GPU) | 6 min 26 s | 3.7 GB | 132 steps of 32 rows; peak CUDA allocation 4.7 GB, peak device memory in use 8.6 GB |
| CAL heads (GPU) | 54 s | 2.0 GB | 1,131 rows |
| Release (CPU) | 27 s | 3.2 GB | |
| ONNX export (CPU) | 1 min 17 s | 3.3 GB | |

Training and selection (uncalibrated balanced involvement Brier on Dev-iter;
lower is better; 94 human and 711 AI-involved rows):

| Evaluation | Step | Dev-iter Brier | Humans flagged at 0.5 | AI-involved rows flagged at 0.5 |
|---:|---:|---:|---:|---:|
| 0 (the untrained start) | 0 | 0.2625 | 94 of 94 | 711 of 711 |
| 1 | 66 | 0.1290 | 2 of 94 | 390 of 711 |
| 2 (selected) | 132 | 0.0965 | 1 of 94 | 480 of 711 |

At step 0 the model reproduced the parent classifier exactly (largest origin
difference 6.0e-8), as the trainer's start gate requires.

Calibration on CAL (1,131 documents, 94 human), as fitted in sample:

- the isotonic map was selected (no fallback); origin temperature 1.699;
- CAL Brier 0.0995 uncalibrated and 0.0839 after the map (in sample, so not
  an estimate of held-out error);
- the AI attribution threshold qualified at 0.830. Known-correct named rate
  0.117 over 1,024 rows of roster generators (42 source groups); all 13 rows
  of the three generators outside the roster were rejected.

Export and checks:

- `export_a_onnx.py`: `qualified: true`, structure digest `b5440ec2…42a731`
  (the plain class), 601,028,257 bytes, largest `h` error against unbanded
  torch 1.4e-5.
- `slop_ninja_artifact_check check-dir`: admitted as `a-plain-f32band-v1`,
  150,058,763 graph and 244,069 head parameters.
- `slop_ninja_miner pack`: an object of 610,660,044 bytes. `check`: the
  artifact spec check passed; the structural check was incomplete without a
  coordinator; exit 3.
- No JSON role file contains a local path; the normalization runs appear as
  `norm/run-000`.

Digests. The two runs produced byte-identical encoder weights and graph. The
role files record the digests of the scripts that wrote them, including
`public_a.py`, so the role files, the object and the package match only for
the same revision of those scripts; the first run built its parent with an
earlier revision of `public_a.py`, so its role files differ from those below
in their recorded digests alone. The table below is for the revision whose
`public_a.py` has SHA-256
`56cb6469b5df35b1e29e7d206de5aeb6663c889d7e0135f9cba9f4156e4a91c0`, before the
normalization fit and the release exporter recorded portable paths. Their own
digests are recorded too, so at later revisions the contract and the files
that bind it are expected to differ in recorded digests alone; the smoke has
not been rerun since. The smoke gave:

| Output | SHA-256 |
|---|---|
| `release/model/encoder.safetensors` | `b9b7ca995b92073cab7f77198b88997c98d5cb1ce5dfb654d7a82db71fae58fc` |
| `a-fp32band.onnx` | `bc8ed2187365a24e7974c2b3644104130fed27f8de8737d1f198e342c70d2b86` |
| `heads.json` | `acbe0eaafcec2c9336f4e3ea3e697a8c2480899fef2989b818ef9b63d16b97e5` |
| `contract.json` | `952bf99e9be96f358ae55080a5884602a74c99843fbcd624ec9d0d4aa036e33a` |
| `calibration.json` | `8a9adf82a86787b37c7cd607b128abe2f3ba149043b46b3e2e7f10f1d861119c` |
| `manifest.json` | `df12c625fdb50e05b07f4a5fdf6f58319408028841b08f9b7fbc10bc4a58e3b0` |
| `tokenizer.json` | `6c8aaa9a542084f2457eab775d4eeb51f92a70c0fd9de28d5edb0ddec3c08d30` |
| `artifact.obj` | `f7e362624f2b39b6709645c605fe48b43c0fdf0c6669967f1cef919c5a7096a0` |
| inference content (`pack`) | `4b2dc52b7e68de854fe09b70aa3e330dfa67ab2a04e8895e73613129f9f56808` |
| package (`check-dir`) | `8b54e717cd65dc681da651519880b43e1fec8eb9fd766ea8b5b4ce8c99a3c164` |

`sha256sum` gives the file digests. These come from the smoke protocol; the
full protocol has not been run.

Disk: about 14 GB for the working directory, of which 6.8 GB is the Python
environment, 0.6 GB the download and 3.4 GB trainer checkpoints. The Rust
build output in the checkout adds about 1.2 GB and the restored datasets
0.13 GB.

## Limits

- **No quality claim.** The recipe has not been evaluated on a held-out
  cohort, against R0 or against any detector. Dev-iter chose the checkpoint,
  so its score is optimistic.
- **Narrow data.** Two generators have Train rows; the other 31 roster names
  have none, so their attribution is untrained. The human rows are historical
  proxies, mostly book excerpts and encyclopedia text.
- **Exposed data.** Every row is public, and some appeared in earlier
  research. Published texts are not hidden evaluation data.
- **Platforms.** Only Linux x86_64 with CUDA 12.8 wheels was run (see the
  table). The ONNX export is tied to the pinned torch and transformers
  versions.
- **The roster rule text.** The contract's `ai_roster_rule` text is pinned by
  the spec and describes how R0 derived its roster ("sorted generator labels
  of Train rows with a positive cohort weight"). A recipe artifact must carry
  the same text, so its contract describes its fixed 33-name roster wrongly:
  only 2 names are supervised. `fit/fit.json` and the release manifest's
  `limits` list the supervised names. Changing the text is a new spec
  version.
- **Warm start not trained end to end.** A parent built from R0's encoder was
  built and loaded on CPU on November 2, 2026; no training run has started
  from one.
- **No small sample.** No small artifact passes `check`: the graph class fixes
  the graph at 601,028,257 bytes of a real export. The smoke run is the
  shortest path to a passing artifact.
