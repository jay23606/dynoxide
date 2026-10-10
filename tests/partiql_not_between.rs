//! `x NOT BETWEEN lo AND hi` in a PartiQL WHERE clause, and the `NOT IN` and
//! `NOT BETWEEN` that follow a size, a literal or a condition function.
//!
//! DynamoDB reads `x NOT BETWEEN lo AND hi` as exactly `NOT (x BETWEEN lo AND
//! hi)`, so an item missing `x`, or holding a string, NULL or boolean there,
//! matches a numeric range's negation. It takes the form wherever BETWEEN
//! stands, refuses it in BETWEEN's own words, and counts its NOT on its own
//! toward the 300-operator limit. Every case here was captured in eu-west-2
//! and us-east-1 on 2026-10-10, and the two regions answered alike.

use dynoxide::Database;
use dynoxide::actions::batch_execute_statement::BatchExecuteStatementRequest;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::ExecuteStatementRequest;
use dynoxide::actions::execute_transaction::ExecuteTransactionRequest;
use dynoxide::actions::get_item::GetItemRequest;
use dynoxide::actions::put_item::PutItemRequest;
use dynoxide::errors::DynoxideError;
use dynoxide::types::{AttributeValue, Item};
use serde_json::{Value, json};

const NOT_WELL_FORMED: &str = "Statement wasn't well formed, can't be processed: ";

/// The captures' items: numbers inside and outside 1 to 3, a string, a
/// missing `x`, NULL, a boolean, and bounds held as attributes.
fn db() -> Database {
    let db = Database::memory().unwrap();
    for (name, key_schema, definitions) in [
        (
            "NbItems",
            json!([{"AttributeName": "pk", "KeyType": "HASH"}]),
            json!([{"AttributeName": "pk", "AttributeType": "S"}]),
        ),
        (
            "NbSorted",
            json!([
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"}
            ]),
            json!([
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "N"}
            ]),
        ),
    ] {
        db.create_table(
            serde_json::from_value::<CreateTableRequest>(json!({
                "TableName": name,
                "KeySchema": key_schema,
                "AttributeDefinitions": definitions,
                "BillingMode": "PAY_PER_REQUEST"
            }))
            .unwrap(),
        )
        .unwrap();
    }
    let items = json!({
        "r1": {"x": {"N": "1"}, "s": {"S": "abc"}, "lo": {"N": "1"}, "hi": {"N": "3"}},
        "r2": {"x": {"N": "2"}, "lo": {"N": "1"}, "hi": {"N": "3"}},
        "r3": {"x": {"N": "3"}},
        "r4": {"x": {"N": "4"}, "lo": {"N": "1"}, "hi": {"N": "3"}},
        "r5": {"x": {"N": "0"}},
        "r6": {"x": {"S": "2"}},
        "r7": {"s": {"S": "abcdef"}},
        "r8": {"x": {"NULL": true}},
        "r9": {"x": {"BOOL": true}},
        "r10": {"x": {"N": "2.5"}, "m": {"M": {"a": {"N": "2"}}}, "l": {"L": [{"N": "5"}]}},
        "r11": {"x": {"S": "m"}},
        "d1": {"x": {"N": "0"}},
        "d2": {"x": {"N": "2"}}
    });
    for (pk, rest) in items.as_object().unwrap() {
        let mut item = rest.as_object().unwrap().clone();
        item.insert("pk".to_string(), json!({"S": pk}));
        put(&db, "NbItems", Value::Object(item));
    }
    for sk in 1..=5 {
        put(
            &db,
            "NbSorted",
            json!({"pk": {"S": "p"}, "sk": {"N": sk.to_string()}}),
        );
    }
    db
}

fn put(db: &Database, table: &str, item: Value) {
    db.put_item(PutItemRequest {
        table_name: table.to_string(),
        item: serde_json::from_value(item).unwrap(),
        ..Default::default()
    })
    .unwrap();
}

fn run(db: &Database, statement: &str, parameters: Value) -> Result<Vec<Item>, DynoxideError> {
    let mut request = json!({"Statement": statement});
    if !parameters.is_null() {
        request["Parameters"] = parameters;
    }
    db.execute_statement(serde_json::from_value::<ExecuteStatementRequest>(request).unwrap())
        .map(|response| response.items.unwrap_or_default())
}

/// The sorted keys `SELECT * FROM "NbItems" WHERE <condition>` returns.
fn rows(db: &Database, condition: &str) -> Vec<String> {
    rows_with(db, condition, Value::Null)
}

