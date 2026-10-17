//! A request built in Rust and handed to `Database` gets the same table-name
//! answer as the same request decoded from a body.
//!
//! The decoders and the `Database` methods ask the same shared checks, but
//! only the wire path had a test, so a regression in the in-process branch
//! would have shown up nowhere.

use dynoxide::Database;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// The message the request decoder gives, with the internal marker the server
/// layer strips before it reaches the wire removed.
fn wire_message<T: DeserializeOwned>(body: Value) -> String {
    match serde_json::from_value::<T>(body) {
        Ok(_) => panic!("the decoder accepted a malformed table name"),
        Err(err) => {
            let text = err.to_string();
            text.strip_prefix("VALIDATION:")
                .unwrap_or(&text)
                .to_string()
        }
    }
}

fn in_process_message<T>(result: dynoxide::Result<T>) -> String {
    match result {
        Ok(_) => panic!("Database accepted a malformed table name"),
        Err(err) => err.to_string(),
    }
}

/// Decode `body` into a request, which must succeed apart from the table name
/// (a valid one is swapped in), and give it back with the bad name set.
fn built_in_rust<T: DeserializeOwned>(body: &Value, set_name: impl FnOnce(&mut T, &str)) -> T {
    let mut good = body.clone();
    good["TableName"] = json!("ValidName");
    let mut request: T = serde_json::from_value(good).expect("the rest of the request is valid");
    set_name(&mut request, "a");
    request
}

fn check<T: DeserializeOwned, R>(
    body: Value,
    set_name: impl FnOnce(&mut T, &str),
    call: impl FnOnce(T) -> dynoxide::Result<R>,
) {
    let mut bad = body.clone();
    bad["TableName"] = json!("a");
    let wire = wire_message::<T>(bad);
    let request = built_in_rust(&body, set_name);
    let in_process = in_process_message(call(request));
    // The answer names the table it was given, so it is the table-name check
    // that fired and not some other refusal.
    assert!(
        wire.contains("Value 'a'"),
        "the wire answer is about the table name: {wire}"
    );
    assert_eq!(in_process, wire);
}

#[test]
fn get_item_names_a_short_table_as_the_wire_does() {
    let db = Database::memory().unwrap();
    check(
        json!({"TableName": "x", "Key": {"pk": {"S": "a"}}}),
        |r: &mut dynoxide::actions::get_item::GetItemRequest, n| r.table_name = n.to_string(),
        |r| db.get_item(r),
    );
}

#[test]
fn put_item_names_a_short_table_as_the_wire_does() {
    let db = Database::memory().unwrap();
    check(
        json!({"TableName": "x", "Item": {"pk": {"S": "a"}}}),
        |r: &mut dynoxide::actions::put_item::PutItemRequest, n| r.table_name = n.to_string(),
        |r| db.put_item(r),
    );
}

#[test]
fn delete_item_names_a_short_table_as_the_wire_does() {
    let db = Database::memory().unwrap();
    check(
        json!({"TableName": "x", "Key": {"pk": {"S": "a"}}}),
        |r: &mut dynoxide::actions::delete_item::DeleteItemRequest, n| r.table_name = n.to_string(),
        |r| db.delete_item(r),
    );
}

#[test]
fn update_item_names_a_short_table_as_the_wire_does() {
    let db = Database::memory().unwrap();
    check(
        json!({"TableName": "x", "Key": {"pk": {"S": "a"}}}),
        |r: &mut dynoxide::actions::update_item::UpdateItemRequest, n| r.table_name = n.to_string(),
        |r| db.update_item(r),
    );
}

#[test]
fn query_names_a_short_table_as_the_wire_does() {
    let db = Database::memory().unwrap();
    check(
        json!({
            "TableName": "x",
            "KeyConditionExpression": "pk = :p",
            "ExpressionAttributeValues": {":p": {"S": "a"}}
        }),
        |r: &mut dynoxide::actions::query::QueryRequest, n| r.table_name = n.to_string(),
        |r| db.query(r),
    );
}

#[test]
fn scan_names_a_short_table_as_the_wire_does() {
    let db = Database::memory().unwrap();
    check(
        json!({"TableName": "x"}),
        |r: &mut dynoxide::actions::scan::ScanRequest, n| r.table_name = n.to_string(),
        |r| db.scan(r),
    );
}

#[test]
fn search_vectors_names_a_short_table_as_the_wire_does() {
    let db = Database::memory().unwrap();
    check(
        json!({
            "TableName": "x", "IndexName": "vix",
            "SearchVector": [{"N": "1"}], "TopK": 1
        }),
        |r: &mut dynoxide::actions::search_vectors::SearchVectorsRequest, n| {
            r.table_name = n.to_string()
        },
        |r| db.search_vectors(r),
    );
}

/// A DeleteItem built in Rust is refused for a `ReturnValues` it does not
/// honour before the table is looked up, and before its expressions are read.
#[test]
fn delete_item_refuses_return_values_it_does_not_honour_in_process() {
    let db = Database::memory().unwrap();
    let build = |return_values: &str, condition: Option<&str>| {
        let mut body = json!({
            "TableName": "Missing",
            "Key": {"pk": {"S": "a"}},
            "ReturnValues": "NONE"
        });
        if let Some(condition) = condition {
            body["ConditionExpression"] = json!(condition);
        }
        let mut request: dynoxide::actions::delete_item::DeleteItemRequest =
            serde_json::from_value(body).unwrap();
        request.return_values = Some(return_values.to_string());
        request
    };
    for value in ["ALL_NEW", "UPDATED_NEW", "UPDATED_OLD", "all_new"] {
        let err = db.delete_item(build(value, None)).unwrap_err();
        assert!(
            err.to_string()
                .contains("ReturnValues can only be ALL_OLD or NONE"),
            "{value}: {err}"
        );
        // Ahead of an expression that would otherwise be refused.
        let err = db.delete_item(build(value, Some(""))).unwrap_err();
        assert!(
            err.to_string()
                .contains("ReturnValues can only be ALL_OLD or NONE"),
            "{value} with an empty condition: {err}"
        );
    }
    // The values it does honour go on to the table lookup.
    for value in ["NONE", "ALL_OLD"] {
        let err = db.delete_item(build(value, None)).unwrap_err();
        assert!(
            !err.to_string().contains("ReturnValues can only be"),
            "{value}: {err}"
        );
    }
}
