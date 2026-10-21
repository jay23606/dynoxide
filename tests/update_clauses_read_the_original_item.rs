//! Every path and value in one update means what it meant in the item as it
//! was before the update. Taking out `l[0]` does not move the element `l[1]`
//! names, a SET reads the old value of an attribute an earlier clause writes,
//! and a path another clause touches is refused as overlapping. PartiQL's
//! REMOVE takes one path per keyword. Captured against eu-west-2,
//! 18 October 2026.

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::ExecuteStatementRequest;
use dynoxide::actions::get_item::GetItemRequest;
use dynoxide::actions::update_item::UpdateItemRequest;
use serde_json::{Value, json};

const MISSING: &str =
    "The provided expression refers to an attribute that does not exist in the item";

fn n(v: u32) -> Value {
    json!({"N": v.to_string()})
}

fn setup() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value::<CreateTableRequest>(json!({
            "TableName": "Orig",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    db.put_item(
        serde_json::from_value(json!({"TableName": "Orig", "Item": {
            "pk": {"S": "k"},
            "n": n(5),
            "s": {"S": "text"},
            "l": {"L": [n(1), n(2)]},
            "lx": {"L": [{"M": {"x": n(1)}}, {"M": {"x": n(2)}}, {"M": {"x": n(3)}}]},
            "m": {"M": {"a": n(1), "deep": {"M": {"b": n(2)}}}}
        }}))
        .unwrap(),
    )
    .unwrap();
    db
}

fn stored(db: &Database) -> Value {
    let item = db
        .get_item(
            serde_json::from_value::<GetItemRequest>(json!({
                "TableName": "Orig", "Key": {"pk": {"S": "k"}}, "ConsistentRead": true
            }))
            .unwrap(),
        )
        .unwrap()
        .item
        .unwrap();
    serde_json::to_value(item).unwrap()
}

fn update_item(expression: &str, values: Value) -> (Result<(), String>, Value) {
    let db = setup();
    let mut request = json!({
        "TableName": "Orig",
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
    (result, stored(&db))
}

fn partiql(clauses: &str) -> (Result<(), String>, Value) {
    let db = setup();
    let result = db
        .execute_statement(
            serde_json::from_value::<ExecuteStatementRequest>(json!({
                "Statement": format!("UPDATE \"Orig\" {clauses} WHERE pk = 'k'")
            }))
            .unwrap(),
        )
        .map(|_| ())
        .map_err(|e| e.to_string());
    (result, stored(&db))
}

fn ok(outcome: (Result<(), String>, Value), what: &str) -> Value {
    let (result, after) = outcome;
    result.unwrap_or_else(|e| panic!("{what}: {e}"));
    after
}

fn refused(outcome: (Result<(), String>, Value), wanted: &str, what: &str) {
    let (result, after) = outcome;
    let error = result.expect_err(what);
    assert!(error.ends_with(wanted), "{what}: {error}");
    assert_eq!(after, stored(&setup()), "{what}: item must be unchanged");
}

#[test]
fn several_removals_in_one_list_each_name_the_original_element() {
    for (route, outcome) in [
        ("UpdateItem", update_item("REMOVE l[0], l[1]", json!({}))),
        ("PartiQL", partiql("REMOVE l[0] REMOVE l[1]")),
        (
            "UpdateItem reversed",
            update_item("REMOVE l[1], l[0]", json!({})),
        ),
        ("PartiQL reversed", partiql("REMOVE l[1] REMOVE l[0]")),
    ] {
        assert_eq!(ok(outcome, route)["l"], json!({"L": []}), "{route}");
    }
    // An element, and a member of another element.
    let after = ok(
        update_item("REMOVE lx[0], lx[1].x", json!({})),
        "UpdateItem",
    );
    assert_eq!(after["lx"], json!({"L": [{"M": {}}, {"M": {"x": n(3)}}]}));
    let after = ok(partiql("REMOVE lx[0] REMOVE lx[1].x"), "PartiQL");
    assert_eq!(after["lx"], json!({"L": [{"M": {}}, {"M": {"x": n(3)}}]}));
    // One past the end is a no-op and does not disturb the other removal.
    let after = ok(update_item("REMOVE lx[0], lx[9]", json!({})), "UpdateItem");
    assert_eq!(after["lx"]["L"].as_array().unwrap().len(), 2);
}

#[test]
fn add_and_delete_see_the_original_indexes_while_a_removal_runs() {
    let after = ok(
        update_item("REMOVE lx[0] ADD lx[1].x :v", json!({":v": n(10)})),
        "REMOVE then ADD",
    );
    assert_eq!(
        after["lx"],
        json!({"L": [{"M": {"x": n(12)}}, {"M": {"x": n(3)}}]})
    );
    // A SET past the end appends to the list as it was, and the removal follows.
    let after = ok(
        update_item("SET l[5] = :v REMOVE l[0]", json!({":v": n(9)})),
        "SET then REMOVE",
    );
    assert_eq!(after["l"], json!({"L": [n(2), n(9)]}));
    let after = ok(
        partiql("SET l[5] = 9 REMOVE l[0]"),
        "PartiQL SET then REMOVE",
    );
    assert_eq!(after["l"], json!({"L": [n(2), n(9)]}));
}

#[test]
fn a_partiql_set_reads_the_item_as_it_was() {
    let after = ok(partiql("SET n = 7, s = n + 1"), "SET n, s = n + 1");
    assert_eq!(after["n"], n(7));
    assert_eq!(after["s"], n(6));
    // An attribute an earlier clause creates does not exist yet.
    refused(
        partiql("SET zz = 1, n = zz + 1"),
        MISSING,
        "SET zz, n = zz + 1",
    );
    refused(
        update_item("SET zz = :v, n = zz + :v", json!({":v": n(1)})),
        MISSING,
        "UpdateItem SET zz, n = zz + 1",
    );
    // Nor does one an earlier REMOVE took out.
    let after = ok(partiql("REMOVE n SET s = n + 1"), "REMOVE n SET s = n + 1");
    assert_eq!(after["s"], n(6));
    assert!(after.get("n").is_none());
}

#[test]
fn partiql_remove_takes_one_path_per_keyword() {
    for statement in [
        "REMOVE l[0], l[1]",
        "REMOVE m.a, m.deep.b",
        "SET n = 1 REMOVE s, l[0]",
    ] {
        let (result, after) = partiql(statement);
        let error = result.expect_err(statement);
        assert!(
            error.ends_with("Unexpected token after expression"),
            "{statement}: {error}"
        );
        assert_eq!(after, stored(&setup()), "{statement}");
    }
    let after = ok(partiql("REMOVE m.a REMOVE m.deep.b"), "two keywords");
    assert_eq!(after["m"], json!({"M": {"deep": {"M": {}}}}));
}

#[test]
fn partiql_refuses_two_paths_that_overlap_and_names_the_later_one_first() {
    let overlap =
        "Two document paths overlap with each other; must remove or rewrite one of these paths; ";
    for (statement, paths) in [
        ("SET n = 1, n = 2", "path one: [n], path two: [n]"),
        ("REMOVE s REMOVE s", "path one: [s], path two: [s]"),
        ("SET s = 1 REMOVE s", "path one: [s], path two: [s]"),
        ("REMOVE m REMOVE m.a", "path one: [m, a], path two: [m]"),
        (
            "SET m = {'q': 1} REMOVE m.a",
            "path one: [m, a], path two: [m]",
        ),
        (
            "SET lx[0].x = 1, lx[0] = 2",
            "path one: [lx, [0]], path two: [lx, [0], x]",
        ),
    ] {
        refused(partiql(statement), &format!("{overlap}{paths}"), statement);
    }
    // Neighbours are not overlaps.
    let after = ok(partiql("SET m.a = 5, m.deep.b = 6"), "neighbours");
    assert_eq!(
        after["m"],
        json!({"M": {"a": n(5), "deep": {"M": {"b": n(6)}}}})
    );
    let after = ok(partiql("SET n = 1 SET s = 2"), "two SET keywords");
    assert_eq!((after["n"].clone(), after["s"].clone()), (n(1), n(2)));
}
