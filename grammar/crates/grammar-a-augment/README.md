# grammar-a-augment

Seeded text noise for A that does not see class labels, and the human
form-injection twins of protocol A2. It serves three uses:

- **Augmented training copies.** A configured fraction of bundle rows of
  every origin (human, mixed and model-only alike) gets one noised copy.
  Because both classes receive the same noise at the same rate, no noise type
  can become a class cue.
- **Consistency probes.** Exact repeats and lightly perturbed copies of
  evaluation rows, with a manifest that holds no text. The competitive-A
  evaluator reads that manifest (`a_comp_evaluate --probes`) and reports how
  far P(involvement) moves.
- **Human injection twins** (`gen_form_inject_v1`, H-inj). Two twins of every
  human document of an evaluation split: H-inj-C with generator-typical
  conventions injected, H-inj-L with generator-typical whitespace layout
  injected. The `inject-audit` subcommand computes which variant each class
  injects (value S0-13).

The tool works offline, makes no model or detector calls, and prints only
counts and digests. Its outputs contain prose, so keep them in ignored storage.

```text
slop_ninja_a_augment copies --rows ROWS.jsonl --rate R --seed S [--profile train_v1 | --profile-file P.json] [--split S] --out COPIES.jsonl
slop_ninja_a_augment probes --rows ROWS.jsonl --split S --repeat-rate R --perturb-rate R --seed S [--profile light_v1] --out DIR
slop_ninja_a_augment probes --rows ROWS.jsonl --split S --profile gen_form_inject_v1 --values VALUES-R2.json --out DIR
slop_ninja_a_augment inject-audit --humans TRAIN_DIR [--humans ...] --positives TRAIN_DIR [--positives ...] --out AUDIT.json
```

## Input rows

The input is the assembler's bundle rows (`slop_ninja_a_bundle_row_v1`). Each
row needs:

- `id` and `text`;
- `origin` (`human_only`, `mixed` or `model_only`);
- model-written spans as UTF-8 byte offsets (`ai_byte_spans`, or the aliases
  `spans` and `replacement_spans`);
- optionally `text_sha256` and `share`.

A row is refused when its digest, origin or share disagrees with its spans,
or when a span cuts a word. The word policy is `unicode_split_whitespace_v1`.

## What a copy keeps and changes

`copies` writes only the augmented copies; the original rows are not
repeated. Each copy keeps every field of its row (split, source group,
writer, domain, population, generator labels, style fields, weights) and
changes these:

| Field | Value |
|---|---|
| `id` | `<id>#aug:<profile>:<seed>` |
| `text`, `text_sha256` | the noised text and its digest |
| spans (same key as the input) | the model-written runs, remapped exactly |
| `share` | realised model-word fraction after the edits; 1 for model-only rows |
| `word_count` | recounted, when present |
| `augmented_from`, `augmented_from_text_sha256` | the source row |
| `augmentation` | the per-row record (below) |

A copy stays in its source row's group and split. Duplicate down-weighting
of a row and its copy is the assembler's weighting decision.

`COPIES.summary.json` holds the inputs, the profile and its digest, and a
per-class audit: rows and augmented rows by origin, and edits per 100 human
words and per 100 model words for each operation.

## Noise model `a_noise_v1`

Operations are drawn word by word, at most one per word, in this order. The
"Aimed at" column names the robustness and diagnostic slices each operation
is meant to cover.

| Operation | Effect | Aimed at |
|---|---|---|
| `typo` | transposition, dropped or doubled letter, QWERTY-neighbour substitution | informal web, forum and chat writing, learner essays |
| `ocr` | `rn`/`m`, `cl`/`d`, `li`/`h`, `l`/`1`, `O`/`0`, `e`/`c`, `vv`/`w` confusions | OCR-tier historical text |
| `synonym` | one-word swap from a small list of our own, case kept; no discourse markers | light lexical variation |
| `article_drop` | delete *a*, *an* or *the* | learner essays |
| `word_drop` | delete a word that does not end a sentence or clause | terse informal text |
| `apostrophe` | `don't` to `dont` | informal web and chat |
| `casing` | lowercase an initial, capitalise one, or all-caps a word | informal web, transcripts |
| `punct` | drop a trailing comma, semicolon or colon | spoken transcripts, removed markup |
| `zero_width` | insert U+200B, U+200C, U+200D or U+2060 inside a word | evasion attempts |
| `homoglyph` | replace one Latin letter with a Cyrillic or Greek lookalike | evasion attempts |
| `whitespace` | double space, no-break space, newline and paragraph-break changes between words | transcripts, removed markup |

