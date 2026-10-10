//! What a PartiQL WHERE clause may hold once it is flattened, and the IN
//! operand limit that bounds it. Every statement here was sent to DynamoDB in
//! eu-west-2 on 2026-10-10: the ones asserted to run, ran there, and the ones
//! asserted refused were refused there in the words asserted.

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::ExecuteStatementRequest;
use serde_json::{Value, json};

fn setup() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value::<CreateTableRequest>(json!({
            "TableName": "Budget",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    db
}

fn run(db: &Database, statement: &str, parameters: Option<Vec<Value>>) -> Result<(), String> {
    let mut request = json!({"Statement": statement});
    if let Some(parameters) = parameters {
        request["Parameters"] = Value::Array(parameters);
    }
    let request: ExecuteStatementRequest = serde_json::from_value(request).unwrap();
    db.execute_statement(request)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// `pk = 'k'` and `pairs` alternations of `a`, ANDed: a clause that flattens
/// to 2^pairs groups, each repeating every condition outside the pairs.
fn pairs(pairs: usize) -> String {
    vec!["(a = 1 OR a = 2)"; pairs].join(" AND ")
}

fn in_list(operands: usize) -> String {
    (0..operands)
        .map(|i| (i % 10).to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn too_many(operands: usize) -> String {
    format!("The IN operator is provided with too many operands; number of operands: {operands}")
}

/// A long literal repeated in every group by flattening is held once, not
/// once per group: DynamoDB runs all three of these.
#[test]
fn a_long_literal_repeated_across_groups_is_held_once() {
    let db = setup();
    let base = r#"SELECT * FROM "Budget" WHERE pk = 'k' AND "#;
    for (pairs_written, literal) in [
        (8, "x".repeat(4_070)),
        (7, "\u{4e2d}".repeat(2_750)),
        (8, "x".repeat(1_000)),
    ] {
        let statement = format!("{base}{} AND s = '{literal}'", pairs(pairs_written));
        assert!(statement.chars().count() <= 8_192);
        run(&db, &statement, None).unwrap_or_else(|e| {
            panic!(
                "{pairs_written} pairs and a {}-character literal: {e}",
                literal.chars().count()
            )
        });
    }
}

/// An IN takes at most 100 operands, negated or not, written as literals or
/// as parameters, after a path or a `size()`.
#[test]
fn an_in_takes_at_most_100_operands() {
    let db = setup();
    let base = r#"SELECT * FROM "Budget" WHERE pk = 'k' AND "#;
    for (clause, operands) in [
        (format!("a IN [{}]", in_list(100)), 100),
        (format!("a IN [{}]", in_list(101)), 101),
        (format!("NOT a IN [{}]", in_list(100)), 100),
        (format!("NOT a IN [{}]", in_list(1_000)), 1_000),
        (format!("a IN ({})", in_list(2_000)), 2_000),
        (format!("size(s) IN [{}]", in_list(101)), 101),
    ] {
        let outcome = run(&db, &format!("{base}{clause}"), None);
        if operands <= 100 {
            outcome.unwrap_or_else(|e| panic!("{operands} operands: {e}"));
        } else {
            let err = outcome.expect_err("past 100 operands");
            assert!(
                err.contains(&too_many(operands)),
                "{operands} operands: {err}"
            );
        }
    }
    let placeholders = vec!["?"; 101].join(",");
    let parameters = vec![json!({"N": "1"}); 101];
    let err = run(
        &db,
        &format!("{base}a IN [{placeholders}]"),
        Some(parameters),
    )
    .expect_err("101 placeholders");
    assert!(err.contains(&too_many(101)), "{err}");
}

/// A long path under a negated IN of 100 operands runs; De Morgan copies the
/// path at most 100 times.
#[test]
fn a_long_path_under_a_negated_in_of_100_operands_runs() {
    let db = setup();
    let statement = format!(
        r#"SELECT * FROM "Budget" WHERE pk = 'k' AND NOT "{}" IN [{}]"#,
        "a".repeat(4_000),
        in_list(100)
    );
    run(&db, &statement, None).unwrap();
}

/// A negated IN past 100 operands under a long path is refused for its
/// operands, however the rest of the clause would flatten, and in either
/// order.
#[test]
fn a_long_negated_in_is_refused_for_its_operands_before_it_is_flattened() {
    let db = setup();
    let negated = format!(r#"NOT "{}" IN [{}]"#, "a".repeat(4_000), in_list(261));
    for clause in [
        format!("{negated} AND {}", pairs(8)),
        format!("{} AND {negated}", pairs(8)),
    ] {
        let statement = format!(r#"SELECT * FROM "Budget" WHERE pk = 'k' AND {clause}"#);
        assert!(statement.chars().count() <= 8_192);
        let err = run(&db, &statement, None).expect_err("261 operands");
        assert!(err.contains(&too_many(261)), "{err}");
    }
}
