# B transformation baseline

Current development uses [source edit menus](PROTOCOL_MENU_V1.md): materialize
word and clause alternatives, measure singles and combinations, and select an
exact revision only after reviewing meaning, requested tone and writing quality.
The [interactive harness design](INTERACTIVE_HARNESS.md) describes how the model
drafting a document could use these components through MCP while retaining its
original brief and argument context. The MCP interface is not yet implemented.
The [two search directions](SEARCH_DIRECTIONS.md) pair grammar/statistical
candidate generation with granular A feedback, leaving semantic selection to
the LLM. [Accelerator screening](SCREENING.md) now provides faster exploratory
observations with separate CPU confirmation.
The [word-choice pool](LEXICAL_POOLS.md) now generates dictionary alternatives
with reference frequencies and parsed grammatical contexts. The selector chooses
exact nomination IDs after reviewing their senses; no passage generation is
required. Dictionary membership alone does not establish a suitable replacement.
The [first word-choice comparison](RESULTS_WORD_CHOICE_V1.md) records the noisy
initial pool, selection by sense and measured effects of complete combinations.
The [local proposal protocol](PROTOCOL_PROPOSALS_V1.md) uses a pinned model to
suggest bounded source replacements, with reference examples and an optional
table of grammar and word-frequency measurements.
Its [first comparison](RESULTS_PROPOSALS_V1.md) found reviewed clause repairs
scoring 3.97% and 7.26% on A, but both remained 100% AI-written on Pangram.
Both lexical batches failed the declared span limit.
The [first menu results](RESULTS_MENU_V1.md) reached 4.14% and 6.90% on A;
Pangram still classified both final passages as 100% AI. This remains an
experimental search process, with no demonstrated joint success.
The [direct Pangram experiments](RESULTS_PANGRAM_MENUS_V1.md) show that sentence
order can change detection while leaving the current count vectors unchanged.
Some revisions reached 0% fully AI-written but 100% AI-assisted. Both classes
count as flagged; all 53 new observations still had zero human fraction.
The [sentence-order comparison](RESULTS_ORDER_FEATURES_V1.md) adds optional
position and transition features. Factorizing the sentence patterns changed
edit rankings, but all nine further Pangram observations still had zero human
fraction. No joint detector success has been demonstrated.
The full-rewrite experiment below remains the original comparison baseline.

The [paired detector check](DETECTOR_TRANSFER.md) also found weak transfer on
a complete private document: A fell from 90.39% to 51.89%, while Pangram's
flagged fraction fell from 100% to 96.30%. A is currently insufficient as the
sole guide for B optimization. New A-only minima require external confirmation;
ordinary detector accuracy does not establish reliable revision rankings.

The primary research objective is to lower both A and Pangram scores on the
complete input. Meaning, details and intended tone must survive, and writing
quality must not decline. Matching a requested writer and improving prose for
a stated audience or venue are additional objectives and possible search
directions. Include those requests in the editing brief. Report detector scores,
author fit and editorial judgments separately; progress on one does not establish
progress on the others. These are objectives within B, not additional launch
mechanisms.

The [blog-guidance experiment](BLOG_GUIDANCE.md) tests writer profiles as a way
to prioritize detector queries on an identical edit menu. It compares three
reference-based orders with uniform candidate sampling at the same query budget.

[Long-form evaluation](LONG_FORM.md) is a required next stage: chapters and books
need complete coverage, edits checked in surrounding context, and preservation
and author-voice review across the assembled work. Short-menu results do not
establish book-length capability.
The [document coverage commands](PROTOCOL_DOCUMENT_V1.md) preserve exact text,
prepare two fixed overlapping A views, retain missing evidence, and reuse only
unchanged scoring contexts. This is the first part of that evaluation path.
The [first coverage checks](RESULTS_DOCUMENT_V1.md) prepared a 70,200-word
synthetic input in 6.41 seconds on the Studio, with all 218 windows within A's
limit. That fixture tests coverage, with no book-level accuracy claim.

The first baseline uses a pinned, locally hosted Qwen decoder to revise prose
toward independent style samples. Rust constructs the requests, records exact
inputs and outputs, measures the existing grammar and word-occurrence spaces,
and joins an editorial review with V6 detector results. Python is used only by
the existing spaCy and detector runtimes.

The Rust client targets Linux and macOS. The current MLX generator experiment
is Mac-specific; Linux serving uses a separately pinned compatible endpoint.
See the [platform requirements](../../docs/platform-support.md). Ordinary
execution on either OS does not qualify for confidential B serving.

