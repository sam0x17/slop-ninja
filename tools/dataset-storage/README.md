# Slop Ninja dataset storage

This Rust utility stores published corpus payloads as zstd frames with checksums.
It does not parse corpus rows, select splits, or call models. Scope is
`datasets/**/*.jsonl`, `datasets/**/data_rights.json`, and
`datasets/**/evidence-index.json`. Licenses, small manifests, and checksums remain
ordinary readable files. Private training material stays in ignored storage.
Existing public parse caches already have their own compressed format.

Build once from the repository root:

```sh
cargo build --release --locked --manifest-path tools/dataset-storage/Cargo.toml
tools/dataset-storage/target/release/slop_ninja_dataset_storage verify .
tools/dataset-storage/target/release/slop_ninja_dataset_storage restore . datasets/detector-v6
```

`restore REPO_ROOT [datasets/PREFIX]` accepts a snapshot directory or one logical
file. Without a prefix it restores all archived payloads. It validates the
compression manifest when available, bounds individual decoded files to 512 MiB,
rejects symlinks, and compares any existing decoded copy byte for byte. Missing
copies are installed atomically without overwriting existing files. A conflicting
copy must be resolved explicitly. Neither `restore` nor `verify` changes split
manifests, corpus bytes, or source provenance. No checkpoint or tensor is converted.

The compression manifest records both compressed and decoded sizes and SHA-256.
Existing dataset checksums and file references retain their original logical names
and decoded-byte semantics. Restore first, then run the dataset's existing
`sha256sum -c SHA256SUMS` command. The restored copies are ignored by Git; only
archives and publication metadata should be committed.

For an already rights-reviewed new publication snapshot, `pack REPO_ROOT
[datasets/PREFIX]` creates archives and prints their identities. It verifies a full
round trip before accepting a new archive, refuses conflicting existing archives,
and leaves source files in place. `manifest REPO_ROOT` writes a new
`datasets/compression-manifest.json` covering all archives; preserve the prior
manifest outside the checkout before explicitly replacing it. Review changes and
rerun `verify` before committing. `compress` is a deterministic stdin-to-stdout
adapter used by the historical migration. Codec settings are zstd level 9,
single threaded, pledged input size, frame checksum enabled; the Cargo lockfile
pins the encoder dependencies. A different encoder can produce different packed
bytes without changing canonical decoded data, so regenerate receipts explicitly.

Published history was migrated from each payload's first appearance. Historical
programs still expect the decoded paths: use this current utility against the old
checkout before running them. Commits before the compression manifest was added
are checked using the zstd frame checksum; run their original `SHA256SUMS` checks
after restoration as well. Old checkouts may show restored copies as untracked;
do not commit them. Existing frozen experiment receipts retain their original Git
IDs. The old-to-new commit map is an alias map, not a change to their artifact or
grammar identities. Recovery retains the original history separately; old active
worktrees are intentionally left on their original commits.

The completed migration's [audit and commit aliases](../../docs/dataset-history-compression.md)
record exact byte verification and the separate checkout/pack size measurements.
