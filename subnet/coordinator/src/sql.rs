//! Storage-neutral SQL access and versioned schema migrations.
//!
//! Durable Object SQL storage and native SQLite (used by tests) both implement
//! [`Sql`], so the same statements run in both. Callers wrap every logical
//! operation in one storage transaction (`transactionSync` in a Durable Object);
//! the functions here never open or commit transactions themselves, because
//! Durable Object SQL rejects `BEGIN`/`COMMIT` statements.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SqlValue {
    Null,
    Integer(i64),
    Text(String),
}

impl From<i64> for SqlValue {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<&str> for SqlValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<String> for SqlValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreError(pub &'static str);

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

pub type Row = Vec<SqlValue>;

pub trait Sql {
    /// Executes one statement with positional `?` bindings and returns all rows.
    fn exec(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Row>, StoreError>;
}

pub fn int(row: &Row, index: usize) -> Result<i64, StoreError> {
    match row.get(index) {
        Some(SqlValue::Integer(value)) => Ok(*value),
        _ => Err(StoreError("expected integer column")),
    }
}

pub fn text(row: &Row, index: usize) -> Result<&str, StoreError> {
    match row.get(index) {
        Some(SqlValue::Text(value)) => Ok(value),
        _ => Err(StoreError("expected text column")),
    }
}

/// Converts a stored nonnegative integer.
pub fn uint(row: &Row, index: usize) -> Result<u64, StoreError> {
    u64::try_from(int(row, index)?).map_err(|_| StoreError("negative integer column"))
}

/// Converts a bounded u64 for storage. Callers validate the range on input.
pub fn bind_u64(value: u64) -> Result<SqlValue, StoreError> {
    if value > crate::wire::MAX_SAFE_INTEGER {
        return Err(StoreError("integer exceeds storage range"));
    }
    Ok(SqlValue::Integer(value as i64))
}

/// One schema version: statements applied in order inside the caller's transaction.
pub struct Migration {
    pub version: u32,
    pub statements: &'static [&'static str],
}

/// A Durable Object class's schema: a family name that must match the stored
/// one, and contiguous migrations starting at version 1.
pub struct Schema {
    pub family: &'static str,
    pub migrations: &'static [Migration],
}

impl Schema {
    pub fn latest(&self) -> u32 {
        self.migrations.last().map_or(0, |m| m.version)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MigrationError {
    /// The schema definition itself is not contiguous from 1.
    Definition,
    /// Storage holds another class's schema.
    Family,
    /// Storage was written by a newer build. Refuse rather than downgrade.
    NewerStoredVersion {
        stored: u32,
        supported: u32,
    },
    Store(StoreError),
}

impl From<StoreError> for MigrationError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MigrationReport {
    pub from: u32,
    pub to: u32,
}

const META_TABLE: &str =
    "CREATE TABLE IF NOT EXISTS schema_meta (name TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT";

pub fn stored_version(sql: &impl Sql) -> Result<(Option<String>, u32), StoreError> {
    sql.exec(META_TABLE, &[])?;
    let rows = sql.exec(
        "SELECT name, value FROM schema_meta WHERE name IN ('family', 'version')",
        &[],
    )?;
    let mut family = None;
    let mut version = 0;
    for row in &rows {
        match text(row, 0)? {
            "family" => family = Some(text(row, 1)?.to_owned()),
            _ => {
                version = text(row, 1)?
                    .parse()
                    .map_err(|_| StoreError("stored schema version"))?
            }
        }
    }
    Ok((family, version))
}

/// Migration entry point. Applies every pending version in order and records
/// the result; must run inside one storage transaction so a failed step leaves
/// the previous version intact.
pub fn migrate(sql: &impl Sql, schema: &Schema) -> Result<MigrationReport, MigrationError> {
    for (index, migration) in schema.migrations.iter().enumerate() {
        if migration.version as usize != index + 1 {
            return Err(MigrationError::Definition);
        }
    }
    let (family, from) = stored_version(sql)?;
    match family.as_deref() {
        Some(family) if family != schema.family => return Err(MigrationError::Family),
        None if from != 0 => return Err(MigrationError::Family),
        _ => {}
    }
    let supported = schema.latest();
    if from > supported {
        return Err(MigrationError::NewerStoredVersion {
            stored: from,
            supported,
        });
    }
    for migration in schema.migrations.get(from as usize..).unwrap_or_default() {
        for statement in migration.statements {
            sql.exec(statement, &[])?;
        }
    }
    if from < supported || family.is_none() {
        let upsert = "INSERT INTO schema_meta (name, value) VALUES (?, ?) \
                      ON CONFLICT(name) DO UPDATE SET value = excluded.value";
        sql.exec(upsert, &["family".into(), schema.family.into()])?;
        sql.exec(upsert, &["version".into(), supported.to_string().into()])?;
    }
    Ok(MigrationReport {
        from,
        to: supported,
    })
}

/// Chain observation journal.
///
/// - v1: webhook deduplication, reconciliation tasks and the finalized cursor.
/// - v2: the per-block heartbeat. Deliveries record their block; block events
///   are deduplicated by (stream, block hash) whether a webhook or RPC supplied
///   them; the verified finalized chain, durable faults, the watchdog mode and
///   its transitions, durable claims, the RPC call budget and RPC head audits.
///   v1 reconciliation tasks are superseded and dropped.
/// - v3: the RPC-confirmed finalized watermark. Webhook-advanced blocks stay
///   provisional until independent RPC evidence covers them. Existing storage
///   is seeded conservatively from its RPC anchor.
///
/// - v4: the coordinator's own signing journal (`crate::signing`): the
///   signer row (pause state, nonce floor, live runtime, the pending step
///   and its claim generation), installed targets, stored runtime metadata,
///   the last schedule observation, one commit operation per subnet epoch,
///   signed attempts with their nonce, era and exact bytes, a content-free
///   signing log and armed testnet drills. Signing starts paused.
/// - v5: signing failure accounting. The signer row gains the block its
///   head runtime was reported at, an RPC-error backoff, the last error, a
///   re-learn counter and its own RPC sub-budget window; `sign_failures`
///   holds per-subject deterministic failures with their block backoff and
///   whether they settled into a diagnostic; `commit_ops.diagnostic` records
///   a settled diagnostic (for example `reveal_unverifiable`).
/// - v6: the reveal search's bounds (`commit_ops.reveal_lo`, `reveal_hi`), so
///   a search cut short by a round's step limit continues where it stopped.
pub const CHAIN_JOURNAL_SCHEMA: Schema = Schema {
    family: "chain_journal",
    migrations: &[
        Migration {
            version: 1,
            statements: &[
                "CREATE TABLE deliveries (
                    delivery_id TEXT PRIMARY KEY,
                    subscription TEXT NOT NULL,
                    event_id TEXT NOT NULL,
                    body_sha256 TEXT NOT NULL,
                    first_attempt INTEGER NOT NULL,
                    received_at_ms INTEGER NOT NULL
                ) STRICT",
                "CREATE INDEX deliveries_event ON deliveries (event_id)",
                "CREATE TABLE reconcile_tasks (
                    event_id TEXT PRIMARY KEY,
                    delivery_id TEXT NOT NULL,
                    journal_position INTEGER NOT NULL,
                    block_number INTEGER NOT NULL,
                    block_hash TEXT NOT NULL,
                    parent_hash TEXT NOT NULL,
                    observed_at_ms INTEGER NOT NULL,
                    state TEXT NOT NULL
                        CHECK (state IN ('pending', 'verified', 'mismatched', 'behind_cursor')),
                    created_at_ms INTEGER NOT NULL
                ) STRICT",
                "CREATE INDEX reconcile_tasks_state ON reconcile_tasks (state, block_number)",
                "CREATE TABLE finalized_cursor (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    block_number INTEGER NOT NULL,
                    block_hash TEXT NOT NULL,
                    updated_at_ms INTEGER NOT NULL
                ) STRICT",
                "CREATE TABLE notified_head (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    block_number INTEGER NOT NULL,
                    block_hash TEXT NOT NULL,
                    updated_at_ms INTEGER NOT NULL
                ) STRICT",
                "CREATE TABLE rpc_finalized_head (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    block_number INTEGER NOT NULL,
                    block_hash TEXT NOT NULL,
                    fetched_at_ms INTEGER NOT NULL
                ) STRICT",
                "CREATE TABLE counters (name TEXT PRIMARY KEY, value INTEGER NOT NULL) STRICT",
            ],
        },
        Migration {
            version: 2,
            statements: &[
                "DROP TABLE reconcile_tasks",
                // Nullable so v1 rows survive; every v2 delivery sets them.
                "ALTER TABLE deliveries ADD COLUMN block_number INTEGER",
                "ALTER TABLE deliveries ADD COLUMN block_hash TEXT",
                "CREATE TABLE block_events (
                    stream TEXT NOT NULL CHECK (stream IN ('finalized')),
                    block_hash TEXT NOT NULL,
                    block_number INTEGER NOT NULL,
                    parent_hash TEXT NOT NULL,
                    source TEXT NOT NULL CHECK (source IN ('webhook', 'rpc')),
                    delivery_id TEXT,
                    state TEXT NOT NULL
                        CHECK (state IN ('pending', 'applied', 'covered', 'below_anchor', 'conflict')),
                    received_at_ms INTEGER NOT NULL,
                    resolved_at_ms INTEGER,
                    PRIMARY KEY (stream, block_hash)
                ) STRICT",
                "CREATE INDEX block_events_state ON block_events (state, block_number)",
                "CREATE TABLE finalized_blocks (
                    block_number INTEGER PRIMARY KEY,
                    block_hash TEXT NOT NULL,
                    source TEXT NOT NULL CHECK (source IN ('anchor', 'webhook', 'rpc')),
                    applied_at_ms INTEGER NOT NULL
                ) STRICT",
                "CREATE TABLE faults (
                    id INTEGER PRIMARY KEY,
                    kind TEXT NOT NULL,
                    block_number INTEGER NOT NULL,
                    expected_hash TEXT,
                    observed_hash TEXT,
                    source TEXT NOT NULL,
                    raised_at_ms INTEGER NOT NULL,
                    cleared_at_ms INTEGER,
                    clear_action TEXT
                ) STRICT",
                "CREATE TABLE watchdog (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    mode TEXT NOT NULL CHECK (mode IN ('primary', 'fallback')),
                    reason TEXT,
                    mode_since_ms INTEGER NOT NULL,
                    last_delivery_ms INTEGER,
                    first_tick_ms INTEGER,
                    last_tick_ms INTEGER,
                    last_head_read_ms INTEGER,
                    behind INTEGER NOT NULL CHECK (behind IN (0, 1)),
                    lag_blocks INTEGER
                ) STRICT",
                "INSERT INTO watchdog (id, mode, reason, mode_since_ms, behind) \
                 VALUES (1, 'primary', NULL, 0, 0)",
                "CREATE TABLE mode_transitions (
                    seq INTEGER PRIMARY KEY,
                    at_ms INTEGER NOT NULL,
                    from_mode TEXT NOT NULL,
                    to_mode TEXT NOT NULL,
                    reason TEXT NOT NULL,
                    cursor_number INTEGER,
                    head_number INTEGER
                ) STRICT",
                "CREATE TABLE claims (
                    op_id TEXT PRIMARY KEY,
                    holder TEXT NOT NULL,
                    generation INTEGER NOT NULL,
                    state TEXT NOT NULL CHECK (state IN ('claimed', 'released', 'done')),
                    claimed_at_ms INTEGER NOT NULL,
                    expires_at_ms INTEGER NOT NULL,
                    finished_at_ms INTEGER
                ) STRICT",
                "CREATE TABLE rpc_budget (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    window_start_ms INTEGER NOT NULL,
                    used INTEGER NOT NULL
                ) STRICT",
                "INSERT INTO rpc_budget (id, window_start_ms, used) VALUES (1, 0, 0)",
                "CREATE TABLE rpc_audits (
                    block_number INTEGER PRIMARY KEY,
                    block_hash TEXT NOT NULL,
                    observed_at_ms INTEGER NOT NULL
                ) STRICT",
            ],
        },
        Migration {
            version: 3,
            statements: &[
                "CREATE TABLE rpc_confirmed (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    block_number INTEGER NOT NULL,
                    block_hash TEXT NOT NULL,
                    basis TEXT NOT NULL CHECK (basis IN ('anchor', 'rpc_head', 'rpc_list')),
                    updated_at_ms INTEGER NOT NULL
                ) STRICT",
                // The anchor is the only RPC evidence v2 storage kept by height.
                "INSERT INTO rpc_confirmed (id, block_number, block_hash, basis, updated_at_ms)
                 SELECT 1, block_number, block_hash, 'anchor', applied_at_ms
                 FROM finalized_blocks WHERE source = 'anchor'
                 ORDER BY block_number DESC LIMIT 1",
            ],
        },
        Migration {
            version: 4,
            statements: &[
                "CREATE TABLE signer (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    state TEXT NOT NULL CHECK (state IN ('paused', 'active')),
                    reason TEXT,
                    nonce_floor INTEGER NOT NULL,
                    runtime_spec INTEGER,
                    runtime_tx INTEGER,
                    pending_plan TEXT,
                    pending_generation INTEGER,
                    updated_at_ms INTEGER NOT NULL
                ) STRICT",
                "INSERT INTO signer (id, state, reason, nonce_floor, updated_at_ms)
                 VALUES (1, 'paused', 'operator_hold', 0, 0)",
                "CREATE TABLE sign_targets (
                    target TEXT PRIMARY KEY,
                    bundle TEXT NOT NULL,
                    cohort INTEGER NOT NULL,
                    cohort_label TEXT NOT NULL,
                    revision INTEGER NOT NULL,
                    mechanism INTEGER NOT NULL,
                    manifest TEXT NOT NULL,
                    package TEXT NOT NULL,
                    reward_rule TEXT NOT NULL,
                    installed_at_ms INTEGER NOT NULL,
                    active INTEGER NOT NULL CHECK (active IN (0, 1))
                ) STRICT",
                "CREATE TABLE runtime_cache (
                    spec_version INTEGER NOT NULL,
                    tx_version INTEGER NOT NULL,
                    metadata_sha256 TEXT NOT NULL,
                    metadata_hex TEXT NOT NULL,
                    block_number INTEGER NOT NULL,
                    block_hash TEXT NOT NULL,
                    fetched_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (spec_version, tx_version)
                ) STRICT",
                "CREATE TABLE chain_schedule (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    block_number INTEGER NOT NULL,
                    block_hash TEXT NOT NULL,
                    tempo INTEGER NOT NULL,
                    last_epoch_block INTEGER NOT NULL,
                    pending_epoch_at INTEGER NOT NULL,
                    subnet_epoch_index INTEGER NOT NULL,
                    blocks_since_last_step INTEGER NOT NULL,
                    reveal_period INTEGER NOT NULL,
                    observed_at_ms INTEGER NOT NULL,
                    retry_block INTEGER
                ) STRICT",
                "CREATE TABLE commit_ops (
                    op_id TEXT PRIMARY KEY,
                    epoch INTEGER NOT NULL UNIQUE,
                    target TEXT NOT NULL,
                    base_op TEXT NOT NULL,
                    state TEXT NOT NULL
                        CHECK (state IN ('preparing', 'committing', 'committed', 'revealed', 'failed')),
                    detail TEXT,
                    preflight_block INTEGER,
                    preflight_hash TEXT,
                    preflight_nonce INTEGER,
                    preflight_json TEXT,
                    uid INTEGER,
                    row_json TEXT,
                    row_digest TEXT,
                    reveal_round INTEGER,
                    reveal_period INTEGER,
                    predicted_reveal_block INTEGER,
                    commit_block INTEGER,
                    commit_block_hash TEXT,
                    commit_index INTEGER,
                    readback TEXT,
                    readback_block INTEGER,
                    reveal_block INTEGER,
                    reveal_block_hash TEXT,
                    reveal_weights_match INTEGER,
                    next_check_block INTEGER,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL
                ) STRICT",
                "CREATE TABLE sign_attempts (
                    op_id TEXT NOT NULL,
                    attempt INTEGER NOT NULL,
                    nonce INTEGER NOT NULL,
                    era_birth INTEGER NOT NULL,
                    era_period INTEGER NOT NULL,
                    expiry INTEGER NOT NULL,
                    era_hash TEXT NOT NULL,
                    floor_block INTEGER NOT NULL,
                    spec_version INTEGER NOT NULL,
                    tx_version INTEGER NOT NULL,
                    reveal_round INTEGER NOT NULL,
                    ciphertext_sha256 TEXT NOT NULL,
                    call_sha256 TEXT NOT NULL,
                    payload_sha256 TEXT NOT NULL,
                    extrinsic_hash TEXT NOT NULL UNIQUE,
                    extrinsic_hex TEXT NOT NULL,
                    state TEXT NOT NULL CHECK (state IN ('signed', 'sending', 'submitted', 'uncertain',
                        'rejected', 'included', 'expired', 'displaced')),
                    sends INTEGER NOT NULL CHECK (sends IN (0, 1)),
                    submit_detail TEXT,
                    search_lo INTEGER,
                    search_hi INTEGER,
                    checked_block INTEGER,
                    included_block INTEGER,
                    included_hash TEXT,
                    included_index INTEGER,
                    dispatch TEXT,
                    signed_generation INTEGER NOT NULL,
                    signed_at_ms INTEGER NOT NULL,
                    sent_at_ms INTEGER,
                    resolved_at_ms INTEGER,
                    PRIMARY KEY (op_id, attempt)
                ) STRICT",
                "CREATE INDEX sign_attempts_state ON sign_attempts (state)",
                "CREATE TABLE sign_log (
                    seq INTEGER PRIMARY KEY,
                    at_ms INTEGER NOT NULL,
                    block_number INTEGER,
                    op_id TEXT,
                    attempt INTEGER,
                    event TEXT NOT NULL,
                    detail TEXT
                ) STRICT",
                "CREATE TABLE sign_drills (
                    name TEXT PRIMARY KEY,
                    remaining INTEGER NOT NULL,
                    armed_at_ms INTEGER NOT NULL
                ) STRICT",
            ],
        },
        Migration {
            version: 5,
            statements: &[
                "ALTER TABLE signer ADD COLUMN runtime_block INTEGER",
                "ALTER TABLE signer ADD COLUMN backoff_until_ms INTEGER",
                "ALTER TABLE signer ADD COLUMN rpc_failures INTEGER NOT NULL DEFAULT 0",
                "ALTER TABLE signer ADD COLUMN relearns INTEGER NOT NULL DEFAULT 0",
                "ALTER TABLE signer ADD COLUMN last_error TEXT",
                "ALTER TABLE signer ADD COLUMN last_error_at_ms INTEGER",
                "ALTER TABLE signer ADD COLUMN budget_window_ms INTEGER NOT NULL DEFAULT 0",
                "ALTER TABLE signer ADD COLUMN budget_used INTEGER NOT NULL DEFAULT 0",
                // The v4 head runtime was learned at the block its metadata
                // was fetched at.
                "UPDATE signer SET runtime_block = (SELECT MAX(r.block_number) FROM runtime_cache r
                    WHERE r.spec_version = signer.runtime_spec AND r.tx_version = signer.runtime_tx)
                 WHERE id = 1",
                "CREATE TABLE sign_failures (
                    subject TEXT PRIMARY KEY,
                    op_id TEXT,
                    attempt INTEGER,
                    failures INTEGER NOT NULL,
                    code TEXT NOT NULL,
                    first_at_ms INTEGER NOT NULL,
                    last_at_ms INTEGER NOT NULL,
                    retry_block INTEGER NOT NULL,
                    settled INTEGER NOT NULL CHECK (settled IN (0, 1))
                ) STRICT",
                "ALTER TABLE commit_ops ADD COLUMN diagnostic TEXT",
            ],
        },
        Migration {
            version: 6,
            statements: &[
                "ALTER TABLE commit_ops ADD COLUMN reveal_lo INTEGER",
                "ALTER TABLE commit_ops ADD COLUMN reveal_hi INTEGER",
            ],
        },
        // v7: the reward-v1 target this coordinator holds active, recorded
        // only from verified bundles' checked closures (absent: the launch
        // burn row), so a replaced target or the launch authorization after
        // the first activation is refused.
        Migration {
            version: 7,
            statements: &["CREATE TABLE sign_held_target (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    target TEXT NOT NULL,
                    activated_epoch INTEGER,
                    updated_at_ms INTEGER NOT NULL
                ) STRICT"],
        },
    ],
};

