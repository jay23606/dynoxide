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

const HASH_KEY_TOO_LARGE: &str = "One or more parameter values were invalid: \
                                  Size of hashkey has exceeded the maximum size limit of2048 bytes";

#[test]
fn the_limit_counts_code_points_not_bytes() {
    let db = setup();
    // 8,192 code points, but more bytes and more UTF-16 units than that: the
    // length check lets them through, and the key literal they pad is then too
    // large for a hash key, which is DynamoDB's answer to both (captured
    // eu-west-2 and us-east-1, 2026-10-10). An 8,193-code-point statement is
    // refused as a member before the key is looked at.
    assert_eq!(
        single(&db, &select_of_length(8192, 'é')),
        Err(HASH_KEY_TOO_LARGE.to_string())
    );
    assert_eq!(
        single(&db, &select_of_length(8192, '\u{1F600}')),
        Err(HASH_KEY_TOO_LARGE.to_string())
    );
}

#[test]
fn a_key_literal_over_2048_bytes_is_refused() {
    // Captured eu-west-2 and us-east-1, 2026-10-10, with `pk = '<8,160 x>'`:
    // ExecuteStatement refuses it, BatchExecuteStatement fails that statement
    // and echoes its table, and ExecuteTransaction cancels with the
    // ValidationError reason.
    let db = setup();
    let long = select_of_length(8192, 'x');
    assert_eq!(single(&db, &long), Err(HASH_KEY_TOO_LARGE.to_string()));

    let request: BatchExecuteStatementRequest =
        serde_json::from_value(json!({"Statements": [{"Statement": long}]})).unwrap();
    let response = db.batch_execute_statement(request).unwrap();
    let member = &response.responses[0];
    let error = member.error.as_ref().expect("the statement fails");
    assert_eq!(
        (error.code.as_str(), error.message.as_str()),
        ("ValidationError", HASH_KEY_TOO_LARGE)
    );
    assert_eq!(member.table_name.as_deref(), Some("Limits"));

    assert_eq!(
        transaction(&db, &[&long]),
        Err(
            "Transaction cancelled, please refer cancellation reasons for specific reasons \
             [ValidationError]"
                .to_string()
        )
    );

    // The boundary is PutItem's: 2,048 bytes run, 2,049 don't (inferred for
    // PartiQL from the PutItem limit; only the 8,160-byte key was captured).
    let at = format!("SELECT * FROM \"Limits\" WHERE pk = '{}'", "x".repeat(2048));
    let over = format!("SELECT * FROM \"Limits\" WHERE pk = '{}'", "x".repeat(2049));
    assert_eq!(single(&db, &at), Ok(()));
    assert_eq!(single(&db, &over), Err(HASH_KEY_TOO_LARGE.to_string()));
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

/// AND, OR, NOT, comparison operators, calls to WHERE functions, IN, BETWEEN
/// and IS all count toward the 300: `pk = 'a'` is one, and each copy below
/// adds the operators it holds. BETWEEN's own AND doesn't count, an IN list's
/// members don't, and the NOT in `IS NOT` or `NOT IN` counts on its own. Every
/// row is the last count DynamoDB runs and the first it refuses (captured
/// eu-west-2 and us-east-1, 2026-10-10), except the `NOT begins_with` row,
/// which follows from the same counts: its NOT counts as any other does. A bag
/// literal counts nothing: its `<<` and `>>` are brackets, not comparisons, so
/// ` AND s = <<'x'>>` costs two like ` AND x = 1` (captured eu-west-2,
/// 2026-10-10).
#[test]
fn comparisons_and_function_calls_count_toward_the_operator_limit() {
    let db = setup();
    let message = "The expression contains too many operators; operator count: 301";
    for (copy, last_run) in [
        (" AND x = 1", 149),
        (" AND NOT x = 1", 99),
        (" AND begins_with(s, 'a')", 149),
        (" AND size(s) > 0", 99),
        (" AND x < 9", 149),
        (" OR x = 1", 149),
        (" AND NOT begins_with(s, 'a')", 99),
        (" AND s = <<'x'>>", 149),
        (" AND x IN [1, 2]", 149),
        (" AND x BETWEEN 1 AND 2", 149),
        (" AND x IS MISSING", 149),
        (" AND x IS NULL", 149),
        (" AND x IS NOT NULL", 99),
        (" AND x IS NOT MISSING", 99),
        (" AND x NOT IN [1, 2]", 99),
    ] {
        let statement = |copies: usize| {
            format!(
                r#"SELECT * FROM "Limits" WHERE pk = 'a'{}"#,
                copy.repeat(copies)
            )
        };
        assert_eq!(
            single(&db, &statement(last_run)),
            Ok(()),
            "{copy} x {last_run}"
        );
        assert_eq!(
            single(&db, &statement(last_run + 1)),
            Err(message.to_string()),
            "{copy} x {}",
            last_run + 1
        );
    }
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
