//! Chunk import for an isolated synthetic package qualification database.
//!
//! The caller authenticates the fixture manifest, uses a fresh isolated store,
//! and wraps each call in a SQL transaction. This module neither clears a
//! database nor accepts SQL from a fixture. Identical chunks can be replayed
//! after an uncertain response; conflicting rows are never replaced.
use crate::sql::{self, Sql, SqlValue, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    pub file: String,
    pub sha256: String,
    pub bytes: usize,
    pub offset: u64,
    pub rows: u64,
    pub after_rowid: i64,
    pub last_rowid: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    pub table: String,
    pub columns: Vec<String>,
    pub rowid_included: bool,
    pub rows: u64,
    pub file: String,
    pub sha256: String,
    pub chunks: Vec<Chunk>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_index: Option<super::package_fixture_metadata::List>,
}

fn refuse(condition: bool, message: &'static str) -> Result<(), StoreError> {
    if condition {
        Ok(())
    } else {
        Err(StoreError(message))
    }
}

/// Read-only reconciliation after a possibly lost package response. This never
/// creates a package or repairs state: all expected output objects must also
/// be independently read back before the caller declares a verified run.
pub fn verify_package_commit(db: &impl Sql, seq: i64, expected: &Value) -> Result<(), StoreError> {
    let rows = db.exec(
        "SELECT phase, package_digest, target_digest, receipt_root, receipt_count FROM cohorts WHERE seq = ?",
        &[seq.into()],
    )?;
    let row = rows.first().ok_or(StoreError("fixture cohort absent"))?;
    refuse(rows.len() == 1 && sql::text(row, 0)? == "transition", "fixture package not committed")?;
    for (column, field) in [(1, "package_digest"), (3, "receipt_root")] {
        let want = expected[field].as_str().ok_or(StoreError("fixture expected digest"))?;
        refuse(sql::text(row, column)? == want, "fixture committed digest differs")?;
    }
    let target = match row.get(2) {
        Some(SqlValue::Null) => Value::Null,
        Some(SqlValue::Text(s)) => Value::String(s.clone()),
        _ => return Err(StoreError("fixture target type")),
    };
    refuse(expected.get("target_digest") == Some(&target), "fixture committed target differs")?;
    refuse(expected["receipts_verified"].as_u64() == Some(sql::uint(row, 4)?), "fixture receipt count differs")?;
    let state = super::v3::state(db, seq).map_err(|_| StoreError("fixture package state"))?;
    refuse(state.result.map(|v| v.0.to_string()).as_deref() == expected["result_digest"].as_str()
        && expected["result_digest"].is_string(), "fixture committed result differs")
}

