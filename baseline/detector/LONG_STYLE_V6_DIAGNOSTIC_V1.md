# V6 diagnostic on the completed long-style pilot

Score the six historical human source summaries and all 36 recorded drafts
from the completed long-style generation pilot. All six model/profile cells
have complete manual source/output reviews. Keep the three admission failures
and three oversized drafts; those are different subsets. No new generation,
provider calls, training admission or source allocation is part of this study.
The 71 reserved confirmation families remain untouched.

This evaluates recorded origin and input coverage. The drafts failed strict
fidelity review, so detection differences cannot be attributed solely to style.
The human sources are historical proxies, not verified human-only workflows.
There are six independent source families, with six related drafts per family.
Profile and generator slices share those sources. This is a Development
diagnostic and cannot supply fresh confirmation or population error estimates.

Freeze the original pilot roots, four complete manual-review reports, six
generation-run manifests, captured requests/responses/profile records, admission
outcomes, this protocol and the V6 artifact manifest before scoring. Verify
every draft against its reviewed text hash, every source against its recorded
hash, and the complete two-model by three-profile by six-source matrix. Retain
the generation model revisions and existing requested-style/fidelity judgments.
Do not filter using admission, length, quality, expected score or detector output.

Use frozen V6 artifact
`4e061df4789e546dfdd58c8005860a98fb8f200a4c7642b7491461f44f16f138`
with its bundled CPU runner, calibration and 2,048-token limit. Send complete
texts, including expected over-limit inputs. Preserve all unsupported responses;
do not truncate, summarize or window them. Verify successful token counts against
the reviewed uncapped counts and unsupported-length reasons against those same
counts. Any unexpected coverage or artifact mismatch stops reporting and retains
the evidence for inspection.

Apply the existing strict greater-than thresholds, 0.8436493155285434 and
0.4287369114260753. Report supported/unsupported counts, human-proxy flags,
model-draft detections and probability ranges by generator/profile. Keep exact
per-source observations and distinguish detection among supported inputs from
coverage of the complete cohort. No threshold tuning, model selection or
reference promotion follows this diagnostic.

Run on the Studio with the bundled float32 eager CPU implementation and its
single-thread default. Keep the active base control/consistency fits, their frozen comparisons,
larger-model launch order and reference artifact unchanged. Results can inform
the next declared data recipe; they do not establish Pangram alignment or useful
gradients for B because these texts have no provider annotations.