A profile sets expected edits per 100 *eligible* words for each operation,
where an eligible word is one the operation can apply to. For `whitespace`
the rate is per 100 gaps between kept words. Each row also draws an
intensity, uniform in `[intensity_min, intensity_max]`, which multiplies the
rates. Word edits are capped at `max_edit_fraction` of the row's words, and
gap edits at the same count. When the random pass makes fewer than
`min_edits` word edits, a keyed fallback edits more words using non-deleting
operations, chosen in proportion to their rates. These records report it as
`forced_edits`.

| Profile | Use | Rates per 100 eligible words | Intensity | Cap | Min edits |
|---|---|---|---|---|---|
| `train_v1` | training copies | typo 1.5, ocr 0.3, synonym 0.8, article_drop 8, word_drop 0.4, apostrophe 6, casing 0.6, punct 4, zero_width 0.2, homoglyph 0.1, whitespace 1 | 0.25 to 2 | 12% | 1 |
| `light_v1` | perturbation probes | typo 0.6, casing 0.4, punct 2, zero_width 0.1, whitespace 0.6 | 1 | 3% | 1 |

These rates are declared starting values and have not been tuned. A change of
rates takes a new profile name, and every record carries the profile's
SHA-256. `--profile-file` reads a JSON profile with the same fields.

### Label blindness and exact labels

- **Draws.** Every random draw comes from SHA-256 of the noise model, the
  profile, the seed, the row key and the word or gap index. Whether an
  operation applies depends only on the word's own characters. Labels never
  enter a draw, so a text gets the same edits whether it is labelled human or
  model. The tests check this byte for byte.
- **Row selection.** Rows are picked for a copy or a probe by a keyed draw of
  (seed, row id) below the rate.
- **Run guard.** This is the one rule that reads labels. A deletion is skipped,
  and counted in `deletions_skipped_by_run_guard`, when it would remove the
  last word of a labelled run. A deletion therefore never changes a row's
  origin or empties a model span.
- **Exact spans.** Edits change only the inside of a word or the whitespace
  between words. Model spans are rebuilt as runs of model words; a span that
  covered leading or trailing whitespace keeps covering it. The remapped
  labels are recomputed from the new spans and checked against the carried
  labels before a row is written.

### Per-row record (`slop_ninja_a_augmentation_v1`)

The record holds no text. It gives:

- the noise model, profile and profile SHA-256, the seed, row key and
  intensity;
- the source and output text digests;
- word counts and model-word counts before and after the edits;
- word and gap edit counts, and edits by operation and class;
- the guard skips, forced edits and the edit cap, and whether the cap was
  reached;
- an edit list of `[source word index, operation, variant, class]`. For
  `whitespace`, the class is 1 when the gap lies inside a model span.

## Consistency probes

`probes` reads the rows of one split. It writes these files:

- `probes.jsonl`: the manifest (`slop_ninja_a_consistency_probe_v1`). Each
  line gives the probe id, base id, split, kind (`repeat` or `perturbed`),
  origin, base and probe text digests, profile and digest, seed, and edit
  counts. It holds no text.
- `probe-rows.jsonl`: the probe texts as bundle rows (`id` = probe id,
  `probe_of` = base id), ready for inference.
- `summary.json`.

Repeat and perturbation selections are independent keyed draws.
Perturbations use `light_v1` by default. Their draws are keyed separately
from the draws for training copies.

## Human injection twins (`gen_form_inject_v1`)

Protocol A2 §7.4 gives every Dev-iter-2, Dev-select-2 and Test-2 human two
twins. Each twin applies its group's class variants together, so the twins
stay human documents whose form looks like a generator's.

