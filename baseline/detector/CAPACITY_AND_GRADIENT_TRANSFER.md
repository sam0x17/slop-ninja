# Capacity, explicit features and useful edit gradients

The objective is an A model that detects heavily revised model writing and
provides edit rankings that transfer to Pangram. Ordinary classification
accuracy, agreement on unedited passages and lower training loss are insufficient.
Keep origin evidence, the provider's measured response and editing quality
separate throughout. The [base origin-consistency comparison](ORIGIN_CONSISTENCY_RESULTS_V1.md)
is complete; the matched large-backbone pair is now fitting under its frozen
protocol on the Studio.

## Distinguish capacity from coverage and optimization

Record training and source-disjoint Development learning curves, including
generator, revision route, length and requested-style slices. Fit vocabularies,
feature scales and any sampling weights on Train only. Count source families
as well as records. A style prompt does not certify its realized style or
meaning preservation.

If a model fits Train but fails on unfamiliar sources or editing routes, examine
coverage and shortcuts. If it cannot fit Train, check optimization, conflicting
targets and undercapacity rather than treating the failure as proof that more
parameters are necessary. After a promising controlled comparison, predeclare
additional seeds and an equal Development tuning budget for both sizes before
claiming a reliable ordering. One seed cannot diagnose a local minimum.

Use the released ModernBERT sizes for the first comparison:

| Backbone | Published size | Layers | Hidden width | Intermediate width |
| --- | ---: | ---: | ---: | ---: |
| ModernBERT-base | 149M | 22 | 768 | 1,152 |
| ModernBERT-large | 395M | 28 | 1,024 | 2,624 |

