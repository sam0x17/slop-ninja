# Detector revision inputs, snapshot 1

This snapshot contains **110 unique Train texts from 22 source families**,
totaling **933,692 bytes (0.93 MB)** with provenance. Slop Ninja Research
published it on 2026-09-13. Ordinary Git stores the files.

[train.jsonl](train.jsonl) is an exact copy of the fifth Pangram annotation
cohort's input. Each family includes a historical human proxy, a model draft,
an edit of the human proxy, and two successive revisions of the model draft.
There are 22 human proxies, 66 model-only passages and 22 mixed-origin passages.
All records retain their original Train assignment, exact text, source rights,
generator provenance, prompts, hashes and ancestry.

These families and exact texts are disjoint from the previously annotated
432 origin records and 96 GPT/Claude captures. They overlap the larger detector
training corpora. This is a supplement, not a replacement for the frozen v5 or
v6 corpus. All 110 inputs received Pangram 4.0 observations. Individual scores
and provider reports remain local under the existing
[annotation publication policy](../../baseline/detector/STANDARD_CORPUS.md).
See the [aggregate results](../../baseline/detector/results/pangram-dataset-v2-fifth-cohort.json).

Selection covered the remaining source-collection and writing-profile cells
after excluding previously annotated families. It followed earlier annotation
experiments, so this adaptive Train collection is unsuitable for estimating
population prevalence or fresh benchmark performance. Source-conditioned
revisions retain model-only origin labels regardless of their detector scores.
Historical human proxies do not certify a documented human-only workflow.
Keep entire source families together in any later fitting or evaluation split.
These annotations have not changed the active v6 experiment.

Preserve the attribution, source URLs, licenses, change notices and ShareAlike
obligations recorded in each row. The source licenses are CC BY 2.5, CC BY 4.0,
CC BY-SA 3.0 and CC BY-SA 4.0. CMU-derived records also retain the collection's
CC BY-SA 3.0 US notice and credit David Bamman and Noah Smith (2013).
The embedded Fix Slop prompt adaptation retains its upstream attribution and
[MIT notice](LICENSE.fix-slop). Code licensing does not replace text licensing;
the [source snapshot notes](../detector-origin-v1/README.md) and
[ShareAlike policy](../../baseline/detector/SHARE_ALIKE_POLICY.md) also apply.
Historical local paths identify acquisition archives; the source URLs and
content hashes provide provenance without requiring those archives.

The Rust corpus auditor on the Mac Studio validated text hashes, origin and
generation evidence, rights fields and complete ancestry. Its output is
[audit.json](audit.json). File bindings are in [manifest.json](manifest.json).
To verify this snapshot from its directory:

```sh
shasum -a 256 -c SHA256SUMS
```
