//! PartiQL's `IS`, operators where a value should be, negated sort key
//! conditions in the overlap check, and a FROM with nothing after it.
//!
//! In a WHERE clause `IS` tests MISSING, NULL, BOOLEAN, BLOB, STRING, NUMERIC,
//! STRUCT, TUPLE and LIST, `IS NOT` the complement, a missing attribute
//! included. Every other type name PartiQL knows is refused naming the type,
//! and a word that isn't one is a syntax error. Outside a statement the four
//! longer names (INT4, INT8, DOUBLE PRECISION, CHARACTER VARYING) read as type
//! names too. An operator standing where a value should be is an unexpected
//! operator. A negated or `<>` sort key condition in any branch stops the
//! overlap check. And a FROM with nothing usable after it is refused in
//! DynamoDB's words (captured eu-west-2 and us-east-1, 2026-10-09).

use dynoxide::Database;
use dynoxide::actions::execute_statement::{ExecuteStatementRequest, ExecuteStatementResponse};
use dynoxide::actions::put_item::PutItemRequest;
use dynoxide::errors::DynoxideError;
use dynoxide::types::{AttributeValue, Item};
use std::collections::HashMap;

const NOT_WELL_FORMED: &str = "Statement wasn't well formed, can't be processed: ";

fn s(v: &str) -> AttributeValue {
    AttributeValue::S(v.to_string())
}

fn table(name: &str, sort_key: bool) -> serde_json::Value {
    let mut keys = vec![serde_json::json!({"AttributeName": "pk", "KeyType": "HASH"})];
    let mut defs = vec![serde_json::json!({"AttributeName": "pk", "AttributeType": "S"})];
    if sort_key {
        keys.push(serde_json::json!({"AttributeName": "sk", "KeyType": "RANGE"}));
        defs.push(serde_json::json!({"AttributeName": "sk", "AttributeType": "S"}));
    }
    serde_json::json!({
        "TableName": name,
        "KeySchema": keys,
        "AttributeDefinitions": defs,
        "BillingMode": "PAY_PER_REQUEST"
    })
}

fn put(db: &Database, table: &str, item: Item) {
    db.put_item(PutItemRequest {
        table_name: table.to_string(),
        item,
        ..Default::default()
    })
    .unwrap();
}

/// A table holding one item per type in `x`, and one with no `x`.
fn typed() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(serde_json::from_value(table("Typed", false)).unwrap())
        .unwrap();
    db.create_table(serde_json::from_value(table("Keyed", true)).unwrap())
        .unwrap();
    let values: Vec<(&str, Option<AttributeValue>)> = vec![
        ("v_s", Some(s("a"))),
        ("v_es", Some(s(""))),
        ("v_n", Some(AttributeValue::N("1".to_string()))),
        ("v_nd", Some(AttributeValue::N("1.5".to_string()))),
        ("v_b", Some(AttributeValue::B(vec![1]))),
        ("v_bool", Some(AttributeValue::BOOL(true))),
        ("v_null", Some(AttributeValue::NULL(true))),
        (
            "v_l",
            Some(AttributeValue::L(vec![AttributeValue::N("1".to_string())])),
        ),
        (
            "v_m",
            Some(AttributeValue::M(HashMap::from([(
                "a".to_string(),
                AttributeValue::N("1".to_string()),
            )]))),
        ),
        ("v_ss", Some(AttributeValue::SS(vec!["a".to_string()]))),
        ("v_ns", Some(AttributeValue::NS(vec!["1".to_string()]))),
        ("v_bs", Some(AttributeValue::BS(vec![vec![1]]))),
        ("v_missing", None),
    ];
    for (pk, x) in values {
        let mut item: Item = HashMap::from([("pk".to_string(), s(pk))]);
        if let Some(x) = x {
            item.insert("x".to_string(), x);
        }
        put(&db, "Typed", item);
    }
    for (pk, sk) in [("a", "1"), ("a", "2"), ("a", "3"), ("b", "1")] {
        put(
            &db,
            "Keyed",
            HashMap::from([("pk".to_string(), s(pk)), ("sk".to_string(), s(sk))]),
        );
    }
    db
}

fn run(db: &Database, statement: &str) -> Result<ExecuteStatementResponse, DynoxideError> {
    db.execute_statement(ExecuteStatementRequest {
        statement: statement.to_string(),
        ..Default::default()
    })
}

fn pks(db: &Database, statement: &str) -> String {
    let response = run(db, statement).unwrap_or_else(|e| panic!("{statement}: {e}"));
    let mut pks: Vec<String> = response
        .items
        .unwrap_or_default()
        .iter()
        .map(|item| match (item.get("pk"), item.get("sk")) {
            (Some(AttributeValue::S(pk)), Some(AttributeValue::S(sk))) => format!("{pk}{sk}"),
            (Some(AttributeValue::S(pk)), None) => pk.clone(),
            other => panic!("{statement}: {other:?}"),
        })
        .collect();
    pks.sort();
    pks.join(",")
}

