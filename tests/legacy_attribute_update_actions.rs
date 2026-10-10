//! A legacy `AttributeUpdates` action is matched exactly, as DynamoDB matches
//! it. `put`, `Put`, `delete`, `add` and `BOGUS` are all refused before the
//! table is looked up, with DynamoDB's own text, stray quote included
//! (captured eu-west-2 and us-east-1, 10 October 2026). A lower-case action
//! used to be read as its upper-case form and written.

use dynoxide::Database;
use dynoxide::DynoxideError;
use serde_json::{Value, json};

const REFUSAL: &str = "1 validation error detected: Member must satisfy enum value set: [ADD, DELETE, PUT]' for Action";

fn request<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

fn db() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(request(json!({
        "TableName": "Tbl",
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        "BillingMode": "PAY_PER_REQUEST"
    })))
    .unwrap();
    db.put_item(request(json!({
        "TableName": "Tbl",
        "Item": {"pk": {"S": "a"}, "a": {"N": "0"}}
    })))
    .unwrap();
    db
}

fn stored(db: &Database) -> Value {
    let item = db
        .get_item(request(
            json!({"TableName": "Tbl", "Key": {"pk": {"S": "a"}}}),
        ))
        .unwrap()
        .item;
    serde_json::to_value(item).unwrap()
}

#[test]
fn an_action_not_written_exactly_is_refused_and_writes_nothing() {
    let db = db();
    for table in ["Tbl", "Absent"] {
        for action in ["put", "Put", "delete", "add", "BOGUS"] {
            let got = db.update_item(request(json!({
                "TableName": table,
                "Key": {"pk": {"S": "a"}},
                "AttributeUpdates": {"a": {"Value": {"N": "1"}, "Action": action}}
            })));
            match got {
                Ok(response) => panic!("{table} {action} was accepted: {response:?}"),
                Err(err) => {
                    assert!(
                        matches!(err, DynoxideError::EnvelopedValidation(_))
                            || err.error_type() == "com.amazon.coral.validate#ValidationException",
                        "{table} {action}: {err:?}"
                    );
                    assert_eq!(err.to_string(), REFUSAL, "{table} {action}");
                }
            }
        }
    }
    assert_eq!(stored(&db), json!({"pk": {"S": "a"}, "a": {"N": "0"}}));
}

#[test]
fn the_exact_actions_still_work() {
    let db = db();
    db.update_item(request(json!({
        "TableName": "Tbl",
        "Key": {"pk": {"S": "a"}},
        "AttributeUpdates": {"a": {"Value": {"N": "2"}, "Action": "ADD"}}
    })))
    .unwrap();
    assert_eq!(stored(&db), json!({"pk": {"S": "a"}, "a": {"N": "2"}}));
}

/// A mis-cased action is refused whatever else is wrong with another update in
/// the same request: every action is checked first. A map's order varies from
/// one request to the next, so the request is sent many times.
#[test]
fn a_mis_cased_action_wins_over_another_updates_error() {
    let db = db();
    for _ in 0..64 {
        let got = db.update_item(request(json!({
            "TableName": "Tbl",
            "Key": {"pk": {"S": "a"}},
            "AttributeUpdates": {
                "a": {"Value": {"N": "1"}, "Action": "put"},
                "b": {"Action": "PUT"}
            }
        })));
        assert_eq!(
            got.expect_err("the request is refused").to_string(),
            REFUSAL
        );
    }
    assert_eq!(stored(&db), json!({"pk": {"S": "a"}, "a": {"N": "0"}}));
}
