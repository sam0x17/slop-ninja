# What the human training sample contains

The current human Train pool already contains examples of promotional, formulaic
and awkward writing resembling the registers observed in recent Test errors.
Their presence does not establish that A learned to handle them. The complete
saved-artifact Train-fit comparison is still needed to distinguish fit from
generalization problems.

The [inspection protocol](TRAIN_REGISTER_REVIEW_V1.md) selects four human roots
from each of the four collections by a fixed hash ranking. The Rust preparer
validated the original 3,833 records and their ancestry, then froze all 16
complete source records. A separate implementation reproduced every selection
and checked each record against the unchanged archive. No detector scores
entered selection or this reading.

I read all 16 texts with their stored provenance. The
[review](results/train-register-review-v1.json) records realized register,
audience, discourse, voice and source defects for each one.

| Collection | Observed in the four selected texts |
| --- | --- |
| Historical book summaries | Promotional suspense questions, plain chronological narration, humorous judgments, long explanations and broken quote punctuation |
| Historical Wikipedia leads | Repeated definitions and topic names, broad importance or popularity claims, technical acronyms and introductory exposition |
| PLOS abstracts | Conventional research-gap framing, dense terminology, numeric results, three first-person plural research voices, explicit limitations and expansive significance claims |
| Wikinews | Entertainment and science recaps, civic analysis, relative dates, typographical defects, and an editorial digest with several article excerpts |

These observations establish that those examples exist in Train. Four texts per
collection cannot establish their prevalence or rule out other registers. None
of the sample is a personal letter or diary; that observation does not establish
absence from the whole pool. All 16 retain `historical_proxy` labels. The PLOS
records also state that historical byte identity is unverified. These are not
documented human-only workflows or independent gold style annotations.

## The digest and source-family boundaries

The selected Wikinews digest contains eight headline/dateline/excerpt units and
editorial status labels. Its stored `news_reporting` description is broader than
a single coherent news article. A compilation's page ID also need not capture
all relationships among the underlying stories.

The [follow-up protocol](TRAIN_DIGEST_OVERLAP_V1.md) compares that exact digest
with all 5,463 records in the four original V6 shards, including generated
descendants. The [results](results/train-digest-overlap-v1.json) retain every
maximal shared run of at least eight normalized words. Seven matching records
belong to the digest's own family, including the human source. One other Train
digest shares a 12-word headline. There are no qualifying matches in Development,
Calibration or Test, and no cross-family run reaches 20 words.

I read the second digest in full. The shared headline introduces excerpts about
different reported actions and dates. This warrants preserving compilation and
story relationships in future data work; it does not establish cross-split
contamination or justify automatic merging. The negative search is limited to
this digest, these archives and exact normalized runs. It cannot establish
corpus-wide independence or rule out paraphrased overlap.

An [independent check](results/train-digest-independent-check-v1.json) reproduced
all eight matching records and every word range using a separate window-coalescing
algorithm. The original protocol described word normalization too broadly. Its
clarified version specifies NFC, internal apostrophes and combining marks; the
second execution retains identical observations with the corrected protocol hash.
Both runs remain saved on the Studio.

## Consequence for A

Do not explain the observed false positives solely by an absence of formulaic
or promotional human examples. Check saved-model fit on the full Train archive,
then compare its errors with the existing source-held-out results. This small
reading supplies neither causal feature attribution nor a reason to change the
running larger-model recipes. It also does not establish enough diversity in
personal voice, generator families or long texts.

Studio formatting, all-target tests, Clippy with warnings denied and release
builds passed. The [completion record](results/train-register-completion-v1.json)
binds the input packet, reviews, source files, executables and checks as they
were at completion. `src/rights.rs` has changed since (2026-11-02: a shorter
attribution-manifest policy string), so its current digest differs from the
recorded one. Linux execution remains unverified. No inference, provider call, relabeling, source
removal, split change or training admission occurred in this inspection.
