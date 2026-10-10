//! A PartiQL `IN` with one value on a key attribute reads like `=`.
//!
//! Captured eu-west-2 and us-east-1, 2026-10-10, on a table keyed `pk` alone
//! holding three items of about 30,000 bytes: `WHERE pk IN ['a']` returns item
//! a for 4 read units through ExecuteStatement and BatchExecuteStatement and 16
//! through ExecuteTransaction, the same as `pk = 'a'`, where a scan of the
//! three items costs 11 and 44. Two statements naming item a, by `IN ['a']`
//! twice or by `=` and `IN`, are duplicates in a batch and a transaction.

use dynoxide::Database;
use dynoxide::errors::DynoxideError;

const TABLE: &str = "OneValueIn";

fn table() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value(serde_json::json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    // pk (2 + 1) + data (4 + 29,990) = 29,997 bytes: 8 strongly consistent
    // read units each, as the captured items were.
    for pk in ["a", "b", "c"] {
        db.put_item(
            serde_json::from_value(serde_json::json!({
                "TableName": TABLE,
                "Item": {"pk": {"S": pk}, "data": {"S": "x".repeat(29_990)}}
            }))
            .unwrap(),
        )
        .unwrap();
    }
    db
}

fn validation(err: DynoxideError) -> String {
    match err {
        DynoxideError::ValidationException(m) => m,
        other => panic!("expected a ValidationException, got {other:?}"),
    }
}

#[test]
fn a_one_value_in_on_the_partition_key_is_read_like_an_equality() {
    let db = table();
    for (statement, units) in [
        (format!("SELECT * FROM \"{TABLE}\" WHERE pk = 'a'"), 4.0),
        (format!("SELECT * FROM \"{TABLE}\" WHERE pk IN ['a']"), 4.0),
    ] {
        let resp = db
            .execute_statement(
                serde_json::from_value(serde_json::json!({
                    "Statement": statement, "ReturnConsumedCapacity": "TOTAL"
                }))
                .unwrap(),
            )
            .unwrap();
        let items = resp.items.unwrap();
        assert_eq!(items.len(), 1, "{statement}");
        assert_eq!(
            items[0].get("pk"),
            Some(&dynoxide::types::AttributeValue::S("a".into()))
        );
        assert_eq!(
            resp.consumed_capacity.unwrap().capacity_units,
            units,
            "{statement}"
        );
    }

    let resp = db
        .batch_execute_statement(
            serde_json::from_value(serde_json::json!({
                "Statements": [{"Statement": format!("SELECT * FROM \"{TABLE}\" WHERE pk IN ['a']")}],
                "ReturnConsumedCapacity": "TOTAL"
            }))
            .unwrap(),
        )
        .unwrap();
    assert!(resp.responses[0].item.is_some(), "the batch read item a");
    assert_eq!(resp.consumed_capacity.unwrap()[0].capacity_units, 4.0);

    let resp = db
        .execute_transaction(
            serde_json::from_value(serde_json::json!({
                "TransactStatements": [{"Statement": format!("SELECT * FROM \"{TABLE}\" WHERE pk IN ['a']")}],
                "ReturnConsumedCapacity": "TOTAL"
            }))
            .unwrap(),
        )
        .unwrap();
    assert_eq!(resp.responses.unwrap().len(), 1);
    assert_eq!(resp.consumed_capacity.unwrap()[0].capacity_units, 16.0);
}

#[test]
fn a_one_value_in_on_the_sort_key_costs_what_an_equality_costs() {
    // Captured 2026-10-10: on a table keyed pk and sk, `pk = 'a' AND sk IN
    // ['1']` costs the same units as `pk = 'a' AND sk = '1'`. Here partition a
    // holds three large items, so reading the whole partition would cost more.
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value(serde_json::json!({
            "TableName": "OneValueInSk",
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"}
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "S"}
            ],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    for sk in ["1", "2", "3"] {
        db.put_item(
            serde_json::from_value(serde_json::json!({
                "TableName": "OneValueInSk",
                "Item": {"pk": {"S": "a"}, "sk": {"S": sk}, "data": {"S": "x".repeat(29_990)}}
            }))
            .unwrap(),
        )
        .unwrap();
    }
    let units = |statement: &str| {
        let resp = db
            .execute_statement(
                serde_json::from_value(serde_json::json!({
                    "Statement": statement, "ReturnConsumedCapacity": "TOTAL"
                }))
                .unwrap(),
            )
            .unwrap();
        assert_eq!(resp.items.unwrap().len(), 1, "{statement}");
        resp.consumed_capacity.unwrap().capacity_units
    };
    let equality = units("SELECT * FROM \"OneValueInSk\" WHERE pk = 'a' AND sk = '1'");
    let one_value_in = units("SELECT * FROM \"OneValueInSk\" WHERE pk = 'a' AND sk IN ['1']");
    assert_eq!(one_value_in, equality);
    assert_eq!(equality, 4.0);
}

#[test]
fn a_one_value_in_names_the_same_item_as_an_equality() {
    let db = table();
    let in_a = format!("SELECT * FROM \"{TABLE}\" WHERE pk IN ['a']");
    let eq_a = format!("SELECT * FROM \"{TABLE}\" WHERE pk = 'a'");
    for pair in [[&in_a, &in_a], [&eq_a, &in_a]] {
        let err = db
            .batch_execute_statement(
                serde_json::from_value(serde_json::json!({
                    "Statements": [{"Statement": pair[0]}, {"Statement": pair[1]}]
                }))
                .unwrap(),
            )
            .expect_err("a batch naming item a twice");
        assert_eq!(
            validation(err),
            "Provided list of item keys contains duplicates",
            "{pair:?}"
        );
        let err = db
            .execute_transaction(
                serde_json::from_value(serde_json::json!({
                    "TransactStatements": [{"Statement": pair[0]}, {"Statement": pair[1]}]
                }))
                .unwrap(),
            )
            .expect_err("a transaction naming item a twice");
        assert_eq!(
            validation(err),
            "Transaction request cannot include multiple operations on one item",
            "{pair:?}"
        );
    }
}
