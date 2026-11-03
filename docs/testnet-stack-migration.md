# Subnet testnet stack migration

In the public snapshot, the branches and commits this note names are not
published; it records the private development history.

Recorded on October 31, 2026. The subnet testnet stack reached main as six
squash commits, one per component, followed by one commit that records the
rtao pin and this map. Later commits on top of them carry the review fixes
listed below. Each squash commit's tree is main merged with the
component's branch tip and the components before it, with the changes listed
below. The tips themselves are unchanged on origin, so every record, receipt
and link that names a branch commit (the runbooks, the M2 and M3 drill
records, and the whitepaper's link to `bdc195b2`) can still be checked against
those branches.

## Alias map

| Commit on main | Component | Squashed tip |
|---|---|---|
| `61330b2529de80c4a673c34a176dd5cf74fdb4d7` | Protocol crate, native validator, Workers coordinator, A Container serving, runtime 486 surface, rtao re-pin | `origin/subnet-testnet/integration` `b35fe3a98fa8a8b9bc1400aec573c2ed1240a0e6` |
| `c075077d428912683bf4b01e99bb2b482093ded6` | Native validator live on testnet (M2) | `origin/subnet-testnet/m1-repin` `d013b35d0552d17727c01fbcd05ebedc238d3ef6` |
| `8a863b7a21e141f594edba6e40283feaf3b0cacb` | Coordinator signing (M3) | `origin/subnet-testnet/m3-signing` `232e67ff7f717c15e97a6212dd7ead5882106de9` |
| `969d5babff8e2d73ea066307e595076ccf702de0` | Hidden evaluation (M4) | `origin/subnet-testnet/m4-eval` `637275d4436c23b88ecce4f142c5359837279a78` |
| `b11128ad6a9af9cbdf9d21b83f8c952c782d8d0c` | Fan-out lanes | `origin/subnet-testnet/fanout` `1e23b121d220cfaa0dc0871bf307f0987682e895` |
| `ac2d6d7ded571fd390c3dc11b8f4f2d80a771731` | `reward-v1` in the protocol crate | `origin/subnet-testnet/reward-v1` `2481e46169112caab88ed3d7202b221d523e64aa` |

The integration tip contains `origin/subnet-testnet/protocol`
(`8ceb86b7dbf731cea5969356fc23b5839a302f26`), `validator`
(`cffddd14d8bb2059ac845a842e701946a2b835e6`), `workers`
(`822ef0878efcf1b3faff801b868072343bc5c944`), `a-serve`
(`a40dc209d44b8989c2f614a010c51b68502c9205`), `metadata486`
(`fa6ab439e0d6ec41423d77d723ca8db301ca93d2`) and `m1-repin` at
`1b8cdd5379c7dcd3af06223049e304e723669c24`. The fan-out and `reward-v1` tips
both contain the integration tip; the M3 tip and the M4 tip are siblings on
it, and the fan-out tip contains the M4 tip.

A branch based on one of these tips comes onto main with
`git rebase --onto <commit on main> <tip>`, followed by a check that the
rebase keeps the changes below. Do not merge the tips' history.

## Changes relative to the tips

- `subnet/coordinator/TESTNET.md` names the testnet Workers' URLs as
  `$COORDINATOR_URL` and `$EVAL_URL` and the Cloudflare account generically;
  the private inventory holds them. Deploys are named by the commit they were
  built from, and one fee remark is shorter. The measurements are unchanged.
- The coordinator combines the M3, M4 and fan-out branches: each finalized
  delivery and each cron tick drives the cohort instances and the M3 signing
  round, and both the signing and evaluation routes are served.
- The coordinator's runtime cache selects stored metadata through an explicit
  `(spec, transaction version)` key, and its runtime-version RPC call is a plain
  result chain, so the `no_runtime_gates` lint passes over the M3 code. What is
  decoded and signed is unchanged.
- The hidden-evaluation modules, their Durable Objects and their routes are
  exempt from the M3 panic lints (`unwrap`, `expect`, `panic!`, unchecked
  indexing) until they are converted. In the M4 commit the cron tick reads the
  cohort reply without indexing.
- The coordinator's T12 row dispatch refuses `reward-v1`, whose version-2
  records it does not read yet.
- Each deployment keeps its own ChainJournal liveness record. Both Workers
  bind the evaluation bucket, and on main both crons time their own journal's
  tick, so one shared object let the evaluation Worker's answered ticks reset
  a wedged coordinator journal to `ok` and hide a stopped coordinator cron.
  The coordinator's record stays at `ops/chain-journal-liveness.json`, which
  its `/status`, runbook and M3 drill records read; the evaluation Worker
  (`EVAL_ROLE = "eval"`) uses `ops/eval/chain-journal-liveness.json`. Neither
  tip had both Workers write: the M3 tip ran only as the coordinator, and the
  fan-out tip's cron wrote no record. This was a review fix on top of the
  squash commits.
- The protocol crate keeps both per-tempo identities:
  `weight_tempo_operation_id` (M2) and `WeightOperationV2` (`reward-v1`). The
  `v1` golden report is the 123-line report that includes `operation.tempo`.
- The parity workspace's lockfile lists `wasm-bindgen-futures`, which the
  fan-out lanes add to the coordinator.
- Main's whitepaper, `AGENTS.md`, design draft, competitive evaluator and A
  baseline sources were kept. `docs/service-evidence.md`, `docs/subnet.md` and
  the implementation plan were merged line by line, and the documentation
  index lists each page once.

The testnet Workers now deployed were built from branch commits named in
`subnet/coordinator/TESTNET.md`. Main's coordinator differs from each of them
as listed above, so a deploy from main is a new build that must be qualified
like any other.
