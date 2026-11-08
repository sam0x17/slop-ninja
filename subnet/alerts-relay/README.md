# Alerts relay (testnet)

A private Cloudflare Worker that delivers SlopNinja testnet alerts to a
private alert feed in its own bucket (decision D18): one content-free object
per event, which the operator reads with private tooling
(see [Alert feed](#alert-feed-slop_ninjaalert_feedv1)). Email is an
optional second path, off by default. The relay has no route, no
`workers.dev` name and no preview URLs: the publisher's service binding is
the only way in, and its own one-minute cron does the rest. Addresses, when
email is on, are deploy-time secrets read from the operator's private files;
no tracked file holds one.

**State:** the email build is deployed on testnet (2026-11-02), private: no
route, no `workers.dev` name, no preview URLs, its bucket without r2.dev or a
custom domain. The publisher posts to it. Drills of the email build: the
drill email, the publisher's `config_warning` (fired and resolved), the
relay's `publisher_silent` (fired while the publisher sent no heartbeat,
resolved at the first one), and a tracker upload with no alert (nothing
sent). Email was turned off the same day (`ALERT_EMAIL_TO` removed). The
feed build described here is not deployed yet.

## Inputs

1. **The publisher's alert posts.** The publisher posts its events (firing,
   reminder, resolved; schema `slop_ninja.publisher.alert/v3`) to
   `POST /alerts` through its `ALERTS_RELAY_TESTNET` service binding. Each
   event carries an id derived from the publisher's alert state and the
   fields it is derived from; the relay recomputes every id and refuses a
   post whose ids do not match. It delivers each id once per sink: one feed
   object, and one email while email is on (see
   [Sent-event ledger](#sent-event-ledger)).
   - It answers 200 only when every event of the post is settled in every
     enabled sink (delivered now or before), so the publisher acknowledges
     exactly then; otherwise 503, and the publisher's next run retries.
   - It answers within half of the publisher's request bound
     (`wire::RELAY_ANSWER_MS`, from the moment its Durable Object starts the
     request; the writes and the send get what is left of it). The other
     half covers the hops the relay cannot time, so a delivery the relay
     records is normally answered before the publisher stops waiting.
   - When the answer is lost anyway, the publisher keeps the firing marked
     as attempted. If the condition clears before a retry is acknowledged,
     the publisher posts an unconfirmed resolution. The relay delivers it to
     a sink only when that sink's ledger rows show the firing may have gone
     out there, and settles it without delivering otherwise. A delivered
     firing is therefore never left without its resolution.
2. **A dead-man check on the publisher.** After every run whose alert phase
   finished and saved its state, the publisher posts a heartbeat
   (`slop_ninja.publisher.heartbeat/v3`) to `POST /heartbeat`.
   - The relay's cron alerts when the heartbeats stop (`publisher_silent`).
     It then also reads the coordinator Workers' `/status` itself with the
     relay's own status tokens (`coordinator_unwatched`), with the
     watermark bound the publisher's last heartbeat carried. With the
     relay's token, a Worker with the status gate answers only the liveness
     fields this rule reads: the confirmed watermark's age, the durable
     fault's kind and the signing state. It sends no cohort status, which
     `/status` embeds for the other callers, and no arena or liveness
     record. The relay does not read `/eval/status`, so it stands in for
     part of the publisher's rules only (see [On call](#on-call)).
   - **Status check after a token change.** The relay's state keeps a
     fingerprint (SHA-256, domain-separated) of the two status tokens whose
     last check answered. On the first tick after a deploy whose tokens
     differ (put, changed or deleted), and on the first tick of a build
     without a stored fingerprint, the relay reads both Workers' `/status`
     once even while the publisher is up and logs
     `relay: status check coordinator <outcome> eval <outcome>`. Each
     outcome is `no_token`, `unanswered` or `ok caller=<caller>`, the caller
     that Worker recognized (`access.caller`: `alerts` for the relay's own
     token, `shared` for the pre-split shared token, `none` before the
     status gate). An `unanswered` read is logged as an error and checked
     again at the next tick, at most two reads a tick, as the dead-man
     check reads while the publisher is silent. The check raises no rule
     and the rules see the reads only while the publisher is down. It
     verifies a token change (coordinator `TESTNET.md`, "Status token
     split", steps 4 and 5) without stopping the publisher. The relay's
     state also keeps the last check's outcome (content-free: its time and
     the two outcomes, `settled` false while a read was unanswered), and
     every `latest.json` states it as `status_check` until the next check
     replaces it, so a check whose log line nobody saw is read after the
     fact from the bucket's `feed/v1/latest.json`. A live `wrangler tail`
     (testnet env) started before the deploy shows the same line as it
     happens.
   - A crashed publisher, a stopped publisher cron, a state object that does
     not answer and a broken binding all stop the heartbeat. The relay is a
     separate Worker, so none of those stops it.
   - Each heartbeat carries the post schema the publisher's build sends and
     how many due events its run left undelivered, since which run. The
     relay also records every post and heartbeat it refuses for its content
     (size, schema, network, ids), until the next one it accepts. So alert
     posts that keep failing while heartbeats arrive are reported too
     (`publisher_alerts_undelivered`). This covers a skew between the two
     Workers' builds, an event that is always refused, and a send path
     that keeps failing.
3. **The spend tracker's upload.** The operator's private spend tracker
   uploads the alert fields and month totals of its latest run to this
   relay's private bucket (`spend-tracker/latest.json`, schema
   `slop_ninja.spend_tracker.relay.v2`) after each scheduled run. The upload
   also states the tracker's own schedule period (`period_secs`, from the
   tracker's configuration beside its timer) and how many of its queries
   failed (never their text).
   - The cron alerts when a month's alert becomes active and is not yet
     acknowledged, reminds while it lasts, and resolves when it is
     acknowledged (with the acknowledgment time) or no longer active.
   - It also alerts when the tracker's output is older than two of its
     schedule periods, when the bucket has not answered for that long, when
     the tracker could not authenticate to Cloudflare, and when its runs
     have not priced everything for two schedule periods (its totals are
     then lower bounds).
   - The threshold itself never leaves the operator's private spend tracker.

The cron is outside the coordinator and the publisher. It reads and
delivers only and advances no protocol state. Every tick rewrites the feed's
`latest.json`, the relay's heartbeat, which a reader outside Cloudflare can
use as its dead-man check. The relay can also ping an external
dead-man URL (`HEARTBEAT_URL`, enabled only by `HEARTBEAT_ENABLED =
"true"`). The ping means "the cron ran and the delivery path works as far
as the relay knows": it is skipped after a busy or stopped tick, a store
error or a refused configuration, and while the relay's writes or sends
have been failing for two cron periods (see [Limits](#limits)).

## Rules (`slop_ninja.alerts_relay.rules/v1`)

| Rule | Subject | Fires when |
| --- | --- | --- |
| `publisher_silent` | `publisher` | no heartbeat for longer than the publisher's run lease plus one cron period, held for two cron periods |
| `publisher_alerts_undelivered` | `publisher` | held two cron periods: the publisher's last heartbeat reports due events left undelivered or names a post schema this relay does not read, or the relay refused the publisher's posts since it last accepted one |
| `coordinator_unwatched` | `coordinator`, `eval_worker` | while the publisher is silent past that hold: the Worker's `/status` does not answer (held two cron periods), holds a journal or signing fault, or its confirmed watermark is older than the watchdog silence plus two cron periods |
| `spend_alert` | the month | the tracker's upload lists the month as an active, unacknowledged alert |
| `spend_tracker_stale` | `tracker` | the tracker's newest output is older than two of its schedule periods, no readable upload exists, or the bucket has not answered for two schedule periods |
| `spend_tracker_blind` | `tracker` | the tracker's newest readable run could not authenticate to Cloudflare |
| `spend_tracker_incomplete` | `tracker` | held two of the tracker's schedule periods: its newest readable runs did not price everything (a non-zero exit, `complete` false, or alert state `undetermined`) |
| `relay_config_warning` | `relay` | an optional relay setting did not parse, so its default is in force (the alert names the settings, never their values) |

Every bound comes from a deployment parameter, never a tuned constant:

- The publisher's cron period, run lease, unanswered-source hold, status
  read bound and watermark bound are constants of the shared wire crate
  (`wire/`). The publisher builds with the same constants (its lease, its
  rules and its status reads use them, and a publisher test checks its
  cron line).
  - A run killed mid-step blocks the next runs' alert phase for one lease
    (two cron periods), and the next run starts within one period.
  - The publisher counts a source as unreachable after two missed runs,
    and the relay holds `publisher_silent` and
    `publisher_alerts_undelivered` for the same two periods (one failed
    post is retried by the next run).
  - Today: silent after 3 minutes, reported after 5 (plus at most one
    relay cron period).
- The coordinator check uses the publisher's own `watermark_stalled` bound
  (its `WATCHDOG_SILENCE_SECS` plus two cron periods), which each heartbeat
  carries. The relay's `WATCHDOG_SILENCE_SECS` is only the fallback before
  the first heartbeat.
- The tracker is stale after two of its schedule periods, which each upload
  states (`period_secs`; the timer runs daily). The relay's
  `TRACKER_PERIOD_SECS` is only the fallback before the first upload that
  states one. Its age counts from the older of its own `generated_at` and
  the bucket's upload time, so a tracker clock that runs ahead cannot hide
  a dead tracker. `spend_tracker_incomplete` holds for the same two
  periods.
- Reminders follow the publisher's `ALERT_RENOTIFY_SECS`, which each
  heartbeat carries. It is never below one cron period: the publisher
  refuses a shorter value, and the relay raises a smaller value in a
  heartbeat to one period.

Each rule fires once, reminds every renotify interval while it lasts and
posts a resolution with what resolved it. The hold/renotify state machine is
the publisher's, shared through the wire crate. Unknown is never a
resolution:

- while the upload is missing or unreadable, the open spend and blind
  alerts stay open;
- while the bucket does not answer, nothing the tracker raised changes,
  until the bucket has not answered for two schedule periods.

## On call

Every alert names its rule and subject. What to do:

| Rule | First steps |
| --- | --- |
| `publisher_silent` | Check the publisher's deployments and live log (`wrangler deployments list`, `wrangler tail`, testnet env). A refused heartbeat in the detail means the two Workers run different wire builds: deploy both from one commit. Until it is back, the relay's `coordinator_unwatched` alerts stand in for the publisher's `durable_fault`, `watermark_stalled` and `status_unreachable` rules only. The cohort, scorer and budget, commit, reveal, ledger and publication rules (`/eval/status` and the publisher's own work) are unwatched until the publisher is back: check `/eval/status` by hand meanwhile. |
| `publisher_alerts_undelivered` | The publisher's own alerts are not reaching the feed. Read the detail: a schema or `event_id` refusal means a build skew (deploy both from one commit); `too_large` means one event the publisher could not fit; undelivered events with no refusal mean the relay's writes or sends fail (its live log shows the code and message, and `latest.json` the last write). |
| `coordinator_unwatched` | Treat it as the publisher's rule of the same fault: a stalled watermark (`watermark_stalled`), a journal or signing fault (`durable_fault`), an unanswered status (`status_unreachable`). Follow the coordinator runbook (`subnet/coordinator/TESTNET.md`). It covers only these three rules; see `publisher_silent`. |
| `spend_alert` | Investigate the month with the operator's private spend tracker and acknowledge it there. Nothing pauses on spend. |
| `spend_tracker_stale` | Check the operator's private spend tracker: its schedule, its last run and its Cloudflare login (an expired login stops the uploads too). If the detail says the bucket has not answered, check the bucket binding and R2. |
| `spend_tracker_blind` | The tracker ran but could not authenticate: check its Cloudflare login in the operator's private spend tracker. |
| `spend_tracker_incomplete` | The tracker's runs keep failing to price something (rate limits, scope or API changes, an unknown product), so a crossed threshold may go unreported. Read the last runs' logs and their run failures in the operator's private spend tracker. The detail gives the exit code, the failure count and the alert state only. |
| `relay_config_warning` | An optional relay setting does not parse and its default is in force (figures off, the external ping off, or a default period). Fix the deploy-time var or secret named in the detail and redeploy the relay. |
| External monitor pages (when enabled) | The relay's cron stopped, its tick fails, its configuration is refused, or its writes or sends have failed for two cron periods. Read the relay's live log: the tick line shows `delivery_ok`, and a failed write or send logs its code and message. |

The publisher's own rules (their table is in `subnet/publisher/src/alerts.rs`
and its README) map to the coordinator runbook the same way. Their
resolutions say what cleared them.

## Alert feed (`slop_ninja.alert_feed/v1`)

Every event the relay delivers (the publisher's posts, the relay's own
dead-man, tracker and configuration rules) is written into its private bucket
as one JSON object, once per event id ([`src/feed.rs`](src/feed.rs)):

- **Keys** are `feed/v1/events/<UTC>-<event id>.json`, the UTC to the
  millisecond (`20261102T195600123Z`), so a lister reads them in write
  order. The relay stamps them strictly increasing inside its Durable
  Object. A post and a tick can still finish their puts out of that order,
  but only within one put bound, which is shorter than one cron period.
- **An object** carries the schema, the write time (`utc`, `utc_ms`), the
  event id, the network, the source (`publisher` or `relay`; `drill` for a
  drill), the sender's rule set, the rule, the subject, the state
  (`FIRING`, `REMINDER` or `RESOLVED`; `DRILL`), whether it is an unconfirmed
  resolution, the subject line an email would have had, the body (the
  event's detail), when the condition was first seen and the age in
  seconds. It never carries a spend figure: a spend alert's detail is cut
  where its figures would start, whatever `SPEND_FIGURES_IN_EMAIL` says.
  The body is one line, and a word holding an address (`@`) or a URL
  (`://`) is dropped, the filter the relay's log lines use, so free text a
  sender passes through (a signing fault's reason) never brings one in.
  Its size is bounded by one post's events bound plus its fixed fields
  (`feed::OBJECT_BYTES`; a test builds the largest).
- **Once per event id, never lost to a timeout.** A write is claimed and
  settled in the sent-event ledger under `feed:<event id>` the way an email
  is, so a retried post writes nothing new. Only an acknowledged put counts
  as written: a put that timed out may or may not have landed, so it is
  written again after each claim period until a put is acknowledged, and
  the post is not acknowledged to its sender before that. An event can
  therefore be written more than once, under keys with the same event id;
  a reader keeps the first.
- **`feed/v1/latest.json`** (`slop_ninja.alert_feed.latest/v1`) is rewritten
  at the end of every tick: the time and the tick count (the relay's
  heartbeat), its cron period, the pull period, its read lease and its
  write gap (the dead-man's inputs), the retention and where it starts, the
  object bound, the horizon (`horizon_s`: the retention plus the ledger's
  row retention, past which no object, firing-set row or ledger row names an
  older event; a reader archives by it), email on or off, the tick's
  counts, whether its deliveries work (`delivery_ok`), the last write's
  result, the last status check after a token change (`status_check`:
  `utc`, `utc_ms`, `coordinator`, `eval`, `settled`; absent before the
  first) and the firing set (the
  conditions whose last object was a firing or a reminder). The firing set
  is listed within one post's events bound and counted in full
  (`firing_count`, `firing_omitted`). Conditions that fired before the feed
  existed enter it at their next reminder. A resolution settled without an
  object (its firing never reached the feed, for example one fired under an
  earlier rule set) removes its row, and a row not refreshed for the
  retention plus one renotify interval is dropped (an open condition is
  restated every renotify interval).
- **The write gap** (`write_gap_s`, 100 s): the longest between two of the
  relay's writes while it runs. A tick writes its objects as it goes and
  `latest.json` at its end, and the next tick starts at the first cron
  period after this one releases its lease. From one write's key stamp:
  that put's answer bound (5 s), one cron period (60 s), the next tick's
  read lease (30 s, three status-read timeouts), and the sweep's answer
  bound (5 s), the one step that writes nothing visible before
  `latest.json`. Inside a tick, consecutive stamps are at most two answer
  bounds apart (an object's put and an email), whatever the number of
  batches. A cron start later than one cron period (one was seen 9 minutes
  after a deploy) is outside it.
- **Retention.** Each tick deletes one batch (R2's limit of 1,000 keys per
  delete) of objects older than two of the spend tracker's schedule periods
  plus one pull period (48 h 1 min today). The period is the larger of the
  one the tracker's last upload states and the configured
  `TRACKER_PERIOD_SECS`, so an upload stating a shorter schedule shortens
  the tracker's staleness rule, never the retention. The bound: every object
  stays at least as long as the relay waits before it reports the tracker
  stale (two tracker periods), plus one pull period (one relay cron
  period). The relay deletes only keys it recorded writing under
  `feed/v1/events/`, never `latest.json`.

### Reading the feed

The operator reads the feed with private tooling outside Cloudflare, once per
relay cron period, keeping the first object of each event id. That tooling is
not part of this repository. It relies only on what this section states: one
object per event id, `latest.json` as the relay's heartbeat with its stated
write gap, and the retention horizon. It raises `relay_silent` itself when
the relay's writes stop and reports a gap past the retention.

## Email (optional, off by default)

Email is on only while the var `ALERT_EMAIL_ENABLED` is `"true"` and the
secret `ALERT_EMAIL_TO` is set (with `ALERT_EMAIL_FROM`). Without the
recipient, email is off: no warning, nothing is sent, and the feed carries
everything. A recipient deployed without the enabling var (by accident)
sends nothing and is a `relay_config_warning`; `latest.json` states email
on or off. A set but invalid address is a `relay_config_warning`, and
email stays off. The email path keeps its own
ledger rows (the bare event ids) and its own outcome, so turning it on or
off never changes the feed.

Cloudflare's documentation (Email Service pages dated 2026-08-25 to
2026-10-30) says:

- A `send_email` binding without a restriction attribute sends to any
  verified destination address of the account.
- Before a sending domain is onboarded to Email Sending, a Worker can send
  only to verified destination addresses, and only from the account's
  routing domains. Those sends are free and outside the sending quota.
- Onboarding a sending domain adds DNS records. No lane makes that change,
  so the recipient must be a verified destination address. An address that
  is only an Email Routing rule may be refused (`E_RECIPIENT_NOT_ALLOWED`;
  workers-rs names the same refusal `RCPT_NOT_ALLOWED`).
- Sends from a Worker show as "dropped" in the Email Routing summary even
  when they were delivered. To track outbound success, the Limits page
  points to the Email Sending metrics and logs (`/email-service/
  observability/`). A send that Cloudflare accepted is therefore not proof
  of delivery: the deploy drill reads the sends' outcomes and asks the user
  to confirm receipt.

The deploy drill (2026-11-02) found:

- A routing-rule address is refused as a recipient: `E_RECIPIENT_NOT_ALLOWED`,
  "destination address is not a verified address". `ALERT_EMAIL_TO` is
  therefore a verified destination address of the account.
- These sends appear in the zone's Email Routing activity (GraphQL
  `emailRoutingAdaptive`), each with status `delivered`. The Email Sending
  datasets (`emailSendingAdaptive` and its groups) recorded none of them.
- The logged `Message-ID` of every send is Cloudflare's own, not the
  relay's: Cloudflare replaces it on the raw path. A message the relay
  sends again therefore does not keep its `Message-ID`.
- SPF, DKIM and DMARC show as `none` in that log. The receiving mailbox may
  file the messages as spam.
- For the structured API, `Date` and `Message-ID` are platform-generated;
  for MIME sent over SMTP, "the same allowlist applies". The raw
  `EmailMessage` path is not described either way.

The relay uses the raw `EmailMessage` path (workers-rs 0.8.7,
`cloudflare:email`), so it controls the headers ([`src/mime.rs`](src/mime.rs)):

- the headers are `From`, `To`, `Subject`, `Date`, `Message-ID`,
  `MIME-Version`, `Content-Type: text/plain; charset=utf-8`,
  `Content-Transfer-Encoding: quoted-printable` and
  `Auto-Submitted: auto-generated`;
- header values are ASCII, and the subject stays within 78 characters;
- the relay sets a deterministic `Message-ID` from the event ids, but
  Cloudflare replaces it (see above);
- subjects carry only the kind, rule and subject, or counts;
- details are in the body, which is quoted-printable, so no line exceeds 76
  characters.

If a drill shows the raw path refused, the structured path is in workers-rs
too (`send_with_builder`), with the same subject and text.

A failed send is logged (live log only) with the error's code (else its
name) and its message, with every word that holds an address or a URL
removed.

Bodies are content-free: rule names, subjects, ages, epochs and fault kinds.
They hold no key, token, URL, account id or threshold. Month totals appear in
a spend alert's email body only when `SPEND_FIGURES_IN_EMAIL = "true"` while
email is on, which the operator sets only once the recipient is known to
deliver to the user's mailbox alone. The feed never carries them.

The Worker never changes Email Routing:

- its configuration has no `addresses` field, which would rewrite the zone's
  routing rules;
- it has no email handler;
- it creates no destination address.

## Sent-event ledger

Before delivering, the relay claims every event id of the batch in its
Durable Object's SQLite table, once per sink: `feed:<event id>` for the
feed's object, the bare event id for email (its rows predate the feed). The
rules below hold for each:

- A claimed id is delivered by its claimant only. A delivered id is
  acknowledged without delivering again.
- A claim lasts one cron period, since a retry arrives no sooner than the
  sender's next run. A claim whose holder never finished is then taken over.
- A refused write or send releases its claims at once.
- A write or send that timed out may have gone out. Its ids are held for
  one more claim period, then retried.
  - Email: the retry's outcome is final: sent, a second timeout (recorded as
    sent), or refused (free again, and a later timeout is recorded as sent).
    An event is emailed at most twice; one whose two sends both timed out
    without landing is lost.
  - Feed: a timeout is never final. The id stays uncertain and is written
    again after each claim period until a put is acknowledged; the sender
    is not acknowledged before that. Duplicate objects are harmless (a
    reader keeps the first copy of an event id); a lost one would not be.
- A sender keeps posting an unacknowledged event with the same id, so a
  claim that finds an id sent refreshes its row.
- Every tick deletes rows untouched for the retention: the publisher's
  renotify interval, and never less than the time after which the relay
  counts the publisher as down (one lease plus three cron periods, 5
  minutes today). That covers the publisher's slowest retry while it runs:
  a run killed while waiting holds its lease for two cron periods, and the
  next run starts within one more. Unsettled rows (claims, unknown
  outcomes) are kept the same way, so a timed-out email is retried once,
  not once per period. A retry that arrives after a longer outage finds no
  row and is delivered again.
- The ledger records how far its sweeps reached. An unconfirmed
  resolution whose firing has no row in a sink is settled there without
  delivering only while the ledger still covers the firing's whole
  episode. Past that (an outage longer than the retention), the resolution
  is delivered, with a note that its firing may not have gone out.

Relay ticks run one at a time behind a tick lease:

- A tick takes the lease for its reads (two status reads, while the
  publisher is down or for the status check after a token change, and the
  tracker object, each within its bound).
- It renews the lease before each batch, the drill, the feed's retention
  sweep and `latest.json` by that step's bound, so the lease covers the
  step under way whatever the number of batches.
- Every write checks that the lease is still the tick's own, and a tick
  releases only its own lease. A tick that overran its lease therefore
  stops and never overwrites a newer tick's state.
- The state is saved after the conditions are observed, before each batch
  and after each delivered batch. These saves happen in the same
  synchronous step as the ledger's records.
- The relay's own events go through the same ledger, so an overlapping tick
  delivers nothing twice.

## Bounded memory (`slop_ninja.alerts_relay.arena/v4`)

The relay applies the publisher's rule:

- **Fixed linear memory**: `arena::LINEAR_MEMORY_BYTES` in whole pages
  (set in `.cargo/config.toml`), initial equal to maximum (a test checks
  they agree).
- **Bounds fixed before reading, derived from the fields.** Every input's
  bound is set before its body is read. A declared length over the bound
  is refused first, and a streamed body as soon as it passes the bound.
  Each bound is the largest valid body, which a test builds and measures:
  - A post: the publisher's splitter bound for its events (40,000
    serialized bytes, Slack's message limit, which the publisher applies to
    every sink; `wire::POST_EVENTS_BYTES`), plus its envelope: the
    schema, and the rule set and network names at the protocol's string
    bound (`MAX_STRING_LEN`, 4,096; a test checks the wire constant
    against it). Names are lowercase ASCII identifiers, so they never
    need escaping; a post or heartbeat with another name is refused
    (`name`).
  - A heartbeat: its fields at their widest (three names at the string
    bound, five `u64`s).
  - The tracker upload: its fields at their widest (every free string is
    checked against the tracker's own values: the auth and alert states,
    the times), plus one alert month and one acknowledgment per month since
    the tracker's first report month. Read within the status-read time
    bound.
  - A coordinator `/status` body: the coordinator's 1 MiB status route
    scope (`WORKER_CONTROL_BYTES`; a test checks they agree).
  - A message's text: the post's fields plus the fixed text per event and
    per message, both summed from the text's own pieces (a test checks
    they are exact).
  - Widening any of these is a contract change: a new wire schema, and a
    new arena rule version.
