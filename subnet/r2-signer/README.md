# SlopNinja internal signer Worker (`slop-ninja-r2-signer-testnet`)

Presigns the direct-to-R2 artifact uploads of admission v2
(`docs/testnet/admission-api.md`, "Direct upload"). Miners PUT the header and
each file straight to R2; no upload byte passes through any SlopNinja Worker.

- **Two credentials, two routes.** The presign key, scoped by the user to
  the private admission bucket only (`secrets_store_secrets` bindings
  `R2_PRESIGN_KEY_ID` and `R2_PRESIGN_SECRET`), signs the mint route; the copy
  key (`R2_COPY_KEY_ID`, `R2_COPY_SECRET`) signs the copy route only. Both are
  read from Secrets Store per request. The S3 host is a Worker secret
  (`R2_S3_HOST`), since it carries the account id.
- **No surface.** No route, no `workers.dev` name, no preview URLs, no R2
  binding. Two callers, each through its service binding on the internal
  host with its own token: the eval Worker (`R2_SIGNER_TESTNET`,
  `SIGNER_INTERFACE_TOKEN`) for the mint route, and the publisher
  (`R2_SIGNER_TESTNET`, `COPY_INTERFACE_TOKEN`) for the copy route. The
  public upload Worker has no binding to it, so a compromised public Worker
  cannot mint (`subnet/tests/credential_placement.rs` checks the configs).
- **What it signs.** `PUT` only, for keys it derives itself
  (`slop_ninja_r2_presign::layout`): the header object, the file objects,
  the upload-path canary under `health/`, and the operator's reference files
  under `reference/v2/files/<sha256>` (`slop_ninja_reference_staging_v1`: entry kind
  `Reference`, cohort and commitment zero, minted only for the eval
  Worker's operator route `POST /eval/reference/v2/urls`). Each URL binds `content-length`,
  `host`, `if-none-match: *` and `x-amz-checksum-sha256`; the payload is
  unsigned, so the signer never sees a body. It never signs GET, HEAD, DELETE,
  POST or multipart requests, and never a copy for the eval Worker.
- **Copy route.** Publication at scoring start copies each admitted header
  and file inside R2, from the admission bucket to the public bucket
  (`POST /internal/r2-signer/v1/copy`, used when the publisher runs
  `ARTIFACT_COPY = "server"`; the tracked default is `"stream"` until probe
  G7-x qualifies a cross-bucket copy). The route signs one `CopyObject` with a
  separate copy credential (`secrets_store_secrets` bindings `R2_COPY_KEY_ID`
  and `R2_COPY_SECRET`: the token `slop-ninja-r2-copy-testnet`, Object Read &
  Write on exactly the admission and public buckets) and a separate token (`COPY_INTERFACE_TOKEN`), so the upload credential
  never signs a copy. Both keys are derived (`slop_ninja_r2_presign::copy`):
  the source from `(cohort, commitment, kind, digest)`, the destination from
  the digest. The signature binds `x-amz-copy-source`, the source etag
  (`x-amz-copy-source-if-match`), `x-amz-metadata-directive: REPLACE` with a
  fixed `content-type` and `cache-control`, and `x-amz-storage-class:
  STANDARD`. `REPLACE` matters: R2 stores unsigned standard headers of a
  presigned PUT, and the default `COPY` directive would carry them (a
  `Content-Encoding` included) to the public copy (qualification G7c).
  Without the copy credential or token the route answers 503 and the
  publisher waits; set to stream (`ARTIFACT_COPY = "stream"`, the default)
  it copies with its own bindings and never calls this route.
- **Bounds.** At most one header plus the spec's file table per request; a
  header at most `MAX_ARTIFACT_HEADER_LEN`; a file at most the largest object of
  any compiled admission spec; validity within R2's 1 s to 7 days. The eval side
  computes validity under the cohort's URL policy
  (`slop_ninja_upload_url_policy_v1`).

Tests: `cargo test` (the decision, natively), `cargo clippy --target
wasm32-unknown-unknown -- -D warnings`. The SigV4 code and its AWS test vectors
live in `crates/r2-presign`.
