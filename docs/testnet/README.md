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
- Admission is currently limited to SlopNinja's operator test hotkeys during
  the admission v2 rollout: an outside hotkey's signed admission requests are
  answered `not_found`. Do not commit until admission is open to your hotkey;
  a commitment you cannot upload costs the chain fee and admits nothing.
- Follow along on the [status page](https://slop.ninja/status/), through the
  [public API](https://slop.ninja/api/) and in the
  [whitepaper](../../whitepaper/slop_ninja.pdf).
- Report problems in the public repository's **Issues** tab, and security
  problems privately ([Getting help](#getting-help)).

## Key facts

| | |
| --- | --- |
| Network | Bittensor testnet, netuid 601 |
| What you submit | one *A* artifact per hotkey per cohort: an ONNX graph of the allow-listed ModernBERT-base architecture, heads, calibration, contract and the pinned tokenizer ([artifact spec](artifact-spec.md)) |
| Cohorts | one a day: 20 tempos of 360 blocks; admission lasts 16 tempos under the testnet template (at least one tempo by the rules; a cohort keeps the window it was queued with, so earlier ones admit for one tempo or 17; `next-cohort` prints each cohort's own) |
| Roster cap | 40 entrants per cohort, earliest finalized commitment first |
| Batch | about 1,000 hidden documents per cohort, human-only, mixed and model-only |
| Reward | AI-involvement Brier, against the reference `R_k`; share, attribution and false-positive tiers are gates |
| Burn regime | until an entrant beats the reference by the margin, no miner earns and the pool burns |
| Reference | `R_0`, scored every cohort; its [reference package](reference-packages.md) is published under CC BY-SA 4.0, attached to the testnet release until public access opens; replaced at most once outside promotion, only by a better reference that passes every gate, announced in advance |
| Your artifact | private until its cohort closes, then public under CC BY-SA 4.0 |
| Code and docs | MIT OR Apache-2.0 |
| Platforms | Linux x86_64 tested; macOS untested |

## Start here

- **Miners:** the [miner guide](miner-guide.md) goes from installing the client
  to a committed, uploaded artifact. The [training guide](training-guide.md)
  trains an admissible artifact from ModernBERT-base and public data, and
  ends with `pack` and `check`.
- **Validators:** the [validator guide](validator-guide.md) (experimental).
- **Everyone:** the [terms](terms.md).
- **Specifications:** the [artifact spec](artifact-spec.md), the
  [admission API](admission-api.md), the
  [scorer threat model](scorer-threat-model.md) and the
  [reference packages](reference-packages.md).
- **Design:** the [whitepaper](../../whitepaper/slop_ninja.pdf).

## What is live

| Piece | State |
| --- | --- |
| Subnet 601 and registration | live |
| Status page and public API, at <https://slop.ninja/status/> and <https://slop.ninja/api/> | live |
| Miner client `pack` and `check`, artifact checker | built and tested; offline |
| Local scorer (the hidden scorer's engine) | built and tested; offline |
| Graph exporter | in the repository |
| Training guide and public training recipe | in the repository; tested end to end on Linux x86_64 with CUDA (a smoke run) |
| Public repository | [sam0x17/slop-ninja](https://github.com/sam0x17/slop-ninja) |
| Release public key | published below ([Release key](#release-key)) |
| Signed release binaries (Linux x86_64) | not yet live: the miner client and the artifact checker, built from the public repository, signed and smoke-tested when the release is published |
| Validator release binaries | not in the first release: the validator client is changing, and a validator release follows later; until then, build from source with the [validator guide](validator-guide.md) |
| Coordinator unsigned cohort read (`check`, `commit`, `status`, `next-cohort`), at `https://testnet-admission.slop.ninja` (pinned in `testnet-trust.json`) | live |
| Coordinator signed admission routes (used by `upload`, `repair` and the hotkey view of `status`; the artifact goes straight to R2 under presigned URLs they return) | open only to SlopNinja's operator test hotkeys during the admission v2 rollout (an outside hotkey's signed requests are answered `not_found`); do not commit until they are open to you |
| Public status host (the data host the clients read with `--public`, separate from the status page): status snapshot, cohort index, signed packages, leaderboard, published artifacts | not yet live |
| Validator package fetcher | built and tested offline; waits for the public status host |
| Validators committing the launch burn row | live since November 3, 2026: both SlopNinja validators commit and reveal it on subnet 601 each tempo, under a launch authorization that does not expire |
| `R_0` reference package and training companion (CC BY-SA 4.0) | built and checked; attached to the testnet release as two tar archives until public access opens |
| S2c span package (CC BY-SA 4.0) | built and checked; downloadable when public access opens |

Links:

- public repository: <https://github.com/sam0x17/slop-ninja>, with its **Issues** tab as the issue tracker;
- status page: <https://slop.ninja/status/>; public API: <https://slop.ninja/api/>; the public status host is not yet live;
- release public key: [below](#release-key);
- private security contact: [security@slop.ninja](mailto:security@slop.ninja) ([security policy](../../SECURITY.md)).

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
chmod 0755 slop_ninja_miner slop_ninja_artifact_check
```

The first command must print
`Good "file" signature for slop-ninja-testnet-release with ED25519 key SHA256:4YzEnNK8C7zjNWvasiQhv0XDJKdrnKB6L33stMQra1Q`,
and the second `OK` for every file. Stop if either does not. If you did not
download the two `R_0` archives, run `sha256sum -c --ignore-missing SHA256SUMS`
instead and check that it prints `OK` for each file you did download. The
first release holds:

| File | What it is |
| --- | --- |
| `slop_ninja_miner` | the miner client |
| `slop_ninja_artifact_check` | the artifact checker |
| `testnet-trust.json` | the trust roots: the coordinator's public application keys and the pinned admission origin |
| `slop-ninja-r0-reference-package-143ca2d9.tar` | the `R_0` [reference package](reference-packages.md), CC BY-SA 4.0 (about 613 MB; optional) |
| `slop-ninja-r0-training-companion-143ca2d9.tar` | `R_0`'s [training companion](reference-packages.md#training-companion), R0's encoder for the [warm start](training-guide.md#warm-start-from-r0), CC BY-SA 4.0 (about 598 MB; optional) |
| `SHA256SUMS`, `SHA256SUMS.sig` | the digests of the five files above, and the signature over them |
| `allowed_signers`, `slop-ninja-release-testnet.pub` | copies of the key above, for convenience only |

The two `R_0` archives are attached because the public data host is not
enabled yet; the [miner guide](miner-guide.md#r_0-archives-optional) says how
to verify and unpack them.

The first release holds no validator binaries: the validator client is
changing, and a validator release follows later. Until then, build the
validator from source as the [validator guide](validator-guide.md) describes.
The validator installer (`subnet/crates/validator/deploy/install.sh`) makes the
same signature and digest checks itself, against a pinned copy of this key,
before it installs the validator's two binaries from a release that holds them.

The binaries are for Linux x86_64. Each release's notes will name the commit
it was built from, the oldest glibc its binaries need (read from their symbol
versions) and the results of a smoke test of the signed files on one Linux
x86_64 host.
There are no macOS or Linux arm64 binaries: build from source as the guides
describe. Those platforms are untested.

## Getting help

Report problems in the public repository's **Issues** tab.
Report security problems privately to [security@slop.ninja](mailto:security@slop.ninja), never in a
public issue.