- **One isolate budget.** The budget accounts each held body, and for each
  message its text and the encoded message (encoded straight into a buffer
  sized once). A reservation past the budget answers 503 and is retried.
  The budget is the working set the relay serves at once: one tick and two
  overlapping posts.
  - v3 adds the feed's pieces, each serialized into one buffer of its
    bound and held one at a time in the slot of one message, which is
    larger than each (tests check it): a feed object (`feed::OBJECT_BYTES`,
    one post's events bound plus its fixed fields), `latest.json`
    (`feed::LATEST_BYTES`, its fixed fields plus the firing set within one
    post's events bound) and one delete batch of keys (1,000 keys of
    `feed::KEY_BYTES`). The budget's figure is unchanged.
  - v4 reserves that slot (`feed::STEP_BYTES`) for each feed step before
    the step allocates, and builds what it writes only inside it: an
    object's write (the object, its key and its body), `latest.json` (the
    firing rows as read, their entries and the body) and a delete batch
    (the keys and the names handed to the bucket); tests check each fits.
    The logged peak therefore covers the feed. A refused step writes or
    deletes nothing and is retried.
- **Measured natively; one live reading.** In the deploy drill the tick log's
  budget peak was 2,097,152 bytes (two status reads while the publisher was
  silent) of the 3,580,119-byte budget. A native counting-allocator test
  (`tests/arena.rs`) checks the accounting against the real heap for the
  largest posts and a tick with two 1 MiB status bodies. The working set
  outside the budget (the decoded post, the claimed copies, the decoded
  status view) stays within a quarter of the reserve
  (`arena::UNACCOUNTED_ALLOWANCE`). The 16 MiB reserve itself is a
  versioned parameter, the publisher's figure for the same runtime glue and
  stack, which no Workers measurement has confirmed. Each tick's log line
  carries the budget's peak, which the deploy drill records.

