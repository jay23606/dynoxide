//! Which second-page requests a PartiQL NextToken resumes. DynamoDB binds the
//! token to the statement exactly as written and to its parameters by value:
//! a page that changes a space, a keyword's case, the projection or a trailing
//! semicolon is refused, as is one whose parameters are different values. A
//! number is the same value only at the same scale, so `0` against `0.0` is
//! refused while `1E2` against `1e+2` resumes, and a set is the same value in
//! any order. A list or map parameter never resumes, even sent identically.
//! Limit may change from page to page. Captured eu-west-2 and us-east-1,
//! 2026-10-10; the regions agree on every row.

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::ExecuteStatementRequest;
use serde_json::{Value, json};

const MISMATCH: &str = "NextToken does not match request";

fn setup() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value::<CreateTableRequest>(json!({
            "TableName": "Tok",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    for pk in ["a", "b", "c", "d"] {
        db.put_item(
            serde_json::from_value(json!({
                "TableName": "Tok",
                "Item": {
                    "pk": {"S": pk},
                    "x": {"N": "0"},
                    "m": {"M": {"a": {"N": "1"}, "b": {"N": "2"}}}
                }
            }))
            .unwrap(),
        )
        .unwrap();
    }
    db
}

fn request(statement: &str, parameters: &Value, limit: u64, token: Option<&str>) -> Value {
    let mut body = json!({"Statement": statement, "Limit": limit});
    if !parameters.is_null() {
        body["Parameters"] = parameters.clone();
    }
    if let Some(token) = token {
        body["NextToken"] = json!(token);
    }
    body
}

/// Page 1 of the first statement, then page 2 as the second request, which
/// may change the statement, the parameters or the limit.
fn second_page(
    first: (&str, Value),
    second: (&str, Value),
    second_limit: u64,
) -> Result<(), String> {
    let db = setup();
    let page = db
        .execute_statement(
            serde_json::from_value::<ExecuteStatementRequest>(request(first.0, &first.1, 1, None))
                .unwrap(),
        )
        .unwrap_or_else(|e| panic!("page 1 of {} was refused: {e}", first.0));
    let token = page.next_token.expect("page 1 stops on its limit");
    db.execute_statement(
        serde_json::from_value::<ExecuteStatementRequest>(request(
            second.0,
            &second.1,
            second_limit,
            Some(&token),
        ))
        .unwrap(),
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// The same statement and parameters on both pages.
fn repeated(statement: &str, parameters: Value) -> Result<(), String> {
    second_page((statement, parameters.clone()), (statement, parameters), 1)
}

const BASE: &str = r#"SELECT pk FROM "Tok" WHERE x = 0"#;

#[test]
fn the_same_statement_resumes_and_may_change_its_limit() {
    assert_eq!(repeated(BASE, Value::Null), Ok(()));
    assert_eq!(
        second_page((BASE, Value::Null), (BASE, Value::Null), 2),
        Ok(())
    );
}

#[test]
fn any_change_to_the_statement_as_written_is_refused() {
    for changed in [
        r#"SELECT pk FROM "Tok"  WHERE x = 0"#,
        r#"select pk from "Tok" where x = 0"#,
        r#"SELECT pk, x FROM "Tok" WHERE x = 0"#,
        r#"SELECT pk FROM "Tok" WHERE x = 0;"#,
    ] {
        assert_eq!(
            second_page((BASE, Value::Null), (changed, Value::Null), 1),
            Err(MISMATCH.to_string()),
            "{changed}"
        );
    }
}

#[test]
fn a_map_literal_resumes_only_as_written() {
    let written = r#"SELECT pk FROM "Tok" WHERE m = {'a': 1, 'b': 2}"#;
    assert_eq!(repeated(written, Value::Null), Ok(()));
    for changed in [
        r#"SELECT pk FROM "Tok" WHERE m = {'b': 2, 'a': 1}"#,
        r#"SELECT pk FROM "Tok" WHERE m = {'a':1,'b':2}"#,
    ] {
        assert_eq!(
            second_page((written, Value::Null), (changed, Value::Null), 1),
            Err(MISMATCH.to_string()),
            "{changed}"
        );
    }
    assert_eq!(
        repeated(r#"SELECT pk FROM "Tok" WHERE m = [1, 2]"#, Value::Null),
        Ok(())
    );
}

#[test]
fn scalar_and_set_parameters_resume_when_sent_again_unchanged() {
    for parameter in [
        json!({"N": "0"}),
        json!({"S": "abc"}),
        json!({"SS": ["p", "q"]}),
        json!({"NS": ["1", "2"]}),
        json!({"BOOL": true}),
        json!({"NULL": true}),
        json!({"B": "YWJj"}),
    ] {
        assert_eq!(
            repeated(r#"SELECT pk FROM "Tok" WHERE s = ?"#, json!([parameter])),
            Ok(()),
            "{parameter}"
        );
    }
}

#[test]
fn a_parameter_that_differs_at_all_is_refused() {
    let statement = r#"SELECT pk FROM "Tok" WHERE s = ?"#;
    for (first, second) in [
        (json!({"N": "0"}), json!({"N": "0.0"})),
        (json!({"S": "p"}), json!({"S": "q"})),
        (json!({"B": "YWJj"}), json!({"B": "eHl6"})),
    ] {
        assert_eq!(
            second_page((statement, json!([first])), (statement, json!([second])), 1),
            Err(MISMATCH.to_string()),
            "{first} then {second}"
        );
    }
}

#[test]
fn a_list_or_map_parameter_never_resumes() {
    let statement = r#"SELECT pk FROM "Tok" WHERE s = ?"#;
    for parameter in [
        json!({"L": [{"N": "1"}, {"N": "2"}]}),
        json!({"M": {"a": {"N": "1"}, "b": {"N": "2"}}}),
        json!({"L": []}),
        json!({"M": {}}),
        json!({"L": [{"SS": ["p"]}]}),
    ] {
        assert_eq!(
            repeated(statement, json!([parameter])),
            Err(MISMATCH.to_string()),
            "{parameter}"
        );
    }
    // Beside a scalar parameter too.
    assert_eq!(
        repeated(
            r#"SELECT pk FROM "Tok" WHERE x = ? AND m = ?"#,
            json!([{"N": "1"}, {"M": {"a": {"N": "1"}, "b": {"N": "2"}}}])
        ),
        Err(MISMATCH.to_string())
    );
}

/// A number parameter matches when it is the same decimal, value and scale
/// both: the exponent's case, a `+` in the exponent or before the number,
/// leading zeros and the sign of zero don't matter, but `1.0` is not `1` and
/// `100` is not `1E2`. Captured eu-west-2 and us-east-1, 2026-10-10.
#[test]
fn a_number_parameter_resumes_as_the_same_decimal() {
    let statement = r#"SELECT pk FROM "Tok" WHERE z = ?"#;
    let page = |first: &str, second: &str| {
        second_page(
            (statement, json!([{"N": first}])),
            (statement, json!([{"N": second}])),
            1,
        )
    };
    for (first, second) in [
        ("1e2", "1E2"),
        ("1E2", "1e2"),
        ("1E+2", "1E2"),
        ("1E2", "1E+2"),
        ("1e-2", "1E-2"),
        ("-0", "0"),
        ("01", "1"),
        ("+1", "1"),
        ("1.5E1", "15"),
        ("1", "1"),
        ("1E2", "1E2"),
    ] {
        assert_eq!(page(first, second), Ok(()), "{first} then {second}");
    }
    for (first, second) in [
        ("1.0", "1"),
        ("0.0", "0"),
        ("0", "0.0"),
        ("1.50", "1.5"),
        ("100", "1E2"),
        ("1E2", "100"),
        ("100", "100.0"),
        ("1E2", "1.0E2"),
        ("0E2", "0"),
    ] {
        assert_eq!(
            page(first, second),
            Err(MISMATCH.to_string()),
            "{first} then {second}"
        );
    }
}

/// A set parameter matches as a set: its members in any order, a number set's
/// members by value whatever their scale, a string or binary set's exactly.
/// Captured eu-west-2 and us-east-1, 2026-10-10.
#[test]
fn a_set_parameter_resumes_as_the_same_set() {
    let statement = r#"SELECT pk FROM "Tok" WHERE z = ?"#;
    let page = |first: Value, second: Value| {
        second_page((statement, json!([first])), (statement, json!([second])), 1)
    };
    let numbers = [
        ("1e2", "1E2"),
        ("1E+2", "1E2"),
        ("1e-2", "1E-2"),
        ("1.0", "1"),
        ("0.0", "0"),
        ("-0", "0"),
        ("01", "1"),
        ("+1", "1"),
        ("1.50", "1.5"),
        ("100", "1E2"),
        ("100", "100.0"),
        ("1.5E1", "15"),
        ("1E2", "1.0E2"),
        ("0E2", "0"),
        ("1", "1"),
    ];
    for (first, second) in numbers {
        assert_eq!(
            page(json!({"NS": [first]}), json!({"NS": [second]})),
            Ok(()),
            "[{first}] then [{second}]"
        );
        assert_eq!(
            page(json!({"NS": [first, "7"]}), json!({"NS": [second, "7"]})),
            Ok(()),
            "[{first}, 7] then [{second}, 7]"
        );
    }
    for (first, second) in [
        (json!({"NS": ["1", "2"]}), json!({"NS": ["2", "1"]})),
        (json!({"NS": ["1", "2"]}), json!({"NS": ["1.0", "2"]})),
        (json!({"NS": ["1", "2"]}), json!({"NS": ["2", "1.0"]})),
        (json!({"SS": ["a", "b"]}), json!({"SS": ["b", "a"]})),
        (
            json!({"BS": ["AQ==", "Ag=="]}),
            json!({"BS": ["Ag==", "AQ=="]}),
        ),
        (json!({"S": "abc"}), json!({"S": "abc"})),
    ] {
        assert_eq!(
            page(first.clone(), second.clone()),
            Ok(()),
            "{first} then {second}"
        );
    }
    for (first, second) in [
        (json!({"SS": ["a", "b", "c"]}), json!({"SS": ["a", "b"]})),
        (json!({"SS": ["a", "b"]}), json!({"SS": ["a", "b", "c"]})),
        (json!({"SS": ["a", "b"]}), json!({"SS": ["A", "b"]})),
        (
            json!({"BS": ["AQ==", "Ag==", "Aw=="]}),
            json!({"BS": ["AQ==", "Ag=="]}),
        ),
        (json!({"S": "abc"}), json!({"S": "ABC"})),
    ] {
        assert_eq!(
            page(first.clone(), second.clone()),
            Err(MISMATCH.to_string()),
            "{first} then {second}"
        );
    }
}
