# Testnet plumbing evaluation manifest

**Testnet-only shortcut (`testnet_plumbing_only`).** Testnet cohorts score *A*
artifacts on a rotating sample of Dev-iter rows. Dev-iter is an iteration split,
exposed by design and favourable to the release candidate, so these scores test
the evaluation plumbing (dispatch, receipts, Brier, score packages) and are not
evidence of model quality. **Removed by:** mainnet hidden evaluation on fresh,
unexposed cohorts. Dev-iter is never an evaluation source outside testnet.

`slop_ninja_a_testnet_eval_manifest` selects rows from files named in the tracked
allow-list
[`allowlists/testnet-plumbing-dev-iter-v1.json`](allowlists/testnet-plumbing-dev-iter-v1.json),
by exact repository path and SHA-256. It writes a manifest of row ids, text
digests and labels under `data/`. The rows and the manifests stay in ignored
storage; nothing is uploaded.

## Refusals

The tool fails closed. It refuses:

- **By path, before opening anything:** any path not under `data/`, not
  normalized, or with a component whose tokens name Dev-select, CAL, Test,
  sealed or Tier C. Examples are `dev-select`, `dev_select`, `cal`,
  `cal-subsample`, `devit-cal`, `test`, `eval-humans-test`, `tier-c`, `Tier_C`,
  `tierc` and `tier/c`. A legitimate name that contains one of these tokens is
  refused too.
- **After resolving symlinks, before opening anything:** the same rules apply
  to the real path, which must also stay under `data/`, and to every component
  of the canonical repository root. `data` itself must be a real directory, not
  a symlink.
- **By allow-list entry:** a split other than `dev_iter`, a purpose other than
  `testnet_plumbing_only`, or a bundle manifest outside the rows' directory.
- **By the bundle:** a `SEALED*` or `UNSEALED*` record beside the rows; a bundle
  manifest whose digest differs, or that is not an unsealed `dev_iter` split
  with Test unopened; a rows file whose digest or row count differs.
- **By row:** any row whose split is not `dev_iter`, whose text digest differs,
  or whose origin, share and generator label disagree.

The tests cover each refusal with synthetic bundles, and always give the
refused split a correct digest and `dev_iter` row labels. They include a
symlinked split directory, a symlinked `data/` and a repository root under Tier
C, and absent Tier C paths, which are refused without being opened. A rows file
edited after allow-listing, with its bundle manifest unchanged, is refused by
its own digest.

## Selection

Rows are filtered to the allow-listed populations (`primary` for Dev-iter) and
grouped into strata by (domain, origin). Within each stratum, rows are ranked by
SHA-256 over a domain separator, a 32-byte seed, the cohort number, the source
index and the row id. Strata then take turns, in key order, each taking its next
candidate whose source group has fewer than `--max-per-source-group` rows (default 1) in
this cohort. They stop at `--per-stratum` rows. The same inputs always give the
same manifest, and another cohort rotates the sample. Shortfalls are reported
per stratum.

The manifest (`slop_ninja_testnet_plumbing_eval_manifest_v1`) records the
allow-list digest, every source's path and digests, the seed's SHA-256 (never
the seed), the selection parameters, per-stratum counts, and for each selected
row its line, id, `text_sha256`, origin, share, domain, generator label and
source group. It never contains text. An existing manifest is never replaced.

## Use

```sh
cargo test --locked --manifest-path tools/a-testnet-eval-manifest/Cargo.toml
cargo build --release --locked --manifest-path tools/a-testnet-eval-manifest/Cargo.toml
slop_ninja_a_testnet_eval_manifest build --repo . --seed-hex HEX64 --cohort N \
  --per-stratum 4 --out data/task-roots/testnet/eval-manifests/cohort-N.json
```

`--allowlist FILE` replaces the tracked allow-list; the same refusals apply. The
command prints counts only.

