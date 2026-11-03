# grammar-a-positives

Positive generation for the competitive A protocol. Plans are mechanical
request lists; generation runs against one local llama-server at a time and
can resume; finalisation assembles accepted responses into records. Records
keep the exact replacement spans and the realised final-word share, and
failures are retained. Outputs contain prose and belong in ignored private
storage. No detector score selects anything.

```text
slop_ninja_a_positives plan     ...   # plan v1: protocol positive_plan
slop_ninja_a_positives plan-v2  ...   # plan v2: style- and task-diverse cohorts
slop_ninja_a_positives plan-v3 --train T [--dev-iter D] [--cal C] \
    [--splits train[,dev_iter]] [--prefix-every 2] [--middle-every 2] --out PLAN_DIR
slop_ninja_a_positives plan-v3 --train T [--cal C] --base-generators granite_base,smollm3_base,ernie_base --out PLAN_DIR
slop_ninja_a_positives plan-test --train T --test TEST --seal SEAL.json --seal-sha256 HEX --out PLAN_DIR
slop_ninja_a_positives replan-check --plan PLAN_DIR [--library L --fix-slop F] --out NEW_DIR
slop_ninja_a_positives stratify-train --plan PLAN_DIR --runs RUNS --partitions P \
    --generator KEY=BUDGET ... --cohort-shares C=S,... --seed S --out OUT
slop_ninja_a_positives generate --plan PLAN_DIR --generator KEY ... [--request-order FILE] \
    [--profile cc_cohort_profile_v2 --profile-population POPULATION --splits SPLIT]
slop_ninja_a_positives finalize --plan PLAN_DIR ...
slop_ninja_a_positives human-sample --train T --train-mining M --describers K1,K2 --out OUT
slop_ninja_a_positives library-session --config C --train T --root ROOT --round N [--hd-sample S]
slop_ninja_a_positives library-compile --root ROOT --eval-manifest SPLIT=PATH ... --out OUT
slop_ninja_a_positives output-breadth | rates | briefs | brief-library ...
```

## Commands

- **`plan-test`** builds the Test plan (schema
  `slop_ninja_a_comp_positive_plan_test_v1`): the protocol's Test cycle, with
  OLMo-2 instruct in slots 0-6 and the OLMo-2 base robustness slice. It refuses
  unless the seal file exists and its SHA-256 equals `--seal-sha256`, and it
  never overwrites a written plan. Train rows serve only as each evaluation
  domain's IDF source. `plan.json` records the inputs' hashes, request files
  and the protocol quotas.
- **`replan-check`** rebuilds a v1, v2 or v3 plan from the inputs that
  `plan.json` records into a new scratch directory. A v3 plan is rebuilt with
  its recorded splits, rates and base generators, from the inputs it recorded
  only, and never opens a recorded `dev_select` input (plans written before
  plan-v3 refused it recorded one; no v3 item uses it). The record lists it
  under `recorded_inputs_not_read`. It refuses if an input's hash changed,
  compares items, plan failures and every request file by SHA-256, lists the
  first differing items, and exits 3 on a mismatch.