| Group (kind) | Class | What the twin does with the class variant |
|---|---|---|
| H-inj-C (`h_inj_c`) | `quote` | every quote rendered in it (`curly` or `ascii`), ASCII to curly by context |
| | `dash` | every dash site rendered in it (`em_closed`, `em_spaced`, `en_spaced`, `hyphen_spaced`, `double_closed`, `double_spaced`) |
| | `ellipsis` | every ellipsis site rendered in it (`char` or `dots`) |
| | `emphasis` | one phrase wrapped in the marker pair (`**` or `__`) |
| H-inj-L (`h_inj_l`) | `trailing_whitespace` | the string as the trailing whitespace of a keyed half of the content lines |
| | `separator` | the blank-line string as every paragraph-boundary separator |

§7.4 lists the variants it expects: curly quotes, the closed em dash, the
ellipsis character, paired markdown emphasis, trailing whitespace and
blank-line separators. The R0 Train form census behind that list is in the
round-2 evidence brief (§1.5). The applied variants are the frozen S0-13
value, which `inject-audit` computes. Quotes form one class: double and
single quotes, apostrophes included, are rendered in the same variant.

**Conventions.** Quotes, dash sites and ellipsis sites are those of
`grammar_a_form::conventions` (`cc_era_parent_norm_v1`). Each line, split at
`\n`, is rendered as the child normaliser renders one: its dash and ellipsis
sites first, then its quotes on the result. Numeric ranges are left as they
are. Unlike the normaliser, the twin does not squash lines or move any
whitespace. A tested invariant: a C twin without emphasis equals
`render_quotes(render_sites(line))`, line by line. Two overlapping sites
(spaced dashes one space apart) cannot both be rendered, so the later one is
skipped and counted.

**Emphasis.** The candidates are two consecutive whitespace words on one
line, each made only of letters and digits. The twin takes the candidate
whose first word has the smallest round-2 plan key (protocol value V21),
with x = `h_inj_c_phrase` and d.id = `<row id>:<word index>`. When there is
no candidate, nothing is placed, and the record says so.

**Layout.** Lines, gaps and paragraph boundaries are those of
`grammar_a_form::measure` (`layout_measure_v1`).

- The keyed half is the first `ceil(n / 2)` of the n content lines, in
  ascending order of the V21 key with x = `h_inj_l_line` and
  d.id = `<row id>:<line index>`.
- Each selected line's trailing whitespace, less the `\r` run of a CRLF
  ending, becomes the trailing-whitespace variant, so CRLF endings stay CRLF.
- Every boundary gap (a blank-line run, a sentence break or a line break)
  takes the separator variant. Hard wraps are kept, and so are the head, the
  tail and each line's leading whitespace. In a line-form text (IRC, verse,
  lists) every line break is a boundary, so every line gets the blank-line
  separator; the record counts line-form twins.
- A `\r` run before a newline is a line ending, not layout. After a line
  that ends in `\r`, the separator is written with CRLF endings, so a CRLF
  text keeps CRLF endings.

**Exactness and label blindness.**

- Edits replace or insert form characters only, so the sequence of
  alphanumeric runs never changes. An L twin also keeps the whitespace-word
  sequence byte for byte. A C twin can join words when it renders a spaced
  dash closed, or split them when it renders a closed dash spaced, so its
  whitespace-word count can change. Twins are probes and do not pass through
  the length window.
- Model spans are remapped exactly, and the labels of the alphanumeric runs
  are re-checked after every injection.
- Draws read only the row id and the text. The one label-aware rule is a
  guard: a dash rendering that joins two words is skipped, and counted in
  `span_guard_skips`, when a span boundary lies on it. Human-only and
  model-only labellings of the same text therefore give the same bytes.

**Variants (S0-13, `inject-audit`).** Each input is an assembler Train split
directory (`MANIFEST.json` and `rows.jsonl`). The manifest must name an
unsealed `train` split whose `rows.jsonl` digest and row count match, and the
read stops at the first row that is not a Train row. There is no other input
mode. The positives are the mixed and model-only rows with positive weight
of the `--positives` splits, meaning the raw Train-2 positives. The humans
are the human-only rows with positive weight of the `--humans` splits.
Zero-weight rows (ELLIPSE) count for nothing. One split may serve both
roles.

