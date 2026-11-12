# SlopNinja detect Worker

The service behind the free paste-and-score page at `slop.ninja/detect/`.
The website Worker forwards `/api/detect/v1/*` here through its service
binding (`DETECT_TESTNET`). This Worker has no route, no custom domain, no
default `workers_dev` subdomain and no preview URLs. It scores one pasted text with A's
reference span package on one scorer Container and returns an evidence band,
the two document percentages and, in the strong bands only, highlighted
passages. A text of 24 to 149 words gets one result instead, "strong signs"
(with its passages) or "no strong signs", with no percentage. A text of more
than 1,000 words is scored on its first 1,000 words, cut back to the last
sentence end, and the result says how many words it scored of how many.
Until the reference package's public bucket opens, every result's
version line says "publication pending".

It shares no Container instance, Durable Object, binding or bucket with
hidden evaluation, and it never sees evaluation text.

## Request path

```
browser ── website Worker ──service binding──► detect Worker ──receipts──► DetectFeedback
                                                 │  (text stays here)       (one, memory only)
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
  3. a declared length within the body bound, whose full footprint fits the
     isolate's scan memory (413 unread above either, 411 without a length);
  4. the intake reservation from the scan memory: 2 bytes per declared body
     byte, for the body and the unescaped text (503 "busy" unread);
  5. the body, which must arrive within Turnstile's 300 s token validity
     (408 past it; a later body carries an expired token anyway);
  6. the strict envelope;
  7. Turnstile siteverify (single use, hostname and action `detect_score`
     checked);
  8. the rest of the reservation (503 "busy");
  9. the free cut (`cut.rs`, "The free cut" below), with exact token counts
     from the scorer's own tokenizer table;
  10. the scored part's length floor (24 words, or 150 with the span switch
      off) and the English screen on it.

  Only the scored part is forwarded to the scorer.

  A refusal answers one JSON object. An accepted text gets an NDJSON stream
  of queue positions, then "starting" when the instance is cold, then the
  result or an error. A line the reader does not accept within one hold
  (`QUEUE_HOLD_MS`) ends the stream.
- `POST /api/detect/v1/feedback` with `{"receipt","claim"}`. It needs no
  challenge, so it never reaches the admission gate. A receipt of the wrong
  shape is refused in the Worker. Any other goes to the `DetectFeedback`
  object, which accepts each valid receipt once; it is then counted as a
  content-free data point (band, length bin, package, button). A flood of
  junk or replayed receipts loads only that object, and a scan waits at most
  one hold for its receipt (it answers without one otherwise).

## The free cut

The free check scores at most the first 1,000 whitespace words of a text and
never stops inside a sentence: a text that stops mid-sentence looks
AI-written to the detector far more often (`docs/detect-evidence.md`, "Long
texts"). In order:

1. **C1.** A text of more than 1,000 words is scored from its start to the
   last unit end at or before the start of word 1,001. A unit end is a
   sentence end (".", "?" or "!" followed by whitespace; not `said."` and
   not "…") or the end of the last character before a blank line. If the
   first 1,000 words hold none, the cut falls at the last line end there
   (chat logs and other text written one line per utterance).
2. **C2.** If that part (or a text of 1,000 words or fewer) is over the
   scorer's 2,048 tokens with the frame, it is cut back to the last unit end
   inside them (the last line end if none fits), found by binary search with
   exact counts.
3. **C3.** If neither finds a unit end or a line end, nothing is scored:
   refusal `no_sentence_end` with the text's `total_words`. A cut leaving
   fewer than 24 words gets `too_short`, as any short text does.

The length bin, the English screen, the notices and the passages all belong
to the scored part. A result carries `words` (scored), `total_words` (sent),
`cut` (`none`, `first_1000_words` or `token_limit`) and `scored_end`, the
scored part's end as a UTF-16 offset into the text sent, so the page can grey
the rest. Refusals of a text after the cut carry the same counts and `cut`.
The rule is the long-text measurement's own: on its 3,457 consumed-Test and
Calibration sources over 1,000 words, C1 here gives byte-identical texts. The
1,000 words are a product rule, not a measured bound; the measured range
reaches them.

## Bounds, derived rather than tuned

| Control | Bound | Source |
|---|---|---|
| Tokens | 2,048 with the frame | the scorer's limit, counted here exactly (`slop-ninja-scorer-tokens`) |
| Body | 2,046 × 512 × 9 + 2,048 + 26 = 9,430,042 bytes | the longest token (512 bytes), the largest escaped-JSON-to-NFC ratio (9, U+01D5), the Turnstile token's documented length and the envelope's fixed bytes; tests recompute 512 from the tokenizer table and 9 from the scorer's NFC crate. With the free cut a longer body could still hold a scorable first 1,000 words, so this is the free route's read budget; the scan memory below admits less |
| Text | 2,046 × 512 × 3 bytes | a decomposed character is at most 3 times its NFC bytes (recomputed by a test); the free cut never tokenizes a longer prefix |
| Forwarded body | the scorer's 8 MiB | measured on the body built here; refused, never truncated |
| Isolate scan memory | the memory model's linear memory less the stack, the glue partition, the objects' memory and warm requests | `routes.rs`; each score request reserves 83 bytes per body byte (body, text, NFC copy, the tokenizer's scratch, the forwarded body) plus the reply bound: 2 of those bytes before it reads anything, the rest once Turnstile passes |
| Gate tickets | the gate's 8 MiB table at 256 bytes per ticket (with its waiting ask's channel) | `routes.rs` |
| Used receipt ids | the feedback object's 1 MiB at 64 bytes per id; past it the object starts a new key, which only loses votes on older receipts | `routes.rs`, `receipt.rs` |
| Concurrent scans | 1 | one standard-3 instance (`max_instances` 1) with one session; more is a decision |

