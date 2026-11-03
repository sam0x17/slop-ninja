# Local model proposals for edit menus

The model supplies alternatives for exact source spans. Rust imports those
alternatives into the existing menu search, retains mechanical failures and
scores complete combinations. Model output cannot become a whole-passage
revision through this path. Meaning, requested tone and writing quality still
require review against the original source.

Start from a saved edit-menu plan. `menu proposal-plan` freezes four requests:
word/phrase and clause alternatives, each with reference examples alone or
examples plus measured style guidance. Each request asks for eight independent
alternatives. Word/phrase spans allow at most four words; clause spans allow
32. The measured condition uses named grammar metrics and function-word rates
from the same independent reference works. These descriptive rates are not
quotas. No detector scores enter these proposal prompts.

The requested audience and register take precedence over incidental formality
in reference samples. Proposals should preserve logical dependencies as well as
facts, and avoid adding repetition or grammatical difficulty. These instructions
apply to newly frozen prompts; earlier request bytes and results stay unchanged.

Keep the source, root claims, requested tone, references, generator revision
and decoding settings fixed across the four requests. Record the generator's
file manifest and loaded runtime instance. One generation per cell, no redraw,
with complete response bytes and failed/truncated calls retained. The local
generator must run on the Studio; use its loopback endpoint there.

```sh
slop_ninja_transformer menu proposal-plan --plan /path/to/saved-menu/plan.json \
  --model-spec /path/to/pinned-model.json --python /path/to/spacy/python \
  --out /path/to/proposal-plan.json
slop_ninja_transformer generate --plan /path/to/proposal-plan.json \
  --max-calls 4 --out /path/to/generation
slop_ninja_transformer menu import-proposals --plan /path/to/proposal-plan.json \
  --run /path/to/generation --request-id clause-measured \
  --out /path/to/imported-menu
```

The other request IDs are `clause-references`, `lexical-measured` and
`lexical-references`. Import verifies the run/plan binding and exact generated
text hash. It accepts strict JSON with one to eight alternatives; it retains
the actual count if the generator supplied fewer than requested. Each before
span must occur exactly once. Unchanged, overlong, duplicate and numerically
altered alternatives fail, along with the existing hidden-character guards.
Every accepted proposal remains unreviewed. Numeric and lexical checks do not
protect meaning or tone.

Prepare the imported `input.json` with the usual `menu prepare` command. For a
continued walk, supply the same bound parent selection used by the source plan.
The original source, claims, references, detector and cumulative word budget
remain fixed. Each imported menu declares depth three and at most 128 states.
Overlapping alternatives remain incompatible; no single-edit score prunes the
search. Use the existing rank, review, selection and external-report commands.

Record individual proposal reviews before opening new A scores. Inspect the
complete finalists against the original source and requested tone before
Pangram submission, and disclose which scores the reviewer saw. A model's
stated rationale is a proposal for review, never a semantic certificate.
Final output needs a concrete editorial benefit. No drop in detector score
can compensate for lost details, a new implication or worse prose.

The first comparison uses the familiar workshop notice and the same two
imperfect Global Voices excerpts. It is adaptive development, with no claim
of held-out performance or independently established author resemblance.
The existing cumulative Pangram budget applies. Both AI-written and AI-assisted
content count as flagged. Keep raw references, alternatives and provider logs
in ignored storage; commit protocols and aggregate observations.
