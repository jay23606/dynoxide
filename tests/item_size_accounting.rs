//! Item-size accounting for numbers, and the agreement it buys between write paths.
//!
//! DynamoDB sizes a number by its significant digits, so the figure survives the
//! normalisation that expands scientific notation on storage. The write paths
//! check the 400KB limit at different points either side of that normalisation,
//! which is only safe while the measure does not move.

use dynoxide::Database;
use dynoxide::actions::batch_write_item::{BatchWriteItemRequest, PutRequest, WriteRequest};
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::ExecuteStatementRequest;
use dynoxide::actions::put_item::PutItemRequest;
use dynoxide::actions::transact_write_items::{
    TransactPut, TransactUpdate, TransactWriteItem, TransactWriteItemsRequest,
};
use dynoxide::actions::update_item::{AttributeValueUpdate, UpdateItemRequest};
use dynoxide::types::*;
use std::collections::HashMap;

fn make_db() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(CreateTableRequest {
        table_name: "Items".to_string(),
        key_schema: vec![KeySchemaElement {
            attribute_name: "pk".to_string(),
            key_type: KeyType::HASH,
        }],
        attribute_definitions: vec![AttributeDefinition {
            attribute_name: "pk".to_string(),
            attribute_type: ScalarAttributeType::S,
        }],
        ..Default::default()
    })
    .unwrap();
    db
}

/// An item whose numbers all expand hugely when normalised. Under a measure
/// taken over the digits of the stored string this crosses the limit; under
/// DynamoDB's it stays well under.
fn scientific_notation_item(pk: &str, count: usize) -> HashMap<String, AttributeValue> {
    let mut item = HashMap::new();
    item.insert("pk".to_string(), AttributeValue::S(pk.to_string()));
    for i in 0..count {
        item.insert(format!("n{i:04}"), AttributeValue::N("1E125".to_string()));
    }
    item
}

