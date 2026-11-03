# grammar-a-assemble

Builds A's training and evaluation bundles and checks the consolidation of
the campaign data layout. All corpus text stays in ignored storage; the tool
prints only counts and digests.

```text
slop_ninja_a_assemble build --config CONFIG.json --out ROOT --commit SHA
slop_ninja_a_assemble merge-eval-humans --part DIR --part DIR... --out NEW_DIR
slop_ninja_a_assemble verify-lines --base FILE --successor FILE
slop_ninja_a_assemble pools-index --spec SPEC.json --out INDEX.json
slop_ninja_a_assemble verify-frontier --old OLD_EXPORT --new NEW_EXPORT
slop_ninja_a_assemble trainer-bundle --build BUILD_DIR --split train --split dev_iter...
    [--take SPLIT:KIND=N...] [--seed S] [--smoke-carve-dev-iter K/M]
    [--unseal-for final_selection|calibration_fit] [--weighting final|cohort-factor] --commit SHA --out NEW_DIR
slop_ninja_a_assemble freeze-styles --config FREEZE.json --out FROZEN.jsonl --commit SHA
slop_ninja_a_assemble splits-successor --config SUCCESSOR.json --out NEW_SPLITS_DIR --commit SHA
slop_ninja_a_assemble ocr-profile --base SPLITS_DIR --lexicon LEXICON --domain D...
slop_ninja_a_assemble supervision --train BUILD_DIR/train --config SUPERVISION.json --commit SHA --out TARGETS.json
slop_ninja_a_assemble ess --train BUILD_DIR/train --commit SHA
```

## Bundles

`build` writes `ROOT/build-<id>/<split>/` for each configured split (`train`,
`dev-iter`, `dev-select`, `cal`; Test is refused) and points
`ROOT/CURRENT.json` at the build. The build id is a digest of the
configuration, every input's SHA-256 and the assembler binary, so a rerun on
unchanged inputs reuses the build, and new positives give a new one. Each
split directory holds:

- `rows.jsonl`: one row per document, sorted by id (schema
  `slop_ninja_a_bundle_row_v1`, below; `slop_ninja_a_bundle_row_v2` in a
  round-2 build);
- `weights.json`: the declared weighting with per-cohort and per-label counts;
- `exclusions.jsonl`: every dropped candidate with its reason;
- `MANIFEST.json`: inputs with SHA-256, byte and row counts; counts per
  domain × origin × generator label; rows per source kind and cohort;
  exclusions by reason; checks; config digest and code identity;
- `SEALED.json` for sealed splits (Dev-select and CAL). Sealed bundles are
  built by pipeline code and are not inspected or scored until their protocol
  use.

### Row contract

| Field | Meaning |
|---|---|
| `id`, `split` | Document id (legacy rows are prefixed `stage3:`) and split |
| `text`, `text_sha256` | Exact text; the digest is verified on read |
| `origin` | `human_only`, `mixed` or `model_only` |
| `share` | Realised final-word fraction in model-produced spans (0 human, 1 model-only) |
| `ai_byte_spans` | Model-produced spans as UTF-8 byte offsets into `text`: empty for humans, one whole-text span for model-only text and full rewrites, the exact replacement spans otherwise; sorted, disjoint, on character boundaries |
| `generator_label` | Canonical attribution label: the frontier label of `docs/a-hosted-output-admission.json` (`claude-opus-5-5`, `requested:gpt-6-astra`, ...), the family id for local and legacy generators (`qwen3.6`, `phi4`, ...), null for humans |
| `generator_key` | Concrete generator: local checkpoint key, frontier export label or legacy profile |
| `cohort_id` | `human:<collection>`, `local:<plan cohort>`, `hosted_frontier_v1`, `frontier-captures-v1`, `hosted-style-v1` or `stage3_legacy_train` |
| `cohort_weight`, `balanced_weight`, `weight` | See Weights |
| `source_group`, `writer_key` | Family/source-group key (the split and bootstrap unit) and writer; positives inherit their parent's |
| `domain`, `genre`, `collection` | Evaluation domain (from the split manifest, else the collection), the pool's genre field, the source collection |
| `parent_id` | Parent human of a positive |
| `voice_id`, `brief_id`, `voice_kind`, `voice_cluster_id`, `voice_origin`, `style_axes`, `style_partition` | Style of the request, resolved against the frozen style partitions only; `style_partition` is the frozen partition (`+`-joined when the voice, brief and text matches differ), `unknown` when an id is not in the frozen file |
| `style_text_match` | The evaluation style item whose normalised text equals the request's voice or brief text, if any |
| `operation` | `human`, `mirror`, `span`, `full`, or the local plan's operation |
| `held_out_style` | The voice or brief belongs to an evaluation partition (or the record says so) |
| `population` | `primary`, the local plan's population, `frontier` or `stage3_legacy` |
| `source_kind` | `human`, `local_positive`, `frontier`, `hosted_capture`, `stage3_legacy` or `external_pool` |
| `provenance_sha256`, `provenance` | SHA-256 of the exact input line, and its input index (in `MANIFEST.json` `inputs`) and line number |

