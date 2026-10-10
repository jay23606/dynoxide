//! A statement whose terms nest very deeply, within DynamoDB's 8,192-character
//! statement limit, is refused rather than read until the stack runs out,
//! whatever does the nesting, on every PartiQL operation.
//!
//! Brackets, list and bag openers, CASE and path indexes nest the reader and
//! are refused as too deep to parse. For a whole statement of `(` or `[` that
//! is a deliberate difference: DynamoDB's own parser fails there with a 500
//! (captured eu-west-2 and us-east-1, 2026-10-10), which isn't worth copying.
//! A run of NOTs or signs isn't nesting: it ends in "Unexpected term", as on
//! DynamoDB (same capture).

use dynoxide::Database;
use dynoxide::actions::batch_execute_statement::BatchExecuteStatementRequest;
use dynoxide::actions::execute_statement::ExecuteStatementRequest;
use dynoxide::actions::execute_transaction::ExecuteTransactionRequest;
use dynoxide::errors::DynoxideError;
use serde_json::json;

const LIMIT: usize = 8192;

fn statements() -> Vec<(&'static str, String)> {
    vec![
        ("brackets", "(".repeat(LIMIT)),
        ("list openers", "[".repeat(LIMIT)),
        ("bag openers", "<<".repeat(LIMIT / 2)),
        ("path indexes", format!("a{}", "[".repeat(LIMIT - 1))),
        ("NOT", "NOT ".repeat(LIMIT / 4)),
        ("signs", "- ".repeat(LIMIT / 2)),
        ("CASE", "CASE ".repeat(LIMIT / 5)),
        (
            "an INSERT value",
            format!(
                r#"INSERT INTO "Tbl" VALUE {{'pk': 'a', 'v': {}"#,
                "[".repeat(8000)
            ),
        ),
        (
            "an INSERT map",
            format!(
                r#"INSERT INTO "Tbl" VALUE {{'pk': 'a', 'v': {}"#,
                "{'x': ".repeat(1300)
            ),
        ),
        (
            "a WHERE operand",
            format!(
                r#"SELECT * FROM "Tbl" WHERE pk = 'a' AND v = {}"#,
                "<<".repeat(4000)
            ),
        ),
        (
            "a WHERE condition",
            format!(r#"SELECT * FROM "Tbl" WHERE {}pk = 'a'"#, "(".repeat(8000)),
        ),
        (
            "a SET value",
            format!(
                r#"UPDATE "Tbl" SET v = {} WHERE pk = 'a'"#,
                "[".repeat(8000)
            ),
        ),
    ]
}

#[test]
fn a_deeply_nested_statement_is_refused_on_every_partiql_operation() {
    let db = Database::memory().unwrap();
    for (what, statement) in statements() {
        let single: ExecuteStatementRequest =
            serde_json::from_value(json!({"Statement": statement})).unwrap();
        assert!(
            matches!(
                db.execute_statement(single),
                Err(DynoxideError::ValidationException(_))
            ),
            "ExecuteStatement, {what}"
        );

        let transaction: ExecuteTransactionRequest = serde_json::from_value(json!({
            "TransactStatements": [{"Statement": statement}]
        }))
        .unwrap();
        assert!(
            matches!(
                db.execute_transaction(transaction),
                Err(DynoxideError::ValidationException(_))
            ),
            "ExecuteTransaction, {what}"
        );

        let batch: BatchExecuteStatementRequest = serde_json::from_value(json!({
            "Statements": [{"Statement": statement}]
        }))
        .unwrap();
        let answer = db.batch_execute_statement(batch);
        let refused = match &answer {
            Err(DynoxideError::ValidationException(_)) => true,
            Ok(response) => response.responses.iter().all(|r| {
                r.error
                    .as_ref()
                    .is_some_and(|e| e.code == "ValidationError")
            }),
            Err(_) => false,
        };
        assert!(refused, "BatchExecuteStatement, {what}: {answer:?}");
    }
}
