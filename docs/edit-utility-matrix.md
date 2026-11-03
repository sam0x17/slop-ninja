# Edit utility matrix

The [synthetic fixture manifest](../grammar/fixtures/edit-utility-v1/manifest.json)
connects the six existing rewrite rules to measurable edits and explicit
preservation questions. It contains six expected suggestions, six expected
abstentions and four manually supplied semantic counterexamples. Every sentence
and expectation was constructed by an assistant. There are no human readability
labels, corpus excerpts or claims of certified equivalence.

The first execution demonstrates why proximity cannot certify fidelity. Changing
“The client may retry” to “The client must retry” increased the frozen score
against every selected target. Swapping *may* and *must* between the Kim/Lee
clauses preserved both new diagnostic family multisets and all thirty tested
seed/target scores while changing the claims. These counterfactuals were supplied
manually; the rewrite rules did not generate them.

The expectations were written before execution. Keep parser or rule mismatches
in the results. Do not change an expectation merely to make the installed parser
pass it.

## What to execute

1. Validate the manifest and every UTF-8 byte anchor against its source or
   candidate. Record the manifest, parser, extractor and rule versions.
2. Parse each source and run only its listed existing rules. Compare the proposed
   texts with the expected suggestion or abstention. Preserve generated patches,
   failures and unexpected outputs. Counterfactual candidates are supplied
   explicitly; they are not proposals made by the rules.
3. Reparse each candidate. Report old word/grammar measurements and the new
   function-word role and predicate-operator observations separately. Preserve
   source-aligned heads and operator attachments alongside aggregate counts.
4. Report each declared protected expectation with the corresponding source and
   candidate observations. These are review prompts, not automatic semantic
   judgments. Leave readability and tone unjudged. Keep no-change available.

Each case has `source_text`, `rule_expectations`, an optional
`explicit_candidate`, and `protected_expectations`. An anchor contains
`start_byte`, `end_byte` and `expected`. Candidate anchors address the single
expected suggestion or explicit candidate; abstention cases have none.
`expected_change` is `unchanged`, `changed` or `requires_review`.

## What the features can establish

Contraction and expansion should preserve the intended local negative clause,
while surface word coordinates record their different forms. Normalized
function lemmas and operator keys may remain equal. Measure the installed
parser's behavior instead of assuming that equivalence.

Sentence boundary edits can change ROOT/conj labels while retaining the clauses.
Raw feature-key equality is therefore unsuitable as a universal preservation
check. Passive-relative reduction removes an explicit finite carrier; the fixture
marks temporal interpretation for review even when the rule licenses deletion.

The counterexample “Kim may leave. Lee must stay.” becomes “Kim must leave.
Lee may stay.” The modal inventory stays the same while each claim changes.
Even equality of the complete operator-family multiset would not establish
fidelity. Source-aligned predicate, argument and operator records address a
different question from an author's aggregate style profile.

## Optional scoring with the frozen coordinate model

The existing
[`Metric::score_profiles`](../grammar/crates/grammar-eval/src/coordinate_inference.rs)
accepts two `grammar_eval::Profile` values and returns a `PairScore` containing
`logit`, `adjusted_family_distances` and `minimum_unselected_tail`.
Load and validate the portable `Artifact` with `Metric::new` once. Bind its hash,
source-space identity, parser and feature schema before supplying profiles;
bare profiles contain no provenance.

Use `grammar_eval::document_values` on the unchanged fourteen-family extraction.
Supply the same frozen target profile for both texts:

```rust
let source = grammar_eval::document_values(&source_legacy14)?;
let candidate = grammar_eval::document_values(&candidate_legacy14)?;
let before = metric.score_profiles(&source, &target)?;
let after = metric.score_profiles(&candidate, &target)?;
let proximity_improvement = after.logit - before.logit;
```

A positive difference means greater proximity under that fixed metric. Preserve
the scores for all three frozen seeds separately if using the scale-study
models. No model weights, numerical scales, vocabulary support or target profile
may change between source and candidate. New function-role/operator families
are diagnostic outputs and must not be appended to this fourteen-family model.

Short fixtures can lack opportunities for an existing feature family. Report
them as unscorable. A separately declared evaluation may place every variant in
the same fixed context, with its own hashes and shifted anchors. Do not silently
add context, fill missing values with zero, or refit the geometry.

