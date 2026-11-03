# Source edit menus, development protocol v1

The generator proposes alternatives for particular words, phrases and clauses.
The selector chooses existing alternatives by ID. Rust applies the edits to
their exact source spans and measures the resulting passage. A whole-passage
rewrite is not a menu choice.

Do not assume that a useful edit immediately lowers A. Enumerate every subset
through the declared search depth, including paths whose first edits increase
the score. A small exhaustive menu is the first control for a later wider
search with multiple retained paths. The current implementation does not claim
to find the best edit outside its menu or depth limit.

The depth limit may span all 16 allowed choices, subject to a maximum of 512
enumerated subsets. Nine binary choices fit a complete search in that budget.
Larger menus need an explicitly shallower depth. Exceeding the state budget
fails rather than silently pruning paths; overlapping patches and word-budget
violations remain recorded rejections. Earlier frozen menus keep their bounds.

The separate `max_source_patches` limit counts physical source spans, including
punctuation and capitalization. It defaults to 16; an experiment may explicitly
set 1 through 64. A sentence join can require two patches, so a bundle can use
this allowance before reaching the choice-depth limit. Increasing it preserves
the word-distance limit, source anchors and editorial requirements. A continued
walk must explain any change with `--amend-patch-budget-reason`; the bound root
source and claim checks remain unchanged. Historical plans without this field
retain their limit of 16.

With `include_grammar_rules: true`, an optional `grammar_rules` list limits
generation to named catalog rules. An empty list or a list with generation
disabled fails. Omitting the list keeps the complete catalog. Imports of
explicit lexical or model proposals clear the grammar selection.

An optional `parser_model` selects the installed English pipeline for the whole
menu and its references. The default remains `en_core_web_sm`. A continued walk
retains its model choice and actual parser asset identity. See the
[parser protocol](../../grammar/PARSER_CHOICES.md) before comparing pipelines;
their fitted spaces are separate experiments.

## What is measured

Each state includes the unchanged passage or a compatible subset of alternatives.
All choices refer to the same original source for that round. An edit may supply
`start_byte` to identify one occurrence of repeated text. The UTF-8 span must
match `before` exactly; an offset never overrides the source check. Grammar
operations retain their original byte positions when imported. Without an
offset, the anchor must be unique, including overlapping substring matches.
Reject overlapping edit spans, ambiguous anchors, changed digit tokens, hidden characters and changes beyond
the word edit budget. These mechanical restrictions do not establish semantic
equivalence. In particular, numeric equality does not protect quantifiers,
conditions, negation, certainty, causal claims or implied obligations.

Parse the full resulting passage with the existing spaCy adapter. Fit the
grammar and word-occurrence spaces only on independent author reference works.
Keep two views: the original full vocabulary and grammar measurement, and a
versioned function-word/grammar view that groups other word counts into one
residual coordinate. The residual preserves the total-word denominator. This
reduces one source of topic matching; it does not make the representation free
of topic effects. Neither distance is a probability of authorship or a quality
score. Retain reference provenance and any copying signals.

The optional [sentence-order audit](../../grammar/ORDER_FEATURES.md) adds a
separate view of sentence position and construction transitions. It preserves
old menu measurements and reports fixed mixtures with the occurrence view.
Use the audit to test candidate rankings; a changed vector or smaller distance
cannot substitute for editorial review or measured detector performance.

Score every distinct full text with the same frozen A artifact. The initial
run uses V6 `4e061df4789e546dfdd58c8005860a98fb8f200a4c7642b7491461f44f16f138`.
Its combined model/assistance probability is not the percentage of AI words.
For each possible next edit, report the difference from its immediate parent,
as well as the whole combination's difference from the source. Report the
departure from the sum of individual effects. A negative departure means the
combination reduces A more than that additive calculation predicts.
Probability calibration and saturation can contribute to this departure; it
does not by itself identify an interaction inside the detector model.

After selecting a revision, construct and score a new menu in its new context.
An edit's effect measured in the previous passage must not be reused as its
effect in the revised passage. Keep the root source, brief and claims throughout
the walk, and compare every selected passage directly to them to catch gradual
meaning drift. Retain unchanged text as an available decision.

## Editorial selection

Rankings are observations, not approved output. Selection requires an explicit
review bound to the plan and exact candidate ID. The selector cannot supply
new prose through this command. Meaning, requested tone, readability, cleanup
and author fit must all pass. Treat readability and cleanup as no-regression
requirements, and seek positive improvement. A detector reduction cannot
compensate for an editorial failure. The intended tone comes from the request;
it can differ from the original tone when the user requests a style change.

For every proposed final revision, name the editorial improvement and show
where it occurs: a clearer referent, a condition next to its qualification,
a more direct request, or a better order for the reader's task. Compare with
both the immediate parent and the original source. Do not count a detector
reduction, a smaller vector distance or mere synonym substitution as evidence
of better writing. Neutral edits may remain research probes or intermediate
states; they do not establish the final revision's quality benefit. Keep the
unchanged source when the available revisions make the writing worse.

