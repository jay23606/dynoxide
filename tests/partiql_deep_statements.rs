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
            "NOT runs between brackets",
            format!(
                r#"SELECT * FROM "Tbl" WHERE {}{}pk = 'a'{}"#,
                format!("{}(", "NOT ".repeat(30)).repeat(64),
                "NOT ".repeat(30),
                ")".repeat(64)
            ),
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

/// `size(` nests through its argument, and a run of `+ 1` after a size is
/// refused term by term: neither may spend the stack. Within the statement
/// limit that is about 1,350 nested calls or 2,000 terms.
#[test]
fn nested_size_calls_and_size_arithmetic_are_refused_on_every_partiql_operation() {
    let db = Database::memory().unwrap();
    let calls = (LIMIT - 64) / 6;
    let nested = |what: &str| {
        format!(
            r#"SELECT * FROM "Tbl" WHERE {what}{}a{} = 1"#,
            "size(".repeat(calls),
            ")".repeat(calls)
        )
    };
    let statements = [
        ("nested size", nested("")),
        (
            "size arithmetic",
            format!(
                r#"SELECT * FROM "Tbl" WHERE size(a){} = 1"#,
                " + 1".repeat((LIMIT - 64) / 4)
            ),
        ),
        (
            "nested size in attribute_type",
            format!(
                r#"SELECT * FROM "Tbl" WHERE attribute_type({}a{}, 'S')"#,
                "size(".repeat(calls - 8),
                ")".repeat(calls - 8)
            ),
        ),
    ];
    for (what, statement) in statements {
        assert!(statement.chars().count() <= LIMIT, "{what} fits the limit");
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
            db.execute_transaction(transaction).is_err(),
            "ExecuteTransaction, {what}"
        );
        let batch: BatchExecuteStatementRequest = serde_json::from_value(json!({
            "Statements": [{"Statement": statement}]
        }))
        .unwrap();
        let answer = db.batch_execute_statement(batch);
        assert!(
            answer.is_err() || answer.unwrap().responses.iter().all(|r| r.error.is_some()),
            "BatchExecuteStatement, {what}"
        );
    }
}

/// The parser itself holds nested `size(` calls to its depth limit, so a
/// caller that skips the request's length limit can't overflow the stack.
#[test]
fn the_parser_refuses_nested_size_calls_without_the_length_limit() {
    let statement = format!(
        r#"SELECT * FROM "Tbl" WHERE {}a{} = 1"#,
        "size(".repeat(10_000),
        ")".repeat(10_000)
    );
    assert!(dynoxide::partiql::parser::parse(&statement).is_err());
}
