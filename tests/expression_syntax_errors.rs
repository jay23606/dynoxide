//! Expression syntax errors name the token the parser stopped at and the
//! source around it, as DynamoDB does: `Syntax error; token: "x", near: ") x"`.
//! `near` runs from the token before the offending one to the token after it;
//! at the end of the expression the token is `<EOF>` and `near` is the last
//! token read.
//!
//! Every case below was captured against real DynamoDB in us-east-1 and
//! eu-west-2 on 2026-10-09, which agreed on all of them. FilterExpression,
//! ConditionExpression and KeyConditionExpression share the rule; PutItem,
//! UpdateItem and DeleteItem wrap the message in the request-validation
//! envelope, the reads and transactions send it bare.

use dynoxide::Database;
use dynoxide::DynoxideError;
use serde_json::{Value, json};

/// (expression, detail after the `Invalid <X>Expression: ` prefix).
const CONDITION_CASES: &[(&str, &str)] = &[
    (
        "attribute_not_exists(",
        r#"Syntax error; token: "<EOF>", near: "(""#,
    ),
    (
        "attribute_exists(pk",
        r#"Syntax error; token: "<EOF>", near: "pk""#,
    ),
    ("a =", r#"Syntax error; token: "<EOF>", near: "=""#),
    ("a = :v AND", r#"Syntax error; token: "<EOF>", near: "AND""#),
    (
        "a BETWEEN :v",
        r#"Syntax error; token: "<EOF>", near: ":v""#,
    ),
    ("(a = :v", r#"Syntax error; token: "<EOF>", near: ":v""#),
    ("a.", r#"Syntax error; token: "<EOF>", near: ".""#),
    ("a[", r#"Syntax error; token: "<EOF>", near: "[""#),
    (
        "attribute_exists(pk) x",
        r#"Syntax error; token: "x", near: ") x""#,
    ),
    (
        "attribute_exists(pk) x y",
        r#"Syntax error; token: "x", near: ") x y""#,
    ),
    ("a = :v :w", r#"Syntax error; token: ":w", near: ":v :w""#),
    (
        "a = :v :w x",
        r#"Syntax error; token: ":w", near: ":v :w x""#,
    ),
    ("a = :v)", r#"Syntax error; token: ")", near: ":v)""#),
    ("a :v", r#"Syntax error; token: ":v", near: "a :v""#),
    ("a = = :v", r#"Syntax error; token: "=", near: "= = :v""#),
    ("= :v", r#"Syntax error; token: "=", near: "= :v""#),
    ("AND a = :v", r#"Syntax error; token: "AND", near: "AND a""#),
    (
        "aaa = :v AND bbbb = :w cccc = :x",
        r#"Syntax error; token: "cccc", near: ":w cccc =""#,
    ),
    (
        "attribute_exists pk",
        r#"Syntax error; token: "pk", near: "attribute_exists pk""#,
    ),
    (
        "attribute_exists(pk,)",
        r#"Syntax error; token: ")", near: ",)""#,
    ),
    (
        "begins_with(a :v)",
        r#"Syntax error; token: ":v", near: "a :v)""#,
    ),
    ("a IN :v", r#"Syntax error; token: ":v", near: "IN :v""#),
    ("a IN ()", r#"Syntax error; token: ")", near: "()""#),
    (
        "a BETWEEN :v OR :w",
        r#"Syntax error; token: "OR", near: ":v OR :w""#,
    ),
    ("NOT", r#"Syntax error; token: "<EOF>", near: "NOT""#),
    ("a[x] = :v", r#"Syntax error; token: "x", near: "[x]""#),
    ("a[1 = :v", r#"Syntax error; token: "=", near: "1 = :v""#),
    ("a = :v !", r#"Syntax error; token: "!", near: ":v !""#),
    ("a ! b", r#"Syntax error; token: "!", near: "a ! b""#),
    ("foo(pk)", "Invalid function name; function: foo"),
    ("foo(pk, :v)", "Invalid function name; function: foo"),
    ("size(a) =", r#"Syntax error; token: "<EOF>", near: "=""#),
];

/// The cases also sent to UpdateItem, DeleteItem, a Query filter and a
/// transaction's ConditionCheck.
const CONDITION_SUBSET: &[&str] = &[
    "attribute_not_exists(",
    "attribute_exists(pk) x",
    "a = :v :w x",
    "= :v",
    "a = :v !",
    "foo(pk)",
];

const KEY_CONDITION_CASES: &[(&str, &str)] = &[
    ("pk =", r#"Syntax error; token: "<EOF>", near: "=""#),
    (
        "pk = :pk AND",
        r#"Syntax error; token: "<EOF>", near: "AND""#,
    ),
    ("pk = :pk x", r#"Syntax error; token: "x", near: ":pk x""#),
    (
        "pk = :pk :w",
        r#"Syntax error; token: ":w", near: ":pk :w""#,
    ),
    ("foo(pk)", "Invalid function name; function: foo"),
    (
        "pk = :pk AND begins_with(sk",
        r#"Syntax error; token: "<EOF>", near: "sk""#,
    ),
    ("pk :pk", r#"Syntax error; token: ":pk", near: "pk :pk""#),
    ("= :pk", r#"Syntax error; token: "=", near: "= :pk""#),
    ("pk = :pk)", r#"Syntax error; token: ")", near: ":pk)""#),
];

fn db() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value(json!({
            "TableName": "Tbl",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        }))
        .unwrap(),
    )
    .unwrap();
    db.put_item(
        serde_json::from_value(json!({
            "TableName": "Tbl", "Item": {"pk": {"S": "a"}, "a": {"S": "v"}}
        }))
        .unwrap(),
    )
    .unwrap();
    db
}

/// Every `:name` the expression mentions, defined, so the only fault is the
/// syntax.
fn values_for(expr: &str, extra: &[&str]) -> Option<Value> {
    let mut map = serde_json::Map::new();
    let bytes = expr.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            map.insert(expr[start..i].to_string(), json!({"S": "v"}));
        } else {
            i += 1;
        }
    }
    for name in extra {
        map.insert(name.to_string(), json!({"S": "v"}));
    }
    (!map.is_empty()).then_some(Value::Object(map))
}

fn with_values(mut request: Value, expr: &str, extra: &[&str]) -> Value {
    if let Some(values) = values_for(expr, extra) {
        request["ExpressionAttributeValues"] = values;
    }
    request
}

fn message(result: Result<(), DynoxideError>, surface: &str, expr: &str) -> String {
    match result {
        Ok(()) => panic!("{surface} accepted {expr:?}"),
        Err(err) => {
            assert_eq!(
                err.error_type(),
                "com.amazon.coral.validate#ValidationException",
                "{surface} {expr:?}: {err:?}"
            );
            err.to_string()
        }
    }
}

fn scan(db: &Database, expr: &str) -> Result<(), DynoxideError> {
    let req = with_values(
        json!({"TableName": "Tbl", "FilterExpression": expr}),
        expr,
        &[],
    );
    db.scan(serde_json::from_value(req).unwrap()).map(|_| ())
}

fn put(db: &Database, expr: &str) -> Result<(), DynoxideError> {
    let req = with_values(
        json!({"TableName": "Tbl", "Item": {"pk": {"S": "s-put"}}, "ConditionExpression": expr}),
        expr,
        &[],
    );
    db.put_item(serde_json::from_value(req).unwrap())
        .map(|_| ())
}

fn update(db: &Database, expr: &str) -> Result<(), DynoxideError> {
    let req = with_values(
        json!({"TableName": "Tbl", "Key": {"pk": {"S": "s-upd"}},
            "UpdateExpression": "SET a = :zz", "ConditionExpression": expr}),
        expr,
        &[":zz"],
    );
    db.update_item(serde_json::from_value(req).unwrap())
        .map(|_| ())
}

fn delete(db: &Database, expr: &str) -> Result<(), DynoxideError> {
    let req = with_values(
        json!({"TableName": "Tbl", "Key": {"pk": {"S": "s-del"}}, "ConditionExpression": expr}),
        expr,
        &[],
    );
    db.delete_item(serde_json::from_value(req).unwrap())
        .map(|_| ())
}

fn query_filter(db: &Database, expr: &str) -> Result<(), DynoxideError> {
    let req = with_values(
        json!({"TableName": "Tbl", "KeyConditionExpression": "pk = :pk", "FilterExpression": expr}),
        expr,
        &[":pk"],
    );
    db.query(serde_json::from_value(req).unwrap()).map(|_| ())
}

fn query_key(db: &Database, expr: &str) -> Result<(), DynoxideError> {
    let req = with_values(
        json!({"TableName": "Tbl", "KeyConditionExpression": expr}),
        expr,
        &[],
    );
    db.query(serde_json::from_value(req).unwrap()).map(|_| ())
}

fn transact_check(db: &Database, expr: &str) -> Result<(), DynoxideError> {
    let check = with_values(
        json!({"TableName": "Tbl", "Key": {"pk": {"S": "a"}}, "ConditionExpression": expr}),
        expr,
        &[],
    );
    let req = json!({"TransactItems": [{"ConditionCheck": check}]});
    db.transact_write_items(serde_json::from_value(req).unwrap())
        .map(|_| ())
}

fn detail(expr: &str) -> &'static str {
    CONDITION_CASES
        .iter()
        .find(|(e, _)| *e == expr)
        .map(|(_, d)| *d)
        .expect("subset case is in the table")
}

#[test]
fn filter_expression_syntax_errors_match_dynamodb() {
    let db = db();
    let mut mismatches = Vec::new();
    for (expr, detail) in CONDITION_CASES {
        let got = message(scan(&db, expr), "Scan", expr);
        let want = format!("Invalid FilterExpression: {detail}");
        if got != want {
            mismatches.push(format!("{expr:?}\n   got  {got}\n   want {want}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} of {} cases differ:\n{}",
        mismatches.len(),
        CONDITION_CASES.len(),
        mismatches.join("\n")
    );
}

#[test]
fn put_item_condition_syntax_errors_are_enveloped() {
    let db = db();
    let mut mismatches = Vec::new();
    for (expr, detail) in CONDITION_CASES {
        let got = message(put(&db, expr), "PutItem", expr);
        let want = format!("1 validation error detected: Invalid ConditionExpression: {detail}");
        if got != want {
            mismatches.push(format!("{expr:?}\n   got  {got}\n   want {want}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} of {} cases differ:\n{}",
        mismatches.len(),
        CONDITION_CASES.len(),
        mismatches.join("\n")
    );
}

#[test]
fn update_and_delete_condition_syntax_errors_are_enveloped() {
    let db = db();
    for expr in CONDITION_SUBSET {
        let want = format!(
            "1 validation error detected: Invalid ConditionExpression: {}",
            detail(expr)
        );
        assert_eq!(message(update(&db, expr), "UpdateItem", expr), want);
        assert_eq!(message(delete(&db, expr), "DeleteItem", expr), want);
    }
}

#[test]
fn query_filter_and_transaction_condition_syntax_errors_are_bare() {
    let db = db();
    for expr in CONDITION_SUBSET {
        assert_eq!(
            message(query_filter(&db, expr), "Query", expr),
            format!("Invalid FilterExpression: {}", detail(expr))
        );
        assert_eq!(
            message(transact_check(&db, expr), "TransactWriteItems", expr),
            format!("Invalid ConditionExpression: {}", detail(expr))
        );
    }
}

#[test]
fn key_condition_syntax_errors_match_dynamodb() {
    let db = db();
    let mut mismatches = Vec::new();
    for (expr, detail) in KEY_CONDITION_CASES {
        let got = message(query_key(&db, expr), "Query", expr);
        let want = format!("Invalid KeyConditionExpression: {detail}");
        if got != want {
            mismatches.push(format!("{expr:?}\n   got  {got}\n   want {want}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} of {} cases differ:\n{}",
        mismatches.len(),
        KEY_CONDITION_CASES.len(),
        mismatches.join("\n")
    );
}

#[test]
fn update_expression_syntax_errors_use_the_same_form() {
    // UpdateItem sends them in the envelope, a transaction's Update bare.
    // Captured us-east-1 and eu-west-2, 2026-10-09.
    let db = db();
    let cases = [
        ("SET a =", r#"Syntax error; token: "<EOF>", near: "=""#),
        ("SET", r#"Syntax error; token: "<EOF>", near: "SET""#),
        ("SET a = :v x", r#"Syntax error; token: "x", near: ":v x""#),
        ("SET a :v", r#"Syntax error; token: ":v", near: "a :v""#),
        ("ADD a", r#"Syntax error; token: "<EOF>", near: "a""#),
        ("REMOVE a :v", r#"Syntax error; token: ":v", near: "a :v""#),
        ("SET a = :v !", r#"Syntax error; token: "!", near: ":v !""#),
        (
            "INVALID SYNTAX HERE",
            r#"Syntax error; token: "INVALID", near: "INVALID SYNTAX""#,
        ),
    ];
    for (expr, detail) in cases {
        let update = with_values(
            json!({"TableName": "Tbl", "Key": {"pk": {"S": "s-ue"}}, "UpdateExpression": expr}),
            expr,
            &[],
        );
        let got = message(
            db.update_item(serde_json::from_value(update.clone()).unwrap())
                .map(|_| ()),
            "UpdateItem",
            expr,
        );
        assert_eq!(
            got,
            format!("1 validation error detected: Invalid UpdateExpression: {detail}")
        );
        let req = json!({"TransactItems": [{"Update": update}]});
        let got = message(
            db.transact_write_items(serde_json::from_value(req).unwrap())
                .map(|_| ()),
            "TransactWriteItems",
            expr,
        );
        assert_eq!(got, format!("Invalid UpdateExpression: {detail}"));
    }
}

#[test]
fn other_condition_parse_errors_are_enveloped_on_every_single_item_write() {
    // Redundant parentheses, reserved words, undefined names and values, and
    // reversed BETWEEN bounds come back in the envelope on DeleteItem as on
    // PutItem and UpdateItem. Captured us-east-1 and eu-west-2, 2026-10-09.
    let db = db();
    let cases: [(&str, Value, Option<Value>, &str); 6] = [
        (
            "((a = :v))",
            json!({":v": {"S": "v"}}),
            None,
            "The expression has redundant parentheses;",
        ),
        (
            "data = :v",
            json!({":v": {"S": "v"}}),
            None,
            "Attribute name is a reserved keyword; reserved keyword: data",
        ),
        (
            "a.data = :v",
            json!({":v": {"S": "v"}}),
            None,
            "Attribute name is a reserved keyword; reserved keyword: data",
        ),
        (
            "a = :nope",
            json!({}),
            None,
            "An expression attribute value used in expression is not defined; \
             attribute value: :nope",
        ),
        (
            "#n = :v",
            json!({":v": {"S": "v"}}),
            None,
            "An expression attribute name used in the document path is not defined; \
             attribute name: #n",
        ),
        (
            "a BETWEEN :hi AND :lo",
            json!({":hi": {"S": "z"}, ":lo": {"S": "a"}}),
            None,
            "The BETWEEN operator requires upper bound to be greater than or equal to \
             lower bound; lower bound operand: AttributeValue: {S:z}, \
             upper bound operand: AttributeValue: {S:a}",
        ),
    ];
    for (expr, values, names, detail) in cases {
        let want = format!("1 validation error detected: Invalid ConditionExpression: {detail}");
        let mut delete = json!({"TableName": "Tbl", "Key": {"pk": {"S": "d1"}},
            "ConditionExpression": expr});
        let mut put = json!({"TableName": "Tbl", "Item": {"pk": {"S": "p1"}},
            "ConditionExpression": expr});
        let mut update = json!({"TableName": "Tbl", "Key": {"pk": {"S": "u1"}},
            "UpdateExpression": "SET b = :zz", "ConditionExpression": expr});
        let mut update_values = values.clone();
        update_values[":zz"] = json!({"S": "z"});
        update["ExpressionAttributeValues"] = update_values;
        if values.as_object().is_some_and(|m| !m.is_empty()) {
            delete["ExpressionAttributeValues"] = values.clone();
            put["ExpressionAttributeValues"] = values.clone();
        }
        if let Some(names) = names {
            delete["ExpressionAttributeNames"] = names.clone();
            put["ExpressionAttributeNames"] = names.clone();
            update["ExpressionAttributeNames"] = names;
        }
        let results = [
            (
                "DeleteItem",
                db.delete_item(serde_json::from_value(delete).unwrap())
                    .map(|_| ()),
            ),
            (
                "PutItem",
                db.put_item(serde_json::from_value(put).unwrap())
                    .map(|_| ()),
            ),
            (
                "UpdateItem",
                db.update_item(serde_json::from_value(update).unwrap())
                    .map(|_| ()),
            ),
        ];
        for (surface, result) in results {
            assert_eq!(message(result, surface, expr), want, "{surface} {expr:?}");
        }
    }
}

#[test]
fn condition_operand_errors_are_enveloped_on_delete_and_put() {
    // Captured us-east-1 and eu-west-2, 2026-10-09.
    let db = db();
    let cases = [
        (
            "contains(a, a)",
            json!(null),
            "The first operand must be distinct from the remaining operands for this \
             operator or function; operator: contains, first operand: [a]",
        ),
        (
            "begins_with(a, :n)",
            json!({":n": {"N": "1"}}),
            "Incorrect operand type for operator or function; operator or function: \
             begins_with, operand type: N",
        ),
        (
            "a BETWEEN :s AND :n",
            json!({":s": {"S": "a"}, ":n": {"N": "1"}}),
            "The BETWEEN operator requires same data type for lower and upper bounds; \
             lower bound operand: AttributeValue: {S:a}, upper bound operand: AttributeValue: {N:1}",
        ),
    ];
    for (expr, values, detail) in cases {
        let want = format!("1 validation error detected: Invalid ConditionExpression: {detail}");
        let mut delete = json!({"TableName": "Tbl", "Key": {"pk": {"S": "d"}},
            "ConditionExpression": expr});
        let mut put = json!({"TableName": "Tbl", "Item": {"pk": {"S": "p"}},
            "ConditionExpression": expr});
        if !values.is_null() {
            delete["ExpressionAttributeValues"] = values.clone();
            put["ExpressionAttributeValues"] = values;
        }
        let got = message(
            db.delete_item(serde_json::from_value(delete).unwrap())
                .map(|_| ()),
            "DeleteItem",
            expr,
        );
        assert_eq!(got, want, "DeleteItem {expr:?}");
        let got = message(
            db.put_item(serde_json::from_value(put).unwrap())
                .map(|_| ()),
            "PutItem",
            expr,
        );
        assert_eq!(got, want, "PutItem {expr:?}");
    }
}

#[test]
fn a_list_index_with_a_leading_zero_is_a_syntax_error() {
    // DynamoDB reads a number starting with 0 as the single digit, so `l[00]`
    // is two numbers in a row and fails at the second. Captured for
    // UpdateItem's UpdateExpression in eu-west-2, 2026-10-09; the condition,
    // filter, projection and transaction cases apply the same rule and have not
    // been sent to DynamoDB.
    let db = db();
    db.put_item(
        serde_json::from_value(json!({"TableName": "Tbl",
            "Item": {"pk": {"S": "l"}, "l": {"L": [{"S": "x"}, {"S": "y"}]}}}))
        .unwrap(),
    )
    .unwrap();

    let update_cases = [
        ("SET l[00] = :v", r#"Syntax error; token: "0", near: "00]""#),
        (
            "SET l[007] = :v",
            r#"Syntax error; token: "0", near: "007""#,
        ),
        ("SET c = l[00]", r#"Syntax error; token: "0", near: "00]""#),
        ("REMOVE l[01]", r#"Syntax error; token: "1", near: "01]""#),
    ];
    for (expr, detail) in update_cases {
        let req = with_values(
            json!({"TableName": "Tbl", "Key": {"pk": {"S": "l"}}, "UpdateExpression": expr}),
            expr,
            &[],
        );
        let got = message(
            db.update_item(serde_json::from_value(req).unwrap())
                .map(|_| ()),
            "UpdateItem",
            expr,
        );
        assert_eq!(
            got,
            format!("1 validation error detected: Invalid UpdateExpression: {detail}")
        );
    }

    let got = message(put(&db, "l[00] = :v"), "PutItem", "l[00] = :v");
    assert_eq!(
        got,
        r#"1 validation error detected: Invalid ConditionExpression: Syntax error; token: "0", near: "00]""#
    );
    let got = message(scan(&db, "l[01] = :v"), "Scan", "l[01] = :v");
    assert_eq!(
        got,
        r#"Invalid FilterExpression: Syntax error; token: "1", near: "01]""#
    );
    let got = message(
        transact_check(&db, "l[007] = :v"),
        "TransactWriteItems",
        "l[007] = :v",
    );
    assert_eq!(
        got,
        r#"Invalid ConditionExpression: Syntax error; token: "0", near: "007""#
    );
    let get = |projection: &str| {
        db.get_item(
            serde_json::from_value(json!({"TableName": "Tbl", "Key": {"pk": {"S": "l"}},
                "ProjectionExpression": projection}))
            .unwrap(),
        )
        .map(|_| ())
    };
    assert_eq!(
        message(get("l[00]"), "GetItem", "l[00]"),
        r#"Invalid ProjectionExpression: Syntax error; token: "0", near: "00]""#
    );

    // A single 0, and an index with no leading zero, still read as numbers.
    get("l[0], l[10]").expect("l[0] and l[10] are valid indexes");
}
