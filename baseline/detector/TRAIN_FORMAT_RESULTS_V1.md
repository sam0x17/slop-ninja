# Collection formatting predicts origin in the Train archive

All 196 human CMU summaries start with exactly one ASCII space. None of their
932 model-only or 548 mixed-origin descendants has leading whitespace. Removing
that single byte is the entire difference between the raw and existing
collapsed-whitespace views for every human CMU summary.

The [Rust audit](examples/train_format_audit.rs) checked all 3,833 original Train
records against their declared alternative views under the
[fixed protocol](TRAIN_FORMAT_AUDIT_V1.md). It reports 13 literal formatting
features in 23 collection, origin and generator/operation buckets. Exact source
text remains unchanged and outside this report.

| Origin | Records | Leading ASCII space | Contains LF | Contains LF-LF | Changes under whitespace collapse |
| --- | ---: | ---: | ---: | ---: | ---: |
| Human proxy | 471 | 196 | 217 | 72 | 422 |
| Model-only | 2,135 | 0 | 867 | 867 | 867 |
| Mixed | 1,227 | 0 | 645 | 643 | 647 |

Collection layout differs as well. Of 178 human HC3 records, 145 contain LF but
none contains LF-LF. All 53 human Wikinews records contain LF-LF. Human CMU
summaries have no LF. These are literal byte patterns, not parsed paragraph
boundaries. They offer potential collection cues in addition to vocabulary.

The earlier collector's plain-content draft/edit path trims outer whitespace;
human imports retain source bytes. Revision outputs retain the returned text,
yet none of the retained model-only revisions has leading whitespace either.
Thus the observed leading-space association partly reflects collection policy.
It does not establish a linguistic distinction between human and model writing.

The completed [Train-fit diagnostic](TRAIN_FIT_RESULTS_V1.md) already scored both
versions. For the CMU human subset, those measurements now isolate removal of
one leading ASCII space across all 196 records:

| Saved detector | Human NLL, raw / trimmed | Strict-cutoff false positives, raw / trimmed | Loose-cutoff false positives, raw / trimmed |
| --- | ---: | ---: | ---: |
| V6 | 0.01187 / 0.12003 | 0 / 3 | 1 / 17 |
| Base control | 0.05743 / 0.10127 | 0 / 1 | 1 / 2 |
| Base consistency | 0.04790 / 0.11434 | 0 / 1 | 2 / 4 |

These are paired observations of the same source texts under the existing
empirical cutoffs. They demonstrate score sensitivity to that byte on this
exposed Train subset. They do not show that adding a space would make generated
text pass, isolate a particular internal feature, or predict unseen performance.
Both base refits already received raw and collapsed-whitespace exposures, so
adding the same augmentation again is not an untested remedy.

The [aggregate report](results/train-format-audit-v1.json) includes ordinary and
equal-family/origin rates. An
[independent recalculation](results/train-format-audit-check-v1.json) reproduced
all counts and rates, verified input/source bindings, and joined the six saved
CMU diagnostic results. Formatting, all-target tests, Clippy with warnings denied,
the build and audit passed on the Studio. Linux execution is unverified.

The [completed paired probe](BOUNDARY_SPACE_RESULTS_V1.md) independently varied
outer whitespace on human, model-only and mixed examples from all four
collections. It confirmed large V6 changes on some generated passages, with
smaller model-only changes under both refits. A future training
comparison can then remove or balance collection formatting without conflating
that change with a new backbone or expanded corpus. Preserve exact source bytes
and the normalized model view separately. No current fit, label or evaluation
threshold changes from this audit, and no new inference or provider call ran.