On October 29, 2026, the tracked allow-list resolved to the Dev-iter split of
bundle `18b3682973982a3f`: 2,873 rows, 1,664 of them in the primary population,
across 7 domains × 3 origins. A run with 4 rows per stratum selected 84 rows
with no shortfall. The same run against that bundle's CAL split was refused by
path.

## Hidden human pool

`pool` exports every allow-listed `human_only` row as the never-reused testnet
human pool, with the same refusals:

```sh
slop_ninja_a_testnet_eval_manifest pool --repo . --pool-id testnet-human-pool-v1 \
  --out-dir data/task-roots/testnet/m4-eval/pools/testnet-human-pool-v1 --shard-rows 64
```

It writes a new directory under `data/`: JSONL shards
(`slop_ninja_testnet_human_pool_row_v1`, with text) and `manifest.json`
(`slop_ninja_testnet_human_pool_v1`: row ids, text digests, domains, source
groups and shard digests, never text). The operator uploads both privately to
the eval bucket; the cohort engine draws whole source groups and records every
used row and group durably so none is drawn twice. On October 30, 2026 the
tracked allow-list gave 570 rows in 505 source groups across 7 domains, in 9
shards.

### A token bound for an existing pool

Under the seeded task order the eval Worker draws only from a pool whose
manifest records a per-row token bound inside the scorer's 2,048-token
contract (`selection.max_tokens`). Pool v1 records none. `pool-token-bound`
writes a manifest for the same pool id and the same shard files with the
rows of at most `--max-tokens` scorer tokens (default 1,000, the
public-candidate bound, so a mixed rewrite stays inside the contract):

```sh
slop_ninja_a_testnet_eval_manifest pool-token-bound \
  --pool-dir data/task-roots/testnet/m4-eval/pools/testnet-human-pool-v1 \
  --max-tokens 1000 --out data/.../manifest-tb1000.json
```

It checks every shard and row by digest, counts each row with the scorer's
compiled-in count (`slop-ninja-scorer-tokens`, `[CLS]` and `[SEP]`
excluded), keeps the original shard list, recomputes the counts, and records
the bound, the count rule, the longest kept row, the rows excluded and the
digest of the manifest it was derived from. It refuses a manifest that
already records a bound and never replaces an existing file. The new
manifest is uploaded beside the original's shards, so a plan can name it
with no shard upload.

## Pool v2: the testnet public stock

**Testnet only (`testnet_public_stock_only`).** Pool v2 replaces the Dev-iter
pool with the unassigned remainder of the evaluation reservations behind the
sealed Test: dated public text, all historical-human proxies. Every document
can be recognized, so this is a stated pilot limit and the stock is never a
mainnet evaluation source.

`stock-v2` reads only files named in
[`allowlists/testnet-public-stock-v2.json`](allowlists/testnet-public-stock-v2.json)
(`slop_ninja_testnet_public_stock_allowlist_v2`), each by exact path and
SHA-256, zstd-decoded when it ends in `.zst`:

- **Group reservations:** `reserved-unassigned/groups.jsonl` files. Every row
  must be a `reserved_unassigned` split group with one family key per unit.
- **Index reservations:** a reservation index whose remainder is not written
  as groups (Wikipedia). The remainder is the index less every evaluation
  assignment the gate lists for that domain and less the reservation's own
  overlap exclusions, and it must equal the count the reservation recorded.
- **Unit URLs** of reserved units, for the gate's URL check.
- **Unit titles:** `title_sha256` of reserved wiki units (Wikibooks,
  Wikivoyage, Wikisource, Wikinews, and the Wikipedia index itself), for the
  RAID title screen.

Stock paths pass the Dev-iter name refusals above. The sealed Test's group
records and acquisition summary are read for rules and counts only: the cap
per group, the per-unit yield, the realized yield and the domain mass. Those
references may name Test, CAL or Dev-select paths and must lie under `data/`.
Every assignment record (sealed Test, evaluation) is refused if it carries a
prose-named field (`text`, `excerpt`, `passage`, `body`, `content`, `prose`) or
any string longer than 63 words, the longest string measured in the
allow-listed assignment files. The refusal names the field, never its content.
The reservations hold no prose.

