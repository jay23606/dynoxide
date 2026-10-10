//! A legacy `Expected` that doesn't read as conditions is refused in
//! DynamoDB's words by the library itself, whatever features it was built
//! with, and nothing is written. Unit tests compile the library with every
//! test-only path in, so CI also runs this file against a library built
//! without the server features (`--no-default-features --features
//! native-sqlite`), the build a library caller gets. Captured against
//! eu-west-2 and us-east-1, 2026-10-10.

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::get_item::GetItemRequest;
use dynoxide::actions::put_item::PutItemRequest;
use serde_json::json;

#[test]
fn an_empty_expected_value_is_refused_in_dynamodbs_words() {
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value::<CreateTableRequest>(json!({
            "TableName": "Expected",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    let request: PutItemRequest = serde_json::from_value(json!({
        "TableName": "Expected",
        "Item": {"pk": {"S": "k"}},
        "Expected": {"guard": {"Value": {}}}
    }))
    .unwrap();
    let err = db.put_item(request).unwrap_err().to_string();
    assert!(
        err.contains(
            "Supplied AttributeValue is empty, must contain exactly one of the supported datatypes"
        ),
        "{err}"
    );
    let read: GetItemRequest = serde_json::from_value(json!({
        "TableName": "Expected",
        "Key": {"pk": {"S": "k"}}
    }))
    .unwrap();
    assert!(db.get_item(read).unwrap().item.is_none());
}
