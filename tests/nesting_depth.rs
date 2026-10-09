//! Document nesting is capped at 32 levels, counting the top-level attribute as
//! level 1. Real DynamoDB rejects deeper values with a ValidationException, on both
//! the stored-item path (PutItem) and the ExpressionAttributeValue path, before the
//! expression is evaluated. Boundary and message captured against real DynamoDB
//! (eu-west-2, 2026-06): accepts 31 map-wraps, rejects 32.

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::delete_item::DeleteItemRequest;
use dynoxide::actions::put_item::PutItemRequest;
use dynoxide::actions::update_item::UpdateItemRequest;
use dynoxide::types::*;
use std::collections::HashMap;

const NEST_MSG: &str = "Nesting Levels have exceeded supported limits";
const BARE_NEST: &str = "Nesting Levels have exceeded supported limits: \
                         Attributes in the item have nested levels beyond supported limit";
const ENVELOPED_NEST: &str = "1 validation error detected: \
                              Nesting Levels have exceeded supported limits: \
                              Attributes in the item have nested levels beyond supported limit";

fn make_db() -> Database {
    Database::memory().unwrap()
}

fn create_table(db: &Database) {
    db.create_table(CreateTableRequest {
        table_name: "Tbl".to_string(),
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
}

/// Wrap a scalar leaf in `depth` single-key maps. depth=1 -> M{ n: S("leaf") }.
fn deep_map(depth: usize) -> AttributeValue {
    let mut v = AttributeValue::S("leaf".into());
    for _ in 0..depth {
        v = AttributeValue::M(HashMap::from([("n".to_string(), v)]));
    }
    v
}

/// Wrap a scalar leaf in `depth` single-element lists.
fn deep_list(depth: usize) -> AttributeValue {
    let mut v = AttributeValue::S("leaf".into());
    for _ in 0..depth {
        v = AttributeValue::L(vec![v]);
    }
    v
}

fn put_with_data(db: &Database, pk: &str, data: AttributeValue) -> dynoxide::errors::Result<()> {
    db.put_item(PutItemRequest {
        table_name: "Tbl".to_string(),
        item: HashMap::from([
            ("pk".to_string(), AttributeValue::S(pk.into())),
            ("data".to_string(), data),
        ]),
        ..Default::default()
    })
    .map(|_| ())
}

fn update_with_deep_condition(db: &Database, depth: usize) -> dynoxide::errors::Result<()> {
    db.update_item(UpdateItemRequest {
        table_name: "Tbl".to_string(),
        key: HashMap::from([("pk".to_string(), AttributeValue::S("k1".into()))]),
        update_expression: Some("SET touched = :t".into()),
        condition_expression: Some("#d = :deep".into()),
        expression_attribute_names: Some(HashMap::from([("#d".to_string(), "data".to_string())])),
        expression_attribute_values: Some(HashMap::from([
            (":t".to_string(), AttributeValue::S("y".into())),
            (":deep".to_string(), deep_map(depth)),
        ])),
        ..Default::default()
    })
    .map(|_| ())
}

// --- Stored item (PutItem) ---

#[test]
fn stored_item_accepts_31_levels() {
    let db = make_db();
    create_table(&db);
    put_with_data(&db, "ok", deep_map(31)).expect("31 levels should store");
}

#[test]
fn stored_item_rejects_32_levels() {
    let db = make_db();
    create_table(&db);
    let err = put_with_data(&db, "bad", deep_map(32)).unwrap_err();
    assert!(
        err.to_string().contains(NEST_MSG),
        "expected nesting ValidationException, got: {err}"
    );
}

#[test]
fn stored_item_nesting_message_is_enveloped() {
    // PutItem now wraps the nesting rejection in the request-validation
    // envelope, for a map or a list. It was bare when captured in eu-west-2 in
    // June; us-east-1 and eu-west-2 both enveloped it on 2026-10-09.
    let db = make_db();
    create_table(&db);
    for value in [deep_map(32), deep_list(32)] {
        let err = put_with_data(&db, "bad", value).unwrap_err();
        assert_eq!(err.to_string(), ENVELOPED_NEST);
    }
}

#[test]
fn expression_value_nesting_message_is_enveloped_on_single_item_writes() {
    // A 32-level ExpressionAttributeValues entry on PutItem, UpdateItem and
    // DeleteItem. Captured us-east-1 and eu-west-2, 2026-10-09; UpdateItem
    // also in 31 of 33 regions on 2026-10-07.
    let db = make_db();
    create_table(&db);
    put_with_data(&db, "k1", AttributeValue::S("x".into())).unwrap();
    let values = || Some(HashMap::from([(":deep".to_string(), deep_map(32))]));

    let err = db
        .put_item(PutItemRequest {
            table_name: "Tbl".to_string(),
            item: HashMap::from([("pk".to_string(), AttributeValue::S("p".into()))]),
            condition_expression: Some("a = :deep".into()),
            expression_attribute_values: values(),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(err.to_string(), ENVELOPED_NEST, "PutItem condition");

    let err = db
        .update_item(UpdateItemRequest {
            table_name: "Tbl".to_string(),
            key: HashMap::from([("pk".to_string(), AttributeValue::S("k1".into()))]),
            update_expression: Some("SET d = :deep".into()),
            expression_attribute_values: values(),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(err.to_string(), ENVELOPED_NEST, "UpdateItem SET");

    let err = update_with_deep_condition(&db, 32).unwrap_err();
    assert_eq!(err.to_string(), ENVELOPED_NEST, "UpdateItem condition");

    let err = db
        .delete_item(DeleteItemRequest {
            table_name: "Tbl".to_string(),
            key: HashMap::from([("pk".to_string(), AttributeValue::S("k1".into()))]),
            condition_expression: Some("a = :deep".into()),
            expression_attribute_values: values(),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(err.to_string(), ENVELOPED_NEST, "DeleteItem condition");
}

#[test]
fn update_that_nests_the_stored_item_too_deep_gets_the_short_message() {
    // Writing a 2-level value 31 levels down a 30-level attribute leaves the
    // item too deep. UpdateItem answers with the short form of the message,
    // bare. Captured us-east-1 and eu-west-2, 2026-10-09.
    let db = make_db();
    create_table(&db);
    put_with_data(&db, "k1", deep_map(30)).unwrap();
    let path = format!("#d{}", ".n".repeat(30));
    let err = db
        .update_item(UpdateItemRequest {
            table_name: "Tbl".to_string(),
            key: HashMap::from([("pk".to_string(), AttributeValue::S("k1".into()))]),
            update_expression: Some(format!("SET {path} = :v")),
            expression_attribute_names: Some(HashMap::from([(
                "#d".to_string(),
                "data".to_string(),
            )])),
            expression_attribute_values: Some(HashMap::from([(":v".to_string(), deep_map(2))])),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Nesting Levels have exceeded supported limits"
    );
}

#[test]
fn batch_write_keeps_the_bare_message() {
    // Captured us-east-1 and eu-west-2, 2026-10-09.
    let db = make_db();
    create_table(&db);
    let err = db
        .batch_write_item(
            serde_json::from_value(serde_json::json!({
                "RequestItems": {"Tbl": [{"PutRequest": {"Item": {
                    "pk": {"S": "b"},
                    "data": serde_json::to_value(deep_map(32)).unwrap(),
                }}}]}
            }))
            .unwrap(),
        )
        .unwrap_err();
    assert_eq!(err.to_string(), BARE_NEST);
}

// --- ExpressionAttributeValue (UpdateItem ConditionExpression) ---

#[test]
fn condition_eav_accepts_31_levels_and_evaluates() {
    let db = make_db();
    create_table(&db);
    // Seed an item with no `data` attribute, so the condition is false once evaluated.
    db.put_item(PutItemRequest {
        table_name: "Tbl".to_string(),
        item: HashMap::from([("pk".to_string(), AttributeValue::S("k1".into()))]),
        ..Default::default()
    })
    .unwrap();

    let err = update_with_deep_condition(&db, 31).unwrap_err();
    let msg = err.to_string();
    // A 31-level value is accepted and the condition is evaluated, so this is a
    // conditional failure, NOT a nesting ValidationException.
    assert!(
        msg.contains("conditional request failed"),
        "expected conditional failure, got: {msg}"
    );
    assert!(
        !msg.contains(NEST_MSG),
        "31 levels must not trip the nesting cap"
    );
}

#[test]
fn condition_eav_rejects_32_levels() {
    let db = make_db();
    create_table(&db);
    db.put_item(PutItemRequest {
        table_name: "Tbl".to_string(),
        item: HashMap::from([("pk".to_string(), AttributeValue::S("k1".into()))]),
        ..Default::default()
    })
    .unwrap();

    let err = update_with_deep_condition(&db, 32).unwrap_err();
    assert!(
        err.to_string().contains(NEST_MSG),
        "expected nesting ValidationException, got: {err}"
    );
}

// --- Far past the limit (in-process; over HTTP a body this deep also has to
// get through the JSON parser) ---

#[test]
fn deep_item_keeps_the_envelope_to_62_levels_and_drops_it_from_63() {
    // A second check takes over between 62 and 63 levels and sends the message
    // bare. Captured us-east-1 and eu-west-2, 2026-10-09, for maps and lists.
    let db = make_db();
    create_table(&db);
    for value in [deep_map(61), deep_map(62), deep_list(62)] {
        let err = put_with_data(&db, "bad", value).unwrap_err();
        assert_eq!(err.to_string(), ENVELOPED_NEST);
    }
    for value in [deep_map(63), deep_list(63), deep_map(80)] {
        let err = put_with_data(&db, "bad", value).unwrap_err();
        assert_eq!(err.to_string(), BARE_NEST);
    }
}

#[test]
fn expression_value_from_63_levels_is_refused_naming_its_key() {
    // On PutItem, UpdateItem and DeleteItem alike, a value at 62 levels gets the
    // enveloped message and one at 63 the "contains invalid value" form naming
    // its key, bare. Captured us-east-1 and eu-west-2, 2026-10-09.
    let db = make_db();
    create_table(&db);
    put_with_data(&db, "k1", AttributeValue::S("x".into())).unwrap();
    let for_key =
        format!("ExpressionAttributeValues contains invalid value: {BARE_NEST} for key :deep");
    for (depth, expected) in [(62, ENVELOPED_NEST.to_string()), (63, for_key.clone())] {
        let values = || Some(HashMap::from([(":deep".to_string(), deep_map(depth))]));
        let err = db
            .put_item(PutItemRequest {
                table_name: "Tbl".to_string(),
                item: HashMap::from([("pk".to_string(), AttributeValue::S("p".into()))]),
                condition_expression: Some("a = :deep".into()),
                expression_attribute_values: values(),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err.to_string(), expected, "PutItem condition at {depth}");

        let err = db
            .update_item(UpdateItemRequest {
                table_name: "Tbl".to_string(),
                key: HashMap::from([("pk".to_string(), AttributeValue::S("k1".into()))]),
                update_expression: Some("SET d = :deep".into()),
                expression_attribute_values: values(),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err.to_string(), expected, "UpdateItem SET at {depth}");

        let err = update_with_deep_condition(&db, depth).unwrap_err();
        assert_eq!(err.to_string(), expected, "UpdateItem condition at {depth}");

        let err = db
            .delete_item(DeleteItemRequest {
                table_name: "Tbl".to_string(),
                key: HashMap::from([("pk".to_string(), AttributeValue::S("k1".into()))]),
                condition_expression: Some("a = :deep".into()),
                expression_attribute_values: values(),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err.to_string(), expected, "DeleteItem condition at {depth}");
    }
}
