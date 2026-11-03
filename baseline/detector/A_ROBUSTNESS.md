# A robustness work

The current priority is to improve A before resuming joint A/B iteration.
The [paired B checks](../transformer/DETECTOR_TRANSFER.md) show that large A
reductions can produce little improvement on Pangram. A's ordinary held-out
detection results do not establish resistance to those edits or usefulness as
a Pangram surrogate. Keep V6 frozen as the comparison model.

The [completed origin-consistency comparison](ORIGIN_CONSISTENCY_RESULTS_V1.md)
reduced the fixed private edit sequence's score drop from 38.50 points with V6
to 9.99 points with consistency training. It also worsened human-text probability
estimates and binary loss on every corpus view. No replacement reference is
qualified. The planned larger-backbone pair is now fitting on the Studio.

The [saved edit-interaction audit](EDIT_INTERACTION_RESULTS_V1.md) finds large
interactions in both underlying class-logit contrasts. Single-edit effects
would predict 72.43% for the three-edit repair; the recorded result is 3.97%.
That exact finalist remained 100% AI-written on Pangram. This retrospective
diagnostic supports investigating combinations on permitted new material;
the private B texts remain excluded from training.

The [structural revision pilot](STRUCTURAL_REVISION_RESULTS_V1.md) found one
accepted Train-source edit that lowered V6 from 90.94% to 72.84%, crossing its
stricter threshold. A [component follow-up](STRUCTURAL_COMPONENTS_RESULTS_V1.md)
isolated a larger drop from the passive recast alone, to 67.71%; the pronoun
substitution alone slightly raised A. No new state fell below 10%,
and most proposed combinations failed editorial review. The next data work
can investigate that particular transformation without admitting private B
outputs or changing the active fits. The [three-source voice follow-up](VOICE_PROBES_RESULTS_V1.md)
found drops below 0.35 points, with every candidate still above 97%. The news
result remains a local failure; a broad passive-voice weakness is unestablished.

The scientific pilot's [completed V6 diagnostic](SCIENTIFIC_PILOT_V6_RESULTS.md)
flags all twelve Qwen drafts but only two of eleven Mistral drafts at the
stricter threshold. Eight Mistral drafts were source-copy exclusions; all three
of its admitted drafts pass the looser threshold. The three historical human
sources score below 4%. Keep those groups distinct and the remaining generation
paused. There are no Pangram annotations for this new cohort.

The working hypothesis is reliance on cues from the limited generator, style
and length coverage of the training data. The score mismatch does not establish
how Pangram represents text. The paired control/consistency experiment tests
whether tying predictions across generation and revision steps improves
detection on other distributions without raising human false-positive rates.

The [capacity and gradient-transfer plan](CAPACITY_AND_GRADIENT_TRANSFER.md)
adds a matched larger-backbone comparison, text/feature/fusion alternatives and
an explicit provider-surrogate target for later testing. It also defines the
distinction between fitting hard examples, generalizing to new sources and
supplying edit directions that transfer to Pangram. None is implied by the
others. These next experiments leave the active fits and comparisons unchanged.

The larger candidates now have a separate
[private transfer protocol](CAPACITY_PRIVATE_TRANSFER_V1.md) and
[completed input preparation](results/capacity-large-private-preparation-v1.json).
The preparation verified 164 bound files and reproduced the three saved V6
reports. It made no candidate predictions. The existing base comparison stays
intact; both sizes must complete the prescribed public checks before the larger
candidates are compared on the known private failure.

## Coverage audit, 2026-09-14

The [reproducible aggregate](results/a-coverage-audit-v1.json) was produced by
`a_coverage_audit` from the exact training shard, its frozen token audit and the
published evaluation reports. It verifies training hashes, source counts,
record admission, artifact identity and complete profile coverage. It runs no
models, makes no provider calls and copies no source text into the aggregate.

V6 trained on 3,833 records from 471 source groups. The human sources comprise
196 CMU book-summary groups, 178 historically matched HC3 Wikipedia groups,
44 PLOS groups and 53 Wikinews groups. There are 471 human-only records, 1,227
mixed-origin records and 2,135 model-only records, including revision stages.
Repeated generations do not increase the number of independent human sources.

