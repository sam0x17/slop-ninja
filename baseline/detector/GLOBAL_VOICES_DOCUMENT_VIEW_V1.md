# Preserve article structure before selecting training passages

The [eight-article review](results/global-voices-source-manual-review-v1.json)
found that the old paragraph filter deletes context and joins unrelated remnants.
Before selecting additional human training passages, build a separate Rust
document view over the unchanged cached article HTML. Keep the old extraction,
its source identities and all historical results intact.

Preserve the ordered block structure: narrative paragraphs, headings, blockquotes,
interview turns, lists, preformatted text, captions, figures and embedded objects.
Record structural containment so a multi-paragraph quote remains one quote
region. Include empty media blocks as observations. Keep the exact input HTML
and its hash; decoded text and parser-serialized HTML are separate views, not
exact source byte spans. Report HTML recovery or unsupported structures where
available instead of claiming lossless reconstruction from a DOM alone.

Separate observed form from interpretation. Quotation marks inside a paragraph
are observations that may mark a nickname, term, slogan, title or direct speech.
They must not automatically remove the surrounding paragraph. A blockquote
records a structural region without asserting who wrote or spoke its contents.
Archive bylines, interview labels, quoted speakers, original sources and
translation credits remain distinct evidence. Uncertain roles stay uncertain.

The first output is a review artifact, with no automatic training export. Keep
all blocks and their ordering, then nominate contiguous narrative regions using
explicit boundaries. Never concatenate across a removed region without recording
the gap. An excerpt must be reviewed for missing antecedents, attribution,
qualifications, conclusions and references to absent media. Do not insert bridges,
repair original wording or infer a human-origin label from coherent prose.

Check structural behavior with synthetic cases covering quoted terms within
narration, nested and multi-paragraph quotations, question/answer turns, headings,
captions, inline formatting, empty media, malformed markup and Unicode. Use
contrasts that distinguish shared punctuation from shared attribution. Then run
the eight already selected source works and inspect every resulting block against
the cached HTML. Bind each output to the source and implementation; retain all
old extraction decisions for comparison. This is an exposed development packet,
so it cannot provide an unseen generalization estimate.

All parsing, builds and checks run on the Studio. This phase makes no new source
requests or A/Pangram calls and changes no active training data, splits, labels or
fits. Later source admission must state its historical-evidence standard and
uncertainty, resolve applicable attribution and reuse obligations, and preserve
source-family grouping. A weaker historical proxy must remain labeled as such.
