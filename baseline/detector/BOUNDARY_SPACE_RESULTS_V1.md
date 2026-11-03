# One leading space exposes a V6 formatting weakness

Adding one ASCII space changed a model-only marketing explanation from 96.73%
to 1.30% combined model/mixed probability under V6. The same intervention changed
a mixed-origin novel summary from 96.82% to 4.37%. Every word, punctuation mark
and internal paragraph boundary stayed identical. These are selected extremes;
the complete panel results below show the frequency and size of the changes.

The [fixed probe](BOUNDARY_SPACE_PROBE_V1.md) completed all 756 observations on
the Studio: two exact views of 126 Train records under three frozen detectors.
The panel contains every descendant of 16 previously hash-selected source
families, with 16 human proxies, 71 model-only records and 39 mixed records.
All three runs completed before result interpretation. No request failed or
exceeded the input limit. The original temperatures and cutoffs remained fixed.

| Detector | Mean absolute change, all rows (percentage points) | Mean absolute change, equal family/origin cells | Largest model-only change |
| --- | ---: | ---: | ---: |
| V6 | 7.802 | 7.588 | 95.431 |
| Base control | 0.732 | 1.249 | 0.944 |
| Base consistency | 0.600 | 0.911 | 1.119 |

The two base refits were much less sensitive on the generated passages in this
panel. Their mean absolute model-only changes were 0.146 and 0.150 points,
compared with 3.381 for V6. On mixed records, the corresponding means were
1.031, 1.124 and 18.337 points. Equal family/origin weighting prevents families
with more descendants from dominating the first table's second summary.

At the original operating points, the prefix changed these flagged counts:

| Detector | Model-only, strict (of 71) | Model-only, loose (of 71) | Mixed, strict (of 39) | Mixed, loose (of 39) |
| --- | ---: | ---: | ---: | ---: |
| V6 | 70 to 65 | 71 to 70 | 34 to 28 | 37 to 29 |
| Base control | 71 to 71 | 71 to 71 | 32 to 30 | 36 to 34 |
| Base consistency | 69 to 69 | 71 to 71 | 31 to 29 | 35 to 36 |

All crossings were from flagged to clear except one mixed news digest under
consistency, which rose from 70.18% to 82.21% and crossed its loose cutoff.
No human proxy crossed either cutoff in this panel. Scores still moved: the
largest human change under control was a decrease from 45.03% to 17.69% on a
porpoise-behavior abstract. Under consistency, an awards report increased from
7.30% to 25.54%. Mean signed changes can therefore conceal sensitivity in
opposite directions; the reports retain signed and absolute changes separately.

Collection slices also matter. V6's mean absolute change on mixed records was
18.46 points for CMU, 26.02 for HC3, 11.50 for PLOS and 14.43 for Wikinews.
The largest model-only failure occurred in HC3, although the earlier collection
audit found the leading-space cue in human CMU sources. The effect extends
beyond that collection within this exposed panel.

The [independent recalculation](results/boundary-space-summary-v1.json) verified
every request/response identity, class-label order, full probability distribution,
paired delta, collection/origin summary and cutoff crossing. Complete per-record
observations are retained for [V6](results/boundary-space-v6-v1.json),
[control](results/boundary-space-control-v1.json) and
[consistency](results/boundary-space-consistency-v1.json). The
[manual inspection](results/boundary-space-case-review-v1.json) covers all eight
distinct passages selected by largest absolute change per detector and origin.
The complete trimmed passages were read, and the paired inputs were checked
for the exact one-byte intervention. This review does not establish fidelity
of the earlier generation processes or validate a new grammatical analysis.

This is evidence of a specific input-format vulnerability in V6. The archive's
origin-correlated whitespace supplies a plausible training shortcut; the probe
does not isolate its learned mechanism. The refits improve this diagnostic but
remain worse than V6 on several broader evaluations, so neither is promoted.
All sources are exposed Train families, not fresh confirmation. There were no
Pangram calls, and no result demonstrates transfer to Pangram or better B prose.

After the fixed capacity comparison finishes, a controlled preprocessing
comparison should test removal or balancing of outer-whitespace cues across
origins, preserving exact source bytes separately from model inputs. Repeat
paired checks on source-disjoint material and retain human false-positive,
generator-transfer and document-edit evaluations. Merely repeating the existing
raw/collapsed exposure is insufficient: both base refits already used it. This
result changes no current training recipe, model input contract or threshold.
