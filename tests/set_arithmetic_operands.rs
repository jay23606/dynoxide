//! `SET n = x + 1` and `SET n = x - 1` need `x` to be a number that is there.
//! A stored string, map or list is `An operand in the update expression has an
//! incorrect data type`, and an attribute the item lacks is `The provided
//! expression refers to an attribute that does not exist in the item`, in
//! UpdateItem and in PartiQL alike. PartiQL used to read a missing attribute as
//! zero. Captured against eu-west-2, 18 October 2026.

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::ExecuteStatementRequest;
use dynoxide::actions::update_item::UpdateItemRequest;
use serde_json::{Value, json};

const OPERAND: &str = "An operand in the update expression has an incorrect data type";
const MISSING: &str =
    "The provided expression refers to an attribute that does not exist in the item";

fn setup() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value::<CreateTableRequest>(json!({
            "TableName": "Sums",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    db.put_item(
        serde_json::from_value(json!({"TableName": "Sums", "Item": {
            "pk": {"S": "k"},
            "n": {"N": "5"},
            "s": {"S": "text"},
            "m": {"M": {"a": {"N": "1"}}},
            "l": {"L": [{"N": "1"}]}
        }}))
        .unwrap(),
    )
    .unwrap();
    db
}

fn stored(db: &Database) -> Value {
    let item = db
        .get_item(
            serde_json::from_value(json!({
                "TableName": "Sums", "Key": {"pk": {"S": "k"}}, "ConsistentRead": true
            }))
            .unwrap(),
        )
        .unwrap()
        .item
        .unwrap();
    serde_json::to_value(item).unwrap()
}

fn update_item(expression: &str) -> (Result<(), String>, Value) {
    let db = setup();
    let result = db
        .update_item(
            serde_json::from_value::<UpdateItemRequest>(json!({
                "TableName": "Sums",
                "Key": {"pk": {"S": "k"}},
                "UpdateExpression": expression,
                "ExpressionAttributeValues": {":v": {"N": "1"}}
            }))
            .unwrap(),
        )
        .map(|_| ())
        .map_err(|e| e.to_string());
    let after = stored(&db);
    (result, after)
}

fn partiql(clause: &str) -> (Result<(), String>, Value) {
    let db = setup();
    let result = db
        .execute_statement(
            serde_json::from_value::<ExecuteStatementRequest>(json!({
                "Statement": format!("UPDATE \"Sums\" {clause} WHERE pk = 'k'")
            }))
            .unwrap(),
        )
        .map(|_| ())
        .map_err(|e| e.to_string());
    let after = stored(&db);
    (result, after)
}

fn unchanged_item() -> Value {
    stored(&setup())
}

#[test]
fn arithmetic_on_a_stored_value_that_is_not_a_number_is_an_operand_type_error() {
    for attribute in ["s", "m", "l"] {
        for op in ["+", "-"] {
            for (route, (result, after)) in [
                (
                    "UpdateItem",
                    update_item(&format!("SET n = {attribute} {op} :v")),
                ),
                ("PartiQL", partiql(&format!("SET n = {attribute} {op} 1"))),
            ] {
                let error = result.expect_err(&format!("{route} {attribute} {op}"));
                assert!(error.contains(OPERAND), "{route} {attribute} {op}: {error}");
                assert_eq!(after, unchanged_item(), "{route} {attribute} {op}");
            }
        }
    }
}

#[test]
fn arithmetic_on_an_attribute_the_item_lacks_is_refused_not_read_as_zero() {
    for op in ["+", "-"] {
        for (route, (result, after)) in [
            ("UpdateItem", update_item(&format!("SET n = zz {op} :v"))),
            ("PartiQL", partiql(&format!("SET n = zz {op} 1"))),
        ] {
            let error = result.expect_err(&format!("{route} zz {op}"));
            assert!(error.contains(MISSING), "{route} zz {op}: {error}");
            assert_eq!(after, unchanged_item(), "{route} zz {op}");
        }
    }
}

#[test]
fn arithmetic_on_a_stored_number_still_works() {
    let (result, after) = partiql("SET n = n + 2");
    result.unwrap();
    assert_eq!(after["n"], json!({"N": "7"}));
    let (result, after) = update_item("SET n = n - :v");
    result.unwrap();
    assert_eq!(after["n"], json!({"N": "4"}));
}
