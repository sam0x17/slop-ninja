# Detector evaluation

The target is a readable revision that preserves argumentation, detail, and tone,
with less than 10% of the full document classified as AI or AI-assisted. The
default gate uses `fraction_ai + fraction_ai_assisted < 0.10`, and also reports
AI-only scores separately. Exactly 10% fails. Never round before applying the gate.

Pangram reports character-weighted class fractions, not the probability that the
whole document was written by AI. Its segment assistance score is a different
quantity. Headline labels are also unsuitable for an exact threshold test.
Returned segments are merged output spans, not independent experimental samples.
See the [Pangram 4 model card](https://www.pangram.com/research/model-card/pangram-4).

## Stored observations

Every request keeps the original submitted text, its SHA-256 over exact UTF-8
bytes, explicit selector, returned version, timestamp, task ID, and full decoded
response. Do not trim text, normalize line endings, or render LaTeX differently
between nominally identical repeats. Preserve returned `text` separately:
Pangram can normalize the input, and its offsets refer to its returned text.
See the [current API reference](https://docs.pangram.com/api-reference/ai-detection).

The adapter submits asynchronously:

```rust
use unslop::{evaluation::PangramClient, util::write_json};
use std::path::Path;

fn inspect_existing_task(task_id: &str) -> anyhow::Result<()> {
    let client = PangramClient::new()?; // PANGRAM_API_KEY
    let result = client.poll(task_id, 600.0)?;
    write_json(Path::new("data/task-result.json"), &result)?;
    Ok(())
}
```

Use the Rust CLI's `study` or `score` command for submission. They persist an
intent before the paid request and a task ID before polling. Re-running an
uncertain slot cannot silently issue another request. Rust UTF-8 file reads
preserve CRLF bytes.

Call `client.models()` to inspect the account's current selectors. Availability
can differ between accounts; `default` can move to a different model. Use an
explicit selector and keep the returned version, as described in the
[model catalog documentation](https://docs.pangram.com/api-reference/models).

POST requests are never automatically retried. If a POST response is lost, a
task might have been created and charged even though no ID was received. A GET
timeout leaves the saved ID available for resumption. The Rust adapter returns
`PangramFailure` with task ID and response metadata; the workflows persist it.
No public dashboard link is requested.

## Provider IDs in shareable results

Raw per-request records keep the provider task ID, because resuming or polling
needs it; keep them in ignored storage under `data/`. Summaries and reports that
may be shared list provider job, bulk and task IDs as `redacted-<16 hex>`: the
first eight bytes of SHA-256(salt || ID). This covers `summarize_runs` and
`paired_study` (`task_ids`), the `study`, `score`, `score-corpus`, `runs` and
`reward` commands, the `edit_screen` report and the transformer's
`menu external-report` (`bulk_id`). The salt is read at run time from
`data/provider-ids/salt`, or from the file named by
`SLOP_NINJA_PROVIDER_ID_SALT`. Commands that can submit paid requests read it
before the first request. Each writer appends the raw IDs it digested, with the
digest and the result path (or the command, for standard output), to the
private map `data/provider-ids/id-map.private.tsv` (mode 600), or to
`SLOP_NINJA_PROVIDER_ID_MAP`. Readers accept both forms: a digest passes through
unchanged, so a raw ID and its digest name the same task.

## Repeated full-document comparisons

`summarize_runs(records, threshold=.1, min_repeats=3, ids)` groups by detector,
selector, returned version, and exact submitted input hash. It shows all scores,
means, medians, and maxima. An identical task response copied twice is one
observation. Conflicting responses for one task ID, invalid fractions, failed
tasks, and mismatched stored hashes cause errors rather than accidental passes.
Different documents or models stay in separate groups.

`paired_study(baseline_runs, candidate_runs, quality_audit, ..., ids)` accepts ordered
baseline/candidate repeat pairs. Each record must explicitly declare
`full_document: true`. Each arm must repeat one exact input, with distinct task
IDs, and each pair must use the same detector, selector, and returned version.
The caller supplies temporal pairing through list order; the function cannot
infer whether the experiment was randomized or interleaved correctly.

For a new edit, randomize or alternate baseline/candidate order in short blocks.
Keep every result, including failures and regressions. Use isolated sentences
to investigate mechanisms, then test the complete document. Predeclare the
candidate and final repeat count before confirmation scans; choosing the best
of many scans is selection bias. Start with three repeat pairs, and increase
the predeclared count if pilot observations show meaningful variation.

The quality audit must contain literal `true` for all five fields:

```json
{
  "reviewed_by_human": true,
  "readability": true,
  "argumentation": true,
  "detail": true,
  "tone": true
}
```

These are human attestations, not automatic fidelity metrics. Set them after
review; never generate a passing audit from detector scores. Record the reviewer,
date, and claim-by-claim notes alongside them. A missing, false, or nonboolean
flag cannot pass. The paired study passes only when the audit passes, both arms
have enough repeats, and every candidate observation passes the strict threshold.

## What a passing document establishes

Three successful repeats support a statement about that input in that test.
They cannot establish reliability on new documents, explain Pangram's internal
decision boundary, or prove human authorship. All summaries therefore set
`reliability_established: false`. Score reductions from one preface are useful
pilot observations, not independent samples of a general rewriting process.

Freeze a process before testing it on untouched source groups. Evaluate multiple
topics, registers, lengths, generator versions, and detectors. Keep revisions of
the same source, paraphrases of the same prompt, and near duplicates in one
split. Report the joint fidelity-and-detector success rate across source groups,
uncertainty from source-group resampling, worst-domain performance, query cost,
and readability review results. Repeat this evaluation after detector updates.

For a potential Bittensor validator, evaluate on private rotating source groups,
with strict content audits before any detector reward. Repeatedly querying a
public test set measures adaptation to those examples. It does not measure
generalization. A subnet reward and its anti-replay mechanisms should be designed
after the local process demonstrates a useful held-out success rate.
