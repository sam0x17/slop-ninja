# Fresh source review for v6

The next detector experiment has 351 newly admitted human-proxy works.
All remain unassigned to fitting or confirmation partitions. This review
made no model-generation or detector calls.

| Source | Reviewed | Admitted | Excluded |
| --- | ---: | ---: | ---: |
| PLOS scientific abstracts | 162 | 100 | 62 |
| Wikinews articles | 325 | 100 | 225 |
| CMU book summaries | 200 | 151 | 49 |

The [aggregate](results/source-review-v6.json) includes every exclusion
category and source-window binding. It has SHA256
`ab5452c739798b81a0b2a9ccff5987a13ef0f0e524e60b9eac8458f2078bf076`.
The admitted human corpus has SHA256
`195a632721f370f84b5ab3bc8c71a5e7f620fdb1c3a4796e04c639cef2887ad4`.
Exact text, rights evidence and per-record decisions remain ignored.

Freshness checks found no matches among these works or against 852 works
in 67 historical artifacts. The checks used canonical work identity,
source group, exact text and lexical-normalized text. The historical
union includes 82 works ever assigned to Test, including sources whose
generated Test variants were never scored. Retire that full set from
further fitting. A separate audit found no intersection with the actual
v5 Train, Development or Calibration partitions.

PLOS records carry explicit CC BY licenses, but the current publisher
XML is not proof of immutable historical text. Wikinews records use
historical revisions and retain their provenance and quotation screens.
CMU summaries require complete historical Wikipedia matches and retain
both applicable CC BY-SA source notices. Follow the existing
[ShareAlike policy](SHARE_ALIKE_POLICY.md) for descendants and model releases.
These are historical proxies; admission does not establish unaided
composition, author or topic independence, or absence from model pretraining.

The separate [revision collection](ENCODER_REVISION_DATA_V6.md) uses only
the existing 471 eligible v5 Train families. Its seed archive contains
471 unchanged human/draft/mixed trios selected without detector scores:
163 Qwen drafts, 141 Mistral drafts and 167 OLMo drafts. The seed file has
SHA256 `d0cfb18e850615def38c5854697a2e2088428fd4ef1a6dce92147f68cfd1a764`.
No new model result follows from either source preparation step.
