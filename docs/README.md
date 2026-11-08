# Documentation

This directory contains design notes, research contracts, and the evidence cited
by [whitepaper draft 0.60](../whitepaper/slop_ninja.pdf), the canonical protocol
proposal. The paper adopts the [Cloudflare design](cloudflare-subnet-draft.md).
Launch includes *A* alone; *B* and *C* require separate activation decisions.
*A* reads raw text; the grammar (CFG) notes are grouped under later *B* work
below.

## Subnet, privacy, and service operation

- [Subnet overview](subnet.md)
- [Current Cloudflare design and billing](cloudflare-subnet-draft.md)
- [Subnet implementation plan](subnet-implementation-plan.md)
- [Native chain adapter](../subnet/README.md)
- [Runtime 486 chain surface](subnet-runtime-486-diff.md)
- [Bounded-arena memory for the Workers coordinator](workers-arena-design.md)
- [Linux and macOS support](platform-support.md)
- [Miner models and training data](miner-models-and-data.md)
- [Paid inference](paid-inference.md)
- [A inference on CPU, Workers and GPU hosts](a-cpu-inference.md)
- [Mixed-int8 CPU serving of A](a-int8-serving.md)
- [f32 serving of A on Cloudflare Containers](a-container-serving.md)
- [A on Cloudflare Workers AI](a-workers-ai-viability.md)
- [A on a RunPod GPU backend](a-runpod-backend.md)
- [A baseline release candidate](a-baseline-release.md)
- [A Container server and served profile](../inference/a-container-serve/README.md)
- [Testnet plumbing evaluation manifest](../tools/a-testnet-eval-manifest/README.md)
- [A span localization protocol](a-spans-protocol.md)
- [A span localization implementation](a-spans-implementation.md)
- [A span training data: spliced rewrites](a-spans-data.md)
- [A span localization results](a-spans-results.md)
- [A span commit aliases](a-spans-aliases.md)
- [SlopNinja on testnet](testnet/README.md): the landing page, with the
  [miner guide](testnet/miner-guide.md),
  [training guide](testnet/training-guide.md),
  [validator guide](testnet/validator-guide.md) and [terms](testnet/terms.md)
- [A artifact spec (testnet)](testnet/artifact-spec.md)
- [Admission API (testnet)](testnet/admission-api.md)
- [Service evidence and recovery](service-evidence.md)
- [Receipts and external-detector evidence](subnet-receipts-and-audits.md)
- [Executable receipt-envelope prototype and version-1 schema](receipt-envelope.md)
- [Subnet testnet stack migration](testnet-stack-migration.md)

## A research and implementation

- [Restoring the stronger A reference](a-reference-comparison.md)
- [Bounded encoder adaptation](a-encoder-adaptation.md)
- [Shared A on fresh source families](a-balanced-baseline.md)
- [Shared A: frozen final Test](a-final-test.md)
- [Competitive A evaluation protocol](a-competitive-protocol.md)
- [Competitive A evaluator](a-competitive-evaluator.md)
- [Frontier-model positives for A](a-frontier-positives.md)
- [A competitive campaign source migration](a-comp-source-migration.md)

## Corpus and evaluation

- [Corpus design and collection](corpus.md)
- [Consolidated corpus](a-consolidated-corpus.md)
- [A corpus acquisition](a-corpus-acquisition.md)
- [Reusable A data and retained CFG parse evidence](a-data-storage.md)
- [Author sources, 2026-10-26](a-baseline-author-sources-20261026.md)
- [Published dataset storage](dataset-history-compression.md)
- [Public datasets](public-datasets.md)
- [Blog Authorship corpus workflow](blog-corpus.md)
- [Training-source acquisition](miner-training-sources.md)
- [Common Crawl human stock sampler and measurements](../grammar/crates/grammar-cc-human/README.md)
- [Detector evaluation](evaluation.md)
- [Pangram benchmark and API funding](pangram-oracle.md)

## Evidence and research references

- [Public-model evaluation policy](public-model-evaluation.md)
- [V6 detector evidence](../baseline/detector/releases/v0.6.0/RESULTS.md)
- [Pangram public-result verification](pangram-public-results.md)
- [Pangram repeatability probe](pangram-repeatability.md)
- [Pangram model-card notes](model-card-notes.md)
- [Watermark provenance](watermark-provenance.md)
- [Research notes](research-notes.md)

## Later B transformation and grammar

*B* and *C* activate separately after launch, each by its own decision. The
native grammar (CFG) is a planned *B* input for edit units and revision
guidance, and the target of *C*'s contributions. The dated CFG comparisons below
are why *A* reads raw text; the writer-profile results also underpin *A*'s
encoder-plus-stylometry profile.

- [Later B style utility and review contract](reward-adjudication.md)
- [Edit-utility evaluation](edit-utility-matrix.md)
- [Native grammar workspace](../grammar/README.md)
- [CFG writer vectors versus A encoder profiles: results](cfg-vs-encoder-results.md)
- [CFG writer vectors versus A encoder profiles: frozen protocol](cfg-vs-encoder-protocol.md)
- [CFG-versus-encoder commit aliases](cfg-vs-encoder-aliases.md)
- [Frozen V6 features with a shared A head (text/CFG fusion)](a-v6-cfg-fusion.md)
- [CFG-vector viability experiment](cfg-vector-viability.md)
- [Author-projection evaluation](author-projection.md)
- [Global Voices transfer evaluation](global-voices-transfer.md)
