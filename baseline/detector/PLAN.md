# Reference detector plan

Planning draft, 2026-09-10. This is the proposed first implementation under
`baseline/detector/`, following the [whitepaper](../../whitepaper/slop_ninja.pdf).
The immediate deliverable is a working, distributable reference **AI-origin
detector**. A's author-gallery interface remains a separate follow-on capability;
the existing author metric is useful comparison code, not an origin detector.

The main acquisition work is a substantial, diverse training collection whose
text terms permit commercial use. Start with the [source register](SOURCES.md),
then record actual eligible counts and item-level evidence before training.
Publicly accessible data and a permissive loader license are insufficient by
themselves. Existing Blog Authorship weights and private correspondence/book
experiments do not enter this reference release.

## 1. Deliverable and boundaries

For English prose, return calibrated probabilities in a fixed order:
`human_only`, `model_only`, `mixed`. Also return their combined model/assistance
probability, the artifact version and any unsupported-input or abstention status.
These are document production-history probabilities, not percentages of words
written by AI. Unknown provenance is a dataset status, not a fabricated fourth
training class or evidence that a text is human.

The release includes executable inference, weights, preprocessing, calibration,
dependency locks, a content manifest, reproducible evaluation and a model card.
Miners and validators must be able to run it without a private service or
training-corpus access. Runtime inputs contain text and declared options, never
miner identities, labels or benchmark assignment metadata.

Reference status means a reproducible starting model. Pangram parity, a qualified
reward-bearing A artifact and a deployed subnet each require further evidence.
This project does not need to implement B, settlement or the full 24-task A/B
tournament before training A. Use the relevant A cases from that planned pilot
to check the interface; use a separate, much larger dataset to measure detection.

## 2. What we can reuse

| Existing component | Planned use | Change or limitation |
| --- | --- | --- |
| `grammar-core` syntax and feature extraction | Word occurrence and fourteen grammar/lexical feature families | Fit vocabularies and transforms on training data only; specify unseen-feature behavior |
| `grammar-spacy::parse_batch` | Batched, versioned dependency parsing | Bundle the Python bridge and parser assets; remove dependence on the original checkout's compile-time path |
| `grammar-eval` sparse author profiles | Separate author/style comparison | Author ranking logits are not calibrated origin probabilities |
| Portable author coordinate models | Architectural reference only | Several modules are binary internals, not public library APIs; do not couple the new detector to them |
| Root Pangram client and recorded-observation format | Later paired external evaluation | Keep provider calls explicit, budgeted and outside automated tests |
| Existing Global Voices/PLOS source screens | Starting provenance and acquisition work | Recheck item rights and origin evidence; provisional extraction is not an admitted training release |

This machine has 128 GiB memory and an available Apple MPS backend. PyTorch and
spaCy are installed; Transformers, safetensors and an ONNX runtime are not.
Use an isolated, pinned ML environment. Begin locally, measure throughput on a
small batch, and size a full run from that result before considering rented GPUs.

## 3. Acquire training data in two evidence lanes

The first collection should combine Global Voices original prose, eligible
PLOS/PMC articles and PLOS blog posts, Wikinews, and selected government-authored
text. This supplies contemporary reporting, opinion, scientific explanation,
blog prose and administrative/argumentative writing. Wikipedia and rights-traced
detector subsets can supplement it under a separate ShareAlike policy. The
[source register](SOURCES.md) records primary license links and exclusions.

Keep two independent dimensions in every record:

| Dimension | Values and use |
| --- | --- |
| Rights | Approved, conditional or excluded for the specific training/release route; retain the exact license and attribution |
| Production evidence | Documented workflow, historical publication proxy or unknown; only adequate workflow evidence supports definitive provenance evaluation |

A pre-LLM publication date is useful weak supervision, not proof of human-only
production. Preserve actual revision dates, not just page-creation dates. Strong
evaluation needs new, independently documented writing, especially because old
public text may already be in an encoder's pretraining corpus.

Acquire in stages:

1. Review about 100 records per candidate source. Measure usable length, rights
   exceptions, quotation contamination, extraction quality and metadata coverage.
