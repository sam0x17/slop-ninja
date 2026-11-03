# Feature-head diagnostics after the fixed grid

Close all eleven fits from [the fixed protocol](FEATURE_NN_V1.md) before any
diagnostic inference. Verify input bindings and artifact files, complete
training histories, and the earliest eligible minimum for each selected epoch.
Report every candidate. Highlight the lowest Development binary NLL within
each representation; never choose a model or threshold using these diagnostics.

For every candidate, derive probabilities from its bundled Calibration R2
logits using its already fitted temperature. Freeze nominal 1% and 5% human
false-positive operating points with the existing Rust `select_operating_points`
function. Freeze all candidates' points before the first diagnostic prediction.
Use the strict-greater-than rule throughout. The 108 Calibration human families
support empirical operating points, not population guarantees.

Reuse the 96 exact hosted requests and 26 reviewed grammar states already frozen
for the [encoder comparison](ORIGIN_COMPARISON_V1.md), together with their saved
V6 responses and source metadata. Preserve the hosted generation/profile labels
and original Pangram fractions. The combined Pangram fraction is the sum of AI
and AI-assisted fractions; a fraction of at least 0.1 is the existing diagnostic
flag. This provider fraction and A's model-origin probability are different
quantities. Do not relabel provenance from provider predictions.

Parse each request once with the frozen small-parser identity and project it
into the three existing coordinate spaces. Validate every request ID and text
hash against the frozen inputs, preserve unsupported responses, and stop a
comparison if any expected score is unavailable. No refitting, truncation,
substitution, paid calls or newly generated text occurs here.

Report hosted detections at both frozen operating points, agreement with the
saved Pangram flags, and generator/profile slices. The hosted set contains model
outputs from 24 Train roots and no independent human controls. It tests
previously inspected style routes, not general detection accuracy or an unseen
source population. Grammar human controls also come from Development families.
Preserve those limitations in every interpretation.

For each grammar state, report the change from its own unchanged source in
calibrated probability and uncalibrated model-origin margin. Compute the source
gradient only for the unchanged controls. Compare `gradient · (edited_vector -
source_vector)` against actual margin changes and within-source rankings.
Retain ties, zero changes and multi-edit masks. Show probability changes
separately. Use an absolute margin-change tolerance of 0.00000001 when counting
direction and within-source ranking ties; retain all tied-pair counts.
The
three-class temperature transformation need not preserve every
ordering of uncalibrated union margins. The saved fidelity judgments remain
separate from detector measurements. No Pangram label exists for these grammar
edits, so this check cannot establish external gradient transfer.

Keep raw projections, gradients, provider evidence and source texts on the
Studio outside the repository. Commit code, input/output hashes and compact
results. All eleven candidates and inspected diagnostics remain research
artifacts. Fresh human controls, source/style/length confirmation, matched
encoder fusion, and external edit-transfer evidence are still required before
reference promotion or using gradients as trusted guidance for B.
