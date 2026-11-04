# Slop Ninja on testnet

Slop Ninja is a Bittensor subnet for detecting AI involvement in writing. Its
first mechanism, *A*, rewards text models that estimate, for each document,
the confidence that AI was involved and the share of words AI wrote or
rewrote, with conditional human and AI attribution. Miners submit models, not
answers: each cohort, the coordinator scores every submitted model on the same
hidden batch, signs the results, and every validator derives the weights from
the signed scores. On testnet, until outside miners have working artifacts,
that batch reuses public human documents that earlier cohorts used
([terms](terms.md)).

Testnet subnet 601 is the rehearsal. It has no monetary value.

## Key facts

| | |
| --- | --- |
| Network | Bittensor testnet, netuid 601 |
| What you submit | one *A* artifact per hotkey per cohort: an ONNX graph of the allow-listed ModernBERT-base architecture, heads, calibration, contract and the pinned tokenizer ([artifact spec](artifact-spec.md)) |
| Cohorts | one a day: 20 tempos of 360 blocks; admission lasts 17 tempos under the current template (at least one tempo by the rules) |
| Roster cap | 40 entrants per cohort, earliest finalized commitment first |
| Batch | about 1,000 hidden documents per cohort, human-only, mixed and model-only |
| Reward | AI-involvement Brier, against the reference `R_k`; share, attribution and false-positive tiers are gates |
| Burn regime | until an entrant beats the reference by the margin, no miner earns and the pool burns |
| Reference | `R_0`, scored every cohort; its [reference package](reference-packages.md) is published under CC BY-SA 4.0 when public access opens; replaced at most once outside promotion, only by a better reference that passes every gate, announced in advance |
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
| Miner client `pack` and `check`, artifact checker | built and tested; offline |
| Local scorer (the hidden scorer's engine) | built and tested; offline |
| Graph exporter | in the repository |
| Training guide and public training recipe | in the repository; tested end to end on Linux x86_64 with CUDA (a smoke run) |
| Public repository | [sam0x17/slop-ninja](https://github.com/sam0x17/slop-ninja) |
| Release public key | published below ([Release key](#release-key)) |
| Signed release binaries (Linux x86_64) | not yet live: built from the public repository, signed and smoke-tested when it is published |
| Coordinator admission routes (`commit`, `upload`, hotkey status), at `https://testnet-admission.slop.ninja` (pinned in `testnet-trust.json`) | not yet live |
| Public status host: status snapshot, cohort index, signed packages, leaderboard, published artifacts | not yet live |
| Validator package fetcher | not yet live |
| Validators committing the launch burn row | live since November 3, 2026: both Slop Ninja validators commit and reveal it on subnet 601 each tempo, under a launch authorization that does not expire |
| Reference packages (`R_0` and the S2c span package, CC BY-SA 4.0) | built and checked; downloadable when public access opens |

The links below will appear here when each goes live:

- public repository: <https://github.com/sam0x17/slop-ninja>; issue tracker: not yet live;
- status host: not yet live;
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
chmod 0755 slop_ninja_miner slop_ninja_artifact_check slop_ninja_validator slop-ninja-journal
```

The first command must print
`Good "file" signature for slop-ninja-testnet-release with ED25519 key SHA256:4YzEnNK8C7zjNWvasiQhv0XDJKdrnKB6L33stMQra1Q`,
and the second `OK` for every file. Stop if either does not. A release holds:

| File | What it is |
| --- | --- |
| `slop_ninja_miner` | the miner client |
| `slop_ninja_artifact_check` | the artifact checker |
| `slop_ninja_validator` | the native validator |
| `slop-ninja-journal` | the validator's journal tool, which the validator installer also installs |
| `testnet-trust.json` | the trust roots: the coordinator's public application keys and any pinned admission origins |
| `SHA256SUMS`, `SHA256SUMS.sig` | the digests of the five files above, and the signature over them |
| `allowed_signers`, `slop-ninja-release-testnet.pub` | copies of the key above, for convenience only |

The binaries are for Linux x86_64. Each release's notes will name the commit
it was built from, the oldest glibc its binaries need (read from their symbol
versions) and the results of a smoke test of the signed files on one Linux
x86_64 host.
There are no macOS or Linux arm64 binaries: build from source as the guides
describe. Those platforms are untested.

## Getting help

Report problems through the public repository's issue tracker once it is live.
Report security problems privately to [security@slop.ninja](mailto:security@slop.ninja), never in a
public issue.
