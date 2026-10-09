//! How a Query answers an invalid ExclusiveStartKey and a KeyConditions value
//! of the wrong type.
//!
//! A Query never prefixes a start-key problem the way a Scan can: a key that
//! doesn't fit the schema, whatever the shape, is "The provided starting key
//! is invalid" and nothing more, and an invalid attribute value in it keeps its
//! own message. A start key outside the key condition has messages of its own.
//! A KeyConditions value whose type isn't the key's schema type is refused with
//! one message for every type. Captured in eu-west-2, us-east-1, us-west-2 and
//! eu-central-1 on 2026-10-09; the regions agree on every case here.

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::put_item::PutItemRequest;
use dynoxide::actions::query::QueryRequest;
use serde_json::{Value, json};

const INVALID: &str = "The provided starting key is invalid";
const TYPE_MISMATCH: &str = "One or more parameter values were invalid: Condition parameter type does not match schema type";

fn deep_map(depth: usize) -> Value {
    let mut v = json!({"S": "leaf"});
    for _ in 0..depth {
        v = json!({"M": {"n": v}});
    }
    v
}

/// `Tbl`: pk (S) and sk (N), with a GSI on gpk (S) and gsk (S). `Hash`: pk (S)
/// only. Two items under pk "a".
fn setup() -> Database {
    let db = Database::memory().unwrap();
    let create = |body: Value| {
        db.create_table(serde_json::from_value::<CreateTableRequest>(body).unwrap())
            .unwrap();
    };
    create(json!({
        "TableName": "Tbl",
        "AttributeDefinitions": [
            {"AttributeName": "pk", "AttributeType": "S"},
            {"AttributeName": "sk", "AttributeType": "N"},
            {"AttributeName": "gpk", "AttributeType": "S"},
            {"AttributeName": "gsk", "AttributeType": "S"}
        ],
        "KeySchema": [
            {"AttributeName": "pk", "KeyType": "HASH"},
            {"AttributeName": "sk", "KeyType": "RANGE"}
        ],
        "GlobalSecondaryIndexes": [{
            "IndexName": "gsi",
            "KeySchema": [
                {"AttributeName": "gpk", "KeyType": "HASH"},
                {"AttributeName": "gsk", "KeyType": "RANGE"}
            ],
            "Projection": {"ProjectionType": "ALL"}
        }],
        "BillingMode": "PAY_PER_REQUEST"
    }));
    create(json!({
        "TableName": "Hash",
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "BillingMode": "PAY_PER_REQUEST"
    }));
    for (sk, gsk) in [("1", "s"), ("2", "t")] {
        db.put_item(
            serde_json::from_value::<PutItemRequest>(json!({
                "TableName": "Tbl",
                "Item": {"pk": {"S": "a"}, "sk": {"N": sk}, "gpk": {"S": "g"}, "gsk": {"S": gsk}}
            }))
            .unwrap(),
        )
        .unwrap();
    }
    db
}

fn query(db: &Database, body: Value) -> Result<usize, String> {
    let request: QueryRequest = serde_json::from_value(body).unwrap();
    db.query(request)
        .map(|r| r.count)
        .map_err(|e| e.to_string())
}

fn on_tbl(esk: Value) -> Value {
    json!({
        "TableName": "Tbl",
        "KeyConditionExpression": "pk = :pk",
        "ExpressionAttributeValues": {":pk": {"S": "a"}},
        "ExclusiveStartKey": esk
    })
}

fn on_hash(esk: Value) -> Value {
    json!({
        "TableName": "Hash",
        "KeyConditionExpression": "pk = :pk",
        "ExpressionAttributeValues": {":pk": {"S": "a"}},
        "ExclusiveStartKey": esk
    })
}

fn on_gsi(esk: Value) -> Value {
    json!({
        "TableName": "Tbl",
        "IndexName": "gsi",
        "KeyConditionExpression": "gpk = :g",
        "ExpressionAttributeValues": {":g": {"S": "g"}},
        "ExclusiveStartKey": esk
    })
}

