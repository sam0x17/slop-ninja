# First reference detector results

Measured 2026-09-11. ModernBERT epoch three is the first reference candidate,
selected by development log loss before the test was opened. It reached 39/57
correct predictions on Qwen and 32/54 on the Mistral transfer cohort. Generator
transfer and whitespace sensitivity remain substantial weaknesses.

The release estimates `human_only`, `model_only` and `mixed` document origins.
The sum of the latter two probabilities describes a model-or-assistance event,
not the fraction of words written by a model. This pilot provides a reproducible
starting detector. It does not establish Pangram parity or subnet qualification.

## Data and selection

We acquired 80 PLOS abstracts under CC BY 4.0 and 80 historical Wikinews articles
under CC BY 2.5. The public source register retains attribution and license
evidence. Historical publication supplies weak human-origin evidence; the
copyedits supply weak mixed-origin labels. These are not documented contemporary
human/model collaborations. Private correspondence, book material and Blog
Authorship data are absent from this training lineage.

Qwen3-30B-A3B-Instruct-2507 generated one draft and one copyedit for each frozen
source. Two drafts failed the prespecified copying check. We retained all 320
attempts locally and excluded both complete source families from the fitted
corpus. Both exclusions affected training. The retained 474 texts comprise 158
examples of each class, with equal representation from the two source registers.

| Partition | Source families | Texts | Texts per class |
| --- | ---: | ---: | ---: |
| Training | 111 | 333 | 111 |
| Development | 12 | 36 | 12 |
| Calibration | 16 | 48 | 16 |
| Final Qwen test | 19 | 57 | 19 |
| Mistral transfer test | 18 | 54 | 18 |

The Mistral-Small-3.2-24B-Instruct-2506 cohort uses the original test source
families. One of its 38 generations failed the copying check, leaving 18 complete
families. It tests a change of generator and tokenizer configuration on mostly
the same source material; it is not an independent author/topic test. Its human
proxies overlap the Qwen test. The two generator comparisons also differ by the
one excluded family.

All model choices follow the frozen pilot protocol. Each candidate selects its
checkpoint by uncalibrated development log loss and fits a temperature on
calibration data. The cross-candidate choice uses the same development criterion.
The final evaluator records that choice and artifact hashes before opening any
test predictions. No test result changes model selection or training settings.

## Candidate comparison

| Candidate | Selected epoch | Development log loss | Qwen accuracy | Qwen log loss | Mistral accuracy | Mistral log loss |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Word | 1 | 0.6502 | 61.4% (35/57) | 0.7396 | 57.4% (31/54) | 0.8647 |
| Grammar | 0 | 1.0986 | 33.3% (19/57) | 1.0986 | 33.3% (18/54) | 1.0986 |
| Combined | 0 | 1.0986 | 33.3% (19/57) | 1.0986 | 33.3% (18/54) | 1.0986 |
| ModernBERT | 3 | 0.3665 | 68.4% (39/57) | 0.6663 | 59.3% (32/54) | 0.8751 |

Test log losses use each model's fitted calibration temperature. ModernBERT's
multiclass Brier score was 0.4306 on Qwen and 0.5413 on Mistral, on the [0,2]
scale. Uniform and training-prior controls coincide at log loss 1.0986, Brier
0.6667 and accuracy 33.3%. The word model slightly outperformed the encoder on
Mistral log loss; this result does not change the development-selected reference.

The grammar and combined controls both selected epoch zero. Neither improved
development log loss over the initial uniform predictor in 25 attempted epochs.
Their fitted weights are zero. This is a failed training result under the fixed
recipe; it does not establish that grammatical features contain no useful signal.
Per-epoch linear losses were not retained in the original reports. A later
[training/development replay](LINEAR_FIT_DIAGNOSTIC.md) reproduced the failed
selection and found overconfident development predictions from the first update.
A smaller-step diagnostic improved development loss without producing new test
results or changing the released controls.

Each linear control has an 8,192-coordinate cap. The word control retains 5,283
word and 2,909 word-bigram coordinates. The combined control retains 3,007 lexical
and 5,185 grammatical coordinates. Its comparison with the word control therefore
changes lexical coverage as well as adding grammar.

## Formatting and false positives

| Encoder test view | Accuracy | Log loss | Brier | Model-or-mixed detections | Human false positives |
| --- | ---: | ---: | ---: | ---: | ---: |
| Qwen original | 39/57 | 0.6663 | 0.4306 | 36/38 | 0/19 |
| Qwen collapsed whitespace | 31/57 | 0.8941 | 0.5936 | 36/38 | 1/19 |
| Mistral original | 32/54 | 0.8751 | 0.5413 | 14/36 | 0/18 |
| Mistral collapsed whitespace | 32/54 | 0.9068 | 0.5699 | 24/36 | 1/18 |