An LLM or human review is a fallible judgment. Record who reviewed, whether they
saw detector scores and what they checked. A self-review is not independent
validation. Review complete combinations, since individually acceptable edits
can create repetition, ambiguity or a different implication together.

The [interactive harness calibration](INTERACTIVE_HARNESS.md#editorial-calibration)
distinguishes promising research candidates from approved edits. Check added
logical connectives for unsupported inferences, and check negative constructions,
repeated directions and formal substitutions against the requested audience.
Reference style does not override that audience or register. Retain specific
gains and repairable failures with reviewer provenance; an informed review by
the source's drafting model is useful feedback, not independent validation.

## First development run

Use the two existing synthetic operational notices so the full-rewrite failures
remain a comparison. Choose eight explicit lexical/clause alternatives per
source before inspecting their A scores; also enumerate applicable existing
grammar rules. Search through three choices with a declared state budget.
Retain failures and do not retry with unrecorded alternatives.

The two reference works are the earliest by date/ID for Juke Carolina in the
local Global Voices transfer collection: `gv:572191` and `gv:589277`, both from
2016. The author was chosen by alphabetical account order among the eligible
pre-2022 accounts, before scoring this menu. These are attributed publisher
excerpts with recorded extraction and provisional license evidence. Their
production history is not verified; no commercial training release is cleared
by this experiment. Keep text and provenance in ignored local data. Removed
quotations left some introductory paragraphs without their quotations, which
limits their usefulness as references. Do not reproduce those omissions in B.

The first lexical choices are assistant proposals, not a trained synonym
distribution. The grammar proposals come from the existing parser-bound rules.
The measured reference distributions inform ranking. Learning contextual
alternative frequencies from a larger author corpus is subsequent work.

Run all parsing, model inference, builds and tests on the Mac Studio. Preserve
exact input, plan, text and detector hashes, rejected states, score tables and
selection reviews. Raw B text stays ignored. Do not spend Pangram credits on
every search node. Review and freeze candidates before external confirmation
under the existing cumulative budget. No score from this adaptive development
run counts as held-out performance, Pangram success or confidential execution.

```sh
slop_ninja_transformer menu prepare --input /path/to/menu-input.json \
  --python /path/to/spacy/python --out /path/to/new-menu
# Use the pinned A runner on new-menu/detector-requests.jsonl.
slop_ninja_transformer menu rank --plan /path/to/new-menu/plan.json \
  --scores /path/to/scores.jsonl --out /path/to/ranking.json
slop_ninja_transformer menu select --plan /path/to/new-menu/plan.json \
  --review /path/to/selection-review.json --out /path/to/selection.json
```

Continue with `menu prepare --parent-selection /path/to/selection.json` and
the parent's exact hash in the new input. The command checks the selected text,
root source, reference set, intent, claims, detector and cumulative edit budget.

The initial 0.40 word edit fraction is a search limit, not a fidelity measure.
Word Levenshtein distance also charges for clause movement. To study a longer
walk, set a new limit in a new input and pass `--amend-word-budget-reason`.
The command records the old limit, new limit and reason in the continuing
binding. The root source, claims, intent, reference set and editorial gates
remain fixed. Earlier experiments and exclusions are not rewritten. The
supported limit is at most 1.0; actual alternatives still use exact source spans
and each menu has a bounded depth and state count.

For external confirmation, the detector crate's `pangram_bulk plan-b` accepts
reviewed menu selections rooted in MIT synthetic fixtures. It freezes the root
sources and selected revisions; it does not submit reference-author excerpts.
The B plan and resulting annotations have separate schemas and exclude these
texts from detector training. The existing submission command uses the same
cumulative budget ledger, reservation, no-retry and exact-echo collection rules
as the corpus experiments. The ordinary corpus planner continues to reject
synthetic fixtures.

Record Pangram's AI, AI-assisted and human fractions separately. For the joint
low-detection objective, count both AI and AI-assisted content as flagged.
Moving content from AI to AI-assisted is not a successful human classification.
Retain every returned window with its span, label and score: segmentation can
change between candidates, so a first-window score cannot represent the full
passage. Window scores are auxiliary observations, not calibrated percentages
of AI content. Repeat promising revisions and compare the complete results.

Join collected evidence without another network call:

```sh
slop_ninja_transformer menu external-report --plan /path/to/menu/plan.json \
  --scores /path/to/scores.jsonl --annotations /path/to/annotations.jsonl \
  --out /path/to/new-external-report.json
```

Repeat `--annotations` for distinct paid batches. The report rejects a second
collection of the same request, checks text echoes and the frozen A artifact,
retains unmeasured menu IDs, and omits raw prose from its aggregate. It lists
each provider bulk ID as a salted digest (see
[the evaluation notes](../../docs/evaluation.md#provider-ids-in-shareable-results)),
so the salt must be readable; raw bulk IDs stay in the collections and the
private sidecar map. Its
word-weighted window mean is a declared diagnostic only; changing segmentation
can change this mean. The joint detector target requires A below 0.1 and total
Pangram flagged content below 0.1. Meeting those numbers would still require
separate editorial, author-fit and independent evaluation evidence.