#[test]
fn a_start_key_that_does_not_fit_the_schema_is_invalid_and_nothing_more() {
    let db = setup();
    let cases = [
        (
            "a map nested 31 levels",
            on_tbl(json!({"pk": deep_map(31), "sk": {"N": "1"}})),
        ),
        (
            "an empty map",
            on_tbl(json!({"pk": {"M": {}}, "sk": {"N": "1"}})),
        ),
        (
            "a number partition key",
            on_tbl(json!({"pk": {"N": "1"}, "sk": {"N": "1"}})),
        ),
        ("no sort key", on_tbl(json!({"pk": {"S": "a"}}))),
        (
            "a string sort key",
            on_tbl(json!({"pk": {"S": "a"}, "sk": {"S": "x"}})),
        ),
        (
            "an extra attribute",
            on_tbl(json!({"pk": {"S": "a"}, "sk": {"N": "1"}, "extra": {"S": "x"}})),
        ),
        ("no attributes", on_tbl(json!({}))),
        ("no partition key", on_tbl(json!({"sk": {"N": "1"}}))),
        (
            "a NULL partition key",
            on_tbl(json!({"pk": {"NULL": true}, "sk": {"N": "1"}})),
        ),
        (
            "a set partition key",
            on_tbl(json!({"pk": {"SS": ["a"]}, "sk": {"N": "1"}})),
        ),
        (
            "the table's keys only, on an index",
            on_gsi(json!({"pk": {"S": "a"}, "sk": {"N": "1"}})),
        ),
        (
            "the index's keys only",
            on_gsi(json!({"gpk": {"S": "g"}, "gsk": {"S": "s"}})),
        ),
        (
            "an extra attribute on a partition-key-only table",
            on_hash(json!({"pk": {"S": "a"}, "extra": {"S": "x"}})),
        ),
        (
            "a number key on a partition-key-only table",
            on_hash(json!({"pk": {"N": "1"}})),
        ),
    ];
    for (shape, body) in cases {
        assert_eq!(query(&db, body), Err(INVALID.to_string()), "{shape}");
    }
}

#[test]
fn an_invalid_attribute_value_in_a_start_key_keeps_its_own_message() {
    let db = setup();
    let cases = [
        (
            on_hash(json!({"pk": {"NULL": false}})),
            "One or more parameter values were invalid: Null attribute value types must have the value of true",
        ),
        (
            on_hash(json!({"pk": {"SS": []}})),
            "One or more parameter values were invalid: An string set  may not be empty",
        ),
        (
            on_tbl(json!({"pk": {"S": ""}, "sk": {"N": "1"}})),
            "One or more parameter values are not valid. The AttributeValue for a key attribute cannot contain an empty string value. Key: pk",
        ),
        (
            on_tbl(json!({"pk": {"S": "a"}, "sk": {"N": "abc"}})),
            "The parameter cannot be converted to a numeric value: abc",
        ),
        // These two keep the starting-key prefix on a Query too.
        (
            on_hash(json!({"pk": {}})),
            "The provided starting key is invalid: Supplied AttributeValue is empty, must contain exactly one of the supported datatypes",
        ),
        (
            on_hash(json!({"pk": {"S": "a", "N": "1"}})),
            "The provided starting key is invalid: Supplied AttributeValue has more than one datatypes set, must contain exactly one of the supported datatypes",
        ),
    ];
    for (body, message) in cases {
        assert_eq!(query(&db, body.clone()), Err(message.to_string()), "{body}");
    }
}

