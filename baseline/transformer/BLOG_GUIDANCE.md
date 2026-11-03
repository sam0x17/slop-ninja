# Blog profiles as detector-search guidance

The experiment tests whether a writer's word and grammar profile helps prioritize
edits that lower A. The main objective remains lower A and Pangram scores on the
complete document, subject to meaning, intended tone and writing quality.
Author resemblance and improvements for a stated audience or venue are separate
objectives. The current test measures exploratory A query ordering only.

Keep one source, editing brief, preservation checklist and candidate menu. Fit
each target from TRAIN posts in the existing Blog Authorship export. The original
reference profile supplies a control. Every target receives exactly the same
candidate texts, so observed A values can be reused by exact text hash and
detector identity. A target profile changes the search order, not the detector.

Run the Rust preparation example on the Studio:

```sh
cargo run --release --example blog_guided_menus -- \
  --template /private/fresh-root-menu.json --export /private/authors-v1 \
  --author AUTHOR_ID_1 --author AUTHOR_ID_2 --author AUTHOR_ID_3 \
  --references 6 --out /private/new-blog-inputs
```

The example reads the selected authors' manifest metadata and only the selected
TRAIN text. Seeded ranks select at most one complete post from each supplied date;
they do not favor a detector score or a preferred writing style. It checks source
hashes, word counts, duplicate reference text, archive attribution and date-group
split consistency. Saved output binds the export, template, author manifests and
selected posts. Existing root text, claims, intent and menu settings survive
unchanged. Continued walks are rejected: changing references starts a separate
research comparison rather than changing a previously accepted walk.

Prepare `control.json` and each `author-ID.json` with the usual `menu prepare`
command. Score one complete set of distinct candidate texts with `screen`.
The comparison requires identical state IDs and edit sets across plans, complete
observations, one detector artifact and matching root/intent/parser bindings.

```sh
cargo run --release --example compare_blog_guidance -- \
  --plan /private/control/plan.json --plan /private/author-1/plan.json \
  --plan /private/author-2/plan.json --plan /private/author-3/plan.json \
  --observations /private/screen/observations.jsonl \
  --budget 8 --out /private/comparison.json
```

Before opening the new A observations, freeze three query orders: full word and
grammar distance, function-word and grammar distance, and function-word/grammar
movement with the sentence-load family excluded. The third keeps the remaining
original family weights and scales. It checks whether a useful order survives
removal of sentence-load effects; it is not a learned or validated style metric.
Break ties by exact text hash. Query the first eight changed states in each order
and retain the unchanged source if none improves A.

Compare that best observed A with the exact expected best from eight uniformly
sampled distinct candidates, also retaining the source. The initial source call
is additional and common to every order. The complete-menu minimum is an
exhaustive-search comparison at its larger budget. This offline diagnostic does
not simulate adaptive search or establish unseen-document performance. MPS/CUDA
observations remain exploratory and need separate reference CPU confirmation.

The first study uses the first three authors in an existing seeded development
export. Its 256-state menu combines seven negative contractions with a prior
five-edit repair bundle. All subsets remain available, including individually
unhelpful edits. This bounds a first test of author-guided ordering; it cannot
settle how useful a larger grammar or lexical menu would be.

Direct inspection of two selected posts per author found varied conversational
and argumentative habits alongside typos, repetition, quoted speech and material
outside the source's intended register. Those posts remain in the recorded
experiment. Their attribution and dates are supplied metadata, and the selected
writers are not an untouched author cohort. Review grammar analyses and proposed
edits directly; low distance does not approve imitation of those defects.

Keep corpus text, fitted profiles and source-derived candidates outside public
artifacts. Blog data remains restricted to the authorized local noncommercial
experiment. Private manuscript inputs and outputs stay outside both the repository
and datasets, including ignored directories. The study makes no Pangram calls
and does not provide a new model-strength or whitepaper milestone.
