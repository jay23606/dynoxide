//! What a PartiQL write hands back when it succeeds.
//!
//! DynamoDB answers a successful INSERT, UPDATE or DELETE through
//! ExecuteStatement with an empty `Items` list unless the statement asks for a
//! `RETURNING` projection, and an ExecuteTransaction of writes with an empty
//! `Responses` list rather than one empty entry per statement. A transaction of
//! reads still returns one entry per statement (captured eu-west-2, us-east-1,
//! eu-west-1 and us-west-2, 2026-10-09).

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::{ExecuteStatementRequest, ExecuteStatementResponse};
use dynoxide::actions::execute_transaction::{
    ExecuteTransactionRequest, ExecuteTransactionResponse, ParameterizedStatement,
};
use dynoxide::types::{AttributeDefinition, KeySchemaElement, KeyType, ScalarAttributeType};

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
    for pk in ["a", "b", "c"] {
        run(
            &db,
            &format!("INSERT INTO \"Things\" VALUE {{'pk': '{pk}', 'n': 1}}"),
        );
    }
    db
}

fn run(db: &Database, statement: &str) -> ExecuteStatementResponse {
    db.execute_statement(ExecuteStatementRequest {
        statement: statement.to_string(),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("{statement}: {e}"))
}

fn transact(db: &Database, statements: &[&str], capacity: Option<&str>) -> serde_json::Value {
    let resp: ExecuteTransactionResponse = db
        .execute_transaction(ExecuteTransactionRequest {
            transact_statements: statements
                .iter()
                .map(|s| ParameterizedStatement {
                    statement: s.to_string(),
                    parameters: None,
                    ..Default::default()
                })
                .collect(),
            return_consumed_capacity: capacity.map(str::to_string),
            ..Default::default()
        })
        .unwrap();
    serde_json::to_value(&resp).unwrap()
}

#[test]
fn a_write_through_execute_statement_returns_an_empty_items_list() {
    let db = db();
    for statement in [
        r#"INSERT INTO "Things" VALUE {'pk': 'new'}"#,
        r#"UPDATE "Things" SET n = 2 WHERE pk = 'a'"#,
        r#"DELETE FROM "Things" WHERE pk = 'b'"#,
        r#"DELETE FROM "Things" WHERE pk = 'missing'"#,
    ] {
        let wire = serde_json::to_value(run(&db, statement)).unwrap();
        assert_eq!(wire, serde_json::json!({ "Items": [] }), "{statement}");
    }
}

#[test]
fn a_returning_clause_still_returns_its_projection() {
    let db = db();
    let wire = serde_json::to_value(run(
        &db,
        r#"UPDATE "Things" SET n = 3 WHERE pk = 'c' RETURNING ALL NEW *"#,
    ))
    .unwrap();
    assert_eq!(
        wire,
        serde_json::json!({ "Items": [{ "pk": { "S": "c" }, "n": { "N": "3" } }] })
    );
}

#[test]
fn a_transaction_of_writes_returns_an_empty_responses_list() {
    let db = db();
    assert_eq!(
        transact(
            &db,
            &[
                r#"UPDATE "Things" SET n = 2 WHERE pk = 'a'"#,
                r#"UPDATE "Things" SET n = 2 WHERE pk = 'b'"#,
            ],
            None,
        ),
        serde_json::json!({ "Responses": [] })
    );
    assert_eq!(
        transact(
            &db,
            &[
                r#"INSERT INTO "Things" VALUE {'pk': 'tx'}"#,
                r#"DELETE FROM "Things" WHERE pk = 'c'"#,
            ],
            None,
        ),
        serde_json::json!({ "Responses": [] })
    );

    let with_capacity = transact(
        &db,
        &[r#"UPDATE "Things" SET n = 3 WHERE pk = 'a'"#],
        Some("TOTAL"),
    );
    assert_eq!(with_capacity["Responses"], serde_json::json!([]));
    assert_eq!(
        with_capacity["ConsumedCapacity"][0]["CapacityUnits"],
        serde_json::json!(2.0)
    );
}

#[test]
fn a_transaction_of_reads_still_returns_one_entry_per_statement() {
    let db = db();
    let wire = transact(
        &db,
        &[
            r#"SELECT * FROM "Things" WHERE pk = 'a'"#,
            r#"SELECT * FROM "Things" WHERE pk = 'missing'"#,
        ],
        None,
    );
    assert_eq!(
        wire,
        serde_json::json!({ "Responses": [{ "Item": { "pk": { "S": "a" }, "n": { "N": "1" } } }, {}] })
    );
}
