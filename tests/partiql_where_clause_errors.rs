//! A PartiQL WHERE clause that stops where a condition should start.
//!
//! DynamoDB reports `Unexpected term` when nothing follows `WHERE`, `NOT` or an
//! opening parenthesis, `Missing right-hand side expression of infix operator`
//! when nothing follows `AND` or `OR`, and `Unexpected keyword` when a keyword
//! stands where the condition should be. The table isn't looked up first
//! (captured eu-west-2, us-east-1, eu-west-1 and us-west-2, 2026-10-09).

use dynoxide::Database;
use dynoxide::actions::batch_execute_statement::{
    BatchExecuteStatementRequest, BatchStatementRequest,
};
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::{ExecuteStatementRequest, ExecuteStatementResponse};
use dynoxide::actions::execute_transaction::{ExecuteTransactionRequest, ParameterizedStatement};
use dynoxide::errors::DynoxideError;
use dynoxide::types::{AttributeDefinition, KeySchemaElement, KeyType, ScalarAttributeType};

const NOT_WELL_FORMED: &str = "Statement wasn't well formed, can't be processed: ";

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
    db
}

fn run(db: &Database, statement: &str) -> Result<ExecuteStatementResponse, DynoxideError> {
    db.execute_statement(ExecuteStatementRequest {
        statement: statement.to_string(),
        ..Default::default()
    })
}

fn refusal(db: &Database, statement: &str) -> String {
    match run(db, statement) {
        Err(DynoxideError::ValidationException(m)) => m,
        other => panic!("{statement}: expected a ValidationException, got {other:?}"),
    }
}

#[test]
fn nothing_where_a_condition_should_start_is_an_unexpected_term() {
    let db = db();
    for statement in [
        r#"SELECT * FROM "Things" WHERE"#,
        r#"SELECT * FROM "Things" WHERE;"#,
        r#"SELECT * FROM "Things" WHERE   "#,
        r#"SELECT pk FROM "Things" WHERE"#,
        r#"UPDATE "Things" SET a = 1 WHERE"#,
        r#"DELETE FROM "Things" WHERE"#,
        r#"SELECT * FROM "Things" WHERE NOT"#,
        r#"SELECT * FROM "Things" WHERE ("#,
        r#"SELECT * FROM "Missing" WHERE"#,
    ] {
        assert_eq!(
            refusal(&db, statement),
            format!("{NOT_WELL_FORMED}Unexpected term"),
            "{statement}"
        );
    }
}

#[test]
fn nothing_after_and_or_or_is_a_missing_right_hand_side() {
    let db = db();
    for statement in [
        r#"SELECT * FROM "Things" WHERE pk = 'p1' AND"#,
        r#"SELECT * FROM "Things" WHERE pk = 'p1' OR"#,
    ] {
        assert_eq!(
            refusal(&db, statement),
            format!("{NOT_WELL_FORMED}Missing right-hand side expression of infix operator"),
            "{statement}"
        );
    }
}

#[test]
fn an_operator_where_a_condition_should_start_is_an_unexpected_operator() {
    // Captured eu-west-2 and us-east-1, 2026-10-09.
    let db = db();
    assert_eq!(
        refusal(&db, r#"SELECT * FROM "Things" WHERE IS NULL"#),
        format!("{NOT_WELL_FORMED}Unexpected operator")
    );
}

#[test]
fn a_keyword_where_a_condition_should_start_is_an_unexpected_keyword() {
    let db = db();
    assert_eq!(
        refusal(&db, r#"SELECT * FROM "Things" WHERE ORDER BY pk"#),
        format!("{NOT_WELL_FORMED}Unexpected keyword")
    );
}

#[test]
fn transactions_and_batches_report_it_the_same_way() {
    let db = db();
    let err = db
        .execute_transaction(ExecuteTransactionRequest {
            transact_statements: vec![ParameterizedStatement {
                statement: r#"SELECT * FROM "Things" WHERE"#.to_string(),
                parameters: None,
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("Validation failed in TransactStatements[0]: {NOT_WELL_FORMED}Unexpected term")
    );

    let mut statement = BatchStatementRequest::default();
    statement.statement = r#"SELECT * FROM "Things" WHERE"#.to_string();
    let resp = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            return_consumed_capacity: None,
            statements: vec![statement],
        })
        .unwrap();
    let error = resp.responses[0]
        .error
        .as_ref()
        .expect("a per-statement error");
    assert_eq!(error.message, format!("{NOT_WELL_FORMED}Unexpected term"));
}
