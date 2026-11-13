# Dev tools: stubs and the local dev session

Dev only. Nothing here is deployed, and nothing here calls a model, a sign-in provider, a mail service, a chain
endpoint or any third-party site. The stubs stand in for those in local `wrangler dev` sessions, so the detection
pages, accounts and the URL check can be exercised end to end on one machine with no Cloudflare login.

| Tool | Kind | Stands in for |
|---|---|---|
| [`stub-scorer`](stub-scorer/) | Worker | the scorer Container: its load protocol and `/v1/score`, answers keyed by markers in the text |
| [`auth-stub`](auth-stub/) | Worker | Google, GitHub and X sign-in, X avatars, and sign-in mail (captured at `/mail`), with failure switches |
| [`fixture-site`](fixture-site/) | Worker | websites: robots variants, `noindex`, redirect chains, deep nesting, large pages, charsets, walls |
| [`wallet-stub`](wallet-stub/) | native CLI | a Bittensor wallet: sr25519 signatures in raw, wrapped, wrong-key and wrong-message variants |
| [`session/`](session/) | scripts | the dev session ([`dev-session.sh`](session/dev-session.sh)) and a smoke check of the stubs |

## The dev session

Three `wrangler dev --local` sessions, each on its own host and port over local HTTPS, joined through one dev
registry so that service bindings cross them:

| Session | Origin | Primary Worker | Other Workers |
|---|---|---|---|
| site | `https://slop.localhost:8787` | the website | detect, account, detect-og, detect-fetch (each once it has `[env.dev]`) |
| secret | `https://s.slop.localhost:8788` | the secret-host Worker | none |
| stubs | `https://auth.slop.localhost:8789` | `auth-stub` | `stub-scorer`, `fixture-site` |

Only a session's primary Worker listens for HTTP, so each public host gets its own session, and the stubs get a third:
the browser must reach the sign-in stubs' authorize pages, and test suites read captured mail and flip switches.
`auth-stub` is the stubs session's door to its two neighbours: `/__scorer/<path>` reaches the stub scorer and
`/__fixture/<host>/<path>` the fixture site.

Local D1 (every migration applied), R2, Durable Objects and queues all run inside the site session. The origins come
from dev vars, so `__Host-` cookies and the Origin checks behave as in production: a cookie set on
`slop.localhost` never reaches `s.slop.localhost`.

```sh
cd subnet/devtools/session
./dev-session.sh cert       # once: a local certificate for *.slop.localhost (trust it in the test browser)
./dev-session.sh build      # the three stub Workers (worker-build 0.8.x; WORKER_BUILD names another binary)
./dev-session.sh migrate    # every D1 migration of the site session's Workers, into its local stores
./dev-session.sh stubs      # terminal 1
WEB_REPO=<website checkout> ./dev-session.sh site      # terminal 2
WEB_REPO=<website checkout> ./dev-session.sh secret    # terminal 3
./dev-session.sh smoke      # checks a running stubs session
```

