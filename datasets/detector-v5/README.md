# Reference detector v5 corpus

This snapshot contains **3,384 unique texts from 624 source families**. The four
JSONL files total **32,780,427 bytes (32.78 MB)**, including text and per-record
provenance. It is the unchanged primary corpus used for the released
[v5 detector](../../baseline/detector/ENCODER_NARRATIVE_RESULTS.md).
Slop Ninja Research published this snapshot on 2026-09-12.

| File | Records | Purpose in the original experiment |
| --- | ---: | --- |
| [train.jsonl](train.jsonl) | 2,925 | Fit model parameters |
| [development.jsonl](development.jsonl) | 183 | Select the checkpoint |
| [calibration.jsonl](calibration.jsonl) | 201 | Fit temperature and decision thresholds |
| [test.jsonl](test.jsonl) | 75 | Evaluate the selected model |

Keep the original source-family assignments when reproducing the experiment.
The Test results have already been published. These records cannot serve as
fresh confirmation or secret subnet challenges. This snapshot does not include
the separate Phi-4 transfer view, whitespace-normalized diagnostic view, or the
ongoing v6 expansion. The [Pangram-input snapshot](../detector-origin-v1/README.md)
overlaps this corpus; do not add the snapshot counts together or randomly split
their concatenation.

## Text and provenance

Each line is a `slop-ninja-origin-record-v1` record with the exact UTF-8 text,
SHA256, source family, split, source URLs, attribution, license and origin
evidence. Generated passages retain their complete prompts, model identifiers,
checkpoint revisions, runtime and sampling settings, timestamps, request and
response hashes, and parent record IDs. Requested writing styles are separate
from generator identity. The generator families are OLMo, Qwen and Mistral.

There are 624 historical human proxies, 1,380 model-written drafts and 1,380
model edits of human proxies. Historical evidence supports the human proxy
labels but does not certify a documented human-only workflow. Model drafts
were conditioned on source material; they are not independent of their source
families. Meaning preservation and writing quality are not certified labels.

The source collections are PLOS abstracts, Wikinews articles, historically
matched Wikipedia answers from HC3, and CMU Book Summaries matched to historical
Wikipedia revisions. Book authors are not the authors of those plot summaries.
The corpus covers a limited set of genres. See [audit.json](audit.json) for
counts by collection, origin, evidence, license and split.

The four shards, [manifest.json](manifest.json), and
[data_rights.json](data_rights.json) are byte-for-byte copies of the frozen
experiment inputs. Historical local paths identify acquisition archives, which
are not bundled or needed to read the records. Use source URLs and hashes to
identify the original material. No private correspondence, preface experiments,
credentials or raw provider invocation logs are included.

## Reuse

Preserve every record's attribution, source and license links, change notices,
and applicable ShareAlike obligations. Text licenses are CC BY 2.5, CC BY 4.0,
CC BY-SA 3.0 and CC BY-SA 4.0 as recorded in each row. CMU-derived records also
retain the collection's CC BY-SA 3.0 US notice and credit David Bamman and Noah
Smith (2013). The embedded Fix Slop prompt adaptations retain their upstream
attribution and [MIT notice](LICENSE.fix-slop).

The repository's code license does not replace these text licenses.
The [ShareAlike policy](../../baseline/detector/SHARE_ALIKE_POLICY.md) and exact
`data_rights.json` describe the project's chosen model-release obligations.
All included records passed the existing admission checks and declare
`redistribute_text: true`; those checks do not guarantee that every source
rights issue has been found. Individual Pangram annotations and the new hosted
GPT/Claude captures are excluded from this snapshot.

## Verification

The existing Rust corpus auditor ran on the Mac Studio and validated all 3,384
records together. The publication check also confirmed unique IDs and text
hashes, no source family shared across splits, and redistribution flags on
every record. All shard and rights-manifest hashes match the frozen manifest.

From this directory, verify the published files with:

```sh
shasum -a 256 -c SHA256SUMS
```

To repeat the corpus audit, concatenate the four JSONL shards into an ignored
temporary file, then run `slop_ninja_detector audit --input` on it. Run corpus
processing and builds on the Studio, following the repository conventions.