fn refusal(db: &Database, statement: &str) -> String {
    match run(db, statement) {
        Err(DynoxideError::ValidationException(m)) => m,
        other => panic!("{statement}: expected a ValidationException, got {other:?}"),
    }
}

const ALL: &[&str] = &[
    "v_b",
    "v_bool",
    "v_bs",
    "v_es",
    "v_l",
    "v_m",
    "v_missing",
    "v_n",
    "v_nd",
    "v_ns",
    "v_null",
    "v_s",
    "v_ss",
];

fn all_but(excluded: &[&str]) -> String {
    ALL.iter()
        .filter(|pk| !excluded.contains(pk))
        .copied()
        .collect::<Vec<_>>()
        .join(",")
}

#[test]
fn is_tests_the_types_dynamodb_supports_in_a_where_clause() {
    let db = typed();
    for (type_name, matches) in [
        ("NULL", vec!["v_null"]),
        ("MISSING", vec!["v_missing"]),
        ("BOOL", vec!["v_bool"]),
        ("BOOLEAN", vec!["v_bool"]),
        ("NUMERIC", vec!["v_n", "v_nd"]),
        ("STRING", vec!["v_es", "v_s"]),
        ("BLOB", vec!["v_b"]),
        ("STRUCT", vec!["v_m"]),
        ("TUPLE", vec!["v_m"]),
        ("LIST", vec!["v_l"]),
    ] {
        assert_eq!(
            pks(
                &db,
                &format!("SELECT pk FROM \"Typed\" WHERE x IS {type_name}")
            ),
            matches.join(","),
            "IS {type_name}"
        );
        assert_eq!(
            pks(
                &db,
                &format!("SELECT pk FROM \"Typed\" WHERE x IS NOT {type_name}")
            ),
            all_but(&matches),
            "IS NOT {type_name}"
        );
    }
    assert_eq!(
        pks(&db, "SELECT pk FROM \"Typed\" WHERE x is string"),
        "v_es,v_s"
    );
    assert_eq!(
        pks(&db, "SELECT pk FROM \"Typed\" WHERE NOT (x IS NULL)"),
        all_but(&["v_null"])
    );
    assert_eq!(
        pks(
            &db,
            "SELECT pk FROM \"Typed\" WHERE pk = 'v_null' AND x IS NULL"
        ),
        "v_null"
    );
    assert_eq!(
        pks(
            &db,
            "SELECT pk FROM \"Typed\" WHERE pk = 'v_missing' AND x IS NULL"
        ),
        ""
    );
}

#[test]
fn is_is_a_write_condition() {
    let db = typed();
    run(
        &db,
        "UPDATE \"Typed\" SET y = 1 WHERE pk = 'v_null' AND x IS NULL",
    )
    .unwrap();
    match run(
        &db,
        "DELETE FROM \"Typed\" WHERE pk = 'v_n' AND x IS STRING",
    ) {
        Err(DynoxideError::ConditionalCheckFailedException(m, _)) => {
            assert_eq!(m, "The conditional request failed")
        }
        other => panic!("expected a failed condition, got {other:?}"),
    }
}

#[test]
fn another_type_name_is_refused_naming_its_type() {
    let db = typed();
    for (written, type_name) in [
        ("SMALLINT", "SmallintType"),
        ("INT", "IntegerType"),
        ("INTEGER", "IntegerType"),
        ("INT4", "Integer4Type"),
        ("INT8", "Integer8Type"),
        ("BIGINT", "Integer8Type"),
        ("FLOAT", "FloatType"),
        ("REAL", "RealType"),
        ("DOUBLE PRECISION", "DoublePrecisionType"),
        ("DECIMAL", "DecimalType"),
        ("TIMESTAMP", "TimestampType"),
        ("DATE", "DateType"),
        ("TIME", "TimeType"),
        ("CHAR", "CharacterType"),
        ("CHARACTER", "CharacterType"),
        ("CHARACTER VARYING", "CharacterVaryingType"),
        ("VARCHAR", "CharacterVaryingType"),
        ("VARCHAR(10)", "CharacterVaryingType"),
        ("CHARACTER VARYING(10)", "CharacterVaryingType"),
        ("SYMBOL", "SymbolType"),
        ("CLOB", "ClobType"),
        ("SEXP", "SexpType"),
        ("BAG", "BagType"),
        ("ANY", "AnyType"),
    ] {
        let expected = format!(
            "Unsupported data type for IS operator: {type_name}. Supported data types are \
             [MISSING, NULL, BOOLEAN, BLOB, STRING, NUMERIC, STRUCT, TUPLE, LIST] at 1:30:1"
        );
        for not in ["", "NOT "] {
            let statement = format!("SELECT pk FROM \"Typed\" WHERE x IS {not}{written}");
            assert_eq!(refusal(&db, &statement), expected, "{statement}");
        }
    }
    for written in ["DOUBLE", "FOO", "NUMBER", "MAP", "TEXT", "BINARY", "SET"] {
        let statement = format!("SELECT pk FROM \"Typed\" WHERE x IS {written}");
        assert_eq!(
            refusal(&db, &statement),
            format!("{NOT_WELL_FORMED}Expected type name"),
            "{statement}"
        );
    }
}

