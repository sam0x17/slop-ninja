# Style-diverse detector results

Measured 2026-09-11. The new candidate improved pooled log loss in all four test
views and detected more AI-or-assisted text in the original primary view. It
also failed to choose `mixed` as the most likely class on any test view, and
three-class accuracy fell on both original-text cohorts. Keep this as an
experimental comparison artifact; the first reference remains available.

The model bundle (omitted from the public copy)
contains the new encoder and all eight aggregate comparisons. Neither this
experiment nor the first release establishes Pangram parity or subnet
qualification. No Pangram calls were made.

## Sources, generation and fitting

The [frozen protocol](STYLE_PILOT_PROTOCOL.md)
prescribed fresh sources, two generators, varied requested styles and matched
formatting exposure. These changes were tested together; the experiment cannot
attribute an outcome to style instructions, generator diversity or formatting
alone. The first encoder and its original calibration remained fixed.

We acquired 80 additional PLOS abstracts and 80 historical Wikinews excerpts
under the recorded CC BY 4.0 and CC BY 2.5 notices. Source-family, URL and
exact/normalized-text checks found no reuse from the earlier source or adopted
generation cohorts. These are still convenience samples from two registers.
Shared authors, events and other near-duplicates remain possible, as does overlap
with encoder pretraining. Historical publication supplies weak human-origin
evidence; generated copyedits supply weak mixed-origin labels. No private
correspondence, book text or Blog Authorship data entered this lineage.

Qwen3-30B-A3B-Instruct-2507 and Mistral-Small-3.2-24B-Instruct-2506 each received
80 frozen source families. The primary cohort used `source-matched`, `plain`,
`informal`, `direct`, `anti-ai` and `fix-slop` instructions. The secondary cohort
used reserved `editorial` and `formal` instructions on the frozen Test sources.
Both teachers occur in training, so this round does not test an unseen generator.
Human originals remain unchanged, and draft/edit siblings share their requested
profile. Assignment was balanced within buckets before admission; exclusions
changed the retained counts.

| Generation cohort | Attempts | Accepted responses | Copying rejections | Retained families |
| --- | ---: | ---: | ---: | ---: |
| Qwen primary | 160 | 158 | 2 | 78 |
| Mistral primary | 160 | 149 | 11 | 69 |
| Qwen reserved | 20 | 20 | 0 | 10 |
| Mistral reserved | 18 | 17 | 1 | 8 |

All 358 requests were attempted. Every rejection affected a draft; the existing
rule also excluded its human source and accepted copyedit from that cohort.
There were no replacements or score-based retries. Retained primary data contain
147 examples of each origin class, with 74 PLOS and 73 Wikinews source families.
Copying exclusions define the retained task distribution; the reports do not
describe every generated response.

| Partition | Source families | Texts |
| --- | ---: | ---: |
| Training | 97 | 291 |
| Development | 19 | 57 |
| Calibration | 14 | 42 |
| Primary Test | 17 | 51 |
| Reserved-profile Test | 18 | 54 |

The two Test cohorts share 17 source families. Their human controls and
whitespace variants are paired observations. Both Mistral `fix-slop` primary
Test families failed the copying check, so that teacher/profile combination has
no retained primary Test evidence. The Qwen `fix-slop` slice has two families.
The [generation manifest](manifests/style-pilot-v2-generation.json)
records all assignments, exclusions, profile counts and runtime hashes.

Prompts requested factual and tonal fidelity; admission did not certify it.
Two training outputs were read for compliance. One retained stock phrasing;
another omitted source details and added an unsupported qualification. This
was not a random fidelity audit. No prompt, label, admission or split changes
followed. These examples establish recorded model use, not successful
meaning-preserving transformation.

The new ModernBERT fit used the pinned base checkpoint, three epochs, seed 17,
batch size four, learning rate 0.00002 and weight decay 0.01. Each training row
appeared once per epoch, alternating raw and collapsed-whitespace views by family
hash parity. All siblings shared the choice. Collapse changed 204 of 291 training
texts; alternate-view exposure counts, including unchanged views, were 132, 159
and 132 across the three epochs. Development and calibration used original text.

| Epoch | Development log loss | Development correct | Selected |
| --- | ---: | ---: | --- |
| 1 | 0.957116 | 32/57 | No |
| 2 | 0.891927 | 33/57 | Yes |
| 3 | 0.894062 | 34/57 | No |

The prespecified uncalibrated development log loss selected epoch two. Epoch
three had better accuracy and Brier score but slightly worse log loss; those
other metrics did not override the selection rule. Model choice and temperature
calibration finished before test inference. No later fit or retuning occurred.

## Comparisons

"Prior" denotes the first released encoder with its original calibration;
"new" denotes the epoch-two candidate. Accuracy uses three-class argmax.
Detection and false-positive counts use each model's fixed threshold on
`P(model_only) + P(mixed)`. These are different decision rules, so an argmax
origin error need not cross the binary detection threshold.

