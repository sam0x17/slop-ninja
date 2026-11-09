# Reference packages

SlopNinja publishes its own *A* models as reference packages. A package is
an admitted artifact's six files ([artifact spec](artifact-spec.md)) as the
scorer stores them, with the manifest that lists them, whose SHA-256 is the
package digest. A package has one of two layouts. In the parts layout
(`slop_ninja_a_artifact_files_v1`) each file is split into content-addressed
parts of at most 64 MiB, which the parts manifest lists. In the files layout
(`slop_ninja_a_artifact_files_v2`) the files manifest lists each file whole,
with the storage key and entity tag of the coordinator's copy. Beside them
are the package's licence notices and the synthetic reference cases its
export manifest binds.

The packages are published under the
[Creative Commons Attribution-ShareAlike 4.0](https://creativecommons.org/licenses/by-sa/4.0/)
licence (CC BY-SA 4.0). They go to the testnet public bucket, where public
access stays off until the announce. Until public access opens, the `R_0`
package and its [training companion](#training-companion) are attached to
the testnet release `testnet-v0.1.0` as
`slop-ninja-r0-reference-package-3c0ecc37.tar` and
`slop-ninja-r0-training-companion-3c0ecc37.tar`
([`R_0` archives](miner-guide.md#r_0-archives-optional)): R0's artifact spec
v3 package in the files layout, the package the `testnet-r1-` cohorts name.
The S2c span package is not downloadable until then. The [public layout](public-layout.md)
gives their paths.

## The two packages

| | `R_0` | S2c span package |
|---|---|---|
| Role | the testnet reference `R_0`, scored in every cohort | SlopNinja's first span-serving package (role `span_head`); never `R_0` |
| Model | R0, the released *A* document model | R0's encoder and document heads, unchanged, with the S2c span tower and token head |
| Graph class | `a-plain-f32band-v1` | `a-tower-f32band-v1` |
| Spec | v1 to v4 | v3 and v4 |
| `profile_id` | `slop_ninja_a_artifact_spec_v3/a-plain-f32band-v1` (the package the cohorts name); `slop_ninja_a_artifact_spec_v2/a-plain-f32band-v1` (the parts package) | `slop_ninja_a_artifact_spec_v3/a-tower-f32band-v1` |
| Package digest | `3c0ecc37376a9287f595f8389fe66faade00c663fd19f772eeb51abb7822c9fe` (spec v3, files layout: the package the cohorts name and the release attaches); `143ca2d95954e502a053fdd2c6deb6501e3e55273846dc04e310a3ee84c57f0c` (spec v2, parts layout); both hold the same six files byte for byte | `daff49c2232e176bb7d3ca793b9e3f8d36273845af77495450edcfc8a2486aef` (parts layout) |
| Inference-content digest | `d7232991612ae9693477a152d73e2b6ca5ac83b2d9cf5cd78e1eac4ecf96a36f` | `1681956c422fc6514cebd24870bb22071f11d55fd9ea4fa93ceb0bafb66a573c` |
| Graph SHA-256 | `08848d95…ca4cb7` (601,028,257 bytes) | `cecccf46…5478c3` (681,508,964 bytes) |
| Parameters | 150,302,832 | 170,592,385 |
| Parts | 14 in the parts layout; the files layout lists the six files whole | 16 |
| Synthetic reference cases | 3 | 215 |

**`R_0`** is R0, the released *A* document model, with its provenance
paths stripped: the package the coordinator names by content digest and
package digest from the chain-record restart on, as the spec v3 package
`3c0ecc37`. Its graph,
tokenizer, weights and calibration values and knots are R0's byte for byte;
only the location strings the tools recorded (the normalization's run
directories in `contract.json`) now keep their final component, and the
digests that bind the rewritten files are rebound
([provenance rewrite](#provenance-rewrite)). Its export manifest binds three
synthetic reference cases and their FP64 predictions.

**The S2c span package** is the span localization result chosen by the
frozen S2 record: arm `S2c-lr3e-5`, seed 17, a four-layer tower (copies of
encoder layers 18 to 21 and of the final norm) and a token head on its
states, with the decoder the record chose (`slop_ninja_span_decoder_v1`,
`tau_hi` 0.85, `delta` 0.2, `min_words` 10, `gap` 2, `snap` 0.8). Its
calibration keeps R0's document map and adds a sentence map (154 knots) and a
span map (35 knots). Its encoder and all 12 document-head tensors are R0's,
bit for bit, so its document outputs are R0's (measured below). Its export
manifest binds 215 synthetic reference cases: the exporter's 15 built-in
cases and the 200 cases of
[`a-public-reference-cases-v1.jsonl`](../../grammar/crates/grammar-eval/python/reference_cases/a-public-reference-cases-v1.jsonl),
written for publication. Neither package holds evaluation or corpus text.

The S2c package was exported by `export_full_a.py` at commit `a4ced4e7` (the
spans S2 branch, whose `full_a_bundle.py` is the bundle reader the fit binds,
`de9c49ea…`) with the `--synthetic-cases` option of commit `5589ba4b`
applied; its export manifest binds that exporter (`exporter_sha256`
`c35865c0…`). The exporter on main reads bundles by block, so its reader
digest differs from the bound one and it refuses this fit. The published
package is that export with its provenance paths stripped the same way as
`R_0`'s (the contract's run directories, the init fit's directory in the
manifest and three calibration provenance files), its document, sentence and
span outputs unchanged.

## Layout

```
v1/reference/packages/<package digest>/
  artifact-files.json                  the parts manifest, exact bytes (its SHA-256 is the package digest)
  parts/<sha256>                       the parts it lists, exact bytes
  package.json                         slop_ninja.public.reference_package/v1: role, digests, the file
                                       table, licence, notice paths and how to reassemble
  NOTICE.md                            the licence notice and attribution
  upstream/ModernBERT-base-LICENSE     the base model's Apache-2.0 licence
  reference/reference-requests.jsonl   the synthetic reference cases the export manifest binds,
  reference/reference-predictions.json and their FP64 predictions (and the FP32 diagnostic for S2c)
  training/companion.json              R_0 only: the training companion (below)
  training/parts/<sha256>              and the parts it lists
  provenance-rewrite.json              what the provenance rewrite changed (below); not a package file
```

The release's `R_0` archives use the files layout instead: `artifact-files.json`
is the files manifest, `files/<sha256>` holds each file whole, and there is no
`package.json`; the [miner guide](miner-guide.md#r_0-archives-optional) gives
the commands that check and unpack it. In the parts layout, rebuild a file by
concatenating its parts in the parts manifest's order and checking each
part's and the file's SHA-256. With the six files in one
directory:

```sh
slop_ninja_artifact_check --spec v3 check-dir DIR     # the graph class, parameters and inference-content digest above
slop_ninja_a_served_profile pack --spec v3 --model-dir DIR --out PARTS   # S2c's package digest above
slop_ninja_a_served_profile pack --spec v2 --model-dir DIR --out PARTS   # R_0's spec v2 package digest above
slop_ninja_a_local_score --model-dir DIR --docs reference/reference-requests.jsonl \
  --reference reference/reference-predictions.json --spans --out results.jsonl --ort-lib LIB
```

`check-dir` prints the inference-content digest (`content_sha256`), the graph
class and the parameter count, not a package digest. `slop_ninja_a_served_profile
pack` rebuilds a parts manifest under the spec's own `profile_id`: S2c's
package digest under spec v3, and the spec v2 `R_0` package's under spec v2
(other specs give other package digests for the same inference-content
digest). A files manifest also records where the coordinator's copy is
stored, so the spec v3 `R_0` package's digest is checked by hashing its
`artifact-files.json`, not rebuilt from the six files.

## Provenance rewrite

The tools that wrote both models recorded some local locations as absolute
paths (the normalization's run directories, an init fit's directory and
calibration source files), which name a home directory and private
directories. The published files keep only each location's final component.
Because the artifact spec requires the export manifest's
`fit_binding.contract_sha256` to equal the contract file's digest, that field
names the rewritten contract, not the contract the fit was bound to. Each
directory's `provenance-rewrite.json` records every rewritten pointer and
rebound digest, the files' digests before and after, the contract digest the
fit was bound to, and, for S2c, that its published `fit_binding` is not the
bytes `fit_hashes.binding_json_sha256` names and that its calibration-source
digest names the original R0 calibration file. It holds pointers and digests
only, and names the tool that made the rewrite and its rule by their source
digests: `grammar/crates/grammar-a-baseline/examples/rewrite_package_provenance.rs`,
which applies `grammar_a_baseline::provenance::portable_at_in` at the known
provenance pointers, rebinds the digests at exactly the pointers that held
them, checks that nothing else changed and copies every other file byte for
byte.

## Licence

The weights are SlopNinja's, published under CC BY-SA 4.0. Both models
derive from ModernBERT-base (Answer.AI and LightOn, revision
`8949b909ec900327062f0ebf497f51aef5e6f0c8`, Apache-2.0), fine-tuned by
SlopNinja; the S2c package adds the span tower and token head. Each
`NOTICE.md` credits the training-source collections and points to the
dataset snapshots' notices. The SlopNinja name and the logo in
`assets/brand/` are not covered by this licence or by the code licence.

The calibration files hold the involvement map's knots: one per calibration
document (5,379), each with that document's raw score, weight and label, and
no text or identifier.

## Training companion

`R_0`'s directory also holds `training/companion.json`
(`slop_ninja.public.reference_training_companion/v1`) and its parts: R0's
encoder (`model/encoder.safetensors`, SHA-256
`a167c6072e946be3ce23aa06a1f6ea0e0150dd29dafa3fb387980fe90c236927`) and its
classifier config (`runtime-base/config.json`), under the same licence. With
the package's `heads.json` and `contract.json` they form the release layout
that `export_a_onnx.py --release-dir` reads, and the encoder is what
`public_a.py parent --encoder` takes to warm-start the
[training guide's](training-guide.md#warm-start-from-r0) recipe from R0.

## Measured on November 2, 2026

On a Linux x86_64 host (Intel Raptor Lake, AVX2), ONNX Runtime 1.31.1 from the
pinned wheel:

- **Admission.** `R_0` passes spec v1, v2 and v3 in the plain class with the
  digests above; the S2c package passes spec v3 in the tower class, and spec
  v1 and v2 refuse it (`graph_size`). `slop_ninja_a_served_profile pack`
  reproduces both parts manifests byte for byte.
- **FP64 replay (R1).** The Rust FP64 runtime reproduced the S2c export's FP64
  references on its 215 synthetic cases, and on the 200 local development
  cases of a private twin export whose files are byte-identical apart from
  those references: every fixed gate passed, every offset equal.
- **Document outputs.** Through the FP64 runtime, R0 and the S2c package give
  bit-identical document outputs on R0's 3 references, 9 latency requests,
  the 215 synthetic cases and the 200 local cases. Through the scorer's f32
  graph, the two packages' predictions and output digests are equal on the 215
  synthetic cases, the 200 local cases and 16 container parity requests.
- **Span outputs.** The scorer against the S2c export's FP64 references on the
  215 synthetic cases: 208 spans with every offset equal, document outputs
  within 5.3e-5, sentences 5.0e-5, spans 2.3e-5, every attribution label equal
  ([scorer parity](../../inference/a-container-serve/README.md#span-head-parity)).
- **Corpus scan.** Every staged object went through the package-corpus scan
  (corpus row identifiers in each file, corpus text hashes in each JSONL
  file) and a byte-level search of every object, binary parts included, for
  each corpus row identifier both as a quoted JSON string and bare, as a
  length-prefixed graph string holds it: no object holds a corpus row
  identifier, no reference case is a corpus row (by exact text hash), and
  every staged reference case is one of the synthetic cases above. The scan
  records the SHA-256 of every object it read, and publication uploads exactly
  those objects: each one read back from the bucket must match its recorded
  digest.