## Configuration

See `wrangler.toml`.

Only `NETWORK` is required: if it is missing or wrong, the relay refuses
every request (503) and delivers nothing. Every other setting is optional:
one that does not parse is ignored, its default is in force, and the relay
reports `relay_config_warning` naming it. `ALERT_EMAIL_TO` absent is email
off, not a warning; `ALERT_EMAIL_TO` set without `ALERT_EMAIL_ENABLED =
"true"` is email off with a warning.

| Name | Kind | Meaning |
| --- | --- | --- |
| `NETWORK` | var, required | must be `testnet` |
| `ALERT_EMAIL_FROM`, `ALERT_EMAIL_TO` | secrets, optional | one plain mailbox each, checked at every request; email is on only while `ALERT_EMAIL_TO` is set and `ALERT_EMAIL_ENABLED` is `true` (off by default) |
| `ALERT_EMAIL_ENABLED` | deploy-time var | `true` turns email on while `ALERT_EMAIL_TO` is set (default off) |
| `WATCHDOG_SILENCE_SECS` | var | fallback for the coordinator's watchdog silence until the first heartbeat carries the publisher's bound (default 300) |
| `TRACKER_PERIOD_SECS` | var | fallback for the tracker's schedule period until an upload states it (default 86,400) |
| `SPEND_FIGURES_IN_EMAIL` | deploy-time var | `true` puts month totals in spend alert email bodies while email is on (default off; never in the feed) |
| `HEARTBEAT_ENABLED` | deploy-time var | `true` pings `HEARTBEAT_URL` after each tick whose delivery path works (default off) |
| `DRILL_ID` | deploy-time var | a name (lowercase letters, digits and `._/-`): the next tick writes one drill object for it to the feed, and emails one drill message while email is on (see [Drills](#drills)); unset by default |
| `COORDINATOR_STATUS_TOKEN`, `EVAL_STATUS_TOKEN` | secrets | the relay's own status token for each Worker (that Worker's `STATUS_TOKEN_ALERTS`, or the shared pre-split token until the split; coordinator README, "The status gate"), which reads only `/status`'s liveness fields and only through the binding; without one, that Worker is reported as not read |
| `HEARTBEAT_URL` | secret | an external dead-man check's https URL (no credentials); when enabled without a valid one, the ping is off and `relay_config_warning` says so |
| `ALERT_EMAIL_TESTNET` | `send_email` binding | unrestricted, as described above; used only while email is on |
| `ALERTS_TESTNET` | R2 binding | the private `alerts-bucket-testnet` bucket: the tracker's upload and the alert feed |
| `COORDINATOR_TESTNET`, `EVAL_WORKER_TESTNET` | service bindings | status reads |
| `ALERTS_RELAY_STATE_TESTNET` | Durable Object | `AlertsRelayState` (SQLite) |

## Drills

A deploy with `DRILL_ID` set makes the next tick write one feed object for
it (state `DRILL`, source `drill`, event id derived from the drill id), and,
while email is on, email one message with the subject `SlopNinja testnet
alert drill`. Both say that it is a drill and needs no action. It tests the
delivery path only: it raises no rule and has no ledger row.

- Each sink records the id in the relay's state once its write or send
  went out, or once it timed out (it may have landed), and never repeats
  that id. The next deploy may leave the setting set or drop it. The deployed
  email build recorded only its email, so the first feed build writes one
  feed drill object for a drill id that is still set.
- A refused drill counts as a failed delivery: the tick logs its code and
  message (`relay: drill feed refused`, `relay: drill refused`) and the
  next tick retries it.
- A new id drills again.

The rule drills use real conditions instead, for example the first
`publisher_silent` before the publisher's heartbeats start, or the
publisher's `config_warning`.

## Limits

- **A Cloudflare-wide cron outage** stops the publisher's cron and the
  relay's cron together, so nothing is delivered until crons run again.
  A reader's dead-man outside Cloudflare (`relay_silent`) sees `latest.json`
  stop. An R2 outage stops the feed (and an Email Service outage the
  email). Posts and ticks are retried, and nothing is marked delivered
  until it is, except an email whose two sends both timed out.
- **A missing or wrong network var** stops all delivery: the relay refuses
  every post, heartbeat and tick. The publisher only logs its undelivered
  events, and `latest.json` stops, so a reader's dead-man outside Cloudflare
  fires.
- **A deleted object is seen late or not at all.** A feed write is never
  lost to a timeout, but an object can still be deleted unread (an absence
  longer than the retention). `latest.json` states where the retained
  objects start (`retained_from_ms`), and its firing set still lists a
  condition whose object was deleted; a resolution deleted unread shows
  only as the firing still listed until the firing-set row expires.
- **The external ping proves the cron and the recent deliveries, not
  receipt.** A ping after an idle tick (nothing due) proves only that the
  cron runs. The relay's own deliveries decide the ping; publisher posts
  that fail show through `publisher_alerts_undelivered` once it is due.
- **An outage of the operator's private spend tracker** stops the
  tracker's uploads. The relay then reports the
  tracker stale after two schedule periods, so it does not report a spend
  threshold crossed during that time.
- **An expired Cloudflare login of the spend tracker** usually shows as
  `spend_tracker_stale`, after up to two schedule periods, not as
  `spend_tracker_blind`. The tracker uploads with the same login it prices
  with, so a blind run cannot upload either. `spend_tracker_blind` fires
  only when a blind run's upload still succeeds.
- **Nothing in the account watches the relay.** If its cron or its Durable
  Object stops, the publisher's posts go unanswered and `latest.json`
  stops: a reader's dead-man outside Cloudflare reports it. The external
  ping (`HEARTBEAT_URL`) is a second, optional check.
- **Event ids follow the publisher's saved state.** The publisher saves its
  observed state, with the marks of the posts about to go out, before it
  posts anything, and posts nothing when that save fails. A run that dies
  after posting, or whose later save fails, therefore leaves the next run
  the same ids, which the relay delivers once. A publisher state that is lost
  or reset, or a rule-set change, starts new ids: open conditions fire
  again once.
- **Outages and the ledger.** A retry after an outage longer than the
  ledger's retention (the renotify interval, 6 hours by default, and never
  less than 5 minutes) is delivered again. An unconfirmed resolution after
  such an outage is still delivered, with the note that its firing may not
  have gone out. Resolutions of firings that went out are delivered after
  an outage of any length.
