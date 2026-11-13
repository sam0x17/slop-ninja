# SlopNinja corpus

All collection batches belong to one logical corpus. The versioned directories
below are retained shards and experiment views of it. New rewrites, prompts,
parser artifacts and detector observations join the same corpus through document
IDs, exact-text hashes and source-family ancestry. Store text once where possible;
preserve distinct production events and every paid observation even when text is
identical. Overlapping snapshots must not multiply a document's training weight.

Each experiment selects a frozen view with its own rights, exposure, reference
and query membership, and source-family splits. Those fields survive collection
and storage changes. Published or previously inspected Test material remains
consumed evaluation evidence. A common corpus does not imply a common learned
feature space across different grammar or encoder versions.

New [model and style acquisition](../docs/a-corpus-acquisition.md) records the
actual generator and exact instruction for every transformation. It uses
free-form generated style prompts rather than a fixed style menu. Live batches,
private material and provider logs remain in ignored storage; completed permitted
snapshots use the compressed publication format below.

| Snapshot | Texts | JSONL size | Contents |
| --- | ---: | ---: | --- |
| [Retained encoder Train data](a-retained-train-20261026-v1/README.md) | 2,365 | 38.76 MB | Remaining reused Train data, original generation recipes, and an index of all 4,157 fitted queries |
| [Encoder adaptation cohort](a-encoder-20261026-v1/README.md) | 2,268 | 35.56 MB | New Train/Development/Calibration queries, consumed Test, independent references and measured rewrite spans |
| [Fresh balanced non-Test cohort](a-balanced-20261026-v1/README.md) | 937 | 14.25 MB | 215 Train, 288 Development and 414 Calibration queries and 20 independent references from 54 source works; no Test text |
| [Train augmentation](a-train-augmentation-20261026-v1/README.md) | 1,049 | 16.28 MB | Append-only Train queries from 96 original V6 Train works, with measured rewrite spans |
| [Shared A supervised cohort](a-supervised-20261026-v1/README.md) | 552 | 7.11 MB | Exposed cohort with measured rewrite spans, role masks, independent references and consumed Test |
| [Revision detector v6](detector-v6/README.md) | 5,463 | 52.17 MB | Frozen primary corpus, including repeated model revisions and complete ancestry |
| [Reference detector v5](detector-v5/README.md) | 3,384 | 32.78 MB | Frozen Train, Development, Calibration and opened Test corpus |
| [Pangram inputs, snapshot 1](detector-origin-v1/README.md) | 432 | 4.03 MB | Inputs to the initial annotation campaigns, including later model revisions |
| [GPT/Claude captures, snapshot 1](detector-hosted-v1/README.md) | 96 generated + 24 supporting roots | 1.99 MB including audit | Four hosted generators, complete prompts, provenance and source attribution |
| [Revision inputs, snapshot 1](detector-revisions-v1/README.md) | 110 | 0.93 MB | Fifth annotation cohort, including complete revision chains |

These snapshots include exact text, generator provenance, source attribution,
licenses and checksums. They overlap; their row counts are not additive.
`raw_path` and similar path fields are local provenance from the original
collection host; the files they name are not part of this repository.
When the project moved to this repository on 2026-11-03, the home-directory
prefix of these fields in the record files of `detector-v6`, `detector-v5`,
`detector-origin-v1`, `detector-hosted-v1` and `detector-revisions-v1`, and of
the run paths in `detector-hosted-v1/capture-audit.json`, was replaced by a
repository-relative path or the shell's home-directory shorthand. Text, labels
and every other field are unchanged. The checksums, sizes and manifest entries
of these files, and the record digests that other files cite, were updated with
them.

The encoder-adaptation snapshot stores six payloads in 2.77 MB compressed,
including its evidence index. It contains the new cohort only. The retained
Train snapshot adds 4.17 MB of compressed payloads and a reconstruction manifest
covering all 4,157 fitted Train queries across four snapshots. Membership binds
original fit order, exact text and supervision, with public identity-index
meanings. Private fitted profiles and weights remain outside Git.
Ordinary Git stores dataset JSONL, document-level rights, and evidence indexes
as lossless `.zst` files. The sizes above describe decoded bytes. Split manifests,
licenses, and existing SHA-256 files remain readable and bind the original,
decoded bytes. Under the project's publication policy, Pangram scores are kept
privately and not published: no snapshot here carries a Pangram score, and
Pangram links or ids may be published only where they serve verification.
Hosted OpenAI and Anthropic outputs may be published, subject to the providers'
terms and third-party rights; each text's source licence still decides whether
it is redistributed. Individual Pangram reports and working invocation logs
remain in private storage. The
hosted captures are admitted for A training and evaluation by record hash in the
[hosted admission manifest](../docs/a-hosted-output-admission.json).

Before running existing readers, restore the required snapshot from the repository
root (Rust is required; a separate zstd installation is not):

```sh
cargo run --release --locked --manifest-path tools/dataset-storage/Cargo.toml -- restore . datasets/detector-v6
```

