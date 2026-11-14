# Dev tools: stubs and the local dev session

Dev only. Nothing here is deployed, and nothing here calls a model, a sign-in provider, a mail service, a chain
endpoint or any third-party site. The stubs stand in for those in local `wrangler dev` sessions, so the detection
pages, accounts and the URL check can be exercised end to end on one machine with no Cloudflare login.

| Tool | Kind | Stands in for |
|---|---|---|
| [`stub-scorer`](stub-scorer/) | Worker | the scorer Container: its load protocol and `/v1/score`, answers keyed by markers in the text |
| [`auth-stub`](auth-stub/) | Worker | Google, GitHub and X sign-in, X avatars, and sign-in mail (captured at `/mail`), with failure switches |
| [`fixture-site`](fixture-site/) | Worker | websites: robots variants, `noindex`, redirect chains, deep nesting, large pages, charsets, walls; and the outside services the detect Worker's host screens and IndexNow call (DoH resolvers, IndexNow) |
| [`x-stub`](x-stub/) | Worker | X's free embed (oEmbed): per-post answers, failure switches, the two control posts, call counters |
| [`wallet-stub`](wallet-stub/) | native CLI | a Bittensor wallet: sr25519 signatures in raw, wrapped, wrong-key and wrong-message variants |
| [`op-stub`](op-stub/) | native CLI | the operator's CLI, for the local Workers: ops signed with the session's throwaway operator key |
| [`session/`](session/) | scripts | the dev session ([`dev-session.sh`](session/dev-session.sh)) and a smoke check of the stubs |

## The dev session

Three `wrangler dev --local` sessions, each on its own host and port over local HTTPS, joined through one dev
registry so that service bindings cross them:

| Session | Origin | Primary Worker | Other Workers |
|---|---|---|---|
| site | `https://slop.localhost:8787` | the website (`wrangler.dev.jsonc` of the website repository) | detect, account, detect-og, detect-fetch |
| secret | `https://s.slop.localhost:8788` | the secret-host Worker | none |
| stubs | `https://auth.slop.localhost:8789` | `auth-stub` | `stub-scorer`, `fixture-site`, `x-stub` |

Only a session's primary Worker listens for HTTP, so each public host gets its own session, and the stubs get a third:
the browser must reach the sign-in stubs' authorize pages, and test suites read captured mail and flip switches.
`auth-stub` is the stubs session's door to its neighbours: `/__scorer/<path>` reaches the stub scorer,
`/__fixture/<host>/<path>` the fixture site and `/__xstub/<path>` the X stub.

Local D1, R2, Durable Objects and queues all run inside the site session. Its two D1 stores, the detect store and the
account store, are named in [`session/stores.toml`](session/stores.toml), and `migrate` applies each Worker's own
`migrations/` to them. The origins come
from dev vars, so `__Host-` cookies and the Origin checks behave as in production: a cookie set on
`slop.localhost` never reaches `s.slop.localhost`.

```sh
cd subnet/devtools/session
./dev-session.sh cert       # once: a local certificate for *.slop.localhost (trust it in the test browser)
WEB_REPO=<website checkout> ./dev-session.sh build   # every session Worker (worker-build 0.8.x; WORKER_BUILD names another binary)
./dev-session.sh migrate    # both D1 stores' migrations, and the dev package into the local package bucket
./dev-session.sh stores     # each local store's tables
./dev-session.sh stubs      # terminal 1
WEB_REPO=<website checkout> ./dev-session.sh site      # terminal 2
WEB_REPO=<website checkout> ./dev-session.sh secret    # terminal 3
./dev-session.sh smoke      # checks a running stubs session
WEB_REPO=<website checkout> ./dev-session.sh run <command>   # all three sessions around one command (the suites)
```

`build` builds each Worker in its own directory: the stubs, every subnet Worker that has joined (with its `dev`
feature where it has one) and, with `WEB_REPO`, the website and the secret-host Worker (`npm run build`,
`npm run build:secret`). A joined Worker's `[env.dev.build]` runs nothing: `wrangler dev` runs a build command in
the session's working directory, not the Worker's, so a session never builds through Wrangler. `run` starts the
three sessions as its children (logs in `$DEV_STATE/logs/`), waits until each origin answers (at most `READY_S`
seconds), runs the command with `DEV_STATE`, `DEV_SUBNET`, `DEV_OP_SEED` and `NODE_EXTRA_CA_CERTS` set, and stops the sessions
when it ends; its exit status is the command's. The website repository's `scripts/it/` and `scripts/e2e/` suites
run this way.

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
- **Local resources only:** D1 bindings, R2 buckets, queues and Durable Objects, all simulated locally. No route,
  custom domain, `workers_dev` or account.
