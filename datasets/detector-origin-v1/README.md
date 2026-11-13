# Detector origin corpus, snapshot 1

This snapshot contains **432 unique texts from 128 source families**, totaling
**4,034,367 bytes (4.03 MB)** with provenance. Slop Ninja Research published it
on 2026-09-12. These are the inputs to the first Pangram collection campaigns;
the complete detector training corpus is larger. Ordinary Git stores this
snapshot; Git LFS is unnecessary at this size.

| File | Split | Texts | Families |
| --- | --- | ---: | ---: |
| [train/pilot.jsonl](train/pilot.jsonl) | Train | 120 | 24 |
| [train/second.jsonl](train/second.jsonl) | Train | 96 | 32 |
| [train/third.jsonl](train/third.jsonl) | Train | 42 | 14 |
| [train/fourth.jsonl](train/fourth.jsonl) | Train | 117 | 39 |
| [development/initial.jsonl](development/initial.jsonl) | Development | 57 | 19 |

Each JSONL row follows `slop-ninja-origin-record-v1` and contains the exact
UTF-8 text, its SHA256, source URLs and attribution, origin evidence, fixed
split and derivation parent. Generated rows also retain the complete prompt,
requested/reported model names, checkpoint revision, runtime, sampling settings,
request/response hashes and timestamp. Follow `parent_id` to recover earlier
generation stages. The source's contributors and the requested writing persona
are distinct from the model that generated a passage.

The files are byte-for-byte copies of the archived inputs, with matching hashes
in [manifest.json](manifest.json). Historical `source.raw_path` values and
references to acquisition files identify the original local archives; those
archives are not bundled or needed to read, audit or train on these records.
Use the source URLs and content hashes for provenance. No text or metadata was
rewritten during packaging.

There are 128 historical human proxies, 176 model-only passages and 128 edits
of human proxies. Human labels use historical evidence of varying strength,
described in each record; they do not certify a documented human-only workflow.
Model-only prose includes source-conditioned drafts and subsequent model
revisions. All these texts may share facts and topics with their source family.
Group the entire ancestry together when fitting or evaluating a detector.

The sources cover PLOS abstracts, Wikinews reporting, historically matched
Wikipedia answers from HC3, and CMU Book Summaries. The latter are Wikipedia
plot summaries, with collective contributor attribution. These four registers
provide limited coverage of writing styles. Generation provenance in this
snapshot covers OLMo, Qwen and Mistral; the
[GPT/Claude collection](../detector-hosted-v1/README.md) is published separately.
Its training admission comes from the
[hosted admission manifest](../../docs/a-hosted-output-admission.json), not from
either publication.

The Train collection was selected adaptively after earlier Pangram results.
Its [collection protocol](../../baseline/detector/PANGRAM_DATASET_V2.md)
documents the sequence. It is unsuitable for an unbiased prevalence estimate
or fresh confirmation. Keep the 57 Development records out of training when
reproducing the existing comparisons. This snapshot contains no Calibration
or Test records and must not substitute for the frozen v6 fitting corpus.

Pangram scores are kept privately and not published, and they never become
origin labels. This snapshot does not include them: individual Pangram
annotations and raw reports remain in private storage (see the
[annotation policy](../../baseline/detector/STANDARD_CORPUS.md)).
The [published aggregate](../../baseline/detector/results/pangram-dataset-v2-four-cohorts.json)
covers all 375 Train texts. The fourth batch's provider echoes now validate
with explicitly recorded whitespace and source soft-hyphen normalization;
the submitted corpus bytes remain unchanged. Origin labels are independent
of detector observations.

Preserve each row's `rights.attribution`, license, source links, change notices
and any `rights.share_alike` obligations when redistributing it. Text licenses
are [CC BY 2.5](https://creativecommons.org/licenses/by/2.5/),
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/),
[CC BY-SA 3.0](https://creativecommons.org/licenses/by-sa/3.0/) and
[CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/).
CMU-derived records additionally retain the collection's
[CC BY-SA 3.0 US](https://creativecommons.org/licenses/by-sa/3.0/us/) notice
and credit David Bamman and Noah Smith (2013). Code licenses do not replace
these text licenses. See the [ShareAlike policy](../../baseline/detector/SHARE_ALIKE_POLICY.md).
The embedded Fix Slop prompt adaptations retain their pinned upstream source
and the accompanying [MIT notice](LICENSE.fix-slop), copyright Hardik Pandya.
No warranties of source-label accuracy or generated-text fidelity are made.

On the Studio, the existing Rust corpus auditor validated all 432 records
together, including text hashes, ancestry, prompt provenance and rights fields.
There are no duplicate text hashes or IDs, and no source family crosses splits.
See [audit.json](audit.json). To check the published files from this directory:

```sh
shasum -a 256 -c SHA256SUMS
```

To repeat the corpus audit from the repository root, concatenate the five JSONL
files into an ignored temporary file and run `slop_ninja_detector audit --input`
on it. Use the Studio for builds and corpus processing, following the repository
conventions.
