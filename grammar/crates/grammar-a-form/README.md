# grammar-a-form

Pure form rules for SlopNinja A positives. This crate is the one source of
the form path shared by the CC cohort tools (`grammar-cc-human`), the local
and spliced positive generators, the frontier collector and the
coordinator's Wasm port. It holds no I/O and no generation context: callers
supply the parent, the span and the cleaned response, and keep the records.

## Modules

| Module | Holds |
|---|---|
| `conventions` | Quote, dash, numeric-range and ellipsis sites; a parent's counts and strict majorities (`own`); the batch prior; `resolve` with the other-quote fallback and keyed draws (`KEY_SCOPE`); rendering of sites and quotes |
| `layout` | The excerpt layout `cc_excerpt_layout_v1`; line squashing and emphasis removal; `Structure` (mirror or replaced span); `place_breaks`; `normalise_child`; paragraph ranges |
| `screen` | The FineWeb line classifier (`list_like`, `boilerplate`, `classify`), the paragraph form screen, and `classify_all` with the licence screen's id `cc_form_licence_screen_v1` |
| `echo` | The echo screen `cc_echo_screen_v1` |
| `window` | The final-document length window `cc_final_length_window_v1` |
| `measure` | Layout measurement `layout_measure_v2`: content lines with their leading and trailing whitespace, gaps and their exact separator strings, sentence ends, column widths, first-pass hard-wrap evidence and its width W, unwrapped lines, lead changes, second-pass wraps, paragraph boundaries, paragraphs and their form-screen reasons, edge frames, paired emphasis, layout classes, text-free `LayoutCounts` and `MeasureDiag` (for profile v2, the layout audit and form-injection twins) |
| `transfer` | The layout transfer `layout_transfer_v6` of profile v2: `SourceLayout` (a mirror's or donor's whole text, or a span's paragraphs and lines; properties only, no recorded text: separators, frames, whitespace pools by role, wrap flags, licences, and the unrecorded `LineContext` of a span's or insertion's first and last document lines) and `transfer` |
| `profile` | The profile ids (`raw`, `cc_era_parent_norm_v1`, `cc_cohort_profile_v1`, `cc_cohort_profile_v2`), `Profile`, the every-class steps at assembly, profile v2's population x domain prior (`PriorV2`, `resolve_v2`), the closed population list `R2_POPULATIONS` and its per-child steps (`child_v2`, `item_v2`, `edit_in_item_v2`, `donor_child_v2`, with an item's layout in parent order, `lay_out_item`) |
| `text`, `rank` | The prose screen, whitespace words and normalisation, `h64`, the sentence splitter `a_comp_sentences_v1`, and the seeded rank |

## Rules

- **Conventions and layout** are those of `cc_era_parent_norm_v1`: the
  parent's majority double quote, single quote, dash, range and ellipsis,
  the other-quote-class fallback, then a keyed draw from the batch prior;
  ASCII quotes become curly by context. Children are squashed, stripped of
  paired emphasis, rendered, and re-paragraphed at the parent's (or the
  replaced span's) break fractions, every paragraph at least eight words.
- **Form screen.** Generated lines must end a sentence and not be list-like;
  each final paragraph holding generated text must pass `classify`.