All generated training records came from three pinned model families: Qwen,
Mistral and OLMo. Their counts as the final generator are 1,296, 1,228 and 838,
respectively. These counts include edits and revisions, and do not replace
the ordered generator histories. The collected Claude/GPT passages did not
enter this fit. Their existing corpus admission flags remain unchanged.

The separate [GPT-OSS compatibility probe](GPT_OSS_GENERATOR_V1.md) verified a
fourth local generator family and exposed a reasoning-channel issue in the
installed MLX endpoint. The Rust collector now extracts its explicit final
channel while preserving the raw response. One synthetic request completed but
failed strict fidelity because it added facts. This prepared the generator pilot
below; it supplies no new Train records or evidence of improved A quality.

The [Train-source GPT-OSS pilot](GPT_OSS_TRAIN_PILOT_V1.md) now freezes 24
requests: four existing Train roots under six requested styles. Its
[preparation check](results/gpt-oss-train-pilot-preparation-check-v1.json)
independently reproduced source selection and verified all rendered prompts,
including a fixed template date. All four source-matched requests completed and
passed collection checks; [manual review](GPT_OSS_TRAIN_PILOT_SOURCE_MATCHED_V1.md)
found source-fidelity failures in all four. The next
[plain-profile review](GPT_OSS_TRAIN_PILOT_PLAIN_V1.md) covers four completed
attempts: three collector records and one source-copy exclusion, all with
fidelity defects. The third [informal-profile review](GPT_OSS_TRAIN_PILOT_INFORMAL_V1.md)
covers four more collector records, again with fidelity defects and inconsistent
realization of the requested register. The fourth
[direct-profile review](GPT_OSS_TRAIN_PILOT_DIRECT_V1.md) covers four further
collector records, all with fidelity defects. The fifth
[anti-AI-profile review](GPT_OSS_TRAIN_PILOT_ANTI_AI_V1.md) reads four complete
responses, including one excluded for repeated analysis messages. The
[V2 format parser](HARMONY_RESPONSE_V2_RESULTS.md) handles that sequence in a
separate replay, preserving the frozen exclusion. All 20 reviewed outputs have
fidelity defects. The final
[Fix Slop review](GPT_OSS_TRAIN_PILOT_FIX_SLOP_V1.md) closes all 24 attempts:
20 collector records, one source-copy exclusion, two format exclusions and one
incomplete response. All 23 complete final passages have fidelity defects.
These are still collection experiments;
the live fits and their evaluation inputs remain unchanged.

A separate [Global Voices cache screen](GLOBAL_VOICES_SOURCE_SCREEN_RESULTS_V1.md)
verified 840 saved account/article observations covering 820 works. The preliminary
date and extraction filters retain 174 works from seven accounts. Manual review
of eight selected article bodies found context loss in all eight old extracts,
including missing interview answers and lost narrative paragraphs. Reusing these
sources requires explicit block and attribution boundaries. A new
[Rust document view](GLOBAL_VOICES_DOCUMENT_VIEW_RESULTS_V1.md) preserves all
233 paragraphs and 319 regions in this packet. Manual review covered every
region. The [first contiguous passage review](GLOBAL_VOICES_PASSAGES_RESULTS_V1.md)
then read all eight nominated openings: seven were locally connected with stated
limits, while one interview introduction still needs more context. Two openings
are only 57 and 61 words. Structural success does not decide passage suitability.
Publisher timestamps do not independently establish historical text or human
authorship, and no records have been admitted to detector training through this
screen.

| Partition | Records | Median tokens | Maximum tokens |
| --- | ---: | ---: | ---: |
| Train | 3,833 | 237 | 1,050 |
| Development | 530 | 302 | 767 |
| Calibration | 540 | 280 | 998 |
| Primary Test ancestry archive | 560 | 287 | 738 |

Counts include tokenizer special tokens. The longest human training record is
727 tokens. A accepts up to 2,048 tokens, but the evaluated private candidate
is 1,878 tokens, outside the observed training-length range. This identifies
a coverage gap; it does not establish length as the cause of that failure.

All six initial prompt profiles were represented during training. The profile
slices therefore evaluate familiar requested styles on other sources. They
are not held-out-style experiments. Primary original-draft binary accuracy
ranges from 85.0% for informal requests to 97.37% for source-matched requests.
Phi-4 original-draft accuracy ranges from 88.24% for Fix Slop to 97.06% for
anti-AI requests. These overlapping, small slices use different source/profile
assignments and cannot isolate a causal style effect. Phi-4 was excluded from
fitting but had been observed in earlier experiments.

