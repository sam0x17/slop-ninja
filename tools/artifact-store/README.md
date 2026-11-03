# Private Slop Ninja artifact storage

This standalone Rust tool snapshots CFG archives, fitted models and retained
experiment evidence for private Cloudflare R2 storage. It does not change the
grammar workspace lockfile, parse data, select models, call detectors or delete
sources. Its manifests and bundles contain private material and belong in ignored
storage, never Git. Keep reusable code and non-sensitive documentation in Git.

Build on the authorized host in a bounded job:

```sh
cargo build --release --locked --manifest-path tools/artifact-store/Cargo.toml -j 1
```

Use a task-specific `CARGO_TARGET_DIR` for durable jobs. The examples below assume
`slop_ninja_artifact_store` resolves to that captured executable. Linux x86-64 is
tested; macOS is not yet qualified. Python and a GPU are not required.

## Snapshot and restore

Create an explicit source specification under ignored storage:

```json
{
  "roots": [
    {"name": "cfg", "kind": "cfg_archive", "path": "/private/task/cfg-archive-v1"},
    {"name": "encoder", "kind": "tree", "path": "/private/task/final-export"},
    {"name": "seal.json", "kind": "file", "path": "/private/task/seal.json"}
  ]
}
```

Root names are logical paths inside the snapshot, such as `data/splits-v5`. They may have several
components, but no root may sit inside another root's path. A `cfg_archive` root keeps a
single-component name. A `tree` root may list `exclude` paths relative to it, such as
`"exclude": ["test-v1", "train-mining/documents.jsonl"]`. Excluded entries and their subtrees are
never opened. Every listed path must exist, so a typo stops `prepare` instead of silently
widening the snapshot. Keep the spec: the manifest lists what was included, not what was excluded.

`tree` and `file` inputs must be immutable completed artifacts. Include licenses,
tokenizers, preprocessing contracts, reference predictions and parent weights
alongside model overlays. Specify datasets, span provenance and paid fixtures
explicitly; never point this at a home directory, credentials or an entire
unreviewed task directory. Symlinks and special files are rejected. Only regular
files are retained; empty directories, ownership, timestamps and extended
attributes are not preserved. Executability is preserved, with private output
permissions.

`cfg_archive` uses SQLite's online backup API to capture a consistent committed
index while its writer remains live. It copies only the immutable parse, record
and file objects referenced by that index, plus the frozen profile and format.
It checks known compressed-object hashes. Uncommitted updates, unreferenced
objects, old sync receipts and writer locks are excluded. Restored `index.sqlite`
is standalone; a live database's WAL is never discarded by copying only its main
file. The index snapshot and manifest define this backup's coverage. Snapshot
again after backfill completes to retain the added documents.

```sh
slop_ninja_artifact_store prepare SPEC.json NEW_SNAPSHOT
slop_ninja_artifact_store verify NEW_SNAPSHOT
slop_ninja_artifact_store restore NEW_SNAPSHOT NEW_RESTORE_DIRECTORY
```

The versioned manifest maps logical file paths to contiguous byte ranges and
SHA256 hashes. Each content-addressed chunk holds at most 64 MiB of input,
compressed with Zstandard level 3 and a frame checksum. Both stored and decoded
bytes have SHA256 bindings. Already compressed CFG files, SQLite indexes,
weights and text all restore exactly as supplied. Chunks bound memory
and API upload sizes and reduce request overhead for thousands of small files.
There is no cross-snapshot file-level deduplication guarantee: inserting files
can move later chunk boundaries. Identical chunks reuse their object keys.

`manifest.json` is written last as local completion evidence. An interrupted
prepare leaves an incomplete directory; retain it and prepare into a new one.
Upload and fetch resume per content-addressed chunk. Restore requires a new
destination, verifies every chunk before writing, then verifies every restored
file against its recorded hash. It refuses existing destinations and path
traversal. Keep local sources until an independently downloaded restore succeeds.
The local `verify` command checks chunk hashes and manifest shape; `restore`
also checks each reconstructed file hash.

## Private R2