- **D1:** the local store is keyed by `database_id`, so a Worker's `[[env.dev.d1_databases]]` entry names its session
  store exactly as `session/stores.toml` does, with `migrations_dir = "migrations"`. No other Worker binds either store.

  | Worker | Binding | `database_id` | Migrations |
  |---|---|---|---|
  | detect | `DETECT_STORE` | `dev-detect-store` | `subnet/detect-worker/migrations/` |
  | account | `ACCOUNT_STORE` | `dev-account-store` | `subnet/account-worker/migrations/` |
- **No build of its own:** `[env.dev.build]` sets `command = ""`; `dev-session.sh build` builds it.
- **HTTPS upstream:** a top-level `[dev]` table with `upstream_protocol = "https"` (deploys ignore it). Wrangler's
  local proxy hands each request to the Worker under the upstream protocol of the joined Worker it loaded last and
  rewrites `Origin` and `Referer` to match; with every joined Worker on HTTPS the Worker sees the origin the
  browser sent, so the Origin checks run as in production.
- **Secrets** come from the Worker's untracked `.dev.vars.dev` (its `.gitignore`), which `dev-session.sh secrets`
  (run by `site`) writes from random values kept in `$DEV_STATE`; no secret value is tracked.
- **The operator key.** `dev-session.sh secrets` also makes the session's own Ed25519 operator key pair once per state
  directory: `$DEV_STATE/dev_op_key` (the seed) and `dev_op_key.pub`, each 64 lowercase hex digits, mode 600, the
  operator CLI's key file format. The public key goes to the detect and account Workers' `.dev.vars.dev` as
  `DETECT_OP_KEY`, over the empty placeholder of their `[env.dev.vars]` (where `OP_ENV` is `staging`), so the dev
  session's op routes (`/api/detect/v1/op`, `/api/account/v1/op`) verify ops signed with that seed for target
  `detect` or `account` and env `staging`. No deployed environment trusts it. `run` passes the seed file to its
  command as `DEV_OP_SEED`; the suites sign ops with [`op-stub`](op-stub/) and post them to the site's op routes, as
  the operator's CLI does against a deployed stack. No tracked config holds a key (a source test).
- **Stubs, only here and only behind the cargo feature `dev`:**
  - detect: `STUB_SCORER` (service `slop-ninja-dev-stub-scorer`) in place of the scorer Container's port. The
    stub is always running; the lane loads the dev package ([`session/dev-package/`](session/dev-package/): a
    files manifest v2 and six placeholder files, no model) from the local package bucket `dev-detect-packages`
    through the real load protocol, pinned to each object's MD5 entity tag. A test
    (`stub-scorer/tests/dev_package.rs`) holds the manifest, the files and the detect Worker's `[env.dev.vars]`
    (`PACKAGE_SHA256`, `PACKAGE_SPEC_SHA256`, the bucket label and the stub's build identity) to each other;
  - account: `AUTH_STUB` (service `slop-ninja-dev-auth-stub`) for the token, user, revoke, avatar and mail calls,
    with the browser sent to `STUB_ORIGIN` for authorize;
  - detect: `DETECT_FETCH` (service `slop-ninja-detect-fetch-dev`), `DETECT_HOST` (its v2 migration) and
    `FETCH_WAIT_MS`, as in the deployed envs, with `DETECT_URLS` and `DETECT_X` on in the vars so the suites' switch
    rows turn them off; the dev build's URL rules admit `DEV_FIXTURE_HOST` (`fixtures.test`) and every host under it,
    each its own domain; and its outside calls (the host screens' DoH queries to `security.cloudflare-dns.com` and
    `family.cloudflare-dns.com`, and IndexNow) go through `DEV_NET` (service `slop-ninja-dev-fixture-site`), so a
    session asks no third party;
  - detect-fetch: its own `wrangler.toml` declares no binding of any kind, so its dev build joins through
    [`session/detect-fetch.dev.toml`](session/detect-fetch.dev.toml): `FIXTURE_SITE` (service
    `slop-ninja-dev-fixture-site`) for `fixtures.test` and every host under it (unscreened, fetched unsigned), and
    `X_STUB` (service `slop-ninja-dev-x-stub`) for X's embed.

The values are fixed in one place, `slop_ninja_dev_http::session` (the origins, the subject prefix and the stores). A
source test (`http/tests/dev_configs.rs`) refuses any `slop-ninja-dev-` name outside `[env.dev]` in a Worker config
under `subnet/`, parses the dev origins with the page contract's `Origins` (as every Worker parses its origin vars),
and holds every `[env.dev.vars]`, every `[env.dev]` D1 entry, the session script and the smoke check to these values.

**Provider endpoints in the account Worker's dev build.** The dev build (auth-core's `dev` feature) moves every
provider URL under the stub origin `S` as `S/<real host><real path and query>`, and the stub serves each endpoint
there: `https://oauth2.googleapis.com/token` is `S/oauth2.googleapis.com/token`. The browser is sent to the authorize
URLs; the server calls go through `AUTH_STUB`.

