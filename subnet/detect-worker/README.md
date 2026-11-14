# SlopNinja detect Worker

The service behind the free paste-and-score page at `slop.ninja/detect/`.
The website and secret-host Workers forward its routes here through their
service bindings (`DETECT_TESTNET`). This Worker has no route, no custom
domain, no default `workers_dev` subdomain and no preview URLs. It scores one pasted text with A's
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
           (strips x-sn-*, sets X-SN-Host       │  (text stays here)       (one, memory only)
            and X-SN-Route)                     │
                 ticket, class and client key ▼     hourly counts (content-free)
                                            DetectGate ──alarm──► detect store (D1)
                                                 │ admitted
                                                 ▼
                                            DetectLane ──► a-scorer Container
```

Every route first checks how the request arrived (`routes::forwarded`):

- A request carrying `X-SN-Route` is a forwarded browser request. Its route
  id must be the path's, its `X-SN-Host` one of the two configured hosts'
  values (from `SITE_ORIGIN` and `SECRET_ORIGIN`) and admitted by the route's
  row of the forwarded route table, and the CSRF check of `auth-core`
  applies (Fetch Metadata and `Origin` against that host's origin).
- `X-SN-Principal` is accepted only on the routes whose table row takes one,
  from the site host. Of the routes served here only `share` does: a free
  check is the same request for everyone, and a principal on it refuses the
  request.
- A request with neither header is a binding call no forwarder made. It is
  served, and its scans are recorded as `uncovered_path`, never counted.
- An `/internal/` request carrying `X-SN-Route` or `X-SN-Host` is refused,
  and only the owner route takes `X-SN-Principal` (and needs it).
- No header outside the forwarders' allow-list is read, and never a cookie
  (`tests/source.rs`).

Refusals answer one JSON object, `{"state":"refused","code":…}` with the
counts some codes carry, under the HTTP status of its code
(`slop-ninja-detect-page`'s refusal table).

- `POST /api/detect/v1/warm`: content-free. It is answered from the
  isolate's awake hint while that holds. Otherwise at most one warm per
  Cloudflare location per rate-limit period (a constant-keyed rate-limit
  binding with limit 1) goes on to the gate. The gate starts the instance
  only when none is awake, and a warm never extends the idle timer; only
  scans do. The answer carries `{state, tool, spans, min_words, package}`.
- `POST /api/detect/v1/score` with `{"text"}`. The checks run in this order:
  1. how the request arrived (above);
  2. the content type;
  3. the switches: each the deploy-time var AND the detect store's
     `switches` row, read in one statement; when the row cannot be read the
     var decides and an `error {d1}` point is written, so a store outage never
     stops free checks;
  4. a declared length within the body bound, whose full footprint fits the
     isolate's scan memory (413 unread above either, 411 without a length);
  5. the intake reservation from the scan memory: 2 bytes per declared body
     byte, for the body and the unescaped text (503 "busy" unread);
  6. the body, which must arrive within `BODY_WAIT_MS` (408 past it);
  7. the strict envelope;
  8. rule `x_post_v1` (`xpost.rs`): a text that is only the link to a post on
     X scores nothing and takes no ticket. It answers `x_post_link` with the
     post's canonical URL (`https://x.com/i/status/<id>`) while `DETECT_URLS`
     and `DETECT_X` are both on, and `x_off` otherwise;
  9. the rest of the reservation (503 "busy");
  10. `canon_v1` (`canon.rs`): CRLF and CR become LF, surrounding whitespace
      is trimmed and the text is NFC under the scorer's own normalizer, so
      the scorer's outputs do not change; the seal binds this form and keeps
      its offsets, and the answer's offsets are mapped back to the exact
      string the page sent;
  11. the free cut (`cut.rs`, "The free cut" below), with exact token counts
      from the scorer's own tokenizer table;
  12. the scored part's length floor (24 words, or 150 with the span switch
      off) and the English screen on it.

  Only the scored part is forwarded to the scorer. Every refusal before a
  ticket is an AE `submit` point and nothing else.

  An accepted text gets an NDJSON stream of queue positions, then
  "starting" when the instance is cold, then the result, or a refusal line.
  A line the reader does not accept within one hold (`QUEUE_HOLD_MS`) ends
  the stream. The result line carries `seal`: the result encrypted under a
  day key (below), with which the website can render it again, make a secret
  link of it, or quote a paid check, without scoring the text again.
- `POST /api/detect/v1/feedback` with `{"receipt","claim"}`. It never
  reaches the admission gate. A receipt of the wrong shape is refused in the
  Worker. Any other goes to the `DetectFeedback` object, which accepts each
  valid receipt once; it is then counted as a content-free data point (band,
  length bin, package, button). A flood of junk or replayed receipts loads
  only that object, and a scan waits at most one hold for its receipt (it
  answers without one otherwise).