fn rows_with(db: &Database, condition: &str, parameters: Value) -> Vec<String> {
    let statement = format!(r#"SELECT * FROM "NbItems" WHERE {condition}"#);
    let items = run(db, &statement, parameters)
        .unwrap_or_else(|e| panic!("{statement}: expected rows, got {e:?}"));
    let mut keys: Vec<String> = items
        .iter()
        .map(|item| match (item.get("pk"), item.get("sk")) {
            (Some(AttributeValue::S(pk)), Some(AttributeValue::N(sk))) => format!("{pk}/{sk}"),
            (Some(AttributeValue::S(pk)), _) => pk.clone(),
            other => panic!("{statement}: no key in {other:?}"),
        })
        .collect();
    keys.sort();
    keys
}

fn refusal(db: &Database, condition: &str) -> String {
    let statement = format!(r#"SELECT * FROM "NbItems" WHERE {condition}"#);
    match run(db, &statement, Value::Null) {
        Err(DynoxideError::ValidationException(m)) => m,
        other => panic!("{statement}: expected a ValidationException, got {other:?}"),
    }
}

fn sorted(keys: &[&str]) -> Vec<String> {
    let mut keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
    keys.sort();
    keys
}

/// Every key but `except`.
fn all_but(except: &[&str]) -> Vec<String> {
    sorted(
        &[
            "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "d1", "d2",
        ]
        .into_iter()
        .filter(|k| !except.contains(k))
        .collect::<Vec<_>>(),
    )
}

fn stored(db: &Database, pk: &str) -> Option<Item> {
    db.get_item(
        serde_json::from_value::<GetItemRequest>(json!({
            "TableName": "NbItems",
            "Key": {"pk": {"S": pk}}
        }))
        .unwrap(),
    )
    .unwrap()
    .item
}

/// The negation of `x BETWEEN 1 AND 3` (d2 r1 r10 r2 r3): every other item,
/// including the one with no `x` and those holding a string, NULL or boolean.
const OUTSIDE_1_TO_3: &[&str] = &["d1", "r11", "r4", "r5", "r6", "r7", "r8", "r9"];

#[test]
fn not_between_after_a_path_is_the_negation_of_between() {
    let db = db();
    assert_eq!(
        rows(&db, "x BETWEEN 1 AND 3"),
        sorted(&["d2", "r1", "r10", "r2", "r3"])
    );
    for condition in [
        "x NOT BETWEEN 1 AND 3",
        "NOT x BETWEEN 1 AND 3",
        "NOT (x BETWEEN 1 AND 3)",
        "x not between 1 and 3",
    ] {
        assert_eq!(rows(&db, condition), sorted(OUTSIDE_1_TO_3), "{condition}");
    }
    assert_eq!(
        rows_with(
            &db,
            "x NOT BETWEEN ? AND ?",
            json!([{"N": "1"}, {"N": "3"}])
        ),
        sorted(OUTSIDE_1_TO_3)
    );
    assert_eq!(rows(&db, "x NOT BETWEEN 'a' AND 'z'"), all_but(&["r11"]));
    assert_eq!(rows(&db, "y NOT BETWEEN 1 AND 3"), all_but(&[]));
    assert_eq!(
        rows(&db, "pk NOT BETWEEN 'r2' AND 'r4'"),
        all_but(&["r2", "r3", "r4"])
    );
    assert_eq!(rows(&db, "x NOT BETWEEN lo AND hi"), all_but(&["r1", "r2"]));
    assert_eq!(rows(&db, "m.a NOT BETWEEN 1 AND 3"), all_but(&["r10"]));
    assert_eq!(rows(&db, "l[0] NOT BETWEEN 1 AND 3"), all_but(&[]));
    assert_eq!(
        rows(&db, "NOT x NOT BETWEEN 1 AND 3"),
        sorted(&["d2", "r1", "r10", "r2", "r3"])
    );
    assert_eq!(
        rows(&db, "x NOT BETWEEN 1 AND 3 AND pk = 'r4'"),
        sorted(&["r4"])
    );
    let mut or_r2 = OUTSIDE_1_TO_3.to_vec();
    or_r2.push("r2");
    assert_eq!(
        rows(&db, "x NOT BETWEEN 1 AND 3 OR pk = 'r2'"),
        sorted(&or_r2)
    );
    assert_eq!(
        rows(&db, "pk = 'r2' AND x NOT BETWEEN 1 AND 3"),
        Vec::<String>::new()
    );
    assert_eq!(
        rows(&db, "pk = 'r7' AND x NOT BETWEEN 1 AND 3"),
        sorted(&["r7"])
    );
}

#[test]
fn not_between_reads_a_sort_key_range() {
    let db = db();
    for condition in [
        "pk = 'p' AND sk NOT BETWEEN 2 AND 4",
        "pk = 'p' AND NOT sk BETWEEN 2 AND 4",
    ] {
        let statement = format!(r#"SELECT * FROM "NbSorted" WHERE {condition}"#);
        let mut keys: Vec<String> = run(&db, &statement, Value::Null)
            .unwrap()
            .iter()
            .map(|item| match item.get("sk") {
                Some(AttributeValue::N(sk)) => sk.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        keys.sort();
        assert_eq!(keys, ["1", "5"], "{condition}");
    }
}

#[test]
fn not_between_after_a_size_or_a_literal_runs() {
    let db = db();
    assert_eq!(rows(&db, "size(s) BETWEEN 1 AND 3"), sorted(&["r1"]));
    assert_eq!(rows(&db, "size(s) NOT BETWEEN 1 AND 3"), all_but(&["r1"]));
    assert_eq!(
        rows(&db, "'2' NOT BETWEEN '1' AND '3'"),
        Vec::<String>::new()
    );
}

#[test]
fn not_in_after_a_size_or_a_condition_function_runs() {
    let db = db();
    assert_eq!(rows(&db, "size(s) NOT IN [3]"), all_but(&["r1"]));
    assert_eq!(
        rows(&db, "attribute_exists(x) NOT IN [true]"),
        sorted(&["r7"])
    );
    assert_eq!(rows(&db, "x NOT IN [1, 2]"), all_but(&["r1", "r2", "d2"]));
}

/// A parameter on the left reads as a literal there does (inferred from the
/// literal form; no parameter-first `NOT BETWEEN` or `NOT IN` was captured).
#[test]
fn not_between_and_not_in_after_a_parameter_read_as_after_a_literal() {
    let db = db();
    assert_eq!(
        rows_with(&db, "? NOT BETWEEN '1' AND '3'", json!([{"S": "2"}])),
        Vec::<String>::new()
    );
    assert_eq!(
        rows_with(&db, "? NOT IN ['a', 'b']", json!([{"S": "c"}])),
        all_but(&[])
    );
}

#[test]
fn not_between_is_refused_in_betweens_words() {
    let db = db();
    assert_eq!(
        refusal(&db, "x NOT BETWEEN 3 AND 1"),
        "The BETWEEN operator requires upper bound to be greater than or equal to lower \
         bound; lower bound operand: AttributeValue: {N:3}, upper bound operand: \
         AttributeValue: {N:1}"
    );
    assert_eq!(
        refusal(&db, "x NOT BETWEEN 1 AND 'z'"),
        "The BETWEEN operator requires same data type for lower and upper bounds; lower \
         bound operand: AttributeValue: {N:1}, upper bound operand: AttributeValue: {S:z}"
    );
    assert_eq!(
        refusal(&db, "attribute_exists(x) NOT BETWEEN 1 AND 3"),
        "Incorrect operand type for operator or function; operator or function: BETWEEN, \
         operand type: BOOL"
    );
    assert_eq!(
        refusal(&db, "x NOT BETWEEN 1 AND x"),
        "The first operand must be distinct from the remaining operands for this operator or \
         function; operator: BETWEEN, first operand: [x]"
    );
    assert_eq!(
        refusal(&db, "x NOT IN [1, x]"),
        "The first operand must be distinct from the remaining operands for this operator or \
         function; operator: IN, first operand: [x]"
    );
    assert_eq!(
        refusal(&db, "x NOT BETWEEN 1 AND MISSING"),
        "MISSING must be compared using IS operator"
    );
    assert_eq!(
        refusal(&db, "x NOT BETWEEN 1"),
        format!("{NOT_WELL_FORMED}Expected AND keyword")
    );
    // Inferred: BETWEEN reads its bounds as NOT BETWEEN does.
    assert_eq!(
        refusal(&db, "x BETWEEN 1 OR 2"),
        format!("{NOT_WELL_FORMED}Expected AND keyword")
    );
    assert_eq!(
        refusal(&db, "x NOT NOT BETWEEN 1 AND 3"),
        format!("{NOT_WELL_FORMED}Unexpected token after expression")
    );
}

#[test]
fn not_between_conditions_a_write() {
    let db = db();
    let ccf = |result: Result<Vec<Item>, DynoxideError>| {
        assert!(
            matches!(
                result,
                Err(DynoxideError::ConditionalCheckFailedException(..))
            ),
            "{result:?}"
        );
    };
    ccf(run(
        &db,
        r#"UPDATE "NbItems" SET z = 1 WHERE pk = 'r2' AND x NOT BETWEEN 1 AND 3"#,
        Value::Null,
    ));
    assert!(!stored(&db, "r2").unwrap().contains_key("z"));
    for pk in ["r4", "r7", "r6"] {
        run(
            &db,
            &format!(r#"UPDATE "NbItems" SET z = 1 WHERE pk = '{pk}' AND x NOT BETWEEN 1 AND 3"#),
            Value::Null,
        )
        .unwrap_or_else(|e| panic!("{pk}: {e:?}"));
        assert_eq!(
            stored(&db, pk).unwrap().get("z"),
            Some(&AttributeValue::N("1".to_string())),
            "{pk}"
        );
    }
    assert!(matches!(
        run(
            &db,
            r#"UPDATE "NbItems" SET z = 1 WHERE pk NOT BETWEEN 'a' AND 'b'"#,
            Value::Null,
        ),
        Err(DynoxideError::ValidationException(m))
            if m == "Where clause does not contain a mandatory equality on all key attributes"
    ));
    ccf(run(
        &db,
        r#"DELETE FROM "NbItems" WHERE pk = 'd2' AND x NOT BETWEEN 1 AND 3"#,
        Value::Null,
    ));
    assert!(stored(&db, "d2").is_some());
    run(
        &db,
        r#"DELETE FROM "NbItems" WHERE pk = 'd1' AND x NOT BETWEEN 1 AND 3"#,
        Value::Null,
    )
    .unwrap();
    assert!(stored(&db, "d1").is_none());
}

#[test]
fn not_between_conditions_a_batch_and_a_transaction() {
    let db = db();
    let response = db
        .batch_execute_statement(
            serde_json::from_value::<BatchExecuteStatementRequest>(json!({"Statements": [
                {"Statement": "UPDATE \"NbItems\" SET z = 2 WHERE pk = 'r2' AND x NOT BETWEEN 1 AND 3"},
                {"Statement": "UPDATE \"NbItems\" SET z = 2 WHERE pk = 'r4' AND x NOT BETWEEN 1 AND 3"}
            ]}))
            .unwrap(),
        )
        .unwrap();
    let errors: Vec<Option<(String, String)>> = response
        .responses
        .iter()
        .map(|r| {
            r.error
                .as_ref()
                .map(|e| (e.code.clone(), e.message.clone()))
        })
        .collect();
    assert_eq!(
        errors,
        [
            Some((
                "ConditionalCheckFailed".to_string(),
                "The conditional request failed".to_string()
            )),
            None
        ]
    );
    assert_eq!(
        stored(&db, "r4").unwrap().get("z"),
        Some(&AttributeValue::N("2".to_string()))
    );

    let transaction = |statements: Value| {
        db.execute_transaction(
            serde_json::from_value::<ExecuteTransactionRequest>(
                json!({"TransactStatements": statements}),
            )
            .unwrap(),
        )
    };
    match transaction(json!([
        {"Statement": "UPDATE \"NbItems\" SET z = 3 WHERE pk = 'r4' AND x NOT BETWEEN 1 AND 3"},
        {"Statement": "UPDATE \"NbItems\" SET z = 3 WHERE pk = 'r2' AND x NOT BETWEEN 1 AND 3"}
    ])) {
        Err(DynoxideError::TransactionCanceledException(message, _)) => assert!(
            message.ends_with("[None, ConditionalCheckFailed]"),
            "{message}"
        ),
        other => panic!("expected a cancellation, got {other:?}"),
    }
    assert_eq!(
        stored(&db, "r4").unwrap().get("z"),
        Some(&AttributeValue::N("2".to_string()))
    );
    transaction(json!([
        {"Statement": "UPDATE \"NbItems\" SET z = 4 WHERE pk = 'r4' AND x NOT BETWEEN 1 AND 3"},
        {"Statement": "UPDATE \"NbItems\" SET z = 4 WHERE pk = 'r5' AND x NOT BETWEEN 1 AND 3"}
    ]))
    .unwrap();
    for pk in ["r4", "r5"] {
        assert_eq!(
            stored(&db, pk).unwrap().get("z"),
            Some(&AttributeValue::N("4".to_string())),
            "{pk}"
        );
    }
}
