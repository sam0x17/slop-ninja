# SlopNinja detect Worker

The service behind the free paste-and-score page at `slop.ninja/detect/`.
The website Worker forwards `/api/detect/v1/*` here through its service
binding (`DETECT_TESTNET`). This Worker has no route, no custom domain, no
default `workers_dev` subdomain and no preview URLs. It scores one pasted text with A's
reference span package on one scorer Container and returns an evidence band,
the two document percentages and, in the strong bands only, highlighted
passages. Until the reference package's public bucket opens, every result's
version line says "publication pending".

It shares no Container instance, Durable Object, binding or bucket with
hidden evaluation, and it never sees evaluation text.

## Request path

```
browser ── website Worker ──service binding──► detect Worker
                                                 │  (text stays here)
                       ticket + client key only  ▼
                                            DetectGate (one, memory only)
                                                 │ admitted
                                                 ▼
                                            DetectLane ──► a-scorer Container
```

- `POST /api/detect/v1/warm`: content-free and needs no Turnstile. It is
  answered from the isolate's awake hint while that holds. Otherwise at most
  one warm per Cloudflare location per rate-limit period (a constant-keyed
  rate-limit binding with limit 1) goes on to the gate. The gate starts the
  instance only when none is awake, and a warm never extends the idle timer;
  only scans do. The answer also carries the switches, the Turnstile site key
  and the package line.
- `POST /api/detect/v1/score` with `{"text","turnstile"}`. The checks run in
  this order:
  1. the content type;
  2. the tool switch;
  3. a declared length within the body bound (413 unread above it, 411
     without one);
  4. a reservation from the isolate's scan memory (503 "busy" unread);
  5. the body;
  6. the strict envelope;
  7. Turnstile siteverify (single use, hostname and action `detect_score`
     checked);
  8. the exact token count with the scorer's own tokenizer table;
  9. the length floor and the English screen.

  A refusal answers one JSON object. An accepted text gets an NDJSON stream
  of queue positions, then "starting" when the instance is cold, then the
  result or an error.
- `POST /api/detect/v1/feedback` with `{"receipt","claim"}`. The receipt is
  verified once by the gate and counted as a content-free data point (band,
  length bin, package, button).

## Bounds, derived rather than tuned

| Control | Bound | Source |
|---|---|---|
| Tokens | 2,048 with the frame | the scorer's limit, counted here exactly (`slop-ninja-scorer-tokens`) |
| Body | 2,046 × 512 × 9 + 2,048 + 26 = 9,430,042 bytes | the longest token (512 bytes), the largest escaped-JSON-to-NFC ratio (9, U+01D5), the Turnstile token's documented length and the envelope's fixed bytes; tests recompute 512 from the tokenizer table and 9 from the scorer's NFC crate |
| Text | 2,046 × 512 × 3 bytes | a decomposed character is at most 3 times its NFC bytes (recomputed by a test); a longer text is refused without being tokenized |
| Forwarded body | the scorer's 8 MiB | measured on the body built here; refused, never truncated |
| Isolate scan memory | the memory model's linear memory less the stack, the glue partition, the objects' memory and warm requests | `routes.rs`; each score request reserves 83 bytes per body byte (body, text, NFC copy, the tokenizer's scratch, the forwarded body) plus the reply bound before it reads anything |
| Gate tickets | the gate's 8 MiB table at 128 bytes per ticket | `routes.rs` |
| Concurrent scans | 1 | one standard-3 instance (`max_instances` 1) with one session; more is a decision |

An added token with `lstrip` (`[MASK]`) also takes the whitespace before it,
however long. A text whose length rests on such whitespace is refused by the
body bound as too large, which only ever refuses more. The isolate scan memory
refuses, as too large, any request that could not fit even alone (about 1 MB
of body); no text within the token limit written as ordinary prose comes
near that.

## Evidence rules the result follows

- Bands cut at the release's Calibration-split operating points: 99.988%
  (0.9998820476527482, the top isotonic level) and 90.9%
  (0.9090909090909091, the 1% human-FPR point), and 50%.
- No score under 150 whitespace words; texts over 450 words carry a label,
  because human false positives were measured on 150 to 450 words.
- Passages are shown only in the two strong bands, only with the span switch
  on, never under 24 words, with UTF-16 offsets checked against the scorer's
  byte offsets (any mismatch drops every passage).
- Confidence is floored to one decimal, "above 99.9%" at the top and
  "below 1%" at the bottom; share is whole percent and never reads above
  confidence.
- The English screen is the human pools' `text::english_screen`, ported
  without allocation and checked against the pools' own verdicts on the
  shared fixture `tests/fixtures/english-screen.json`.

## Privacy

- The text stays in the request's memory. Nothing derived from it is stored,
  logged, cached or returned beyond the allow-list in `answer.rs`: no text
  hash, no output digest, no attribution, no timing.
- The gate holds tickets and client keys only. A client key is an HMAC of the
  address's network prefix (the first 24 bits of an IPv4 address, the first
  64 bits of an IPv6 address) under a key held only in the gate's memory and
  replaced each UTC day. It orders the fair queue and refuses nobody.
- Panics write nothing. workers-rs installs a hook that prints the panic
  message, and a string-slicing panic's message quotes the string, so every
  entry point replaces it first (`silence_panics`). The crate also denies the
  lints that panic on input. A test forces a panic in a child process and
  checks that its output holds neither the text nor the message.
- Observability is off, and no Logpush or Tail Worker is attached.
  `tests/source.rs` checks that no source writes to the console and that the
  Wrangler file sets no route, log sink or account.

## Operator switches and the package pointer

`DETECT_TOOL` and `DETECT_SPANS` (`on` or `off`) are the kill switches. The
package vars (`PACKAGE_SHA256`, `PACKAGE_MANIFEST_KEY`, `PACKAGE_SPEC_SHA256`,
`PUBLISHED_PACKAGE_DIGEST`, `PUBLICATION`) are the serving pointer. Each one
changes by a deploy through the deploy wrappers, which journal it, and is
reverted the same way. Nothing switches automatically.

The package copy is server-side (`CopyObject`, same keys, so the files
manifest v2 bytes and the load digest stay the staged package's). Before the
first load, each copy's entity tag and length must match the manifest's
pinned values. The copy needs an R2 S3-API token scoped to read the staging
bucket and write the detect bucket.

## Checks

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo clippy --target wasm32-unknown-unknown --lib -- -D warnings
worker-build --release
```

The comparison test for the English screen also runs in
`grammar/crates/grammar-a-pools` (`tests/english_screen_fixture.rs`).

## Status

The build is complete and offline-tested. Not yet done: the measured memory
model, Cloudflare latency and cold-start measurements, a parity run of the
public synthetic reference cases on the detect Container, the privacy canary
through the live path, and the abuse-path checks. The resources do not exist
yet.
