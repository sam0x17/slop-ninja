# Comparison after origin-consistency training

Retain both candidates from [the declared ablation](ORIGIN_CONSISTENCY_V1.md),
regardless of the outcome. Their roles are fixed: control uses consistency
weight zero; treatment uses 0.5. The selected V6 artifact remains the baseline.
This comparison does not select a new model or fit a new temperature.

Freeze the existing public diagnostic inputs and V6 reports before either
candidate is available. After both fits close successfully, verify their
recorded training recipe, original input and pairing hashes, and earliest
eligible Development minimum. Require both complete artifacts before any
candidate threshold calculation. Use each artifact's bundled CPU runner.
Freeze the two nominal 1% and 5% human false-positive operating points for
both candidates on their unchanged Calibration R2 view before the first
diagnostic prediction. These are empirical operating points, not population
false-positive guarantees.

Reuse `encoder_evaluation` and `compare_encoder_reports` for primary R0/R1/R2,
Phi-4 R0/R1/R2, and all 117 original human roots. Preserve the exact source
archives and observation views from the original V6 comparison. Report each
candidate against V6 and the treatment against the control, with the existing
512 paired source-group bootstrap draws. Repeated human and mixed records
across stages are dependent observations; do not pool stage denominators.

Also score all 96 previously captured hosted outputs and all 26 reviewed
grammar-pilot states. Reuse their exact V6 predictions and saved Pangram
fractions. Report the hosted generator/profile slices and the grammar states'
changes relative to their own unmodified source, with human controls retained.
Pangram annotations apply only to the hosted texts that were submitted; none
is assigned to a different text. No new provider call is permitted here.

Preserve command arguments, stdout, stderr, response coverage, unsupported
inputs and terminal status. An error stops the comparison with partial files
retained. No automatic restart, skipped candidate, overwritten response,
threshold change or artifact substitution follows a failure. Recheck frozen
files after execution. Lower training loss alone is not evidence of better A.

These are previously inspected diagnostics. The private full-document B
failures still require a separate exact-text comparison on the Studio outside
the repository and datasets. Fresh style, generator and document-length
confirmation remains necessary before a quality milestone or reference
promotion. The newly collected longer human summaries are outside this fixed
comparison and outside both active fits.

For the [predeclared large-backbone experiment](CAPACITY_LARGE_V1.md), prepare a
separate plan with `--checkpoint /path/to/large-checkpoint`. The default remains
the original V6 checkpoint. All other data and scoring rules above still apply;
the large experiment retains its own frozen training protocol and source
catalog. Existing plans continue to use their original frozen executable and
policy hash. Do not run a new executable against those plans.
