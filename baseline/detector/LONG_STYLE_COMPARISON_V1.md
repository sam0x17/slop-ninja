# Fresh comparison of length and requested style

Use the 47 and 30 complete historical human summaries admitted by the two
long-source protocols. Their source hashes are fixed in those admission
reports. Preserve all 77 roots, including the two that exceed A's 2,048-token
limit. These are historical proxies from one genre, with multiple Wikipedia
contributors; the named book authors are not the summary authors.

Reserve the first three roots in each cohort's admission order for a six-source
generation pilot. The first cohort's three roots have already received a manual
syntax review. Assign these six roots `development` solely for generation QA;
they must not enter either active fit, checkpoint selection or calibration.
Reserve all remaining 71 source families as `test` for this comparison. All
descendants inherit their root's allocation. Keep complete texts and report the
two unsupported roots separately, leaving 69 supported confirmation roots.
No source can move from confirmation into the pilot after outputs are seen.

For every source, request an independently composed draft from Qwen3-30B-A3B
Instruct-2507 and Phi-4, using their already pinned MLX conversions and local
runtime. For each generator, request all three existing profiles separately:
`source-matched`, `formal`, and `editorial`. Exact instructions remain those of
`style-mix-v1`. Verify that `formal` and `editorial` do not occur in the active
Train archive and that Phi-4 remains absent from fitting. They are unused prompt
profiles, not proven disjoint regions of writing style. The paired source and
generator comparisons let us measure realized changes rather than infer them
from the profile names.

Use the current source-conditioned draft prompt, temperature 0.7, one request
at a time and a 4,096-generator-token output cap. Record the actual checkpoint,
runtime, prompts, raw responses and finish reasons. Generate drafts only;
auxiliary edits of human prose are outside this comparison. Preserve the
existing source-copy checks and failure records. Never truncate an output to
fit A, redraw a failed generation, or silently replace a source. Count missing,
copied, incomplete and over-limit outputs before calculating detection rates.

The first execution is limited to the six pilot roots: two generators by three
profiles, at most 36 generation calls. Freeze model specifications, runtime and
source bindings before these calls. Inspect the complete outputs, source
fidelity, length and actual syntax/style differences before launching the
remaining 426 confirmation calls. Retain the pilot even if the generation
design proves unsuitable. A changed prompt or runtime needs an explicit amended
protocol and new cache identity before confirmation, not retrospective edits.

Keep all human controls when reporting false positives, regardless of whether
their generated siblings passed admission. For paired model comparisons, show
both the full attempted denominators and the intersection with complete,
supported outputs. Repeat neither human counts across profiles nor related
revisions as independent evidence. Report source-group uncertainty, and keep
the two original length cohorts separate as well as pooled.

The fixed detector candidates are V6 and both members of the origin-consistency
ablation. Use their original calibration and operating thresholds. No fresh
comparison result may change either fit or choose between its epochs. Detection
scores stay closed during generation QA. This is limited fresh evidence about
one genre, two generators and three requested profiles; it cannot establish
general style independence or a low population false-positive rate. No Pangram
calls or parity claims are part of this phase. Private B failures and broader
fresh genres remain separate requirements before reference promotion.
