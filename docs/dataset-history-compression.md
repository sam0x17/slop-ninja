# Published dataset storage

Published dataset payloads use lossless zstd from their first appearance in the
published branches and release tags. There are 31 payloads: corpus JSONL plus
per-document rights and evidence indexes. Licenses, split manifests, ordinary
reports, and their original checksums remain readable. Large public CFG parse
caches already use zstd and were unchanged. Private training data, tensor caches,
model checkpoints, and recovered releases were outside the migration.

The payloads occupy 9,426,109 compressed bytes instead of 144,690,596 decoded
bytes: 93.5% less for those files in a compressed checkout. This is not a 93.5%
Git repository reduction. Git already compresses its objects: the isolated
published-ref mirror's Git-reported pack storage was 2,317,657 KiB before and
2,313,285 KiB after, about 4.5 MB less. The latter includes the new storage utility
and manifest. Recovery copies and existing decoded working files are retained,
so these measurements do not claim local disk was reclaimed.

Verification covered 2,379 mapped commits and all 31 distinct transformed blobs.
Every archive decoded exactly to its original blob. Every other tree entry,
including code, grammar assets, dataset licenses and split manifests, was
identical to its mapped source commit. A fresh checkout restored all payloads
and passed all 72 entries across the existing dataset `SHA256SUMS` files. The
storage utility's three focused tests cover exact byte restoration, damaged
archives, manifest completeness, overwrite refusal, and path scope.

See the [machine-readable audit](dataset-history-audit.json),
[old-to-new commit map](dataset-history-commit-map.tsv), and
[compression manifest](../datasets/compression-manifest.json). The map covers the
pre-publication migration; the reporting commit is a normal subsequent commit.
Commit messages at published tips gained `[skip ci]` where absent. Other commit
messages retain their embedded original Git IDs. Historical receipts and release
manifests keep original IDs as evidence; map them to find the rewritten code,
rather than changing frozen artifact identities or recalculating model provenance.

Use the [Rust restore command](../tools/dataset-storage/README.md) before existing
dataset readers or checks. Exact decoded filenames, bytes, SHA-256 values,
production histories, splits, and grammar exposure are unchanged. Restoring an
old snapshot is not authorization to fit on an exposed or consumed Test.

Publication uses an atomic push with an explicit lease for each of the seven
existing branch refs and five release tags. A verified bundle of the original
published refs, original local-ref and worktree inventories, blob mappings, and
execution receipts are retained under the ignored task root
`data/task-roots/git-data-compression-20261026/`. Active and historical worktrees
keep their existing bytes and recovery state. The main checkout follows rewritten
main; other clones should fetch the rewritten refs and rebase/cherry-pick only
unpublished changes, or clone afresh. Merging the old main into the rewritten
history would reintroduce uncompressed history. Do not mirror-push recovery refs.
