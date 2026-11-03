# V6 on the scientific generation pilot

V6 flagged every Qwen draft at both frozen thresholds. It flagged two of eleven
Mistral drafts at the stricter threshold and six at the looser threshold. All
three historical human sources scored below 4%. These observations come from
three shared source families under four requested profiles. They do not supply
a population error rate or isolate a model, style, length or copying effect.

The [complete report](results/scientific-pilot-v6-diagnostic-v1.json) preserves
all 26 exact-input responses and the separate generation timeout. Every supplied
text was supported without truncation. The prior complete-source/output reviews
were frozen before scoring, including the ten source-copy exclusions. V6 and
the active fits have not trained on this new cohort.

| Input group | Texts | Stricter flags | Looser flags | Model-or-assistance score range |
| --- | ---: | ---: | ---: | ---: |
| Historical human proxies | 3 | 0 | 0 | 0.08%-3.78% |
| Qwen, admitted drafts | 10 | 10 | 10 | 94.18%-99.29% |
| Qwen, source-copy exclusions | 2 | 2 | 2 | 95.57%-96.48% |
| Mistral, admitted drafts | 3 | 1 | 3 | 51.13%-86.76% |
| Mistral, source-copy exclusions | 8 | 1 | 3 | 0.33%-93.34% |

The stricter and looser thresholds are 0.8436493155285434 and
0.4287369114260753, both applied with strict greater-than comparisons. All
thirteen admitted drafts remain above 0.5. Mistral's admitted source-matched
and informal wellbeing drafts score 59.04% and 51.13%; its admitted plain
human-evolution draft scores 86.76%. The low Mistral detection count across all
captures therefore combines moderate-confidence admitted drafts with several
low-scoring source-copy exclusions. Those groups need separate accounting in
the next training comparison.

The [post-score reuse comparison](results/scientific-pilot-v6-copy-context-v1.json)
joins every row of the earlier eleven-response copy audit to its exact new
detector observation. Two excluded Mistral maize drafts have 25.71% and 36.54%
of normalized output words covered by shared runs of at least twenty words;
they score 7.62% and 0.45%. Two other exclusions have only one shared 21-word
run: coverage of 3.74% for a Mistral draft and 2.22% for a Qwen draft, with
scores of 83.52% and 96.48%. These measurements describe one earlier subset,
not every low-scoring capture. They do not measure the fraction of human
authorship, and the two extensive-reuse examples share both source and model.

Independent bookkeeping rechecked all 132 bound input files, 26 expected file
absences, exact request and response identities, token counts, calibrated
probabilities and aggregate flags. All 26 per-source result rows were read.
The shared Rust reporter also reproduced the earlier long-pilot report byte
for byte. Execution used the bundled V6 CPU runner on Studio and made no
provider calls. The timed-out generation has no captured text or detector score.

This result identifies a generator and source-reuse distinction to preserve in
later data work. It does not demonstrate a capacity limit or validate gradients
for B. There are no Pangram observations for these drafts, so even the near-zero
scores do not establish the same transfer failure as the private B examples.
Keep the existing admission decisions and the remaining 80 calls paused. A
revised generation recipe must establish usable style variation and reliable
capture; any later fit must declare its data weights and comparisons before
consuming these records. The running control/consistency experiment and the
larger-model launch order remain unchanged.