An added token with `lstrip` (`[MASK]`) also takes the whitespace before it,
however long. A text whose length rests on such whitespace is refused by the
body bound as too large, or cut shorter than it had to be, which only ever
refuses or cuts more. The isolate scan memory refuses, as too large, any
request that could not fit even alone (about 1 MB of body, well over a
hundred thousand words of ordinary prose); the page then asks for a section
of the document.

## Liveness

Every wait is bounded, and nothing needs an operator to recover
(`waits.rs`, `cf_lane.rs`).

| Wait | Value | What it bounds |
|---|---|---|
| `SCORER_START_WAIT_MS` | 60 s | a started instance answering its health check |
| `LOAD_WAIT_MS` | 180 s | the package load, or a wait for one already running |
| `SCORE_WAIT_MS` | 60 s | one score call and its reply (a 2,048-token scan is about 5 s warm on standard-3) |
| `QUEUE_HOLD_MS` | 20 s | a waiting ask (its call gets two holds), the score path's other internal calls, each line written to the reader |
| scan lease | derived: the three waits plus one hold | an admitted ticket at the gate |

- The lane works within the time the Worker sends: what is left of the
  three waits since the ticket's admission. Every instance call, health
  checks included, is raced against its stage's end, and a wait for a load
  running in the alarm counts against the same time. The lease is that
  bound plus one hold, so the gate never admits the next ticket while the
  lane may still be busy.
- A score call that outlasts `SCORE_WAIT_MS` destroys the instance. A
  failed call destroys it unless it still reports the package ready (then
  only the request failed). A start or load that fails destroys it, and new
  starts back off from `SCORER_START_WAIT_MS`, doubling, never past the idle
  timeout.
- A waiting ask is answered early only when its ticket is admitted (or
  gone), otherwise at the end of its hold with its current position. A
  queue change therefore wakes one ask, not every waiting one.

The values are deployment parameters from measured times, re-measured at
the launch gate; they are not abuse thresholds.

## Evidence rules the result follows

- Bands cut at the release's Calibration-split operating points: 99.988%
  (0.9998820476527482, the top isotonic level) and 90.9%
  (0.9090909090909091, the 1% human-FPR point), and 50%.
- The four bands apply from 150 to 1,000 scored words. The band table was
  measured at 150-450 words (bin `150_450`); for texts that end at a sentence
  end, its human rates were measured to hold at 451-1,000 words (bin
  `451_1000`, so the page can say so and add that a small AI part in a long
  text is caught less often; `docs/detect-evidence.md`, "Long texts"). A
  longer text is scored on its first 1,000 words ("The free cut").
- From 24 to 149 words (bins `24_49`, `50_99`, `100_149`) the result is
  `strong` only when the confidence reaches a release level and a passage's
  own confidence reaches a level of the span confidence map, per bin: 24-99
  words, the top level (99.988%) and a passage at 96.38% or more; 100-149
  words, 90.9% and a passage at 94.20% or more. Otherwise it is
  `no_strong_signs`. The pairs were chosen on the Calibration split by the
  release's 1% rule and measured on the consumed Test
  (`docs/detect-evidence.md`, "Short texts"); they belong to span package
  d24. A short result carries no percentage, share or passage confidence,
  and passages only when `strong`.
- A short text needs passages: with the span switch off it is refused as
  `too_short` with `min_words` 150, and so is a scored short text whose
  passages are missing or fail the checks below. Under 24 words (the span
  floor) nothing is scored.
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
- The feedback object holds its receipt key and the ids already voted, in
  memory only. A receipt is a random id, the band and the length bin under
  an HMAC tag: nothing derived from the text.
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
model, Cloudflare latency and cold-start measurements (including the
liveness waits above), a parity run of the public synthetic reference cases
on the detect Container, the privacy canary through the live path, and the
abuse-path checks. Those include:

- a flood of junk and replayed feedback receipts never reaches the gate, and
  scans keep their admission latency during it;
- a slow score body holds only its intake reservation, and is cut at the
  token validity (or the edge is shown to buffer request bodies);
- a hung or failing instance is destroyed and the lane serves again without
  an operator;
- waiting tickets cost one ask per hold each, whatever the queue's changes.

The resources do not exist yet.
