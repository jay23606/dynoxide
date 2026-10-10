//! Tests for PartiQL IMPORTANT correctness fixes (I12-I22).

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::{ExecuteStatementRequest, ExecuteStatementResponse};
use dynoxide::actions::put_item::PutItemRequest;
use dynoxide::types::{
    AttributeDefinition, AttributeValue, KeySchemaElement, KeyType, ScalarAttributeType,
};
use std::collections::HashMap;

fn create_test_table(db: &Database, table_name: &str) {
    let request = CreateTableRequest {
        table_name: table_name.to_string(),
        key_schema: vec![KeySchemaElement {
            attribute_name: "pk".to_string(),
            key_type: KeyType::HASH,
        }],
        attribute_definitions: vec![AttributeDefinition {
            attribute_name: "pk".to_string(),
            attribute_type: ScalarAttributeType::S,
        }],
        ..Default::default()
    };
    db.create_table(request).unwrap();
}

fn create_composite_table(db: &Database, table_name: &str) {
    let request = CreateTableRequest {
        table_name: table_name.to_string(),
        key_schema: vec![
            KeySchemaElement {
                attribute_name: "pk".to_string(),
                key_type: KeyType::HASH,
            },
            KeySchemaElement {
                attribute_name: "sk".to_string(),
                key_type: KeyType::RANGE,
            },
        ],
        attribute_definitions: vec![
            AttributeDefinition {
                attribute_name: "pk".to_string(),
                attribute_type: ScalarAttributeType::S,
            },
            AttributeDefinition {
                attribute_name: "sk".to_string(),
                attribute_type: ScalarAttributeType::S,
            },
        ],
        ..Default::default()
    };
    db.create_table(request).unwrap();
}

fn exec(db: &Database, statement: &str) -> ExecuteStatementResponse {
    db.execute_statement(ExecuteStatementRequest {
        statement: statement.to_string(),
        parameters: None,
        ..Default::default()
    })
    .unwrap()
}

fn exec_with_limit(db: &Database, statement: &str, limit: usize) -> ExecuteStatementResponse {
    db.execute_statement(ExecuteStatementRequest {
        statement: statement.to_string(),
        parameters: None,
        limit: Some(limit),
        ..Default::default()
    })
    .unwrap()
}

fn exec_err(db: &Database, statement: &str) -> String {
    db.execute_statement(ExecuteStatementRequest {
        statement: statement.to_string(),
        parameters: None,
        ..Default::default()
    })
    .unwrap_err()
    .to_string()
}

fn put_item(db: &Database, table_name: &str, item: HashMap<String, AttributeValue>) {
    db.put_item(PutItemRequest {
        table_name: table_name.to_string(),
        item,
        ..Default::default()
    })
    .unwrap();
}

// ---------------------------------------------------------------------------
// I12: UPDATE REMOVE clause
// ---------------------------------------------------------------------------

#[test]
fn test_update_remove_clause() {
    let db = Database::memory().unwrap();
    create_test_table(&db, "Tbl");

    let mut item = HashMap::new();
    item.insert("pk".to_string(), AttributeValue::S("k1".to_string()));
    item.insert("name".to_string(), AttributeValue::S("Alice".to_string()));
    item.insert("age".to_string(), AttributeValue::N("30".to_string()));
    item.insert(
        "email".to_string(),
        AttributeValue::S("alice@example.com".to_string()),
    );
    put_item(&db, "Tbl", item);

    // Remove age and email attributes
    exec(
        &db,
        "UPDATE \"Tbl\" SET name = 'Alicia' REMOVE age, email WHERE pk = 'k1'",
    );

    let sel = exec(&db, "SELECT * FROM \"Tbl\" WHERE pk = 'k1'");
    let items = sel.items.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].get("name"),
        Some(&AttributeValue::S("Alicia".to_string()))
    );
    assert!(!items[0].contains_key("age"), "age should be removed");
    assert!(!items[0].contains_key("email"), "email should be removed");
}

#[test]
fn test_update_remove_only() {
    let db = Database::memory().unwrap();
    create_test_table(&db, "Tbl");

    let mut item = HashMap::new();
    item.insert("pk".to_string(), AttributeValue::S("k1".to_string()));
    item.insert("temp".to_string(), AttributeValue::S("data".to_string()));
    put_item(&db, "Tbl", item);

    exec(&db, "UPDATE \"Tbl\" REMOVE temp WHERE pk = 'k1'");

    let sel = exec(&db, "SELECT * FROM \"Tbl\" WHERE pk = 'k1'");
    let items = sel.items.unwrap();
    assert_eq!(items.len(), 1);
    assert!(!items[0].contains_key("temp"), "temp should be removed");
}

