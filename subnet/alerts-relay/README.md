# Alerts relay (testnet)

A private Cloudflare Worker that turns Slop Ninja testnet alerts into email
(decision D18). It has no route, no `workers.dev` name and no preview URLs:
the publisher's service binding is the only way in, and its own one-minute
cron does the rest. Addresses are deploy-time secrets read from the
operator's private files; no tracked file holds one.

**State:** built and tested natively, with `wasm32` checks and a local
`worker-build --release` bundle. Not deployed.

## Inputs

1. **The publisher's alert posts.** The publisher posts its events (firing,
   reminder, resolved; schema `slop_ninja.publisher.alert/v3`) to
   `POST /alerts` through its `ALERTS_RELAY_TESTNET` service binding. Each
   event carries an id derived from the publisher's alert state and the
   fields it is derived from; the relay recomputes every id and refuses a
   post whose ids do not match. It emails each id once (see
   [Sent-event ledger](#sent-event-ledger)).
   - It answers 200 only when every event of the post is settled (sent now
     or before), so the publisher acknowledges exactly then; otherwise 503,
     and the publisher's next run retries.
   - It answers within half of the publisher's request bound
     (`wire::RELAY_ANSWER_MS`, from the moment its Durable Object starts the
     request; the send gets what is left of it). The other half covers the
     hops the relay cannot time, so a send the relay records as sent is
     normally answered before the publisher stops waiting.
   - When the answer is lost anyway, the publisher keeps the firing marked
     as attempted. If the condition clears before a retry is acknowledged,
     the publisher posts an unconfirmed resolution. The relay emails it only
     when its ledger shows the firing may have gone out, and settles it
     without an email otherwise. A firing email is therefore never left
     without its resolution.
2. **A dead-man check on the publisher.** After every run whose alert phase
   finished and saved its state, the publisher posts a heartbeat
   (`slop_ninja.publisher.heartbeat/v3`) to `POST /heartbeat`.
   - The relay's cron emails when the heartbeats stop (`publisher_silent`).
     It then also reads the coordinator Workers' `/status` itself with their
     read-only tokens (`coordinator_unwatched`), with the watermark bound the
     publisher's last heartbeat carried. It reads `/status` only, not
     `/eval/status`, so it stands in for part of the publisher's rules only
     (see [On call](#on-call)).
   - A crashed publisher, a stopped publisher cron, a state object that does
     not answer and a broken binding all stop the heartbeat. The relay is a
     separate Worker, so none of those stops it.
   - Each heartbeat carries the post schema the publisher's build sends and
     how many due events its run left undelivered, since which run. The
     relay also records every post and heartbeat it refuses for its content
     (size, schema, network, ids), until the next one it accepts. So alert
     posts that keep failing while heartbeats arrive are emailed too
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
   - The cron emails when a month's alert becomes active and is not yet
     acknowledged, reminds while it lasts, and resolves when it is
     acknowledged (with the acknowledgment time) or no longer active.
   - It also emails when the tracker's output is older than two of its
     schedule periods, when the bucket has not answered for that long, when
     the tracker could not authenticate to Cloudflare, and when its runs
     have not priced everything for two schedule periods (its totals are
     then lower bounds).
   - The threshold itself never leaves the tracker host.

The cron is outside the coordinator and the publisher. It reads and emails
only and advances no protocol state. It can also ping an external dead-man
URL (`HEARTBEAT_URL`, enabled only by `HEARTBEAT_ENABLED = "true"`), so a
stopped relay is caught outside Cloudflare. The ping means "the cron ran
and the email path works as far as the relay knows": it is skipped after a
busy or stopped tick, a store error or a refused configuration, and while
the relay's sends have been failing for two cron periods (see
[Limits](#limits)).

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
| `relay_config_warning` | `relay` | an optional relay setting did not parse, so its default is in force (the email names the settings, never their values) |

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
  - Today: silent after 3 minutes, emailed after 5 (plus at most one relay
    cron period).
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

Every email names its rule and subject. What to do:

| Rule | First steps |
| --- | --- |
| `publisher_silent` | Check the publisher's deployments and live log (`wrangler deployments list`, `wrangler tail`, testnet env). A refused heartbeat in the detail means the two Workers run different wire builds: deploy both from one commit. Until it is back, the relay's `coordinator_unwatched` emails stand in for the publisher's `durable_fault`, `watermark_stalled` and `status_unreachable` rules only. The cohort, scorer and budget, commit, reveal, ledger and publication rules (`/eval/status` and the publisher's own work) are unwatched until the publisher is back: check `/eval/status` by hand meanwhile. |
| `publisher_alerts_undelivered` | The publisher's own alerts are not reaching email. Read the detail: a schema or `event_id` refusal means a build skew (deploy both from one commit); `too_large` means one event the publisher could not fit; undelivered events with no refusal mean the relay's sends fail (its live log shows the code and message). |
| `coordinator_unwatched` | Treat it as the publisher's rule of the same fault: a stalled watermark (`watermark_stalled`), a journal or signing fault (`durable_fault`), an unanswered status (`status_unreachable`). Follow the coordinator runbook (`subnet/coordinator/TESTNET.md`). It covers only these three rules; see `publisher_silent`. |
| `spend_alert` | Investigate the month on the tracker host (`reports/<month>.md`), ask the user for guidance, then `track.sh --ack <month>` and `upload-latest.sh`. Nothing pauses on spend. |
| `spend_tracker_stale` | On the tracker host: the timer (`systemctl --user list-timers`; a reboot removes the transient timer), the last run's exit, and `wrangler whoami` (an expired login stops the uploads too). If the detail says the bucket has not answered, check the bucket binding and R2. |
| `spend_tracker_blind` | The tracker ran but could not authenticate: `wrangler whoami` on the tracker host. Logging in again is the user's action. |
| `spend_tracker_incomplete` | The tracker's runs keep failing to price something (rate limits, scope or API changes, an unknown product), so a crossed threshold may go unreported. Read the last runs' logs and the report's "Run failures" on the tracker host. The detail gives the exit code, the failure count and the alert state only. |
| `relay_config_warning` | An optional relay setting does not parse and its default is in force (figures off, the external ping off, or a default period). Fix the deploy-time var or secret named in the detail and redeploy the relay. |
| External monitor pages (when enabled) | The relay's cron stopped, its tick fails, its configuration is refused, or its sends have failed for two cron periods. Read the relay's live log (`wrangler tail --env testnet` in `subnet/alerts-relay`): the tick line shows `email_ok`, and a failed send logs its code and message. |

The publisher's own rules (their table is in `subnet/publisher/src/alerts.rs`
and its README) map to the coordinator runbook the same way. Their
resolution emails say what cleared them.

## Email

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
  of delivery: the deploy drill reads the sends' outcomes there and asks
  the user to confirm receipt.
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
- the relay sets a deterministic `Message-ID` from the event ids, so a
  message sent again after an unknown outcome carries the same id;
  Cloudflare may replace it (the drill compares the delivered headers);
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
a spend alert's body only when `SPEND_FIGURES_IN_EMAIL = "true"`, which the
operator sets only once the recipient is known to deliver to the user's
mailbox alone.

The Worker never changes Email Routing:

- its configuration has no `addresses` field, which would rewrite the zone's
  routing rules;
- it has no email handler;
- it creates no destination address.

## Sent-event ledger

Before sending, the relay claims every event id of the batch in its Durable
Object's SQLite table:

- A claimed id is sent by its claimant only. A sent id is acknowledged
  without sending again.
- A claim lasts one cron period, since a retry arrives no sooner than the
  sender's next run. A claim whose holder never finished is then taken over.
- A refused send releases its claims at once.
- A send that timed out may have gone out. Its ids are held for one more
  claim period, then retried once. The retry's outcome is final: sent, a
  second timeout (recorded as sent), or refused (free again, and a later
  timeout is recorded as sent). An event is emailed at most twice. An event
  whose two sends both timed out without delivering is lost; a
  still-firing condition's next reminder has a new id.
- A sender keeps posting an unacknowledged event with the same id, so a
  claim that finds an id sent refreshes its row.
- Every tick deletes rows untouched for the retention: the publisher's
  renotify interval, and never less than the time after which the relay
  counts the publisher as down (one lease plus three cron periods, 5
  minutes today). That covers the publisher's slowest retry while it runs:
  a run killed while waiting holds its lease for two cron periods, and the
  next run starts within one more. Unsettled rows (claims, unknown
  outcomes) are kept the same way, so a timed-out send is retried once,
  not once per period. A retry that arrives after a longer outage finds no
  row and is emailed again.
- The ledger records how far its sweeps reached. An unconfirmed
  resolution whose firing has no row is settled without an email only
  while the ledger still covers the firing's whole episode. Past that (an
  outage longer than the retention), the resolution is emailed, with a
  note that its firing may not have gone out.

Relay ticks run one at a time behind a tick lease:

- A tick takes the lease for its reads (two status reads and the tracker
  object, each within its bound).
- It renews the lease before each batch by the batch's send bound, so the
  lease covers the step under way whatever the number of batches.
- Every write checks that the lease is still the tick's own, and a tick
  releases only its own lease. A tick that overran its lease therefore
  stops and never overwrites a newer tick's state.
- The state is saved after the conditions are observed, before each batch
  and after each delivered batch. These saves happen in the same
  synchronous step as the ledger's records.
- The relay's own events go through the same ledger, so an overlapping tick
  sends nothing twice.

## Bounded memory (`slop_ninja.alerts_relay.arena/v2`)

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
- **Measured natively, not yet live.** A native counting-allocator test
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

Only `NETWORK`, `ALERT_EMAIL_FROM` and `ALERT_EMAIL_TO` are required: if
one is missing or invalid, the relay refuses every request (503) and sends
nothing. Every other setting is optional: one that does not parse is
ignored, its default is in force, and the relay emails
`relay_config_warning` naming it.

| Name | Kind | Meaning |
| --- | --- | --- |
| `NETWORK` | var, required | must be `testnet` |
| `ALERT_EMAIL_FROM`, `ALERT_EMAIL_TO` | secrets, required | one plain mailbox each, checked at every request |
| `WATCHDOG_SILENCE_SECS` | var | fallback for the coordinator's watchdog silence until the first heartbeat carries the publisher's bound (default 300) |
| `TRACKER_PERIOD_SECS` | var | fallback for the tracker's schedule period until an upload states it (default 86,400) |
| `SPEND_FIGURES_IN_EMAIL` | deploy-time var | `true` puts month totals in spend alert bodies (default off) |
| `HEARTBEAT_ENABLED` | deploy-time var | `true` pings `HEARTBEAT_URL` after each tick whose email path works (default off) |
| `COORDINATOR_STATUS_TOKEN`, `EVAL_STATUS_TOKEN` | secrets | read-only status tokens; without one, that Worker is reported as not read |
| `HEARTBEAT_URL` | secret | an external dead-man check's https URL (no credentials); when enabled without a valid one, the ping is off and `relay_config_warning` says so |
| `ALERT_EMAIL_TESTNET` | `send_email` binding | unrestricted, as described above |
| `ALERTS_TESTNET` | R2 binding | the private `alerts-bucket-testnet` bucket |
| `COORDINATOR_TESTNET`, `EVAL_WORKER_TESTNET` | service bindings | status reads |
| `ALERTS_RELAY_STATE_TESTNET` | Durable Object | `AlertsRelayState` (SQLite) |

## Limits

- **A Cloudflare-wide cron outage** stops the publisher's cron and the
  relay's cron together, so nothing alerts until crons run again (an
  external dead-man check, when enabled, still pages). An Email Service
  outage stops delivery. Posts and ticks are retried, and nothing is marked
  sent until it is, except an event whose two sends both timed out.
- **A missing or invalid address secret or network var** stops all email:
  the relay refuses every post, heartbeat and tick. The publisher only logs
  its undelivered events. An enabled external check pages (no tick
  finishes, so no ping); without one, nothing says so. The deploy step
  checks the secrets, and drills cover it.
- **The external ping proves the cron and the recent sends, not delivery.**
  A ping after an idle tick (nothing due) proves only that the cron runs:
  a send path that broke while nothing was due shows only at the next due
  event. Sends that Cloudflare accepted but did not deliver look healthy.
  The relay's own sends decide the ping; publisher posts that fail at the
  send path show through `publisher_alerts_undelivered` once it is due,
  whose own send then fails too. Drills, the Email Sending metrics and
  logs, and the user's confirmations cover delivery.
- **A tracker host outage** (a power cut, or a reboot that drops the
  transient timer) stops the tracker's uploads. The relay then reports the
  tracker stale after two schedule periods, so it does not report a spend
  threshold crossed during that time.
- **An expired Cloudflare login on the tracker host** usually shows as
  `spend_tracker_stale`, after up to two schedule periods, not as
  `spend_tracker_blind`. The tracker uploads with the same login it prices
  with, so a blind run cannot upload either. `spend_tracker_blind` fires
  only when a blind run's upload still succeeds.
- **Nothing in the account watches the relay.** If its cron or its Durable
  Object stops, the publisher's posts go unanswered. The publisher logs the
  undelivered events, keeps them due and retries them every run, but no
  email says so. The external dead-man ping (`HEARTBEAT_URL`) closes this
  gap once the user picks a service; until then, drills cover it.
- **Event ids follow the publisher's saved state.** The publisher saves its
  observed state, with the marks of the posts about to go out, before it
  posts anything, and posts nothing when that save fails. A run that dies
  after posting, or whose later save fails, therefore leaves the next run
  the same ids, which the relay emails once. A publisher state that is lost
  or reset, or a rule-set change, starts new ids: open conditions fire
  again once.
- **Outages and the ledger.** A retry after an outage longer than the
  ledger's retention (the renotify interval, 6 hours by default, and never
  less than 5 minutes) is emailed again. An unconfirmed resolution after
  such an outage is still emailed, with the note that its firing may not
  have gone out. Resolutions of firings that went out are emailed after an
  outage of any length.
- **A rule-set change between a firing and its resolution.** An
  unconfirmed resolution is looked up under the post's rule set. A firing
  sent under an earlier rule set is not found, so its resolution is not
  emailed. Rule sets change only with a deploy.
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

- Deployment, with the relay deployed before the publisher:
  - the private bucket;
  - the secrets;
  - the first tracker upload;
  - a publisher deploy that turns the relay sink on, carrying the
    publisher's current deploy-time vars.
- Live checks:
  - a drill per publisher rule and per relay rule;
  - a stopped heartbeat;
  - a retried post that sends one email;
  - a stale and a blind tracker upload;
  - the user's confirmation that the drill emails arrived, and where.
- Confirm in a drill whether Cloudflare accepts the intended recipient (see
  [Email](#email)).
- Record the budget's live peak from the tick log in a drill.
- The user's choice of an external dead-man service for the relay's own
  ping, or acceptance of the relay as the dead-man without one.
