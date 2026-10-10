//! PartiQL's FROM-first update, and which WHERE clauses name a single item.
//!
//! DynamoDB reads `FROM "T" WHERE ... SET ...|REMOVE ...` as an UPDATE, and an
//! UPDATE of either form may carry any number of SET and REMOVE clauses in any
//! order. An UPDATE or DELETE has to pin every key attribute with an equality
//! among the WHERE clause's top-level conditions: an OR, a NOT or an IN there
//! is refused as "Where clause does not contain a mandatory equality on all key
//! attributes", even when every branch names the same item, while an OR inside
//! one of the conditions is only a condition (captured eu-west-2 and
//! us-east-1, 2026-10-09).

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

const NOT_WELL_FORMED: &str = "Statement wasn't well formed, can't be processed: ";
const MANDATORY: &str = "Where clause does not contain a mandatory equality on all key attributes";

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
    for pk in ["p1", "p2"] {
        let item: Item = HashMap::from([
            ("pk".to_string(), AttributeValue::S(pk.to_string())),
            ("a".to_string(), AttributeValue::N("1".to_string())),
            ("b".to_string(), AttributeValue::S("bee".to_string())),
        ]);
        db.put_item(PutItemRequest {
            table_name: "Things".to_string(),
            item,
            ..Default::default()
        })
        .unwrap();
    }
    db
}

fn run(db: &Database, statement: &str) -> Result<ExecuteStatementResponse, DynoxideError> {
    run_with(db, statement, None)
}