- A document shows a variant when it holds at least one occurrence:
  - quote glyphs, double and single alike;
  - dash and ellipsis sites;
  - a `**` or `__` pair on one line;
  - a content line's trailing whitespace, less the `\r` run of a CRLF ending;
  - a blank-line run (a gap of two or more newlines) in LF form. Single
    breaks (sentence breaks, line breaks and hard wraps) show no separator
    variant.
- Documents are unweighted, and a mixed positive counts as a whole
  document.
- The score of variant v is `p_v/P − h_v/H`, compared exactly as
  `p_v·H − h_v·P`.
- Per class, the largest positive score wins. Ties go to the larger `p_v`,
  then to the class order in the table above. For the whitespace classes, the
  order is that of the escaped string.
- A class with no positive score gets no variant (`null`), and its twins
  inject nothing for it.

The audit JSON (`slop_ninja_a_form_inject_audit_v1`) holds, for every shown
variant, its counts, shares, difference and exact score numerator, plus the
chosen variants and each input's manifest and rows digests and row counts. Whitespace variants are written
escaped: `\s` space, `\t`, `\r`, `\n`, and `\u{XXXX}` for any other
character.

**Outputs of `probes --profile gen_form_inject_v1`.** The run reads the
variants from the S0-13 entry of `--values`, the round-2 value register
(`VALUES-R2.json`). It refuses a register without that entry, so no twin is
built before the variants are frozen. Every row of `--rows` must be of
`--split`: a row of another split stops the run. It writes:

- `probes.jsonl`: the manifest (`slop_ninja_a_form_inject_probe_v1`). Each
  line gives the probe id (`probe:<kind>:<id>`), base id, split, kind, the
  group's variants and the value register's digest, the base and twin text
  digests, `text_changed`, edits by class, guard and overlap counts, and
  whitespace words before and after. It holds no text. The noise-probe
  reader in `a_comp_evaluate` refuses this schema; the round-2 evaluator
  reads it.
- `probe-rows.jsonl`: the twins as bundle rows, with `id` set to the probe
  id, `probe_of` set to the base id, no spans, share 0, and the
  `form_inject` record (`slop_ninja_a_form_inject_v1`, no text).
- `summary.json`: counts by kind, unchanged twins, edits by kind and class,
  the rows without twins, by origin, and the register's digest and S0-13
  entry.

Only human-only rows get twins. The profile takes no seed, rates or profile
file.

## Attribution

Two ideas follow the MIT-licensed It's AI validator
(<https://github.com/It-s-AI/llm-detection>, Copyright 2024 It's AI):
applying cheap character- and word-level noise (typos, deletions, synonym
swaps, zero-width characters) to both classes, and re-querying a detector on
repeated text. Everything else here is our own, and no code or word lists
were copied: the operation set, rates, profiles, word lists, span remapping,
the run guard, the records and the probe format.

## Tests

`cargo test -p grammar-a-augment` runs on synthetic text only. It checks:

- determinism, and sensitivity to the seed and row key;
- byte-identical edits under human and model labels;
- exact span remapping with deletions, origin preservation and the edit cap;
- the run guard;
- each operation;
- refusal of inconsistent rows;
- class-blind selection near the configured rate;
- equal per-class audit rates;
- probe manifests that hold no text;
- forced minimum edits on short texts;
- profile validation;
- `gen_form_inject_v1`:
  - rendering in each variant, with the word sequence unchanged;
  - line-by-line equality with `render_sites` and then `render_quotes`, over
    every quote, dash and ellipsis variant;
  - overlapping sites;
  - the keyed phrase and the keyed half, each checked against an
    independent computation of the V21 keys;
  - separators that replace boundaries and keep hard wraps, never remove a
    blank line, and keep CRLF endings;
  - trailing whitespace set, not appended, on the selected lines;
  - blank-line runs as the only separator variants (wraps, sentence breaks
    and line breaks show none);
  - exact span remapping, including span edges inside separators and on
    dashes;
  - the joining-dash guard;
  - byte-identical twins under human and model-only labels;
  - the audit's counts, scores, tie rules (whitespace byte order included)
    and empty-variant rule on synthetic rows;
  - the audit's inputs: Train split manifests, the stop at a non-Train row,
    roles, zero weights and duplicate ids;
  - variants read from the value register's S0-13 entry only;
  - twins of one split only;
  - variant validation and escaping;
  - twins of human-only rows only, with text-free manifests and records.
