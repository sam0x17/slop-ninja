# Pangram comparison and annotation expansion

Declared 2026-09-12, before the v5 follow-up evaluation or any new paid
submission. The v6 generation and fitting protocol remains unchanged.

## Compare the released detector now

Evaluate the released v5 artifact on the same 57 Development records already
annotated by Pangram 4.0. Use its original temperature and both original
Calibration thresholds. Do not fit, recalibrate or select a new checkpoint.
Use the frozen `encoder_evaluation` CLI and bundled CPU inference on the Studio.

- V5 artifact: `da3a604d7bd1708f5ba1c4a4a4be946d77670492b702e41e267fb86237dfb8c9`.
- Records SHA256: `a5967f8f564d0f25fabb82ba3f9c90777ee094c781bfb9a32f712766b2e94bbf`.
- First-pass Pangram annotations SHA256:
  `888b729e466c78546070f35455193c41286d8baed118650fc43b26791ed7e8d4`.
- V5 threshold-file SHA256:
  `95ce23144752e8a589bd0471f6bda155ddc1171b2d2993d7473264096006c301`.

All 19 source families remain in v5 Development. A check against its complete
assembly found no training-family or exact training-text overlap. Development
has already informed model selection, so this comparison is descriptive and
cannot establish performance on a fresh sample.

Retain Pangram's existing diagnostic rule, AI plus AI-assisted content fraction
at least 0.10. Report human false positives and separate model-only and mixed
detection counts at both v5 thresholds. Preserve three-class confusion and
profile/provider slices. Different frozen operating points do not constitute a
comparison at matched population false-positive rates. Pangram content fractions
are not calibrated origin probabilities. Use all 57 first-pass observations;
the two repeat passes add repeatability evidence, not independent examples.

## Prepare a reusable training annotation collection

The first prepared collection contains all 2,925 unique texts in the existing
v5 Train shard, with its original source-family assignments, origin evidence,
rights records, generator identities and prompts. The frozen Rust planner
validates permission for external evaluation, retains exact input hashes and
deduplicates identical texts before creating bulk requests. No paid submission
has been made for this expansion.

| Prepared collection | Amount |
| --- | ---: |
| Unique texts | 2,925 |
| Whitespace-delimited words | 581,348 |
| Estimated Pangram 4 billable units | 7,301 |
| Bulk requests | 8 |
| Posted-price estimate | $292.04 |

Input SHA256:
`61c46872d730563bd8531e1a7e2a705afa9d7595f5b74dcee1e2dabbd12331df`.
Planner executable SHA256:
`b0f74d23534ae5c908e9600536f4bd59047d7686e4a1290b2d4863591202057f`.

Pangram 4 costs $0.05 per started 100-word block, with a 20% bulk discount.
The bulk limit is 1,000 units per job. Provider word counting can differ from
our estimate. These rates and limits were rechecked on 2026-09-12 against the
[pricing page](https://www.pangram.com/pricing) and
[bulk API documentation](https://docs.pangram.com/api-reference/bulk-api).

The [revised collection plan](PANGRAM_DATASET_V2.md) replaces submission of this
full prepared corpus with small, adaptive cohorts that aim for varied scores,
preferably spanning 60-100%. The spend of the two completed jobs is recorded in
the private ledger, and later collections use the same ledger.

## Training and benchmark use

Keep independently recorded origin evidence and Pangram annotations as separate
fields. A provider prediction must not overwrite known model generation or turn
a historical human proxy into verified human authorship. Train-only annotations
can support a separately declared auxiliary objective or selection of difficult
training examples. The existing 57-text Development set remains ineligible for
training. Any teacher-supervised fit needs its own protocol and ablation against
the same corpus trained on provenance labels alone.

Extend annotation coverage to a source-disjoint benchmark under a separate
declaration. Freeze membership before opening either detector's scores, include
human, model-only, mixed and repeated-revision cases, and report source/generator
slices, coverage failures and false-positive uncertainty. Keep v6's sealed
comparison intact. Compare future models on identical texts and retain each
Pangram version and observation date so provider updates remain visible.

Archive every submitted text, receipt, response, version and failed item. Bulk
results expire 48 hours after completion. Raw annotations stay in private
storage. Under the project's publication policy, Pangram scores are kept
privately and not published, and they never become origin labels; Pangram links
and ids may be published where they serve verification. Training on them follows the teacher-supervised protocol rule
above.
