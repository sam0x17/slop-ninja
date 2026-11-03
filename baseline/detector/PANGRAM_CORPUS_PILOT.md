# Pangram corpus rehearsal

Declared before the first bulk submission, 2026-09-11. Bulk experimentation
runs against a cumulative private budget ledger, which reserves headroom before
any paid request. An uncertain submission keeps its reservation and cannot be
automatically repeated.

Submit one pass over all 57 original Development documents from style pilot v2:
19 source families with a historical human proxy, a recorded model draft and a
recorded model edit of that proxy in each family. Preserve all text, existing
splits, source labels and evidence qualifications. This is already-opened
development material, not a new confirmation sample.

Use the prepared Pangram 4 bulk request with SHA256
`cd192f9652c9bcf630350a05f6844da7000cc9e536331d16eb7ef18581b89401`.
The exact source-records file has SHA256
`a5967f8f564d0f25fabb82ba3f9c90777ee094c781bfb9a32f712766b2e94bbf`.
Pin selector `pangram-4`, retain the returned version, initial receipt, every
status/result response and all failures. Require exact coverage, task identity
and echoed-text equality before importing annotations. Never publish raw corpus
text or provider responses through this rehearsal.

Report Pangram's observed content fractions and native label distribution.
For the prespecified binary diagnostic, mark an input as flagged when
`fraction_ai + fraction_ai_assisted >= 0.10`. Report flagged counts separately
for the three source-origin labels, teacher families and prompt profiles, with
missing observations visible. These weak source labels qualify all apparent
errors. Content fractions are not calibrated document-origin probabilities, so
do not calculate a Pangram origin log loss or Brier score from them.

Compare descriptively with the already selected word/grammar candidate
`combined-0.001` and v2 encoder
`52f0054fc5b3c3f22d04a34fb1dcd9855e7296a0ff3ad943fc270741ccc3224e`.
Use each local artifact's frozen calibration operating points, without refitting
or selecting models using Pangram. Different operating points and 19 human
families do not establish matched-FPR superiority or a quality-anchor switch.
Three-class argmax confusion may be reported for our own models separately.

Preserve the annotations for reuse across later comparisons on these exact
texts. Do not use these Development annotations as new training labels. Any
repeats or further source batches need their own predeclared sample and must
charge the same cumulative ledger. Reported costs are estimates.

## Transport amendment after first receipt

The first result collection stopped before import because Pangram changed
whitespace in its returned text. An inspection limited to identity, field names
and text equality found 20 exact echoes and 37 whitespace-only echoes. Every
returned text has the same whitespace-delimited token sequence as its submitted
input. No fractions or predicted labels were inspected for this amendment.

Permit whitespace-only echoes through an explicit collection flag, retaining
both exact hashes and `echo_match` on every observation. All other text changes
still fail validation. Report exact-echo and whitespace-only slices separately
alongside the complete cohort; do not silently call all 57 echoes exact. This
does not establish which representation Pangram used internally. The transmitted
texts, sample, model selectors, labels and diagnostic threshold remain fixed.

## Full-cohort repeat extension

After completing the first pass, schedule two further passes over all 57 exact
inputs, using repeat indices one and two. No input selection depends on its
score. The first pass flagged every model-only/mixed proxy and no human proxy;
the extension measures repeatability and does not create independent accuracy
evidence. Submit the 114 observations as one additional bulk job. The
posted-price estimate, alone and with the first pass, is recorded in the
private ledger.
Reserve headroom against the same cumulative budget. No additional rows or
model fits enter this extension.

Compare document fractions, native labels, echoed text and complete result
objects across the three observations per source. Report all ranges and threshold
crossings, preserving raw observations and failures. Same-account, same-day
repeatability cannot establish cross-account or future-version determinism.
