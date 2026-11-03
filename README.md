# Slop Ninja

Slop Ninja's main objective is to transform writing so it sounds natural and
matches the author's intended voice while preserving meaning, readability,
and tone. The proposed subnet trains A and B adversarially: B learns to revise
text against public A detectors, while authorized B revisions challenge future
A training after evaluation retirement. Mechanism A also supplies
supporting attribution signals and launches first as a public
evaluator and initial detection service. Its model reports AI share,
AI-involvement confidence, and human/model attribution from raw text. The
native English grammar (CFG) is reserved for later B work, as an input for
edit units and revision guidance; it is not part of the A detector.

The proposal has three logical mechanisms, activated separately:

- **A** launches first, with coordinator-led evaluation of public models and Rust validators checking signed records before submitting weights.
- **B** would transform authorized writing while preserving meaning, readability, and tone, using the native grammar to define edit units and guide revision. Its proposed Cloudflare path keeps content from miners and private weights from the public, with provider/operator trust and qualification still required.
- **C** would credit accepted reusable contributions to the shared grammar that later B work uses, once each, under its own activation decision.

The current design and evidence boundaries are in the [whitepaper](whitepaper/slop_ninja.pdf).
Testnet subnet 601 has its own [landing page](docs/testnet/README.md), with the miner,
training and validator guides and the testnet terms.

Development moved to this repository on 2026-11-03. Its first commit imports
the project from a private development repository. Commit ids, branch names
(such as `origin/...`) and ignored `data/` paths cited in evidence, receipts and
documents written before then refer to that private history and its local
inputs, which are not published. Execution hosts and home directories are named
by placeholders (`build-host`, `remote-host`, `workstation-host`,
`/home/operator`, and `~/` for a home directory). Where the import changed a
file (host and path placeholders, resource names, links), digests cited for
that file refer to its private bytes. The exception is the files whose home
paths the import removed (pinned detector and experiment records, and the
dataset record files and capture audit noted in
[datasets/README.md](datasets/README.md)): their checksums, sizes and record
digests were updated wherever they are cited, together with the digests of the
files that cite them. Release bundles and
model weights cited as releases are not part of this repository. The grammar
workspace's native grammar is not yet included; see
[grammar/README.md](grammar/README.md).

## Repository map

- [docs](docs/README.md): current operating guides, corpus policy, API evaluation, privacy, and retained evidence.
- [baseline/detector](baseline/detector/README.md): the V6 reference detector and its released evidence.
- [baseline/transformer](baseline/transformer/README.md): the current B research baseline.
- [whitepaper](whitepaper/README.md): LaTeX source and PDF build.
- [grammar](grammar/README.md): the parser-free A crates (baseline, training data preparation, Common Crawl stock), the spaCy feature crates and, in `grammar/crates/grammar-eval/python/`, the public A training recipe and exporter. The native English grammar for later B work is not yet included.

## Local development

The root Rust package provides collection, profiling, detector orchestration, and receipt utilities:

```sh
cargo check
```

Before dataset-backed checks or experiments, restore the published corpus bytes
from their compressed Git storage (see [dataset storage](tools/dataset-storage/README.md)):

```sh
cargo run --release --locked --manifest-path tools/dataset-storage/Cargo.toml -- restore .
```

The grammar workspace, which holds the parser-free A crates, is separate:

```sh
cd grammar
cargo check -p grammar-a-baseline
```

Run focused builds and tests in bounded jobs with explicit memory limits and no
swap. The repository has no CI.

Build the whitepaper from the repository root:

```sh
bash whitepaper/build.sh
```

## Data and privacy

Keep API keys, model weights, customer text, private corpora, generated run directories, and temporary artifacts under ignored `data/` paths or a task-scoped temporary directory. Do not commit source prose from private material. Public A artifacts may be shared; B weights and customer plaintext follow the whitepaper's [current privacy contract](whitepaper/sections/05-confidentiality.tex).

The repository contains research artifacts and prototypes. It does not operate a live subnet, confidential B serving endpoint, or production validator.

## Licence

Project code and documentation are licensed under either of the following, at your option:

- the MIT License ([LICENSE-MIT](LICENSE-MIT));
- the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)).

Miner submissions and the reference model weights (`R_k`) are licensed under [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/). A released weight bundle also keeps its base model's licence and notices. Slop Ninja publishes its own references (`R_0` and any later reference it trains) on these terms, in packages that carry synthetic replay cases and no evaluation text; a miner artifact that becomes the reference is published with its cohort like every admitted artifact.

The Slop Ninja logo files in `assets/brand/` are not covered by the code and documentation licences above.

Files, directories and datasets that carry their own licence or notice keep those terms:

- `datasets/`: corpus text keeps its source licences. Each snapshot's `README.md`, `NOTICE.md`, `LICENSE.fix-slop` or `LICENSES.md`, `licenses/` copies and per-row rights records state its terms; see [datasets/README.md](datasets/README.md).
- `baseline/detector/`: `LICENSE` (MIT); `prompts/LICENSE.fix-slop` for the third-party prompts; `releases/v0.6.0/NOTICE.md` for the released weights and their base model.
- `inference/a-v6-cpu/`: `LICENSE` (MIT) and the third-party licences in `licenses/`.
- `experiments/shared-pool-gameability/upstream/`: the vendored upstream source in `source/` and the adapted `math_standalone.rs` (its import-path changes are recorded in `manifest.json` and reproduced by `verify.sh`), under `source/LICENSE` (Apache-2.0).
- `experiments/shared-pool-gameability/src/native.rs`: the division method bodies of its `compatibility` module, copied unchanged from the vendored `upstream/source/primitives/safe-math/src/lib.rs`, under `upstream/source/LICENSE` (Apache-2.0).
- Global Voices excerpts in dataset texts: [NOTICE](NOTICE) (CC BY 3.0).
- Dataset records quoted or adapted in tests keep their records' licences (PLOS ONE CC BY 4.0, Wikinews CC BY 2.5, historical Wikipedia and HC3 `wiki_csai` CC BY-SA 3.0, CMU Book Summaries CC BY-SA 3.0 US): [NOTICE](NOTICE).
- Rust crates whose `Cargo.toml` declares `license = "MIT"` are MIT only: the crates of the `grammar/` workspace, which inherit it from `grammar/Cargo.toml`; `baseline/detector`; `baseline/transformer`; `inference/a-container-serve`; `inference/a-v6-cpu`; and `tools/a-testnet-eval-manifest`, `tools/artifact-store` and `tools/corpus-store`.
