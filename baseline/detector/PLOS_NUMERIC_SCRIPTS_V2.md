# Numeric notation in scientific introductions

The [completed review](results/plos-numeric-scripts-review-v2.json) checked all
four newly rendered introductions, including 49 notation elements and 168
citation labels and target bindings. All four fit A's current context limit.
The subsequent [supplemental admission](results/plos-numeric-scripts-admission-v2.json)
adds four unsplit historical proxies to the earlier 13, for 17 admitted sources
in total. Existing source files, training runs and generation plans are unchanged.

Extend the existing Rust preparation command with an explicit
`--numeric-scripts` option. Revisit the same frozen 33-candidate inventory,
including previous failures. Preserve every disposition. Existing admitted
texts, splits, generation plans and running fits remain unchanged.

The new `jats-prose-numeric-scripts-v2` projection renders ASCII digits and
positive/negative signs inside JATS `sub` and `sup` elements as the corresponding
Unicode subscript or superscript characters. Both hyphen-minus and mathematical
minus map to the script minus. Record every element's exact XML span, source
hash, decoded contents and rendered notation. Preserve surrounding words,
punctuation, citation labels and paragraph order.

Do not flatten scripts into ordinary baseline characters. Reject letters,
spaces, empty scripts and nested markup inside a script. Equations, figures,
tables, lists and other unsupported structures remain whole pending candidates.
This bounded notation policy does not claim to render general mathematics.

Continue verifying the original captured article and section hashes. Check the
inventory's original flattened text hash separately from the new notation-aware
rendering hash. The former verifies correspondence to the frozen inventory;
only the latter identifies the actual text proposed for later use. Running the
command without the option must reproduce the original preparation report.

Before admitting a newly renderable introduction, read its entire rendering
against the complete original section, inspect every notation conversion and
citation binding, check attribution and permissions, and count its complete
length with A's pinned tokenizer. Repeat the declared source-family and text
overlap checks. A successful rendering alone does not admit a record, establish
human authorship or prove scientific correctness.

Run the experiment and required Rust checks on Studio. The four expected
numeric-only cases are a diagnostic expectation from markup inspection, not a
target to reach by changing inclusion rules. Keep unexpected failures visible.
No model inference, generation or paid detector annotation is part of this
preparation. Linux execution requires separate qualification.

The expanded overlap screen retained the earlier 29 corpus inputs and added a
frozen snapshot of the 13 admitted sources plus nine completed generation
responses, including rejected drafts. No new candidate had an existing-work
match or a flagged shared passage; the longest shared run was six normalized
words. The 13 earlier sources correctly matched their existing records and were
excluded from the supplemental write. Later generation responses are outside
this snapshot. The screen cannot establish independence from all writing.

Running without `--numeric-scripts` reproduced the original preparation report
byte for byte. An independent Ruby XML projection reproduced all four new texts,
and the required Rust checks passed on Studio. Fifteen candidates still contain
unsupported notation or structures; the previously identified quotation case
also remains pending. No source text was shortened to obtain admission.
