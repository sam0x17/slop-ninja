# V6 detector evaluation

The frozen evaluation, all-human diagnostic and fresh Pangram comparison are
complete. V6 improves the declared binary log loss and
reduces human false positives at its stricter cutoff. Detection sensitivity
and mixed-origin classification have tradeoffs detailed below. Pangram still
leads the fresh detection check.

## Experiment and interpretation

V6 compares a new ModernBERT recipe with the released v0.5.0 detector. Candidate
selection used human/model Development log loss; Calibration then fitted the
selected checkpoint's temperature and decision thresholds. The selected model
came from epoch 2 at learning rate 0.00001. Test observations did not select
the checkpoint, change its thresholds or trigger another fit.

The recipe includes additional model revisions, fresh Development and
Calibration families, and a 2,048-token input limit. V5 retains its original
temperature, thresholds and 1,024-token limit. The comparison cannot isolate
the effect of revision training from these other changes. Both models evaluate
the same supported text without truncation. The frozen protocol and coverage
reports describe the complete-family selection and separate human-root check.
Every record in both complete-family evaluation archives fits V5's token limit;
the full and common-support views are identical. These results therefore do
not demonstrate an advantage on longer inputs. All 117 assigned human roots
also fit both models.

`human_only`, `model_only` and `mixed` are document-origin labels under the
corpus evidence policy. Historical human proxies do not certify an observed
human-only workflow. For binary metrics, the score is
`P(model_only) + P(mixed)`. We report binary metrics on human and model-only
rows, excluding mixed-origin rows. These probabilities are not percentages
of AI-written words.

## Primary comparison

Each stage contains 336 rows from the same 112 source families: 112 historical
human proxies, 112 model texts and 112 model edits of human proxies. R0 uses
the original model draft, R1 its first revision and R2 its second revision.
Human and mixed-origin rows are reused across stages. These are dependent views
of one cohort, not three independent confirmations.

Qwen, Mistral or OLMo composed each primary draft from its assigned historical
source. Qwen then performed the Anti-AI revision and Mistral the Fix Slop
revision. The direct edits of human text came from the assigned initial
composer. The six initial writing profiles and generation records preserve
these distinct workflows; source-conditioned composition is not an evaluation
of unrestricted writing from scratch.

The declared primary statistic is the R2 difference in binary log loss, V6
minus V5. Lower loss is better. Its paired 95% interval is entirely below zero.
The original-draft interval includes zero.

| Stage | V5 binary log loss | V6 binary log loss | Difference | Paired 95% interval |
| --- | ---: | ---: | ---: | --- |
| Original draft (R0) | 0.2727 | 0.2035 | -0.0692 | [-0.1625, 0.0253] |
| First revision (R1) | 0.2632 | 0.1033 | -0.1599 | [-0.2215, -0.0981] |
| Second revision (R2) | 0.2653 | 0.1007 | -0.1647 | [-0.2229, -0.1075] |

These intervals use the frozen 512 paired source-family bootstrap draws on
224 binary rows. They assume independent source families and do not account
for arbitrary new genres, generators or pretraining contamination. R0 and R1
are diagnostic views; their intervals are not adjusted for multiple comparisons.

At the diagnostic score boundary of 0.5, binary accuracy rises from 88.4% to
92.0% on original drafts and from 88.4% to 95.1% after either revision. R2
binary Brier score falls from 0.08751 to 0.03264; its paired difference interval
is [-0.07586, -0.03305].

### Frozen operating points

At each model's stricter cutoff, calibrated toward a 1% human false-positive
rate, V5 flags 7/112 human texts and V6 flags 3/112. The observed Test rates
are 6.25% and 2.68%, respectively. The Calibration target is not an observed
Test rate or a certified population bound.

| Stage | V5 model texts detected | V6 model texts detected |
| --- | ---: | ---: |
| R0 | 104/112 | 101/112 |
| R1 | 111/112 | 111/112 |
| R2 | 110/112 | 109/112 |