/// Cohort authority (M4): v1 was the idle stub; v2 the cohort engine's tables;
/// v3 the active-tempo clock, abandonment and attempt bookkeeping; v4 the
/// fan-out lanes; v5 an index on a job's receipts; v6 the purge of private
/// results and summaries; v7 resumable isolation and artifact attribution; v8
/// reward-v1 cohorts and probe builds.
pub const EVAL_COHORT_SCHEMA: Schema = Schema {
    family: "eval_cohort",
    migrations: &[
        Migration {
            version: 1,
            statements: &[
                "CREATE TABLE cohort_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                phase TEXT NOT NULL CHECK (phase IN ('idle')),
                updated_at_ms INTEGER NOT NULL
            ) STRICT",
                "INSERT INTO cohort_state (id, phase, updated_at_ms) VALUES (1, 'idle', 0)",
            ],
        },
        // v2 (M4): cohort phases, the never-reuse pool set, documents, generation
        // usage, scoring jobs and chunks, predictions, receipts and targets.
        Migration {
            version: 2,
            statements: crate::eval::engine::SCHEMA_V2,
        },
        // v3: the active-tempo clock, the abandoned phase, outage counters and
        // per-chunk claim fences with the highest committed attempt.
        Migration {
            version: 3,
            statements: crate::eval::engine::SCHEMA_V3,
        },
        // v4: fan-out lanes, batch shards, holders and claim generations, crash
        // isolation, generation fences and reservations, job-scoped faults.
        Migration {
            version: 4,
            statements: crate::eval::engine::SCHEMA_V4,
        },
        // v5: an index on a job's receipt rows.
        Migration {
            version: 5,
            statements: crate::eval::engine::SCHEMA_V5,
        },
        // v6: when a finished cohort's private results and summaries were purged.
        Migration {
            version: 6,
            statements: crate::eval::engine::SCHEMA_V6,
        },
        // v7: resumable crash isolation (claim kinds, handed-back attempts and
        // per-document progress) and artifact attribution per job.
        Migration {
            version: 7,
            statements: crate::eval::engine::SCHEMA_V7,
        },
        // v8: reward-v1 cohorts and probe builds: document sets, the reuse
        // ledger, the published index, probe versions, the reference chain.
        Migration {
            version: 8,
            statements: crate::eval::v3::SCHEMA_V8,
        },
        // v9: the rewrite-source ledger's voices, operations and pool, and the
        // reuse gap in drawn reward-v1 cohorts.
        Migration {
            version: 9,
            statements: crate::eval::v3::SCHEMA_V9,
        },
        // v10: coordinator admission: windows, sessions, parts, last
        // refusals, verdicts, the cutoff snapshot, the standing template.
        Migration {
            version: 10,
            statements: crate::eval::admission::SCHEMA_V10,
        },
        // v11: the finalized commitment log and its runtime.
        Migration {
            version: 11,
            statements: crate::eval::commitments::SCHEMA,
        },
    ],
};
