# Grammar search and granular detector feedback

B should generate a space of explicit word and grammar alternatives. The LLM's
main job is to select and review edits for meaning, requested tone and readability
using the document's context. The statistical model supplies choices and measured
style differences; detector feedback helps allocate the search effort. LLM
proposals remain useful experimental controls, but generation of the search space
should not depend on asking a model to rewrite the passage.

## Grammar and word choices

Represent each state as exact document text plus its syntax and word-occurrence
measurements. An edge records a versioned operation, source spans, preconditions
and the resulting text. Keep the original claims, intended voice and cumulative
revision history across every path. Grammatical validity permits consideration;
it does not establish semantic equivalence.

Build alternatives from syntax operations and observed lexical choices in
compatible grammatical contexts. Fit the target writer's distributions on
independent works, with explicit counts, denominators and uncertainty for sparse
contexts. Compare grammar and word choices together. Matching raw vocabulary
alone can reward topic imitation; copying reference passages is not style transfer.
Requested audience and register still govern how historical samples are used.

The current dependency-based rule catalog supplies ten operation families,
including guarded fronting and complete-clause boundary alternatives that
retain embedded clauses and logical connectors.
Expand it through [corpus inspection](../../grammar/CAMPAIGN.md), checking
actual syntax rather than treating a successful parser call as certification.
It can generate a finite neighborhood without an LLM. It is an initial control,
not a complete English grammar or a learned lexical substitution model. Next,
learn contextual lexical alternatives and expand the rule catalog with explicit
morphology and clause constraints. Retain multiple paths and measure combinations:
a useful sequence need not improve a score after every individual edit.

The first [lexical pool](LEXICAL_POOLS.md) combines WordNet sense membership with
reference word counts and parsed grammatical contexts. Its unigram ranking is a
proposal aid; full grammar measurement and meaning review remain separate. It
supplies a reproducible dictionary baseline for a later learned lexical model.

## Granular A feedback

Investigate token and construction attribution on the released A encoder, then
test the nominated spans with actual replacements. The first gradient experiment
uses [Integrated Gradients](https://arxiv.org/abs/1703.01365) on combined
model/assistance log odds. It records the embedding baseline, token offsets,
numerical integration settings and completeness residual. Attribution relative
to a synthetic embedding baseline is a diagnostic, not an inherent AI label for
a word or a prediction that a particular replacement will help.

Aggregate token evidence onto words and parsed constructions without dropping
overlap or punctuation. Compare attribution-guided choices with reference-distance
ranking, random choices and measured finite-menu effects at the same scoring
budget. Report failures and interactions. Check stability across integration
settings, document context and detector revisions before trusting a ranking.
Attention weights alone are not the proposed attribution method.

The two directions can work together: nominate a span, materialize grammatical
and lexical alternatives, measure the complete resulting context, then let the
LLM review the candidates. The [accelerator path](SCREENING.md) supports wider
exploration; final detector observations still use the reference runtime.

## Text edited by several models

Evaluate controlled editing chains as well as text generated in one pass. Keep
the initial source and each known generator, model revision, instruction, input,
output and accepted edit with their provenance. Group an entire chain in one
dataset split. Unknown history stays unknown; a local attribution map does not
recover model identities or editing chronology.

A document may contain several local patterns, including overlapping edits
within a sentence. Evaluate both local context and the assembled work. The
selector aims for the requested author's voice across that work, while preserving
its arguments and qualifications through the whole revision chain. Model-family
or editing-stage heads are a possible extension to A; the current three-class
detector does not supply them.

Several AI editors still constitute model-only origin when the entire passage
was model-written. A's `mixed` label refers to human/model origin, not several
model families. Preserve that distinction when recording revision chains.
The existing [revision snapshot](../../datasets/detector-revisions-v1/README.md)
provides training examples with successive edits and ancestry. It is not a fresh
evaluation set for this research; compare on separately held-out source families.

Use provenance-cleared generation and editing chains for training experiments.
Private B candidates and source material stay excluded from training releases.
The initial studies are development experiments on existing private material,
with no claim yet of a learned transformation grammar, validated granular A or
general success on mixed-model documents.