fn identifier(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn value(v: Value) -> Result<SqlValue, StoreError> {
    match v {
        Value::Null => Ok(SqlValue::Null),
        Value::String(s) => Ok(s.into()),
        Value::Number(n) => {
            let n = n.as_i64().ok_or(StoreError("fixture non-integer value"))?;
            refuse(
                n.unsigned_abs() <= crate::wire::MAX_SAFE_INTEGER,
                "fixture integer exceeds JS precision",
            )?;
            Ok(n.into())
        }
        _ => Err(StoreError("fixture SQL value type")),
    }
}

/// Validate descriptors against the actual migrated schema before constructing
/// any SQL. Only ordinary tables with exactly the declared columns are accepted.
pub fn check_table(db: &impl Sql, table: &Table) -> Result<(), StoreError> {
    refuse(
        identifier(&table.table) && table.rowid_included,
        "fixture table identity",
    )?;
    refuse(
        !table.table.starts_with("sqlite_") && !table.table.eq_ignore_ascii_case("_cf_KV"),
        "fixture internal table",
    )?;
    let names = db.exec(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
        &[table.table.as_str().into()],
    )?;
    refuse(names.len() == 1, "fixture unknown table")?;
    let info = db.exec(&format!("PRAGMA table_info(\"{}\")", table.table), &[])?;
    let columns: Vec<_> = info
        .iter()
        .map(|r| sql::text(r, 1).map(str::to_owned))
        .collect::<Result<_, _>>()?;
    refuse(
        !columns.is_empty() && columns == table.columns && columns.iter().all(|c| identifier(c)),
        "fixture column schema",
    )?;
    if let Some(index) = &table.chunk_index {
        return refuse(
            table.chunks.is_empty() && index.valid() && (index.count == 0) == (table.rows == 0)
                && index.count as u64 <= table.rows,
            "fixture paged table descriptors",
        );
    }
    let mut offset = 0_u64;
    let mut after = 0_i64;
    for chunk in &table.chunks {
        refuse(
            chunk.rows > 0
                && chunk.offset == offset
                && chunk.after_rowid == after
                && chunk.last_rowid > after,
            "fixture chunk sequence",
        )?;
        offset = offset
            .checked_add(chunk.rows)
            .ok_or(StoreError("fixture row overflow"))?;
        after = chunk.last_rowid;
    }
    refuse(offset == table.rows, "fixture table count")
}

/// Exact table roster, not just a subset of tables convenient to the fixture.
pub fn check_roster(db: &impl Sql, tables: &[Table]) -> Result<(), StoreError> {
    // workerd stores Durable Object KV in this platform-owned table.
    let actual = db.exec(
        "SELECT name FROM sqlite_master WHERE type = 'table'
         AND name NOT GLOB 'sqlite_*' AND name != '_cf_KV' ORDER BY name",
        &[],
    )?;
    let actual: Vec<_> = actual
        .iter()
        .map(|r| sql::text(r, 0))
        .collect::<Result<_, _>>()?;
    refuse(
        actual == tables.iter().map(|t| t.table.as_str()).collect::<Vec<_>>(),
        "fixture table roster",
    )?;
    for table in tables {
        check_table(db, table)?;
    }
    Ok(())
}

fn canonical_row(row: Vec<SqlValue>) -> Result<Vec<u8>, StoreError> {
    let values: Vec<Value> = row.into_iter().map(|v| match v {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(n) => Value::from(n),
        SqlValue::Text(s) => Value::from(s),
    }).collect();
    let mut bytes = serde_json::to_vec(&values).map_err(|_| StoreError("fixture row encoding"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(feature = "package-qualification")]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TableReadback {
    after_rowid: i64,
    rows: u64,
    // Synthetic SQL digest state, including its last partial SHA-256 block.
    sha256_state: Vec<u8>,
}

/// Hash one page of stored rows. Persist the returned cursor before continuing.
/// None means every row, the count and the whole-table digest have verified.
#[cfg(feature = "package-qualification")]
pub fn verify_table_page(
    db: &impl Sql,
    table: &Table,
    cursor: Option<TableReadback>,
) -> Result<Option<TableReadback>, StoreError> {
    use fixture_sha2::{Digest as _, Sha256 as ResumableSha256};
    use fixture_sha2::digest::common::hazmat::{SerializableState, SerializedState};

    check_table(db, table)?;
    let (mut digest, mut after, mut count) = match cursor {
        Some(c) => {
            refuse(c.after_rowid > 0 && c.rows > 0 && c.rows <= table.rows, "fixture readback cursor")?;
            let state = SerializedState::<ResumableSha256>::slice_as_array(&c.sha256_state)
                .ok_or(StoreError("fixture readback state length"))?;
            let digest = ResumableSha256::deserialize(state).map_err(|_| StoreError("fixture readback state"))?;
            (digest, c.after_rowid, c.rows)
        }
        None => {
            let invalid = db.exec(&format!("SELECT rowid FROM \"{}\" WHERE rowid <= 0 LIMIT 1", table.table), &[])?;
            refuse(invalid.is_empty(), "fixture nonpositive row ID")?;
            (ResumableSha256::new(), 0, 0)
        }
    };
    let page = db.exec(
        &format!("SELECT rowid, * FROM \"{}\" WHERE rowid > ? ORDER BY rowid LIMIT 256", table.table),
        &[after.into()],
    )?;
    let more = page.len() == 256;
    for row in page {
        after = sql::int(&row, 0)?;
        count = count.checked_add(1).ok_or(StoreError("fixture readback count"))?;
        refuse(count <= table.rows, "fixture table count")?;
        digest.update(canonical_row(row)?);
    }
    if more {
        Ok(Some(TableReadback { after_rowid: after, rows: count, sha256_state: digest.serialize().to_vec() }))
    } else {
        refuse(count == table.rows && hex::encode(digest.finalize()) == table.sha256, "fixture table digest")?;
        Ok(None)
    }
}

/// Hash the canonical exported rows in bounded pages. This verifies the final
/// imported table, including implicit row IDs and empty tables.
pub fn verify_table(db: &impl Sql, table: &Table) -> Result<(), StoreError> {
    check_table(db, table)?;
    let mut digest = Sha256::new();
    let mut after = 0_i64;
    let mut count = 0_u64;
    loop {
        let rows = db.exec(
            &format!(
                "SELECT rowid, * FROM \"{}\" WHERE rowid > ? ORDER BY rowid LIMIT 256",
                table.table
            ),
            &[after.into()],
        )?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            after = sql::int(&row, 0)?;
            digest.update(canonical_row(row)?);
            count += 1;
        }
    }
    let all = db.exec(&format!("SELECT COUNT(*) FROM \"{}\"", table.table), &[])?;
    let all = sql::uint(all.first().ok_or(StoreError("fixture count missing"))?, 0)?;
    refuse(
        all == count && count == table.rows && hex::encode(digest.finalize()) == table.sha256,
        "fixture table digest",
    )
}

/// Only a byte-matching freshly migrated baseline can be cleared for import.
/// All checks precede the first DELETE; the caller still uses one transaction.
pub fn clear_pristine(
    db: &impl Sql,
    baseline: &[Table],
    incoming: &[Table],
) -> Result<(), StoreError> {
    check_roster(db, baseline)?;
    check_roster(db, incoming)?;
    for table in baseline {
        verify_table(db, table)?;
    }
    // Keep the migration marker across DO eviction during chunked import.
    // The fixture must use the exact same current schema marker.
    for table in baseline {
        if table.table == "schema_meta" {
            let target = incoming
                .iter()
                .find(|t| t.table == "schema_meta")
                .ok_or(StoreError("fixture schema marker"))?;
            refuse(
                target.sha256 == table.sha256 && target.rows == table.rows,
                "fixture schema marker differs",
            )?;
        }
    }
    for table in baseline {
        if table.table != "schema_meta" {
            db.exec(&format!("DELETE FROM \"{}\"", table.table), &[])?;
        }
    }
    Ok(())
}

/// Insert a manifest-bound chunk in order, or verify an identical replay. The
/// row count and preceding row ID form the cursor; a missing chunk cannot be
/// skipped. Caller transactions roll back a whole chunk on any refusal.
pub fn import_chunk(
    db: &impl Sql,
    table: &Table,
    part: usize,
    bytes: &[u8],
) -> Result<i64, StoreError> {
    let chunk = table
        .chunks
        .get(part)
        .ok_or(StoreError("fixture chunk index"))?;
    import_descriptor(db, table, chunk, bytes)
}

/// The caller authenticates a paged descriptor and its position against the
/// manifest before use. SQL checks also bind its offset and row IDs to the
/// rows already imported. Inline callers resolve the descriptor above.
pub fn import_descriptor(
    db: &impl Sql,
    table: &Table,
    chunk: &Chunk,
    bytes: &[u8],
) -> Result<i64, StoreError> {
    check_table(db, table)?;
    refuse(
        chunk.rows > 0 && chunk.last_rowid > chunk.after_rowid
            && chunk.after_rowid >= 0 && (chunk.offset == 0) == (chunk.after_rowid == 0)
            && chunk.offset.checked_add(chunk.rows).is_some_and(|end| end <= table.rows),
        "fixture descriptor bounds",
    )?;
    refuse(
        bytes.len() == chunk.bytes && hex::encode(Sha256::digest(bytes)) == chunk.sha256,
        "fixture chunk digest",
    )?;
    let text = std::str::from_utf8(bytes).map_err(|_| StoreError("fixture UTF-8"))?;
    refuse(text.ends_with('\n'), "fixture chunk terminator")?;
    let rows: Vec<Vec<SqlValue>> = text
        .lines()
        .map(|line| {
            let values: Vec<Value> =
                serde_json::from_str(line).map_err(|_| StoreError("fixture row JSON"))?;
            refuse(values.len() == table.columns.len() + 1, "fixture row width")?;
            values.into_iter().map(value).collect()
        })
        .collect::<Result<_, _>>()?;
    refuse(rows.len() as u64 == chunk.rows, "fixture chunk row count")?;
    let mut after = chunk.after_rowid;
    for row in &rows {
        let rowid = sql::int(row, 0)?;
        refuse(rowid > after, "fixture row-ID order")?;
        after = rowid;
    }
    refuse(after == chunk.last_rowid, "fixture final row ID")?;
    let count = db.exec(&format!("SELECT COUNT(*) FROM \"{}\"", table.table), &[])?;
    let count = sql::uint(count.first().ok_or(StoreError("fixture count missing"))?, 0)?;
    let end = chunk
        .offset
        .checked_add(chunk.rows)
        .ok_or(StoreError("fixture row overflow"))?;
    refuse(
        count == chunk.offset || count >= end,
        "fixture incomplete prefix",
    )?;
    if chunk.offset > 0 {
        let offset = sql::bind_u64(chunk.offset - 1)?;
        let previous = db.exec(
            &format!(
                "SELECT rowid FROM \"{}\" ORDER BY rowid LIMIT 1 OFFSET ?",
                table.table
            ),
            &[offset],
        )?;
        refuse(
            sql::int(
                previous
                    .first()
                    .ok_or(StoreError("fixture missing prefix"))?,
                0,
            )? == chunk.after_rowid,
            "fixture prefix row ID",
        )?;
    }
    if count == chunk.offset {
        let columns = std::iter::once("rowid".to_owned())
            .chain(table.columns.iter().map(|c| format!("\"{c}\"")))
            .collect::<Vec<_>>()
            .join(",");
        let slots = vec!["?"; table.columns.len() + 1].join(",");
        let insert = format!(
            "INSERT INTO \"{}\" ({columns}) VALUES ({slots})",
            table.table
        );
        for row in &rows {
            db.exec(&insert, row)?;
        }
    }
    let actual = db.exec(
        &format!(
            "SELECT rowid, * FROM \"{}\" ORDER BY rowid LIMIT ? OFFSET ?",
            table.table
        ),
        &[sql::bind_u64(chunk.rows)?, sql::bind_u64(chunk.offset)?],
    )?;
    refuse(actual == rows, "fixture stored rows differ")?;
    Ok(after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Sqlite, tx};

    fn chunk(part: u64, after: i64, last: i64, bytes: &[u8]) -> Chunk {
        Chunk {
            file: format!("{part}.jsonl"),
            sha256: hex::encode(Sha256::digest(bytes)),
            bytes: bytes.len(),
            offset: part,
            rows: 1,
            after_rowid: after,
            last_rowid: last,
        }
    }
    fn fixture() -> (Sqlite, Table, [Vec<u8>; 2]) {
        let db = Sqlite::open();
        db.exec(
            "CREATE TABLE sample (name TEXT UNIQUE, n INTEGER) STRICT",
            &[],
        )
        .unwrap();
        let bytes = [b"[7,\"a\",1]\n".to_vec(), b"[10000009,\"b\",2]\n".to_vec()];
        let t = Table {
            table: "sample".into(),
            columns: vec!["name".into(), "n".into()],
            rowid_included: true,
            rows: 2,
            file: "sample.jsonl".into(),
            sha256: String::new(),
            chunk_index: None,
            chunks: vec![chunk(0, 0, 7, &bytes[0]), chunk(1, 7, 10000009, &bytes[1])],
        };
        (db, t, bytes)
    }
    #[cfg(feature = "package-qualification")]
    #[test]
    fn table_readback_resumes_exact_hash_and_checks_tail() {
        let (db, mut table, _) = fixture();
        let mut expected = Sha256::new();
        for n in 1_i64..=512 {
            let name = format!("value-{n}");
            db.exec("INSERT INTO sample(rowid,name,n) VALUES (?,?,?)",
                &[(n*3).into(), name.clone().into(), n.into()]).unwrap();
            let mut row = serde_json::to_vec(&serde_json::json!([n*3,name,n])).unwrap();
            row.push(b'\n');
            expected.update(row);
        }
        table.rows = 512;
        table.chunks.truncate(1);
        table.chunks[0].rows = 512;
        table.chunks[0].last_rowid = 1536;
        table.sha256 = hex::encode(expected.finalize());
        verify_table(&db, &table).unwrap();
        let first = verify_table_page(&db, &table, None).unwrap().unwrap();
        assert_eq!(first.rows, 256);
        let saved = serde_json::to_vec(&first).unwrap();
        let resume = || serde_json::from_slice(&saved).unwrap();
        db.exec("UPDATE sample SET n=NULL WHERE rowid=1536", &[]).unwrap();
        let changed = verify_table_page(&db, &table, Some(resume())).unwrap().unwrap();
        assert!(verify_table_page(&db, &table, Some(changed)).is_err());
        db.exec("UPDATE sample SET n=512 WHERE rowid=1536", &[]).unwrap();
        let last = verify_table_page(&db, &table, Some(resume())).unwrap().unwrap();
        assert_eq!(last.rows, 512);
        assert!(verify_table_page(&db, &table, Some(last.clone())).unwrap().is_none());
        db.exec("INSERT INTO sample(rowid) VALUES(1537)", &[]).unwrap();
        assert!(verify_table_page(&db, &table, Some(last)).is_err());
        db.exec("INSERT INTO sample(rowid) VALUES(-1)", &[]).unwrap();
        assert!(verify_table_page(&db, &table, None).is_err());
    }

    #[test]
    fn ordered_chunks_resume_and_preserve_rowid_gaps() {
        let (db, t, b) = fixture();
        assert!(tx(&db, |s| import_chunk(s, &t, 1, &b[1])).is_err());
        for i in [0, 0, 1, 0, 1] {
            tx(&db, |s| import_chunk(s, &t, i, &b[i])).unwrap();
        }
        assert_eq!(db.count("sample"), 2);
        db.exec("UPDATE sample SET n=3 WHERE name='a'", &[])
            .unwrap();
        assert!(tx(&db, |s| import_chunk(s, &t, 0, &b[0])).is_err());
    }
    #[test]
    fn paged_descriptors_restore_the_same_rows_and_match_the_inline_digest() {
        use super::super::package_fixture_metadata::{self as metadata, Read};
        let (db, mut table, bodies) = fixture();
        table.sha256 = hex::encode(Sha256::digest(bodies.concat()));
        let descriptors: Vec<_> = table.chunks.drain(..).map(|c| serde_json::to_value(c).unwrap()).collect();
        let mut pages = std::collections::BTreeMap::new();
        let list = metadata::write(&descriptors, &mut |k, v| { pages.insert(k.to_owned(), v.to_vec()); });
        table.chunk_index = Some(list.clone());
        for part in [1, 0, 0, 1] {
            let cursor = list.lookup(part).unwrap();
            let bytes = &pages[&metadata::key(&cursor.digest)];
            let Read::Item(value) = cursor.read(bytes).unwrap() else { panic!("expected leaf"); };
            let descriptor: Chunk = serde_json::from_value(value).unwrap();
            let result = tx(&db, |db| import_descriptor(db, &table, &descriptor, &bodies[part]));
            if part == 1 && db.count("sample") == 0 {
                assert!(result.is_err());
            } else {
                result.unwrap();
            }
        }
        verify_table(&db, &table).unwrap();
    }

    #[test]
    fn changed_bytes_schema_and_unsafe_integers_refuse() {
        let (db, mut t, b) = fixture();
        assert!(tx(&db, |s| import_chunk(s, &t, 0, b"[7,\"a\",9]\n")).is_err());
        t.columns.swap(0, 1);
        assert!(check_table(&db, &t).is_err());
        t.table = "sample\"; DROP TABLE sample; --".into();
        assert!(check_table(&db, &t).is_err());
        assert!(value(serde_json::json!(9007199254740992_i64)).is_err());
        assert!(value(serde_json::json!(1.5)).is_err());
        assert!(value(serde_json::json!(true)).is_err());
        assert_eq!(db.count("sample"), 0);
        assert!(!b[0].is_empty());
    }
    #[test]
    fn conflicting_second_row_rolls_back_whole_chunk() {
        let (db, mut t, _) = fixture();
        let b = b"[7,\"same\",1]\n[8,\"same\",2]\n";
        t.chunks = vec![Chunk {
            rows: 2,
            ..chunk(0, 0, 8, b)
        }];
        assert!(tx(&db, |s| import_chunk(s, &t, 0, b)).is_err());
        assert_eq!(db.count("sample"), 0);
    }

    #[test]
    fn fixture_import_preserves_workerd_kv_and_checks_application_roster() {
        let (db, incoming, bytes) = fixture();
        db.exec("CREATE TABLE _cf_KV (name TEXT UNIQUE, n INTEGER)", &[]).unwrap();
        db.exec("INSERT INTO _cf_KV VALUES ('progress', 42)", &[]).unwrap();
        let mut reserved = incoming.clone();
        reserved.table = "_cf_KV".into();
        assert!(check_table(&db, &reserved).is_err());
        assert!(import_chunk(&db, &reserved, 0, &bytes[0]).is_err());

        let mut baseline = incoming.clone();
        baseline.rows = 0;
        baseline.chunks.clear();
        baseline.sha256 = hex::encode(Sha256::digest(b""));
        let clear = || tx(&db, |s| clear_pristine(
            s, std::slice::from_ref(&baseline), std::slice::from_ref(&incoming),
        ));
        db.exec("CREATE TABLE sqliteXapplication (n INTEGER)", &[]).unwrap();
        assert!(clear().is_err());
        db.exec("DROP TABLE sqliteXapplication", &[]).unwrap();
        clear().unwrap();
        tx(&db, |s| import_chunk(s, &incoming, 0, &bytes[0])).unwrap();
        assert_eq!(db.exec("SELECT name,n FROM _cf_KV", &[]).unwrap(),
                   vec![vec![SqlValue::from("progress"), SqlValue::from(42_i64)]]);
    }

    #[test]
    fn pristine_check_refuses_existing_data_and_missing_tables() {
        let (db, incoming, _) = fixture();
        let mut baseline = incoming.clone();
        baseline.rows = 0;
        baseline.chunks.clear();
        baseline.sha256 = hex::encode(Sha256::digest(b""));
        assert!(check_roster(&db, &[]).is_err());
        tx(&db, |s| {
            clear_pristine(
                s,
                std::slice::from_ref(&baseline),
                std::slice::from_ref(&incoming),
            )
        })
        .unwrap();
        db.exec("INSERT INTO sample VALUES ('existing', 5)", &[])
            .unwrap();
        assert!(
            tx(&db, |s| clear_pristine(
                s,
                std::slice::from_ref(&baseline),
                std::slice::from_ref(&incoming)
            ))
            .is_err()
        );
        assert_eq!(db.count("sample"), 1);
        db.exec("UPDATE sample SET rowid=-1", &[]).unwrap();
        assert!(verify_table(&db, &baseline).is_err());
        assert_eq!(db.count("sample"), 1);
    }
}
