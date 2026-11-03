# V6 results on the register pilot

Frozen V6 flagged all four admitted drafts at its strict threshold. The Qwen
drafts scored 99.21-99.77%; the admitted Mistral draft scored 93.95%. This small
pilot did not supply the near-zero examples needed to reproduce A's failures
on private B outputs. The reference model has not changed.

All six captures and their three complete sources were read before these scores
were opened. Every draft failed strict fidelity through omissions or added
interpretations. Four passed automatic origin admission; two Mistral drafts
were excluded for source copying. All nine inputs fit the 2,048-token limit
without truncation. Scores are V6's model-or-assistance probabilities.

| Generator | Source | Earlier informal draft | New register draft | New origin admission | New words in shared runs of at least 20 |
| --- | --- | ---: | ---: | --- | ---: |
| Qwen | Human evolution | 98.90% | 99.33% | Passed | 0.00% |
| Qwen | Wellbeing | 98.74% | 99.77% | Passed | 0.00% |
| Qwen | Maize calibration | 98.08% | 99.21% | Passed | 0.00% |
| Mistral | Human evolution | 93.34% | 94.61% | Copy exclusion | 3.86% |
| Mistral | Wellbeing | 51.13% | 93.95% | Passed | 0.00% |
| Mistral | Maize calibration | 60.20% | 56.38% | Copy exclusion | 34.21% |

The strict threshold is greater than 84.3649%; the looser threshold is greater
than 42.8737%. Five of the six captured drafts exceeded the strict threshold;
all six exceeded the looser threshold. The original human proxies scored 0.08%,
0.11% and 3.78%, matching their earlier complete responses exactly. None was
flagged. The maize copy exclusion had a longest shared run of 61 words; the
evolution exclusion had a longest shared run of 21 words. Zero coverage by
20-word runs does not mean no shorter phrases were reused.

Changing the register instruction and drawing another sample produced uneven
changes in wording and content. These three shared Train families, one new
draft per source/model pair, fidelity failures and copied spans prevent a
style-only causal conclusion. The lower maize score may reflect several factors;
this comparison does not isolate copying as its cause. No Pangram calls were
made, so it provides no new evidence of Pangram alignment or useful gradients
for B.

The [complete review](results/scientific-profile-register-review-v1.json),
[nine-input report](results/scientific-profile-register-v6-diagnostic-v1.json)
and [paired comparison and independent audit](results/scientific-profile-register-comparison-v1.json)
retain all captures, source hashes, admission decisions and measured scores.
The Rust preparation/reporting extension passed Studio formatting, all-target
tests and Clippy with warnings denied. It reproduced the previous study's
requests, plan and report byte for byte. Independent checks verified source
copying, token counts, response coverage and threshold arithmetic, and confirmed
that wrong-mode and incomplete-response reports are rejected.

Keep the remaining 80 scientific generation calls paused. More samples of this
recipe are not yet justified as a way to obtain the missing near-zero failures.
The active control/consistency fit and its frozen transfer comparisons remain
the next model comparison; any capacity follow-up must wait for both base fits
and their evaluations. No model, threshold, training split or admission policy
is promoted by this result.
