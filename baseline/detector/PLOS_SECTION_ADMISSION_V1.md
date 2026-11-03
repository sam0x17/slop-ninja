# Scientific introductions for weak supervision

Status: [13 complete introductions admitted](results/plos-section-admission-v1.json)
as unsplit historical proxies. Twenty candidates remain pending: 19 lack a
supported complete rendering and one has unresolved quotation permissions.
No sections have been assigned a split, generated from or scored under this
protocol. Both running A fits and their comparisons stay frozen. This cohort
is for a later training experiment.

The later [numeric-notation extension](PLOS_NUMERIC_SCRIPTS_V2.md) admitted four
additional introductions in a separate supplement. The original 13 records and
this protocol's captured artifacts remain unchanged; the two cohorts together
contain 17 admitted sources. The new supplement has no split assignment.

The [cached inventory](results/plos-section-inventory-v1.json) identifies 33
candidate introductions from distinct works absent from the existing source
exclusion inputs. They have 650–1,600 whitespace words, English research-article
metadata, a 2019 publication year and an explicit CC BY link. Use that fixed
pool, ordered by section hash, without searching for replacements after failures.
Retain every candidate's disposition and reason.

## Evidence and permitted claims

Use the [training plan's historical-proxy lane](PLAN.md#3-acquire-training-data-in-two-evidence-lanes).
The current publisher XML records an old publication date; it does not prove
that the captured bytes existed then or document a human-only workflow. Keep
`Evidence::HistoricalProxy` and that limitation in every admitted record. The
`HumanOnly` class is a weak training target, not a verified authorship finding.
An immutable older revision would strengthen the evidence but is not required
for this explicitly declared weak-supervision cohort.

Do not use the cohort for a definitive human false-positive claim, a claim of
unseen encoder pretraining data, or an individual-writer benchmark. Preserve
the complete credited author list; a multi-author article is not a sample of
one person's verified writing style. Any later evaluation must report the
evidence class and its limits separately.

## Item review and faithful extraction

Verify the captured request/body hash, DOI, publication metadata, exact section
range and section hash against the inventory. Check the full permission notice
and credited authors, including any item-specific exceptions. Record the title,
DOI, license version/link, permission evidence and extraction changes in the
attribution. A license link alone does not complete admission.

Read every selected introduction and inspect its markup before admitting it.
The [first three readings](results/plos-section-manual-review-v1.json) already
cover their complete introductions. Preserve their identified source errors;
do not correct gene identifiers, agreement or punctuation in a human reference.
Review quote-like material in context. Terms and response labels are different
from externally attributed passages; record the decision instead of silently
deleting either kind of text.

Implement the extraction in Rust, beginning with a deliberately small supported
JATS subset. Preserve all direct prose paragraphs in order and separate them by
blank lines. Keep the heading in metadata. Decode entities and normalize only
intra-paragraph whitespace. Plain emphasis may lose typography, with that change
recorded. Do not omit nested sections, lists, equations, figures or other
unhandled content to make a section pass; retain the whole candidate as pending
or rejected until its representation is supported.

Preserve each citation's visible label and surrounding brackets, punctuation
and range marks. Resolve its bibliography target IDs in the same article and
retain those bindings as metadata. Do not insert bibliography prose into the
introduction. Missing targets, unknown cross-reference types or an ambiguous
projection require review.

Subscripts and superscripts need a separate faithful representation; stripping
the tags can change notation. The third reviewed introduction contains receptor
subscripts and must remain pending until that support is implemented and its
complete rendering checked. Do not silently flatten it to pass the initial
renderer. Record raw paragraph spans and hashes alongside the rendered text so
manual comparison can trace every change. Parser acceptance is not approval of
the extraction or certification of the source's grammar.

## Admission and later use

Check canonical DOI membership and exact/normalized text duplicates against the
frozen historical work ledger, V6 fresh sources and all existing split records.
Inspect related-work metadata and substantial shared passages; different DOIs
alone do not establish independence. Flag unresolved overlap before assigning a
split. Preserve citation text while distinguishing short conventional phrases
from evidence of a shared document.

Count complete rendered texts with the pinned A tokenizer on the Studio,
including special tokens and without truncation or inference. Keep unsupported
lengths in the inventory; no shortened substitute may become a whole-document
observation. Inspect the complete final rendering before admitting an unsplit
record. Store source text and detailed audit artifacts on the Studio; commit
code, policy and aggregate metadata only during this preparation.

Freeze cohort membership, source-group allocation and any generation recipe
before a later fit. All descendants inherit their source group. The active
train, development, calibration and test files receive no changes. Recorded
model drafts can supply model-origin training examples even when a draft fails
fidelity review; such failures cannot support a claim that a comparison isolates
style while holding meaning fixed. This preparation makes no Pangram calls.

## Bound corpus overlap screen

The Rust `audit_plos_overlap` example screens the 14 supported complete
renderings. Its plan binds the preparation, historical ledger, original input
manifest and every comparison file. Recover every historical ledger row by
its original file and line, verifying identity and exact and normalized text
hashes. A subset of input files may cover that ledger, but no ledger row may
remain unrecovered. Include current V6 splits and assembly, all fresh V6 roots,
the two admitted long-source cohorts and the hosted-model archive as additional
bound inputs. Compare the prepared introductions with one another too.

Use the existing Rust normalized word tokenizer and find shared contiguous
runs of at least five words. Record exact and normalized duplicates, the
longest shared run and the fraction of candidate word positions covered by
such runs in each comparison text. Nominate manual review for a normalized
duplicate, a run of at least 20 words or coverage of at least 20%. These fixed
thresholds identify review work; they do not establish copying or independence.
Retain the top five matches for each candidate, including unflagged matches.
Do not count repeated occurrences of the same corpus text as independent texts.

The audit retains related-item metadata for inspection against the observed
work groups. Short conventional phrases, citation labels and a related work's
existence need context. Paraphrased overlap may evade this lexical screen.
An empty result means no shared run of five or more normalized words in the
declared inputs. It does not certify independence from all published writing.
Only matched passages nominated for review need to be opened; preserve any
resulting inspection history for affected evaluation families. The audit makes
no split assignments, training admissions or model calls.

The completed screen recovered all 6,847 historical ledger rows through 20
original files. With the additional corpora, it covered 29 bound files,
25,060 record occurrences and 9,243 distinct existing texts. None of the 14
supported introductions matched an existing work group or had a duplicate or
flagged shared passage. The longest normalized shared run was nine words.
These are results for the declared inputs and thresholds, with the limits above.

`admit_plos_sections` materializes the separately reviewed decisions. It binds
the complete-rendering reviews, tokenizer audit and overlap results; checks
each permission notice against its original XML span; builds full attribution;
and validates the resulting origin records. Every original candidate receives
a disposition. Existing source errors and citation labels remain in the texts.
The 13 admitted records have no split and remain on the Studio outside the
repository, together with the exact source and review artifacts.

The companion DOI `10.1371/journal.pone.0205138` was absent from the checked
corpus. Its alias is reserved under the same family as
`10.1371/journal.pone.0201196`. A future importer or split assignment must honor
that mapping if the companion is acquired; two distinct DOIs do not authorize
splitting this pair. The introduction with external quotations,
`10.1371/journal.pone.0204187`, remains pending in full. Its lexical result does
not resolve its separate permission question.

Format, tests and all-target Clippy checks passed on Studio macOS arm64, as did
the actual corpus admission. Linux execution of these commands remains
unverified. Admission supplies new source material, not a detector milestone.
