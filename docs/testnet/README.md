# SlopNinja on testnet

SlopNinja is a Bittensor subnet for AI detection and transformation of
writing. Its first mechanism, *A* (Detection), rewards text models that
estimate, for each document, the confidence that AI was involved and the share
of words AI wrote or rewrote, with conditional human and AI attribution.
Miners submit models, not answers: each cohort, the coordinator scores every
submitted model on the same hidden batch and signs the weight row the scores
imply, and every validator verifies that row against pinned keys and commits
it on chain. On testnet, until outside miners have working artifacts, that
batch reuses public human documents that earlier cohorts used
([terms](terms.md)).

Testnet subnet 601 is the rehearsal. It has no monetary value.

## Status

- Subnet 601 is live on Bittensor testnet, and mechanism *A* runs one cohort
  a day.
- Admission is open to every hotkey registered on subnet 601 (since November
  7, 2026). Commit and upload anew for every cohort, with a new model: a
  model you entered before is a published copy that is not executed.
- Two signed releases are on the repository's Releases page
  ([Release key](#release-key)): `testnet-v0.1.0` (November 9, 2026) holds
  the miner client, the artifact checker and `R_0`'s archives;
  `testnet-v0.2.0` (November 11, 2026) holds the validator and its journal
  tool.
- Follow along on the [status page](https://slop.ninja/testnet/status/), through the
  [public API](https://slop.ninja/api/), in the
  [whitepaper](../../whitepaper/slop_ninja.pdf) and in the SlopNinja channel
  for subnet 601 on the Bittensor Discord ([Getting help](#getting-help)).
- Ask questions in the Discord channel or the public repository's **Issues**
  tab, report problems in the **Issues** tab, and report security problems
  privately ([Getting help](#getting-help)).

## Key facts

| | |
| --- | --- |
| Network | Bittensor testnet, netuid 601 |
| What you submit | one *A* artifact per hotkey per cohort: an ONNX graph of the allow-listed ModernBERT-base architecture, heads, calibration, contract and the pinned tokenizer ([artifact spec](artifact-spec.md)) |
| Cohorts | one a day: 20 tempos of 360 blocks; admission lasts 16 tempos (about 19 hours) under the testnet template (at least one tempo by the rules; a cohort keeps the window it was queued with; `next-cohort` prints a cohort's window once the coordinator has planned it) |
| Roster cap | 40 entrants per cohort, earliest finalized commitment first |
| Batch | about 1,000 hidden documents per cohort, human-only, mixed and model-only |
| Reward | AI-involvement Brier, against the reference `R_k`; share, attribution and false-positive tiers are gates; released scores are quantized on a grid with seeded noise (`quant-v1`), and the cutoff, credit and rank use them alone |
| Burn regime | until an entrant beats the reference by the margin, no miner earns and the pool burns |
| Revenue pot | planned, separate from emissions: the detection products' revenue pays the mainnet registration, then buys mainnet alpha, half burned and half distributed to testnet miners in proportion to the testnet-era revenue earned by their models that beat the reference and took its place; a planned operator policy, not an entitlement ([terms](terms.md#the-pots-planned-distribution)) |
| Reference | `R_0`, scored every cohort; its [reference package](reference-packages.md) (`3c0ecc37`) is published under CC BY-SA 4.0, attached to the testnet release until public access opens; it changes by promotion or by a recorded operator reference decision ([terms](terms.md#operator-decisions)) |
| Your artifact | private until its cohort's scoring starts, then public under CC BY-SA 4.0; it counts as a published copy in every later cohort, so enter a new model each time |
| Code and docs | MIT OR Apache-2.0 |
| Platforms | Linux x86_64 tested; macOS untested |

## Start here

- **Miners:** the [miner guide](miner-guide.md) goes from installing the client
  to a committed, uploaded artifact; its [Quick path](miner-guide.md#quick-path)
  lists the steps and what tells you each one worked. The [training guide](training-guide.md)
  trains an admissible artifact from ModernBERT-base and public data, and
  ends with `pack` and `check`.
- **Validators:** the [validator guide](validator-guide.md) (experimental).
- **Everyone:** the [terms](terms.md), and where each licence and notice is
  ([licences and notices](licences.md)).
- **Specifications:** the [artifact spec](artifact-spec.md), the
  [admission API](admission-api.md), the
  [scorer threat model](scorer-threat-model.md) and the
  [reference packages](reference-packages.md).
- **Design:** the [whitepaper](../../whitepaper/slop_ninja.pdf).

## What is live

| Piece | State |
| --- | --- |
| Subnet 601 and registration | live |
| Status page and public API, at <https://slop.ninja/testnet/status/> and <https://slop.ninja/api/> | live |
| Miner client `pack` and `check`, artifact checker | built and tested; offline |
| Local scorer (the hidden scorer's engine) | built and tested; offline |
| Graph exporter | in the repository |
| Training guide and public training recipe | in the repository; tested end to end on Linux x86_64 with CUDA (a smoke run) |
| Public repository | [sam0x17/slop-ninja](https://github.com/sam0x17/slop-ninja) |
| Release public key | published below ([Release key](#release-key)) |
| Signed miner release (Linux x86_64) | live: `testnet-v0.1.0`, published November 9, 2026, holds the miner client and the artifact checker, built from the public repository, signed and smoke-tested |
| Signed validator release (Linux x86_64) | live: `testnet-v0.2.0`, published November 11, 2026, holds the validator and its journal tool with their `THIRD-PARTY-LICENSES`, built from the public repository, signed and smoke-tested; install it with the [validator guide](validator-guide.md#2-install). Upgrading to it is roll-forward only |
| Coordinator unsigned cohort read (`check`, `commit`, `status`, `next-cohort`), at `https://testnet-admission.slop.ninja` (pinned in `testnet-trust.json`) | live |
| Coordinator signed admission routes (used by `upload`, `repair` and the hotkey view of `status`; the artifact goes straight to R2 under presigned URLs they return) | open to every hotkey registered on subnet 601 since November 7, 2026 |
| Public status host (the data host the clients read with `--public`, separate from the status page): status snapshot, cohort index, signed packages, leaderboard, published artifacts | not yet live |
| Validator package fetcher | built and tested offline; waits for the public status host |
| Validators committing the launch burn row | live since November 3, 2026: both SlopNinja validators commit and reveal it on subnet 601 each tempo, under a launch authorization that does not expire |
| `R_0` reference package and training companion (CC BY-SA 4.0) | built and checked; attached to the testnet release as two tar archives until public access opens |
| Span package d24 (CC BY-SA 4.0), SlopNinja's span-serving package | built and checked; downloadable when public access opens |

Links:

- public repository: <https://github.com/sam0x17/slop-ninja>, with its **Issues** tab as the issue tracker;
- Discord: the SlopNinja channel for testnet subnet 601 on the Bittensor Discord, <https://discord.com/channels/799672011265015819/1569363956831756318>; the link opens for members of the Bittensor community server, whose invite <https://discord.gg/rY4XsCGrd2> does not expire;
- status page: <https://slop.ninja/testnet/status/>; public API: <https://slop.ninja/api/>; the public status host is not yet live;
- release public key: [below](#release-key);
- private security contact: [security@slop.ninja](mailto:security@slop.ninja) ([security policy](../../SECURITY.md));
- legal notices, such as copyright notices about published material: the address in the [terms](terms.md#legal-notices-and-removals) ([removal record](removals.md)).

## Release key

Each release carries `SHA256SUMS` and a detached signature over it,
`SHA256SUMS.sig`, made with `ssh-keygen -Y sign -n file` by this OpenSSH
Ed25519 key:

```text
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGCXMX0I4LTRrHt0MkCsDfg/OUeQ7Sfd0/EytxMUdYPK slop-ninja testnet release
```

Its fingerprint is
`SHA256:4YzEnNK8C7zjNWvasiQhv0XDJKdrnKB6L33stMQra1Q`. The allowed-signers
line for `ssh-keygen -Y verify` is:

```text
slop-ninja-testnet-release namespaces="file" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGCXMX0I4LTRrHt0MkCsDfg/OUeQ7Sfd0/EytxMUdYPK slop-ninja testnet release
```

The key signs testnet releases only; mainnet releases will use another key.
Take the key from this page, not from the release: the copies of
`allowed_signers` and the `.pub` file in a release prove nothing by
themselves. Verification needs OpenSSH 8.1 or later (`ssh -V`).

In the directory holding the downloaded release files:

```sh
printf '%s\n' 'slop-ninja-testnet-release namespaces="file" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGCXMX0I4LTRrHt0MkCsDfg/OUeQ7Sfd0/EytxMUdYPK slop-ninja testnet release' > allowed_signers
ssh-keygen -Y verify -f allowed_signers -I slop-ninja-testnet-release -n file \
  -s SHA256SUMS.sig < SHA256SUMS
sha256sum -c SHA256SUMS
```

The first command must print
`Good "file" signature for slop-ninja-testnet-release with ED25519 key SHA256:4YzEnNK8C7zjNWvasiQhv0XDJKdrnKB6L33stMQra1Q`,
and the second `OK` for every file. Stop if either does not. If you did not
download every file of a release (for example the two `R_0` archives of
`testnet-v0.1.0`), run `sha256sum -c --ignore-missing SHA256SUMS` instead and
check that it prints `OK` for each file you did download. Then install the
binaries as the [miner guide](miner-guide.md#release-binaries-linux-x86_64)
or the [validator guide](validator-guide.md#2-install) shows.

| Release | Published | For | Built from |
| --- | --- | --- | --- |
| `testnet-v0.1.0` | November 9, 2026 | miners: the miner client, the artifact checker and `R_0`'s archives | commit `939fda70` |
| `testnet-v0.2.0` | November 11, 2026 | validators: the validator and its journal tool; miners need nothing from it | commit `38aafbf6` |

`testnet-v0.1.0` holds:

| File | What it is |
| --- | --- |
| `slop_ninja_miner` | the miner client |
| `slop_ninja_artifact_check` | the artifact checker |
| `testnet-trust.json` | the trust roots: the coordinator's public application keys and the pinned admission origin |
| `slop-ninja-r0-reference-package-3c0ecc37.tar` | the `R_0` [reference package](reference-packages.md), R0's artifact spec v3 package `3c0ecc37`, CC BY-SA 4.0 (about 613 MB; optional) |
| `slop-ninja-r0-training-companion-3c0ecc37.tar` | `R_0`'s [training companion](reference-packages.md#training-companion), R0's encoder for the [warm start](training-guide.md#warm-start-from-r0), CC BY-SA 4.0 (about 598 MB; optional) |
| `SHA256SUMS`, `SHA256SUMS.sig` | the digests of the five files above, and the signature over them |
| `allowed_signers`, `slop-ninja-release-testnet.pub` | copies of the key above, for convenience only |

The two `R_0` archives are attached because the public data host is not
enabled yet; the [miner guide](miner-guide.md#r_0-archives-optional) says how
to verify and unpack them.

`testnet-v0.2.0` holds:

| File | What it is |
| --- | --- |
| `slop_ninja_validator` | the native validator ([validator guide](validator-guide.md), experimental) |
| `slop-ninja-journal` | the validator's offline journal tool (`inspect`, `pause`, `review-failure`) |
| `testnet-trust.json` | the trust roots, byte for byte the copy in `testnet-v0.1.0` |
| `THIRD-PARTY-LICENSES` | the licences of the third-party Rust packages the two binaries are built from ([licences and notices](licences.md#release-binaries)) |
| `SHA256SUMS`, `SHA256SUMS.sig` | the digests of the four files above, and the signature over them |
| `allowed_signers`, `slop-ninja-release-testnet.pub` | copies of the key above, for convenience only |

The validator installer (`subnet/crates/validator/deploy/install.sh`) makes the
same signature and digest checks itself, against a pinned copy of this key,
before it installs the validator's two binaries from the release directory
([validator guide](validator-guide.md#persistent-operation)). Run it from a
checkout of this repository at the release's tag. An upgrade to
`testnet-v0.2.0` is roll-forward only: once it has written its held-target
record in the new `v2` schema, no earlier build opens that record, and
`rollback-check` exits 3 ([validator
guide](validator-guide.md#upgrading-to-testnet-v020)).

The binaries are for Linux x86_64. Each release's notes name the commit it
was built from, the oldest glibc its binaries need (read from their symbol
versions) and the results of a smoke test of the signed files on one Linux
x86_64 host. `testnet-v0.1.0` was built from commit `939fda70` and
`testnet-v0.2.0` from commit `38aafbf6`; both need glibc 2.34 or later.
There are no macOS or Linux arm64 binaries: build from source as the guides
describe. Those platforms are untested.

## Getting help

Ask questions about mining and validating in the SlopNinja channel for
testnet subnet 601 on the Bittensor Discord or in the public repository's
**Issues** tab:

- channel: <https://discord.com/channels/799672011265015819/1569363956831756318>
- Bittensor community server invite: <https://discord.gg/rY4XsCGrd2> (it does not expire)

The channel link opens for members of the Bittensor community server; if you
are not one yet, join with the invite first. The channel is for the subnet's
mechanism: mining, validating, artifacts, cohorts and rewards.

Report problems in the public repository's **Issues** tab.
Report security problems privately to [security@slop.ninja](mailto:security@slop.ninja), never in a
public issue. Send legal notices privately to the address in the
[terms](terms.md#legal-notices-and-removals).
