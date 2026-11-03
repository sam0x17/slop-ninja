# CFG writer vectors versus A encoder profiles: frozen protocol

Frozen on October 31, 2026, on build-host, before any metric it governs was
computed. This note gives aggregates only. The full protocol and the frozen
constants are in ignored storage
(`data/grammar-private/cfg-vs-encoder-20261031/PROTOCOL.md` and `FROZEN.json`).

| Item | Value |
|---|---|
| Question | Do CFG writer vectors beat, or add to, writer profiles built from the released A encoder (R0: arm `E1aux`, seed 17)? |
| `FROZEN.json` SHA-256 | `4a33a404b8986a5075fa24e51e1f0c5906ef9c5f970b586b28f0bf3b670944af` |
| Code | `origin/main` `3aa51700`; branch commit IDs and their IDs on main are in the [alias map](cfg-vs-encoder-aliases.md) |
| Status | Evaluated once on October 31, 2026, and independently verified. Outcome under the frozen rule: **CFG adds value** (fusion with the encoder on T1 only); CFG does not win clearly. [Results](cfg-vs-encoder-results.md). A version 2 follow-up on the same evaluation authors found a resolved but very small T1 gain from adding CFG to the encoder plus the stylometric control ([follow-up](cfg-vs-encoder-results.md#follow-up-cfg-on-top-of-encoder-plus-stylometry)). |

If CFG does not win clearly, the design may drop component *C* (shared CFG
contributions) and build writer profiles from the encoder instead. Arm C below is
a stylometric control, not that component.

## Populations

**Authors never seen by either system.** These are Train-mining writers with no
document in the A Train split, in R0's trainer bundle, or among the passages
used to derive style voices. Their own documents were not CFG campaign sources.
Both systems still have indirect exposure to these domains: CFG campaign pools
included Blog Authorship, PLOS One and a small FineWeb pilot, and R0 trained on
other authors in every domain (see the
[results](cfg-vs-encoder-results.md#data-and-exposure)). Authors are assigned to
training, evaluation and feasibility-only roles by a fixed hash. Training authors
fit every statistic and choose every configuration; evaluation authors are scored
once.

| Domain (author key) | Evaluation authors (cap) | Training authors (cap) |
|---|---:|---:|
| Stack Exchange (site user) | 300 | 200 |
| CourtListener (judge) | 200 | 150 |
| Project Gutenberg (author, cross-work only) | about 7 | about 5 |
| FOMC transcripts (speaker) | about 16 | about 14 |
| PLOS blogs (writer) | about 18 | about 22 |
| Web blogs (blog host, a weak label) | about 89 | about 83 |

Each author contributes up to 3 queries and 5 references. No query shares a
source group (a thread, opinion, work, meeting or post) with any of its
author's references.

**Sources excluded before measurement:**

- Global Voices: 175 of its 186 multi-document writers are in A Train, and it
  was a CFG campaign source.
- Caselaw Access Project: the same judge can appear under a separate,
  unmatched CourtListener key.
- Single-document-per-writer pools.
- The A evaluation splits.
- One private single-author collection.

The earlier Blog Authorship corpus is not on this host.

**T3 data.** No five-post style-transfer rewrites exist, and no voice-fit
measure exists. The closest data are hosted-model rewrites written in a voice
that a model derived from a single Gutenberg passage. The primary T3
population has 249 target authors and 438 records: 97 full rewrites and 341
mirrors. None of these records is in R0's training bundle. Of the target
authors, 129 have other text in A Train.

## Arms

All arms share one pooling contract: a writer profile is the L2-normalized mean
of standardized reference vectors, and a query is scored by its cosine with the
profile. The evaluator instead averaged unit-normalized references for every arm;
the [results](cfg-vs-encoder-results.md#verification) report this departure and
its effect.

| Arm | Representation | Candidates chosen on training authors |
|---|---|---|
| A | CFG writer vector: document-export forest expectations with the current numeric projection | `leaf_grammar`, `structure`, `all`, and two pool-merged variants |
| B | R0 encoder profile | pooled head representation `h_raw`, shared `z` |
| C | Stylometric control | 203 function words and the 2,000 most frequent character 3-grams |
| D | Score fusion of B and C | logistic fusion fitted on training authors |
| E | Score fusion of A and B | logistic fusion fitted on training authors |

## Tasks

- **T1, verification.** k = 1, 3 and 5 references, scored by AUC, EER and FPR at
  TPR 0.90, with impostors from the same domain. The primary endpoint is pooled
  author-weighted AUC at k = 5.
- **T2, identification.** Per-domain galleries, scored by top-1 and MRR, with a
  rejection threshold fitted on training authors; 20% of evaluation authors are
  withheld from the galleries.
- **T3, author fit.** For each target author, the rewrites toward that author
  should rank above rewrites toward other authors and above their own
  unrewritten sources. The primary endpoint is the mean per-target AUC.

Results are also stratified by domain and query length.

## Decision rule

Intervals are paired author-bootstrap 95% intervals (2,000 replicates). No margin
is tuned.

- **CFG wins clearly** only if A − B lies above zero on both T1 and T3.
- Otherwise, **CFG adds value** if E − B lies above zero on T1 or on T3.
- Otherwise, there is **no evidence for CFG**.

## Feasibility (52 public documents, feasibility-only authors)

| Measure | CFG export | R0 encoder |
|---|---|---|
| Tokens in complete windows | 59.6% (34.0% native) | n/a |
| Tokens in partial / resource-limited windows | 38.1% / 2.3% | n/a |
| Unresolved tokens | 0.10% | n/a |
| Documents with every window complete | 6 of 52 (11.5%) | 52 of 52 supported |
| Time per document, one core | p50 2.79 s, p95 7.56 s | 1.56 s mean (FP64) |
| Throughput per core | 77 tokens/s end to end | 187 tokens/s |
| Peak RSS | about 0.4 GB | about 1.8 GB |
| Failures | 0 | 0 |

A test restricted to fully parsed documents is therefore infeasible. The primary
comparison uses all text, as a deployed CFG writer vector would.

The protocol adds a **coverage-restricted comparison**. Every arm receives the
same text, made only of windows that parsed completely. A document qualifies
with at least 100 tokens of that text. The protocol reports the **coverage
cost** (the primary AUC minus the restricted AUC, per arm) as a result; it does
not enter the decision rule.

The estimated compute for the full run is about 12 CFG core-hours and 7 encoder
core-hours.

## Feature extraction (October 31, 2026)

Features were extracted once for every document the frozen rules select, with
the frozen CFG exporter and R0 runtime identities, by the
`cfg_vs_encoder_features` example in `grammar-a-baseline` (tests use synthetic
text only). Feature files, parse caches and manifests with input hashes stay in
ignored storage. No vocabulary, normalization, profile or task metric was
computed.

| Population | Authors | Documents |
|---|---:|---:|
| T1/T2 evaluation authors | 630 | 5,027 |
| T1/T2 training authors | 474 | 3,782 |
| T3 primary (249 targets, 438 records: 97 full, 341 mirror) | 249 | 1,712 |
| T3 secondary strata (span, R0-trained, Dev-iter) | 942 target-strata | 5,025 |
| All unique documents | | 15,546 |

Every capped domain had enough eligible authors. The eligible counts match the
frozen estimates, and the primary T3 population equals the frozen inventory.

| Arm | Coverage | Failures | Runtime |
|---|---|---|---|
| A, CFG export | 15,546 documents, 4.28M parser tokens; 58.1% of tokens in complete windows (35.0% native), 37.2% partial, 4.7% resource-limited; 0.17% unresolved tokens; 547 documents fully complete | 0 execution, projection or budget failures | 18.5 parse core-hours, 4 workers, 6.6 h wall |
| B, R0 encoder, all text | 15,546 of 15,546 supported, 4.82M tokens, none truncated | 0 unsupported | 13.4 shard-hours, 4 one-thread FP64 shards, 3.4 h wall |
| C, stylometric control, all text | 15,546 documents, 3.62M word tokens | 0 | 7 s, one thread |

**Coverage-restricted inputs (section 7 of the protocol).** 7,327 of 8,809 T1/T2
documents (83.2%) have at least 100 tokens in complete windows. All five
references and at least one query qualify for 293 of 630 evaluation authors
(46.5%) and 226 of 474 training authors (47.7%). B ran on these restricted texts
(7,327 supported, 1.51M tokens, 4.1 shard-hours), and so did C.

Host contention made the run about twice the frozen compute estimate. The
private deviation log records the feature-build clarifications and the
execution record. None of them changes a frozen rule.

## Evaluation (October 31, 2026)

The `cfg_vs_encoder_evaluate` example in `grammar-a-baseline` ran the frozen arms
once (binary SHA-256 `4e4fe0cd…`, commit `25f8cd9c`), after two rehearsals on
training authors only. Training authors chose every configuration, fusion weight
and T2 threshold, and that selection was recorded before any evaluation author or
T3 record was scored. Rule 1 fails: A − B on T1 is −0.034 [−0.047, −0.022]. Rule 2
holds through T1: E − B is +0.027 [+0.022, +0.032]. The outcome is **CFG adds
value**. The [results note](cfg-vs-encoder-results.md) gives every arm and task
with intervals, the verifiers' findings, the limits and what the evidence bears on.
