# B evaluation for chapters and books

Book-length inference is a product requirement. Local repairs must preserve the
meaning and requested voice of the assembled work. The present notice experiments
do not establish that capability. Neither a successful sentence nor a collection
of independently successful sections establishes a successful book transformation.

## Editing and scoring context

Keep one immutable source document and an ordered revision history. Bind every
edit to exact source bytes, with stable chapter and paragraph identities. Carry
the original claims and requested tone through every revision; a new section
must not become a new semantic baseline that forgets earlier changes.

Separate three kinds of context:

- The local proposal context contains the candidate span and enough surrounding
  prose to resolve its grammar and immediate referents. The model chooses bounded
  replacements for nominated spans; it does not rewrite the entire context.
- The document context retains definitions, names, chronology, qualifications,
  cross-references and the argument's dependencies. A locally equivalent synonym
  can still break a term defined elsewhere. A changed condition can contradict a
  later conclusion. These require checks beyond a local similarity score.
- The author profile describes word and grammar choices across independent
  writing samples. Measure resemblance across the book and by chapter, so a
  closer overall average cannot conceal chapters with incompatible voices.

Preserve quotations, citations, equations and code through format-aware handling.
Do not treat an extraction that drops them as the original work. Begin the
experiment interface with exact plain text; document-format adapters must retain
their source mapping and report unsupported constructs before editing.

The reference A artifact currently accepts at most 2,048 tokenizer tokens and
rejects longer input. It never silently truncates. Long-form orchestration must
retain that identity and limit. An overlapping-window wrapper would be a new
evaluation protocol, not a newly calibrated whole-book detector.

Pangram describes 512-token overlapping inference windows, aligned predictions,
and a conditional random field across the document before final segment labels
and character-weighted fractions. Independent chunk calls therefore do not
reproduce its documented document-level procedure. [Pangram 4 model card](https://www.pangram.com/research/model-card/pangram-4)

## Next benchmark

Use complete works at increasing lengths: multi-paragraph essays, chapters, then
books. Suggested engineering bands are 2,000-5,000, 10,000-30,000 and
60,000-120,000 words. These are coverage targets, not input limits or evaluated
performance claims. Retain whole source works across dataset splits, including
their revisions and excerpts. Keep untouched works for later confirmation.

Freeze source bytes, reference works, intent, context policy and detector
artifacts before each comparison. Compare local proposals evaluated in isolation
with the same proposal budget evaluated in document context. Retain every failed
candidate. Add boundary and distant-dependency cases: a pronoun with its antecedent
in the preceding paragraph, an exception in the following paragraph, a term
defined in an earlier chapter, and a conclusion dependent on an earlier premise.

For supported input sizes, score the complete assembled text directly. For longer
input, create a versioned coverage manifest with source ranges, exact window
hashes and actual tokenizer counts. Use overlapping windows and explicit boundary
probes. Every source range must be covered; an unsupported window is missing
evidence. Preserve all window results, including the worst regions and failures.
Never report the mean of A probabilities as a calibrated book probability or
percentage of AI-written text.

Freeze window placement rules before inspecting scores. Report sensitivity to
predeclared shifted boundaries. Do not choose cuts because they improve detector
results. When the provider supports the full document, obtain its full-document
result as well. Verify actual provider request limits before a paid long-form
run; the current synthetic B planner's 1,000-word cap is our experiment limit,
not evidence of Pangram's maximum supported length.

After editing, reassemble the work and repeat the coverage checks. Recompute
scores wherever the full scoring context changes, including neighboring windows
and section boundaries. Reuse a cached result only when exact input bytes,
detector identity and execution policy match. Keep coverage and context-policy
changes visible in the comparison.

## Quality and cost evidence

Review complete sections and their connections, not just edited sentences. Check
that every argument, qualification and important detail survives and that the
requested tone remains consistent. Keep factual preservation, author resemblance
and concrete editorial improvements separate from detector results. Extraction
checks, embeddings and model reviews are fallible evidence; none certifies that
a book's meaning is unchanged.

Measure time and cost per accepted document alongside total model calls, tokens,
rescoring, revisions and discarded proposals. Compare search strategies at the
same budget. A method that needs dozens of detector calls for every sentence may
not be commercially usable for a book, even if one short example succeeds.

The present short-menu implementation reparses each complete candidate and
computes word edit distance with a quadratic dynamic program. Applying that
unchanged to a book would waste computation. The long-form path needs source-range
edits, reusable paragraph annotations and exact-context score caching. Its edit
accounting must have an explicit identity; changing it must not silently alter
the cumulative distance recorded by earlier experiments.

Paid book jobs need resumable processing, durable encrypted checkpoints and an
explicit completion/coverage report. The confidentiality requirements for B still
apply to every chunk, document summary, glossary, cache and checkpoint. Chunking
does not authorize exposing different parts of a customer's book to additional
miners or relaxing the qualified-runtime requirement.

The [first document protocol](PROTOCOL_DOCUMENT_V1.md) implements coverage and
contextual rescoring. The next step is an equal-budget comparison on complete
multi-paragraph sources. The present local proposal client, short-menu enumeration
and A runner remain components of that experiment. They are not yet a book
transformation pipeline, and no long-form accuracy milestone is claimed.
