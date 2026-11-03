# Generation prompt profiles

`generate_samples --prompt-profile-set style-mix-v1` adds a frozen mix of style
instructions to source-conditioned drafting and copyediting. The
[style-diverse pilot](STYLE_PILOT_RESULTS.md)
records the first generated cohorts and detector comparisons using these profiles.
Omitting the option retains the original pilot prompts, task IDs,
run metadata and admission rules.

| Profile ID | Requested variation |
| --- | --- |
| `source-matched` | Match the source's formality, vocabulary, rhythm and tone. |
| `plain` | Use familiar wording and manageable sentences, keeping technical details. |
| `editorial` | Use polished explanatory prose with clear connections between claims and evidence. |
| `informal` | Use conversational phrasing without adding jokes, anecdotes or a casual attitude. |
| `formal` | Use restrained, precise formal prose. |
| `direct` | Put actors and actions early without summarizing away information. |
| `anti-ai` | Explicitly try not to sound AI-generated; avoid stock phrasing and repeated templates. |
| `fix-slop` | Apply the pinned, adapted Fix Slop prose instructions. |

Every profile must preserve facts, qualifications, attributions, uncertainty,
stance and argumentative relationships. Requested formality and rhythm may
change; the passage's intended seriousness and attitude must remain. Drafts
still require independent composition. Edits must retain most wording and the
order of ideas, so a requested style may appear more weakly in an edit. These
instructions do not certify compliance, semantic fidelity, originality or
detector evasion. Existing admission checks cover length, formatting, unchanged
text and conservative draft-copy rejection; no quality judge or detector is
added.

## Assignment and reserved profiles

Before generating anything, the CLI groups the entire frozen input by source
family and then by split and register. Register is the sorted set of exact source
collection identifiers in that family. Within each bucket it sorts families by
a domain-separated SHA-256 rank and assigns the selected profiles round-robin,
with a deterministic bucket offset. Profile counts differ by at most one per
bucket. Every excerpt and draft/edit sibling in a family receives the same
profile. Neither model identity, origin label nor detector score enters this
assignment. Human originals remain untouched; sharing instructions between the
two generated labels does not eliminate all style-related origin shortcuts.

Assignments are computed before `--split` filtering. A Test-only invocation and
a whole-cohort invocation therefore agree when supplied the same frozen input
and profile subset. Adding/removing families can change round-robin positions;
the exact input file hash is recorded and bound into each profiled task's cache
identity. Assignment is stable under row reordering for a given family set,
although reordering changes the input file hash and therefore the cache IDs.

Use `--prompt-profile-ids` to reserve entire instruction families for a later
transfer check. For example, a future protocol could prescribe:

```sh
# Illustrative commands; choose fresh frozen roots and an admitted model spec.
generate_samples --input fresh-frozen-roots.jsonl --model-spec model.json \
  --output-dir data/training-styles --split train \
  --prompt-profile-set style-mix-v1 \
  --prompt-profile-ids source-matched,plain,informal,direct,anti-ai,fix-slop \
  --complete-families-only

generate_samples --input fresh-frozen-roots.jsonl --model-spec model.json \
  --output-dir data/primary-test-styles --split test \
  --prompt-profile-set style-mix-v1 \
  --prompt-profile-ids source-matched,plain,informal,direct,anti-ai,fix-slop \
  --complete-families-only

generate_samples --input fresh-frozen-roots.jsonl --model-spec model.json \
  --output-dir data/reserved-styles --split test \
  --prompt-profile-set style-mix-v1 --prompt-profile-ids editorial,formal \
  --complete-families-only
```

The default mix includes all eight profiles in training. This alternative keeps
both explicit anti-AI instructions and Fix Slop in training while reserving two
other styles. The primary fresh-source Test should use the training profile
distribution; reserved-profile transfer is a separate secondary evaluation. In
this example both Test runs share source families, so their results are paired,
not independent samples.

Development and calibration need their own prescribed subsets and invocations.
The CLI rejects empty, duplicate and unknown IDs; it canonicalizes valid subsets
to catalog order. It does not decide an experimental split policy. Freeze the
cohort, subsets and evaluation plan before fitting. Use fresh source families for
the next sealed confirmation: the first pilot's opened Test set cannot become a
new sealed test. Report held-out-profile results separately from primary results,
and retain failures and whole-family exclusions under the same admission policy.

## Evidence and versioning

`run.json` contains the selected subset, the complete catalog and the hash of
`profile-plan.json`. The plan records every input family's assigned profile,
rank, bucket size, offset and input hash before calls start. Each attempted call
retains the exact API payload in `request.json` and its assignment in
`profile.json`; successful records also include `generation.prompt_profile`.
That optional field is omitted when using the original protocol. The full prompt,
request and response hashes remain in generation provenance.

Profiled task IDs hash the original request/model/operation tuple plus the full
profile provenance. Resume checks reject changed plans, instructions or cached
metadata. The API receives the style instructions in its user message; profile
selection metadata is retained locally rather than sent as an unsupported API
field. Instruction, guard, assignment or catalog changes require a new version;
do not edit `style-mix-v1` after a corpus has used it.

The [Fix Slop snapshot](prompts/fix-slop-v1.txt) is an adapted prose-only subset of
the public [Fix Slop skill at revision
aa5566a](https://github.com/sam0x17/fix-slop/blob/aa5566a3e5f30e06ba4fa99ff61ebe280f872deb/SKILL.md).
The source SHA-256 is
`d0bc56baf7144958ff5cafd271bc790680cd7531ba30bbeee36cb49789a8e538`;
the adapted snapshot SHA-256 is
`20dc81885665cb5d914fddb894ddff4cad798e2926807007764acb14f0fb6cc5`.
The snapshot retains the [MIT notice](prompts/LICENSE.fix-slop), including
Copyright (c) 2025 Hardik Pandya. The catalog records the source revision and
source, instruction and license hashes. It has no dependency on a personal skill
installation and does not claim to execute the skill's external references or
grep pass.
