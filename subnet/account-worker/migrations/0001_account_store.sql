-- slop_ninja_account_store_v1: the account Worker's D1 store (its only writer).
--
-- No column holds text, an IP address or a provider token, and no slug is
-- stored in clear. There is no name, location or avatar column except X's
-- avatar, which X requires. Timestamps are integer milliseconds UTC. Every
-- *_h column is a SHA-256 or HMAC-SHA-256 under its own domain prefix.
-- Foreign keys are enforced (D1 enforces them by default).
--
-- The QuotaLimiter Durable Object holds the per-key buckets (sign-in mail,
-- connect mail per address and per account, paid quotes); they need no table.
-- A closed plan period has allowance = used and api_cap = api_used, and a
-- closed payment has tokens_left = 0, so the CHECKs make any later draw from
-- either fail and roll back its whole batch.

CREATE TABLE schema_info (k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE switches (name TEXT PRIMARY KEY, state TEXT NOT NULL, at INTEGER NOT NULL, journal_seq INTEGER NOT NULL);

CREATE TABLE users (
  user_id BLOB PRIMARY KEY,                  -- 16 random bytes
  state TEXT NOT NULL CHECK (state IN ('active','locked','deleted')),
  merged_into BLOB, created_at INTEGER NOT NULL, deleted_at INTEGER,
  flags INTEGER NOT NULL DEFAULT 0 CHECK (flags >= 0));   -- set_flag bits (paid_preview = 1), forwarded in X-SN-Principal

CREATE TABLE identities (
  identity_id INTEGER PRIMARY KEY, user_id BLOB NOT NULL REFERENCES users(user_id),
  provider TEXT NOT NULL CHECK (provider IN ('email','google','github','x','bittensor')),
  subject TEXT NOT NULL,                     -- email_norm | Google sub | GitHub numeric id | X user id | sr25519 pubkey hex
  email TEXT, email_verified INTEGER NOT NULL DEFAULT 0,
  handle TEXT,                               -- GitHub login, X username, SS58 (prefix 42) for display to the holder only
  consent_at INTEGER,                        -- X and wallet links: when the explicit connect was confirmed
  created_at INTEGER NOT NULL, last_used_at INTEGER NOT NULL,
  UNIQUE (provider, subject));
CREATE INDEX identities_user ON identities(user_id);

CREATE TABLE avatars (identity_id INTEGER PRIMARY KEY REFERENCES identities(identity_id),   -- X only
  ref BLOB NOT NULL UNIQUE,                  -- 16 random bytes, redrawn at each fetch; the only id the route takes
  content_type TEXT NOT NULL CHECK (content_type IN ('image/jpeg','image/png','image/gif','image/webp')),  -- magic bytes
  bytes BLOB NOT NULL, fetched_at INTEGER NOT NULL);                              -- the 48 px image, at most 64 KiB

CREATE TABLE sessions (
  token_h BLOB PRIMARY KEY, user_id BLOB NOT NULL, identity_id INTEGER NOT NULL,
  created_at INTEGER NOT NULL, last_seen_day TEXT NOT NULL, idle_expires_at INTEGER NOT NULL,
  abs_expires_at INTEGER NOT NULL, auth_time INTEGER NOT NULL, ua_family TEXT);
CREATE INDEX sessions_user ON sessions(user_id);

CREATE TABLE auth_flows (state_h BLOB PRIMARY KEY, binding_h BLOB NOT NULL, provider TEXT NOT NULL,
  intent TEXT NOT NULL CHECK (intent IN ('signin','link','step_up')),
  user_id BLOB, session_h BLOB,              -- link and step_up: the session that started the flow; NULL for signin
  pkce_verifier TEXT NOT NULL, nonce TEXT,
  return_to TEXT NOT NULL,                   -- the re-serialized path from auth-core::return_to, never the raw value
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
  CHECK ((intent = 'signin') = (user_id IS NULL)));
CREATE INDEX auth_flows_exp ON auth_flows(expires_at);
CREATE TABLE email_tokens (token_h BLOB PRIMARY KEY, addr_h BLOB NOT NULL,
  email_norm TEXT NOT NULL,                  -- the only recipient: the exact string that keys the identity
  email_typed TEXT NOT NULL,                 -- display on the holder's own pages only; never a recipient
  intent TEXT NOT NULL CHECK (intent IN ('signin','link','step_up','attach_pending','recover')),
  user_id BLOB,                              -- link, step_up, recover: the account the token acts for
  session_h BLOB,                            -- link, step_up: SHA-256 of the session token that asked; confirm needs it
  binding_h BLOB NOT NULL,                   -- the requesting browser's flow binding (recover: random bytes).
                                             -- attach_pending: also the only browser whose confirm is honoured
  return_to TEXT NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
  UNIQUE (addr_h, binding_h));               -- one outstanding link per address AND browser, never per address
CREATE INDEX email_tokens_exp ON email_tokens(expires_at);
CREATE TABLE wallet_chals (chal_h BLOB PRIMARY KEY, binding_h BLOB NOT NULL, pubkey BLOB NOT NULL, message BLOB NOT NULL,
  intent TEXT NOT NULL CHECK (intent IN ('signin','link','step_up')),
  user_id BLOB, session_h BLOB,              -- link and step_up, as in auth_flows
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
  UNIQUE (binding_h, pubkey));               -- a newer challenge from one browser replaces its older one
CREATE INDEX wallet_chals_exp ON wallet_chals(expires_at);
CREATE TABLE pending_links (                 -- a resolved identity waiting for its confirm POST
  binding_h BLOB PRIMARY KEY,                -- SHA-256 of a fresh __Host-sn_flow binding set when the row is written
  kind TEXT NOT NULL CHECK (kind IN ('link','merge','email_in_use')),
  user_id BLOB NOT NULL,                     -- link, merge: the signed-in user; email_in_use: the account owning the address
  session_h BLOB,                            -- link, merge: the session that ran the flow
  provider TEXT NOT NULL, subject TEXT NOT NULL, email TEXT, email_verified INTEGER NOT NULL, handle TEXT,
  from_user_id BLOB,                         -- merge: the account B that owns the identity now
  return_to TEXT NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL);
CREATE INDEX pending_links_exp ON pending_links(expires_at);
CREATE TABLE email_suppress (addr_h BLOB PRIMARY KEY, reason TEXT NOT NULL, at INTEGER NOT NULL);

CREATE TABLE owned (                         -- "my detections"
  entry_id INTEGER PRIMARY KEY, user_id BLOB NOT NULL, det_id_h BLOB NOT NULL,
  enc_ref BLOB NOT NULL,                     -- nonce ‖ AES-256-GCM(K_owned(user_id), {slug, kind, created, band, words, expires})
  is_owner INTEGER NOT NULL, created_at INTEGER NOT NULL, UNIQUE (user_id, det_id_h));

CREATE TABLE api_keys (key_h BLOB PRIMARY KEY, user_id BLOB NOT NULL, prefix TEXT NOT NULL, label TEXT,
  created_at INTEGER NOT NULL, last_used_day TEXT, revoked_at INTEGER);                       -- the paid API's keys (later)

CREATE TABLE payments (                      -- every source of credit; grants are non-cash
  payment_id INTEGER PRIMARY KEY, user_id BLOB NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('grant','processor','tao')), cash INTEGER NOT NULL,
  tokens_total INTEGER NOT NULL, tokens_left INTEGER NOT NULL CHECK (tokens_left >= 0),
  amount_cents INTEGER, currency TEXT, processor_ref TEXT UNIQUE,
  state TEXT NOT NULL CHECK (state IN ('active','refunded','reversed','void')),
  refundable_until INTEGER, created_at INTEGER NOT NULL, journal_seq INTEGER NOT NULL,
  CHECK (state = 'active' OR tokens_left = 0));  -- a closed source has no headroom, so a late decrement aborts