Omit the final snapshot argument to restore all published data. Restored files
are ignored compatibility copies; a differing local file causes an error rather
than being overwritten. `verify .` checks the archives and the compression
manifest without creating decoded copies. Original dataset `SHA256SUMS` checks
can then run unchanged in their snapshot directories. See
[dataset storage](../tools/dataset-storage/README.md) for publication and historical
checkout instructions. Compression changes storage only: exact text, split
membership, exposure, provenance, and existing checksums are unchanged.

As of 2026-09-13, the local Pangram collection covers 581 distinct Train texts
from 155 source families, 57 Development texts from 19 families, and 162 fresh
Test texts from 18 families. That is **800 distinct annotated texts from 192
source families**. The fresh comparison retains its disjointness checks against
the earlier collection. Repeated scans do not increase these counts.

All 162 fresh Pangram requests succeeded. The [collection report](../baseline/detector/results/pangram-revision-v6-collection.json)
records the score histograms and budget. Scores remain concentrated
at 100%: 106 of 108 model-only texts fall in that bin, and only eight nonhuman
texts fall between 60% and 100%. The [completed v5/v6 comparison](../baseline/detector/releases/v0.6.0/RESULTS.md)
includes the fresh Pangram check and both models' frozen operating points.
These Test observations remain outside fitting; individual annotations stay
in ignored storage.

The published v6 primary corpus contains 5,463 records from 797 families:
3,833 Train, 530 Development, 540 Calibration and 560 Test. Its four JSONL files
total 52,171,181 bytes (52.17 MB), with another 7,988,644 bytes of rights metadata.
V6 training, calibration and the declared final evaluation are complete.
Publishing these exact copies did not change the selected model, thresholds
or evaluation. The v6 corpus overlaps v5; it is not an additional disjoint
collection. Published Test inputs must be retired from future hidden benchmarks.

## Global Voices attribution

