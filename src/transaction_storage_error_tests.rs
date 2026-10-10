//! Transactions that fail in SQLite itself, part way through.
//!
//! On some errors (a full database among them) SQLite rolls back the whole
//! transaction, not just the statement that failed. A statement run after
//! that would commit on its own, and the transaction's own rollback would
//! then fail and leave it kept. The database here can't grow, so a write
//! that needs a new page fails with `SQLITE_FULL` while a small one still fits
//! the pages it has.

use super::*;
use serde_json::json;

fn request<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
    serde_json::from_value(value).unwrap()
}

/// A file-backed database holding table `Tbl` and its item `seed`, capped at
/// the pages it already has.
fn full_database(dir: &tempfile::TempDir) -> Database {
    let path = dir.path().join("full.db");
    let db = Database::new(path.to_str().unwrap()).unwrap();
    db.create_table(request(json!({
        "TableName": "Tbl",
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        "BillingMode": "PAY_PER_REQUEST"
    })))
    .unwrap();
    db.put_item(request(
        json!({"TableName": "Tbl", "Item": {"pk": {"S": "seed"}}}),
    ))
    .unwrap();
    db.with_storage(|s| {
        let pages: i64 = s
            .conn()
            .query_row("PRAGMA page_count", [], |row| row.get(0))?;
        s.conn().pragma_update(None, "max_page_count", pages)?;
        Ok(())
    })
    .unwrap();
    db
}

/// A value too big for the pages the database has left.
fn too_big() -> String {
    "x".repeat(200_000)
}

/// Every key in `Tbl`, sorted.
fn keys(db: &Database) -> Vec<String> {
    let mut keys: Vec<String> = db
        .scan(request(json!({"TableName": "Tbl"})))
        .unwrap()
        .items
        .unwrap_or_default()
        .iter()
        .map(|item| match item.get("pk") {
            Some(AttributeValue::S(pk)) => pk.clone(),
            other => panic!("unexpected key {other:?}"),
        })
        .collect();
    keys.sort();
    keys
}

/// The storage error as SQLite reported it. SQLite has already rolled the
/// transaction back, so the closing rollback finds none to end; that adds
/// nothing, and the caller sees the error that stopped the write alone.
fn assert_full(err: DynoxideError) {
    assert!(
        matches!(err, DynoxideError::InternalServerError(_)),
        "{err:?}"
    );
    assert_eq!(err.to_string(), "backend: database or disk is full");
}

/// The database still takes a small write, so nothing is left holding a
/// transaction open.
fn assert_still_writable(db: &Database) {
    db.put_item(request(
        json!({"TableName": "Tbl", "Item": {"pk": {"S": "later"}}}),
    ))
    .unwrap();
    assert_eq!(keys(db), ["later", "seed"]);
}

#[test]
fn a_full_database_fails_a_big_write_and_takes_a_small_one() {
    let dir = tempfile::tempdir().unwrap();
    let db = full_database(&dir);
    let err = db
        .put_item(request(json!({
            "TableName": "Tbl",
            "Item": {"pk": {"S": "big"}, "v": {"S": too_big()}}
        })))
        .unwrap_err();
    assert_full(err);
    assert_still_writable(&db);
}

#[test]
fn execute_transaction_stops_at_a_storage_error_and_keeps_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = full_database(&dir);
    let statement = |statement: &str, parameters: Option<Vec<AttributeValue>>| {
        actions::execute_transaction::ParameterizedStatement {
            statement: statement.to_string(),
            parameters,
            ..Default::default()
        }
    };
    let err = db
        .execute_transaction(actions::execute_transaction::ExecuteTransactionRequest {
            transact_statements: vec![
                statement("INSERT INTO \"Tbl\" VALUE {'pk': 'before'}", None),
                statement(
                    "INSERT INTO \"Tbl\" VALUE {'pk': 'big', 'v': ?}",
                    Some(vec![AttributeValue::S(too_big())]),
                ),
                statement("INSERT INTO \"Tbl\" VALUE {'pk': 'after'}", None),
            ],
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(keys(&db), ["seed"], "nothing the transaction wrote is kept");
    assert_full(err);
    assert_still_writable(&db);
}

#[test]
fn transact_write_items_stops_at_a_storage_error_and_keeps_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = full_database(&dir);
    let put = |item: serde_json::Value| json!({"Put": {"TableName": "Tbl", "Item": item}});
    let err = db
        .transact_write_items(request(json!({
            "TransactItems": [
                put(json!({"pk": {"S": "before"}})),
                put(json!({"pk": {"S": "big"}, "v": {"S": too_big()}})),
                put(json!({"pk": {"S": "after"}}))
            ]
        })))
        .unwrap_err();
    assert_eq!(keys(&db), ["seed"], "nothing the transaction wrote is kept");
    assert_full(err);
    assert_still_writable(&db);
}