These are upstream backbone labels, not measured trainable counts for our
three-class classifier. Record exact total and trainable counts when loaded.
The large release is about 2.65 times the published base size. Both belong to
the same encoder family and are released under Apache-2.0. Their published
8,192-token capability does not validate our detector at that length.
[Base model card](https://huggingface.co/answerdotai/ModernBERT-base),
[large model card](https://huggingface.co/answerdotai/ModernBERT-large).

The first size comparison adds large-backbone control and consistency fits to
the two base-backbone candidates. Keep the same existing Train pairs, four
epochs, seed 17, learning rate, effective batch, origin weights, precision,
selection criterion and Calibration protocol. Retain both loss settings at
both sizes, regardless of the first diagnostic outcome. Change neither data
nor the 2,048-token execution limit in this comparison. This compares pretrained
backbone choices under a common recipe; it cannot isolate parameter count from
their different pretrained weights or establish that either recipe is optimal.

The [runtime check](results/checkpoint-capacity-runtime-v1.json) now verifies
both pinned checkpoints on Studio macOS arm64 CPU. The loader and artifact
packager preserve the actual upstream identity and required file hashes. Both
sizes completed one synthetic optimizer step, save/reload, bundled inference
and a finite-difference check of an input-embedding gradient. The classifiers
contain 149,607,171 and 395,834,371 parameters, respectively. This does not train
the larger detector or establish useful edit rankings. A subsequent
[paired MPS check](results/large-paired-mps-2048-v1.json) completed one step at
batch size four and 2,048 tokens on both branches. The
[large training protocol](CAPACITY_LARGE_V1.md) fixes the remaining comparison.
Linux execution remains pending. The paired training has launched on Studio. Larger
architectures remain eligible if the learning curves justify them; 395M is not
a final size cap.

The [large-run preparation](results/capacity-large-preparation-v1.json) binds
the unchanged training script and inputs. Base and large tokenizer assets match
byte for byte; all 4,903 Train, Development and Calibration records fit the
configured limit. The [comparison preflight](results/capacity-large-comparison-preflight-v1.json)
verified the same seven public views, 96 hosted captures and 26 grammar states
as the base comparison. After completing the base comparison, the large pair
launched with these frozen plans. Public diagnostic inference waits for both
large fits to complete; the known private transfer comparison follows the
prescribed public review.

Expand data in a separate comparison at fixed model choices: more source
families, longer coherent texts, additional admitted generator families,
cross-model revision chains and hard human examples. Match source/topic/length
coverage across origins so that dataset identity cannot substitute for origin.
Retain difficult attempts and their provenance; fidelity failures remain model
examples but cannot establish a controlled effect of style alone. All
descendants stay in their source group. Previously inspected diagnostics do not
become fresh tests, and private B material does not enter training or datasets.

## Compare text, explicit features and their combination

The [explicit-feature preparation](EXPLICIT_FEATURES_V1.md) now provides a Rust
exporter for frozen sparse inputs and has completed the unchanged V6 fitting
archives on the Studio. The encoder trainer still consumes text. A separate
[neural feature-head comparison](FEATURE_NN_V1.md) now includes completed fits
and frozen external diagnostics. The
[first feature grid](FEATURE_NN_RESULTS.md) completed all eleven fits. Combined
features improved on similar-sized word-only controls on Development, while
the existing V6 encoder still had lower Development loss. All eleven feature
heads then missed more saved hosted outputs than V6 at both frozen operating
points. Local gradient arithmetic checks passed, but none of those edits has
a Pangram observation. No feature head qualifies as trusted guidance for B;
fusion and useful external edit-gradient transfer remain outstanding.

An encoder already consumes token embeddings. The question is whether an
explicit grammar/lexical vector improves detection and gives B useful edit
coordinates. The existing [linear probe](LINEAR_STYLE_RESULTS.md) supplies a
reason to test this, not evidence that features should replace the encoder.

Compare text-only, feature-only and combined models on the same admitted data.
Start with the versioned Rust `FeatureSpace`: word occurrences, word bigrams
and grammar families, with separate coordinate budgets and Train-fitted scales.
Preserve parser identity, missing-family indicators and unknown-feature counts.
Do not treat a failed parse as a vector of observed zeros. A small neural head
can learn interactions between explicit coordinates; fusion can combine that
representation with the encoder's document representation. Report the added
parameter count and compare against a similarly sized text-only head before
attributing an improvement specifically to grammar information.

For editing, add occurrence evidence: each coordinate's supporting token,
clause and sentence spans, tied to exact UTF-8 text and parser/version hashes.
Some coordinates describe a distribution across the document and have no unique
culprit span. Word and construction counts can lose ordering and other context;
the feature-only comparison tests that tradeoff instead of assuming sufficiency.
Reparse and recompute affected document features after actual edits, including
changed denominators. Corpus statistics and author-style targets have distinct
provenance and must not silently change between candidate comparisons.

## Train the target needed by B

Keep a provenance head for `human_only`, `model_only` and `mixed`. Its combined
model-origin probability is not Pangram's estimated fraction of AI text. If a
provider-surrogate head is added, give it a separate name, output schema and
loss against the exact recorded provider field. Save provider/version, text
hash, raw response, workflow and generating/revising model history. Provider
mistakes or variance do not rewrite the independent origin label.

Use provider annotations only from explicitly allocated Train families for
that loss; mask missing annotations rather than inventing zero targets. Allocate
source-disjoint Development, Calibration and confirmation families before
fitting it. Private comparisons remain evaluation-only. Hosted outputs enter
training only as their own weighted cohort under the
[hosted admission manifest](../../docs/a-hosted-output-admission.json).
Predeclare how multiple losses are weighted and selected on Development. The
current experiment contains no provider loss and is not a distillation fit.
Further paid annotations need their own declared plan;
this plan launches no provider requests.

The [exact coverage audit](PANGRAM_TRAIN_COVERAGE_RESULTS_V1.md) now identifies
485 existing local-generator Train observations. Their revision scores are
mostly saturated, and only 41 observations lie in the 60-100% interior. These
counts define the current supervision limits; they do not qualify a provider
head or change the active provenance-only fits.

## Turn gradients into candidate edits

Differentiate a declared scalar score with respect to input embeddings or
scaled explicit features, with the model frozen in evaluation mode. For the
origin head, start with the model-versus-human logit margin,
`logsumexp(z_model, z_mixed) - z_human`, to avoid the extra saturation from
differentiating the output probability. Keep the reported calibrated
probability alongside it. A provider head needs its own declared scalar target.

For an equal-token-length, aligned substitution, a first-order estimate is

`estimated_delta = sum_i gradient_i · (new_embedding_i - old_embedding_i)`.

For an explicit vector `f`, use

`estimated_delta = gradient_f · (f(edited_text) - f(source_text))`.

These are local estimates of this model's response, not exact causal
attributions. The discrete-gradient approach has precedent in
[HotFlip](https://aclanthology.org/P18-2006/). It is not necessary to differentiate
through a grammar parser: the parser and Rust rules can materialize actual
feature changes, while the classifier supplies gradients in feature space.

Use grammar and lexical menus to generate edits, and use the gradients to
prioritize evaluating them. If an edit changes token count, tokenization of
surrounding words or alignment, do not pretend the aligned substitution formula
still applies. Re-tokenize and score the actual candidate. Keep integrated
gradients as an optional diagnostic with a recorded baseline/path; it is not a
guarantee that removing the highest-attribution word improves the objective.
[Integrated Gradients paper](https://proceedings.mlr.press/v70/sundararajan17a.html).

Reject meaning, qualification, entity, number or intended-tone regressions
before acceptance. The LLM can select or veto menu edits using author context;
a lower detector score is not a fidelity certificate. Preserve the original,
patches and review decisions. Use a bounded beam with single and multi-edit
branches so that a plateau does not force acceptance of a bad local edit.
After each accepted step, recompute gradients and score the assembled document.
Windows remain diagnostics unless a whole-document aggregation rule has been
trained and calibrated. Book-length evaluation cannot be replaced by averaging
successful sentence scores.

## Verify edit-direction transfer before relying on it

First compare gradient estimates with actual A score changes over a fixed menu
and over edit combinations. Include equal-budget random selection, existing
menu ordering and exact A rescoring controls. Record gradient failures,
tokenization changes, ranking errors, interaction effects and runtime cost.
Finite-difference checks and provenance bindings verify the implementation;
they do not establish useful editing.

For the external comparison, freeze source-disjoint documents, edit menus,
selection budgets and fidelity decisions before opening new provider scores.
Evaluate the selected edits and matched controls, preserving all attempts.
Measure change-direction agreement, within-source ranking agreement, actual
Pangram improvement at a fixed search budget and A-only successes. Report the
unchanged baseline, effect sizes and source-group uncertainty; flat or tied
provider results remain ties. Existing exact-text annotations can support only
the comparisons they actually cover. Their reuse cannot validate an unscored
edit, and the three known private document states are not a general ranking test.

Promote A only on evaluated detection, human-error and calibration results;
qualify gradient guidance separately on external edit-direction transfer and
fidelity. A model that fits Train but supplies exploitable gradients has not
met the A-to-B objective. Keep a fresh confirmation set outside all fitting,
architecture selection, threshold tuning and adaptive edit search.
