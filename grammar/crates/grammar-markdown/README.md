# Markdown text for grammatical analysis

`render` creates an explicit CommonMark text view in Rust. It retains the complete
original source, its hash, structural regions and a map from every rendered byte
to source text or generated spacing. The parser version and rendering policy
have a separate identity. Existing spaCy adapters and fitted spaces are unchanged.

The initial policy uses pinned `pulldown-cmark` with no optional extensions.
Emphasis delimiters and link syntax are removed from the text view; link labels
remain. Soft line breaks become spaces, hard breaks remain newlines, and block
boundaries become blank lines. Inline and fenced code remain as protected text.
Raw HTML and images are explicitly unsupported. A view with unsupported regions
must not be treated as a complete grammatical account of the document. This
policy does not claim to reproduce browser layout or preserve every rhetorical
effect of formatting.

Call `project` on a rendered token or clause span to obtain its source fragments.
`verbatim_source_range` is available only for contiguous, unprotected source
bytes identical to that rendered span. Decoded entities, normalized code,
generated whitespace and words spanning emphasis delimiters cannot qualify.
A verbatim correspondence establishes offsets; meaning, tone, syntax and Markdown
validity still need checking before applying an edit. The existing syntax IR
continues to describe the rendered text and its offsets.

The renderer uses CommonMark's structural interpretation, including literal
asterisks, escaped punctuation, backtick code and underscore-containing words.
The library does not guess whether an arbitrary input was intended as Markdown.
Choose this view explicitly and use it consistently throughout a fitted space.
Retain raw input for origin comparisons and record the text-view policy with
model artifacts.

Two research examples accept JSON on stdin. Run builds and NLP on the Studio:

```sh
# A JSON array of Markdown strings; no NLP or detector execution.
cargo run -p grammar-markdown --example render_views < sources.json > views.json

# Objects with id and text; compare raw/rendered parses and project every token.
cargo run -p grammar-markdown --example parse_views -- \
  /path/to/python en_core_web_sm < cases.json > comparison.json
```

Store text-bearing inputs and reports outside the repository for private sources.
The comparison refuses unsupported constructs and records full annotations for
manual inspection. Successful parsing or offset projection does not certify
grammatical correctness. A lower detector score is not evidence of better parsing.

`entity_cases` prepares a bounded context-sensitivity diagnostic from the same
`id`/`text` input shape. For every literal `&nbsp;`, it emits variants decoding
that occurrence alone and all occurrences except it, plus raw and fully decoded
controls. It records original UTF-8 byte ranges and preserves duplicate texts
with distinct experimental roles. Every variant must render to the same text;
otherwise preparation fails. Sources with more than sixteen occurrences are
rejected rather than sampled. This is an opt-in diagnostic, not a new input policy.

```sh
cargo run -p grammar-markdown --example entity_cases < sources.json > prepared.json
# Feed prepared.json's cases array to parse_views; retain the complete manifest.
```

The upstream [parser documentation](https://docs.rs/pulldown-cmark/0.13.4/pulldown_cmark/struct.Parser.html)
describes the event iterator and source offsets. The new adapter needs separate
Linux execution qualification before a cross-platform claim.
