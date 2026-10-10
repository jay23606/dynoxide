//! DynamoDB's limits on a PartiQL statement: 8,192 code points as a request
//! member, checked before the statement is parsed, and 300 operators in a
//! WHERE clause. Captured in eu-west-2 and us-east-1, 2026-10-10; the regions
//! agree on every case here.

use dynoxide::Database;
use dynoxide::actions::batch_execute_statement::BatchExecuteStatementRequest;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::ExecuteStatementRequest;
use dynoxide::actions::execute_transaction::ExecuteTransactionRequest;
use serde_json::{Value, json};

fn setup() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value::<CreateTableRequest>(json!({
            "TableName": "Limits",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    db
}

/// A SELECT padded inside its key literal to exactly `length` code points,
/// using `pad` as the padding character.
fn select_of_length(length: usize, pad: char) -> String {
    let head = r#"SELECT * FROM "Limits" WHERE pk = '"#;
    let fixed = head.chars().count() + 1;
    format!("{head}{}'", pad.to_string().repeat(length - fixed))
}

fn single(db: &Database, statement: &str) -> Result<(), String> {
    let request: ExecuteStatementRequest =
        serde_json::from_value(json!({"Statement": statement})).unwrap();
    db.execute_statement(request)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn transaction(db: &Database, statements: &[&str]) -> Result<(), String> {
    let statements: Vec<Value> = statements.iter().map(|s| json!({"Statement": s})).collect();
    let request: ExecuteTransactionRequest =
        serde_json::from_value(json!({"TransactStatements": statements})).unwrap();
    db.execute_transaction(request)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn batch(db: &Database, statements: &[&str]) -> Result<Vec<Option<(String, String)>>, String> {
    let statements: Vec<Value> = statements.iter().map(|s| json!({"Statement": s})).collect();
    let request: BatchExecuteStatementRequest =
        serde_json::from_value(json!({"Statements": statements})).unwrap();
    db.batch_execute_statement(request)
        .map(|response| {
            response
                .responses
                .into_iter()
                .map(|r| r.error.map(|e| (e.code, e.message)))
                .collect()
        })
        .map_err(|e| e.to_string())
}

fn too_long(value: &str, path: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy constraint: \
         Member must have length less than or equal to 8192"
    )
}

#[test]
fn a_statement_over_8192_code_points_is_refused_as_a_member() {
    let db = setup();
    let long = select_of_length(8193, 'x');
    let fine = select_of_length(100, 'y');
    assert_eq!(single(&db, &long), Err(too_long(&long, "statement")));
    assert_eq!(
        batch(&db, &[&fine, &long]),
        Err(too_long(&long, "statements.2.member.statement"))
    );
    assert_eq!(
        transaction(&db, &[&fine, &long]),
        Err(too_long(&long, "transactStatements.2.member.statement"))
    );
}

#[test]
fn the_limit_counts_code_points_not_bytes() {
    let db = setup();
    // 8,192 code points, but more bytes and more UTF-16 units than that.
    assert_eq!(single(&db, &select_of_length(8192, 'é')), Ok(()));
    assert_eq!(single(&db, &select_of_length(8192, '\u{1F600}')), Ok(()));
}

#[test]
fn an_empty_statement_is_refused_as_a_member() {
    let db = setup();
    assert_eq!(
        single(&db, ""),
        Err(
            "1 validation error detected: Value '' at 'statement' failed to satisfy \
             constraint: Member must have length greater than or equal to 1"
                .to_string()
        )
    );
}

#[test]
fn a_where_clause_past_300_operators_is_refused() {
    let db = setup();
    let statement = format!(
        r#"SELECT * FROM "Limits" WHERE {}pk = 'a'"#,
        "NOT ".repeat(1020)
    );
    let message = "The expression contains too many operators; operator count: 301";
    assert_eq!(single(&db, &statement), Err(message.to_string()));
    assert_eq!(
        transaction(&db, &[&statement]),
        Err(format!(
            "Validation failed in TransactStatements[0]: {message}"
        ))
    );
    assert_eq!(
        batch(&db, &[&statement]),
        Ok(vec![Some((
            "ValidationError".to_string(),
            message.to_string()
        ))])
    );
}

#[test]
fn a_run_of_nots_or_signs_with_no_term_is_an_unexpected_term() {
    let db = setup();
    let unexpected = "Statement wasn't well formed, can't be processed: Unexpected term";
    assert_eq!(single(&db, &"- ".repeat(4096)), Err(unexpected.to_string()));
    assert_eq!(
        single(&db, &"NOT ".repeat(2048)),
        Err(unexpected.to_string())
    );
}
