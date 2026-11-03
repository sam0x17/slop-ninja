# Large-backbone origin-consistency comparison

This is the first capacity experiment in the
[capacity and gradient-transfer plan](CAPACITY_AND_GRADIENT_TRANSFER.md).
It adds ModernBERT-large control and consistency candidates to the two
ModernBERT-base candidates already fitting under the
[origin-consistency protocol](ORIGIN_CONSISTENCY_V1.md). Retain all four candidates
and report both within-size and across-size comparisons. No result selects a
new reference detector by itself.

## Fixed inputs and recipe

Use `answerdotai/ModernBERT-large` at revision
`45bb4654a4d5aaff24dd11d4781fa46d39bf8c13`. The downloaded checkpoint manifest
SHA256 is `2727e31d1e1fd7cd55e0188ec4a68a067196152f745128f3619365daea8db765`.
The three-class classifier has 395,834,371 parameters, compared with
149,607,171 for base. This compares different pretrained backbones under one
recipe; it cannot isolate parameter count from pretrained weights.

Keep the original 3,833 Train rows from 471 families, their complete revision
pairing, raw/whitespace exposure schedule, Development R2 view and Calibration
R2 view. Preserve the rights manifest, source-group boundaries and 2,048-token
limit. No private B text, hosted capture, new human source or Test example
enters these fits. No provider requests or new generation are included.

The training settings remain seed 17, four epochs, batch size four, two forward
passes per pair, MPS float32 eager execution, AdamW at learning rate 0.00001,
weight decay 0.01, gradient clipping at 1.0 and source-origin weighting. The
control averages the two supervised losses; the treatment adds 0.5 times the
paired Jensen-Shannon divergence. Do not reduce batch size or change precision
silently if the larger model cannot execute this recipe.

The [full-length synthetic GPU check](results/large-paired-mps-2048-v1.json)
completed one paired optimizer step with four 2,048-token anchors and four
2,048-token peers. Loss, divergence and the pre-clipping gradient norm were
finite. The step took about 122 seconds while other Studio work was running;
this is not an isolated throughput benchmark. The MPS driver reported about
100 GB allocated after the step, including its cache; that is not a measured
peak-memory bound. The random token inputs establish execution at this shape,
not detector quality or numerical equivalence across devices.

Select the earliest eligible epoch with minimum binary human/model Development
log loss. Require positive recall for both classes. Fit temperature only on the
original Calibration R2 view after epoch selection. Keep the complete learning
curves and failed epochs. The training loss implementation and training script
must match the base experiment; loader and packaging changes are separately
bound and limited to checkpoint support and source identity.

## Execution and comparison

Snapshot source code, this protocol, the original pairing protocol, all input
hashes, the checkpoint manifest and launch commands before fitting. The planned
order is large control followed by large consistency, after inspecting the
terminal status of the existing base fits. A launch script must refuse to
overwrite artifacts or resume an incomplete optimizer state. Execution remains
on the Studio without restarting shared services.

Use the [public comparison](ORIGIN_COMPARISON_V1.md) with an explicit
`--checkpoint` pointing to the large checkpoint directory. This changes the
bound upstream checkpoint; the source archives, seven evaluation views, 96
hosted captures, 26 reviewed grammar states and V6 baseline reports stay fixed.
The comparison binds the new checkpoint catalog alongside the training and
inference sources. Freeze both candidates' nominal 1% and 5% human false-positive
thresholds before their diagnostic predictions. These are empirical operating
points, not population guarantees.

Once both sizes have complete public reports, use the existing paired-report
comparer for large control against base control and large consistency against
base consistency on each of the seven views. Preserve the within-size
comparisons and the V6 comparisons. Repeated stages share source families; do
not pool their denominators or treat them as independent trials. Keep hosted
generator/profile slices, grammar deltas and exact private full-document
transfer results separate. Reuse Pangram annotations only for identical texts.

This comparison is diagnostic. It does not select between models after seeing
private failures, establish a causal role for parameter count, validate gradient
transfer or justify resuming B optimization with an unconfirmed detector.
Broader training coverage and fresh source/style/generator confirmation remain
required. Predeclare replication seeds and an equal tuning budget before
claiming a reliable capacity advantage.
