# Varied scientific introductions for later A training

Use the 13 complete introductions admitted under
[the section policy](PLOS_SECTION_ADMISSION_V1.md). Freeze all 13 as Train for
this new cohort before generation. Their text, attribution, historical-proxy
evidence and related-work aliases remain unchanged. This allocation does not
modify the active encoder fits, their selection data or any existing benchmark.
All descendants of each root inherit its source family and Train assignment.

The purpose is to expose A to longer scientific prose under several model and
style combinations. The cohort is one additional register, with no Pangram
labels or claim that its outputs are hard for A before measurement. Preserve
every attempt instead of selecting outputs using detector scores.

## Fixed generation matrix

Use the already pinned MLX conversions of Qwen3-30B-A3B-Instruct-2507 and
Mistral-Small-3.2-24B-Instruct-2506. Preserve their Apache-2.0 evidence, exact
checkpoint revisions and runtime-file bindings. Record the actual Studio
runtime and Mistral tokenizer adapter. No hosted model, B output or private
manuscript enters this cohort.

Each model receives every root separately under four existing `style-mix-v1`
profiles: `source-matched`, `plain`, `informal` and `fix-slop`. This supplies
104 possible independent drafts across 13 roots. Exact profile instructions
and their meaning/tone guard remain unchanged. Profiles describe requested
styles; they do not establish that the output achieved them.

Use the initial draft operation, temperature 0.7, a 4,096-output-token cap and
one request at a time. The collection-specific prompt requests a complete
scientific introduction, preserving background, competing explanations,
qualifications, open questions, citation labels and stated purpose. It must
not request an abstract or imply that introductory commentary should replace
the section. Existing abstract and other collection prompts keep their bytes.

Keep source-relative output admission: at least 80 lexical words and between
half and twice the source length. Preserve completion, wrapper and source-copy
checks. Count complete outputs with A's pinned tokenizer without truncation;
the generator's limit does not establish A support. Keep incomplete, copied,
oversized and otherwise rejected attempts with their reasons. Do not redraw.

## Pilot and continuation

Rank roots by the SHA256 of compact JSON
`["slop-ninja-scientific-style-pilot-v1", source_group]`, breaking ties by group.
Use the first three for a 24-call generation pilot: three roots, two models and
four profiles. Reserve the other ten for the remaining 80 calls. Both subsets
remain Train; the pilot is generation QA, not a Development evaluation set.
Freeze the complete allocation and model/profile matrix before the first call.

Inspect complete pilot outputs alongside their sources for citation handling,
factual and argumentative fidelity, intended tone, realized register, length,
truncation and copying. Keep origin evidence separate from editing quality.
A fully recorded model draft can supply an origin-training example even if it
contains a fidelity error; it cannot support a style-only comparison with
meaning held fixed. Preserve that judgment as a separate annotation.

Before continuing to the remaining roots, require a functioning complete-output
path for every model/profile cell and inspect failures. A prompt or runtime
change needs a new frozen run, with existing attempts preserved. This protocol
does not require every pilot draft to pass strict fidelity before origin-data
collection can continue. It does require reporting the fidelity failures and
rejecting any claim that detector differences isolate style when they do not.

Keep each original human control once in later assembly, including roots whose
drafts failed. Preserve all model-written ancestors in any later cross-model
revision study. That study needs its own declared recipe; this first matrix
does not perform revisions or light edits of human prose.

## Execution and use

Run on Studio with dedicated loopback workers and isolated caches. Do not
restart LM Studio or shared services. Keep at most one generation worker active;
the existing encoder comparison and blog audit continue unchanged. Record
Linux runtime qualification separately from the tested macOS environment.

No Pangram calls or other paid annotations are part of this collection. Do not
use provider scores as origin labels. A later fit must declare cohort weights,
source-family splits and evaluation before learning from these records. Retain
the existing 71 reserved long-style confirmation roots and the original fits'
comparisons. These training examples do not themselves establish improved A
accuracy, Pangram agreement or useful gradients for B.

