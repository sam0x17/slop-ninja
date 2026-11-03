# Revision detector v6 corpus

This snapshot contains **5,463 records from 797 source families**. The four
JSONL files total **52,171,181 bytes (52.17 MB)** with text and provenance.
The rights manifest adds **7,988,644 bytes (7.99 MB)**. These are exact copies
of the frozen primary corpus for the
[v6 experiment](../../baseline/detector/ENCODER_REVISION_V6.md).

| File | Records | Purpose in the original experiment |
| --- | ---: | --- |
| [train.jsonl](train.jsonl) | 3,833 | Fit model parameters |
| [development.jsonl](development.jsonl) | 530 | Select the checkpoint using the terminal revision view |
| [calibration.jsonl](calibration.jsonl) | 540 | Fit temperature and freeze decision thresholds using the declared view |
| [test.jsonl](test.jsonl) | 560 | Evaluate the selected model on the three declared revision-stage views |

The files include complete ancestry. An evaluation stage uses its declared
human/model/mixed view, not every ancestor as an independent observation.
Keep the original source-family assignments and use the experiment's frozen
views when reproducing its results. The Train shard includes the 2,925 v5 Train
records and 908 model revisions. Other splits use the fresh primary families
that completed the required generation chain. Separate failure and all-human
coverage reports remain necessary; complete-chain membership is a selection
condition, not evidence that every assigned source succeeded.

This snapshot excludes the separate Phi-4 transfer route, the all-assigned-human
diagnostic, whitespace-normalized copies and per-record detector predictions.
It overlaps [v5](../detector-v5/README.md) and the annotation-input snapshots.
Do not add their counts together or randomly split their concatenation.
Published inputs cannot serve as secret subnet challenges or independent fresh
confirmation for models subsequently trained on them.

Each line follows `slop-ninja-origin-record-v1` and retains exact UTF-8 text,
SHA256, source URLs and attribution, license, source family, split and origin
evidence. Generated records retain the complete prompt, model identifiers,
checkpoint revision, runtime, sampling settings, request/response hashes,
timestamp and parent record ID. A writing profile is distinct from the model
that generated a passage. Revisions of model drafts retain their model-only
workflow label; edits of historical human proxies retain mixed-origin labels.
Detector scores do not replace those labels.

The sources cover PLOS abstracts, Wikinews reporting, historically matched HC3
Wikipedia answers and CMU Wikipedia plot summaries. Their human-proxy labels
rely on historical evidence and do not certify a documented human-only workflow.
The author of a summarized book is not the author of its Wikipedia synopsis.
This limited genre coverage and source-conditioned generation restrict what
the corpus can establish about general authorship detection. Meaning, intended
tone and writing quality are not certified labels.

The shards, [manifest.json](manifest.json) and
[data_rights.json](data_rights.json) are unchanged experiment inputs. Historical
local paths identify acquisition archives, which are not required to read the
snapshot. Preserve source URLs, content hashes and ancestry when repackaging.
No private correspondence, preface experiments, hosted GPT/Claude captures,
credentials or individual Pangram reports are included.

Preserve the attribution, source and license links, change notices and
ShareAlike obligations recorded in each row. Source licenses are CC BY 2.5,
CC BY 4.0, CC BY-SA 3.0 and CC BY-SA 4.0. CMU-derived records additionally
retain the collection's CC BY-SA 3.0 US notice and credit David Bamman and Noah
Smith (2013). The embedded Fix Slop prompt adaptations retain their upstream
attribution and [MIT notice](LICENSE.fix-slop). The repository's code license
does not replace these text licenses. The
[ShareAlike policy](../../baseline/detector/SHARE_ALIKE_POLICY.md) and exact
rights manifest describe the project's chosen model-release obligations.

The Rust corpus auditor on the Mac Studio checked all records together,
including text hashes, origin evidence, ancestry and source-family split
boundaries. See [audit.json](audit.json). Every record declares permission to
redistribute its text, and each shard and rights-manifest hash matches the
frozen manifest. These checks cannot guarantee that every source rights issue
has been found. To verify the published files from this directory:

```sh
shasum -a 256 -c SHA256SUMS
```

To repeat the corpus audit, concatenate the four shards into an ignored file
and run `slop_ninja_detector audit --input` on it. Run corpus processing and
builds on the Studio, following the repository conventions.
