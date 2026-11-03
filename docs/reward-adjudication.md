# Style utility and semantic review

[Whitepaper draft 0.60](../whitepaper/slop_ninja.pdf) proposes these candidate formulas for the
[later B mechanism](../whitepaper/sections/07a-adjudication.tex). They are a
research contract; *B* is deferred, and its scoring authority, review process,
and activation remain open. Launch *A* scoring follows the separate
[reward policy](../whitepaper/sections/07-rewards.tex).

The [offline subnet prototype](../src/subnet.rs) implements a binary
reward helper. It does not implement the rank, headroom utility, or complete
semantic-review contract below. Existing style experiments measure components;
they do not qualify this utility for emissions.

## Frozen numerical inputs

A comparison fixes source `x`, candidate `z`, authorized target-author
references, a public style evaluator, and a nonempty calibration multiset
`C = [c_1, ..., c_M]`. The evaluator returns a nonnegative integer distance
`d_theta(text, references)` or an explicit error. Its artifact binds feature
meanings, pooling, parameters, runtime, arithmetic, and resource limits.
Calibration uses development queries and independent same-author reference
works, separated from fitting and evaluation. Preserve repeated distances.

For the intended personal-voice service, the target is the private writer
profile that *A* uses: trusted preprocessing represents each of the customer's
independent writing samples by the *A* encoder's output and stylometric features
(function-word frequencies and character n-grams), then pools them under a
qualified, versioned pooling contract. A normalized weighted mean is the
starting proposal; the contract fixes feature scaling, per-work weights,
missing-channel handling, and the encoder, feature, and pooling versions. The
learned author-fit scorer consumes that profile instead of raw references;
editing still needs the source prose, and preprocessing still reads the samples.
Grammar-level writer-style features from the native CFG are a candidate further
*B* input that needs its own qualification: CFG writer vectors scored below the
encoder on verification and, like every profile tested, near chance on author
fit ([writer-profile comparison](cfg-vs-encoder-results.md)). Freeze the profile
and its reference bindings for the comparison. Keep the samples and profile
private under the job or customer scope, with no disclosure to serving miners.

*B* uses continuous *A* author-fit feedback to rank iterative revisions toward
that writer. No profile type qualifies to supply it yet; the feedback waits for
a learned matcher trained on fresh authors and for rewrites conditioned on
several of a writer's own samples. *A*'s `provided_writer` named match, displayed as "Provided writer",
is a categorical result under the normal match rule; it cannot measure each
incremental improvement or authenticate a real-world identity. A qualified
mapping from *A*'s continuous fit signal to `d_theta` may supply this evaluator.
That mapping, its numerical behavior, and its calibration need validation before
reward use. This proposal leaves the distance-rank and utility formulas below
unchanged.

For `Q = 1_000_000` and distance `d`, define:

```text
greater = count(c in C where c > d)
equal = count(c in C where c == d)
q = floor(Q * (2*greater + equal) / (2*M))
```

Use integer arithmetic with sufficient width. Smaller distance cannot reduce
`q`; exact ties receive half credit before quantization. A distance below every
calibration value scores `Q`, and one above every value scores zero. This is a
distance rank, not an authorship probability or a semantic verdict.

Compute `q_x` and `q_z` with the same references, evaluator, and calibration:

```text
if q_x == Q:
    V_numerator = 0
    V_denominator = 1
else:
    V_numerator = max(0, q_z - q_x)
    V_denominator = Q - q_x
V = V_numerator / V_denominator
```

Store reduced rationals with a positive denominator and zero as `0/1`.
Retain exact rational arithmetic through aggregation. A tie or a source
already at `Q` earns zero improvement. Check raw `q_z >= q_x` separately;
clipping negative improvement cannot conceal regression.

## Semantic fields

The brief fixes the intended style, protected information, readability
requirements, and tone before generation. Source tone supplies defaults where
the brief is silent. Deliberate informality can be a feature to preserve.
Stronger rhetoric cannot increase factual certainty or obligations.

Review the complete source and candidate, including claims omitted from any
checklist, before exposing numerical scores. Each field records pass, fail,
or unresolved, with reasons and relevant spans:

| Field | Required result |
| --- | --- |
| `P`, preservation | Preserve claims, entities, quantities, negation, uncertainty, citations, attribution, and argument relations; add no unsupported meaning |
| `R`, readability | Avoid material regression and satisfy the stated cleanup/readability requirements |
| `T`, tone | Satisfy the requested tone and audience constraints while preserving intended voice features |

`G = 1` requires every approval. An established failure gives `G = 0`;
missing evidence leaves `G` unknown. Detector scores provide none of these
verdicts. Choosing the review authority, dispute rules, and validated use of
automated judges remains part of *B* activation.

## Detector improvement and combined utility

The proposal uses three Pangram observations per source/candidate
and three frozen public *A* artifacts. Let `F_r(t)` be Pangram's AI-plus-assisted
fraction and `H_j(t)` the panel member's AI-involvement probability:

```text
Delta_P = min_r F_r(x) - max_r F_r(z)
Delta_S = median_j (H_j(x) - H_j(z))
E = (Delta_P + Delta_S) / 2  if Delta_P >= 0 and Delta_S >= 0
E = 0                      otherwise
```

Fractions and involvement probabilities remain different measurements.
Freeze report counts, request identities, retries, provider versions, and
selection before generation. Retain required observations and failures;
neither miner nor operator may select favorable reports afterward.
Reopening one report creates no new observation.

With complete evidence, utility is `G*(V+E)/2` only when `q_z >= q_x`,
`Delta_P >= 0`, and `Delta_S >= 0`; any regression gives zero. Missing
judgments retain an unresolved status. The paper separates incremental
utility from its stricter detector-success target and specifies the proposed
handling of unscorable entries. No threshold crossing alone supplies approval.

## Validation before rewards

Test adaptive optimization of the style evaluator under declared budgets,
including copying references, changing topics, padding, and semantic errors.
Among acceptable revisions, higher rank must agree with independent judgments
of author fit. Compare against unchanged sources and ordinary editing using
held-out source families and writers. Retain failed candidates, search costs,
semantic failures, and reader judgments alongside metric gains.
Test the effect of sample length, topic, register, and averaging heterogeneous
works. Compare independent queries and richer reference representations before
adopting a single mean. Check that iterative *A*-guided gains survive independent
author-fit judgments and do not rely on copying samples. Higher fit leaves AI
share/confidence and documented production history as separate measurements.

Review the assembled document. Sentence or chapter success cannot certify a
whole manuscript's claims, terminology, or voice. Qualify every supported
length class separately. The [author-projection](author-projection.md),
[transfer](global-voices-transfer.md), and
[edit-utility](edit-utility-matrix.md) studies supply bounded research evidence.
They do not establish a trained production evaluator or a live reward engine.