### Selection rules

- A positive's parent must be a human of the same split (for Train, also a
  Train-mining human listed in `train_side_manifests`) with the same source
  group and domain.
- Hosted captures (`hosted_captures`: the 116 earlier captures that
  `docs/a-hosted-output-admission.json` admits by text hash) have no split
  parent. A capture is kept only in Train, only when its source group is a
  Train, Train-mining or stage-3 legacy Train source group, and it takes that
  group's domain; others are excluded as `capture_source_not_train_family`.
- Train rows whose source group or writer appears in any evaluation split
  (reservation manifests and groups of Dev-iter, Dev-select, CAL, Test and
  reserved-unassigned, diagnostic-slice manifests, plus the built evaluation
  humans) are dropped. Legacy rows and captures are also dropped when their
  Gutenberg book id or the first and last tokens of their author key match an
  evaluation writer.
- Styles come only from the frozen partition file (`frozen_partitions`, see
  below). Held-out-style rows (an id or a normalised voice or brief text in an
  evaluation partition) and rows with a style id the file does not list are
  dropped from Train; evaluation splits keep held-out-style rows, flagged.
- Train rows whose text is an evaluation document (built evaluation humans or
  a diagnostic manifest digest) are dropped; evaluation rows in a Train source
  group are dropped.
- Duplicate ids and texts keep the first row in source order (humans, local,
  frontier, captures, legacy; then id).
- Frontier records must be `slop_ninja_frontier_positive_v2` with an admission
  basis. Invalid records are listed as `invalid_record` exclusions.
- **External Train pools** (`external_train_pools`). Each entry names a pool
  documents file (`slop_ninja_a_pool_document_v1`; the baseline release used
  the RAID pool this way), the cohorts to read (`include_cohorts`), an optional
  `max_repeated_word_8gram_share` bound, a seed and two per-mille rates. A
  source group is kept when the per-mille digest of `group:<source_group>` is
  below `group_per_mille`, and a kept model-only row when that of `row:<id>`
  is below `positive_per_mille`; cohorts listed in `keep_all_cohorts` skip
  both samples. Skipped rows are counted as `external:<pool>:<reason>`. A kept
  row must be admitted, Train-only, unassigned and cleared for commercial
  training; it is `human_only` without spans or `model_only` with one
  whole-text span, keeps the pool's id, cohort, source group, domain and
  generator label, and has `source_kind: external_pool` and population
  `external:<pool>`. The other selection rules apply to it as to any Train
  row.

### Weights

Train (`domain_origin_share_v1`, `weights.rs`):

1. Domain `d` gets loss mass proportional to its number of source groups.
2. Within a domain, the human, mixed and model-only cells carry declared
   shares: `weighting.human_share` (default 1/3), the rest split equally
   between mixed and model-only unless `mixed_share` / `model_only_share` are
   declared. Groups are pooled across the whole split, so a group need not
   hold every origin. A domain without one of the cells hands that share to
   its present cells in proportion.