#[test]
fn every_write_path_accepts_an_item_that_normalising_expands() {
    let db = make_db();
    let item = scientific_notation_item("k", 8000);

    db.put_item(PutItemRequest {
        table_name: "Items".to_string(),
        item: item.clone(),
        ..Default::default()
    })
    .expect("PutItem");

    db.batch_write_item(BatchWriteItemRequest {
        request_items: HashMap::from([(
            "Items".to_string(),
            vec![WriteRequest {
                put_request: Some(PutRequest {
                    item: scientific_notation_item("k-batch", 8000),
                }),
                delete_request: None,
            }],
        )]),
        ..Default::default()
    })
    .expect("BatchWriteItem");

    db.transact_write_items(TransactWriteItemsRequest {
        transact_items: vec![TransactWriteItem {
            put: Some(TransactPut {
                table_name: "Items".to_string(),
                item: scientific_notation_item("k-transact", 8000),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    })
    .expect("TransactWriteItems");
}

#[test]
fn the_size_recorded_for_a_stored_item_is_the_size_that_was_checked() {
    // The reported bug: PutItem admitted an item on one measure and the engine
    // then recorded it under another, feeding table statistics and the item
    // collection size estimate a figure the limit had never been applied to.
    let db = make_db();
    let item = scientific_notation_item("k", 8000);
    let checked = item_size(&item);

    db.put_item(PutItemRequest {
        table_name: "Items".to_string(),
        item,
        ..Default::default()
    })
    .unwrap();

    let stats = db.table_stats().unwrap();
    let stored = stats.iter().find(|s| s.table_name == "Items").unwrap();
    assert_eq!(stored.size_bytes as usize, checked);
    assert!(checked < MAX_ITEM_SIZE);
}

#[test]
fn update_item_sizes_expanding_numbers_the_same_way() {
    // Built the other way round from the PutItem cases: a base item already near
    // the limit, then an update that adds numbers which expand on storage. The
    // update expression stays small, so this isolates the size measure from the
    // separate 4KB expression limit. Sized to clear UpdateItem's own lower
    // ceiling, which the tests below cover.
    let db = make_db();
    let mut base = HashMap::new();
    base.insert("pk".to_string(), AttributeValue::S("k".to_string()));
    base.insert("b".to_string(), AttributeValue::S("x".repeat(408_000)));
    db.put_item(PutItemRequest {
        table_name: "Items".to_string(),
        item: base,
        ..Default::default()
    })
    .expect("PutItem base");

    let mut sets = Vec::new();
    let mut values = HashMap::new();
    for i in 0..50 {
        sets.push(format!("n{i:04} = :v{i}"));
        values.insert(format!(":v{i}"), AttributeValue::N("1E125".to_string()));
    }

    db.update_item(UpdateItemRequest {
        table_name: "Items".to_string(),
        key: HashMap::from([("pk".to_string(), AttributeValue::S("k".to_string()))]),
        update_expression: Some(format!("SET {}", sets.join(", "))),
        expression_attribute_values: Some(values),
        ..Default::default()
    })
    .expect("UpdateItem");

    // Same item shape through PutItem, which measured it at a different point.
    let stats = db.table_stats().unwrap();
    let stored = stats.iter().find(|s| s.table_name == "Items").unwrap();
    assert!((stored.size_bytes as usize) < MAX_ITEM_SIZE);
}

/// One `SET` of a blob of `blob_len` bytes onto key `pk`, which upserts.
fn set_blob(db: &Database, pk: &str, blob_len: usize) -> Result<(), String> {
    db.update_item(UpdateItemRequest {
        table_name: "Items".to_string(),
        key: HashMap::from([("pk".to_string(), AttributeValue::S(pk.to_string()))]),
        update_expression: Some("SET #b = :v".to_string()),
        expression_attribute_names: Some(HashMap::from([("#b".to_string(), "b".to_string())])),
        expression_attribute_values: Some(HashMap::from([(
            ":v".to_string(),
            AttributeValue::S("x".repeat(blob_len)),
        )])),
        ..Default::default()
    })
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[test]
fn update_item_is_measured_against_a_lower_ceiling_than_put_item() {
    // Captured against eu-west-2. An update is not sized the way a put is: it
    // is charged for what its request carries, three bytes for the statement
    // plus nineteen, the target and the value for a SET. With a one-character
    // key the resulting item may reach 409,581, where PutItem accepts 409,600.
    //
    //   charge         = 3 + 19 + "b" (1) + blob
    //   resulting item = pk (2 + 1) + "b" (1) + blob
    let db = make_db();
    let ceiling = 409_581 - 4;

    set_blob(&db, "k", ceiling).expect("at the ceiling");
    let err = set_blob(&db, "k2", ceiling + 1).expect_err("one byte over");
    assert_eq!(
        err,
        "Item size to update has exceeded the maximum allowed size"
    );

    // The same finished item is within reach of PutItem, which is the asymmetry
    // the capture found: an item can be puttable and not updatable.
    let mut item = HashMap::new();
    item.insert("pk".to_string(), AttributeValue::S("k3".to_string()));
    item.insert("b".to_string(), AttributeValue::S("x".repeat(ceiling + 1)));
    db.put_item(PutItemRequest {
        table_name: "Items".to_string(),
        item,
        ..Default::default()
    })
    .expect("PutItem accepts what UpdateItem refused");
}

#[test]
fn each_update_action_adds_to_what_the_gate_measures() {
    // A second SET costs nineteen plus its target and value, so the ceiling
    // drops by twenty-one.
    let db = make_db();
    let two_set_ceiling = 409_581 - 19 - 4 - 2; // second attribute is name + 1 byte

    let run = |pk: &str, blob_len: usize| {
        db.update_item(UpdateItemRequest {
            table_name: "Items".to_string(),
            key: HashMap::from([("pk".to_string(), AttributeValue::S(pk.to_string()))]),
            update_expression: Some("SET #b = :v, #c = :w".to_string()),
            expression_attribute_names: Some(HashMap::from([
                ("#b".to_string(), "b".to_string()),
                ("#c".to_string(), "c".to_string()),
            ])),
            expression_attribute_values: Some(HashMap::from([
                (":v".to_string(), AttributeValue::S("x".repeat(blob_len))),
                (":w".to_string(), AttributeValue::S("z".to_string())),
            ])),
            ..Default::default()
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
    };

    run("k", two_set_ceiling).expect("at the two-action ceiling");
    run("k2", two_set_ceiling + 1).expect_err("one byte over");
}

#[test]
fn a_transacted_write_reports_an_oversized_item_the_way_dynamodb_does() {
    // Captured against eu-west-2. The two actions differ, because a put's size is
    // knowable from the request and an update's is not:
    //   Put    -> a plain ValidationException, before the transaction runs
    //   Update -> a cancellation reason, and the message names it as an update
    let db = make_db();

    let mut item = HashMap::new();
    item.insert("pk".to_string(), AttributeValue::S("k".to_string()));
    item.insert("b".to_string(), AttributeValue::S("x".repeat(409_601)));
    let err = db
        .transact_write_items(TransactWriteItemsRequest {
            transact_items: vec![TransactWriteItem {
                put: Some(TransactPut {
                    table_name: "Items".to_string(),
                    item,
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Item size has exceeded the maximum allowed size"
    );

    let err = db
        .transact_write_items(TransactWriteItemsRequest {
            transact_items: vec![TransactWriteItem {
                update: Some(TransactUpdate {
                    table_name: "Items".to_string(),
                    key: HashMap::from([("pk".to_string(), AttributeValue::S("k2".to_string()))]),
                    update_expression: "SET b = :v".to_string(),
                    expression_attribute_values: Some(HashMap::from([(
                        ":v".to_string(),
                        AttributeValue::S("x".repeat(409_601)),
                    )])),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap_err();
    match err {
        dynoxide::errors::DynoxideError::TransactionCanceledException(_, reasons) => {
            assert_eq!(reasons[0].code, "ValidationError");
            assert_eq!(
                reasons[0].message.as_deref(),
                Some("Item size to update has exceeded the maximum allowed size")
            );
        }
        other => panic!("expected a cancellation, got {other:?}"),
    }
}

#[test]
fn a_transacted_update_is_not_charged_the_standalone_update_overhead() {
    // The standalone UpdateItem is charged for what its request carries; a
    // transacted one is measured flat against the resulting item. So an item
    // that UpdateItem refuses goes through inside a transaction.
    let db = make_db();
    let just_over_update_ceiling = 409_581 - 4 + 1;

    set_blob(&db, "k", just_over_update_ceiling).expect_err("UpdateItem refuses it");

    db.transact_write_items(TransactWriteItemsRequest {
        transact_items: vec![TransactWriteItem {
            update: Some(TransactUpdate {
                table_name: "Items".to_string(),
                key: HashMap::from([("pk".to_string(), AttributeValue::S("k".to_string()))]),
                update_expression: "SET b = :v".to_string(),
                expression_attribute_values: Some(HashMap::from([(
                    ":v".to_string(),
                    AttributeValue::S("x".repeat(just_over_update_ceiling)),
                )])),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    })
    .expect("a transacted update accepts it");
}

#[test]
fn an_update_can_never_store_an_item_over_the_hard_limit() {
    // An update is held both to its request charge and to the finished item's
    // own size. The key never enters the charge, so past a 20-byte key the
    // item's own size binds and the ceiling is flat at 400KB however long the
    // key gets, which was measured out to 1024 bytes. Without the item check, a
    // long key would let an over-limit item through and its size would then
    // feed table statistics.
    let db = Database::memory().unwrap();
    db.create_table(CreateTableRequest {
        table_name: "Big".to_string(),
        key_schema: vec![KeySchemaElement {
            attribute_name: "pk".to_string(),
            key_type: KeyType::HASH,
        }],
        attribute_definitions: vec![AttributeDefinition {
            attribute_name: "pk".to_string(),
            attribute_type: ScalarAttributeType::S,
        }],
        ..Default::default()
    })
    .unwrap();

    let key = "k".repeat(2000);
    let err = db
        .update_item(UpdateItemRequest {
            table_name: "Big".to_string(),
            key: HashMap::from([("pk".to_string(), AttributeValue::S(key.clone()))]),
            update_expression: Some("SET #b = :v".to_string()),
            expression_attribute_names: Some(HashMap::from([("#b".to_string(), "b".to_string())])),
            expression_attribute_values: Some(HashMap::from([(
                ":v".to_string(),
                // resulting item = pk (2 + 2000) + "b" (1) + blob, one byte over
                AttributeValue::S("x".repeat(MAX_ITEM_SIZE - 2003 + 1)),
            )])),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Item size to update has exceeded the maximum allowed size"
    );

    let stats = db.table_stats().unwrap();
    let big = stats.iter().find(|s| s.table_name == "Big").unwrap();
    assert_eq!(big.item_count, 0, "nothing over the limit was stored");

    // The crossover, captured against eu-west-2: with a 20-byte key the finished
    // item (22 bytes of key, then "b" and the blob) is exactly as large as the
    // charge (3 + 19 + "b" + the blob), so the two checks meet and the ceiling
    // lands on 400KB itself. Below this the charge binds; above it the item's
    // own size does.
    let key = "k".repeat(20);
    let at_ceiling = MAX_ITEM_SIZE - 2 - 20 - 1;
    db.update_item(UpdateItemRequest {
        table_name: "Big".to_string(),
        key: HashMap::from([("pk".to_string(), AttributeValue::S(key.clone()))]),
        update_expression: Some("SET #b = :v".to_string()),
        expression_attribute_names: Some(HashMap::from([("#b".to_string(), "b".to_string())])),
        expression_attribute_values: Some(HashMap::from([(
            ":v".to_string(),
            AttributeValue::S("x".repeat(at_ceiling)),
        )])),
        ..Default::default()
    })
    .expect("a 20-byte key reaches exactly 400KB");

    db.update_item(UpdateItemRequest {
        table_name: "Big".to_string(),
        key: HashMap::from([("pk".to_string(), AttributeValue::S("k2".repeat(10)))]),
        update_expression: Some("SET #b = :v".to_string()),
        expression_attribute_names: Some(HashMap::from([("#b".to_string(), "b".to_string())])),
        expression_attribute_values: Some(HashMap::from([(
            ":v".to_string(),
            AttributeValue::S("x".repeat(at_ceiling + 1)),
        )])),
        ..Default::default()
    })
    .expect_err("one byte past it");
}

/// Run a PartiQL statement, returning the error text if it was refused.
fn partiql(db: &Database, statement: String) -> Result<(), String> {
    partiql_with(db, statement, Vec::new())
}

/// Run a PartiQL statement with `?` parameters. A value near 400KB has to
/// travel as a parameter: DynamoDB refuses a statement over 8,192 characters
/// before it reads it, so a literal that size never reaches the size check.
fn partiql_with(
    db: &Database,
    statement: String,
    parameters: Vec<AttributeValue>,
) -> Result<(), String> {
    db.execute_statement(ExecuteStatementRequest {
        statement,
        parameters: (!parameters.is_empty()).then_some(parameters),
        ..Default::default()
    })
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[test]
fn the_partiql_write_paths_hold_the_item_size_limit() {
    // Both used to compute a size, hand it to storage and never check it, so the
    // symptom this issue is about survived on two more write surfaces: an
    // over-limit row stored, and its size fed to table statistics.
    let db = make_db();
    let over = "x".repeat(MAX_ITEM_SIZE + 1);

    let err = partiql_with(
        &db,
        "INSERT INTO \"Items\" VALUE {'pk': 'k1', 'b': ?}".to_string(),
        vec![AttributeValue::S(over.clone())],
    )
    .expect_err("INSERT over the limit");
    assert_eq!(err, "Item size has exceeded the maximum allowed size");

    partiql(&db, "INSERT INTO \"Items\" VALUE {'pk': 'k2'}".to_string()).unwrap();
    let err = partiql_with(
        &db,
        "UPDATE \"Items\" SET b = ? WHERE pk = 'k2'".to_string(),
        vec![AttributeValue::S(over)],
    )
    .expect_err("UPDATE over the limit");
    assert_eq!(
        err,
        "Item size to update has exceeded the maximum allowed size"
    );

    let stats = db.table_stats().unwrap();
    let items = stats.iter().find(|s| s.table_name == "Items").unwrap();
    assert!(
        (items.size_bytes as usize) < MAX_ITEM_SIZE,
        "nothing over the limit was stored"
    );
}

#[test]
fn a_partiql_update_is_measured_flat_like_an_insert() {
    // Captured against eu-west-2: a PartiQL UPDATE accepts a resulting item of
    // 409,600, which the standalone UpdateItem refuses at the same key because
    // it charges the request rather than the item. Of the write surfaces, only
    // UpdateItem carries that rule.
    let db = make_db();
    partiql(&db, "INSERT INTO \"Items\" VALUE {'pk': 'k'}".to_string()).unwrap();

    // resulting item = pk (2 + 1) + "b" (1) + blob, landing exactly on the limit
    let blob = "x".repeat(MAX_ITEM_SIZE - 4);
    partiql_with(
        &db,
        "UPDATE \"Items\" SET b = ? WHERE pk = 'k'".to_string(),
        vec![AttributeValue::S(blob)],
    )
    .expect("PartiQL UPDATE accepts an item at the flat limit");

    set_blob(&db, "k2", MAX_ITEM_SIZE - 4).expect_err("UpdateItem refuses the same item");
}

/// Store `item` at key `pk`, replacing whatever was there.
fn seed(db: &Database, pk: &str, mut item: HashMap<String, AttributeValue>) {
    item.insert("pk".to_string(), AttributeValue::S(pk.to_string()));
    db.put_item(PutItemRequest {
        table_name: "Items".to_string(),
        item,
        ..Default::default()
    })
    .unwrap();
}

/// An UpdateItem on `Items` at key `pk`, returning the error text if refused.
fn update(
    db: &Database,
    pk: &str,
    expression: &str,
    values: Vec<(&str, AttributeValue)>,
) -> Result<(), String> {
    db.update_item(UpdateItemRequest {
        table_name: "Items".to_string(),
        key: HashMap::from([("pk".to_string(), AttributeValue::S(pk.to_string()))]),
        update_expression: Some(expression.to_string()),
        expression_attribute_values: Some(
            values
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        ),
        ..Default::default()
    })
    .map(|_| ())
    .map_err(|e| e.to_string())
}

const UPDATE_REFUSAL: &str = "Item size to update has exceeded the maximum allowed size";

#[test]
fn an_update_does_not_charge_an_attribute_it_leaves_alone() {
    // Captured against eu-west-2 on 9 October 2026, and pinned by the Parity
    // Suite: an untouched 10-byte attribute lifts the accepted finished item by
    // its own 11 bytes, from 409,581 to 409,592, because the update is charged
    // for what its request carries and the stored item never enters the charge.
    //
    //   charge   = 3 + SET (19 + "b" 1 + blob)
    //   finished = pk (2 + 1) + "u" (1 + 10) + "b" (1) + blob
    let db = make_db();
    let untouched = || HashMap::from([("u".to_string(), AttributeValue::S("y".repeat(10)))]);
    let ceiling = 409_577;

    seed(&db, "k", untouched());
    set_blob(&db, "k", ceiling).expect("accepted at a finished item of 409,592");
    seed(&db, "k", untouched());
    let err = set_blob(&db, "k", ceiling + 1).expect_err("one byte over the charge");
    assert_eq!(err, UPDATE_REFUSAL);
}

#[test]
fn a_set_through_a_list_index_costs_one_byte_more_than_a_plain_set() {
    // Pinned by the Parity Suite and captured against eu-west-2: writing the
    // same finished list through `d[0]` rather than as a whole costs one byte
    // more, because the target `d[0]` charges 1 + 4 + 1 where `d` charges 1,
    // and the value it writes is the bare string rather than the list around it.
    //
    //   SET d = :whole   3 + 19 + 1 + list (3 + 1 + blob)
    //   SET d[0] = :pad  3 + 19 + 6 + blob
    let db = make_db();
    let one_element = || {
        HashMap::from([(
            "d".to_string(),
            AttributeValue::L(vec![AttributeValue::S("x".to_string())]),
        )])
    };
    let whole = |len: usize| AttributeValue::L(vec![AttributeValue::S("x".repeat(len))]);
    let ceiling = 409_573;

    seed(&db, "k", one_element());
    update(&db, "k", "SET d = :whole", vec![(":whole", whole(ceiling))])
        .expect("the plain SET reaches the ceiling");
    seed(&db, "k", one_element());
    let err = update(
        &db,
        "k",
        "SET d = :whole",
        vec![(":whole", whole(ceiling + 1))],
    )
    .expect_err("the plain SET one byte over");
    assert_eq!(err, UPDATE_REFUSAL);

    seed(&db, "k", one_element());
    let err = update(
        &db,
        "k",
        "SET d[0] = :pad",
        vec![(":pad", AttributeValue::S("x".repeat(ceiling)))],
    )
    .expect_err("the list-index SET is refused where the plain SET is accepted");
    assert_eq!(err, UPDATE_REFUSAL);
    seed(&db, "k", one_element());
    update(
        &db,
        "k",
        "SET d[0] = :pad",
        vec![(":pad", AttributeValue::S("x".repeat(ceiling - 1)))],
    )
    .expect("and accepted one byte lower");
}

#[test]
fn an_over_limit_charge_is_refused_before_the_condition_is_evaluated() {
    // Captured against eu-west-2 on 9 October 2026: an update whose request
    // charge is over the limit is refused for its size whether its condition
    // would fail, would pass, or is absent. The charge needs nothing from the
    // stored item, so it is checked before the item is read.
    let db = make_db();
    let huge = || AttributeValue::S("h".repeat(409_700));
    for condition in [
        Some("attribute_exists(nope)"),
        Some("attribute_exists(c)"),
        None,
    ] {
        seed(
            &db,
            "k",
            HashMap::from([("c".to_string(), AttributeValue::S("x".to_string()))]),
        );
        let err = db
            .update_item(UpdateItemRequest {
                table_name: "Items".to_string(),
                key: HashMap::from([("pk".to_string(), AttributeValue::S("k".to_string()))]),
                update_expression: Some("SET c = if_not_exists(c, :huge)".to_string()),
                condition_expression: condition.map(str::to_string),
                expression_attribute_values: Some(HashMap::from([(":huge".to_string(), huge())])),
                ..Default::default()
            })
            .expect_err("over the limit");
        assert!(
            matches!(&err, dynoxide::errors::DynoxideError::ValidationException(m) if m == UPDATE_REFUSAL),
            "condition {condition:?}: expected a top-level size refusal, got {err:?}"
        );
    }
}

#[test]
fn the_charge_is_checked_after_request_validation_and_before_the_update_applies() {
    // Captured against eu-west-2 on 9 October 2026, pairing an over-limit
    // charge with one other problem. Request validation still answers first:
    // unused and undefined names and values, a missing table, a key that does
    // not match the schema, a malformed or undefined condition, and an invalid
    // ReturnValues. The charge answers ahead of everything that happens once the
    // update is under way: a SET on a key attribute, a path or type the stored
    // item cannot take, and a condition that fails.
    let db = make_db();
    let huge = || AttributeValue::S("h".repeat(409_700));
    let base = |expression: &str| UpdateItemRequest {
        table_name: "Items".to_string(),
        key: HashMap::from([("pk".to_string(), AttributeValue::S("k".to_string()))]),
        update_expression: Some(expression.to_string()),
        expression_attribute_values: Some(HashMap::from([(":v".to_string(), huge())])),
        ..Default::default()
    };
    let with_value = |mut r: UpdateItemRequest, name: &str, v: AttributeValue| {
        r.expression_attribute_values
            .as_mut()
            .unwrap()
            .insert(name.to_string(), v);
        r
    };
    let small = || AttributeValue::S("s".to_string());

    let validation_first: Vec<(&str, UpdateItemRequest, &str)> = vec![
        (
            "unused value",
            with_value(base("SET c = :v"), ":unused", small()),
            "unused in expressions: keys: {:unused}",
        ),
        (
            "unused name",
            UpdateItemRequest {
                expression_attribute_names: Some(HashMap::from([(
                    "#u".to_string(),
                    "u".to_string(),
                )])),
                ..base("SET c = :v")
            },
            "unused in expressions: keys: {#u}",
        ),
        (
            "undefined value",
            base("SET c = :v, d = :nope"),
            "attribute value: :nope",
        ),
        (
            "undefined name",
            with_value(base("SET c = :v, #nope = :w"), ":w", small()),
            "attribute name: #nope",
        ),
        (
            "missing table",
            UpdateItemRequest {
                table_name: "Missing".to_string(),
                ..base("SET c = :v")
            },
            "Requested resource not found",
        ),
        (
            "key attribute name wrong",
            UpdateItemRequest {
                key: HashMap::from([("pk2".to_string(), AttributeValue::S("k".to_string()))]),
                ..base("SET c = :v")
            },
            "The provided key element does not match the schema",
        ),
        (
            "key attribute type wrong",
            UpdateItemRequest {
                key: HashMap::from([("pk".to_string(), AttributeValue::N("1".to_string()))]),
                ..base("SET c = :v")
            },
            "The provided key element does not match the schema",
        ),
        (
            "condition syntax error",
            UpdateItemRequest {
                condition_expression: Some("attribute_exists(".to_string()),
                ..base("SET c = :v")
            },
            "Invalid ConditionExpression",
        ),
        (
            "condition undefined value",
            UpdateItemRequest {
                condition_expression: Some("c = :nope".to_string()),
                ..base("SET c = :v")
            },
            "attribute value: :nope",
        ),
        (
            "invalid ReturnValues",
            UpdateItemRequest {
                return_values: Some("BOGUS".to_string()),
                ..base("SET c = :v")
            },
            // The generic form, captured with this ordering run (eu-west-2,
            // 2026-10-09).
            "Member must satisfy enum value set: [ALL_OLD, UPDATED_OLD, ALL_NEW, UPDATE_NEW, NONE]",
        ),
    ];
    let charge_first: Vec<(&str, UpdateItemRequest)> = vec![
        (
            "SET on the key attribute",
            with_value(base("SET pk = :k, c = :v"), ":k", small()),
        ),
        (
            "SET through a string",
            with_value(base("SET c = :v, s.x = :w"), ":w", small()),
        ),
        (
            "ADD to a string",
            with_value(
                base("SET c = :v ADD s :one"),
                ":one",
                AttributeValue::N("1".to_string()),
            ),
        ),
        (
            "list index on a missing list",
            with_value(base("SET c = :v, nolist[0] = :w"), ":w", small()),
        ),
        (
            "condition fails",
            UpdateItemRequest {
                condition_expression: Some("attribute_exists(nope)".to_string()),
                ..base("SET c = :v")
            },
        ),
    ];

    let reseed = || {
        seed(
            &db,
            "k",
            HashMap::from([
                ("c".to_string(), AttributeValue::S("x".to_string())),
                ("s".to_string(), AttributeValue::S("str".to_string())),
            ]),
        )
    };
    for (what, request, expected) in validation_first {
        reseed();
        let err = db.update_item(request).expect_err(what).to_string();
        assert!(
            err.contains(expected),
            "{what}: request validation should answer first, got: {err}"
        );
    }
    for (what, request) in charge_first {
        reseed();
        let err = db.update_item(request).expect_err(what).to_string();
        assert_eq!(
            err, UPDATE_REFUSAL,
            "{what}: the charge should answer first"
        );
    }
}

#[test]
fn attribute_updates_are_measured_by_the_finished_item_alone() {
    // Captured against eu-west-2 on 9 October 2026: the legacy AttributeUpdates
    // parameter carries no per-action charge. With a one-byte key it reaches a
    // finished item of exactly 409,600 with one PUT, with ten more PUTs beside
    // it, and with PUT, ADD and DELETE together.
    let db = make_db();
    let put_blob = |pk: &str, len: usize, extra: Vec<(&str, &str, Option<AttributeValue>)>| {
        let mut updates = HashMap::from([(
            "b".to_string(),
            AttributeValueUpdate {
                action: "PUT".to_string(),
                value: Some(AttributeValue::S("x".repeat(len))),
            },
        )]);
        for (name, action, value) in extra {
            updates.insert(
                name.to_string(),
                AttributeValueUpdate {
                    action: action.to_string(),
                    value,
                },
            );
        }
        db.update_item(UpdateItemRequest {
            table_name: "Items".to_string(),
            key: HashMap::from([("pk".to_string(), AttributeValue::S(pk.to_string()))]),
            attribute_updates: Some(updates),
            ..Default::default()
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
    };

    // pk (2 + 1) + "b" (1) + blob
    put_blob("k", MAX_ITEM_SIZE - 4, vec![]).expect("a single PUT reaches 409,600");
    let err = put_blob("k2", MAX_ITEM_SIZE - 3, vec![]).expect_err("409,601");
    assert_eq!(err, UPDATE_REFUSAL);

    // A DELETE that removes a whole attribute frees its bytes for the finished
    // item and adds nothing of its own.
    seed(
        &db,
        "k3",
        HashMap::from([
            ("n".to_string(), AttributeValue::N("1".to_string())),
            ("s".to_string(), AttributeValue::S("y".repeat(10))),
        ]),
    );
    // pk (2 + 2) + "n" (1 + 2) + "b" (1) + blob, with "s" deleted
    put_blob(
        "k3",
        MAX_ITEM_SIZE - 8,
        vec![
            ("n", "ADD", Some(AttributeValue::N("1".to_string()))),
            ("s", "DELETE", None),
        ],
    )
    .expect("PUT, ADD and DELETE together reach 409,600");
}
