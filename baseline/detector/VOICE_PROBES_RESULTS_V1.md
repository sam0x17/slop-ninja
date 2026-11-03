# Voice changes on three additional Train passages

The news passage's large V6 drop did not recur in the three follow-up probes.
Each passive recast lowered its parent's probability by less than 0.35 points;
all candidates stayed above 97%. Both raw class contrasts also changed much
less than in the news example. The current evidence does not establish a broad
passive-voice weakness.

| Collection | Parent A | Passive candidate A | Change, points | Human control A | Edit review |
| --- | ---: | ---: | ---: | ---: | --- |
| Book summary | 99.2424% | 99.1676% | -0.0748 | 0.3031% | Accepted |
| Encyclopedia | 99.1805% | 99.1471% | -0.0333 | 1.7850% | Accepted |
| Scientific abstract | 97.4174% | 97.0733% | -0.3441 | 0.1553% | Harder to read |

The [protocol](VOICE_PROBES_V1.md) used the other three parents already selected
for the structural pilot. Each received one manually specified exact edit. No
proposal search or retries followed. The book summary and abstract were Qwen
drafts; the encyclopedia explanation was an OLMo draft. All three parents and
their human roots were verified against the unchanged Train archive and recorded
commercial-training/model-release permissions. No new text entered training.

All three source pairs and complete candidates were read before new scores.
Each recast preserves the immediate parent's meaning and register. The scientific
candidate moves a long subject before the predicate, making it harder to process;
that concern was recorded before scoring. Its result remains separate from the
two accepted edits. All three parents' older factual deviations from their human
roots remain documented and are not approved by this edit review.

The model-human logit changes were -0.2269, -0.1283 and -0.3535 respectively.
The mixed-human changes were -0.0361, +0.0278 and -0.1274. Thus the small
probability movements are accompanied by small class-logit movements; probability
saturation alone does not conceal a news-sized class-contrast change here.
The earlier news passive changed those contrasts by -2.8731 and -2.9681.
Different passages and recasts remain distinct interventions.

Frozen V6 scored only the three new candidates, using its bundled CPU runner
on Studio. Six saved parent/human responses were reused with exact text and
artifact bindings. Every model state exceeds both existing cutoffs and no human
control does. Candidate token counts are 320, 229 and 344, each two tokens longer
than its parent; nothing was truncated. These familiar Train controls do not
provide a held-out false-positive estimate. No Pangram call was made.

The [numeric report](results/voice-probes-v6-v1.json) includes all nine inputs,
probabilities, class contrasts and threshold decisions. The
[manual review](results/voice-probes-review-v1.json) records the pre-score
judgments. Exact reconstruction, Train membership, response coverage, runtime
and calibration bindings were checked. Rust formatting, all-target tests and
Clippy passed. The updated preparer reproduced the earlier captured-component
plan and requests byte for byte and rejected attempts to mislabel a manual probe
as a captured candidate.
The [completion record](results/voice-probes-completion-v1.json) binds the source,
binary, checks and final reports. Full texts remain in the Studio experiment archive.

Keep the news example as a local detector failure to investigate. These three
probes do not justify bulk passive augmentation as an established remedy. The
paired origin-consistency comparison remains the next test of a changed A model;
the new probes are outside its frozen diagnostics. No reference promotion or
Pangram-transfer improvement follows from this experiment.
