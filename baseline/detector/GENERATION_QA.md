# Qwen generation preflight

This review compared all eight completed outputs in the local `qwen-v1` smoke
run with their originals in `acquisition-v1/frozen-roots.jsonl`: six drafts and
two edits. Seven source roots were assigned to training and one to development;
no sealed test root was inspected. The audit made no model or detector calls,
changed no records or labels, and performed no fitting or evaluation. The entire
smoke run may be excluded if the generation runtime changes for throughput.

All eight responses reported `finish_reason: stop` and the expected
`slop-ninja-qwen` response identifier. None contained a title, markdown fence,
visible reasoning wrapper or obvious truncation. The drafts passed the declared
source-copy screen. These checks establish the recorded generation format and
history. Factual fidelity remains uncertified, and the observed outputs cannot
be described as semantically matched or faithful transformations.

## Source comparison

The first column abbreviates the hexadecimal suffix of each `generation:` ID.
Full record IDs and raw requests/responses remain in the ignored smoke directory.
Source extraction hashes and attribution appear in the
[acquisition manifest](manifests/acquisition-v1.jsonl).

| Generation | Operation and source | Observed differences |
| --- | --- | --- |
| `009023394637` | Draft; [DARC/HIV report, revision 2783204](https://en.wikinews.org/w/index.php?oldid=2783204) | Preserves the central association and reported quantities, but presents the proposed malaria-defense explanation as an established evolutionary adaptation. It also changes the population wording and describes the 11% estimate as new AIDS cases. Those changes go beyond the supplied text. |
| `00d11b3a55c0` | Draft; [Asian Cup report, revision 851797](https://en.wikinews.org/w/index.php?oldid=851797) | Extends Australia's tournament-debut description to both teams. It says Schwarzer's saves preserved parity while Australia were still losing 1-0, and drops several named participants and match details. |
| `02482fe83663` | Draft; [zebrafish microinjection abstract](https://doi.org/10.1371/journal.pone.0202377) | Retains the 93%, 42 micrometer, under-100-millisecond and approximately 80% quantities. It adds CRISPR/Cas9 ribonucleoproteins and equivalent genotypic outcomes; the source only states comparable injection efficiency. |
| `034c84a1f576` | Draft; [Safia Ahmed-jan report, revision 1985139](https://en.wikinews.org/w/index.php?oldid=1985139) | Retains the central events, but attributes confirmation of death at the scene to hospital officials. The source attributes that confirmation to the governor's spokesman. It also adds claims about condemnation and securing the attack site. |
| `046cd2903633` | Draft; [retinopathy-screening review](https://doi.org/10.1371/journal.pone.0198979) | Retains the study counts, percentages and odds ratios, but adds multivariate analysis and statistical significance that the supplied abstract does not establish. It also adds family experience of visual impairment. |
| `06b4f18ebcb1` | Edit; [word-reading study](https://doi.org/10.1371/journal.pone.0199084) | Rewrites almost every sentence, adds significance to the unconstrained-attention comparison, and strengthens the concluding interpretation. The source says N400 amplitudes were larger; it does not state that added significance claim. |
| `07afdb3587f7` | Edit; [urban-foraging study](https://doi.org/10.1371/journal.pone.0202450) | Changes a finding about tested rinsed greens into a causal assertion that rinsing reduces metal uptake to safe levels. It also strengthens a favorable nutrient comparison to matching or exceeding the comparator. |
| `09117af03e2d` | Draft; [bovine-microbiome study](https://doi.org/10.1371/journal.pone.0200974) | Preserves the main sampling times and microbial-diversity pattern. Its closing description of community dynamics as a determinant of reproductive outcome is stronger than the source's reported association. |

The clearest failure is that the generation prompt asks for unchanged facts and
qualifications, while admission currently checks length, wrappers, successful
completion, nonidentity and, for drafts, long copied source spans. None of those
checks establishes semantic preservation. The observations above demonstrate
failures of that prompt instruction; they do not invalidate the record of which
model produced each response.

## Editing strength and formatting

The two edits retain approximately 50% and 53% of source words in an exact,
ordered longest-common-subsequence comparison. Their longest unchanged word
runs are 13 and 11 words. This measurement uses lowercased Unicode word tokens;
it measures surface overlap rather than meaning. Both outputs substantially
rewrite the source prose, so this sample provides limited evidence about subtle
assistance or light human/model editing workflows.

Every output keeps or reduces the source's paragraph count. Across the eight
pairs, the count falls from 38 to 22, including one scientific draft that merges
five paragraphs into one. All eight outputs are shorter than their sources
under the same token count. Paragraph structure and compression are potential
classification shortcuts for this generator and prompt. Future format controls
and independent generators should measure their contribution before broader
detector claims.

The source texts remain historical human proxies, and the edits retain the
`model_edit_of_historical_proxy` evidence category. The drafts are recorded
source-conditioned model generations; shorter copied phrases and complete
originality remain unverified. No detector labels, source partitions or sample
selection rules were changed because of this review. The preflight supplies no
evidence of B success or reference-detector accuracy.
