# Longer complete source texts for A

Declared before collecting or scoring this cohort. V6's training median is
237 tokens and its longest training text is 1,050 tokens, despite a 2,048-token
input limit. The private failure that prompted this work is 1,878 tokens.
The running origin-consistency ablation retains the original training lengths.
This separate source collection addresses the missing length coverage without
changing either active fit.

## Fixed source review

Use the existing CMU summary file with SHA256
`94516c64d1b4ac6b8b397ef106ee598b43d0e5364ffd8305edc945dce04f4305`.
Keep complete summaries of 650-1,000 lexical words, preserving the TSV field
byte for byte. Do not concatenate unrelated summaries or trim them into the
requested range. Rank eligible rows by SHA256 of
`slop-ninja-cmu-long-review-v1|page_id` and take the first 100, with offset zero.
This uses a separate review schema and leaves the old 80-500-word policy intact.
The TSV stores each summary on one line. This collection tests length and
narrative content; it does not restore the historical paragraph layout.

Exclude the frozen historical ledger of 852 source works, including retired
Test roots. Its SHA256 is
`62a4f08c839fdbc05700a224187933910bfa6cf1d94aabecdb6a367527822e8f`.
Also exclude all 351 later V6 human roots, including unsuccessful generation
families, from the admitted-fresh archive with SHA256
`195a632721f370f84b5ab3bc8c71a5e7f620fdb1c3a4796e04c639cef2887ad4`.
Check canonical page identity, source group, exact text and lexical-normalized
text. Freeze the selected IDs, source hashes, exclusion hashes and this policy
hash before retrieval. No replacement source or shifted offset follows failure.

Retain the existing full historical match against both literal wikitext and
rendered paragraphs, using the fixed November 2012 then June 2013 cutoffs.
Keep every request, response, failure and source-notice observation. At most
four Wikipedia requests per selected source are needed, with the existing
sequential pacing and no automatic retries. No detector or generation calls
are part of source review.

## Admission and later experiments

Reuse the existing captured collection grant and source notices described in
[CMU source review](CMU_SOURCE_REVIEW.md) and the project's
[ShareAlike policy](SHARE_ALIKE_POLICY.md). The admission extension changes the
declared word range; it retains historical containment, identity, date, notice
and quotation checks. Preserve both collection and original-text attribution.
Keep all exclusions and denominators. A historical match supplies a human-origin
proxy, not documented unaided composition or independence from pretraining.

Admitted records remain unassigned to experiment splits. Do not insert them into
the running fits. Before generation or detector scoring, declare their complete
source-family allocation and counterpart generation profiles. Audit every full
text with the pinned A tokenizer. Retain over-limit documents as coverage
failures; do not silently truncate or replace them. Compare whole documents,
using excerpts only as separately identified diagnostics.

Inspect actual source text and syntax annotations, including long sentences,
dialogue, plot transitions and named characters. Record manual-review coverage
and unresolved problems. Successful parsing and automated source matching do
not certify fluent prose, intended meaning, accurate grammar or an author profile.

This first source collection covers longer narrative summaries. Scientific,
news, personal and book-length documents remain distinct coverage requirements.
Human-only results cannot establish model sensitivity. Model counterparts need
their own recorded provenance and matched length/style design. New Pangram
annotations require a reconciled allowance or new authorization; existing
annotations apply only to their exact original texts. No new Pangram spending,
detector-quality claim or reference promotion follows from collection alone.
