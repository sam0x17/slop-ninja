# CFG-vector viability experiment

The latest [frozen final-Test evaluation](a-final-test.md) covers 414 queries
from sixteen new source works. The selected text residual improves mixed-inclusive
involvement Brier over frozen V6, but remains well behind mapped Pangram and
flags more human excerpts. Aggregate CFG and the four-bin probe do not establish
a viable CFG-only detector. All trained outputs, limitations and extraction/runtime
costs are reported there; Test is consumed and cannot guide further fitting.

Research experiment, updated October 26, 2026. The
[supervised shared A baseline](../grammar/crates/grammar-a-baseline/SUPERVISED_BASELINE.md)
now has trained origin, fractional-share and conditional human/model heads. Frozen
Test evaluation covers 102 documents from 15 source families, including 21
human-labeled documents. Selected leaf-grammar joint seeds 17/29/41 have Brier
0.125825 / 0.129129 / 0.134119 versus mapped Pangram 0.095745, with human FPR at
0.5 of 66.67% / 86.67% / 66.67%. All heads being trained does not establish
qualification; share, attribution, open-set rejection and Workers execution
remain unqualified. Test was excluded from model selection, while conservative
grammar exposure and historical human proxies remain explicit. The target is
parity with Pangram, or the closest supported result, prioritizing AI-involvement
quality. [Earlier structural checkpoints](../grammar/crates/grammar-a-baseline/SHARED_BASELINE.md)
and the preparation evidence below retain their dated populations and limits.
This experiment informs the input contract for mechanism *A* in the
[subnet draft](cloudflare-subnet-draft.md). Run compute on build-host only.

The subsequent [frozen V6/text-plus-CFG pilot](a-v6-cfg-fusion.md) completed 21
matched fits and Rust numeric replays on the same exposed non-Test cohort.
Development Brier is 0.05177–0.05834 for text-only joint heads and
0.04842–0.05659 for combined, versus 0.12573–0.13122 for the matched CFG-only
control and 0.07705 for mapped Pangram. The combined-minus-text mean gap is
−0.00150 with a family interval spanning zero; human false positives, weak writer
retrieval, historical exposure and serverless limitations remain. These are
Development-selected results, not a new Test or a Pangram-parity qualification.

The [fresh-family comparison](a-balanced-baseline.md) then completed 39 fits on
929 non-Test records, with two known generators, a Calibration-only generator,
measured partial rewrites and independent writer works. Balanced Development
involvement Brier is 0.04103–0.04626 for text and 0.04474–0.04762 for text+CFG;
CFG lowers share error and observed human flags but does not improve that primary
metric. Aggregate and four-bin ordered CFG remain weak. The public snapshot
contains 937 records including separate references. Six subsequent V6 warm-start
fits preserved original predictions at initialization. Their text-only residual
has Brier 0.03664–0.03910 and share MAE 8.27–8.73 points; adding CFG gives
0.04287–0.04555 and 8.81–9.27. Test was closed during selection. These 215-Train-example,
frozen-encoder experiments do not establish CFG's ceiling or Pangram parity.
Pangram's content percentages remain distinct from our share and confidence targets.