| Test view | Encoder | Correct | Log loss | Brier | AI-or-assisted detections | Human-proxy false positives |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Primary original | Prior | 28/51 | 0.9681 | 0.5508 | 19/34 | 1/17 |
| Primary original | New | 26/51 | 0.8556 | 0.5367 | 25/34 | 1/17 |
| Primary whitespace | Prior | 24/51 | 1.0427 | 0.6319 | 24/34 | 1/17 |
| Primary whitespace | New | 24/51 | 0.9028 | 0.5679 | 27/34 | 1/17 |
| Reserved original | Prior | 32/54 | 0.8496 | 0.5282 | 23/36 | 1/18 |
| Reserved original | New | 29/54 | 0.8360 | 0.5257 | 27/36 | 1/18 |
| Reserved whitespace | Prior | 23/54 | 1.0139 | 0.6555 | 30/36 | 1/18 |
| Reserved whitespace | New | 27/54 | 0.8994 | 0.5699 | 28/36 | 1/18 |

The primary log-loss improvement was 0.1124. Brier score, on the [0,2] scale,
also improved in all four pooled views. Three-class accuracy declined in both
original views, tied on primary whitespace and improved on reserved whitespace.
The new candidate's mixed-class recall was zero in every view: it assigned some
probability to `mixed` but never ranked it first. This is a failed class under
the selected recipe, despite better pooled probability scores.

The original-text improvement was concentrated in the Mistral slices. On primary
Mistral, log loss fell from 1.3277 to 0.9150 and detections rose from 3/14 to 9/14.
On primary Qwen, log loss worsened from 0.7163 to 0.8141 while detections stayed
at 16/20. Reserved original Mistral also improved, while reserved original Qwen
worsened on log loss and detections. Each teacher slice includes its unchanged
human-source controls. Their small denominators and changed training allocation
prevent a broad generator-ranking claim.

Whitespace sensitivity remains. The new primary loss increased by 0.0472 after
collapse, compared with 0.0746 for the prior model. Its reserved loss increased
by 0.0634, compared with 0.1644 previously. Smaller changes do not establish
formatting invariance. The new candidate also detected fewer AI-or-assisted
texts than the prior encoder on reserved whitespace, 28/36 versus 30/36.

The prior threshold is strictly greater than 0.9375192210349048; the new threshold
is strictly greater than 0.8935751794569573. Each model uses the same threshold
across its four views. Both nominal 1% and 5% calibration targets select the same
threshold within each model, with zero observed calibration false positives.
They are configured targets, not achieved population guarantees. The new
calibration set has only 14 human proxies, and the tests have 17 or 18.

Primary sensitivity was 55.9% for the prior model and 73.5% for the new one.
Their 95% source-family bootstrap intervals were 32.4%-79.4% and 52.9%-91.2%,
respectively. These are individual-model intervals, not a confidence interval
for their paired difference or evidence of population superiority. The
[aggregate reports](results/style-pilot-v2.json) retain all confusion matrices,
calibration diagnostics, operating counts, register/evidence slices and
descriptive teacher/profile slices. No improvement is claimed from a slice
without reporting its denominator.

## Reproduction and next work

Generation used the exact native MLX checkpoints and runtime from the first
pilot on the M5 Max. It took 51m32s. Training and calibration on the M3 Ultra took
696 seconds. All four token audits passed; the maximum input was 727 tokens,
below the fixed 2,048-token cap. No input was truncated or dropped for length.

An initial launch check used the Studio's system Python 3.9 and failed before
training. Repeating that check with the existing pinned Python 3.12 environment
succeeded. The data and recipe did not change, and there was one training run.
The worker verified frozen inputs, artifacts and calibration exports throughout
evaluation. The local result transfer verified 3,578 frozen file bindings; all
eight report hashes matched. Bundled synthetic reference checks and a separate
synthetic CPU inference request passed after transfer to the M5. These are
runtime checks, not further accuracy observations.

The new artifact is
`52f0054fc5b3c3f22d04a34fb1dcd9855e7296a0ff3ad943fc270741ccc3224e`,
with source pinned to `8b36e1fa98db7a9de6a91e99f2f41b1de7f046b4`.
The [selection record](results/style-pilot-v2-selection.json) and
[encoder manifest](results/style-pilot-v2-encoder-manifest.json) retain the
pre-test bindings. The public bundle includes weights, the inference runtime,
licenses, source credits and aggregate results. It omits corpus text, raw
responses and per-record predictions, supporting reproducible inference rather
than independent reconstruction of every training response.

The probabilities describe document-origin classes in this weakly labeled
mixture, not AI-written word fractions. Performance on genuine collaboration,
personal writing, author identity, other languages and unseen generators remains
unmeasured. Low detection scores do not establish quality or meaning preservation.

Before another candidate, investigate the mixed-class failure using training and
development data and freeze any revised selection rule before fresh confirmation.
Broader registers and stronger production evidence are still needed. The
[linear-control diagnosis](LINEAR_FIT_DIAGNOSTIC.md)
also identified overconfident fitting and motivates a controlled smaller-step
grammar comparison that holds lexical coverage fixed. The tests opened here are
now diagnostic material; future confirmation needs new sources.
