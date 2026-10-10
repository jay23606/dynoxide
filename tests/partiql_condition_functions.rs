//! Condition functions in a PartiQL WHERE clause.
//!
//! DynamoDB accepts `attribute_exists(path)` and `attribute_not_exists(path)`
//! in a SELECT, UPDATE or DELETE WHERE clause, in any case, and compared with
//! a boolean, a path, a size or a parameter (captured 2026-10-10 for the last
//! three). A literal or parameter operand, or the wrong number of operands,
//! is refused without the syntax envelope. A function it doesn't recognise,
//! `EXISTS(path)` among them, is refused as `Unrecognized function: <name> at
//! line:column:length`, ahead of an operand refusal; a syntax error comes
//! before both, and the table is looked up after all three (captured
//! eu-west-2 and us-east-1, 2026-10-09).

use dynoxide::Database;
use dynoxide::actions::batch_execute_statement::{
    BatchExecuteStatementRequest, BatchStatementRequest,
};
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::{ExecuteStatementRequest, ExecuteStatementResponse};
use dynoxide::actions::execute_transaction::{ExecuteTransactionRequest, ParameterizedStatement};
use dynoxide::actions::get_item::GetItemRequest;
use dynoxide::actions::put_item::PutItemRequest;
use dynoxide::errors::DynoxideError;
use dynoxide::types::{
    AttributeDefinition, AttributeValue, Item, KeySchemaElement, KeyType, ScalarAttributeType,
};
use std::collections::HashMap;

fn s(v: &str) -> AttributeValue {
    AttributeValue::S(v.to_string())
}

fn n(v: &str) -> AttributeValue {
    AttributeValue::N(v.to_string())
}

