# Voice and pronoun components of the news revision

The passive recast alone lowered frozen V6 from 90.94% to 67.71%, a 23.23-point
drop. Replacing the later repeated name with a pronoun alone raised it to 91.60%.
Combining the edits produced the previously recorded 72.84%. On this passage,
the pronoun change partly offsets the voice change.

| State | A probability | Tokens | Model-human logit | Mixed-human logit |
| --- | ---: | ---: | ---: | ---: |
| Original parent | 90.94% | 295 | 2.0054 | 3.6875 |
| Passive only | 67.71% | 297 | -0.8676 | 0.7194 |
| Pronoun only | 91.60% | 291 | 2.1540 | 3.8469 |
| Both | 72.84% | 293 | -0.4407 | 1.1967 |

The [protocol](STRUCTURAL_COMPONENTS_V1.md) fixes a two-component menu over
one previously selected Train passage. Rust applied exact disjoint edits and
verified that their combination reproduced the captured revision. The saved
parent and combined scores were reused; only two new CPU inputs were scored
on Studio with the unchanged bundled V6 runner. Neither input was truncated.

All four complete states were read before the two new scores. The three edited
states were accepted relative to the immediate model parent. In this context,
the later pronoun refers naturally to the named appointee even with the active
clause retained. The earlier parent's factual errors against its human root
remain separate. This is an assistant review, without independent author approval.

Both states containing the passive miss the strict 84.3649% cutoff; all four
exceed the loose 42.8737% cutoff. The probability interaction is +4.47 points,
with corresponding raw class-logit interactions of +0.2784 and +0.3179.
These are finite component effects, not gradients or hidden-neuron attribution.

The source was selected after its earlier score drop. The passive recast changes
word order, auxiliary and preposition words, and token positions together. It
does not isolate an abstract grammar feature or establish a general voice effect.
No state reached 10%, no Pangram call was made, and no data entered training.
V6 remains unchanged. The subsequent [three-source follow-up](VOICE_PROBES_RESULTS_V1.md)
tested equivalent edits with unedited human controls. Its drops were all below
0.35 points and did not reproduce the large news effect.

The [numeric report](results/structural-components-v6-v1.json) includes every
probability, class contrast, threshold result and input hash. The
[review](results/structural-components-review-v1.json) records each judgment.
The preparation and reporter checked source reconstruction, cached response
identity, runtime, calibration, complete response coverage and effect arithmetic.
Rust formatting, all-target tests and Clippy passed on Studio. Full texts and
execution records remain in the ignored experiment archive.