**Expected documents.** Each stock group follows the Test's excerpt rules:
the Test cap per group, and the smallest per-unit yield among the domain's
uncapped Test groups. If every Test group of a domain is capped, a stock group
with at least as many units as the smallest capped Test group is capped too,
and a smaller one counts zero. Capacity scales each domain by the yield the
Test's own acquisition realized on the groups it processed, never above 1. The
yield margin (`poisson_lower_wilson_hilferty_v1`) is the one-sided 95% lower
bound of that realized count.

**Completeness.** The allow-list pins the split set's `SHA256SUMS`. Every
split manifest, group, family and blocked-group file it lists must be a gate
reference, a stock file, a Test rule file or a declared parent reservation
(`not_gated`, with a reason), each with the digest `SHA256SUMS` records. Text
files (`documents.jsonl`) are covered by the manifest beside them. Every gate
reference records its row count, and a different count refuses. Every role is
required.

**Reservation gate.** The stock's group keys, family keys, normalized FineWeb
sites, reserved-unit URLs, unit titles, SciPost submission ids and (once rows
are acquired) text digests are indexed. Every reference is streamed against
them. Keys are compared by the first 16 bytes of their SHA-256, so a Test
`contributor_key_sha256` matches the group key it hashes. JSON references are
read field-blind: every string of every record is tried as a key, as a site
when it carries a site prefix (`fineweb_site:`, `fineweb_blog:`, `cc_site:`,
`cc_host:`), and as a hashed key and text digest when it is a 64-digit hex
digest. A row keyed only by `split_group_key`, or a site under another prefix,
is found without a schema change. Hosts are normalized: lowercase, no port or
trailing dot, and no leading `www.` or `m.`.

- **Hits that refuse the stock**, because the split construction excludes
  them by design:
  - `train`: the splits-v5 Train and Train-mining manifests;
  - `sealed_test`: Test groups, families and reservation manifests, and the
    Test acquisition metadata;
  - `evaluation`: CAL, Dev-select and Dev-iter groups, families and
    manifests, and the two diagnostic slices.
- **Hits that exclude the whole stock group**, with a content-free receipt in
  `exclusions.jsonl` naming only the checks that hit that group:
  - `train_held`: the held Train-side manifest, and the FineWeb-blog writers
    the evaluation reservation blocked;
  - `r0_training_lineage`: the reference model's trainer bundle
    (`18b3682973982a3f`, every non-text string and digest), and screen S1 of
    the pre-registered RAID lineage screen, applied here from title hashes
    (`raid_train_title_lineage`). The allow-list binds the screen record. The
    title list and bundle rows must be its inputs by digest. Allow-list v3 can
    add the active reference's bundle as `reference_training_lineage` (see the
    Test-2 carve section);
  - `cc_mainnet_stock`: WARC record id, normalized URL, host, registered
    domain or the row's own `cc_site:`/`cc_host:` group;
  - `public_candidate_v1`: keys or digests.
- **Common Crawl rows on declared source hosts.** A row on a declared source
  host of a stock domain (the host or any parent domain; the host is taken from
  the URL when the row has none) is mapped to a stock key by the host's URL
  rule: `openreview_forum_url_v1`, or `scipost_submission_url_v1`, where
  `/submissions/<id>` names the submission and `/contributor/<n>` is a profile
  page. A row that no rule resolves refuses the stock.
- **Mirror hosts.** Rows on declared mirror hosts (court-opinion, article and
  book mirrors) are counted for the text-level check at acquisition. Keys
  cannot clear them.

**Acquisition screens.** The manifest lists what acquisition must still apply
before any row is admitted:

