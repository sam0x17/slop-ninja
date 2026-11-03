# grammar-a-dryrun

The counting kernel of the §4.3 dry run's uncounted figures and child-side
stop invariants (protocol A2 item 14.1; BUILD-PLAN 12.3, "B23"; ruling
W7-R1). It is pure: it reads nothing from disk and makes no call. The
batch readers are `grammar-a-positives`'s `dryrun-count` (local, spliced
and rows batches) and the frontier example's `dryrun-count` (frontier
plans); `grammar-a-positives dryrun-merge` merges their records.

| Module | Holds |
|---|---|
| `engine` | The landed `grammar-a-form` and amendment 7's `grammar-a-form-a7` behind one interface (`Code`): the record's conventions read into each code's type and checked to round-trip, `child_v2`, `item_v2`, `donor_child_v2`, `splice`, the window, the frontier and rows verdict wiring (`simple_verdict`), the laid-out child and the landed transfer with its diagnostics, an item's edit laid out in its item (`Source::InItem`, `edit_in_item_v2`; protocol A2 amendment 29 item 29.1). Every call is caught: amendment 7's code panics on some lines, and a panic keeps only its code location. |
| `figures` | Count 2 (greeting and sign-off merges) and count 3 (list markers left mid-line) on one child under one code. |
| `invariants` | Counts 4-6, the stop invariants, on one child's landed transfer: count 4's two readings (the document reading's one-word exemption read on the whole document line, protocol A2 amendment 29 item 29.2), count 5 with `closed_split`, and item 28.9's two count-6 readings, now reported beside the gate. |
| `count6` | Count 6's gate (amendment 31 item 31.1): item 13.1's constructed final paragraph holding each placed-break body unit, read through `grammar_a_form::profile::constructed_paragraphs` on the item assembled from every edit with a re-derived laid-out child and on the edit alone in its parent; the re-measured reading and the record reader for item 31.3's fail-safe. |
| `tally` | One child at a time: the re-derivation identity, then the six counts into cells by source × class × domain, reasons by source × class, ids-only lines; totals, the gate and the merge. |

The definitions and readings this build fixes where the text leaves them
open (BUILD-PLAN 12.3 open items (a)-(g)) are in each module's notes and in
every record's `rules` (`grammar_a_dryrun::rules`). The stop invariants
gate on their broadest reading, so no reading narrows item 14.1:
- count 4: accepted children (the text's word), a line over its budget on
  the replacement (with the source's `column` and `tail_column`) or in the
  final document, where a document line is exempt only when it holds one
  word (amendment 29 item 29.2); distinct document lines beside;
- count 5: every child, the accepted ones beside; `closed_split` beside it,
  flagged when it is not 0 (amendment 30 item 30.3);
- count 6: every child, item 13.1's constructed final paragraph (the item's
  or the edit's alone in its parent, either reading), plus item 31.3's
  fail-safe for re-measured short paragraphs whose record shows no
  re-measured difference; the own-words, `pre_words`/`post_words`,
  re-measured and fixed-boundary readings beside (amendment 31).

`dryrun-merge` checks each cell's children and accepted children against
the figures records: index 60's `DRY-RUN-FIGURES.json`, each re-run batch's
figures with its children equal to index 60's for that batch (item 29.3),
and a not-counted batch's index-60 figures, which are set aside (ruling
W9-R3: the count of the other batches is complete except that batch).

The crate root also holds the split readers: `line_split` (every value of
a key, from a line's bytes) and `request_split`, a request line's own
top-level `split` with `/voice/derived_from/split` beside it as provenance
(amendment 32 item 32.1; `TRAIN_SIDE_DERIVED` is the guard's set). A derived
split that is not one plain value (repeated, escaped or not a string) is
`DerivedUnreadable`, and a caller refuses a Train line that holds one.

`tests/counts.rs` pins each stop invariant's counted case and its near
misses, item 32.2's request-split set, counts 1-3 on a synthetic email whose verdict differs between the
codes, amendment 7's caught panic, the re-derivation identity, the ids-only
lines and the merge, on synthetic text only.