- **A rule-set change between a firing and its resolution.** An
  unconfirmed resolution is looked up under the post's rule set. A firing
  sent under an earlier rule set is not found, so its resolution is not
  delivered. Rule sets change only with a deploy (the publisher's v4, with
  the unknown-read rule, is one).
- **The tracker sees spend about a day late** (it runs daily). A spend alert
  is therefore no faster than the tracker; see the tracker's own notes on
  detection lag.

## Checks

On the project's build host, as memory-limited jobs under the shared cargo
lock:

```sh
cargo test --manifest-path subnet/alerts-relay/Cargo.toml --workspace
cargo clippy --manifest-path subnet/alerts-relay/Cargo.toml --workspace --all-targets -- -D warnings
cargo clippy --manifest-path subnet/alerts-relay/Cargo.toml --target wasm32-unknown-unknown -- -D warnings
```

The publisher's sink tests run with the publisher's own suite, and the
`no_runtime_gates` lint scans this crate.
## Not yet done

- The feed build's deploy (from main, after review), then the publisher's
  rule set v4: before v4 one missed status read still flaps a status rule
  (a new event id each time).
- Live checks of the feed: the first tick's `latest.json`, the drill object
  of the drill id still set, a publisher post and its retry (one object),
  that the account API honours `start_after` (two or more objects) and
  serves an object on its key with the slashes kept and the retention sweep
  (needs 48 h).
- Live checks of the email build (email is off; they apply if it is turned
  on again):
  - drills of the remaining relay rules (`coordinator_unwatched`,
    `publisher_alerts_undelivered`, `spend_alert`, the stale, blind and
    incomplete tracker rules, `relay_config_warning`);
  - a heartbeat that stops after heartbeats had arrived (the drill covered
    a publisher that never sent one);
  - a retried post that sends one email (native tests only);
  - a stale and a blind tracker upload;
  - the user's confirmation that the drill emails arrived, and where, and
    the delivered `Message-ID` and `Date`.
