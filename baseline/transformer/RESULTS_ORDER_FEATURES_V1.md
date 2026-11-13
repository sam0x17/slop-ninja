# Sentence-order features: first B comparison

No candidate met the joint detector target. Across nine Pangram observations,
every passage remained entirely AI-written or AI-assisted. The new features
changed which edits ranked closest to the reference profile, but this experiment
does not demonstrate better author resemblance or a successful B transformation.

The [aggregate measurements](results/order-feature-results-v1/summary.json)
retain all 93 menu states, the eight weight comparisons and input hashes.
The [external report](results/order-feature-results-v1/external.json) lists
all nine Pangram observations by text hash and request id, without passage text.
Row-level Pangram results are kept privately and not published.
Raw plans, reference excerpts and revisions remain in ignored storage.

## Representation and selection

The preceding experiment found sentence permutations with identical default
word-occurrence and grammar features. The optional
[order extractors](../../grammar/ORDER_FEATURES.md) add sentence position and
within-paragraph transitions. Both versions preserve the default extractor and
use distinct feature identities.

Joint v1 combines opening POS tags, root structure, root morphology and length
into one sentence pattern. It distinguished the earlier permutations, but their
distance from the reference profile still tied at 1.0. With only two reference
works, these detailed patterns had too little overlap to guide this comparison.
Factorized v2 measures each descriptor separately. It distinguished both the
points and their distances, at the cost of losing some descriptor correlations.
The new menu had 201 joint coordinates and 470 factorized coordinates.

The source was the familiar synthetic workshop notice. The two Global Voices
reference excerpts were unchanged. Their incomplete extraction, uncertain
production history and different genre remain limits on any author-fit claim.
The menu combined eight word, clause and paragraph choices up to depth three,
producing 93 distinct states. An earlier unscored 79-state preparation was
replaced after fixing artificial overlap between edit anchors.

Four order weights were fixed for comparison: 0, 0.1, 0.25 and 0.5. Squared
distance is `(1-w) * occurrence_squared + w * order_squared`; the target and
scales were fitted only on the reference works. These are exploratory weights.
None was trained or validated on held-out authors.

Eleven candidates received full-passage assistant reviews before the new A or
Pangram scores were opened: eight single edits and three distinct style winners.
Previous adaptive results and style distances were known. Reviews covered the
original claims, requested tone, readability, cleanup and author fit; they were
not independent validation. Seven revisions were then sent to Pangram: the
three style winners and four best remaining A scores from the reviewed pool.
The batch also included the unchanged parent and original source controls.

## Results

Lower A probability is better. Pangram flagged content includes both its
AI-written and AI-assisted fractions. Those measures have different meanings;
A probability is not a percentage of AI-written words.

| Representation | Order weight | Selected edits | A probability |
|---|---:|---|---:|
| Joint v1 | 0 | Cancellation clause, review clause | 95.99% |
| Joint v1 | 0.1 | Same | 95.99% |
| Joint v1 | 0.25 | Same | 95.99% |
| Joint v1 | 0.5 | Same | 95.99% |
| Factorized v2 | 0 | Cancellation clause, review clause | 95.99% |
| Factorized v2 | 0.1 | Cancellation paragraph break and clause, review clause | 95.63% |
| Factorized v2 | 0.25 | Same | 95.63% |
| Factorized v2 | 0.5 | Booking and cancellation paragraph breaks, cancellation clause | 87.23% |

None of the selected revisions met the joint target on Pangram.

The parent scored 94.85% on A. The factorized 0.5 selection improved on that
parent and the old feature ranking. The best A score across all 93 states was
86.98%, but that candidate lacked a pre-score review and was not sent to Pangram.
Neither result approaches the required A probability below 10%.

For the seven measured revisions, Pangram split the flagged content between
its AI-written and AI-assisted classes. Human fraction was zero in every
observation. Paragraph-only variants matched their corresponding controls in every returned
window field, despite different returned text hashes. A and the style features
did respond to those paragraph edits. This establishes an observed difference
between the systems, without establishing Pangram's internal preprocessing.

Meaning and requested tone remain requirements. A neutral word-order or
formatting probe can help measure the search space, but a final revision still
needs a concrete editorial benefit. Smaller distances or lower detector scores
cannot supply that benefit.

## Validation and limits

All model inference, spaCy extraction, feature fitting, builds and tests ran on
the Studio. The 39 grammar-core tests and nine B tests passed, as did formatting
and clippy. Two core tests were added for order collisions and opportunity
counts. Installed spaCy extraction ran on the actual menus; the refactored joint
v1 audit reproduced the earlier audit exactly. Linux was not rerun for this
change, and ordinary Mac execution provides no confidential-compute qualification.

This batch's spend is recorded in the private ledger. All nine paid requests
were collected.

The comparison uses one repeatedly edited source and two imperfect reference
excerpts. It supplies no held-out-author result, independently confirmed
preservation result or trained B policy. The whitepaper has no new baseline
milestone to report. Further work needs content-sensitive alternatives and
broader source/reference comparisons; the current paragraph changes did not
improve Pangram's human classification.