The [pilot review](results/scientific-style-pilot-review-v1.json) records both
completed source-matched cells. All three Qwen drafts and one Mistral draft
passed automatic origin admission; two Mistral drafts were excluded for copied
source passages. All six complete responses fit A's context limit and were read
against their sources. Each retains detail or qualification errors. The four
admitted drafts remain candidate origin-training examples with those judgments
attached; the excluded attempts remain available for audit without redraws.
The Qwen plain-profile cell is also complete and reviewed. Two drafts passed
origin admission and one was excluded for source copying. All three fit A's
context limit, but all have fidelity errors and remain largely academic in
register. One omits every numbered citation; another misstates the number of
studies supporting a particular regional finding. Requested profile names are
not verified labels for realized style.

The Mistral plain-profile cell is now also complete and reviewed. One draft
passed automatic origin admission; two were excluded for source copying.
All three complete outputs fit A's context limit. The third preserves the study
design and direct participant ratings, but drops details and strengthens some
claims. All twelve responses across the four reviewed cells have strict fidelity
failures; seven passed origin admission and five were excluded for source reuse.

The Qwen informal-profile cell is also complete and reviewed. All three drafts
passed automatic origin admission and fit A's context limit. None realizes the
requested conversational register: all remain academic and add claims or lose
qualifications. The third invents specific parameter examples and omits a
planting-date application and calibration review. Fifteen outputs now have
complete source-relative reviews; ten passed origin admission and five were
excluded for source reuse. All fifteen have strict fidelity failures.

The Mistral informal cell is complete and reviewed. Its admitted draft is more
accessible in places but compresses away study details. The other two drafts
use noticeably conversational language; both were excluded for source copying,
omit every numbered citation, and lose qualifications or change details. All
three fit A's context limit and fail strict fidelity. Eighteen outputs across
six cells are now reviewed: eleven admitted and seven excluded for source reuse.

The Qwen Fix Slop cell is complete and reviewed. Two drafts passed origin
admission and one was excluded for source copying; all three fit A's limit and
fail strict fidelity. The wellbeing draft omits every numbered citation and
adds neural and methodological claims. The maize draft loses a planting-date
application and calibration details. The human-evolution draft adds technical
restrictions to population, admixture and model claims. The outputs retain
academic prose and several constructions targeted by the requested profile.

The Mistral Fix Slop cell closed with one client timeout and two captured
responses, both excluded for source copying. Both complete responses were read.
The human-evolution draft turns a probabilistic fixation argument into a
guarantee, loses population-size citations and adds a verb-form error. The maize
draft loses application scope and calibration details and strengthens a
tentative claim. The timeout has no captured text, fidelity judgment or detector
result. The client used a 900-second request timeout; the server log records
a broken pipe after the client disconnected. This does not establish why
generation exceeded the deadline.

The generation campaign exited with failure in its final profile. Manual review
is complete for all 23 captured responses: 13 passed origin admission, ten were
excluded for source copying, and none passed strict fidelity. The twenty-fourth
request is the separately recorded timeout. Keep the failed campaign record and
all raw attempts. Review completion does not make that execution successful.

The remaining ten roots have not been launched. The pilot must establish useful
realized variation among usable examples before expanding this style-diversity
recipe. The collection prompt's research-journal register may constrain the
requested informal style; this is a hypothesis for a later recipe, not a change
to the frozen calls. No detector scores were used to select these outputs, and
no new fit has consumed them. The fixed
[V6 diagnostic](SCIENTIFIC_PILOT_V6_RESULTS.md) has evaluated all 23 captures and
the three human controls, preserving the timeout without a score. All Qwen
drafts pass both thresholds. Mistral scores vary with admission status and
source reuse; all three admitted Mistral drafts pass the looser threshold, but
only one passes the stricter threshold. The results leave the existing admission
decisions intact and provide no Pangram comparison.

A separate [copy audit](SCIENTIFIC_STYLE_COPY_AUDIT_V1.md) measures the extent of
reuse in the frozen first eleven reviewed outputs, including rejected drafts. Some exclusions
involve one 21-word match; others cover much larger portions of the output.
The experiment preserves the existing admission decisions and supplies no
detector-performance result.
