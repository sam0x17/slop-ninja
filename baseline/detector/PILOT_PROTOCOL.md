# First detector pilot

This protocol was written before fitting any detector to the acquired corpus or
opening test predictions. It defines a small reference-candidate experiment.
Its results cannot qualify a subnet detector or establish Pangram parity.

## Corpus and partitions

The seed contains 160 works: 80 PLOS scientific abstracts and 80 historical
Wikinews articles. Acquisition is a bounded convenience sample. Historical
publication is weak human-origin evidence; PLOS's current XML endpoint does not
prove that the retrieved bytes existed at the original publication date.

The Rust splitter uses `slop-ninja-origin-pilot-v1` as its seed, merging exact
and normalized duplicate groups before hashing. It assigned 113 source groups
to training, 12 to development, 16 to calibration and 19 to test. Every descendant
inherits its source family's partition. No partition is rerolled to improve a
score. Shared events, authors and near-duplicates beyond these source families
remain possible confounders; this is not an author-held-out evaluation.

For each source, Qwen3-30B-A3B-Instruct-2507 produces one source-conditioned draft
and one light to moderate copyedit. The draft supplies recorded model-composed
prose; the copyedit supplies weak mixed-origin evidence with a historical-proxy
ancestor. Neither is an observed contemporary human/model collaboration. Exact
requests, responses, source attribution, model revision, quantization, runtime,
settings and rejection reasons remain in the local run records.

Use the frozen generation prompts in `src/bin/generate_samples.rs`, temperature
0.7 and a maximum of 1,800 output tokens. Admit only completed responses within
80–800 words and half to twice the source length. Reject unchanged outputs,
reasoning/formatting wrappers and source-copying drafts under the executable
copy check. This is a corpus admission rule, not an independent fidelity test.
Failed calls remain visible. Do not retry based on detector scores. If a source
family has an inadmissible output, exclude that whole family from the balanced
pilot and report the resulting counts and selection bias. Never silently replace
failed generations or omit only the difficult class.

An additional Mistral-Small-3.2-24B-Instruct-2506 cohort uses only the frozen test
sources with the same operations. It measures a limited change of generator.
Neither those outputs nor their scores enter fitting, selection or calibration.
Both adopted cohorts use native MLX-LM 0.31.3 / MLX 0.32.2 on the M5 Max, with
separate recorded checkpoint and tokenizer configurations. The Mistral tokenizer
requires its recorded regex correction. The comparison does not isolate a
checkpoint-only effect from those tokenizer differences.

Before fitting, we moved Qwen generation to native MLX after the laptop became
available on AC power. An actual unseeded, temperature-0.7 HTTP smoke produced
128 tokens in 1.86 seconds; the Studio runtime was substantially slower. The
adopted `qwen-native-v3` run starts all 320 tasks afresh. Preserve the earlier
GGUF and Studio MLX runs in full as operational preflights, without incorporating
their outputs into this pilot. This decision uses measured runtime and compute
availability, with no fitted detector or test predictions available. Prompts,
source families, partitions, sampling settings and admission rules are unchanged.
Training and evaluation remain on the Studio after a verified corpus transfer.

## Prespecified candidates

Fit three Rust linear softmax controls: word occurrence, grammar, and combined.
Use the implementation defaults: at most 8,192 coordinates, minimum document
frequency 2, 300 maximum epochs, learning rate 0.03, L2 0.01 and patience 25.
Vocabulary and feature scaling use training data only; select the checkpoint by
development log loss. Fit a scalar temperature on calibration data only.

Train the pinned ModernBERT-base three-class encoder for three epochs with seed
17, batch size 4, AdamW learning rate 0.00002 and the recorded default weight
decay. Use MPS for training and the bundled CPU reference path for inference.
Select its epoch by development log loss, then fit the calibration temperature.
Use a 2,048-token limit including special tokens, with no truncation. Audit every
shard's lengths before training. Any overflow requires an explicit protocol
amendment before model fitting; it never authorizes selective deletion.

Report all four prespecified candidates. If choosing a first reference candidate,
use the lowest uncalibrated development log loss; do not select using the test
or Mistral result. Report ties and differences too small for this development
sample to resolve. Do not conduct an adaptive hyperparameter sweep on this test.

## Evaluation and publication

After weights, calibration and operating points are frozen, open each final test
once. Report three-class Brier score, log loss, confusion and per-class recall,
alongside uniform and training-prior controls. For the document-level event
`model_only or mixed`, freeze empirical human false-positive thresholds at 1%
and 5% using calibration data, then report test events and denominators. Sixteen
calibration human proxies and nineteen test proxies are far too few to establish
a reliable rare-error bound. A bootstrap with no observed errors does not prove
a zero population error rate.

Separate source-register and generator results where supported. Report the
weak-label, short-text, limited-register and single-training-generator coverage
limits prominently. No claim about author identity, genuine collaboration,
meaning preservation, all languages or detector evasion follows from this pilot.

Before fitting, add a secondary formatting control motivated by the eight-output
generation preflight: paragraph counts fell from 38 to 22 across those pairs.
For each admitted test cohort, run `whitespace_control` to create a separate view
of all frozen test roots and descendants. Collapse the fixed Unicode White_Space
set to single ASCII spaces and trim surrounding whitespace, preserving all words,
punctuation and their order. Keep record IDs, family membership and origin labels;
bind original and derived text hashes in the accompanying summary. Retained
source/generation metadata describes the original writing, and the deterministic
formatting change is recorded separately. Evaluate each view as a paired
observation of the same writing, and count each source family once in uncertainty
estimates.

Evaluate this control only after the model, calibration and thresholds are
frozen. Report its results separately to expose reliance on paragraph and
whitespace formatting. The original test remains primary; the control changes
no partition, admission, fitting, candidate-selection or calibration rule. No
control results may guide model selection or an adaptive parameter search.

Pangram remains the subnet's initial external quality anchor. This pilot does
not establish an anchor change. A paired Pangram experiment still needs a
separately declared sample, repeat rule and API spending cap before execution.

Commit code, pinned source metadata, licenses, aggregate reports and model
manifests. Corpus text and invocation logs stay in ignored `data/`. Any published
weights must carry their actual weak-evidence status, exact runtime and source
lineage, rather than a claim of launch qualification.