Both models flag 103/112 mixed-origin texts at these stricter cutoffs. At the
looser cutoffs, calibrated toward 5%, human false positives increase from
9/112 for V5 to 11/112 for V6. Model detection is 106/112 for both on R0,
111/112 for both on R1, and 110/112 versus 111/112 on R2. Mixed-origin
detection increases from 103/112 to 106/112. Each model uses its own frozen
cutoffs; these are not comparisons at matched population false-positive rates.

### Three-class results and concentration of errors

| Stage | V5 accuracy | V6 accuracy | V5 log loss | V6 log loss |
| --- | ---: | ---: | ---: | ---: |
| R0 | 68.45% | 69.64% | 0.6226 | 0.6436 |
| R1 | 71.13% | 80.06% | 0.5947 | 0.4587 |
| R2 | 72.32% | 80.65% | 0.5958 | 0.4622 |

The original-draft three-class loss worsens slightly. Its paired difference
interval, [-0.07025, 0.11711], includes zero. Correct mixed-origin
classification falls from 78/112 to 66/112 across all stages, even though
most of these texts still exceed the combined detection cutoff. The binary
selection objective and the three-class task must be assessed separately.

The primary slices show where the aggregate changes occur:

- All four fewer human false positives at the stricter cutoff come from
  Wikinews: 5/33 becomes 1/33. PLOS remains at 2/32 and CMU summaries at 0/47.
- Of the twelve fewer correctly classified mixed-origin texts, eleven are
  CMU summaries: 35/47 becomes 24/47. PLOS remains at 19/32 and Wikinews
  changes from 24/33 to 23/33.
- On original drafts, PLOS three-class log loss rises from 0.7303 to 0.8558,
  and model detection at the stricter cutoff falls from 27/32 to 25/32.
  In the Mistral-composer slice, three-class loss rises from 0.6751 to 0.8148.
- On R2, three-class loss decreases in each collection and composer slice.
  The anti-AI initial-profile slice changes little, from 0.5606 to 0.5577
  across 17 families. The Fix Slop slice improves in three-class accuracy
  but loses one model detection at the stricter cutoff, from 15/15 to 14/15.

These are descriptive, overlapping slices with small denominators. They do
not identify causal effects of a prompt, generator or genre. In particular,
the original composer does not identify every reviser in a generation chain.

## Phi-4 transfer

Phi-4 supplies its own initial draft, direct human edit and both revision
passes. It is absent from fitting, Development and Calibration. Its earlier
V5 results were inspected, so this is transfer to a previously observed
generator excluded from fitting, evaluated on fresh source families.

The complete-chain cohort contains 104 of 117 assigned families, with 312
rows per stage. The primary cohort contains 112 complete families from the
same assignments. Their completion cohorts and prompt assignments differ;
directly comparing their headline rates would not isolate a generator effect.
Both routes reuse human sources and require the separate all-assigned-human
diagnostic to expose selection effects from generation completion.

After two Phi-4 revisions (R2), the paired results are:

| Metric | V5 | V6 | Paired difference and 95% interval |
| --- | ---: | ---: | --- |
| Binary log loss | 0.2984 | 0.1434 | -0.1549 [-0.2150, -0.0899] |
| Binary Brier score | 0.09861 | 0.04440 | -0.05422 [-0.07529, -0.03105] |
| Binary accuracy at 0.5 | 86.54% | 93.75% | +7.21 points [3.37, 11.06] |
| Three-class accuracy | 63.14% | 73.08% | +9.94 points [4.49, 15.38] |
| Three-class log loss | 0.6904 | 0.6829 | -0.0075 [-0.1051, 0.0927] |

The binary loss interval supports improvement on these 208 human/model rows
from 104 families. The three-class loss interval includes zero. At the
stricter cutoffs, human false positives fall from 7/104 to 3/104, while model
detection falls from 98/104 to 95/104 and mixed-origin detection from 78/104
to 73/104. At the looser cutoffs, human false positives rise from 9/104 to
11/104, model detection rises from 99/104 to 102/104 and mixed detection from
81/104 to 88/104. These retain each model's own Calibration settings.