- RAID screens S2 (Reddit author lineage) and S3 (text lineage);
- folded 12-token windows and folded 50-word sequences against Train-side
  text, the trainer bundle, the Common Crawl rows and the public-candidate
  texts, besides exact digests;
- the mirror-host comparison;
- the 2,048-token length filter under the R_k tokenizer;
- the two-sentence parent count;
- a re-run of this gate on the rows' text digests.

**Domain weights (`test_mass_waterfill_v1`).** The base is the sealed Test's
human documents per domain. Domains with no stock are retired. A domain whose
stock cannot last the horizon is held to the largest weight its stock sustains
for the whole horizon, checked by simulating its demand. The other domains
share the remainder in proportion to their base. The weights are filled on the
lower-bound capacity, so a realized yield at that bound still lasts the
horizon. The manifest records the weights, the retired and reduced domains and
the total-variation change of the estimand. Sigma, delta and w are re-derived
for these weights before live rewards.

**Capacity (`public_stock_capacity_v2`).** A cohort-by-cohort simulation of a
per-domain passage ledger:

- Negatives are fresh and scored once.
- Under `rewrite_source_ledger_v1`, a mixed child's parent is, in order, one of
  its own cohort's negatives, an earlier passage eligible again (oldest first),
  or fresh stock used only as a parent. A passage serves at most `K` times, with
  uses at least `G` cohorts apart.
- `never_reused_one_child_v1` is the live engine's rule: one child per
  same-cohort negative, and nothing is drawn twice.
- Quotas follow the weights with a carried remainder, so each domain stays
  within one document of its weight.
- A binding domain is retired and its weight spread over the other domains.
- The probe is one batch drawn first, whose humans are never drawn again. With
  `--probe-versions per-reweight`, every reweighting draws a new probe version.
- Lookup-canary audits drawn from the stock (`--canary-report`, or reserved in
  the weights with `--plan-canary`) take `docs` fresh human documents per
  audit. A promotion candidate takes one audit, or two after a first flag.
- A cohort counts only when its batch and every draw scheduled in it fill.

Human demand does not depend on the roster, because every entrant scores the
same batch and probe. Shortfall tolerance is zero. Documents are counted one by
one. The engine draws whole source groups, so the report gives, per domain, how
many cohorts of quota its largest group holds. Tests compare the bucketed ledger
with a passage-level reference.

**Output.** A new owner-only directory under `data/` holding:

- `manifest.json` (`slop_ninja_testnet_human_pool_v2`, stage `reservation`);
- `groups.jsonl` and `exclusions.jsonl`;
- `gate.json` and `capacity.json`.

The v1 engine reads only `slop_ninja_testnet_human_pool_v1` and refuses this
manifest. Text shards, row digests and the text-level gate come from
acquisition under the same allow-list. Nothing is uploaded.

```sh
slop_ninja_a_testnet_eval_manifest stock-v2 --repo . --pool-id testnet-human-pool-v2 \
  --out-dir data/task-roots/testnet/human-stock-v2/pools/testnet-human-pool-v2 \
  --n 1000 --mix 20/55/25 --reuse ledger:4:7 --probe-docs 1024 --horizon 365 \
  --report-horizons 90,180,365,730 --engine-mix 40/40/20 --roster 40 --cohorts-per-day 1 \
  --probe-versions per-reweight --canary-report 20000:2:1,7,30,90,365
```

On October 31, 2026, the build took 127 s and 1.2 GB of memory.

**Stock and gate.**

- The tracked allow-list gave 1,864,491 reserved groups: 334,822 written
  groups and a Wikipedia remainder of 1,529,669 pages. That remainder is the
  1,585,894-page index less 56,158 evaluation pages and 67 overlap exclusions.
- All 47 split files in `SHA256SUMS` were accounted for: 39 gate references,
  3 stock files, 3 Test rule files and 2 parent-reservation files.