This starts with prompting, before fitting an adapter. The installed
Qwen3-30B-A3B-Instruct-2507 4-bit checkpoint supplies the first control; it is
not the paper's eventual 8B starter or a newly trained Slop Ninja model.
Compare reference-only prompting with reference plus named grammar/function-word
measurements. Both variants receive the same source, claims, target voice,
independent references, seed and output budget. Neither receives detector
feedback during generation. Retain one output per variant, including failures.

The [first protocol](PROTOCOL_V1.md) freezes the experiment and its limitations.
The [completed comparison](RESULTS_V1.md) found three editorial passes out of
eight, with no reduction in V6 detection scores. All eight improved measured
style distance, including the five meaning failures.
The [fixtures](fixtures/pilot-v1.json) are synthetic, MIT-licensed engineering
examples. They are not human-author training data or hidden benchmark material.
The complete source and review checklist stay beside every revision. Ordinary
Studio execution is research-only and has no attestation or subnet eligibility.

## Commands

Run builds, parsing, generation and scoring on the Mac Studio. From this crate:

Feature extraction uses one persistent parser process and discards each full
annotation after extracting its Rust features. Progress counts go to stderr;
plans are still written after preparation finishes. This change does not add
resumption or change the parser profile. Batch and stream feature outputs matched
on four synthetic inputs under both installed spaCy models on the Studio.

```sh
cargo build --release
target/release/slop_ninja_transformer prepare \
  --fixtures fixtures/pilot-v1.json --model-spec /path/to/pinned-model.json \
  --python /path/to/spacy/python --out /path/to/new-plan
target/release/slop_ninja_transformer generate \
  --plan /path/to/new-plan/plan.json --max-calls 8 --out /path/to/new-run
target/release/slop_ninja_transformer analyze \
  --plan /path/to/new-plan/plan.json --run /path/to/new-run \
  --python /path/to/spacy/python --out /path/to/new-analysis
```

Fill a copy of `reviews.template.json` before opening A scores. Review every
claim, possible unsupported additions, target tone, readability, cleanup and
author fit. A missing judgment remains unresolved; a failed judgment cannot be
overruled by a detector score. Name the reviewer and whether the review was
independent, blinded, human or assisted.

Use the released V6 `encoder/runner/infer.py` with its pinned environment to
score `detector-requests.jsonl`. It accepts only exact text and its hash as ID.
Then join the complete score file and bound review:

```sh
target/release/slop_ninja_transformer report \
  --analysis /path/to/new-analysis/analysis.json \
  --scores /path/to/detector-responses.jsonl --reviews /path/to/reviews.json \
  --out /path/to/new-report.json
```

Existing outputs are never overwritten. Interrupted or ambiguous generation
requires inspection; the client does not automatically retry it. Each run has
a fixed maximum of 32 requests, with the requested call budget covering all
planned candidates. Generated text, logs, plans and weights stay in ignored
`data/` or the dedicated Studio run directory. Commit reviewed aggregate
results and explicitly approved synthetic examples to this private repository.

## Route to a trained baseline

1. Build word, phrase and clause menus using parser rules and contextual
   lexical alternatives. Keep proposal provenance and the unchanged option.
   Compare assistant proposals with the WordNet/reference-count pool. Conditional
   alternative frequencies and context-sensitive sense selection still need to
   be learned.
2. Fit author word-occurrence and grammar profiles on independent works.
   Separate works and authors across training, development and evaluation.
   Measure both individual edits and their combinations in the full passage.
   Preserve the original source, meaning checklist and requested tone across
   rounds, including when the requested tone differs from the source.
3. Train a proposal/ranking policy from reviewed menus and observed contextual
   score changes. An LLM may propose local alternatives and choose menu IDs;
   Rust materializes the selected edits. Keep several search paths when useful
   and test lookahead against greedy selection at the same scoring budget.
4. Use independently reviewed preferences to reject fluent meaning changes,
   copied reference material, worse prose and edits that erase the requested
   voice. Compare a trained policy with the recorded menu baseline on unseen
   sources and authors before claiming a stronger B model.
5. Confirm a frozen version against fresh A artifacts and Pangram under the
   existing cumulative spending cap. Introduce qualified attested execution
   before any claim of emission eligibility or paid confidential serving.

Update the whitepaper when an evaluated milestone changes the demonstrated
baseline strength. Routine prompt changes belong in experiment notes. The first
B strength milestone requires held-out-author evidence, preservation review,
author-fit improvement and both A/Pangram measurements; a low A score on these
synthetic examples is insufficient. Report pass denominators and uncertainty.
