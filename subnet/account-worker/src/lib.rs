//! The SlopNinja account Worker.
//!
//! Sign-in by email link, Google, GitHub, X and a Bittensor wallet; sessions;
//! identities, linking and merging; saved links ("my detections"); export and
//! deletion; the billing ledger; and the signed account ops. Its stores are
//! the D1 `account-store` (`migrations/0001_account_store.sql`) and the
//! `QuotaLimiter` Durable Object. It binds no other Worker.
//!
//! The contracts it serves are fixed in `slop-ninja-account-wire` (views,
//! saved links, billing, sign-in bodies, quota buckets),
//! `slop-ninja-detect-page` (refusal codes, `X-SN-*` headers, origins, queue
//! messages, metric points, switches) and `slop-ninja-signed-op` (signed
//! ops).
