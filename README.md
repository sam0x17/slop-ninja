<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/brand/slop-ninja-wordmark.svg">
  <source media="(prefers-color-scheme: light)" srcset="assets/brand/slop-ninja-wordmark-on-light.svg">
  <img alt="SlopNinja" src="assets/brand/slop-ninja-wordmark-on-light.svg" width="460">
</picture>

### Don’t just detect slop. Fix it.

**AI detection. Slop transformation.**

[![Bittensor testnet: subnet 601](https://img.shields.io/badge/Bittensor%20testnet-subnet%20601-e8173f?labelColor=2a3b41)](docs/testnet/README.md)
[![Licence: MIT OR Apache-2.0](https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-28faba?labelColor=2a3b41)](#licence)

**[Testnet&nbsp;status](https://slop.ninja/testnet/status/)** ·
**[API](https://slop.ninja/api/)** ·
**[Whitepaper](whitepaper/slop_ninja.pdf)** ·
**[Miner&nbsp;guide](docs/testnet/miner-guide.md)** ·
**[Validator&nbsp;guide](docs/testnet/validator-guide.md)** ·
**[Discord](#community)** ·
**[X&nbsp;@SlopNinja](https://x.com/SlopNinja)**

</div>

## What it is

SlopNinja is a Bittensor subnet for writing. Its main objective is to
transform text so it sounds natural and matches the author's intended voice,
while preserving meaning, readability and tone. Its first mechanism, *A*,
detects AI involvement: for each document it reports the share of words AI
wrote or rewrote and the confidence that AI was involved, with conditional
human and AI attribution. *A* runs first, on testnet subnet 601.

## How it works

Miners submit models, not answers. Each cohort, the coordinator (SlopNinja's
lead validator, which runs serverless on Cloudflare) scores every admitted
model on the same hidden batch of documents and signs the weight row those
scores imply. Every other validator verifies that row against pinned public
keys and commits it on chain through Bittensor's native commit-reveal. No
validator receives the hidden documents, their labels or per-document
predictions. Rewards are measured against a published reference model: until
an entrant beats it by the required margin, no miner earns and the pool burns.

| Mechanism | What it does | State |
| --- | --- | --- |
| **A**&nbsp;·&nbsp;Detection | Rewards text models that estimate AI share, AI-involvement confidence and human/AI attribution. | Live&nbsp;on testnet |
| **B**&nbsp;·&nbsp;Transformation | Transforms authorized writing toward the author's voice, keeping meaning, readability and tone. | Planned |
| **C**&nbsp;·&nbsp;Shared grammar | Credits accepted, reusable contributions to the grammar that *B* uses, once each. | Planned |

*B* and *C* activate separately. *B* is trained adversarially against *A* and
uses the native English grammar (CFG) to define its edit units.

The [whitepaper](whitepaper/slop_ninja.pdf) (draft 0.65) describes the full
design, its trust model and its evidence boundaries.

## Testnet status

Testnet subnet 601 is the rehearsal. It has no monetary value.

- **Running:** mechanism *A*, one cohort a day (20 tempos of 360 blocks),
  scored against the reference model `R_0`. Both SlopNinja validators commit
  and reveal the launch burn row every tempo.
- **Admission:** open to every hotkey registered on subnet 601. Commit and
  upload anew for every cohort: a commitment counts for one cohort only, and a
  model you entered before is a published copy that earns nothing, so each
  cohort needs a new model.
- **Releases:** two signed releases for Linux x86_64 are on the repository's
  Releases page. `testnet-v0.1.0` (November 9, 2026) holds the miner client,
  the artifact checker and `R_0`'s reference package (`3c0ecc37`);
  `testnet-v0.2.0` (November 11, 2026) holds the validator and its journal
  tool. On other platforms, build from source as the guides describe.
- **Scores:** every released score is quantized on a grid with seeded noise
  (`quant-v1`), and credit and rank follow the quantized scores; exact scores
  stay with the coordinator. The [miner guide](docs/testnet/miner-guide.md#13-rewards)
  explains the grid, the noise and the cutoff.
- **Revenue pot (planned):** the detection products' revenue is to go into a
  public pot that pays the mainnet registration, then buys mainnet alpha, half
  burned and half distributed to testnet miners in proportion to the
  testnet-era revenue earned by their models that beat the reference and took
  its place. It is a planned operator policy, not an entitlement, and has
  received no money yet
  ([terms](docs/testnet/terms.md#the-pots-planned-distribution)).
- **Follow along:** the [status page](https://slop.ninja/testnet/status/), the
  [public API](https://slop.ninja/api/), the [whitepaper](whitepaper/slop_ninja.pdf),
  the [SlopNinja channel](#community) for subnet 601 on the Bittensor Discord
  and [@SlopNinja](https://x.com/SlopNinja) on X.

The [testnet landing page](docs/testnet/README.md) has the key facts, what is
live and the release key.

## Get started

| Who | Start here |
| --- | --- |
| **Miners** | The [miner guide](docs/testnet/miner-guide.md) goes from installing the client to a committed, uploaded artifact. The [training guide](docs/testnet/training-guide.md) trains an admissible artifact from ModernBERT-base and public data. |
| **Validators** | The [validator guide](docs/testnet/validator-guide.md) (experimental) runs the native Rust validator. |
| **Everyone** | Read the [testnet terms](docs/testnet/terms.md) before you submit. |

The specifications are the [artifact spec](docs/testnet/artifact-spec.md), the
[admission API](docs/testnet/admission-api.md), the
[scorer threat model](docs/testnet/scorer-threat-model.md) and the
[reference packages](docs/testnet/reference-packages.md).

## Community

The SlopNinja channel for testnet subnet 601 on the Bittensor Discord is open:

- channel: <https://discord.com/channels/799672011265015819/1569363956831756318>
- Bittensor community server invite: <https://discord.gg/rY4XsCGrd2> (it does not expire)

The channel link opens for members of the Bittensor community server; if you
are not one yet, join with the invite first. The channel is for the subnet's
mechanism: mining, validating, artifacts, cohorts and rewards.

Ask questions in the channel or in the repository's **Issues** tab. Bug
reports go to the **Issues** tab; security problems go to the private contact
[below](#security).

## Repository layout

| Path | What it holds |
| --- | --- |
| [`subnet/`](subnet/README.md) | The native Rust miner and validator clients, the protocol crate, and the Cloudflare Workers coordinator, admission, generation and publishing services. |
| [`grammar/`](grammar/README.md) | The parser-free *A* crates (baseline, training data preparation, Common Crawl stock), the spaCy feature crates and the public *A* training recipe and exporter. The native English grammar for later *B* work is not yet included. |
| [`inference/`](inference/) | *A* inference: the V6 CPU reference and the Cloudflare Container server. |
| [`baseline/`](baseline/) | The [V6 reference detector](baseline/detector/README.md) and its released evidence, and the [*B* research baseline](baseline/transformer/README.md). |
| [`datasets/`](datasets/README.md) | Published corpus snapshots with exact text, provenance, source licences, fixed splits and checksums, in zstd storage. |
| [`docs/`](docs/README.md) | Operating guides, the [testnet docs](docs/testnet/README.md), design notes, corpus policy, privacy and retained evidence. |
| [`whitepaper/`](whitepaper/README.md) | LaTeX source and PDF build of the design. |
| `src/`, `tests/`, `examples/` | The root Rust package: collection, profiling, detector orchestration and receipt utilities. |
| [`tools/`](tools/) | Dataset storage, artifact and corpus stores, the testnet evaluation manifest and other utilities. |
| [`experiments/`](experiments/) | Retained experiments, such as the gameability studies. |
| `site/` | The page template of the public status host. |
| [`assets/brand/`](assets/brand/README.md) | The approved SlopNinja logo files. |
| `python/`, `scripts/` | The spaCy annotation bridge and the platform check. |

## Development

The root Rust package provides collection, profiling, detector orchestration,
and receipt utilities:

```sh
cargo check
```

Before dataset-backed checks or experiments, restore the published corpus bytes
from their compressed Git storage (see [dataset storage](tools/dataset-storage/README.md)):

```sh
cargo run --release --locked --manifest-path tools/dataset-storage/Cargo.toml -- restore .
```

The grammar workspace, which holds the parser-free *A* crates, is separate:

```sh
cd grammar
cargo check -p grammar-a-baseline
```

Run focused builds and tests in bounded jobs with explicit memory limits and no
swap. The repository has no CI. Build the whitepaper from the repository root
with `bash whitepaper/build.sh`.

Keep API keys, model weights, customer text, private corpora, generated run
directories, and temporary artifacts under ignored `data/` paths or a
task-scoped temporary directory. Do not commit source prose from private
material. Public *A* artifacts may be shared; *B* weights and customer
plaintext follow the whitepaper's
[current privacy contract](whitepaper/sections/05-confidentiality.tex).

The repository contains research artifacts, prototypes and the testnet
clients and services. There is no mainnet subnet, confidential *B* serving
endpoint or production validator.

<details>
<summary><b>Repository history and placeholders</b></summary>

<br>

This is the public SlopNinja repository. Its first commit imports the project
from a private development repository, and later changes are ported from it.
Commit ids, branch names (such as `origin/...`) and ignored `data/` paths cited
in evidence, receipts and documents written before the import refer to that
private history and its local inputs, which are not published. Execution hosts
and home directories are named by placeholders (`build-host`, `remote-host`,
`workstation-host`, `/home/operator`, and `~/` for a home directory). Where the
import changed a file (host and path placeholders, resource names, links),
digests cited for that file refer to its private bytes. The exception is the
files whose home paths the import removed (pinned detector and experiment
records, and the dataset record files and capture audit noted in
[datasets/README.md](datasets/README.md)): their checksums, sizes and record
digests were updated wherever they are cited, together with the digests of the
files that cite them. Release bundles and model weights cited as releases are
not part of this repository. The grammar workspace's native grammar is not yet
included; see [grammar/README.md](grammar/README.md).

</details>

## Security

Report security problems privately by email, never in a public issue. The
[security policy](SECURITY.md) gives the address and says what to report and
what to include.

## Legal notices

Send legal notices about published material, such as a copyright notice about
a published artifact, privately to the address in the
[testnet terms](docs/testnet/terms.md#legal-notices-and-removals), never in a
public issue. The terms say how SlopNinja handles them: a removal is a recorded
operator action, listed in the [removal record](docs/testnet/removals.md), and
changes no score, row or payment.

## Licence

Project code and documentation are licensed under either of the following, at
your option:

- the MIT License ([LICENSE-MIT](LICENSE-MIT));
- the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)).

Miner submissions and the reference model weights (`R_k`) are licensed under
[CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/). A released
weight bundle also keeps its base model's licence and notices. SlopNinja
publishes its own references (`R_0` and any later reference it trains) on these
terms, in packages that carry synthetic replay cases and no evaluation text; a
miner artifact that becomes the reference is published with its cohort like
every admitted artifact.

The SlopNinja logo files in `assets/brand/` are not covered by the code and
documentation licences above.

<details>
<summary><b>Files, directories and datasets with their own terms</b></summary>

<br>

Files, directories and datasets that carry their own licence or notice keep those terms:

- `datasets/`: corpus text keeps its source licences. Each snapshot's `README.md`, `NOTICE.md`, `LICENSE.fix-slop` or `LICENSES.md`, `licenses/` copies and per-row rights records state its terms; see [datasets/README.md](datasets/README.md).
- `baseline/detector/`: `LICENSE` (MIT); `prompts/LICENSE.fix-slop` for the third-party prompts; `releases/v0.6.0/NOTICE.md` for the released weights and their base model.
- `inference/a-v6-cpu/`: `LICENSE` (MIT) and the third-party licences in `licenses/`.
- `experiments/shared-pool-gameability/upstream/`: the vendored upstream source in `source/` and the adapted `math_standalone.rs` (its import-path changes are recorded in `manifest.json` and reproduced by `verify.sh`), under the Apache-2.0 licence in `source/LICENSE`.
- `experiments/shared-pool-gameability/src/native.rs`: the division method bodies of its `compatibility` module, copied unchanged from the vendored `upstream/source/primitives/safe-math/src/lib.rs`, under `upstream/source/LICENSE` (Apache-2.0).
- Global Voices excerpts in dataset texts: [NOTICE](NOTICE) (CC BY 3.0).
- Dataset records quoted or adapted in tests keep their records' licences (PLOS ONE CC BY 4.0, Wikinews CC BY 2.5, historical Wikipedia and HC3 `wiki_csai` CC BY-SA 3.0, CMU Book Summaries CC BY-SA 3.0 US): [NOTICE](NOTICE).
- Rust crates whose `Cargo.toml` declares `license = "MIT"` are MIT only: the crates of the `grammar/` workspace, which inherit it from `grammar/Cargo.toml`; `baseline/detector`; `baseline/transformer`; `inference/a-container-serve`; `inference/a-v6-cpu`; and `tools/a-testnet-eval-manifest`, `tools/artifact-store` and `tools/corpus-store`.

</details>
