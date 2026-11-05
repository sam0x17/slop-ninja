# Dashboard data contract (`dashboard/v1`)

The public dashboard at `slop.ninja/dashboard/` reads a small set of JSON
documents that the publisher Worker (`subnet/publisher`, `src/dashboard.rs`)
writes on every run. This file is the contract between the publisher and
the website. The website's typed reading of it lives in its own repository
(`src/model.rs`, `src/clean.rs`), and the publisher's tests round-trip every
document through a copy of those types.

## Data path

1. The publisher builds each document from typed fields of the coordinator's
   and the eval Worker's status, the cohort index and the cohort summaries.
   It never copies a status response. It writes the documents under
   `dashboard/v1/` into a dedicated bucket that holds nothing else.
2. The dashboard feed Worker (`subnet/dashboard-feed`) serves those objects.
   It answers `GET` and `HEAD` on the exact paths below and nothing else. It
   has no route, no custom domain, no workers.dev name and no preview URLs,
   and its only binding is the dashboard bucket. It never writes, deletes or
   lists objects.
3. The website Worker reaches the feed only through a service binding. It
   checks every value again and re-encodes the document into its own types,
   so a field outside the contract never reaches the page.

The validator layout `v1/` is separate, and validators never read
`dashboard/v1`. Nothing in this path needs a public bucket, a custom domain
or a DNS change.

## Keys

`dashboard/v1/` followed by exactly one of:

| Key | Schema | Bound | Cache |
|---|---|---|---|
| `index.json` | `slop_ninja.dashboard.index/v1` | 64 KiB | 60 s |
| `m/<key>/now.json` | `slop_ninja.dashboard.now/v1` | 64 KiB | 60 s |
| `m/<key>/cohorts/index.json` | `slop_ninja.dashboard.cohorts/v1` | 64 KiB + 16,384 × 1 KiB | 60 s |
| `m/<key>/cohorts/<label>.json` | `slop_ninja.dashboard.cohort/v1` | 64 KiB + 4,096 × 4 KiB | 300 s |
| `m/<key>/months/<YYYY-MM>.json` | `slop_ninja.dashboard.month/v1` (not written yet) | the cohort list's bound | 300 s |

`<key>` is `a`, `b` or `c`. Only `a` is written today. `<label>` uses the
cohort label alphabet `[a-z0-9._-]`: 1 to 4,096 bytes, never `.`, `..` or
`index`. `<YYYY-MM>` is a UTC month from `01` to `12`. The grammar and the
bounds (rule `slop_ninja.dashboard.read_bounds/v1`) are defined in one
source file, `subnet/dashboard-feed/src/contract.rs`, which the feed and the
publisher both compile. Each bound follows from bounded fields: fixed
fields plus a few protocol strings for `index` and `now`, at most 4,096
entrant rows of 4 KiB each for a cohort, and 16,384 cohort rows of 1 KiB each
for the list. The publisher refuses to write a document over its bound and
raises an alert. The feed answers 502 for a stored object over its bound.

## Common header

Every document carries `schema`, `network` (`testnet`), `chain{genesis,
netuid}`, `generated_at` (unix seconds, floored to the minute) and
`sample: false`. Documents for a mechanism also carry `mechanism`.

## Rules every document follows

- **Allow-list.** Each kind has a fixed list of JSON paths. The publisher
  refuses a write with any other path, and the redaction scan
  (`redact::scan_value`) runs on every write. The scan allows two contract
  keys that it otherwise refuses by name: `kind`, only as a plan stage's
  kind with a closed value, and `results`, only as `cohort.results` with a
  boolean value.
- **Stage times are tempo boundaries.** A stage is shown as of the last
  tempo boundary the coordinator observed. Each phase is `{stage, epoch,
  at}`, where `epoch` is the boundary at which the stage was first seen. A
  change the engine makes in its boundary step (opening, admission window
  close, close) is seen at that boundary. Results published (`transition`)
  is the boundary of the cohort's `published_epoch`; the engine's log entry
  for the package commit, which falls inside a tempo, is never used for it,
  and until that boundary the cohort shows packaging. Any other change
  (work committing) is seen at the next boundary. `at` is that boundary's
  observed time to the minute. It is `null` when no one observed the
  boundary in sequence. Phases only move forward in the bar's order: a
  stage the engine returns to is not shown again. Probe builds and load
  cohorts are not part of the record.
