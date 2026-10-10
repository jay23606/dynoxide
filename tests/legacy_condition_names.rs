//! Legacy conditions run on any attribute name, as DynamoDB runs them: one
//! with a `-` or a `.` in it, or a character outside ASCII, is the attribute
//! of that name. Dynoxide converts each condition to an expression, and the
//! name used to go into the expression's placeholder as written, where the
//! expression reader stops at such a character and refused the request.
//!
//! The answers below were captured against eu-west-2 and us-east-1 on 10
//! October 2026, on a table keyed on `user-id` and `s.k`; both regions agreed.
//! The non-ASCII name and the DeleteItem case are inferred.

use dynoxide::Database;
use dynoxide::DynoxideError;
use serde_json::{Value, json};

fn request<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

fn first() -> Value {
    json!({"user-id": {"S": "u1"}, "s.k": {"S": "1"}, "a-b": {"N": "5"}, "x.y": {"N": "1"}})
}

fn second() -> Value {
    json!({"user-id": {"S": "u1"}, "s.k": {"S": "2"}, "a-b": {"N": "6"}, "x.y": {"N": "2"}})
}

/// Table `Legacy` keyed on `user-id` and `s.k`, holding the capture's two
/// items.
fn db() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(request(json!({
        "TableName": "Legacy",
        "KeySchema": [
            {"AttributeName": "user-id", "KeyType": "HASH"},
            {"AttributeName": "s.k", "KeyType": "RANGE"}
        ],
        "AttributeDefinitions": [
            {"AttributeName": "user-id", "AttributeType": "S"},
            {"AttributeName": "s.k", "AttributeType": "S"}
        ],
        "BillingMode": "PAY_PER_REQUEST"
    })))
    .unwrap();
    for item in [first(), second()] {
        db.put_item(request(json!({"TableName": "Legacy", "Item": item})))
            .unwrap();
    }
    db
}

fn eq(value: Value) -> Value {
    json!({"AttributeValueList": [value], "ComparisonOperator": "EQ"})
}

/// A read's items as JSON, and its Count and ScannedCount.
fn answer(items: Option<Vec<dynoxide::types::Item>>, count: usize, scanned: usize) -> Value {
    let items: Vec<Value> = items
        .unwrap_or_default()
        .into_iter()
        .map(|item| serde_json::to_value(item).unwrap())
        .collect();
    json!({"Items": items, "Count": count, "ScannedCount": scanned})
}

fn query(db: &Database, body: Value) -> Value {
    let response = db.query(request(body)).unwrap();
    answer(response.items, response.count, response.scanned_count)
}

fn scan(db: &Database, body: Value) -> Value {
    let response = db.scan(request(body)).unwrap();
    answer(response.items, response.count, response.scanned_count)
}

#[test]
fn key_conditions_run_on_names_with_a_hyphen_or_a_dot() {
    let db = db();
    let got = query(
        &db,
        json!({"TableName": "Legacy", "KeyConditions": {
            "user-id": eq(json!({"S": "u1"})),
            "s.k": eq(json!({"S": "1"}))
        }}),
    );
    assert_eq!(
        got,
        json!({"Items": [first()], "Count": 1, "ScannedCount": 1})
    );
}

#[test]
fn query_and_scan_filters_run_on_names_with_a_hyphen_or_a_dot() {
    let db = db();
    let got = query(
        &db,
        json!({
            "TableName": "Legacy",
            "KeyConditions": {"user-id": eq(json!({"S": "u1"}))},
            "QueryFilter": {"a-b": eq(json!({"N": "5"}))}
        }),
    );
    assert_eq!(
        got,
        json!({"Items": [first()], "Count": 1, "ScannedCount": 2})
    );
    let got = scan(
        &db,
        json!({"TableName": "Legacy", "ScanFilter": {"a-b": eq(json!({"N": "5"}))}}),
    );
    assert_eq!(
        got,
        json!({"Items": [first()], "Count": 1, "ScannedCount": 2})
    );
    let got = scan(
        &db,
        json!({"TableName": "Legacy", "ScanFilter": {"x.y": eq(json!({"N": "2"}))}}),
    );
    assert_eq!(
        got,
        json!({"Items": [second()], "Count": 1, "ScannedCount": 2})
    );
    // Two conditions together, joined by ConditionalOperator (inferred).
    let got = scan(
        &db,
        json!({
            "TableName": "Legacy",
            "ScanFilter": {"a-b": eq(json!({"N": "5"})), "x.y": eq(json!({"N": "2"}))},
            "ConditionalOperator": "OR"
        }),
    );
    assert_eq!(got["Count"], 2);
}

#[test]
fn expected_runs_on_a_name_with_a_hyphen() {
    let db = db();
    db.put_item(request(json!({
        "TableName": "Legacy",
        "Item": first(),
        "Expected": {"a-b": {"Value": {"N": "5"}}}
    })))
    .unwrap();
    let err = db
        .put_item(request(json!({
            "TableName": "Legacy",
            "Item": {"user-id": {"S": "u1"}, "s.k": {"S": "1"}, "a-b": {"N": "7"}},
            "Expected": {"a-b": {"Value": {"N": "6"}}}
        })))
        .unwrap_err();
    assert!(
        matches!(err, DynoxideError::ConditionalCheckFailedException(..)),
        "{err:?}"
    );
    assert_eq!(err.to_string(), "The conditional request failed");
    // DeleteItem converts Expected the same way (inferred).
    let err = db
        .delete_item(request(json!({
            "TableName": "Legacy",
            "Key": {"user-id": {"S": "u1"}, "s.k": {"S": "2"}},
            "Expected": {"x.y": {"Value": {"N": "1"}}}
        })))
        .unwrap_err();
    assert!(
        matches!(err, DynoxideError::ConditionalCheckFailedException(..)),
        "{err:?}"
    );
    let got = scan(&db, json!({"TableName": "Legacy"}));
    assert_eq!(got["Count"], 2, "nothing was written or deleted: {got}");
    assert_eq!(
        db.get_item(request(json!({
            "TableName": "Legacy",
            "Key": {"user-id": {"S": "u1"}, "s.k": {"S": "1"}}
        })))
        .unwrap()
        .item
        .map(|item| serde_json::to_value(item).unwrap()),
        Some(first())
    );
}

/// Inferred: a name outside ASCII is an attribute like any other.
#[test]
fn legacy_conditions_run_on_a_name_outside_ascii() {
    let db = db();
    db.put_item(request(json!({
        "TableName": "Legacy",
        "Item": {"user-id": {"S": "u2"}, "s.k": {"S": "1"}, "größe": {"N": "3"}}
    })))
    .unwrap();
    let got = scan(
        &db,
        json!({"TableName": "Legacy", "ScanFilter": {"größe": eq(json!({"N": "3"}))}}),
    );
    assert_eq!(got["Count"], 1, "{got}");
    let got = query(
        &db,
        json!({
            "TableName": "Legacy",
            "KeyConditions": {"user-id": eq(json!({"S": "u2"}))},
            "QueryFilter": {"größe": eq(json!({"N": "4"}))}
        }),
    );
    assert_eq!(got, json!({"Items": [], "Count": 0, "ScannedCount": 1}));
    db.put_item(request(json!({
        "TableName": "Legacy",
        "Item": {"user-id": {"S": "u2"}, "s.k": {"S": "1"}, "größe": {"N": "4"}},
        "Expected": {"größe": {"Value": {"N": "3"}}}
    })))
    .unwrap();
}
