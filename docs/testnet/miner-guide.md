# Testnet miner guide

This guide takes you from nothing to an *A* artifact that is committed on
chain and uploaded for a cohort on Bittensor testnet subnet 601. It covers the
miner client `slop_ninja_miner`, wallets and registration with rtao, training,
packing and checking, the commitment, the upload, what happens when a cohort
closes, how rewards work, and what each refusal means.

Testnet has no monetary value. Read the [terms](terms.md) before you submit:
submissions are licensed CC BY-SA 4.0 and become public after their cohort
closes.

## What is live

| Piece | State |
| --- | --- |
| Subnet 601 on testnet, and registration on it | live |
| `slop_ninja_miner pack` and `check`, and `slop_ninja_artifact_check` | built and tested on Linux x86_64; they run offline |
| Local scorer (`slop_ninja_a_local_score`), the hidden scorer's engine on your machine | built and tested on Linux x86_64; runs offline |
| Graph exporter (`export_a_onnx.py`) | in the repository |
| [Training guide](training-guide.md) and its public recipe, the path from ModernBERT-base and public data to an admissible artifact | in the repository; tested end to end on Linux x86_64 with CUDA (a smoke run) |
| Release public key | published on the [landing page](README.md#release-key) |
| Public repository | [sam0x17/slop-ninja](https://github.com/sam0x17/slop-ninja) |
| Signed release binaries (Linux x86_64) | not yet live: binaries built from the public repository, signed and smoke-tested when it is published |
| Coordinator admission routes, used by `commit`, `upload`, `abort` and the hotkey view of `status`, served by the upload Worker at `https://testnet-admission.slop.ninja` | not yet live |
| Public status host, used by `status` and `next-cohort`, with the published packages, leaderboard and artifacts | not yet live |
| The coordinator's admission origin pinned in the release trust file | pinned: `https://testnet-admission.slop.ninja` |
| Validators committing the launch burn row ([Rewards](#13-rewards)) | live since November 3, 2026: both Slop Ninja validators commit and reveal it on subnet 601 each tempo |
| Reference packages (`R_0` and the S2c span package) | built and checked; downloadable when public access opens ([Reference model](#reference-model)) |

The [training guide](training-guide.md) produces an admissible artifact from
public material and ends with `pack` and `check`. Until the admission routes
are live you cannot commit or upload: `commit` takes the admitting cohort from
the coordinator, so it does not run without one. The steps below say where they depend on a piece that is not yet
live. The public repository is
<https://github.com/sam0x17/slop-ninja>; this page will name the status host
when it goes live. The release key is on the [landing page](README.md#release-key).

## 1. What you need

- A Linux or macOS machine for the client. Linux x86_64 is tested; macOS and
  Linux arm64 are untested.
- Rust 1.89 or later to build from source (rtao's minimum). The tested
  toolchain is Rust 1.99.1.
- A training machine as the [training guide](training-guide.md#what-you-need)
  describes: tested on Linux x86_64 with a 24 GB NVIDIA GPU and CUDA 12.8
  wheels, and about 16 GB of disk.
- About 1.3 GB of free disk per artifact: the six model files (about 605 MB)
  and the packed object (about 610 MB).
- Test TAO on a testnet coldkey, for registration and transaction fees. The
  registration cost floats: it read about 0.155 τ on October 30, 2026 and
  0.0005 τ on November 1, 2026. Slop Ninja does not hand out test TAO; use the
  community testnet sources.
- An uplink that can send about 610 MB inside a cohort's admission window
  (at least one tempo, about 72 minutes).

## 2. Install

### Release binaries (Linux x86_64)

Releases appear on the public repository's Releases page (not yet live).
Each release holds the Linux x86_64 binaries `slop_ninja_miner` and
`slop_ninja_artifact_check` (and the validator's two), `testnet-trust.json`,
`SHA256SUMS` and the detached signature `SHA256SUMS.sig`. The binaries need
glibc 2.34 or later; on any other platform, build from source.

Download every file of the release into one directory, then verify it before
you run anything, with the key and allowed-signers line from the [landing
page](README.md#release-key):

```sh
printf '%s\n' 'slop-ninja-testnet-release namespaces="file" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGCXMX0I4LTRrHt0MkCsDfg/OUeQ7Sfd0/EytxMUdYPK slop-ninja testnet release' > allowed_signers
ssh-keygen -Y verify -f allowed_signers -I slop-ninja-testnet-release -n file \
  -s SHA256SUMS.sig < SHA256SUMS
sha256sum -c SHA256SUMS
mkdir -p ~/.local/bin
install -m 0755 slop_ninja_miner slop_ninja_artifact_check ~/.local/bin/
slop_ninja_miner --version
```

`ssh-keygen` must print `Good "file" signature for
slop-ninja-testnet-release` with the fingerprint
`SHA256:4YzEnNK8C7zjNWvasiQhv0XDJKdrnKB6L33stMQra1Q`, and `sha256sum` `OK`
for every file; stop if either does not. Take the key from the landing page,
not from the release's own copy. Keep the release's `testnet-trust.json`: the
signature covers it, and the client reads it to decide where it may upload.

### Build from source

Build from the public repository:

```sh
git clone https://github.com/sam0x17/slop-ninja
cd <checkout>/subnet
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
`--spec v2` or `--spec v3` before the command; use the spec your cohort
names, which is the spec the hidden scorer admits your artifact by. The checker
also has `check-dir DIR` and `check-object FILE`
(the admission checks on a directory or a packed object), `graph FILE` (the
structure digest and parameter count of one graph) and `spec` (the whole spec
as JSON). Its `pack DIR OUT` and `parts DIR OUT` are operator tools: to pack
an artifact, use `slop_ninja_miner pack`, which refuses an existing output and
writes the object with mode 0600.

### rtao, for wallets and registration

The client signs with an ordinary Bittensor hotkey file. Install the rtao
command line tool at the same pin the client uses:

```sh
cargo install --locked --git https://github.com/sam0x17/rtao \
  --rev 0c12816009baa65eec3135bac9a9dbcc8eaba77d rtao-cli
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
  the structure digest with `slop_ninja_artifact_check graph a-fp32band.onnx`.
  Spec v2 also serves the span class `a-span-f32band-v1` (the plain graph
  plus its final-norm token states) with heads that read those states: a
  spans-v1 token head in heads v2, exported as export v3 (the form of Slop
  Ninja's S0+ span head). The training guide's recipe does not train a token
  head. Spec v1 refuses the span class with `graph_class_not_served`. The
  artifact spec's [Spec v2](artifact-spec.md#spec-v2) section has the details.
  Spec v3 also serves the tower class `a-tower-f32band-v1` (the plain graph
  plus a span tower and its normed token states) with heads v3, the form of
  Slop Ninja's S2c span package; see [Spec v3](artifact-spec.md#spec-v3). The
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
  3,583,228 bytes. It is not in the repository. The upstream ModernBERT-base
  `tokenizer.json` is refused as it is (`tokenizer_mismatch`); the artifact
  spec's [Tokenizer](artifact-spec.md#tokenizer) section shows how to rebuild
  the pinned bytes from it at a pinned revision.
- **JSON files** must be in their canonical layouts and pass the role schemas,
  including the pinned contract fields and the calibration policy.
- **Parameters**: at most 160,000,000 for the plain and span classes;
  spec v3's tower class has its own derived bound, 173,549,185. Any other
  file in the directory, an external-data sidecar included, is refused.
- **Input.** Cohorts feed raw text, at most 2,048 tokens including `[CLS]` and
  `[SEP]`, with nothing truncated; no batch document falls outside that
  profile. The spec's canonical text profile records exactly what the scorer
  does to a document.

Check the directory before you pack it:

```sh
slop_ninja_artifact_check check-dir ./my-artifact
```

A pass prints the report (graph class, parameters, package digest) and exits
0. A refusal prints `refused <code>: <detail>` and exits 2; a path it cannot
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
that exists. Keep the object and the model files private until the cohort
closes: anyone who has them before then can enter the same model.

`check` runs the spec's admission checks, the same streaming checks the
coordinator runs at upload. Every `slop_ninja_miner` command uses the same
exit statuses:

| Exit | Meaning |
| --- | --- |
| 0 | success; for `check`, every check ran and passed |
| 1 | refused: an admission check refused the artifact (`check` prints the code the coordinator would report), or the coordinator refused the upload session |
| 2 | error: usage, configuration, network or file; nothing was decided about the artifact |
| 3 | `check` only: nothing refused the artifact, but some checks did not run |

Without a coordinator `check` cannot read the cohort's output contract and
size bound, so an offline check that refuses nothing exits 3 with the note
`cohort_unknown`. The same happens when endpoints are set but the cohort
cannot be read, for example because the coordinator is unreachable or no trust
file pins its origin: `check` prints a warning, still runs every local check,
and names the reason in the `cohort_unknown` note. Once the admission routes
are live and the endpoints of the next section are set, `check` also reads the
admitting cohort, checks its output contract and size bound, and exits 0 on a
pass.

`check` uses the artifact spec the cohort names; without a cohort, the newest
spec the build defines (spec v3). A note `artifact_spec_mismatch` means the
cohort checks a spec your build does not define: update the client.

The JSON files are published with the artifact after its cohort closes. If one
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
Build it from the checkout and give it the ONNX Runtime 1.31.1 library: the
hidden scorer uses `onnxruntime/capi/libonnxruntime.so.1.31.1` from the
onnxruntime 1.31.1 cp312 manylinux_2_28_x86_64 wheel (SHA-256
`fc44112fbe06d7d689c57e11d3862031f64711007e4ee020f8eed5185fd2c1d3`); the
training guide's environment already has it.

```sh
cargo build --release --locked --manifest-path inference/a-container-serve/Cargo.toml \
  --bin slop_ninja_a_local_score
inference/a-container-serve/target/release/slop_ninja_a_local_score \
  --model-dir ./my-artifact --docs my-docs.jsonl --out my-results.jsonl \
  --spec v2 --ort-lib "$ORT_LIB"
```

`--spec` is the spec your cohort names (`v2` or `v3`); without it, the local
scorer admits by spec v3, which also admits the tower class that a spec v2
cohort refuses.

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

An upload sends your opening, with its secret nonce, and your whole artifact
before the cohort closes, so the client sends signed requests only to an
authenticated coordinator origin. The release trust file lists one origin in
`admission_origins`: `https://testnet-admission.slop.ninja`, the coordinator's
upload Worker. The client uses only that origin:

- With `SLOP_NINJA_TRUST` set and neither `--coordinator` nor `--public`,
  `commit`, `upload`, `abort` and `status` use the one origin the trust file
  pins. This works now, before the public status host is live.
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

The upload Worker holds no signing key. It checks every signed request before
it reads the body or forwards anything: the signature must verify, and the
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

`next-cohort` reads the public status snapshot (not yet live) and prints the
admitting cohort and its planned cutoff epoch, or the next cohort and its
earliest opening epoch, each with its distance from the snapshot's confirmed
epoch, for example `planned cutoff at epoch 23170 (1 epoch after the confirmed
epoch)`.

Cohort times are subnet epoch indices: the chain's `SubnetEpochIndex` for
subnet 601, which advances by one at each tempo boundary, about every 360
blocks or 72 minutes. The snapshot's first line gives the confirmed block and
its epoch. An epoch that lies *N* epochs after the confirmed one therefore
starts roughly *N* × 72 minutes after the confirmed epoch started, at roughly
the confirmed epoch's first block plus *N* × 360.
Boundaries can slip, so treat these as estimates and read `next-cohort` again
as the time approaches.

Testnet runs one cohort a day: 20 tempos of 360 blocks, about 24 hours. The
admission window opens with the cohort and lasts at least one tempo, about
72 minutes. A commitment counts for a cohort only when its block is at or
after the cohort's opening block and before its cutoff block. A commitment
made earlier is refused with `commitment_before_open`, so you cannot reserve a
place ahead of time. During a coordinator outage the cutoff moves later,
never earlier, and the cohort is never skipped.

Have the artifact packed and checked before the window opens. The commitment
needs finality and the coordinator's observation of it (nominally up to
60 seconds after finality, sometimes longer) before an upload can start, and
the upload must complete before the cutoff. With a one-tempo window that
leaves roughly an hour for the commitment, its logging and the upload of an
object of up to about 700 MB, so budget your uplink for the whole object in
well under that time and start at the opening. The public snapshot's
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

Both forms need the coordinator (not yet live). `commit` reads the admitting
cohort from it and your registration from finalized chain state, makes a fresh
secret 256-bit nonce and the opening, and stores the opening in the state
directory with mode 0600 before it signs anything. It then signs
`Commitments.set_commitment(601, [Raw44(payload)])` with the hotkey, where the
44-byte payload is `"sna1" || u64_le(cohort) || commitment digest`, and reads
the commitment back at finality. `--dry-run` prints the opening and the call
and stores, signs and submits nothing. `--network` defaults to `test`; `--rpc`
names endpoints.

Rules:

- **One cohort per opening.** An opening names exactly one cohort and can never
  be carried to a later one.
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

`upload` (not yet live) opens a session for your opening, sends the object in
parts, completes the session and waits while the coordinator verifies it. It
picks the local opening whose payload matches the commitment the coordinator
observed; `--opening FILE` names one. Rerun the same command to resume an
interrupted upload: parts the coordinator already holds with the same digest
are skipped.

- The client holds one part in memory at a time. A part's timeout grows with
  its size at `--min-upload-kib-per-sec` (default 32); raise it on a fast
  link, lower it on a slow one.
- The upload Worker holds a part's request open for as long as the part
  streams, in a small memory scope of its own. Its memory bookkeeping
  expects that scope to end within the Worker's arena deadline (15 minutes,
  borrowed from the longest documented Workers wall limit; a fetch itself
  has none): a 64 MiB part (the testnet `part_size`) slower than about 0.6
  Mbit/s (75 KiB/s) still completes, but is counted as overdue while it
  streams. A busy Worker answers `unavailable`, which the client retries.
- Retryable errors are retried with backoff (`--attempts`, default 6). A busy
  upload Worker answers the contract's `unavailable` with `Retry-After`. An
  edge error page without a contract body (502, 503, 504, or a Cloudflare
  52x) is retried the same way.
- `--restart` aborts a refused attempt, for example after `object_mismatch`,
  and sends every part again.
- To upload a different object for the same commitment, abort the live session
  first with `slop_ninja_miner abort`. To upload a different model, pack it,
  commit again (`commit --replace`) and upload; the new commitment supersedes
  the old session and moves your position.

One hotkey enters at most one artifact per cohort. The session ends as
`verified` or `refused`.

## 11. Status

```sh
slop_ninja_miner status
```

`status` prints the public snapshot (unsigned, and labelled so), your local
openings, and the coordinator's view of your hotkey for the admitting cohort or
`--cohort N`: registration, the commitment that counts and whether it is in the
window, each session with its state and refusal, and the last refused request.
With no openings it says so and names the state directory it read. Without
the endpoints it prints only the local openings and a line saying the snapshot
and the coordinator's view were not read; without a named hotkey it skips the
coordinator's view and says so. The public snapshot and the hotkey view are
not yet live.

Session states: `uploading`, `verifying`, `verified`, `refused`, `admitted`,
`overflow`, `superseded` (a newer commitment replaced it) and `aborted`.

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

**Overflow does not carry over.** To enter the next cohort, commit again inside
its window and upload again; you are ranked by the new commitment. An overflow
upload is deleted at the close.

The cohort then runs without you:

- **Generation.** The coordinator draws the hidden batch, about 1,000
  human-only, mixed and model-only documents in the proportions the cohort
  manifest fixes, and generates its AI-side text.
- **Scoring.** Every admitted artifact, the reference and its predecessor, when
  one exists, score the same documents, each document independently. Every
  executed entrant also runs once on a private 1,024-document probe used for
  the public-copy gate.
- **Publication.** The coordinator signs and publishes the score package and
  its receipt index, nominally by tempo 18.
- **Transition.** At least two tempos with no new challenges. At the closing boundary
  the coordinator publishes every admitted artifact and its opening, and the
  cohort's target activates.

Validators then commit the cohort's row at every tempo during the next cohort,
so cohort *N* is paid during the following day while cohort *N*+1 is scored.

Human documents: until outside miners have working artifacts, testnet cohorts
reuse human documents that earlier cohorts already used. Fresh human stock is
drawn only once outside miners have working artifacts. A reused document is no
longer unseen, so a score from such a cohort shows that your artifact runs and
is scored; it is not evidence of quality on unseen text. Every text in the
testnet stock is public and can be recognized, an accepted testnet limit.

What becomes public, and what never does:

- **Public after the close:** your artifact and opening, if admitted; the
  signed package with your hotkey, UID, commitment position, artifact digest,
  exact score, each failed gate with its reason code, your cluster, credit and
  integer units.
- **Never public:** the hidden documents, their labels and per-document
  predictions. Refused, overflow, superseded and aborted uploads are deleted
  no later than the close and never published.

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

**The paper rule applies on testnet.** Until some entrant beats the reference
by the margin `δ`, no miner earns: the active target is the burn row, which
gives the whole pool to the subnet owner hotkey's UID, which burns it.
`δ` is `max(0.01, 2σ)`, where `σ` is the pooled paired standard error of `q`
at the batch size. At 1,000 documents under the whitepaper's 20/55/25 mix,
`σ` is about 0.035, so `δ` is about 0.07: an entrant must cut the reference's
remaining loss by about 7%. Each manifest freezes its exact value. The status
snapshot's mode and notice say which regime is in force.

**Committing the burn row is live.** Both of Slop Ninja's validators commit
and reveal the burn row on subnet 601 each tempo (see the
[validator guide](validator-guide.md#the-launch-burn-row)). On November 3,
2026 the chain held both validators' rows as weight 1.0 on the owner UID and
nothing for any miner UID. Expect no payout while the burn regime lasts.

When a gated entrant beats `R_k` by `δ`:

- every gated entrant at or above the cutoff `S_R + δ(1 - S_R)` earns;
- the top-ranked eligible entrant that also passes the promotion checks
  becomes the next reference, `R_{k+1}`. Its artifact is published with its
  cohort. The whitepaper's promotion checks are the strict pure-human
  false-positive tier and the lookup canary. **Testnet only:** promotions on
  testnet run without the lookup canary, and the leaderboard says so. An
  operator drill of the blocking path, named in the cohort's plan, can still
  flag a candidate and block its promotion. The canary is required before
  mainnet.

Once a predecessor reference `R_{k-1}` exists, a cohort in which nobody clears
the margin, but some gated entrant beats `R_{k-1}`, is scored against a fallback
cutoff between `R_{k-1}`'s score and the top score, so rewards keep flowing;
the reference does not advance. In any other case the previous target stays
live.

Credit within the eligible set follows `reward-v1`'s band rule: entrants whose
predictions are near-identical share one cluster; clusters are placed in score
bands about two standard errors wide; credit falls geometrically by 0.85 per
band and per rank within a band; and the shares are rounded into 65,535 integer
units by largest remainder. A lone cluster takes the whole pool, and two take
54.1% and 45.9%. Rank order is score, then commitment position, then hotkey
bytes; within a band it sets the split even when the difference is below the
noise. The whitepaper's rewards section gives every step.

**Copies earn nothing.** An entrant whose inference content equals any
published artifact's is not executed. One within the near-copy distance of a
published artifact, or of either reference, fails the public-copy gate unless
it scores more than half a band above it.

### Reference model

Testnet's reference is `R_0`, Slop Ninja's released *A* document model,
scored in every cohort. Its score on each batch is in the signed package, and
it is the bar that ends the launch burn. Its
[reference package](reference-packages.md) (the six files as the scorer
loads them, package `143ca2d9…c57f0c`) is published under CC BY-SA 4.0 when
public access opens, so you can score it locally with the same engine as your
own artifact. Slop Ninja's S2c span package is published the same way; it is
a span-serving package, not a reference.

The testnet reference will be replaced at most once outside the promotion rule:
by a better reference Slop Ninja trains, only if it passes every required gate,
and announced in advance on the status page. The replacement takes `R_0`'s
place with no predecessor and moves no reward already earned. Every later change
follows the promotion rule above. An entrant that becomes the reference was
already published with its cohort.

## 14. Refusals and what they mean

Every refusal has a stable code. `check` reports the artifact spec's codes
locally; the coordinator reports them inside `check_failed` (in
`detail.check`). `status` shows session refusals and the last refused request.

### Artifact checks (`check`, and `check_failed` at upload)

| Code | Meaning | What to do |
| --- | --- | --- |
| `object` | the object does not parse, or a file does not match its header digest | pack again from the model directory |
| `undeclared_file`, `missing_file` | a file other than the six, such as an external-data sidecar or notes, or one of the six missing | fix the directory and pack again |
| `contract_id`, `file_role`, `file_size` | another output contract, a wrong role, or a length outside the spec's bounds | pack with `--dir`, which sets the contract and roles; re-export the file |
| `tokenizer_mismatch` | the tokenizer is not byte-identical to the pinned one | use the pinned `tokenizer.json` |
| `json_layout` | a JSON file is not in its canonical layout (whitespace, number spelling, escapes) | write it with the release writers |
| `schema`, `binding` | a role schema violation, or files that do not agree (digests, thresholds, rosters, arm) | regenerate the JSON files from one fit |
| `graph_custom_domain`, `graph_external_data` | a custom operator domain, or external data | export with the release exporter |
| `graph_op_not_allowed`, `graph_dtype_not_allowed`, `graph_unknown_field`, `graph_unsupported`, `graph_malformed` | the graph uses an operator, element type or protobuf feature outside the allow-list, or does not parse | export with the release exporter |
| `graph_not_allowed`, `graph_class`, `graph_size` | the graph is not an allow-listed class, or does not match the role files | export the plain class in the pinned environment and check its structure digest |
| `graph_class_not_served` | the span class under spec v1 | check with `--spec v2` if your cohort names spec v2; otherwise export `--variant plain` |
| `too_many_parameters` | more parameters than the graph class's bound (160,000,000; spec v3's tower class 173,549,185) | use the allow-listed architecture |
| `json_budget` | the JSON files together exceed spec v2's 256 MiB, or the coordinator's own budget | remove oversized free-text provenance |
| `json_value_budget` | parsing the JSON files would hold more than the spec's value budget (for example, very many small objects or strings), or the coordinator's own | remove oversized free-text provenance; R0's files use 5.8% of the budget |

### Requests (`commit`, `upload`, `abort`, `status`)

| Code | Meaning | What to do |
| --- | --- | --- |
| `not_admitting` | no cohort is admitting | wait for the next one (`next-cohort`) |
| `after_cutoff` | admission has closed for this cohort | commit again in the next cohort's window |
| `commitment_not_found` | no commitment observed yet: none made, or not final | wait for finality; retried |
| `commitment_before_open` | the commitment was made before the cohort opened | commit again inside the window |
| `commitment_mismatch` | the opening is not the hotkey's current commitment | upload the opening of your latest commitment |
| `wrong_cohort`, `wrong_chain`, `hotkey_mismatch` | the opening names another cohort, chain, subnet or hotkey | commit again for the admitting cohort with this hotkey |
| `not_registered`, `registration_mismatch` | the hotkey has no UID, or registered again since the opening | register, then commit again |
| `second_artifact` | another live session exists for this commitment | `abort` it first; `status` names it |
| `object_too_large` | the object exceeds the cohort's bound | check the file sizes |
| `part_length_mismatch`, `part_out_of_range`, `part_digest_mismatch` | a part was not the expected bytes | rerun `upload`; the last is retried |
| `parts_missing` | completion before every part arrived | rerun `upload` |
| `stale_attempt`, `session_not_uploading` | the request names an old attempt, or a session that no longer accepts it | read `status` and act on the current session |
| `request_expired`, `rate_limited`, `paused`, `unavailable`, `internal` | transient | retried automatically; a long `paused` means the coordinator is catching up, and the cutoff moves later |
| `bad_signature`, `forbidden`, `request_validity_too_long`, `content_digest_mismatch`, `bad_request`, `unsupported_media_type`, `not_found` | a malformed or wrongly signed request | update the client; report it if it persists |

### Session refusals

| Code | When | Meaning |
| --- | --- | --- |
| `object_mismatch` | completion | the assembled object is not the one declared; rerun with `upload --restart` |
| `artifact_invalid` | completion | the object does not parse or verify |
| `contract_mismatch` | completion | the object's output contract is not the cohort's |
| `content_mismatch` | completion | the object is not the content you committed to |
| `check_failed` | completion | an artifact check refused it; see the table above |
| `commitment_overwritten` | cutoff | a later commitment before the cutoff replaced the one this session opens |
| `incomplete_at_cutoff` | cutoff | the session was not completed before the close |
| `not_registered`, `registration_mismatch` | cutoff | the registration changed before the cutoff |

After scoring, the package lists each failed gate with its reason code. An
attributable failed, missing or invalid result scores zero utility for that
document and keeps its weight. A Container crash or out-of-memory failure
counts against your artifact only when it reproduces on a fresh instance with
the document scored alone; otherwise it is an infrastructure error, and
operator downtime never becomes a miner failure.

## 15. Reporting problems

Open an issue on the public repository's issue tracker (the repository is not
yet live). Include:

- `slop_ninja_miner --version`, `slop_ninja_artifact_check --version` and your
  platform;
- the command, its exit code and the refusal code;
- the cohort, your hotkey's public address and the session id from `status`.

Never post your mnemonic, keyfiles or password. Before the cohort closes, never
post your artifact or opening: they let others enter your model. Report a
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