## The admission gate

`DetectGate` admits one scan per slot (one at launch):

- **Classes.** `interactive` (a free paste), `url` (a public URL check),
  `paid` and `background` (operator re-runs). Admission alternates `paid`
  and the free group (`interactive` with `url`) while both wait; within a
  group it goes round-robin across client keys, first in first out per key;
  `background` goes only when nothing else waits.
- **Client keys.** An HMAC, under a key held only in the gate's memory and
  replaced each UTC day, of the address's network prefix (anonymous), the
  owner hash (signed-in paid) or the API key's hash. A client key with an
  admitted ticket gets no second admission until the first is done.
- **Position-scaled hold.** A waiting ticket at position p asks again after
  `max(QUEUE_HOLD_MS, (p - 1) × the fastest measured scan / slots)`, the
  earliest it could be admitted, so the gate's request rate grows with the
  logarithm of the line, not its length. Its admission still wakes it at
  once.
- **Hourly counts.** The release that ends a scan carries its channel
  (`free_text` for the site host's score route), the package it ran under and
  its outcome (`scored`, `uncovered_path` or a refusal code). The gate adds it
  to the hour's row in its own SQLite before answering, so an eviction loses
  no acknowledged count, and an alarm writes the rows to the store's
  `hour_counters` as absolute values (`counters.rs`): a repeated flush writes
  the same numbers, and the gate is the table's only writer.

## Result seals

`seal.rs`: `base64url(v ‖ nonce96 ‖ AES-256-GCM(K(day), nonce, json))` with
AAD `slop_ninja_detect_seal_v1`, where `K(day)` is HKDF-SHA256 of the Worker
secret `DETECT_STORE_SECRET` with info `"seal"` and the UTC day. The payload
is `{day, text_sha256, scored_end, cut, package_load, policy_v, result_full,
issued_ms}`, so the answer carries no text-derived value in the clear. A
seal opens under today's or yesterday's key (24 to 48 hours) and only for
the text whose `canon_v1` form's SHA-256 it names; its nonce's hash lets a
link be made from it once. Without the secret a result carries no seal.
`handoff.rs` turns an opened seal's result into the store's record, only
under the package that scored it.

## The store

Secret links, pages and the background work run as pure flows over one SQL
and I/O seam (`db.rs`), tested natively on the store's own migration, with
the Cloudflare glue in `cf_store.rs` and the routes in `cf_routes.rs`:

- `POST /api/detect/v1/share` (Get a link, from the site host, with
  `X-SN-Principal` when saved), `/manage` (the secret host), `/rerun` (a
  secret page's re-run: the same cut, ticket class and lane as a paste, and
  counted like one) and `/state` (a pending public page's poll);
- the binding-only `/internal/v1/` routes: `page`, `preview`, `card`,
  `states`, `owner`, `examples`, `example`, `sitemap/<n>` and `removals`;
- the `detect-jobs` consumer (one message per invocation; purges, owner
  purges and re-keys, legal notices, sitemap marks) and the hourly cron
  (expiry, the orphan backstop, stale claims, sitemap shards).

A secret page's text is stored only in R2, encrypted under a key derived
from its own slug and the slug pepper (`DETECT_SLUG_PEPPER_V1`), so the
store's rows hold nothing of it. The link stores, and a preview renders, the
text's `canon_v1` form, the string the scorer read, so the seal's passages
fall on the words the result named whichever equal-canonical string made
the request. A link or a preview reserves its footprint
from the scan memory by its declared length, and a document read by the
document's size.

## Paid checks

The paid whole-document check (`paid.rs`, its sections in `sections.rs`, the
Cloudflare glue in `cf_paid.rs`) runs on the account Worker's ledger through
the `ACCOUNT` binding; at launch only operator grants back it.

- `POST /api/detect/v1/paid/quote` (the site host, signed in): the free
  check's seal must bind the text under the serving package; the whole text
  passes the English screen; the sections are planned and section 1 must be
  the seal's free cut. The reader is billed the content tokens past the free
  cut (at least 128), and the answer carries a quote seal binding the text,
  the account, the free seal and the hold's idempotency key.
- `POST /api/detect/v1/paid/start`: the hold, then the job text and section
  1's result in R2 (under the page's own key, with the job AAD), then one
  batch inserts the pending page and its job. A seal starts one check.
- `POST /api/detect/v1/job` (the secret host): an NDJSON stream of job lines
  while one driver at a time scores the sections, fenced by a lease token
  (CLAIM, RENEW, ADVANCE, SCORED). Each section takes a gate ticket of class
  `paid` keyed by the page's owner hash and is counted on channel
  `paid_page` as an operation, never as pot basis. The page goes live in the
  SCORED batch; the hold commits after it, and a failed commit retries
  through `commit_hold` and the hourly sweep. A section refused for good
  fails the job and the page and releases the hold; deleting the page
  cancels a running job and its hold goes back the same way.

**Sections** (rules W1 to W6): at most 1,000 words and 2,046 content tokens
each, cut with the free check's own rule; consecutive sections overlap by at
least 48 words (two span floors); the last section is the longest tail that
fits; a stretch with no sentence or line end within a section's reach, or a
section the rules cannot make long enough or that one scan cannot hold, is
listed in the quote and neither scored nor billed; each overlap is split at
its middle word, and owned words weight the document's share.

**Bounds.** A paid text is at most `PAID_TEXT_BYTES` (2 MiB canonical, about
340,000 words): its job holds the text, its plan and the assembled document
in half the scan memory, one section's scan in the other half. Quotes and
starts reserve their footprint by their declared body
(`routes::paid_footprint`), the job by its text's size, and each section its
own scan. The free check takes the whole document first, and its own
reservation (98 bytes per body byte) is the tighter bound in practice: about
860 KB, some 140,000 words.

**Subrequests.** A job invocation counts its subrequests against the
documented limit per invocation (10,000); every D1, R2, gate, lane and
ledger call counts.
A section takes about six (RENEW, the gate's enter and done, the lane, the
section's put, ADVANCE) plus its gate asks, and the end two per section read
back plus a fixed few: a 100,000-word document (107 sections) measured 860
in one invocation. A driver starts a section only while the budget holds one
more section and the whole end; otherwise it stops cleanly, lets its lease
go, and the next open resumes.

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
| Body | 2,046 × 512 × 9 + 11 = 9,427,979 bytes | the longest token (512 bytes), the largest escaped-JSON-to-NFC ratio (9, U+01D5) and the envelope's fixed bytes; tests recompute 512 from the tokenizer table and 9 from the scorer's NFC crate. With the free cut a longer body could still hold a scorable first 1,000 words, so this is the free route's read budget; the scan memory below admits less |
| Text | 2,046 × 512 × 3 bytes | a decomposed character is at most 3 times its NFC bytes (recomputed by a test); the free cut never tokenizes a longer prefix |
| Forwarded body | the scorer's 8 MiB | measured on the body built here; refused, never truncated |
| Isolate scan memory | the memory model's linear memory less the stack, the glue partition, the objects' memory and warm requests | `routes.rs`; each score request reserves 83 bytes per body byte (body, text, NFC copy, the tokenizer's scratch, the forwarded body) plus the reply bound: 2 of those bytes before it reads anything, the rest once its envelope parses |
| Gate tickets | the gate's 8 MiB table at 256 bytes per ticket (with its waiting ask's channel) | `routes.rs` |
| Count flush | 20 rows per statement, 50 statements per alarm | D1's 100 bound parameters per statement (5 per row) and its smallest documented queries per invocation; a longer backlog takes further alarms |
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
| `QUEUE_HOLD_MS` | 20 s | the shortest waiting ask (its call gets two holds; the gate scales it with the place in line), the score path's other internal calls, each line written to the reader |
| `BODY_WAIT_MS` | 300 s | a score request's body arriving whole; meanwhile it holds only its intake reservation |
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
- A reader that left while its ticket waited costs no scan: the stream is
  checked before the lane is called.

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
  hash, no output digest, no attribution, no timing. Only Get a link stores
  a text, encrypted, as its secret page (above).
- The gate holds tickets and client keys in memory, and content-free hourly
  counts in its own storage. A client key is an HMAC of the
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

`DETECT_TOOL`, `DETECT_SPANS`, `DETECT_URLS` (`on` or `off`) and `DETECT_X`
(`off`, `on` or `purged`) are the kill switches read here. Each is its var
AND the detect store's `switches` row: a var set to its off state is the hard
off, and the `set_switch` op flips the row at once. The package vars (`PACKAGE_SHA256`, `PACKAGE_MANIFEST_KEY`, `PACKAGE_SPEC_SHA256`,
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
- a slow score body holds only its intake reservation, and is cut at
  `BODY_WAIT_MS` (or the edge is shown to buffer request bodies);
- a hung or failing instance is destroyed and the lane serves again without
  an operator;
- waiting tickets cost one ask per (scaled) hold each, whatever the
  queue's changes, and a stream left idle for a long hold stays open;
- the hourly counts survive a restarted gate and a repeated flush (local
  Workers run; the SQL is tested natively against both schemas).

The resources do not exist yet.