- **Echo screen** (protocol A2 §7.2 with amendment 4 item 4.2). A child is
  rejected when it echoes its source: a mixed child whose generated span
  equals the parent span it replaces, or a mirror equal to its parent.
  Every comparison runs two checks and echoes when either matches:
  - (a) `raw`: the raw response span (the generated text as the raw twin
    splices it, before normalisation) against the raw source span;
  - (b) `rendered`: both spans after `echo::render`, the child normaliser's
    line steps applied alike to each: lines squashed, paired emphasis
    stripped, quotes, dashes, ranges and ellipses rendered in the item's
    `Conventions`.

  Check (b) catches a copy that normalisation would change (a
  minority-convention glyph, paired emphasis) and a response that differs
  from its span only in those; check (a) catches a verbatim copy whatever
  rendering does to it. Matching means byte-identical, or identical after
  whitespace folding (equal sequences of Unicode whitespace-separated
  tokens, the same equivalence as `text::normalized`). Case and
  punctuation still count. A span that is not a character-boundary range of
  the parent fails both checks closed (`echo_span_outside_parent`).
  - **Item verdict.** Callers record `echo::screen_item(parent, conv,
    child)` only, with the raw child. A mirror is one comparison. A mixed
    child is screened edit by edit, and the item fails when any edit echoes;
    a mixed child with no edit fails closed (`echo_mixed_no_edits`). An
    insertion (`s == e`) is compared with the empty span it replaces, so it
    echoes only when it adds no token, raw or rendered; it has its own
    target, so span echo counts exclude it. The record holds the edit count
    and each comparison's target and both checks' outcomes. The verdict is
    the item's, so both twins fail.
  - Reasons, check (a): `echo_span_byte_identical`,
    `echo_span_whitespace_folded_identical`, `echo_insertion_byte_identical`,
    `echo_insertion_whitespace_folded_identical`, `echo_mirror_byte_identical`,
    `echo_mirror_whitespace_folded_identical`; check (b): the same with
    `_rendered` after the target (`echo_span_rendered_byte_identical` and so
    on); both: `echo_span_outside_parent`, `echo_mixed_no_edits`.
- **Window.** 150..=450 whitespace words of the final document, inside
  acceptance and at assembly; reasons `final_document_below_150_words` and
  `final_document_above_450_words`.
