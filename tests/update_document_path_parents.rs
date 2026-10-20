//! An update never builds the path it writes through. SET, REMOVE, ADD and
//! DELETE all need the parent of their last step to exist, as a map where the
//! step names a key and a list where it is an index; otherwise the update is
//! refused with `The document path provided in the update expression is invalid
//! for update` and the item is left as it was. The last step itself may be
//! absent (a REMOVE of it is a no-op, a SET or ADD creates it).
//!
//! Each case runs as UpdateItem and, where PartiQL has the clause, as an
//! `UPDATE` statement. Captured against eu-west-2, 18 October 2026.

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::ExecuteStatementRequest;
use dynoxide::actions::get_item::GetItemRequest;
use dynoxide::actions::update_item::UpdateItemRequest;
use serde_json::{Value, json};

const BAD_PATH: &str = "The document path provided in the update expression is invalid for update";
const BAD_TYPE: &str = "An operand in the update expression has an incorrect data type";

fn base() -> Value {
    json!({
        "pk": {"S": "k"},
        "m": {"M": {"a": {"N": "1"}, "deep": {"M": {"b": {"N": "2"}}}}},
        "s": {"S": "text"},
        "n": {"N": "5"},
        "l": {"L": [{"N": "1"}, {"N": "2"}]},
        "lm": {"L": [{"M": {"x": {"N": "1"}}}, {"N": "7"}]},
        "ss": {"SS": ["a", "b"]}
    })
}

fn setup() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value::<CreateTableRequest>(json!({
            "TableName": "Paths",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    db.put_item(serde_json::from_value(json!({"TableName": "Paths", "Item": base()})).unwrap())
        .unwrap();
    db
}

fn item(db: &Database) -> Value {
    let response = db
        .get_item(
            serde_json::from_value::<GetItemRequest>(json!({
                "TableName": "Paths",
                "Key": {"pk": {"S": "k"}},
                "ConsistentRead": true
            }))
            .unwrap(),
        )
        .unwrap();
    serde_json::to_value(response.item.unwrap()).unwrap()
}

/// What the item holds afterwards, or the refusal's message. The item is read
/// either way: a refusal has to leave it untouched.
fn via_update_item(expression: &str, values: Value) -> (Result<(), String>, Value) {
    let db = setup();
    let mut request = json!({
        "TableName": "Paths",
        "Key": {"pk": {"S": "k"}},
        "UpdateExpression": expression
    });
    if values.as_object().is_some_and(|v| !v.is_empty()) {
        request["ExpressionAttributeValues"] = values;
    }
    let result = db
        .update_item(serde_json::from_value::<UpdateItemRequest>(request).unwrap())
        .map(|_| ())
        .map_err(|e| e.to_string());
    let after = item(&db);
    (result, after)
}

fn via_partiql(clause: &str) -> (Result<(), String>, Value) {
    let db = setup();
    let statement = format!("UPDATE \"Paths\" {clause} WHERE pk = 'k'");
    let result = db
        .execute_statement(
            serde_json::from_value::<ExecuteStatementRequest>(json!({"Statement": statement}))
                .unwrap(),
        )
        .map(|_| ())
        .map_err(|e| e.to_string());
    let after = item(&db);
    (result, after)
}

fn refused(outcome: (Result<(), String>, Value), message: &str, context: &str) {
    let (result, after) = outcome;
    let error = result.expect_err(&format!("{context}: expected a refusal"));
    assert!(error.contains(message), "{context}: got `{error}`");
    assert_eq!(after, base(), "{context}: the item must be left as it was");
}

fn accepted(outcome: (Result<(), String>, Value), context: &str) -> Value {
    let (result, after) = outcome;
    result.unwrap_or_else(|e| panic!("{context}: expected success, got `{e}`"));
    after
}

fn n(v: u32) -> Value {
    json!({"N": v.to_string()})
}

/// (PartiQL clause, UpdateExpression, expression values) for paths whose
/// parent is missing or the wrong type.
fn bad_parent_cases() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        ("SET zz.y = 1", "SET zz.y = :v", json!({":v": n(1)})),
        ("SET zz.y.w = 1", "SET zz.y.w = :v", json!({":v": n(1)})),
        ("SET m.nope.y = 1", "SET m.nope.y = :v", json!({":v": n(1)})),
        ("SET s.y = 1", "SET s.y = :v", json!({":v": n(1)})),
        (
            "SET nolist[0] = 1",
            "SET nolist[0] = :v",
            json!({":v": n(1)}),
        ),
        ("SET m[0] = 1", "SET m[0] = :v", json!({":v": n(1)})),
        ("SET lm[1].y = 1", "SET lm[1].y = :v", json!({":v": n(1)})),
        ("SET lm[9].y = 1", "SET lm[9].y = :v", json!({":v": n(1)})),
        ("REMOVE zz.y", "REMOVE zz.y", json!({})),
        ("REMOVE m.nope.y", "REMOVE m.nope.y", json!({})),
        ("REMOVE nolist[0]", "REMOVE nolist[0]", json!({})),
        ("REMOVE s.y", "REMOVE s.y", json!({})),
        ("REMOVE m[0]", "REMOVE m[0]", json!({})),
    ]
}