3. Inside a cell every source group gets equal mass and every document of a
   group equal mass; each row is multiplied by its declared cohort weight
   (`cohort_weights.default`, `.cohorts`) and the cell is renormalised, so a
   cohort weight moves mass only between cohorts of the same cell. Weight 0
   removes a cohort.
4. Last, in every hosted cohort (frontier records and captures), each
   attribution label gets the same total weight on the final weights
   (`hosted_frontier_label_balanced_v1`), or the relative weights declared in
   `cohort_weights.labels`. Iterative proportional fitting alternates the
   cell masses and the label totals and ends on the label step: label totals
   hold exactly, cell masses to `label_fit.max_relative_cell_deviation`.

Weights are scaled to mean 1 over rows with positive weight. `balanced_weight`
is the step-3 family-balanced weight, `cohort_weight` the declared factor and
`weight` the final weight. `weights.json` (`slop_ninja_a_bundle_weights_v2`)
records the declaration, the effective mass per origin, source kind, cohort,
label and domain × origin, the cell targets, and the label-fit iterations and
deviations.

Evaluation splits: `weight = balanced_weight`, computed within each
population (equal source-group mass, equal observed origin-cell mass within a
group, equal document mass within a cell); cohort weights are not applied.

### Round-2 options

A config may carry a `round2` block (protocol A2 §6.3–§6.4, as amendment 5
reads them). Absent, or with every option off, the build's bytes are exactly
those of a config without it (a test pins the row and weight digests of a
fixture build and its trainer bundles). With any option on, the build is a
round-2 build:

- its rows carry `slop_ninja_a_bundle_row_v2` (v1 plus the optional `block`
  tag; Train weights raked when covariate balance is on), in every split and
  in each manifest's `row_schema`, and its Train weights the scheme
  `domain_origin_share_r2_v1`, so a version-1 rights snapshot reads none of
  them;
- its Train bundle is refused when its Kish ESS under the trainer's weighting
  (below), rounded to the nearest integer, is below R0's 32,827 (V24). There
  is no switch.

The options:

- `covariate_balance: "a_covariate_balance_v1"` (`covariate.rs`). After step 3
  and before the hosted label step, the positive weights of every Train
  domain × origin cell (mixed, model-only) are raked so that the cell's
  weighted distribution over whitespace-word-count bins matches the domain's
  positive-weight humans (weighted by their step-3 weights); cell masses are
  unchanged. Bins are equal-mass quantiles of the domain's human word counts
  (merged ties, open edge bins). A partition qualifies when every bin has a
  positive and no raking factor exceeds R0's 5× row cap. The candidates are
  the partitions of every nominal b from 1 to the domain's search bound
  ⌈T / m_min⌉ + 1 (human mass over the smallest mass at one word count),
  beyond which every b gives the finest partition; a cell takes the
  qualifying candidate with the most realised bins, at the smallest nominal
  b. Raked bin shares must equal the human targets to 1e-12 or the build
  fails. The report gives, per cell, the realised bin count, the nominal b
  and the cuts (S0-14), the targets, factors, the raked and final (after the
  label step) bin shares, and the bin shares under the trainer's weighting.
  The trainer's caps still apply afterwards: they bound a row at 5× its
  origin class's mean and at 5× the mean of all Train rows, so they can
  truncate raked weights. The report counts the rows and mass above both
  caps on the final weights, per raked cell and overall (reported, not
  gated).
