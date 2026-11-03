# A competitive campaign source migration

In the public snapshot, the branches and commits this note names are not
published; it records the private development history.

Recorded on October 30, 2026. The A competitive campaign reached main as one
squash commit, built from six branch tips. The squash changed some files
relative to those tips:

- guard descriptions in the frontier collector, its collection driver and
  [frontier positives](a-frontier-positives.md) were reworded, and the
  collector's private config keys and receipt call-log field were renamed (the
  [wording revision of 2026-10-31](#wording-revision-of-2026-10-31) renamed
  them again);
- licence interpretations and comparator remarks in the pool adapters are
  removed, and pool records keep only the terms as stated, with their links;
- the Hacker News, Goodreads, CORAAL and MediaSum adapters and subcommands are
  removed (see the "Not admitted" list in the
  [protocol](a-competitive-protocol.md#admitted-human-sources));
- the style library's seed voice examples moved to a private file;
- the `inference/a-v6-cpu` source pins follow its requalification.

The tips themselves are unchanged on origin, so every receipt and seal that
hashes an earlier file can still be checked against those branches.

## Squashed tips

| Branch | Tip |
|---|---|
| `origin/a-comp-tierc` | `75470c8bb14555fdabb016e378c456e14999e6a6` |
| `origin/a-comp-raid` | `2e1e1d739e91e6b19c0be97a0e1ed58a341e7c33` |
| `origin/a-comp-uspto` (contains `origin/a-comp-pile` at `ad2c2bccaf8f3fa5853baf1355db2597219ecbe2`) | `ffff8044f9c84e30309c22a4cbd690ad401aae07` |
| `origin/a-comp-sn32-gen` | `86c63ceb1708c2ab0978acc6151aa6abfef22419` |
| `origin/a-comp-sn32-eval` | `e283d3b8753163bb7eea094e19e816d54fdc340c` |
| `origin/a-comp-exist` | `ee672c03fe7fb9c8551ddc5f55a57da60fed312c` |

`origin/a-comp-sched` (`794c8c4f`) is not part of the squash.
`origin/a-spans-v1` and `origin/a-spans-v1-data` contain `origin/a-comp-tierc`.
Bring them onto main with `git rebase --onto <main> origin/a-comp-tierc`, then
check that the rebase keeps the changes above. Do not merge their history.

## Sealed and source-hashed files whose bytes changed

| File | SHA-256 at the tip | SHA-256 on main | What binds the earlier bytes |
|---|---|---|---|
| `tools/a-comp-tierc/frontier-route-driver.sh` (omitted from the public copy) | `5ce98d6f916be752eff668be1c912265fb932a82392e406bda3c82b6942caa65` | `a735f86657ad32062957726c4de4d3ff22be64856609625c3419bc6a0ea6b289` | Tier C `SEAL.json` items `frontier-route-driver.sh` and `script-frontier-route-driver.sh` |
| `examples/a_frontier_positives.rs` (omitted from the public copy) | `312fc3ee39bcf8c48c8c84f1e83a0ae37d9f27d727c52762c5eef2c6582bf942` | `e723443cd2e7c47a634a87ba7a8f0c58051aa397920ad68312954ae074ed441e` | `producer_sha256` in the Test plan and `collector_sha256` in every Test frontier receipt; the sealed collector binary was built from these bytes |
| `grammar/crates/grammar-a-positives/src/diverse.rs` | `a179e012889813e50f5ac8f2c0c95c9e93f0392369491c17c05043a7503adec9` | `13541b9eaf30c86ea11f4586dad4c32fc32c28a1bfdd2cf7c94565964c091d85` | `producer_sha256` in plan v2 and v3 metadata, including the existing plan v2 `plan.json` |
| `inference/a-v6-cpu/src/main.rs` | `ad6867eb8558b103d0ba9b00abc1e2f826005b097ca173418bf3e98ccd8eada3` | `ddb592f485b9d8a8c408e3e72ec31d0fb92fff081ce36daba116a4f13dab2dea` | None. Its source pins were stale, so a build of these bytes refused to start and wrote no receipt |

The consumed Tier C Test receipts hash the earlier bytes: all 829 frontier
receipts and the Test plan record `312fc3ee…`, and the seal binds `5ce98d6f…`.
Both files remain byte for byte in `origin/a-comp-tierc`. Audit or reproduce a
Tier C Test step from that branch, not from main. Run from main,
`test-postseal.sh` stops at its seal check, because the collection driver's hash
differs.

`test-postseal.sh` and `frontier-test-driver.sh` (omitted from the public copy) already differed from
`SEAL.json` at the tierc tip. The Tier C seal addendum (`ADDENDUM-1.json`)
binds those amended bytes (`b36ffd91…` and `a3ebf169…`), and the squash leaves
both files unchanged.

In `diverse.rs` only test-module fixtures changed. `replan-check` does not
compare producer hashes, so the existing plan v2 still checks. The earlier
bytes remain in `origin/a-comp-sn32-gen` and `origin/a-comp-tierc`.

The earlier qualification receipt of `inference/a-v6-cpu` (`runtime-main-v1`)
binds `main_rs` `cb5779c72e9ef021ed861491b21f94257204235d21dcee6de680a6ab4d86d89b`
and the earlier source pins. The requalification receipt
(`aa0c51eb6c9396951e759443bba73a2837b3bb02d9832ea35b8797ae1cc9e1e3`) binds the
main bytes `ddb592f4…`. From the tip bytes to main, only the three source
pins in `src/main.rs` changed.

The collector and the collection driver behave as before, apart from wording in
logs, stop notes, reports and two descriptive receipt strings
(`identity_source` and `cost_basis`), and the renames below. New runs write
the stop reasons and report fields that the private config's description
lists (see [Guard wording of 2026-11-01](#guard-wording-of-2026-11-01)), and
the Claude guard snapshot `readiness-claude.json`. Records written earlier
keep their earlier strings.

- **Private config schema v4.** The collector reads schema v4 of its private
  config, named by `--guard-config` or `A_FRONTIER_GUARD_CONFIG`. Its keys have
  neutral names and are described with the private config, and unknown keys
  are refused. The collector refuses earlier configs. The private v4 config
  holds the same values as the last v2 config. The private v1, v2 and v3
  configs are unchanged, the v1 configs still serve the sealed Test builds,
  and each receipt keeps recording the SHA-256 of the config it used.
- **Call-log field.** New attempt receipts carry the schema
  `slop_ninja_frontier_attempt_receipt_v2` and keep the joined call-log rows
  under `request_log`. Earlier receipts have no schema field and keep those rows
  under an older key, which the collector and the corpus-store adapter find by
  the SHA-256 of its name. Receipts are evidence, so they are not rewritten.
  `assemble` copies the rows into each record under its receipt's key, so
  re-assembling an earlier receipt gives the earlier record bytes, while new
  records carry `request_log`. The collector's `status` report and the
  `frontier_wave` adapter of `tools/corpus-store` read either key. The
  adapter records the logged model as the event parameter `call_log_model`;
  events stored before 2026-10-31 keep their earlier parameter name, and the
  public-candidate filter drops both.

Other changes for new runs only:

- **`inference/a-v6-cpu`.** On main, `modernbert.rs`, `reference_layer_norm.rs`
  and `trace.rs` carry the optional MKL and FP64 numeric profiles and f64 trace
  output. Their pins in `src/main.rs` and `UPSTREAM.json` were renewed after
  the fixed 41-case roster passed again on 2026-10-30
  ([requalification](../inference/a-v6-cpu/QUALIFICATION.md)).
- **RAID notes.** `grammar-a-pools/src/raid.rs` writes its generator `note` and
  domain `human_terms` strings into each RAID row and into
  `admission/rights.json`. `admission/rights.json` also records each domain's
  `generated_rows_basis`, which now states only that prompts carry the source
  title. New RAID runs write the edited strings; existing pool and rights
  records keep the earlier ones. No record binds the hash of `raid.rs`. The
  generator `terms` strings and the IMDb `human_terms` now name the terms
  only; new runs write the shorter strings and existing records keep the
  earlier ones.
- **FineWeb notes.** The rights `notes` of `grammar-a-pools/src/fineweb.rs` and
  `fineweb_blogs.rs` no longer carry the comparator remark. The protocol's
  FineWeb comparator-exposure note and the seal are unchanged. New runs write
  the shorter note; existing pool records keep the earlier one. No record
  binds these files' hashes.
- **Style library prompts.** `library::voice_invent_prompt` and
  `human_derived::describe_prompt` read the seed voice examples, and the
  sentence that introduces them, from the private file `SEED_EXAMPLES_FILE`.
  With that file present, both prompts are byte-identical to the earlier
  prompts. The ignored test
  `private_seed_examples_reproduce_the_earlier_prompt_bytes` pins the earlier
  prompts' SHA-256 for fixed inputs, and it passed on 2026-10-30. Without the
  file, a library session refuses to start.
- **Enron fixtures.** The unit tests of `grammar-a-pools/src/pile.rs` use a
  synthetic address, mailbox and phone number. The adapter is unchanged.
- **Prior registries.** The registries that name pools of removed adapters as
  overlap priors are inputs of earlier runs, and are unchanged. Their
  descriptions name the runs they served, and private admission records bind
  the hashes of several of them.

## Wording revision of 2026-10-31

Five files that the sections above, sealed evidence or private plans cover
changed again on 2026-10-31. Their earlier main bytes are retained privately.

| File | Earlier SHA-256 on main | SHA-256 on main | What binds the earlier bytes |
|---|---|---|---|
| `tools/a-comp-tierc/frontier-route-driver.sh` (omitted from the public copy) | `a735f86657ad32062957726c4de4d3ff22be64856609625c3419bc6a0ea6b289` | `ec5c7306aadc6cdc26ddcd0a5ce3deea525125bac9be8a1b03515b76d1929e12` | None; the seal binds the tip bytes `5ce98d6f…` |
| `examples/a_frontier_positives.rs` (omitted from the public copy) | `e723443cd2e7c47a634a87ba7a8f0c58051aa397920ad68312954ae074ed441e` | `35b57ea41d25e05e8087a78b98f7c0f7fcdc45c6bf414c3278591b3d71dd9e2f` | `producer_sha256` in plans made from main before 2026-10-31 (recorded, not compared) |
| `examples/a_frontier_positives/admission.rs` (omitted from the public copy) | `5916eba1a29688c6aa7785fc287b8e221f8fc1cd061cbf90593885fca290b52d` | `cfa855f647a2a590406fd4ae7eb1cb659907b83ea47e0daaa547cd33bc7e94ed` | `admission_source_sha256` in the scale-v1 `wave2` and `wave2b` plans (recorded, not compared) |
| `docs/a-competitive-protocol.md` | `a2febe6013ca361f115fcbabf9c50ce9e50297543b48348514f67d1dc4666257` | `a8b0a02f3b3960068e59b79cdbed29e9616884aba72ca648491df1b46fe66580` | Tier C `SEAL.json` `protocol.document_sha256` |
| `docs/a-hosted-output-admission.json` | `e402401ac102e681e80bf77f491356f18f9c60c64f7dade778dd30d8f35618ee` | `12688c28246c36b02def1c8f1f1e4a12edc1eac9358e1f890bf38f73815536c8` | `training_admission.manifest_sha256` of every frontier record collected before 2026-10-31, the sealed Test frontier exports, and the `hosted_admission` input of assembler bundles |

In the collector and the collection driver, comments, log and stop strings,
error messages, internal names and the two descriptive receipt strings
changed, and the operator-facing names were renamed: the private config schema
v3 and its keys, `--guard-config`, `A_FRONTIER_GUARD_CONFIG`, `guard-check`,
the snapshot file `readiness-claude.json` and its fields, and the driver's
`GUARD_CONFIG`, `PROVIDER_BACKOFF` and provider backoff file
`backoff/provider` (a backoff written earlier under its previous name is not
read). Collection, guard decisions and receipt fields are otherwise
unchanged. The protocol
document's budget wording and decision-record reference are neutral, and its
rules are unchanged; audit or reproduce a Tier C step against the sealed bytes.
The hosted admission manifest keeps its path, schema, cohorts, records,
conditions and identity policy. Its provider-terms note is neutral, and it
records the earlier digest as `predecessor`, which `a_hosted_admission check`
reports as `predecessor`. Plans that record their own admission basis keep it;
older plans (pilot-v1, smoke-v6, wave1) keep the predecessor digest, so
exporting any plan again reproduces its earlier records, and export manifests
cite their records' basis.

## Guard wording of 2026-11-01

The collector's per-call guard checks were reworded again. Three files that the
sections above or private plans cover changed. Their earlier main bytes are
retained privately.

| File | Earlier SHA-256 on main | SHA-256 on main | What binds the earlier bytes |
|---|---|---|---|
| `tools/a-comp-tierc/frontier-route-driver.sh` (omitted from the public copy) | `ec5c7306aadc6cdc26ddcd0a5ce3deea525125bac9be8a1b03515b76d1929e12` | `f17a3815f6d5e3471e1aeb159189b4124be49c2a33f66045e8d6adfcc0fd75b8` | None; the seal binds the tip bytes `5ce98d6f…` |
| `examples/a_frontier_positives.rs` (omitted from the public copy) | `35b57ea41d25e05e8087a78b98f7c0f7fcdc45c6bf414c3278591b3d71dd9e2f` | `011c0817ab5b22bab60ab53ad9508f4eb904e9062b83c1c86c31fb9befb10bcb` | `producer_sha256` in plans made from main before 2026-11-01 (recorded, not compared) |
| `examples/a_frontier_positives/eval_plan.rs` (omitted from the public copy) | `f671791925ab9decbe87e16342bed2d089682c33d740430ff4f1d777f5a8b85a` | `0af382f20f8fe7b20b9b0e505126c4342ad880cbeb6590cbf34c6f88c7b0dc3e` | `eval_plan_source_sha256` in evaluation plans made from main before 2026-11-01 (recorded, not compared); only a test fixture changed |

Private config schema v4 renames three schema v3 keys and adds one private
setting that was a tracked default before; the keys are described with the
private config. The private v4 config holds the v3 values under the v4 keys.
New runs write new stop reasons, a collect-log field for the CLI's rate
readings and two new `guard-check` fields, all listed in the private config's
description. Collection, guard decisions, exit codes and receipt fields are
otherwise unchanged. The driver changed only in comments and one log line.

## Protocol wording of 2026-11-02

The protocol's data policy and its OLMo-2 card review now state the admitted
and excluded terms and the card statements without an interpretation of them.
A second pass the same day reduced the Reddit-dump exclusion and the OLMo-2
publication rule to the recorded facts. Its rules, criteria and admission
decisions are unchanged.

| File | Earlier SHA-256 on main | SHA-256 on main | What binds the earlier bytes |
|---|---|---|---|
| `docs/a-competitive-protocol.md` | `a8b0a02f3b3960068e59b79cdbed29e9616884aba72ca648491df1b46fe66580` | `6bee53930913d2a9f43528e2c50cc5d61b60789de248be44afe919980eaea3fe` | None; the seal binds `a2febe60…` |
| `docs/a-competitive-protocol.md` (second pass) | `6bee53930913d2a9f43528e2c50cc5d61b60789de248be44afe919980eaea3fe` | `77267862d8a2e0cc4aec98e92fea930e3c7b01ce3a9d0cb5db074e138b5c22cf` | None; the seal binds `a2febe60…` |

## Commit aliases

[Mixed-int8 CPU serving](a-int8-serving.md) cites evaluator
`a_comp_evaluate-5915cd86` and pre-registration commit `2314d433`. Those
commits exist only in the local branch `a-comp-rc-oppoint`. Their rewritten
twins, with identical trees, are on `origin/a-comp-rc-oppoint` and
`origin/a-comp-tierc`:

| Cited commit | Alias | Tree |
|---|---|---|
| `5915cd869090a73b888a1acc0eda3404530903a2` | `4f914a84473bdec7add55a447eb318e1cf1a54ad` | `2eb6cf00f84eacf66cb2ec8e993df4b18d88f0d4` |
| `2314d4331881ea8f7a97b8725a19157132c8a83d` | `9a1f71b99ca9f3d334682c88a405a2024f3bb950` | `80346e2a580948a40ffcac81b11fe55bc484d218` |
