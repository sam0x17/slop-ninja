//! Immutable reward/quantization rules for one evaluation authority.
//!
//! Loading a mismatched deployment preserves execution under the stored pair.
//! New plans are refused until the configured pair is restored or the operator
//! selects a fresh authority. Retirement never replaces the stored rules.

use crate::config::{Authority, RecordRules};
use crate::sql::{Sql, SqlValue, StoreError, bind_u64, text};

pub const SCHEMA_V32: &[&str] = &[
    "CREATE TABLE record_rules (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        reward_rule TEXT NOT NULL, quant_rule TEXT NOT NULL,
        set_ms INTEGER NOT NULL
    ) STRICT",
    "CREATE TRIGGER record_rules_no_replace BEFORE INSERT ON record_rules
        WHEN EXISTS (SELECT 1 FROM record_rules)
        BEGIN SELECT RAISE(ABORT, 'record rules are immutable'); END",
    "CREATE TRIGGER record_rules_no_update BEFORE UPDATE ON record_rules
        BEGIN SELECT RAISE(ABORT, 'record rules are immutable'); END",
    "CREATE TRIGGER record_rules_no_delete BEFORE DELETE ON record_rules
        BEGIN SELECT RAISE(ABORT, 'record rules are immutable'); END",
    "CREATE TABLE record_rule_observations (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        configured_rules TEXT NOT NULL, stored_rules TEXT NOT NULL,
        mismatch INTEGER NOT NULL CHECK (mismatch IN (0, 1)),
        at_ms INTEGER NOT NULL
    ) STRICT",
];

pub const MISMATCH: &str = "record_rules_mismatch: restore this authority's configured rules or switch to burn and open a fresh authority";

pub fn stored(sql: &impl Sql) -> Result<Option<RecordRules>, StoreError> {
    let rows = sql.exec(
        "SELECT reward_rule, quant_rule FROM record_rules WHERE id = 1",
        &[],
    )?;
    rows.first()
        .map(|row| {
            let name = format!("{},{}", text(row, 0)?, text(row, 1)?);
            RecordRules::parse(Some(&name)).map_err(|_| StoreError("invalid stored record rules"))
        })
        .transpose()
}

/// Call within a storage transaction. Read-only audits never initialize a row
/// or append an observation. A retired legacy authority with no row retains
/// the legacy pair, independently of the deployment's current authority.
pub fn configure(
    sql: &impl Sql,
    authority: &Authority,
    now_ms: u64,
    persist: bool,
) -> Result<Authority, StoreError> {
    let recorded = stored(sql)?;
    let rules = recorded.unwrap_or_else(|| {
        if authority.is_retired() {
            RecordRules::default()
        } else {
            authority.configured_rules()
        }
    });
    if persist && !authority.is_retired() {
        if recorded.is_none() {
            sql.exec(
                "INSERT INTO record_rules (id, reward_rule, quant_rule, set_ms) VALUES (1, ?, ?, ?)",
                &[rules.reward.name().into(), rules.quant.name().into(), bind_u64(now_ms)?],
            )?;
        }
        let configured = authority.configured_rules().name();
        let stored_name = rules.name();
        let previous = sql.exec(
            "SELECT configured_rules, stored_rules FROM record_rule_observations ORDER BY seq DESC LIMIT 1",
            &[],
        )?;
        let unchanged = previous
            .first()
            .map(|row| {
                Ok::<_, StoreError>(text(row, 0)? == configured && text(row, 1)? == stored_name)
            })
            .transpose()?
            .unwrap_or(false);
        if !unchanged {
            sql.exec(
                "INSERT INTO record_rule_observations (configured_rules, stored_rules, mismatch, at_ms) VALUES (?, ?, ?, ?)",
                &[
                    configured.into(), stored_name.into(),
                    SqlValue::Integer(i64::from(authority.configured_rules() != rules)), bind_u64(now_ms)?,
                ],
            )?;
        }
    }
    Ok(authority.clone().with_persisted_rules(rules))
}

