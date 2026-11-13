# Direct Pangram menu experiments

None of the 53 Pangram observations met B's joint detector target, and none
had any human-classified content. Some revisions moved content from Pangram's
AI-written class to its AI-assisted class without reducing the flagged total.
The process remains experimental. Row-level Pangram results are kept privately
and not published.

These six batches used one familiar synthetic workshop notice, 183 additional
A scoring requests, and the same two author-reference excerpts as the
[first menu experiment](RESULTS_MENU_V1.md). All substantive changes came from
exact word, phrase and clause patches or sentence permutations. No B weights
were trained. The [aggregate evidence](results/pangram-menus-v1/summary.json)
links every observation by text hash and request id, with the frozen plan/score
hashes. Unmeasured menu states remain explicit in each batch report.

| Revision | A probability |
| --- | ---: |
| Original notice | 72.85% |
| Earlier A-selected revision | 4.14% |
| Move purpose after practical details | 90.26% |
| Also put cancellation request before late-arrival warning | 89.33% |
| Also put reservation opening before session duration | 94.85% |

None of these revisions met the joint target on Pangram; their Pangram
fractions are kept privately.

The first two rows are controls from the earlier experiment. The remaining
rows continue its source-bound walk, including intermediate wording edits.
A reports a model/assistance probability; Pangram reports content fractions.
The joint objective uses both Pangram's AI and AI-assisted classes as flagged.
A favorable change in one column does not establish progress on the other.

## Findings for the search

Sentence order matters beyond the current count vectors. Moving the purpose
sentence after the booking details changed neither the full vocabulary/grammar
vector nor the function-word/grammar vector. It nevertheless changed both
detectors substantially. Moving two intact blocks in the cancellation paragraph
also left both vectors unchanged. These are concrete gaps in this
representation; they do not show that order alone measures style or quality.
An [extraction audit](results/pangram-menus-v1/vector-collision-audit.json)
confirmed equality of every raw feature family, including denominators and
metrics, under the same parser and feature identities. This check compared
the actual features, beyond merely observing equal scalar distances.
The next feature experiment should retain sentence position and transitions
between constructions alongside the existing occurrence distributions.

The ranking of wording alternatives changed with context. One opening scored
better than another when tested alone, but worse when both were paired with a
change to the trial-purpose clause: their order by Pangram window score
reversed in combination, and none of the four observations met the target.
Exhaustive combinations exposed this reversal; selecting only the best single
edit would have missed it.

Further edits were often worse. On one branch, adding a more direct
cancellation request moved content back from Pangram's AI-assisted class to its
AI-written class, and combining it with the reordered trial explanation moved
more. None of these additions increased the human fraction. Keep the unchanged
option and remeasure effects in their actual context.

Pangram also changed its window boundaries. A first-window score can improve
while a later window remains worse. The new Rust `menu external-report`
command retains all windows and the three content fractions, checks text echoes
and the pinned A artifact, and refuses to count a second collection of one
request as another inference. Its word-weighted window mean is only an
auxiliary diagnostic. It is neither a content fraction nor a calibrated
probability. Some repeats matched exactly; others varied in their window
scores.

## Writing quality and limits

Review preceded the new detector measurements in the first five batches.
For the sixth batch, A results and earlier Pangram observations were already
known. Every review was an assistant self-review, with earlier adaptive results
available. These judgments do not certify meaning preservation, better writing
or resemblance to the target author.

The editorial aim was to put useful booking information first, keep conditions
next to their qualifications, shorten indirect requests and clarify referents.
Review caught an ambiguity in a proposed trial-sentence move: “keep them” could
refer to waits. The tested version names “these arrangements.” No score had
been measured for that proposal when it was repaired. The protocol now requires
a stated editorial improvement for a proposed final output; a detector gain,
smaller vector distance or synonymous wording cannot supply that evidence.

All selected passages were compared with the original timing rules, equipment
scope, induction exception, conditional consequences, cancellation request and
reason, fee statement, provisional trial, review criteria, uncertainty and
safety requirement. The root, claims and intended tone stayed fixed. The word
distance limit was explicitly raised from 0.40 to 0.75, then to 1.0 because
sentence moves consume Levenshtein distance despite preserving their words.
Earlier exclusions remain recorded. Increasing that search limit did not relax
the editorial requirements.

The spend of the six batches is recorded in the private ledger.
Raw text, author excerpts and provider logs remain ignored, and B annotations
remain excluded from detector training. Parsing,
A inference, builds and checks ran on the Studio. Nine focused Rust tests,
formatting and clippy passed; the offline report command processed all six
batches. No whitepaper strength milestone has been reached.
