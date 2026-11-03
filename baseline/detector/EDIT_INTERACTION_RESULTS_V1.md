# Class-score interactions in the saved B repair menu

The saved three-edit repair scored 3.97% on A. Adding its single-edit effects
in class-logit space would predict 72.43%. The difference persists before
temperature scaling and probability conversion: both recovered class-logit
contrasts depart substantially from the single-edit prediction. This identifies
an interaction in A's response to these particular combined edits.

The [audit method](EDIT_INTERACTION_AUDIT_V1.md) uses the sixteen saved CPU
responses from the [B proposal experiment](../transformer/RESULTS_PROPOSALS_V1.md).
It recovers model-versus-human and mixed-versus-human logit differences from
the three recorded probabilities and fitted temperature, 1.8776105793. All
subsets of the four edits are present, with matching text hashes, ranking,
artifact and calibration. No weights were loaded and no model or provider
calls were made.

| State | Observed A probability | Probability from additive class logits | Model-human logit residual | Mixed-human logit residual |
| --- | ---: | ---: | ---: | ---: |
| Unchanged parent | 94.85% | 94.85% | 0.00 | 0.00 |
| Three core edits | 3.97% | 72.43% | -7.36 | -8.25 |
| Four edits, explicit referent | 7.26% | 75.30% | -6.62 | -7.16 |

The three core edits individually scored 89.17%, 89.88% and 91.91%. Their
pairwise model-human interaction coefficients are -1.60, -1.03 and -3.16;
the remaining third-order coefficient is -1.56. These sum to the three-edit
residual of -7.36. The residual therefore includes pairwise and third-order
effects. It is not a measure of the third-order term alone.

Edit effects also depend on the other choices already applied. Across its
eight contexts, `proposal-07` lowered A by 3.71 to 74.24 percentage points.
Its model-human logit change ranged from -1.39 to -7.85. A priority list based
only on each edit against the unchanged parent would lose this information.
The explicit-referent edit raised A in all eight contexts, including from
3.97% to 7.26% in the reviewed finalist. Its clarity benefit remains a separate
editorial judgment.

## Interpretation and limits

Both finalists still had 100% Pangram AI-written content in the earlier paid
comparison. This audit adds no external detector evidence and improves no A
model. The menu was selected during adaptive B development; it is one known
failure, without an unseen-source performance estimate. Only the two finalists
received the complete final editorial review. Intermediate states remain
distinct from approved transformations.

The recovered values describe the combined tokenizer/model response on this
finite set of texts. They do not locate a responsible neuron or grammatical
feature, establish a useful continuous gradient, or show that these edits
behave similarly on other texts. Retain actual text interventions when testing
future gradient proposals. Keep multi-edit search and recompute priorities
after edits change the context.

For harder A data, investigate independently constructed, reviewed combinations
on permitted sources. Keep these B texts out of training. The active paired
fits, frozen comparisons and thresholds remain unchanged; this result does
not authorize a new training admission or select a replacement detector.

## Reproduction and verification

The [Rust auditor](examples/audit_edit_interactions.rs) accepts a private config
binding the saved plan, scores, ranking, artifact manifest, calibration and
protocol. It checks complete subset coverage, rejects zero or invalid class
probabilities, and rechecks the bound inputs before creating a new report:

```sh
cargo run --example audit_edit_interactions -- --config config.json --out report.json
```

The [aggregate report](results/edit-interaction-audit-v1.json) contains all
sixteen states on five scales, additive predictions, interaction coefficients
and contextual effect ranges. It contains no source passages or edits. The
[independent arithmetic check](results/edit-interaction-independent-check-v1.json)
used direct signed-subset enumeration and ratio/log reconstruction; all five
scales, all subsets and all contextual ranges matched within `1e-10`. It also
reconstructed every observation by summing its subset coefficients.

Studio formatting, all-target tests and Clippy with warnings denied passed.
The focused tests cover nonlinear probabilities with additive class logits
and a known injected third-order class-logit effect. The final executable
reproduced the first audit report byte for byte.

| Artifact | SHA-256 |
| --- | --- |
| Aggregate report | `0c99eca7870f4743c640585ae7d640632486409c95fc1a6e39d38c4515e5ee38` |
| Rust source | `116db68cc3af072f22a049d3119fea8bb589459bca8455cdece57a68cf2d5c2c` |
| Studio executable | `34692b7d08695546265343a6871a4c302a7a1a17e9646dd070e3789e44ffc365` |

Exact input paths and the independent check script remain in the ignored
Studio study archive. The config schema and report bindings permit replay by
an operator with those saved inputs. This is a reproducible local diagnostic;
the published aggregate alone does not permit independent rescoring of the
private texts.
