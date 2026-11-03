//! Derived joins, recomputed idempotently after ingestion.

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{Value, json};

/// Canonical split per text: the most restrictive exposure it ever had, over
/// every experiment's split membership, every record's recorded split and every
/// Pangram observation's split context (consumed_test > test_sealed > test >
/// cal > dev > held > train). A text whose source group ever touched an
/// evaluation role is evaluation-only as well (groups are matched by their
/// recorded key, never re-cut).
const SPLIT_STEPS: [(&str, &str); 4] = [
    ("text_split_roles", "DROP TABLE IF EXISTS text_split_role;
      CREATE TABLE text_split_role AS
      WITH raw(text_sha256, role, basis) AS (
        SELECT text_sha256, role, 'split_member:' || experiment FROM split_member WHERE text_sha256 IS NOT NULL
        UNION SELECT text_sha256, lower(split_hint), 'record:' || source_id FROM record
          WHERE text_sha256 IS NOT NULL AND split_hint IS NOT NULL AND lower(split_hint) NOT IN ('true', 'false')
        UNION SELECT o.text_sha256,
            CASE lower(COALESCE(json_extract(o.prediction_json, '$.context.split'), json_extract(src.spec_json, '$.params.split_default')))
              WHEN 'test' THEN 'test_consumed'
              ELSE lower(COALESCE(json_extract(o.prediction_json, '$.context.split'), json_extract(src.spec_json, '$.params.split_default'))) END,
            'pangram:' || o.context
          FROM observation o JOIN source src ON src.source_id = o.source_id
          WHERE o.detector = 'pangram' AND o.text_sha256 IS NOT NULL
            AND COALESCE(json_extract(o.prediction_json, '$.context.split'), json_extract(src.spec_json, '$.params.split_default')) IS NOT NULL
      )
      SELECT text_sha256, role, basis,
        CASE
          WHEN role = 'test_consumed' THEN 7
          WHEN role = 'test_sealed' THEN 6
          WHEN role IN ('test', 'heldout_test') THEN 5
          WHEN role IN ('cal', 'calibration') THEN 4
          WHEN role IN ('dev_iter', 'dev-iter', 'dev_select', 'dev-select', 'dev', 'development', 'reserved_unassigned', 'reference', 'references')
               OR role LIKE '%diagnostic%' THEN 3
          WHEN role = 'held' THEN 1
          WHEN role IN ('train', 'train_mining', 'train-mining') THEN 0
          ELSE NULL END AS rank
      FROM raw;
      CREATE INDEX text_split_role_text ON text_split_role(text_sha256)"),
    ("text_split", "DROP TABLE IF EXISTS text_split;
      CREATE TABLE text_split (text_sha256 TEXT PRIMARY KEY, canonical_split TEXT, rank INTEGER, roles TEXT,
        evaluation_only INTEGER NOT NULL, group_evaluation_only INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID;
      INSERT INTO text_split(text_sha256, canonical_split, rank, roles, evaluation_only)
      SELECT t.text_sha256,
        (SELECT r.role FROM text_split_role r WHERE r.text_sha256 = t.text_sha256 AND r.rank IS NOT NULL ORDER BY r.rank DESC LIMIT 1),
        (SELECT MAX(r.rank) FROM text_split_role r WHERE r.text_sha256 = t.text_sha256),
        (SELECT json_group_array(DISTINCT r.role) FROM text_split_role r WHERE r.text_sha256 = t.text_sha256),
        COALESCE((SELECT MAX(r.rank) FROM text_split_role r WHERE r.text_sha256 = t.text_sha256), 0) >= 3
      FROM text t"),
    ("evaluation_groups", "DROP TABLE IF EXISTS evaluation_group;
      CREATE TABLE evaluation_group AS SELECT DISTINCT r.source_group FROM record r JOIN text_split s ON s.text_sha256 = r.text_sha256
        WHERE s.evaluation_only = 1 AND r.source_group IS NOT NULL
        UNION SELECT DISTINCT source_group FROM split_member WHERE source_group IS NOT NULL
          AND role IN ('dev_iter', 'dev_select', 'cal', 'test', 'test_sealed', 'test_consumed', 'reserved_unassigned', 'calibration', 'development', 'dev');
      CREATE INDEX evaluation_group_key ON evaluation_group(source_group)"),
    ("group_evaluation_only", "UPDATE text_split SET group_evaluation_only = 1 WHERE text_sha256 IN (
        SELECT r.text_sha256 FROM record r JOIN evaluation_group g ON g.source_group = r.source_group WHERE r.text_sha256 IS NOT NULL)"),
];

pub fn run(db: &Connection) -> Result<Value> {
    let mut out = serde_json::Map::new();
    let steps: [(&str, &str); 4] = [
        // Frontier attempts and requests -> the assembled exported document.
        (
            "frontier_outputs",
            "UPDATE event SET output_text_sha256 = (
               SELECT r.text_sha256 FROM record r WHERE r.role = 'frontier_positive' AND r.record_id = event.request_id
               ORDER BY r.record_uid LIMIT 1)
             WHERE kind IN ('frontier_request', 'frontier_attempt') AND output_text_sha256 IS NULL
               AND (kind = 'frontier_request' OR status = 'accepted')
               AND EXISTS (SELECT 1 FROM record r WHERE r.role = 'frontier_positive' AND r.record_id = event.request_id)",
        ),
        // Local accepted generations -> the finalized positive whose requests list them
        // (through a one-off request map, not a correlated JSON scan).
        (
            "local_outputs",
            "DROP TABLE IF EXISTS temp.local_request_output;
             CREATE TEMP TABLE local_request_output (request_id TEXT PRIMARY KEY, text_sha256 TEXT NOT NULL) WITHOUT ROWID;
             INSERT OR IGNORE INTO local_request_output
               SELECT json_extract(j.value, '$.request_id'), r.text_sha256
               FROM record r, json_each(r.extra_json, '$.requests') j
               WHERE r.role = 'local_positive' AND r.text_sha256 IS NOT NULL AND json_extract(j.value, '$.request_id') IS NOT NULL
               ORDER BY r.record_uid;
             UPDATE event SET output_text_sha256 = (SELECT m.text_sha256 FROM local_request_output m WHERE m.request_id = event.request_id)
             WHERE kind = 'local_generate' AND status = 'accepted' AND output_text_sha256 IS NULL
               AND request_id IN (SELECT request_id FROM local_request_output)",
        ),
        // Parent text of positives, from any record of the parent id.
        (
            "parent_texts",
            "UPDATE record SET parent_text_sha256 = (
               SELECT p.text_sha256 FROM record p WHERE p.record_id = record.parent_record_id AND p.text_sha256 IS NOT NULL
               ORDER BY p.record_uid LIMIT 1)
             WHERE parent_record_id IS NOT NULL AND parent_text_sha256 IS NULL",
        ),
        // Observations that name an item but not a text, through any record of that id.
        (
            "observation_texts",
            "UPDATE observation SET text_sha256 = (
               SELECT r.text_sha256 FROM record r WHERE r.record_id = observation.item_id AND r.text_sha256 IS NOT NULL
               ORDER BY r.record_uid LIMIT 1)
             WHERE text_sha256 IS NULL AND item_id IS NOT NULL",
        ),
    ];
    for (name, sql) in steps.iter().copied().chain(SPLIT_STEPS.iter().copied()) {
        let t = std::time::Instant::now();
        let n = if sql.contains(';') {
            db.execute_batch(sql)?;
            db.changes() as usize
        } else {
            db.execute(sql, [])?
        };
        eprintln!("link {name}: {n} rows in {:.1}s", t.elapsed().as_secs_f64());
        out.insert(name.into(), json!(n));
    }
    Ok(Value::Object(out))
}
