# Testnet miner guide

This guide takes you from nothing to an *A* artifact that is committed on
chain and uploaded for a cohort on Bittensor testnet subnet 601. It covers the
miner client `slop_ninja_miner`, wallets and registration with rtao, training,
packing and checking, the commitment, the upload, what happens when a cohort
closes, how rewards work, and what each refusal means.

Testnet has no monetary value. Read the [terms](terms.md) before you submit:
submissions are licensed CC BY-SA 4.0, and an admitted artifact becomes
public when its cohort's scoring starts.

## Quick path

The whole path, with the section for each step and what tells you it worked:

1. **Get test TAO** on a testnet coldkey, for the registration cost and fees
   ([What you need](#1-what-you-need)).
2. **Download and verify the release**, then install the two binaries
   ([Install](#2-install)): `ssh-keygen` prints `Good "file" signature`,
   `sha256sum` prints `OK` for each file, and `slop_ninja_miner --version`
   prints `slop_ninja_miner 0.1.0`.
3. **Make a hotkey and register it on subnet 601** with rtao, which you build
   with Rust ([steps 3 and 4](#3-wallet-and-hotkey)):
   `rtao subnets metagraph 601 --network test` lists your hotkey with a UID.
4. **Train an artifact** with the [training guide](training-guide.md)
   ([step 5](#5-train-an-artifact)). Its smoke run gives an admissible but
   weak model; its steps took about 12 minutes on the tested 24 GB GPU host,
   not counting installs and downloads.
   `slop_ninja_artifact_check --spec v3 check-dir` exits 0 on the result.
5. **Pack and check it** with the release trust file set
   ([steps 6 and 7](#6-pack-and-check)): `slop_ninja_miner check` exits 0
   while a cohort admits (3 between windows: every local check passed, with
   no cohort to compare against).
6. **Wait for an admitting cohort** ([step 8](#8-wait-for-an-admitting-cohort)):
   `slop_ninja_miner next-cohort` prints `admitting cohort <N>` with its
   planned cutoff.
7. **Commit and upload** in that cohort's window ([steps 9 and 10](#9-commit)),
   or let `slop_ninja_miner resolve --artifact <object> --watch` do both:
   `upload` prints `session <id> in cohort <N>: verified` and exits 0.
8. **Follow the cohort** with `slop_ninja_miner status` and the
   [status page](https://slop.ninja/testnet/status/) ([step 11](#11-status)).
   After the cutoff your entry reads `admitted`, or `overflow` if the roster
   was already full (40 entrants). Results are published when the cohort
   closes, about a day after it opened
   ([step 12](#12-at-the-cutoff-and-the-cohort-close)).
9. **Enter a new model next time.** An admitted artifact is published when its
   cohort's scoring starts, so entering it again earns nothing
   ([Rewards](#13-rewards)).

No miner is paid until an entrant passes the gates and its released score
reaches the margin cutoff above `R_0` ([Rewards](#13-rewards)).

## What is live

| Piece | State |
| --- | --- |
| Subnet 601 on testnet, and registration on it | live |
| Status page and public API, for people following along: <https://slop.ninja/testnet/status/> and <https://slop.ninja/api/> | live; the client does not read them |
| `slop_ninja_miner pack` and `check`, and `slop_ninja_artifact_check` | built and tested on Linux x86_64; they run offline |
| Local scorer (`slop_ninja_a_local_score`), the hidden scorer's engine on your machine | built and tested on Linux x86_64; runs offline |
| Graph exporter (`export_a_onnx.py`) | in the repository |
| [Training guide](training-guide.md) and its public recipe, the path from ModernBERT-base and public data to an admissible artifact | in the repository; tested end to end on Linux x86_64 with CUDA (a smoke run) |
| Release public key | published on the [landing page](README.md#release-key) |
| Public repository | [sam0x17/slop-ninja](https://github.com/sam0x17/slop-ninja) |
| Signed release binaries (Linux x86_64) | live: `testnet-v0.1.0`, published November 9, 2026, holds the miner client and the artifact checker, built from the public repository, signed and smoke-tested; the validator's binaries are in `testnet-v0.2.0`, which miners do not need |
| Coordinator unsigned cohort read (`GET /admission/v2/cohort`), used by `check`, `commit`, `status` and `next-cohort`, served by the upload Worker at `https://testnet-admission.slop.ninja` | live |
| Coordinator signed admission routes, used by `upload`, `repair` and the hotkey view of `status`; the artifact goes straight to R2 under presigned URLs they return | open to every hotkey registered on subnet 601 since November 7, 2026; an unregistered hotkey is answered `not_registered` |
| Public status host (`--public`), with the published packages, leaderboard and artifacts | not yet live; until it is, `status` and `next-cohort` show the calendar from the coordinator's unsigned cohort read |
| The coordinator's admission origin pinned in the release trust file | pinned: `https://testnet-admission.slop.ninja` |
| Validators committing the launch burn row ([Rewards](#13-rewards)) | live since November 3, 2026: both SlopNinja validators commit and reveal it on subnet 601 each tempo |
| `R_0` reference package and training companion | built and checked; attached to the testnet release until public access opens ([`R_0` archives](#r_0-archives-optional)) |
| Span package d24, SlopNinja's span-serving package | built and checked; downloadable when public access opens ([Reference model](#reference-model)) |
| Status API (`https://slop.ninja/api/`), read-only JSON for each cohort ([The status API](#the-status-api)) | live; each miner's intake state while a cohort runs is not filled in yet and follows with pending evaluation and publisher updates |
| `status` intake answers and `resolve` ([Status](#11-status), [Resolve](#resolve-intake-automatically)) | built and tested against simulated answers on Linux x86_64; their per-miner answers fill in as the status API publishes each miner's intake state and challenge counts |

The [training guide](training-guide.md) produces an admissible artifact from
public material and ends with `pack` and `check`. `commit` needs only the
unsigned cohort read and the chain, so it signs and submits an on-chain
commitment for any registered hotkey whenever a cohort admits. `upload` needs
the signed routes, which are open to every registered hotkey. **Each cohort
needs a new commitment and a new model**: a commitment counts for one cohort
only, and a model you entered before is a published copy that is not executed
([Copies](#13-rewards)). The steps below say where they depend on a piece that
is not yet live. The public repository is
<https://github.com/sam0x17/slop-ninja>; this page will name the status host
when it goes live. The release key is on the [landing page](README.md#release-key).

## 1. What you need

- A Linux or macOS machine for the client. Linux x86_64 is tested; macOS and
  Linux arm64 are untested.
- Rust 1.89 or later (rtao's minimum; tested: 1.99.1) and a C compiler,
  also when you use the release binaries: you build rtao, the wallet and
  registration tool, from source, and the optional local scorer too. On
  platforms without release binaries you also build the client.
- A training machine as the [training guide](training-guide.md#what-you-need)
  describes: tested on Linux x86_64 with a 24 GB NVIDIA GPU and CUDA 12.8
  wheels, and about 16 GB of disk.
- About 1.3 GB of free disk per artifact: the six model files and the packed
  object, about 613 MB each for R0.
- Test TAO on a testnet coldkey, for registration and transaction fees. The
  registration cost floats: on November 10, 2026 it read 0.0005 τ, subnet
  601's floor (`rtao subnets burn-cost 601 --network test`); each
  registration raises it, and it falls back toward the floor over the
  following hours. It read about 0.155 τ on October 30, 2026. Keep a little
  more for the call's fee ([step 4](#4-register-on-subnet-601)). SlopNinja
  does not hand out test TAO; get it from the Bittensor community's testnet
  sources, for example by asking in the Bittensor Discord.
- An uplink that can send your packed object, up to about 955 MB (the spec's
  largest object), inside a cohort's admission window (16 tempos, about 19
  hours, under the testnet template; `next-cohort` prints each cohort's own; see
  [step 8](#8-wait-for-an-admitting-cohort)).

## 2. Install

### Release binaries (Linux x86_64)

Releases appear on the public repository's Releases page. The first,
`testnet-v0.1.0`, published on November 9, 2026, holds:

- the Linux x86_64 binaries `slop_ninja_miner` and
  `slop_ninja_artifact_check`;
- `testnet-trust.json`, the trust file;
- two optional tar archives, `slop-ninja-r0-reference-package-3c0ecc37.tar`
  and `slop-ninja-r0-training-companion-3c0ecc37.tar`: `R_0`'s reference
  package and its training companion, about 1.2 GB together
  ([below](#r_0-archives-optional));
- `SHA256SUMS`, the digests of those five files, and its detached signature
  `SHA256SUMS.sig`;
- `allowed_signers` and `slop-ninja-release-testnet.pub`, copies of the key
  for convenience only.

It holds no validator binaries. Those are in `testnet-v0.2.0`, published on
November 11, 2026 ([validator guide](validator-guide.md#2-install)), which
holds nothing for miners: take the miner client, the artifact checker and
the archives from `testnet-v0.1.0`. The binaries need glibc 2.34 or later;
on any other platform, build from source.

Download the release's files into a new, empty directory:

```sh
mkdir slop-ninja-release && cd slop-ninja-release
REPO=https://github.com/sam0x17/slop-ninja
TAG=testnet-v0.1.0
for f in slop_ninja_miner slop_ninja_artifact_check testnet-trust.json SHA256SUMS SHA256SUMS.sig; do
  curl -fLO "$REPO/releases/download/$TAG/$f"
done
```

The two archives are optional ([below](#r_0-archives-optional)). Verify the
files before you run anything, with the key and allowed-signers line from the
[landing page](README.md#release-key):

```sh
printf '%s\n' 'slop-ninja-testnet-release namespaces="file" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGCXMX0I4LTRrHt0MkCsDfg/OUeQ7Sfd0/EytxMUdYPK slop-ninja testnet release' > allowed_signers
ssh-keygen -Y verify -f allowed_signers -I slop-ninja-testnet-release -n file \
  -s SHA256SUMS.sig < SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
mkdir -p ~/.local/bin
install -m 0755 slop_ninja_miner slop_ninja_artifact_check ~/.local/bin/
slop_ninja_miner --version
```

`ssh-keygen` must print `Good "file" signature for
slop-ninja-testnet-release` with the fingerprint
`SHA256:4YzEnNK8C7zjNWvasiQhv0XDJKdrnKB6L33stMQra1Q`, and `sha256sum` `OK`
for every file you downloaded (with `--ignore-missing` it skips the archives
you left out; check that both binaries and `testnet-trust.json` are among
them);
stop if either does not. Take the key from the landing page, not from the
release's own copy. `slop_ninja_miner --version` then prints
`slop_ninja_miner 0.1.0`; `~/.local/bin` must be on your `PATH`, or call the
binaries by path. Keep the release's `testnet-trust.json`: the signature
covers it, and the client reads it to decide where it may upload
([step 7](#7-choose-the-endpoints) points `SLOP_NINJA_TRUST` at it).

### `R_0` archives (optional)

You need the two archives only to score against `R_0` locally or to start
training from its encoder ([Warm start from
R0](training-guide.md#warm-start-from-r0)). The public data host is not
enabled yet, so they are attached to the release until public access opens;
then `R_0`'s reference package is also downloadable from the [public
layout](public-layout.md). Both are licensed CC BY-SA 4.0; the package's
`NOTICE.md`, which also covers the companion, says how to credit R0 and its
base model. The companion archive does not carry its own copy of that notice
or of the base model's licence: both are in the reference-package archive, as
`NOTICE.md` and `upstream/ModernBERT-base-LICENSE`, so keep the two archives
together when you pass the companion on.

The archives hold R0's artifact spec v3 package, the package the
`testnet-r1-` cohorts name. Its digest is the SHA-256 of its files manifest,
`artifact-files.json` (`slop_ninja_a_artifact_files_v2`), byte for byte as
the coordinator stores it, and both archives are rooted at that digest.
Extracted into one directory they give the manifest; `files/<sha256>`, the six
model files, each whole and named by its SHA-256; `NOTICE.md` and
`upstream/ModernBERT-base-LICENSE`; `reference/`, the three synthetic
reference cases R0's export manifest binds, with their FP64 predictions;
`provenance-rewrite.json` ([reference
packages](reference-packages.md#provenance-rewrite)); and, from the
companion archive, `training/companion.json` and `training/parts/<sha256>`,
R0's encoder and its classifier config as 64 MiB parts. The [reference
packages](reference-packages.md) page describes the package.

To download them, run the download loop [above](#release-binaries-linux-x86_64)
again in the same directory, with their two names as its list. The archive files are covered by `SHA256SUMS`, so verify them with the
commands above. In the directory holding the release files, extract them and
check the package digest and every file and part:

```sh
tar -xf slop-ninja-r0-reference-package-3c0ecc37.tar
tar -xf slop-ninja-r0-training-companion-3c0ecc37.tar
cd 3c0ecc37376a9287f595f8389fe66faade00c663fd19f772eeb51abb7822c9fe
sha256sum artifact-files.json   # must print 3c0ecc37376a9287f595f8389fe66faade00c663fd19f772eeb51abb7822c9fe
for f in files/* training/parts/*; do printf '%s  %s\n' "${f##*/}" "$f"; done | sha256sum -c --quiet
```

The last command prints nothing when every file and part matches its name.
Then put the six model files under their names in `../r0-artifact`, rebuild
the companion (R0's encoder `model/encoder.safetensors` and its
`runtime-base/config.json`) into `../r0-companion` by concatenating each
file's parts in the listed order, and check both. This needs `jq`;
extracting and rebuilding take about 2.4 GB of disk beside the archives.

```sh
mkdir ../r0-artifact ../r0-companion
jq -r '.files[] | "\(.sha256) \(.name)"' artifact-files.json |
  while read -r h name; do cp "files/$h" "../r0-artifact/$name"; done
jq -r '.files[] | .name as $n | .parts[] | "\(.sha256) \($n)"' training/companion.json |
  while read -r h name; do mkdir -p "../r0-companion/${name%/*}"; cat "training/parts/$h" >> "../r0-companion/$name"; done
jq -r '.files[] | "\(.sha256)  ../r0-companion/\(.name)"' training/companion.json | sha256sum -c
slop_ninja_artifact_check --spec v3 check-dir ../r0-artifact
```

`check-dir` must report graph class `a-plain-f32band-v1` and the
inference-content digest (`content_sha256`)
`d7232991612ae9693477a152d73e2b6ca5ac83b2d9cf5cd78e1eac4ecf96a36f`, and
`sha256sum -c` must print `OK` for both companion files. The rebuild appends
to the files it writes, so run it once, into the new directory the `mkdir`
creates. `../r0-artifact` is the directory to give the [local
scorer](#score-it-locally-optional), and
`../r0-companion/model/encoder.safetensors` is the encoder for the warm start.
Its `tokenizer.json` is the pinned tokenizer byte for byte
([Tokenizer](artifact-spec.md#tokenizer)). Do not enter R0's files as they
are: R0 is a published reference, so an entry with its inference content is
admitted but never executed ([Copies earn nothing](#13-rewards)).

### Build from source

Build from the public repository:

```sh
git clone https://github.com/sam0x17/slop-ninja
cd slop-ninja/subnet
cargo build --release --locked -p slop_ninja_miner -p slop-ninja-artifact-check
mkdir -p ~/.local/bin
install -m 0755 target/release/slop_ninja_miner target/release/slop_ninja_artifact_check ~/.local/bin/
slop_ninja_miner --version
slop_ninja_artifact_check --version
```

`~/.local/bin` must be on your `PATH`; otherwise install the two binaries
elsewhere or call them by path. The build compiles rtao from Git at the pinned
revision and needs no Python or btcli; some dependencies need a C compiler.
The trust file is `subnet/coordinator/testnet-trust.json` in the checkout.

`slop_ninja_artifact_check --version` prints every artifact spec id the build
defines, with its digest. The checker checks spec v1 unless you put
`--spec v2`, `--spec v3` or `--spec v4` before the command; use the spec your cohort
names, which is the spec the hidden scorer admits your artifact by. The checker
also has `check-dir DIR` and `check-object FILE`
(the admission checks on a directory or a packed object), `graph FILE` (the
graph's class, by a byte comparison of its structure stream with the class's
reference stream, and its parameter count) and `spec` (the whole spec
as JSON). Its `pack DIR OUT` and `files DIR OUT` are operator tools: to pack
an artifact, use `slop_ninja_miner pack`, which refuses an existing output and
writes the object with mode 0600.

### rtao, for wallets and registration

The client signs with an ordinary Bittensor hotkey file. Install the rtao
command line tool at the same pin the client uses:

```sh
cargo install --locked --git https://github.com/sam0x17/rtao \
  --rev fa83052d37b39f002f7a4d3cc5c04f25503e1d11 rtao-cli
rtao --version
```

A wallet that another tool already wrote in the standard layout
(`~/.bittensor/wallets/<wallet>/hotkeys/<hotkey>` with its `<hotkey>pub.txt`)
works as it is.

## 3. Wallet and hotkey

The coldkey holds funds and pays for registration. The hotkey is the miner's
identity: it signs the commitment and every upload request. Use a fresh hotkey
for testnet. The names below are examples.

```sh
rtao wallet create --wallet my-wallet --wallet-hotkey my-miner
rtao wallet show --wallet my-wallet
```

`wallet create` asks for a coldkey password, writes an encrypted coldkey and
an unencrypted hotkey, and prints a recovery mnemonic for each. Store them
offline. `rtao wallet new-hotkey --wallet my-wallet --wallet-hotkey <name>`
adds another hotkey later. Fund the coldkey's address with test TAO.

`slop_ninja_miner` reads the hotkey with `--wallet my-wallet --hotkey my-miner`
(or `SLOP_NINJA_WALLET` and `SLOP_NINJA_HOTKEY`) under `--wallet-path`
(`SLOP_NINJA_WALLET_PATH`), which defaults to `~/.bittensor/wallets`, or from
`--hotkey-file PATH`. It never needs the coldkey.

## 4. Register on subnet 601

```sh
rtao subnets burn-cost 601 --network test
rtao subnets register --network test --netuid 601 --wallet my-wallet --wallet-hotkey my-miner
rtao subnets metagraph 601 --network test
```

The coldkey pays the subnet's current registration cost, which is known only
when the call executes. `rtao subnets register --help` describes the call in
full. At the pinned revision it says:

- **Burn and collateral.** When the subnet's collateral lock share `p` is
  zero, the whole cost is burned. When `p` is above zero, `1 - p` of it is
  burned and `p` is staked to your hotkey and locked as miner collateral,
  released only through earned emission.
- **Spend cap.** If you configured an rtao spend cap, it blocks the call until
  you raise it, because the exact charge is unknown in advance.
- **MEV shield.** rtao always submits this call MEV-shielded and refuses
  `--no-mev-shield` for it. The shield's outer carrier fee is paid in free
  test TAO, so keep some free TAO on the coldkey beyond the registration cost.

The metagraph shows your hotkey's UID once the registration is final.

Facts that matter for a miner:

- **Immunity.** Subnet 601's immunity period is 21,600 blocks, about three
  days. On testnet a commitment counts only inside its cohort's admission
  window ([step 8](#8-wait-for-an-admitting-cohort)), so if the cohort runs on
  schedule and you earn, your first payout arrives about a day after your
  commitment: the cohort closes about 24 hours after it opened, validators
  commit its row from the next tempo, and the subnet reveals a commitment one
  tempo after it is made. A longer admission window, an overrun or a
  coordinator outage moves it later. While the launch burn lasts, no miner is
  paid at all ([Rewards](#13-rewards)). Immunity is not absolute: when pruning
  a non-immune neuron would leave too few non-immune UIDs, the chain prunes the
  lowest-emission immune neuron instead.
- **Pruning.** Once the subnet's UID slots are full, a new registration
  replaces the non-immune neuron with the lowest emission (on a tie, the one
  registered earlier, then the one with the lower UID). While the launch burn
  lasts (below), every miner's emission is zero, so any miner past immunity can
  be replaced.
- **Registration generation.** Credit follows your hotkey and its registration
  block. If your hotkey is pruned and registers again, it starts a new
  generation: an opening made under the old one no longer counts, and credit
  still awaiting payout under the old one burns.

## 5. Train an artifact

Follow the [training guide](training-guide.md), which starts from
ModernBERT-base and public data; `R_0`'s
[reference package](reference-packages.md) and its training companion let you
score against it locally and start from its encoder instead. The guide gives
the whole
command sequence and one pinned Python environment for training and export:
it builds a public parent model and the pinned tokenizer from ModernBERT-base,
builds the training data from the redistributable datasets under `datasets/`,
trains with the release trainer, calibrates on the calibration split, writes
the release files, exports the graph with `export_a_onnx.py --variant plain`
and assembles the artifact directory.

The release trainer in the repository
(`grammar/crates/grammar-eval/python/train_full_a.py`) is not a path on its
own. It accepts only the private parent model that R0 was trained from, which
is not distributed; the training guide's recipe supplies a public parent in
its place. And it writes a fit directory, not the six files: calibration, the
release files, the graph export and the assembly come after it.

The result is a directory with exactly six files, under the output contract
`slop_ninja_full_a_contract_v2`:

| File | Role |
| --- | --- |
| `a-fp32band.onnx` | the graph |
| `heads.json` | the head weights |
| `contract.json` | the output schema |
| `calibration.json` | the calibration |
| `manifest.json` | the export record (owner metadata; not part of the committed content) |
| `tokenizer.json` | the tokenizer |

The [artifact spec](artifact-spec.md) is the authority. The points that most
often decide admission:

- **The graph class.** The graph must have the structure of the allow-listed
  plain class `a-plain-f32band-v1`: the banded ModernBERT-base *A* graph with
  22 layers, 601,028,257 bytes. Only the values of its initializer weights may
  differ, and no two weights may be equal: the exporter merges initializers
  with equal values, which changes the structure. Export in the pinned
  environment the spec names (Python 3.12, torch 2.9.1, transformers 4.57.6;
  the exporter refuses others), and confirm
  the class with `slop_ninja_artifact_check --spec v3 graph a-fp32band.onnx`
  (`graph_class` names it when the structure matches; otherwise
  `structure_difference` is the stream offset of the first differing byte).
  Spec v2 also serves the span class `a-span-f32band-v1` (the plain graph
  plus its final-norm token states) with heads that read those states: a
  spans-v1 token head in heads v2, exported as export v3 (the form of
  SlopNinja's S0+ span head). The training guide's recipe does not train a token
  head. Spec v1 refuses the span class with `graph_class_not_served`. The
  artifact spec's [Spec v2](artifact-spec.md#spec-v2) section has the details.
  Spec v3 also serves the tower class `a-tower-f32band-v1` (the plain graph
  plus a span tower and its normed token states) with heads v3, the form of
  SlopNinja's S2c span package; see [Spec v3](artifact-spec.md#spec-v3). The
  tower is computed on every document, so it costs CPU time whether or not
  spans are asked. The tower starts as copies of encoder layers 18 to 21 and
  of the final norm, and every tower tensor must end up differing from every
  other weight: a tensor left equal to the encoder tensor it was copied from
  (a norm or layer you froze or did not train) is merged with it, and the
  graph leaves the class (`graph_size`). `export_a_onnx.py` refuses such a
  model before export and names the equal tensors.
- **Rosters are fixed** at 33 AI profiles and an empty human roster, because
  the graph contains the document heads. The 33 names, in order, are listed in
  the artifact spec's [AI profile names](artifact-spec.md#ai-profile-names);
  `contract.json` and `heads.json` must carry exactly those.
- **The tokenizer** must be byte-identical to the pinned file: SHA-256
  `6c8aaa9a542084f2457eab775d4eeb51f92a70c0fd9de28d5edb0ddec3c08d30`,
  3,583,228 bytes. It is not in the repository, but it is in R0's reference
  package ([`R_0` archives](#r_0-archives-optional)). The upstream ModernBERT-base
  `tokenizer.json` is refused as it is (`tokenizer_mismatch`); the artifact
  spec's [Tokenizer](artifact-spec.md#tokenizer) section shows how to rebuild
  the pinned bytes from it at a pinned revision.
- **JSON files** must be in their canonical layouts and pass the role schemas,
  including the pinned contract fields and the calibration policy.
- **Parameters**: at most 160,000,000 for the plain and span classes;
  spec v3's tower class has its own derived bound, 173,549,185. Any other
  file in the directory, an external-data sidecar included, is refused.
- **Input.** Cohorts feed raw text, with nothing truncated; the scorer
  accepts at most 2,048 tokens including `[CLS]` and `[SEP]` and answers a
  longer document `unsupported_length`, for every artifact alike. The
  spec's canonical text profile records exactly what the scorer does to a
  document.

Check the directory before you pack it, under the spec your cohort names
(spec v3 for the testnet cohorts; without `--spec` the checker checks spec
v1, which admission never uses):

```sh
slop_ninja_artifact_check --spec v3 check-dir ./my-artifact
```

A pass prints the report as JSON (the spec, the inference-content digest
`content_sha256`, the graph class and its report, the role files and the
parameter count) and exits 0. A refusal prints `refused <code>: <detail>` and exits 2; a path it cannot
read is refused as `object`. A usage error exits 64, and `--help` exits 0.

There is no small example artifact that passes. The graph class fixes the
graph at 601,028,257 bytes of a real export, and the role files must agree with
it, so only a real export can pass. A directory of placeholder files still
exercises the tools, but reaches only refusals. The shortest path to a passing
artifact is the training guide's smoke run.

## 6. Pack and check

```sh
slop_ninja_miner pack --dir ./my-artifact --out ./my-artifact.bin
slop_ninja_miner check ./my-artifact.bin
```

`pack` writes the artifact object with mode 0600 and refuses an output path
that exists. It hashes each file exactly once, while it writes it: those
digests go into the object's header, your commitment binds them, and R2
checks each uploaded file against them. Nothing hashes your model again.
Keep the object and the model files private until the cohort's scoring
starts: anyone who has them before then can enter the same model. Do not
change the object after you commit; a changed file is refused at upload
(`BadDigest`).

`check` runs the spec's admission checks on the object in place, the same
checks the scorer runs on the files at admission. It reads the object's header
and takes every digest from it, hashing no file. Every `slop_ninja_miner`
command uses the same exit statuses:

| Exit | Meaning |
| --- | --- |
| 0 | success; for `check`, every check ran and passed |
| 1 | refused: an admission check refused the artifact (`check` prints the code the coordinator would report), the upload session ended `refused`, or the coordinator refused a request with a code that retrying cannot change (for example `not_registered` for a signed `status` read); the code is printed ([Refusals](#14-refusals-and-what-they-mean)) |
| 2 | error: usage, configuration, network or file; a check the client makes itself before it signs anything (for example `commit` while no cohort is admitting); or a retryable answer, such as `unavailable` or `commitment_not_found`, still there after the attempts. Nothing was decided about the artifact |
| 3 | `check` only: nothing refused the artifact, but some checks did not run |

Without a coordinator `check` cannot read the cohort's output contract and
size bound, so an offline check that refuses nothing exits 3 with the note
`cohort_unknown`. The same happens when endpoints are set but the cohort
cannot be read, for example because the coordinator is unreachable or no trust
file pins its origin: `check` prints a warning, still runs every local check,
and names the reason in the `cohort_unknown` note. With the endpoints of the
next section set (`SLOP_NINJA_TRUST` alone is enough), `check` also reads the
admitting cohort from the coordinator's unsigned cohort read, checks its
output contract and size bound, and exits 0 on a pass. Between windows it
uses the next cohort's contract, size bound and spec when the cohort read
names them, with the note `next_cohort` naming that cohort (check again once
it admits); otherwise it exits 3 with the note `cohort_unknown: no cohort is
admitting`.

`check` uses the artifact spec the cohort names; without a cohort, the newest
spec the build defines (spec v4). A note `artifact_spec_mismatch` means the
cohort checks a spec your build does not define: update the client.

The JSON files are published with the artifact when its cohort's scoring
starts. If one
holds a string or key that reads as an absolute local path (one that starts
with `/`, `~/` or a Windows drive), carries one (a home directory or a file
URL in text) or names a path under a `data/` tree, it would publish your
directory layout, user name included. `pack` warns about it on stderr and
`check` reports `[WARNING] absolute_path` with the JSON pointers of those
strings (never their values). A warning never refuses the artifact and does
not change the exit status. The check can miss a path written another way, so
read your JSON files before you publish them. The repository's tools record
portable paths (resolved against the working directory: repository-relative
inside the repository, the final component elsewhere): run them as the
[training guide](training-guide.md#steps) does, then export and pack again.
A contract built from a normalization with absolute run paths keeps them,
because the fit binds the contract's digest: rebuild the normalization and the
contract before training.

### Score it locally (optional)

`slop_ninja_a_local_score` runs your artifact through the hidden scorer's own
engine: the same admission, load guards, graph contract and heads runtime.
The release does not include it. Build it with Rust from a checkout of the
repository and give it the ONNX Runtime 1.31.1 library: the
hidden scorer uses `onnxruntime/capi/libonnxruntime.so.1.31.1` from the
onnxruntime 1.31.1 cp312 manylinux_2_28_x86_64 wheel (SHA-256
`fc44112fbe06d7d689c57e11d3862031f64711007e4ee020f8eed5185fd2c1d3`); the
training guide's environment already has it (the library is then
`venv/lib/python3.12/site-packages/onnxruntime/capi/libonnxruntime.so.1.31.1`
under the guide's working directory).

```sh
cargo build --release --locked --manifest-path inference/a-container-serve/Cargo.toml \
  --bin slop_ninja_a_local_score
inference/a-container-serve/target/release/slop_ninja_a_local_score \
  --model-dir ./my-artifact --docs my-docs.jsonl --out my-results.jsonl \
  --spec v3 --ort-lib /path/to/libonnxruntime.so.1.31.1
```

`--spec` is the spec your cohort names (`v3` for the testnet cohorts); without
it, the local scorer admits by spec v4, which admits exactly what spec v3
admits.

Each line of `my-docs.jsonl` is `{"id", "text"}`, optionally with
`"involved"` (true when AI was involved) and `"share"` (the AI-written
fraction, 0 to 1). Each output line is the hidden scorer's result for one
document. Standard error gets a summary with the counts by status and, with
labels, the mean confidence Brier score and share error over the documents
that scored. That summary is a check on your own data, not your cohort
score, which uses the cohort's hidden documents, its source balancing and its
failure rules. With `--spans` and a span head, the results carry sentences
and spans too. The scorer's limits and failure modes are in the
[scorer threat model](scorer-threat-model.md).

## 7. Choose the endpoints

Set these once per shell:

```sh
export SLOP_NINJA_TRUST=/path/to/testnet-trust.json   # from the release
export SLOP_NINJA_PUBLIC=<public status host>         # not yet live; leave unset until it is
export SLOP_NINJA_WALLET=my-wallet SLOP_NINJA_HOTKEY=my-miner
```

An upload sends your opening, with its secret nonce, in signed requests,
and the coordinator answers with presigned URLs for your artifact's objects,
so the client sends signed requests only to an authenticated coordinator
origin. The release trust file lists one origin in `admission_origins`:
`https://testnet-admission.slop.ninja`, the coordinator's upload Worker. The
client uses only that origin:

- With `SLOP_NINJA_TRUST` set and neither `--coordinator` nor `--public`,
  `check`, `commit`, `upload`, `repair`, `status` and `next-cohort` use the one
  origin the trust file pins. This works now, before the public status host is
  live.
- `--coordinator https://testnet-admission.slop.ninja` (or
  `SLOP_NINJA_COORDINATOR` set to it) names the same origin explicitly. Any
  other origin is refused while the trust file pins one.
- Once the public status host is live, `--public` alone also works: the
  client takes the origin from the snapshot's `coordinator` field, which it
  accepts only because the trust file pins it.

Never take an upload origin from a chat message or from the unsigned
snapshot alone.
The client prints the origin before it opens a session, never follows
redirects, and accepts plain HTTP only for loopback hosts.

**Your artifact's bytes go straight to R2**, the coordinator's private object
storage, never through a SlopNinja Worker: one presigned PUT for the header
and one for each file. The client needs outbound HTTPS to the coordinator
origin and to the R2 S3 host the URLs name (`<account>.r2.cloudflarestorage.com`).
Before it sends a byte it checks each URL: `https`; the host and bucket the
release pins (the trust file's `upload_host_sha256`, the SHA-256 of the R2 S3
host's name, and its `upload_bucket`; the client compiles the release's pins in,
so they apply even without `--trust`, unless you pass `--any-upload-host` for a
deployment other than the release's); the object key it derives itself from
the cohort, your commitment and the file's digest; and the signed headers. An
error message names the object and the host, never the URL's signed query. The URL shows the storage account's host, the bucket name and
the signing key's id; none of these is a credential.

The upload Worker holds no signing key and no storage binding, and takes no
upload byte: its largest request body is one session envelope. It checks every
signed request before it reads the body or forwards anything: the signature must verify, and the
hotkey must hold a UID on subnet 601 at the finalized head (the Worker
reads the subnet's hotkeys once a block, so a hotkey registered in the
last few blocks may answer `not_registered` until finality and that read
catch up; send again a block later). A request from an
unregistered hotkey is refused with `not_registered`, so register before you
commit. Testnet enforces protocol bounds only. There is one session per
commitment and one artifact per hotkey per cohort, and sizes are bounded by
the artifact spec. Cloudflare's DDoS protection sits in front of the upload
host. There is no per-IP or per-hotkey request rate limit.

## 8. Wait for an admitting cohort

```sh
slop_ninja_miner next-cohort
```

`next-cohort` prints the admitting cohort and its planned cutoff epoch, and the
next cohort with its earliest opening epoch and how long its admission window
lasts, each with its distance from the confirmed epoch, for example `planned
cutoff at epoch 23170 (1 epoch after the confirmed epoch)`. With `--public` it
reads the public status snapshot (not yet live). Without it, it reads the
coordinator's unsigned cohort read (`GET /admission/v2/cohort`,
[admission API](admission-api.md#get-admissionv2cohort-unsigned)) from the
origin the trust file pins, or from `--coordinator`; its first line names
that origin, the finalized block and the coordinator's confirmed epoch. A
coordinator that reports no confirmed epoch gets absolute epochs only. The
same read works without the client:

```sh
curl -s https://testnet-admission.slop.ninja/admission/v2/cohort
```

Its `admitting` is `null` outside a window, and `next` names the next cohort
and its `earliest_open_epoch`. When the coordinator knows the next cohort's
plan, `next` also carries its `admission_tempos` (then `next-cohort` prints the
window) and its `output_contract`, `max_object_len` and `artifact_spec` (then
`check` compares with them between windows). The earliest opening is a
lower bound: the next cohort opens at the boundary that closes the one before
it, which slow work or a coordinator outage moves later.

Cohort times are subnet epoch indices: the chain's `SubnetEpochIndex` for
subnet 601, which advances by one at each tempo boundary, about every 360
blocks or 72 minutes. It is a per-subnet counter (239 for subnet 601 on
November 10, 2026), not the block number divided by 360; the epochs in this
guide's examples are illustrative. The report's first line gives the
finalized block and the confirmed epoch. An epoch that lies *N* epochs after
the confirmed one therefore starts roughly *N* × 72 minutes after the
confirmed epoch started, at roughly the confirmed epoch's first block plus
*N* × 360. A coordinator that reports no confirmed epoch gets absolute epochs
only; then read the current epoch from the chain (`SubnetEpochIndex` for
subnet 601 in the Subtensor storage, in a chain explorer) and count from it.
Boundaries can slip, so treat these as estimates and read `next-cohort` again
as the time approaches.

Testnet runs one cohort a day: 20 tempos of 360 blocks, about 24 hours. Each
cohort opens at the tempo boundary that closes the one before it, and its
admission window opens with it. The testnet standing template's window is 16
tempos (5,760 blocks, about 19 hours 12 minutes): the cohort then freezes its
roster, generates, scores and packages in the next two tempos, publishes at
the boundary after that and closes two tempos later, when the next cohort
opens. Between two such windows there are about 4 tempos (about 4 hours 48
minutes) in which no cohort admits. The window is a plan parameter frozen with
each cohort when it is queued, so a cohort queued before the template took its
current value keeps the window it was queued with (the first testnet cohorts
admit for one tempo, and the first long-window cohorts for 17 tempos, with
about 3 tempos between windows). `next-cohort` prints each cohort's own
window when the cohort read carries it; trust it over this page, and treat
the cutoff it prints as planned, not promised.

The window's length comes from measurements, not a chosen figure: it is the
cohort period less the two-tempo transition and the tempos the work after the
cutoff needs (measured work, a submission completing at the cutoff, the
verdicts of a full roster still being verified at the cutoff, one model load
of the largest object and one entrant's scoring of it, sized to at most half
of its tempos), and the time from a commitment to the `verified` answer
for the largest object the spec admits (about 955 MB; about 15 minutes in the
operator's measurement, scaled from a 613 MB submission) fits in under half of
it. Your own uplink sets your time; a slow one still fits easily unless you
start in the window's last hours.

A commitment counts for a cohort only when its block is at or after the
cohort's opening block and before its cutoff block. A commitment made earlier
is refused with `commitment_before_open`, so you cannot reserve a place ahead
of time: wait for `next-cohort` to show the cohort admitting, then commit for
it. During a coordinator outage the cutoff moves later, never earlier, and the
cohort is never skipped.

Have the artifact packed and checked before you commit. The commitment needs
finality and the coordinator's observation of it (nominally up to 60 seconds
after finality, sometimes longer) before an upload can start, and the upload
must complete before the cutoff, so commit and upload together rather than
committing early and uploading near the cutoff. The public snapshot's
`admitting` field is set only while the window is open; once it closes the
cohort's phase stays `admission` until the roster freezes, but nothing more is
admitted.

From your second cohort on, the coordinator still holds your previous
commitment until its commitment reader logs the new one. Right after
`commit`, `upload` may hear `commitment_not_found` with `Retry-After`; it
retries as told (`--attempts`). `upload` sends the block at which `commit`
confirmed your commitment, so the coordinator keeps answering
`commitment_not_found` until its reader has polled at or past that block,
even when the coordinator is behind the chain. A definitive answer
(`commitment_before_open`, `commitment_mismatch`, `not_registered`,
`registration_mismatch`) comes at the reader's next completed poll after
that, when the opening really is wrong. If the retries run out first,
`upload` stops with `commitment_not_found`; run it again later. Without `--opening`, `upload` picks the newest opening this
wallet confirmed on chain when it is newer than the commitment the
coordinator observed, and refuses locally when that opening is for another
artifact than `--artifact` (after `commit --replace`, upload the new
artifact).

## 9. Commit

```sh
slop_ninja_miner commit --artifact ./my-artifact.bin --dry-run
slop_ninja_miner commit --artifact ./my-artifact.bin
```

Both forms need only the coordinator's unsigned cohort read and the chain;
nothing here uses a signed admission route. Commit only when the artifact is
packed and checked and you can upload it in the window. `commit` reads the admitting cohort from the cohort read and your registration from finalized chain state, makes a fresh
secret 256-bit nonce and the opening, and stores the opening in the state
directory with mode 0600 before it signs anything. It then signs
`Commitments.set_commitment(601, [Raw44(payload)])` with the hotkey, where the
44-byte payload is `"sna1" || u64_le(cohort) || commitment digest`, and reads
the commitment back at finality. `--dry-run` prints the opening and the call
and stores, signs and submits nothing. It also reads your registration: for a
hotkey without a UID it stops with `<address> is not registered on subnet 601
at finalized block <B>` (exit 2). `--network` defaults to `test`; `--rpc`
names endpoints.

Rules:

- **One cohort per opening; commit again every cohort.** An opening names
  exactly one cohort and can never be carried to a later one. Nothing carries
  over between cohorts: to enter the next cohort, commit again inside its
  window and upload again.
- **A new model every cohort.** Once your artifact is admitted, it is
  published when its cohort's scoring starts, and every published artifact
  counts as a copy in every later cohort, your own included. Entering the
  same model again (the same inference content) gets it admitted but not
  executed, so it earns nothing; a near copy of it fails the public-copy gate
  ([Copies earn nothing](#13-rewards)). Train or change the model before each
  commitment.
- **The last commitment counts.** The chain keeps one commitment per hotkey; a
  new one overwrites the old and moves you to the new position. `commit`
  refuses a second opening for the same cohort unless you pass `--replace`.
- **Position.** Your admission position is the finalized block and extrinsic
  index of your commitment. Under the roster cap, earlier positions win.
- **Cost.** The chain charges no fee for `set_commitment`. It limits each
  account's commitment space per epoch; a call over it fails with
  `SpaceLimitExceeded`.

The state directory is `--state-dir`, `SLOP_NINJA_MINER_HOME`, else
`$XDG_DATA_HOME/slop_ninja_miner`, `~/.local/share/slop_ninja_miner`, or
`~/Library/Application Support/slop_ninja_miner` on macOS. Its directories are
mode 0700 and its files mode 0600; the client refuses a file that group or
others can read. Back it up until the cohort closes: an upload needs the
opening.

## 10. Upload

```sh
slop_ninja_miner upload --artifact ./my-artifact.bin
```

`upload` opens a session for
your opening, PUTs the object's header straight to R2 with the URL the
session returns, asks for the file URLs (the coordinator checks the header
first, so an artifact doomed by its header is refused before any file is
sent), PUTs each file straight to R2, largest first, completes the session and
waits while the coordinator verifies it. It picks the local opening whose
payload matches the commitment the coordinator observed; `--opening FILE` names
one. Progress lines on standard error show each file's bytes, rate and time
left (`--quiet` turns them off).

- **Resume is per file.** A single PUT cannot resume, so an interrupted file is
  sent again from its first byte; files R2 already holds are not sent again.
  Rerun the same command to resume.
- **Each URL lives long enough** for your whole artifact at 1 Mbit/s plus 15
  minutes (2 h 22 min for the largest spec v3 artifact). R2 checks a URL's
  expiry when the PUT starts, so a file whose PUT started in time finishes at
  any speed. Expiry never ends anything: the client asks for a fresh URL
  whenever one expires before its PUT starts, at once. A file's PUT times out
  locally after `--http-timeout-secs` plus its size at
  `--min-upload-kib-per-sec` (default 32); raise it on a fast link, lower it
  on a slow one.
- **Retries** (`--attempts`, default 6, per request and per file): a dropped
  connection, a 5xx or 429 from R2 gets a fresh URL and the file goes again
  after a doubling wait. 412 means R2 already holds the file. A busy upload
  Worker answers the contract's `unavailable` with `Retry-After`; an edge error
  page without a contract body (502, 503, 504, or a Cloudflare 52x) is retried
  the same way.
- **501 `NotImplemented`** from R2 is a refusal: nothing is stored and the
  client stops (it checks every URL first, so this should not happen).
- **Send nothing else.** The client sends only the URL's signed headers. A
  tool that adds `Content-Type`, `Content-Encoding` or similar to the PUT
  gets its object refused: R2 stores those headers with the object, and
  admission refuses an object with any stored HTTP or custom metadata.
- **`BadDigest`** means the object on disk no longer matches the digests you
  committed to: it changed after `pack`. Restore it, or pack again from the
  same sources (the same files give the same digests); a different object
  needs a new commitment.
- **One object per commitment.** A refused session stays refused. To upload a
  different object or model, pack it, commit again (`commit --replace`) and
  upload; the new commitment supersedes the old session and moves your
  position.
- **Repair.** If an object the coordinator recorded later goes missing on its
  side (a platform fault, never yours), `status` lists it and
  `slop_ninja_miner repair --artifact ./my-artifact.bin` (or `status --repair
  --artifact ...`) sends it again from your local object, also after the
  cutoff. Keep the object until your cohort's scoring starts.

One hotkey enters at most one artifact per cohort. The session ends as
`verified` or `refused`.

**How you know it worked.** Once the coordinator has verified the upload,
`upload` prints `session <id> in cohort <N>: verified; position block <B>
extrinsic <E>` and exits 0. `verified` means the artifact passed every
admission check; whether it is admitted is decided at the cutoff, by
commitment position when the roster freezes
([step 12](#12-at-the-cutoff-and-the-cohort-close)). If
the coordinator is still verifying when `--wait-secs` (600 s) runs out,
`upload` prints `verifying` and also exits 0: read `status` later. A refused
session prints its code and exits 1.

## 11. Status

```sh
slop_ninja_miner status
slop_ninja_miner status --ss58 <your hotkey address>   # public data only, no wallet
```

With a hotkey named (your wallet, or `--ss58`), `status` first answers the
questions a miner has about the admitting or current cohort, each with
`yes`, `NO`, `not yet` or `unknown` and its source:

```text
hotkey 5F… in cohort 26 (testnet-r1-26): uploading
  registered?          yes      uid 12, hotkey 5F…k2Qz (status API)
  commitment seen?     yes      inside the admission window (status API)
  upload verified?     not yet  uploading
  admitted?            not yet  decided when the roster freezes after the cutoff
  challenges started?  NO       only admitted entries receive challenges
  challenges done?     NO       none
  next: the upload is incomplete: resume it (fresh URLs; files R2 holds are not sent again)
  not automatic: no --artifact named: the upload needs the object
```

Every failure state has a next step: not registered (register with rtao),
no commitment counted (commit), a commitment this state directory cannot
open (upload from where you committed), one for another artifact, one made
before the window opened, an incomplete upload (resume it), a refusal by
class (`commitment`, `artifact`, `cutoff`, `registration`), overflow, a
missed cutoff, objects to repair, and, during scoring, `challenges pending`
until your entry's scoring starts and then `3 of 40` style counts. The
answers come from the [status API](#the-status-api) (unsigned, refreshed
about once a minute, so a change shows up to a minute or two late), the
coordinator's unsigned cohort read, its signed view of your hotkey when
your wallet is named, and your local openings. `status` reads no chain
state and changes nothing. `--json` prints the answers as one JSON object;
`--watch` reads again about once a minute and prints them when they change
(`--max-wait-secs` bounds it). Until the status API publishes each miner's
intake state, many answers read `unknown`. `registered?` reads `unknown` until
it publishes the registry, and the next step printed under the answers can then
say to commit even for a hotkey with no UID. Check your registration with
`rtao subnets metagraph 601 --network test`; with your wallet named, the
coordinator's view at the end of `status` answers `not_registered` (exit 1)
for such a hotkey.

After the answers, `status` prints the public snapshot (unsigned, and labelled so) or, without
`--public`, the same calendar from the coordinator's unsigned cohort read
(as `next-cohort` does), your local openings, and the coordinator's view of
your hotkey for the admitting cohort or `--cohort N`: registration, the
commitment that counts and whether it is in the window, each session with its
state, its recorded objects, its refusal and any objects to repair, and the
last refused request. With no openings it says so
and names the state directory it read. The local openings are always printed:
when the cohort read or the public snapshot cannot be read (a coordinator
outage, an unreadable trust file), `status` prints them first, then reports
the failure and exits 2. Without any endpoint (no
`--coordinator`, no `--public` and no trust file that pins one origin) it
prints only the local openings and a line saying the cohort and the
coordinator's view were not read; without a named hotkey it skips the
coordinator's view and says so. A signed read the coordinator refuses (for
example `not_registered` for a hotkey without a UID) exits 1 with the code.
The public snapshot is not yet live.

Session states: `uploading`, `verifying`, `verified`, `refused`, `admitted`,
`overflow` and `superseded` (a newer commitment replaced it).

### The status API

The [Testnet Status](https://slop.ninja/testnet/status/) page's tracker links to a
read-only JSON API that needs no key and allows any origin (CORS):

```sh
curl -s https://slop.ninja/api/v1/testnet/a/status          # the current cohort
curl -s https://slop.ninja/api/v1/testnet/a/cohorts         # every listed cohort
curl -s https://slop.ninja/api/v1/testnet/a/cohorts/<label> # one cohort
curl -s https://slop.ninja/api/                             # endpoints and schemas
```

A cohort's answer has its stage and phase history, tempos credited and
required, admission progress, the work percent, the expected stage end and
close with estimated times, the pause state, the references and the miner
list with each hotkey's status for the cohort. The estimated times are lower
bounds: a cohort can run later, never earlier. The API is unsigned and
publishes no score, rank or result before a cohort closes; after it closes,
each entrant's gate bits, band, cluster and roster position. Use `status`
for your own sessions' details: the API shows one status per hotkey
(`committed`, `uploading`, `verifying`, `verified`, `refused` with a class,
`overflow`, `admitted`, `evaluating`, then `scored`, `withheld` or
`not_executed`), and until its per-hotkey documents are live it shows only
part of that list. While a cohort is scored, each admitted entrant's row also
has `challenges` (`pending` until its scoring starts, then `done` of `total`).
These are counts only, with no score, and they update about once a minute. Set
your miner on the status page to see a checklist for your hotkey: registered,
commitment seen, upload verified, admitted, challenges started, and how many
are done, each with its next step; `status` in the client answers the same
questions. The fields and their sources are in the
[public layout](public-layout.md#the-public-status-api-slopninjaapi).

### Resolve intake automatically

```sh
slop_ninja_miner resolve --artifact ./my-artifact.bin            # one pass
slop_ninja_miner resolve --artifact ./my-artifact.bin --watch    # until verified
```

`resolve` reads what `status` reads plus the chain's finalized registration
and commitment for your hotkey (rtao, `--network`, `--rpc`), and takes the one
action your state allows:

| State | What `resolve` does |
| --- | --- |
| no commitment counts for the admitting cohort (none made, a call that never landed, one made before the window, a newer registration than the opening's, a `commitment`-class refusal, an `object_mismatch` or `object_replaced` refusal) | commits for `--artifact` (a fresh nonce; the opening is stored first) |
| an artifact-class refusal, with `--artifact` naming a changed model and `--allow-replace` | commits for the changed model; its upload follows |
| committed, no upload yet, or the upload is incomplete | uploads or resumes, asking for fresh URLs as they expire; files R2 holds are not sent again |
| all files in R2 but not completed, or verifying | completes the session again and follows the verification |
| a recorded object went missing on the coordinator's side | repairs it from `--artifact` |
| the status API, the coordinator or the RPC endpoint is down, `paused`, or behind | waits and reads again, backing off from one minute to one tempo |
| verified, admitted, scoring, closed, overflow, refused at the cutoff | nothing; `--watch` stops |

A refusal decides only while it belongs to the commitment that counts. Once a
fresh commitment from this state directory counts, the old refused session
no longer decides, and `resolve` uploads the new commitment. With `--watch`,
one run commits again and then uploads.

It never does these by itself:

- **Spend funds.** Registration burns TAO from your coldkey, which the client
  never loads: register with rtao ([step 4](#4-register-on-subnet-601)).
- **Change your submitted model.** It commits only for the artifact your
  hotkey last committed for this cohort, unless you pass `--allow-replace`:
  needed to commit for another artifact (for example after an artifact-class
  refusal, once you have fixed and packed the model) or to replace a
  commitment made from another machine or state directory.
- **Commit after the cutoff.** It commits only while the coordinator's cohort
  read says the cohort admits and the cutoff's earliest block is more than a
  margin past the finalized head. The margin is `--deadline-secs` (600) at
  12 s a block, 50 blocks by default, and it is checked again just before
  signing. The commitment is signed with a mortal era of at most half the
  margin (16 blocks by default), so the chain can include it only within that
  many blocks of the finalized head it was signed at, which is before the
  cutoff. The bound fails only if the finalized head advances by more than
  half the margin between that last check and the signature. It
  warns early when less than twice an upload's planned time is left (your
  object's size at the coordinator's URL-policy rate of 1 Mbit/s plus 15
  minutes, the same allowance the URLs carry) and again when less than once
  is left.
- **Commit from the feed alone.** A commitment the status API has not shown
  yet may only be its minute of lag: `resolve` decides from the chain, and
  uploads a landed commitment instead of committing again.
Each pass is idempotent, so running it again (or from cron) is safe; one
pass at a time acts on a state directory (`resolve.lock`). Every decision and
its outcome is appended to `resolve-log.jsonl` in the state directory (mode
0600), without your opening, any URL or signature. `--dry-run` prints what it
would do. Exit 0 when the state needs nothing more from you, 1 when a manual
step is needed (it prints which), 2 on an error.

## 12. At the cutoff and the cohort close

When admission closes, the coordinator:

1. refuses every session not completed in time (`incomplete_at_cutoff`);
2. for each verified session, checks that your last commitment before the
   cutoff block is still the one the session opens and that your registration
   is unchanged (`commitment_overwritten`, `not_registered` or
   `registration_mismatch` otherwise);
3. orders the remaining sessions by position (block, then extrinsic index) and
   admits the first 40, the testnet roster cap (`admitted`);
4. marks the rest `overflow`.

**Nothing carries over, overflow included.** To enter the next cohort, commit
again inside its window and upload again; you are ranked by the new
commitment. An overflow upload is deleted at the close. An admitted model is
published when scoring starts, so the next cohort needs a new one.

The cohort then runs without you:

- **Generation.** The coordinator draws the hidden batch, about 1,000
  human-only, mixed and model-only documents in the proportions the cohort
  manifest fixes, and generates its AI-side text.
- **Publication of the artifacts.** When scoring starts, every admitted
  artifact (header and files) is published in the public bucket, except an
  operator's or a reference's: the header at
  `artifacts/v2/<header sha256>/header` and each file at
  `blobs/sha256/<file sha256>`. Until the public status host goes live, the
  public bucket cannot be read from outside; the artifacts already in it
  become readable when it opens.
- **Scoring.** Every admitted artifact, the reference and its predecessor, when
  one exists, score the same documents, each document independently. Every
  executed entrant also runs once on a private 1,024-document probe used for
  the public-copy gate.
- **Package.** The coordinator signs its score package (the exact scores and
  every hidden measurement) and its receipt index, nominally by tempo 18. Both
  stay private to the coordinator.
- **Transition.** At least two tempos with no new challenges. At the closing
  boundary the coordinator publishes every admitted entrant's opening and the
  cohort's quantized result, and the cohort's target activates.

Validators then commit the cohort's row at every tempo during the next cohort,
so cohort *N* is paid during the following day while cohort *N*+1 is scored.

Human documents: until outside miners have working artifacts, testnet cohorts
reuse human documents that earlier cohorts already used. Fresh human stock is
drawn only once outside miners have working artifacts. A reused document is no
longer unseen, so a score from such a cohort shows that your artifact runs and
is scored; it is not evidence of quality on unseen text. Every text in the
testnet stock is public and can be recognized, an accepted testnet limit.

What becomes public, and what never does:

- **Public when scoring starts:** your artifact (header and files), if
  admitted.
- **Public after the close:** your opening, and the cohort's quantized
  result: for each executed entrant in roster order, its hotkey and UID, its
  quantized score (the result's `q` field) unless it is withheld
  ([Rewards](#13-rewards)), its band, one bit for whether it passed the gates,
  one for the public-copy gate, and its cluster, with the grid width and both
  references' quantized scores. The
  coordinator's signed row, with each paid UID's integer weight, is published
  beside its authorization.
- **Never public:** exact scores, which gate failed and why, the hidden
  documents, their labels and per-document predictions, and the coordinator's
  score package and receipts. Refused, overflow and superseded uploads are
  deleted no later than the close and never published.

## 13. Rewards

The score is binary Brier utility on AI involvement, `1 - (a - y)^2` per
document, averaged over the batch: `a` is your predicted probability of any AI
involvement, `y` is 1 for model-only or mixed documents and 0 for human-only
ones. Improvement over the reference `R_k` is measured in units of its
remaining loss: `q = (S - S_R) / (1 - S_R)`. Artifact qualification, the
public-copy gate, share, attribution and the per-entrant pure-human
false-positive tier are gates: they decide eligibility and carry no reward
weight. The testnet gate thresholds are provisional and will be published in
each signed cohort manifest (not yet live). A failed gate earns nothing.

**Released scores are quantized and noisy.** Exact scores never leave the
coordinator. Every released score, yours and both references', is a quantized
score under the versioned rule `quant-v1`: the coordinator adds a secret
offset and seeded discrete-Laplace noise to the exact score and rounds the
result down onto a grid of width `g`. The offset and the noise are drawn from
the cohort's private seed, its manifest and the model's inference content, so
identical copies under several hotkeys share one draw. The noise is set by
a privacy parameter ε = 2 per cohort, divided equally among every executed
entrant's score and the references', so the noise grows with the roster. `g`
is the
larger of twice `R_k`'s standard error on the batch and the noise scale,
rounded up on a versioned ladder. At 1,000 documents and the full roster of
40 entrants, the noise's standard deviation is about 0.030 and the grid is
0.03; with five entrants the noise is about 0.005. The cutoff, bands, ranks,
credit, units and the row all compare quantized scores, never exact ones. An
entrant whose attributable failures exceed the minimum-success gate's limit is
withheld: its score is drawn but not released.

**The paper rule applies on testnet.** Until some gated entrant's quantized
score reaches the margin cutoff, no miner earns: the active target is the burn
row, which gives the whole pool to the subnet owner hotkey's UID, which burns
it. The margin is `δ = max(0.01, 2σ)`, where `σ` is the pooled paired standard
error of `q` at the batch size; at 1,000 documents under the whitepaper's
20/55/25 mix, `δ` is about 0.07 of the reference's remaining loss, and each
manifest freezes its exact value. The cutoff is `R_k`'s quantized score plus
`δ` times its quantized remaining loss, rounded up onto the grid. At testnet's
full roster the 0.03 grid is wider than that margin, so the cutoff is one grid
step above `R_k`'s quantized score. Release noise cuts both ways: it can keep
an entrant that truly beats `R_k` by the margin below the cutoff, and an
entrant no better than `R_k` reaches the cutoff about 35% of the time at the
full roster. The status snapshot's mode and notice say which regime is in
force.

**Committing the burn row is live.** Both of SlopNinja's validators commit
and reveal the burn row on subnet 601 each tempo (see the
[validator guide](validator-guide.md#the-launch-burn-row)). On November 3,
2026 the chain held both validators' rows as weight 1.0 on the owner UID and
nothing for any miner UID. Expect no payout while the burn regime lasts.

When a gated entrant's quantized score reaches the cutoff:

- every gated entrant at or above the cutoff earns;
- the first retained cluster head in rank order whose exact score beats
  `R_k`'s by the margin (tested privately; only the outcome is released) and
  that also passes the promotion checks becomes the next reference, `R_{k+1}`.
  Its artifact was published when its cohort's scoring started. The
  whitepaper's promotion checks are the strict pure-human false-positive tier
  and the lookup canary. **Testnet only:** promotions on testnet run without
  the lookup canary, and the leaderboard says so. An operator drill of the
  blocking path, named in the cohort's plan, can still flag a candidate and
  block its promotion. The canary is required before mainnet.

Once a predecessor reference `R_{k-1}` exists, a cohort in which nobody
reaches the margin cutoff, but some gated entrant's quantized score is above
`R_{k-1}`'s, is scored against a fallback cutoff between `R_{k-1}`'s quantized
score and the top one, so rewards keep flowing; the reference does not
advance. In any other case the previous target stays live.

Credit among the entrants at or above the cutoff follows `reward-v2`, the
whitepaper's band rule on quantized scores. Entrants with the same inference
content, or within the near-copy distance of a cluster's head on the batch,
share one cluster. Clusters are placed
in bands at least one grid step wide (one 0.03 step at the full roster);
credit falls geometrically by 0.85 per band and per rank within a band;
clusters with equal quantized scores share the credit of their positions
equally (with bands one grid step wide, every cluster in a band ties); each member of a cluster gets an equal share of its credit; and the
shares are rounded into 65,535 integer units by largest remainder. A lone
cluster takes the whole pool; two with different quantized scores take 54.1%
and 45.9%, and two with equal ones 50% each. Rank order is quantized score,
then roster order (commitment block, then extrinsic index), and equal
remainders also go in roster order; no exact score enters it. The
whitepaper's rewards section gives every step.

**Copies earn nothing.** Every artifact published by an earlier cohort or as a
reference counts, including your own earlier entries. An entrant whose
inference content equals any published artifact's is not executed: entering
the model you entered before gets it admitted but never scored, so every
cohort needs a new model. The inference content covers every file except
`manifest.json`, so a changed export record alone does not make a new model.
One within the near-copy distance of a published artifact, or of either
reference, fails the public-copy gate unless it scores more than half a band
above it.

### Reference model

Testnet's reference is `R_0`, SlopNinja's released *A* document model,
scored in every cohort. Its quantized score on each batch is in the cohort's
published result, and it is the bar that ends the launch burn. Its
[reference package](reference-packages.md) (the six files as the scorer
loads them, the spec v3 package `3c0ecc37…22c9fe` that the cohorts name) is
published under CC BY-SA 4.0, attached to the testnet release until public
access opens ([`R_0` archives](#r_0-archives-optional)), so you can score it
locally with the same engine as your own artifact. SlopNinja's span-serving
package, span package d24 ([reference packages](reference-packages.md)), is
published when public access opens; it is not the reference.

Besides promotion, the reference changes only by an operator reference
decision with a recorded reason ([terms](terms.md#operator-decisions)). Two
are planned. Before the first cohort that enforces the span gate (a check of
each entrant's AI spans against d24's on the same batch; no cohort enforces
it yet), one signed span-equivalent reference decision passes `R_0`'s
position to span package d24, after d24 runs once as an ordinary entry, so
that its artifact is published and fingerprinted. d24's document outputs are
`R_0`'s, the decision changes no reward input, and it is not the planned
replacement. That replacement is the other: replacing `R_0` by the next
reference SlopNinja trains, only if it passes every required gate, announced
in advance on the status page. The replacement takes `R_0`'s place with no
predecessor and moves no reward already earned. An entrant that becomes the
reference was already published when its cohort's scoring started.

## 14. Refusals and what they mean

Every refusal has a stable code. `check` reports the artifact spec's codes
locally; the coordinator reports them inside `check_failed` (in
`detail.check`). `status` shows session refusals and the last refused request.

### Artifact checks (`check`, and `check_failed` at upload)

| Code | Meaning | What to do |
| --- | --- | --- |
| `object` | the object's header does not parse, or the object's length is not its header's (a file that no longer matches its header digest is refused by R2 at upload, `BadDigest`) | pack again from the model directory |
| `undeclared_file`, `missing_file` | a file other than the six, such as an external-data sidecar or notes, or one of the six missing | fix the directory and pack again |
| `contract_id`, `file_role`, `file_size` | another output contract, a wrong role, or a length outside the spec's bounds | pack with `--dir`, which sets the contract and roles; re-export the file |
| `tokenizer_mismatch` | the tokenizer is not byte-identical to the pinned one | use the pinned `tokenizer.json` |
| `json_layout` | a JSON file is not in its canonical layout (whitespace, number spelling, escapes) | write it with the release writers |
| `schema`, `binding` | a role schema violation, or files that do not agree (digests, thresholds, rosters, arm) | regenerate the JSON files from one fit |
| `graph_custom_domain`, `graph_external_data` | a custom operator domain, or external data | export with the release exporter |
| `graph_op_not_allowed`, `graph_dtype_not_allowed`, `graph_unknown_field`, `graph_unsupported`, `graph_malformed` | the graph uses an operator, element type or protobuf feature outside the allow-list, or does not parse | export with the release exporter |
| `graph_not_allowed`, `graph_class`, `graph_size` | the graph is not an allow-listed class, or does not match the role files | export the plain class in the pinned environment and check its class with `slop_ninja_artifact_check graph` (the refusal names the class of the graph's length and the first differing structure byte) |
| `graph_class_not_served` | the span class under spec v1 | check with `--spec v2` if your cohort names spec v2; otherwise export `--variant plain` |
| `too_many_parameters` | more parameters than the graph class's bound (160,000,000; spec v3's tower class 173,549,185) | use the allow-listed architecture |
| `json_budget` | the JSON files together exceed spec v2's 256 MiB, or the coordinator's own budget | remove oversized free-text provenance |
| `json_value_budget` | parsing the JSON files would hold more than the spec's value budget (for example, very many small objects or strings), or the coordinator's own | remove oversized free-text provenance; R0's files use 5.8% of the budget |

### Requests (`commit`, `upload`, `repair`, `status`)

A command that gets one of these codes prints it. The coordinator marks
`not_admitting`, `commitment_not_found`, `header_missing`, `objects_missing`
and the transient codes retryable: the client retries them where the command
allows (`--attempts`), and a command they outlast exits 2. Every other code
here exits 1.

| Code | Meaning | What to do |
| --- | --- | --- |
| `not_admitting` | no cohort is admitting | wait for the next one (`next-cohort`) |
| `after_cutoff` | admission has closed for this cohort | commit again in the next cohort's window |
| `commitment_not_found` | no commitment observed yet: none made, or not final | wait for finality; retried |
| `commitment_before_open` | the commitment was made before the cohort opened | commit again inside the window |
| `commitment_mismatch` | the opening is not the hotkey's current commitment | upload the opening of your latest commitment |
| `wrong_cohort`, `wrong_chain`, `hotkey_mismatch` | the opening names another cohort, chain, subnet or hotkey | commit again for the admitting cohort with this hotkey |
| `not_registered`, `registration_mismatch` | the hotkey has no UID, or registered again since the opening | register, then commit again |
| `second_artifact` | another live session exists for this commitment (another object for the same commitment) | upload that session's object, or commit again for the new one; `status` names the session |
| `header_too_large` | the header length is zero or over 1 MiB | pack with the release client |
| `header_missing`, `objects_missing` | the coordinator found no header, or not every file, in R2 yet | the client sends them again; rerun `upload` |
| `not_needed` | `repair` for an object that is present or no longer needed | nothing to do |
| `session_not_uploading` | the session no longer takes uploads | read `status` and act on the current session |
| `request_expired`, `rate_limited`, `paused`, `unavailable`, `internal` | transient | retried automatically; a long `paused` means the coordinator is catching up, and the cutoff moves later |
| `bad_signature`, `forbidden`, `request_validity_too_long`, `content_digest_mismatch`, `bad_request`, `unsupported_media_type`, `not_found` | a malformed or wrongly signed request, or an unknown session or cohort (`not_found`) | update the client; report it if it persists |

### Session refusals

| Code | When | Meaning |
| --- | --- | --- |
| `object_mismatch` | header | the header R2 stores is not the one your envelope names |
| `artifact_invalid` | header | the header does not decode or validate |
| `contract_mismatch` | header | the object's output contract is not the cohort's |
| `content_mismatch` | header | the header's content is not the content you committed to |
| `check_failed` | header, admission run | an artifact check refused it; see the table above |
| `object_replaced` | any | a recorded object was replaced in R2 (another entity tag or stored attribute); only a client that sends more than the signed headers can cause it |
| `commitment_overwritten` | cutoff | a later commitment before the cutoff replaced the one this session opens |
| `incomplete_at_cutoff` | cutoff | the session was not completed before the close |
| `not_registered`, `registration_mismatch` | cutoff | the registration changed before the cutoff |

After scoring, the private package records each failed gate with its reason;
the published result carries only the gate bits. An attributable failed, missing or invalid result scores zero utility for that
document and keeps its weight. A Container crash or out-of-memory failure
counts against your artifact only when it reproduces on a fresh instance with
the document scored alone; otherwise it is an infrastructure error, and
operator downtime never becomes a miner failure.

## 15. Reporting problems

Open an issue in the public repository's **Issues** tab. Include:

- `slop_ninja_miner --version`, `slop_ninja_artifact_check --version` and your
  platform;
- the command, its exit code and the refusal code;
- the cohort, your hotkey's public address and the session id from `status`.

Never post your mnemonic, keyfiles or password. Never post your artifact
before your cohort's scoring starts, or your opening before the cohort
closes: they let others enter your model. Report a
security problem privately to [security@slop.ninja](mailto:security@slop.ninja), not in a public issue
([security policy](../../SECURITY.md)).

## Reference

- [Artifact spec](artifact-spec.md): the six files, every check and its code.
- [Admission API](admission-api.md): the commitment, signed requests,
  sessions, the cutoff and every error code.
- [Miner client](../../subnet/crates/miner/README.md): every command, flag
  and environment variable; `slop_ninja_miner <command> --help` prints the
  same.
- [Validator guide](validator-guide.md) and [terms](terms.md).
- The [whitepaper](../../whitepaper/slop_ninja.pdf): rewards, cohorts and the
  reference model.