// ---------------------------------------------------------------------------
// I13: SET with nested paths
// ---------------------------------------------------------------------------

#[test]
fn test_set_nested_path() {
    let db = Database::memory().unwrap();
    create_test_table(&db, "Tbl");

    let mut address = HashMap::new();
    address.insert("city".to_string(), AttributeValue::S("London".to_string()));
    address.insert(
        "postcode".to_string(),
        AttributeValue::S("SW1A".to_string()),
    );

    let mut item = HashMap::new();
    item.insert("pk".to_string(), AttributeValue::S("k1".to_string()));
    item.insert("address".to_string(), AttributeValue::M(address));
    put_item(&db, "Tbl", item);

    exec(
        &db,
        "UPDATE \"Tbl\" SET address.city = 'Manchester' WHERE pk = 'k1'",
    );

    let sel = exec(&db, "SELECT * FROM \"Tbl\" WHERE pk = 'k1'");
    let items = sel.items.unwrap();
    let addr = match items[0].get("address") {
        Some(AttributeValue::M(m)) => m,
        other => panic!("Expected map, got {other:?}"),
    };
    assert_eq!(
        addr.get("city"),
        Some(&AttributeValue::S("Manchester".to_string()))
    );
    // Postcode should be untouched
    assert_eq!(
        addr.get("postcode"),
        Some(&AttributeValue::S("SW1A".to_string()))
    );
}

// ---------------------------------------------------------------------------
// I14: SET expressions (count + 1, count - 1)
// ---------------------------------------------------------------------------

#[test]
fn test_set_expression_add() {
    let db = Database::memory().unwrap();
    create_test_table(&db, "Tbl");

    let mut item = HashMap::new();
    item.insert("pk".to_string(), AttributeValue::S("k1".to_string()));
    item.insert("count".to_string(), AttributeValue::N("10".to_string()));
    put_item(&db, "Tbl", item);

    exec(&db, "UPDATE \"Tbl\" SET count = count + 1 WHERE pk = 'k1'");

    let sel = exec(&db, "SELECT * FROM \"Tbl\" WHERE pk = 'k1'");
    let items = sel.items.unwrap();
    assert_eq!(
        items[0].get("count"),
        Some(&AttributeValue::N("11".to_string()))
    );
}

#[test]
fn test_set_expression_subtract() {
    let db = Database::memory().unwrap();
    create_test_table(&db, "Tbl");

    let mut item = HashMap::new();
    item.insert("pk".to_string(), AttributeValue::S("k1".to_string()));
    item.insert("count".to_string(), AttributeValue::N("10".to_string()));
    put_item(&db, "Tbl", item);

    exec(&db, "UPDATE \"Tbl\" SET count = count - 3 WHERE pk = 'k1'");

    let sel = exec(&db, "SELECT * FROM \"Tbl\" WHERE pk = 'k1'");
    let items = sel.items.unwrap();
    assert_eq!(
        items[0].get("count"),
        Some(&AttributeValue::N("7".to_string()))
    );
}

// ---------------------------------------------------------------------------
// I15: DELETE with missing sort key
// ---------------------------------------------------------------------------

