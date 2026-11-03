# Slop Ninja A corpus acquisition

Slop Ninja has one growing corpus with versioned collection batches and frozen
experiment views. Preserve the existing releases, split memberships, model
selection seals and calibration receipts. Acquisition alone does not admit new
records to a fitted model or establish held-out performance.

`a_style_dataset prepare-random` prepares a bounded acquisition from five public
Train source genres: fiction, essays, history/biography, science/nature and
travel. It selects one established source work per genre, without detector
feedback. Ten independent draws at each of two rewrite extents across three
actual generators request 300 transformed documents. These five source works
are an implementation pilot; many variants of five works are still only five
independent works. Further expansion needs more source families.

The completed first random-prompt collection retained 184 rewrites: 63 from
Qwen, 66 from Phi and 55 from Mistral. Source-aware prompt screening rejected
116 planned cells after the bounded attempts. All 184 accepted prompts produced
complete rewrite records; exact prompts, reviews, responses and realized spans
are retained, and completed payloads are compressed. These counts establish
execution and provenance, not fidelity or detector quality. Prompt invention
showed a recurring archival/academic preference despite distinct wording;
broader prompt inventors and manual semantic-diversity checks are needed.

Every transformed document receives a separately generated, free-form style
instruction. The number of draws is a collection budget, not a vocabulary of
ten styles. A pinned local model invents a coherent voice for the passage's
genre. Some draws additionally request British English, human-sounding prose,
or the pinned [fix-slop skill](https://github.com/sam0x17/fix-slop/blob/78555d2f173db50b8ca03a0cb0961cac4abee0e0/SKILL.md).
The remaining draws have no prescribed style family. Each instruction receives
a separate quality-review call. Deterministic checks reject malformed prompts,
duplicate names and near-duplicate instructions. Failed attempts remain evidence;
there are at most two attempts per instruction.

Prompt policy `source-aware-diverse-v3` supplies the actual source during invention
as well as review. Both calls also see the last eight accepted instructions and
must distinguish a new prose style from a renamed persona. This bounded comparison
helps discourage repeated academic voices. Five seeded numeric dimensions
(register, diction, sentence length, syntactic complexity and rhythmic variation)
give each invention a different starting point without a fixed persona menu.
Review requires explicit checks for preserved stance, applicability to partial
excerpts and a distinct voice. These remain model judgments: spot checks have
found accepted instructions that overstate certainty or flatten rhetoric. No
automatic acceptance establishes fidelity or corpus-wide semantic diversity.
Exact requests, policy identity and recent briefs are retained,
and earlier collections remain tied to their captured implementations.

`prepare-random-wide` independently sets works per genre and draws per
source/model/extent. It selects one passage per distinct Train work, refuses
insufficient source coverage, and never borrows evaluation works to fill a quota.
The broader collection requested three works per genre and one draw per
model/extent: fifteen works and ninety cells. It completed 48 rewrites from
fourteen of those works: 15 Qwen, 19 Phi and 14 Mistral; 42 prompt requests were
rejected. This broadens the original five-work pilot within already exposed
Train families. It is acquisition, not fresh evaluation or another fit.

Review checks whether the instruction is specific, coherent, usable for the
actual source passage, and compatible with preserving meaning, facts, perspective,
setting and approximate length. Personas specify prose choices, not invented
biography. A separate call to the same model is automatic screening, not an
independent human quality judgment. The resulting prose's fidelity and adherence
to the requested style remain unverified. Neither prompts nor outputs are
selected using Pangram scores.

The initial local generators are Qwen3.6-35B-A3B, Phi-4-mini-instruct and
Mistral-7B-Instruct-v0.3, with existing weight, runtime, quantization and rights
receipts. A separate bounded hosted batch now contains ten rewrites requested
from `gpt-6-astra` and ten whose prose events report `claude-sonnet-5-5`. Each call
invents a free-form brief and applies it to one exact source span. All twenty
responses assembled successfully, with both partial and full replacements across
five public Train works. This joint invention/rewrite process has no independent
quality certification. The Codex CLI does not expose the resolved model snapshot:
retain its requested selector and leave the reported identity null. Claude's
reported model name is retained separately from the requested `sonnet` alias.
A reported prose model is the attribution label; with only a requested selector,
the label is `requested:<selector>`, reported as its own identity basis and never
presented as a resolved snapshot. Both roles' involvement and measured share
remain available regardless of whether a resolved generator identity is exposed.

Hosted and frontier outputs are admitted for A training and evaluation under the
[hosted admission manifest](a-hosted-output-admission.json). It admits these 20
rewrites and the 96 earlier frontier captures by exact record hash; their
archived exports keep the capture-time `commercial_training_admitted: false`
unchanged. New acquisitions record the admission basis in their manifests and
rows, and request medium reasoning effort from both CLIs. Hosted records form a
separate cohort with explicit training weights, so their influence can be
measured and the cohort removed. Only public passages from admitted pools are
sent to hosted models, never Test passages before the seal. The admission is a
project decision recorded in the manifest, and hosted outputs may be published
under the project's publication policy. The original source rights remain
separately preserved.

Every row preserves the exact source, prompt, generator configuration, response
receipt and replacement bytes. Its source family and Train membership are
inherited unchanged. Partial and full source-conditioned rewrites have mixed
production history. Realized share uses final words in replacement spans divided
by all final words, including when that share is 100%. Requested rewrite extent,
Pangram content fractions and native A involvement confidence remain separate
measurements. Model identities, source IDs, prompts, and review metadata are
provenance, not learned numeric inputs.

The Rust examples are:

```text
a_style_dataset prepare-random INPUT.jsonl NEW_OUT MODEL.json MODEL.json ...
a_style_dataset prepare-random-wide INPUT.jsonl NEW_OUT WORKS_PER_GENRE DRAWS MODEL.json MODEL.json ...
a_random_style_prompts PLAN.json PROMPT_OUT PROMPT_GENERATOR.json FIX_SLOP_DIR
a_balanced_generate HUMANS.jsonl MODEL_OUT MODEL.json --recipe random_style_diversity_v1 CATALOG.json
a_style_dataset status COLLECTION_OUT NEW_REPORT.json
a_style_dataset export COLLECTION_OUT NEW_EXPORT
a_hosted_style_rewrites prepare PUBLIC_TRAIN_HUMANS.jsonl NEW_COLLECTION
a_hosted_style_rewrites collect COLLECTION astra|claude
a_hosted_style_rewrites assemble COLLECTION ROLE NEW_EXPORT
a_hosted_admission build RESTORED_HOSTED_REVIEWS.jsonl HOSTED_STYLE_ROOT NEW_ADMISSION.json
a_hosted_admission check docs/a-hosted-output-admission.json RECORDS.jsonl
```

`prepare-random` writes the source-family-bound request matrix and local model
configurations. The prompt collector records each generation and review before
producing an immutable catalog. The rewrite collector consumes only accepted
catalog entries and retains rejected, unsupported and interrupted work. Status
counts actual bound outputs; planned model names and shell exit codes are not
completion evidence. Preserve all raw receipts, compress completed payloads,
and join CFG parse/vector caches by exact text plus the frozen extraction
profile. Never treat a reused parse as compatible with an arbitrary new profile.

`export` requires a terminal state for every planned cell, rechecks completed
records and response hashes, and preserves source and generation fields in a
single Train batch. Its manifest supports frozen CFG cache preprocessing. It
does not admit the batch into a fit or certify prose fidelity; known quality
defects and automatic-screening limits must be addressed before training reuse.

The hosted collector uses installed authenticated CLIs through Rust orchestration.
It retains exact input and output bytes, CLI versions/arguments, token usage and
raw events. Codex user configuration, shell tools and shell snapshots are disabled;
Claude uses safe mode with no tools. Tool use or incomplete output fails capture,
and uncertain invocations are not automatically repeated. The transport follows
the [documented non-interactive event stream](https://learn.chatgpt.com/docs/non-interactive-mode);
[the Astra selector](https://developers.openai.com/api/docs/models/gpt-6-astra)
is explicitly requested, without substituting another model.

Live jobs run on build-host in task-owned systemd units, with one model loaded at
a time, explicit CPU/RAM limits, zero swap, and checked VRAM headroom. The
recorded task root contains captured binaries, hashes, restart instructions and
per-attempt receipts. No new fitting or Test selection is part of acquisition.