The Qwen formatting change reduced mixed-class recall from 12/19 to 4/19.
Unchanged Mistral accuracy hides changes in individual predictions and detection
rates. Word-control scores were effectively unchanged. Its word bigrams depend
on parsed sentence boundaries, so preserving words alone need not make its
probabilities exactly invariant.

We report original text and the prespecified whitespace-collapse control
separately. That control preserves words, punctuation and their order, while
collapsing whitespace. The paired versions do not add independent source
families. Calibration predictions and thresholds remain identical across the
four test views.

The encoder's fixed threshold is `P(model_only) + P(mixed) > 0.9375192210349048`.
Both nominal 1% and 5% calibration targets produce this same threshold, with
zero observed calibration false positives. Original Qwen sensitivity was 94.7%
(36/38; 95% source-family bootstrap interval 84.2%-100%); Mistral sensitivity was
38.9% (14/36; interval 22.2%-61.1%). Neither original test had observed human
false positives. These threshold decisions differ from the three-class argmax
used for the accuracy column.

A post hoc descriptive comparison restricted to the 18 common source families
still gives 34/36 Qwen detections versus 14/36 Mistral detections, with identical
human probabilities and 0/18 human false positives. The omitted family does not
explain the transfer gap. This uses existing predictions only; the complete
prespecified cohorts above remain the primary reports.

Sixteen calibration human proxies and 19 primary test human proxies cannot
establish reliable 1% or 5% population false-positive bounds. Configured operating
targets are not achieved guarantees. The aggregate reports retain event counts,
denominators, register/evidence slices and source-family bootstrap intervals.
Interval estimates are unavailable for degenerate observed event rates.

## Reproduction and scope

Download the experimental model bundle (omitted from the public copy).
It includes all four immutable artifacts, all 16 detailed aggregate reports,
selection records, runtime locks, licenses, source attribution and SHA256 checksums.
The encoder weights use Apache-2.0; original linear weights and detector software
use MIT. Underlying source texts retain their CC BY terms.

The repository keeps the [compact results](results/pilot-v1.json),
[pre-test selection](results/pilot-v1-selection.json),
[encoder manifest](results/pilot-v1-encoder-manifest.json) and
[generation provenance](manifests/pilot-generation-v1.json).
Public report projections omit per-record predictions and local filesystem paths;
all aggregate observations and original report hashes remain available. Corpus
text and invocation logs stay local, so the release supports reproducible
inference rather than independent reconstruction of every training response.

Classifier source is pinned to `631184809c241f79a3db9575db598f48169d492f`.
The native generator adapter and runtime lock are pinned separately to
`b3a5e6332300c60b52d75b90e72ab8825e9d662c`. Both adopted generator cohorts ran on
the M5 Max with MLX-LM 0.31.3 / MLX 0.32.2. Qwen generation took 22m16s for 320
attempts, averaging 85.16 generated tokens per second over the entire run.
Earlier operational generation preflights were preserved and excluded in full.

All four candidates were fitted on the M3 Ultra Studio. The encoder uses the
pinned ModernBERT-base checkpoint with a three-class head, three epochs, batch
size four and seed 17. It passed a complete length audit: the longest input was
719 tokens including special tokens, below the fixed 2,048-token cap. No row was
dropped or truncated for length. Runtime admission limits exceed the input
lengths measured in this pilot and do not establish performance at those limits.

Encoder training and calibration took 810 seconds. The final evaluation verified all
50 frozen input/artifact hashes and all 128 recorded output hashes. All 16
reports reuse each model's original calibration probabilities, training prior
and operating thresholds. One transfer-metadata contract mismatch was corrected
before any model fitting; the failed launch and original marker were archived.
There were no model retries or test-driven parameter changes.

The transferred word and encoder artifacts both passed local inference checks
on the M5 after training on the Studio. The encoder's bundled synthetic reference
checks passed, and a separate synthetic input returned finite probabilities
summing to one through the CPU FP32 reference runner. These are runtime checks,
not additional accuracy observations. Other operating systems and CPU
architectures have not been numerically qualified.

The acquisition is a small convenience sample of two registers, with one
training generator and one transfer generator. Shared authors, events and
near-duplicates beyond the source families were not independently clustered;
public source material may also occur in encoder pretraining. The generation
admission checks do not establish semantic fidelity. Performance on genuine
collaboration, personal writing, other languages, other generators, author
identity and meaning-preserving transformations remains unmeasured. No Pangram
comparison was run.

The next corpus should vary requested voice, explicit anti-AI style instructions
and versioned fix-slop guidance. Use the same assigned profile for draft/edit
siblings to avoid making the profile itself an origin-label shortcut. Include
human and model formatting controls and reserve entire prompt families for
transfer evaluation. Broader registers and multiple training generators are
also needed. None of these additions has been evaluated in this first pilot.

Investigate the grammar/combined training failure with recorded per-epoch losses
and a comparison that holds lexical coverage fixed. Use a new frozen protocol
and fresh confirmation sources for the next candidate; these opened tests are
now diagnostic material.
