# Ordered document view: first article review

The new [Rust HTML view](../../src/html_document.rs) preserves article structure
before any prose selection. It retains the exact input HTML separately from the
recovered DOM, including headings, quotations, captions, media, inline elements,
attributes and decoded text. Each region has an explicit position and parent.
Quoted words no longer cause surrounding narration to disappear from this view.

The [eight-article run](results/global-voices-document-view-run-v1.json) contains
1,623 DOM nodes and 319 observed regions. All 233 paragraphs remain available,
including the 136 paragraphs excluded by the old filter. It also retains 52
blockquotes, seven headings, 11 captions and 15 media elements. Those categories
overlap; a caption can also be a paragraph.

The [manual review](results/global-voices-document-view-review-v1.json) read all
319 region projections against the earlier full source review, then inspected
media attributes and selected inline structures. The new view retains the
context missing from the old concatenations: introductions and following posts,
quoted terms inside narration, a reported arrest's outcome, and all seven
interview answers. Three of those answers contain multiple paragraphs, correcting
an earlier note that counted two. The screenshot between interview turns stays
outside both answers.

This is document-structure evidence. The view does not determine who spoke a
quotation or whether a quoted word is a nickname, slogan or direct speech. It
retains informal spelling, expressive punctuation, hashtags, emoticons and source
errors without claiming to have parsed their English syntax. A decoded-text
concatenation is also not a rendering: line breaks and other inline structure
remain separate nodes and require an explicit rendering policy.

The [binding check](results/global-voices-document-view-check-v1.json) verifies
the saved packet, source and implementation hashes, region topology, text-node
ownership and paragraph coverage. It cannot prove the manual judgments. Exact
HTML is retained alongside the DOM; node indices are not source byte offsets.
Input bytes are bounded before parsing, while node and depth limits are checked
after DOM recovery. The implementation reports limits rather than truncating.

Four focused structural tests cover quoted terms, nested and multi-paragraph
quotations, interview turns, headings, lists, preformatted text, empty media,
Unicode, conditional content, malformed markup and explicit resource limits.
Studio formatting, all-target tests and Clippy with warnings denied passed, as
did the eight-article run. Linux execution remains unverified. No heavy work ran
on the laptop.

The subsequent [contiguous passage review](GLOBAL_VOICES_PASSAGES_RESULTS_V1.md)
materializes explicit boundaries for all eight works and inspects their context.
One interview introduction still needs more context despite successful extraction.
Do not concatenate across omitted regions. The
source texts and full views remain outside Git, and this run admits no training
records. The existing extractor and A inputs are unchanged. No source retrieval,
model call or Pangram call was needed. All eight works are exposed development
examples; this result supplies no unseen accuracy or detector improvement claim.