#[test]
fn the_longer_type_names_read_as_type_names_outside_a_statement_too() {
    let db = typed();
    for statement in [
        "x IS INT4",
        "x IS INT8",
        "x IS DOUBLE PRECISION",
        "x IS CHARACTER VARYING",
        "x IS CHAR(3)",
    ] {
        assert_eq!(
            refusal(&db, statement),
            "Unsupported operation: IsType",
            "{statement}"
        );
    }
    assert_eq!(
        refusal(&db, "x IS DOUBLE"),
        format!("{NOT_WELL_FORMED}Expected type name")
    );
}

#[test]
fn an_operator_where_a_value_should_be_is_unexpected() {
    let db = typed();
    for statement in [
        "SELECT pk FROM \"Typed\" WHERE pk = LIKE",
        "SELECT pk FROM \"Typed\" WHERE pk = IN",
        "SELECT pk FROM \"Typed\" WHERE pk = BETWEEN 1 AND 2",
        "SELECT pk FROM \"Typed\" WHERE pk = IS",
        "SELECT pk FROM \"Typed\" WHERE pk = AND",
        "SELECT pk FROM \"Typed\" WHERE pk = OR",
        "SELECT pk FROM \"Typed\" WHERE pk = 'v_s' AND x = LIKE 'a'",
    ] {
        assert_eq!(
            refusal(&db, statement),
            format!("{NOT_WELL_FORMED}Unexpected operator"),
            "{statement}"
        );
    }
    assert_eq!(
        refusal(&db, "SELECT pk FROM \"Typed\" WHERE pk = WHERE"),
        format!("{NOT_WELL_FORMED}Unexpected keyword")
    );
}

#[test]
fn a_negated_sort_key_condition_stops_the_overlap_check() {
    let db = typed();
    for clause in [
        "pk = 'a' AND NOT sk = '1'",
        "pk = 'a' AND sk <> '1'",
        "pk = 'a' AND NOT begins_with(sk, 'x')",
        "pk = 'a' AND NOT sk IN ['1', '3']",
        "pk = 'a' AND NOT sk > '5'",
        "pk = 'a' AND NOT (sk = '1')",
    ] {
        let statement = format!("SELECT * FROM \"Keyed\" WHERE {clause} OR pk = 'a' AND sk = '2'");
        run(&db, &statement).unwrap_or_else(|e| panic!("{statement}: {e}"));
    }
    run(
        &db,
        "SELECT * FROM \"Keyed\" WHERE pk = 'a' OR pk = 'a' AND NOT sk = '1'",
    )
    .unwrap();
    // The controls: a non-key condition, negated or not, doesn't.
    for clause in ["pk = 'a' AND x = '1'", "pk = 'a' AND x <> '1'"] {
        let statement = format!("SELECT * FROM \"Keyed\" WHERE {clause} OR pk = 'a' AND sk = '2'");
        assert_eq!(
            refusal(&db, &statement),
            "Overlapping conditions with range keys are not supported in where clause",
            "{statement}"
        );
    }
}

#[test]
fn a_from_with_nothing_usable_after_it_is_refused_in_dynamodbs_words() {
    let db = typed();
    for statement in [
        "DELETE FROM",
        "DELETE FROM   ",
        "DELETE FROM WHERE pk = 'a'",
        "DELETE FROM;",
    ] {
        assert_eq!(
            refusal(&db, statement),
            format!("{NOT_WELL_FORMED}Expected identifier for simple path"),
            "{statement:?}"
        );
    }
    for statement in ["SELECT * FROM", "SELECT * FROM;"] {
        assert_eq!(
            refusal(&db, statement),
            format!("{NOT_WELL_FORMED}Unexpected term"),
            "{statement:?}"
        );
    }
    assert_eq!(
        refusal(&db, "SELECT * FROM WHERE pk = 'a'"),
        format!("{NOT_WELL_FORMED}Unexpected keyword")
    );
    assert_eq!(
        refusal(&db, "DELETE"),
        format!("{NOT_WELL_FORMED}Expected FROM after DELETE")
    );
}
