# First B baseline result

The eight-request comparison completed on the Mac Studio on 2026-09-13.
**None reduced the V6 detection score.** Three passed the coding assistant's
editorial review; five introduced unsupported claims or requirements. This is
a prompted Qwen control, not a newly trained B adapter.

The [protocol](PROTOCOL_V1.md) fixed two synthetic sources, two synthetic target
profiles and two prompt variants before generation. Each cell produced one
complete candidate, with no retry or detector feedback. The full source, claim
checklist and independent style samples accompanied each request. An editorial
review was recorded and hashed before detector inference. It was assisted,
unblinded and not independently validated by a human reviewer.

| Source / target voice | Prompt | Editorial review | V6 combined origin probability | Style-distance improvement |
| --- | --- | --- | ---: | ---: |
| Workshop / practical | References | Pass | 0.9843 | 0.0430 |
| Workshop / practical | References + vector | Pass | 0.9930 | 0.0562 |
| Offline notes / practical | References | Pass | 0.9911 | 0.0694 |
| Offline notes / practical | References + vector | Fail | 0.9899 | 0.0717 |
| Workshop / measured | References | Fail | 0.9971 | 0.0565 |
| Workshop / measured | References + vector | Fail | 0.9817 | 0.0696 |
| Offline notes / measured | References | Fail | 0.9912 | 0.0299 |
| Offline notes / measured | References + vector | Fail | 0.9865 | 0.0071 |

The unchanged workshop source scored 0.7285; the unchanged offline-notes source
scored 0.8506. V6's frozen stricter detection rule is a score greater than
0.8436493155285434. All eight rewrites exceed it. The workshop source was already
below that cutoff. These scores are `P(model_only) + P(mixed)`, not percentages
of AI-written words. All sources and revisions retain their model-written
production history regardless of the scores.

Every rewrite improved both its measured combined word/grammar distance and
its grammar-only distance. Adding measured guidance improved the distance
gain in three of four paired cells, yet five of the eight outputs failed meaning
preservation. The measured profile also drifted toward bureaucratic language
instead of the intended precise, connected voice. These are concrete failures
of distance-only selection. The synthetic samples provide no estimate of
performance on real authors, and two sources are too few for general claims.

## Preservation failures

The practical offline rewrite with vector guidance invented a side-by-side
display for conflicting notes. The source specified keeping both versions and
letting the user choose, without specifying a display layout.

The measured workshop rewrites introduced reassignment criteria or assurances
that the source did not provide, and broadened the absence of a cancellation
fee into the absence of penalties. One also turned the safety notice into a
more forceful statement than the target voice allowed.

Both measured offline rewrites made prior attachment downloads into an explicit
or manual-download requirement and added behavior for linked attachments.
One added cloud hosting and changed a recommendation to check sync into an
obligation. Fluent explanations and reasonable-sounding details still changed
the product contract.

The three editorial passes retained the operational conditions and fit the
practical voice in this assisted review. Some phrases were looser than the
source, and the review notes preserve that judgment. A pass here is not an
independent preservation certificate.

## Consequence for the next iteration

Use these failures to build preservation controls before fine-tuning: source
claims should authorize each proposed addition, and unsupported implementation
details, strengthened obligations and invented reasons must be rejected.
Compare bounded edits anchored to source spans with another full rewrite.
Keep the unchanged source among the candidates, and allow abstention when
neither editorial requirements nor detector objectives improve.

Training needs reviewed source/reference/brief/revision tuples from permitted
writing, with held-out authors and works. Prefer preservation-approved pairs
for supervised fitting and preference training. A single distance or detector
reward would teach the wrong behavior on this pilot. Do not fit these eight
examples and call their rescoring held-out improvement.

No Pangram requests were made, no three-A panel or attested B runtime was used,
and no full B success is claimed. This result stays in the experiment notes;
it does not meet the whitepaper's next B strength milestone. The
[aggregate evidence](results/prompt-v1.json) retains every cell and review,
generator provenance, timings, full A probabilities and hash bindings. Raw B
outputs, exact response logs and the complete analysis remain in ignored storage.