| Provider | Authorize (browser) | Token | User and the rest |
|---|---|---|---|
| Google | `S/accounts.google.com/o/oauth2/v2/auth` | `S/oauth2.googleapis.com/token` | ID token claims; key set `S/www.googleapis.com/oauth2/v3/certs`, discovery `S/accounts.google.com/.well-known/openid-configuration` |
| GitHub | `S/github.com/login/oauth/authorize` | `S/github.com/login/oauth/access_token` | `S/api.github.com/user`, `S/api.github.com/user/emails` |
| X | `S/x.com/i/oauth2/authorize` | `S/api.x.com/2/oauth2/token` | `S/api.x.com/2/users/me?user.fields=...`; revoke `S/api.x.com/2/oauth2/revoke`; avatars `https://pbs.twimg.com/<path>` at `S/pbs.twimg.com/<path>` |

The table is `Endpoint` in [`auth-stub/src/routes.rs`](auth-stub/src/routes.rs), and a test drives each provider
through these URLs; another (`auth-stub/tests/providers.rs`) holds the table to the account Worker's own
constants in `slop-ninja-auth-core` and to its dev `endpoints_at` form. Google's discovery document names the moved endpoints. The stub's own routes are `/`, the consent
page `/__stub/consent/<provider>`, the mail routes and `/__stub/*`; no provider URL moves onto them.

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

Its providers are the account contract's OAuth sign-in methods (`slop-ninja-account-wire` `Provider`, under the same
wire strings), and `SITE_ORIGIN` and `STUB_ORIGIN` parse with the page contract's `Origin`: a malformed origin answers
500 rather than falling back to a default. The avatar answers are labelled with the contract's stored types
(`AvatarType`); the SVG, HTML and oversized answers are ones the account Worker drops.

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
| `avatar` | X: `png`, `jpeg`, `gif`, `webp`, `svg`, `html`, `big`, `redirect` (a 302 to another avatar under `S`, never the real host), `missing` |

Mail switches use `{"provider": "mail", "error": "<code>", "suppressed": [...], "delay_ms": n}`.

## `stub-scorer`

It holds one instance in a Durable Object and speaks the real scorer's single-use protocol: `GET /healthz` (the slot,
the binding, a build block with one session, two threads, the spec ids and digests a real scorer admits loads by,
and its limits), `POST /v2/prepare` (the files manifest; its SHA-256 must be the package digest),
`PUT /v2/files/<index>/<offset>` (chunk bytes are counted, never kept), `POST /v2/load` and `POST /v1/score`. Replies
have the real shape, and `token_count` is the scorer's own count from the compiled-in tokenizer table, so the
2,048-token refusal matches.

Answers come from markers in the text, one token each (`sn-stub:<key>=<value>`); with none the document is in the
`weak` band. Band values are the page contract's band codes (`slop-ninja-detect-page` `Band`), and the fixture site's
`/article/<band>` takes the same codes. The full table is in [`stub-scorer/src/markers.rs`](stub-scorer/src/markers.rs):

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

The detect Worker's dev build sends its outside calls here through `DEV_NET`, on their real hosts and paths:
`security.cloudflare-dns.com` and `family.cloudflare-dns.com` answer `GET /dns-query?name=<host>&type=A` (JSON,
`Accept: application/dns-json` only) with `0.0.0.0` for the `malware` and `phishing` fixture hosts (and, on the family
resolver, `adult`) and a TEST-NET address for every other name; `api.indexnow.org` answers `POST /indexnow` with 200.
The log keeps each call, so a suite sees which screens and pings ran.

## `x-stub`