/// Admission rechecks the row in the transaction that writes the new plan.
/// A refusal rolls back any initialization in that transaction.
pub fn require(sql: &impl Sql, authority: &Authority, now_ms: u64) -> super::engine::R<()> {
    if configure(sql, authority, now_ms, true)?.rules_mismatch() {
        return Err(super::engine::EvalError::Plan(MISMATCH));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::{EVAL_COHORT_SCHEMA, MigrationError, Schema, int, migrate};
    use crate::testing::{Sqlite, tx};

    fn db() -> Sqlite {
        let db = Sqlite::open();
        migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
        db
    }

    fn successor() -> Authority {
        Authority::named("successor", 50)
            .unwrap()
            .with_rules(RecordRules::parse(Some("reward-v3,quant-v2")).unwrap())
    }

    #[test]
    fn configuration_drift_preserves_rules_and_records_recovery_once() {
        let db = db();
        let original = Authority::default();
        tx(&db, |sql| configure(sql, &original, 10, true)).unwrap();
        let changed = original.clone().with_rules(successor().rules());
        for now in [20, 30] {
            let bound = tx(&db, |sql| configure(sql, &changed, now, true)).unwrap();
            assert!(bound.rules_mismatch());
            assert_eq!(bound.rules(), original.rules());
            assert_eq!(bound.configured_rules(), changed.rules());
            assert_eq!(bound.clone().with_persisted_rules(original.rules()), bound);
        }
        let restored = tx(&db, |sql| configure(sql, &original, 40, true)).unwrap();
        assert!(!restored.rules_mismatch());
        let rows = db
            .exec(
                "SELECT mismatch, at_ms FROM record_rule_observations ORDER BY seq",
                &[],
            )
            .unwrap();
        assert_eq!(
            rows,
            vec![
                vec![0i64.into(), 10i64.into()],
                vec![1i64.into(), 20i64.into()],
                vec![0i64.into(), 40i64.into()]
            ]
        );
        for statement in [
            "UPDATE record_rules SET reward_rule = 'reward-v3'",
            "DELETE FROM record_rules",
            "INSERT OR REPLACE INTO record_rules VALUES (1, 'reward-v3', 'quant-v2', 50)",
        ] {
            assert!(tx(&db, |sql| sql.exec(statement, &[])).is_err());
        }
        assert_eq!(stored(&db).unwrap(), Some(original.rules()));
    }

    #[test]
    fn retired_authorities_recover_their_own_rules() {
        let db = db();
        let configured = successor();
        tx(&db, |sql| configure(sql, &configured, 10, true)).unwrap();
        let retired = Authority::retired(configured.name());
        let bound = tx(&db, |sql| configure(sql, &retired, 20, true)).unwrap();
        assert!(bound.is_retired());
        assert!(!bound.rules_mismatch());
        assert_eq!(bound.rules(), configured.rules());
        assert_eq!(
            int(
                &db.exec("SELECT COUNT(*) FROM record_rule_observations", &[])
                    .unwrap()[0],
                0
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn read_only_audits_and_legacy_retirement_do_not_initialize_rules() {
        let db = db();
        let configured = successor();
        assert_eq!(
            configure(&db, &configured, 10, false).unwrap().rules(),
            configured.rules()
        );
        assert_eq!(
            configure(&db, &Authority::retired("legacy"), 20, true)
                .unwrap()
                .rules(),
            RecordRules::default()
        );
        assert_eq!(stored(&db).unwrap(), None);
        assert!(
            db.exec("SELECT seq FROM record_rule_observations", &[])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn migration_preserves_the_legacy_store_and_refuses_downgrade() {
        let db = Sqlite::open();
        let legacy = Schema {
            family: EVAL_COHORT_SCHEMA.family,
            migrations: &EVAL_COHORT_SCHEMA.migrations[..31],
        };
        migrate(&db, &legacy).unwrap();
        let rules_schema = Schema {
            family: EVAL_COHORT_SCHEMA.family,
            migrations: &EVAL_COHORT_SCHEMA.migrations[..32],
        };
        let upgraded = migrate(&db, &rules_schema).unwrap();
        assert_eq!((upgraded.from, upgraded.to), (31, 32));
        assert_eq!(stored(&db).unwrap(), None);
        tx(&db, |sql| configure(sql, &Authority::default(), 10, true)).unwrap();
        assert!(matches!(
            migrate(&db, &legacy),
            Err(MigrationError::NewerStoredVersion {
                stored: 32,
                supported: 31
            })
        ));
        // Existing rule bindings survive subsequent migrations.
        let upgraded = migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
        assert_eq!((upgraded.from, upgraded.to), (32, EVAL_COHORT_SCHEMA.latest()));
        assert_eq!(stored(&db).unwrap(), Some(RecordRules::default()));
    }

    #[test]
    fn malformed_persisted_rules_never_become_legacy_defaults() {
        let db = db();
        db.exec(
            "INSERT INTO record_rules VALUES (1, 'unknown', 'quant-v1', 10)",
            &[],
        )
        .unwrap();
        assert!(configure(&db, &Authority::default(), 20, true).is_err());
        assert!(configure(&db, &Authority::retired("legacy"), 20, false).is_err());
    }
}
