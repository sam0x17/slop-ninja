# First B prompt comparison

Declared before generation on 2026-09-13. This is an engineering baseline, not
an estimate of performance on human authors. Two synthetic sources are crossed
with two synthetic author profiles and two prompting variants: eight requests,
one candidate per cell, with no redraw or best-of selection. Both conditions
explicitly seek writer fit and detector resistance while preserving all meaning
and satisfying the target tonal intent. V6 is fixed before this experiment.

Each profile has two independent reference works and one reserved diagnostic
sample on another topic. The reserved sample never enters a prompt, a fitted
target or the choice of conditioning. None of this material comes from the
detector's Train, Development, Calibration or Test corpus. As model-authored
fixtures, the style samples are not human controls for A.

Use the already pinned Qwen MLX checkpoint, with an 8,192-token context,
temperature 0.7, seed 17, a 900-token output cap and one request at a time.
Record the exact conversion revision, file-manifest hash, runtime instance and
decoding settings. A requested seed is not a proof of repeatability. Retain
complete request/response bytes and the server's finish status. Refuse truncated
or empty responses as candidates; retain their failed cells in the denominator.

The measured condition adds raw sentence/clause/dependency-load means and a
fixed list of function-word rates from the reference works. The control gets
the same examples and brief without that table. It does not shorten the source
unless the requested voice calls for shorter sentences; brevity is not a
quality objective on its own. The target tone is declared before generation.

Use the existing versioned Rust grammar/word extractor and geometry. Fit target
means and scales from references only. Candidate vocabulary may extend the
categorical basis with zero target mass; reproject source and candidate into
that same extended space, without refitting. Report the combined word/grammar
distance and a grammar-only diagnostic. The combined distance remains sensitive
to topic; neither distance is an authorship probability or a semantic test.
Record the reserved sample's distance without using it to calibrate acceptance.

Before opening A results, inspect each candidate against the full source and
claim list. Record meaning, target tone, readability, cleanup and author fit
separately, with explanations for failed or uncertain judgments. The initial
review is by the coding assistant and is not independent or human validation.
Eight-word copying from references is a review signal, not a proof of plagiarism
or an automatic preservation verdict. No amount of style or detector gain can
turn a failed editorial judgment into a pass.

Score every completed candidate and each unchanged source with the immutable
V6 CPU runner. Preserve all three origin probabilities and combined
`P(model_only) + P(mixed)`. Report paired changes and the previously frozen
strict cutoff, 0.8436493155285434, without adjusting it for these outputs.
The source is allowed to score below that cutoff. Its workflow label remains
model-only, irrespective of detector results.

Do not claim subnet success from this pilot. No qualified three-A panel,
independent editorial quorum, attested execution or Pangram confirmation is
present. The cumulative Pangram reservation is recorded in the private ledger;
this first comparison makes no provider calls. Any later confirmation
must use the existing ledger and reserve its cost before submission.

Report every cell, failure, editorial judgment, distance and A result. Use the
observed failures to specify the next experiment. Do not train on these eight
examples and then present them as held-out improvement.
