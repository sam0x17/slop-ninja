# detect-fetch

The stateless Worker behind SlopNinja's public URL check (release R2). The
detect Worker reaches it only through its service binding `DETECT_FETCH`; it
has no route, no binding of its own (outbound Fetch only, never a Tunnel or
VPC binding) and keeps nothing between requests. The contract is the `wire`
crate (`slop-ninja-detect-fetch-wire`), which the detect Worker shares by
path; the URL rules are `subnet/crates/detect-url`.

| Route | Does |
|---|---|
| `POST /robots` | one robots.txt read under RFC 9309 (five redirects, 500 KiB, 4xx allowed, 5xx disallowed), parsed into the group for `SlopNinjaBot`, with content signals and `Crawl-delay` |
| `POST /fetch` | one page: `GET` with the SlopNinjaBot user agent and Web Bot Auth signatures, manual redirects (each hop re-checked by the URL rules, its host screened against 1.1.1.1 for Families, its origin's robots.txt read, `https` to `http` refused), `X-Robots-Tag` and TDM checks, the lol_html strip pass into at most `HTML_BOUND` bytes with the depth and element bounds, then dom_smoothie extraction; Bluesky posts through the public AppView |

Modules: `fetch.rs` (the pipeline), `redirects.rs`, `robots.rs`, `screen.rs`,
`strip.rs`, `extract.rs`, `bound.rs` (every bound and its derivation),
`bluesky.rs`, `sign.rs` (Web Bot Auth), `fresh.rs`, `net.rs` (the network
trait and the in-process fakes the tests use), `routes.rs` (arena routes and
the isolate's fetch memory) and `cf.rs` (the wasm32 export).

Configuration: vars `DEPLOY_ENV`, `SITE_ORIGIN`, `SECRET_ORIGIN`, and on
staging only `URL_FIXTURE_PREFIX`; the secret `WEB_BOT_AUTH_KEY` (an Ed25519
private JWK). Its public key is published in the site's
`/.well-known/http-message-signatures-directory` (`sign::Signer::directory`).

Checks:

```sh
cargo test                                   # unit tests with in-process fakes; no test reaches the network
cargo test --features arena-witness arena_witness -- --test-threads=1 --nocapture
worker-build --release                       # the wasm32 build
```