State (the certificate, the dev registry and each session's local stores) lives in `subnet/devtools/.dev/`, or in
`DEV_STATE`. The script turns off Wrangler's telemetry, update check, `Request.cf` download and log files, and passes
`--local`, so a session needs no network and no login. The stubs use workers-rs 0.8.7, as the other Workers do; build
them with worker-build 0.8.x (`cargo install worker-build@^0.8`), since worker-build 0.9 refuses worker 0.8.

### Joining the session

A Worker joins the site or secret session by defining `[env.dev]` in its Wrangler config:

- **Name:** `slop-ninja-web-dev`, `slop-ninja-web-secret-dev`, `slop-ninja-detect-dev`, `slop-ninja-account-dev`,
  `slop-ninja-detect-og-dev` or `slop-ninja-detect-fetch-dev`. Service bindings keep their production binding names
  and point at these names.
- **Vars:** `SITE_ORIGIN = "https://slop.localhost:8787"`, `SECRET_ORIGIN = "https://s.slop.localhost:8788"`,
  `MAIL_SUBJECT_PREFIX = "[dev] "`, and `STUB_ORIGIN = "https://auth.slop.localhost:8789"` where the browser is sent
  to a sign-in stub.
- **Local resources only:** D1 bindings with a placeholder `database_id` and their `migrations_dir`, R2 buckets,
  queues and Durable Objects, all simulated locally. No route, custom domain, `workers_dev` or account.
- **Stubs, only here and only behind the cargo feature `dev`:**
  - detect: `STUB_SCORER` (service `slop-ninja-dev-stub-scorer`) in place of the scorer Container's port;
  - account: `AUTH_STUB` (service `slop-ninja-dev-auth-stub`) for the token, user and mail calls, with the browser sent
    to `STUB_ORIGIN` for authorize;
  - detect-fetch: `FIXTURE_SITE` (service `slop-ninja-dev-fixture-site`) for hosts under `fixtures.test`.

A source test (`http/tests/dev_configs.rs`) refuses any `slop-ninja-dev-` name outside `[env.dev]` in a Worker
config under `subnet/`.

**Provider endpoints in the account Worker's dev build** (stub origin `S`, server calls through `AUTH_STUB`):

| Provider | Authorize (browser) | Token | User |
|---|---|---|---|
| Google | `S/google/o/oauth2/v2/auth` | `S/google/token` | (ID token claims); key set `S/google/oauth2/v3/certs`, discovery `S/google/.well-known/openid-configuration` |
| GitHub | `S/github/login/oauth/authorize` | `S/github/login/oauth/access_token` | `S/github/api/user`, `S/github/api/user/emails` |
| X | `S/x/i/oauth2/authorize` | `S/x/2/oauth2/token` (revoke `S/x/2/oauth2/revoke`) | `S/x/2/users/me`; avatars: `https://pbs.twimg.com/<path>` is fetched as `S/x/pbs/<path>` |

The dev clients are `dev-<provider>-client-id` with secret `dev-<provider>-client-secret-not-real` and the redirect URI
`https://slop.localhost:8787/auth/<provider>/callback` (vars `<P>_CLIENT_ID`, `<P>_CLIENT_SECRET`,
`<P>_REDIRECT_URIS` change them).

**Mail capture.** The account Worker's dev mail implementation posts each message to `AUTH_STUB` `POST /mail` as JSON
`{to, subject, text, from?, html?, headers?}` and gets `201 {id}`. A switched failure answers `422 {error}` with an
Email Sending code (`E_RECIPIENT_SUPPRESSED`, `E_RATE_LIMIT_EXCEEDED`, `E_DAILY_LIMIT_EXCEEDED`, ...). Tests read
`GET /mail?to=<address>`, `GET /mail/latest?to=<address>` (each message carries its `links`) and clear with
`POST /mail/clear`.

## `auth-stub`

Test users (all synthetic, under `example.com` and `example.org`):

| User | Email | Avatar served |
|---|---|---|
| `alice` | verified | PNG |
| `bob` | verified | JPEG |
| `carol` | unverified | SVG labelled `image/png` |
| `dave` | none | HTML labelled `image/png` |
| `erin` | verified | PNG over 64 KiB |

The stub is stricter than the providers where SlopNinja's flows promise more: `state` and an S256 (or, except
GitHub, plain) PKCE challenge are required, the redirect URI must match exactly, and each code is single use. It
follows the providers where their behaviour matters: GitHub answers a failed exchange with 200 and an `error` field
and refuses an API call without `User-Agent`, X codes expire 30 seconds after consent and X returns only the fields
asked for (`confirmed_email` only with the `users.email` scope), and Google's ID token is RS256 under a key the stub
generates once and publishes in its key set. GitHub lists a verified non-primary address before the primary one.

Switches (`POST /__stub/switch` with `{"provider": "google" | "github" | "x", ...}`; `POST /__stub/reset`;
`GET /__stub/state` also counts requests per route):

