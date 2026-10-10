//! How a `BOOL` or `NULL` member is read when the library decodes a value
//! from DynamoDB JSON, at the top of an item and nested in a list or a map.
//!
//! The wire tests in `bool_and_null_values_http.rs` hold the captured rules
//! (eu-west-2 and us-east-1, 2026-10-10): the strings `true`, `t`, `yes`, `y`
//! and `1` read as true and `false`, `f`, `no`, `n` and `0` as false, in any
//! case; any other string is a malformed request; a NULL that reads as false
//! is refused. The refusals of a false NULL nested in a list or a map were
//! captured too. A nested BOOL or NULL that reads as true is inferred: the
//! same decoder reads every value, however deep.

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::get_item::GetItemRequest;
use dynoxide::actions::put_item::PutItemRequest;
use dynoxide::types::AttributeValue;
use serde_json::{Value, json};
use std::collections::HashMap;

const NULL_FALSE: &str = "One or more parameter values were invalid: Null attribute value types \
                          must have the value of true";

fn decode(value: Value) -> Result<AttributeValue, String> {
    serde_json::from_value::<AttributeValue>(value).map_err(|e| e.to_string())
}

#[test]
fn a_decoded_value_reads_the_strings_as_booleans() {
    assert_eq!(
        decode(json!({"BOOL": "yes"})),
        Ok(AttributeValue::BOOL(true))
    );
    assert_eq!(
        decode(json!({"BOOL": "N"})),
        Ok(AttributeValue::BOOL(false))
    );
    assert_eq!(decode(json!({"BOOL": "1"})), Ok(AttributeValue::BOOL(true)));
    assert_eq!(
        decode(json!({"NULL": "TRUE"})),
        Ok(AttributeValue::NULL(true))
    );
    assert_eq!(
        decode(json!({"BOOL": true})),
        Ok(AttributeValue::BOOL(true))
    );
}

#[test]
fn a_nested_value_reads_the_strings_as_booleans() {
    assert_eq!(
        decode(json!({"L": [{"BOOL": "no"}, {"NULL": "t"}, {"BOOL": "Yes"}]})),
        Ok(AttributeValue::L(vec![
            AttributeValue::BOOL(false),
            AttributeValue::NULL(true),
            AttributeValue::BOOL(true),
        ]))
    );
    assert_eq!(
        decode(json!({"M": {"k": {"BOOL": "Y"}, "m": {"M": {"n": {"NULL": "1"}}}}})),
        Ok(AttributeValue::M(HashMap::from([
            ("k".to_string(), AttributeValue::BOOL(true)),
            (
                "m".to_string(),
                AttributeValue::M(HashMap::from([(
                    "n".to_string(),
                    AttributeValue::NULL(true)
                )]))
            ),
        ])))
    );
}

#[test]
fn a_nested_false_null_or_unreadable_bool_is_refused() {
    for value in [
        json!({"L": [{"NULL": "false"}]}),
        json!({"M": {"k": {"NULL": "no"}}}),
        json!({"L": [{"NULL": false}]}),
    ] {
        let refused = decode(value.clone()).expect_err("a false NULL is refused");
        assert!(refused.contains(NULL_FALSE), "{value}: {refused}");
    }
    for value in [
        json!({"L": [{"BOOL": "abc"}]}),
        json!({"M": {"k": {"BOOL": "off"}}}),
    ] {
        let refused = decode(value.clone()).expect_err("an unreadable BOOL is refused");
        assert!(
            refused.contains("Unexpected token received from parser"),
            "{value}: {refused}"
        );
    }
}

#[test]
fn a_request_decoded_from_json_stores_what_the_strings_read_as() {
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value::<CreateTableRequest>(json!({
            "TableName": "Values",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    db.put_item(
        serde_json::from_value::<PutItemRequest>(json!({
            "TableName": "Values",
            "Item": {
                "pk": {"S": "a"},
                "b": {"BOOL": "t"},
                "n": {"NULL": "yes"},
                "l": {"L": [{"BOOL": "0"}]},
                "m": {"M": {"k": {"BOOL": "TRUE"}}}
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let item = db
        .get_item(
            serde_json::from_value::<GetItemRequest>(json!({
                "TableName": "Values",
                "Key": {"pk": {"S": "a"}}
            }))
            .unwrap(),
        )
        .unwrap()
        .item
        .expect("the item was stored");
    assert_eq!(item.get("b"), Some(&AttributeValue::BOOL(true)));
    assert_eq!(item.get("n"), Some(&AttributeValue::NULL(true)));
    assert_eq!(
        item.get("l"),
        Some(&AttributeValue::L(vec![AttributeValue::BOOL(false)]))
    );
    assert_eq!(
        item.get("m"),
        Some(&AttributeValue::M(HashMap::from([(
            "k".to_string(),
            AttributeValue::BOOL(true)
        )])))
    );
}
