# Generation Worker (`slop-ninja-gen-testnet`)

The internal Worker that makes generation v2's frontier calls (OpenAI
Responses and Anthropic Messages, streamed) for the testnet eval Worker. It
has no public surface: no route, no custom domain, no `workers.dev` name, no
preview URL and no cron. The eval Worker reaches it only through its service
binding `GEN_TESTNET`; requests must name the internal host, the exact path
and method, and the interface marker (`slop_ninja_gen_core::iface`), and
anything else is a 404. A marker naming another interface version is a 409
`interface_version`, answered before the body is read. Only the eval Worker
may bind this service: it holds both vendor keys, and the binding plus the
marker are its only checks (verify at every deploy review that no other
Worker binds `slop-ninja-gen-testnet`).

- `POST /internal/gen/v1/generate`: one call (`GenCall` in, `GenReply` out).
  The request body is the one the eval Worker built from the generator
  manifest; this Worker adds the key header and sends it unchanged. OpenAI
  Flex's "429 Resource Unavailable" runs once more on the standard tier;
  every other non-200 answer is classified from its status and structured
  fields only and returned as a class (throttle, spend cap, key unavailable,
  cell refused, outage). Redirects are never followed (they would carry the
  key header); a 3xx is an outage. The stream is read through the bounded
  SSE reader.
- `GET /internal/gen/v1/health`: `{interface, openai, anthropic}`: the
  interface version and whether each key resolves to a usable header value.

Keys come from the account's default Secrets Store (Workers scope):
`SLOP_NINJA_OPENAI_API_KEY_TESTNET` and `SLOP_NINJA_ANTHROPIC_API_KEY_TESTNET`,
bound as `OPENAI_API_KEY` and `ANTHROPIC_API_KEY`. A key is read for one call,
zeroed on drop, and never logged or returned. The store's id is filled in
at deploy time from private configuration (`wrangler.toml` holds a placeholder).

Memory: every invocation runs in its own bounded arena scope
(`memory-model.json`, `src/routes.rs`); a call admits its message and parse
from their declared sizes, then its whole heap (reader buffers, reply, a
Flex retry's body) before the key is used or anything is sent, and answers a
throttle without contacting the vendor when the isolate cannot hold it.

Deploy order: this Worker first, then the eval Worker; any change to the
wire types bumps `iface::MARKER_VALUE` (a test pins the field sets to it).
Never roll this Worker back below the eval Worker's interface version. The
deploy-time config overlay (`wrangler.deploy.toml`, with the Secrets Store id)
is git-ignored and deleted after the deploy.

Checks:

```sh
cargo test                                   # driver, routes
cargo test --features arena-witness -- --test-threads=1
cargo clippy --all-targets -- -D warnings
cargo clippy --target wasm32-unknown-unknown --release -- -D warnings
```
