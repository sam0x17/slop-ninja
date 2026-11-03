# Longer source range after the first token audit

The first [long-source cohort](LONG_DOCUMENT_DATA_V1.md) admitted 47 complete
summaries from the fixed 100-source review. Their A-tokenizer lengths are
815-1,324 tokens, with a median of 1,044. No record exceeds the 2,048-token
context limit. They extend the old training range but do not reach the
1,878-token private failure. These are token counts, not detector observations.

This separate cohort changes the lexical-word interval to 1,200-1,600 and the
ranking domain to `slop-ninja-cmu-long-review-v2`. Take the first 100 eligible
rows by SHA256 of `slop-ninja-cmu-long-review-v2|page_id`, with offset zero.
Freeze the selected source IDs and hashes before any retrieval. Do not replace
failed sources or move the offset. The purpose is to measure coverage nearer
A's context limit; the actual uncapped token audit determines that coverage.

Use the same complete CMU TSV input, SHA256
`94516c64d1b4ac6b8b397ef106ee598b43d0e5364ffd8305edc945dce04f4305`.
Exclude the same pre-V6 historical ledger, SHA256
`62a4f08c839fdbc05700a224187933910bfa6cf1d94aabecdb6a367527822e8f`,
and all 351 fresh V6 roots, SHA256
`195a632721f370f84b5ab3bc8c71a5e7f620fdb1c3a4796e04c639cef2887ad4`.
The input has one row per page ID and the two declared word intervals do not
overlap. No first-cohort page can enter the second cohort. Retain the prior
group, exact-text and lexical-normalized exclusion checks.

Keep the first protocol's full literal and rendered historical match, fixed
November 2012 then June 2013 cutoffs, captured grants, notices, quotation screen
and attribution. Preserve each complete summary field byte for byte. Do not
concatenate passages or restore paragraph breaks by guessing. Preserve all
requests, failures and denominators, with at most four sequential Wikipedia
requests per selected source and no automatic retries.

Use separate v2 review and admission schemas, bound to this policy hash.
Records remain unassigned to experiment splits. The uncapped token audit
retains every admitted record and reports any that exceed 2,048 tokens.
Over-limit records are coverage failures for the current runner; they are not
shortened, silently filtered or replaced. Neither cohort changes the active
origin-consistency fits or their evaluation archives.

Before any later generation, scoring or fitting, declare the combined source
allocation and matched model-counterpart design. Human-only source collection
cannot establish detection sensitivity, style independence or agreement with
Pangram. Inspect actual text and syntax and state the review's scope. No new
generation, detector call, paid Pangram request, reference promotion or
whitepaper accuracy milestone is part of this collection.