The next [Train-expansion comparison](a-balanced-baseline.md#train-expansion-comparison)
completed six more matched fits after adding 1,049 historical-source examples.
With 1,264 Train queries, balanced Development Brier worsened to
0.07354–0.08332 for text and 0.07044–0.07211 for text+CFG. The earlier text
residual remains selected. The domain mixture and rewrite recipe changed along
with sample count; this does not show that additional representative data cannot
help. All data and failed/excluded generation evidence remain retained. No new
Pangram calls or final-Test reads were needed. This CFG input consists of pooled
grammatical-leaf counts and opportunity masks; it preserves neither sentence
order nor tree topology.

## Question and intended model

Can a model trained entirely on resolved CFG-derived inputs estimate AI
involvement and share, and attribute writing to human or AI-model profiles well enough to
serve the launch API? One *A* artifact must produce all outputs from a shared
learned document encoding in one invocation. Known AI models are predefined
authors in the reference gallery; human profiles use independently held
reference works.

The intended benefit is mutual: author and generator distinctions can improve
origin detection, while production-history supervision can improve attribution
by separating writer habits from patterns introduced by generation or editing.
Both tasks shape the shared encoder. This is an empirical hypothesis, and
neither predicted output replaces the other or supplies an authoritative label.

The intended model returns:

```text
origin = {human_only, model_only, mixed} probabilities
ai_probability = origin.model_only + origin.mixed
ai_confidence_percent = 100 * ai_probability
ai_share_percent = 100 * expected_model_produced_word_share
0 <= 100 * origin.model_only <= ai_share_percent <= ai_confidence_percent <= 100
human_probability = origin.human_only + origin.mixed
attribution.human = conditional human-profile matches or other_human
attribution.ai = conditional AI-model matches or other_ai
```

The probability measures model involvement under a declared production-history
policy. Binary training labels are zero for documented human-only work and one
for model-only or mixed work. The model learns a probability from those labels;
it does not convert a hard decision into a percentage. Documented three-way
histories additionally supervise the origin probabilities. Every completed
inference returns numeric share and confidence scores. Unavailable inference is an explicit failure,
never a zero score. Freeze calibration with the artifact; benchmark operating
thresholds do not impose a required public boolean.

Confidence estimates the probability of any AI involvement; share estimates the
expected fraction of final words in model-written or model-rewritten spans.
Both are required document-level outputs. Human-only work has zero share;
model-only work has unit share under this convention. A coherent estimate is
`origin.model_only + origin.mixed * expected_share_given_mixed`, where the last
term is a learned conditional expectation in `[0, 1]` for the document.
A 20% share estimate can accompany 99% confidence that AI contributed.
Confidence is not a probability that the share estimate is exactly correct;
uncertainty intervals for share require their own validation. Binary involvement
labels supervise confidence; the controlled-rewrite labels below supervise
fractional share.

Return both role attributions with their involvement probabilities, without a
presence cutoff. Each asks which contributor fits if that role contributed;
a conditional result does not assert that the role is present. Human and AI
involvement probabilities may sum above one because mixed work includes both,
and they are not shares of the text. When no named profile for a role meets
the frozen matching rule, return `other_human` ("Other human") or `other_ai`
("Other AI"). Neither fallback asserts that the identity is absent from the
gallery or proves authorship. Both can appear in mixed work. Multiple
contributors need not be forced into one identity. There is no unknown or
attribution-abstention label in a completed result; failures stay separate.
Supported named-model matches remain visible at low AI probability, without
either output overriding the other or supplying a production-history label.

Multiple output heads within this model are allowed. Separate origin and
attribution models are experimental controls only; they do not satisfy the
intended production contract. Origin quality has priority. Attribution and
open-set rejection must be reported even when they do not determine the primary
reward score.

## Available evidence and its limits

The existing [numeric baseline](../grammar/crates/grammar-a-baseline/README.md)
retains an origin-only synthetic control and frozen numeric contracts. Its shared
Rust inference module now executes the supervised model described above. The
current Train population has 229 origin labels, 169 share labels (16 fractional),
32 human-identity and 159 model-identity targets. Eight independent reference
rows are excluded from fitting and normalization. The initial structural cohort
had only origin and endpoint-share supervision; those earlier measurements and
the following preparation inventory remain historical evidence. Missing targets
are masked rather than invented.

The earlier preparation metadata inventory on build-host found:

| Retained material | Availability | Limitation |
| --- | --- | --- |
| Public v6 origin corpus | 5,463 records, 797 source families: 797 historical human proxies, 3,113 model-only, 1,553 mixed | Proxy human labels do not establish documented human-only production histories. |
| Public native cache manifest | 288 documents, 3,428 candidate spans, 14 shards | Cache presence alone does not establish current whole-document projection or untouched evaluation. |
| External public documents in that cache cohort | 281: 223 human proxies, 30 model-only, 28 mixed | The 58 generated/mixed records were originally Development; grammar exposure prevents treating them as clean held-out examples now. |
| Selected private blog sample | 75 documents from 75 author hashes | One work per author cannot support independent reference/query attribution. |
| Selected Global Voices sample | 75 documents from 16 author hashes | Must check independent works, translation credits, language, rights, and usable parse coverage before attribution use. |

The remaining seven public cache documents are outside that external-corpus
join and must retain explicit eligibility reasons. The full author-corpus
directory is absent on build-host at this inventory point. Existing selected
private texts and metadata remain in ignored storage; no private passages or
source identifiers belong in checked-in reports.

The first bounded cohort can use 28 matched source families: 28 human proxies,
28 generated drafts, and 24 mixed edits, for 80 documents. The generated drafts
use one pinned Qwen3-30B-A3B revision. All 80 were originally Development and are
now CFG-exposed. This cohort supports preparation and an initial origin screen;
it cannot establish general attribution or unseen-generator performance.

Retained campaign caches store selected derivations. They do not directly meet
the production native-vector interface, which requires paragraph-bound analyses
and its own resolution qualification. Sentence-cache projection therefore
needs an explicitly named research format. It must not fabricate production
qualification receipts or silently equate old parser profiles.

## First preparation checkpoint

Prepare a bounded export from retained real parses before paying for new
parsing or fitting. Keep an immutable input manifest and source-bound audit:

1. Join corpus records to cache candidates by verified identity, retaining
   source-family membership, original split, origin label status, and available
   generator identity. Missing generator metadata remains unknown. Do not infer
   identity from the text's style or its origin label.
2. Check candidate ordering and coverage of the entire source document. Keep
   missing, partial, stale, unresolved, and ineligible documents in the requested
   population ledger. A selected tree does not by itself certify correctness.
3. Export only measurements supported by the selected derivation. Separate
   structural categories, parent/child relations, and shape features from
   dictionary-bound lexical identities. If lexical identities cannot be
   reconstructed, mark that block unavailable. Raw source-word features may
   form a separately labelled control, but cannot stand in for CFG lexical
   inputs. IDs and hashes are categorical keys, never scalar measurements.
4. Bind every feature meaning, parser/cache identity, projection version,
   segmentation policy, source manifest, and exposure ledger. When retained
   profiles differ, report separate cohorts rather than claim one frozen parser.
   Features with no opportunities have an explicit unavailable mask; do not
   invent denominators to make rows appear complete.
5. Record feature dimensions and memory estimates before fitting. Keep selected
   derivations available for a later ordered/tree representation; an
   aggregate-count export is only one representation candidate.

All grammar-exposed source families remain Train under the numeric baseline's
strict contract. A research diagnostic that resplits these families must label
its result as conditional on prior grammar exposure and cannot satisfy the
launch holdout requirement. No silent change of the old Development labels
creates independent evidence.

The preparation checkpoint should deliver a runnable Rust exporter, bounded
real-data output on build-host, a manifest of included and excluded documents,
and commands for reproducing it. It need not train a model to be useful.
Fitting, calibration, and model-quality claims are separate recorded outcomes.

### Prepared checkpoint

The [Rust cache exporter](../grammar/CFG_VECTOR_PREPARATION.md) was run on the
80-document cohort on build-host. It produced whole-document structural features
for 70 documents: 20 human proxies, 27 model drafts, and 23 mixed edits. The
other 10 documents remain explicitly unavailable because 58 candidate spans
lack a retained ambiguity selection. No new parsing or LLM resolution was run.
These exclusions also change the class balance and must be accounted for in
any subsequent fitting or matched-family comparison. Twenty families retain
both their human and model document.

The export took 9.97 seconds with a peak RSS of 1,263,268 KiB (~1.2 GiB) and no
swap, under a one-CPU, 4 GiB limit. This measures projection from existing cache
trees, not first-time parsing or end-to-end inference. A focused exporter test
and a three-document smoke export preceded the full run.

The retained result includes all 80 population rows, source/candidate and
selected-tree bindings, input/output hashes, and historical parser/inventory
identities. Lexical and combined inputs remain unavailable. No model was fitted,
no attribution targets were certified, and no clean holdout result was produced.
The cache spans four parser engines and 899 resolved grammar identities across
six grammar/profile prefixes. Those bindings are retained; this export does
not establish one frozen representation for model comparison.
The implementation and reproduction commands are checked in; the cohort,
exports, and execution logs remain in ignored task data on build-host.

## Viability comparison after preparation

Use the same source families, requested documents, identity gallery, and label
policy for each feature comparison:

- CFG lexical features only.
- CFG structure features only.
- Combined CFG features in the intended shared model.
- A raw-text reference model as a comparator.

The recovered V6 artifact now has a qualified Linux CPU profile and a
[matched V6/CFG/Pangram replay](a-reference-comparison.md) on 315 exact documents,
with source-family exposure audited and mixed examples kept separate from its
historical headline result. The recovered release remains unchanged. Native
frozen projection matched all 450 non-Test vectors,
including eight references, exactly; nine synthetic projector tests and a
three-document unlabelled end-to-end smoke passed. The numeric library's
`wasm32-unknown-unknown` build passed, while full Workers execution is unqualified.

Within a fixed feature representation, compare the shared model against an
origin-only control and an attribution-only control. Keep encoder capacity,
training populations, training budgets, and gallery policy matched. Measure
whether attribution supervision helps or harms origin detection and whether
origin supervision helps or harms attribution. Each single-task control omits
the other task's supervision; declare the capacity and compute matching rule
and report any unavoidable differences. Do not conflate representation
changes with different training populations or larger budgets.
An unsuccessful count-vector baseline warrants an ordered/tree-feature test
before rejecting the entire CFG hypothesis.

For clean evaluation, freeze the grammar, resolver, projection, gallery
construction procedure, and feature catalog before exposing new holdout data.
Keep source works and all generated or edited descendants in one split.
Separate Train, Development, Calibration, and final Test. Fit vocabulary,
normalization, hyperparameters, open-set rejection policy, calibration, and
benchmark operating thresholds without final-test information. Parsing previously
unseen holdout text with the frozen resolver is allowed. Adapting grammar, resolution policy, projection,
or feature selection to test text, parse failures, or labels creates exposure
and requires a new holdout.

Attribution needs multiple independent works per known author or generator.
Include unseen writers, unseen generator families and versions, different
topics/prompts/registers, and models writing in a requested human style.
Retain generator version and production-history evidence outside the model's
numeric inputs. Multiple contributors do not receive a fabricated single label.

## Controlled replacement-span experiment

Prepare a separate bounded experiment using documented human-only sources with
the necessary permissions. Freeze the source population, generators, requested
replacement fractions, maximum attempts, and compute budget before generation.
Include unchanged controls, partial section replacements at several sizes and
positions, dispersed edits, and complete rewrites. Model outputs replace only
the selected spans; retain the rest byte for byte. All variants of a source
stay in one split.

For each nonempty final document, define `replacement_span_share` as the number
of final words in recorded model-produced replacement spans divided by all
final-document words, under one fixed word-counting and boundary convention.
Record exact original and replacement bytes, final span boundaries, original
and replacement lengths, model versions, prompts, and the assembly procedure.
Keep failed attempts visible. Use the realized fraction, which can differ from
the requested one when a rewrite changes length. Words copied unchanged within
a model-produced span count under this provenance convention; the target does
not measure semantic dependence or editing effort.

Retain the binary involvement label and production history separately. A full
model rewrite of a human source can have 100% replacement-span share and still
have mixed history. Identical final text can have different recorded histories,
so the extent label may not be recoverable exactly from the text. Do not infer
fractions for arbitrary mixed histories from the mixed label, detector scores,
or a requested rewrite percentage. Such examples lack this target unless the
required assembly evidence exists.
Documented untouched human examples supply zero-share targets. Complete model
generation records with every final word retained from model output supply
one-share targets. Intermediate shares need the span evidence above.

Control for splice and formatting cues, vary generator and editing intensity,
and test transfer to unseen sources, generators, and rewrite procedures. Keep
calibration and final evaluation separate from fitting. Report absolute and
squared error in realized share, signed bias, calibration by share range,
endpoint behavior, and uncertainty grouped by source family. Retain identical-
text/different-history cases as a separately identified limit. Show coverage
and failures for the requested population, without silently evaluating only
successful or easy replacements. Measure any effect on the shared model's
AI-involvement and attribution performance as well.

The extent output belongs to the same shared model and versioned contract,
and needs qualification before document-level paid service. Its measurements
remain separate from binary Brier utility. Confidence remains the primary
reward comparison; any secondary share reward must be frozen before a cohort.
Successful controlled-splice tests do not by themselves establish accuracy for
arbitrary revision histories or token-level authorship.

## Later sentence scores and explanations

Document-level share and confidence come first. Later experiments would return
both percentages per sentence, preserving exact source spans and label meanings.
Paragraph-level summaries follow sentence-level qualification. Keep exact final
replacement offsets and a versioned sentence/paragraph segmentation contract so
future labels can be derived from retained spans, including partial sentences.
They need sentence-level provenance, contextual calibration, and consistency
with paragraph and whole-document outputs. Share can aggregate with final-word
weights over complete, nonoverlapping partitions. Paragraph and document confidence require joint calibration;
sentences are dependent, so neither averaging their confidences nor multiplying
their complements establishes that probability.

Production histories and measured replacement spans supply the supervision.
Pangram's content percentage is a comparator observation, not ground truth for
either AI share or any-AI confidence. A Calibration-fitted conversion of that
percentage must be identified as our confidence proxy, not Pangram's native output.

Later explanations could identify influential words and CFG productions. Link
features to source spans, change or remove them under meaning/context controls,
reparse, and measure the model response. Test stability across valid parses and
innocuous formatting. Attention weights or generated narratives alone are not
faithfulness evidence. An influential feature explains the model calculation,
not actual authorship. These extensions are not initial launch gates.

## Measurements and decision

Primary origin measurements are source-family-balanced binary Brier score on
`ai_probability`, AI-involvement calibration, and sensitivity at fixed human
false-positive rates. Mixed and model-only examples both have positive
AI-involvement labels. Retain three-class Brier as a diagnostic; better
mixed/model-only discrimination must not conceal worse AI-involvement detection.
Report known and unseen generators, human proxies, documented human histories,
and mixed edits separately. Confidence intervals must account for source-family
dependence. A small proxy cohort may be unable to establish a low false-positive
rate; report its uncertainty rather than promise a rate it cannot resolve.

Secondary measurements are attribution retrieval, open-set rejection, and
cross-topic performance on independent reference/query works. Rejection counts
completed decisions that accept no named match for the evaluated writer or
generator role, with `other_human` and `other_ai` as the corresponding
fallback outcomes. Evaluate each documented role, including both roles in
mixed work, regardless of its predicted involvement probability. Credit
successful rejection only when the ground-truth identity is absent from the
gallery and the corresponding fallback accepts no named match for that role.
A wrong named match is an attribution error; fallback on a listed identity is
not successful open-set rejection. Missing role results and execution failures
remain in the assigned population and earn no rejection credit. Report
joint-model tradeoffs in both directions against the corresponding single-task control.
Report `other_ai` use on held-out generators and ambiguous listed-model cases,
with fallback frequency on listed models. Report `other_human` use on held-out
writers and ambiguous listed-writer cases, with fallback frequency on listed
writers. Include mixed work with neither
contributor identified and supported named-model matches at low AI probability.
Do not score conditional identities for roles absent from the documented
history; a conditional fallback in an absent role is not itself a false
assignment. Evaluate false role probabilities through involvement calibration
and false-positive measurements, including human involvement on model-only
text and AI involvement on human-only text. Score named-profile retrieval
separately; neither fallback earns credit as a correct named identity.
Attribution-only controls can use the same role-conditional identity metrics
without producing involvement probabilities.
Role-level rejection covers a single contributor or an all-withheld role.
Known-plus-withheld contributors within one role require a frozen set-valued
metric before rejection credit; retain and report these cases separately.
Freeze the origin-equivalence tolerance and any secondary scoring rule on
Development before final comparison.

Report paired complete-document performance and full requested-population
coverage/failures. Include cold/warm parsing, resolution calls and tokens,
export and inference latency, memory, tail cost, and total cost per delivered
document. Current cache reuse avoids work in this experiment but does not make
first-time customer parsing free.

A launch decision needs predeclared quality and cost targets, a clean held-out
comparison, and evidence for all outputs of the shared model. Prepared data,
synthetic tests, exposed-corpus accuracy, or past raw-text results alone cannot
settle CFG-only eligibility.

## Execution and retention

Use an isolated checkout and a recorded task-owned root on build-host. Start
with one build/parser worker, a durable systemd user job, an explicit memory
cap, zero swap, and a conservative CPU quota. Preserve the host's live services
and unrelated dirty checkout. Do not use GitHub Actions.

Retain bounded exports, manifest/checksums, commands, logs, and exit status in
ignored data. Remove task-created build and scratch directories after checking
processes, services, configuration, symlinks, and other live dependencies.
Keep private source material and identifiers out of the repository.