#[test]
fn test_delete_missing_sort_key_error() {
    let db = Database::memory().unwrap();
    create_composite_table(&db, "CTbl");

    let mut item = HashMap::new();
    item.insert("pk".to_string(), AttributeValue::S("p1".to_string()));
    item.insert("sk".to_string(), AttributeValue::S("s1".to_string()));
    item.insert("data".to_string(), AttributeValue::S("hello".to_string()));
    put_item(&db, "CTbl", item);

    let err = exec_err(&db, "DELETE FROM \"CTbl\" WHERE pk = 'p1'");
    assert!(
        err.contains("mandatory equality on all key attributes"),
        "Expected sort key error, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// I16: SELECT with LIMIT
// ---------------------------------------------------------------------------

#[test]
fn test_select_with_limit() {
    let db = Database::memory().unwrap();
    create_test_table(&db, "Tbl");

    for i in 0..10 {
        let mut item = HashMap::new();
        item.insert("pk".to_string(), AttributeValue::S(format!("k{i}")));
        put_item(&db, "Tbl", item);
    }

    let resp = exec_with_limit(&db, "SELECT * FROM \"Tbl\"", 3);
    let items = resp.items.unwrap();
    assert_eq!(items.len(), 3);
}

// ---------------------------------------------------------------------------
// I17: COUNT(*) is rejected (real DynamoDB has no PartiQL aggregates)
// ---------------------------------------------------------------------------

#[test]
fn test_select_count_star_is_rejected() {
    // Formerly a dynoxide-only extension returning a Count item; real DynamoDB
    // rejects the projection with this bare message (captured eu-west-2).
    let db = Database::memory().unwrap();
    create_test_table(&db, "Tbl");

    let err = exec_err(&db, "SELECT COUNT(*) FROM \"Tbl\"");
    assert_eq!(err, "Unexpected path component at 1:8:5");
}

// ---------------------------------------------------------------------------
// I18: Set literal syntax (<< >>)
// ---------------------------------------------------------------------------

#[test]
fn test_set_literal_string_set() {
    let db = Database::memory().unwrap();
    create_test_table(&db, "Tbl");

    exec(
        &db,
        "INSERT INTO \"Tbl\" VALUE {'pk': 'k1', 'tags': <<'a', 'b', 'c'>>}",
    );

    let sel = exec(&db, "SELECT * FROM \"Tbl\" WHERE pk = 'k1'");
    let items = sel.items.unwrap();
    match items[0].get("tags") {
        Some(AttributeValue::SS(ss)) => {
            assert!(ss.contains(&"a".to_string()));
            assert!(ss.contains(&"b".to_string()));
            assert!(ss.contains(&"c".to_string()));
        }
        other => panic!("Expected SS, got {other:?}"),
    }
}

#[test]
fn test_set_literal_number_set() {
    let db = Database::memory().unwrap();
    create_test_table(&db, "Tbl");

    exec(
        &db,
        "INSERT INTO \"Tbl\" VALUE {'pk': 'k1', 'nums': <<1, 2, 3>>}",
    );

    let sel = exec(&db, "SELECT * FROM \"Tbl\" WHERE pk = 'k1'");
    let items = sel.items.unwrap();
    match items[0].get("nums") {
        Some(AttributeValue::NS(ns)) => {
            assert!(ns.contains(&"1".to_string()));
            assert!(ns.contains(&"2".to_string()));
            assert!(ns.contains(&"3".to_string()));
        }
        other => panic!("Expected NS, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// I19: OR conditions
// ---------------------------------------------------------------------------

#[test]
fn test_where_or_condition() {
    let db = Database::memory().unwrap();
    create_test_table(&db, "Tbl");

    let mut item_a = HashMap::new();
    item_a.insert("pk".to_string(), AttributeValue::S("k1".to_string()));
    item_a.insert("status".to_string(), AttributeValue::S("A".to_string()));
    put_item(&db, "Tbl", item_a);

    let mut item_b = HashMap::new();
    item_b.insert("pk".to_string(), AttributeValue::S("k2".to_string()));
    item_b.insert("status".to_string(), AttributeValue::S("B".to_string()));
    put_item(&db, "Tbl", item_b);

    let mut item_c = HashMap::new();
    item_c.insert("pk".to_string(), AttributeValue::S("k3".to_string()));
    item_c.insert("status".to_string(), AttributeValue::S("C".to_string()));
    put_item(&db, "Tbl", item_c);

    let resp = exec(
        &db,
        "SELECT * FROM \"Tbl\" WHERE status = 'A' OR status = 'B'",
    );
    let items = resp.items.unwrap();
    assert_eq!(items.len(), 2);
    let statuses: Vec<String> = items
        .iter()
        .filter_map(|i| match i.get("status") {
            Some(AttributeValue::S(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(statuses.contains(&"A".to_string()));
    assert!(statuses.contains(&"B".to_string()));
}

// ---------------------------------------------------------------------------
// I20: Nested paths in WHERE function conditions
// ---------------------------------------------------------------------------

#[test]
fn test_begins_with_nested_path() {
    let db = Database::memory().unwrap();
    create_test_table(&db, "Tbl");

    let mut addr = HashMap::new();
    addr.insert("city".to_string(), AttributeValue::S("London".to_string()));

    let mut item = HashMap::new();
    item.insert("pk".to_string(), AttributeValue::S("k1".to_string()));
    item.insert("address".to_string(), AttributeValue::M(addr));
    put_item(&db, "Tbl", item);

    let mut addr2 = HashMap::new();
    addr2.insert(
        "city".to_string(),
        AttributeValue::S("Manchester".to_string()),
    );

    let mut item2 = HashMap::new();
    item2.insert("pk".to_string(), AttributeValue::S("k2".to_string()));
    item2.insert("address".to_string(), AttributeValue::M(addr2));
    put_item(&db, "Tbl", item2);

    let resp = exec(
        &db,
        "SELECT * FROM \"Tbl\" WHERE BEGINS_WITH(address.city, 'Lon')",
    );
    let items = resp.items.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].get("pk"),
        Some(&AttributeValue::S("k1".to_string()))
    );
}

// ---------------------------------------------------------------------------
// I22: INSERT IF NOT EXISTS
// ---------------------------------------------------------------------------

#[test]
fn test_insert_if_not_exists_on_duplicate_succeeds() {
    let db = Database::memory().unwrap();
    create_test_table(&db, "Tbl");

    exec(
        &db,
        "INSERT INTO \"Tbl\" VALUE {'pk': 'k1', 'name': 'Alice'}",
    );

    // With IF NOT EXISTS, duplicate insert should silently succeed (no-op)
    exec(
        &db,
        "INSERT INTO \"Tbl\" VALUE {'pk': 'k1', 'name': 'Bob'} IF NOT EXISTS",
    );

    // Original item should be unchanged
    let sel = exec(&db, "SELECT * FROM \"Tbl\" WHERE pk = 'k1'");
    let items = sel.items.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].get("name"),
        Some(&AttributeValue::S("Alice".to_string()))
    );
}

#[test]
fn test_insert_without_if_not_exists_on_duplicate_fails() {
    let db = Database::memory().unwrap();
    create_test_table(&db, "Tbl");

    exec(
        &db,
        "INSERT INTO \"Tbl\" VALUE {'pk': 'k1', 'name': 'Alice'}",
    );

    // Without IF NOT EXISTS, duplicate insert should fail
    let err = exec_err(&db, "INSERT INTO \"Tbl\" VALUE {'pk': 'k1', 'name': 'Bob'}");
    assert!(
        err.contains("Duplicate primary key"),
        "Expected duplicate key error, got: {err}"
    );
}

/// An UPDATE that sets or removes a key attribute is refused and writes
/// nothing. It used to store the item under its old key with the new key
/// value inside it, so the item no longer matched its own key. The refusal is
/// UpdateItem's words, and comes before the item is read, so a missing item, a
/// false condition, a condition on a missing attribute or the key in a second
/// SET clause make no difference (captured eu-west-2 and us-east-1, 10 October
/// 2026).
#[test]
fn an_update_of_a_key_attribute_is_refused_and_writes_nothing() {
    let db = Database::memory().unwrap();
    create_composite_table(&db, "Keyed");
    let item = HashMap::from([
        ("pk".to_string(), AttributeValue::S("a".to_string())),
        ("sk".to_string(), AttributeValue::S("1".to_string())),
        ("v".to_string(), AttributeValue::N("0".to_string())),
    ]);
    put_item(&db, "Keyed", item.clone());
    let refusal = |attribute: &str| {
        format!(
            "One or more parameter values were invalid: Cannot update attribute \
             {attribute}. This attribute is part of the key"
        )
    };
    for (statement, attribute) in [
        (
            r#"UPDATE "Keyed" SET pk = 'b' WHERE pk = 'a' AND sk = '1'"#,
            "pk",
        ),
        (
            r#"UPDATE "Keyed" SET v = 1, sk = '2' WHERE pk = 'a' AND sk = '1'"#,
            "sk",
        ),
        (
            r#"UPDATE "Keyed" SET pk = 'a' WHERE pk = 'a' AND sk = '1'"#,
            "pk",
        ),
        (
            r#"UPDATE "Keyed" REMOVE pk WHERE pk = 'a' AND sk = '1'"#,
            "pk",
        ),
        (
            r#"UPDATE "Keyed" REMOVE sk WHERE pk = 'a' AND sk = '1'"#,
            "sk",
        ),
        (
            r#"UPDATE "Keyed" SET pk = 'b' WHERE pk = 'missing' AND sk = '1'"#,
            "pk",
        ),
        (
            r#"UPDATE "Keyed" SET v = 2, pk = 'b' WHERE pk = 'a' AND sk = '1' AND v = 99"#,
            "pk",
        ),
        (
            r#"UPDATE "Keyed" SET pk = 'b' WHERE pk = 'a' AND sk = '1' AND nosuch = 1"#,
            "pk",
        ),
        (
            r#"UPDATE "Keyed" SET v = 5 SET pk = 'b' WHERE pk = 'a' AND sk = '1'"#,
            "pk",
        ),
    ] {
        assert_eq!(exec_err(&db, statement), refusal(attribute), "{statement}");
    }
    let stored = exec(&db, r#"SELECT * FROM "Keyed" WHERE pk = 'a' AND sk = '1'"#);
    assert_eq!(stored.items, Some(vec![item]));
    let missing = exec(
        &db,
        r#"SELECT * FROM "Keyed" WHERE pk = 'missing' AND sk = '1'"#,
    );
    assert_eq!(missing.items, Some(vec![]));
}

const KEY_UPDATE: &str = "One or more parameter values were invalid: Cannot update attribute pk. This attribute is part of the key";

/// In a batch, an UPDATE of a key attribute fails its own statement with a
/// `ValidationError` in UpdateItem's words, echoing the table as DynamoDB
/// does, and writes nothing (captured eu-west-2 and us-east-1, 10 October
/// 2026).
#[test]
fn a_batch_update_of_a_key_attribute_fails_its_statement_and_echoes_the_table() {
    let db = Database::memory().unwrap();
    create_composite_table(&db, "Keyed");
    let item = HashMap::from([
        ("pk".to_string(), AttributeValue::S("a".to_string())),
        ("sk".to_string(), AttributeValue::S("1".to_string())),
        ("v".to_string(), AttributeValue::N("0".to_string())),
    ]);
    put_item(&db, "Keyed", item.clone());
    let request = serde_json::from_value(serde_json::json!({"Statements": [
        {"Statement": r#"UPDATE "Keyed" SET pk = 'b' WHERE pk = 'a' AND sk = '1'"#}
    ]}))
    .unwrap();
    let response = serde_json::to_value(db.batch_execute_statement(request).unwrap()).unwrap();
    assert_eq!(
        response,
        serde_json::json!({"Responses": [{
            "Error": {"Code": "ValidationError", "Message": KEY_UPDATE},
            "TableName": "Keyed"
        }]})
    );
    let stored = exec(&db, r#"SELECT * FROM "Keyed" WHERE pk = 'a' AND sk = '1'"#);
    assert_eq!(stored.items, Some(vec![item]));
}

/// In a transaction, an UPDATE of a key attribute cancels the transaction with
/// a `ValidationError` reason in UpdateItem's words, and nothing is written,
/// not even another statement's update. The reason was captured for a
/// transaction of that statement alone (eu-west-2 and us-east-1, 10 October
/// 2026); with a second statement beside it, `None` for that one follows the
/// transaction's other refusals and is inferred.
#[test]
fn a_transaction_update_of_a_key_attribute_cancels_it_and_writes_nothing() {
    let db = Database::memory().unwrap();
    create_composite_table(&db, "Keyed");
    let item = |pk: &str| {
        HashMap::from([
            ("pk".to_string(), AttributeValue::S(pk.to_string())),
            ("sk".to_string(), AttributeValue::S("1".to_string())),
            ("v".to_string(), AttributeValue::N("0".to_string())),
        ])
    };
    put_item(&db, "Keyed", item("a"));
    put_item(&db, "Keyed", item("b"));
    let cancelled = |statements: serde_json::Value| {
        let request =
            serde_json::from_value(serde_json::json!({"TransactStatements": statements})).unwrap();
        let error = db
            .execute_transaction(request)
            .expect_err("the transaction is cancelled");
        let mut body: serde_json::Value = serde_json::from_str(&error.to_json()).unwrap();
        body.as_object_mut().unwrap().remove("Message");
        body
    };
    let refused = serde_json::json!({"Code": "ValidationError", "Message": KEY_UPDATE});
    let key_update = serde_json::json!({"Statement": r#"UPDATE "Keyed" SET pk = 'z' WHERE pk = 'a' AND sk = '1'"#});
    assert_eq!(
        cancelled(serde_json::json!([key_update])),
        serde_json::json!({
            "__type": "com.amazonaws.dynamodb.v20120810#TransactionCanceledException",
            "CancellationReasons": [refused]
        })
    );
    assert_eq!(
        cancelled(serde_json::json!([
            {"Statement": r#"UPDATE "Keyed" SET v = 9 WHERE pk = 'b' AND sk = '1'"#},
            key_update
        ])),
        serde_json::json!({
            "__type": "com.amazonaws.dynamodb.v20120810#TransactionCanceledException",
            "CancellationReasons": [{"Code": "None"}, refused]
        })
    );
    for pk in ["a", "b"] {
        let stored = exec(
            &db,
            &format!(r#"SELECT * FROM "Keyed" WHERE pk = '{pk}' AND sk = '1'"#),
        );
        assert_eq!(stored.items, Some(vec![item(pk)]), "{pk}");
    }
}
