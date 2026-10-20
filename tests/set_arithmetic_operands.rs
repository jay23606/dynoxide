//! `SET n = x + 1` and `SET n = x - 1` need `x` to be a number that is there.
//! A stored string, map or list is `An operand in the update expression has an
//! incorrect data type`, and an attribute the item lacks is `The provided
//! expression refers to an attribute that does not exist in the item`, in
//! UpdateItem and in PartiQL alike. PartiQL used to read a missing attribute as
//! zero. A value of the wrong type in the expression is refused before the item
//! is read, and names the operator and the operand's type: by its short code
//! (`S`, `N`, `L`...) for arithmetic and `list_append`, spelled out for ADD and
//! DELETE (`STRING`, `NUMBER`, `Binary`, `LIST`, `MAP`) with a `typeSet`
//! suffix. Either side of an arithmetic or `list_append` may be an attribute
//! as well as a value. Captured against eu-west-2, 18 October 2026.

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::ExecuteStatementRequest;
use dynoxide::actions::update_item::UpdateItemRequest;
use serde_json::{Value, json};

const OPERAND: &str = "An operand in the update expression has an incorrect data type";
const MISSING: &str =
    "The provided expression refers to an attribute that does not exist in the item";

fn setup() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value::<CreateTableRequest>(json!({
            "TableName": "Sums",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    db.put_item(
        serde_json::from_value(json!({"TableName": "Sums", "Item": {
            "pk": {"S": "k"},
            "n": {"N": "5"},
            "s": {"S": "text"},
            "m": {"M": {"a": {"N": "1"}}},
            "l": {"L": [{"N": "1"}]},
            "ss": {"SS": ["a"]}
        }}))
        .unwrap(),
    )
    .unwrap();
    db
}

fn stored(db: &Database) -> Value {
    let item = db
        .get_item(
            serde_json::from_value(json!({
                "TableName": "Sums", "Key": {"pk": {"S": "k"}}, "ConsistentRead": true
            }))
            .unwrap(),
        )
        .unwrap()
        .item
        .unwrap();
    serde_json::to_value(item).unwrap()
}

fn update_item(expression: &str) -> (Result<(), String>, Value) {
    let db = setup();
    let mut request = json!({
        "TableName": "Sums",
        "Key": {"pk": {"S": "k"}},
        "UpdateExpression": expression
    });
    if expression.contains(":v") {
        request["ExpressionAttributeValues"] = json!({":v": {"N": "1"}});
    }
    let result = db
        .update_item(serde_json::from_value::<UpdateItemRequest>(request).unwrap())
        .map(|_| ())
        .map_err(|e| e.to_string());
    let after = stored(&db);
    (result, after)
}

fn partiql(clause: &str) -> (Result<(), String>, Value) {
    let db = setup();
    let result = db
        .execute_statement(
            serde_json::from_value::<ExecuteStatementRequest>(json!({
                "Statement": format!("UPDATE \"Sums\" {clause} WHERE pk = 'k'")
            }))
            .unwrap(),
        )
        .map(|_| ())
        .map_err(|e| e.to_string());
    let after = stored(&db);
    (result, after)
}

fn unchanged_item() -> Value {
    stored(&setup())
}

#[test]
fn arithmetic_on_a_stored_value_that_is_not_a_number_is_an_operand_type_error() {
    for attribute in ["s", "m", "l"] {
        for op in ["+", "-"] {
            for (route, (result, after)) in [
                (
                    "UpdateItem",
                    update_item(&format!("SET n = {attribute} {op} :v")),
                ),
                ("PartiQL", partiql(&format!("SET n = {attribute} {op} 1"))),
            ] {
                let error = result.expect_err(&format!("{route} {attribute} {op}"));
                assert!(error.contains(OPERAND), "{route} {attribute} {op}: {error}");
                assert_eq!(after, unchanged_item(), "{route} {attribute} {op}");
            }
        }
    }
}

#[test]
fn arithmetic_on_an_attribute_the_item_lacks_is_refused_not_read_as_zero() {
    for op in ["+", "-"] {
        for (route, (result, after)) in [
            ("UpdateItem", update_item(&format!("SET n = zz {op} :v"))),
            ("PartiQL", partiql(&format!("SET n = zz {op} 1"))),
        ] {
            let error = result.expect_err(&format!("{route} zz {op}"));
            assert!(error.contains(MISSING), "{route} zz {op}: {error}");
            assert_eq!(after, unchanged_item(), "{route} zz {op}");
        }
    }
}

#[test]
fn arithmetic_on_a_stored_number_still_works() {
    let (result, after) = partiql("SET n = n + 2");
    result.unwrap();
    assert_eq!(after["n"], json!({"N": "7"}));
    let (result, after) = update_item("SET n = n - :v");
    result.unwrap();
    assert_eq!(after["n"], json!({"N": "4"}));
}

const STATIC: &str = "Incorrect operand type for operator or function; operator or function";
/// What UpdateItem puts in front of an expression's own refusal; PartiQL's has none.
const LEAD: &str = "1 validation error detected: Invalid UpdateExpression: ";

/// UpdateItem with a value of each type in the placeholder `:v`.
fn update_with(expression: &str, value: Value) -> String {
    let db = setup();
    db.update_item(
        serde_json::from_value::<UpdateItemRequest>(json!({
            "TableName": "Sums",
            "Key": {"pk": {"S": "k"}},
            "UpdateExpression": expression,
            "ExpressionAttributeValues": {":v": value}
        }))
        .unwrap(),
    )
    .map(|_| "ok".to_string())
    .unwrap_or_else(|e| e.to_string())
}

#[test]
fn a_value_of_the_wrong_type_names_the_operator_and_its_type() {
    let values = [
        ("S", json!({"S": "x"})),
        ("B", json!({"B": "eA=="})),
        ("BOOL", json!({"BOOL": true})),
        ("NULL", json!({"NULL": true})),
        ("L", json!({"L": [{"N": "1"}]})),
        ("M", json!({"M": {"a": {"N": "1"}}})),
        ("SS", json!({"SS": ["a"]})),
        ("NS", json!({"NS": ["1"]})),
        ("BS", json!({"BS": ["eA=="]})),
    ];
    for (code, value) in values {
        for op in ["+", "-"] {
            let message = update_with(&format!("SET n = n {op} :v"), value.clone());
            assert!(message.starts_with(LEAD), "{op} {code}: {message}");
            assert!(
                message.ends_with(&format!("{STATIC}: {op}, operand type: {code}")),
                "{op} {code}: {message}"
            );
        }
        if code != "L" {
            for expression in ["SET l = list_append(l, :v)", "SET l = list_append(:v, l)"] {
                let message = update_with(expression, value.clone());
                assert!(
                    message.ends_with(&format!("{STATIC}: list_append, operand type: {code}")),
                    "{expression} {code}: {message}"
                );
            }
        }
    }
    // A number is a fine operand for arithmetic and the wrong one for a list.
    assert_eq!(update_with("SET n = n + :v", json!({"N": "1"})), "ok");
    let message = update_with("SET l = list_append(l, :v)", json!({"N": "1"}));
    assert!(
        message.contains("list_append, operand type: N"),
        "{message}"
    );
}

#[test]
fn add_and_delete_spell_the_type_out_and_name_the_type_set() {
    for (name, value) in [
        ("STRING", json!({"S": "x"})),
        ("Binary", json!({"B": "eA=="})),
        ("BOOL", json!({"BOOL": true})),
        ("NULL", json!({"NULL": true})),
        ("LIST", json!({"L": [{"N": "1"}]})),
        ("MAP", json!({"M": {"a": {"N": "1"}}})),
    ] {
        for (expression, operator) in [("ADD fresh :v", "ADD"), ("DELETE ss :v", "DELETE")] {
            let message = update_with(expression, value.clone());
            assert!(message.starts_with(LEAD), "{expression} {name}: {message}");
            let wanted = format!(
                "operator: {operator}, operand type: {name}, typeSet: ALLOWED_FOR_ADD_OPERAND"
            );
            assert!(message.contains(&wanted), "{expression} {name}: {message}");
        }
    }
    let message = update_with("DELETE ss :v", json!({"N": "1"}));
    assert!(message.contains("operand type: NUMBER, typeSet: ALLOWED_FOR_ADD_OPERAND"));
}

#[test]
fn a_literal_of_the_wrong_type_in_partiql_is_refused_before_the_item_is_read() {
    // `zz` is missing and `s` is a string, and the literal is refused first.
    for (clause, wanted) in [
        ("SET n = zz + 'x'", "+, operand type: S"),
        ("SET n = s - true", "-, operand type: BOOL"),
        ("SET n = n + [1]", "+, operand type: L"),
        ("SET n = n + {'a': 1}", "+, operand type: M"),
        ("SET n = n + <<'a'>>", "+, operand type: SS"),
        ("SET l = list_append(l, 2)", "list_append, operand type: N"),
        ("SET l = list_append(zz, 2)", "list_append, operand type: N"),
        (
            "SET l = list_append('abc', [2])",
            "list_append, operand type: S",
        ),
        (
            "SET l = list_append(l, 'abc')",
            "list_append, operand type: S",
        ),
    ] {
        let (result, after) = partiql(clause);
        let error = result.expect_err(clause);
        assert!(
            error.contains(&format!("{STATIC}: {wanted}")),
            "{clause}: {error}"
        );
        assert!(!error.contains("1 validation error"), "{clause}: {error}");
        assert_eq!(after, unchanged_item(), "{clause}");
    }
}

#[test]
fn either_side_of_arithmetic_and_list_append_may_be_an_attribute() {
    let (result, after) = partiql("SET n = n + n");
    result.unwrap();
    assert_eq!(after["n"], json!({"N": "10"}));
    let (result, after) = update_item("SET n = n - n");
    result.unwrap();
    assert_eq!(after["n"], json!({"N": "0"}));
    let (result, after) = partiql("SET n = n + m.a");
    result.unwrap();
    assert_eq!(after["n"], json!({"N": "6"}));
    for expression in ["SET l = list_append(s, l)", "SET l = list_append(l, n)"] {
        let (result, after) = update_item(expression);
        let error = result.expect_err(expression);
        assert!(error.contains(OPERAND), "{expression}: {error}");
        assert_eq!(after, unchanged_item(), "{expression}");
    }
    let (result, after) = partiql("SET l = list_append(l, l)");
    result.unwrap();
    assert_eq!(after["l"], json!({"L": [{"N": "1"}, {"N": "1"}]}));

    for (clause, wanted) in [
        ("SET n = n + zz", MISSING),
        ("SET n = zz + n", MISSING),
        ("SET n = n + s", OPERAND),
        ("SET l = list_append(l, zz)", MISSING),
        ("SET l = list_append(zz, l)", MISSING),
        ("SET l = list_append(l, n)", OPERAND),
        ("SET l = list_append(s, l)", OPERAND),
    ] {
        let (result, after) = partiql(clause);
        let error = result.expect_err(clause);
        assert!(error.contains(wanted), "{clause}: {error}");
        assert_eq!(after, unchanged_item(), "{clause}");
    }
}
