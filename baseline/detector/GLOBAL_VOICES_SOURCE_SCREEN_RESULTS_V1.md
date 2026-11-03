# Global Voices cache-screen result

The [Rust screen](examples/global_voices_archive_screen.rs) verifies the unchanged
saved publisher responses and applies the [declared protocol](GLOBAL_VOICES_SOURCE_SCREEN_V1.md).
It identifies 174 works from seven author accounts for further source review.
None has entered the detector's training data through this experiment.

| Metadata check | Distinct works |
| --- | ---: |
| Entire saved collection | 820 |
| Publication before 2022-11-01 | 264 |
| Publication and modification before that cutoff | 253 |
| Also publication no later than modification, with neither after retrieval | 250 |
| Also passes the previous extraction exclusions | 174 |

The 820 works occur in 840 account/article observations. Verification checked
all 62 cached responses, exact article joins, content and text hashes, date
fields, and archive/license evidence bindings. The old extraction rules leave
456 observations eligible before this date screen. Their exclusions overlap and
include translation or writer-credit concerns, origin notices, duplicate text
and retained length. Passing those rules does not establish editorial coherence
or authorship.

There are 177 observations across 169 works whose modification timestamp precedes
publication. Such ordering can reflect scheduled publication. The first packet
conservatively excludes them while retaining the flags for investigation.
An initial report applied that ordering check before counting raw date matches;
the corrected report separates the raw and ordered counts. The selected eight
records are byte-identical across both runs.

The [report](results/global-voices-source-screen-v1.json) preserves aggregate
counts, source identities, exclusions, flags and all input bindings. A separate
[recomputation on the Studio](results/global-voices-source-screen-check-v1.json)
verified the joins, counts and deterministic eight-record selection. Of the 174
preliminary candidates, 79 appeared in the earlier author-transfer experiment.
That does not qualify its Blog-trained weights, vocabulary or normalization for
the distributable detector.

The [manual extraction review](results/global-voices-source-manual-review-v1.json)
has now read all eight cached article bodies, the retained and removed prose,
and the corresponding archive cards. Every selected extract loses context: an
interview retains all questions but no answers; another report loses the paragraph
describing the release of arrested students; quoted terms cause whole narrative
paragraphs to disappear. Introductions to absent quotations and media also remain.
The [review binding check](results/global-voices-source-manual-review-check-v1.json)
verifies source identities, selection order and referenced paragraph dispositions.
It does not prove the assistant's semantic judgments.

The original articles remain candidates. The damaged concatenations cannot serve
as coherent whole-article samples. The new
[ordered document view](GLOBAL_VOICES_DOCUMENT_VIEW_RESULTS_V1.md) now retains
the source regions and has a complete structural review of this eight-work packet.
Attribution and explicit, reviewed passage boundaries remain the next step.
Article-level bylines, quoted speakers and translated or embedded material need
separate records. None of this review assigns a new A origin label.

Commercial admission and independent historical-evidence review remain pending.
Publisher timestamps do not establish that the exact present-day bytes existed
at the stated publication date. The publisher's editorial policy likewise does
not certify individual human authorship. The selected article text stays outside
Git. No live article page, linked report or embedded media was retrieved; item
notices visible in the cached bodies were inspected, with remaining limits saved
in the review.

Studio formatting, all-target Rust tests and Clippy with warnings denied passed.
The screen made no new source requests, model calls or Pangram calls. It changed
no splits, fitted models or active training records. Linux execution of this
screen remains unverified.