The primary revision route always uses Qwen Anti-AI followed by Mistral Fix
Slop. Its successful revision detection does not establish robustness to
arbitrary editing methods, local grammatical changes or other model chains.

The [fixed human Train inspection](TRAIN_REGISTER_RESULTS_V1.md) read 16 complete
sources selected independently of scores. Promotional, formulaic and visibly
awkward prose already occurs in this sample, so those registers are not wholly
absent from Train. Review also found an editorial news digest. A comparison
against all 5,463 original shard records found one shared headline in another
Train digest and no runs of eight or more normalized words in the other splits.
The inspection changes no source label or fit. The subsequent
[complete Train-fit diagnostic](TRAIN_FIT_RESULTS_V1.md) scored all 3,833 records
under three saved models and both formatting views. Binary accuracy is
98.89-99.65%, while three-class accuracy is 82.29-87.40%. Eight selected error
texts received complete review. One source differs by a single leading space,
yet V6's combined model/mixed score changes from 14.25% to 97.18%. Strong binary
Train fit supports a generalization problem, with unresolved model/mixed
separation and formatting sensitivity. The larger-capacity comparison remains
unchanged and must finish before choosing a new fit recipe.

The [formatting audit](TRAIN_FORMAT_RESULTS_V1.md) subsequently confirmed that
all 196 human CMU summaries start with one ASCII space, while none of their
1,480 model-only or mixed descendants does. Removing that byte is the only
change in the human subset's paired views. V6's loose-cutoff false positives
rise from 1 to 17 on those same texts. This identifies a collection artifact
and measured sensitivity, without proving that a prefix evades A on model text.
The [paired boundary-space probe](BOUNDARY_SPACE_PROBE_V1.md) now tests that
intervention on every descendant of the 16 earlier hash-selected Train roots.
Its [preflight](results/boundary-space-preflight-v1.json) independently verified
126 records, two exact views each and the original three detector artifacts.
The [completed 756-observation probe](BOUNDARY_SPACE_RESULTS_V1.md) found a
model-only V6 score change from 96.73% to 1.30% after adding one leading space.
Both base refits were much less sensitive on model-only text, but human and
mixed cases retained larger changes in either direction. All three results and
their collection/origin slices were reviewed together; eight selected passages
received complete manual inspection. This identifies a specific V6 weakness
without qualifying either refit or establishing Pangram transfer.

Surface errors must not become a shortcut to human origin. The
[next paired panel](SURFACE_NOISE_PREPARATION_V1.md) prepares 386 interventions
for extra internal spacing, spelling transpositions, repeated determiners and
terminal punctuation. The first three cover the same 126 records across all
origins; punctuation eligibility covers only eight records from two families.
All mutations were independently checked, and all eight punctuation contexts
received complete source review. The other 378 contexts remain unreviewed;
scoring has not started. Future invariance training must retain absolute human
and model classification performance, rather than reward constant predictions.

The separate [draft-method comparison](GPT_OSS_DRAFT_METHODS_V1.md) has
[prepared four prompt conditions](results/gpt-oss-draft-methods-preparation-v1.json)
to test word-count targets and a source-claim check. The new opt-in CLI policies
reproduced all 24 old pilot requests, task identities and run metadata exactly
when disabled. Sixteen new requests are prepared, but rendered-prompt review
and generation are on hold while surface-error robustness takes priority.

The [saved Pangram coverage audit](PANGRAM_TRAIN_COVERAGE_RESULTS_V1.md) exactly
matches 485 observations from 131 families to Train, covering 12.65% of its
records with no held-out hash matches. Only 41 observations lie between 60%
and 100% exclusive; 91 of 92 successive revisions score 100%. This limits the
available graded supervision for a future edit-sensitive provider surrogate.
It changes no provenance label, fitting recipe or evaluation assignment.

## Hosted-generator diagnostic

All 96 exact hosted captures were evaluated with the frozen V6 CPU runner.
There were no unsupported inputs, truncations or failed responses. The
[aggregate comparison](results/hosted-v6-diagnostic-v1.json) reuses their
existing V5 and Pangram observations, with no new provider calls. The input
preparer validates the complete capture audit and prior provenance; the
reporter checks exact request hashes, coverage, probabilities, artifact,
runtime and frozen thresholds.

