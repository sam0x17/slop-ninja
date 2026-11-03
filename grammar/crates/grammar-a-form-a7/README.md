# grammar-a-form-a7

Protocol A2 amendment 7's form rules: `grammar-a-form`'s sources at
`ed25a148` (B2's landing with amendment 7's fixes, log index 19), copied byte
for byte under the crate name `grammar_a_form_a7`.

It exists for one use: the §4.3 dry run's amendment-7 comparison (item 10.12
figure 8, item 14.2's last bullet; B23's counting tool,
`grammar-a-dryrun`). Nothing in a generation, acceptance or assembly path
depends on it.

- `tests/blob_ids.rs` pins every file under `src/` to its git blob id at
  `ed25a148`, so no rule is re-implemented. A change to any of them fails
  that test.
- The crate keeps `ed25a148`'s behaviour exactly, including what later
  amendments fixed: for example `conventions::render_sites` panics on two
  spaced dashes one space apart (`" - - "`), which the landed code skips.
  Callers that run it on real children catch the panic and count it.