- **Profile v2** (`cc_cohort_profile_v2`, protocol A2 §6.2 as amended by
  amendments 7, 10, 13, 29, 30, 38 and 42). Four entry points: `child_v2` (a mirror, or a mixed
  child with one replaced span, spliced by `generation::assemble`'s rules),
  `item_v2` (a spliced item: any number of edits, insertions with their
  separators included, laid out in parent order and assembled by
  `assemble_item`, the spliced generator's `splice::assemble_parts` rules),
  `edit_in_item_v2` (one edit of such an item judged before its siblings:
  `item_v2` over that edit, laid out in its item with the siblings' raw
  texts known so far) and `donor_child_v2` (a parentless row laid
  out like a layout donor's `SourceLayout::whole`). Records name the
  components: `layout_measure_v2`, `layout_transfer_v6` and
  `cc_form_licence_screen_v1`. Each runs:
  - **the measurement** (`measure`, `layout_measure_v2`). Lines split at
    `'\n'`, each content line's leading and trailing whitespace, each gap's
    separator bytes, the head and tail. Widths are columns: the characters
    of a line's leading whitespace and body (a tab is one column). A line
    ends a sentence when the sealed splitter's last sentence of it is
    punctuated. The first pass: a run (lines joined by single breaks inside
    a sentence) is wrap evidence when every one of its breaks is one a
    greedy wrapper could have made (the line, a space and the next word are
    wider than the block's fit bound: its widest line of several words
    among the lines of runs holding a break), its first line is not
    `list_like` and no two consecutive lines of it are, its last line ends a
    sentence and its block holds at least 8 words. A run whose first break
    fails the fit and whose first line starts its block, is short and not
    `list_like` (a greeting or title) is tested again from its second line,
    each break also against the whole text's bound. The text is
    hard-wrapped iff some run is evidence, and W is the widest line of
    several words among the evidence runs' lines. In a hard-wrapped text a
    line is unwrapped when it lies in no evidence run, holds several words
    and is wider than W by more than its last word and a space; the continuation
    lead is the commonest leading whitespace after evidence, and a single
    break before a line with another lead is a boundary; any other single
    break is a wrap when it is evidence or (the second pass) the line, a
    space and the next word exceed W, neither line is `list_like` or
    unwrapped and, inside a sentence, its block holds 8 words. Every other
    gap is a boundary. A region is line-form when more than half of its
    lines lie in failing paragraphs that hold no wrap. A whole text's edge
    frames are its maximal runs of first and last paragraphs under 8 words
    with no `list_like` line, none when no body would remain. `MeasureDiag`
    counts the candidate runs and their rejection causes, the second-pass
    wraps, lead changes, unwrapped lines, frames, retested runs, blocks
    whose widest line is outside the fit bound and inner `list_like` lines.
  - **the layout transfer** (`transfer`, `layout_transfer_v6`). The source
    is the whole parent for a mirror (or a layout donor) and the replaced
    span's paragraphs and lines for a mixed child (for an insertion, the
    paragraph and line holding its point). A prose child's frame lines (on
    the sides where a whole-text source has a frame) are kept as squashed
    lines; its body is rebuilt by v1's step (`layout::rebuild_child`: paired
    emphasis kept only if the source uses it) at the source body's break
    fractions, except that, for licensed reasons only, list lines are kept as
    their own paragraphs and an unpunctuated paragraph of at least 8 words is
    a closed paragraph, a kept unit of its own: both edges of each are fixed
    boundaries, a break target on one or inside a kept unit is realised by
    it, and the text between fixed boundaries is a segment of its own (one
    under 8 words is one paragraph). A body paragraph is greedy-wrapped
    when the source paragraph at its midpoint word fraction is wrapped
    (hard-wrapped source, no unwrapped line): each line's budget is W less
    the lead it takes, a span's first line W less its column, and a span's
    last line fits with the parent text after the span on that line (its
    maximal fitting suffix of words moves to a new line). The edits of an
    item are laid out one at a time in parent order (`lay_out_item`): a
    later edit's column is the document line's text before it in the item
    assembled so far, an earlier edit's tail counts only the parent text up
    to the next edit when that edit starts on the same line (an insertion
    whose separator holds a line break starts its text on a new line, so the
    tail before it is the parent's to the line's body end, as for the edit
    alone), and a later
    edit whose first word has no room on the shared line starts on a new
    line with a continuation lead (`sibling_column_wraps`; its replacement
    then begins with that line break and lead, outside its final span).
    Then each line of the document that holds a word of the child's
    wrapped unit, holds more than one word and is wider than W is split
    (amendment 38 item 38.1 as amendment 39 reads it,
    `document_line_splits`, recorded only when not 0): only inside the
    child's text, between two of its words on the line or just before its
    first word when that word starts the replacement and a word precedes it
    on the line (the replacement then begins with a line break and lead,
    which `splice`, `assemble_item`, `splice::assemble_parts`,
    `generation::assemble` and the frontier export accept, outside the
    final span), at the earliest break where the moved part, with the
    source's next continuation lead, fits in the child's own W; when none
    fits, the boundary break (amendment 42 item 42.1) just after the edit's
    last word on the line, which exists when parent words follow it there
    after a whitespace run; else the last; repeated on the shortened line
    and then the new one. At a boundary break the parent's whitespace run
    stays on the line as its trailing whitespace, then one line break and
    the next continuation lead, then the parent text, not re-wrapped (the
    (a-ii) construction, item 42.2): the replacement ends with that run,
    line break and lead (`replacement_tail`), its range is read as ending
    after the parent's run, and its final span ends at its last word.
    `splice`, `assemble_item`, `splice::assemble_laid_parts`,
    `generation::assemble_laid` and the frontier export accept it and check
    the run against the parent byte for byte ("human characters changed");
    the raw assemblers (`splice::assemble_parts`, `generation::assemble`)
    refuse every trailing whitespace as before. When item 13.7 finds no
    suffix that fits and the boundary break exists, it does not split (item
    42.3). Boundary breaks are counted (`tail_boundary_breaks`), and an edit
    whose end line needed one but whose following parent text starts
    without whitespace (an insertion), or whose run holds a character that
    is Python whitespace but not Unicode White_Space (U+001C..U+001F), is
    counted once (`tail_boundary_unavailable`), each only when not 0, in
    the transfer record and in the renorm, rows and frontier summaries
    (`profile::tail_boundary_breaks`, `profile::tail_boundary_unavailable`
    read a record's); the verdict's single-span assembly reads a profile
    v2 child with `generation::assemble_laid`; the transfer
    exposes the break read-only (`Transferred::tail_break`,
    `last_line_shares_parent_line`) for the dry run's count 4. A line is
    over when it is wider than the smallest W of the edits whose wrapped
    words it holds (an item's earlier edits keep theirs). The line left
    before a break between two words takes the source's next wrap trail
    (an empty pool counted); a break before the first word adds nothing
    before it. One-word lines are exempt; a line whose overflow lies in
    parent text with no boundary break, or that has no break point, is left
    for count 4; a line of parent text alone is never read. A single
    edit reads its parent's text on its first and last lines; an item's
    edit reads the item assembled so far before it and, after it, the
    parent text up to the next edit only (for an insertion whose separator
    breaks the line, up to that break), with break points in its own text,
    so a later sibling never changes an earlier edit's text
    (`FormPost`'s `item_sibling_text_mismatch` stays exact). The boundary
    between a frame and the body takes the source's body-adjacent boundary
    on that side; frame-internal boundaries take the source's
    frame-internal ones from the frame's outer edge, body boundaries the
    source's body boundaries, cycled. A child whose every paragraph is short
    has no frame. Lines take the source's whitespace by role:
    paragraph-first and continuation leads, wrap-followed and
    paragraph-final trails, frame lines the source frame lines'; a mirror
    takes the head and tail, and a span's first and last lines take none (the
    parent's own bytes around the span supply them; the no-room line break
    above is the one exception). A line-form source
    keeps the child's own lines: squashed and rendered, and every break
    between them takes the source's boundary separators in order (one
    newline when it has none, counted), with source line (j mod L)'s
    whitespace;
  - **the licence screen** (`cc_form_licence_screen_v1`) on the prose path:
    `screen::classify_all` gives every reason of a paragraph (the list test
    split into `quoted_line` and `list_marker`, on every line); the body
    licence is their union over the source's body paragraphs, the frame
    licence over its frame paragraphs plus `short_paragraph` and
    `no_sentence_end`. The final paragraphs are read by construction, never
    by measuring the document again: the parent's own boundaries for parent
    text, the transfer's units for generated text, an insertion's separator
    holding a newline as a boundary; a re-measurement is recorded
    (`remeasure`) and decides nothing. A final body paragraph holding
    generated text, or a raw child body line, with a reason outside the
    body licence is rejected
    (v1's first-match codes), and so is a child frame line with a reason
    outside the frame licence (`form_frame_<reason>`). A source that passes
    the screen has no frame and an empty licence, so its children get §6.2's
    screen exactly; line-form children are not screened (would-be reasons
    recorded);
  - **the echo screen** on the raw child (`echo::screen_item`; every edit of
    an item, insertions against the empty span). A donor row has no parent
    span, so its record says no comparison ran.

  Conventions come from `resolve_v2`: v1's `resolve` with each keyed draw
  taken from the child's population x domain cell when it holds a parent
  with the convention, else the population-wide prior, else none
  (`keyed_prior_domain`, `keyed_prior_population`, `none`), under the
  population's seed `norm_prior_seed` (V21's plan key with x =
  `"norm_prior"`); population ids come from the closed list
  `R2_POPULATIONS`. On a CC excerpt-layout parent (squashed lines, `"\n\n"`
  separators, no per-line whitespace, head, tail, hard wrap or paired
  emphasis, every paragraph passing the screen) the v2 child is v1's byte
  for byte; when the source uses paired emphasis, the child's is kept, so
  v2 then differs from v1 exactly when the child has some. The every-class
  step is v1's in the C block (`every_class`) and the window alone in the T
  block (`every_class_t_block`).
  - **Stated limits** (amendments 7, 10 and 13). Verse whose every line
    break a greedy wrapper could have made, in stanzas that pass the screen,
    is read as hard-wrapped prose, and with columns so is verse whose
    indented short lines fit the wrapper's test, so its children take the
    prose path; a second-pass wrap also exempts a failing paragraph from the
    line-form test. A run holding one break a wrapper would not have made is
    not first-pass evidence. Without first-pass evidence there is no W.
    There is one W per document: a block wrapped narrower than W splits at
    its sentence ends. A single-newline paragraph whose last line is wider
    than W by at most its last word reads as wrapped into the next. A
    `list_like` line inside a run that is not its first and has no
    `list_like` neighbour is read as a continuation, and a wrapped list
    item's continuation lines start paragraphs. A trimmed first-line indent
    is invisible and a tab counts one column. A short sign-off after a long
    last line joins the last paragraph. Lines of an unmarked list or verse
    ending within one word of W read as wraps. With two continuation leads,
    the less common one starts paragraphs. An email whose frame lines are
    more than half its lines is line-form. The parent's text after a span
    is human text and is never re-wrapped, so a re-measurement of a spliced
    child can split its paragraph. `tests/profile_v2.rs`,
    `tests/a10_rules.rs` and `tests/a13_rules.rs` pin them.
- **Records** hold ids, counts, outcomes and digests, never text.

## Diagnostics for the dry run's counts

`profile::constructed_paragraphs`, `profile::overlapping` and
`profile::Placed` are public so that the counting tool reads count 6's
paragraph, item 13.1's constructed final paragraph, through the screen's
own code (protocol A2 amendment 31 item 31.1; B27). Making them public
changes no output.

`Transferred::diag` (`TransferDiag`) and `Rebuilt::sources`
(`ParagraphSource`) are diagnostics for the §4.3 dry run's counting tool
(protocol A2 item 14.1; B23, crate `grammar-a-dryrun`). No rule and no
record reads them, and every text and record is unchanged by them; the
tests `v1_identity`, `profile_v2`, `a10_rules` and `a13_rules` are
unchanged.
- One `UnitDiag` per unit of `Transferred::paragraphs`: its role (head or
  tail frame line, body paragraph, kept list line, line-form line), whether
  a body paragraph is wrapped, the prose segment whose `place_breaks` made it
  (two body paragraphs of one segment are separated by a placed break; every
  other boundary is fixed), and the raw child's byte range it draws from.
- One `KeptUnit` per kept unit of the raw child (a kept list line, or a
  closed unpunctuated paragraph), with its byte range.
- `ParagraphSource` gives each paragraph `rebuild_child` rebuilds the input
  lines it draws from and whether a break was placed inside a line.

## Purity and Wasm

No function performs I/O, reads a clock or uses global or thread-local
state; randomness is only the seeded `rank`. Output size is linear in the
input. `place_breaks` holds two `(k + 1) x (n + 1)` tables for `k` target
breaks and `n` sentences; everything else works in linear memory. The
echo screen's check (a) allocates nothing, and check (b) renders each side
once. `measure` holds one entry per line, gap and paragraph of its input,
and `transfer` builds its output in one pass, so profile v2 is linear too.
The crate depends only on serde, serde_json and sha2, and `cargo
check -p grammar-a-form --target wasm32-unknown-unknown` passes.

## Where the code came from

The conventions, layout, paragraph screen, profiles and every-class step
moved verbatim from `grammar-cc-human`'s `era_norm`, which re-exports them
under their v1 names. `layout::normalise_child` is now `rebuild_child`'s
paragraphs joined by one blank line, so profile v2 shares the rebuild; the
golden digests pin the v1 outputs. The prose screen, word counts, whitespace
normalisation, `h64`, the document bounds and the FineWeb line classifier
moved from `grammar_a_pools::{text, fineweb}`, and `rank` from
`grammar_a_pools::splits`; grammar-a-pools re-exports them, so its paths are
unchanged. The sentence splitter and the window reasons came from
grammar-a-positives, which now depends on this crate:
`grammar_a_positives::text::sentences` (with `Sentence` and `SPLITTER_ID`)
re-exports this crate's splitter, and
`grammar_a_positives::generation::doc_window_reasons` delegates to
`window::reasons`, so this crate is their one source.

## Tests

- `tests/a10_probes.rs` turns the independent checker's probes of amendment
  7 (problems 1-4) into regression tests on its own seeded generators
  (`tests/support/a10_gen.rs`); it uses only APIs that existed at
  `ed25a148`, where every test but the no-indent control fails.
  `tests/a10_rules.rs` tests each rule of amendment 10 (columns, the first
  and second passes, unwrapped lines, lead changes, line-form, wrap flags,
  edge frames, the licence screen, kept units, whitespace by role, ids and
  the population list), the order of work's counterexamples, the design-B
  prototype's adversarial generators and hand cases, and the stop
  invariants of item 10.12 over synthetic corpora. `tests/a13_rules.rs`
  tests amendment 13's changes: the screen's reading by construction
  (mixed children and insertions in hard-wrapped parents), the fit bound,
  the list test, the greeting retest, W and unwrapped lines, a span's
  last-line budget, frame separators and the child frame rule, and exact
  break targets. `tests/a29_rules.rs` tests amendment 29 item 29.1 (an
  item's edits laid out in parent order: shared lines on the assembled
  document, count 4's three documents' structure, insertions, the no-room
  case, its continuation lead and its assembly, the tail before an
  insertion whose separator breaks the line, one-edit items and edits on
  lines of their own unchanged, a long first word after the parent's column
  laid out alone and then moved by amendment 38), `tests/a38_rules.rs`
  amendment 38 item 38.1 (count 4's 159-line shapes on synthetic
  hard-wrapped book prose, IRC messages with a continuation indent and
  reviews with indented paragraphs: single spans, insertions, one-edit
  items and two edits sharing a line; the document reading before and
  after, an independent check that no line over W keeps a break point, the
  replacement reading at 0, every entry point alike, unchanged records
  byte for byte, the first-word and parent-overflow cases, one-word lines,
  whole texts, and item 29.1's order kept), and amendment 39 (an earlier
  edit's line ending at a breaking insertion's separator, the earlier
  edit's W on a shared line, a flowed parent's wrap trail at a break
  between two words), `tests/a42_rules.rs` amendment 42 (one child word
  and a tail over W with and without room above, a 2-space run, start-side
  and end-side breaks on one edit, an insertion at the end of a line and
  one glued to the parent text, a later edit on the tail line, child_v2's
  screen on the assembled document), with the break geometry in
  `transfer`'s unit tests
  (the earliest fitting break, a moved part of exactly W, the parent text
  moved with it, the smallest W on a shared line, repetition over three
  lines, lead and trail cycling and empty pools; the boundary break's
  order, 1- and 2-space runs, a blank line above, a tail wider than W, the
  glued and U+001C cases), and `tests/a30_rules.rs` items 30.1 and 30.2 (a closed
  paragraph with two fixed boundaries, targets inside it, short text
  before it, its end and a closed first paragraph as before).
- `tests/profile_v2.rs` runs profile v2 on the synthetic fixtures in
  `tests/fixtures/` (IRC, verse, transcript, list, talk page, a RAID-like
  human with trailing whitespace and single-newline paragraphs, the same
  with a short closing line, a hard-wrapped email, the same with a greeting
  and a sign-off, a prose parent with mixed separators, a hard-wrapped text
  whose character and byte widths differ, an indented hard-wrapped letter,
  and prose followed by a list): layout classes, the transfer on each,
  line-form children kept line for line with the source's separators, the
  screen iff the source passes and the echo screen always, widths and
  columns in characters of the body, the whole parent's W for a span,
  titles, lead-ins, signatures and verse, the v2 ≡ v1 byte identity (texts,
  change counts, layout and form reasons) on excerpt-layout parents over
  mirrors and spans and where step 4 governs instead, spliced items with
  insertions and several edits, layout donors, the population x domain
  prior and its fallbacks, the per-population seed, and records without
  text. `grammar-cc-human`'s `profile_v2_parity` pins `assemble_item` to
  `splice::assemble_laid_parts` (the boundary break and its restated check
  included), `splice::assemble_parts` to its unchanged raw rules, and
  `norm_prior_seed` to V21's plan key.
- `tests/v1_identity.rs` and `grammar-cc-human`'s `era_norm_v1_identity`
  run one harness (`tests/support/form_v1_harness.rs`, synthetic fixtures)
  through this crate's paths and through the old public paths. Both must
  reproduce `tests/support/form_v1_golden.tsv`, whose group digests were
  recorded from the v1 code before the move. The exact-tie break grids
  (`breaks/12` to `breaks/14`) were added later and recorded the same way,
  from the unmodified v1 code at the pin commit.
- `tests/v1_rules.rs` holds copies of the v1 rule tests, plus direct checks
  of `place_breaks`' earliest-on-ties rules; the module tests cover the echo
  screen (each check alone, the verbatim copies that normalisation changes,
  and a control copy already in the parent's conventions) and the window.
- `grammar-cc-human`'s `era_norm_form_parity` checks the splitter and the
  window reasons through grammar-a-positives' paths: an alphabet sweep over
  the splitter's branches, and a sweep of every Unicode scalar value through
  each position the terminal, closer and whitespace classes decide.