- No key or hashed key was shared with these split files:
  - Train: the Train and Train-mining manifests, 2,714,013 rows;
  - the sealed Test: groups, families, reservation manifests and acquisition
    metadata;
  - CAL, Dev-select and Dev-iter: groups, families and manifests;
  - the diagnostic slices.
- Text digests were not compared, because the stock holds no text yet.
- The gate excluded 7,080 groups:

  | Reason | Groups |
  |---|---|
  | Common Crawl pilot stock (416,492 rows): a page on the group's site | 6,862 |
  | of which a page also has a reserved unit's URL | 20 |
  | The trainer bundle behind R_0: a RAID Train title (S1) | 194 (188 Wikipedia, 6 Wikisource) |
  | Held Train-side rows by the writer, or a blocked writer, not already excluded by Common Crawl | 24 |

  No WARC record id, trainer-bundle key or public-candidate key matched, and
  no row sat on an unresolved source host.
- Common Crawl rows on mirror hosts, pending the text check:
  - law.cornell.edu: 102;
  - justia.com: 56;
  - leagle.com: 3;
  - ncbi.nlm.nih.gov (PLOS): 48;
  - archive.org: 39;
  - bartleby.com: 13.

**Size.**

- 1,857,411 groups remain, with 233,376 expected documents. At the Test's
  realized yield that is 221,280 documents; at its 95% lower bound, 197,692.
- Four Test domains have no remainder and are retired: Federal Register,
  historical manuscripts, spoken transcripts and student essays.

**Capacity at n = 1,000, 20/55/25, K = 4, G = 7 and a 1,024-document probe,
with a new probe version after each reweighting:**

| Scenario | Fresh stock per cohort | Days before a domain binds | Days with reweighting |
|---|---|---|---|
| Manifest weights, horizon 365, lower-bound yield | 550 for cohorts 1-7, then 200 | 365 | 956 |
| Manifest weights, realized yield | 550 for cohorts 1-7, then 200 | 368 | 1,062 |
| Manifest weights, metadata yield | 550 for cohorts 1-7, then 200 | 368 | 1,122 |
| Stock-proportional weights | 550 for cohorts 1-7, then 200 | 1,090 | 1,090 |
| Same, metadata yield | 550 for cohorts 1-7, then 200 | 1,150 | 1,150 |
| Raw Test mass | | 0 (Wikinews) | 1,058 |
| Live engine rule, 40/40/20 | 400 | 552 | 552 |
| Live engine rule, 20/55/25 | | infeasible: 205 of 563 probe parents | |

The probe takes 563 fresh documents: 205 negatives and 358 parents. The
horizon-365 weights move the estimand by a total variation of 0.540 from the
Test mass. Other horizons move it by 0.424 (90), 0.494 (180) and 0.604 (730).

**Lookup canary from this stock.** The weights reserve no canary audit. The
design's audit draws 20,000 fresh documents, about 100 cohorts of steady
demand, and a first flag draws a second. Days at the manifest weights and the
realized yield, by how often a promotion candidate is audited:

| Candidate every | Days before a bind, 1 audit | Days with reweighting, 1 audit | Days before a bind, 2 audits | Days with reweighting, 2 audits |
|---|---|---|---|---|
| 1 cohort | 3 | 9 | 1 | 4 |
| 7 cohorts | 27 | 69 | 13 | 34 |
| 30 cohorts | 89 | 246 | 59 | 119 |
| 90 cohorts | 179 | 534 | 166 | 359 |
| 365 cohorts | 364 | 868 | 364 | 729 |

A testnet canary drawn from pool v2 at the design size is therefore
sustainable only at low promotion rates. The canary's size and source on
testnet are a policy choice (D12b). `--plan-canary` refills the weights for a
chosen rate.

All figures assume one cohort a day, and that every group yields a parent with
at least two sentences. They also assume a realized yield no lower than the
bound, before the length filter and the text-level screens. The per-domain
lumpiness is in `capacity.json`. Global Voices' largest group holds about 49
cohorts of its quota, Project Gutenberg's about 4 and Wikinews' about 2. The
engine therefore needs a carried quota to keep these domains on their weights.