| Generator | Pangram flags | V5 stricter | V6 stricter | V6 looser |
| --- | ---: | ---: | ---: | ---: |
| GPT-6 Astra | 23/24 | 23/24 | 21/24 | 24/24 |
| GPT-5.6 Sol | 21/24 | 21/24 | 20/24 | 22/24 |
| Claude Opus | 20/24 | 11/24 | 13/24 | 17/24 |
| Claude Sonnet | 21/24 | 17/24 | 14/24 | 20/24 |
| Total | 85/96 | 72/96 | 68/96 | 83/96 |

V6's stricter threshold is 0.8436493155 and its looser threshold is
0.4287369114, both applied with strict greater-than comparisons. The stricter
comparison has 64 joint detections, 21 Pangram-only detections, four V6-only
detections and seven joint misses. All 96 workflows are model-only regardless
of either detector's output. No Pangram-flagged text scored below 10% on V6 in
this unoptimized cohort; the near-zero adversarial B results are a distinct
failure mode.

By requested profile, V6's stricter setting detects 16/16 Anti-AI outputs but
only 6/16 Fix Slop outputs; Pangram detects 15/16 and 14/16, respectively.
V6's looser setting detects 13/16 Fix Slop outputs. That difference supports
investigating style sensitivity but cannot isolate its cause: each profile
uses only four source families, and those families differ between profiles.
Claude Opus also remains a weak generator slice at the looser setting.

The 24 roots occur in Train, and the cohort has no independent human controls.
It supplies neither a fresh-source accuracy estimate nor a false-positive rate.
The original evaluation reported V6 human flags of 3/117 at the stricter cutoff
and 12/117 at the looser one. Those controls are a separate cohort; they explain
why threshold relaxation needs a sensitivity/false-positive comparison but do
not create matched population operating points for this hosted sample.

The legacy sum of Pangram's two float32 components exceeded one by `2.98e-8`
on one record. The new report checks the raw components and retains that sum,
then uses `1 - fraction_human` as the bounded flagged fraction. It changes no
Pangram flag in this cohort. The first report attempt rejected the over-one
sum before writing results; no inference was repeated.

## Fixed grammatical interventions

The first [reviewed stress diagnostic](results/a-edit-stress-v1.json) contains
26 full-text states from nine passages: a human original, an initial model
draft and a twice-revised model draft for each of three Development source
families. Select the first family in each collection's previously frozen hash
ranking. The broader preparation contains 145 states from 36 passages; the
remaining 119 states have not been scored or manually approved.

The preparer selects the first negative-form change and up to two clause-boundary
changes by source position, then enumerates their combinations. Selection uses
no detector observations. This reviewed subset contains sentence joins and
negative contractions; expansion and semicolon splitting were eligible rules
but supplied no selected edits here.

Before scoring, the assistant read every complete state and its patches and
inspected subject, predicate and negation dependencies. The
[frozen review](results/a-edit-stress-v1-review.json) records nine unmodified
controls, nine edited states admitted for this diagnostic, six with density or
discourse concerns, and two with a formal-register change. All 17 edited states
retain the claims of their immediate source. This does not certify the quality
of those sources or fidelity of earlier model revisions to the human ancestor.
The review is one assistant's assessment, without author approval.

| Source collection | Largest human score change, absolute pp | Largest model score change, absolute pp |
| --- | ---: | ---: |
| CMU historical summaries | 0.0014 | 0.0881 |
| PLOS | 1.7576 | 0.3062 |
| Wikinews | 0.0290 | 0.1005 |

All 26 CPU responses passed exact-text, artifact, probability and token-limit
checks. No intervention changed a classification at either frozen V6 threshold.
The largest model-score movement was a 0.3062-point decrease; the largest human
movement was a 1.7576-point decrease. No provider calls or fitting occurred.
These small familiar-source results do not establish general robustness or
external transfer. They do show that these particular punctuation and contraction
edits alone do not reproduce the earlier large A failures.

Manual inspection also found wrong roots for absolute constructions and a false
sentence split at an abbreviation in unedited context. Those findings are in
the review and the [grammar review record](../../grammar/CAMPAIGN.md).
The edited clauses can be usable even while other parts of their syntax
annotation are wrong. Do not treat either result as full-parse certification.