Correct mixed-origin classification changes from 65/104 to 64/104. Within
that class, assignments to `human_only` increase from 7/104 to 18/104. The
binary improvement therefore does not establish improved handling of
partially assisted writing.

On original Phi-4 drafts (R0), binary log loss improves from 0.2939 to
0.1805, with a paired difference interval of [-0.1913, -0.0208]. Binary
accuracy at 0.5 rises from 87.02% to 93.27%. Three-class loss worsens from
0.6690 to 0.7430; its difference interval, [-0.0372, 0.1906], includes zero.
Three-class accuracy changes from 67.63% to 69.55%.

R0 model detection at the stricter cutoff falls from 97/104 to 93/104; at
the looser cutoff it rises from 99/104 to 101/104. Human and mixed-origin
counts match R2 because those rows are reused. All four lost model detections
at the stricter cutoff occur in PLOS, from 29/31 to 25/31. R0 three-class loss
in that slice rises from 0.7481 to 0.9408; the Fix Slop profile slice rises
from 0.7498 to 1.1897.

After the first Phi-4 revision (R1), binary loss improves from 0.3078 to
0.1663, with a paired difference interval of [-0.2086, -0.0646]. Binary
accuracy at 0.5 rises from 87.02% to 94.23%. Three-class loss worsens from
0.6880 to 0.7116; its difference interval, [-0.0739, 0.1268], includes zero.
Three-class accuracy rises from 65.38% to 70.19%. Model detection falls from
98/104 to 93/104 at the stricter cutoff and rises from 99/104 to 102/104
at the looser cutoff. Human and mixed-origin counts again match R2.

Thus binary loss improves at every Phi-4 stage, while none of its three-class
loss difference intervals excludes zero. PLOS and Fix Slop three-class loss
worsen at every Phi-4 stage. For R1, PLOS loss rises from 0.7571 to 0.8785
and Fix Slop loss from 0.8046 to 1.0967. These observations do not establish
that V6 is uniformly preferable across origin tasks or writing profiles.

R2 three-class loss rises from 0.7614 to 0.8567 in the PLOS slice (31
families) and from 0.7497 to 1.0220 in the Fix Slop initial-profile slice
(17 families), despite higher accuracy in both. Detection at the stricter
cutoff falls from 29/31 to 27/31 for PLOS and from 14/17 to 13/17 for that
profile. These small, overlapping slices show that the binary improvement
does not establish better probability estimates for every writing profile.

## All 117 human sources

Both models evaluated the same 117 assigned human roots, including those whose
generation chains did not complete. All fit both token limits; none was
truncated or excluded. These sources overlap the stage comparisons above and
are not another independent human sample.

| Calibration target | V5 human false positives | V6 human false positives |
| --- | ---: | ---: |
| 1% | 7/117 (5.98%) | 3/117 (2.56%) |
| 5% | 9/117 (7.69%) | 12/117 (10.26%) |

The stricter-cutoff improvement again comes entirely from Wikinews, from
5/34 to 1/34; PLOS stays at 2/33 and CMU at 0/50. At the looser cutoff,
PLOS false positives rise from 3/33 to 6/33, while Wikinews stays at 5/34
and CMU at 1/50. The complete-family stage views missed one of V6's looser
cutoff false positives, which is retained here.

Human-only log loss falls from 0.5039 to 0.1673, with a paired difference
interval of [-0.4451, -0.2287]. This diagnostic has no model-only or
mixed-origin observations and cannot estimate detection sensitivity. Its
small historical sample, including zero observed CMU errors at the stricter
cutoff, cannot certify a population false-positive bound.

## Fresh Pangram comparison

All 162 assigned provider requests succeeded and returned Pangram version 4.0.
The cohort contains 18 human texts, 108 distinct model drafts/revisions and
36 direct model edits of human texts, from 18 families. Sampling preceded
scores and balanced source collection and original primary composer. Both
routes cover all six initial writing profiles. These are correlated texts
from 18 families, not 162 independent sources.

Pangram flags use the declared rule of at least 10% combined AI and AI-assisted
content. Encoder flags use their own frozen Calibration thresholds. The
quantities have different meanings; the table is an observed comparison, not
evidence at matched population false-positive rates.

