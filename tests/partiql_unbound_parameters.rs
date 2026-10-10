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