- `cohort_roles`: the round-2 cohorts (`enron_train_half`, `base_model`,
  `cc_n2`), each selected by cohort-id prefixes and collections; a row
  belongs to at most one role. `cc_n2` is self-weighting (no declared cohort
  factor, and no declared label factor for any of its cohorts, hosted ones
  included, other than 1), as is any role with `self_weighting: true`.
  `enron_train_half` must select `enron_email_historical` and name a
  writer-key allow-list (`writer_keys: {file, pointer}`, a JSON array at an
  RFC 6901 pointer, such as S0-9's Train half); its rows must carry a listed
  writer key, and an Enron Train row outside the role is refused. No role may
  overlap the RAID cap prefix `raid-v1-`.
- `block_tags: {c_cohort_prefixes, c_collections}` tags every row `C` (its
  cohort or collection is listed, or its parent is a `C` human of the split)
  or `T`, and each manifest counts `rows_by_block`. Trainer rows do not carry
  the tag; the trainer manifest counts rows per split and block.
- `rows_renorm: DIR` (`rows_renorm.rs`; protocol A2 amendment 20 item 20.7
  and amendment 22) names a `renorm-rows` output directory: the rows
  re-verdict of the Train rows that no journal regenerates (RAID rows, stage-3
  legacy rows and hosted captures with model text) under profile
  `cc_cohort_profile_v2`, population `train2`. The assembler checks `rows-reverdict.jsonl`,
  `rows-normalised.jsonl` and the prior `form-prior-<sha256>.json` against
  `ROWS-RENORM.json`'s digests before it uses a line, then every line against
  the others and the summary's counts (two receipts with one raw digest are
  refused: the re-verdicted rows are one bundle's Train rows, unique by text);
  all four files are build inputs, bound in every manifest. As the Train
  candidates are gathered, before selection, dedup and weights (so the
  covariate bins, the Kish ESS and the RAID share see the laid-out rows), and
  after the Train-mining parents and the source groups that captures name are
  resolved from the candidates as gathered (a planned stage-3 row that the
  re-verdict drops still names its source group):
  - every candidate whose bundle row id has a receipt must be a row with model
    text of the receipt's source kind, origin and cohort, and its text must
    hash to the receipt's `raw_text_sha256` (the row as re-verdicted), or the
    build is refused;
  - a `keep` candidate takes the `text`, `text_sha256`, `ai_byte_spans` and
    `share` of its `rows-normalised.jsonl` line (whose `raw_ai_byte_spans` must
    equal the candidate's spans); origin and every other field, provenance
    included, are kept;
  - a `drop_with_receipt` candidate is excluded
    (`rows_renorm_drop_with_receipt`, with the receipt's file digest, line and
    reasons), and so is a `keep` candidate whose raw text is an evaluation
    text (`rows_renorm_raw_text_in_evaluation_split`), since its raw twin would
    repeat that text.

  At selection's dedup step a laid-out row and its raw twin are one
  document. In selection's order (humans first, then by source kind and id),
  a laid-out row whose raw text an earlier kept row holds is excluded
  (`rows_renorm_raw_text_duplicate`), and a kept one marks its text and its
  raw text as seen, so a later row with the raw text is its `duplicate_text`,
  as in R0's selection. A candidate with model text of those three kinds that
  has no receipt and whose text is any receipt's `raw_text_sha256` (a kept or
  dropped planned row) is that row's raw twin and is excluded
  (`rows_renorm_duplicate_of_planned_raw_text`, naming the planned row and
  its action). After selection, a Train row with model text of those three
  source kinds that holds no laid-out text refuses the build (every Train-2
  positive is under profile v2, §6.2); a candidate with no receipt that
  selection drops for its own reason is not refused. `renorm-rows` plans
  external-pool rows of `raid-v1-*` cohorts only, so a selected positive of
  any other external pool always refuses the build. The Train manifest's
  `rows_renorm` record gives the rules, the files' digests, the receipts'
  counts, the candidates replaced, dropped and without a receipt, and the raw
  twins and raw-text duplicates excluded, by source kind, and the receipts
  without a candidate
  (`receipts_without_a_candidate`, by action); counts and digests, no text. The private check
  after the rows re-verdict compares the kept and dropped counts with
  `ROWS-RENORM.json`'s and requires `receipts_without_a_candidate` to be empty
  or each entry explained by a recorded difference between the Train-2 config
  and R0's. Without the option, the fixture build's rows, weights and
  exclusions keep the digests pinned on the assembler before it.

A round-2 Train `weights.json` has a `round2` report
(`slop_ninja_a_r2_train_weights_v1`): the Kish ESS, the human share of
document-loss mass, the RAID cohorts' share of AI-involved mass before and
under the trainer recipe's inherited caps, the covariate report, each role's
rows, masses and declared factors, and the mass per block. A trainer bundle
of such a build repeats it as `round2_build` and adds the Kish ESS of its own
projected Train rows under the trainer's weighting (`round2_trainer_train`,
reported, not refused).

