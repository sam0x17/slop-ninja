# Edit-menu development results

Word and clause menus substantially reduced V6 A scores on two synthetic
notices. Neither final revision met the joint target on Pangram, so this
experiment has not met B's joint objective. Row-level Pangram results are kept
privately and not published.

| Notice | Original A | After first menu | Final A |
| --- | ---: | ---: | ---: |
| Workshop bookings | 72.85% | 45.10% | 4.14% |
| Offline notes | 85.06% | 76.80% | 6.90% |

A's number is its combined model/assistance probability. Pangram's fraction
measures the share of text assigned to a class. These are different quantities.
The table does not treat either as proof of authorship.

The [protocol](PROTOCOL_MENU_V1.md) and [aggregate evidence](results/edit-menu-v1.json)
record the method, hashes, all rounds and external observations. Each round
enumerated compatible subsets through three edits, retaining choices whose
individual scores increased. Rust applied exact source patches, reparsed the
full text and measured the reference-fitted word and grammar spaces. The
assistant proposed local lexical/clause alternatives and selected menu IDs;
the initial menus also included applicable existing grammar rules. No new B
weights were trained and no wholesale rewrite was used in this experiment.

There were 771 A scoring requests across seven menus, including repeated
starting states and the repetition-repair experiment. These are adaptive
development requests, not 771 independent benchmark examples. The first menus
contained 176 and 130 states; each subsequent menu contained 93. All runs used
the same immutable V6 artifact and canonical CPU runner on the Mac Studio.

The final function-word/grammar distances were 1.250674 for workshop bookings
and 1.157951 for offline notes, compared with 1.258719 and 1.200435 for their
originals. Full-vocabulary/grammar distances also improved. The two reference
works came from one attributed Global Voices author, selected before scoring.
Two excerpts from one author cannot establish writer identification or transfer
to unseen authors. The first-round detector winners actually worsened the
function-word/grammar distance, so a low A score cannot stand in for style fit.

## What the menus exposed

Some effects depended on surrounding edits. Rephrasing the workshop cancellation
request reduced A by as much as 9.72 percentage points in one context and
increased it by 0.98 points in another. In the offline-notes repair menu, the
best three-edit combination reduced A by 64.35 points from that round's start,
24.37 points more than the sum of its separately measured reductions. This is
nonadditivity in reported probabilities; calibration and saturation can
contribute, so it is not a claim about the detector's internal representation.

Reviewing complete passages changed a selection. A 16.56% offline-notes
candidate repeated “users” within one sentence. It remained unselected. A
repaired menu used an unambiguous pronoun and reached 12.45%; the next round
reached 6.90%. The selected passages retained the original operational conditions,
uncertainty and requested tone in the assistant's review. Those reviews were
neither independent nor blinded to A scores. They do not certify semantic
preservation or author resemblance.

## External confirmation

Pangram 4.0 returned nine successful observations: the original notices,
selected checkpoints, final revisions and a repeat of the offline-notes source.
None met the joint target or had any human-classified content. Pangram normalized paragraph whitespace. The first exact-echo collection failed;
inspection showed only whitespace changes, and a subsequent GET-only collection
recorded that explicitly. Both submitted and returned hashes remain bound to
the evidence. No paid submission was retried.

The document fractions concealed smaller changes in the window-level
`ai_assistance_score`, which moved slightly between revisions and between the
two observations of the repeated offline-notes source. These observations warrant
measuring Pangram directly, with repeats for effects near observed variability.
They do not establish a deterministic or calibrated search signal.

The spend of the two batches and the bulk estimate are recorded in the private
ledger.
Raw B text and author excerpts stay ignored. The B annotations use a separate
schema and are excluded from detector training.

The next experiment should use a bounded set of Pangram-scored alternatives,
including branches that improve prose but temporarily worsen A. Keep meaning,
requested tone and readability as separate requirements, measure the reported
content fractions as the external objective, and retain window scores only as
an additional observed signal. Check new source/author cases before claiming a
reliable B process or updating the whitepaper's demonstrated baseline strength.
