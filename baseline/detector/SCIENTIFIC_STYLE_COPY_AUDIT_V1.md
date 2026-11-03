# Source reuse in the scientific pilot

The [eleven-response audit](results/scientific-style-copy-audit-v1.json) found
substantially different amounts of reuse among four drafts rejected by the same
source-copy rule. Two share one run of 21 normalized words, covering 2.22% and
3.74% of their output words. The other two have 25.71% and 36.54% of output words
covered by shared runs of at least 20 words. Their longest runs are 38 and 61
words. The seven admitted drafts have longest shared runs of 8 to 14 words.

These are three Train source families under two models and two requested
profiles. One model/profile cell is incomplete in this snapshot. All eleven
complete outputs have separate source-relative fidelity reviews; none passes
strict preservation of every source detail and qualification. The audit cannot
establish that rejected drafts are more faithful, that they would be harder for
A, or that the filter causes A's generalization failures.

The measurements use the same word normalization as the existing generation
guard. Digits and punctuation disappear from the token sequence, and runs can
cross citation or sentence boundaries. The percentages measure normalized-word
coverage by runs of at least 20 words, not total reuse or the fraction of text
authored by a human. A zero value follows from successful admission under that
rule and does not establish originality. Proper names and technical formulations
can contribute to a match without proving inappropriate copying.

The Rust diagnostic accepts a separately prepared, hash-bound JSONL snapshot of
admitted and rejected captures. Each row retains exact source and draft text,
their hashes, the Train family, generator/profile, raw request/response hashes
and original admission result. Preparation rechecked the eleven rows against
their pinned generation plans, sources, model specifications, profiles, saved
responses and tokenizer/provenance audits. The diagnostic checks text hashes,
replays the existing source-copy guard and measures reuse without constructing
new origin records. It rejects oversized pairs rather than truncating them.

```sh
# Run only on Studio, using a new output path.
cargo run --release --example audit_draft_source_copy -- \
  --input /path/to/pairs.jsonl --input-sha256 <snapshot-sha256> \
  --output /path/to/new-audit.json
```

Manual inspection covered the longest matches and the existing complete-output
reviews. An independent Ruby implementation reproduced all eleven sets of word
counts, longest runs, source/draft ranges and coverage values. The required Rust
checks passed on macOS arm64; Linux execution remains unverified.

Retain the frozen pilot's admission decisions. For later data work, preserve
continuous reuse measurements alongside the rejection reason. A separate
training experiment can test whether excluding minimally overlapping drafts
harms generalization, using unchanged evaluation families and explicit origin
evidence. It must also distinguish those drafts from outputs that retain large
source passages. No detector score, new generation, label change or training
admission is part of this audit.