**Kish ESS** (`kish_ess_trainer_weighting_v1`) is (Σf)²/Σf² of the trainer
recipe's Train weights f: `full_a_bundle.train_weights` under R0's
`train_weighting` (`assembler_row_weight_v1`, class and global row caps 5×,
`cohort_positive_mass_caps` {`raid-v1-`: 0.2}) over the rows with positive
assembler weight, the weighting R0's 32,826.79 was measured under.
`covariate::trainer_weighting` ports that function, Python's float `sum`
included; a test matches it bit for bit against the Python function on
synthetic weights. `ess --train BUILD_DIR/train` reports it for any build's
Train split (a report, no refusal; it reads the Train split as `supervision`
does).

### Domain supervision targets

`supervision` writes `r2_domain_supervision_v1` (S0-8; `supervision.rs`) from
a build's Train split directory, once (mode 600). It reads only the Train
split (the manifest must name it and match the rows' digest, and a non-Train
row stops the run) and only metadata fields; the output holds counts,
shares and digests. Every count is over rows with positive weight. For each
covered domain (`domains`: those with evaluation humans in a round-2
population) the targets meet: (1) positives ≥ ⌈H × 37,396/49,179⌉; (2)
frontier records (source kind `frontier`; RAID, stage-3 and capture rows are
not) ≥ ⌈H × 10,713/49,179⌉, waived and recorded for `enron_domains`; (3)
every `rule3_generators` cell ≥ 59 rows, planned positives included; (4) the
domain's share of Train-2 rows ≥ ½ its Tier C Test human mass share + ½ the
CC shares mapped to it (`cc_stratum_domains`). New frontier and rule-3 rows
count toward rule 1; rule 4 adds humans with their rule 1–3 positives and is
iterated to the least fixed point over the total rows. `planned_humans` and
`planned_positives` (such as the Enron Train half) count as current. S0-8
holds each domain's frontier total; allocating the added records over labels
(label-balanced, Terra ≥ Astra, more to gpt-6.1-terra) is the top-up plan's
(B5b), which checks it.

### Sealed splits

Dev-select and CAL bundles carry `SEALED.json`, and their files are written
read-only. `trainer-bundle` refuses a sealed split unless
`--unseal-for final_selection` (Dev-select) or `--unseal-for calibration_fit`
(CAL) is given; the use is recorded once as
`<build>/<split>/UNSEALED-<purpose>.json` (created exclusively, so a second
use is refused), the trainer bundle's files are read-only and its manifest
lists `unsealed_for`. A smoke bundle never opens a sealed split.

## Trainer bundles

The full-A trainer reads one directory whose `rows.jsonl` holds every split it
uses, with exactly its row fields. `trainer-bundle` projects split directories
of one build into that layout (`train` is required; Test is refused):

- rows keep their exact text, spans, labels, cohort weight and provenance
  digest, with the trainer's 20 fields only (`trainer::TRAINER_FIELDS`);
- `voice_id` is the voice prompt, or the brief when the request had none;
  `style_axes` is the sorted list of persona-axis names (`axis`,
  `library_axis`, `persona_axis` values) or null; `operation` is null for
  humans;
- with `--weighting final` (default), each Train row's `cohort_weight` is its
  final build weight and `weights.json` (`slop_ninja_a_trainer_weights_v1`)
  has `base_weighting: assembler_row_weight_v1` with per cohort and label the
  Train `records`, `weight_sum`, `weight_min` and `weight_max`: the trainer
  uses the row weight as is and does not recompute family/origin balance.
  With `--weighting cohort-factor`, `cohort_weight` is the declared factor,
  constant per cohort and label, and `base_weighting` is
  `family_origin_balanced` (the trainer recomputes the balance). Evaluation
  rows carry 1;