## Test-2 carve and mock reserve (`r2_test2_carve_v3`)

Round 2 of the A comparison takes its sealed Test-2 T humans, and a reserve for
the Test-2 mock cohorts, from the same Test-reservation remainder as pool v2
(protocol A2 §5.3, §12.5 and §14.2; amendment 1 items 1.4, 1.5, 1.11 and 1.16;
amendment 3). The carve is computed before anything is acquired from pool v2,
so the testnet and the mock share one estimand: the re-waterfilled weights
W_A8.

**Frame and units.** The carve reads a reservation-stage pool v2 directory
(`manifest.json`, `groups.jsonl`, `exclusions.jsonl`, `capacity.json`, each
checked against the manifest's digests) and the allow-list it was built from.
It draws only from the groups the reservation gate kept. Every target and
every mock demand is counted in the pool's lower-bound documents: a selection
holding `u` expected micro-documents counts floor(u / 10^6 ×
realization_lower), as the capacity report does. The pool's manifest weights
must first reproduce exactly from its own groups and plan, or the tool
refuses.

**Keyed order.** Each carved domain's groups are ranked by
HMAC-SHA-256(K_test2, `slop_ninja_a2_test2_carve_v3:` + domain + `:` +
group_key) over the UTF-8 bytes, compared as 32-byte big-endian integers,
ascending. The key is a 32-byte owner-only file in an owner-only directory. Its
SHA-256 must equal both the commitment given on the command line and the one
A6 logged in the freeze log. Neither the key nor any rank is written or
printed.

**Rule.**
- **Test-2 T** is the shortest keyed prefix holding the domain's target in
  lower-bound documents and at least 20 groups: legal opinions 450, Wikipedia
  400, Reddit, Ubuntu IRC, Wikipedia talk and FineWeb blogs 300, FineWeb 220,
  and Wikisource, PLOS, instructional and peer review 195 (150 humans in 15
  groups at the inherited 1.3 buffer, both counts buffered).
- **The reserve** continues in the same order until the domain's carve plus
  reserve covers its demand for 58 mock batches of 1,000 at 20/55/25 under
  `rewrite_source_ledger_v1` (K 4, G 7, no probe). Batches 1-7 take 550 fresh
  humans and later ones 200, 14,050 in all. The first sizing uses the manifest
  weights renormalised over the carved domains.
- **Surplus bound.** A domain that does not bind at the horizon (H = 365)
  under the manifest in force must keep its carve plus reserve within its
  surplus there. Domains that bind at the horizon are reported as accepted for
  the re-waterfill.
- **W_A8** is `test_mass_waterfill_v1` at H on the stock less the carve and
  the reserve. **AR-8** is re-run on it: at least 90 days before the first
  bind at the lower-bound yield, and no newly retired domain.
- **Coverage.** The reserve is checked against W_A8's mock demand. If any
  domain's demand rises above its reserve, a second iteration extends the
  reserve by the same rule and re-waterfills. Both iterations are recorded.
- **TV.** The record gives the total variation from the manifest weights,
  over all domains and over the carved domains, for the full carve and for the
  Test-2 T carve alone, and W_A8's change from the Test mass.
- **Groups.** The consumed Test's own groups are checked absent by group id,
  over its whole assignment (a superset of its unreached groups); the
  reservation gate already refuses a stock that shares a group with those
  files. The record gives the group-id digests of that set and of the
  reservation remainder, which includes the Wikipedia index remainder.

On failure (AR-8, coverage after the second iteration, the surplus bound, or a
domain that cannot fill), nothing can be applied. The record names the binding
domains and two values from a descending search, each computed by the full
procedure at that B_res′: the largest B_res′ ≤ 58 at which AR-8 passes, and the
largest at which every criterion above passes.

```sh
slop_ninja_a_testnet_eval_manifest carve-v3 --repo . --pool-dir data/.../testnet-human-pool-v2 \
  --key-file data/.../keys/K_test2 --key-commitment HEX --freeze-log data/.../FROZEN-A2.json \
  --protocol-sums data/.../splits-v4/SHA256SUMS:HEX --out-dir data/.../a8-draft [--allowlist FILE]
```

It writes a new owner-only directory holding `CARVE-A2.draft.json` (status
`computed_draft`) and `carve-groups.jsonl`. `--protocol-sums` is required: the
protocol's split set must list the same digests for the consumed Test's group
files and the reserved-unassigned files the allow-list reads. The command
prints counts, digests and the verdict only.

**The freeze log.** Every command that reads `FROZEN-A2.json` first verifies
its hash chain on the file's own bytes. The file just before entry k was
appended is its bytes up to the end of entry k − 1, followed by the bytes after
the last entry. From the first entry that carries `prev_sha256`, every entry
must carry one and a `utc`, and each must match. `carve-v3-log-check --repo .
--freeze-log FILE` runs that check and prints counts and stamps only. The tool
reads these entry fields:
- `commitments_sha256.K_test2`: A6's commitment. Exactly one entry may hold it.
- `carve_draft_sha256`: the draft's digest (S0-5).
- `user_decisions`: an object mapping U-item ids to `confirmed` or
  `overridden`. The last decision logged for an item governs.
- `test2_t_groups_sha256`: the seal's sealed Test-2 T membership file.
- `test2_mock_used_groups_sha256`: the carve groups the Test-2 mock used,
  logged with the mock's sealed journal.
- `releases_carve` and `release_condition`: a release amendment.

**Apply (F0b).** `carve-v3-apply` refuses unless the chain verifies, an entry
logs the draft's digest as `carve_draft_sha256`, and user decisions are logged
for U1, U2, U16 and U17 with U1 confirmed. It checks all three output paths
before writing anything: each parent must exist under `data/`, the names must
pass the name refusals, and nothing may exist there yet. It recomputes the
draft from the inputs the draft records and requires the same result. It then
rebuilds pool v2 under allow-list v3, which is the v2 allow-list plus a
`carve_exclusion` naming the groups file, the draft and W_A8. The build refuses
before writing anything if its own weights differ from the record's. Carve
groups get no group line and no receipt in the rebuilt pool; its manifest and
gate report give counts by domain and role. The command then writes allow-list
v3, a copy of it in the pool directory, and `CARVE-A2.json` with status
`applied`. Allow-list v3, the groups file and the carve records name sealed
groups. They stay in ignored storage and are never tracked.

```sh
slop_ninja_a_testnet_eval_manifest carve-v3-apply --repo . --draft data/.../CARVE-A2.draft.json \
  --draft-sha256 HEX --freeze-log data/.../FROZEN-A2.json --allowlist-out data/.../allowlist-v3.json \
  --pool-out data/.../testnet-human-pool-v2-wa8 --record-out data/.../CARVE-A2.json [--pool-id ID]
```

**Release.** Unused reserve groups return to pool v2 only after the Test-2 mock
is built. `carve-v3-release` takes the applied record, the allow-list v3 in
force and the pool built from it, the sealed Test-2 T membership and the
mock's used-group file (JSONL rows with a `group_key`). Both group files must be
the ones the freeze log binds. The log must also hold an entry whose
`releases_carve` is the record's digest and whose `release_condition` is one of
`effective_cohort_closed`, `replacement_lapsed` or `u17_failed`. Every group of
the sealed membership stays excluded, used by the mock or not; it must hold
every carved Test-2 T group, and only legal's reserve may have moved into it at
Stage 3. Groups the mock used stay excluded too. The re-waterfill runs on the
stock in force, after every current exclusion including the active
reference's lineage, plus the released groups. The command writes the
successor groups file, a release record with the new weights, their TV from
W_A8 and the AR-8 re-run, and a successor allow-list v3. That allow-list is the
one in force, active reference and all, with the successor carve exclusion.
Pool v2 is then rebuilt under it; the build refuses unless its weights are the
release record's. If the rebuild fails, the new release directory is removed.
The mock's evidence stays at W_A8.

```sh
slop_ninja_a_testnet_eval_manifest carve-v3-release --repo . --record data/.../CARVE-A2.json \
  --record-sha256 HEX --allowlist data/.../allowlist-v3.json:HEX --pool-dir data/.../POOL \
  --test2-t data/.../test2-t-groups.jsonl:HEX --mock-used data/.../mock-used.jsonl:HEX \
  --freeze-log data/.../FROZEN-A2.json --out-dir data/.../a8-release --pool-out data/.../POOL-RELEASED
```

**The active reference's lineage.** Allow-list v3 may name an
`active_reference`: an id, its trainer bundle and its own pre-registered RAID
lineage screen. Its trainer-bundle rows and title list are gate references of
role `reference_training_lineage`, bound to that screen exactly as
`r0_training_lineage` is bound to R0's screen. Hits exclude whole groups with
receipts. R0's lineage stays gated. When a later reference replaces R_0, the
pool is rebuilt with that reference's bundle. If its exclusions would change
any weight, the build refuses and the replacement waits for an amendment. A hit
on a carve group refuses the build with its own message: the reference's
training lineage then touches Test-2 T or the mock reserve.

## Replay of the v1 draws

`replay-v1` checks the cohorts the coordinator drew from
`testnet-human-pool-v1` against the content-free record in
[`fixtures/v1-draws.json`](fixtures/v1-draws.json).

- From counts alone, the domain quotas are the engine's largest-remainder
  split of the rows still available, so the seed is not needed.
- With `--pool-dir`, it runs the engine's own `pool::draw` and sentence
  splitter on the real manifest and shards, under a stand-in seed.
- Group counts depend on the coordinator's private seed and are not replayed.

The engine files are frozen copies, byte for byte, of
`subnet/coordinator/src/eval/{pool.rs,textgen.rs}` at commit `9051a52a`, which
drew rehearsal 3 and the shortfall drill. Rehearsals 1 and 2 drew on earlier
deploys; their nearest commit, `d456f5d2`, has the same manifest parser, rank,
proportional and draw code. The record names the commit and both files'
SHA-256, and both replays refuse when the frozen copies differ. Later engine
changes cannot alter what the replay tests. The frozen files' own 8 unit tests
run with the tool's tests.

On October 31, 2026, both replays reproduced each recorded cohort: rehearsals
1-3 and the n = 4,000 shortfall drill. They matched availability, quotas,
humans, mixed and scratch counts, and the drill's 86/86/800 shortfall. The
result was 484 used rows with 86 left, and all 570 rows had at least two
sentences.

```sh
slop_ninja_a_testnet_eval_manifest replay-v1 --repo . \
  --pool-dir data/task-roots/testnet/m4-eval/pools/testnet-human-pool-v1
SLOP_NINJA_REPO=$PWD cargo test --locked --manifest-path tools/a-testnet-eval-manifest/Cargo.toml -- --ignored
```

## Limits

- **Pool v2 holds no text yet.** Its documents are expected yields from
  reservation metadata. Acquiring the rows and applying the acquisition
  screens (RAID S2 and S3, near-duplicate windows, mirror hosts, the length
  filter, the text-digest gate) is a separate step.
- **The Common Crawl reference is the pilot's admitted stock.** Re-run the gate
  against the frozen quarterly pool before it is used.
- **Not hidden from operators.** Testnet miners run by the operator can read
  Dev-iter, so testnet does not test hiddenness.
- **Hard links** to sealed files cannot be told apart from the allow-listed
  file by path; the digest and row-split checks still apply.
- The seed source and the cohort numbering belong to the coordinator and are
  not defined here.