#[test]
fn a_start_key_outside_the_key_condition_is_refused() {
    let db = setup();
    assert_eq!(
        query(&db, on_tbl(json!({"pk": {"S": "b"}, "sk": {"N": "1"}}))),
        Err(
            "The provided starting key is outside query boundaries based on provided conditions"
                .to_string()
        )
    );
    assert_eq!(
        query(
            &db,
            json!({
                "TableName": "Tbl",
                "KeyConditionExpression": "pk = :pk AND sk < :x",
                "ExpressionAttributeValues": {":pk": {"S": "a"}, ":x": {"N": "2"}},
                "ExclusiveStartKey": {"pk": {"S": "a"}, "sk": {"N": "5"}}
            })
        ),
        Err("The provided starting key does not match the range key predicate".to_string())
    );
    assert_eq!(
        query(&db, on_hash(json!({"pk": {"S": "b"}}))),
        Err("The provided exclusive start key is invalid".to_string())
    );
}

#[test]
fn a_valid_start_key_still_pages() {
    let db = setup();
    assert_eq!(
        query(&db, on_tbl(json!({"pk": {"S": "a"}, "sk": {"N": "1"}}))),
        Ok(1)
    );
    assert_eq!(
        query(
            &db,
            on_gsi(
                json!({"gpk": {"S": "g"}, "gsk": {"S": "s"}, "pk": {"S": "a"}, "sk": {"N": "1"}})
            )
        ),
        Ok(1)
    );
    // Inside the range predicate.
    assert_eq!(
        query(
            &db,
            json!({
                "TableName": "Tbl",
                "KeyConditionExpression": "pk = :pk AND sk BETWEEN :lo AND :hi",
                "ExpressionAttributeValues": {":pk": {"S": "a"}, ":lo": {"N": "1"}, ":hi": {"N": "2"}},
                "ExclusiveStartKey": {"pk": {"S": "a"}, "sk": {"N": "1"}}
            })
        ),
        Ok(1)
    );
}

fn key_conditions(pk: Value, sk: Option<Value>) -> Value {
    let mut conditions = json!({"pk": {"ComparisonOperator": "EQ", "AttributeValueList": [pk]}});
    if let Some(sk) = sk {
        conditions["sk"] = sk;
    }
    json!({"TableName": "Tbl", "KeyConditions": conditions})
}

#[test]
fn a_key_conditions_value_of_the_wrong_type_is_refused() {
    let db = setup();
    for value in [
        json!({"M": {}}),
        json!({"M": {"a": {"S": "x"}}}),
        deep_map(31),
        json!({"N": "1"}),
        json!({"L": []}),
        json!({"BOOL": true}),
        json!({"NULL": true}),
        json!({"SS": ["a"]}),
        json!({"B": "YQ=="}),
    ] {
        assert_eq!(
            query(&db, key_conditions(value.clone(), None)),
            Err(TYPE_MISMATCH.to_string()),
            "partition key value {value}"
        );
    }
    for value in [json!({"S": "x"}), json!({"M": {}})] {
        assert_eq!(
            query(
                &db,
                key_conditions(
                    json!({"S": "a"}),
                    Some(
                        json!({"ComparisonOperator": "EQ", "AttributeValueList": [value.clone()]})
                    )
                )
            ),
            Err(TYPE_MISMATCH.to_string()),
            "sort key value {value}"
        );
    }
}

#[test]
fn a_between_key_condition_keeps_its_own_messages() {
    let db = setup();
    assert_eq!(
        query(
            &db,
            key_conditions(
                json!({"S": "a"}),
                Some(json!({"ComparisonOperator": "BETWEEN", "AttributeValueList": [{"M": {}}, {"M": {}}]}))
            )
        ),
        Err("One or more parameter values were invalid: ComparisonOperator BETWEEN is not valid for M AttributeValue type".to_string())
    );
    assert_eq!(
        query(
            &db,
            key_conditions(
                json!({"S": "a"}),
                Some(json!({"ComparisonOperator": "BETWEEN", "AttributeValueList": [{"N": "1"}, {"S": "x"}]}))
            )
        ),
        Err("One or more parameter values were invalid: AttributeValues inside AttributeValueList must be of same type".to_string())
    );
    assert_eq!(query(&db, key_conditions(json!({"S": "a"}), None)), Ok(2));
}