| Detector setting | Human flags | Model-only detections | Mixed-origin detections |
| --- | ---: | ---: | ---: |
| Pangram, combined content >=10% | 0/18 | 108/108 | 34/36 |
| V5, stricter cutoff | 2/18 | 100/108 | 32/36 |
| V6, stricter cutoff | 0/18 | 98/108 | 29/36 |
| V5, looser cutoff | 2/18 | 101/108 | 32/36 |
| V6, looser cutoff | 2/18 | 102/108 | 33/36 |

V6 trades two fewer human flags for fewer detections at the stricter cutoff.
At the looser cutoff it detects one more model-only text and one more mixed
text than V5, with the same two human flags. Pangram detects more of both
model-involved classes than either encoder setting. V6 does detect one
mixed-origin text that Pangram misses. None of these counts changes its
recorded production-history label.

The six stage views each contain 54 observations, including the same 18 human
roots and the route's reused human edits. For the primary route, V6 detects
17/18 original drafts, 17/18 first revisions and 16/18 second revisions at its
stricter cutoff; it detects 17/18 at every stage at the looser cutoff. On the
Phi-4 route, the counts are 16/18 at every stage at the stricter cutoff and
17/18 at the looser cutoff. Pangram detects all 18 model-only texts in each
view. Mixed-origin detections for V6 are 17/18 on the primary route and
12/18 on the Phi-4 route at the stricter cutoff; the latter becomes 16/18 at
the looser cutoff. Pangram's corresponding mixed counts are 18/18 and 16/18.

The subset's 512 paired-family comparisons retain the following binary
loss differences, V6 minus V5:

| Route and stage | Difference | Paired 95% interval |
| --- | ---: | --- |
| Primary R0 | -0.1116 | [-0.3358, 0.1392] |
| Primary R1 | -0.1318 | [-0.3513, 0.1039] |
| Primary R2 | -0.1778 | [-0.3530, -0.0219] |
| Phi-4 R0 | -0.0800 | [-0.3220, 0.2226] |
| Phi-4 R1 | -0.1140 | [-0.3408, 0.1345] |
| Phi-4 R2 | -0.1299 | [-0.3347, 0.0762] |

Only the primary R2 binary interval excludes zero in this small subset. All
six three-class loss intervals include zero. These observations are nested
within the full evaluation, not independent confirmation of its results.

Score diversity remains limited: 106/108 model-only texts fall in the 100%
combined-content bin. Only eight nonhuman texts fall between 60% and 100%
exclusive: two model-only texts and six mixed texts. Mean Pangram fractions
for entirely model-origin texts are 22.76% AI and 76.84% AI-assisted. The
provider's content categories do not replace the generation provenance.

The join verified the exact payload, membership, text echoes and all successful
responses. Shared texts have identical saved encoder probabilities across
stage exports, with no threshold disagreements. No inference, fitting or
provider calls occurred during report assembly. Raw provider reports remain
outside the public bundle. The spend of this batch and of the collection jobs
is recorded in the private ledger.

## Release assessment

V6 is the next experimental binary detector. Its main R2 loss difference and
the broader human diagnostic favor the new recipe; Phi-4 binary loss also
improves at all stages. Keep V5 available for the reported sensitivity and
three-class comparisons. The larger input limit has not been evaluated on
texts exceeding V5's limit.

The model remains an unqualified research candidate. These results do not
establish Pangram parity, rare population error rates, semantic fidelity,
readability or preservation of intended tone. Pangram remains the external
quality anchor. Better handling of mixed-origin writing, scientific abstracts
and some Fix Slop profiles still requires evidence from a new experiment.
The published source families cannot be reused as hidden or fresh Test data.

Release assembly checked all 22 declared commands, fourteen evaluations and
seven full-cohort paired comparisons, then verified six paired Pangram-subset
comparisons. Overall metrics were recomputed from saved probabilities. The
bundle preserves the selected encoder bytes and the separate successful CPU
reference-runtime receipt, with source attribution and the observed failures.