fn run_with(
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

fn refusal(db: &Database, statement: &str) -> String {
    match run(db, statement) {
        Err(DynoxideError::ValidationException(m)) => m,
        other => panic!("{statement}: expected a ValidationException, got {other:?}"),
    }
}

/// The item's attributes other than the key, as `name=value` in name order.
fn read(db: &Database, pk: &str) -> String {
    let item = db
        .get_item(GetItemRequest {
            table_name: "Things".to_string(),
            key: HashMap::from([("pk".to_string(), AttributeValue::S(pk.to_string()))]),
            ..Default::default()
        })
        .unwrap()
        .item
        .unwrap_or_default();
    let mut fields: Vec<String> = item
        .into_iter()
        .filter(|(name, _)| name != "pk")
        .map(|(name, value)| match value {
            AttributeValue::S(s) => format!("{name}={s}"),
            AttributeValue::N(n) => format!("{name}={n}"),
            other => format!("{name}={other:?}"),
        })
        .collect();
    fields.sort();
    fields.join(",")
}

#[test]
fn a_from_first_statement_runs_as_an_update() {
    for (statement, expected) in [
        (
            r#"FROM "Things" WHERE pk = 'p1' SET c = 1"#,
            "a=1,b=bee,c=1",
        ),
        (r#"FROM "Things" WHERE pk = 'p1' REMOVE b"#, "a=1"),
        (
            r#"FROM "Things" WHERE pk = 'p1' SET c = 2 REMOVE a"#,
            "b=bee,c=2",
        ),
        (
            r#"FROM "Things" WHERE pk = 'p1' REMOVE b SET c = 1"#,
            "a=1,c=1",
        ),
        (
            r#"FROM "Things" WHERE pk = 'p1' SET c = 1 REMOVE a SET d = 2"#,
            "b=bee,c=1,d=2",
        ),
        (
            r#"FROM "Things" WHERE pk = 'p1' SET c = 1 SET d = 2"#,
            "a=1,b=bee,c=1,d=2",
        ),
        (
            r#"from "Things" where pk = 'p1' set e = 4"#,
            "a=1,b=bee,e=4",
        ),
        (
            r#"FROM "Things" WHERE pk = 'p1' SET c = 1;"#,
            "a=1,b=bee,c=1",
        ),
        (
            r#"FROM "Things" WHERE pk = 'p1' AND a = 1 SET c = 5"#,
            "a=1,b=bee,c=5",
        ),
    ] {
        let db = db();
        let response = run(&db, statement).unwrap_or_else(|e| panic!("{statement}: {e}"));
        assert_eq!(response.items, Some(vec![]), "{statement}");
        assert_eq!(read(&db, "p1"), expected, "{statement}");
        assert_eq!(read(&db, "p2"), "a=1,b=bee", "{statement}: only p1 changes");
    }
}

#[test]
fn a_from_first_statement_takes_parameters_and_returning() {
    let db = db();
    run_with(
        &db,
        r#"FROM "Things" WHERE pk = ? SET c = ?"#,
        Some(vec![
            AttributeValue::S("p1".to_string()),
            AttributeValue::N("7".to_string()),
        ]),
    )
    .unwrap();
    assert_eq!(read(&db, "p1"), "a=1,b=bee,c=7");

    let response = run(
        &db,
        r#"FROM "Things" WHERE pk = 'p2' SET d = 3 RETURNING ALL NEW *"#,
    )
    .unwrap();
    let items = response.items.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get("d"), Some(&AttributeValue::N("3".to_string())));
    assert_eq!(
        items[0].get("b"),
        Some(&AttributeValue::S("bee".to_string()))
    );
}

#[test]
fn a_from_first_statement_fails_its_condition_like_an_update() {
    let db = db();
    for statement in [
        r#"FROM "Things" WHERE pk = 'p1' AND a = 2 SET c = 5"#,
        r#"FROM "Things" WHERE pk = 'nope' SET c = 1"#,
    ] {
        match run(&db, statement) {
            Err(DynoxideError::ConditionalCheckFailedException(m, _)) => {
                assert_eq!(m, "The conditional request failed", "{statement}")
            }
            other => panic!("{statement}: expected a failed condition, got {other:?}"),
        }
    }
    assert_eq!(read(&db, "p1"), "a=1,b=bee");
}

#[test]
fn a_from_first_statement_that_is_not_an_update_is_refused_as_dynamodb_refuses_it() {
    let db = db();
    for (statement, expected) in [
        (r#"FROM "Things" SET c = 1"#, MANDATORY.to_string()),
        (r#"FROM "Things" REMOVE a"#, MANDATORY.to_string()),
        (
            r#"FROM "Things" WHERE a = 1 SET c = 1"#,
            MANDATORY.to_string(),
        ),
        (
            r#"FROM "Things" WHERE pk = 'p1' OR pk = 'p2' SET c = 1"#,
            MANDATORY.to_string(),
        ),
        (
            r#"FROM "Things" WHERE pk = 'p1' OR pk = 'p1' SET c = 1"#,
            MANDATORY.to_string(),
        ),
        (
            r#"FROM "Things" WHERE pk IN ['p1'] SET c = 1"#,
            MANDATORY.to_string(),
        ),
        (
            r#"FROM "Things" SET c = 1 WHERE pk = 'p1'"#,
            format!("{NOT_WELL_FORMED}Unexpected token after expression"),
        ),
        (
            r#"FROM "Things" SET c = 1 WHERE pk = 'p1' SET d = 2"#,
            format!("{NOT_WELL_FORMED}Unexpected token after expression"),
        ),
        (
            r#"FROM "Things" WHERE pk = 'p1' SET c = 1 WHERE pk = 'p1'"#,
            format!("{NOT_WELL_FORMED}Unexpected token after expression"),
        ),
        (
            r#"FROM "Things""#,
            format!("{NOT_WELL_FORMED}Expected data manipulation"),
        ),
        (
            r#"FROM "Things" WHERE pk = 'p1'"#,
            format!("{NOT_WELL_FORMED}Expected data manipulation"),
        ),
        (
            r#"FROM "Things" WHERE pk = 'p1' DELETE"#,
            format!("{NOT_WELL_FORMED}Expected data manipulation"),
        ),
        (
            r#"FROM "Things" WHERE pk = 'p1' SELECT *"#,
            format!("{NOT_WELL_FORMED}Expected data manipulation"),
        ),
        (
            r#"FROM "Things" WHERE SET c = 1"#,
            format!("{NOT_WELL_FORMED}Expected data manipulation"),
        ),
        (
            r#"FROM "Things" WHERE pk = 'p1' REMOVE"#,
            format!("{NOT_WELL_FORMED}Expected identifier for simple path"),
        ),
        (
            r#"FROM "Things" WHERE pk = 'p1' SET"#,
            format!("{NOT_WELL_FORMED}Expected identifier for simple path"),
        ),
        (
            r#"FROM "Things" t WHERE pk = 'p1' SET c = 1"#,
            "Aliasing is not supported".to_string(),
        ),
    ] {
        assert_eq!(refusal(&db, statement), expected, "{statement}");
    }
    assert_eq!(read(&db, "p1"), "a=1,b=bee");
}

#[test]
fn a_from_first_statement_on_a_missing_table_is_not_found_before_its_where_is_judged() {
    let db = db();
    for statement in [
        r#"FROM "Missing" WHERE pk = 'x' SET c = 1"#,
        r#"FROM "Missing" SET c = 1"#,
    ] {
        match run(&db, statement) {
            Err(DynoxideError::ResourceNotFoundException(_)) => {}
            other => panic!("{statement}: expected ResourceNotFound, got {other:?}"),
        }
    }
}

#[test]
fn transactions_and_batches_run_a_from_first_statement() {
    let db = db();
    let response = db
        .execute_transaction(ExecuteTransactionRequest {
            transact_statements: vec![ParameterizedStatement {
                statement: r#"FROM "Things" WHERE pk = 'p1' SET c = 1"#.to_string(),
                parameters: None,
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(response.responses.map(|r| r.len()), Some(0));
    assert_eq!(read(&db, "p1"), "a=1,b=bee,c=1");

    let mut member = BatchStatementRequest::default();
    member.statement = r#"FROM "Things" WHERE pk = 'p2' SET c = 1"#.to_string();
    let response = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            statements: vec![member],
            ..Default::default()
        })
        .unwrap();
    assert!(
        response.responses[0].error.is_none(),
        "{:?}",
        response.responses[0]
    );
    assert_eq!(read(&db, "p2"), "a=1,b=bee,c=1");
}

#[test]
fn an_update_takes_set_and_remove_clauses_in_any_order_and_number() {
    for (statement, expected) in [
        (
            r#"UPDATE "Things" SET c = 1 SET d = 2 WHERE pk = 'p1'"#,
            "a=1,b=bee,c=1,d=2",
        ),
        (
            r#"UPDATE "Things" REMOVE a SET c = 1 WHERE pk = 'p1'"#,
            "b=bee,c=1",
        ),
        (
            r#"UPDATE "Things" SET c = 1 REMOVE a SET d = 2 WHERE pk = 'p1'"#,
            "b=bee,c=1,d=2",
        ),
        (r#"UPDATE "Things" REMOVE a REMOVE b WHERE pk = 'p1'"#, ""),
    ] {
        let db = db();
        run(&db, statement).unwrap_or_else(|e| panic!("{statement}: {e}"));
        assert_eq!(read(&db, "p1"), expected, "{statement}");
    }
    let db = db();
    assert_eq!(
        refusal(
            &db,
            r#"UPDATE "Things" SET c = 1 WHERE pk = 'p1' SET d = 2"#
        ),
        format!("{NOT_WELL_FORMED}Unexpected token after expression")
    );
    assert_eq!(read(&db, "p1"), "a=1,b=bee");
}

#[test]
fn an_update_or_delete_needs_its_key_equality_among_the_top_level_conditions() {
    let db = db();
    for statement in [
        r#"UPDATE "Things" SET c = 1"#,
        r#"DELETE FROM "Things""#,
        r#"UPDATE "Things" SET c = 1 WHERE pk = 'p1' OR pk = 'p2'"#,
        r#"UPDATE "Things" SET c = 1 WHERE pk = 'p1' OR pk = 'p1'"#,
        r#"DELETE FROM "Things" WHERE pk = 'p1' OR pk = 'p2'"#,
        r#"UPDATE "Things" SET c = 1 WHERE NOT (NOT pk = 'p1' AND NOT pk = 'p2')"#,
        r#"UPDATE "Things" SET c = 1 WHERE NOT (NOT pk = 'p1')"#,
        r#"UPDATE "Things" SET c = 1 WHERE pk IN ['p1']"#,
        r#"DELETE FROM "Things" WHERE a = 1"#,
    ] {
        assert_eq!(refusal(&db, statement), MANDATORY, "{statement}");
    }
    assert_eq!(read(&db, "p1"), "a=1,b=bee");
    assert_eq!(read(&db, "p2"), "a=1,b=bee");
}

#[test]
fn two_different_values_for_the_key_are_refused_as_more_than_one_item() {
    let db = db();
    assert_eq!(
        refusal(
            &db,
            r#"UPDATE "Things" SET c = 1 WHERE pk = 'p1' AND pk = 'p2'"#
        ),
        "Multiple conditions on same key pk. Only single item Update/Insert/Delete are supported"
    );
    assert_eq!(read(&db, "p1"), "a=1,b=bee");
}

#[test]
fn an_or_inside_a_top_level_condition_is_only_a_condition() {
    for (statement, expected) in [
        (
            r#"UPDATE "Things" SET c = 2 WHERE pk = 'p1' AND (c = 'q' OR d IS MISSING)"#,
            "a=1,b=bee,c=2",
        ),
        (
            r#"UPDATE "Things" SET c = 3 WHERE (pk = 'p1')"#,
            "a=1,b=bee,c=3",
        ),
        (
            r#"UPDATE "Things" SET c = 4 WHERE pk = 'p1' AND pk = 'p1'"#,
            "a=1,b=bee,c=4",
        ),
        (
            r#"UPDATE "Things" SET c = 6 WHERE (pk = 'p1' AND d IS MISSING)"#,
            "a=1,b=bee,c=6",
        ),
        (
            r#"UPDATE "Things" SET c = 1 WHERE pk = 'p1' AND NOT (c = 'q' AND d = 'q')"#,
            "a=1,b=bee,c=1",
        ),
    ] {
        let db = db();
        run(&db, statement).unwrap_or_else(|e| panic!("{statement}: {e}"));
        assert_eq!(read(&db, "p1"), expected, "{statement}");
        assert_eq!(read(&db, "p2"), "a=1,b=bee", "{statement}");
    }
    let db = db();
    run(
        &db,
        r#"DELETE FROM "Things" WHERE pk = 'p2' AND (c = 'q' OR d IS MISSING)"#,
    )
    .unwrap();
    assert_eq!(read(&db, "p2"), "");
}