| Field | Effect |
|---|---|
| `user` | the test user consented as (Google's `login_hint` naming a test user wins) |
| `chooser` | show a consent page listing the users and a Deny button |
| `deny` | redirect back with `error=access_denied` |
| `token_error` | the token endpoint answers this OAuth error |
| `token_delay_ms` | the token endpoint answers after this delay (X's slow exchange: over 30,000) |
| `userinfo_status` | the user endpoints answer this status (X: 402 credits, 429 limits) |
| `email` | `missing`, `unverified`, or (GitHub) `placeholder`, a `users.noreply.github.com` primary address |
| `id_token` | Google: `iss_bare`, `iss_wrong`, `aud_wrong`, `nonce_wrong`, `nonce_missing`, `expired` |
| `avatar` | X: `png`, `jpeg`, `gif`, `webp`, `svg`, `html`, `big`, `redirect`, `missing` |

Mail switches use `{"provider": "mail", "error": "<code>", "suppressed": [...], "delay_ms": n}`.

## `stub-scorer`

It holds one instance in a Durable Object and speaks the real scorer's single-use protocol: `GET /healthz` (the slot,
the binding, a build block with one session, two threads, the spec ids and digests a real scorer admits loads by,
and its limits), `POST /v2/prepare` (the files manifest; its SHA-256 must be the package digest),
`PUT /v2/files/<index>/<offset>` (chunk bytes are counted, never kept), `POST /v2/load` and `POST /v1/score`. Replies
have the real shape, and `token_count` is the scorer's own count from the compiled-in tokenizer table, so the
2,048-token refusal matches.

Answers come from markers in the text, one token each (`sn-stub:<key>=<value>`); with none the document is in the
`weak` band. The full table is in [`stub-scorer/src/markers.rs`](stub-scorer/src/markers.rs):

| Marker | Effect |
|---|---|
| `band=very_strong`, `strong`, `unclear`, `weak` | confidence 99.995, 95, 70 or 12.5, with a share below it |
| `conf=<p>`, `share=<p>` | exact percentages |
| `spans=auto`, `none`, `off`, `bad`, `short`; `span-begin` ... `span-end`; `span_conf=<p>` | passages: by default one over the whole text in a strong band, or the marked regions; never under 24 words except `short` |
| `status=unsupported_length`, `empty_text`, `no_word_tokens`, `error_tokenize`, `error_model` | the document's status |
| `http=<status>`, `delay_ms=<n>`, `package=other`, `reply=garbage`, `two`, `none` | faults of the whole reply |

Switches (`POST /__stub/switch` with a JSON object): `load_ms`, `load_error` (a load that ends `failed`; a refusal
reads `refused <code>: <detail>`), `healthz_status`, `binary_sha256`, `ort_sha256`, `sessions`, `threads`,
`token_head` and `bind_any` (score without a load). `POST /__stub/destroy` empties the slot as a destroyed container
would; `POST /__stub/reset` also restores every switch.

## `fixture-site`

One host per scenario under `fixtures.test` (a reserved top-level domain, so a fetch that escapes the dev build
resolves nowhere): `plain`, `robots-disallow`, `robots-agent`, `robots-wildcards`, `robots-404`, `robots-500`,
`robots-redirect`, `robots-loop`, `robots-big`, `robots-slow`, `noindex`, `redirects`, `deep`, `large`, `charset`,
`types`, `walls`, `slow` and `status`. Every host also serves `/article/<band>` (with the stub scorer's band marker),
`/article/short` and `/canonical`. The catalogue is in [`fixture-site/src/pages.rs`](fixture-site/src/pages.rs).
Every request is logged with the headers a test checks (agent, Web Bot Auth, cookie, referrer):
`GET /__fixture/log` and `POST /__fixture/log/clear` on any fixture host.

## `wallet-stub`

```sh
cargo run --manifest-path subnet/devtools/wallet-stub/Cargo.toml -- address --seed alice
cargo run --manifest-path subnet/devtools/wallet-stub/Cargo.toml -- variants --seed alice --message-file challenge.txt
cargo run --manifest-path subnet/devtools/wallet-stub/Cargo.toml -- verify --ss58 <address> --signature 0x... --message-file challenge.txt
```

A seed name maps to the mini secret `SHA-256("slop_ninja_dev_wallet_stub/v1/" ‖ name)`; the keys are public test
keys, never Substrate's `//Alice`, and must never hold funds. Signatures use the `substrate` signing context and are
derandomized from the seed and the bytes, so fixtures stay stable. `wrapped` signs `<Bytes>…</Bytes>`, `wrong-key`
signs with another test key under the requested address, and `wrong-message` signs the message with ` (altered)`
appended. A test checks every variant against `rtao_core::keys::verify_strict` at the fleet's rtao pin.

## Checks

- Native unit tests: `cargo test --workspace` here, and `cargo test` in `wallet-stub/`. They include the stub
  scorer's protocol read through `slop-ninja-scorer-client`, the client the detect Worker's lane uses.
- `./dev-session.sh smoke` against a running stubs session: the three OAuth code flows with PKCE (the ID token
  checked against the stub's key set), mail capture, the slow-exchange switch, the scorer protocol and markers, and
  the fixture site.

First run, 2026-11-13, with Wrangler 4.159.0 and no Cloudflare login: the stubs session served every smoke check over
`https://auth.slop.localhost:8789`; site and secret sessions with stand-in Workers on `slop.localhost:8787` and
`s.slop.localhost:8788` reached all three stubs and each other through the dev registry; D1 migrations applied under
`[env.dev]` to the site session's local store; R2, a Durable Object and a queue (producer and consumer) worked
locally; and a `__Host-` cookie set on the site host was not sent to the secret host. The site session has not yet
run with the real Workers, which join as each gains its `[env.dev]`.
