# Reporting the fresh v6 Pangram comparison

The [comparison protocol](PANGRAM_REVISION_V6.md) and
[162-text membership](results/pangram-revision-v6-preparation.json) are frozen.
Two Rust examples produce the reports from saved predictions and annotations.
Neither example runs a model, fits a threshold or contacts Pangram. Run them on
the Mac Studio after the original evaluation and provider collection finish.

`subset_revision_reports` requires the successful completion receipt from the
original evaluation coordinator. It rechecks the pre-Test file bindings,
opening receipt, selected artifacts and thresholds. It then checks each saved
request against the original text and each report probability against the saved
inference response, with full coverage and origin-label checks.

For each route and revision stage, it writes a new observation view over the
exact selected ancestry archive. It recomputes metrics, denominators and cutoff
diagnostics on the 54 selected observations using unchanged probabilities,
training-prior controls and operating points. Each derived report identifies
its original report and view. The existing frozen `compare_encoder_reports`
binary supplies all six paired comparisons and their 512 family-bootstrap
draws. The original full-cohort reports remain intact.

`compare_revision_anchor` joins those reports to the completed Pangram job.
It checks the exact plan and payload hashes, the collection receipt, every
annotation's membership and text hash, allowed echo normalization, fractions,
versions and terminal status. It retains failed observations and reports their
denominators. Paired Pangram counts use successful observations; detector
counts continue to include all assigned texts. Origin labels come from the
recorded workflow and never change in response to a detector score.

Each stage view uses its actual saved predictions, including that view's
exports of shared human and edited texts. The unique-text summary uses the
primary R2 human export, each route's R2 direct human edit, and each distinct
model draft or revision at its own stage. The report preserves all repeated
exports and records numerical variation and threshold disagreements. Each
shared text counts once in the unique-text summary.

For example, on the Studio after building the examples:

```sh
cargo build --examples
run_root=~/slop_ninja_runs/revision_expansion_v6/detector_v6_v1
prep_root="$run_root/pangram-confirmation-v1"
target/debug/examples/subset_revision_reports \
  --run-root "$run_root" --preparation-dir "$prep_root" \
  --output-dir "$prep_root/encoder-subsets-v1"
target/debug/examples/compare_revision_anchor \
  --subset-dir "$prep_root/encoder-subsets-v1" \
  --plan-dir "$prep_root/bulk-plan-v1" \
  --collection-dir /path/to/archived-job/collection-TIMESTAMP \
  --output-dir "$prep_root/comparison-v1"
```

Copy the complete provider job archive to the Studio, including its binding,
plan, request and terminal collection. Use new output directories. A failure
preserves partial output and requires inspection before another invocation.
The commands above use the declared artifact layout. The completed v6 run
produced all twelve subset reports and six paired comparisons, then joined all
162 successful Pangram annotations. Repeated exports of shared text had
identical encoder probabilities and no threshold disagreements.

Both output directories contain a `summary.json` suitable for aggregate
publication. Keep per-record predictions, failed provider responses, input
paths and raw annotations in ignored storage. Report collection, original
composer, initial-profile and origin slices along with the full denominators.
The small paired cohort cannot establish rare population false-positive rates
or superiority at a matched population FPR. Pangram content fractions and the
detectors' document-origin probabilities measure different quantities.

The published [subset comparisons](results/pangram-revision-v6-subsets.json),
[Pangram comparison](results/pangram-revision-v6-comparison.json) and
[results assessment](releases/v0.6.0/RESULTS.md) retain the completed evidence
and its limits. No additional inference or provider calls were needed for
these reporting steps.
