# B as an interactive editing harness

The model drafting a document should be able to use B throughout revision. It
can retain the author's brief, discussion and argument context, propose bounded
word and clause alternatives, and inspect their measured effects before choosing
an edit. B supplies the grammar and word-occurrence measurements, materializes
combinations, and records detector observations and editorial decisions.

The intended division of work is developed in [SEARCH_DIRECTIONS.md](SEARCH_DIRECTIONS.md):
grammar and statistical models generate the alternatives, while the LLM mainly
selects and reviews them for preserved meaning and requested voice. Granular A
feedback is a complementary research direction. Model-proposed alternatives in
the current experiments are controls for that longer-term design.

An MCP server is a useful interface for this workflow. MCP exposes tools and
context to a host application; the host determines how its model uses that
context. The drafting conversation does not automatically become available to
the server. The client must supply the relevant brief and document context.
See the [MCP architecture](https://modelcontextprotocol.io/docs/learn/architecture).

This is a proposed interface around the existing Rust CLI. No MCP server or
validated interactive advantage has been implemented. The current B baseline
remains an experimental editing process with no newly trained B weights.

## The editing loop

1. Freeze the original document, requested voice and audience, meaning checklist,
   reference works, detector identities and budgets. Retain the drafting context
   needed to interpret defined terms, qualifications and distant references.
   Historical samples inform the author profile; the requested register governs
   this document. An academic sample does not require academic phrasing in a book
   for general readers.
2. B generates exact alternatives from grammar rules and contextual word-choice
   statistics. The writing model reviews these choices using the brief and may
   reject an alternative, request another neighborhood or keep the source.
   Model-proposed replacements remain available as an experimental comparison
   and a way to identify gaps in the grammar. An unchanged option remains
   available at every round.
3. B materializes compatible combinations and measures the resulting text. Keep
   several paths, including edits that initially leave detection unchanged or
   increase it. Report observed conditional effects; no measurement establishes
   the best edit outside the evaluated menu and depth.
4. The writing model examines complete candidates using the original brief and
   document context. It records gains, regressions and unresolved questions,
   including interactions between individually acceptable edits. A useful
   research candidate can still need correction before adoption.
5. Accept an exact reviewed revision or continue from an existing state. Keep
   the original meaning and requested tone across rounds. Rollback creates a
   recorded decision; earlier observations and failed proposals remain intact.
6. Reassemble the document and evaluate the accepted text in context. Use paid
   Pangram confirmation within the existing budget after freezing a finalist.
   Excerpt success does not establish complete-document success.

The same model with the original conversation may make better editorial choices
than a model given only a passage. That is a hypothesis about access to context.
Model identity alone provides no memory of the original drafting session, and
authoring a passage does not make a model's self-review independent validation.

## Proposed tool operations

Names below describe the intended contract, not installed tools. Keep one Rust
implementation behind CLI and MCP adapters. Store document and revision state
explicitly, independently of the transport connection.

| Operation | Result | Existing component and remaining work |
| --- | --- | --- |
| Open a revision session | Immutable root, brief, reference and budget bindings; revision ID | Menu walk bindings exist; session and document-context storage need an adapter. |
| Inspect style and context | Named grammar/word measurements, reference differences and exact scoring coverage | Menu measurements and document coverage exist; selective retrieval remains to be built. |
| Evaluate an edit menu | Candidate IDs, exact diffs, conditional score effects, style distances and missing evidence | `menu prepare`, pinned A inference and `menu rank` exist; bounded job scheduling needs an adapter. |
| Record review and choose | Exact reviewed revision, reasons, unresolved checks and unchanged/rollback decisions | `menu select` records positive selection; review history, rejection and rollback need a session interface. |
| Confirm a document | Full coverage report and separately identified A/Pangram evidence | Document and external-report commands exist; production confidential routing remains required. |

Long operations should return a job handle with progress and resumable results.
Use expected revision hashes to reject stale edits. Bind retries to the same job
and exact request, so a reconnect cannot duplicate paid calls. Return a compact
summary first, with candidate text and detailed observations available by ID.

Score reductions, style resemblance and editorial judgments stay separate in
the response. Existing selection requires meaning, requested tone, readability,
cleanup and author fit to pass. A promising but unresolved candidate remains
available for repair, without being presented as accepted output. The original
author's informed assessment is useful feedback; record its provenance and
which scores the reviewer saw.

## Editorial calibration

Review logical relations as part of meaning. Adding a causal or inferential
connective can create a claim even when every neighboring factual sentence
survives. For example, in this synthetic pair, changing "The report describes
the process. Its scope includes all branches" to "The report is descriptive,
therefore it applies to all branches" invents a dependency.

Review access and voice at the requested register. Unnecessary negative
constructions, repeated directions, formal substitutions and impersonal phrasing
can worsen an otherwise faithful revision. Sentence splitting and clearer
locations or referents can help. These are contextual editorial judgments,
not universal bans on negation, passive voice, technical terms or inanimate
subjects. Repeatedly adding an explicit author can also make prose awkward.

A detector result remains valid evidence when the candidate needs editing.
Record partial gains and specific repairs without treating the entire approach
as a failure. Final adoption still requires correction of meaning changes and
quality regressions. A neutral word swap can help a search path, but it supplies
no evidence by itself of improved writing.

## Experiment and deployment boundaries

Compare a context-limited editor, an editor given the original brief and document
context, and the original drafting session using this harness. Keep source works,
references, model configuration where applicable, proposal count, detector calls
and time/cost budgets comparable. Record differences in available context and
model identity. Review outputs independently and without scores where feasible.
Measure preserved meaning, requested voice, editorial gains and complete-text
detection together; include rejected candidates and total cost per accepted work.
The first trial can replay the existing CLI without building a server first.

An MCP interface adds no confidentiality guarantee. The chosen drafting provider
can see whatever its client sends, and remote B work still requires the paper's
qualified attested runtime for confidential serving or emissions. Authorize
Pangram disclosure separately under the existing request and budget policy.
Document summaries, references, logs and checkpoints receive the same protection
as source text. Local development keeps ML/NLP and scoring on the Studio; the
interface must support Linux and macOS without relying on an MLX-only backend.

This integration does not change the subnet's A/B mechanisms or establish a
baseline milestone. The long-form requirements in [LONG_FORM.md](LONG_FORM.md)
still govern context, coverage, claim preservation and evaluation.