R2 must first be activated in the intended Cloudflare account. Authenticate with
Wrangler. No S3 key or new credential file is required: the tool captures
`wrangler auth token --json` internally with Wrangler disk logging disabled,
accepts OAuth/API bearer tokens, marks the header sensitive, and never prints it.
Do not run that token command into a
shared terminal/log. `SLOP_NINJA_WRANGLER` optionally names the executable.
If R2 answers HTTP 401 or 403 because a captured OAuth token has expired, the tool asks Wrangler for
the current token again (Wrangler's normal automatic renewal) and repeats that one request once,
at most 64 times per run. It never logs in, logs out or edits credentials. Transport errors, 5xx
answers (including Cloudflare's 524 timeout) and read-backs interrupted mid-body are retried up
to four times with backoff. These are idempotent GETs of immutable objects, or PUTs of
content-addressed values that are always read back. A complete object whose size or SHA256
differs is never retried or overwritten. After the bounded attempts the run stops; restart it
explicitly, and existing objects are verified again. No
unattended paid-provider retries are involved.

Provision the intended bucket once, explicitly disabling public access:

```sh
export CLOUDFLARE_ACCOUNT_ID=YOUR_ACCOUNT_ID
wrangler r2 bucket create research-bucket-private --update-config=false
wrangler r2 bucket dev-url disable research-bucket-private --force
wrangler r2 bucket dev-url get research-bucket-private
wrangler r2 bucket domain list research-bucket-private
```

The uploader refuses managed public access or any custom domain, including a
disabled domain. It checks before upload and again before publishing the
manifest. The bucket should remain dedicated to private research, without
public Workers bindings, lifecycle deletion or third-party sharing. Managed URL
and domain checks do not audit all account credentials or arbitrary Workers.
No public endpoint, deployment or bucket lifecycle policy is created here.

```sh
slop_ninja_artifact_store upload SNAPSHOT ACCOUNT_ID BUCKET [KEY_PREFIX]
slop_ninja_artifact_store fetch ACCOUNT_ID BUCKET MANIFEST_SHA256 NEW_DOWNLOAD_CACHE [KEY_PREFIX]
slop_ninja_artifact_store restore NEW_DOWNLOAD_CACHE NEW_RESTORE_DIRECTORY
```

Objects use `v1/blobs/SHA256` and `v1/manifests/SHA256.json`. An optional final `KEY_PREFIX`
argument to `upload` and `fetch` places one snapshot's chunks and manifest under
`KEY_PREFIX/v1/...`. Prefix segments are ASCII letters, digits, `-`, `_` and `.`. Organise prefixes
by rights class, such as `restricted/...`, `evidence/...`, `models/...` and `public-candidate/...`,
and never mix rights classes in one snapshot. A later publication then copies one prefix to a
separate public bucket, and private bytes cannot share a chunk with public ones. Chunks are not
deduplicated across prefixes.

Small immutable objects such as READMEs and catalogs use readable keys:

```sh
slop_ninja_artifact_store put-object ACCOUNT_ID BUCKET KEY FILE
slop_ninja_artifact_store get-object ACCOUNT_ID BUCKET KEY SHA256 NEW_FILE
```

`put-object` checks bucket privacy, uploads at most 64 MiB, reads the object back and verifies
its SHA256. An existing object must be byte-identical and is never replaced, so put a version in
catalog keys. Keys may not use `v1/` or the `blobs`/`manifests` snapshot namespaces. Upload verifies
existing objects by full authenticated GET before reuse, and reads back every
new object before accepting it. A differing existing object causes an error,
never replacement. An uncertain PUT is reconciled by GET on the next run.
The manifest is uploaded only after every chunk verifies. The local append-only
`upload-journal.jsonl` binds account, bucket, checksums and completion. Reruns
verify remote bytes again rather than trusting a stale journal or an ETag.
Download requires a separately retained manifest SHA256, so it does not trust a
mutable latest pointer. Manifests and receipts must remain backed up outside
this one host as well. No command deletes local or remote objects.

Use one upload process per snapshot. Run heavy preparation, verification and
restore under the repository's CPU/RAM/no-swap limits. The offline tests cover
chunk-boundary reconstruction, corruption, interrupted PUT reconciliation,
privacy checks, traversal/symlinks and a SQLite snapshot with an uncommitted
concurrent write. They make no network or detector calls. A live R2 restore is
a separate qualification; passing these tests alone does not establish it.
