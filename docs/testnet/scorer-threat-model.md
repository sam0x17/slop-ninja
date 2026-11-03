# Scorer threat model

This page covers the hidden-evaluation scorer (`a-scorer`, the
`a-scorer-testnet` Container) and the local scorer miners can run
(`slop_ninja_a_local_score`). Both are built from
[`inference/a-container-serve`](../../inference/a-container-serve) and share
one engine. It says what an artifact can attempt, what stops it, where, and
what remains. The [adversarial artifact set](#adversarial-artifact-set) tests
each case.

## What is trusted

| Trusted | Pinned by |
|---|---|
| The coordinator, which streams parts, posts the manifest and documents, and reads results | its own deployment |
| The scorer binary and the ONNX Runtime library | `SCORER_BINARY_SHA256` and `SCORER_ORT_SHA256`, which the coordinator compares with the instance's `/healthz` before it loads anything |
| The heads runtime (`full_a.rs`, `full_a_spans.rs`, `involvement_calibration.rs`, `shared.rs`) | the served profile; the scorer refuses to start on other sources |
| The span tower runtime (`full_a_tower.rs`) | `TOWER_RUNTIME_SHA256`; the scorer refuses to start on another source |
| The load guards | the release guard by spec v1 (`scorer_guard_sha256`); the shared load guard (`load_guard.rs`) by spec v2 and v3 (`scorer_load_guard_sha256`) |
| The tokenizer | the artifact spec: one byte-identical file |

The artifact is not trusted. A miner chooses every byte of it.

## What an artifact can control

Admission (the [artifact spec](artifact-spec.md)) narrows a miner's choices to:

- the values of the graph's initializer weights. They must be finite; the
  operators, shapes, names, attributes, constants and file length are fixed by
  the allow-listed graph class;
- the values of the head weights and the calibration, within the role schemas
  and the bindings;
- free-text provenance strings and lists in the JSON files, within the JSON
  byte and value budgets.

Everything below assumes an artifact can use all of that freedom.

## Assets

- **Hidden documents and labels.** They must not leave the evaluation.
- **Scores.** Each artifact's score must depend only on its own outputs on the
  cohort's documents.
- **Spend.** An artifact must not make scoring cost more than its documents.
- **Other artifacts.** No artifact may affect another's load or outputs.

## Threats and defenses

| Threat | Defense | Enforced | What remains |
|---|---|---|---|
| Code execution through the graph: custom operators, model-local functions, subgraphs, external data paths | The graph pre-parse refuses custom domains, functions, subgraph attributes, external data, unknown fields and operators outside the allow-list, and requires an allow-listed structure digest | At admission, and again inside the scorer on the assembled files before ONNX Runtime opens the graph (`engine::admit`) | A defect in ONNX Runtime reachable through allow-listed operators with adversarial finite values. Shapes and structure are fixed, which limits the reachable code. The Container runs without Internet access, as a non-root user, on a distroless image |
| Code execution through JSON | No JSON file is executed; each is parsed by typed schemas (`serde_json`, recursion limit 128) and the heads runtime | Admission and scorer | A parser defect |
| Exfiltrating hidden text | The Container has no Internet access; the artifact cannot run code; outputs are numbers and labels in a fixed schema that go only to the coordinator; per-document predictions are never published | Container start (`enableInternet` false), coordinator | Aggregate scores are public. A model cannot see the cohort's documents before scoring them, so it cannot select documents to leak; lookup and memorization are the evaluation stock's concern, not the scorer's |
| Memory exhaustion | The graph's length is fixed by its class; the four JSON files together are at most the spec v2 JSON budget (256 MiB), checked from the manifest before any part is read; their value cost (what parsing them can hold) is at most the graph class's value budget (spec v2's for the plain and span classes, spec v3's own for the tower class), checked in the layout pass before any file is parsed, so hostile JSON inside the byte budget is refused `json_value_budget` instead of exhausting memory; a document is at most 2,048 tokens; a score request is at most 8 MiB and 128 documents; a load is refused as a platform fault when the container's memory limit (its cgroup limit, else the VM's `MemTotal`) is below the bound every artifact of the manifest graph's class fits ([memory model](../../inference/a-container-serve/README.md#memory-model)) | Scorer: the byte budget before assembly, the value budget after assembly and before any parse | Running out of memory kills the container. The coordinator's crash isolation then attributes the failure to the artifact only on a freshly started instance. The value budget's price list is an upper bound tested against a counting allocator for the costly shapes, not a proof for every allocator. The session terms are measurements, and a session's memory grows with the documents it has served and their order: the tower class's term covers instances that serve up to 1,500 documents in the orders measured (a cohort scores about 1,000); R0 on the same stream peaked 616 MB above the plain class's term, inside its JSON terms ([memory model](../../inference/a-container-serve/README.md#memory-model)) |
| Disk exhaustion | Parts are at most 64 MiB, at most 64 per file; the manifest is checked against the spec before assembly; parts are deleted once assembled; an instance holds one artifact | Scorer | The coordinator bounds the bytes it streams by the plan's artifact bound |
| Slow documents | The structure, and so the work per token, is fixed by the class. Subnormal float32 values can slow CPU arithmetic: the `subnormal-weights` case measured no slowdown on the test host ([Local run](#local-run)) | The coordinator's per-request timeout destroys a hung instance; the failure is attributed on a fresh instance | Not measured on the Cloudflare hosts (AMD Zen 4). If a slowdown appears there, it costs up to the request timeout per request of such an artifact. Flushing subnormals to zero in ONNX Runtime would remove it, but it can change the last bits of every artifact's outputs, so it needs the same requalification as a heads runtime change before it is enabled |
| Slow or unbounded admission | Before the span tower runtime reads heads v3, they are checked for unknown or repeated top-level members in one pass, because the runtime's own repeated-member check compares each member with every member before it; and their `span_tower` declaration must equal the tower class's, refused `graph_class` otherwise, because the runtime builds the layer list a declaration's `encoder_layers` implies before comparing it, so one integer could otherwise reserve memory without bound | Admission and scorer (`roles.rs`; `tests/spec_v3.rs` refuses a million unknown members, with the whole object check, in 1.3 s on the test host, and declarations of 1,600,000,000 and 2^64 - 1 encoder layers) | The other checks' parsing time grows with the files' bytes, which the JSON byte and value budgets bound; the coordinator destroys a load that outlasts its load allowance |
| Non-finite or garbage outputs | `h` and every token state must be finite; a document whose outputs are not finite returns `error` / `model`; a warm-up document whose outputs are not finite fails the load. Finite garbage is scored as what it is | Scorer | None; all three are the artifact's |
| Crashing the process | A panic aborts the process, so the coordinator sees a container death, never a half-answered request; core dumps are off, so a crash writes no weights or text to disk | Scorer (`panic = abort` hook, `RLIMIT_CORE` 0) | Attribution as above |
| Admitting by a spec other than the cohort's | Each load admits by the spec it names by digest, its cohort's; a load naming none is admitted by spec v2, and a load naming spec v1 or a digest the build does not define is refused 400 `spec`; the spec is part of the instance's binding, so a later load under another spec is `instance_bound` | Scorer (`instance.rs`, `engine::admit`; route tests) | The coordinator must name a spec v3 cohort's spec on its loads; until it does, the scorer refuses tower-class artifacts `graph_size` |
| Affecting another artifact | One process serves one artifact for one cohort: the first accepted load binds it to its package digest, cohort and spec; a load of anything else, a part upload after binding, and a score request for another package or cohort are refused with `instance_bound`; at start the process removes any earlier artifact's parts and files from its work directory. The coordinator's lane client names its cohort label on load and score, and destroys a running instance that `/healthz` shows bound to another package or cohort before it streams anything, so each artifact and cohort gets a fresh instance | Scorer (`instance.rs`, `a_scorer.rs`; route tests in `tests/routes.rs`); coordinator (`cf_lanes.rs`, `eval/scorer_client.rs`) | The client names the cohort only to a scorer whose `/healthz` reports `single_use`; with an earlier scorer build it compares packages only |
| Output depending on other documents (batch variance) | Each document runs alone at batch 1, padded to a multiple of 64 with a masked pad; all sessions of an instance are identical | Scorer | None. The engine tests score the same documents alone, concurrently and in reverse order over three sessions and compare digests |
| Span outputs changing document outputs | Sentences and spans are computed only when a score request asks; the document prediction and `output_sha256` are computed first and do not depend on the request; the token head cannot feed the document heads (`feeds_document_heads` false, checked by the heads runtime) | Scorer, heads runtime | A failing span emission returns `span_status` `unsupported` and the document outputs stand |
| A miner's span head reading the wrong states | Which token states the token head reads is declared by the export and heads schemas and checked against the graph's outputs at load; never chosen by a runtime version | Scorer (`load_guard.rs`, `engine.rs`) | None |
| Tokenization differences | The tokenizer is one pinned file; no artifact-controlled tokenization | Admission | The scorer's empty-text rule differs from the trainer's on four control characters ([findings](artifact-spec.md#findings)) |
| Different hosts giving different bits | Not an artifact's attack: outputs can differ in their last bits across CPU types. The scorer reports its CPU model | Coordinator records the host | Scores compare artifacts on the same documents in the same cohort |

## Adversarial artifact set

`slop_ninja_a_adversarial write --base DIR --out DIR` derives each case from an
admitted base artifact; `run` gives each to the scorer's own code paths and
compares the outcome with the case's expectation. Model cases change one file
of the base at its own length, so a length check alone cannot refuse them.
Manifest cases write only the parts manifest whose declared lengths are the
attack.

| Case | What it does | Required outcome |
|---|---|---|
| `nonfinite-outputs` | the weight of the LayerNormalization that produces `h` set to 3.0e38 (finite in the file, so admitted) | the load fails on the warm-up document (`invalid h`): the artifact's failure |
| `saturated-weights` | every FLOAT initializer value set to 3.0e38 | loads and scores: the LayerNormalizations keep `h` finite, so the outputs are garbage, scored as such |
| `subnormal-weights` | every FLOAT initializer value set to 1e-39, a float32 subnormal | loads and scores; time measured against the base |
| `custom-domain` | the first `Add` node in a custom operator domain | refused `graph_custom_domain` before ONNX Runtime |
| `operator-not-allowed` | the first `Add` node changed to `Abs` | refused `graph_op_not_allowed` before ONNX Runtime |
| `external-data` | the first initializer declares external data | refused `graph_external_data` before ONNX Runtime |
| `never-loads` | `heads.json` bound to another contract (one hex digit changed) | refused `binding` before ONNX Runtime |
| `oversize-protobuf` | a graph declared at 2 GiB + 1 bytes | refused `graph_size` from the manifest, before any part is read |
| `oversize-heads` | `heads.json` declared one byte past 64 MiB | refused `file_size` from the manifest |
| `oversize-json` | `calibration.json` declared at 300 MiB | refused `json_budget` from the manifest |
| `json-value-budget` | `manifest.json` with an array of `{"a":0}` objects past the value budget, about 20 MB, far inside the byte budget | refused `json_value_budget` before any file is parsed |
| `json-objects-in-budget` | the same array under the value budget, as the export manifest's `token_head`, which the schema parses as a JSON value | parsed within the value budget, then refused `schema` (from a base with a token head, whose schema takes any value there: refused `binding` by the load guard) |
| `padded-provenance` | `manifest.json`'s free-text `limits` list padded to the longest list of one-letter strings whose price (the strings and the slots of its capacity, the next power of two of its length) fits the value budget (2^23, 34 MB, for R0) | admitted; loads and scores within the memory model |

Two cases of the readiness plan cannot be built as admitted artifacts:

- **A crash on every document.** With the structure fixed, an artifact cannot
  reach code that panics. The nearest behaviour, an artifact that loads and
  then fails every document, cannot be built from an admitted graph by changing
  weights alone either: the LayerNormalizations normalize away an overflow
  that only real documents would reach. The engine tests build one from a
  synthetic graph (its warm-up output is exactly zero and every document
  overflows) and check that each document is `error` / `model` and the load
  succeeds. A crash for any other reason is a container death, which the
  coordinator's crash isolation attributes.
- **Batch-variant output.** The engine scores documents one at a time, so no
  artifact can make one document's output depend on another's; the engine
  tests check invariance under order and concurrency.

### Local run

Run on November 1, 2026, on a Linux x86_64 host (Intel Raptor Lake, AVX2),
with R0 as the base, ONNX Runtime 1.31.1 from the pinned wheel, one session of
one thread and four synthetic documents of 14 to 512 tokens. Every case met
its required outcome.

| Case | Stage reached | Outcome | Time |
|---|---|---|---|
| base (R0) | scored | 4 `ok` | load 3.5 s, score 1.59 s |
| `nonfinite-outputs` | load | the load failed (`invalid h`) after ONNX Runtime opened the graph | |
| `saturated-weights` | scored | 4 `ok` (garbage values) | score 1.55 s |
| `subnormal-weights` | scored | 4 `ok` | score 1.58 s: no slowdown on this host |
| `custom-domain` | admission | refused `graph_custom_domain`; ONNX Runtime never opened the graph | admission 2.3 s |
| `operator-not-allowed` | admission | refused `graph_op_not_allowed`; not opened | 0.30 s |
| `external-data` | admission | refused `graph_external_data`; not opened | 0.27 s |
| `never-loads` | admission | refused `binding`; not opened | 2.3 s |
| `oversize-protobuf` | manifest | refused `graph_size`; no part read | |
| `oversize-heads` | manifest | refused `file_size`; no part read | |
| `oversize-json` | manifest | refused `json_budget`; no part read | |

The admission times show where each walk stopped: a graph refusal comes as
soon as the walk reaches the offending field, so a field near the start is
refused in a fraction of a second.

The JSON cases were run the same way on November 1, 2026, each alone after
the base, in a process limited to 8,458,779,657 bytes (the memory model's
one-session bound):

| Case | Stage reached | Outcome | Peak resident memory | Time |
|---|---|---|---:|---|
| `json-value-budget` | admission | refused `json_value_budget` (20 MB manifest) before any file was parsed | 0.97 GB, the base's | admission 1.3 s |
| `json-objects-in-budget` | admission | 1,690,886 objects (13.5 MB) parsed, then refused `schema` | 1.26 GB | admission 1.7 s |
| `padded-provenance` | scored | admitted; 4 `ok` | 0.99 GB | load 3.5 s, score 1.63 s |

Each peak includes the base's load. The allocator reuses what the base freed,
so these peaks understate a cold process; the value-cost test measures each
shape's own peak against its price.

On November 2, 2026, the whole set also ran from the S2c span package (spec
v3, tower class), with four documents, one session of one thread: every case
met its outcome. From that base `json-objects-in-budget` is refused `binding`,
since the package's export manifest has a token head. After the tower class's
value budget was re-derived from the cohort-sized memory measurement, the
three JSON cases ran again from that base and met their outcomes, peaking at
1.80 GB against the tower class's one-session bound of 6.85 GiB.
`padded-provenance` pads to 2^22 strings there: one more string is priced at
capacity 2^23, past the budget, so a single list reaches 59% of the room the
package's own files leave.

The instance policy is covered by the route tests (`tests/routes.rs`), which
start the built `a-scorer`: after a load binds, a reload of the same binding
returns 200; a load of another package, of the same package for another or
no cohort, or under another spec, returns 409 `instance_bound`; a load admits
by the spec it names (a tower-class manifest is refused `graph_size` under
spec v2 or no spec, and checked under spec v3), and a spec digest the scorer
does not admit by returns 400; an upload of a part that is not the
bound package's returns 409 and stores nothing; the bound package's parts read
as present; and a score request for another package or cohort returns 409,
also once the artifact is ready (with R0). The same tests check that no
document text or id reaches standard error, `/healthz` or a refusal, and that
core dumps are off.

## Open items

- **The live run.** The readiness criterion (AR-16) asks for the set to run
  against the deployed reward-v1 scorer on eval-testnet. That needs this
  scorer's image deployed and its digest pinned (`SCORER_BINARY_SHA256` in the
  coordinator's `wrangler.toml`) and the cases uploaded through admission or
  an operator plan.
- **The reference chain at deployment.** This scorer refuses the reward-v1
  rehearsal artifacts as they were built (`refused profile_id`, and
  `refused tokenizer_mismatch` for `no-load`). Their
  spec v2 re-encode keeps each artifact's values and its drill role
  ([Deployment prerequisites](../../inference/a-container-serve/README.md#deployment-prerequisites)),
  under new package and content digests, while the main instance's reference
  chain names the earlier rehearsal reference. With this scorer deployed, the
  main instance runs no reward-v1 cohort until its chain record is restarted
  (the announce-time restart makes the released R0, `2f88b0d2…678559`, R_0).
- **The live memory limit.** Record `memory_limit` from a live `/healthz` of
  the deployed instance (the operator scorer probe's reply carries it as
  `cold.limits`); the 507 guard relies on it.
- **Published manifests.** The scorer refuses a parts manifest whose
  `profile_id` no packer writes, but a manifest is published from the
  coordinator's records, not the scorer's. The publisher should apply the same
  rule. One rehearsal package manifest (`soup`) carries free text there and
  must not be published as it is.
- **Subnormals.** Enabling flush-to-zero needs a requalification of every
  served output.
- **ONNX Runtime fuzzing** with allow-listed structures and adversarial
  values is not done.
