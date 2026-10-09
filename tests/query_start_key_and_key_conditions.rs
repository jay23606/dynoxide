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

const AT_MOST_ONE_ROW: &str = "The query can return at most one row and cannot be restarted";
const OUTSIDE: &str =
    "The provided starting key is outside query boundaries based on provided conditions";
const RANGE_PREDICATE: &str = "The provided starting key does not match the range key predicate";

/// `Wide`: pk (S) and sk (S), an LSI on lsk, a GSI on gh and gr, a hash-only
/// GSI on gh, and a GSI keyed on h1 and h2 then a and b.
fn wide() -> Database {
    let db = Database::memory().unwrap();
    let definitions: Vec<Value> = ["pk", "sk", "lsk", "gh", "gr", "h1", "h2", "a", "b"]
        .iter()
        .map(|n| json!({"AttributeName": n, "AttributeType": "S"}))
        .collect();
    db.create_table(
        serde_json::from_value::<CreateTableRequest>(json!({
            "TableName": "Wide",
            "AttributeDefinitions": definitions,
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"}
            ],
            "LocalSecondaryIndexes": [{
                "IndexName": "lsi",
                "KeySchema": [
                    {"AttributeName": "pk", "KeyType": "HASH"},
                    {"AttributeName": "lsk", "KeyType": "RANGE"}
                ],
                "Projection": {"ProjectionType": "ALL"}
            }],
            "GlobalSecondaryIndexes": [
                {
                    "IndexName": "single",
                    "KeySchema": [
                        {"AttributeName": "gh", "KeyType": "HASH"},
                        {"AttributeName": "gr", "KeyType": "RANGE"}
                    ],
                    "Projection": {"ProjectionType": "ALL"}
                },
                {
                    "IndexName": "hashonly",
                    "KeySchema": [{"AttributeName": "gh", "KeyType": "HASH"}],
                    "Projection": {"ProjectionType": "ALL"}
                },
                {
                    "IndexName": "multi",
                    "KeySchema": [
                        {"AttributeName": "h1", "KeyType": "HASH"},
                        {"AttributeName": "h2", "KeyType": "HASH"},
                        {"AttributeName": "a", "KeyType": "RANGE"},
                        {"AttributeName": "b", "KeyType": "RANGE"}
                    ],
                    "Projection": {"ProjectionType": "ALL"}
                }
            ],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    db
}

fn on_wide(index: Option<&str>, condition: &str, values: Value, esk: Value) -> Value {
    let mut body = json!({
        "TableName": "Wide",
        "KeyConditionExpression": condition,
        "ExpressionAttributeValues": values,
        "ExclusiveStartKey": esk
    });
    if let Some(index) = index {
        body["IndexName"] = json!(index);
    }
    body
}

fn s(value: &str) -> Value {
    json!({"S": value})
}

// Captured in eu-west-2 and us-east-1 on 2026-10-09, identically. The key
// condition is walked in order: a partition key attribute other than the last
// that differs ends it with "at most one row"; then any sort key condition the
// start key breaks; then a different last partition key attribute, which is
// "at most one row" when a sort key condition follows it and "outside query
// boundaries" when none does.
#[test]
fn a_start_key_from_another_partition_with_a_sort_key_condition_cannot_be_restarted() {
    let db = wide();
    let values = json!({":p": s("p1"), ":s": s("m")});
    assert_eq!(
        query(
            &db,
            on_wide(
                None,
                "pk = :p AND sk > :s",
                values.clone(),
                json!({"pk": s("p2"), "sk": s("z")})
            )
        ),
        Err(AT_MOST_ONE_ROW.to_string())
    );
    // A sort key outside the condition is reported first.
    assert_eq!(
        query(
            &db,
            on_wide(
                None,
                "pk = :p AND sk > :s",
                values,
                json!({"pk": s("p2"), "sk": s("a")})
            )
        ),
        Err(RANGE_PREDICATE.to_string())
    );
}

#[test]
fn an_index_start_key_outside_the_key_condition_is_refused() {
    let db = wide();
    let base = |extra: Value| {
        let mut esk = json!({"pk": s("p1"), "sk": s("s1")});
        for (k, v) in extra.as_object().unwrap() {
            esk[k] = v.clone();
        }
        esk
    };
    let single = |condition: &str, values: Value, esk: Value| {
        query(&db, on_wide(Some("single"), condition, values, base(esk)))
    };
    assert_eq!(
        single(
            "gh = :h",
            json!({":h": s("x")}),
            json!({"gh": s("y"), "gr": s("m")})
        ),
        Err(OUTSIDE.to_string())
    );
    assert_eq!(
        single(
            "gh = :h AND gr > :r",
            json!({":h": s("x"), ":r": s("m")}),
            json!({"gh": s("x"), "gr": s("a")})
        ),
        Err(RANGE_PREDICATE.to_string())
    );
    assert_eq!(
        single(
            "gh = :h AND gr > :r",
            json!({":h": s("x"), ":r": s("m")}),
            json!({"gh": s("y"), "gr": s("a")})
        ),
        Err(RANGE_PREDICATE.to_string())
    );
    assert_eq!(
        single(
            "gh = :h AND begins_with(gr, :r)",
            json!({":h": s("x"), ":r": s("ab")}),
            json!({"gh": s("x"), "gr": s("zz")})
        ),
        Err(RANGE_PREDICATE.to_string())
    );
    assert_eq!(
        single(
            "gh = :h AND gr > :r",
            json!({":h": s("x"), ":r": s("m")}),
            json!({"gh": s("x"), "gr": s("z")})
        ),
        Ok(0)
    );
    // A hash-only index answers "outside", as a table with a sort key does.
    assert_eq!(
        query(
            &db,
            on_wide(
                Some("hashonly"),
                "gh = :h",
                json!({":h": s("x")}),
                base(json!({"gh": s("y")}))
            )
        ),
        Err(OUTSIDE.to_string())
    );
    // An LSI follows the same rule.
    let lsi = |condition: &str, values: Value, esk: Value| {
        query(&db, on_wide(Some("lsi"), condition, values, esk))
    };
    assert_eq!(
        lsi(
            "pk = :p",
            json!({":p": s("p1")}),
            json!({"pk": s("p2"), "sk": s("s1"), "lsk": s("m")})
        ),
        Err(OUTSIDE.to_string())
    );
    assert_eq!(
        lsi(
            "pk = :p AND lsk > :l",
            json!({":p": s("p1"), ":l": s("m")}),
            json!({"pk": s("p2"), "sk": s("s1"), "lsk": s("a")})
        ),
        Err(RANGE_PREDICATE.to_string())
    );
}

#[test]
fn a_multi_attribute_start_key_is_checked_on_every_key_attribute() {
    let db = wide();
    let multi = |condition: &str, values: Value, esk: Value| {
        let mut full = json!({"pk": s("p1"), "sk": s("s1")});
        for (k, v) in esk.as_object().unwrap() {
            full[k] = v.clone();
        }
        query(&db, on_wide(Some("multi"), condition, values, full))
    };
    let hh = json!({":h1": s("x"), ":h2": s("y")});
    let hha = json!({":h1": s("x"), ":h2": s("y"), ":a": s("T")});
    let key = |h1: &str, h2: &str, a: &str, b: &str| json!({"h1": s(h1), "h2": s(h2), "a": s(a), "b": s(b)});
    // The first partition key attribute differs.
    assert_eq!(
        multi("h1 = :h1 AND h2 = :h2", hh.clone(), key("q", "y", "T", "m")),
        Err(AT_MOST_ONE_ROW.to_string())
    );
    assert_eq!(
        multi(
            "h1 = :h1 AND h2 = :h2 AND a = :a",
            hha.clone(),
            key("q", "y", "F", "m")
        ),
        Err(AT_MOST_ONE_ROW.to_string())
    );
    // The last partition key attribute differs.
    assert_eq!(
        multi("h1 = :h1 AND h2 = :h2", hh.clone(), key("x", "q", "T", "m")),
        Err(OUTSIDE.to_string())
    );
    assert_eq!(
        multi(
            "h1 = :h1 AND h2 = :h2 AND a = :a",
            hha.clone(),
            key("x", "q", "F", "m")
        ),
        Err(RANGE_PREDICATE.to_string())
    );
    // Sort key attributes outside the condition.
    assert_eq!(
        multi(
            "h1 = :h1 AND h2 = :h2 AND a = :a",
            hha.clone(),
            key("x", "y", "F", "m")
        ),
        Err(RANGE_PREDICATE.to_string())
    );
    assert_eq!(
        multi(
            "h1 = :h1 AND h2 = :h2 AND a = :a AND b > :b",
            json!({":h1": s("x"), ":h2": s("y"), ":a": s("T"), ":b": s("m")}),
            key("x", "y", "T", "a")
        ),
        Err(RANGE_PREDICATE.to_string())
    );
    assert_eq!(
        multi(
            "h1 = :h1 AND h2 = :h2 AND a > :a",
            json!({":h1": s("x"), ":h2": s("y"), ":a": s("M")}),
            key("x", "y", "A", "m")
        ),
        Err(RANGE_PREDICATE.to_string())
    );
    // Inside the condition, or no sort key condition at all.
    assert_eq!(
        multi(
            "h1 = :h1 AND h2 = :h2 AND a = :a",
            hha,
            key("x", "y", "T", "m")
        ),
        Ok(0)
    );
    assert_eq!(
        multi("h1 = :h1 AND h2 = :h2", hh, key("x", "y", "Z", "z")),
        Ok(0)
    );
}
