//! A placeholder with no parameter to fill it never makes a condition true, so
//! a statement short of parameters can't read or delete items by accident.
//! Several conditions are true of an item that lacks the attribute (`<>`, and
//! the negated forms), and they used to be decided on that before the missing
//! parameter was noticed.

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::ExecuteStatementRequest;
use dynoxide::types::AttributeValue;
use serde_json::json;

fn db_with_item() -> Database {
    let db = Database::memory().unwrap();
    let table: CreateTableRequest = serde_json::from_value(json!({
        "TableName": "Tbl",
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        "BillingMode": "PAY_PER_REQUEST"
    }))
    .unwrap();
    db.create_table(table).unwrap();
    run(&db, r#"INSERT INTO "Tbl" VALUE {'pk': 'a'}"#, None).unwrap();
    db
}

fn run(
    db: &Database,
    statement: &str,
    parameters: Option<Vec<AttributeValue>>,
) -> Result<usize, String> {
    db.execute_statement(ExecuteStatementRequest {
        statement: statement.to_string(),
        parameters,
        ..Default::default()
    })
    .map(|r| r.items.map_or(0, |items| items.len()))
    .map_err(|e| e.to_string())
}

fn s(v: &str) -> AttributeValue {
    AttributeValue::S(v.to_string())
}

/// Each condition is true of an item without `flag` once its parameters are
/// supplied (the control), and selects nothing without them.
#[test]
fn a_condition_short_of_its_parameters_selects_nothing() {
    let db = db_with_item();
    let cases: [(&str, Vec<AttributeValue>); 7] = [
        ("flag <> ?", vec![s("x")]),
        ("NOT flag < ?", vec![s("x")]),
        ("NOT begins_with(flag, ?)", vec![s("x")]),
        ("NOT contains(flag, ?)", vec![s("x")]),
        ("NOT flag BETWEEN ? AND ?", vec![s("a"), s("b")]),
        ("NOT (flag = ?)", vec![s("x")]),
        ("size(flag) <> ?", vec![AttributeValue::N("1".to_string())]),
    ];
    for (condition, parameters) in cases {
        let statement = format!(r#"SELECT * FROM "Tbl" WHERE pk = 'a' AND {condition}"#);
        assert_eq!(
            run(&db, &statement, Some(parameters)),
            Ok(1),
            "control: `{condition}` holds for an item without flag"
        );
        let unbound = run(&db, &statement, None);
        assert!(
            matches!(unbound, Err(_) | Ok(0)),
            "`{condition}` with no parameters selected the item: {unbound:?}"
        );
    }
}

#[test]
fn a_delete_short_of_its_parameters_deletes_nothing() {
    let db = db_with_item();
    let _ = run(
        &db,
        r#"DELETE FROM "Tbl" WHERE pk = 'a' AND flag <> ?"#,
        None,
    );
    assert_eq!(
        run(&db, r#"SELECT * FROM "Tbl" WHERE pk = 'a'"#, None),
        Ok(1),
        "the item is still there"
    );
}

const MISMATCH: &str = "Number of parameters in request and statement don't match.";

/// Fewer parameters than placeholders is refused with DynamoDB's message,
/// bare, before the statement runs; more are accepted and the extra ignored
/// (captured eu-west-2 and us-east-1, 2026-10-10).
#[test]
fn execute_statement_refuses_fewer_parameters_than_placeholders() {
    let db = db_with_item();
    let one = r#"SELECT * FROM "Tbl" WHERE pk = ?"#;
    assert_eq!(run(&db, one, None), Err(MISMATCH.to_string()));
    let two = r#"SELECT * FROM "Tbl" WHERE pk = ? AND flag <> ?"#;
    assert_eq!(run(&db, two, Some(vec![s("a")])), Err(MISMATCH.to_string()));
    assert_eq!(run(&db, one, Some(vec![s("a"), s("extra")])), Ok(1));
    assert_eq!(
        run(
            &db,
            r#"DELETE FROM "Tbl" WHERE pk = 'a' AND flag <> ?"#,
            None
        ),
        Err(MISMATCH.to_string())
    );
    assert_eq!(
        run(&db, r#"SELECT * FROM "Tbl" WHERE pk = 'a'"#, None),
        Ok(1)
    );
}

/// An empty `Parameters` list is a request member error.
#[test]
fn execute_statement_refuses_an_empty_parameter_list() {
    let db = db_with_item();
    assert_eq!(
        run(&db, r#"SELECT * FROM "Tbl" WHERE pk = 'a'"#, Some(vec![])),
        Err(
            "1 validation error detected: Value '[]' at 'parameters' failed to satisfy \
             constraint: Member must have length greater than or equal to 1"
                .to_string()
        )
    );
}

/// In a batch only the statement short of parameters fails, and it takes no
/// part in the mixed read and write check, so a SELECT beside a failed DELETE
/// still runs.
#[test]
fn a_batch_fails_only_the_statement_short_of_parameters() {
    use dynoxide::actions::batch_execute_statement::BatchExecuteStatementRequest;
    let db = db_with_item();
    let batch: BatchExecuteStatementRequest = serde_json::from_value(json!({
        "Statements": [
            {"Statement": "DELETE FROM \"Tbl\" WHERE pk = 'a' AND flag <> ?"},
            {"Statement": "SELECT * FROM \"Tbl\" WHERE pk = 'a'"}
        ]
    }))
    .unwrap();
    let responses = db.batch_execute_statement(batch).unwrap().responses;
    let failed = responses[0].error.as_ref().expect("the DELETE fails");
    assert_eq!(failed.code, "ValidationError");
    assert_eq!(failed.message, MISMATCH);
    assert!(responses[1].error.is_none(), "the SELECT runs");
    assert!(
        responses[1].item.is_some(),
        "and finds the item, still there"
    );
}

/// In a transaction it refuses the whole request at the top level, with the
/// statement's index in front.
#[test]
fn a_transaction_refuses_a_statement_short_of_parameters() {
    use dynoxide::actions::execute_transaction::ExecuteTransactionRequest;
    let db = db_with_item();
    let transaction: ExecuteTransactionRequest = serde_json::from_value(json!({
        "TransactStatements": [
            {"Statement": "DELETE FROM \"Tbl\" WHERE pk = 'a' AND flag <> ?"}
        ]
    }))
    .unwrap();
    let err = db.execute_transaction(transaction).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("Validation failed in TransactStatements[0]: {MISMATCH}")
    );
    assert_eq!(
        run(&db, r#"SELECT * FROM "Tbl" WHERE pk = 'a'"#, None),
        Ok(1)
    );
}
