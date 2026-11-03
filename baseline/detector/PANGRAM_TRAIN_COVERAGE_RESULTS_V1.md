# Saved Pangram coverage in the frozen training set

The [exact-match audit](results/pangram-train-coverage-v1.json) joins all 485
observations from the five local-generator cohorts to original V6 Train. They
cover 12.65% of its 3,833 records and 131 of 471 source families (27.81%).
Every submitted hash matches Train only, with no additional same-text aliases
or matches into Development, Calibration or Test. All observations are Pangram
4.0. This audit excludes other provider cohorts and makes no new calls.

| Final recorded generator | Train records | Annotated records |
| --- | ---: | ---: |
| Human source root | 471 | 131 |
| Qwen3-30B-A3B-Instruct-2507 | 1,296 | 140 |
| Mistral-Small-3.2-24B-Instruct-2506 | 1,228 | 94 |
| OLMo-3-7B-Instruct | 838 | 120 |

Generator counts include model drafts, human-text edits and successive revisions.
The report retains all three declared model revisions and ordered generator
routes. Both requested and returned model names are `default_model` in these
records; the declared revisions distinguish the loaded models. The first audit
grouped the request alias alone, which hid this distinction. That initial output
is retained on Studio. The corrected grouping uses the existing attribution
identity tuple; all other aggregates and the local joined observations are
unchanged.

| Origin or revision stage | Annotated | Exactly zero | 60% to below 100% | 100% bin |
| --- | ---: | ---: | ---: | ---: |
| Human roots | 131 | 131 | 0 | 0 |
| Entirely model-written, all stages | 223 | 0 | 18 | 203 |
| Model edits of human roots | 131 | 2 | 23 | 98 |
| First successive model revision | 46 | 0 | 1 | 45 |
| Second successive model revision | 46 | 0 | 0 | 46 |

The last two rows are subsets of the entirely model-written row. Twelve nonhuman
texts fall below 60%, including the two mixed-origin zeros; the table's selected
columns therefore do not sum to every row total. The 100% bin uses the original
0.00001 floating-point tolerance. Raw fractions remain unchanged.

Only 41 texts occupy the requested 60-100% interior. Of 92 successive revisions,
91 score 100%. Every model-only text scores at least 10%, while every human root
scores zero. These statements concern Pangram content fractions, not A's
probabilities or calibrated provenance. A zero for a mixed workflow does not
erase its recorded model involvement.

The data supplies varied source and prompt examples, but little graded response
evidence for successive revisions. It does not establish a surrogate that tracks
word or clause edits, and it does not test the known private B failure. The
capacity comparison must still determine whether the larger pretrained model
generalizes better with the existing data. The current base and large
comparisons retain their provenance objectives;
neither incorporates these teacher observations. Finish the six saved-model
Train-fit runs and the fixed capacity comparison before choosing a remedy.
The [completed Train-fit review](TRAIN_FIT_RESULTS_V1.md) now supplies the former:
strong binary Train fit alongside remaining provenance and formatting errors.
The fixed capacity comparison remains in progress.
Any later provider-supervised comparison needs its own protocol and evaluation
families, as specified in the [audit protocol](PANGRAM_TRAIN_COVERAGE_V1.md).

The Rust audit rechecked exact provenance, fractions and echoed text. There are
174 exact echoes, 310 whitespace-only echoes and one source-soft-hyphen removal.
An [independent check](results/pangram-train-coverage-independent-v1.json)
reproduced all slice counts, raw joined fractions and generator routes across
all 5,463 original shard records. Formatting, all-target tests and Clippy passed
on the Studio. Linux execution remains unverified. No model, training input,
split assignment, paid reservation or reference designation changed.

On 2026-11-01 the protocol's closing paragraph and the report's policy fields
were restated under the project's publication policy. The report's protocol
and source digests and the independent check's report digest now bind the
restated files; every audited count, fraction and route is unchanged.
