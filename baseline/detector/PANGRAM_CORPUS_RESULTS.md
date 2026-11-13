# Pangram corpus results

The [v5 follow-up](PANGRAM_V5_RESULTS.md) compares the current released detector
on these same cached annotations. The original v2 comparison follows below.

Pangram flagged all 38 model-only or model-edited texts in the 57-row v2
Development cohort, with no flags on its 19 historical human proxies. The encoder
flagged 20/38 and the word/grammar candidate 12/38 at their frozen calibration
thresholds, also with no human flags. Our detectors still miss much of the
model involvement that Pangram recognizes in this corpus.

This is a small, previously used Development set with weak human/mixed labels.
The [protocol](PANGRAM_CORPUS_PILOT.md) fixed the sample and diagnostic rule before
submission. This comparison does not establish general accuracy, matched-FPR
superiority or an anchor switch. No model, calibration or origin label changed.

| Detector | Binary operating rule | Human flagged / 19 | Model-only flagged / 19 | Mixed flagged / 19 |
| --- | --- | ---: | ---: | ---: |
| Pangram 4.0 | AI plus AI-assisted content fraction >= 0.10 | 0 | 19 | 19 |
| Selected v2 encoder | Model-or-mixed probability > 0.8935751794569573 | 0 | 12 | 8 |
| Selected combined linear model | Model-or-mixed probability > 0.8367218649416995 | 0 | 7 | 5 |

Each local model's nominal 1% and 5% operating targets collapsed to the same
threshold on only 14 human Calibration families. Zero flags on 19 Development
families cannot establish either population bound. The different rules above
are descriptive operating points, not a controlled comparison at matched FPR.

The first pass's native Pangram labels were 19 Human, 6 AI and 32 Mixed.
Pangram's mixed-content classification does not equal our mixed-production
history label. All ten Qwen families and nine Mistral families had both model
outputs flagged. Every prompt profile was flagged, including all eight anti-AI
and all eight Fix Slop outputs. Preserve these as detectable examples with known
generation histories; instructions to avoid model phrasing did not evade this
provider in the cohort.

After their already fitted temperatures, Development log loss was 0.832055 for
the encoder and 0.896674 for the combined model. Their three-class argmax results
remained 33/57 and 35/57. These calibrated losses differ from the uncalibrated
losses used for checkpoint selection. Pangram content fractions were never used
as origin probabilities to compute a log loss or Brier score.

## Three-pass repeatability

Two bulk jobs produced 171 successful observations: one 57-item first pass and
one 114-item job for two deliberate repeats. Each source had three distinct task
IDs, and every result returned version `4.0`. No requests failed or were retried.

All three document fractions and their AI-plus-assisted sum were exactly stable
for all 57 inputs. Native labels never changed, and no observation crossed the
10% boundary. Complete result objects matched for 27 texts and differed for 30;
the only varying fields were window `ai_assistance_score` and `humanizer_score`.
The [repeatability record](results/pangram-corpus-repeatability-v1.json) preserves
the aggregate results and annotation-file hashes.

These are same-account, same-day observations. They extend the earlier two-text
probe but do not establish cross-account verification, future-version stability
or a cryptographic proof of execution. Subnet verification should distinguish
stable document fractions from auxiliary scores and preserve the provider
version and exact submitted input.

Pangram returned exact text for 20 inputs and changed only whitespace for 37,
consistently across repeats. The first collector stopped on the mismatch. A
recorded transport amendment permits an explicit whitespace-only comparison,
while retaining both hashes and rejecting all other text changes. Both echo
slices independently had every model/mixed text flagged and no human flags.
Echoed formatting does not establish the provider's internal preprocessing.

## Cost and retained artifacts

The posted-price estimate of the charge is recorded in the private ledger.
The [API pricing](https://www.pangram.com/pricing)
and [bulk documentation](https://docs.pangram.com/api-reference/bulk-api) were
checked on 2026-09-11. A completed job's results have a 48-hour retention window;
all responses here have already been archived locally.

Raw text, receipts, provider responses, complete annotations and per-document
comparisons remain in ignored `data/baseline-detector/pangram-budget-20260911/`
and `data/baseline-detector/pangram-corpus-v1/`. The
[aggregate comparison](results/pangram-corpus-development-v1.json) contains no
raw texts or per-document provider annotations. Under the project's publication
policy, Pangram scores are kept privately and not published, and they never
become origin labels; Pangram links and ids may be published where they serve
verification. Do not train on this Development annotation set or relabel its
sources with provider predictions.

The next model work should broaden the admitted source registers, add generator
families and improve documented mixed-workflow coverage. Further optimization on
these 19 Development families cannot provide fresh evidence of progress.
