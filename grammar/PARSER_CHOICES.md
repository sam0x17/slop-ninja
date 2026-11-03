# Explicit parser choices

The default remains `en_core_web_sm`. To compare another installed English
pipeline, use the explicit Rust adapter configuration or CLI option:

```sh
unslop-grammar --python /path/to/parser/python \
  --parser-model en_core_web_trf annotate /path/to/source.txt \
  --out /path/to/new-annotation.json
```

The same choice applies to `features` and `neighborhood`, including reference
samples and candidate reparses. `grammar_spacy::Parser { python, model }` exposes
single and batch parsing. Existing free functions preserve their default model.
The adapter checks exact text, converts Unicode offsets to UTF-8 bytes and
validates the resulting document as before. The identity records the actual
pipeline version and asset fingerprint. No silent model fallback occurs.

B menu inputs accept an optional `parser_model` string. Menu measurements,
grammar generation, lexical pools, proposal guidance and order audits retain
that choice. A continued walk binds both the requested model and the observed
parser identity. To continue an older selection that lacks the identity field,
reselect its original frozen plan with its existing review; the selection
command takes the identity from that plan. Changing the parser requires a
separate experiment with all references and candidates parsed consistently.
Distances from different parser spaces are not directly comparable.

## Paragraph profiles

The streaming adapter offers three explicit choices:

| Rust method | Coverage/review CLI option | NLP input |
| --- | --- | --- |
| `stream()` | Neither option | Whole source |
| `stream_with_paragraph_boundaries()` | `--paragraph-boundaries` | Independent blocks, including their edge whitespace |
| `stream_with_paragraph_content()` | `--paragraph-content` | Independent blocks with edge whitespace omitted from NLP |

The last two bind `paragraph-documents-v1` and `paragraph-content-v2`,
respectively, in the parser identity. They are mutually exclusive CLI options.
The experimental v2 profile retains the exact document text and maps token
offsets back to that source. Blank-line separators and other whitespace at a
block's edges remain unannotated gaps. It does not normalize internal whitespace,
repair headings or infer missing grammatical relations.

The choice currently applies to `slop_ninja_grammar_coverage` and
`slop_ninja_syntax_check`. Existing single/batch adapters, `unslop-grammar` and
B menu configurations retain their previous behavior. Refit reference spaces
and reparse candidates consistently before using a different profile for B.
Even when word attachments match, removing layout sentences changes the
denominators of sentence-based features.

The [inspection report](CAMPAIGN.md) records
the v2 comparison and its remaining failures. These checks ran on macOS arm64;
Linux execution of the new profile remains unverified.

## Transformer comparison environment

The optional [macOS lock](../requirements-parser-trf-macos.lock) records an
isolated Python 3.12.14 environment tested on the Studio. Install it into its
own virtual environment; the detector environment remains independently pinned.
Run parsing and installation on the Studio. The transformer pipeline uses
PyTorch, and can cost more than the small pipeline even when no generation is
involved. The experiment used CPU execution with two OpenMP/BLAS threads.
Linux execution of this environment remains to be checked.

The official [en_core_web_trf 3.8.0 release](https://github.com/explosion/spacy-models/releases/tag/en_core_web_trf-3.8.0)
describes a RoBERTa-based pipeline and an MIT license. Its wheel hash is
`272a31e9d8530d1e075351d30a462d7e80e31da23574f1b274e200f3fff35bf5`,
verified during installation. The observed parser asset fingerprint was
`d92fc1f65e76182fcfaac41f3fffa2e38d422a934dc69fb479ab18b659db6bfd`.
Retain release notices and exact installed versions with experiment evidence.

## What a stronger parser changes

An initial private comparison corrected useful POS and subject/predicate
attachments in complex prose. It also retained several `ccomp` links between
clauses separated by semicolons. A dependency label alone therefore cannot
decide whether splitting the punctuation preserves a reporting relationship.
Inspect complete clause spans and retain an explicit meaning review.

This was an inspected development example, without gold syntax labels or a
held-out accuracy estimate. A changed parse is not automatically a better one,
and better syntax annotations do not establish a detector or writing milestone.