Target proximity supplies a candidate ranking. The matrix evaluates rule
coverage, observable changes and preservation questions separately. Readability
and tonal usefulness still require judgments from readers who have not seen the
distance scores.

## Completed execution

The Rust
[`slop_ninja-edit-utility`](../grammar/crates/grammar-eval/src/bin/slop_ninja-edit-utility.rs)
harness parsed 48 distinct texts representing 26 case-variants in two contexts.
All 26 raw variants and all 26 variants with the predeclared appended context
had the required original-family opportunities and were scored. No parse failed,
and no missing family was filled with zero.

Eleven of twelve rule expectations matched: five expected suggestions and all
six abstentions. The semicolon-split case produced no suggestion. For “The
service reads records; users check results,” spaCy attached *reads* as `ccomp`
under the root *check*. The unchanged scope guard therefore rejected the edit.
The expected sentence split remains in the report as an explicit hypothetical
candidate, with its origin distinguished from generated candidates.

The target set was fixed before execution: the first ten lexicographically
sorted original TRAIN authors, using all 151 of their eligible TRAIN posts.
Each profile averages posts within dates, then averages dates. The three frozen
3,557-coordinate models supply thirty seed/target comparisons per variant.
These are correlated measurements against a fixed target set, not thirty
independent editing trials.

| Explicit counterfactual | Raw: closer / equal / farther | Appended context: closer / equal / farther |
| --- | ---: | ---: |
| Change *may retry* to *must retry* | 30 / 0 / 0 | 30 / 0 / 0 |
| Delete *does not* and assert *retries* | 0 / 0 / 30 | 0 / 0 / 30 |
| Swap the modals assigned to Kim and Lee | 0 / 30 / 0 | 0 / 30 / 0 |
| Move negation from reporting into the reported clause | 9 / 0 / 21 | 9 / 0 / 21 |

Every seed/target logit and candidate-minus-source difference is retained. For
the *may* to *must* counterfactual, raw differences range from +0.7343 to +8.6118;
with appended context, they range from +0.5268 to +1.9673. These are model-logit
differences, not probabilities or comparable estimates of writing quality.

The modal swap changes the original word-bigram counts, although both new
diagnostic families and all thirty frozen target scores remain exactly equal
in each context. Moving negation into the reported clause leaves function-role
counts equal but changes the predicate-operator family. Source-aligned
observations preserve the association that aggregate counts can lose.

Contraction and expansion preserve both new families in this installed parser,
while changing surface word counts. Sentence boundary edits change the operator
family. The expected passive-relative reduction is generated and removes the
explicit finite carrier, retaining its declared temporal-review obligation.
No candidate receives a semantic-equivalence, readability or tone certificate.

The ignored run artifacts are under
`data/author-corpora/blog-authorship-2004/edit-utility-v1/`. The protocol was frozen
before target-cache reads or edit outcomes. `run-v2/report.json` has SHA256
`46dc3afb37df74e734c32649bb13d0b7ca5c899249166313423df69f0082007d`.
The first attempt stopped before fixture parsing because a cached release
dependency retained a bridge path from before the repository rename. Its binary,
log and target audit remain in place. Rebuilding that dependency corrected the
path; the successful run used identical source and protocol bytes.

With those local frozen inputs present, reproduce into a new directory:

```sh
cargo build --release --manifest-path grammar/Cargo.toml \
  -p grammar-eval --bin slop_ninja-edit-utility
grammar/target/release/slop_ninja-edit-utility \
  --protocol data/author-corpora/blog-authorship-2004/edit-utility-v1/protocol.json \
  --expected-protocol-sha256 e05adc3b0cc75248dd65be5a6ee3712d7599ff792facddd347156985a0fb3d3f \
  --out data/author-corpora/blog-authorship-2004/edit-utility-v1/run-new
```

Twenty-seven focused tests cover the harness, scoring adapter, inherited metric
and new feature modules, including installed spaCy probes. Target Clippy passes.
An independent check reproduced the saved rankings and deltas for all 1,560
score entries, checked all 48 annotations and replayed twelve feature
extractions. It also verified the ten target profiles and their 151 cache files.
The full Rust workspace passes formatting, 243 test invocations and Clippy with
warnings denied. Shared modules are tested in several binary targets; eleven
test invocations exercise the installed parser.
No LLM, detector or parameter update was used.
