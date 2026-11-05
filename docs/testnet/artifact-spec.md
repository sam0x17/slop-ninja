# A artifact spec

Three versions are defined. **Spec v1** (`slop_ninja_a_artifact_spec_v1`) is
the first; this page describes it, and its values and digest are unchanged.
**Spec v2** (`slop_ninja_a_artifact_spec_v2`) keeps every v1 value and adds
span heads; [Spec v2](#spec-v2) lists exactly what it changes. **Spec v3**
(`slop_ninja_a_artifact_spec_v3`) keeps every v2 value for the plain and span
classes and adds the span tower class; [Spec v3](#spec-v3) lists exactly what
it changes. A cohort names the spec it checks by id and digest, and the
scorer admits each of the cohort's artifacts by that spec: a load names it
by digest, and a load that names none is admitted by spec v2. The scorer
admits by spec v2 or v3, never by spec v1, which bounds no JSON file, so an
artifact spec v1 admits can still be refused when its JSON exceeds spec v2's
budgets. Testnet cohorts opened by the standing template name spec v3.
Spec v3 admits every artifact spec v2 admits, with the same results, and adds
the tower class, which only a cohort that names spec v3 admits.

Spec v1 defines what a miner submits for
*A* on testnet 601, what admission checks before any runtime opens a file, and
how the coordinator hands an admitted artifact to the scorer. The checks are
implemented in [`subnet/crates/artifact-check`](../../subnet/crates/artifact-check)
(`slop_ninja_artifact_check`), and the graph export in
[`export_a_onnx.py`](../../grammar/crates/grammar-eval/python/export_a_onnx.py).

Every bound below comes from a schema, an allow-list, the measured reference
artifact R0 or a limit of the runtime that reads the file. None is tuned. A
change to any value is a new spec version, and `slop_ninja_artifact_check spec`
prints the whole spec as one JSON record with its digest.

`slop_ninja_artifact_check` exits 0 on success and 2 on a refusal, which it
prints as `refused <code>: <detail>`; a path it cannot read is refused as
`object`. It exits 64 on a usage error, and `--help` prints the usage and
exits 0. It checks spec v1 unless given `--spec v2`, `--spec v3` (or a full
spec id) before the command; `--version` prints every spec id it defines with
its digest.
`canonical DIR OUT` writes a copy of a model directory with its JSON files
re-encoded in their canonical layouts and the digests that bound the old bytes
rebound, then checks the copy; it is for exporters whose writers use other
layouts.

## One format: the artifact object

A miner submits one **artifact object**: the protocol encoding from
`slop_ninja_protocol::artifact_object`.

- Layout: `u32_le(len(H)) || H || files`.
- `H` is an `artifact-header` v1 record. It lists each file's name, role,
  SHA-256 and length, sorted by name, so one set of files has exactly one
  object.
- The miner commits on chain to the object's **inference-content digest**: the
  contract plus the inference files by role, digest and length.
- The object is the miner's local file and the published form, but it is not
  uploaded whole. Admission v2 ([admission API](admission-api.md)) uploads the
  header bytes `H` and each distinct file as separate objects, each with one
  presigned R2 `PutObject` that binds its committed SHA-256: the header's for
  `H`, each file's own for the file. R2 verifies every object against that
  digest as it arrives. The miner's `pack` hashes each file once; nothing
  after it hashes a model byte (the graph class check is a byte comparison,
  [Structure stream](#structure-stream)). Anyone can rebuild the object by
  concatenating `u32_le(len(H))`, `H` and the files in header order.

The header contract must be `slop_ninja_full_a_contract_v2`. The object holds
exactly these six files:

| File | Header role | Scorer role | In content identity |
|---|---|---|---|
| `a-fp32band.onnx` | Graph | `encoder_graph` | yes |
| `heads.json` | Weights | `heads` | yes |
| `contract.json` | OutputSchema | `contract` | yes |
| `calibration.json` | Calibration | `calibration` | yes |
| `manifest.json` | OwnerMetadata | `export_manifest` | no |
| `tokenizer.json` | Tokenizer | `tokenizer` | yes |

`manifest.json` is the export record. The scorer's load guard reads it, but no
prediction depends on it, so it is owner metadata. Any other file is refused
as undeclared. That includes external-data sidecars, notes and a second graph.

### Mapping to the scorer's files manifest (v2)

The scorer loads an admitted artifact file by file, straight from object
storage, under a files manifest (`slop_ninja_a_artifact_files_v2`,
`subnet/crates/artifact-check/src/files_manifest.rs`, read by
`inference/a-container-serve/src/artifact.rs`). The coordinator derives it from
the header and the objects admission recorded. The mapping is exact, so there
is no second upload format.

| Manifest field | Value |
|---|---|
| `schema` | `slop_ninja_a_artifact_files_v2` |
| `profile_id` | `slop_ninja_a_artifact_spec_v1/<graph class id>` (the spec version's id) |
| `contract` | the header contract |
| `graph` | `a-fp32band.onnx` |
| `files` | the six files in the table order above (the scorer's order, not the header's), each with the header's name, size and SHA-256, the scorer role, and where it is stored: `bucket` (`admission` for a miner's upload, `eval` for an operator-staged reference package), `key` and the `etag` admission recorded |
| bytes | `serde_json` pretty printing with a final newline |

The package digest is the SHA-256 of the manifest bytes (a small record).
A load moves each file in ranged, etag-pinned reads of at most 64 MiB (the
scorer's `MAX_RANGE_BYTES`, the former part size) written at their offsets,
and hashes nothing: the digests are the header's, which R2 verified at
upload. The files manifest v1 (`slop_ninja_a_artifact_files_v1`, a file's
consecutive 64 MiB parts each with its own SHA-256) remains the layout of the
staged [reference packages](reference-packages.md).

The mapping reproduces R0's deployed package byte for byte (`2f88b0d2…678559`)
when given that package's `profile_id` (`a-baseline-rc-s17-f32band-v1`). Under
this spec, R0's files map to `profile_id`
`slop_ninja_a_artifact_spec_v1/a-plain-f32band-v1` and package `3d54a7d2…3aa9a7`.
R0's inference-content digest is `63a0e608…c2a0a6`. The scorer refuses a
`profile_id` that no packer writes (`refused profile_id`): it must be
`<spec id>/<graph class id>` for a version and class the tools define, or the
served profile's id. A reference packed for a cohort keeps R0's deployed
package digest only through `slop_ninja_a_served_profile pack`, which writes
the served profile's id. R0's deployed package is one of Slop Ninja's
[reference packages](reference-packages.md), published under CC BY-SA 4.0
when public access opens at the announce; with its files, anyone can check
the R0 digests on this page and run the R0 tests below.

## Checks, in order

Admission checks the header first (in the key-free upload Worker,
`admission::check_header_bytes`), then the scorer runs the files check
(`files::check_files_with`) on the stored files before it loads them. The
graph and the tokenizer are never buffered. Each of the four JSON role files
is buffered at exactly the length the header declares. A tool that holds a
whole object (the miner's self-test) uses `ObjectCheck`, which streams it and
also checks every file against its header digest.

1. **Files.** Each stored file's size and SHA-256 are R2's (verified at
   upload) and must equal the header's; no file is hashed again. The header
   is checked against its schema.
2. **File table.** It checks the contract id, exactly the six names, each
   name's role, the tokenizer byte-match and every declared length (see
   [Lengths](#lengths)). All of these are decided from the header alone,
   before any file byte is read.
3. **Graph pre-parse** while the graph streams (see the next section).
4. **Graph class.** The graph's structure stream must equal, byte for byte,
   the pinned reference stream of the served class of its length.
5. **Role schemas and bindings** (see [Role schemas](#role-schemas)). The
   export manifest's digests of the heads, contract and calibration files,
   and the load guard's contract digest, are compared with the header's
   digests, never with a fresh hash of the files.
6. **Parameter bound.** Graph `FLOAT` parameters plus head weights must be at
   most 160,000,000 (spec v3: the graph class's bound, which is 160,000,000
   for the plain and span classes and 173,549,185 for the tower class). The
   graph walk enforces its own share early, at the start of the payload that
   would pass the bound.

Every refusal has a stable code, such as `undeclared_file`, `file_size`,
`tokenizer_mismatch`, `json_layout`, `schema`, `binding`, `graph_size`,
`graph_custom_domain`, `graph_external_data`, `graph_op_not_allowed`,
`too_many_parameters`, `graph_not_allowed`, `graph_class_not_served`,
`graph_class`, `json_budget` or (spec v2) `json_value_budget`.

### Lengths

No byte cap is set for its own sake. Each declared length is checked against
the runtime that reads the file:

| File | Length | Source |
|---|---|---|
| `a-fp32band.onnx` | exactly a served class's length (601,028,257 for the plain class) | the structure stream keeps every byte except the initializer float payloads, and keeps their lengths, so a class fixes its file length |
| `tokenizer.json` | exactly 3,583,228 | the pinned tokenizer |
| `heads.json` | 1 to 67,108,864 (64 MiB) | the heads runtime refuses larger heads (`full_a.rs` `FullAHeads::from_json`) |
| the other three JSON files | 1 to 4,294,967,296 (64 ranges of 64 MiB) | the scorer refuses an empty file and more than 64 ranges per file (`artifact.rs` `MAX_CHUNKS_PER_FILE`) |

The files manifest must also fit the scorer's 1 MiB `MAX_MANIFEST_BYTES`; with
six files it always does. Because the graph's length is
fixed, an admitted graph cannot reach the parameter bound: at four bytes per
float, a file of 601,028,257 bytes holds fewer than 160,000,000 floats, and
the plain and span classes hold exactly 150,058,763. The tower class of
spec v3 holds exactly 170,151,195, inside its own bound.

Every JSON role file must be in its canonical layout (see
[Canonical JSON layouts](#canonical-json-layouts)), so its length is a
function of its content: whitespace, number spelling and escape padding are
refused. Free-text provenance strings and lists (`limits`, normalization
`runs`) are still free; their size is bounded only by the declared length.

The buffer `ObjectCheck` needs for the JSON files is the sum of their declared
lengths (`mapping::json_buffer_bytes`), known from the header. A caller with a
memory budget, such as a Workers arena, passes it to
`ObjectCheck::with_json_budget`; a header that needs more is refused with
`json_budget` before any file byte is read. The budget is the caller's
platform limit, not a spec value.

## Graph

### Pre-parse

The pre-parse is a streaming protobuf walk over the subset of `onnx.proto` the
allow-listed graphs use. It runs before ONNX Runtime opens the graph. It
refuses each of these:

- any field outside the subset (unknown fields included);
- a node or opset domain other than the default domain;
- model-local functions;
- training information and device configurations;
- external data: `data_location` EXTERNAL or any `external_data` entry;
- sparse initializers and quantization annotations;
- subgraph, sparse and type-proto attributes;
- string tensors and tensor segments;
- operators outside the allow-list;
- element types other than `FLOAT` and `INT64`;
- attribute types other than FLOAT, INT, STRING, TENSOR and INTS;
- a tensor whose payload length differs from its shape;
- non-finite float values;
- a data field before `data_type`, a repeated `data_type`, unpacked
  `float_data`, and malformed varints or lengths.

The operator allow-list is the 34 default-domain operators of R0's graph at
opset 17:

`Add`, `Cast`, `Concat`, `Constant`, `ConstantOfShape`, `Cos`, `Div`,
`Equal`, `Erf`, `Expand`, `Gather`, `Gemm`, `LayerNormalization`,
`LogSoftmax`, `MatMul`, `Mul`, `Neg`, `Pad`, `Range`, `ReduceLogSumExp`,
`ReduceSum`, `Reshape`, `Shape`, `Sigmoid`, `Sin`, `Slice`, `Softmax`,
`Split`, `Squeeze`, `Sub`, `Tanh`, `Transpose`, `Unsqueeze`, `Where`.

### Structure stream

The structure stream (`slop_ninja_onnx_structure_stream_v1`) is the graph
file's bytes with the payload of every FLOAT initializer data field removed:
the `raw_data` or `float_data` of `GraphProto.initializer` tensors. Each
field's tag and length prefix stay in the stream. Float tensors inside
`Constant` nodes stay like every other byte.

A graph is of a class when its structure stream equals the class's pinned
reference stream byte for byte. The walk compares the stream as it produces
it, with the reference of the class of the graph's length (every class has its
own length); the removed payloads never reach the comparison, though the walk
still reads them to check that every value is finite. Nothing is hashed
(decision D5): `subnet/tests/no_object_hash.rs` fails if a hash reaches the
walk, the class check or the scorer's and the miner's paths to them. A
refusal (`graph_not_allowed`) names the class of the graph's length and the
stream offset of the first differing byte. The repository's graph exporter
(`export_a_onnx.py`), a miner tool run before pack, still checks its own
output against the class's structure digest; that check runs before the
commitment and is the miner's own, so from pack on nothing hashes a model
byte but the pack itself.

The reference streams ship with the spec in
`subnet/crates/artifact-check/structure/`, compiled into the scorer and the
miner. `slop_ninja_artifact_check structure FILE OUT` writes a graph's stream;
each class's stream is its reference export's:

| Class | Reference stream bytes | SHA-256 of the stream file |
|---|---|---|
| `a-plain-f32band-v1` | 1,623,305 | `69b149ce63a044efa0de648bd34f8d179f694e89e8ef9bd2ad4dd828faa692a3` |
| `a-span-f32band-v1` | 1,623,451 | `e8172eedeb643e33eeea850cb4c138907f9fe258f92071a0cf1dfcb4caf6a68b` |
| `a-tower-f32band-v1` | 1,860,300 | `55abdd78664fa52446f3beeeef16e8f4f7a82334f0809992a4c8731030865fbc` |

Spec v1 to v3 state the same rule as a digest, `slop_ninja_onnx_structure_v2`:

```
SHA-256( "slop_ninja_onnx_structure_v2\0" || structure stream )
```

Each class's digest in those records is its reference stream's (a spec test
recomputes it from the shipped stream), so the byte comparison, which every
version's check runs, accepts exactly the graphs the digest accepts. Spec v4
states the rule as the comparison and pins the streams by length and SHA-256
([Spec v4](#spec-v4)).

Two graphs have the same structure stream exactly when their bytes are
identical apart from their initializer float values. Identical means the same:

- operators, node order, names and attributes;
- shapes and integer constants;
- float constants: the 14 folded `[64, 192]` band patterns of the local
  layers, the rotary tables, the attention scale and the scalar that sets the
  padding mask's polarity;
- opsets, IR version and producer fields.

A miner can change the initializer values and nothing else. In R0 these are
the 150 initializers: 136 of the 138 encoder tensors (all but the classifier,
which the graph does not read), the 12 head tensors and the heads' text mean
and scale, 149,851,238 values. R0's other 163 float tensors are `Constant`
node tensors (207,525 values) and are fixed by the class.

Every class keeps each weight as its own initializer (the tower class adds
the 25 tower tensors), so no two weights may have equal values. The exporter merges initializers of the same type
and shape whose values are equal into one (the TorchScript exporter's
initializer deduplication; it compares values, so 0 and -0 count as equal),
which gives a graph of another structure and length, refused `graph_size`.
The likely case is a span tower tensor left equal to the encoder tensor it was
copied from, such as a norm or layer that was frozen or never trained. The
[exporter](#export) refuses such a model before it traces the graph and names
the equal tensors.

The walk also refuses a tensor with a repeated `data_type` (a payload left out
of the stream as one type and read as another), and counts a tensor whose packed
`float_data` is split over several fields once.

### Allow-listed graph classes

| Class | Structure digest | Bytes | Served (v1) | Served (v2) | Served (v3) | Outputs | Heads (v1) |
|---|---|---|---|---|---|---|---|
| `a-plain-f32band-v1` | `b5440ec244c6dc519b961560b9cc69201652ec617c7da90ba0bb36fd4c42a731` | 601,028,257 | yes | yes | yes | `origin_logits`, `involvement_logit`, `conditional_mixed_share`, `ai_logits`, `human_logits`, `h` | `span_head` null |
| `a-span-f32band-v1` | `3a35507f1233803d5dc5355594d9ec8c0f292d52ac3d6f06df61c259441c2f6a` | 601,028,403 | no | yes | yes | the same, plus `token_states` | `span_head` set |
| `a-tower-f32band-v1` | `0b1feeb0b90d79d77999993958fdbfd0b033a6d1d9c29d6b08605451839e3b9c` | 681,508,964 | not defined | not defined | yes | the plain outputs, plus `span_tower_token_states` | heads v3 only ([Spec v3](#spec-v3)) |

Every class is the banded f32 ModernBERT-base *A* graph:

- 22 layers;
- banded local attention in 64-query blocks;
- inputs `input_ids` and `attention_mask` (INT64, dynamic batch and sequence),
  padded to a multiple of 64.

The scorer reads `h` for the document heads. The span class adds the
final-norm token states (`[batch, seq, 768]`) that a span head reads in the f64
heads runtime. Both classes are allow-listed. Spec v1 does not serve the span
class: it refuses it with `graph_class_not_served`, from its length in the
header. Spec v2 serves it, together with the heads that read its token states
([Spec v2](#spec-v2)).

The tower class, defined by spec v3 only, is the plain graph plus a span
tower: copies of encoder layers 18 to 21 (global, local, local and global
attention) and of the final norm, run from the hidden state entering layer 18
with the encoder's masks and positions. It outputs the tower's normed token
states (`[batch, seq, 768]`), which a heads v3 token head reads in the f64
heads runtime. Its document outputs come from the same path as the plain
class's. Under spec v1 and v2 its length is no class's, so it is refused as
`graph_size`.

The plain digest is R0's graph (`a-fp32band.onnx`, SHA-256
`08848d95…ca4cb7`): 4,712 nodes, 150 initializers and 150,058,763 float
parameters. The span digest is the span export of R0's weights. The tower
digest is the tower export of the S2c span package (R0's encoder and document
heads with the frozen S2c tower): 5,458 nodes, 175 initializers and
170,151,195 float parameters.

The graph also contains the document heads that the scorer recomputes from
`heads.json`. So its structure fixes the roster sizes at 33 AI profiles and
none for humans. The empty human roster keeps a single placeholder row.

### Export

`export_a_onnx.py --release-dir DIR --variant plain|span|tower --out FILE`
exports a release-layout model (`runtime-base/config.json`,
`model/encoder.safetensors`, `model/heads.json`, `model/contract.json`, and
for the tower variant `model/tower.safetensors` with heads v3). It writes
only the graph and its
record; the other five artifact files come from the release layout. The
trainer does not write that layout: it writes a fit directory (`selected.pt`).
Between the two come calibration on the calibration split (the Rust
`grammar-a-baseline` calibrator) and the release exporter, and after the
graph export the six files are assembled into one directory. The
[training guide](training-guide.md) gives the whole command sequence from
ModernBERT-base and public data, with a pinned Python environment for
training and export. The export uses R0's recipe:

- `torch.onnx.export` with the TorchScript exporter (dynamo off);
- opset 17 with constant folding;
- banded eager attention;
- a trace at one 256-token input with dynamic axes.

Before export, the script checks every loaded encoder and head tensor bit for
bit against the release files. The one exception is the empty human roster's
placeholder row, which the release does not contain; the script writes zeros
there. It then refuses a model in which two of the tensors that become
initializers (the encoder tensors the graph reads, the head tensors with that
placeholder row, the heads' text mean and scale, and the tower tensors) have
equal values, and names them, because the export would merge them
([Structure stream](#structure-stream)).

After export, it validates every output against the unbanded torch model with
ONNX Runtime, on synthetic token ids of 64 to 2,048 tokens. It writes a record
(`slop_ninja_a_onnx_export_v1`) holding the versions, input digests, graph
digest, structure digest and validation errors. With `--compare-graph`, the
record also lists the float tensors that differ from a reference graph.

Structure depends on the exporter and model-code versions. The script
therefore refuses to run outside its pinned environment: Python 3.12,
torch 2.9.1 and transformers 4.57.6, the environment that produced R0. To run
elsewhere, pass `--allow-unpinned`.

The record says `qualified: true` only when all of these hold, and lists the
failures otherwise:

- the environment is the pinned one;
- the structure digest equals the variant's allow-listed class digest (the
  script's `CLASS_STRUCTURE`, which a spec test compares with the crate);
- every validation output is finite.

A structure or finiteness failure also exits with status 1 after the record
is written. Validation errors are recorded, not gated, because they depend on
the weights.

The record is evidence, never an authority. Only the Rust walk decides
admission. The script's own digest (`export_a_onnx.py structure FILE`) is an
independent implementation for the record: it refuses a repeated `data_type`
and data before `data_type`, so the two digests cannot disagree on a tensor
layout, but it checks nothing else.

Measured on October 31, 2026, on a Linux x86_64 host, from R0's release
package in the pinned environment (`qualified: true`). The pinned environment
is Python 3.12.15, torch 2.9.1+cu128, transformers 4.57.6, onnx 1.23.2,
onnxruntime 1.31.1 and numpy 2.5.4, run on the CPU.

| | Plain | Span |
|---|---|---|
| Structure digest (v2), Rust walk and Python walk | `b5440ec2…42a731`, equal to R0's | `3a35507f…1c2f6a` |
| Graph bytes | 601,028,257, R0's size | 601,028,403 |
| Float parameters | 150,058,763, R0's count | 150,058,763 |
| Largest error of `h` against unbanded torch, 64 to 2,048 tokens | 5.8e-5 | 5.8e-5 |
| Largest error of `token_states` | | 3.7e-3 (at 2,048 tokens) |

- **Bytes.** The plain export differs from R0's bytes only in the human
  placeholder row: 2 of R0's 313 float tensors (`human.weight` and
  `human.bias`, both initializers). The comparison covered all 2,370 tensors
  of every type (150 initializers and 2,220 `Constant` tensors). R0 kept an
  untrained placeholder there; the export writes zeros. The structure digest
  is therefore R0's, and the file digest is not.
- **Reproducible.** A second run of the current script, about two hours
  later, wrote byte-identical graphs (plain `fbabd7c0…f8739`, span
  `140f9303…d55f2`), and both records say `qualified: true` under the
  class-digest rule.
- **Span head error.** The `token_states` error bounds the input of the f64
  span head. Span-head parity in the scorer is still to be qualified by the
  scorer stream.
- **Independent digest.** `export_a_onnx.py structure FILE` computes the
  digest without the Rust crate. On R0's graph it gives the same value as the
  Rust walk.

The tower variant was measured on November 1 and 2, 2026, in the same
environment, from the frozen S2c fit's release layout (heads v3 and
`tower.safetensors`). The script checks the heads' `span_tower` declaration
against the encoder and every tower tensor bit for bit; the token head's
weights stay in `heads.json`, outside the graph.

| | Tower |
|---|---|
| Structure digest (v2), Rust walk and Python walk | `0b1feeb0…839e3b9c` |
| Graph bytes | 681,508,964 |
| Float parameters | 170,151,195 (169,912,166 in 175 initializers, 239,029 in constants) |
| Operators, element types, attribute types | R0's: 34 default-domain operators, `FLOAT` and `INT64`, attribute types FLOAT, INT, STRING, TENSOR and INTS |
| Largest error of `h` against unbanded torch, 64 to 2,048 tokens | 5.8e-5, as the span export's |
| Largest error of `span_tower_token_states` | 3.1e-3 (at 2,048 tokens) |

Two runs from the uncalibrated export's release layout and one from the
calibrated package's wrote byte-identical graphs (SHA-256 `cecccf46…5478c3`):
the graph depends only on the encoder, the document heads and the tower, not
on the calibration.

## Tokenizer

`tokenizer.json` must be byte-identical to the pinned tokenizer: SHA-256
`6c8aaa9a542084f2457eab775d4eeb51f92a70c0fd9de28d5edb0ddec3c08d30`,
3,583,228 bytes. It is the tokenizer R0 was trained and exported with, and the
same bytes are R0's release `runtime-base/tokenizer.json`.

It is the public ModernBERT-base tokenizer, re-serialized. The upstream file,
`tokenizer.json` of
[`answerdotai/ModernBERT-base`](https://huggingface.co/answerdotai/ModernBERT-base)
at revision `8949b909ec900327062f0ebf497f51aef5e6f0c8` (SHA-256
`9fd55248d51d33976b324fc11592e28071da7d41e0e9401dfb7082e30574b7b1`,
2,132,967 bytes), is written compactly, so it is refused as it is
(`tokenizer_mismatch`). Loading it with tokenizers 0.22.2 and saving it
pretty-printed reproduces the pinned bytes:

```sh
REV=8949b909ec900327062f0ebf497f51aef5e6f0c8
mkdir -p upstream
curl -fL -o upstream/tokenizer.json \
  https://huggingface.co/answerdotai/ModernBERT-base/resolve/$REV/tokenizer.json
python3 -c 'import sys; from tokenizers import Tokenizer; Tokenizer.from_file(sys.argv[1]).save(sys.argv[2], pretty=True)' \
  upstream/tokenizer.json tokenizer.json
sha256sum tokenizer.json   # 6c8aaa9a542084f2457eab775d4eeb51f92a70c0fd9de28d5edb0ddec3c08d30
```

Measured on November 1, 2026, with tokenizers 0.22.2 (the version in the
exporter's pinned environment). Use the pinned revision rather than the
upstream `main` branch, which can change, and check the digest: another
tokenizers version may write other bytes. The training guide's parent step
rebuilds the file the same way and refuses any other bytes.

## Role schemas

Each JSON file must first be in its canonical layout. It is then parsed into a
typed schema that refuses unknown fields:

- Fields that define what the model reads and returns are pinned, and so is
  the calibration policy.
- Provenance fields are typed: lowercase SHA-256 digests, flags, counts,
  normalization run shards (integers) and the trainer's arm definition
  (`train_full_a.py` `validate_arm`, field for field). Free-text provenance
  strings and lists are free.
- `heads.json` is also parsed by the scorer's own heads runtime:
  `full_a.rs`, `full_a_spans.rs`, `involvement_calibration.rs` and
  `shared.rs` at the same repository revision. The scorer compiles the same
  four files, and its served profile pins the same digests.

### Canonical JSON layouts

Each JSON role file must be byte-identical to the canonical serialization of
its own parsed value, with a final newline. The layouts are the ones R0's
writers use:

| File | Layout | Writer |
|---|---|---|
| `heads.json`, `manifest.json` | `json.dumps(v, sort_keys=True, separators=(",", ":"), ensure_ascii=True, allow_nan=False)`: compact, keys sorted, non-ASCII escaped, floats as Python `repr` | the release exporter (`v6_embeddings.py` `canonical`) |
| `contract.json`, `calibration.json` | `serde_json::to_vec_pretty` of a `Value`: two-space indent, keys sorted, raw UTF-8, floats as `serde_json` 1.0.146 and later writes them (`1e-7`, `1e+16`) | `grammar-a-baseline` contract and calibration tools |

Both float spellings use the shortest digits that round-trip. Python's
`repr` uses fixed notation when the decimal point position is in -3..=16 and
otherwise writes at least two exponent digits (`1e-05`); `serde_json` uses
fixed notation for -4..=16 and writes no exponent padding (`0.00001`,
`1.5e-7`). The crate defines both spellings itself rather than calling a
formatting library, because `serde_json` changed its spelling of large
exponents (`1e16` before 1.0.146, `1e+16` after); tests compare them with
CPython and with the current `serde_json`.

The check is one pass over the bytes. It builds no value and holds only the
previous key of each open object. Integers must fit `i64` or `u64`, and
nesting is limited by `serde_json`'s recursion limit (128). R0's four files
pass, and re-encoding each parsed file reproduces it byte for byte: about
270,000 floats in the Python spelling and 45,000 in the `serde_json`
spelling. `canonical::encode` writes a value in a role's layout for tools.

| File | Schema | Pinned |
|---|---|---|
| `contract.json` | `slop_ninja_full_a_contract_v2`, all 17 fields typed, the embedding normalization's 22 fields typed | input mode `text`; raw dimension 768; 2,048 tokens including specials; no truncation; origin order human_only, model_only, mixed; span coordinates `utf8_byte_offsets_in_exact_final_text`; span label policy `slop_ninja_token_span_label_v1`; 1,820 zero CFG coordinates; the AI roster rule; an empty human roster; the 33 AI profile names ([listed below](#ai-profile-names), pinned by digest); normalization schema and dimension; `final_test_opened` false |
| `heads.json` | `slop_ninja_full_a_heads_v1`, R0's schema (the heads runtime also reads v2, which adds the spans token head; the spec does not admit it) | architecture 2588, 768, 64, 768, 0 human and 33 AI profiles; supervision keys; each weight's shape matches its values, which are finite |
| `calibration.json` | `slop_ninja_full_a_calibration_v2`, all 20 fields typed, including the fit report, its policy and both role records | complete; fit split `cal`; map `isotonic_v1_clipped` with its algorithm and interpolation; the policy `slop_ninja_involvement_calibration_policy_v3` with its primary, fallback, selection, bound and temperature rule; knots exactly `{involvement, rows, score, weight, weighted_positives}`, sorted, monotone and inside `[lo, hi]` (heads runtime); at most `cal_rows` knots; report support summing to `cal_rows`; `final_test_opened` false |
| `manifest.json` | `slop_ninja_full_a_export_v2`, all 22 fields and the 22-field fit binding typed, with the arm definition typed as the trainer validates it | complete and calibrated; the arm definition's value rules; the file names of heads, contract and calibration |

The contract's 1,820 CFG coordinates are pinned at zero and carry no input;
*A* reads raw text.

`cal_rows` is the miner's own count, so "at most `cal_rows` knots" does not
bound the knot list by itself. The knot list is bounded because the heads
carry it and `heads.json` is at most 64 MiB; the calibration's two copies must
equal the heads' list.

### AI profile names

`contract.json`'s `ai_profiles` must be exactly these 33 names in this order,
and so must the heads' AI roster. The checker pins the SHA-256 of their
compact JSON array (`json.dumps(names, separators=(",", ":"))`):
`2e1c6f39d0f313959a43c6478843934633fb812ad0b109c4245faeb6ee37fa55`. A spec
test checks this list against that digest. The graph's AI head has one row per
name, so the count is fixed by the graph class. A name with no training rows
stays in the roster, untrained.

```json
[
 "claude-fable-5-1",
 "claude-haiku-5-5",
 "claude-opus-5",
 "claude-opus-5-5",
 "claude-sonnet-5",
 "claude-sonnet-5-5",
 "cohere-command-chat",
 "cohere-command-generate",
 "dipper-paraphraser-xxl",
 "ernie4.5",
 "gemma3",
 "gpt-3.5-turbo-0613",
 "gpt-4-0613",
 "gpt-oss",
 "gpt2-xl",
 "granite",
 "llama-2-70b-chat",
 "mistral",
 "mistral-7b-instruct-v0.1",
 "mistral-7b-v0.1",
 "mpt-30b",
 "phi4",
 "qwen3.6",
 "requested:gpt-5.6-luna",
 "requested:gpt-5.6-sol",
 "requested:gpt-5.6-terra",
 "requested:gpt-6-astra",
 "requested:gpt-6-luna",
 "requested:gpt-6-sol",
 "requested:gpt-6.1-sol",
 "requested:gpt-6.1-terra",
 "smollm3",
 "text-davinci-002"
]
```

### Bindings

The scorer's load guard (`engine.rs` `guard`) is evaluated with its own
predicates (`guard.rs`): the same expressions on `serde_json::Value` views of
the same files. So the comparisons have the scorer's semantics; for example,
an integer threshold `1` in the calibration does not equal a float `1.0` in
the heads, and the artifact is refused as the scorer would refuse it. Each
number is first rebuilt from its value, as the scorer's build holds it, so
equal floats compare equal however each layout spells them (`1e-05` in the
heads, `0.00001` in the calibration). That keeps the comparisons the same in a
build that enables `serde_json`'s `arbitrary_precision`, as the miner client
and the coordinator do through rtao, where numbers otherwise keep their source
text. The guard checks:

- the export is complete and calibrated, and the contract's input meaning;
- the heads and the calibration both name `contract.json`'s SHA-256;
- the calibration's arm, seed and checkpoint equal the export manifest's;
- the calibration's selected map equals the heads map;
- each role is either an empty roster with threshold 1, or a qualified roster
  with the heads threshold;
- the heads rosters equal the contract rosters;
- the heads carry no span head.

The spec pins the SHA-256 of the scorer's `guard` source
(`SCORER_GUARD_SHA256`). When the scorer's guard changes, a spec test fails
until the mirror is re-derived and the pin updated.

The checker also adds:

- the export manifest's SHA-256 values for the heads, contract and calibration,
  and the fit binding's contract digest, must match the files;
- the arm definition's span head and share mode must agree with the heads;
- the heads' span head must agree with the graph class.

## Canonical text profile

Profile id `slop_ninja_a_text_profile_v1`. It records how the scorer turns a
document into model input today. It was derived from
`inference/a-container-serve` (`engine.rs` `Model::score`, `main.rs`
`predict`) and the pinned tokenizer, then compared with the trainer
(`full_a.py` `hf_tokenize`). It describes and does not choose: where the two
differ, the difference is listed as a finding.

| Aspect | Scorer behaviour |
|---|---|
| Bytes | A document is a JSON string, so it is valid UTF-8. Invalid UTF-8 or a lone-surrogate escape fails the whole score request, not one document. |
| Digest | `text_sha256` is the SHA-256 of the exact bytes received. |
| Empty | `text.trim().is_empty()` (Rust, Unicode `White_Space`) gives `unsupported_input` / `empty_text` and no prediction. |
| Unicode normalization | NFC, by the tokenizer's normalizer, inside tokenization only. The text is not rewritten. |
| Whitespace | Kept, with no stripping or collapsing. The byte-level pre-tokenizer encodes every byte, and space runs can match the tokenizer's whitespace added tokens. |
| Invisible characters | Kept: zero-width characters, joiners, bidi controls, BOM and soft hyphen. NFC leaves them, and they become byte tokens. |
| Added and special tokens | Matched in the raw text before BPE, special tokens included. A literal `[SEP]` in a document becomes the SEP id. |
| Frame | `[CLS]` + tokens + `[SEP]` |
| Over-length | More than 2,048 tokens, including the frame, gives `unsupported_input` / `unsupported_length` with the token count and no prediction. Nothing is truncated or windowed. |

The scorer and the trainer use the same `tokenizer.json` and the same
tokenizers core version, 0.22.2. So NFC, added-token matching and BPE agree.

`slop_ninja_artifact_check spec` prints the profile's own digest. Both it and
the spec's digest are taken over the record with its keys sorted, so every
build computes the same values. The profile digest is a candidate for the
cohort manifest's `input_profile`. For this spec version the profile digest is
`0a4419df3d982d7932a6281014860e2a9df2b0b24d0f10520242644a35979328`, and the
spec's digest is
`7fceee2c93436bda1c01a8123ec4ac05807f47d1017b31200d56df0705cb7147`. A cohort
names the spec it checks in `artifact_spec.sha256`; compare it with what your
build prints (`slop_ninja_artifact_check --version` or `spec`), not with this
page. A spec test fails until this page quotes the compiled values.

### Findings

1. **Empty text differs.**
   - The scorer tests Rust `str::trim`, which uses Unicode `White_Space`.
   - The trainer tests Python `str.strip`, which also strips U+001C..U+001F.
   - So a document made only of those separators, with or without other
     whitespace, is refused by the trainer and scored by the scorer.
   - A test compares every code point with a fixture recorded from
     CPython 3.12.15 (Unicode 15.0.0): 29 code points strip to empty in
     Python, 25 trim to empty in Rust, and they differ on exactly these four.
2. **The failure shape differs.**
   - The trainer raises on empty and over-length text. Training rows are
     filtered before they reach it.
   - The scorer returns a status for each document.
   - Both count `[CLS]` and `[SEP]`, and neither truncates.
3. **Edge whitespace can differ by origin in evaluation data.**
   - The coordinator trims leading and trailing whitespace from generated text
     (`eval/textgen.rs` `common_checks`).
   - It passes human pool text as stored
     (`tools/a-testnet-eval-manifest` `export_human_pool`).
   - Whether stored human rows carry edge whitespace is not measured here.

## Spec v2

Spec id `slop_ninja_a_artifact_spec_v2`, digest
`0f25e636ba52b45269c0a4d0520c2f01550b51325bb6399d4ddae418c8384b50`.
It is spec v1 with these changes, and no others (a spec test compares the two
records key by key):

| Change | Spec v1 | Spec v2 |
|---|---|---|
| The span class `a-span-f32band-v1` | not served | served |
| Graph class rule | a class with token states needs a `span_head`; one without needs none | a class with token states needs heads that read them: a span head, or a heads v2 token head on the final-norm states; a class without needs heads that read none |
| Heads | `slop_ninja_full_a_heads_v1` | v1, or `slop_ninja_full_a_heads_v2` with a spans-v1 token head (`token_head` on `final_norm_token_states`, rule `slop_ninja_token_head_v1`, label policy `slop_ninja_token_span_label_v1`, `feeds_document_heads` false) and its `span_emission` spec |
| Export manifest | `slop_ninja_full_a_export_v2` | v2 with heads v1; `slop_ninja_full_a_export_v3` with heads v2, every export v3 field present and typed, a null `diagnostic_train_subset`, and a fit binding with `spans_rule_sha256` and `init_fit` |
| Calibration | `slop_ninja_full_a_calibration_v2` | v2 with heads v1; `slop_ninja_full_a_calibration_v3` with heads v2: v2 plus the exported `spans` spec and its provenance |
| Heads supervision keys | `ai`, `human`, `origin`, `share`, `token` | the same for heads v1; heads v2 add `token_head` |
| Arm definition | the base fields | also the trainer's spans-v1 fields (`freeze`, `token_head`, `token_weighting`, all or none) with its value rules; a frozen document model (`encoder_and_document_heads`) trains only a token head |
| Load guard | the release guard (mirror pinned by `scorer_guard_sha256`) | the release guard for heads without a span or token head; for every other form the scorer's shared load guard (`inference/a-container-serve/src/load_guard.rs`), which both the checker and the scorer compile, pinned by `scorer_load_guard_sha256` |
| JSON budget | the caller's (`ObjectCheck::with_json_budget`) | also `max_json_bytes`: the four JSON role files together at most 268,435,456 bytes (256 MiB, four times the heads runtime's 64 MiB limit), refused from the header as `json_budget` |
| JSON value budget | none | `max_json_value_bytes`: the four files' value costs (`json_value_cost`, model `slop_ninja_json_value_cost_v1`) together at most 2,029,195,723, refused before any file is parsed as `json_value_budget`; a caller can set a lower one (`ObjectCheck::with_json_value_budget`) |
| Parts manifest `profile_id` | `slop_ninja_a_artifact_spec_v1/<class>` | `slop_ninja_a_artifact_spec_v2/<class>` |

The object, the file table, the tokenizer, the graph pre-parse, the structure
digests, the parameter bound, the contract pins, the canonical layouts and
the text profile are spec v1's.

**The JSON value budget.** A byte budget does not bound what parsing the JSON
holds: `{"a":0},` is 8 bytes, and parsed it is a map node of several hundred
bytes, so a file inside the byte budget could exhaust the scorer's memory
before the schema refuses it. The layout check, which already reads every
byte without building a value, therefore also prices each file: an upper
bound on the heap a `serde_json::Value` of the file holds, the costliest form
any reader builds (a typed schema holds no more for the same bytes). The
prices are per node, each the larger of the two `serde_json` builds in use
(the default, and the one with `preserve_order` and `arbitrary_precision`
that a build linking rtao gets): 80 bytes for the root; per non-empty array,
16 bytes plus 160 per slot of its capacity (at least 4, else the next power
of two of its length); per non-empty object, 744 bytes for the first entry
and 352 per further entry; per non-empty key, string or number, 32 bytes plus
its length in the file; and per file, twice its longest string for the
parser's scratch buffer. A test parses costly and artifact-like shapes under
a counting allocator, in both builds, and checks that no parse holds more
than the price.

The budget comes from the scorer's memory model
(`slop_ninja_a_scorer_memory_v2`, in the
[scorer's README](../../inference/a-container-serve/README.md#memory-model)):
what a standard-4 instance left the scorer process, measured on October 29,
2026, less the measured process base, four sessions at the largest served
graph, the in-flight request bodies and two copies of the JSON byte budget,
divided among the three parses of the JSON that can be live at once. R0's
four files cost 118,493,491, 5.8% of it. The check runs before any role file is parsed,
so every parse, of an admitted artifact or of one refused later, stays within
it. The inference-content digest a miner commits
does not depend on the version; the package digest does, through
`profile_id`.

**The load guard for span heads.** It checks what the release guard checks
(complete and calibrated export, input meaning, heads and calibration bound to
the contract, the calibration's arm, seed, checkpoint and map, role
thresholds and rosters) with the same predicates, and adds:

- the export, heads and calibration schemas pair as above (and export v4
  with heads v3, which only spec v3 admits);
- the export manifest's `token_head` and `span_emission` equal the heads';
  the calibration's `spans` equals the heads' `span_emission`;
- only an export v4 declares a span tower, as the heads declare it.

**What a span-head artifact is.** The S0+ form of the span localization
results: R0's encoder and document heads, frozen, with a token head trained on
the final-norm token states, exported as export v3 and paired with the span
class's graph. The public training recipe does not train token heads.

**The span tower form (S2c)** is spec v3's ([Spec v3](#spec-v3)). Its token
head reads the states of copied encoder layers, which only the tower class
outputs, so spec v2 refuses its graph as `graph_size`.

**Measured on November 1, 2026.**

- R0 passes spec v2 in the plain class, with the same inference-content
  digest as under spec v1 (`63a0e608…c2a0a6`).
- The shipped S0+ span head package writes `contract.json` and
  `calibration.json` with a one-space indent, which is not their canonical
  layout, so as shipped it is refused (`json_layout`). Re-encoded with
  `slop_ninja_artifact_check canonical` (values unchanged; the contract digest
  rebound in the heads, the calibration and the fit binding, and the export
  manifest's file digests rebound) and paired with the span export of R0's
  weights, it passes spec v2 in the span class, and spec v1 refuses it
  (`graph_class_not_served`).
- Its parameters (graph plus heads, the token head included) are well inside
  the bound: the token head adds 197,121 to R0's 244,069 head parameters.

## Spec v3

Spec id `slop_ninja_a_artifact_spec_v3`, digest
`0bb326a93b1f4669018dadac3186f08bcc0ac527983eed0c75ce1e889e970d25`.
It is spec v2 with these changes, and no others (a spec test compares the two
records key by key):

| Change | Spec v2 | Spec v3 |
|---|---|---|
| Graph classes | plain and span, both served | also the tower class `a-tower-f32band-v1`, served: the plain outputs plus `span_tower_token_states`, 681,508,964 bytes, structure digest `0b1feeb0…839e3b9c` |
| Graph class rule | a class with token states needs heads that read them; one without needs heads that read none | the same, and a class with the tower's states needs heads v3 whose `span_tower` is the class's; only the tower class takes heads v3 |
| Heads | v1; v2 with a spans-v1 token head | also `slop_ninja_full_a_heads_v3`: heads v2 plus a `span_tower` declaration (copies of encoder layers 18 to 21 of 22, attention global, local, local, global, local window 128, read from the hidden state entering layer 18, with a final-norm copy, weights in `tower.safetensors`), and a token head on `span_tower_final_norm_token_states` |
| Heads runtime | the four pinned files | heads v3 are read by the span tower runtime (`grammar-a-baseline` `full_a_tower.rs`, pinned by `tower_runtime_sha256`), which checks what heads v3 add and hands the pinned runtime the same object as heads v2 |
| Export manifest | v2 with heads v1; v3 with heads v2 | also `slop_ninja_full_a_export_v4` with heads v3: every export v3 field, plus `span_tower` (the declaration, equal to the heads', and the weights file: `F32`, `tower.safetensors`, its digest, its parameter count, 5,015,040 per copied layer plus the norm's 768 (20,060,928), and 6 tensors per copied layer plus the norm), `fit_hashes` (the fit's files, declaring its encoder and document heads equal to its init fit's, bit for bit, and a frozen document check with error 0: the checker cannot see the init fit, so these are the exporter's declarations, and the graph class fixes what is served) and `reference_tower_rotary_modules` (the tower's depth); the fit binding adds `s2` (mechanism `span_tower`, the tower rule and the module digest) |
| Calibration | v2 with heads v1; v3 with heads v2 | also v3 with heads v3 (schema unchanged) |
| Arm definition | the base fields, and the spans-v1 fields all or none | also `s2`, only with the spans-v1 fields and only in an export v4: mechanism `span_tower`, keys exactly `layers`, `learning_rate` and `mechanism`, `layers` the declaration's depth, a positive finite `learning_rate`, `freeze` `encoder_and_document_heads` and a token head; any other mechanism is refused `schema` |
| Parameter bound | `max_parameters` 160,000,000 for every class | per class: 160,000,000 for the plain and span classes; for the tower class its graph parameters plus the largest heads its form admits, 170,151,195 + 244,069 (the pinned document heads) + 3,153,921 (a token head of 4,096 hidden units, the arm's widest) = 173,549,185 |
| JSON value budget | `max_json_value_bytes` 2,029,195,723 | per class: 2,029,195,723 for the plain and span classes; 1,540,445,560 for the tower class |
| Parts manifest `profile_id` | `slop_ninja_a_artifact_spec_v2/<class>` | `slop_ninja_a_artifact_spec_v3/<class>` |

The object, the file table, the tokenizer, the graph pre-parse and its
operator allow-list, the structure digest rule, the contract pins, the JSON
byte budget, the canonical layouts and the text profile are spec v2's.

**The heads' declaration is checked first.** Before the span tower runtime
reads heads v3, the checker compares their `span_tower` with the tower class's
declaration and refuses any other declaration `graph_class`, and an absent or
null one `schema`. The runtime builds the layer list a declaration's
`encoder_layers` implies before comparing it, so this check keeps one integer
from making admission reserve memory without bound.

**Why per class.** The tower class's graph has more parameters than the plain
graph, and the bound is derived from its structure like every other limit, so
it is a value of the class. The plain and span classes keep spec v2's bound
and value budget, so every artifact spec v2 admits passes spec v3 with the
same graph class, parameters, roles and inference-content digest; only the
package digest changes, through `profile_id`.

**The tower class's value budget** comes from the scorer's memory model
(`slop_ninja_a_scorer_memory_v3`, in the
[scorer's README](../../inference/a-container-serve/README.md#memory-model)) as
spec v2's does, at the tower graph's length and its own measured resident
bytes per session: what a standard-4 instance left the scorer process, less
the process base, four sessions of the tower graph, the in-flight request
bodies and two copies of the JSON byte budget, divided among the three parses
that can be live at once. A test in the scorer recomputes both budgets.

**What a span tower artifact costs.** ONNX Runtime computes every graph
output, so a tower-class artifact pays for the tower on every document,
whether or not spans are asked: about 18 to 20% of the encoder's CPU time.

**Measured on November 2, 2026.**

- R0 and the S0+ span-head artifact pass spec v3 with the same graph class,
  parameters, roles and inference-content digests as under spec v2.
- The S2c span package (R0's encoder and document heads with the frozen
  S2c-lr3e-5 seed-17 tower and token head, calibrated, export v4) passes
  spec v3 in the tower class: 170,592,385 parameters (170,151,195 in the
  graph, 441,190 in the heads: R0's 244,069 and the token head's 197,121),
  JSON value cost 171,765,310 (11.2% of the tower class's budget). Spec v2 and
  v1 refuse it (`graph_size`). Its digests are in
  [Reference packages](reference-packages.md).

## Spec v4

Spec v4 (`slop_ninja_a_artifact_spec_v4`) keeps every spec v3 value and
admits exactly what spec v3 admits. Its record states the graph class rule as
the byte comparison ([Structure stream](#structure-stream)):

| | Spec v3 | Spec v4 |
|---|---|---|
| `graph.structure_digest` | `slop_ninja_onnx_structure_v2` | removed |
| `graph.structure_stream` | none | `slop_ninja_onnx_structure_stream_v1`: the stream's definition and the rule that it equals the class's reference stream byte for byte |
| Each class | `structure_sha256` | `reference_structure`: the reference stream's length and SHA-256 |
| Parts manifest `profile_id` | `slop_ninja_a_artifact_spec_v3/<class>` | `slop_ninja_a_artifact_spec_v4/<class>` |

Every check of every version runs the comparison, so v1 to v3 also hash no
graph byte; v4 is the record that says so. Admission lists v2, v3 and v4
(`ADMISSION_SPECS`), the scorer admits by all three and reports v4 as its
newest, and the miner's `check` defaults to v4 when no cohort names a spec.
`tests/spec_v4.rs` compares the v4 record with v3's key by key, pins the v1 to
v3 digests, and, with the reference model directories, checks that each
reference artifact gets the same report under every version that defines its
class, that a structure-byte mutation (an operator name, the producer name)
is refused under each, and that a weight change is admitted.
`tests/structure.rs` runs both rules over a synthetic corpus and every
single-byte mutation of it and finds them equal.

The guard pin and the scorer mapping schema could later move to the files
manifest v2; that is not part of spec v4 and would need its own version.

## Tests

`cargo test -p slop-ninja-artifact-check`, from the `subnet/` workspace, runs
on Linux x86_64, CPU only, with no model calls. Use `--release` with the R0
variables: the R0 tests stream the 601 MB graph about 20 times, which takes
seconds in release and minutes in a debug build. The tests have nine parts,
and `tests/cli.rs` checks the binary's exit statuses.

- **Spec v4** (`tests/spec_v4.rs`). Spec v1 to v3 keep their digests; the v4
  record differs from v3's only in how the graph class rule is stated; v4 has
  v3's classes and bounds. With `A_R0_MODEL_DIR`, `A_SPAN_MODEL_DIR` and
  `A_TOWER_MODEL_DIR`, each reference artifact gets the same report under
  every version of its class, a structure-byte mutation (an operator name,
  the producer name) is refused `graph_not_allowed` under each, and a weight
  change is admitted.
- **Structure stream** (`tests/structure.rs`). Each shipped reference stream
  has its record's length and SHA-256, and its spec v1 to v3 digest is the
  class's. Over synthetic graphs, every chunking and every single-byte
  mutation, the byte comparison accepts exactly what the digest accepts; a
  mutated structure byte is refused at its stream offset; the stream holds no
  initializer float payload byte.
- **Spec v3** (`tests/spec_v3.rs`). Spec v1's and v2's records and digests
  are unchanged; spec v3's record differs from v2's only in the keys listed
  under [Spec v3](#spec-v3), and its plain and span classes carry exactly
  spec v2's values; the tower runtime pin is the compiled source's and the
  scorer's; the exporter records the tower class digest; the tower class is
  defined and served under v3 only, with its own parameter and value bounds,
  and the graph walk applies its bound; a tower-length header is `graph_size`
  under v1 and v2. With `A_R0_MODEL_DIR` and `A_SPAN_MODEL_DIR`, both pass v3
  with v2's results. With `A_TOWER_MODEL_DIR` (the S2c span package: the
  tower export with the calibrated S2c export's role files, re-encoded by
  `slop_ninja_artifact_check canonical`), it passes v3 in the tower class and
  v1 and v2 refuse it; heads v3 on the span or plain class's graph, a heads
  v2 token head or document heads on the tower graph are refused
  `graph_class`; and
  each of these mutations is refused: an anchor or pair-contrast `s2`,
  `s2.layers` unlike the declaration's depth, a zero tower learning rate, a
  non-integer or null `s2`, an export v4 without the arm's `s2` or without
  `fit_hashes`, a fit binding with another tower rule, an encoder unlike the
  init fit's, an inexact frozen document check, fit hashes unlike the
  manifest's encoder, another tower tensor or parameter count, rotary
  modules unlike the depth, a heads v3 token head on the final-norm states,
  heads v3 without a token head or without a span tower, and a million
  unknown top-level heads members, which the checker refuses in one pass over
  the members before the span tower runtime reads them (`schema`); a span
  tower arm whose `freeze` is not `encoder_and_document_heads`, a tower file
  of another dtype or name, document heads unlike the init fit's, a frozen
  document check that is not bitwise, and fit hashes whose `s2` module,
  selected checkpoint or init fit digests differ from the rest of the
  manifest (`schema`, each with its own detail); the manifest's tower unlike the
  heads' (`binding`); and both declarations unlike the class's, and heads v3
  declaring 1,600,000,000 or 2^64 - 1 encoder layers with the manifest's
  declaration unchanged, refused before the span tower runtime reads them
  (`graph_class`). An arm `s2` outside an export v4 is
  refused `schema` under v2 and v3. With `A_EXPORT_TOWER`, the tower export
  walks to the class digest and parameter count, and the plain and span
  classes' bound refuses it.

- **Spec v2** (`tests/spec_v2.rs`). Spec v1's record and digest are
  unchanged; spec v2's record differs from v1's only in the keys listed under
  [Spec v2](#spec-v2); the shared load guard has its pinned digest; versions
  are named by id and digest; the span class is served under v2 only; the JSON
  budget is enforced from the header under v2 only; the value budget is
  checked before any role file is parsed, under v2 only; a parts manifest
  listing is checked like a header. With `A_R0_MODEL_DIR`, R0 passes v2 with the same
  content digest and a v2 package id. With `A_SPAN_MODEL_DIR` (a span-head
  artifact: the span export with the S0+ package's role files, re-encoded by
  `slop_ninja_artifact_check canonical`), the artifact
  passes v2 and v1 refuses it; the class and the heads must agree; and each of
  these mutations is refused: the manifest's token head differing from the
  heads' and the calibration's span spec differing from the heads' (`binding`),
  a calibration v3 without its span spec, heads v2 supervision without
  `token_head`, a fit binding without `spans_rule_sha256`, a frozen document
  model without a token head, a diagnostic subset export and a span tower key
  in an export v3 (`schema`). With `A_R0_MODEL_DIR`, R0's export manifest with
  an array of small objects inside the byte budget but past the value budget
  is refused `json_value_budget`, and the same key under the value budget is
  parsed and refused `schema`.
- **Value cost** (`tests/value_cost.rs`). A counting allocator records the
  peak heap of parsing each shape into a `serde_json::Value`: small objects,
  nested objects, deep arrays, short and empty strings, short numbers, empty
  containers, one wide object, a long escaped string, and artifact-like float
  arrays and knot objects, in both layouts. No parse holds more than the
  file's price. With `A_R0_MODEL_DIR`, R0's four files too.

- **Synthetic graphs.** Each refusal class above is exercised. The structure
  stream is shown to be invariant to initializer float values and sensitive
  to float constants, shapes, names, integer constants and opsets. Results do
  not depend on chunk size. The parameter bound is enforced before the
  payload is read, and split `float_data` is counted once. A repeated
  `data_type` is refused.
- **Canonical layouts.** Python `repr` floats match CPython on edge values;
  both layouts round-trip and refuse padding, respelled numbers and escapes,
  duplicate or unsorted keys and over-deep nesting.
- **Spec.** It checks:
  - the compiled heads runtime against the spec's pins, and the scorer's
    served profile against them;
  - the scorer's guard source against `SCORER_GUARD_SHA256`;
  - the parts and heads size bounds against the scorer's and heads runtime's
    sources;
  - the exporter's class digests against the spec;
  - that the spec values are consistent and the parts manifest has the
    scorer's layout, order and part limit;
  - the header refusals, including lengths and the JSON budget;
  - that this page quotes the compiled spec and text-profile digests, and
    that its AI profile names hash to the pinned roster digest.
- **R0** (with `A_R0_MODEL_DIR`). R0 passes and maps to its deployed package,
  and its JSON files are in their canonical layouts. The pinned tokenizer has
  the recorded profile. Each of these mutations is refused with its code:
  - an extra file, an external-data sidecar, a missing file, a wrong role;
  - the tokenizer one byte off;
  - padding: 1 MiB of trailing spaces, the contract re-encoded compactly, a
    number with a trailing zero (`json_layout`);
  - declared lengths past the heads runtime's and the scorer's limits
    (`file_size`);
  - a broken calibration knot (a missing field, a score outside `[0, 1]`), a
    changed calibration policy, a non-integer run shard, unknown contract,
    heads and arm-definition fields, a changed pinned contract field
    (`schema`);
  - heads bound to another contract, and an integer calibration threshold
    against a float heads threshold, which the scorer refuses (`binding`;
    the same thresholds as floats pass);
  - R0's role files checked against the span class (`graph_class`);
  - an operator outside the allow-list and a custom domain, each a one-field
    change of R0's graph at its own length;
  - graphs of other lengths: a custom-domain graph, external data, a graph of
    allow-listed operators outside the classes, a graph past the parameter
    bound (`graph_size`);
  - the span export (`graph_class_not_served`).
- **Export** (with `A_EXPORT_PLAIN`, `A_EXPORT_SPAN`). Each export walks and
  reproduces its class's structure digest and length. Both reproduce R0's
  parameter count.

Without `A_R0_MODEL_DIR`, each R0 test prints a notice and passes without
checking R0; without `A_EXPORT_PLAIN`, `A_EXPORT_SPAN` or `A_EXPORT_TOWER`,
the export tests do the same, and the tower package tests without
`A_TOWER_MODEL_DIR`. R0's files are those of its
[reference package](reference-packages.md), and the S2c span package's give
`A_TOWER_MODEL_DIR` and, as its graph alone, `A_EXPORT_TOWER`. A plain export
of your own, such as the
[training guide's](training-guide.md) `graph/a-fp32band.onnx`, can be given as
`A_EXPORT_PLAIN`. `--nocapture` shows the notices.

## Limits

- **The heads runtime pin.** The checker compiles `grammar-a-baseline`'s
  `full_a.rs`, `full_a_spans.rs`, `involvement_calibration.rs` and
  `shared.rs` at the same repository revision, and the scorer's served
  profile pins the same four digests (re-pinned on October 31, 2026). The
  spec test fails on any difference, so a scorer re-pin or a change to those
  sources needs a matching spec update.
- **The guard mirror** repeats the scorer's guard as source, not as shared
  code: the scorer's guard lives with its runtime. The pinned guard digest
  turns any scorer change into a failing spec test, but a shared guard module
  compiled by both would remove the copy.

- **Roster sizes are fixed** at 33 AI profiles and an empty human roster,
  because the graph contains the document heads. A different roster needs a
  new graph class.
- **Admission does not execute the graph.** Float values are free, so an
  admitted artifact can still score badly or fail on some inputs. The scorer
  attributes those outcomes.
- **Free-text provenance strings and lists** in the JSON role files are free,
  so the JSON files can be as large as their runtime bounds (64 MiB for the
  heads, 4 GiB for the others), within spec v2's byte and value budgets. Their
  buffer is exactly the declared length, known from the header. Admission v2
  checks the files in the scorer Container, not in a Worker; a tool with a
  memory budget passes it to `ObjectCheck::with_json_budget` and
  `ObjectCheck::with_json_value_budget`.
- **Not yet arena-ready decoding.** The schemas and the guard mirror parse
  owned values (`String`, `Vec`, `serde_json::Value` views), whose memory is
  bounded by the files' value cost, not by an arena. A borrowed decode is
  still to be written for the Workers port.
- **Span class not served under spec v1.** Spec v1 refuses the span class;
  spec v2 serves it.
- **The tower class is one structure.** Its tower is S2c's: four copied layers
  from layer 18 with a final-norm copy. A tower of another depth or position
  is another structure and needs a new class.
- **Equal weights leave the class.** The class digests assume every weight
  is its own initializer, and the exporter merges weights with equal values
  ([Structure stream](#structure-stream)). The exporter refuses such a model;
  the checker sees only the resulting length, `graph_size`.
