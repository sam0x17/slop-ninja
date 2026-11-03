# Run the fixed private comparison after public review

`private_origin_transfer` executes the existing
[base private protocol](ORIGIN_PRIVATE_TRANSFER_V1.md). It retains the two
candidate roles and three source versions from the previously frozen private
plan. Keep the plan, review receipt, source text and every execution output on
the Studio outside repositories and datasets.

The runner requires the completed public comparison and a review receipt before
creating an output directory or invoking a tokenizer or model. The receipt is
a record of an actual human or assistant review, not independent certification.
It binds the public plan, completion, artifact opening, and every required public
report. It cannot substitute for reading those results.

The receipt schema is `slop_ninja_public_comparison_review_v1`, with these fields:

- `reviewer` and a nonempty `notes` array describing the assessment and its scope.
- `public_plan_sha256`, `public_completion_sha256`, `public_opening_sha256`.
- `reviewed_reports`, mapping absolute report paths to their SHA-256 hashes.

For both candidates, review every declared public case report and its comparison
with V6, plus the hosted and grammar reports. Also review each treatment/control
case comparison. The current seven-case plan requires 39 files. Preserve the
operating points, source-group denominators, paired uncertainty and known limits
when assessing recall, human false positives and agreement with saved Pangram
observations. Do not select a different epoch or threshold from those results.

After that review, run the built example on Studio:

```sh
private_origin_transfer \
  --plan /private/study/plan.json \
  --public-review /private/study/public-review.json \
  --out /private/study/candidate-evaluation-v1
```

The runner rechecks the frozen private and public inputs, artifacts, and both
original Calibration operating points. For each source/candidate pair it invokes
the pinned Rust document preparer, checks exact source bytes and window geometry,
runs the artifact's bundled CPU scorer, and invokes the existing document reporter.
It retains arguments, stdout, stderr, exit status and complete window observations.
Failures preserve partial evidence and stop; the runner never resumes an output
directory or automatically retries a call.

Extract the direct `whole` observation for comparison with the saved V6 and
Pangram results. The old V6 reports have whole-text scores but missing overlapping
window scores. New candidate runs must score every planned window; their window
coverage can therefore improve while geometry remains identical. Never substitute
a window minimum or average for the whole-text observation. Pangram content
fractions and A's production-history probabilities remain distinct quantities.

The runner also supports the frozen [capacity extension](CAPACITY_PRIVATE_TRANSFER_V1.md)
with the additional evidence described below. It changes no frozen protocol,
starts no training, makes no provider request and promotes no reference model.
The first [complete base execution](ORIGIN_CONSISTENCY_RESULTS_V1.md)
followed review of all 39 public reports and scored all 30 planned observations
across the two candidates and three source versions. Every version retained its
original source bytes, token counts and window geometry.

The [Studio validation record](results/private-origin-runner-validation-v1.json)
records passing formatting, all-target Rust tests, Clippy with warnings denied,
and an example build. A live invocation against the incomplete public comparison
stopped before creating its output directory or invoking a tokenizer or model.
The whole-text extraction test rejects a missing or mismatched whole observation
even when a lower-scoring window is present. These checks cover the runner's
prerequisites and extraction behavior. That validation record predates the
completed private execution described above.

## Larger-backbone comparison

Supply the existing base review and completed private output, plus a record of
the actual review of all fourteen corresponding-role public comparisons:

```sh
private_origin_transfer \
  --plan /private/large-study/plan.json \
  --public-review /private/large-study/public-review.json \
  --base-public-review /private/base-study/public-review.json \
  --base-private-output /private/base-study/candidate-evaluation-v1 \
  --cross-size-review /private/large-study/cross-size-review.json \
  --out /private/large-study/candidate-evaluation-v1
```

Both sizes' fits and public comparisons must have closed successfully. Use the
existing `compare_encoder_reports` binary for large control against base control
and large consistency against base consistency on each of the seven fixed views.
Keep its baseline/candidate direction and save every output. This command reads
existing predictions; it does not invoke a detector:

```sh
compare_encoder_reports \
  --baseline /public/base/control/primary-R0/report.json \
  --candidate /public/large/control/primary-R0/report.json \
  --output /public/cross-size/control-primary-R0.json
```

After reading those results, create a `slop_ninja_capacity_cross_size_review_v1`
receipt. It contains `reviewer`, nonempty `notes`, `base_public_plan_sha256`,
`large_public_plan_sha256`, and a `comparisons` array. Each of its fourteen
entries contains `role`, `case`, an absolute `report` path and that file's
`sha256`. A case is the existing view name, such as `primary-R0` or `all-humans`.
Every role/case pair must occur exactly once. No receipt supplies independent
certification or substitutes for reading the reports.

The runner verifies all four manifests and their sealed files, original
thresholds, identical tokenizer assets, completed base-private observations and
unchanged source geometry. It checks cross-size report direction, input hashes,
artifact IDs, views, original operating points and comparison recipe. Base raw
responses must match all saved window observations. The final private report
retains both sizes' scores and flags, changes from each original, corresponding
large/base differences and within-size consistency/control differences. Three
versions of one known source remain one source family.

Add `--preflight-only` with a separate new output directory to verify these
prerequisites without tokenization or inference. Its completion explicitly has
`private_comparison_complete: false`; it cannot satisfy a completed-run requirement.
The execution records distinguish tested prerequisite handling from an actual
completed capacity comparison. The large fits remain in progress, so the full
capacity path still requires validation on their eventual artifacts and results.

The [capacity-runner validation record](results/private-capacity-runner-validation-v1.json)
records passing Studio formatting, all-target tests, Clippy and build checks.
The live early check stopped before output creation when the large public
comparison was absent. The new executable also passed a preflight against the
completed base artifacts, their original thresholds and actual public review,
with no tokenizer or scorer invocation. The additional base-private evidence
checker and complete four-candidate path still need their live capacity run.