- **`stratify-train`** writes one item-order file per generator for
  `generate --request-order` (`{GEN}` in that path names the generator).
  Request files are grouped by domain, so a plain `--limit` would cover only
  the first domains. The selection follows the Train human counts per domain
  (with `--floor`, optional `--domain-weights`, and credit for Train items
  that `--prior` runs already accepted), mixes cohorts by `--cohort-shares` and
  share bins within a domain, keeps voice ladders whole and admits only styles
  whose frozen partition is Train. The order interleaves domains, so every
  prefix is itself stratified. A budget counts items to attempt, not accepted
  rows. `--floor` defaults to 60; that default is the tool's, not a protocol
  value. `--target GEN --values VALUES-R2.json` budgets a generator at
  `v_fam` (S0-7, read from the register, never the command line; refused
  while absent) less its existing accepted Train items (D7, counting R0's
  base rows through `--prior`). Every plan and journal is read for its Train
  lines only: each line's split is read from its bytes before any parse, and
  lines of other splits are skipped unparsed and counted in `selection.json`
  (a journal line without a split is read when its request is a Train item's).
- **Style library v2** (`library.rs`). `human-sample` draws the passages for
  human-derived descriptions from Train and Train-mining and assigns their
  describers. `library-session` runs one inventor model against a local
  llama-server: cross-family review of pending candidates, its human-derived
  descriptions, then timed invention. Every request, response, candidate,
  review and decision is journalled, and sessions resume from the journals.
  `library-compile` writes the accepted and rejected items, cluster-level
  partitions, breadth and evaluation-author conflicts. Voice invention and
  human-derived descriptions read their seed voice examples from a private
  file (`library::SEED_EXAMPLES_FILE` under the repository root, or the path
  in `A_STYLE_SEED_EXAMPLES`); the session refuses to start without it.
  `briefs` and `brief-library` invent and compile style briefs alone
  (`briefs.rs`).
- **`output-breadth`** and **`rates`** report the style breadth of accepted
  outputs and per-generator rates from the journals.

## Raw twins and the screen-fail stratum in `finalize`

`finalize` emits two R-form outputs (protocol A2 §7.2, as read by amendment
19) whenever the journal's verdicts carry profile v2's raw-response
digest; with the profile off its outputs are byte for byte those of the code
before them. No flag is needed.
- **`raw_twin`**, in each accepted record: the same accepted raw responses
  (each verdict's `raw_text`, read exactly and refused unless its SHA-256 is
  the post record's `raw_response_sha256`) spliced into the same parent,
  which must match `parent_text_sha256`. It is a whole record with the
  N-form record's labels and its own text, spans, share, words and tokens;
  `id` is `rform:<id>`, `form` is `r_form`, `stratum` is `raw_twin`,
  `twin_of` names the N-form record and `rform` holds the raw digests and
  the rule. Its window verdict is the N-form document's.
- **`rform_screen_fail_v1`**, in the record, `failures.jsonl` entry or
  `exclusions.jsonl` entry of each item whose first raw responses failed the
  form screen only. A request's first raw response is its lowest attempt
  whose verdict shows model content (none of `transport_error`,
  `http_status_<code>` or `no_content`), accepted or not; a response
  rejected before the profile step is one and keeps the item out. The
  document is those raw responses spliced into the parent, window-checked
  here, with id `rform_screen_fail_v1:<item id>` and no `twin_of`. A window
  or splice failure leaves a note in its place with the reasons and no text.
  The form screen's codes are `form_line_*`, `form_paragraph_*` and
  `form_frame_*`; every other form code is outside it.

So `failures.jsonl` and `exclusions.jsonl` can now hold generated text (a
stratum document), as records do. Both outputs are emitted on every split
and counted in the summary (`rform_raw_twin`, `rform_screen_fail_v1`). The
stratum enters only the R-form evaluation populations (Dev-iter-2,
Dev-select-2, Test-2); N0's and S0-13's positives take raw twins only, so a
consumer reads `finalize::raw_twin_doc` for them and `screen_fail_doc` only
for an R-form evaluation population. `fs_pairs` pairs N-form records with
their twins and never with the stratum. The R-form documents of one run are
unique by text: a duplicate refuses the run.

## Raw twins and the screen-fail stratum in the spliced `finalize`

`slop_ninja_a_spliced finalize` emits the same two R-form outputs for the
spliced span sets (protocol A2 §7.2 and §7.5; amendment 19), through
`finalize`'s own helpers (`raw_twin_of`, `screen_fail_of`), whenever its
journals' verdicts carry profile v2's raw-response digest. With the profile
off its outputs are byte for byte those of the code before them.
- **`raw_twin`**, in each accepted record: the accepted raw responses (each
  verdict's `raw_text`, digest-checked) assembled into the parent as the
  accepted texts are (`splice::assemble_parts`, insertions with their
  separator). It is a whole spliced record with the N-form record's labels
  and its own text, spans, share, words, tokens and edit history; `id` is
  `rform:<id>` and `twin_of` names the N-form record. Its window verdict is
  the N-form document's.
- **`rform_screen_fail_v1`**, in the record or `failures/<population>.jsonl`
  entry of each item whose edits' first raw responses include a rejection
  and every rejected one failed the form screen only (the spliced
  generator's own checks are outside the screen). The document is every
  edit's first raw response, accepted ones included, assembled the same way
  and window-checked at finalize; a window or assembly failure leaves a note
  with the reasons and no text. Its edit history gives, per edit, the attempt
  of the response it splices (`accepted_attempt`) and that response's
  `verdict` (`accepted` or `rejected`). The stratum outcome rides with the
  item's entry, so an item not yet generated (`not_run`), which the spliced
  `finalize` gives no entry, has none (protocol A2 amendment 25 item
  25.4); the local `finalize` keeps its `not_run` failures' outcome.

Both are counted in `reports/quality.json` (`rform_raw_twin`,
`rform_screen_fail_v1`); the R-form documents of one run, both populations,
are unique by text.

## Form profile in the acceptance path

`generate` and `slop_ninja_a_spliced generate` take `--profile
cc_cohort_profile_v2 --profile-population P` (protocol A2 §6.2, as amended by
amendments 7 and 10). Without `--profile`, or with `--profile raw`, nothing
changes: `generate` keeps `generation::verdict` with no post-processing and
no final-document window, the spliced generator keeps its own acceptance, and
the journals are byte for byte those of the code before the profile (the
clock fields aside). The v1 profiles are refused. `form_post.rs` holds the
step; every rule comes from `grammar-a-form`:
- **An item with one request.** A mirror or a single-span request goes
  through `profile::child_v2` (the layout transfer from its source, the form
  screen when that source passes it, the echo screen on the raw response). A
  spliced edit goes through `profile::item_v2` with that edit, insertions
  with their separator. The window `cc_final_length_window_v1` (150..=450
  whitespace words) reads the item's final document: the laid-out mirror, or
  the parent with the laid-out span or edit.
- **An item with several requests** (a multi-span mixed item, or a spliced
  item with several edits) is judged as a whole when its last request is
  judged: `profile::item_v2` over every edit's raw text, so the form screen
  reads the assembled item's generated paragraphs (amendment 7 item 7.9), the
  echo screen compares every edit, and the window reads the final document,
  assembled as finalize assembles it. A rejection there is that request's
  attempt's and regenerates within its attempt limit; accepted requests are
  never reopened. Earlier requests of the item get the rules that read only
  their own edit (the transfer's line reasons and empty check, the echo
  comparison); their partial-document paragraph reasons are recorded as
  `deferred_reasons` and reject nothing. The item's requests are judged one
  at a time, and the siblings' raw texts come from earlier verdicts
  (`raw_text`), so a resumed run judges the item the same way. The spliced
  generator holds the batch binding's lock for a run, because an item's
  edits can come from two generators' runs.
- **Text.** The accepted text is the laid-out child; the verdict keeps the
  cleaned response the step read as `raw_text` (the journal's `raw_body`
  keeps the HTTP body). Only profile v2's step keeps `raw_text`: every
  `generation::PostProcess` states whether it is profile v2's
  (`is_profile_v2`, no default), and a verdict of any other step (the CC
  generator's v1 normaliser) carries none, so v1 journals keep their bytes. A mirror's laid-out text keeps its parent's edge
  whitespace and line leads, so consumers must not trim it. Every echo
  verdict is `echo::screen_item`'s.
- **Order.** `generate` runs its standard checks on the laid-out text. The
  spliced generator runs its own checks on the raw response, because they
  read the response's own line breaks, markdown, quotes and punctuation, and
  then the profile step. Any reason rejects the attempt.
- **Conventions** are `profile::resolve_v2` from the parent's counts and the
  prior of the run population's parents (by the parent's domain, then
  population-wide, then none) under the population's seed
  `norm_prior_seed(P)`, keyed by the item id, so every request and attempt of
  an item sees the same choice.
- **The prior** holds the parents of the plan's items in the run's one split
  (`generate` refuses a profile run over several splits) or, for the spliced
  generator, in its plan population. It is computed from the plan's bound
  inputs (`items_sha256`, the split's input digest, or `parents.jsonl`), so a
  pilot and its bulk compute the same prior. The dry run prints its record
  (`profile_prior`; no text); the first real run writes it as
  `RUNS/form-prior-<sha256>.json` (mode 600), whose bytes hash to the
  `prior_sha256` that every post record, run record and binding names. P must
  be on amendment 10 item 10.8's closed list,
  `grammar_a_form::profile::R2_POPULATIONS`.
- **The batch binding.** A batch (a `generate` split, or a spliced plan
  population) is judged under one profile record. A v2 run writes
  `RUNS/form-profile-split-<split>.json` or
  `RUNS/form-profile-population-<population>.json` (mode 600) before its
  first request. A later run with another record (profile, population, seed
  or prior) is refused, and so is a run with the profile off after a v2 run,
  or a v2 run after requests of the batch were judged with the profile off.
  The dry run checks the binding and writes nothing.

`tests/form_profile.rs` runs both generators' acceptance against a local
mock server on synthetic text: profile off reproduces the journal digest
recorded from the code before the profile; profile v2 lays children out as
`grammar-a-form` does, and its form, echo and window rejections fire, with
records that hold no text. Items with two spans or edits are judged on their
final document in both directions at the window's bounds and for the
paragraph screen, regenerate within the attempt limit, resume from the
journal and serialise concurrent workers. On the binaries it checks the
flags, the dry run's prior record and population, the binding and its
refusals, the spliced prior's population scope and lock, the item check's
wiring, and runs with the profile off, `raw` (the same journal) and on.
`form_post`'s unit tests cover the flags, the prior, the item- and
population-keyed draws and the binding.

## Re-verdict under profile v2 (`renorm`, `renorm-rows`)

`renorm.rs` re-verdicts existing positives under profile v2 with no model
call (protocol A2 §4.3, with amendment 20's readings): it reads journals,
plans, rows and parents from disk, holds no HTTP client and writes a new
directory (files mode 600). Every echo verdict comes from
`echo::screen_item`, through `FormPost` or `profile::child_v2`.
- **Splits (§2.4).** `dev_select` and `test` are refused as a batch's split.
  Every line of a plan's items, requests and input, of a journal, and of a
  rows or donors file is selected by the split its split key names, read
  from the line's bytes before it is parsed (amendment 15 item 15.4's
  reading). Lines of other splits are never parsed; the summaries count them
  by split. A line whose split cannot be read from its bytes refuses the file
  by line number (a journal line without one is counted as torn).
- **`renorm --plan PLAN --runs RUNS --profile cc_cohort_profile_v2
  --profile-population P`**, with `--split S --tokenizer TOK [--records
  IDS]` for a local plan or `--population POP` for a spliced plan. One split
  or plan population is one batch, with the `FormPost` that `generate
  --profile` (or the spliced generator) builds for it. Each request's record
  response (its earliest accepted response, the one `finalize` reads) is
  re-verdicted as if the profile had been in the acceptance path; a later
  accepted response of the same request is rewritten as rejected
  (`renorm_superseded_accepted_response`). The raw child is the verdict's
  `raw_text` when profile v2's step ran (checked against the post record's
  digest), else, when no post step ran, the verdict's `text`, never trimmed.
  A post-processed verdict without `raw_text` (another step's, such as a v1
  normaliser's) is refused, since its raw child cannot be recovered. The
  journal's `raw_body` is re-cleaned as a check (`raw_body_check`). A local
  verdict is the profile step, then `generation::verdict`'s checks on the
  laid-out text in its order, except the overlap screen (the protected index
  is not read) and the final-document window (the step's). A spliced
  verdict is the original splice verdict with the step merged in, as
  `spliced_post` merges it. An item with several requests is judged in item
  order, as `generate --profile` judges it: the last request with every
  sibling accepted gets the item check.
  - Outputs: the batch's binding and prior record; the re-verdict journals
    (`GEN/journal-renorm.jsonl`: every line of the batch in input order, byte
    for byte, except each re-verdicted response, whose verdict is replaced
    and which gains a `renorm` record, and each superseded accepted response;
    intents carry over), so `generate --profile` resumes the output with the
    same record; `reverdict-BATCH.jsonl` (one record per item, no text, with
    each request's full profile record for the dry run's counts);
    `regeneration-plan-BATCH.jsonl` (keyed by request id: each rejected
    request of an item of R0's records, `IDS`, one item id per line, whose
    every request was accepted, with attempts counted per request from the
    first); `regenerate-order-GEN.txt` (local batches) and
    `RENORM-BATCH.json`.
  - Actions: `drop_out_of_attempts` (a rejected request has no attempt left;
    §6.4 refills), `regenerate_mirror` (a mirror: B5b plans a new request
    with `mirror_target_words_v2` and the remaining attempts) and
    `regenerate` (any other item: D4 resumes it).
  - **D4's resume** runs only the `regenerate` items: `generate --profile
    --profile-population P --splits S --runs OUT --request-order
    OUT/regenerate-order-{GEN}.txt` on the output journals. Without the
    order file every request of the split that is not done would be pending,
    including items R0 never attempted. Mirrors are regenerated through B5b's
    new requests, not by resume. `generate` refuses a request or items
    file of more than one split, so D4 runs on a one-split copy of R0's plan
    (`split-copy`, below).
- **`renorm-rows --rows ROWS [--donors HUMANS] [--raid-documents DOCS]
  --profile cc_cohort_profile_v2 --profile-population P`** normalises Train
  bundle rows that no journal regenerates.
  - RAID rows (`raid-v1-*`) take their RAID human: the one unattacked
    (`attack` `none`) human document of their source group in DOCS, the RAID
    pool documents the assembler read (the bundle's row when it holds it).
    Attacked human variants are never layout sources.
  - Stage-3 legacy rows and hosted captures take a recorded parent that is a
    human row of ROWS. A stage-3 legacy row with no recorded parent and one
    span takes the stage-3 legacy human of its source group whose bytes equal
    the row outside the span. A row with no recorded parent whose model text
    is all of it takes a layout donor: the same-domain human of HUMANS (ROWS
    when omitted; `human_only` with weight > 0) with the smallest plan key
    `SHA-256("slop_ninja_a_comp_r2_plan_v1:layout_donor:" + row id + ":" +
    human id)`, whose layout properties only are transferred. Other rows, and
    rows that fail, are dropped with a receipt.
  - The batch's prior holds its parents only (RAID humans and recorded or
    recovered parents); donors are not parents. The window runs on every
    laid-out document.
  - Outputs: `rows-reverdict.jsonl` (no text), `rows-normalised.jsonl` (the
    kept rows' laid-out text, spans and share, keyed by bundle row id, with
    the raw twin's digests; it replaces those fields of the rows it lists,
    and a planned row it does not list is dropped), the prior record and
    `ROWS-RENORM.json`. No landed assembler input reads the normalised rows
    yet.

Each summary names its rules (`rules`) and readings. The unit tests in
`renorm.rs` pin the local re-verdict to `generation::verdict` with the
profile on and the spliced one to `spliced_post`, and cover the item rule,
the request-id keyed plan, its attempts and actions, the request orders,
superseded accepted responses, the journal bytes, determinism, the raw-text
digest, the donor draw, the rows' layout sources, keyed draws by row id, the
window on rows, the RAID echo and the byte-level split selection with
deliberately unparsable lines of other splits; `tests/renorm_cli.rs`
re-verdicts profile-off journals written by `generate` and resumes the
output with `generate --profile --request-order`.

## One-split copies, D4's plan and keyed pilots (B5b)

Round 2's planner steps (protocol A2 §4.3, §6.3, D4, V22, V23; BUILD-PLAN
rulings R2 and R6). Every output is a new directory or file, mode 700 and
600; records hold ids, counts and digests.
- **One split per plan file (R2; amendment 20 item 20.1).** `generate`
  refuses a request file, and under `--profile` an items file, that holds
  more than one split; `finalize` refuses the same for its items, request and
  plan-failure files, and `mirror-plan` and `pilot` for the files they read.
  Every line's split keys are read from its bytes before any parse
  (amendment 15 item 15.4's reading, `renorm::line_split`), and the refusal
  names the line number and the split's class, never an id. A line naming
  `dev_select` or `test` is refused even when every line of the file names
  it. In a plan-failures file a plan-wide line (no split, item or parent
  key, such as plan v2's ladder relaxation count) is passed over. `generate`
  and `finalize` also refuse, before reading any plan file, a plan that
  records an old Dev-select input they would read.
- **`split-copy --plan PLAN --split S --out COPY`** writes a one-split copy
  of a local plan. Each line of its items, request and plan-failure files is
  selected by its split read from its bytes and copied byte for byte; lines
  of other splits are skipped unparsed and counted; a line whose split
  cannot be read refuses the copy by line number (a plan-wide plan-failure
  line is skipped and counted: it describes the source plan's ladders over
  every split). `dev_select` and `test` are refused as the split.
  `plan.json` keeps the split's input and, when the source records one and
  the split is not CAL, the CAL input unchanged as `generate`'s overlap extra
  (R0's screen for the same plan: CAL is not in the protected index;
  protocol A2 amendment 24). A consumed-Test input is never kept. It also
  holds the copied files' digests and a `split_copy` record with the
  source's digests and input names. Profile v2's batch binding names no plan
  path or digest, so renorm's output runs resume under `generate --profile
  --plan COPY`. `replan-check` refuses a copy.
- **Carried attempts (R6).** A request may carry `attempt_limit`, the
  attempts it gets counted from its first; absent (every planned request) it
  gets `generation::ATTEMPTS` and is not serialised, so request bytes and
  `replan-check` are unchanged. `generate`'s attempt loop, its pending
  selection, `finalize` and renorm's regeneration plan read it. `generate`
  journals it on the request's intent lines (only when present, so R0's
  journal bytes are unchanged), so readers that hold journals alone
  (`rates`, `stratify-train`) count such a request done at its own limit.
- **`mirror-plan --plan COPY --renorm OUT --split S --values VALUES-R2.json
  --out D4`** (`mirror_regen.rs`) turns renorm's `regenerate_mirror` rows into
  new requests of the same items: `mirror_target_words_v2` words
  (`max(parent words, ⌈150 / r05⌉)`, r05 read from the register's S0-6 as an
  exact decimal or ratio, refused while S0-6 is absent),
  the old prompt with only its length changed, a request digest and seed of
  their own, and `attempt_limit` = the row's remaining attempts. Every other
  line is the copy's. The new mirrors are judged in renorm's batch (D4 is one
  batch with one prior, item 10.8): the plan keeps every parent, and it
  checks that `generate --profile` builds renorm's record before writing.
  `d4-order-GEN.txt` lists the `regenerate` items and the new mirrors; D4 is
  `generate --profile ... --plan D4 --runs OUT --request-order
  D4/d4-order-{GEN}.txt`.
- **`pilot --plan PLAN [--request-order ORDER] --step D4 --out P`**
  (`pilot.rs`; protocol A2 §4.3, V22 and amendment 24): the D-step's rows
  are its planned items, one per item (the listed items, or every item), in
  ascending plan key `SHA-256("slop_ninja_a_comp_r2_plan_v1:pilot:" + item
  id)`; a row's cell is domain × origin (`model_only` for a mirror, `mixed`
  otherwise). The pilot is the shortest prefix holding 59 rows of every
  touched cell; when a touched cell holds fewer than 59 rows in the whole
  D-step, the D-step is piloted whole. It writes `pilot-order-GEN.txt`,
  `bulk-order-GEN.txt` (input order) and `PILOT.json`, the record S0-11
  binds. Pilot and bulk are one batch. D8's spliced plans are not piloted
  here.
- **`enron-half --pool POOL --root ROOT --values VALUES-R2.json --out
  TRAIN`** (`enron_half.rs`, V23): the writer keys ordered by plan key
  `SHA-256("slop_ninja_a_comp_r2_plan_v1:enron_half:" + key)`, the first 50
  of 99 the Train half. The pool must be the one S0-9's `SHA256SUMS` input
  names under ROOT, with S0-9's digest, and its `documents.jsonl` the digest
  that `SHA256SUMS` lists (amendment 23 item 23.1's rule for Enron inputs).
  Each line's `writer_key` and `collection` are read from its bytes; only
  Train-half lines are parsed, and they are written with `frozen_split`
  `train`. The halves must equal S0-9's lists, and the Train half's document
  count and words, and the diagnostic half's document count, S0-9's. Writer
  keys are never printed.

`tests/b5b_cli.rs` runs these on synthetic text: byte-level selection with
deliberately unparsable `dev_select` and `test` lines, the refusals, a
one-split copy that runs, renorm's binding accepted on the copy, D4's plan
from renorm's output with its new mirrors pending in renorm's batch,
attempts that stop at the carried limit, the pilot, `stratify-train`'s Train
lines and targets, and the Enron half, with a negative case for each guard
the B5b review's mutations reached (files of a refused split alone, the
split checks of `generate --profile` and `finalize`, mirror-plan's renorm
bindings and order files, carried limits in renorm and `rates`, the pilot's
order files, the Enron bindings).

**Not built here** (BUILD-PLAN section 4's B5b scope, open): a Train-only
planner for new mirrors (D5's top-up mirrors, D6's Enron positives) at
`mirror_target_words_v2`, the top-up plan from Train-mining against S0-8,
D6's plan over the Enron Train half, and D8's pilot on spliced plans.

## Plan v3: exact-span supervision cohorts

Plan v3 is additive. It holds only two cohorts, keyed under its own prefix
(`slopninja-a-comp-plan-v3:`), so its item and request ids never collide with
those of plans v1 and v2, and existing runs stay valid. `generate` and
`finalize` take a v3 plan directory like any other. The plan has been
written and tested but not run. With `--base-generators` it plans the Train
base-model cohort instead (below).

Only the inputs given are read, here and by `generate` and `finalize` on the
plan; `--train` is required and every planned split needs its input.
`--dev-select` is refused before any file is opened: old Dev-select is never
read, scored or parsed (protocol A2 §2.4), and no v3 split uses it. `generate`,
`finalize` and `replan-check` never open a `dev_select` input that an older v3
plan recorded. `--dev-iter` is read only as a planned split. `--cal` is read
only as `generate`'s overlap extras: its humans are dedup sources for the
generated text, and nothing is planned from them. The Train IDF comes from
`--train` alone, so omitting the other inputs leaves the plan unchanged.
Without `--cal`, `generate` has no CAL humans to add as dedup extras, and the
protected index still applies. That index excludes CAL, so a plan without
`--cal` does not screen generated text against CAL. A run that needs v1.2's
check against every other split passes `--cal`.

Both cohorts are source-conditioned rewrites of a human passage. They form
their own cohorts and populations, separate from writing generated from
scratch. They exist to give the share output measured fractional
supervision with exact per-word labels.

| Cohort | Operation | What the model sees and writes | Label shape |
|---|---|---|---|
| `prefix_continuation` | `prefix_continue` | A whole-sentence human prefix holding 30% to 70% of the words; the model writes the rest, about as many words as it replaces | human, then model |
| `middle_rewrite` | `middle_rewrite` | The passage split into BEFORE, MIDDLE and AFTER; the model rewrites MIDDLE in context | human, model, human |
| `middle_rewrite` | `middle_infill` | BEFORE and AFTER plus up to six TF-IDF key terms of the removed run, which itself is hidden; the model writes the missing passage | human, model, human |

### Rules

- **Selection.** A human gets a prefix continuation when
  `rank(k_prefix) mod prefix_every == 0`, and a middle rewrite when
  `rank(k_middle) mod middle_every == 0`. Ranks are taken within
  (split, domain), as in the earlier plans.
- **Prefix cut.** A target kept fraction is drawn per item, uniform in
  [30%, 70%] of the words. The cut is the whole-sentence boundary closest to
  the target inside that range, with ties going to the shorter prefix. A
  passage with no boundary in range gets a recorded plan failure.
- **Interior run.** At least one sentence and 10% of the words stay before
  and after the run. A size target in [15%, 50%] and a midpoint target in
  [30%, 70%] are drawn per item. The run closest in size is chosen, then the
  one whose midpoint is closest, then the earlier one. The mode is set by
  `rank(k_middle_mode) mod 2`.
- **Generators and decoding.** Train uses the eight Train instruct families
  and Dev-iter the seven Dev families. Decoding is sampled per item with the
  plan-v2 sampler. Sealed splits (Dev-select, CAL, Test) are refused.
  Dev-iter items are supplementary and ungated.
- **Acceptance.** The plan-v2 checks apply, plus two copy screens:
  - A response may not share a 12-word window with the kept human context.
  - For `prefix_continue` and `middle_infill`, it may not share one with the
    removed run either, since recalled human words would be mislabelled as
    model words.
  - `middle_rewrite` shows the run, so windows it shares with the run are
    recorded (`shares_removed_run_12_word_window`) but allowed.
- **Records.** Train records carry the population `train_span_supervision`;
  Dev-iter records carry `supplementary_eval_span_supervision_not_gated`.
  `share_evidence.word_label_runs` gives the exact per-word labels as
  `[[label, words], ...]` runs (0 human, 1 model), computed from the
  assembled spans under `unicode_split_whitespace_v1`. The share is always
  the realised share, never the requested fraction.

### Base-model mode

`--base-generators` (a comma-separated subset of `granite_base`,
`smollm3_base` and `ernie_base`) switches plan v3 to the Train base-model
cohort `train_base_v3`, and no instruct cohort is planned. It plans Train
only. It reads `--train` and, when given, `--cal` (overlap extras only), and
it refuses `--dev-iter` and `--dev-select`, other splits, `--prefix-every`,
`--middle-every`, repeated names and any family that is not a Train base
family. Every refusal fires before any file is opened.

- **Assignment.** Every Train human gets one assignment. Its rank is that of
  the round-2 plan key `SHA-256("slop_ninja_a_comp_r2_plan_v1:base_cohort:" +
  id)` (unkeyed, label `x = base_cohort`) within (train, domain). The
  generator is `base_generators[rank mod G]`, in the fixed order above. The
  kind is a continuation when `floor(rank / G) mod 2 == 0`, else a splice.
  Assignments are therefore 1:1, to within one, in every domain × generator
  cell when plan failures are counted. Plan failures fall unevenly by kind (a
  splice needs both a prefix and a suffix), so the planned items need not be
  1:1.
- **Spans, prompt and decoding.** These are the Tier C robustness rules of the
  Test plan's OLMo-2 base slice (`plan::build_test`), applied unchanged:
  - A continuation keeps the shortest whole-sentence prefix with at least 30%
    of the words, and the model writes the rest.
  - A splice keeps the shortest whole-sentence prefix and suffix with at least
    25% each, and the model writes the middle from the prefix alone.
  - The request goes to `/completion`. Its prompt is the verbatim prefix, with
    no template.
  - The cut budget and the target are the removed words.
  - Decoding is `plan::robustness_decoding`: temperature 0.8, top_p 0.95,
    max_tokens 1024.
  - A passage with no such plan is a recorded plan failure. No other kind or
    generator is substituted.
- **Acceptance.** This is `generate`'s completion path: the cut at a sentence
  end, the word window, continuations of at least 20 words, the Train-side
  12-word copy screen against the removed text, and the overlap and token
  checks.
- **Labels.** The cohort is in `span_ops::COHORTS`, so `finalize` writes
  `share_evidence.word_label_runs`. Its records carry the population
  `robustness_train_only`, as the base cohorts of plans v1 and v2 do.
- **Size.** The plan holds candidates only, and `stratify-train` chooses what
  runs. Protocol A2 §6.4 asks for `v_fam` accepted rows per Train base family,
  counting the family's existing base rows. A `--generator` budget counts
  items to attempt, not accepted rows. The cohort therefore runs in passes:
  - Each pass sets family f's budget to `v_fam` minus f's accepted base rows
    so far: its existing rows plus this cohort's accepted rows.
  - Passes repeat until that is 0 or f has no candidates left.
  - `--prior PLAN_DIR=RUNS_DIR` names the plans and runs of the existing base
    rows, which then count as existing rows per domain in the quotas. Those
    counts come from the runs' journals, which can differ from a trainer
    bundle's counts.
  - The rule's inputs (`v_fam`, the existing counts and `--prior` pairs,
    `--floor`, `--seed` and the domains covered) are fixed before the plan
    runs.

  One pass, for budgets `N_g`, `N_s` and `N_e`:

  ```text
  slop_ninja_a_positives stratify-train --plan PLAN_DIR --runs RUNS \
      --prior EXISTING_PLAN=EXISTING_RUNS --partitions P --cohort-shares train_base_v3=1 \
      --generator granite_base=N_g --generator smollm3_base=N_s --generator ernie_base=N_e \
      --floor F --seed S --out OUT
  ```
- **Kind balance.** `stratify-train` balances this cohort's kinds across
  passes. Inside a domain, each pick takes the kind with fewer accepted plus
  selected items of this cohort for that generator, with ties in rotation
  order. The kinds therefore move to 1:1 and then stay within one, per domain
  and generator, while both kinds have candidates. Failed attempts are not
  credited, so later passes make them up. Other cohorts keep the plain
  rotation, which restarts at the first stratum on every call, so an odd
  quota favours that stratum. `selection.json` reports the selected and
  credited counts per domain, cohort and kind (`balanced_kinds`).

`plan.json` records `mode: base_model`, the base generators, the plan key
prefix and name, coverage by generator, kind and domain, and plan failures by
kind and reason.

### Plan records

`plan.json` records:
- the inputs and their digests;
- producer digests;
- the rules;
- coverage by cohort, operation, generator, domain, replaced-fraction decile
  and kept-prefix decile;
- plan failures by reason.

### Attribution

The idea of human-prefix continuations, and of regenerating an interior span
between kept human context as a source of exact word labels, comes from the
MIT-licensed It's AI validator
(<https://github.com/It-s-AI/llm-detection>, Copyright 2024 It's AI). The
span selection, prompts, single-request infill, acceptance screens, keys and
records are our own and run inside this planner. No code or prompts were
copied.

### Tests

`cargo test -p grammar-a-positives span_ops` runs on synthetic passages. It
checks:
- prefix cuts inside 30% to 70%;
- interior runs with kept context;
- keyed draws;
- both cohorts and all three modes, with the spans, prompts and targets they
  produce;
- exact label runs after assembly;
- that generation is deterministic and its ids are disjoint from plan v2;
- refusal of sealed splits;
- the copy screens;
- that instruct-mode output is byte-identical to the output from before base
  mode (pinned digests);
- for base mode:
  - the argument checks: `--dev-select` refused in both modes, and
    `--dev-iter`, other splits and the instruct rates refused in base mode;
  - the input rule by which `generate`, `finalize` and `replan-check` skip a
    `dev_select` input recorded by a v3 plan;
  - the generator validation and the generator and kind rule;
  - the Tier C spans, prompts, budgets and decoding, equal to
    `plan::build_test`'s base slice for the same passage and kind;
  - exact labels and 1:1 kinds in every cell;
  - a `/completion` run through `generate`'s acceptance path against a local
    mock server, with no model call. Two replies are accepted, and `finalize`
    turns them into `robustness_train_only` records with exact label runs. A
    reply reciting a 12-word window of the removed text and a 19-word
    continuation are rejected, each for its own reason only, on both attempts.

`cargo test -p grammar-a-positives stratify` also checks the base cohort's
kind balance over two batches with odd per-domain quotas, against the plain
rotation. `tests/plan_v3_cli.rs` runs the binary on synthetic inputs:
- every refusal fires before any file is opened or the output is written;
- a base plan, with and without `--cal`, re-plans byte for byte;
- `replan-check` does not open a `dev_select` input that a plan recorded.