2. Build a pilot of up to 10,000 distinct original source works across at least
   four registers. This is an acquisition target, not an existing inventory.
   Avoid letting one large scientific or encyclopedic source dominate.
3. Commission or accept explicitly licensed writing alongside the public corpus.
   The whitepaper's initial target is 50 writers, twelve independent pieces and
   two edits each. That would provide 600 originals and 100 edit sessions before
   exclusions. Preserve drafts and assistance records, and split writers before
   collection assignments. This is a pilot, not enough for strong claims about
   rare false positives across all populations.
4. Expand toward 50,000-100,000 source works only after measuring license yield,
   label quality and learning curves. Grow documented mixed workflows and
   informal writing alongside volume. A later author-reference track needs
   hundreds of qualified writers with multiple independent works each.

The principal missing registers are ordinary personal writing and contemporary
informal prose with reliable process records. Fill them through explicit grants
or commissioned work, rather than quietly admitting the noncommercial Blog
corpus. Agreements must specify commercial training, public model distribution,
private evaluation, any raw-text release and external detector submission.
Customer inference grants none of these rights automatically.

## 4. Record generations and editing histories

Freeze the label policy before producing examples:

| Workflow | Initial treatment |
| --- | --- |
| Documented human composition without model-generated wording assistance | `human_only` |
| Model drafts from a writing brief, including subsequent model-to-model revisions | `model_only` |
| Human revision of a model draft, or model editing of a human-authored draft | `mixed`, with the complete authoring chain retained |
| Human editing of human text | Human control, where the process is documented |
| Uncertain assistance, disputed source history or an unsupported assistance subtype | Retain for review; do not invent a hard label |

Distinguish reference facts and prompts from an authorial draft. A human-written
instruction does not make every model answer mixed. Copied human prefixes,
translations, accepted suggestions and partial replacements require explicit
rules and retained parent spans. Model-to-model evasion remains model-only.
Text alone cannot always distinguish different workflows that produce the same
wording, so uncertainty and class-specific failures must remain visible.

Generate matched examples from the same licensed briefs, source families,
registers and length ranges. Include independent model families, prompt variants,
decoding settings, source-conditioned rewrites and fresh composition. Plan for
at least four training generator families and a withheld family, subject to
their actual licenses/output terms and the approved generation budget. Reserve
later model versions and prompt families for additional transfer tests.

Record exact prompts, model/weight/provider versions, output terms, settings,
timestamps, outputs and all revision steps. A model's suggestions are not proof
of human editing. Algorithmic splices and perturbations are useful separately
identified stress tests; they do not replace real collaborative-writing records.
Retain human-only edits and formatting changes so that editing itself cannot
become a shortcut for the mixed label.

## 5. Manifest, extraction and leakage control

Use Rust for acquisition, extraction, SQLite metadata, deduplication, splitting
and export. Preserve raw bytes and exact extracted spans under ignored
`data/baseline-detector/`; keep credentials and private writer agreements there
as well. Source registries and aggregate reports can be public when permitted.

Each record binds its original URL/revision, acquisition terms, license evidence,
attribution, allowed uses, original author/coauthors/editor/translator roles,
publication and retrieval dates, content hashes, extraction version, production
evidence, generation/editing ancestry and source-family identifiers. Rights to
the acquired text, public redistribution and the intended model release are
separate decisions. Store loader and pretrained-weight licenses separately.

Split connected source families before fitting or generating descendants.
Originals, quotations retained as separate records, excerpts, translations,
paraphrases, article revisions and all synthetic descendants stay together.
Deduplicate across imported corpora as well as within them. Account for shared
prompts, topics and authors when constructing the stricter transfer evaluations.
Publish discarded counts and exclusion reasons.

Use distinct training, model-selection, calibration and sealed final-test sets.
Select hyperparameters only on development data; fit thresholds/calibration on
the calibration set; open the final test once for a frozen candidate. If a test
informs another round, retire it and obtain new confirmation material. Strip
metadata, prompt wrappers and scrape boilerplate consistently. Match source,
topic, length and register across origins so class labels cannot be inferred
from which dataset supplied the paragraph.

