# SlopNinja account Worker

Sign-in, sessions and accounts for the detection pages: email links, Google, GitHub, X and a Bittensor wallet; linking,
merging and removing sign-in methods; saved links ("my detections"); export and deletion; and the signed account ops.
The website reaches it through its `ACCOUNT` service binding and the detect Worker calls its saved-link routes. It has
no route, custom domain, default `workers_dev` subdomain or preview URL, and it binds no other Worker: work it starts on
detections (an account's deletion, a merge) goes on the `detect-jobs` queue as content-free messages carrying user ids.
The billing ledger and its routes come with paid checks.

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
| `quota.rs` | the `QuotaLimiter` Durable Object's token buckets (sign-in mail, connect mail per address and per account) |
| `ops.rs` | signed account ops, `x_signin_purge` included |
| `sweep.rs` | the crons (expired flows every 5 minutes, sessions hourly) |
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
- **Content-free records.** No metric point, journal row or queue message carries an address, handle, token, slug or
  provider subject; an op that names an address journals only a keyed hash of it.

## Configuration

Origins come from `SITE_ORIGIN` and `SECRET_ORIGIN`; `wrangler.toml` carries placeholders, which the deploy wrappers
replace. Secrets: `ACCOUNT_STORE_SECRET` (derives the saved-link, address, quota, journal and session-reference keys)
and the three OAuth client secrets. A provider whose client id or secret is unset is absent from the sign-in page;
`X_CLIENT_ID` unset keeps X sign-in off entirely. `DETECT_OP_KEY` empty turns the op route off.

## Tests

`cargo test` runs every flow natively against the account store's own migration in SQLite, the dev auth stub's
provider routes (`subnet/devtools/auth-stub`), a capturing mailer, signed ops and wallet signatures; `tests/source.rs`
holds the source rules. Build the Worker with `worker-build --release` (worker-build 0.8.x); the `dev` environment of
`wrangler.toml` builds with `--features dev` and joins the local dev session (`subnet/devtools/README.md`).
