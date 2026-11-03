# Narrative source expansion

Declared 2026-09-11 after the completed v4 comparison. This stage tests the
admission yield of CMU Book Summaries before selecting another detector recipe.
It makes no new detection claim and does not reopen v4 Test for model selection.

The [CMU collection](https://www.cs.cmu.edu/~dbamman/booksummaries.html) supplies
16,559 Wikipedia-derived plot summaries with book metadata. Its authors'
[paper, section 4.1](https://arxiv.org/html/1305.1319) describes 14,120 summaries
from the November 2, 2012 Wikipedia dump. The staged download has a larger count;
the paper alone therefore cannot date every downloaded row. The captured archive
returned a June 2, 2013 Last-Modified header, which is supporting metadata rather
than definitive composition evidence.

## Fixed historical review

Use the staged `booksummaries.txt` with SHA256
`94516c64d1b4ac6b8b397ef106ee598b43d0e5364ffd8305edc945dce04f4305`.
Require a unique positive Wikipedia page ID and exactly seven TSV columns,
preserving the summary field's bytes. Select complete summaries of 80-500 lexical
words and exclude previously used source families or normalized-text duplicates.
Order the remaining rows by SHA256 of `slop-ninja-cmu-review-v1|page_id`; take
the first 100. Preserve the full selected-ID/hash plan before retrieval.

For each page ID, retrieve the last revision at or before
`2012-11-02T00:00:00Z`. Require the complete summary, modulo whitespace, to occur
both in the literal historical wikitext and in rendered article paragraphs.
The literal check does not expand templates. A current template can therefore
not supply the only evidence of old wording. If this fails or cannot resolve,
try the fixed `2013-06-01T00:00:00Z` cutoff. Stop at the first full match, keeping
both observations when both were attempted. No title-based identity guesses,
partial-summary substitutions or searches driven by detector scores are used.

`resolve_books` archives exact responses, hashes, revision dates and identity,
full-match decisions and potential source-notice terms. It writes each result
as it finishes and a completion summary only at the end. Missing or mismatched
sources remain in the denominator. A completed review exports no training rows.
The page ID identifies the Wikipedia work; the book's author and publication date
never become the summary's author or composition date.

## Rights and eventual admission

The CMU page links specifically to [CC BY-SA 3.0 United States](https://creativecommons.org/licenses/by-sa/3.0/us/legalcode).
Preserve that collection grant alongside the historical Wikipedia text's CC BY-SA
3.0 Unported terms, CMU extraction credit, article URLs and contributor histories.
The US license's section 4(b) permits later versions with the same license
elements for derivatives; CC's [compatibility guidance](https://creativecommons.org/compatible-licenses/)
also describes later BY-SA versions. The existing CC BY-SA 4.0 model-release
choice can accommodate this source while retaining both original notices. This
is the project's distribution policy, not a general legal claim about training.

Admission must recheck the archived bytes and complete historical containment,
screen additional notices and substantial quotations, preserve attribution and
explicit extraction changes, and freeze source-family partitions before model
generation. It must retain every exclusion and all attempted denominators.
The current detector label remains `historical_proxy`; a full source match does
not document human-only composition or independent pretraining novelty.

Narrative summaries broaden the current scientific/news/encyclopedia mixture.
They do not supply personal essays, verified contemporary authorship or genuine
human editing of model drafts. Those remain separate data requirements. No paid
Pangram calls or imported upstream detector labels are part of this review.