Some snapshot texts contain excerpts from articles published by
[Global Voices](https://globalvoices.org), or short passages that match them.
Global Voices publishes under the
[Creative Commons Attribution 3.0](https://creativecommons.org/licenses/by/3.0/)
licence by default (CC BY 3.0; individual items can carry other terms). Credit:
Global Voices and its contributing writers; see the publisher's
[attribution policy](https://globalvoices.org/about/global-voices-attribution-policy/).
Excerpts were extracted from article paragraphs without markup, and rewrites
record their changes per row. These changes are not endorsed by Global Voices or
the writers. The payloads concerned share only short runs of words with those
articles, and some runs may be ordinary phrases:

- `a-encoder-20261026-v1/test.jsonl.zst` and `a-encoder-20261026-v1/sources/pg-2015.rdf`
- `a-retained-train-20261026-v1/records.jsonl.zst`
- `a-train-augmentation-20261026-v1/records.jsonl.zst`
- `detector-origin-v1/train/third.jsonl.zst`
- `detector-revisions-v1/train.jsonl.zst`
- `detector-v5/train.jsonl.zst`
- `detector-v6/calibration.jsonl.zst` and `detector-v6/train.jsonl.zst`

This notice adds attribution only; no payload byte or checksum changed. The
repository's top-level `NOTICE` lists every covered file.

## Gemma licence copies, 2026-10-31

The Gemma Terms of Use and Prohibited Use Policy in `a-balanced-20261026-v1/licenses/`
and `a-encoder-20261026-v1/licenses/` were retained HTML page snapshots. On
2026-10-31 they were replaced by plain-text copies of the same article text,
each with its source URL and fetch date (`gemma-terms.txt`,
`gemma-prohibited-use.txt`). The HTML snapshots are retained privately. No
record payload changed. Each snapshot's `NOTICE.md`, `manifest.json` and
`SHA256SUMS` were updated to match:

| File | SHA-256 before | SHA-256 after |
| --- | --- | --- |
| `licenses/gemma-terms.html` → `.txt` (both snapshots) | `7d7f4ab3b835f8c38ea0a5f4bdc0e6663a44fc6841006aa51e3616035fd97586` | `d60bd6f05c5f433c1c0a04178d4f057ec06846c4e87cf9ff5ef5359cf4b1d7f8` |
| `licenses/gemma-prohibited-use.html` → `.txt` (both snapshots) | `4d8174989f116f8db6208a6582aab8da25cf3c004836f20e632eb9d3971a4100` | `f08605a6bff5ab3e8187bae70f06112629d4042bdd42ca6fd9d2fe8cab035048` |
| `a-balanced-20261026-v1/NOTICE.md` | `a23be99671e06dda15264f95dba33b52355a099e5616dbaa230673dcf8cfaeb0` | `058c875eddf3b8ae0cd6fa5d2b5b710fb4d2800175e9cf1313ed1a1895774400` |
| `a-balanced-20261026-v1/manifest.json` | `715109bd24140ba3f6dfe986967c02996a8ff01bc065aa08ef96e7b9ceebc652` | `9a1a6a8805fd0d1663f63f51904ef10d971af21fd69913428ca5f67ddde16849` |
| `a-balanced-20261026-v1/SHA256SUMS` | `68d7cd4e9c1660cd839f657a07b4c70c8c3dac1e4f5e0eed838020dc3b9f77e0` | `1c0fd5ec4226afbb5415f9a75f4586e5e645d842ed8515346b912458fd4f87fa` |
| `a-encoder-20261026-v1/NOTICE.md` | `9ac0b654287765fbbef04bfa211a39e63343302d1329df7f0e94b41bbf9b7bca` | `4237cd2ff126be8f768227645440b70e3a777651cd82a8c6240cc7c9b12892aa` |
| `a-encoder-20261026-v1/manifest.json` | `56449f989f546fc76f66db3d0f08c2ab4f6d56f1e22929cbbda0faac23e67909` | `b55f902c7f81b2fcb9fd353d8da001697fa1d12b2e40ebdf753091581368f223` |
| `a-encoder-20261026-v1/SHA256SUMS` | `311ab3263d189f3e1b0d37c60453b6006f15c801e91b0be8498bf3794b388fe4` | `8ac00587786b87b7b121f679f18e5ad23be881efc83f4a42fe8409b4c8ba7abc` |

These records still cite the earlier bytes, as evidence of what existed when they
were written:

- `a-balanced-20261026-v1/generators.json`: its `license_evidence` digests
  identify the HTML snapshots inspected at generation time. The file is
  unchanged, so every pin of its own digest still holds.
- `a-train-augmentation-20261026-v1/manifest.json`: `previous_snapshot_checksums_sha256`
  is the earlier `a-balanced` checksum index. `"previous_snapshot_modified": false`
  describes that publication, which left the earlier snapshot unchanged.
- `publish_train_augmentation` (`OLD_SUMS_SHA`) checks that earlier index. A
  reproduction of that snapshot therefore needs the earlier `a-balanced` bytes,
  which are retained privately.
- `publish_balanced_snapshot` and `publish_encoder_snapshot` copy the HTML
  snapshots from private generator evidence, so they reproduce the earlier
  bytes of the two snapshots.

## Licence notices, 2026-11-02

The `NOTICE.md` files of `a-balanced-20261026-v1`, `a-encoder-20261026-v1` and
`a-train-augmentation-20261026-v1` now state the model licences, the Gemma terms
and the dataset terms without readings of them. The same text change was made
to the `NOTICE` constants of `publish_balanced_snapshot`,
`publish_encoder_snapshot` and `publish_train_augmentation`. No record payload
changed. Each snapshot's `SHA256SUMS`, and the `manifest.json` that lists its
`NOTICE.md`, were updated to match:

| File | SHA-256 before | SHA-256 after |
| --- | --- | --- |
| `a-balanced-20261026-v1/NOTICE.md` | `058c875eddf3b8ae0cd6fa5d2b5b710fb4d2800175e9cf1313ed1a1895774400` | `9ca43e3b903499f7fbb2d0c414f8c583072bf4c36e77a8e130ec36ab2cc766e9` |
| `a-balanced-20261026-v1/manifest.json` | `9a1a6a8805fd0d1663f63f51904ef10d971af21fd69913428ca5f67ddde16849` | `9e4fafe0f303d2c528c0715b56e06ed3ab1970559bda468a13a244ed5a8c56e2` |
| `a-balanced-20261026-v1/SHA256SUMS` | `1c0fd5ec4226afbb5415f9a75f4586e5e645d842ed8515346b912458fd4f87fa` | `87a8dc67f2f9ede38ddd5fe230bd51afcb6a40df7d71528fbe94786985578c77` |
| `a-encoder-20261026-v1/NOTICE.md` | `4237cd2ff126be8f768227645440b70e3a777651cd82a8c6240cc7c9b12892aa` | `595b99886777f591f28642fee4aa42352dea9435245b95a871c2934f275f697e` |
| `a-encoder-20261026-v1/SHA256SUMS` | `8ac00587786b87b7b121f679f18e5ad23be881efc83f4a42fe8409b4c8ba7abc` | `e4181eb1e994884fce49fc491131dec31a0c149ca5ebdbc787dfe4bcd2ef1cdd` |
| `a-train-augmentation-20261026-v1/NOTICE.md` | `5379fed7cf52264022fa0b7d5ec16022dddf4cd85245c8cc432f0263b65c0aca` | `66f231f425671503e374c391b8ad01236ce9074046f2cde93d6330f69ae2e7d1` |
| `a-train-augmentation-20261026-v1/manifest.json` | `4dd360d2ad4fad45775d123bdca848c113910694446c22083213a6678814c620` | `3e8842a0c856f1b662f7c999b4455d6db7c8331c4fed79c70ca473448d34ecc9` |
| `a-train-augmentation-20261026-v1/SHA256SUMS` | `17ceeef219c55da24c052b0cd5e9a83494d313abf6eb96c0292608be639a9b6a` | `a99358f37df2db575550838c633be424529f3d3da0b9a369926667819c61011d` |

The `a-encoder` manifest does not list its `NOTICE.md`, so it is unchanged. The
`publisher_sha256` and `implementation_sha256` values in the `a-encoder` and
`a-retained-train` manifests identify the publishing revision of each publisher
and are unchanged; no check reads them.
