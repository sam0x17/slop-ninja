# grammar-a-dryrun

The counting kernel of the §4.3 dry run's uncounted figures and child-side
stop invariants (protocol A2 item 14.1; BUILD-PLAN 12.3, "B23"; ruling
W7-R1). It is pure: it reads nothing from disk and makes no call. The
batch readers are `grammar-a-positives`'s `dryrun-count` (local, spliced
and rows batches) and the frontier example's `dryrun-count` (frontier
plans); `grammar-a-positives dryrun-merge` merges their records.

| Module | Holds |
|---|---|
| `engine` | The landed `grammar-a-form` and amendment 7's `grammar-a-form-a7` behind one interface (`Code`): the record's conventions read into each code's type and checked to round-trip, `child_v2`, `item_v2`, `donor_child_v2`, `splice`, the window, the frontier and rows verdict wiring (`simple_verdict`), the laid-out child and the landed transfer with its diagnostics. Every call is caught: amendment 7's code panics on some lines, and a panic keeps only its code location. |
| `figures` | Count 2 (greeting and sign-off merges) and count 3 (list markers left mid-line) on one child under one code. |
| `invariants` | Counts 4-6, the stop invariants, on one child's landed transfer. |
| `tally` | One child at a time: the re-derivation identity, then the six counts into cells by source × class × domain, reasons by source × class, ids-only lines; totals, the gate and the merge. |

The definitions and readings this build fixes where the text leaves them
open (BUILD-PLAN 12.3 open items (a)-(g)) are in each module's notes and in
every record's `rules` (`grammar_a_dryrun::rules`). The stop invariants
gate on their broadest reading, so no reading narrows item 14.1:
- count 4: accepted children (the text's word), a line over its budget on
  the replacement (with the source's `column` and `tail_column`) or in the
  final document;
- count 5: every child, the accepted ones beside;
- count 6: every child, the paragraph's own words, with the final
  document's (the parent words a span's first and last paragraphs join, as
  `place_breaks` counts them) beside.

`tests/counts.rs` pins each stop invariant's counted case and its near
misses, counts 1-3 on a synthetic email whose verdict differs between the
codes, amendment 7's caught panic, the re-derivation identity, the ids-only
lines and the merge, on synthetic text only.