- **Work progress is a percent on the grid 0, 10, ..., 90.** It is read
  once per tempo boundary from the coordinator's aggregate counts, by the
  publisher's first observation of a boundary that follows the previous one
  in sequence, saved before any document carries it, and kept until the
  next boundary. When the publisher's state is fresh or lost, or it missed
  boundaries, `work` is `null` until the next boundary. The cohort-fault
  pause classes follow the same rule. It is never 100: a work stage reaches 100% by
  ending. It appears only while the current stage is a work stage
  (`generating`, `scoring`). No document carries a done or total count, a
  per-kind or per-entrant figure, or the old bucket words.
- **Lower bounds only.** Every `expected.*` value and every
  `plan.stages[].earliest_*` value is the earliest possible boundary,
  credit-adjusted. It is never behind the next boundary. An epoch converts
  to a block by
  `boundary_block + (epoch - boundary_epoch) * (tempo_blocks + 1)`.
- **No score before quantization.** Until the coordinator publishes
  quantized scores `q` under a quantization block, every score, band edge,
  cutoff value and `quantization` field is `null`. The quality chart then
  shows bands only.
- **No rank.** Entrants are listed in roster order, which admission fixes
  before any document is scored. No document carries a rank or any
  within-band order.
- **Never in `dashboard/v1`:** anything per document or per task, receipts,
  outcome classes, task indices, exact work counts, exact scores, near-copy
  statistics, fingerprints, strata, fault codes, notes, step errors, spend,
  budgets, tokens, model names, lanes, hosts, keys and template hashes.

## `index.json`

```json
{ "schema": "slop_ninja.dashboard.index/v1", "network": "testnet",
  "chain": { "genesis": "0x…", "netuid": 601 }, "generated_at": 1793000040, "sample": false,
  "mechanisms": [
    { "key": "a", "name": "Detection", "state": "live", "chain_mechanism": 0, "stage_set": "cohort/v1" },
    { "key": "b", "name": "Rewriting", "state": "planned", "chain_mechanism": null, "stage_set": null },
    { "key": "c", "name": "Shared grammar", "state": "planned", "chain_mechanism": null, "stage_set": null } ] }
```

## `m/a/now.json`

