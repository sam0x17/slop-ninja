# Slop Ninja dashboard feed

A private, read-only Cloudflare Worker that serves the publisher's
`dashboard/v1` documents to the website Worker. The website reaches it only
through its service binding (`DASHBOARD_FEED_TESTNET`). The feed has no
route, no custom domain, no workers.dev name and no preview URLs.

- It answers `GET` and `HEAD` on the exact `dashboard/v1` paths
  (`src/contract.rs`). Any other method gets a 405 with
  `Allow: GET, HEAD`. Any other path, or a path with a query string, gets an
  empty 404.
- It rebuilds the R2 key from the parsed path, never from the raw path.
- It checks the stored object's size against that document kind's bound
  before reading any byte. An object over the bound gets a 502 with no body.
  A missing object gets a 404, and a bucket error gets a 503.
- A 200 answer carries `Content-Type: application/json`,
  `Cache-Control: public, max-age=60` (`300` for cohort and month
  documents) and `X-Content-Type-Options: nosniff`. The body streams from R2
  inside the runtime, never through Wasm memory. Linear memory is fixed at
  2 MiB (`.cargo/config.toml`).
- It never writes, deletes or lists objects, never fetches, reads no
  variable and has no secret or other binding. Its one var is the testnet
  marker `NETWORK`, which the code never reads. `tests/source.rs` checks the
  sources and `wrangler.toml` for all of these.

`src/contract.rs` is the single definition of the key grammar and byte
bounds. The publisher compiles it too (`#[path]`). The contract is
documented in `docs/testnet/dashboard-contract.md`.

## Checks

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo clippy --target wasm32-unknown-unknown --lib -- -D warnings
```

## Deploy (testnet, operator only)

The dashboard bucket must exist first, and the publisher deploy that writes
it should come first too. The website deploy that adds the service binding
comes after this one:

```sh
WRANGLER_WRITE_LOGS=false WRANGLER_SEND_METRICS=false \
  wrangler deploy --env testnet --tag testnet --message "slop-ninja dashboard feed testnet <sha8>"
```

After the deploy, confirm that the Worker's workers.dev subdomain and
preview URLs are disabled, that an anonymous request to its workers.dev name
gets a 404, and that no zone route or custom domain names it.

Teardown, in order: redeploy the website without the binding, run
`wrangler delete --env testnet` here, redeploy the publisher without its
dashboard binding, then delete the dashboard objects and the bucket.
