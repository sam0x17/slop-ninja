# Corpus expansion v4

Generation, assembly and both encoder fits are complete. The
[v4 comparison](ENCODER_EXPANSION_RESULTS.md) improved fresh Test accuracy and
recovered mixed predictions; no qualified subnet reference has been promoted.
No further Pangram calls were made in this stage.

## Source families and generation

Start with the 261 rights-admitted HC3 historical human excerpts, with corpus
SHA256 `178b2c9ab55a74107fb6792b72a5d69e369a9bd103b17fa3343caeaac989c23d`.
Keep the frozen source-family splits. Select one representative from each of the
243 families by ascending SHA256 of record ID, independent of text scores. Keep
all other admitted excerpts in the source archive and record this selection.
The selected corpus hash is
`4481783ee88a43df8369988f811a854b67b690a73212297588a2c8502fb9afb2`:
184 Train, 21 Development, 21 Calibration and 17 Test families.

Produce one independently composed, source-conditioned draft and one light to
moderate copyedit per representative. Keep the encyclopedia register for these
sources; the old news/scientific register choices remain unchanged. Preserve
source meaning, qualifications, intended tone and attribution in the prompts.
These instructions do not establish factual fidelity. Record model drafts and
edits of historical proxies under the existing weak-label policy.

Use the existing `style-mix-v1` assignment for six profiles: source-matched,
plain, informal, direct, anti-ai and fix-slop. Assign by frozen split, register and
source-family hash before generation. Use the same profile for siblings. Do not
select profiles, texts or retries by detector score. Retain every response and
failure; the existing whole-family admission rule requires both outputs to meet
the declared completion, length, formatting and source-copy checks. Report the
attempted denominator and exclusions. An incomplete run is not a training corpus.

OLMo 3 7B Instruct adds a third generator family to the existing Qwen and Mistral
work. The MLX conversion is pinned to
`mlx-community/Olmo-3-7B-Instruct-4bit@d732c91ae02e90cd5d86e810fbeb9741794b4dd9`.
The upstream [model card](https://huggingface.co/allenai/Olmo-3-7B-Instruct/blob/6e5971d9eba42665f5bd5a0fcf047f299ce1dccc/README.md)
declares Apache 2.0. Each runtime file was verified against the pinned Hub tree,
including the 4.11 GB weight file's SHA256. The local server uses the existing
MLX environment, disables prompt caching and serves one request at a time.
Generation uses temperature 0.7 and a 1,200-token completion cap, with complete
requests, model settings and raw responses retained locally.

The first run requests 486 outputs, two per selected family. Its immutable local
plan and response archive are under
`data/baseline-detector/corpus-expansion-v4/olmo-hc3/`. A finished summary is
required before training. The current 80-word output floor can exclude otherwise
usable edits of short sources; retain these failures when assessing corpus yield.

The compatibility probe completed one synthetic 100-word passage, 118 generated
tokens, in 1.38 seconds including prompt processing. This verifies that the
model works through our local HTTP path; it does not establish a workload-wide
throughput estimate or writing quality. See the
[probe report](results/olmo-generator-probe-v1.json).

## Completed generation and assembly

All four requested cohorts reached terminal completion. Counts below include
every attempted model call; only complete admitted draft/edit pairs entered the
corpus.

| Cohort | Attempts | Admitted outputs | Complete source families | Excluded families |
| --- | ---: | ---: | ---: | ---: |
| OLMo on HC3 | 486 | 397 | 188 | 55 |
| Qwen on HC3 | 486 | 469 | 229 | 14 |
| Mistral on HC3 | 486 | 445 | 210 | 33 |
| OLMo on existing Train sources | 194 | 192 | 95 | 2 |

The earlier Qwen startup attempt produced 486 connection-refused errors before
reaching a model. Its transport archive remains separate from these counts.
The healthy Qwen run followed a server-readiness check. All unused successful
siblings and admission failures remain in the generation archives.

The final corpus has 1,891 records from 361 source families. Train contains
1,633 records from 275 families: 275 human proxies and 679 examples of each model
origin. Source-origin loss weights give each family and origin equal total mass.
Development has 105 records/35 families, Calibration 102/34, and fresh Test 51/17.
Each evaluation family contributes one human root and one generator's pair,
chosen by the declared hash rule before any detector predictions.

All 1,891 records fit the 1,024-token contract, with no audit-driven removals or
truncation. Maximum lengths are 727 Train, 638 Development, 601 Calibration and
342 Test tokens. The separately derived Train whitespace view also passes.
The corpus SHA256 is
`e7d2a2a0727b8c257c8c7dd4f57daae64b0d1bd4c6e3051ef6fd7e63f0d1c87b`.
The all-variant archive, including retired old Test families and unused
evaluation variants, has SHA256
`ab40f42fd54f845e0291d9b5f544279b0de587a5ade4543aa53b29ad6d972851`.

## Detector fit and remaining data work

The three generators now cover the encyclopedia register, and OLMo also covers
the existing news/science Train sources. This reduces the original confounding
between generator family and register. Admission and prompted constraints do
not independently establish factual fidelity or editing strength. Review the
staged CMU book summaries to add narrative prose. Personal writing and documented
human editing sessions remain missing registers/workflows.

Freeze the combined corpus and candidate recipes before training. Select using
Development, calibrate on Calibration and open fresh Test predictions only after
selection. Source-history checks and generation are allowed before that point;
detector results on Test are not. Use existing Pangram observations for the old
Development comparison only. Keep provider predictions separate from origin
labels, and do not use them as training targets without a separate declared and
rights-reviewed experiment.

The new lane follows the [ShareAlike release policy](SHARE_ALIKE_POLICY.md).
Ship source attribution with any resulting public detector. The model's encoder
architecture and sparse word/grammar controls remain unchanged in this stage.