X's free embed on its real host and path: `GET /publish.x.com/oembed?url=https://twitter.com/i/status/<id>&omit_script=1&dnt=true`
on the stubs origin, or `https://publish.x.com/oembed?...` through a service binding. A request without
`omit_script=1` and `dnt=true`, with a handle in the URL, or carrying a cookie, key or referrer answers 400. Every
post is synthetic (the test names the id; the text, name and handle are the stub's own), and the success body follows
X's documented oEmbed shape: a `rich` object whose `html` is a `blockquote.twitter-tweet` with the text in its first
`<p>`. Answers per post (`POST /__x/switch {"id", "answer"}`, `{"all": ...}` for every id, `{"id", "gone_at_ms"}`,
`{"slow_ms"}`; `POST /__x/reset`; `GET /__x/state`): `plain`, `reply` (leading mentions), `media` (a trailing
`pic.x.com` link), `quote` (a trailing link), `long` (cut, ending in "…"), `short`, `gone` (JSON 404), `forbidden`
(JSON 403), `html403`, `busy` (429), `error` (503), `slow`, `oversize` and `changed` (a `<span>` in the `<p>`). The
two control posts (vars `LIVE_CONTROL`, `GONE_CONTROL`, synthetic ids) answer present and JSON 404. The state route
counts calls per id, every call, and the most calls in flight at once. The catalogue is in
[`x-stub/src/oembed.rs`](x-stub/src/oembed.rs).

## `op-stub`

```sh
cargo run --manifest-path subnet/devtools/op-stub/Cargo.toml -- pub --seed-file "$DEV_OP_SEED"
cargo run --manifest-path subnet/devtools/op-stub/Cargo.toml -- sign --seed-file "$DEV_OP_SEED" \
  --target detect --env staging --op set_switch --args '{"name":"DETECT_URLS","state":"off","reason":"dev"}'
```

`sign` prints the op route's body (`slop-ninja-signed-op` `Wire`), signed by that crate's own `sign` with the
session's key: the local Workers verify it exactly as a deployed Worker verifies the operator's CLI. `--issued-ms`,
`--valid-ms` and `--nonce` give the variants a test of the op rules needs (an expired op, a reused nonce), and
`--env` and `--target` the refused ones. A test checks each against `Verifier`. Local sessions only: the key is a
throwaway pair made per state directory.

## `wallet-stub`

```sh
cargo run --manifest-path subnet/devtools/wallet-stub/Cargo.toml -- address --seed alice
cargo run --manifest-path subnet/devtools/wallet-stub/Cargo.toml -- variants --seed alice --message-file challenge.txt
cargo run --manifest-path subnet/devtools/wallet-stub/Cargo.toml -- verify --ss58 <address> --signature 0x... --message-file challenge.txt
cargo run --manifest-path subnet/devtools/wallet-stub/Cargo.toml -- variants --seed alice --challenge-file answer.json
```

`--challenge-file` reads the account Worker's `/wallet/challenge` answer (`slop-ninja-account-wire`
`WalletChallengeAnswer`, JSON; `-` reads stdin) and signs its `message`; `sign` and `variants` then also print
`verify_request`, the `/wallet/verify` body (`WalletVerifyRequest`) that answers it.

A seed name maps to the mini secret `SHA-256("slop_ninja_dev_wallet_stub/v1/" ‖ name)`; the keys are public test
keys, never Substrate's `//Alice`, and must never hold funds. Signatures use the `substrate` signing context and are
derandomized from the seed and the bytes, so fixtures stay stable. `wrapped` signs `<Bytes>…</Bytes>`, `wrong-key`
signs with another test key under the requested address, and `wrong-message` signs the message with ` (altered)`
appended. A test checks every variant against `rtao_core::keys::verify_strict` at the fleet's rtao pin, signing the account
contract's own `/wallet/challenge` fixture issued to the stub's address and carrying each signature in a
`/wallet/verify` body.

## Checks

- Native unit tests: `cargo test --workspace` here, and `cargo test` in `wallet-stub/` and `op-stub/`. They include the stub
  scorer's protocol read through `slop-ninja-scorer-client`, the client the detect Worker's lane uses.
- Source checks (`http/tests/dev_configs.rs`): local-only stub and store configs, `[env.dev]`-only stub names, the dev
  origins and stores agreed everywhere, and a session script that never logs in, deploys or leaves `--local`.
- `./dev-session.sh smoke` against a running stubs session: the three OAuth code flows with PKCE through the real-host
  paths above (the ID token checked against the stub's key set; X's avatar and revoke), mail capture, the
  slow-exchange switch, the scorer protocol and markers, and the fixture site.

First run, 2026-11-13, with Wrangler 4.159.0 and no Cloudflare login: the stubs session served every smoke check over
`https://auth.slop.localhost:8789`; site and secret sessions with stand-in Workers on `slop.localhost:8787` and
`s.slop.localhost:8788` reached all three stubs and each other through the dev registry; D1 migrations applied under
`[env.dev]` to the site session's local store; R2, a Durable Object and a queue (producer and consumer) worked
locally; and a `__Host-` cookie set on the site host was not sent to the secret host. Second run, the same day, after
the detect and account stores' first migrations landed: `migrate` applied both (`0001_detect_store.sql`, 31
statements; `0001_account_store.sql`, 28) to the site session's local state and a second `migrate` found nothing to
apply; a stand-in Worker in the site session, bound to both stores under the session's ids from its own config, read
every table, saw foreign keys on, and had its CHECK and NOT NULL violations refused. Third run, the same day, with
the real Workers (Wrangler 4.160.1): the website and the secret-host Worker with the detect, account and card
Workers in the site session and the three stubs, under `run`; the free check loaded the dev package through the
stub scorer and answered with its seal, Get a link, the secret page, its card and the manage delete worked end
to end, and the website repository's HTTP suites and browser flows ran against it.
