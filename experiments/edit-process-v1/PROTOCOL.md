# Fixed editing process v1: development evaluation

The training screen produced one complete revision repeatedly scoring 0% combined
AI + assisted text and a replicated effect from one reduced relative clause.
This next experiment evaluates a fixed editing instruction on previously unused
development source families. It does not continue tuning those training examples.

The process is one Codex CLI invocation per input, requested model `gpt-6-astra`
with the collector's low reasoning effort and isolated, tool-free configuration.
The exact instruction file is frozen before development text is opened for
editing. It encodes clarity and preservation procedures derived from the training
review, without a list of detector-favoured synonyms or a target length.

Select every `dev` document in both complete model corpora: two source families
and two model baselines, giving four inputs but only two independent families.
Keep the test split untouched. The writer receives only the immediate model text,
never the human reference, previous detector results, or source metadata containing
the earlier human-to-model prompt. Record exact hashes and generator provenance.

Generate exactly one candidate per input (four model invocations), with no
post-generation manual edits or detector feedback. An independent assistant
reviews each candidate against its immediate model source before seeing scores.
Review failures remain in the denominator. All human-review flags stay false.
This process cannot remove unsupported claims inherited from the supplied source.

For every input, predeclare three fresh alternating baseline/candidate pairs on
`pangram-4`: six requests per input, 24 total. Keep all results, including unchanged
or worsened scores and quality failures. Do not select only favourable baselines,
exclude initially low scores, or stop after the first passing repeat. An unchanged
candidate is a valid editing control: record it explicitly and use separate
single-input repeats if the paired harness rejects identical arms.

Report baseline and candidate score ranges, repeated detector pass counts,
assistant preservation findings, and newly improved versus already passing inputs.
The joint human-audited success gate remains unmet without human review. Two
families cannot establish general reliability, cross-domain performance, or
subnet economics. Changing the prompt after this experiment requires a new version
and a new evaluation; these development sources are no longer untouched afterward.
