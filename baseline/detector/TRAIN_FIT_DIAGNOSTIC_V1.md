# Frozen Train-fit diagnostic

The saved base fits keep reducing their online training loss while Development
log loss worsens after the selected epoch. Those training losses use dropout,
sample weights, paired views and, for the treatment, a consistency penalty.
They are not a direct measure of the saved detector's training-set errors.

Score the complete original Train archive with frozen V6, base control and base
consistency. Use all 3,833 records from 471 source families, separately in the
original raw and collapsed-whitespace views already used in training. Retain
both views for all three models. Do not select inputs by score or remove mixed
examples, failed observations or difficult human sources. Confirm identical
IDs, origins, ancestry and groups across formatting views before inference.

Use each artifact's bundled CPU reference inference, one process at a time on
the Studio. Bind the artifacts, their original calibration and the thresholds
already recorded in the completed public comparison. Preserve every request,
response and failure. Inputs to the detector contain only IDs and exact text.
No private source, new generation or Pangram call is involved. Live larger fits
keep their original recipes; this diagnostic cannot alter their selection.

Report three-class confusion and loss, the binary human/model objective with
mixed-origin rows excluded, and each class's contribution. Alongside ordinary
row-weighted metrics, average within each source-family/origin cell and then
equally across cells. This prevents families with more generated variants from
dominating the diagnostic. Record human loss and original-cutoff false positives
by the human root's collection. Keep original calibrated scores and reconstruct
temperature-one scores from saved positive probabilities; fit nothing new.

The artifact is the already selected epoch, not the final optimizer state or
all four epochs. Inference disables training dropout and has no consistency
penalty. Raw and whitespace results therefore describe saved-model fit on
observed training texts; they are not a reproduction of the online loss.
Good Train fit plus poor unseen-source results would support a generalization
problem. Poor Train fit would leave optimization, contradictory supervision,
objective mismatch and capacity as competing explanations. Neither result
alone decides which intervention will work.

After all six runs complete, inspect errors by class and source collection.
Read selected Train errors before proposing a remedy. Preserve the earlier Test
diagnostics and historical-proxy label limits. No recipe change, new temperature,
threshold fitting, reference promotion or fresh accuracy claim is authorized by
this diagnostic's result alone. Any later training experiment needs its own
declared recipe and new-source confirmation.
