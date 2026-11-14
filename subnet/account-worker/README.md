# SlopNinja account Worker

Sign-in, sessions and accounts for the detection pages: email links, Google, GitHub, X and a Bittensor wallet; linking,
merging and removing sign-in methods; saved links ("my detections"); export and deletion; and the signed account ops.
The website reaches it through its `ACCOUNT` service binding and the detect Worker calls its saved-link routes. It has
no route, custom domain, default `workers_dev` subdomain or preview URL, and it binds no other Worker: work it starts on
detections (an account's deletion, a merge) goes on the `detect-jobs` queue as content-free messages carrying user ids.
It also keeps the billing ledger for paid checks: holds and their one end (commit or release), operator grants and
plan periods, served to the detect Worker through the internal billing routes.

## Layout

| Module | What it does |
|---|---|
| `routes.rs` | the one entry: internal routes (refused when they carry `X-SN-Route` or `X-SN-Host`), forwarded routes (the route table of `slop-ninja-detect-page`, the CSRF rule of `auth-core`, content types) |
| `flows.rs` | OAuth start and callback for Google, GitHub and X (the code exchanged in the callback itself: X's code lives 30 seconds), and X's avatar |
| `email.rs`, `wallet.rs` | email links (start, peek, confirm) and wallet sign-in (extension and command-line modes) |
| `signin.rs` | what a proven identity does under its flow's intent: sign in, create, hold a pending link or merge, step up |
| `sessions.rs` | sessions: issue, check, daily renewal, rotation, revocation |
| `link.rs` | the connect, merge and unlink actions |
| `owned.rs`, `views.rs`, `account.rs`, `delete.rs` | saved links, the account pages' data, sign out, sessions, export and deletion |
| `ledger.rs` | the billing ledger (`billing-policy/v1`): `/internal/v1/billing/{peek,hold,commit,release}`, the draw order (the plan period first, then payments first paid, first used), holds checked by the store's own constraints, a hold's one end, and the grant ops' statements |
| `quota.rs` | the `QuotaLimiter` Durable Object's token buckets (sign-in mail, connect mail per address and per account, paid quotes per account) |
| `ops.rs` | signed account ops, `x_signin_purge`, `grant_credit`, `grant_plan` and `void_grant` included |
| `sweep.rs` | the crons (expired flows every 5 minutes; sessions and ended plan periods hourly) |
| `host.rs`, `cf.rs` | the platform seam and its Cloudflare implementation: D1, the quota objects, mail, outbound fetch, the queue, Analytics Engine |
| `memory.rs` | the arena routes and footprints; `memory-model.json` is the build's memory model |

## Rules it keeps

- **No lockout by identifier.** Sign-in links are spaced and replaced per address and browser, so nobody else's request
  can void or delay a holder's link; connect mail draws on its own per-address and per-account budgets, never on the
  sign-in budget. Wallets have no per-key quota.
- **No silent linking.** A provider address already in use waits for a mail to that address, confirmed in the same
  browser; X never links by email. Every link and merge waits in a server-side pending row under this browser's flow
  binding and the session that ran it, for an explicit POST that names nothing but its action.
- **Nothing connects to or merges out of an account that is not active**, and such an account gets no session.
- **No hold without its deductions, and no overdraw.** A hold reads its sources, plans its parts, then runs one batch of
  unguarded decrements and the hold row: a concurrent hold, a void or a period end since the read breaks a balance
  CHECK and rolls the whole batch back, and the hold reads and plans once more before it answers 402. A hold is
  idempotent by its quote's nonce, and it ends once: commit and release each begin with the `hold_ends` row, whose
  key refuses a second end. A release returns nothing into a voided grant or a closed period.
- **The `BILLING` switch** (`off`, `grants`, `live`) gates new quotes and holds only; holds placed before a flip still
  end. `grants` runs paid checks end to end on operator grants (non-cash, never counted for the pot); `live` waits for
  the payment processor and answers `not_configured`.
- **Content-free records.** No metric point, journal row or queue message carries an address, handle, token, slug or
  provider subject; an op that names an address journals only a keyed hash of it.

## Configuration

Origins come from `SITE_ORIGIN` and `SECRET_ORIGIN`; `wrangler.toml` carries placeholders, which the deploy wrappers
replace. Secrets: `ACCOUNT_STORE_SECRET` (derives the saved-link, address, quota, journal and session-reference keys)
and the three OAuth client secrets. A provider whose client id or secret is unset is absent from the sign-in page;
`X_CLIENT_ID` unset keeps X sign-in off entirely. `DETECT_OP_KEY` empty turns the op route off. `BILLING_POLICY` is
`billing-policy/v1` (or its JSON, which may resize the billing quota buckets); the minimums it bills by (128 tokens on
credit, 512 from a plan, a plan's API part of 40,960) are the version's own, shared with the detect Worker's quote.
`BILLING` is `off` in production and `grants` on staging and in the dev session.

## Tests

`cargo test` runs every flow natively against the account store's own migration in SQLite, the dev auth stub's
provider routes (`subnet/devtools/auth-stub`), a capturing mailer, signed ops and wallet signatures; `tests/source.rs`
holds the source rules. Build the Worker with `worker-build --release` (worker-build 0.8.x); the `dev` environment of
`wrangler.toml` builds with `--features dev` and joins the local dev session (`subnet/devtools/README.md`).
