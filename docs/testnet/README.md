# Slop Ninja on testnet

Slop Ninja is a Bittensor subnet for detecting AI involvement in writing. Its
first mechanism, *A*, rewards text models that estimate, for each document,
the confidence that AI was involved and the share of words AI wrote or
rewrote, with conditional human and AI attribution. Miners submit models, not
answers: each cohort, the coordinator scores every submitted model on the same
fresh hidden batch, signs the results, and every validator derives the weights
from the signed scores.

Testnet subnet 601 is the rehearsal. It has no monetary value.

## Key facts

| | |
| --- | --- |
| Network | Bittensor testnet, netuid 601 |
| What you submit | one *A* artifact per hotkey per cohort: an ONNX graph of the allow-listed ModernBERT-base architecture, heads, calibration, contract and the pinned tokenizer ([artifact spec](artifact-spec.md)) |
| Cohorts | one a day: 20 tempos of 360 blocks; admission lasts at least one tempo |
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
| Signed release binaries and the release public key | not yet live |
| Coordinator admission routes (`commit`, `upload`, hotkey status) | not yet live |
| Public status host: status snapshot, cohort index, signed packages, leaderboard, published artifacts | not yet live |
| Validator package fetcher | not yet live |
| Validators committing the launch burn row | not yet live |
| Reference packages (`R_0` and the S2c span package, CC BY-SA 4.0) | built and checked; downloadable when public access opens |

The links below will appear here when each goes live:

- public repository: <https://github.com/sam0x17/slop-ninja>; issue tracker: not yet live;
- status host: not yet live;
- release public key: not yet live;
- private security contact: not yet live.

## Getting help

Report problems through the public repository's issue tracker once it is live.
Report security problems privately, never in a public issue.
