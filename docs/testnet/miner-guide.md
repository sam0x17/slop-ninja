# Testnet miner guide

The publisher's summary v5 and allow-list v6 support the built, not yet
activated reward-v3/quant-v2 rules. The summary names both rules separately.
Under reward-v3, every gated entrant keeps its cluster label; paid cluster
members may lie below the cutoff or above their head, while unretained
clusters receive no payment. Quant-v2 adds public release SD and privacy
parameters. See [the public layout](public-layout.md#the-cohort-summary-summaryjson)
and [the dashboard contract](dashboard-contract.md) for field definitions.
Current live rules remain those named by the cohort's signed manifest.

This guide takes you from nothing to an *A* artifact that is committed on
chain and uploaded for a cohort on Bittensor testnet subnet 601. It covers the
miner client `slop_ninja_miner`, wallets and registration with rtao, training,
packing and checking, the commitment, the upload, what happens when a cohort
closes, how rewards work, and what each refusal means.

The guide describes how mining works and how to do each step. Values that
change (the current reference, cohort numbers and windows, the artifact spec a
cohort names, release versions, costs) are not written here: the
[table below](#where-to-find-current-values) says where to read each one.

Testnet has no monetary value. Read the [terms](terms.md) before you submit:
they set your submission's licence and when it becomes public
([Your submission](terms.md#your-submission)), and the
[licences page](licences.md#entrant-artifacts) says how published artifacts
are credited.

## How mining works

Miners submit models, not answers, and one hotkey enters at most one artifact
per cohort. A cohort runs like this:

1. **Register** a hotkey on subnet 601. It stays registered until the chain
   prunes it.
2. **Commit** inside a cohort's admission window: your hotkey writes a digest
   of your packed artifact on chain. The commitment binds every file by its
   SHA-256 and names exactly one cohort.
3. **Upload** the artifact straight to the coordinator's private R2 storage
   under presigned URLs. No upload byte passes through a SlopNinja Worker.
   The coordinator checks the artifact against the artifact spec the cohort
   names and answers `verified` or `refused`.
4. **Cutoff.** When the window closes, the roster freezes: verified entries
   are admitted in commitment order (finalized block, then extrinsic index) up
   to the cohort's roster cap, and the rest are `overflow`.
5. **Scoring.** When scoring starts, every admitted artifact is published on
   the public status host. The coordinator scores every admitted artifact, the
   current reference `R_k` and its predecessor, when one exists, on the same
   hidden batch.
6. **Close.** The coordinator publishes the cohort's quantized result and
   every admitted entrant's opening. A cohort pays the entrants that pass the
   gates and reach its cutoff, and the first of them in rank order that also
   beats `R_k` by the margin and passes the promotion checks becomes the next
   reference ([Rewards](#rewards)).
7. **Payment.** If the cohort produced a target, validators commit its row
   every tempo from the close until a later cohort's target replaces it.

Nothing carries over between cohorts. Every cohort needs a new commitment, a
new upload and a new model: a model you entered before has been published and
counts as a copy, which earns nothing ([Copies](#rewards)).

## Where to find current values

The coordinator's cohort read is the unsigned `GET <origin>/admission/v2/cohort`
([admission API](admission-api.md#get-admissionv2cohort-unsigned)); its origin
is the one the release trust file lists in `admission_origins`. The status API
is <https://slop.ninja/api/> (its own discovery document lists every
endpoint), and the public status host is <https://testnet.slop.ninja>
([public layout](public-layout.md)).

| Value | Where to read it |
| --- | --- |
| Whether a cohort admits now; its number, label, opening and planned cutoff; the next cohort's earliest opening and window | `slop_ninja_miner next-cohort`; the cohort read's `admitting` and `next`; the [status page](https://slop.ninja/testnet/status/) |
| The artifact spec a cohort admits by | the cohort read's `(.admitting // .next).artifact_spec.id`; `slop_ninja_miner check` names the spec it checked by |
| Whether that spec requires a token head (spans) | `slop_ninja_artifact_check --spec <id> spec \| jq .spec.span_requirements` (`null` when it does not) |
| A cohort's output contract, largest object, roster cap and upload URL policy | the cohort read's `output_contract`, `max_object_len`, `roster_cap` and `upload_policy` |
| A cohort's length, admission window, work and transition tempos; the tempo length; its expected close | status API `cohort.plan.template`, `clock.tempo_blocks`, `cohort.expected` |
| The finalized block and the confirmed epoch | the first line of `slop_ninja_miner next-cohort`; the cohort read's `finalized_block` and `confirmed_epoch` |
| Your hotkey's intake state | `slop_ninja_miner status`; status API `miners.rows[]`; the status page, with your miner set |
| The current reference and its predecessor | the status page; `v1/reference/current.json` and `v1/reference/previous.json` on the public status host |
| Every staged reference package (among them `R_0` and the span gate's anchor) | `v1/reference/packages/index.json` on the public status host, by `role` |
| Which row validators commit (a cohort's target or the burn row) | `v1/status.json` `mode` and `notice`; `v1/authorizations/current.json` |
| A closed cohort's grid width, cutoff, bands and gate bits | `v1/cohorts/<label>/summary.json`; `v1/leaderboard.json`; status API `cohorts/<label>` |
| Whether promotions run the lookup canary | `v1/leaderboard.json` `promotion_rule` |
| What the status API serves per entrant, and when | the status API's discovery document `/api/`, `notes` |
| The rules for a cohort's batch (size, mix, generator sources) | [reward rules](../agents/rewards.md#cohort-batches-and-human-stock); during scoring, status API `miners.rows[].challenges.total` is an admitted entry's document count |
| Graph classes and their parameter bounds, AI profile names, the tokenizer's digest, the token limit, the JSON budget | `slop_ninja_artifact_check --spec <id> spec`; the [artifact spec](artifact-spec.md) |
| The artifact specs your checker defines | `slop_ninja_artifact_check --version` |
| The newest release for miners, its platforms and the oldest glibc it needs | the command in [Release binaries](#release-binaries); the landing page's [release table](README.md#release-key); the release notes |
| Your rtao version; the Rust version rtao needs | `rtao --version`; `cargo install` names any minimum your toolchain misses |
| The registration cost | `rtao subnets burn-cost 601 --network test` |
| Immunity period, tempo, collateral lock share, registration floor | `rtao subnets hyperparameters 601 --network test` |
| Your UID | `rtao subnets metagraph 601 --network test` |
| The ONNX Runtime the hidden scorer uses | [scorer README](../../inference/a-container-serve/README.md) |
| A client flag's default | `slop_ninja_miner <command> --help` |
| Operator and testnet notices | `v1/status.json` `notice`; status API `site_notice` |

The same reads without the client, with `jq` and the release trust file:

```sh
HOST=https://testnet.slop.ninja
API=https://slop.ninja/api/v1/testnet/a
ORIGIN=$(jq -r '.admission_origins[0]' testnet-trust.json)   # in the release directory
curl -fsS "$ORIGIN/admission/v2/cohort" | jq '{admitting, next, confirmed_epoch}'
curl -fsS "$API/status" | jq '{template: .cohort.plan.template, close: .cohort.expected.close, tempo_blocks: .clock.tempo_blocks}'
curl -fsS "$HOST/v1/status.json" | jq '{mode, notice}'
```

## Quick path

The whole path, with the section for each step and what tells you it worked:

1. **Get test TAO** on a testnet coldkey, for the registration cost and fees
   ([What you need](#what-you-need)).
2. **Download and verify the newest release for miners**, then install its two
   binaries ([Install](#install)): `ssh-keygen` prints `Good "file" signature`,
   `sha256sum` prints `OK` for each file, and `slop_ninja_miner --version`
   prints the client's version.
3. **Install rtao** with `cargo install --locked rtao`
   ([rtao](#rtao-for-wallets-and-registration)), then **make a hotkey and
   register it on subnet 601** ([Wallet and hotkey](#wallet-and-hotkey),
   [Register](#register-on-subnet-601)):
   `rtao subnets metagraph 601 --network test` lists your hotkey with a UID.
4. **Train an artifact** with the [training guide](training-guide.md)
   ([Train an artifact](#train-an-artifact)):
   `slop_ninja_artifact_check --spec "$SPEC" check-dir` exits 0 on the result,
   with `SPEC` the spec your cohort names. The guide's recipe trains no token
   head, so its artifact passes only under a spec without
   `span_requirements`; under one with them, add a token head
   ([Spans](#rewards)).
5. **Pack and check it** with the release trust file set
   ([Pack and check](#pack-and-check), [Choose the endpoints](#choose-the-endpoints)):
   `slop_ninja_miner check` exits 0. It exits 3 when it had no cohort to check
   against: every local check passed, but the cohort's contract and size bound
   were not checked.
6. **Wait for an admitting cohort**
   ([Wait](#wait-for-an-admitting-cohort)): `slop_ninja_miner next-cohort`
   prints `admitting cohort <N>` with its planned cutoff.
7. **Commit and upload** in that cohort's window ([Commit](#commit),
   [Upload](#upload)), or let
   `slop_ninja_miner resolve --network test --artifact <object> --watch` do
   both: `upload` prints `session <id> in cohort <N>: verified` and exits 0.
8. **Follow the cohort** with `slop_ninja_miner status` and the
   [status page](https://slop.ninja/testnet/status/)
   ([Follow your entry](#follow-your-entry)). After the cutoff your entry reads
   `admitted`, or `overflow` if the roster was already full. Results are
   published when the cohort closes
   ([At the cutoff and the close](#at-the-cutoff-and-the-close)).
9. **Enter a new model next time.** An admitted artifact is published when its
   cohort's scoring starts, so entering it again earns nothing
   ([Rewards](#rewards)).

## What you need

- A Linux or macOS machine for the client. Each release's notes list the
  platforms it has binaries for and the oldest glibc they need; on any other
  platform, build the client from source.
- A Rust toolchain and a C compiler, also when you use the release binaries:
  `cargo install` builds rtao, the wallet and registration tool, from source,
  and you build the optional local scorer from source too. A current stable
  toolchain (`rustup update stable`) is enough; cargo names any minimum
  version a crate declares that yours does not meet.
- `curl`, `jq`, `sha256sum` and OpenSSH 8.1 or later (for
  `ssh-keygen -Y verify`). `python3` is needed only to download a reference
  promoted from a cohort ([below](#the-current-reference-optional)).
- A training machine as the [training guide](training-guide.md#what-you-need)
  describes.
- Free disk for about twice your packed object: the model files plus the
  object. No admissible object is larger than the cohort's `max_object_len`.
- Test TAO on a testnet coldkey, for registration and transaction fees. The
  registration cost floats: each registration raises it, and it falls back
  toward the subnet's floor over the following hours. Read it just before you
  register ([Register](#register-on-subnet-601)) and keep a little more for
  fees. SlopNinja does not hand out test TAO; get it from the Bittensor
  community's testnet sources, for example by asking on the Bittensor
  Discord (server invite <https://discord.gg/rY4XsCGrd2>).
- An uplink that sends your packed object well inside a cohort's admission
  window. `next-cohort` prints the window
  ([Wait](#wait-for-an-admitting-cohort)).

## Install

### Release binaries

Releases are on the public repository's Releases page, each signed with the
testnet release key published on the [landing page](README.md#release-key).
The landing page's release table says which release is for miners; the
newest one that carries the miner client holds its binaries
`slop_ninja_miner` and `slop_ninja_artifact_check`, the trust file
`testnet-trust.json`, `SHA256SUMS` and its signature `SHA256SUMS.sig`. Its
release notes list every file, the platform and the oldest glibc the binaries
need. This prints that release's tag:

```sh
curl -fsS https://api.github.com/repos/sam0x17/slop-ninja/releases |
  jq -r '[.[] | select(any(.assets[]; .name == "slop_ninja_miner"))][0].tag_name'
```

Download its files into a new, empty directory, with `TAG` set to that tag:

```sh
mkdir slop-ninja-release && cd slop-ninja-release
REPO=https://github.com/sam0x17/slop-ninja
TAG=<the tag printed above>
for f in slop_ninja_miner slop_ninja_artifact_check testnet-trust.json SHA256SUMS SHA256SUMS.sig; do
  curl -fLO "$REPO/releases/download/$TAG/$f"
done
```

Verify the files before you run anything, with the key and allowed-signers
line from the [landing page](README.md#release-key):

```sh
printf '%s\n' 'slop-ninja-testnet-release namespaces="file" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGCXMX0I4LTRrHt0MkCsDfg/OUeQ7Sfd0/EytxMUdYPK slop-ninja testnet release' > allowed_signers
ssh-keygen -Y verify -f allowed_signers -I slop-ninja-testnet-release -n file \
  -s SHA256SUMS.sig < SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
mkdir -p ~/.local/bin
install -m 0755 slop_ninja_miner slop_ninja_artifact_check ~/.local/bin/
slop_ninja_miner --version
slop_ninja_artifact_check --version
```

`ssh-keygen` must print `Good "file" signature for
slop-ninja-testnet-release` with the fingerprint
`SHA256:4YzEnNK8C7zjNWvasiQhv0XDJKdrnKB6L33stMQra1Q`, and `sha256sum` `OK`
for every file you downloaded (with `--ignore-missing` it skips files you
left out; check that both binaries and `testnet-trust.json` are among them).
Stop if either does not. Take the key from the landing page, not from the
release's own copy. `~/.local/bin` must be on your `PATH`, or call the
binaries by path. `slop_ninja_miner --version` prints the client's version,
and `slop_ninja_artifact_check --version` lists every artifact spec the
checker defines, with its digest: check that the spec your cohort names is
among them, and install a newer release when it is not.

Keep the release's `testnet-trust.json`: the signature covers it, and the
client reads it to decide where it may upload
([Choose the endpoints](#choose-the-endpoints)). The public status host
serves the same file at `v1/trust/testnet-trust.json`.

### Build from source

On a platform without release binaries, or to run code newer than the newest
release, build from the public repository:

```sh
git clone https://github.com/sam0x17/slop-ninja
cd slop-ninja/subnet
cargo build --release --locked -p slop_ninja_miner -p slop-ninja-artifact-check
mkdir -p ~/.local/bin
install -m 0755 target/release/slop_ninja_miner target/release/slop_ninja_artifact_check ~/.local/bin/
slop_ninja_miner --version
slop_ninja_artifact_check --version
```

To build a release's own source instead of `main`, run
`git checkout <release tag>` before the build; the landing page's release
table names the commit each release was built from. The build compiles rtao
at the version the subnet's crates pin and needs no Python or btcli; some
dependencies need a C compiler. The trust file is
`subnet/coordinator/testnet-trust.json` in the checkout.

The checker's commands: `check-dir DIR` and `check-object FILE` run the
admission checks on a directory or a packed object; `graph FILE` reports the
graph's class (by a byte comparison of its structure stream with the class's
reference stream) and its parameter count; `spec` prints the whole spec as
JSON; `canonical DIR OUT` writes a copy of a model directory with its JSON
files re-encoded in their canonical layouts and checks it. Put `--spec <id>`
before the command, with a short id such as `v3` or a full one such as
`slop_ninja_a_artifact_spec_v3`. Always pass it: without it the checker uses
the default its usage line names (`slop_ninja_artifact_check --help`), which
need not be the spec your cohort names. Its `pack DIR OUT` and
`files DIR OUT` are operator tools: to pack an artifact, use
`slop_ninja_miner pack`.

### rtao, for wallets and registration

The client signs with an ordinary Bittensor hotkey file. Install the newest
rtao command line tool from crates.io:

```sh
cargo install --locked rtao
rtao --version
```

If cargo stops with "binary `rtao` already exists in destination", an older
install under the package name `rtao-cli` holds the name: run
`cargo uninstall rtao-cli`, then install again. If cargo names a minimum Rust
version your toolchain misses, run `rustup update stable`.

rtao reads and writes wallets in the standard Bittensor layout
(`~/.bittensor/wallets/<wallet>/hotkeys/<hotkey>` with its
`<hotkey>pub.txt`), so a wallet another tool wrote works as it is.

## Wallet and hotkey

The coldkey holds funds and pays for registration. The hotkey is the miner's
identity: it signs the commitment and every upload request. Use a fresh hotkey
for testnet. The names below are examples.

```sh
rtao wallet create --wallet my-wallet --wallet-hotkey my-miner
rtao wallet show --wallet my-wallet --wallet-hotkey my-miner
```

`wallet create` asks for a coldkey password, writes an encrypted coldkey and
an unencrypted hotkey, and prints a recovery mnemonic for each. Store them
offline. `rtao wallet new-hotkey --wallet my-wallet --wallet-hotkey <name>`
adds another hotkey later. Fund the coldkey's address with test TAO.

`slop_ninja_miner` reads the hotkey with `--wallet my-wallet --hotkey my-miner`
(or `SLOP_NINJA_WALLET` and `SLOP_NINJA_HOTKEY`) under `--wallet-path`
(`SLOP_NINJA_WALLET_PATH`), which defaults to `~/.bittensor/wallets`, or from
`--hotkey-file PATH`. It never needs the coldkey.

## Register on subnet 601

```sh
rtao subnets burn-cost 601 --network test
rtao subnets register --help
rtao subnets register --network test --netuid 601 --wallet my-wallet --wallet-hotkey my-miner
rtao subnets metagraph 601 --network test
```

The coldkey pays the registration cost the chain charges when the call
executes; `burn-cost` reads it just before. `rtao subnets register --help`
describes the call in your rtao version: what it charges, how it treats a
spend cap you configured, and how it is submitted. Read it before you
register. When the subnet's collateral lock share (`collateral_lock_share` in
`rtao subnets hyperparameters 601 --network test`) is above zero, that share
of the cost is staked to your hotkey as miner collateral instead of burned.
Keep some free TAO on the coldkey beyond the cost for fees. The metagraph
shows your hotkey's UID once the registration is final.

Facts that matter for a miner:

- **Immunity.** A new registration is immune from pruning for the subnet's
  immunity period (`immunity_period`, in blocks, in the hyperparameters). A
  commitment counts only inside its cohort's admission window, and a cohort
  pays only from its close, so if you earn, your first payout comes after your
  cohort's close (status API `cohort.expected.close` estimates it). Immunity
  is not absolute: the chain's pruning rules can still prune an immune
  neuron when too few non-immune UIDs are left.
- **Pruning.** Once the subnet's UID slots are full, a new registration
  replaces a non-immune neuron with low emission, by the chain's rules. A
  miner the active target does not credit has zero emission, so it can be
  replaced once it is past immunity.
- **Registration generation.** Credit follows your hotkey and its registration
  block. If your hotkey is pruned and registers again, it starts a new
  generation: an opening made under the old one no longer counts, and credit
  still awaiting payout under the old one burns.

## Train an artifact

Follow the [training guide](training-guide.md), which trains an artifact
from ModernBERT-base and public data and ends with `pack` and `check`. Its
recipe trains no token head, so the artifact is admissible only under a spec
without `span_requirements` ([Spans](#rewards)). The current reference's
published files let you score locally against the model you have to beat,
and `R_0`'s package and training companion let you start from R0's encoder
([Optional local tools](#optional-local-tools)).

The release trainer (`grammar/crates/grammar-eval/python/train_full_a.py`)
writes a fit directory. The training guide supplies its public parent,
calibration, release files, graph export and assembly steps. Its optional
[teacher classes](../a-teacher-training.md) support fresh document heads and
frozen-document span experiments, but teacher fits have no artifact export
path. Use the training guide for an admissible submission.

The result is a directory holding the files the [artifact spec](artifact-spec.md)
lists for the output contract the cohort names (`output_contract` in the
cohort read). Under `slop_ninja_full_a_contract_v2` they are exactly six:

| File | Role |
| --- | --- |
| `a-fp32band.onnx` | the graph |
| `heads.json` | the head weights |
| `contract.json` | the output schema |
| `calibration.json` | the calibration |
| `manifest.json` | the export record (owner metadata; not part of the inference content) |
| `tokenizer.json` | the tokenizer |

The artifact spec is the authority, and `slop_ninja_artifact_check --spec <id>
spec` prints the values of the spec your cohort names. Set `SPEC` to that id
from the cohort read (`ORIGIN` as in
[Where to find current values](#where-to-find-current-values)):

```sh
SPEC=$(curl -fsS "$ORIGIN/admission/v2/cohort" | jq -r '(.admitting // .next).artifact_spec.id')
echo "$SPEC"
slop_ninja_artifact_check --spec "$SPEC" spec | jq '.spec.graph.classes[] | {id, max_parameters}'
slop_ninja_artifact_check --spec "$SPEC" spec | jq '{tokenizer: .spec.tokenizer, max_tokens: .spec.pinned_contract.maximum_tokens_including_specials, max_json_bytes: .spec.max_json_bytes, span_requirements: .spec.span_requirements}'
```

`null` for the spec means the coordinator has not named the next cohort's
plan: read it again later. The points that most often decide admission:

- **The graph class.** The graph must have, byte for byte, the structure of
  one allow-listed class the spec serves
  ([Allow-listed graph classes](artifact-spec.md#allow-listed-graph-classes)).
  Only the values of its initializer weights may differ, and no two weights
  may be equal: the exporter merges initializers with equal values, which
  changes the structure. A class with a span tower starts its tower as copies
  of encoder tensors, so every tower tensor must end up differing from the
  one it was copied from; `export_a_onnx.py` refuses such a model before
  export and names the equal tensors. Export in the environment the spec
  names ([Export](artifact-spec.md#export)); the exporter refuses any other.
  Confirm the class with
  `slop_ninja_artifact_check --spec "$SPEC" graph a-fp32band.onnx`:
  `graph_class` names it when the structure matches, and otherwise
  `structure_difference` is the stream offset of the first differing byte.
- **Parameters.** Each class has a parameter bound derived from its
  structure (`max_parameters` above). Any file the spec does not list, an
  external-data sidecar included, is refused.
- **Rosters are fixed**, because the graph contains the document heads. The
  AI profile names, in order, are in the spec
  (`.spec.pinned_contract.ai_profiles`) and in
  [AI profile names](artifact-spec.md#ai-profile-names); `contract.json` and
  `heads.json` must carry exactly those.
- **The tokenizer** must be byte-identical to the file the spec pins
  (`.spec.tokenizer`: its SHA-256 and length). It is not in the repository,
  but it is in `R_0`'s package and, as a file of every published artifact, on
  the public status host at `blobs/sha256/<its SHA-256>`; check its SHA-256
  after you download it. The upstream ModernBERT-base `tokenizer.json` is
  refused as it is (`tokenizer_mismatch`); the spec's
  [Tokenizer](artifact-spec.md#tokenizer) section shows how to rebuild the
  pinned bytes from it.
- **JSON files** must be in their canonical layouts and pass the role schemas,
  including the pinned contract fields and the calibration policy.
- **Input.** Cohorts feed raw text, with nothing truncated. The scorer accepts
  at most the spec's token limit, special tokens included
  (`maximum_tokens_including_specials`), and answers a longer document
  `unsupported_length`, for every artifact alike. The spec's canonical text
  profile records exactly what the scorer does to a document.
- **Spans.** When the spec has `span_requirements`, admission refuses an
  artifact without a token-level head whose decoder never emits a span under
  the floor it names ([Spans](#rewards)).

Check the directory before you pack it, under the spec your cohort names:

```sh
slop_ninja_artifact_check --spec "$SPEC" check-dir ./my-artifact
```

A pass prints the report as JSON (the spec, the inference-content digest
`content_sha256`, the graph class and its report, the role files and the
parameter count) and exits 0. A refusal prints `refused <code>: <detail>` and
exits 2; a path it cannot read is refused as `object`. A usage error exits
64, and `--help` exits 0.

There is no small example artifact that passes. The graph class fixes the
graph's structure and length to those of a real export, and the role files
must agree with it, so only a real export can pass. A directory of
placeholder files still exercises the tools, but reaches only refusals. The
shortest path to a passing artifact is the training guide's
[smoke run](training-guide.md#smoke-run).

## Pack and check

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
| 1 | refused: an admission check refused the artifact (`check` prints the code the coordinator would report), the upload session ended `refused`, or the coordinator refused a request with a code that retrying cannot change (for example `not_registered` for a signed `status` read); the code is printed ([Refusals](#refusals-and-what-they-mean)) |
| 2 | error: usage, configuration, network or file; a check the client makes itself before it signs anything (for example `commit` while no cohort is admitting); or a retryable answer, such as `unavailable` or `commitment_not_found`, still there after the attempts. Nothing was decided about the artifact |
| 3 | `check` only: nothing refused the artifact, but some checks did not run |

With the endpoints set ([Choose the endpoints](#choose-the-endpoints);
`SLOP_NINJA_TRUST` alone is enough), `check` reads the admitting cohort from
the coordinator's cohort read, checks its output contract, size bound and
artifact spec, and exits 0 on a pass. Between windows it uses the next
cohort's values when the cohort read names them, with the note `next_cohort`
naming that cohort: check again once it admits. Otherwise, or without a
coordinator, it still runs every local check, exits 3 when nothing refused
the artifact, and names the reason in the note `cohort_unknown`.

The notes say which spec `check` used. `admitted_by_spec` gives the graph
class and profile the spec admitted the artifact as. With no cohort to read,
`check` uses its build's default spec and says so in `artifact_spec_assumed`.
`artifact_spec_mismatch` means the cohort names a spec your build does not
define: install a newer release.

The JSON files are published with the artifact when its cohort's scoring
starts. If one holds a string or key that reads as an absolute local path
(one that starts with `/`, `~/` or a Windows drive), carries one (a home
directory or a file URL in text) or names a path under a `data/` tree, it
would publish your directory layout, user name included. `pack` warns about
it on stderr and `check` reports `[WARNING] absolute_path` with the JSON
pointers of those strings (never their values). A warning never refuses the
artifact and does not change the exit status. The check can miss a path
written another way, so read your JSON files before you publish them. The
repository's tools record portable paths (resolved against the working
directory: repository-relative inside the repository, the final component
elsewhere): run them as the [training guide](training-guide.md#steps) does,
then export and pack again. A contract built from a normalization with
absolute run paths keeps them, because the fit binds the contract's digest:
rebuild the normalization and the contract before training.

## Optional local tools

### Score it locally (optional)

`slop_ninja_a_local_score` runs your artifact through the hidden scorer's own
engine: the same admission, load guards, graph contract and heads runtime.
The release does not include it. Build it with Rust from a checkout of the
repository and give it the ONNX Runtime library the hidden scorer uses, the
version its [README](../../inference/a-container-serve/README.md) names. The
training guide's Python environment installs an `onnxruntime` wheel whose
library you can use
(`venv/lib/python3.12/site-packages/onnxruntime/capi/libonnxruntime.so.<version>`
under the guide's working directory); check that its version is the
scorer's.

```sh
cargo build --release --locked --manifest-path inference/a-container-serve/Cargo.toml \
  --bin slop_ninja_a_local_score
inference/a-container-serve/target/release/slop_ninja_a_local_score \
  --model-dir ./my-artifact --docs my-docs.jsonl --out my-results.jsonl \
  --spec "$SPEC" --ort-lib /path/to/libonnxruntime.so.<version>
```

Always pass `--spec`: for your own artifact the spec your cohort names, and
for a reference a spec that admits it
([The current reference](#the-current-reference-optional)). Without it the
local scorer admits by its build's default.

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

### The current reference (optional)

To score against the current reference `R_k` locally, download its files.
The public status host's `v1/reference/current.json` (and `previous.json`
for its predecessor) names the reference by inference-content digest
(`content`) and artifact header digest (`artifact`), and says where its files
are:

- `path`: the reference was promoted from a cohort, and `path` is its
  published header; the header lists its files, each at
  `blobs/sha256/<file sha256>`. Use the commands below.
- `package`: the reference is a staged [reference package](reference-packages.md)
  whose manifest lists the header `artifact` names; download
  it as [Reference packages](#reference-packages-optional) shows, selecting
  it in the package index by that path.
- neither: `withheld` says why. The [package index](#reference-packages-optional)
  still lists every staged package by role.

For a promoted reference (needs `jq` and `python3`, and disk for about one
object):

```sh
HOST=https://testnet.slop.ninja
curl -fsS -o ref-current.json "$HOST/v1/reference/current.json"
jq -r '.path, .package, .withheld' ref-current.json
H=$(jq -r .artifact ref-current.json)
curl -fsS -o ref-header "$HOST/artifacts/v2/$H/header"
echo "$H  ref-header" | sha256sum -c
# list each file's SHA-256 and name from the header:
python3 -I -c '
import struct, sys
b = open(sys.argv[1], "rb").read()
def text(o):
    n = struct.unpack_from("<I", b, o)[0]
    return b[o + 4:o + 4 + n].decode(), o + 4 + n
_, o = text(0); o += 2; _, o = text(o)  # record kind, version, output contract
(count,) = struct.unpack_from("<I", b, o); o += 4
for _ in range(count):  # each file: name, role byte, SHA-256, u64 length
    name, o = text(o)
    print(b[o + 1:o + 33].hex(), name)
    o += 41
' ref-header > ref-files.txt
mkdir ref-artifact
while read -r h name; do curl -fsS -o "ref-artifact/$name" "$HOST/blobs/sha256/$h"; done < ref-files.txt
sed 's#^\([0-9a-f]*\) #\1  ref-artifact/#' ref-files.txt | sha256sum -c
REFSPEC=$SPEC   # or another spec that admits the reference; see below
slop_ninja_artifact_check --spec "$REFSPEC" check-dir ref-artifact | jq -r .content_sha256
jq -r .content ref-current.json
```

`sha256sum -c` must print `OK` for the header and for every file, and the
last two commands must print the same inference-content digest. Check and
score a reference under a spec that admits it. That is `$SPEC`, unless
`$SPEC` has `span_requirements` and the reference has no token head: then
`check-dir` refuses it with `token_head_required` and prints no digest, so
set `REFSPEC` to the newest spec your checker defines without them
(`slop_ninja_artifact_check --version` lists the specs, and
`slop_ninja_artifact_check --spec <id> spec | jq .spec.span_requirements`
prints `null` for one without them). `ref-artifact` is then the directory to
give the [local scorer](#score-it-locally-optional), with
`--spec "$REFSPEC"`. A promoted entrant's artifact was published when its
cohort's scoring started, under the CC BY-SA 4.0 licence its miner
granted. The published artifacts' licence notice, which says how
to attribute one and gives the base model's licence, sits beside them under
`artifacts/v2/` on the public status host
([licences](licences.md#entrant-artifacts); its text is
[`artifacts-v2-NOTICE.md`](../../subnet/publisher/notices/artifacts-v2-NOTICE.md)
in this repository). Do not enter the reference's files: an entry with a published artifact's inference content
is admitted but never executed ([Copies](#rewards)).

### Reference packages (optional)

SlopNinja publishes reference packages on the public status host
([reference packages](reference-packages.md)). The package index lists every
staged package by `role`, among them `R_0`, the first reference, with its
training companion, and span package d24 (role `span_head`), the span gate's
fixed anchor ([Spans](#rewards)). Which artifact is the reference, and which
its predecessor, is what `v1/reference/current.json` and `previous.json` say
([The current reference](#the-current-reference-optional)). Each index entry
gives the package digest, the inference-content digest, the graph class, the
specs that admit it and the `path`. To download one in the parts layout and
check it (needs `jq`):

```sh
HOST=https://testnet.slop.ninja
ROLE=R_0   # or another role the index lists, such as span_head
curl -fsS "$HOST/v1/reference/packages/index.json" |
  jq --arg r "$ROLE" '.packages[] | select(.role == $r)' > pkg.json
# or, for a reference pointer that names a package (ref-current.json as above):
# curl -fsS "$HOST/v1/reference/packages/index.json" |
#   jq --arg p "$(jq -r .package ref-current.json)" '.packages[] | select(.package_sha256 as $d | $p | contains($d))' > pkg.json
P=$HOST/$(jq -r .path pkg.json)
curl -fsS -o pkg-files.json "${P%/}/artifact-files.json"
echo "$(jq -r .package_sha256 pkg.json)  pkg-files.json" | sha256sum -c
mkdir pkg-artifact
jq -r '.files[] | .name as $n | .parts[] | "\(.sha256) \($n)"' pkg-files.json |
  while read -r h name; do curl -fsS "${P%/}/parts/$h" >> "pkg-artifact/$name"; done
jq -r '.files[] | "\(.sha256)  pkg-artifact/\(.name)"' pkg-files.json | sha256sum -c
slop_ninja_artifact_check --spec "$(jq -r '.specs[-1]' pkg.json)" check-dir pkg-artifact | jq -r .content_sha256
jq -r .content_sha256 pkg.json
```

`sha256sum -c` must print `OK` for the parts manifest, whose SHA-256 is the
package digest, and for every file, and the last two commands must print the
same inference-content digest. The rebuild appends to the files it writes,
so run it once, into the new directory. The package's `NOTICE.md`, which
says how to credit it, and `upstream/ModernBERT-base-LICENSE` are under the
same path, and `package.json` there describes the package.

### `R_0` archives (optional)

`R_0`'s package in the files layout and its training companion are also
attached to the release `testnet-v0.1.0` as two tar archives,
`slop-ninja-r0-reference-package-3c0ecc37.tar` and
`slop-ninja-r0-training-companion-3c0ecc37.tar`, about 1.2 GB together. You
need them, or the companion's parts under `R_0`'s package path on the public
status host, only to start training from R0's encoder
([Warm start from R0](training-guide.md#warm-start-from-r0)). Both are
licensed CC BY-SA 4.0; the package's `NOTICE.md`, which also covers the
companion, says how to credit R0 and its base model. The companion archive
does not carry its own copy of that notice or of the base model's licence:
both are in the reference-package archive, as `NOTICE.md` and
`upstream/ModernBERT-base-LICENSE`, so keep the two archives together when
you pass the companion on.

The archives hold R0's artifact spec v3 package. Its digest is the SHA-256 of
its files manifest, `artifact-files.json` (`slop_ninja_a_artifact_files_v2`),
and both archives are rooted at that digest. Extracted into one directory they
give the manifest; `files/<sha256>`, the six model files, each whole and named
by its SHA-256; `NOTICE.md` and `upstream/ModernBERT-base-LICENSE`;
`reference/`, the synthetic reference cases R0's export manifest binds, with
their FP64 predictions; `provenance-rewrite.json`
([reference packages](reference-packages.md#provenance-rewrite)); and, from
the companion archive, `training/companion.json` and `training/parts/<sha256>`,
R0's encoder and its classifier config as 64 MiB parts.

Download them with the release's `SHA256SUMS` and verify them as
[above](#release-binaries):

```sh
mkdir slop-ninja-r0 && cd slop-ninja-r0
REPO=https://github.com/sam0x17/slop-ninja
for f in slop-ninja-r0-reference-package-3c0ecc37.tar slop-ninja-r0-training-companion-3c0ecc37.tar SHA256SUMS SHA256SUMS.sig; do
  curl -fLO "$REPO/releases/download/testnet-v0.1.0/$f"
done
```

Run the `printf`, `ssh-keygen` and `sha256sum -c --ignore-missing` lines of
[Release binaries](#release-binaries) in this directory. Then extract the
archives and check the package digest and every file and part:

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
file's parts in the listed order, and check both. Extracting and rebuilding
take about 2.4 GB of disk beside the archives.

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
creates. `../r0-artifact` is a directory to give the
[local scorer](#score-it-locally-optional), and
`../r0-companion/model/encoder.safetensors` is the encoder for the warm start.
Do not enter R0's files as they are: R0 is a published reference, so an
entry with its inference content is admitted but never executed
([Copies](#rewards)).

## Choose the endpoints

Set these once per shell:

```sh
export SLOP_NINJA_TRUST=/path/to/testnet-trust.json   # from the release
export SLOP_NINJA_WALLET=my-wallet SLOP_NINJA_HOTKEY=my-miner
# optional: export SLOP_NINJA_PUBLIC=https://testnet.slop.ninja   # the public status host; see below
```

An upload sends your opening, with its secret nonce, in signed requests,
and the coordinator answers with presigned URLs for your artifact's objects,
so the client sends signed requests only to an authenticated coordinator
origin: the one the trust file lists in `admission_origins`
(`jq -r '.admission_origins[]' "$SLOP_NINJA_TRUST"` prints it).

- With `SLOP_NINJA_TRUST` set and neither `--coordinator` nor `--public`,
  `check`, `commit`, `upload`, `repair`, `status` and `next-cohort` use the
  origin the trust file pins. This path needs only the coordinator.
- `--coordinator <origin>` (or `SLOP_NINJA_COORDINATOR`) names that origin
  explicitly. Any other origin is refused while the trust file pins one.
- `--public https://testnet.slop.ninja` (or `SLOP_NINJA_PUBLIC`) alone also
  works: the client takes the origin from the status snapshot's `coordinator`
  field, which it accepts only because the trust file pins it. Without
  `--coordinator`, a command given `--public` then needs the public status
  host to answer as well as the coordinator, uploads included; leave it unset
  to use the pinned origin directly.

Never take an upload origin from a chat message or from the unsigned
snapshot alone. The client prints the origin before it opens a session, never
follows redirects, and accepts plain HTTP only for loopback hosts.

**Your artifact's bytes go straight to R2**, the coordinator's private object
storage, never through a SlopNinja Worker: one presigned PUT for the header
and one for each file. The client needs outbound HTTPS to the coordinator
origin and to the R2 S3 host the URLs name. Before it sends a byte it checks
each URL: `https`; the host and bucket the release pins (the trust file's
`upload_host_sha256`, the SHA-256 of the R2 S3 host's name, and its
`upload_bucket`; the client compiles the release's pins in, so they apply
even without `--trust`, unless you pass `--any-upload-host` for a deployment
other than the release's); the object key it derives itself from the cohort,
your commitment and the file's digest; and the signed headers. An error
message names the object and the host, never the URL's signed query. The URL
shows the storage account's host, the bucket name and the signing key's id;
none of these is a credential.

The coordinator checks every signed request before it reads the body or
forwards anything: the signature must verify, and the hotkey must hold a UID
on subnet 601 at the finalized head. A hotkey registered in the last few
blocks may therefore hear `not_registered` until finality catches up; send
again a block later. A request from an unregistered hotkey is refused with
`not_registered`, so register before you commit. There is one session per
commitment and one artifact per hotkey per cohort, and sizes are bounded by
the artifact spec.

## Wait for an admitting cohort

```sh
slop_ninja_miner next-cohort
```

`next-cohort` prints the admitting cohort and its planned cutoff epoch, and the
next cohort with its earliest opening epoch and how long its admission window
lasts, each with its distance from the confirmed epoch, for example
`planned cutoff at epoch <E> (<n> epochs after the confirmed epoch)`. Without
`--public` it reads the coordinator's cohort read from the origin the trust
file pins, or from `--coordinator`; its first line names that origin, the
finalized block and the coordinator's confirmed epoch. With
`--public https://testnet.slop.ninja` it reads the public status snapshot
(`v1/status.json`) instead. A coordinator that reports no confirmed epoch
gets absolute epochs only. The same read without the client:

```sh
curl -fsS "$(jq -r '.admission_origins[0]' "$SLOP_NINJA_TRUST")/admission/v2/cohort" | jq .
```

Its `admitting` is `null` outside a window, and `next` names the next cohort
and its `earliest_open_epoch`. When the coordinator knows the next cohort's
plan, `next` also carries its `admission_tempos` (then `next-cohort` prints the
window), `output_contract`, `max_object_len` and `artifact_spec` (then
`check` compares with them between windows). The earliest opening is a
lower bound: the next cohort opens at the boundary that closes the one before
it, which slow work or a coordinator outage moves later.

Cohort times are subnet epoch indices: the chain's `SubnetEpochIndex` for
subnet 601, which advances by one at each tempo boundary. It is a per-subnet
counter, not the block number divided by the tempo length. An epoch that lies
*N* epochs after the confirmed one starts roughly *N* tempos after the
confirmed epoch started (the status API's `clock.tempo_blocks` gives the tempo
length in blocks). Boundaries can slip, so treat these as estimates and read
`next-cohort` again as the time approaches. The status API's
`cohort.expected` gives estimated times, which are lower bounds: a cohort can
run later, never earlier.

A cohort lasts at least the number of tempos its plan fixes, frozen with the
cohort when it is queued, and opens at the tempo boundary that closes the one
before it; its admission window opens with it. After the window, the cohort
freezes its roster, generates, scores and packages, publishes, and closes
after a transition of at least two tempos, when the next cohort opens. The
status API's `cohort.plan.template` gives the cohort's length, window, work
window and transition in tempos, and `cohort.plan.stages` its stages. No
cohort admits between the end of one window and the next cohort's opening.
Trust what `next-cohort` prints over any figure you remember, and treat the
cutoff it prints as planned, not promised.

The window is sized from measurements so that the largest object a cohort
admits uploads well within it. Your own uplink sets your time; a slow one
still fits unless you start near the window's end.

A commitment counts for a cohort only when its block is at or after the
cohort's opening block and before its cutoff block. A commitment made earlier
is refused with `commitment_before_open`, so you cannot reserve a place ahead
of time: wait for `next-cohort` to show the cohort admitting, then commit for
it. During a coordinator outage the cutoff moves later, never earlier, and the
cohort is never skipped.

Have the artifact packed and checked before you commit. An upload can start
only once the commitment is final and the coordinator has observed it, and it
must complete before the cutoff, so commit and upload together rather than
committing early and uploading near the cutoff. The public snapshot's
`admitting` field is set only while the window is open; once it closes the
cohort's phase stays `admission` until the roster freezes, but nothing more is
admitted.

Right after `commit`, `upload` may hear `commitment_not_found` with
`Retry-After` while the coordinator has not yet observed the commitment; it
retries as told (`--attempts`). `upload` sends the block at which `commit`
confirmed your commitment, so the coordinator keeps answering
`commitment_not_found` until its reader has polled at or past that block,
even when the coordinator is behind the chain. A definitive answer
(`commitment_before_open`, `commitment_mismatch`, `not_registered`,
`registration_mismatch`) comes at the reader's next completed poll after
that, when the opening really is wrong. If the retries run out first,
`upload` stops with `commitment_not_found`; run it again later. Without
`--opening`, `upload` picks the newest opening this wallet confirmed on chain
when it is newer than the commitment the coordinator observed, and refuses
locally when that opening is for another artifact than `--artifact` (after
`commit --replace`, upload the new artifact).

## Commit

```sh
slop_ninja_miner commit --network test --artifact ./my-artifact.bin --dry-run
slop_ninja_miner commit --network test --artifact ./my-artifact.bin
```

Both forms need only the coordinator's cohort read and the chain; nothing
here uses a signed admission route. Commit only when the artifact is packed
and checked and you can upload it in the window. `commit` reads the admitting
cohort from the cohort read and your registration from finalized chain state,
makes a fresh secret 256-bit nonce and the opening, and stores the opening in
the state directory with mode 0600 before it signs anything. It then signs
`Commitments.set_commitment(601, [Raw44(payload)])` with the hotkey, where the
44-byte payload is `"sna1" || u64_le(cohort) || commitment digest`
([The commitment](admission-api.md#2-the-commitment)), and reads the
commitment back at finality. `--dry-run` prints the opening and the call and
stores, signs and submits nothing. It also reads your registration: for a
hotkey without a UID it stops with `<address> is not registered on subnet 601
at finalized block <B>` (exit 2). Always pass `--network test`, as the
commands here do, rather than rely on the default
(`slop_ninja_miner commit --help` prints it); `--rpc` names endpoints.

Rules:

- **One cohort per opening; commit again every cohort.** An opening names
  exactly one cohort and can never be carried to a later one. To enter the
  next cohort, commit again inside its window and upload again.
- **A new model every cohort.** Once your artifact is admitted, it is
  published when its cohort's scoring starts, and every published artifact
  counts as a copy in every later cohort, your own included. Entering the
  same model again (the same inference content) gets it admitted but not
  executed, so it earns nothing; a near copy of it fails the public-copy gate
  ([Copies](#rewards)). Train or change the model before each commitment.
- **The last commitment counts.** The chain keeps one commitment per hotkey; a
  new one overwrites the old and moves you to the new position. `commit`
  refuses a second opening for the same cohort unless you pass `--replace`.
- **Position.** Your admission position is the finalized block and extrinsic
  index of your commitment. Under the roster cap, earlier positions win.
- **Space.** The chain limits each account's commitment space per epoch; a
  call over it fails with `SpaceLimitExceeded`.

The state directory is `--state-dir`, `SLOP_NINJA_MINER_HOME`, else
`$XDG_DATA_HOME/slop_ninja_miner`, `~/.local/share/slop_ninja_miner`, or
`~/Library/Application Support/slop_ninja_miner` on macOS. Its directories are
mode 0700 and its files mode 0600; the client refuses a file that group or
others can read. Back it up until the cohort closes: an upload needs the
opening.

## Upload

```sh
slop_ninja_miner upload --artifact ./my-artifact.bin
```

`upload` opens a session for your opening, PUTs the object's header straight
to R2 with the URL the session returns, asks for the file URLs (the
coordinator checks the header first, so an artifact doomed by its header is
refused before any file is sent), PUTs each file straight to R2, largest
first, completes the session and waits while the coordinator verifies it. It
picks the local opening whose payload matches the commitment the coordinator
observed; `--opening FILE` names one. Progress lines on standard error show
each file's bytes, rate and time left (`--quiet` turns them off).
`slop_ninja_miner upload --help` prints every flag with its default.

- **Resume is per file.** A single PUT cannot resume, so an interrupted file is
  sent again from its first byte; files R2 already holds are not sent again.
  Rerun the same command to resume.
- **Each URL lives long enough** for your whole artifact at the cohort's
  upload-policy rate plus its skew allowance (`upload_policy` in the cohort
  read: `b_min` in bytes per second and `skew_secs`;
  [Direct upload](admission-api.md#6-direct-upload)). R2 checks a URL's
  expiry when the PUT starts, so a file whose PUT started in time finishes at
  any speed. Expiry never ends anything: the client asks for a fresh URL
  whenever one expires before its PUT starts. A file's PUT times out locally
  after `--http-timeout-secs` plus its size at `--min-upload-kib-per-sec`;
  raise that rate on a fast link, lower it on a slow one.
- **Retries** (`--attempts`, per request and per file): a dropped
  connection, a 5xx or 429 from R2 gets a fresh URL and the file goes again
  after a doubling wait. 412 means R2 already holds the file. A busy
  coordinator answers the contract's `unavailable` with `Retry-After`; an
  edge error page without a contract body (502, 503, 504, or a Cloudflare
  52x) is retried the same way.
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
  cutoff. After sending, it asks the coordinator to check the objects again
  and exits 0 once none is missing; it reports an object as still missing
  only after that check. Keep the object until your cohort's scoring starts.

The session ends as `verified` or `refused`.

**How you know it worked.** Once the coordinator has verified the upload,
`upload` prints `session <id> in cohort <N>: verified; position block <B>
extrinsic <E>` and exits 0. `verified` means the artifact passed every
admission check; whether it is admitted is decided at the cutoff, by
commitment position when the roster freezes
([At the cutoff and the close](#at-the-cutoff-and-the-close)). If the
coordinator is still verifying when `--wait-secs` runs out, `upload` prints
`verifying` and also exits 0: read `status` later. A refused session prints
its code and exits 1.

## Follow your entry

```sh
slop_ninja_miner status
slop_ninja_miner status --ss58 <your hotkey address>   # public data only, no wallet
```

With a hotkey named (your wallet, or `--ss58`), `status` first answers the
questions a miner has about the admitting or current cohort, each with
`yes`, `NO`, `not yet` or `unknown` and its source:

```text
hotkey 5F… in cohort <N> (<label>): uploading
  registered?          yes      uid <uid>, hotkey 5F…k2Qz (status API)
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
until your entry's scoring starts and then `<done> of <total>` counts. The
answers come from the [status API](#the-status-api) (unsigned and refreshed
about once a minute, so a change shows up a minute or two late), the
coordinator's cohort read, its signed view of your hotkey when your wallet is
named, and your local openings. `status` reads no chain state and changes
nothing. `--json` prints the answers as one JSON object; `--watch` reads
again about once a minute and prints them when they change (`--max-wait-secs`
bounds it). A hotkey without a UID answers `registered?` `NO` (from the
status API's registry); check your registration with
`rtao subnets metagraph 601 --network test`. With your wallet named, the
coordinator's view at the end of `status` answers `not_registered` (exit 1)
for such a hotkey. An answer the status API has not published for your
hotkey yet reads `unknown`.

After the answers (with your wallet named; `--ss58` and `--json` print the
answers only), `status` prints the public snapshot (unsigned, and labelled
so) or, without `--public`, the same calendar from the coordinator's cohort
read (as `next-cohort` does), your local openings, and the coordinator's view
of your hotkey for the admitting cohort or `--cohort N`: registration, the
commitment that counts and whether it is in the window, each session with its
state, its recorded objects, its refusal and any objects to repair, and the
last refused request. With no openings it says so and names the state
directory it read. The local openings are always printed: when the cohort
read or the public snapshot cannot be read (a coordinator outage, an
unreadable trust file), `status` prints them first, then reports the failure
and exits 2. Without any endpoint (no `--coordinator`, no `--public` and no
trust file that pins one origin) it prints only the local openings and a line
saying the cohort and the coordinator's view were not read; without a named
hotkey it skips the coordinator's view and says so. A signed read the
coordinator refuses (for example `not_registered` for a hotkey without a UID)
exits 1 with the code.

Session states: `uploading`, `verifying`, `verified`, `refused`, `admitted`,
`overflow` and `superseded` (a newer commitment replaced it).

### The status API

The [status page](https://slop.ninja/testnet/status/)'s tracker links to a
read-only JSON API that needs no key and allows any origin (CORS):

```sh
curl -fsS https://slop.ninja/api/v1/testnet/a/status          # the current cohort
curl -fsS https://slop.ninja/api/v1/testnet/a/cohorts         # every listed cohort
curl -fsS https://slop.ninja/api/v1/testnet/a/cohorts/<label> # one cohort
curl -fsS https://slop.ninja/api/                             # endpoints, schemas and notes
```

A cohort's answer has its stage and phase history, its plan, tempos
credited and required, admission progress, the work percent, the expected
stage end and close with estimated times, the pause state, the references and
the miner list with each hotkey's status for the cohort. The estimated times
are lower bounds: a cohort can run later, never earlier. The API is unsigned.
It shows one status per hotkey (`committed`, `uploading`, `verifying`,
`verified`, `refused` with a class, `overflow`, `admitted`, `evaluating`,
then `scored`, `withheld` or `not_executed`); use `status` for your own
sessions' details. While a cohort is scored, each admitted entrant's row also
has `challenges` (`pending` until its scoring starts, then `done` of
`total`): counts only, updated about once a minute. The discovery document's
`notes` say exactly which per-entrant values the API serves and when. Set
your miner on the status page to see a checklist for your hotkey:
registered, commitment seen, upload verified, admitted, challenges started,
and how many are done, each with its next step; `status` in the client
answers the same questions. The fields and their sources are in the
[public layout](public-layout.md#the-public-status-api-slopninjaapi).

### Resolve intake automatically

```sh
slop_ninja_miner resolve --network test --artifact ./my-artifact.bin            # one pass
slop_ninja_miner resolve --network test --artifact ./my-artifact.bin --watch    # until verified
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

- **Spend funds.** Registration charges your coldkey, which the client never
  loads: register with rtao ([Register](#register-on-subnet-601)).
- **Change your submitted model.** It commits only for the artifact your
  hotkey last committed for this cohort, unless you pass `--allow-replace`:
  needed to commit for another artifact (for example after an artifact-class
  refusal, once you have fixed and packed the model) or to replace a
  commitment made from another machine or state directory.
- **Commit after the cutoff.** It commits only while the coordinator's cohort
  read says the cohort admits and the cutoff's earliest block is more than a
  margin past the finalized head. The margin is `--deadline-secs` at the
  nominal block time, and it is checked again just before signing. The
  commitment is signed with a mortal era of at most half the margin, so the
  chain can include it only within that many blocks of the finalized head it
  was signed at, which is before the cutoff. The bound fails only if the
  finalized head advances by more than half the margin between that last
  check and the signature. It warns early when less than twice an upload's
  planned time is left (your object's size at the cohort's URL-policy rate
  plus its skew allowance, the same allowance the URLs carry) and again when
  less than once is left.
- **Commit from the feed alone.** A commitment the status API has not shown
  yet may only be its minute of lag: `resolve` decides from the chain, and
  uploads a landed commitment instead of committing again.

Each pass is idempotent, so running it again (or from cron) is safe; one
pass at a time acts on a state directory (`resolve.lock`). Every decision and
its outcome is appended to `resolve-log.jsonl` in the state directory (mode
0600), without your opening, any URL or signature. `--dry-run` prints what it
would do. Exit 0 when the state needs nothing more from you, 1 when a manual
step is needed (it prints which), 2 on an error.

## At the cutoff and the close

When admission closes, the coordinator:

1. refuses every session not completed in time (`incomplete_at_cutoff`);
2. for each verified session, checks that your last commitment before the
   cutoff block is still the one the session opens and that your registration
   is unchanged (`commitment_overwritten`, `not_registered` or
   `registration_mismatch` otherwise);
3. orders the remaining sessions by position (block, then extrinsic index) and
   admits them in that order up to the cohort's roster cap (`admitted`), under
   the selection rule the cohort read names
   ([The cutoff and the roster](admission-api.md#7-the-cutoff-and-the-roster));
4. marks the rest `overflow`.

**Nothing carries over, overflow included.** To enter the next cohort, commit
again inside its window and upload again; you are ranked by the new
commitment. An overflow upload is deleted at the close. An admitted model is
published when scoring starts, so the next cohort needs a new one.

The cohort then runs without you:

- **Generation.** The coordinator draws the hidden batch under the cohort's
  plan: human-only, mixed and model-only documents, whose AI-side text comes
  from frontier models the coordinator has access to through configured
  credentials and from open models on Workers AI, in the proportions the plan
  sets ([batch rules](../agents/rewards.md#cohort-batches-and-human-stock)).
- **Publication of the artifacts.** When scoring starts, every admitted
  artifact (header and files) is published on the public status host,
  `https://testnet.slop.ninja`, except an operator's or a reference's: the
  header at `artifacts/v2/<header sha256>/header` and each file at
  `blobs/sha256/<file sha256>` under that host. The cohort's
  `v1/cohorts/<label>/artifacts.json` there lists each entry's header and
  files, and each cohort page on the
  [status page](https://slop.ninja/testnet/status/) links every published
  header.
- **Scoring.** Every admitted artifact, the reference and its predecessor,
  when one exists, score the same documents, each document independently.
  Every executed entrant also runs once on a private probe used for the
  public-copy gate ([copies](../agents/rewards.md#copies)).
- **Package.** The coordinator signs its score package (the exact scores and
  every hidden measurement) and its receipt index before the transition. Both
  stay private to the coordinator. The live index uses the flat receipt format.
  The candidate chunked format changes how the coordinator stores this evidence.
  Miners use the same artifact and submission commands in either case.
- **Transition.** At least two tempos with no new challenges. At the closing
  boundary the coordinator publishes every admitted entrant's opening and the
  cohort's quantized result, and the cohort's target, if it produced one,
  activates.

Validators then commit the cohort's row at every tempo from its activation
until a later cohort's target replaces it. Cohort *N* is therefore paid while
cohort *N*+1 runs, and for longer when later cohorts produce no new target
([Rewards](#rewards)).

Human documents: testnet cohorts may reuse human documents that earlier
cohorts used, and those documents may be exposed. A reused document is no
longer unseen, so a score from such a cohort shows that your artifact runs and
is scored; it is not evidence of quality on unseen text. Every text in the
testnet human stock is public and can be recognized, an accepted testnet
limit.

What becomes public, and what never does
([What is public](terms.md#what-is-public)):

- **Public when scoring starts:** your artifact (header and files), if
  admitted. It stays published: SlopNinja removes published material only by
  a recorded legal removal, listed in the public [removal record](removals.md),
  which changes no score, result, row or payment
  ([terms](terms.md#legal-notices-and-removals)).
- **During scoring, if your cohort's template releases live values:** each
  executed entrant's quantized score, or its withheld outcome, shown as
  pre-finalized once it is fixed, and a few noised `live-v1` segment
  readings under the cohort's own privacy budget, which the operator can
  stop ([pre-finalized scores and readings](../agents/rewards.md#quantization-noise-and-the-bit-budget)).
  The status API discovery document's `notes` say which values it serves and
  when ([The status API](#the-status-api)).
- **Public no later than the close:** your quantized score (the result's `q`
  field), unless it is withheld ([Rewards](#rewards)).
- **Public at the close:** your opening, and the rest of the cohort's
  quantized result: for each executed entrant in roster order, its hotkey and
  UID, its band, one bit for whether it passed the gates, one for the
  public-copy gate, and its cluster, with the grid width and both references'
  quantized scores. The coordinator's signed row, with each paid UID's
  integer weight, is published beside its authorization. Before the close
  nothing shows a rank, band, cutoff, gate bit or cluster. Built, not deployed:
  the summary also gives the admission reason for an unexecuted entrant
  (`copy` or `qualification`). For a cohort whose plan names the gate
  feedback record, it gives the gate thresholds and each scored entrant's
  failed classes: `share`, `attribution` and `human_fpr` (the loose
  pure-human false-positive tier).
- **Never public:** exact scores, gate statistics, margins by which a gate
  was missed, private reason codes, the hidden documents, their labels and
  per-document predictions, and the coordinator's
  score package and receipts. Refused, overflow and superseded uploads are
  deleted no later than the close and never published.

## Rewards

The full rules are in the [reward rules](../agents/rewards.md) and the
whitepaper's rewards section; each signed cohort manifest freezes the
parameters that cohort uses.

The reward and quantization rules belong to a chain record. A rule change
requires a recorded restart into a fresh record; it does not rescore an old
cohort. Read the cohort's signed manifest and the published release notes
before entering after a restart. Validator support for a new rule must be
released before that rule is activated.

**The score.** The score is binary Brier utility on AI involvement,
`1 - (a - y)^2` per document, averaged over the batch: `a` is your predicted
probability of any AI involvement, and `y` is 1 for model-only or mixed
documents and 0 for human-only ones. The published result's `q` field, the
status page's scores and the leaderboard hold quantized utility scores, and
the cutoff and band edges are given on the same scale.

**Gates.** Artifact qualification, the public-copy gate, share, attribution,
the per-entrant pure-human false-positive tier, and the span gate where the
cohort's manifest enforces it, are gates: they decide eligibility and carry
no reward weight. Each cohort's signed manifest freezes its gate
thresholds; the published result carries the gate bits, and the private
score package records each failed gate with its reason. A cohort whose plan
names the gate feedback record also publishes its thresholds and each
scored entrant's coarse failed-gate classes after the close (built, not
deployed). A failed gate earns nothing.

**Spans.** The span gate measures each entrant's AI spans against span
package d24, the gate's fixed anchor
([Reference packages](#reference-packages-optional)), on the cohort's batch
under the 24-word floor. Your cohort's template decides whether the gate
runs and in which mode. If it runs the gate in report mode, the spans are
measured and the gate fails no one. If it enforces the gate, every entry must
carry a token-level head whose decoder never emits a span under 24 words,
and an entrant that fails the gate earns nothing. In either mode the
coordinator signs each scored entrant's span outcome, and whether the gate
applied, was waived or was voided for the cohort, in a separate span-gate
record, apart from the quantized result; the
[public layout](public-layout.md#one-cohort-v1cohortslabel) lists every file
a cohort publishes. A cohort whose artifact spec has `span_requirements`
refuses an artifact without such a head at admission (`token_head_required`,
`span_decoder_floor`); a manifest that enforces the gate under a spec without
them fails such an entrant at scoring instead. The training guide's recipe
trains no token head.

**Released scores are quantized and noisy.** Exact scores never leave the
coordinator. Every released score, yours and both references', is the exact
score plus a secret offset and seeded discrete-Laplace noise, rounded down
onto a grid, under the cohort's versioned quantization rule
([Quantization](../agents/rewards.md#quantization-noise-and-the-bit-budget)).
The noise is drawn from the model's inference content, so identical copies
under several hotkeys share one draw, and it grows with the roster. Each
closed cohort's summary gives its rule and grid width (`quantization` in
`v1/cohorts/<label>/summary.json`). The cutoff, bands, ranks, credit, units
and the row all compare quantized scores, never exact ones. An entrant whose
attributable failures exceed the minimum-success gate's limit is withheld:
its score is drawn but not released.

**The cutoff.** A cohort pays only gated entrants whose quantized scores
reach its cutoff: `R_k`'s quantized score plus the manifest's margin of
`R_k`'s remaining loss, rounded up onto the grid
([Rule and reference](../agents/rewards.md#rule-and-reference)). Release
noise cuts both ways: it can keep an entrant that truly beats `R_k` below
the cutoff, and lift one no better than `R_k` to it. Once a predecessor
`R_{k-1}` exists, a cohort in which nobody reaches the margin cutoff but some
gated entrant is above `R_{k-1}`'s quantized score is scored against a lower
fallback cutoff, and the reference does not advance
([When nobody beats R_k](../agents/rewards.md#when-nobody-beats-r_k)). In any
other case the previous target stays in force. Each closed cohort's summary
gives its cutoff and which rule set it (`quality.cutoff`: `q` and `mode`).

**Credit.** Credit among the entrants at or above the cutoff follows the
band rule on quantized scores ([Bands](../agents/rewards.md#bands)): entrants
with the same inference content, or within the near-copy distance of a
cluster's head on the batch, share one cluster; clusters are placed in bands
at least one grid step wide; credit falls geometrically per band and per rank
within a band; clusters with equal quantized scores share the credit of their
positions equally; each member of a cluster gets an equal share of its
credit; and the shares are rounded into the row's integer units. Rank order is
quantized score, then roster order; no exact score enters it. Each closed
cohort's summary gives its bands (`quality.bands`).

**Promotion.** The first retained cluster head in rank order whose exact
score beats `R_k`'s by the margin (tested privately; only the outcome is
released) and that also passes the promotion checks becomes the next
reference, `R_{k+1}`. The whitepaper's promotion checks are the strict
pure-human false-positive tier and the lookup canary; the leaderboard's
`promotion_rule` says whether the current record's promotions run the canary.

**Copies earn nothing.** Every artifact published by an earlier cohort or as a
reference counts, including your own earlier entries. An entrant whose
inference content equals any published artifact's is not executed: entering
the model you entered before gets it admitted but never scored. The inference
content covers every file except `manifest.json`, so a changed export record
alone does not make a new model. One within the near-copy distance of a
published artifact, or of either reference, fails the public-copy gate unless
it scores more than half a band above it ([Copies](../agents/rewards.md#copies)).

**Which row validators commit.** Validators commit the active target's
signed row each tempo ([validator guide](validator-guide.md)), and the public
status host's `v1/authorizations/current.json` names the authorization and
row in force. A record starts with the launch burn row as its active target,
which gives the whole pool to the subnet owner hotkey's UID, which burns it.
The first cohort with a gated entrant at the cutoff replaces it with its
target, and a later cohort that produces no new target leaves the previous
target in force. Within a record nothing authorizes the burn row again; only
an operator's switch to burn, which starts a fresh record, brings it back
([terms](terms.md#operator-decisions)). Units credited to a hotkey that is no
longer registered under the same registration go to the owner's UID and
burn. The status snapshot's `mode` (`target` or `burn`) and its notice say
which is in force.

**The revenue pot is separate.** Revenue from SlopNinja's detection products
goes into a public pot, which changes no score, row or emission; the
[terms](terms.md#the-pots-planned-distribution) say what it is and is not.
The planned miner distribution uses each winning model's share of all
qualifying detection requests before mainnet launches, under a counting
rule published before the pot's first deposit. Requests served by the
operator's models remain in the total; their portion is burned.

### Reference model

Every cohort scores the current reference `R_k` and its predecessor
`R_{k-1}`, when one exists, on its batch, and its published result gives
both quantized scores. The [status page](https://slop.ninja/testnet/status/)
names the current reference, and the public status host gives each one's
content and artifact digests and where its files are
([The current reference](#the-current-reference-optional)):

```sh
curl -fsS https://testnet.slop.ninja/v1/reference/current.json
curl -fsS https://testnet.slop.ninja/v1/reference/previous.json
```

A record starts from `R_0`, the first reference, whose package SlopNinja
publishes ([Reference packages](#reference-packages-optional)). The reference
then changes by promotion from a cohort ([Rewards](#rewards)) or by an
operator reference decision with a recorded reason
([terms](terms.md#operator-decisions)). Such a decision sets the current and
previous references for later cohorts, leaves a signed, content-free public
record of its reason, and moves no reward already earned. An entrant that
becomes the reference was already published when its cohort's scoring
started. Do not enter a reference's files: an entry with a published
artifact's inference content is admitted but never executed
([Copies](#rewards)).

## Refusals and what they mean

Every refusal has a stable code. `check` reports the artifact spec's codes
locally; the coordinator reports them inside `check_failed` (in
`detail.check`). `status` shows session refusals and the last refused request.

### Artifact checks (`check`, and `check_failed` at upload)

| Code | Meaning | What to do |
| --- | --- | --- |
| `object` | the object's header does not parse, or the object's length is not its header's (a file that no longer matches its header digest is refused by R2 at upload, `BadDigest`) | pack again from the model directory |
| `undeclared_file`, `missing_file` | a file the spec does not list, such as an external-data sidecar or notes, or a listed file missing | fix the directory and pack again |
| `contract_id`, `file_role`, `file_size` | another output contract, a wrong role, or a length outside the spec's bounds | pack with `--dir`, which sets the contract and roles; re-export the file |
| `tokenizer_mismatch` | the tokenizer is not byte-identical to the pinned one | use the pinned `tokenizer.json` |
| `json_layout` | a JSON file is not in its canonical layout (whitespace, number spelling, escapes) | write it with the release writers, or re-encode the directory with `slop_ninja_artifact_check --spec "$SPEC" canonical DIR OUT` and check the copy |
| `schema`, `binding` | a role schema violation, or files that do not agree (digests, thresholds, rosters, arm) | regenerate the JSON files from one fit |
| `graph_custom_domain`, `graph_external_data` | a custom operator domain, or external data | export with the release exporter |
| `graph_op_not_allowed`, `graph_dtype_not_allowed`, `graph_unknown_field`, `graph_unsupported`, `graph_malformed` | the graph uses an operator, element type or protobuf feature outside the allow-list, or does not parse | export with the release exporter |
| `graph_subnormal_initializer` | a `FLOAT` initializer holds a float32 subnormal value (rule `slop_ninja_onnx_no_subnormal_initializers_v1`, every spec; a checker built before the rule does not apply it) | set those weights to zero (or retrain) and export again; see the [artifact spec](artifact-spec.md) |
| `graph_not_allowed`, `graph_class`, `graph_size` | the graph is not an allow-listed class, or does not match the role files | export the class you trained in the environment the spec names and check it with `slop_ninja_artifact_check --spec "$SPEC" graph a-fp32band.onnx`. The refusal names the class of the graph's length and the first differing structure byte |
| `graph_class_not_served` | a graph class the checked spec does not serve | check with the spec your cohort names |
| `too_many_parameters` | more parameters than the graph class's bound (`max_parameters` in the spec) | use the allow-listed architecture, and check under the cohort's spec: an older spec may not define your class |
| `json_budget` | the JSON files together exceed the spec's budget (`max_json_bytes`), or the coordinator's own | remove oversized free-text provenance |
| `json_value_budget` | parsing the JSON files would hold more than the spec's value budget (for example, very many small objects or strings), or the coordinator's own | remove oversized free-text provenance |
| `token_head_required`, `span_decoder_floor` | under a spec with `span_requirements`: no token head, or a span decoder other than the one the spec names or with a minimum under its floor | add a token head with the decoder and floor the spec names ([Spans](#rewards)) |

### Requests (`commit`, `upload`, `repair`, `status`)

A command that gets one of these codes prints it. The coordinator marks
`not_admitting`, `commitment_not_found`, `header_missing`, `objects_missing`
and the transient codes retryable: the client retries them where the command
allows (`--attempts`), and a command they outlast exits 2. Every other code
here exits 1. The [admission API](admission-api.md#8-errors) lists every
code.

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
| `header_too_large` | the header length is zero or over the protocol's bound | pack with the release client |
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

After scoring, an attributable failed, missing or invalid result scores zero
utility for that document and keeps its weight. A Container crash or
out-of-memory failure counts against your artifact only when it reproduces on
a fresh instance with the document scored alone; otherwise it is an
infrastructure error, and operator downtime never becomes a miner failure.

## Getting help and reporting problems

Ask questions in the SlopNinja channel for testnet subnet 601 on the
Bittensor Discord or in an issue:

- channel: <https://discord.com/channels/799672011265015819/1569363956831756318>
- Bittensor community server invite: <https://discord.gg/rY4XsCGrd2> (it does not expire)

The channel link opens for members of the Bittensor community server; if you
are not one yet, join with the invite first. The channel is for the subnet's
mechanism: mining, validating, artifacts, cohorts and rewards.

To report a problem, open an issue in the **Issues** tab of the public
repository, <https://github.com/sam0x17/slop-ninja>. Include:

- `slop_ninja_miner --version`, `slop_ninja_artifact_check --version`, the
  release tag or commit you installed, and your platform;
- the command, its exit code and the refusal code;
- the cohort, your hotkey's public address and the session id from `status`.

Never post your mnemonic, keyfiles or password. Never post your artifact
before your cohort's scoring starts, or your opening before the cohort
closes: they let others enter your model. Report a security problem privately
to [security@slop.ninja](mailto:security@slop.ninja), not in a public issue
([security policy](../../SECURITY.md)). Send legal notices, such as a
copyright notice about a published artifact, privately to the address in the
[terms](terms.md#legal-notices-and-removals), not in a public issue.

## Specifications and further reading

- [Artifact spec](artifact-spec.md): the files, every check and its code.
- [Admission API](admission-api.md): the commitment, signed requests,
  sessions, the cutoff and every error code.
- [Miner client](../../subnet/crates/miner/README.md): every command, flag
  and environment variable; `slop_ninja_miner <command> --help` prints the
  same.
- [Reward rules](../agents/rewards.md), [terms](terms.md),
  [licences and notices](licences.md) and the [public layout](public-layout.md).
- [Reference packages](reference-packages.md) and the
  [validator guide](validator-guide.md).
- The [whitepaper](../../whitepaper/slop_ninja.pdf): rewards, cohorts and the
  reference model.

The next dashboard build will retain the combined quantization and live-reading
privacy bound after a cohort closes. Its quantization term is from the signed
result; the live term is retained operator policy metadata. An unavailable
policy report means the combined bound is unavailable, not zero. This display
change does not change scores, gates, rewards, submitted artifacts or validator
rows. It is built but has not been deployed.
