//! What a PartiQL WHERE clause may hold once it is flattened, and the IN
//! operand limit that bounds it. Unless its test says the case is inferred,
//! every statement here was sent to DynamoDB in eu-west-2 on 2026-10-10: the
//! ones asserted to run, ran there, and the ones asserted refused were refused
//! there in the words asserted.

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
        (format!("a IN [{}]", in_list(50)), 50),
        (format!("a IN [{}]", in_list(100)), 100),
        (format!("a IN [{}]", in_list(101)), 101),
        (format!("a IN [{}]", in_list(1_000)), 1_000),
        (format!("a IN [{}]", in_list(2_000)), 2_000),
        (format!("NOT a IN [{}]", in_list(100)), 100),
        (format!("NOT a IN [{}]", in_list(1_000)), 1_000),
        (format!("NOT a IN [{}]", in_list(2_000)), 2_000),
        (format!("size(s) IN [{}]", in_list(100)), 100),
        (format!("size(s) IN [{}]", in_list(101)), 101),
    ] {
        let outcome = run(&db, &format!("{base}{clause}"), None);
        if operands <= 100 {
            outcome.unwrap_or_else(|e| panic!("{clause:.40}: {e}"));
        } else {
            assert_eq!(outcome, Err(too_many(operands)), "{clause:.40}");
        }
    }
    for count in [100, 101] {
        let placeholders = vec!["?"; count].join(",");
        let parameters = vec![json!({"N": "1"}); count];
        let outcome = run(
            &db,
            &format!("{base}a IN [{placeholders}]"),
            Some(parameters),
        );
        if count == 100 {
            outcome.unwrap_or_else(|e| panic!("100 placeholders: {e}"));
        } else {
            assert_eq!(outcome, Err(too_many(count)), "{count} placeholders");
        }
    }
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
/// operands: 2,000 under a 4,000-character path was refused in exactly these
/// words.
#[test]
fn a_long_negated_in_is_refused_for_its_operands() {
    let db = setup();
    let statement = format!(
        r#"SELECT * FROM "Budget" WHERE pk = 'k' AND NOT "{}" IN [{}]"#,
        "a".repeat(4_000),
        in_list(2_000)
    );
    assert_eq!(run(&db, &statement, None), Err(too_many(2_000)));
}

/// The same refusal arrives before the clause is flattened, whichever side
/// of the alternations it is written on. Inferred from the capture above: the
/// operand limit is a property of the IN, not of what it's ANDed with.
#[test]
fn a_long_negated_in_is_refused_before_it_is_flattened() {
    let db = setup();
    let negated = format!(r#"NOT "{}" IN [{}]"#, "a".repeat(4_000), in_list(261));
    for clause in [
        format!("{negated} AND {}", pairs(8)),
        format!("{} AND {negated}", pairs(8)),
    ] {
        let statement = format!(r#"SELECT * FROM "Budget" WHERE pk = 'k' AND {clause}"#);
        assert!(statement.chars().count() <= 8_192);
        assert_eq!(run(&db, &statement, None), Err(too_many(261)));
    }
}

/// Two wide conjunctions joined by OR run, however many condition slots they
/// flatten to together: each arm here is seven alternations and ten more
/// conditions, 128 groups of 17, so the pair holds 4,352 slots, each arm well
/// within the budget on its own. Neither names the key, so it is a scan, and
/// the overlap rule for key conditions doesn't apply. Inferred: DynamoDB runs
/// far larger expansions (40 alternations, captured eu-west-2, 2026-10-10),
/// so a slot count is no reason to refuse.
#[test]
fn two_wide_conjunctions_joined_by_or_run() {
    let db = setup();
    let arm = |name: char| {
        let conditions = (1..=10)
            .map(|i| format!("{name}{i} = {i}"))
            .collect::<Vec<_>>()
            .join(" AND ");
        format!("({} AND {conditions})", pairs(7))
    };
    let statement = format!(
        r#"SELECT * FROM "Budget" WHERE {} OR {}"#,
        arm('c'),
        arm('d')
    );
    run(&db, &statement, None).unwrap();
}
