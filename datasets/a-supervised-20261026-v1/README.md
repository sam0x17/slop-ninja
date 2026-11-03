# Shared A supervised research cohort

This snapshot preserves **552 records from 85 source families**, including 88
examples with measured fractional AI share. The five JSONL files total
7,106,505 bytes (7.11 MB). They are the public projection of the completed
[shared CFG baseline](../../grammar/crates/grammar-a-baseline/SUPERVISED_BASELINE.md),
also used for the [recovered V6 comparison](../../docs/a-reference-comparison.md).
Publication does not alter the fitted models or their original input hashes.

| File | Records | Purpose |
| --- | ---: | --- |
| [train.jsonl](train.jsonl) | 229 | Fit the shared encoder and available heads |
| [development.jsonl](development.jsonl) | 106 | Checkpoint and experiment selection |
| [calibration.jsonl](calibration.jsonl) | 107 | Probability and named-profile calibration |
| [reference.jsonl](reference.jsonl) | 8 | Independent reference works; excluded from fitting and normalization |
| [test.jsonl](test.jsonl) | 102 | Already-opened, consumed retrospective evaluation |

This is an **exposed research population**, not a future hidden benchmark or a
clean generalization claim. Grammar-development exposure is retained. Historical
detector sources overlap existing snapshots; their counts are not additive.
References retain their original `frozen_split: train` assignment but are marked
`purpose: reference` and separated from training queries. Keep each source work
and every descendant in its original partition. Never randomly split excerpts,
merge reference/query works, or use the published Test for future selection.

Each `slop_ninja_a_supervised_record_v1` row preserves exact text and its SHA256,
source family, split, origin evidence, role-presence and identity masks, and
available fractional-share supervision. Historical human labels remain proxies,
not authenticated human-only production histories. Unknown supervision is null;
it is not an output category or a fabricated target. A complete model rewrite
of a human source remains mixed history even when replacement-span share is 100%.

Local partial rewrites retain the exact original text, source/replacement spans,
generated replacement, final assembly, word-count policy, and realized fraction
of final words inside replacement spans. Requested edit percentages are not
share targets. Generation requests and pinned generator identities remain;
live provider logs, filesystem paths, credentials and fitted profiles are absent.
The manifest records excluded records and original dataset/manifest hashes.

337 rows refer to exact records already published in the detector snapshots;
their source path, record ID, line checksum and file checksum bind the full
historical provenance. The other 215 are excerpts or local derivatives from 26
recorded public-domain author works. They retain author/title, source links,
acquisition hashes, byte spans, extraction policy and geographic rights scope.
Public author identities are source metadata, never learned feature values.

Local derivatives use `unsloth/Qwen3.6-35B-A3B-GGUF` at revision
`a483e9e6cbd595906af30beda3187c2663a1118c`, model digest
`649d7508507b84638732c4f52c24c8b15843c6dca2f3ff793ae07c14a67ebbb3`,
with `llama.cpp-b11756-CUDA12.8`; sampling and prompt details remain per record.
The [pinned model card](https://huggingface.co/unsloth/Qwen3.6-35B-A3B-GGUF/blob/a483e9e6cbd595906af30beda3187c2663a1118c/README.md)
declares Apache-2.0. Model license, text rights and training permission are
distinct; the generator's license does not replace underlying source rights.

Preserve each row's attribution, change notices and license links. Historical
sources retain CC BY 2.5/4.0 and CC BY-SA 3.0/4.0 obligations, including the CMU
collection's CC BY-SA 3.0 US notice and David Bamman/Noah Smith attribution.
The [V6 notices](../detector-v6/README.md),
[origin snapshot notices](../detector-origin-v1/README.md),
[revision notices](../detector-revisions-v1/README.md), and
[Fix Slop MIT notice](../detector-v6/LICENSE.fix-slop) remain applicable to the
linked source records and adapted prompts. Author excerpts rely on recorded
**US** public-domain determinations; this snapshot does not expand that scope.
Project Gutenberg branding is excluded from these excerpts. New generated text
and curation are offered under
[CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/), to the extent
rights exist and subject to underlying rights and generator terms. The code
license does not replace these text terms. See the
[model ShareAlike policy](../../baseline/detector/SHARE_ALIKE_POLICY.md).

The Rust `publish_supervised_snapshot` example verifies source-file and exact
record bindings, text hashes, targets, span counts, parent links, partition
boundaries and an explicit publication field allowlist. It does not reopen the
source acquisition archives or independently establish source rights. Its six
synthetic tests cover forged provenance, unsafe paths, leaked credentials,
null targets, rewrite evidence and symlinks. To check this copy:

```sh
sha256sum -c SHA256SUMS
```

To reproduce from retained original manifests on build-host, build the example
in the capped task-owned Cargo job, then run:

```sh
publish_supervised_snapshot REPO NON_TEST_RECORDS NON_TEST_MANIFEST \
  OPENED_TEST_RECORDS OPENED_TEST_MANIFEST NEW_DATASETS_CHILD
```

Original evidence, numeric exports, models, and saved Pangram observations
remain under ignored task storage. Pangram outputs are comparator observations,
never authorship or share ground truth. No paid call is needed to read or
validate this snapshot.
