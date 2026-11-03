# A origin-consistency ablation

This experiment tests whether explicit agreement across recorded model revisions
improves A beyond supervised training on the same examples. The earlier
[diagnostics](A_ROBUSTNESS.md) show weak transfer to Pangram on optimized text,
generator/profile gaps, and inadequate long-document training coverage. The
small fixed grammar pilot did not reproduce the failures. This first ablation
uses existing style revisions; it does not fill the length or generator gaps.

## Inputs and pairing

Use the unchanged V6 Train, Development and Calibration archives and their
existing source-group boundaries. Train contains 3,833 records from 471
families. Its SHA256 is
`023d6e5428236502e877bac0afe0e9e9ee4aa9d10ab38eb940ca8e915cf7502e`.
Retain the original checkpoint, rights manifest and whitespace-derived Train
view. No private manuscript, B output, hosted capture or Test record enters
training. No generation or paid detector calls are part of this experiment.

The Rust preparer follows recorded model-only revision parents, verifies that
each chain is complete and unbranched, and cycles its members in depth order.
The final member pairs with the original draft. Records without a revision
chain pair with themselves. Every input ID occurs once as anchor and once as
peer, within the same existing source family and origin class. This preserves
the source-origin supervised weighting instead of oversampling difficult
outputs. The mapping uses no detector scores.

The anchor follows V6's raw/collapsed-whitespace schedule by family and epoch;
the peer uses the opposite formatting view. Human and mixed-origin examples
therefore receive paired formatting exposures, while recorded model chains
also receive different revision stages. Claims can drift during the original
generation workflow. These pairs assert unchanged recorded origin, not
semantic equivalence or successful author-style imitation.

Preparation found 454 complete three-member revision chains, supplying 1,362
cross-revision pairs. The other 2,471 rows use formatting-only pairs. The Rust
preparer validated the recorded ancestry and a bijection over all 3,833 rows.
The first preflight rejected an accidentally abbreviated expected hash before
writing a manifest; the corrected full hash matched the unchanged archive.

## Fitting

Train two candidates from the original ModernBERT-base checkpoint. Retain V6's
seed 17, four epochs, learning rate 0.00001, batch size four, AdamW, weight decay
0.01, gradient clipping at 1.0, source-origin weighting and 2,048-token limit.
Use full, untruncated texts. Both candidates perform the same two forward
passes with independent training dropout and gradients through both branches.

For each pair, average the two supervised three-class cross-entropies. The
control uses this loss alone. The treatment adds 0.5 times the Jensen-Shannon
divergence between the two three-class probability distributions. Apply the
existing source-origin weight to both terms. Report the divergence separately.
The control isolates the extra exposure from the consistency penalty; frozen
V6 supplies the previous single-exposure comparison. There is no weight search.

For each candidate, select the earliest eligible epoch with minimum
uncalibrated binary human/model Development log loss on the original V6 R2
view. Require nonzero recall for both classes. Retain every epoch and failure.
Fit temperature on the unchanged Calibration R2 view only after epoch
selection. Freeze each candidate's existing-style nominal 1% and 5% human
false-positive thresholds before diagnostic evaluation. These empirical
operating points are not guarantees of population error rates.

Run sequentially on the Mac Studio in isolated experiment outputs. Snapshot
training/runtime code and bind the input files, pairing manifest, checkpoint
and protocol before fitting. Preserve raw logs, process identity and terminal
exit status. Do not resume or redraw a failed fit without first inspecting it.

The [synthetic MPS smoke](results/origin-pairs-smoke-v1.json) completed one paired
optimizer step with finite loss and gradient norm. Numerical tests cover zero
divergence for identical predictions, branch symmetry, bounded extreme-logit
behavior, gradients through both branches, and rejected input/mapping changes.
These checks establish execution behavior, not detector quality.

## Comparison and limits

Keep both candidates regardless of which looks better. Compare binary loss,
human false positives, model sensitivity and calibration against frozen V6.
Use the existing R0/R1/R2 and Phi-4 evaluation archives, the separate human-root
cohort, the 96 hosted captures with saved Pangram annotations, and the reviewed
grammar stress pilot. These are previously inspected diagnostics. They remain
excluded from gradient updates but cannot supply a fresh confirmation claim.
Keep source-family dependence and failed or unsupported observations explicit.

Evaluate the recorded private B failures on the Studio outside the repository
and datasets after both fits close. Reuse exact Pangram observations only for
unchanged texts. Agreement on those failures does not establish general transfer.
No threshold or candidate switching may follow diagnostic inspection within
this ablation. Coherent long documents, held-out styles and additional generator
coverage still need separate preparation and fresh evaluation before a quality
milestone or reference promotion. A null or worse result is a useful outcome;
do not relabel a training change as an improvement without measured evidence.
