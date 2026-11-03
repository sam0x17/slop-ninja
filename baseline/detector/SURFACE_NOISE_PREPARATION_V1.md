# Surface-error panel prepared; scoring awaits review

The [Rust preparer](examples/surface_noise_panel.rs) produced 386 candidate
edits under the [fixed protocol](SURFACE_NOISE_PROBE_V1.md), with 126 shared
baselines. Each frozen detector will therefore receive 512 inputs. The source
panel remains the 16 previously exposed Train families, including all 126
human, model-only and mixed records.

| Intervention | Eligible records | Source families | Review status |
| --- | ---: | ---: | --- |
| Second internal space | 126 | 16 | Pending |
| Common-word transposition | 126 | 16 | Pending |
| Repeated determiner | 126 | 16 | Pending |
| Repeated terminal mark | 8 | 2 | All eight complete source passages read |

The first three interventions have identical record coverage across origins.
The terminal-mark subset contains one human proxy, six model-only records and
one mixed record. Its 118 ineligible cases remain recorded. Seven eligible
cases descend from one novel summary; the eighth is a conversational technical
explanation. All eight end in a question mark, so this subset will supply no
exclamation-mark result.

An [independent check](results/surface-noise-panel-check-v1.json) reproduced every
eligible span, exact candidate text, source/candidate hash, exclusion and
collection/origin count. The original texts remain separate from model views.
Formatting, all-target tests, Clippy with warnings denied and release builds
passed on the Studio. Linux execution remains unverified.

The [terminal review](results/surface-noise-terminal-review-v1.json) found that
every selected edit repeats a prose-final question mark. No word changes, but
the extra mark may increase emphasis or insistence. The damaged final question
in the human novel summary remains unresolved. Retain these as origin-sensitivity
probes without claiming tonal equivalence or better writing. The other 378 edit
contexts still require manual inspection; no detector inference has started.

The training objective is accurate origin estimates that withstand superficial
noise, including errors added deliberately. Insensitivity alone is insufficient:
retain human false-positive rates and absolute class losses alongside paired
changes. The same corruption policy should apply across origins in a later
controlled fit, with explicit consistency penalties and independent evaluation.
The running capacity comparison remains unchanged.
