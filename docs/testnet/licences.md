# Licences and notices (testnet)

What SlopNinja publishes for testnet, under which licence, and where each
licence text and notice is.

| What | Licence | Licence texts and notices |
| --- | --- | --- |
| Code and documentation in this repository | MIT OR Apache-2.0, at your option | `LICENSE-MIT`, `LICENSE-APACHE` and `NOTICE` at the repository root; files and datasets with their own licence keep it, as the README's Licence section lists |
| Dataset snapshots in `datasets/` | each snapshot's own | each snapshot's `NOTICE.md`, licence copies and per-row source attribution |
| Release binaries | SlopNinja's code: MIT OR Apache-2.0; the third-party Rust packages built into them: their own | [Release binaries](#release-binaries) |
| Reference packages (`R_0` and span package d24) | CC BY-SA 4.0 | each package directory's `NOTICE.md` and `upstream/ModernBERT-base-LICENSE` ([reference packages](reference-packages.md#licence)) |
| Entrant artifacts | CC BY-SA 4.0, granted by the miner that entered each one ([terms](terms.md#your-submission)) | [Entrant artifacts](#entrant-artifacts) |
| The SlopNinja name and the logo in `assets/brand/` | not licensed by any of the above | |

## Release binaries

A release's binaries are built from this repository, and they also contain
code from third-party Rust packages, each under its own licence, and the
Rust standard library (MIT OR Apache-2.0).

`testnet-v0.1.0` does not list those packages. Later releases carry a
`THIRD-PARTY-LICENSES` file among their assets, covered by the signed
`SHA256SUMS`. For each third-party package built into the release's binaries
it gives the version, the declared licence (an SPDX expression), the source
(crates.io, or a git repository at a commit) and the licence texts from the
published package. Where a package ships no text for a licence it declares,
the file gives that licence's standard text.

## Entrant artifacts

An admitted artifact is published when its cohort's scoring starts
([public layout](public-layout.md#artifacts-artifactsv2sha256header-and-blobssha256sha256)),
under the CC BY-SA 4.0 licence its miner granted. Each artifact also
contains work that others license:

- its `tokenizer.json` is the pinned tokenizer
  ([artifact spec](artifact-spec.md#tokenizer)), which is ModernBERT-base's
  tokenizer, re-serialized;
- every allow-listed graph class is built on ModernBERT-base's architecture,
  and an artifact trained from ModernBERT-base's weights, or from a SlopNinja
  reference package (which derives from them), contains a modified version
  of those weights.

ModernBERT-base is by Answer.AI and LightOn
(<https://huggingface.co/answerdotai/ModernBERT-base>, revision
`8949b909ec900327062f0ebf497f51aef5e6f0c8`) and is licensed under the Apache
License 2.0. The repository's `LICENSE-APACHE` is that licence's text, byte
for byte the copy in each reference package's
`upstream/ModernBERT-base-LICENSE`.

The public status host, where artifacts are published, is not yet
readable. A notice for the published artifacts, with a copy of the base
model's licence, is planned beside them under `artifacts/v2/` before public
access opens; the [public layout](public-layout.md) will give their paths
once the publisher writes them.