CREATE INDEX payments_fifo ON payments(user_id, created_at) WHERE state = 'active' AND tokens_left > 0;

CREATE TABLE plan_periods (period_id INTEGER PRIMARY KEY, user_id BLOB NOT NULL, payment_id INTEGER NOT NULL,
  starts_at INTEGER NOT NULL, ends_at INTEGER NOT NULL,
  allowance INTEGER NOT NULL, api_cap INTEGER NOT NULL,   -- the drawable ceilings; closing sets them to used, api_used
  allowance_granted INTEGER NOT NULL, api_cap_granted INTEGER NOT NULL,   -- the plan's figures, never changed
  used INTEGER NOT NULL DEFAULT 0, api_used INTEGER NOT NULL DEFAULT 0, cash INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('active','ended','reversed')),
  CHECK (used >= 0
         AND used <= allowance),
  CHECK (api_used >= 0
         AND api_used <= api_cap),
  CHECK (state = 'active' OR (allowance = used AND api_cap = api_used)));

CREATE TABLE holds (hold_id TEXT PRIMARY KEY, user_id BLOB NOT NULL, channel TEXT NOT NULL, tokens INTEGER NOT NULL,
  parts TEXT NOT NULL,                       -- JSON [{src:"period"|"payment", id, tokens, cash}]
  idem TEXT NOT NULL UNIQUE, state TEXT NOT NULL CHECK (state IN ('held','committed','released')),
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE hold_ends (hold_id TEXT PRIMARY KEY,   -- a hold commits or releases exactly once
  kind TEXT NOT NULL CHECK (kind IN ('committed','released')), at INTEGER NOT NULL);
CREATE TABLE debits (debit_id INTEGER PRIMARY KEY, hold_id TEXT NOT NULL, user_id BLOB NOT NULL, src TEXT NOT NULL,
  src_id INTEGER NOT NULL, tokens INTEGER NOT NULL, cash INTEGER NOT NULL, package_pub TEXT NOT NULL,
  receipt_id TEXT NOT NULL, channel TEXT NOT NULL, at INTEGER NOT NULL, UNIQUE (hold_id, src, src_id));

CREATE TABLE journal (seq INTEGER PRIMARY KEY, at INTEGER NOT NULL, actor TEXT NOT NULL,   -- user | operator:<fpr> | auto:<rule>
  op TEXT NOT NULL,                          -- canonical JSON, content-free: no address, handle, slug or token
  user_h BLOB, reason TEXT, signature BLOB, nonce_h BLOB UNIQUE);

INSERT INTO schema_info (k, v) VALUES ('schema', 'slop_ninja_account_store_v1');