- `MANIFEST.json` lists the source split digests, rows per split, counts per
  split × domain × origin × generator and the selection.

The trainer's own Train weighting (`train_weighting` in its recipe, read by
`grammar-eval/python/full_a_bundle.py`) can bound cohorts further. With either
row-form scheme (`assembler_row_weight_v1` or `label_domain_balanced_v1`),
`cohort_positive_mass_caps` maps a `cohort_id` prefix to the largest share
`c` in (0, 1) of the AI-involved Train mass that its rows may hold. An
over-cap prefix is scaled down to `c` and the other AI-involved rows up in
proportion, so the AI-involved total is kept and human rows never move; the
row caps (`max_row_weight_ratio`, then `max_row_weight_global_ratio`) are
reapplied, and the two steps alternate until both hold. The baseline release
recipe caps the RAID cohorts (`raid-v1-`) at 0.2.

`--take SPLIT:KIND=N` keeps the N rows of a split and source kind with the
lowest SHA-256 of seed and id. `--smoke-carve-dev-iter K/M` moves every Train
source group whose SHA-256 of seed and group is below K/M into Dev-iter (cohort
weight 1, capped as `carved_<kind>`); such a bundle is marked `smoke_only` and
is for pipeline smoke runs only, never a protocol Dev-iter.

## Frozen style partitions

`freeze-styles` maps every voice prompt and brief id to its partition at one
fixed library snapshot (`styles.rs`): snapshot partitions; explicit Train
admissions (the hand-written pilot seed); items of earlier snapshots and
generation records keep their partition, and an item ever seen in an
evaluation partition never returns to `train`; equal normalised texts
(lowercase letters and digits, single spaces) and, with `close_clusters`,
whole snapshot clusters join an evaluation item's partition, to a fixed point.
It writes `FROZEN.jsonl` (`slop_ninja_a_frozen_style_partition_v1`: id, kind,
partition, text key, cluster, origin, axis, basis, history) and `FROZEN.json`
(snapshot digests, inputs, counts by kind, partition and basis). The
assembler and the frontier collector's `plan` (`frozen_partitions` in the plan
config) read partitions only from this file.

## Splits successor

`splits-successor` writes a new splits directory whose evaluation files are
hard links to the base (assignments cannot change) and rewrites only the
Train side (`successor.rs`): an OCR screen of the OCR newspaper domains
(mid-word case flips, broken hyphenation, non-word and odd-token rates against
fixed thresholds, measured with the adapters' lexicon) and a Train-mining cap
per domain (tier `a` first, then SHA-256 of salt and id); rows of the listed
domains with private-use code points or U+FFFD dropped whole; and the ELLIPSE
half split (prompts ranked by SHA-256 of salt and prompt key, even ranks to
Train, odd ranks and any evaluation prompt held in the ungated non-native
diagnostic). `successor-changes.jsonl` lists every dropped or moved row (id,
reason, measures; no text) and `SUCCESSOR.json` the rules, thresholds,
lexicon digest and counts. `ocr-profile` prints text-free quantiles of the
measures per domain for choosing thresholds.

## Consolidation commands

- `merge-eval-humans` concatenates the per-split `documents.jsonl`,
  `metadata.jsonl` and `skips.jsonl` of several evaluation-human directories
  (lines byte for byte, parts in order), refuses duplicate ids or texts, and
  writes per-split `summary.json` and `SHA256SUMS` and a merged
  `ACQUISITION.json` that embeds each part's record.
- `verify-lines` checks that every line of a file occurs byte for byte in its
  successor (as a multiset), for example a split manifest against the next
  split version.
- `pools-index` writes the current pools index from a layer specification,
  adding each admitted file's SHA-256 and row count.
- `verify-frontier` checks a v2 frontier export against the v1 export and
  admission sidecar it replaces.