## 6. Train controls and a principal encoder candidate

| Candidate | Purpose | Implementation |
| --- | --- | --- |
| Uniform and training-prior predictors | Fixed probabilistic controls | Rust |
| Word-only, grammar-only and combined linear softmax models | Establish how far the existing representation gets; expose source shortcuts | Rust training and inference, training-only vocabulary/transforms |
| ModernBERT-base with a three-class origin head | Principal learned text-encoder comparison | Python/PyTorch for ML; pinned inputs/weights and an exportable inference graph |
| ModernBERT-large with the same origin head | Matched capacity comparison after the current base control/consistency fits | Same inputs and comparison rules; separate verified upstream pin and correct artifact identity |
| Encoder plus grammar/word features | Optional fusion ablation | Attempt only if independent development results justify the extra parser/runtime cost |

ModernBERT-base is the whitepaper's concrete 149M-parameter candidate, with an
Apache-2.0 weight release. Its published context capacity is up to 8,192 tokens;
that does not imply this first classifier is validated at that length. Pin the
checkpoint revision and test classification/export compatibility early.
[Official model card](https://huggingface.co/answerdotai/ModernBERT-base).

The [capacity and gradient-transfer plan](CAPACITY_AND_GRADIENT_TRANSFER.md)
extends this comparison to the published 395M large backbone, explicit-feature
neural models and fusion, while separating capacity, data coverage and training
targets. It defines how later A gradients can rank B's grammar/lexical edits,
and why provider score agreement and edit-direction transfer need separate
evaluation. A provider-surrogate head, if trained, must remain distinct from
the provenance probabilities described below.

Start with a bounded context, for example 1,024 tokens. Evaluate short inputs and
long documents separately. Reject unsupported lengths or use an explicitly
trained and calibrated chunk-aggregation rule; never silently truncate a
manuscript and present the result as a whole-document verdict.

Fit development learning curves at increasing numbers of source groups. Use a
small, predeclared hyperparameter search and confirm the selected configuration
across several seeds. Compare strong-workflow-only training with the permitted
historical-data supplement; report performance on both evidence lanes. Do not
select the winner using the final test or Pangram agreement. A fusion model must
beat its simpler components enough to justify its inference cost.

## 7. Calibration, evaluation and Pangram

Measure multiclass Brier score, log loss, per-class precision/recall, confusion
matrices and calibration. For the combined AI-origin event, use
`P(model_only) + P(mixed)`. Freeze threshold selection at declared human
false-positive operating points, initially including 1% and 5%, and report the
observed false-positive rates and sensitivity with uncertainty. An operating
target is not an achieved bound. Account for clustering by writer and source
family; many excerpts from one work do not provide independent evidence.

Report failures on unseen generators, source families, registers, time periods,
human writers with different English backgrounds, short inputs, formulaic human
prose, heavy editing and genuine mixed workflows. Include formatting and prompt
injection controls, meaning-preserving rewrites, hard human negatives and
highly edited model positives. Inspect false positives before increasing model
size. If mixed examples are insufficient, report that limitation and keep any
binary-only result as an intermediate experiment rather than claim a validated
three-class reference.

Run Pangram on a presampled, rights-approved evaluation set after the models and
comparison policy are frozen. Use the exact same analyzed text and the documented
production labels. Reuse these provider observations across candidate models;
additional A candidates need no additional calls for unchanged texts. Keep
provider versions, repeats, failures and report-selection rules. Pangram's
fraction is not an interchangeable document-origin probability; compare
sensitivity at the common false-positive operating points, and validate a common
event mapping before comparing calibration scores.

Use a capped pilot to establish cost and repeat behavior, then a separately
sized confirmation study. Predeclare the number of texts, word limits, repeats
and maximum spend using the current provider rate. Query failures and retries
consume that budget. This plan authorizes no new API spending by itself.

Pangram starts as the quality anchor. A superior detector can replace it only
after fresh confirmation under the whitepaper's frozen comparison policy.
Continue monitoring Pangram so an improved provider can become the target again.
Publishing a first reference model does not claim an anchor change. Preserve A's
fixed public reward normalization and the separate B-panel selection rules.

## 8. Package the reference runtime

Proposed layout:

```text
baseline/detector/
  PLAN.md
  SOURCES.md
  Cargo.toml, Cargo.lock
  src/                 # corpus commands, features, linear models, metrics, inference
  ml/                  # encoder training/export and pinned ML requirements
  schemas/             # records, inference requests/results, artifact manifests
  configs/             # frozen training, selection and calibration recipes
  tests/fixtures/      # small synthetic/publicly permitted protocol examples
  reports/             # aggregate evaluation, source audit and model card
data/baseline-detector/ # ignored source text, checkpoints, runs and private evidence
```

Start as a standalone Rust crate with path dependencies on `grammar-core` and
`grammar-spacy`. Give every artifact its own versioned preprocessing contract.
Do an early encoder export and runtime spike before spending on a full training
run. Prefer Rust inference with a verified exported runtime when supported;
if conversion changes outputs or lacks operators, keep a pinned reference
PyTorch runner for the ML portion and report that limitation.

Bind tokenizer/parser files, features, weights, calibration, output ordering,
postprocessing, executable source/build, dependency/environment locks,
runtime/precision, quantization, limits and numerical tolerances in the
inference-content hash. Define a canonical rule for resolving numerical boundary
disagreements; a tolerance alone cannot decide conflicting threshold outcomes.
Exclude owner identity/signatures from model identity. Provide an offline
inference command, batch evaluation and canonical input/output test vectors.
Training can use MPS; qualification uses the declared reference runtime and
measured tolerances, not a promise of bit-identical results on every GPU.
Test portability from a fresh checkout without the author's original paths.

Publish weights as a versioned artifact with SHA256 and a model card, with code
and recipes in Git. Preserve required upstream license/attribution notices.
Publish aggregate dataset provenance and exclusions; release raw text only where
its separate permissions permit it. The first release should explicitly state
supported languages/lengths, evidence quality, test populations, detection
limits, calibration and CPU/MPS speed and memory measurements.

## 9. Execution milestones and acceptance

| Milestone | Concrete output | Condition to proceed |
| --- | --- | --- |
| Source screening | Source register, review samples, rights/provenance counts, extraction pilot | A useful eligible pool across registers; no unknown-license rows in the admitted manifest |
| Dataset contract | Versioned manifests, label examples, grouped splits and reserved confirmation cohort | No source/duplicate leakage; ambiguous histories excluded from hard evaluation labels |
| Pipeline and runtime spike | Small end-to-end dataset, linear inference and encoder export check | Reproducible outputs and complete lineage; failures retained |
| Recorded production pilot | Matched generations and real editing sessions, with cost/yield report | Enough evidence for the classes being trained; control for source and task shortcuts |
| Baseline tournament | Word/grammar controls, encoder run, fixed development comparison | Select by predeclared metrics; diagnose false positives and weak-label effects |
| Frozen evaluation | Calibration and sealed test report, shared Pangram observations | Honest uncertainty and slice reporting; no claims beyond supported evidence |
| Reference release | Runnable immutable bundle, model card, commands and hashes | Fresh-install reproduction; admitted data/model terms support the intended release |

The first reference should beat fixed probabilistic controls on the frozen
evaluation and provide usable, calibrated outputs. Record the size and
uncertainty of the improvement rather than inventing a Pangram-level target the
first run must meet. Qualification for live rewards uses the stronger acceptance
limits in the whitepaper and may require a larger documented evaluation cohort.

Run meaningful checks for split leakage, lineage/label handling, probability
validity, artifact tampering and reference execution. Keep tests focused on
failure modes; do not build another large collection of implementation-mirroring
tests. For changed Rust crates, run formatting, tests and Clippy as required by
`AGENTS.md`; shared parser changes also require the real spaCy integration check.
Automated tests make no live model or detector calls.

The first implementation step is the source registry, item-level manifest and
bounded acquisition review. Full training follows the data contract and runtime
spike. Commissioning, external API budgets and any GPU rental need concrete
estimates from those pilots; there is no reason to commit to large training
spend before measuring the usable data and local throughput.
