# Controlled edit screen v1

This round tests whether small lexical edits, individual sentence changes, and
complete editorial revisions reduce Pangram's combined AI + assisted fraction
while preserving the input's claims and intended scientific register.

## Frozen scope before scoring

Three source families are selected from the existing training split:
`doi:10.1371/journal.pone.0186133`, `doi:10.1371/journal.pone.0179877`, and
`doi:10.1371/journal.pone.0188280`. Their earlier model rewrites were flagged;
this is deliberately a search on known difficult training examples. It is not
an estimate of performance on randomly selected or unseen documents.

For each Codex source, screen two independent single-word edits and two
independent one-sentence revisions. Each candidate starts from the same baseline;
edits are not accumulated. For each Claude Opus source, screen one complete
revision written without consulting its human original or its detector output.
These arms use different model texts and cannot establish whether full-document
editing is generally better than local editing.

Freeze exact source and candidate bytes and their SHA-256 hashes before candidate
requests. Use the full abstract as context and the explicit `pangram-4` selector.
Record returned versions, task IDs, full responses, and all unsuccessful results.
Run one fresh baseline and one observation per candidate for screening, at most
21 initial requests. Process word candidates before sentence candidates within
each source. Model profiles motivate hypotheses, not universal replacement rules.

## Confirmation and review

Inspect every candidate against its source before scoring, and compare complete
revisions with the original human abstract for inherited distortions. Preserve
numbers, attribution, scope, uncertainty, and all listed methods and outcomes.
Record these as assistant reviews with `reviewed_by_human: false`. A detector score
cannot generate a passing human audit.

Select up to two promising candidates for three fresh alternating baseline /
candidate pairs each, at most 12 additional requests. Selection uses the combined
fraction, preservation review, and evidence of an actual label change rather than
merely reducing the size of an unchanged flagged passage. Retain screening scores
separately from confirmation. Do not adapt the selected bytes during confirmation.
If none improves, record that result and predeclare any further experiment as a
new round. The initial screen and confirmation together permit at most 33 calls.

Report all candidate results, not just winners. Distinguish one successful scan,
repeated detector success, assistant preservation review, and a human-certified
quality result. Any finding in these three reused training families remains
exploratory. Development and test sources are untouched during this round.