## Cached scientific sections

The [offline PLOS inventory](results/plos-section-inventory-v1.json) checks the
full article XML already captured by the earlier abstract collector. Of 150
distinct DOI captures, 70 are absent from the frozen historical work index and
V6 fresh-source archive. Thirty-six introductions from those works contain
658–1,284 whitespace words. Thirty-three are marked as English research articles
from 2019 and contain an explicit CC BY link. These are candidates for review;
no section has been admitted or assigned to an experiment.

All 36 contain citation cross-references, 14 contain double quotation marks and
three contain formula markup. The counts overlap. The
[Rust inventory](examples/inventory_plos_sections.rs) records whole-section byte
ranges, hashes and markup while keeping section boundaries separate. Its
paragraph word counts exclude figure, table, footnote and supplement containers;
they are diagnostic projections, not approved text extractions or A-token counts.
Review content, item rights and rendering before generation. A current capture
with a 2019 publication date can support the [plan's weak-supervision lane](PLAN.md#3-acquire-training-data-in-two-evidence-lanes),
provided its unverified historical byte identity remains explicit. It cannot
establish documented human authorship or support a definitive human false-positive
benchmark. Stronger historical claims require version evidence. The inventory
makes no network or model calls and changes neither active fit.

The [first three introduction reviews](results/plos-section-manual-review-v1.json)
cover all 21 paragraphs of three samples chosen by section hash from the 33
sources with the stated date, language, article-type and licence-link flags.
Each has an explicit attribution-based CC BY notice in the cached XML. The
first source alternates between Gly428Ser and Gly482Ser; the third has an
unclosed parenthesis and a verb-agreement error. Preserve those source defects.
The second develops opposed theoretical predictions that a later rewrite must
keep separate. Citation ranges and receptor subscripts also need deliberate
rendering. This was a manual prose/markup review, not a parser-accuracy test;
historical byte identity, admission and A-token counts remain unresolved.
The [section admission protocol](PLOS_SECTION_ADMISSION_V1.md) defines a future
weak-supervision cohort and the remaining rights, rendering and overlap checks.
It admits no text and does not change the active training or evaluation inputs.

The [Rust preparation run](results/plos-section-preparation-v1.json) rendered
14 of the 33 fixed candidates completely; all 14 fit A at 854–1,698 tokens.
The remaining 19 require support for notation, other reference types, links or
nested sections. They remain whole and unadmitted. All 14 supported renderings
have now been read in full: 78 paragraphs, with source errors and qualifications
preserved. The review also covers credited authors, permission notices and
sampled citation bindings. It does not certify the source's scientific claims
or grammatical parses.

Two admission issues emerged from that reading. One article has a companion
comment that must be considered during source-group and overlap checks. Another
quotes two external papers whose available reuse notices do not establish
commercial permission for the quoted material; keep that whole section pending
under the explicit-license corpus policy. The review records those sources and
the limits of that conclusion. Exact, normalized and substantial shared-passage
overlap checks and final attribution remain before admitting any section.

## Work order

Both fits and the fixed public/private evaluations in the
[origin-consistency ablation](ORIGIN_CONSISTENCY_V1.md) have closed successfully
on the Studio. The [results](ORIGIN_CONSISTENCY_RESULTS_V1.md) retain both
candidates and their original calibration thresholds. The earlier interrupted
run remains documented in the [recovery record](ORIGIN_RECOVERY_V1.md); the
completed fits restarted from the original checkpoint with unchanged settings.
The [larger-backbone pair](CAPACITY_LARGE_V1.md) is now fitting. V6 remains the
frozen comparison model; no reference promotion or length-coverage improvement
has been established.

The [comparison protocol](ORIGIN_COMPARISON_V1.md) retains both candidates and
freezes both sets of Calibration thresholds before diagnostic scoring. Reuse
the seven original corpus views, all 96 hosted captures with their saved
Pangram annotations, and the 26 reviewed grammar states. The Rust harness
checks the fitting recipe and original inputs before running the existing
CPU evaluator and paired source-group comparisons. The
[private transfer comparison](ORIGIN_PRIVATE_TRANSFER_V1.md) now freezes the
three existing full-document B versions and their saved Pangram annotations
outside the repository and datasets. Replaying the saved V6 document reports
reproduced all three byte-for-byte without inference. Those saved reports contain
direct whole-text observations; their overlapping window views were unscored.
The [private comparison runner](PRIVATE_ORIGIN_RUNNER.md) completed both
candidates after the recorded public review. It scored every planned window
and kept the direct whole-text comparison separate. The improved result for
one known private source does not establish general transfer to Pangram.

The separate [longer-source protocol](LONG_DOCUMENT_DATA_V1.md) selects 100
complete CMU summaries of 650-1,000 lexical words, excluding all earlier source
works before historical review. Admission preserves the existing source and
quotation checks and leaves experiment splits unassigned. The tokenizer audit
reports every complete record, including over-limit texts. This collection
does not enter either running fit; matched model counterparts and a fresh
evaluation allocation must precede any later fitting or detection comparison.

The first long-source collection completed with [47 admitted summaries](results/long-sources-v1-admission.json)
from 100 reviewed: 41 lacked a full historical match, four had source notices,
and eight contained quotations requiring separate review. The [uncapped audit](results/long-sources-v1-tokens.json)
found 815-1,324 tokens, median 1,044, with no over-limit record. These sources
extend beyond V6's training lengths but still fall below the 1,878-token failure.
A [separate second range](LONG_DOCUMENT_DATA_V2.md), 1,200-1,600 lexical words,
addresses that gap without replacing the first cohort or changing its results.

The second collection completed with [30 admitted summaries](results/long-sources-v2-admission.json)
from its fixed 100-source review. Of the other 70, 53 lacked a full historical
match, five had source notices, and 12 contained quotations requiring separate
review. Its [token audit](results/long-sources-v2-tokens.json) spans 1,526-2,168
tokens, median 1,781. Two complete records exceed the current limit at 2,069
and 2,168 tokens; retain them as unsupported coverage rather than trimming or
replacing them. The two cohorts supply 77 unassigned historical human proxies,
75 within the token limit. Its length coverage improves the available source
pool, but supplies no model-quality result.

The [second manual pilot](results/long-sources-v2-manual-review.json) read three
complete summaries and all 206 sentence roots, with selected dependency checks.
All three contain clear parser errors, including finite verbs tagged as nouns
and subordinate clauses promoted above main assertions. The sources also
contain fragments, typos and ambiguous wording. Better detection on generated
counterparts could therefore reflect prose-quality differences as well as
origin signals; keep this limitation in the comparison.

The [fresh length/style comparison](LONG_STYLE_COMPARISON_V1.md) allocates six
sources to generation QA and the other 71 families to confirmation, including
two unsupported roots. Qwen and Phi-4 will each compose drafts under all three
profiles: source-matched, formal and editorial. The latter two prompt profiles
and Phi-4 are absent from the active Train archive, as checked before allocation.
They do not establish disjoint underlying styles. The pilot permits 36 local
calls; inspect its output fidelity, completeness and realized style differences
before launching confirmation. Neither source allocation enters the active fits.

Generation QA covers **all 36** fixed pilot outputs. All six model/profile
groups are complete and manually reviewed, including rejected raw responses.
Those reviews were frozen before detector scoring. Each review compares the
complete saved output with its complete source.

| Generator / profile | Reviewed outputs | Automatically admitted | Within A's 2,048-token limit | Strict fidelity passes |
| --- | ---: | ---: | ---: | ---: |
| Qwen / source-matched | 6 | 6 | 5 | 0 |
| Phi-4 / source-matched | 6 | 5 | 6 | 0 |
| Qwen / formal | 6 | 6 | 6 | 0 |
| Phi-4 / formal | 6 | 5 | 6 | 0 |
| Qwen / editorial | 6 | 6 | 4 | 0 |
| Phi-4 / editorial | 6 | 5 | 6 | 0 |

These are six shared source families, not 36 independent sources. The reports
preserve the [first two Qwen drafts](results/long-style-pilot-initial-review-v1.json),
[remaining four Qwen drafts](results/long-style-pilot-additional-review-v1.json),
[Phi-4 source-matched group](results/long-style-pilot-phi4-review-v1.json),
and [formal/editorial reviews](results/long-style-profile-pilot-review-v1.json).
All outputs have known model-only provenance. None passes this one assistant's
strict test of retaining every fact, qualification and argumentative relationship.

Length and realized style remain confounds. Qwen's oversized source-matched
response has 2,667 tokens against its source's 1,977. Five of its six formal
drafts stay within 4.5% of their source token counts, but factual changes and
interpretive additions remain. Phi-4 frequently shortens the material; its
rejected formal output has 835 tokens against 1,870. Two Qwen editorial outputs
exceed A's limit at 2,150 and 2,263 tokens, despite automatic admission. The
former has only 2,046 native generator completion tokens; that tokenizer does
not determine A's input length. Preserve both complete outputs as unsupported.

The editorial drafts retain many details but repeatedly add dramatic
interpretation and change participants, motives or causal sequence. In the
final Qwen response, a succession due later that year becomes seven years away,
and a character is declared dead where the source establishes only mutilation.
The first Phi-4 editorial response assigns a revolt to the wrong group and
changes organizing conspiracies into unraveling them. Sentence repairs and
clearer paragraphing coexist with these substantive errors. Length and readable
prose do not establish fidelity.

The reviews record source defects separately, including an incomplete
enumeration, a sentence fragment and unclear participant references. Keep these
limitations visible: detecting an embellished, shortened or less accurate draft
does not isolate recognition of model origin across styles. A requested profile
label does not establish a faithful or distinct realization of that style.

The [admission amendment](LONG_STYLE_ADMISSION_V2.md) removes the legacy
800-word ceiling only by explicit opt-in, retaining the relative-length and
source-copy checks. The first two exact saved responses were rechecked without
regeneration. Original errors, the oversized response and all raw responses
rejected for excessive shortening remain in the evidence. The third Phi-4
editorial response is another shortening rejection: 725 tokens against 1,870,
with much of the plot's causal explanation omitted. The fourth was admitted at
730 tokens against 1,188 but changes the timing of birth observations and turns
a requested bargain into fulfilled conditions. Keep every attempted
response and all six original human controls in later accounting, including
sources omitted from complete-family exports after a generated counterpart
failed admission. Phi-4 formal's successful-record count is five, although
all six raw responses completed.

The fixed generation run and final upstream model-file verification completed
successfully. Across the 36 attempts, 33 passed automatic generation admission
and 33 fit A's token limit; these are different subsets. None passed the strict
fidelity review. The final Phi-4 outputs change a library expedition's injured
participant and travel sequence, and confuse a magical duel with an emotional
appeal. These are substantive changes, not just editorial preferences.

Do not launch the 426-call confirmation under this recipe. Retain every pilot
output as a known-origin Development diagnostic, including its admission and
length failures. The untouched confirmation sources remain reserved. A revised
pilot must establish whether it can produce meaningful style contrasts while
preserving the source's claims and relationships before expanding that test.
Failure of this style-control design does not invalidate recorded model origin,
and does not move Development examples into training.

The separate [V6 pilot diagnostic](LONG_STYLE_V6_DIAGNOSTIC_V1.md) scores all
36 reviewed drafts and the six original human proxies, including admission
failures and expected unsupported inputs. It measures detection and coverage
on this Development cohort without reopening confirmation or changing the
active fits. Fidelity and length differences remain explicit confounds; these
texts have no Pangram observations.

The [completed diagnostic](results/long-style-v6-diagnostic-v1.json) returned
all 42 responses. Every supported generated draft exceeded both frozen V6
thresholds. All three oversized drafts returned the expected unsupported-length
reason, with no score or truncation. The admitted and supported draft subsets
remain distinct: all three shortening rejections were supported and detected.

| Origin / generator | Supported / submitted | Stricter flags / supported | Looser flags / supported | Model-or-assistance score range |
| --- | ---: | ---: | ---: | ---: |
| Historical human proxies | 6/6 | 0/6 | 1/6 | 0.08%-44.64% |
| Qwen drafts | 15/18 | 15/15 | 15/15 | 98.90%-99.88% |
| Phi-4 drafts | 18/18 | 18/18 | 18/18 | 97.32%-99.45% |

The looser human flag is on the 919-token source; the longest supported human
source, at 1,977 tokens, scores 0.08%. These six observations do not establish a
general relationship between length and false positives. An independent audit
rechecked all 158 input bindings, complete request/response coverage, exact text
identities, token counts, probabilities and reported aggregates. All 42
per-source score and coverage rows were inspected. Execution completed on Studio
using the bundled single-thread CPU runner. No additional provider calls occurred.

V6 handles these ordinary long rewrites at both thresholds, including drafts
from the unfitted Phi-4 family. This result does not resolve its failure on the
previously optimized B candidates, establish a pure style effect or demonstrate
agreement with Pangram. Keep the 426-call confirmation paused under the failed
fidelity recipe. The next detector decision remains the frozen comparison of
the control and consistency fits, including their separate private transfer
check; no model or threshold is promoted from this pilot.

The [manual pilot](results/long-sources-v1-manual-review.json) read the first
three complete admitted texts and inspected all 92 sentence roots plus selected
dependencies. Each parse contains clear errors, including parenthetical and
introductory predicates promoted above main clauses, a hyphen tagged as a verb,
and wrong agent attachments. Source punctuation and wording also have defects.
Retain the historical texts and the partial syntax findings separately; no
parse-accuracy estimate or detector-quality result follows from this review.

1. The hosted comparison above is complete. Retain its generator and profile
   failures as diagnostics, with unchanged thresholds and admission flags.
2. Extend the reviewed local-edit pilot above across the remaining frozen
   families and broader edits. Preserve source ancestry and inspect actual
   meaning, grammar and combinations before scoring. The first pilot does not
   justify treating punctuation augmentation alone as the repair for A.
   Existing Pangram scores bind only original texts; modified texts need their
   own observations before claiming external agreement.
3. Audit the existing commercially admitted training pool and length coverage.
   Add coherent longer human/model passages and more generator/style coverage
   where rights permit. Match topic and length strata across origins. Keep
   complete documents as evaluation units and treat window scores as diagnostics.
4. Freeze a new comparison with separate held-out styles, generators and edit
   families. Keep source ancestry together and retire previously opened Test
   material from claims of fresh validation. Choose fitting changes on
   Development only, then calibrate and run the reserved evaluation once.

For the first fitting ablation, retain the architecture and address the measured
style/generator and length gaps with admitted training material. Once reviewed
interventions reproduce a weakness, compare augmentation alone with a bounded
consistency objective on the same training-only pairs. Include human controls
so invariance does not simply increase all AI scores. Keep data and loss changes
separately identifiable. Generator-attribution or style-invariance heads are
later ablations if the diagnostics support them; this audit does not establish
that either will improve detection. Freeze training details and source
membership in a separate experiment declaration before fitting.

Assess source-group detection loss, calibration, human false positives,
generator/style/length slices, edit-induced score changes, and paired Pangram
agreement separately. Confidence on one familiar genre cannot compensate for
failure on an omitted evaluation cell. Report uncertainty over source groups,
not individual related revisions. No new model-strength milestone or whitepaper
claim follows from this audit.

Private book text, correspondence and B outputs remain evaluation-only.
The hosted corpus is admitted for A training and evaluation under the
[hosted admission manifest](../../docs/a-hosted-output-admission.json); its
output-publication permission is a separate matter. The remaining
Pangram experiment allowance after the full-document comparison is recorded in
the private ledger and does not cover a new submission, so this phase starts
with saved annotations and Studio inference.

## Reproduction

Run on the Mac Studio, with the archived V6 inputs:

```sh
cargo run --example a_coverage_audit -- \
  --train /path/to/v6/shards-v1/train.jsonl \
  --token-audit /path/to/v6/full-token-audit-v1.json \
  --evaluation results/encoder-revision-v6.json \
  --out /path/to/new-coverage-audit.json
```

`prepare_hosted_rescore` validates the complete original capture audit, joins
the exact prior comparison by text hash and provenance, and freezes requests
and V6's operating thresholds before inference. It does not admit captures for
training, execute a model or contact Pangram. Use the artifact's bundled CPU
runner for the emitted requests. Both tools passed formatting and all-targets
Clippy on macOS arm64; a Linux rerun is not claimed.

`prepare_a_edit_stress` freezes the Development family selection, parses the
texts on the Studio and records every nominated edit, combined state and
rejection. `review_a_edit_stress freeze` binds a completed manual review to the
exact state texts and patches before emitting CPU requests.
`review_a_edit_stress report` validates full response coverage and computes
paired changes at the existing thresholds. Both examples passed formatting,
the detector Cargo test suite and all-targets Clippy on the Studio. No NLP
implementation changed and no new Linux run is claimed.