#[test]
fn a_missing_or_mistyped_parent_refuses_the_update_and_changes_nothing() {
    for (clause, expression, values) in bad_parent_cases() {
        refused(via_partiql(clause), BAD_PATH, &format!("PartiQL {clause}"));
        refused(
            via_update_item(expression, values),
            BAD_PATH,
            &format!("UpdateItem {expression}"),
        );
    }
}

#[test]
fn a_bad_path_among_good_clauses_refuses_the_whole_update() {
    refused(
        via_partiql("SET n = 9, zz.y = 1"),
        BAD_PATH,
        "PartiQL SET n = 9, zz.y = 1",
    );
    refused(
        via_update_item("SET n = :v, zz.y = :w", json!({":v": n(9), ":w": n(1)})),
        BAD_PATH,
        "UpdateItem SET n, zz.y",
    );
    refused(
        via_partiql("SET n = 9 REMOVE zz.y"),
        BAD_PATH,
        "PartiQL SET n = 9 REMOVE zz.y",
    );
}

#[test]
fn add_and_delete_through_a_missing_or_mistyped_parent_name_the_path() {
    for (expression, values) in [
        ("ADD zz.y :v", json!({":v": n(1)})),
        ("ADD m.nope.y :v", json!({":v": n(1)})),
        ("ADD s.y :v", json!({":v": n(1)})),
        ("ADD lm[1].q :v", json!({":v": n(1)})),
        ("DELETE zz.y :v", json!({":v": {"SS": ["a"]}})),
        ("DELETE s.y :v", json!({":v": {"SS": ["a"]}})),
    ] {
        refused(via_update_item(expression, values), BAD_PATH, expression);
    }
}

#[test]
fn a_last_step_that_is_absent_is_fine_where_the_parent_is_there() {
    // REMOVE of an absent key, of an absent top-level attribute and of an
    // index past the end are all no-ops.
    for (clause, expression) in [
        ("REMOVE m.nope", "REMOVE m.nope"),
        ("REMOVE zz", "REMOVE zz"),
        ("REMOVE l[5]", "REMOVE l[5]"),
    ] {
        let after = accepted(via_partiql(clause), clause);
        assert_eq!(after, base(), "PartiQL {clause}");
        let after = accepted(via_update_item(expression, json!({})), expression);
        assert_eq!(after, base(), "UpdateItem {expression}");
    }

    // ADD and DELETE of an absent key under a present map.
    let after = accepted(
        via_update_item("ADD m.fresh :v", json!({":v": n(3)})),
        "ADD m.fresh",
    );
    assert_eq!(after["m"]["M"]["fresh"], n(3));
    let after = accepted(
        via_update_item("DELETE m.nope :v", json!({":v": {"SS": ["a"]}})),
        "DELETE m.nope",
    );
    assert_eq!(after, base());
}

#[test]
fn a_parent_that_is_there_is_written_through() {
    let after = accepted(via_partiql("SET m.deep.c = 3"), "PartiQL SET m.deep.c");
    assert_eq!(after["m"]["M"]["deep"]["M"]["c"], n(3));
    let after = accepted(via_partiql("SET lm[0].y = 5"), "PartiQL SET lm[0].y");
    assert_eq!(after["lm"]["L"][0]["M"]["y"], n(5));
    let after = accepted(via_partiql("REMOVE m.deep.b"), "PartiQL REMOVE m.deep.b");
    assert_eq!(after["m"]["M"]["deep"]["M"], json!({}));
    let after = accepted(via_partiql("REMOVE l[0]"), "PartiQL REMOVE l[0]");
    assert_eq!(after["l"]["L"], json!([n(2)]));
    let after = accepted(
        via_update_item("REMOVE lm[0].x", json!({})),
        "UpdateItem REMOVE lm[0].x",
    );
    assert_eq!(after["lm"]["L"][0]["M"], json!({}));
    // A whole map in one go creates the attribute, as ever.
    let after = accepted(via_partiql("SET zz = {'y': 1}"), "PartiQL SET zz = map");
    assert_eq!(after["zz"]["M"]["y"], n(1));
}

#[test]
fn an_operand_type_error_in_add_or_delete_wins_over_a_bad_path_elsewhere() {
    // `ADD s :v` adds a number to a string. The SET's bad path is earlier in
    // the expression and still loses to it.
    refused(
        via_update_item("SET zz.y = :v ADD s :v", json!({":v": n(1)})),
        BAD_TYPE,
        "SET zz.y ADD s",
    );
    refused(
        via_update_item(
            "SET zz.y = :w DELETE s :v",
            json!({":w": n(1), ":v": {"SS": ["a"]}}),
        ),
        BAD_TYPE,
        "SET zz.y DELETE s",
    );
}

#[test]
fn a_failing_condition_still_wins_over_a_bad_path() {
    let db = setup();
    let error = db
        .update_item(
            serde_json::from_value::<UpdateItemRequest>(json!({
                "TableName": "Paths",
                "Key": {"pk": {"S": "k"}},
                "UpdateExpression": "SET zz.y = :v",
                "ConditionExpression": "attribute_exists(nothere)",
                "ExpressionAttributeValues": {":v": n(1)}
            }))
            .unwrap(),
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("The conditional request failed"), "{error}");
}
