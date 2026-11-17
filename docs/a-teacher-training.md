# Teacher training and frozen span heads

The full-A trainer supports two experimental teacher classes:

| Class | Upstream encoder | Width | Layers |
|---|---|---:|---:|
| `modernbert_base_v1` | ModernBERT-base | 768 | 22 |
| `modernbert_large_v1` | ModernBERT-large | 1,024 | 28 |

Both use fresh document heads and a class-specific normalization fitted on
Train inputs. `teacher_a.py` pins the upstream revision, file sizes and
SHA-256 digests. Loading is offline and checks every pinned file. Teacher
fits are research outputs; the artifact exporter refuses them. For miner
submissions, follow the [training guide](testnet/training-guide.md).

The protocol and contract must name the same `teacher_class`. Use
`teacher_a.py check --teacher-class CLASS --artifact SNAPSHOT` to verify a
snapshot, and `teacher_a.py embed` to prepare its normalization inputs.
`prepare_full_a_contract --teacher-class CLASS` binds the class and width.
The trainer takes the snapshot as `--artifact` and omits `--init`.

A token-head arm uses `freeze: encoder_and_document_heads` and starts from
a completed same-class fit through `--init-fit`. The head reads the encoder's
actual hidden width. An optional S2 span tower copies the requested final
encoder layers and final norm after restoring that fit. Its copied layers
retain their original layer indices and global/local attention windows.
Only the token head and optional tower train; document tensors stay frozen.
A warm start from an existing tower restores its trained weights and requires
the same tower depth. It refuses a destination that would discard the tower.

These span fits require `span_target_floor` with policy
`slop_ninja_span_target_floor24_v1` and `min_words: 24`. Shorter AI runs are
masked from span supervision and span metrics. Their words still count in
document share and involvement.

`--final-step --init-fit` requires frozen encoder and document heads. Before
training, its identity gate compares unpadded FP32 document outputs with the
initial fit's retained outputs, allowing at most `1e-6` absolute error. This
pass reads input arrays and row identities only. It computes no loss,
calibration, selection score or diagnostic and writes only the document and
token identity-gate summaries. Evaluation metrics are computed once after
the final checkpoint is fixed and retained in the fit directory. Keep those
files behind the evaluation access rules for the experiment.

The initial fit must contain every identity-gate row, with matching text
digests. A changed evaluation population needs a separately bound reference
output preparation step; missing rows do not waive the identity gate.

For a completed fit, `raw_full_a.py rows --token-logits` writes the ordinary
document outputs plus `token-logits.jsonl` and `token-offsets.jsonl`. The
second file records exact-text Unicode character offsets and word-token
masks, without labels. The manifest hashes both files. This route accepts
Train, Train-mining, Dev-iter and CAL rows; it refuses Dev-select and Test.
The optional files contain raw outputs, without span decoding or confidence
calibration. Their use remains subject to the experiment's frozen read gates.

The Rust example `a_spans_raw_floor24 CONFIG.json OUT.json` evaluates these
outputs after the experiment permits reading labels. It uses the runtime's
word grouping, share tilt, decoder and confidence maps. Every decoder must
have `min_words >= 24`. It never fits a map or selects a decoder.

The config schema is `slop_ninja_raw_span_floor24_config_v1`. It names a
`split`, four inputs (`rows`, `offsets`, `logits`, `documents`), and an
`emissions` object mapping decoder names to runtime emission specs. Each
input is an object with `path` and a full `sha256`. The rows use the trainer
row contract with exact text and `ai_byte_spans`. Offsets and logits are the
files described above. Each document JSONL row has schema
`slop_ninja_span_document_v1`, `id`, `text_sha256`, `share`, and `confidence`;
the two probabilities are on the 0..1 scale, after applying that system's
frozen calibration. Preparing this file must preserve the calibration's
provenance in the experiment record. All four files must contain exactly
the same identities. Duplicate rows, digest mismatches, missing outputs and
nonfinite probabilities or logits fail the run.

Gold runs are maximal sequences of fully AI-labelled words. Runs shorter
than 24 words and partially AI-labelled words are excluded from span counts
and word Brier. Their characters count neither as AI nor as human. An
unmatched decoded span is ignored if it touches no qualifying gold run and
at least half its non-space characters are excluded AI words. Other
unmatched spans count as false positives. Matching uses non-space character
IoU of at least 0.5, greedily by descending IoU, with ties resolved by gold
then decoded order.

The report retains per-document statistics and these aggregates:

- Character F1 is the equal-document mean over mixed documents containing
  at least one qualifying gold run. Human false positives pool human
  characters in those documents. Span precision, recall and recall bands
  (24-49, 50-99, 100+ words) pool runs in the same population.
- The human flag rate counts human documents with any decoded span.
- Raw and tilted word Brier average each document's scored words, then
  average documents equally. Both use the same floor mask.
- Confidence ECE uses ten equal-width bins and pools events. A sentence's
  event is any AI word; a decoded span's event is at least half AI words.
  Any event touching an excluded word is omitted, with exclusion counts
  retained. This confidence population is explicit in the report and must
  be bound before comparison; it is not interchangeable with older
  all-length confidence reports.

Empty denominators produce `null`. Output contains identities and
statistics without source text and should remain private with the
experiment. The tool refuses Test and Dev-select split names; this check
does not replace experiment-specific access rules for other split labels.
