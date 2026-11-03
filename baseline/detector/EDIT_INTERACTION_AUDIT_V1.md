# Interaction audit of the saved B repair menu

Audit the complete four-edit, sixteen-state workshop repair menu already used
in B development. The three-edit and four-edit finalists have known near-zero
A scores and 100% Pangram-flagged content. This is retrospective investigation
of a selected failure, with no new text generation, inference or provider call.
Keep all B source text, edits and author references in their existing private
storage. This audit does not admit those texts to A training.

Use every subset, including the unchanged parent and all single edits. Bind
the saved plan, complete three-class CPU responses, prior ranking, V6 artifact
manifest and fitted temperature. Verify text hashes, unique subsets, complete
cube coverage, unchanged aggregate probabilities and strictly positive finite
class probabilities. A zero probability makes the inverse logit unavailable;
stop rather than clip it into a fabricated finite value.

Let `q_h`, `q_m` and `q_x` be the recorded probabilities for human, model and
mixed origin. If the fitted temperature is `T`, softmax calibration gives:

```text
model_minus_human_logit = T * (ln(q_m) - ln(q_h))
mixed_minus_human_logit = T * (ln(q_x) - ln(q_h))
raw_union_margin = logsumexp(model_minus_human_logit, mixed_minus_human_logit)
calibrated_union_margin = ln(q_m + q_x) - ln(q_h)
```

These recover class-logit differences up to floating-point error without
loading model weights. The common additive logit offset is irrelevant. Union
pooling and the sigmoid can themselves introduce nonadditivity, so retain both
individual class-logit differences alongside union margins and probabilities.

For each scale and subset `S`, compare the observed value with the prediction
from the unchanged parent plus its single-edit differences:

```text
additive_prediction(S) = f(empty) + sum(i in S, f({i}) - f(empty))
residual(S) = f(S) - additive_prediction(S)
interaction(empty) = f(empty)
interaction(S) = f(S) - sum(R proper subset of S, interaction(R))
```

The final expression separates each order's interaction from lower-order
terms. Also combine the two additive class-logit predictions through the
original temperature and softmax to obtain a predicted detector probability.
If class-logit residuals remain large, temperature scaling alone cannot explain
the collapse. This does not identify a particular neuron, word or linguistic
cause, validate an off-manifold gradient, or establish a population property.

Report all sixteen states and contextual edit-effect ranges. Preserve the
existing editorial judgments: the two finalists received complete reviews,
but not every intermediate state was approved. Pangram observations apply only
to the exact submitted texts. New comparisons do not change the earlier
adaptive selection or turn these examples into held-out evidence.