- `clock`: `confirmed_block`, `confirmed_epoch`, `age_secs` (age of the
  coordinator's chain observation), `boundary_block`, `boundary_epoch`,
  `tempo_blocks`, `block_secs_nominal`.
- `pause{paused, class}`: `class` is one of `coordinator_silent`,
  `waiting_on_infrastructure`, `operator_hold`, `observation_lag` or
  `calendar_paused`. The cohort-fault classes are read at boundaries.
- `cohort`: `cohort`, `label`, `stage`, `phases[]`, `tempos{credited,
  required}` (schedule stages only), `expected{stage_end_epoch,
  stage_end_block, close_epoch, close_block, packaging_deadline_epoch}`,
  `progress{admission{admitted, roster_cap}, work{stage, pct,
  as_of_epoch}}`, `plan{template{k_tempos, admission_tempos,
  transition_tempos, work_window_tempos}, stages[{stage, kind,
  expected_tempos, earliest_epoch, earliest_block}]}`, `results`, `mode`.
  `stage` is one of the eight bar stages (`admission_open`,
  `admission_closing`, `generating`, `sealing`, `scoring`, `packaging`,
  `transition`, `closed`) or `queued`, `ended`, `idle`, `unknown`. Each bar
  stage has a fixed kind: `schedule` for admission open and results
  published, `work` for building and scoring, `step` for closing, sealing
  and packaging, and `end` for closed.
- `next{cohort, earliest_open_epoch, earliest_open_block, admission_tempos}`.
- `best{index, label, since_cohort, since_label, owner{uid, hotkey},
  latest_score, latest_score_label, quantization, artifact{state, download},
  licence}`: the current reference `R_k` and the cohort whose promotion
  made it.
- `latest_cohort`, `notice` (kinds only: `testnet`, `burn`, `target`,
  `operator`), `advisory` (always `null`), `sources_ok`.

A cohort in its admission window, with every boundary credited:

```json
"cohort": { "cohort": 7, "label": "testnet-r0-c7", "stage": "admission_open",
  "phases": [ { "stage": "admission_open", "epoch": 100, "at": 1790433060 } ],
  "tempos": { "credited": 10, "required": 16 },
  "expected": { "stage_end_epoch": 116, "stage_end_block": 8041876, "close_epoch": 122,
                "close_block": 8044042, "packaging_deadline_epoch": null },
  "progress": { "admission": { "admitted": 12, "roster_cap": 40 }, "work": null },
  "plan": { "template": { "k_tempos": 22, "admission_tempos": 16, "transition_tempos": 2,
                          "work_window_tempos": 4 },
            "stages": [ { "stage": "admission_open", "kind": "schedule", "expected_tempos": 16,
                          "earliest_epoch": null, "earliest_block": null },
                        { "stage": "admission_closing", "kind": "step", "expected_tempos": null,
                          "earliest_epoch": 116, "earliest_block": 8041876 },
                        "…",
                        { "stage": "closed", "kind": "end", "expected_tempos": 0,
                          "earliest_epoch": 122, "earliest_block": 8044042 } ] },
  "results": false, "mode": "burn" }
```

## `m/a/cohorts/index.json`

`cohorts[]` lists rows in ascending order: `cohort`, `label`, `state`
(`closed`, `ended` or `retired_unpublished`), `opened{epoch, at}`,
`admission_closed`, `published`, `closed`, `mode`, `promoted{uid, hotkey}`,
`entrants`, `reference_index`, `best_score`, `quantization`, `leaderboard`
and `ended_class`. A cohort appears once the publisher lists it. An ended
cohort appears once its end is visible at a boundary. The list also carries
`records[{first_label}]` for earlier chain records and `record_prefix`.
`mode` and `ended_class` are `null` for now.

## `m/a/cohorts/<label>.json`

The document carries the cohort's row, `phases[]`, and `plan` without the
earliest fields. While the coordinator's status is unreadable, the cohort
list and the cohort documents keep their last version. A closed cohort with its summary also carries:

- `entrants[{uid, hotkey, score, band, eligible, row_share_pct,
  gates_passed, promoted, artifact{state, download}}]` in roster order.
  `band` is reward-v1's `kappa` (1 is the top occupied band). It is set
  exactly when `eligible` is true. `gates_passed` means every enforced gate
  and the public-copy gate passed.
- `quality{cutoff, bands[{band, lower, upper}]}`: the occupied bands
  `1..N`, without edges for now.
- `reference{index, label, score}`, `predecessor`, `score_better`,
  `promotion_rule` (`testnet_no_canary` on the testnet record), and the
  `previous` and `next` labels.

`score` and `row_share_pct` are `null` for now. An ended cohort carries its
phases only. `entrants` and `quality` appear only for `published` and
`closed` cohorts. The publisher writes `published` documents for the
current cohort only after it can verify the cohort's records before
closure, which it does not do yet, so `cohort.results` stays `false` until
the cohort is listed.

## Inputs from the coordinator (E1)

The publisher reads these from `/eval/status`, behind its status token. A
coordinator that does not report a field leaves the matching dashboard
value `null`.

- Per cohort: `opened_block`, `packaged_epoch`, `closed_epoch`,
  `published_active`, `roster_cap`, `self_clearing`, `window_closed_epoch`
  (the boundary its admission window closed at), whether `reward_v1` is
  present, `reward_v1.published_epoch` and `reward_v1.probe_build`, the
  phase `log[]` (its notes are never
  parsed), and the aggregate `progress{generation{ready, total},
  scoring{done, total}}`. The counts stay private: only the grid percent
  is published.
- `calendar.current`: `opened_block`, `window_closed_epoch`,
  `earliest_cutoff_epoch` and `earliest_close_epoch_credited`.
  `calendar.template.paused`.
- `observed.epoch.last_epoch_block`, `observed.epoch_live`, and
  `admission.cohorts[].admitted`.
