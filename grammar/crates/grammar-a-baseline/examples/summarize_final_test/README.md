# Frozen final Test aggregate report

`summarize_final_test PLAN.json NEW_PUBLIC_JSON` reads retained numeric evidence,
validates the frozen selection, and writes one new aggregate-only JSON file. It
does no inference, provider calls, calibration, fitting, or model selection. It
does not open dataset records, reference prose, or generation logs. It reads the
metadata population audits and numeric rows already bound by the final comparators.

The private plan has schema `slop_ninja_final_test_summary_plan_v1`. Every file
object below is `{ "path": "ABSOLUTE_PATH", "sha256": "64_HEX_DIGITS" }`.
The plan itself is hashed into the public result. Transitive inputs are verified
against their manifests/receipts and rechecked before publication.

```json
{
  "schema": "slop_ninja_final_test_summary_plan_v1",
  "selection": {"path": ".../selection-v3.json", "sha256": "..."},
  "dataset_manifest": {"path": ".../dataset-v1/manifest.json", "sha256": "..."},
  "runs": [
    {
      "candidate": "warm_text",
      "seed": 17,
      "numeric_manifest": {"path": ".../numeric-test/manifest.json", "sha256": "..."},
      "prediction_receipt": {"path": ".../prediction-receipt.json", "sha256": "..."},
      "evaluation": {"path": ".../test-report.json", "sha256": "..."},
      "pangram_comparison": {"path": ".../pangram-test.json", "sha256": "..."}
    }
  ],
  "v6": {
    "numeric_manifest": {"path": ".../numeric-test/manifest.json", "sha256": "..."},
    "report": {"path": ".../v6-test-report.json", "sha256": "..."},
    "frozen_embedding_manifest": {"path": ".../original-embeddings/manifest.json", "sha256": "..."},
    "embedding_manifest": {"path": ".../test-embeddings/manifest.json", "sha256": "..."},
    "embedding_rows": {"path": ".../test-embeddings/rows.jsonl", "sha256": "..."}
  }
}
```

Expand `runs` to all **16 frozen candidate names × seeds 17, 29, 41**. Exactly 48
unique entries are required, including complete receipts/reports for all-failed
runs. Fit paths, weights and the original CAL isotonic mapping come exclusively
from the selection receipt. No fit directories are written. Original8,
expanded2, mixture2 and source-size4 candidates remain in their declared order;
Test scores never reorder them or change the selected candidate.

The public table contains each seed's involvement Brier, FPR, calibration bins,
three-way origin, share, conditional attribution and open-set scores. Equal-seed
means/min/max are descriptive; if any seed's estimate is unavailable the summary
estimate is null and `available_seeds` shows the gap. Missing gallery/head metrics
are explicit. No confidence-interval endpoints are averaged.

The scalar allow-list also retains balanced origin accuracy/Brier and the frozen
Train-prior involvement Brier. Six fixed realized-share bins (`0`, `(0,25]`,
`(25,50]`, `(50,75]`, `(75,100)`, `100`) expose
`share_bin_{bin}_{mae_percentage_points,mse_percentage_points_squared,bias_percentage_points,documents}`.
Reference retrieval exposes `reference_{human,ai}_{top1,top3,mrr,head_known_top1,head_withheld_top1}`,
each metric's `_documents`, and `reference_{human,ai}_gallery_identities`.
All appear per seed and in the fixed-seed summary. Missing bins or unavailable
reference galleries remain null; no identities or query diagnostics are projected.

Paired involvement differences use the existing `summarize_v6_fusion` and
`summarize_warm_start` helpers: average three fixed-seed differences before the
1000-draw source-family bootstrap. Selected/V6/Pangram comparisons use one common
intersection across all three seeds and both comparators. Candidate comparisons
use one common support across all 48 candidate runs; a completely failed
candidate therefore makes that intersection empty, while full-support tables and
selected/V6/Pangram comparisons remain available.
Balanced cell counts are distinct from document counts. Full-support table and
common-support intervals retain their separate denominators.

The upstream requested/assembled/excluded/terminal-failure counts come from the
exact bound dataset manifest, never from successful prediction counts. The
reported 416 requested, 414 assembled, two terminal Phi failures are not fabricated
defaults: the actual retained manifests must reconcile when the parent runs this
helper. Detailed IDs and failure evidence remain in the pinned private artifacts;
public output contains counts and hashes only.

All five requested stratification axes are implemented for **paired involvement
differences** on selected/V6/Pangram common support: generator (anonymous known
slots and withheld), production history, realized share bins, replacement position,
and writer known/withheld/unknown/absent status. Full rewrite remains mixed history
at realized share 1. No individual author is invented. Stratified all-head metrics
are not implemented; conditional attribution/open-set metrics remain in the full
per-seed table. Pangram calibration bins are absent from the comparator schema and
reported unavailable. Its frozen CAL mapping remains a proxy, not native confidence.

V6 is origin-only, uses the original temperature and 2048-token/no-truncation
contract, and retains unsupported/failure counts independently of CFG support.
Archived binary V6 evidence is explicitly separate from this fresh mixed-origin
evaluation; it is never pooled or relabelled.

Only synthetic tests are supplied. No actual final Test artifacts were opened
while developing this helper. The parent coordinates compilation and execution.