fn db() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(CreateTableRequest {
        table_name: "Things".to_string(),
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
    let items: Vec<Item> = vec![
        HashMap::from([
            ("pk".to_string(), s("a1")),
            (
                "ss".to_string(),
                AttributeValue::SS(vec!["a".to_string(), "b".to_string()]),
            ),
            ("l".to_string(), AttributeValue::L(vec![n("1")])),
            (
                "m".to_string(),
                AttributeValue::M(HashMap::from([("sub".to_string(), s("x"))])),
            ),
        ]),
        HashMap::from([("pk".to_string(), s("a2")), ("q".to_string(), n("1"))]),
        HashMap::from([("pk".to_string(), s("a3"))]),
        HashMap::from([("pk".to_string(), s("a4"))]),
    ];
    for item in items {
        db.put_item(PutItemRequest {
            table_name: "Things".to_string(),
            item,
            ..Default::default()
        })
        .unwrap();
    }
    db
}

fn run(
    db: &Database,
    statement: &str,
    parameters: Option<Vec<AttributeValue>>,
) -> Result<ExecuteStatementResponse, DynoxideError> {
    db.execute_statement(ExecuteStatementRequest {
        statement: statement.to_string(),
        parameters,
        ..Default::default()
    })
}

/// The `pk` of every item a SELECT returns, sorted and comma-joined.
fn pks(db: &Database, statement: &str) -> String {
    let response = run(db, statement, None).unwrap_or_else(|e| panic!("{statement}: {e}"));
    let mut pks: Vec<String> = response
        .items
        .unwrap_or_default()
        .iter()
        .map(|item| match item.get("pk") {
            Some(AttributeValue::S(pk)) => pk.clone(),
            other => panic!("{statement}: no pk in {other:?}"),
        })
        .collect();
    pks.sort();
    pks.join(",")
}

fn refusal(db: &Database, statement: &str, parameters: Option<Vec<AttributeValue>>) -> String {
    match run(db, statement, parameters) {
        Err(DynoxideError::ValidationException(m)) => m,
        other => panic!("{statement}: expected a ValidationException, got {other:?}"),
    }
}

fn exists(db: &Database, pk: &str) -> bool {
    db.get_item(GetItemRequest {
        table_name: "Things".to_string(),
        key: HashMap::from([("pk".to_string(), s(pk))]),
        ..Default::default()
    })
    .unwrap()
    .item
    .is_some()
}

fn attribute(db: &Database, pk: &str, name: &str) -> Option<AttributeValue> {
    db.get_item(GetItemRequest {
        table_name: "Things".to_string(),
        key: HashMap::from([("pk".to_string(), s(pk))]),
        ..Default::default()
    })
    .unwrap()
    .item
    .and_then(|item| item.get(name).cloned())
}

#[test]
fn attribute_exists_and_attribute_not_exists_filter_a_select() {
    let db = db();
    for (condition, expected) in [
        ("attribute_exists(ss)", "a1"),
        ("attribute_exists(nope)", ""),
        ("attribute_not_exists(nope)", "a1"),
        ("attribute_not_exists(ss)", ""),
        ("NOT attribute_exists(nope)", "a1"),
        ("attribute_exists(pk)", "a1"),
        ("attribute_exists(m.sub)", "a1"),
        ("attribute_exists(m.nope)", ""),
        ("attribute_exists(\"ss\")", "a1"),
        ("ATTRIBUTE_EXISTS(ss)", "a1"),
        ("(attribute_exists(nope) OR attribute_exists(ss))", "a1"),
        ("attribute_exists(l[0])", "a1"),
        ("attribute_exists(l[3])", ""),
    ] {
        let statement = format!("SELECT pk FROM \"Things\" WHERE pk = 'a1' AND {condition}");
        assert_eq!(pks(&db, &statement), expected, "{statement}");
    }
    assert_eq!(
        pks(&db, "SELECT pk FROM \"Things\" WHERE attribute_exists(q)"),
        "a2"
    );
    assert_eq!(
        pks(
            &db,
            "SELECT pk FROM \"Things\" WHERE attribute_not_exists(pk)"
        ),
        ""
    );
}

#[test]
fn a_condition_function_compares_with_a_boolean() {
    let db = db();
    for (condition, expected) in [
        ("attribute_exists(ss) = true", "a1"),
        ("attribute_exists(ss) = false", ""),
        ("attribute_exists(nope) = false", "a1"),
        ("attribute_exists(ss) <> true", ""),
        ("attribute_not_exists(nope) = true", "a1"),
        ("attribute_exists(ss) = 1", ""),
        ("attribute_exists(ss) = 'true'", ""),
        ("true = attribute_exists(ss)", "a1"),
    ] {
        let statement = format!("SELECT pk FROM \"Things\" WHERE pk = 'a1' AND {condition}");
        assert_eq!(pks(&db, &statement), expected, "{statement}");
    }
}

/// The items the comparisons below were captured against: `a` there or not,
/// beside a `b` that is each boolean, missing, or another type.
fn compared_db() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(CreateTableRequest {
        table_name: "Compared".to_string(),
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
    let a = || ("a".to_string(), s("x"));
    let b = |v: AttributeValue| ("b".to_string(), v);
    let items: Vec<(&str, Vec<(String, AttributeValue)>)> = vec![
        ("i1", vec![a(), b(AttributeValue::BOOL(true))]),
        ("i2", vec![a(), b(AttributeValue::BOOL(false))]),
        ("i3", vec![b(AttributeValue::BOOL(false))]),
        ("i4", vec![b(AttributeValue::BOOL(true))]),
        ("i5", vec![a()]),
        ("i6", vec![]),
        ("i7", vec![a(), b(s("true"))]),
        ("i8", vec![a(), b(n("1"))]),
        ("i9", vec![a(), b(AttributeValue::NULL(true))]),
        (
            "i10",
            vec![
                a(),
                b(AttributeValue::M(HashMap::from([(
                    "c".to_string(),
                    AttributeValue::BOOL(true),
                )]))),
            ],
        ),
    ];
    for (pk, attributes) in items {
        let mut item: Item = attributes.into_iter().collect();
        item.insert("pk".to_string(), s(pk));
        db.put_item(PutItemRequest {
            table_name: "Compared".to_string(),
            item,
            ..Default::default()
        })
        .unwrap();
    }
    db
}

/// A condition function is a boolean, and compares with a path, a size or a
/// parameter as one: equal only to the same boolean, so unequal to a missing
/// value or one of another type, and in a list when any member equals it,
/// with the call written first or after a path. An ordering operator refuses
/// it. Every row is
/// DynamoDB's answer (captured eu-west-2 and us-east-1, 2026-10-10; the
/// regions agree on every row).
#[test]
fn a_condition_function_compares_with_a_path_size_or_parameter() {
    let db = compared_db();
    let all = "i1,i10,i2,i3,i4,i5,i6,i7,i8,i9";
    let not_one = "i10,i2,i4,i5,i6,i7,i8,i9";
    let a_there = "i1,i10,i2,i5,i7,i8,i9";
    let bool_param = |b: bool| Some(vec![AttributeValue::BOOL(b)]);
    for (condition, parameters, expected) in [
        ("attribute_exists(a) = b", None, "i1,i3"),
        ("attribute_exists(a) <> b", None, not_one),
        ("attribute_exists(a) != b", None, not_one),
        ("NOT attribute_exists(a) = b", None, not_one),
        ("attribute_exists(a) IN [b]", None, "i1,i3"),
        ("attribute_exists(a) IN [b, b]", None, "i1,i3"),
        (
            "attribute_exists(a) IN [b, true]",
            None,
            "i1,i10,i2,i3,i5,i7,i8,i9",
        ),
        ("attribute_exists(a) = ?", bool_param(true), a_there),
        ("attribute_exists(a) <> ?", bool_param(true), "i3,i4,i6"),
        ("attribute_exists(a) = ?", Some(vec![s("x")]), ""),
        ("attribute_exists(a) <> ?", Some(vec![s("x")]), all),
        (
            "attribute_exists(a) = ?",
            Some(vec![AttributeValue::NULL(true)]),
            "",
        ),
        ("attribute_exists(a) IN [?]", bool_param(false), "i3,i4,i6"),
        (
            "attribute_exists(a) IN [?, ?]",
            Some(vec![s("x"), AttributeValue::BOOL(true)]),
            a_there,
        ),
        ("attribute_not_exists(a) = b", None, "i2,i4"),
        ("attribute_type(a, 'S') = b", None, "i1,i3"),
        ("attribute_exists(a) = b.c", None, "i10"),
        ("attribute_exists(a) = size(b)", None, ""),
        ("attribute_exists(a) <> size(b)", None, all),
        ("attribute_exists(a) IN [size(b)]", None, ""),
        ("b = attribute_exists(a)", None, "i1,i3"),
        ("b <> attribute_exists(a)", None, not_one),
    ] {
        let statement = format!("SELECT pk FROM \"Compared\" WHERE {condition}");
        let response =
            run(&db, &statement, parameters).unwrap_or_else(|e| panic!("{statement}: {e}"));
        let mut pks: Vec<String> = response
            .items
            .unwrap_or_default()
            .iter()
            .map(|item| match item.get("pk") {
                Some(AttributeValue::S(pk)) => pk.clone(),
                other => panic!("{statement}: no pk in {other:?}"),
            })
            .collect();
        pks.sort();
        assert_eq!(pks.join(","), expected, "{statement}");
    }
    assert_eq!(
        refusal(
            &db,
            "SELECT pk FROM \"Compared\" WHERE attribute_exists(a) < b",
            None
        ),
        "Incorrect operand type for operator or function; operator or function: <, operand \
         type: BOOL"
    );
}

#[test]
fn a_condition_function_is_a_write_condition() {
    let db = db();
    run(
        &db,
        "UPDATE \"Things\" SET z = 1 WHERE pk = 'a2' AND attribute_exists(q)",
        None,
    )
    .unwrap();
    assert_eq!(attribute(&db, "a2", "z"), Some(n("1")));
    run(
        &db,
        "UPDATE \"Things\" SET y = 1 WHERE pk = 'a2' AND attribute_not_exists(y)",
        None,
    )
    .unwrap();
    assert_eq!(attribute(&db, "a2", "y"), Some(n("1")));
    run(
        &db,
        "UPDATE \"Things\" SET w = 1 WHERE pk = 'a2' AND NOT attribute_exists(w)",
        None,
    )
    .unwrap();
    assert_eq!(attribute(&db, "a2", "w"), Some(n("1")));
    run(
        &db,
        "DELETE FROM \"Things\" WHERE pk = 'a4' AND attribute_not_exists(nope)",
        None,
    )
    .unwrap();
    assert!(!exists(&db, "a4"));

    for statement in [
        "UPDATE \"Things\" SET z = 2 WHERE pk = 'a2' AND attribute_exists(nope)",
        "DELETE FROM \"Things\" WHERE pk = 'a3' AND attribute_exists(nope)",
        "UPDATE \"Things\" SET z = 1 WHERE pk = 'nope' AND attribute_not_exists(pk)",
    ] {
        match run(&db, statement, None) {
            Err(DynoxideError::ConditionalCheckFailedException(m, _)) => {
                assert_eq!(m, "The conditional request failed", "{statement}")
            }
            other => panic!("{statement}: expected a failed condition, got {other:?}"),
        }
    }
    assert_eq!(attribute(&db, "a2", "z"), Some(n("1")));
    assert!(exists(&db, "a3"));
}

#[test]
fn an_operand_a_condition_function_cant_take_is_refused_bare() {
    let db = db();
    let path = "Operator or function requires a document path; operator or function: ";
    let count = "Incorrect number of operands for operator or function; operator or function: ";
    for (condition, parameters, expected) in [
        (
            "attribute_exists('ss')",
            None,
            format!("{path}attribute_exists"),
        ),
        (
            "attribute_exists(?)",
            Some(vec![s("ss")]),
            format!("{path}attribute_exists"),
        ),
        (
            "ATTRIBUTE_EXISTS('ss')",
            None,
            format!("{path}attribute_exists"),
        ),
        (
            "attribute_not_exists(1)",
            None,
            format!("{path}attribute_not_exists"),
        ),
        (
            "attribute_exists()",
            None,
            format!("{count}attribute_exists, number of operands: 0"),
        ),
        (
            "attribute_exists(ss, l)",
            None,
            format!("{count}attribute_exists, number of operands: 2"),
        ),
        (
            "attribute_not_exists()",
            None,
            format!("{count}attribute_not_exists, number of operands: 0"),
        ),
    ] {
        let statement = format!("SELECT pk FROM \"Things\" WHERE pk = 'a1' AND {condition}");
        assert_eq!(
            refusal(&db, &statement, parameters),
            expected,
            "{statement}"
        );
    }
}

#[test]
fn an_unrecognised_function_is_refused_with_its_position() {
    let db = db();
    for (condition, expected) in [
        ("EXISTS(ss)", "Unrecognized function: exists at 1:45:6"),
        ("exists(ss)", "Unrecognized function: exists at 1:45:6"),
        ("NOT EXISTS(ss)", "Unrecognized function: exists at 1:49:6"),
        ("foo(ss)", "Unrecognized function: foo at 1:45:3"),
        ("foo(ss) = 1", "Unrecognized function: foo at 1:45:3"),
        (
            "set_add(ss, <<'x'>>) = ss",
            "Unrecognized function: set_add at 1:45:7",
        ),
        // An unrecognised function is reported ahead of an operand refusal,
        // wherever each is written.
        (
            "EXISTS(ss) AND attribute_exists('ss')",
            "Unrecognized function: exists at 1:45:6",
        ),
        (
            "attribute_exists('ss') AND EXISTS(ss)",
            "Unrecognized function: exists at 1:72:6",
        ),
    ] {
        let statement = format!("SELECT pk FROM \"Things\" WHERE pk = 'a1' AND {condition}");
        assert_eq!(refusal(&db, &statement, None), expected, "{statement}");
    }
    assert_eq!(
        refusal(&db, "SELECT pk FROM \"Things\" WHERE EXISTS(ss)", None),
        "Unrecognized function: exists at 1:31:6"
    );
}

#[test]
fn a_syntax_error_comes_first_and_the_table_is_looked_up_last() {
    let db = db();
    assert_eq!(
        refusal(
            &db,
            "SELECT pk FROM \"Things\" WHERE pk = 'a1' AND attribute_exists('ss') AND",
            None
        ),
        "Statement wasn't well formed, can't be processed: \
         Missing right-hand side expression of infix operator"
    );
    assert_eq!(
        refusal(
            &db,
            "SELECT * FROM \"Missing\" WHERE pk = 'a' AND attribute_exists('x')",
            None
        ),
        "Operator or function requires a document path; operator or function: attribute_exists"
    );
    assert_eq!(
        refusal(
            &db,
            "SELECT * FROM \"Missing\" WHERE pk = 'a' AND EXISTS(x)",
            None
        ),
        "Unrecognized function: exists at 1:44:6"
    );
    match run(
        &db,
        "SELECT * FROM \"Missing\" WHERE pk = 'a' AND attribute_exists(x)",
        None,
    ) {
        Err(DynoxideError::ResourceNotFoundException(_)) => {}
        other => panic!("expected ResourceNotFound, got {other:?}"),
    }
}

#[test]
fn transactions_and_batches_read_a_condition_function() {
    let db = db();
    let err = db
        .execute_transaction(ExecuteTransactionRequest {
            transact_statements: vec![ParameterizedStatement {
                statement: "SELECT * FROM \"Things\" WHERE pk = 'a1' AND attribute_exists(ss)"
                    .to_string(),
                parameters: None,
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(
            "Select statements within ExecuteTransaction must specify the primary key in the \
             where clause."
        ),
        "{err}"
    );

    db.execute_transaction(ExecuteTransactionRequest {
        transact_statements: vec![ParameterizedStatement {
            statement: "UPDATE \"Things\" SET t = 1 WHERE pk = 'a2' AND attribute_exists(q)"
                .to_string(),
            parameters: None,
            ..Default::default()
        }],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(attribute(&db, "a2", "t"), Some(n("1")));

    // Reads and writes can't share a batch, so each goes on its own.
    let batch = |statement: &str| {
        let mut member = BatchStatementRequest::default();
        member.statement = statement.to_string();
        db.batch_execute_statement(BatchExecuteStatementRequest {
            statements: vec![member],
            ..Default::default()
        })
        .unwrap()
        .responses
        .remove(0)
        .error
        .expect("the member is refused")
        .message
    };
    assert_eq!(
        batch("SELECT * FROM \"Things\" WHERE pk = 'a1' AND attribute_exists(ss)"),
        "Select statements within BatchExecuteStatement must specify the primary key in the \
         where clause."
    );
    assert_eq!(
        batch("UPDATE \"Things\" SET z = 9 WHERE pk = 'a2' AND attribute_exists(nope)"),
        "The conditional request failed"
    );
}
