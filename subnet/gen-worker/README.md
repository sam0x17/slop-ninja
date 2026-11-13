# Generation Worker (`slop-ninja-gen-testnet`)

The internal Worker that makes generation v2's frontier calls (OpenAI
Responses and Anthropic Messages, streamed) for the testnet eval Worker. It
has no public surface: no route, no custom domain, no `workers.dev` name, no
preview URL and no cron. The eval Worker reaches it only through its service
binding `GEN_TESTNET`; requests must name the internal host, the exact path
and method, and the interface marker (`slop_ninja_gen_core::iface`), and
anything else is a 404. A marker naming another interface version is a 409
`interface_version`, answered before the body is read. Only the eval Worker
may bind this service: it holds both vendors' endpoint credentials, and the
binding plus the marker are its only checks (verify at every deploy review
that no other Worker binds `slop-ninja-gen-testnet`).

- `POST /internal/gen/v1/generate`: one call (`GenCall` in, `GenReply` out).
  The request body is the one the eval Worker built from the generator
  manifest; this Worker adds the key header (and any configured further
  headers) and sends it unchanged to the vendor's configured endpoint, once:
  every non-200 answer is classified from its status and structured fields
  only and returned as a class (throttle, spend cap, key unavailable, cell
  refused, outage). Redirects are never followed (they would carry the key
  header); a 3xx is an outage. The stream is read through the bounded SSE
  reader.
- `GET /internal/gen/v1/health`: `{interface, openai, anthropic,
  openai_route, anthropic_route}`: the interface version, whether each
  vendor's endpoint secret validates with a usable key, and each vendor's
  route (always `configured_endpoint_v1`).

Configured endpoints (`src/route.rs`, interface v2): the coordinator has
access to frontier models through configured credentials, and this Worker
reaches each vendor only at a configured endpoint that speaks the vendor's
wire format. It binds no vendor key and has no other route. Each vendor has
one Worker secret, `GEN_OPENAI_ENDPOINT` or `GEN_ANTHROPIC_ENDPOINT`, put
from stdin with `wrangler secret put`. It holds JSON with the fields `route`
(`configured_endpoint_v1`), `endpoint` (a full https URL), `auth` (`bearer`
or `x-api-key`), `key`, and optional further `headers`, `rate_limit_codes`
and `unsupported_codes`. It carries credentials, so it never goes in a
tracked file. It is read for one call or one health answer, its key and
header values zeroed on drop and never logged or returned. A vendor without
its secret, or with one that does not validate, is unavailable: `/health`
reports it false and every call answers `key_unavailable` without sending
anything, which pauses that vendor in the eval Worker (its frontier items
then fall back to Workers AI). The generator manifest's `routes` entry for
each vendor must be `configured_endpoint_v1` (OpenAI without Flex, Anthropic
with the manifest's optional system prefix, priced at the manifest's
endpoint price): `/health` reports each vendor's route, plan admission
refuses a mismatch, and a request built for the direct route is never sent.
The `global_fetch_strictly_public` flag sends every fetch on the public
path.

Memory: every invocation runs in its own bounded arena scope
(`memory-model.json`, `src/routes.rs`); a call admits its message and parse
from their declared sizes, then its whole heap (reader buffers, reply, a
Flex retry's body) before the key is used or anything is sent, and answers a
throttle without contacting the vendor when the isolate cannot hold it.

Deploy order: this Worker first, then the eval Worker; any change to the
wire types bumps `iface::MARKER_VALUE` (a test pins the field sets to it).
Never roll this Worker back below the eval Worker's interface version. The
tracked `wrangler.toml` holds nothing account-specific; a deploy-time config
overlay (`wrangler.deploy.toml`), if the deploy tooling writes one, is
git-ignored and deleted after the deploy.

Checks:

```sh
cargo test                                   # driver, routes
cargo test --features arena-witness -- --test-threads=1
cargo clippy --all-targets -- -D warnings
cargo clippy --target wasm32-unknown-unknown --release -- -D warnings
```
