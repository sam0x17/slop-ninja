# Neural word and grammar comparison

This is a feature-only comparison over the completed
[Rust exports](EXPLICIT_FEATURES_V1.md). Keep the existing encoder and
origin-consistency experiments unchanged. The comparison asks whether nonlinear
interactions improve on a linear classifier over these particular coordinates.
It does not establish that features should replace the text encoder.

Use all 3,833 Train records, the original R2 Development selection (318 records,
106 families) and R2 Calibration selection (324 records, 108 families). Preserve
every ancestor in the input archive checks. Test, hosted diagnostics and private
B outputs are excluded. No provider calls or provider loss are involved.

Fit these eleven candidates, retaining every result:

| Rust representation | Input coordinates | Hidden widths |
| --- | ---: | --- |
| Word | 8,192 | linear, 128, 256, 512, 1,024 |
| Grammar | 8,192 | linear, 128, 512 |
| Combined | 16,384 | linear, 128, 512 |

The nonlinear head is Linear, GELU, dropout 0.1, Linear to the three origin
logits. The linear control maps coordinates directly to those logits, without
dropout. The additional word widths approximately match the parameter counts
of combined widths 128 and 512; report exact counts. Equal-width word and grammar
heads have identical parameter counts. This is one seed and one optimizer
recipe, with no claim that it optimizes each architecture equally well.

Use CPU float32, two threads, seed 17, 30 epochs, batch size 64, AdamW learning
rate 0.0003, weight decay 0.001 and gradient-norm clipping at 1.0. Each epoch
visits every Train row once in a seeded shuffled order. Use the existing
source-origin weights: every family-origin receives equal total loss weight.
Retain measured-zero and unavailable-family coordinates. Convert the exported
Rust f64 values directly to float32, without fitting additional preprocessing.

Select the earliest epoch with minimum uncalibrated R2 Development binary NLL
among epochs whose human and model-only recalls are both positive at probability
0.5. Mixed examples contribute to three-class training; selection uses complete
model drafts and historical human proxies. Retain unsuccessful candidates if
no epoch qualifies. Fit the existing unweighted three-class temperature grid
on R2 Calibration only after epoch selection. Select architectures on Development,
never on their Calibration losses.

Save training and Development curves, three-class and binary metrics, exact
parameter counts, source/export/view hashes, selected weights, calibration,
rights notices and runtime source. These candidates do not use whitespace
augmentation or the origin-consistency loss. Comparisons within this grid are
controlled; differences from the running encoder experiment also include those
recipe differences. A matched fusion experiment remains separate work.

The runtime differentiates the uncalibrated margin
`logsumexp(z_model, z_mixed) - z_human` with respect to the scaled feature vector.
Numerical finite differences check implementation. Useful edit rankings still
require actual candidate reparsing, rescoring, fidelity inspection and external
transfer evaluation. Input gradients alone do not identify a meaning-preserving
edit or prove Pangram agreement.

The implementation uses the documented PyTorch
[linear layer](https://docs.pytorch.org/docs/2.14/generated/torch.nn.Linear.html)
and [autograd gradient operation](https://docs.pytorch.org/docs/2.14/generated/torch.autograd.grad.html).
Run fitting and checks on the Studio in the existing isolated ML environment.
The first execution target is macOS arm64 CPU. Linux remains to be qualified.
