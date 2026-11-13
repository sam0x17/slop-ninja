# slop-ninja-auth-core

Pure sign-in pieces for the SlopNinja account Worker (`subnet/account-worker`), and the CSRF and cookie checks the
website and detect Workers share with it. Nothing here does I/O: the Worker owns D1, the quota Durable Object, the
network fetches, the clock and the random source, and calls these functions with what it read. Every random value
(a token, a PKCE verifier, a wallet nonce) is drawn by the caller from the runtime's `crypto.getRandomValues` and
passed in; every time is Unix milliseconds passed in.

The crate is its own Cargo workspace, like `crates/arena`: its Cloudflare mail binding is a wasm32-only dependency,
and Workers depend on it by path from their own workspaces. A Worker that enables the `wallet` feature carries the
same rtao `[patch]` blocks as `Cargo.toml` here (as the admission Worker does).

## Modules

| Module | What it decides |
|---|---|
| `token` | 256-bit tokens (`state`, OIDC nonce, flow binding, session, email link, wallet challenge id): 43 characters of base64url, strict parse; storage holds only `SHA-256` of the 32 bytes |
| `pkce` | S256 verifiers (32 random bytes, 43 characters) and challenges; `verify` is the provider's side, for the auth stub and tests |
| `state` | the callback query (each parameter at most once, bounded); `check_flow` on the row a `DELETE … RETURNING` took: replayed or unknown `state`, missing, malformed or different flow cookie, provider mix-up, expiry; `check_email_token` for the email link row |
| `oidc` | Google ID token claims: `iss` in both forms, `aud` (and `azp`), `exp`, `iat` skew, `nonce`, `sub`; the email counts only when `email_verified` is true. No signature check (the token comes straight from the token endpoint over TLS with the client secret), so no JWKS fetch |
| `providers` | Google, GitHub and X endpoints and parameters as constants; authorize URLs (RFC 3986 encoding, S256 PKCE on every provider, a nonce for Google only, no offline access anywhere); token exchange requests (client secret in the body for Google and GitHub, Basic for X); profile reads; bounded parsers for token, `/user`, `/user/emails` and `/2/users/me` answers; the X avatar URL rule and byte sniffing |
| `siws` (feature `wallet`) | the wallet sign-in message (Sign-In-with-Substrate layout for extensions, one line for the command-line tools), SS58 decoding with any registered prefix, signature hex parsing, and verification through `rtao_core::keys::verify_strict` over the issued bytes raw or `<Bytes>`-wrapped and nothing else; `check_challenge` on the challenge row |
| `cookie` | `__Host-sn_session` and `__Host-sn_flow`: `Set-Cookie` strings, clearing, and lookup (a repeated name counts as absent) |
| `csrf` | `check(method, Sec-Fetch-Site, Origin, host origin)`, its scope by request class, and the body content types |
| `email_addr` | ASCII dot-atom local parts, UTS 46 to A-label domains, `email_norm` (ASCII lower case only), `addr_h` |
| `return_to` | the post-sign-in destination: character rules on the raw and percent-decoded value, a `url`-crate join against the configured origin, the prefix list, the length bound, an absolute `Location` |
| `policy` | `auth-policy/v1` as JSON with a reason beside every value; the mail token buckets and which mail kinds draw on which |
| `mail` | the mail kinds and their copy, `Mailer`, the Cloudflare Email Sending implementation (wasm32), and `Capture` (feature `dev`) |
| `origin` | `SITE_ORIGIN` and `SECRET_ORIGIN` from config, validated as canonical origins |
| `time` | RFC 3339 timestamps with milliseconds |

## Features

- `wallet`: wallet sign-in through rtao's portable core (`rtao-core` with `default-features = false`, at the same
  revision the other crates pin). Off by default, so the website and detect Workers, which need only `csrf` and
  `cookie`, link no chain code.
- `dev`: local development only. It adds `Spec::endpoints_at` (every provider endpoint moved under the auth stub's
  origin as `<stub origin>/<real host><real path and query>`, so the stub routes on the real host and path),
  `mail::capture::Capture` and, on wasm32, `ServiceCapture` (posts each message to the stub's `/mail` route over a
  service binding), `oidc::test_token`, and `http` origins on loopback hosts. Release builds never enable it.

## Bounds

Every parser checks its input's length before reading it. The account Worker's memory model takes these as its
per-request inputs.

| Input | Bound |
|---|---|
| callback query | 4 KiB (`state::CALLBACK_QUERY_MAX`); `code` at most 1 KiB |
| token endpoint answer | 16 KiB (`providers::TOKEN_BODY_MAX`); access token 4 KiB, ID token 8 KiB |
| profile or address-list answer | 64 KiB (`providers::PROFILE_BODY_MAX`) |
| avatar body | 64 KiB (`providers::AVATAR_MAX`) |
| `Cookie` header | 16 KiB (`cookie::COOKIE_HEADER_MAX`) |
| email address | 254 octets, local part 64 |
| `return_to` | `auth-policy/v1` `return_to_max` (512 bytes), raw and re-serialized |
| wallet address text | 64 characters; signature exactly 64 bytes |

## How the account Worker uses it

- **OAuth start:** draw `state`, the flow binding (when the browser has none), a nonce and a PKCE verifier; store
  their hashes and the verifier with the provider, the sanitized `return_to` and `now + flow_ttl_s`; set the flow
  cookie; redirect to `providers::authorize_url`.
- **OAuth callback:** `CallbackQuery::parse`, take the row by `state.hash()`, `check_flow`, then `token_request`,
  `parse_token_response`, and `oidc::check` (Google) or the profile reads (GitHub, X). The tokens are dropped.
- **Email:** `email_addr::parse`, the policy buckets for the mail kind, `mail::compose`, `Mailer::send`; on confirm,
  take the row by the posted token's hash and `check_email_token`.
- **Wallet:** `siws::decode_address`, `siws::message` (store the exact bytes), and on verify take the row,
  `check_challenge`, `parse_signature`, `siws::verify`.
- **Every browser route:** `csrf::check_for` first, and `csrf::content_type_ok` before reading a body.

## Tests

`cargo test --features wallet,dev` runs the auth-core rows of gate G-A1 (`tests/g_a1.rs`, `tests/siws.rs`,
`tests/return_to.rs` with every bypass fixture, `tests/email_addr.rs` with the folding fixtures, `tests/csrf.rs` with
the Fetch Metadata and `Origin` matrix including the secret host posting to the site host), the property that the
mail recipient equals the identity's subject byte for byte (`tests/email_addr.rs`, `tests/capture.rs`), the separate
budget for connect mail (`tests/mail_budgets.rs`) and the provider tables and parsers (`tests/providers.rs`). No test
calls a provider, mail service or chain endpoint.
