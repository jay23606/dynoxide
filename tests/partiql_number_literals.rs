//! Number literals written with an exponent, and tokens left over after a
//! statement ends.
//!
//! The tokeniser used to stop a number at the `e`, so `1e5` became the number
//! `1` followed by an identifier `e5`, and the statement parsers never checked
//! that they had read every token. Together that ran `WHERE pk = 1e5` as
//! `WHERE pk = 1` and returned the wrong item. DynamoDB reads `1e5` as one
//! number and refuses a token after the WHERE expression (captured eu-west-2,
//! 2026-10-08).
//!
//! Refusing leftover tokens meets two things DynamoDB reads differently:
//! comments, which it reads as whitespace, and a table alias, which it refuses
//! with its own message (captured eu-west-2, 2026-10-09).

use dynoxide::Database;
use dynoxide::actions::batch_execute_statement::{
    BatchExecuteStatementRequest, BatchStatementRequest,
};
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::{ExecuteStatementRequest, ExecuteStatementResponse};
use dynoxide::actions::execute_transaction::{ExecuteTransactionRequest, ParameterizedStatement};
use dynoxide::errors::DynoxideError;
use dynoxide::types::{
    AttributeDefinition, AttributeValue, KeySchemaElement, KeyType, ScalarAttributeType,
};

const NOT_WELL_FORMED: &str = "Statement wasn't well formed, can't be processed: ";

fn numeric_table(db: &Database) {
    db.create_table(CreateTableRequest {
        table_name: "Nums".to_string(),
        key_schema: vec![KeySchemaElement {
            attribute_name: "pk".to_string(),
            key_type: KeyType::HASH,
        }],
        attribute_definitions: vec![AttributeDefinition {
            attribute_name: "pk".to_string(),
            attribute_type: ScalarAttributeType::N,
        }],
        ..Default::default()
    })
    .unwrap();
}

fn run(db: &Database, statement: &str) -> Result<ExecuteStatementResponse, DynoxideError> {
    db.execute_statement(ExecuteStatementRequest {
        statement: statement.to_string(),
        parameters: None,
        ..Default::default()
    })
}

fn seeded() -> Database {
    let db = Database::memory().unwrap();
    numeric_table(&db);
    for pk in ["1", "15", "100000", "1.5"] {
        run(&db, &format!("INSERT INTO \"Nums\" VALUE {{'pk': {pk}}}")).unwrap();
    }
    db
}

fn pks(resp: ExecuteStatementResponse) -> Vec<String> {
    let mut pks: Vec<String> = resp
        .items
        .unwrap_or_default()
        .into_iter()
        .map(|item| match item.get("pk") {
            Some(AttributeValue::N(n)) => n.clone(),
            other => panic!("expected a numeric pk, got {other:?}"),
        })
        .collect();
    pks.sort();
    pks
}

fn message(err: DynoxideError) -> String {
    match err {
        DynoxideError::ValidationException(m) => m,
        other => panic!("expected a ValidationException, got {other:?}"),
    }
}

#[test]
fn an_exponent_literal_selects_the_number_it_spells() {
    let db = seeded();
    for (literal, expected) in [
        ("1e5", "100000"),
        ("1E5", "100000"),
        ("1e+5", "100000"),
        ("1.5e1", "15"),
        ("15e-1", "1.5"),
    ] {
        let resp = run(&db, &format!("SELECT * FROM \"Nums\" WHERE pk = {literal}"))
            .unwrap_or_else(|e| panic!("{literal}: {e:?}"));
        assert_eq!(pks(resp), vec![expected.to_string()], "{literal}");
    }
}

#[test]
fn an_exponent_literal_is_accepted_in_an_insert_and_an_update() {
    let db = Database::memory().unwrap();
    numeric_table(&db);
    run(&db, "INSERT INTO \"Nums\" VALUE {'pk': 2e3, 'n': 3E2}").unwrap();
    run(&db, "UPDATE \"Nums\" SET m = 4e1 WHERE pk = 2000").unwrap();

    let resp = run(&db, "SELECT * FROM \"Nums\" WHERE pk = 2000").unwrap();
    let items = resp.items.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].get("n"),
        Some(&AttributeValue::N("300".to_string()))
    );
    assert_eq!(
        items[0].get("m"),
        Some(&AttributeValue::N("40".to_string()))
    );
}

#[test]
fn a_token_after_the_where_expression_is_refused() {
    let db = seeded();
    for statement in [
        "SELECT * FROM \"Nums\" WHERE pk = 1 foo",
        "SELECT * FROM \"Nums\" WHERE pk = 1 e5",
        "DELETE FROM \"Nums\" WHERE pk = 1 foo",
        "UPDATE \"Nums\" SET a = 'x' WHERE pk = 1 foo",
    ] {
        let err = run(&db, statement).expect_err(statement);
        assert_eq!(
            message(err),
            format!("{NOT_WELL_FORMED}Unexpected token after expression"),
            "{statement}"
        );
    }
    // Nothing was deleted or updated by the refused statements.
    let resp = run(&db, "SELECT * FROM \"Nums\" WHERE pk = 1").unwrap();
    let items = resp.items.unwrap();
    assert_eq!(items.len(), 1);
    assert!(!items[0].contains_key("a"));
}

#[test]
fn a_token_after_an_insert_is_refused() {
    let db = Database::memory().unwrap();
    numeric_table(&db);
    for statement in [
        "INSERT INTO \"Nums\" VALUE {'pk': 7} junk",
        "INSERT INTO \"Nums\" VALUE {'pk': 7} IF NOT EXISTS junk",
    ] {
        let err = run(&db, statement).expect_err(statement);
        assert_eq!(
            message(err),
            format!("{NOT_WELL_FORMED}Unexpected token after expression"),
            "{statement}"
        );
    }
    let resp = run(&db, "SELECT * FROM \"Nums\" WHERE pk = 7").unwrap();
    assert!(resp.items.unwrap().is_empty(), "the refused INSERT wrote");
}

#[test]
fn an_exponent_too_large_to_read_is_refused_as_not_well_formed() {
    let db = seeded();
    for statement in [
        "SELECT * FROM \"Nums\" WHERE pk = 1e99999999999",
        "SELECT * FROM \"Nums\" WHERE pk = 1e-99999999999",
        "DELETE FROM \"Nums\" WHERE pk = 1e99999999999",
    ] {
        let err = run(&db, statement).expect_err(statement);
        assert_eq!(message(err), NOT_WELL_FORMED, "{statement}");
    }
    assert_eq!(
        pks(run(&db, "SELECT * FROM \"Nums\"").unwrap()),
        vec!["1", "1.5", "100000", "15"],
        "a refused statement changed nothing"
    );
}

#[test]
fn a_comment_reads_as_whitespace() {
    // A trailing `--` used to pass because the tokens after the WHERE
    // expression were never read; refusing leftover tokens must not turn it
    // into an error. DynamoDB accepts both comment forms anywhere whitespace
    // can go (captured eu-west-2, 2026-10-09).
    let db = seeded();
    for statement in [
        "SELECT * FROM \"Nums\" WHERE pk = 1 -- note",
        "SELECT * FROM \"Nums\" WHERE pk = 1 --",
        "SELECT * FROM \"Nums\" -- c\nWHERE pk = 1",
        "-- lead\nSELECT * FROM \"Nums\" WHERE pk = 1",
        "SELECT * FROM \"Nums\" WHERE pk = 1 /* c */",
        "SELECT * FROM \"Nums\" /* c */ WHERE pk = 1",
    ] {
        let resp = run(&db, statement).unwrap_or_else(|e| panic!("{statement:?}: {e:?}"));
        assert_eq!(pks(resp), vec!["1".to_string()], "{statement:?}");
    }
    for statement in [
        "SELECT * FROM \"Nums\" WHERE pk = 1 # c",
        "SELECT * FROM \"Nums\" WHERE pk = 1 /* unterminated",
    ] {
        let err = run(&db, statement).expect_err(statement);
        assert_eq!(message(err), NOT_WELL_FORMED, "{statement:?}");
    }
}

#[test]
fn an_exponent_whose_scale_does_not_fit_is_refused_as_not_well_formed() {
    let db = seeded();
    for literal in [
        "1e-2147483648",
        "1.5e-2147483647",
        "0.1e-2147483647",
        "1.50e-2147483646",
        "0e-2147483648",
    ] {
        for statement in [
            format!("SELECT * FROM \"Nums\" WHERE pk = {literal}"),
            format!("INSERT INTO \"Nums\" VALUE {{'pk': {literal}}}"),
        ] {
            let err = run(&db, &statement).expect_err(&statement);
            assert_eq!(message(err), NOT_WELL_FORMED, "{statement}");
        }
    }
    assert_eq!(
        pks(run(&db, "SELECT * FROM \"Nums\"").unwrap()),
        vec!["1", "1.5", "100000", "15"],
        "a refused statement changed nothing"
    );
}

#[test]
fn an_alias_is_refused_with_dynamodbs_message() {
    let db = seeded();
    for statement in [
        "SELECT * FROM \"Nums\" t WHERE pk = 1",
        "SELECT * FROM \"Nums\" AS t WHERE pk = 1",
        "SELECT * FROM \"Nums\" t",
        "SELECT t.pk FROM \"Nums\" t WHERE t.pk = 1",
        "DELETE FROM \"Nums\" t WHERE pk = 15",
        "UPDATE \"Nums\" t SET a = 3 WHERE pk = 1",
    ] {
        let err = run(&db, statement).expect_err(statement);
        assert_eq!(message(err), "Aliasing is not supported", "{statement}");
    }
    let err = run(&db, "INSERT INTO \"Nums\" t VALUE {'pk': 8}").expect_err("insert alias");
    assert_eq!(
        message(err),
        format!("{NOT_WELL_FORMED}Unexpected token after expression")
    );
    // Nothing was written, deleted or updated.
    assert_eq!(
        pks(run(&db, "SELECT * FROM \"Nums\"").unwrap()),
        vec!["1", "1.5", "100000", "15"]
    );
    let resp = run(&db, "SELECT * FROM \"Nums\" WHERE pk = 1").unwrap();
    assert!(!resp.items.unwrap()[0].contains_key("a"));
}

#[test]
fn an_order_by_after_a_select_is_still_accepted() {
    // DynamoDB accepts ORDER BY on a SELECT, so the check for leftover tokens
    // leaves it alone rather than turning a query that returns rows into an
    // error. Ordering by the partition key of a keyed read changes nothing.
    let db = seeded();
    let resp = run(&db, "SELECT * FROM \"Nums\" WHERE pk = 15 ORDER BY pk").unwrap();
    assert_eq!(pks(resp), vec!["15".to_string()]);
}

#[test]
fn an_order_by_cannot_carry_a_where_clause_after_it() {
    // The ORDER BY exemption used to accept anything after it, so a WHERE
    // written after the ORDER BY was dropped and the read ran unfiltered.
    let db = seeded();
    let err = run(&db, "SELECT * FROM \"Nums\" ORDER BY pk WHERE pk = 15")
        .expect_err("a WHERE after ORDER BY must not run unfiltered");
    assert_eq!(
        message(err),
        format!("{NOT_WELL_FORMED}Unexpected token after expression")
    );
    let resp = run(&db, "SELECT * FROM \"Nums\" WHERE pk = 15 ORDER BY pk DESC").unwrap();
    assert_eq!(pks(resp), vec!["15".to_string()]);
}

/// A table keyed on a string partition key and a numeric sort key, so a
/// number literal reaches the sort-key encoding.
fn sorted_table(db: &Database) {
    db.create_table(CreateTableRequest {
        table_name: "Sorted".to_string(),
        key_schema: vec![
            KeySchemaElement {
                attribute_name: "pk".to_string(),
                key_type: KeyType::HASH,
            },
            KeySchemaElement {
                attribute_name: "sk".to_string(),
                key_type: KeyType::RANGE,
            },
        ],
        attribute_definitions: vec![
            AttributeDefinition {
                attribute_name: "pk".to_string(),
                attribute_type: ScalarAttributeType::S,
            },
            AttributeDefinition {
                attribute_name: "sk".to_string(),
                attribute_type: ScalarAttributeType::N,
            },
        ],
        ..Default::default()
    })
    .unwrap();
    for (sk, n) in [("5", "-100000"), ("10", "10"), ("150", "2"), ("2000", "7")] {
        run(
            db,
            &format!("INSERT INTO \"Sorted\" VALUE {{'pk': 'a', 'sk': {sk}, 'n': {n}}}"),
        )
        .unwrap();
    }
}

fn sks(resp: ExecuteStatementResponse) -> Vec<String> {
    let mut sks: Vec<String> = resp
        .items
        .unwrap_or_default()
        .into_iter()
        .map(|item| match item.get("sk") {
            Some(AttributeValue::N(n)) => n.clone(),
            other => panic!("expected a numeric sk, got {other:?}"),
        })
        .collect();
    sks.sort_by(|a, b| {
        a.parse::<f64>()
            .unwrap()
            .total_cmp(&b.parse::<f64>().unwrap())
    });
    sks
}

#[test]
fn an_exponent_literal_works_on_a_sort_key_and_in_a_filter() {
    let db = Database::memory().unwrap();
    sorted_table(&db);
    let cases = [
        ("pk = 'a' AND sk BETWEEN 1e1 AND 1.5e2", vec!["10", "150"]),
        ("pk = 'a' AND sk IN [1e1, 2E3]", vec!["10", "2000"]),
        ("pk = 'a' AND sk > 1.5e2", vec!["2000"]),
        ("pk = 'a' AND n = 1e1", vec!["10"]),
        ("pk = 'a' AND n = -1e5", vec!["5"]),
        ("n = 7e0", vec!["2000"]),
    ];
    for (clause, expected) in cases {
        let resp = run(&db, &format!("SELECT * FROM \"Sorted\" WHERE {clause}"))
            .unwrap_or_else(|e| panic!("{clause}: {e:?}"));
        assert_eq!(sks(resp), expected, "{clause}");
    }
}

#[test]
fn an_exponent_literal_works_in_set_arithmetic() {
    let db = Database::memory().unwrap();
    sorted_table(&db);
    run(
        &db,
        "UPDATE \"Sorted\" SET n = n + 1e1 WHERE pk = 'a' AND sk = 10",
    )
    .unwrap();
    run(
        &db,
        "UPDATE \"Sorted\" SET n = n - 2.5e-1 WHERE pk = 'a' AND sk = 150",
    )
    .unwrap();
    let resp = run(
        &db,
        "SELECT * FROM \"Sorted\" WHERE pk = 'a' AND sk IN [10, 150]",
    )
    .unwrap();
    let mut by_sk: Vec<(String, AttributeValue)> = resp
        .items
        .unwrap()
        .into_iter()
        .map(|item| {
            let sk = match item.get("sk") {
                Some(AttributeValue::N(n)) => n.clone(),
                other => panic!("{other:?}"),
            };
            (sk, item.get("n").cloned().unwrap())
        })
        .collect();
    by_sk.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        by_sk,
        vec![
            ("10".to_string(), AttributeValue::N("20".to_string())),
            ("150".to_string(), AttributeValue::N("1.75".to_string())),
        ]
    );
}

#[test]
fn a_leftover_token_is_refused_in_a_batch_and_a_transaction() {
    let db = seeded();

    let mut bad = BatchStatementRequest::default();
    bad.statement = "SELECT * FROM \"Nums\" WHERE pk = 1 foo".to_string();
    let mut good = BatchStatementRequest::default();
    good.statement = "SELECT * FROM \"Nums\" WHERE pk = 1e5".to_string();
    let resp = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            return_consumed_capacity: None,
            statements: vec![bad, good],
        })
        .unwrap();
    let err = resp.responses[0]
        .error
        .as_ref()
        .expect("the statement with a leftover token errors");
    assert_eq!(err.code, "ValidationError");
    assert_eq!(
        err.message,
        format!("{NOT_WELL_FORMED}Unexpected token after expression")
    );
    assert!(resp.responses[1].error.is_none(), "the valid sibling runs");
    assert_eq!(
        resp.responses[1].item.as_ref().and_then(|i| i.get("pk")),
        Some(&AttributeValue::N("100000".to_string())),
        "1e5 reads 100000 inside a batch too"
    );

    let err = db
        .execute_transaction(ExecuteTransactionRequest {
            transact_statements: vec![
                ParameterizedStatement {
                    statement: "UPDATE \"Nums\" SET a = 'x' WHERE pk = 15".to_string(),
                    parameters: None,
                },
                ParameterizedStatement {
                    statement: "UPDATE \"Nums\" SET a = 'y' WHERE pk = 1 foo".to_string(),
                    parameters: None,
                },
            ],
            ..Default::default()
        })
        .expect_err("a transaction with a leftover token is refused");
    let msg = message(err);
    assert!(
        msg.contains("Unexpected token after expression"),
        "unexpected message: {msg}"
    );
    let resp = run(&db, "SELECT * FROM \"Nums\" WHERE pk = 15").unwrap();
    assert!(
        !resp.items.unwrap()[0].contains_key("a"),
        "the refused transaction wrote its first statement"
    );
}

#[test]
fn a_number_with_no_leading_digit_is_read_as_a_number() {
    // DynamoDB reads `.5e1` as 5 and `.25e1` as 2.5, with a sign in front or
    // without (captured eu-west-2, 2026-10-09). The tokeniser only started a
    // number on a digit, so the `.` came through on its own and the statement
    // was refused.
    let db = Database::memory().unwrap();
    numeric_table(&db);
    run(&db, "INSERT INTO \"Nums\" VALUE {'pk': 5}").unwrap();

    let resp = run(&db, "SELECT * FROM \"Nums\" WHERE pk = .5e1").unwrap();
    assert_eq!(pks(resp), vec!["5".to_string()]);
    for literal in [".5", ".5e2", "-.5e1"] {
        let resp = run(&db, &format!("SELECT * FROM \"Nums\" WHERE pk = {literal}"))
            .unwrap_or_else(|e| panic!("{literal}: {e:?}"));
        assert!(pks(resp).is_empty(), "{literal}");
    }

    run(&db, "INSERT INTO \"Nums\" VALUE {'pk': .25e1}").unwrap();
    let resp = run(&db, "SELECT * FROM \"Nums\" WHERE pk = 2.5").unwrap();
    assert_eq!(pks(resp), vec!["2.5".to_string()]);
}

const OVERFLOW: &str =
    "Number overflow. Attempting to store a number with magnitude larger than supported range";
const UNDERFLOW: &str =
    "Number underflow. Attempting to store a number with magnitude smaller than supported range";
const TOO_PRECISE: &str = "Attempting to store more than 38 significant digits in a Number";
const DIGITS_39: &str = "123456789012345678901234567890123456789";
const DIGITS_38: &str = "12345678901234567890123456789012345678";

#[test]
fn a_where_literal_dynamodb_cannot_store_is_refused_under_root() {
    // DynamoDB checks a number written in a WHERE clause as it checks one being
    // stored, and says where the problem sits, `root` being the value it was
    // written as part of. Every comparison form gets the check, and an IN list
    // or BETWEEN bound is a value of its own (captured eu-west-2 and us-east-1,
    // 2026-10-09). These used to run as comparisons and return nothing.
    let db = seeded();
    for (clause, expected) in [
        ("pk = 1e126".to_string(), format!("{OVERFLOW} under root")),
        ("pk = 1e-131".to_string(), format!("{UNDERFLOW} under root")),
        (
            format!("pk = {DIGITS_39}"),
            format!("{TOO_PRECISE} under root"),
        ),
        ("a = 1e126".to_string(), format!("{OVERFLOW} under root")),
        (
            "pk IN [1, 1e126]".to_string(),
            format!("{OVERFLOW} under root"),
        ),
        (
            format!("pk IN [1, {DIGITS_39}]"),
            format!("{TOO_PRECISE} under root"),
        ),
        (
            "pk BETWEEN 1e-131 AND 1".to_string(),
            format!("{UNDERFLOW} under root"),
        ),
        (
            "pk BETWEEN 1 AND 1e126".to_string(),
            format!("{OVERFLOW} under root"),
        ),
        ("pk > 1e126".to_string(), format!("{OVERFLOW} under root")),
        ("pk <> 1e126".to_string(), format!("{OVERFLOW} under root")),
        (
            "NOT pk = 1e126".to_string(),
            format!("{OVERFLOW} under root"),
        ),
        (
            "contains(a, 1e126)".to_string(),
            format!("{OVERFLOW} under root"),
        ),
        ("a = -1e126".to_string(), format!("{OVERFLOW} under root")),
        (
            "a = [1, 1e126]".to_string(),
            format!("{OVERFLOW} under root[1]"),
        ),
        (
            "a = {'x': 1e126}".to_string(),
            format!("{OVERFLOW} under root.x"),
        ),
        (
            "a = <<1, 1e126>>".to_string(),
            format!("{OVERFLOW} under root"),
        ),
        (
            "a = [{'y': [1, 1e-131]}]".to_string(),
            format!("{UNDERFLOW} under root[0].y[1]"),
        ),
    ] {
        let statement = format!("SELECT * FROM \"Nums\" WHERE {clause}");
        let err = run(&db, &statement).expect_err(&statement);
        assert_eq!(message(err), expected, "{statement}");
    }

    // The same check on UPDATE and DELETE, and before the table is looked up.
    for statement in [
        "UPDATE \"Nums\" SET a = 1 WHERE pk = 1e126",
        "DELETE FROM \"Nums\" WHERE pk = 1e126",
        "SELECT * FROM \"Missing\" WHERE pk = 1e126",
    ] {
        let err = run(&db, statement).expect_err(statement);
        assert_eq!(
            message(err),
            format!("{OVERFLOW} under root"),
            "{statement}"
        );
    }

    // The edges of the range still run.
    for literal in ["1e125", "1e-130", DIGITS_38] {
        let resp = run(&db, &format!("SELECT * FROM \"Nums\" WHERE pk = {literal}"))
            .unwrap_or_else(|e| panic!("{literal}: {e:?}"));
        assert!(pks(resp).is_empty(), "{literal}");
    }
}

#[test]
fn a_written_number_dynamodb_cannot_store_names_its_path() {
    // The path starts at the attribute's value, or at the value a SET assigns,
    // so the attribute's own name and the SET's target path are left out:
    // `root.x`, `root[1]`. A set member is named by its set (captured
    // eu-west-2 and us-east-1, 2026-10-09). The messages used to end at the
    // range.
    let db = seeded();
    for (statement, expected) in [
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 1e126}".to_string(),
            format!("{OVERFLOW} under root"),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 1e-131}".to_string(),
            format!("{UNDERFLOW} under root"),
        ),
        (
            format!("INSERT INTO \"Nums\" VALUE {{'pk': {DIGITS_39}}}"),
            format!("{TOO_PRECISE} under root"),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, 'm': {'x': 1e126}}".to_string(),
            format!("{OVERFLOW} under root.x"),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 10, 'l': [1, 1e126]}".to_string(),
            format!("{OVERFLOW} under root[1]"),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, 'm': {'x': {'y': [1, {'z': 1e126}]}}}"
                .to_string(),
            format!("{OVERFLOW} under root.x.y[1].z"),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, 'l': [[1, [2, 1e126]]]}".to_string(),
            format!("{OVERFLOW} under root[0][1][1]"),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, 's': <<1, 1e126>>}".to_string(),
            format!("{OVERFLOW} under root"),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, 'm': {'a b': 1e126}}".to_string(),
            format!("{OVERFLOW} under root.a b"),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, 'm': {'a.b': 1e126}}".to_string(),
            format!("{OVERFLOW} under root.a.b"),
        ),
        (
            "UPDATE \"Nums\" SET a = 1e126 WHERE pk = 1".to_string(),
            format!("{OVERFLOW} under root"),
        ),
        (
            "UPDATE \"Nums\" SET a = 1e-131 WHERE pk = 1".to_string(),
            format!("{UNDERFLOW} under root"),
        ),
        (
            format!("UPDATE \"Nums\" SET a = {DIGITS_39} WHERE pk = 1"),
            format!("{TOO_PRECISE} under root"),
        ),
        (
            "UPDATE \"Nums\" SET a = {'x': 1e126} WHERE pk = 1".to_string(),
            format!("{OVERFLOW} under root.x"),
        ),
        (
            "UPDATE \"Nums\" SET a = [1e126] WHERE pk = 1".to_string(),
            format!("{OVERFLOW} under root[0]"),
        ),
        (
            "UPDATE \"Nums\" SET m.x = 1e126 WHERE pk = 1".to_string(),
            format!("{OVERFLOW} under root"),
        ),
        (
            "UPDATE \"Nums\" SET a = a + 1e126 WHERE pk = 1".to_string(),
            format!("{OVERFLOW} under root"),
        ),
        (
            "UPDATE \"Nums\" SET a = list_append(a, [1e126]) WHERE pk = 1".to_string(),
            format!("{OVERFLOW} under root[0]"),
        ),
        (
            "UPDATE \"Nums\" SET a = 1, b = {'y': [1e126]} WHERE pk = 1".to_string(),
            format!("{OVERFLOW} under root.y[0]"),
        ),
        // Refused before the item or the table is looked up.
        (
            "UPDATE \"Nums\" SET a = 1e126 WHERE pk = 424242".to_string(),
            format!("{OVERFLOW} under root"),
        ),
        (
            "UPDATE \"Missing\" SET a = 1e126 WHERE pk = 1".to_string(),
            format!("{OVERFLOW} under root"),
        ),
        (
            "INSERT INTO \"Missing\" VALUE {'pk': 1e126}".to_string(),
            format!("{OVERFLOW} under root"),
        ),
    ] {
        let err = run(&db, &statement).expect_err(&statement);
        assert_eq!(message(err), expected, "{statement}");
    }

    // Nothing was written.
    for pk in ["9", "10"] {
        let resp = run(&db, &format!("SELECT * FROM \"Nums\" WHERE pk = {pk}")).unwrap();
        assert!(pks(resp).is_empty(), "pk {pk}");
    }
    let resp = run(&db, "SELECT * FROM \"Nums\" WHERE pk = 1").unwrap();
    let item = &resp.items.unwrap()[0];
    assert!(!item.contains_key("a") && !item.contains_key("m"));
}

#[test]
fn the_first_number_written_is_the_one_reported() {
    // With two numbers DynamoDB can't store, the one written first is
    // reported, inside a map literal as much as across attributes, and an
    // UPDATE's WHERE clause is checked before its SET clause. The number check
    // also comes before the check on ordering a type that has no ordering
    // (captured eu-west-2 and us-east-1, 2026-10-09).
    let db = seeded();
    for (statement, expected) in [
        (
            "INSERT INTO \"Nums\" VALUE {'b': 1e-131, 'pk': 9, 'a': 1e126}",
            format!("{UNDERFLOW} under root"),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 1e126, 'a': 1e-131}",
            format!("{OVERFLOW} under root"),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, 'm': {'y': 1e-131, 'x': 1e126}}",
            format!("{UNDERFLOW} under root.y"),
        ),
        (
            "SELECT * FROM \"Nums\" WHERE a = {'y': 1e-131, 'x': 1e126}",
            format!("{UNDERFLOW} under root.y"),
        ),
        (
            "UPDATE \"Nums\" SET a = 1e126 WHERE pk = 1e-131",
            format!("{UNDERFLOW} under root"),
        ),
        (
            "UPDATE \"Nums\" SET a = 1e126, b = 1e-131 WHERE pk = 1",
            format!("{OVERFLOW} under root"),
        ),
        (
            "UPDATE \"Nums\" SET b = 1e-131, a = 1e126 WHERE pk = 1",
            format!("{UNDERFLOW} under root"),
        ),
        (
            "SELECT * FROM \"Nums\" WHERE pk = 1 AND a = 1e126 AND b = 1e-131",
            format!("{OVERFLOW} under root"),
        ),
        (
            "SELECT * FROM \"Nums\" WHERE b = 1e-131 OR a = 1e126",
            format!("{UNDERFLOW} under root"),
        ),
        (
            "SELECT * FROM \"Nums\" WHERE a > true AND pk = 1e126",
            format!("{OVERFLOW} under root"),
        ),
    ] {
        let err = run(&db, statement).expect_err(statement);
        assert_eq!(message(err), expected, "{statement}");
    }
}

#[test]
fn a_parameter_dynamodb_cannot_store_is_refused_without_a_path() {
    // A parameter is checked before the statement is read, used or not, with
    // the message any attribute value gets and nothing after it (captured
    // eu-west-2 and us-east-1, 2026-10-09). A bad parameter used to reach the
    // read and match nothing.
    let db = seeded();
    for statement in [
        "SELECT * FROM \"Nums\" WHERE pk = ?",
        "SELECT * FROM \"Nums\" WHERE pk = 1",
        "SELECT * FROM \"Missing\" WHERE pk = ?",
        "INSERT INTO \"Nums\" VALUE {'pk': ?}",
        "UPDATE \"Nums\" SET a = ? WHERE pk = 1",
    ] {
        let err = db
            .execute_statement(ExecuteStatementRequest {
                statement: statement.to_string(),
                parameters: Some(vec![AttributeValue::N("1e126".to_string())]),
                ..Default::default()
            })
            .expect_err(statement);
        assert_eq!(message(err), OVERFLOW, "{statement}");
    }
}

#[test]
fn partiql_reads_the_top_of_the_exponent_range_as_underflow() {
    // At the very top of the exponent range DynamoDB's PartiQL reports
    // underflow, not overflow: once a literal's precision less its scale
    // reaches 2,147,483,647. PutItem says overflow for the same number, so
    // this is PartiQL's own reading (captured eu-west-2, 2026-10-09:
    // `1e2147483645` is an overflow and `1e2147483646` an underflow).
    let db = seeded();
    for statement in [
        "SELECT * FROM \"Nums\" WHERE pk = 1e2147483646",
        "SELECT * FROM \"Nums\" WHERE pk = 100e2147483646",
        "SELECT * FROM \"Nums\" WHERE pk = 1.5e2147483647",
        "INSERT INTO \"Nums\" VALUE {'pk': 1e2147483647}",
        "UPDATE \"Nums\" SET a = 1e2147483647 WHERE pk = 1",
        "DELETE FROM \"Nums\" WHERE pk = 1e2147483647",
    ] {
        let err = run(&db, statement).expect_err(statement);
        assert_eq!(
            message(err),
            format!("{UNDERFLOW} under root"),
            "{statement}"
        );
    }
}

/// The sort keys in the order the read returned them.
fn sks_as_returned(resp: ExecuteStatementResponse) -> Vec<String> {
    resp.items
        .unwrap_or_default()
        .into_iter()
        .map(|item| match item.get("sk") {
            Some(AttributeValue::N(n)) => n.clone(),
            other => panic!("expected a numeric sk, got {other:?}"),
        })
        .collect()
}

#[test]
fn order_by_desc_on_the_sort_key_returns_the_rows_in_reverse() {
    // Captured eu-west-2, 2026-10-09: with sort keys 1, 2 and 3 under one
    // partition key, `ORDER BY sk DESC` returns 3, 2, 1 and `ORDER BY sk ASC`
    // returns 1, 2, 3. ORDER BY used to be read and dropped, so DESC came back
    // ascending.
    let db = Database::memory().unwrap();
    sorted_table(&db);
    let ascending = ["5", "10", "150", "2000"].map(String::from).to_vec();
    let descending = ["2000", "150", "10", "5"].map(String::from).to_vec();

    for (statement, expected) in [
        (
            "SELECT * FROM \"Sorted\" WHERE pk = 'a' ORDER BY sk DESC",
            &descending,
        ),
        (
            "SELECT * FROM \"Sorted\" WHERE pk = 'a' order by sk desc",
            &descending,
        ),
        (
            "SELECT * FROM \"Sorted\" WHERE pk = 'a' ORDER BY \"sk\" DESC",
            &descending,
        ),
        (
            "SELECT * FROM \"Sorted\" WHERE pk = 'a' ORDER BY sk ASC",
            &ascending,
        ),
        (
            "SELECT * FROM \"Sorted\" WHERE pk = 'a' ORDER BY sk",
            &ascending,
        ),
        ("SELECT * FROM \"Sorted\" WHERE pk = 'a'", &ascending),
    ] {
        let resp = run(&db, statement).unwrap_or_else(|e| panic!("{statement}: {e:?}"));
        assert_eq!(&sks_as_returned(resp), expected, "{statement}");
    }
}

#[test]
fn a_descending_read_pages_in_descending_order() {
    // A continuation picks up where the previous page stopped, in the same
    // direction, so the pages together hold every row once, in order.
    let db = Database::memory().unwrap();
    sorted_table(&db);
    let mut seen = Vec::new();
    let mut next_token = None;
    for _ in 0..10 {
        let resp = db
            .execute_statement(ExecuteStatementRequest {
                statement: "SELECT * FROM \"Sorted\" WHERE pk = 'a' ORDER BY sk DESC".to_string(),
                limit: Some(2),
                next_token: next_token.take(),
                ..Default::default()
            })
            .unwrap();
        next_token = resp.next_token.clone();
        seen.extend(sks_as_returned(resp));
        if next_token.is_none() {
            break;
        }
    }
    assert_eq!(seen, ["2000", "150", "10", "5"].map(String::from).to_vec());
}

#[test]
fn a_continuation_cannot_change_the_direction_of_the_read() {
    // A token carries the read it was minted for. Replaying an ascending
    // read's token on a descending one would resume from the wrong end.
    let db = Database::memory().unwrap();
    sorted_table(&db);
    let first = db
        .execute_statement(ExecuteStatementRequest {
            statement: "SELECT * FROM \"Sorted\" WHERE pk = 'a' ORDER BY sk ASC".to_string(),
            limit: Some(2),
            ..Default::default()
        })
        .unwrap();
    let token = first
        .next_token
        .expect("a page of two from four rows has a token");
    let err = db
        .execute_statement(ExecuteStatementRequest {
            statement: "SELECT * FROM \"Sorted\" WHERE pk = 'a' ORDER BY sk DESC".to_string(),
            limit: Some(2),
            next_token: Some(token),
            ..Default::default()
        })
        .expect_err("a token minted for an ascending read must not resume a descending one");
    assert_eq!(message(err), "NextToken does not match request");
}

/// The ORDER BY capture's table: a string partition key, a numeric sort key,
/// a GSI on (`gpk`, `gsk`) and an LSI on `lsk`, both projecting everything.
fn ordered_table() -> Database {
    let db = Database::memory().unwrap();
    let req = serde_json::json!({
        "TableName": "Ordered",
        "KeySchema": [
            {"AttributeName": "pk", "KeyType": "HASH"},
            {"AttributeName": "sk", "KeyType": "RANGE"}
        ],
        "AttributeDefinitions": [
            {"AttributeName": "pk", "AttributeType": "S"},
            {"AttributeName": "sk", "AttributeType": "N"},
            {"AttributeName": "gpk", "AttributeType": "S"},
            {"AttributeName": "gsk", "AttributeType": "N"},
            {"AttributeName": "lsk", "AttributeType": "N"}
        ],
        "GlobalSecondaryIndexes": [{
            "IndexName": "gsi",
            "KeySchema": [
                {"AttributeName": "gpk", "KeyType": "HASH"},
                {"AttributeName": "gsk", "KeyType": "RANGE"}
            ],
            "Projection": {"ProjectionType": "ALL"}
        }],
        "LocalSecondaryIndexes": [{
            "IndexName": "lsi",
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "lsk", "KeyType": "RANGE"}
            ],
            "Projection": {"ProjectionType": "ALL"}
        }]
    });
    db.create_table(serde_json::from_value(req).unwrap())
        .unwrap();
    for (pk, sk, a, gsk, lsk) in [
        ("a", "1", "30", "3", "2"),
        ("a", "2", "10", "1", "3"),
        ("a", "3", "20", "2", "1"),
        ("b", "1", "5", "4", "1"),
    ] {
        run(
            &db,
            &format!(
                "INSERT INTO \"Ordered\" VALUE {{'pk': '{pk}', 'sk': {sk}, 'a': {a}, \
                 'gpk': 'g', 'gsk': {gsk}, 'lsk': {lsk}, 'm': {{'x': {a}}}}}"
            ),
        )
        .unwrap();
    }
    run(
        &db,
        "INSERT INTO \"Ordered\" VALUE {'pk': 'b', 'sk': 2, 'a': 6, 'lsk': 2}",
    )
    .unwrap();
    db
}

/// Each row as `pk` and `sk`, in the order the read returned them.
fn keys_as_returned(resp: ExecuteStatementResponse) -> Vec<String> {
    resp.items
        .unwrap_or_default()
        .into_iter()
        .map(|item| {
            let pk = match item.get("pk") {
                Some(AttributeValue::S(s)) => s.clone(),
                other => panic!("expected a string pk, got {other:?}"),
            };
            let sk = match item.get("sk") {
                Some(AttributeValue::N(n)) => n.clone(),
                other => panic!("expected a numeric sk, got {other:?}"),
            };
            format!("{pk}{sk}")
        })
        .collect()
}

#[test]
fn order_by_reads_each_partition_in_the_order_dynamodb_does() {
    // Captured eu-west-2 and us-east-1, 2026-10-09, on this table. The sort
    // key term sets the direction. A partition key term alone reads the
    // partition descending, whichever way it is written. An index read orders
    // by the index's own key.
    let db = ordered_table();
    for (clause, expected) in [
        ("WHERE pk = 'a' ORDER BY sk DESC", "a3 a2 a1"),
        ("WHERE pk = 'a' ORDER BY sk ASC", "a1 a2 a3"),
        ("WHERE pk = 'a' ORDER BY sk", "a1 a2 a3"),
        ("WHERE pk = 'a' ORDER BY \"sk\" DESC", "a3 a2 a1"),
        ("WHERE pk = 'a' order by sk desc", "a3 a2 a1"),
        ("WHERE pk = 'a' ORDER BY pk", "a3 a2 a1"),
        ("WHERE pk = 'a' ORDER BY pk ASC", "a3 a2 a1"),
        ("WHERE pk = 'a' ORDER BY pk DESC", "a3 a2 a1"),
        ("WHERE pk = 'a' ORDER BY pk, sk", "a1 a2 a3"),
        ("WHERE pk = 'a' ORDER BY pk DESC, sk ASC", "a1 a2 a3"),
        ("WHERE pk = 'a' ORDER BY pk ASC, sk DESC", "a3 a2 a1"),
        ("WHERE pk = 'a' ORDER BY sk ASC, pk DESC", "a1 a2 a3"),
        ("WHERE pk = 'a' ORDER BY sk DESC, sk ASC", "a3 a2 a1"),
        ("WHERE pk = 'a' AND sk > 1 ORDER BY sk DESC", "a3 a2"),
        (
            "WHERE pk = 'a' AND sk BETWEEN 1 AND 2 ORDER BY sk DESC",
            "a2 a1",
        ),
        ("WHERE pk = 'a' AND a > 0 ORDER BY sk DESC", "a3 a2 a1"),
        ("WHERE pk IN ['a'] ORDER BY sk DESC", "a3 a2 a1"),
        ("WHERE pk = 'zz' ORDER BY sk DESC", ""),
        // More than one partition: in partition-key order, each partition by
        // the sort key term, or descending without one.
        ("WHERE pk IN ['a', 'b'] ORDER BY pk", "a3 a2 a1 b2 b1"),
        ("WHERE pk IN ['b', 'a'] ORDER BY pk DESC", "b2 b1 a3 a2 a1"),
        (
            "WHERE pk IN ['a', 'b'] ORDER BY pk, sk DESC",
            "a3 a2 a1 b2 b1",
        ),
        (
            "WHERE pk IN ['a', 'b'] ORDER BY pk DESC, sk DESC",
            "b2 b1 a3 a2 a1",
        ),
        ("WHERE pk IN ['a', 'b'] ORDER BY sk, pk", "a1 a2 a3 b1 b2"),
        ("WHERE pk = 'a' OR pk = 'b' ORDER BY pk", "a3 a2 a1 b2 b1"),
    ] {
        let statement = format!("SELECT * FROM \"Ordered\" {clause}");
        let resp = run(&db, &statement).unwrap_or_else(|e| panic!("{statement}: {e:?}"));
        assert_eq!(keys_as_returned(resp).join(" "), expected, "{statement}");
    }

    // An index orders by its own key: the GSI by `gsk`, the LSI by `lsk`.
    for (statement, expected) in [
        (
            "SELECT * FROM \"Ordered\".\"gsi\" WHERE gpk = 'g' ORDER BY gsk DESC",
            "b1 a1 a3 a2",
        ),
        (
            "SELECT * FROM \"Ordered\".\"gsi\" WHERE gpk = 'g' ORDER BY gpk",
            "b1 a1 a3 a2",
        ),
        (
            "SELECT * FROM \"Ordered\".\"gsi\" WHERE gpk = 'g' ORDER BY gpk, gsk",
            "a2 a3 a1 b1",
        ),
        (
            "SELECT * FROM \"Ordered\".\"lsi\" WHERE pk = 'a' ORDER BY lsk DESC",
            "a2 a1 a3",
        ),
        (
            "SELECT * FROM \"Ordered\".\"lsi\" WHERE pk = 'a' ORDER BY lsk",
            "a3 a1 a2",
        ),
        (
            "SELECT * FROM \"Ordered\".\"lsi\" WHERE pk = 'a' ORDER BY pk",
            "a2 a1 a3",
        ),
    ] {
        let resp = run(&db, statement).unwrap_or_else(|e| panic!("{statement}: {e:?}"));
        assert_eq!(keys_as_returned(resp).join(" "), expected, "{statement}");
    }
}

#[test]
fn an_ordered_read_across_partitions_pages_straight_through() {
    // Captured eu-west-2 and us-east-1, 2026-10-09: with Limit 2, the pages
    // were [b2, b1], [a3, a2], [a1].
    let db = ordered_table();
    let mut pages = Vec::new();
    let mut next_token = None;
    for _ in 0..10 {
        let resp = db
            .execute_statement(ExecuteStatementRequest {
                statement: "SELECT * FROM \"Ordered\" WHERE pk IN ['a', 'b'] \
                            ORDER BY pk DESC, sk DESC"
                    .to_string(),
                limit: Some(2),
                next_token: next_token.take(),
                ..Default::default()
            })
            .unwrap();
        next_token = resp.next_token.clone();
        pages.push(keys_as_returned(resp).join(" "));
        if next_token.is_none() {
            break;
        }
    }
    assert_eq!(pages, ["b2 b1", "a3 a2", "a1"]);
}

#[test]
fn order_by_is_refused_where_dynamodb_refuses_it() {
    // Each message and the order they come in, captured eu-west-2 and
    // us-east-1, 2026-10-09: the WHERE clause, then each term in turn (a
    // plain name, then a key), then the partition key pinned in every branch,
    // then the partition key among the terms when the clause names more than
    // one partition. A missing table or index is reported before any of it,
    // and a number DynamoDB can't store before that.
    const NO_WHERE: &str = "Must have WHERE clause in the statement when using ORDER BY clause.";
    const NOT_PINNED: &str = "Must have at least one non-optional hash key condition in \
                              WHERE clause when using ORDER BY clause.";
    const NEEDS_PK: &str = "Must have hash key in ORDER BY clause when more than one hash \
                            key condition specified in WHERE clause.";
    let not_key =
        |attr: &str| format!("Attribute {attr} in ORDER BY clause must be part of the primary key");
    let not_a_name = |found: &str, at: &str| {
        format!("Argument to ORDER BY must be of type VariableReference. Type: {found} at {at}")
    };
    let db = ordered_table();
    // `SELECT * FROM "Ordered" WHERE pk = 'a' ORDER BY ` is 48 characters, so
    // a term written straight after it starts at column 49. (The capture's
    // longer table name put the same term at column 65.)
    for (statement, expected) in [
        (
            "SELECT * FROM \"Ordered\" ORDER BY sk DESC",
            NO_WHERE.to_string(),
        ),
        (
            "SELECT * FROM \"Ordered\" ORDER BY m.x",
            NO_WHERE.to_string(),
        ),
        (
            "SELECT * FROM \"Ordered\".\"gsi\" ORDER BY gsk",
            NO_WHERE.to_string(),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' ORDER BY a DESC",
            not_key("a"),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' ORDER BY sk DESC, a",
            not_key("a"),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' ORDER BY gsk DESC",
            not_key("gsk"),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' ORDER BY SK",
            not_key("SK"),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' ORDER BY \"SK\"",
            not_key("SK"),
        ),
        (
            "SELECT * FROM \"Ordered\".\"gsi\" WHERE gpk = 'g' ORDER BY sk DESC",
            not_key("sk"),
        ),
        (
            "SELECT * FROM \"Ordered\".\"lsi\" WHERE pk = 'a' ORDER BY sk DESC",
            not_key("sk"),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE a = 1 ORDER BY a",
            not_key("a"),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk IN ['a', 'b'] ORDER BY a",
            not_key("a"),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' ORDER BY a, m.x",
            not_key("a"),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' ORDER BY m.x DESC",
            not_a_name("PATH", "1:49:1"),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' ORDER BY m.x, a",
            not_a_name("PATH", "1:49:1"),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' ORDER BY sk[0] DESC",
            not_a_name("PATH", "1:49:2"),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' ORDER BY   \"m\".x DESC",
            not_a_name("PATH", "1:51:3"),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' ORDER BY abc.def[1]",
            not_a_name("PATH", "1:49:3"),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a'\nORDER BY m.x",
            not_a_name("PATH", "2:10:1"),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' ORDER BY 'sk'",
            not_a_name("sk", "1:49:4"),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE sk = 1 ORDER BY sk DESC",
            NOT_PINNED.to_string(),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk <> 'a' ORDER BY sk",
            NOT_PINNED.to_string(),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk > 'a' ORDER BY sk",
            NOT_PINNED.to_string(),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE NOT pk = 'a' ORDER BY sk",
            NOT_PINNED.to_string(),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE begins_with(pk, 'a') ORDER BY sk",
            NOT_PINNED.to_string(),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' OR sk = 1 ORDER BY sk",
            NOT_PINNED.to_string(),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND pk = 'b' ORDER BY sk",
            NOT_PINNED.to_string(),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE a = 30 ORDER BY sk",
            NOT_PINNED.to_string(),
        ),
        (
            "SELECT * FROM \"Ordered\".\"gsi\" WHERE pk = 'a' ORDER BY gsk",
            NOT_PINNED.to_string(),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk IN ['a', 'b'] ORDER BY sk DESC",
            NEEDS_PK.to_string(),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' OR pk = 'b' ORDER BY sk DESC",
            NEEDS_PK.to_string(),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND sk > 1 OR pk = 'b' ORDER BY sk",
            NEEDS_PK.to_string(),
        ),
        (
            "SELECT * FROM \"Ordered\".\"nope\" WHERE pk = 'a' ORDER BY sk",
            "The table does not have the specified index".to_string(),
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 1e126 ORDER BY m.x",
            format!("{OVERFLOW} under root"),
        ),
    ] {
        let err = run(&db, statement).expect_err(statement);
        assert_eq!(message(err), expected, "{statement}");
    }

    for statement in [
        "SELECT * FROM \"Missing\" ORDER BY sk",
        "SELECT * FROM \"Missing\" WHERE pk = 'a' ORDER BY m.x",
    ] {
        let err = run(&db, statement).expect_err(statement);
        assert!(
            matches!(err, DynoxideError::ResourceNotFoundException(_)),
            "{statement}: {err:?}"
        );
    }
}

#[test]
fn malformed_numbers_are_refused_as_dynamodb_refuses_them() {
    // Captured eu-west-2 and us-east-1, 2026-10-09. A number takes one point,
    // so a second one is left over; an exponent marker needs digits; and a
    // point or sign with nothing after it can't start a term. A trailing point
    // and a sign written apart from its number are both fine.
    let db = seeded();
    for (literal, expected) in [
        (
            "1.2.3",
            format!("{NOT_WELL_FORMED}Unexpected token after expression"),
        ),
        (
            ".5.5",
            format!("{NOT_WELL_FORMED}Unexpected token after expression"),
        ),
        ("5e", NOT_WELL_FORMED.to_string()),
        (".5e", NOT_WELL_FORMED.to_string()),
        (".", format!("{NOT_WELL_FORMED}Unexpected term")),
        (".e5", format!("{NOT_WELL_FORMED}Unexpected term")),
        ("-.", format!("{NOT_WELL_FORMED}Unexpected term")),
        ("..5", format!("{NOT_WELL_FORMED}Unexpected term")),
    ] {
        let statement = format!("SELECT * FROM \"Nums\" WHERE pk = {literal}");
        let err = run(&db, &statement).expect_err(&statement);
        assert_eq!(message(err), expected, "{statement}");
    }
    for (literal, expected) in [
        ("15.", vec!["15"]),
        ("- .15e2", vec![]),
        ("+.15e2", vec!["15"]),
    ] {
        let statement = format!("SELECT * FROM \"Nums\" WHERE pk = {literal}");
        let resp = run(&db, &statement).unwrap_or_else(|e| panic!("{statement}: {e:?}"));
        assert_eq!(pks(resp), expected, "{statement}");
    }
}

/// A literal map nested `levels` deep: `{'n': {'n': ... 'leaf'}}`.
fn nested_map_literal(levels: usize) -> String {
    (0..levels).fold("'leaf'".to_string(), |inner, _| format!("{{'n': {inner}}}"))
}

/// The same depth as an attribute value, for a parameter.
fn nested_map_value(levels: usize) -> AttributeValue {
    (0..levels).fold(AttributeValue::S("leaf".to_string()), |inner, _| {
        AttributeValue::M([("n".to_string(), inner)].into_iter().collect())
    })
}

const NESTED_TOO_DEEP: &str = "Nesting Levels have exceeded supported limits";
const NESTED_TOO_DEEP_LONG: &str = "Nesting Levels have exceeded supported limits: Attributes \
                                    in the item have nested levels beyond supported limit";

#[test]
fn a_literal_nested_32_deep_is_refused_where_it_reaches_the_limit() {
    // DynamoDB reads a literal 31 levels deep, and refuses one at the 32nd
    // level as it reads it, naming the path to that level however much deeper
    // the literal goes, on INSERT, UPDATE and in a WHERE clause alike, in the
    // order written against a bad number (captured eu-west-2 and us-east-1,
    // 2026-10-09).
    let db = seeded();
    let at_32_in_a_map = format!("{NESTED_TOO_DEEP} under root{}", ".n".repeat(32));
    let at_32_in_a_list = format!("{NESTED_TOO_DEEP} under root{}", "[0]".repeat(32));
    let list_32 = (0..32).fold("'leaf'".to_string(), |inner, _| format!("[{inner}]"));
    for (statement, expected) in [
        (
            format!(
                "INSERT INTO \"Nums\" VALUE {{'pk': 9, 'd': {}}}",
                nested_map_literal(32)
            ),
            at_32_in_a_map.clone(),
        ),
        (
            format!(
                "INSERT INTO \"Nums\" VALUE {{'pk': 9, 'd': {}}}",
                nested_map_literal(40)
            ),
            at_32_in_a_map.clone(),
        ),
        (
            format!("INSERT INTO \"Nums\" VALUE {{'pk': 9, 'd': {list_32}}}"),
            at_32_in_a_list,
        ),
        (
            format!(
                "UPDATE \"Nums\" SET d = {} WHERE pk = 1",
                nested_map_literal(32)
            ),
            at_32_in_a_map.clone(),
        ),
        (
            format!(
                "SELECT * FROM \"Nums\" WHERE d = {}",
                nested_map_literal(32)
            ),
            at_32_in_a_map.clone(),
        ),
        (
            format!(
                "INSERT INTO \"Nums\" VALUE {{'pk': 9, 'd': {}, 'x': 1e126}}",
                nested_map_literal(32)
            ),
            at_32_in_a_map.clone(),
        ),
        (
            format!(
                "INSERT INTO \"Nums\" VALUE {{'pk': 9, 'x': 1e126, 'd': {}}}",
                nested_map_literal(32)
            ),
            format!("{OVERFLOW} under root"),
        ),
    ] {
        let err = run(&db, &statement).expect_err(&statement);
        assert_eq!(message(err), expected, "{statement}");
    }

    run(
        &db,
        &format!(
            "INSERT INTO \"Nums\" VALUE {{'pk': 31, 'd': {}}}",
            nested_map_literal(31)
        ),
    )
    .expect("31 levels are accepted");
}

#[test]
fn a_parameter_nested_32_deep_gets_the_short_message() {
    // A parameter is checked on the item it ends up in, after the item is
    // found: exactly 32 levels gets the short message and 33 the long one,
    // and an UPDATE of an item that isn't there fails its condition first
    // (captured eu-west-2 and us-east-1, 2026-10-09).
    let db = seeded();
    let with_parameter = |statement: &str, levels: usize| {
        db.execute_statement(ExecuteStatementRequest {
            statement: statement.to_string(),
            parameters: Some(vec![nested_map_value(levels)]),
            ..Default::default()
        })
    };
    for (statement, levels, expected) in [
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, 'd': ?}",
            32,
            NESTED_TOO_DEEP,
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, 'd': ?}",
            33,
            NESTED_TOO_DEEP_LONG,
        ),
        (
            "UPDATE \"Nums\" SET d = ? WHERE pk = 1",
            32,
            NESTED_TOO_DEEP,
        ),
        (
            "UPDATE \"Nums\" SET d = ? WHERE pk = 1",
            33,
            NESTED_TOO_DEEP_LONG,
        ),
    ] {
        let err = with_parameter(statement, levels).expect_err(statement);
        assert_eq!(message(err), expected, "{statement} at {levels} levels");
    }
    let err = with_parameter("UPDATE \"Nums\" SET d = ? WHERE pk = 424242", 32)
        .expect_err("a missing item fails its condition");
    assert!(
        matches!(err, DynoxideError::ConditionalCheckFailedException(..)),
        "{err:?}"
    );
    with_parameter("INSERT INTO \"Nums\" VALUE {'pk': 31, 'd': ?}", 31)
        .expect("31 levels are accepted");
}

#[test]
fn order_by_accepts_nulls_first_and_last() {
    // Captured eu-west-2, 2026-10-09: accepted, and a key is never null, so
    // the order is the sort key term's.
    let db = ordered_table();
    for (clause, expected) in [
        ("WHERE pk = 'a' ORDER BY sk NULLS FIRST", "a1 a2 a3"),
        ("WHERE pk = 'a' ORDER BY sk DESC NULLS LAST", "a3 a2 a1"),
    ] {
        let statement = format!("SELECT * FROM \"Ordered\" {clause}");
        let resp = run(&db, &statement).unwrap_or_else(|e| panic!("{statement}: {e:?}"));
        assert_eq!(keys_as_returned(resp).join(" "), expected, "{statement}");
    }
}

fn batch_statement(
    statement: &str,
    parameters: Option<Vec<AttributeValue>>,
) -> BatchStatementRequest {
    let mut request = BatchStatementRequest::default();
    request.statement = statement.to_string();
    request.parameters = parameters;
    request
}

fn transaction(
    db: &Database,
    statements: &[(&str, Option<Vec<AttributeValue>>)],
) -> Result<(), DynoxideError> {
    db.execute_transaction(ExecuteTransactionRequest {
        transact_statements: statements
            .iter()
            .map(|(statement, parameters)| ParameterizedStatement {
                statement: statement.to_string(),
                parameters: parameters.clone(),
            })
            .collect(),
        ..Default::default()
    })
    .map(|_| ())
}

#[test]
fn a_missing_table_gets_dynamodbs_short_message_on_every_partiql_surface() {
    // Captured eu-west-2 and us-east-1, 2026-10-09: `Requested resource not
    // found` with nothing after it, on every statement kind and on an
    // index-qualified read; against the statement in a batch, code
    // `ResourceNotFound`; and as the cancellation reason in a transaction.
    // PartiQL used to add `: Table: <name> not found`.
    const NOT_FOUND: &str = "Requested resource not found";
    let db = seeded();
    for statement in [
        "SELECT * FROM \"Missing\" WHERE pk = 1",
        "SELECT * FROM \"Missing\"",
        "INSERT INTO \"Missing\" VALUE {'pk': 1}",
        "UPDATE \"Missing\" SET a = 1 WHERE pk = 1",
        "DELETE FROM \"Missing\" WHERE pk = 1",
        "SELECT * FROM \"Missing\".\"idx\" WHERE pk = 1",
    ] {
        match run(&db, statement) {
            Err(DynoxideError::ResourceNotFoundException(m)) => {
                assert_eq!(m, NOT_FOUND, "{statement}")
            }
            other => panic!("{statement}: expected ResourceNotFoundException, got {other:?}"),
        }
    }

    let resp = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            return_consumed_capacity: None,
            statements: vec![
                batch_statement("SELECT * FROM \"Missing\" WHERE pk = 1", None),
                batch_statement("SELECT * FROM \"Missing\" WHERE pk = 2", None),
            ],
        })
        .unwrap();
    for response in &resp.responses {
        let err = response.error.as_ref().expect("a missing table errors");
        assert_eq!(
            (err.code.as_str(), err.message.as_str()),
            ("ResourceNotFound", NOT_FOUND)
        );
    }

    for statement in [
        "INSERT INTO \"Missing\" VALUE {'pk': 1}",
        "UPDATE \"Missing\" SET a = 1 WHERE pk = 1",
    ] {
        match transaction(&db, &[(statement, None)]) {
            Err(DynoxideError::TransactionCanceledException(m, reasons)) => {
                assert_eq!(
                    m,
                    "Transaction cancelled, please refer cancellation reasons for specific \
                     reasons [ResourceNotFound]",
                    "{statement}"
                );
                assert_eq!(reasons.len(), 1, "{statement}");
                assert_eq!(reasons[0].code, "ResourceNotFound", "{statement}");
                assert_eq!(
                    reasons[0].message.as_deref(),
                    Some(NOT_FOUND),
                    "{statement}"
                );
            }
            other => panic!("{statement}: expected a cancellation, got {other:?}"),
        }
    }
}

const EMPTY_KEY: &str = "Path component cannot be an empty string";

#[test]
fn an_empty_map_key_in_a_literal_is_refused() {
    // Captured eu-west-2 and us-east-1, 2026-10-09: an empty key anywhere in a
    // literal, an INSERT's own attribute names included, is refused with
    // `Path component cannot be an empty string`, as the statement is read:
    // before a missing table, in the order written against a number or a
    // depth DynamoDB can't take, and an UPDATE's WHERE literal first. A key
    // that is a space, and an empty string in a set, are fine.
    let db = seeded();
    let at_32 = format!("{NESTED_TOO_DEEP} under root{}", ".n".repeat(32));
    for (statement, expected) in [
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, 'm': {'': 1}}".to_string(),
            EMPTY_KEY.to_string(),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, '': 1}".to_string(),
            EMPTY_KEY.to_string(),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, 'm': {'x': {'': 1}}}".to_string(),
            EMPTY_KEY.to_string(),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, 'l': [{'': 1}]}".to_string(),
            EMPTY_KEY.to_string(),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, 'x': 1e126, 'm': {'': 1}}".to_string(),
            format!("{OVERFLOW} under root"),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, 'm': {'': 1}, 'x': 1e126}".to_string(),
            EMPTY_KEY.to_string(),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, 'm': {'': 1e126}}".to_string(),
            EMPTY_KEY.to_string(),
        ),
        (
            "INSERT INTO \"Nums\" VALUE {'pk': 9, 'm': {'a': 1e126, '': 1}}".to_string(),
            format!("{OVERFLOW} under root.a"),
        ),
        (
            format!(
                "INSERT INTO \"Nums\" VALUE {{'pk': 9, 'm': {{'': 1}}, 'd': {}}}",
                nested_map_literal(32)
            ),
            EMPTY_KEY.to_string(),
        ),
        (
            format!(
                "INSERT INTO \"Nums\" VALUE {{'pk': 9, 'd': {}, 'm': {{'': 1}}}}",
                nested_map_literal(32)
            ),
            at_32,
        ),
        (
            "UPDATE \"Nums\" SET a = {'': 1} WHERE pk = 1".to_string(),
            EMPTY_KEY.to_string(),
        ),
        (
            "UPDATE \"Nums\" SET a = {'': 1} WHERE pk = 1e126".to_string(),
            format!("{OVERFLOW} under root"),
        ),
        (
            "SELECT * FROM \"Nums\" WHERE a = {'': 1}".to_string(),
            EMPTY_KEY.to_string(),
        ),
        (
            "SELECT * FROM \"Nums\" WHERE a = [{'': 1}]".to_string(),
            EMPTY_KEY.to_string(),
        ),
        (
            "SELECT * FROM \"Nums\" WHERE a IN [{'': 1}]".to_string(),
            EMPTY_KEY.to_string(),
        ),
        (
            "INSERT INTO \"Missing\" VALUE {'pk': 9, 'm': {'': 1}}".to_string(),
            EMPTY_KEY.to_string(),
        ),
    ] {
        let err = run(&db, &statement).expect_err(&statement);
        assert_eq!(message(err), expected, "{statement}");
    }

    run(&db, "INSERT INTO \"Nums\" VALUE {'pk': 16, 'm': {' ': 1}}").expect("a space is a key");
    run(
        &db,
        "INSERT INTO \"Nums\" VALUE {'pk': 17, 's': <<'', 'a'>>}",
    )
    .expect("an empty string in a set is fine");

    // Against the statement in a batch, and with the statement's index in a
    // transaction, which writes nothing.
    let resp = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            return_consumed_capacity: None,
            statements: vec![batch_statement(
                "INSERT INTO \"Nums\" VALUE {'pk': 9, 'm': {'': 1}}",
                None,
            )],
        })
        .unwrap();
    let err = resp.responses[0]
        .error
        .as_ref()
        .expect("an empty key errors");
    assert_eq!(
        (err.code.as_str(), err.message.as_str()),
        ("ValidationError", EMPTY_KEY)
    );
    let err = transaction(
        &db,
        &[
            ("INSERT INTO \"Nums\" VALUE {'pk': 78}", None),
            ("INSERT INTO \"Nums\" VALUE {'pk': 9, 'm': {'': 1}}", None),
        ],
    )
    .expect_err("an empty key refuses the transaction");
    assert_eq!(
        message(err),
        format!("Validation failed in TransactStatements[1]: {EMPTY_KEY}")
    );
    let resp = run(&db, "SELECT * FROM \"Nums\" WHERE pk = 78").unwrap();
    assert!(
        pks(resp).is_empty(),
        "the refused transaction wrote nothing"
    );
}

#[test]
fn overlapping_key_conditions_are_refused() {
    // Captured eu-west-2 and us-east-1, 2026-10-09. When every OR branch pins
    // the partition key, DynamoDB reads the clause as a set of key ranges and
    // refuses one where two ranges share a key: the same partition twice with
    // no sort key condition, sort key ranges that meet (closed ends touching
    // count), a key repeated in an IN list, or `1` and `1.0`. A branch that
    // doesn't pin the partition key turns the read into a scan, and nothing is
    // checked. A missing table is reported first.
    const OVERLAP: &str =
        "Overlapping conditions with range keys are not supported in where clause";
    let db = ordered_table();
    for clause in [
        "pk = 'a' OR pk = 'a'",
        "pk IN ['a', 'a']",
        "pk IN ['a', 'b'] OR pk = 'a'",
        "pk = 'a' OR pk = 'b' OR pk = 'a'",
        "pk IN ['a'] OR pk = 'a'",
        "pk = 'a' AND sk > 1 OR pk = 'a' AND sk < 3",
        "pk = 'a' AND sk > 1 OR pk = 'a' AND sk > 2",
        "pk = 'a' AND sk >= 1 OR pk = 'a' AND sk <= 1",
        "pk = 'a' OR pk = 'a' AND sk = 1",
        "pk = 'a' AND sk = 1 OR pk = 'a' AND sk = 1",
        "pk = 'a' AND sk = 1 OR pk = 'a' AND sk = 1 AND a = 30",
        "pk = 'a' AND a = 30 OR pk = 'a' AND a = 10",
        "pk = 'a' AND sk = 1 OR pk = 'a' AND a = 10",
        "pk IN ['a', 'b'] AND sk > 1 OR pk = 'b'",
        "(pk = 'a' OR pk = 'a') AND sk = 1",
        "pk = 'a' AND (sk = 1 OR sk = 1)",
        "pk = 'a' AND sk IN [1, 1]",
        "pk = 'a' AND sk = 1 OR pk = 'a' AND sk IN [1, 2]",
        "pk = 'a' OR pk = 'a' ORDER BY pk",
        "pk = 'a' OR pk = 'a' ORDER BY sk DESC",
    ] {
        let statement = format!("SELECT * FROM \"Ordered\" WHERE {clause}");
        let err = run(&db, &statement).expect_err(&statement);
        assert_eq!(message(err), OVERLAP, "{statement}");
    }
    let err = db
        .execute_statement(ExecuteStatementRequest {
            statement: "SELECT * FROM \"Ordered\" WHERE pk = 'a' OR pk = ?".to_string(),
            parameters: Some(vec![AttributeValue::S("a".to_string())]),
            ..Default::default()
        })
        .expect_err("a parameter naming the same partition overlaps");
    assert_eq!(message(err), OVERLAP);
    let err = run(
        &db,
        "SELECT * FROM \"Ordered\".\"gsi\" WHERE gpk = 'g' OR gpk = 'g'",
    )
    .expect_err("an index read is checked on the index key");
    assert_eq!(message(err), OVERLAP);
    let nums = seeded();
    for clause in ["pk = 1 OR pk = 1", "pk = 1 OR pk = 1.0"] {
        let statement = format!("SELECT * FROM \"Nums\" WHERE {clause}");
        let err = run(&nums, &statement).expect_err(&statement);
        assert_eq!(message(err), OVERLAP, "{statement}");
    }
    let err = run(&db, "SELECT * FROM \"Missing\" WHERE pk = 'a' OR pk = 'a'")
        .expect_err("a missing table");
    assert!(
        matches!(err, DynoxideError::ResourceNotFoundException(_)),
        "{err:?}"
    );

    for (clause, expected) in [
        ("pk = 'a' OR pk = 'b'", "a1 a2 a3 b1 b2"),
        ("pk = 'a' AND sk = 1 OR pk = 'a' AND sk = 2", "a1 a2"),
        ("pk = 'a' AND sk = 1 OR pk = 'b' AND sk = 1", "a1 b1"),
        (
            "pk = 'a' AND sk BETWEEN 1 AND 2 OR pk = 'a' AND sk BETWEEN 3 AND 4",
            "a1 a2 a3",
        ),
        ("pk = 'a' AND sk < 2 OR pk = 'a' AND sk > 2", "a1 a3"),
        ("pk = 'a' OR sk = 1", "a1 a2 a3 b1"),
        ("pk = 'a' AND (sk = 1 OR sk = 2)", "a1 a2"),
        (
            "pk IN ['a', 'b'] AND sk = 1 OR pk = 'b' AND sk = 2",
            "a1 b1 b2",
        ),
        ("pk = 'a' AND sk > 1 OR pk = 'b' AND sk > 1", "a2 a3 b2"),
        ("pk = 'a' AND sk IN [1, 2]", "a1 a2"),
        ("NOT pk = 'a' OR pk = 'a'", "a1 a2 a3 b1 b2"),
        ("a = 30 OR a = 30", "a1"),
    ] {
        let statement = format!("SELECT * FROM \"Ordered\" WHERE {clause}");
        let resp = run(&db, &statement).unwrap_or_else(|e| panic!("{statement}: {e:?}"));
        let mut keys = keys_as_returned(resp);
        keys.sort();
        assert_eq!(keys.join(" "), expected, "{statement}");
    }
}

#[test]
fn an_or_with_no_key_condition_under_it_is_a_filter_not_a_second_range() {
    // Captured eu-west-2 and us-east-1, 2026-10-09. An OR under a conjunct
    // that names no key attribute narrows nothing, so the read stays one key
    // range and runs. An OR with a key condition anywhere under it splits the
    // read as before, and is refused when its ranges meet.
    const OVERLAP: &str =
        "Overlapping conditions with range keys are not supported in where clause";
    let db = ordered_table();
    for (clause, expected) in [
        ("pk = 'a' AND (a = 30 OR a = 10)", "a1 a2"),
        ("pk = 'a' AND (a = 30 OR a = 10) AND sk = 1", "a1"),
        (
            "(pk = 'a' AND (a = 30 OR a = 10)) OR pk = 'b'",
            "a1 a2 b1 b2",
        ),
        ("pk = 'a' AND NOT (a = 30 AND a = 10)", "a1 a2 a3"),
        (
            "pk = 'a' AND (a = 30 OR a = 10) AND (a = 30 OR a = 20)",
            "a1",
        ),
        ("pk IN ['a', 'b'] AND (a = 30 OR a = 5)", "a1 b1"),
        (
            "pk = 'a' AND (attribute_exists(nope) OR attribute_exists(a))",
            "a1 a2 a3",
        ),
    ] {
        let statement = format!("SELECT * FROM \"Ordered\" WHERE {clause}");
        let resp = run(&db, &statement).unwrap_or_else(|e| panic!("{statement}: {e:?}"));
        let mut keys = keys_as_returned(resp);
        keys.sort();
        assert_eq!(keys.join(" "), expected, "{statement}");
    }
    for clause in [
        "pk = 'a' AND (sk = 1 OR a = 10)",
        "pk = 'a' AND (a = 30 OR a = 10) OR pk = 'a' AND sk = 3",
        "pk = 'a' AND (a = 30 OR sk > 2)",
    ] {
        let statement = format!("SELECT * FROM \"Ordered\" WHERE {clause}");
        let err = run(&db, &statement).expect_err(&statement);
        assert_eq!(message(err), OVERLAP, "{statement}");
    }
}

#[test]
fn parameters_are_number_checked_in_batches_and_transactions() {
    // Captured eu-west-2 and us-east-1, 2026-10-09. In a batch each statement's
    // parameters are checked against that statement, used or not, ahead of a
    // missing table, with the bare message and code `ValidationError`, while
    // the rest run. In a transaction they refuse the whole request with the
    // bare message, and nothing is written. A literal DynamoDB can't store is
    // reported against its statement in a batch, and in a transaction with the
    // statement's index in front.
    let db = seeded();
    let big = || Some(vec![AttributeValue::N("1e126".to_string())]);
    let resp = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            return_consumed_capacity: None,
            statements: vec![
                batch_statement("SELECT * FROM \"Nums\" WHERE pk = ?", big()),
                batch_statement("SELECT * FROM \"Nums\" WHERE pk = 1", big()),
                batch_statement("SELECT * FROM \"Nums\" WHERE pk = 15", None),
                batch_statement("SELECT * FROM \"Missing\" WHERE pk = ?", big()),
                batch_statement(
                    "SELECT * FROM \"Nums\" WHERE pk = ?",
                    Some(vec![AttributeValue::L(vec![AttributeValue::N(
                        "1e126".to_string(),
                    )])]),
                ),
                batch_statement(
                    "SELECT * FROM \"Nums\" WHERE pk = ?",
                    Some(vec![AttributeValue::N("1e-131".to_string())]),
                ),
                batch_statement("SELECT * FROM \"Nums\" WHERE pk = 1e126", None),
            ],
        })
        .unwrap();
    let errors: Vec<Option<(String, String)>> = resp
        .responses
        .iter()
        .map(|r| {
            r.error
                .as_ref()
                .map(|e| (e.code.clone(), e.message.clone()))
        })
        .collect();
    let invalid = |m: String| Some(("ValidationError".to_string(), m));
    assert_eq!(
        errors,
        vec![
            invalid(OVERFLOW.to_string()),
            invalid(OVERFLOW.to_string()),
            None,
            invalid(OVERFLOW.to_string()),
            invalid(OVERFLOW.to_string()),
            invalid(UNDERFLOW.to_string()),
            invalid(format!("{OVERFLOW} under root")),
        ]
    );

    let resp = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            return_consumed_capacity: None,
            statements: vec![batch_statement(
                "INSERT INTO \"Nums\" VALUE {'pk': ?}",
                big(),
            )],
        })
        .unwrap();
    let err = resp.responses[0]
        .error
        .as_ref()
        .expect("a bad parameter errors");
    assert_eq!(
        (err.code.as_str(), err.message.as_str()),
        ("ValidationError", OVERFLOW)
    );

    for statements in [
        vec![("INSERT INTO \"Nums\" VALUE {'pk': ?}", big())],
        vec![
            ("INSERT INTO \"Nums\" VALUE {'pk': 77}", None),
            ("INSERT INTO \"Nums\" VALUE {'pk': ?}", big()),
        ],
        vec![("SELECT * FROM \"Nums\" WHERE pk = ?", big())],
        vec![("INSERT INTO \"Missing\" VALUE {'pk': ?}", big())],
        vec![("UPDATE \"Nums\" SET a = ? WHERE pk = 1", big())],
        vec![("UPDATE \"Nums\" SET a = 1 WHERE pk = 1", big())],
    ] {
        let err = transaction(&db, &statements).expect_err("a bad parameter refuses it");
        assert_eq!(message(err), OVERFLOW, "{statements:?}");
    }
    for (statements, index) in [
        (vec![("INSERT INTO \"Nums\" VALUE {'pk': 1e126}", None)], 0),
        (
            vec![
                ("INSERT INTO \"Nums\" VALUE {'pk': 78}", None),
                ("INSERT INTO \"Nums\" VALUE {'pk': 1e126}", None),
            ],
            1,
        ),
    ] {
        let err = transaction(&db, &statements).expect_err("a bad literal refuses it");
        assert_eq!(
            message(err),
            format!("Validation failed in TransactStatements[{index}]: {OVERFLOW} under root")
        );
    }
    for pk in ["77", "78"] {
        let resp = run(&db, &format!("SELECT * FROM \"Nums\" WHERE pk = {pk}")).unwrap();
        assert!(
            pks(resp).is_empty(),
            "pk {pk} was written by a refused transaction"
        );
    }
}

#[test]
fn a_key_parameter_of_a_non_key_type_gets_dynamodbs_type_message() {
    // Captured eu-west-2, 2026-10-09: `SELECT * FROM t WHERE pk = ?` with a
    // map parameter answers `Key value must be of type S, N, or B. Key name:
    // pk, Key type: M` on ExecuteStatement and against the statement in a
    // batch, and cancels a transaction with GetItem's reason, `The provided
    // key element does not match the schema`. They used to say `Invalid key
    // value`, and the batch claimed the key was missing.
    const TYPE_MESSAGE: &str = "Key value must be of type S, N, or B. Key name: pk, Key type: M";
    let db = seeded();
    let map = || {
        Some(vec![AttributeValue::M(
            [("n".to_string(), AttributeValue::S("leaf".to_string()))]
                .into_iter()
                .collect(),
        )])
    };
    let err = db
        .execute_statement(ExecuteStatementRequest {
            statement: "SELECT * FROM \"Nums\" WHERE pk = ?".to_string(),
            parameters: map(),
            ..Default::default()
        })
        .expect_err("a map is not a key");
    assert_eq!(message(err), TYPE_MESSAGE);

    let resp = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            return_consumed_capacity: None,
            statements: vec![batch_statement(
                "SELECT * FROM \"Nums\" WHERE pk = ?",
                map(),
            )],
        })
        .unwrap();
    let err = resp.responses[0]
        .error
        .as_ref()
        .expect("a map is not a key");
    assert_eq!(
        (err.code.as_str(), err.message.as_str()),
        ("ValidationError", TYPE_MESSAGE)
    );

    match transaction(&db, &[("SELECT * FROM \"Nums\" WHERE pk = ?", map())]) {
        Err(DynoxideError::TransactionCanceledException(_, reasons)) => {
            assert_eq!(reasons[0].code, "ValidationError");
            assert_eq!(
                reasons[0].message.as_deref(),
                Some("The provided key element does not match the schema")
            );
        }
        other => panic!("expected a cancellation, got {other:?}"),
    }
}

#[test]
fn an_insert_of_an_existing_key_is_a_duplicate_before_its_depth_is_checked() {
    // Captured eu-west-2, 2026-10-09: an INSERT whose key already exists, with
    // a parameter that leaves the item 32 levels deep, is a duplicate on
    // ExecuteStatement, in a batch and in a transaction; the short nesting
    // message only comes for a new key.
    let db = seeded();
    let deep = || Some(vec![nested_map_value(32)]);
    let err = db
        .execute_statement(ExecuteStatementRequest {
            statement: "INSERT INTO \"Nums\" VALUE {'pk': 1, 'd': ?}".to_string(),
            parameters: deep(),
            ..Default::default()
        })
        .expect_err("the key exists");
    assert!(
        matches!(err, DynoxideError::DuplicateItemException(ref m) if m == "Duplicate primary key exists in table"),
        "{err:?}"
    );
    let resp = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            return_consumed_capacity: None,
            statements: vec![batch_statement(
                "INSERT INTO \"Nums\" VALUE {'pk': 1, 'd': ?}",
                deep(),
            )],
        })
        .unwrap();
    let err = resp.responses[0].error.as_ref().expect("the key exists");
    assert_eq!(err.code, "DuplicateItem");
    match transaction(
        &db,
        &[("INSERT INTO \"Nums\" VALUE {'pk': 1, 'd': ?}", deep())],
    ) {
        Err(DynoxideError::TransactionCanceledException(_, reasons)) => {
            assert_eq!(reasons[0].code, "DuplicateItem");
        }
        other => panic!("expected a cancellation, got {other:?}"),
    }
}

#[test]
fn a_batch_or_transaction_select_may_name_only_the_primary_key() {
    // Captured eu-west-2, 2026-10-09: `WHERE pk = 'a' AND data = ?` runs on
    // ExecuteStatement, but a batch refuses it against the statement and a
    // transaction refuses the request, each with its own message. Both used to
    // run it.
    let db = seeded();
    let data = || Some(vec![AttributeValue::S("x".to_string())]);
    db.execute_statement(ExecuteStatementRequest {
        statement: "SELECT * FROM \"Nums\" WHERE pk = 1 AND data = ?".to_string(),
        parameters: data(),
        ..Default::default()
    })
    .expect("ExecuteStatement runs it");
    let resp = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            return_consumed_capacity: None,
            statements: vec![batch_statement(
                "SELECT * FROM \"Nums\" WHERE pk = 1 AND data = ?",
                data(),
            )],
        })
        .unwrap();
    let err = resp.responses[0]
        .error
        .as_ref()
        .expect("a batch refuses it");
    assert_eq!(
        (err.code.as_str(), err.message.as_str()),
        (
            "ValidationError",
            "Select statements within BatchExecuteStatement must specify the primary key in \
             the where clause."
        )
    );
    let err = transaction(
        &db,
        &[("SELECT * FROM \"Nums\" WHERE pk = 1 AND data = ?", data())],
    )
    .expect_err("a transaction refuses it");
    assert_eq!(
        message(err),
        "Validation failed in TransactStatements[0]: Select statements within \
         ExecuteTransaction must specify the primary key in the where clause."
    );
}

#[test]
fn a_transaction_reports_a_statement_that_does_not_parse_by_its_index() {
    // Captured eu-west-2 and us-east-1, 2026-10-09: `INSRT INTO ...` in a
    // transaction is `Validation failed in TransactStatements[0]: Statement
    // wasn't well formed, can't be processed: Unexpected token after
    // expression`. The first word reads as an expression and the next as a
    // token after it; `SLECT * FROM ...` stays `Expected data manipulation`.
    let db = seeded();
    let err = transaction(&db, &[("INSRT INTO \"Nums\" VALUE {'pk': 50}", None)])
        .expect_err("the statement does not parse");
    assert_eq!(
        message(err),
        format!(
            "Validation failed in TransactStatements[0]: {NOT_WELL_FORMED}Unexpected token after expression"
        )
    );
    let err = transaction(
        &db,
        &[
            ("INSERT INTO \"Nums\" VALUE {'pk': 50}", None),
            ("SLECT * FROM \"Nums\"", None),
        ],
    )
    .expect_err("the second statement does not parse");
    assert_eq!(
        message(err),
        format!(
            "Validation failed in TransactStatements[1]: {NOT_WELL_FORMED}Expected data manipulation"
        )
    );
    let resp = run(&db, "SELECT * FROM \"Nums\" WHERE pk = 50").unwrap();
    assert!(
        pks(resp).is_empty(),
        "the refused transaction wrote nothing"
    );
}

#[test]
fn a_bad_parameter_in_a_batch_on_the_wire_fails_only_its_statement() {
    // Over the wire the request is decoded before the batch runs, and a number
    // decoded there used to refuse the whole request. DynamoDB answers it
    // against the statement and runs the rest (captured eu-west-2 and
    // us-east-1, 2026-10-09).
    let db = seeded();
    let decode = |body: serde_json::Value| -> BatchExecuteStatementRequest {
        serde_json::from_value(body).expect("a bad parameter doesn't stop the request decoding")
    };
    let errors =
        |resp: dynoxide::actions::batch_execute_statement::BatchExecuteStatementResponse| {
            resp.responses
                .iter()
                .map(|r| {
                    r.error
                        .as_ref()
                        .map(|e| (e.code.clone(), e.message.clone()))
                })
                .collect::<Vec<_>>()
        };
    let invalid = |m: &str| Some(("ValidationError".to_string(), m.to_string()));

    let resp = db
        .batch_execute_statement(decode(serde_json::json!({
            "Statements": [
                {"Statement": "SELECT * FROM \"Nums\" WHERE pk = ?", "Parameters": [{"N": "1e126"}]},
                {"Statement": "SELECT * FROM \"Nums\" WHERE pk = 15"},
                {"Statement": "SELECT * FROM \"Nums\" WHERE pk = ?", "Parameters": [{"N": "1e-131"}]}
            ]
        })))
        .unwrap();
    assert_eq!(
        errors(resp),
        vec![invalid(OVERFLOW), None, invalid(UNDERFLOW)]
    );

    let resp = db
        .batch_execute_statement(decode(serde_json::json!({
            "Statements": [
                {"Statement": "INSERT INTO \"Nums\" VALUE {'pk': 60}", "Parameters": [{"N": "1e126"}]},
                {"Statement": "INSERT INTO \"Nums\" VALUE {'pk': 61}"}
            ]
        })))
        .unwrap();
    assert_eq!(errors(resp), vec![invalid(OVERFLOW), None]);
    let resp = run(&db, "SELECT * FROM \"Nums\" WHERE pk = 60").unwrap();
    assert!(pks(resp).is_empty(), "the refused statement wrote nothing");
    let resp = run(&db, "SELECT * FROM \"Nums\" WHERE pk = 61").unwrap();
    assert_eq!(pks(resp), vec!["61".to_string()], "its sibling ran");
}

const KEY_SCHEMA_MISMATCH: &str =
    "Key attribute's data type should match its data type in table's schema: Key";

fn key_type_message(key: &str, kind: &str) -> String {
    format!("Key value must be of type S, N, or B. Key name: {key}, Key type: {kind}")
}

fn with_parameters(
    db: &Database,
    statement: &str,
    parameters: Vec<AttributeValue>,
) -> Result<ExecuteStatementResponse, DynoxideError> {
    // No parameters means no Parameters member: an empty list is a request
    // member error on DynamoDB.
    db.execute_statement(ExecuteStatementRequest {
        statement: statement.to_string(),
        parameters: (!parameters.is_empty()).then_some(parameters),
        ..Default::default()
    })
}

#[test]
fn key_values_of_the_wrong_type_get_dynamodbs_messages() {
    // Captured eu-west-2 and us-east-1, 2026-10-09. A key value of a type no
    // key can have names its type; a scalar of the wrong type names the key
    // instead, on the sort key too, in literals and parameters, on SELECT,
    // UPDATE, DELETE and INSERT (INSERT says the second either way). Only
    // equality on a key is checked this way, a range on the sort key too; two
    // equalities on one key are left alone.
    let db = seeded();
    let ordered = ordered_table();
    let map = AttributeValue::M(
        [("n".to_string(), AttributeValue::S("x".to_string()))]
            .into_iter()
            .collect(),
    );
    let cases: Vec<(&Database, &str, Vec<AttributeValue>, String)> = vec![
        (
            &db,
            "SELECT * FROM \"Nums\" WHERE pk = ?",
            vec![AttributeValue::L(vec![])],
            key_type_message("pk", "L"),
        ),
        (
            &db,
            "SELECT * FROM \"Nums\" WHERE pk = ?",
            vec![AttributeValue::BOOL(true)],
            key_type_message("pk", "BOOL"),
        ),
        (
            &db,
            "SELECT * FROM \"Nums\" WHERE pk = ?",
            vec![AttributeValue::NULL(true)],
            key_type_message("pk", "NULL"),
        ),
        (
            &db,
            "SELECT * FROM \"Nums\" WHERE pk = ?",
            vec![AttributeValue::SS(vec!["a".to_string()])],
            key_type_message("pk", "SS"),
        ),
        (
            &db,
            "SELECT * FROM \"Nums\" WHERE pk = ?",
            vec![AttributeValue::S("x".to_string())],
            format!("{KEY_SCHEMA_MISMATCH} pk"),
        ),
        (
            &db,
            "SELECT * FROM \"Nums\" WHERE pk = ? OR pk = 5",
            vec![AttributeValue::BOOL(true)],
            key_type_message("pk", "BOOL"),
        ),
        (
            &db,
            "SELECT * FROM \"Nums\" WHERE pk IN [?]",
            vec![AttributeValue::BOOL(true)],
            key_type_message("pk", "BOOL"),
        ),
        (
            &db,
            "UPDATE \"Nums\" SET a = 1 WHERE pk = ?",
            vec![map.clone()],
            key_type_message("pk", "M"),
        ),
        (
            &db,
            "DELETE FROM \"Nums\" WHERE pk = ?",
            vec![AttributeValue::BOOL(true)],
            key_type_message("pk", "BOOL"),
        ),
        (
            &db,
            "UPDATE \"Nums\" SET a = 1 WHERE pk = ?",
            vec![AttributeValue::S("x".to_string())],
            format!("{KEY_SCHEMA_MISMATCH} pk"),
        ),
        (
            &db,
            "DELETE FROM \"Nums\" WHERE pk = ?",
            vec![AttributeValue::S("x".to_string())],
            format!("{KEY_SCHEMA_MISMATCH} pk"),
        ),
        (
            &db,
            "INSERT INTO \"Nums\" VALUE {'pk': ?}",
            vec![map.clone()],
            format!("{KEY_SCHEMA_MISMATCH} pk"),
        ),
        (
            &db,
            "INSERT INTO \"Nums\" VALUE {'pk': ?}",
            vec![AttributeValue::S("x".to_string())],
            format!("{KEY_SCHEMA_MISMATCH} pk"),
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND sk = ?",
            vec![map.clone()],
            key_type_message("sk", "M"),
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = ? AND sk = 1",
            vec![map.clone()],
            key_type_message("pk", "M"),
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND sk = ?",
            vec![AttributeValue::S("x".to_string())],
            format!("{KEY_SCHEMA_MISMATCH} sk"),
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND sk > ?",
            vec![AttributeValue::S("x".to_string())],
            format!("{KEY_SCHEMA_MISMATCH} sk"),
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' OR pk = 'a' AND sk = ?",
            vec![AttributeValue::BOOL(true)],
            key_type_message("sk", "BOOL"),
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND sk = ? ORDER BY a",
            vec![AttributeValue::BOOL(true)],
            "Attribute a in ORDER BY clause must be part of the primary key".to_string(),
        ),
    ];
    for (db, statement, parameters, expected) in cases {
        let err = with_parameters(db, statement, parameters).expect_err(statement);
        assert_eq!(message(err), expected, "{statement}");
    }
    for (statement, expected) in [
        (
            "SELECT * FROM \"Nums\" WHERE pk = {'a': 1}",
            key_type_message("pk", "M"),
        ),
        (
            "SELECT * FROM \"Nums\" WHERE pk = [1]",
            key_type_message("pk", "L"),
        ),
        (
            "SELECT * FROM \"Nums\" WHERE pk = true",
            key_type_message("pk", "BOOL"),
        ),
        (
            "SELECT * FROM \"Nums\" WHERE pk = null",
            key_type_message("pk", "NULL"),
        ),
        (
            "SELECT * FROM \"Nums\" WHERE pk = 'x'",
            format!("{KEY_SCHEMA_MISMATCH} pk"),
        ),
        (
            "UPDATE \"Nums\" SET a = 1 WHERE pk = {'a': 1}",
            key_type_message("pk", "M"),
        ),
    ] {
        let err = run(&db, statement).expect_err(statement);
        assert_eq!(message(err), expected, "{statement}");
    }
    let resp = with_parameters(
        &db,
        "SELECT * FROM \"Nums\" WHERE pk = ? AND pk = 'x'",
        vec![AttributeValue::BOOL(true)],
    )
    .expect("two equalities on one key are not a key read");
    assert!(pks(resp).is_empty());
}

#[test]
fn key_values_of_the_wrong_type_in_batches_and_transactions() {
    // Captured eu-west-2 and us-east-1, 2026-10-09. A batch reports either
    // message against the statement. A transaction refuses an UPDATE or DELETE
    // key of a non-key type up front, with the statement's index; a wrong
    // scalar there, or any bad key on a SELECT, cancels with GetItem's reason;
    // and an INSERT key of the wrong type cancels with PutItem's.
    let db = seeded();
    let map = || {
        AttributeValue::M(
            [("n".to_string(), AttributeValue::S("x".to_string()))]
                .into_iter()
                .collect(),
        )
    };
    let s = || AttributeValue::S("x".to_string());
    let resp = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            return_consumed_capacity: None,
            statements: vec![
                batch_statement(
                    "SELECT * FROM \"Nums\" WHERE pk = ?",
                    Some(vec![AttributeValue::BOOL(true)]),
                ),
                batch_statement("SELECT * FROM \"Nums\" WHERE pk = ?", Some(vec![s()])),
            ],
        })
        .unwrap();
    let messages: Vec<String> = resp
        .responses
        .iter()
        .map(|r| r.error.as_ref().expect("refused").message.clone())
        .collect();
    assert_eq!(
        messages,
        vec![
            key_type_message("pk", "BOOL"),
            format!("{KEY_SCHEMA_MISMATCH} pk")
        ]
    );
    let resp = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            return_consumed_capacity: None,
            statements: vec![
                batch_statement("UPDATE \"Nums\" SET a = 1 WHERE pk = ?", Some(vec![map()])),
                batch_statement("UPDATE \"Nums\" SET a = 1 WHERE pk = ?", Some(vec![s()])),
                batch_statement("INSERT INTO \"Nums\" VALUE {'pk': ?}", Some(vec![map()])),
            ],
        })
        .unwrap();
    let messages: Vec<String> = resp
        .responses
        .iter()
        .map(|r| r.error.as_ref().expect("refused").message.clone())
        .collect();
    assert_eq!(
        messages,
        vec![
            key_type_message("pk", "M"),
            format!("{KEY_SCHEMA_MISMATCH} pk"),
            format!("{KEY_SCHEMA_MISMATCH} pk")
        ]
    );

    for (statement, parameter, expected) in [
        (
            "UPDATE \"Nums\" SET a = 1 WHERE pk = ?",
            map(),
            key_type_message("pk", "M"),
        ),
        (
            "DELETE FROM \"Nums\" WHERE pk = ?",
            AttributeValue::BOOL(true),
            key_type_message("pk", "BOOL"),
        ),
    ] {
        let err = transaction(&db, &[(statement, Some(vec![parameter]))]).expect_err(statement);
        assert_eq!(
            message(err),
            format!("Validation failed in TransactStatements[0]: {expected}")
        );
    }
    for (statement, parameter, reason) in [
        ("SELECT * FROM \"Nums\" WHERE pk = ?", AttributeValue::BOOL(true), "The provided key element does not match the schema".to_string()),
        ("SELECT * FROM \"Nums\" WHERE pk = ?", s(), "The provided key element does not match the schema".to_string()),
        ("UPDATE \"Nums\" SET a = 1 WHERE pk = ?", s(), "The provided key element does not match the schema".to_string()),
        ("INSERT INTO \"Nums\" VALUE {'pk': ?}", map(), "One or more parameter values were invalid: Type mismatch for key pk expected: N actual: M".to_string()),
        ("INSERT INTO \"Nums\" VALUE {'pk': ?}", s(), "One or more parameter values were invalid: Type mismatch for key pk expected: N actual: S".to_string()),
    ] {
        match transaction(&db, &[(statement, Some(vec![parameter]))]) {
            Err(DynoxideError::TransactionCanceledException(_, reasons)) => {
                assert_eq!(reasons[0].code, "ValidationError", "{statement}");
                assert_eq!(reasons[0].message.as_deref(), Some(reason.as_str()), "{statement}");
            }
            other => panic!("{statement}: expected a cancellation, got {other:?}"),
        }
    }
}

#[test]
fn a_statement_that_is_not_a_verb_is_read_as_dynamodb_reads_it() {
    // Captured eu-west-2 and us-east-1, 2026-10-09. DynamoDB reads a statement
    // that doesn't start with SELECT, INSERT, UPDATE or DELETE as an
    // expression. A whole expression is refused by what it is; one with
    // tokens after it, or one that breaks off, as a statement that is not well
    // formed, with the reason.
    let db = seeded();
    let unsupported = |kind: &str| format!("Unsupported operation: {kind}");
    let exists = "EXISTS can only be used in ExecuteTransaction write requests.".to_string();
    let malformed = |detail: &str| format!("{NOT_WELL_FORMED}{detail}");
    for (statement, expected) in [
        ("FOO", unsupported("Id")),
        ("INSRT", unsupported("Id")),
        ("FOO;", unsupported("Id")),
        ("(FOO)", unsupported("Id")),
        ("\"FOO\"", unsupported("Id")),
        ("FOO.BAR", unsupported("Path")),
        ("FOO[0]", unsupported("Path")),
        ("FOO.BAR.BAZ", unsupported("Path")),
        ("FOO.*", unsupported("Path")),
        ("\"FOO\".bar", unsupported("Path")),
        ("FOO IS MISSING", unsupported("IsType")),
        ("FOO IS NULL", unsupported("IsType")),
        ("1", unsupported("Lit")),
        ("'a'", unsupported("Lit")),
        ("TRUE", unsupported("Lit")),
        ("NULL", unsupported("Lit")),
        ("-1", unsupported("Lit")),
        ("MISSING", unsupported("Missing")),
        ("[1]", unsupported("List")),
        ("{'a': 1}", unsupported("Struct")),
        ("<<1>>", unsupported("Bag")),
        ("FOO AND BAR", exists.clone()),
        ("FOO OR BAR", exists.clone()),
        ("1 + 1", exists.clone()),
        ("FOO = 1", exists.clone()),
        ("FOO(1)", exists.clone()),
        ("FOO * BAR", exists.clone()),
        ("FOO IN [1]", exists.clone()),
        ("FOO NOT IN [1]", exists.clone()),
        ("FOO BETWEEN 1 AND 2", exists.clone()),
        ("FOO LIKE 'x'", exists.clone()),
        ("FOO IS NOT MISSING", exists.clone()),
        ("NOT FOO", exists.clone()),
        ("SLECT * 1", exists.clone()),
        ("EXISTS (SELECT * FROM \"Nums\")", exists.clone()),
        (
            "INSRT INTO \"Nums\" VALUE {'pk': 1}",
            malformed("Unexpected token after expression"),
        ),
        (
            "UPDTE \"Nums\" SET a = 1 WHERE pk = 1",
            malformed("Unexpected token after expression"),
        ),
        (
            "DELET FROM \"Nums\" WHERE pk = 1",
            malformed("Unexpected token after expression"),
        ),
        (
            "SELEC pk FROM \"Nums\"",
            malformed("Unexpected token after expression"),
        ),
        ("FOO BAR", malformed("Unexpected token after expression")),
        ("FOO, BAR", malformed("Unexpected token after expression")),
        ("'a' b", malformed("Unexpected token after expression")),
        ("FOO 1", malformed("Unexpected token after expression")),
        ("(FOO) BAR", malformed("Unexpected token after expression")),
        (
            "FOO + 'a' BAR",
            malformed("Unexpected token after expression"),
        ),
        (
            "SLECT * FROM \"Nums\"",
            malformed("Expected data manipulation"),
        ),
        ("SLECT * FROM", malformed("Unexpected term")),
        ("FOO = FROM", malformed("Unexpected term")),
        ("FOO (", malformed("Unexpected term")),
        (";", malformed("Unexpected term")),
        (
            "1 +",
            malformed("Missing right-hand side expression of infix operator"),
        ),
        (
            "FOO =",
            malformed("Missing right-hand side expression of infix operator"),
        ),
        (
            "FOO AND",
            malformed("Missing right-hand side expression of infix operator"),
        ),
        (
            "SLECT *",
            malformed("Missing right-hand side expression of infix operator"),
        ),
        ("FOO.", malformed("Invalid path dot component")),
        ("FOO..BAR", malformed("Invalid path dot component")),
        (
            "FOO;;",
            malformed("Unexpected token after semicolon. (Only one query is allowed.)"),
        ),
    ] {
        let err = run(&db, statement).expect_err(statement);
        assert_eq!(message(err), expected, "{statement}");
    }
    let err = with_parameters(&db, "?", vec![AttributeValue::N("1".to_string())]).expect_err("?");
    assert_eq!(message(err), unsupported("Parameter"));

    // The same in a batch, and with the index in front in a transaction.
    let resp = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            return_consumed_capacity: None,
            statements: vec![batch_statement("FOO = 1", None)],
        })
        .unwrap();
    assert_eq!(resp.responses[0].error.as_ref().unwrap().message, exists);
    let err = transaction(&db, &[("FOO.BAR.BAZ", None)]).expect_err("a path");
    assert_eq!(
        message(err),
        format!(
            "Validation failed in TransactStatements[0]: {}",
            unsupported("Path")
        )
    );
}

#[test]
fn a_statement_may_end_with_one_semicolon() {
    // Captured eu-west-2 and us-east-1, 2026-10-09: a trailing semicolon is
    // accepted, with or without a space before it, and in a transaction; a
    // second statement after it is refused.
    let db = seeded();
    for statement in [
        "SELECT * FROM \"Nums\" WHERE pk = 1;",
        "SELECT * FROM \"Nums\" WHERE pk = 1 ;",
    ] {
        let resp = run(&db, statement).unwrap_or_else(|e| panic!("{statement}: {e:?}"));
        assert_eq!(pks(resp), vec!["1".to_string()], "{statement}");
    }
    run(&db, "INSERT INTO \"Nums\" VALUE {'pk': 77};").unwrap();
    let resp = run(&db, "SELECT * FROM \"Nums\" WHERE pk = 77").unwrap();
    assert_eq!(pks(resp), vec!["77".to_string()]);
    transaction(&db, &[("SELECT * FROM \"Nums\" WHERE pk = 1;", None)])
        .expect("a transaction takes it too");
    for statement in [
        "SELECT * FROM \"Nums\"; SELECT * FROM \"Nums\"",
        "SELECT * FROM \"Nums\" WHERE pk = 1; foo",
    ] {
        let err = run(&db, statement).expect_err(statement);
        assert_eq!(
            message(err),
            format!(
                "{NOT_WELL_FORMED}Unexpected token after semicolon. (Only one query is allowed.)"
            ),
            "{statement}"
        );
    }
}

#[test]
fn a_batch_or_transaction_select_names_one_value_for_each_key_attribute() {
    // Captured eu-west-2 and us-east-1, 2026-10-09: the same equality twice,
    // and an IN with one value, are accepted; two different values for one
    // key, an IN with two, or a key repeated in an IN that overlaps are not.
    let db = ordered_table();
    for statement in [
        "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND sk = 1 AND sk = 1",
        "SELECT * FROM \"Ordered\" WHERE sk = 1 AND pk = 'a'",
        "SELECT * FROM \"Ordered\" WHERE pk IN ['a'] AND sk = 1",
        "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND sk IN [1]",
        "SELECT pk FROM \"Ordered\" WHERE (pk = 'a' AND sk = 1)",
    ] {
        let resp = db
            .batch_execute_statement(BatchExecuteStatementRequest {
                return_consumed_capacity: None,
                statements: vec![batch_statement(statement, None)],
            })
            .unwrap();
        assert!(
            resp.responses[0].error.is_none(),
            "{statement}: {:?}",
            resp.responses[0].error
        );
        transaction(&db, &[(statement, None)]).unwrap_or_else(|e| panic!("{statement}: {e:?}"));
    }
    for (statement, batch_message, transaction_message) in [
        (
            "SELECT * FROM \"Ordered\" WHERE pk IN ['a', 'b'] AND sk = 1",
            "Select statements within BatchExecuteStatement must specify the primary key in the where clause.",
            "Select statements within ExecuteTransaction must specify the primary key in the where clause.",
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND sk = 1 AND sk = 2",
            "Select statements within BatchExecuteStatement must specify the primary key in the where clause.",
            "Select statements within ExecuteTransaction must specify the primary key in the where clause.",
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND sk IN [1, 2]",
            "Select statements within BatchExecuteStatement must specify the primary key in the where clause.",
            "Select statements within ExecuteTransaction must specify the primary key in the where clause.",
        ),
        (
            "SELECT * FROM \"Ordered\" WHERE pk IN ['a', 'a'] AND sk = 1",
            "Overlapping conditions with range keys are not supported in where clause",
            "Overlapping conditions with range keys are not supported in where clause",
        ),
    ] {
        let resp = db
            .batch_execute_statement(BatchExecuteStatementRequest {
                return_consumed_capacity: None,
                statements: vec![batch_statement(statement, None)],
            })
            .unwrap();
        assert_eq!(
            resp.responses[0].error.as_ref().expect(statement).message,
            batch_message,
            "{statement}"
        );
        let err = transaction(&db, &[(statement, None)]).expect_err(statement);
        assert_eq!(
            message(err),
            format!("Validation failed in TransactStatements[0]: {transaction_message}")
        );
    }
}

/// A table with a string sort key, for `begins_with`.
fn strings_table() -> Database {
    let db = Database::memory().unwrap();
    let req = serde_json::json!({
        "TableName": "Strings",
        "KeySchema": [
            {"AttributeName": "pk", "KeyType": "HASH"},
            {"AttributeName": "sk", "KeyType": "RANGE"}
        ],
        "AttributeDefinitions": [
            {"AttributeName": "pk", "AttributeType": "S"},
            {"AttributeName": "sk", "AttributeType": "S"}
        ]
    });
    db.create_table(serde_json::from_value(req).unwrap())
        .unwrap();
    for sk in ["x", "xy", "y"] {
        run(
            &db,
            &format!("INSERT INTO \"Strings\" VALUE {{'pk': 'a', 'sk': '{sk}'}}"),
        )
        .unwrap();
    }
    db
}

#[test]
fn overlap_covers_begins_with_and_skips_a_branch_with_two_sort_key_conditions() {
    // Captured eu-west-2 and us-east-1, 2026-10-09. `begins_with` covers the
    // keys with its prefix. A branch with two different sort key conditions
    // isn't read as a key range, so the clause isn't checked at all; the same
    // condition twice still is.
    const OVERLAP: &str =
        "Overlapping conditions with range keys are not supported in where clause";
    let strings = strings_table();
    for (clause, overlaps) in [
        ("begins_with(sk, 'x') OR pk = 'a' AND sk = 'xy'", true),
        ("begins_with(sk, 'x') OR pk = 'a' AND sk = 'y'", false),
        (
            "begins_with(sk, 'x') OR pk = 'a' AND begins_with(sk, 'xy')",
            true,
        ),
        (
            "begins_with(sk, 'x') OR pk = 'a' AND begins_with(sk, 'y')",
            false,
        ),
        ("begins_with(sk, 'x') OR pk = 'a' AND sk > 'xz'", true),
        ("begins_with(sk, 'x') OR pk = 'a' AND sk > 'y'", false),
        ("begins_with(sk, 'x') OR pk = 'a' AND sk < 'x'", false),
        ("begins_with(sk, 'x') OR pk = 'a' AND sk <= 'x'", true),
        ("begins_with(sk, 'x') OR pk = 'a' AND sk > 'x'", true),
        (
            "begins_with(sk, 'xy') OR pk = 'a' AND sk BETWEEN 'w' AND 'x'",
            false,
        ),
        (
            "begins_with(sk, 'x') OR pk = 'a' AND sk >= 'xy' AND sk < 'y'",
            false,
        ),
    ] {
        let statement = format!("SELECT * FROM \"Strings\" WHERE pk = 'a' AND {clause}");
        let outcome = run(&strings, &statement);
        if overlaps {
            assert_eq!(
                message(outcome.expect_err(&statement)),
                OVERLAP,
                "{statement}"
            );
        } else {
            outcome.unwrap_or_else(|e| panic!("{statement}: {e:?}"));
        }
    }
    let ordered = ordered_table();
    for (clause, overlaps) in [
        (
            "pk = 'a' AND sk > 0 AND sk < 3 OR pk = 'a' AND sk = 2",
            false,
        ),
        (
            "pk = 'a' AND sk >= 1 AND sk <= 1 OR pk = 'a' AND sk = 1",
            false,
        ),
        ("pk = 'a' AND sk > 0 AND sk < 3 OR pk = 'a'", false),
        (
            "pk = 'a' AND sk = 1 AND sk = 1 OR pk = 'a' AND sk = 1",
            true,
        ),
        (
            "pk = 'a' AND sk BETWEEN 0 AND 3 AND a = 30 OR pk = 'a' AND sk = 2",
            true,
        ),
    ] {
        let statement = format!("SELECT * FROM \"Ordered\" WHERE {clause}");
        let outcome = run(&ordered, &statement);
        if overlaps {
            assert_eq!(
                message(outcome.expect_err(&statement)),
                OVERLAP,
                "{statement}"
            );
        } else {
            outcome.unwrap_or_else(|e| panic!("{statement}: {e:?}"));
        }
    }
}

#[test]
fn order_by_terms_are_checked_before_overlap() {
    // Captured eu-west-2 and us-east-1, 2026-10-09: a term that isn't a key,
    // or isn't a plain name, is reported ahead of overlapping conditions, which
    // come ahead of the partition-key checks.
    let db = ordered_table();
    for (clause, expected) in [
        (
            "pk = 'a' OR pk = 'a' ORDER BY a",
            "Attribute a in ORDER BY clause must be part of the primary key".to_string(),
        ),
        (
            "pk = 'a' OR pk = 'a' ORDER BY a, m.x",
            "Attribute a in ORDER BY clause must be part of the primary key".to_string(),
        ),
        (
            "pk = 'a' OR pk = 'a' ORDER BY m.x",
            "Argument to ORDER BY must be of type VariableReference. Type: PATH at 1:61:1"
                .to_string(),
        ),
        (
            "pk IN ['a', 'a'] ORDER BY sk",
            "Overlapping conditions with range keys are not supported in where clause".to_string(),
        ),
        (
            "pk = 'a' AND sk = 1 OR pk = 'a' AND sk = 1 ORDER BY pk",
            "Overlapping conditions with range keys are not supported in where clause".to_string(),
        ),
    ] {
        let statement = format!("SELECT * FROM \"Ordered\" WHERE {clause}");
        let err = run(&db, &statement).expect_err(&statement);
        assert_eq!(message(err), expected, "{statement}");
    }
}

#[test]
fn every_bad_parameter_value_is_reported_like_a_bad_number() {
    // Captured eu-west-2 and us-east-1, 2026-10-09: empty sets, duplicate set
    // members and unreadable numbers in a parameter are refused with the
    // messages any attribute value gets, against the statement in a batch,
    // and for the whole request otherwise.
    let db = seeded();
    for (parameter, expected) in [
        (
            AttributeValue::SS(vec![]),
            "One or more parameter values were invalid: An string set  may not be empty",
        ),
        (
            AttributeValue::NS(vec![]),
            "One or more parameter values were invalid: An number set  may not be empty",
        ),
        (
            AttributeValue::NS(vec!["1".to_string(), "1".to_string()]),
            "Input collection contains duplicates",
        ),
        (
            AttributeValue::SS(vec!["a".to_string(), "a".to_string()]),
            "One or more parameter values were invalid: Input collection [a, a] contains duplicates.",
        ),
        (
            AttributeValue::N("abc".to_string()),
            "The parameter cannot be converted to a numeric value: abc",
        ),
        (
            AttributeValue::N(String::new()),
            "The parameter cannot be converted to a numeric value",
        ),
        (
            AttributeValue::L(vec![AttributeValue::N("abc".to_string())]),
            "The parameter cannot be converted to a numeric value: abc",
        ),
    ] {
        let err = with_parameters(
            &db,
            "SELECT * FROM \"Nums\" WHERE pk = ?",
            vec![parameter.clone()],
        )
        .expect_err(expected);
        assert_eq!(message(err), expected);
        let resp = db
            .batch_execute_statement(BatchExecuteStatementRequest {
                return_consumed_capacity: None,
                statements: vec![
                    batch_statement(
                        "SELECT * FROM \"Nums\" WHERE pk = ?",
                        Some(vec![parameter.clone()]),
                    ),
                    batch_statement("SELECT * FROM \"Nums\" WHERE pk = 1", None),
                ],
            })
            .unwrap();
        let err = resp.responses[0].error.as_ref().expect(expected);
        assert_eq!(
            (err.code.as_str(), err.message.as_str()),
            ("ValidationError", expected)
        );
        assert!(
            resp.responses[1].error.is_none(),
            "{expected}: the sibling runs"
        );
        let err = transaction(
            &db,
            &[("SELECT * FROM \"Nums\" WHERE pk = ?", Some(vec![parameter]))],
        )
        .expect_err(expected);
        assert_eq!(message(err), expected);
    }
    // An empty attribute value can only arrive over the wire.
    let request: BatchExecuteStatementRequest = serde_json::from_value(serde_json::json!({
        "Statements": [
            {"Statement": "SELECT * FROM \"Nums\" WHERE pk = ?", "Parameters": [{}]},
            {"Statement": "SELECT * FROM \"Nums\" WHERE pk = 1"}
        ]
    }))
    .expect("an empty value doesn't stop the request decoding");
    let resp = db.batch_execute_statement(request).unwrap();
    assert_eq!(
        resp.responses[0].error.as_ref().map(|e| e.message.as_str()),
        Some(
            "Supplied AttributeValue is empty, must contain exactly one of the supported datatypes"
        )
    );
    assert!(resp.responses[1].error.is_none());
}

#[test]
fn key_values_are_checked_branch_by_branch_until_one_names_a_key_twice() {
    // Captured eu-west-2 and us-east-1, 2026-10-09. Each OR branch that names
    // the partition key once has its key values checked, in the order written,
    // `=` and an `IN` of the same values counting as one. A branch naming the
    // partition key, or the sort key, two different ways ends the checking
    // there, its own values included; a branch not naming the partition key is
    // passed over.
    let nums = seeded();
    let ordered = ordered_table();
    let bool_ = || AttributeValue::BOOL(true);
    let s = || AttributeValue::S("x".to_string());
    for (db, statement, parameters, expected) in [
        (
            &nums,
            "SELECT * FROM \"Nums\" WHERE pk = ? OR a = 1",
            vec![bool_()],
            Some(key_type_message("pk", "BOOL")),
        ),
        (
            &nums,
            "SELECT * FROM \"Nums\" WHERE pk = ? OR pk > 0",
            vec![bool_()],
            Some(key_type_message("pk", "BOOL")),
        ),
        (
            &nums,
            "SELECT * FROM \"Nums\" WHERE pk = ? AND pk = 5 OR pk = ?",
            vec![bool_(), bool_()],
            None,
        ),
        (
            &nums,
            "SELECT * FROM \"Nums\" WHERE pk = ? OR pk = 5 AND pk = 6",
            vec![bool_()],
            Some(key_type_message("pk", "BOOL")),
        ),
        (
            &nums,
            "SELECT * FROM \"Nums\" WHERE pk = 1 AND pk IN [1] OR pk = ?",
            vec![bool_()],
            Some(key_type_message("pk", "BOOL")),
        ),
        (
            &nums,
            "SELECT * FROM \"Nums\" WHERE pk = ? AND pk IN [?]",
            vec![s(), s()],
            Some(format!("{KEY_SCHEMA_MISMATCH} pk")),
        ),
        (
            &nums,
            "SELECT * FROM \"Nums\" WHERE pk IN [?, 5]",
            vec![bool_()],
            Some(key_type_message("pk", "BOOL")),
        ),
        (
            &nums,
            "SELECT * FROM \"Nums\" WHERE pk IN [5, ?]",
            vec![s()],
            Some(format!("{KEY_SCHEMA_MISMATCH} pk")),
        ),
        (
            &nums,
            "INSERT INTO \"Nums\" VALUE {'pk': ?}",
            vec![bool_()],
            Some(format!("{KEY_SCHEMA_MISMATCH} pk")),
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE a = 1 AND sk = ?",
            vec![bool_()],
            None,
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND sk = 1 AND sk = ?",
            vec![bool_()],
            None,
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = ? AND sk = 1 AND sk = 2",
            vec![bool_()],
            None,
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND sk > 0 AND sk < ?",
            vec![s()],
            None,
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND sk = ? AND sk = ?",
            vec![bool_(), bool_()],
            Some(key_type_message("sk", "BOOL")),
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND sk = ? OR pk = 'b' AND sk = 1 AND sk = 2",
            vec![bool_()],
            Some(key_type_message("sk", "BOOL")),
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND pk = 'a' AND sk = ?",
            vec![bool_()],
            Some(key_type_message("sk", "BOOL")),
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND sk <> ?",
            vec![s()],
            None,
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND begins_with(sk, ?)",
            vec![s()],
            Some(format!("{KEY_SCHEMA_MISMATCH} sk")),
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\".\"gsi\" WHERE gpk = ?",
            vec![AttributeValue::N("1".to_string())],
            Some(format!("{KEY_SCHEMA_MISMATCH} gpk")),
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\".\"gsi\" WHERE gpk = ?",
            vec![bool_()],
            Some(key_type_message("gpk", "BOOL")),
        ),
        (
            &ordered,
            "UPDATE \"Ordered\" SET a = 1 WHERE pk = 'a' AND sk = ?",
            vec![s()],
            Some(format!("{KEY_SCHEMA_MISMATCH} sk")),
        ),
        (
            &ordered,
            "UPDATE \"Ordered\" SET a = 1 WHERE pk = ? AND sk = ?",
            vec![s(), bool_()],
            Some(key_type_message("sk", "BOOL")),
        ),
    ] {
        let outcome = with_parameters(db, statement, parameters);
        match expected {
            Some(expected) => assert_eq!(
                message(outcome.expect_err(statement)),
                expected,
                "{statement}"
            ),
            None => {
                outcome.unwrap_or_else(|e| panic!("{statement}: {e:?}"));
            }
        }
    }

    // A batch takes `=` and an `IN` of the same single value as one key.
    for statement in [
        "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND pk IN ['a'] AND sk = 1",
        "SELECT * FROM \"Ordered\" WHERE pk IN ['a'] AND pk = 'a' AND sk IN [1] AND sk = 1",
        "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND pk = 'a' AND sk = 1",
    ] {
        let resp = ordered
            .batch_execute_statement(BatchExecuteStatementRequest {
                return_consumed_capacity: None,
                statements: vec![batch_statement(statement, None)],
            })
            .unwrap();
        assert!(
            resp.responses[0].error.is_none(),
            "{statement}: {:?}",
            resp.responses[0].error
        );
    }

    // A DELETE key of the wrong type: per statement in a batch, a cancellation
    // in a transaction, and one no key can have refused up front.
    let resp = nums
        .batch_execute_statement(BatchExecuteStatementRequest {
            return_consumed_capacity: None,
            statements: vec![batch_statement(
                "DELETE FROM \"Nums\" WHERE pk = ?",
                Some(vec![s()]),
            )],
        })
        .unwrap();
    assert_eq!(
        resp.responses[0].error.as_ref().unwrap().message,
        format!("{KEY_SCHEMA_MISMATCH} pk")
    );
    match transaction(
        &nums,
        &[("DELETE FROM \"Nums\" WHERE pk = ?", Some(vec![s()]))],
    ) {
        Err(DynoxideError::TransactionCanceledException(_, reasons)) => assert_eq!(
            reasons[0].message.as_deref(),
            Some("The provided key element does not match the schema")
        ),
        other => panic!("expected a cancellation, got {other:?}"),
    }
    let err = transaction(
        &ordered,
        &[(
            "DELETE FROM \"Ordered\" WHERE pk = 'a' AND sk = ?",
            Some(vec![bool_()]),
        )],
    )
    .expect_err("a sort key no key can have");
    assert_eq!(
        message(err),
        format!(
            "Validation failed in TransactStatements[0]: {}",
            key_type_message("sk", "BOOL")
        )
    );
}

#[test]
fn begins_with_and_between_operands_are_checked_before_the_table() {
    // Captured eu-west-2 and us-east-1, 2026-10-09: `begins_with` takes a
    // string or binary operand, and BETWEEN two bounds of one type, the lower
    // first; anywhere in a WHERE clause, negated or not, ahead of a missing
    // table, in written order with the other operand checks.
    let db = seeded();
    let begins = |kind: &str| {
        format!(
            "Incorrect operand type for operator or function; operator or function: begins_with, operand type: {kind}"
        )
    };
    let mixed = |low: &str, high: &str| {
        format!(
            "The BETWEEN operator requires same data type for lower and upper bounds; \
             lower bound operand: AttributeValue: {{{low}}}, upper bound operand: AttributeValue: {{{high}}}"
        )
    };
    for (statement, parameters, expected) in [
        ("SELECT * FROM \"Nums\" WHERE begins_with(a, 1)", vec![], Some(begins("N"))),
        ("SELECT * FROM \"Missing\" WHERE begins_with(a, 1)", vec![], Some(begins("N"))),
        ("SELECT * FROM \"Nums\" WHERE NOT begins_with(a, 1)", vec![], Some(begins("N"))),
        ("UPDATE \"Nums\" SET b = 1 WHERE pk = 1 AND begins_with(a, 1)", vec![], Some(begins("N"))),
        ("SELECT * FROM \"Nums\" WHERE begins_with(a, 'x') AND begins_with(b, 1)", vec![], Some(begins("N"))),
        ("SELECT * FROM \"Nums\" WHERE begins_with(a, ?)", vec![AttributeValue::SS(vec!["a".to_string()])], Some(begins("SS"))),
        ("SELECT * FROM \"Nums\" WHERE begins_with(a, ?)", vec![AttributeValue::NULL(true)], Some(begins("NULL"))),
        ("SELECT * FROM \"Nums\" WHERE begins_with(a, ?)", vec![AttributeValue::B(b"1".to_vec())], None),
        ("SELECT * FROM \"Nums\" WHERE a BETWEEN 'x' AND 3", vec![], Some(mixed("S:x", "N:3"))),
        ("SELECT * FROM \"Missing\" WHERE a BETWEEN 'x' AND 3", vec![], Some(mixed("S:x", "N:3"))),
        ("DELETE FROM \"Nums\" WHERE pk = 1 AND a BETWEEN 'x' AND 3", vec![], Some(mixed("S:x", "N:3"))),
        ("SELECT * FROM \"Nums\" WHERE NOT a BETWEEN 'x' AND 3", vec![], Some(mixed("S:x", "N:3"))),
        (
            "SELECT * FROM \"Nums\" WHERE a BETWEEN ? AND ?",
            vec![AttributeValue::N("1".to_string()), AttributeValue::B(b"1".to_vec())],
            Some(mixed("N:1", "B:[MQ==]")),
        ),
        (
            "SELECT * FROM \"Nums\" WHERE a BETWEEN 3 AND 1",
            vec![],
            Some(
                "The BETWEEN operator requires upper bound to be greater than or equal to lower bound; \
                 lower bound operand: AttributeValue: {N:3}, upper bound operand: AttributeValue: {N:1}"
                    .to_string(),
            ),
        ),
        (
            "SELECT * FROM \"Nums\" WHERE a BETWEEN true AND 3",
            vec![],
            Some("Incorrect operand type for operator or function; operator or function: BETWEEN, operand type: BOOL".to_string()),
        ),
        ("SELECT * FROM \"Nums\" WHERE pk = 1 AND begins_with(a, 1) AND a > true", vec![], Some(begins("N"))),
        (
            "SELECT * FROM \"Nums\" WHERE pk = 1 AND a > true AND begins_with(a, 1)",
            vec![],
            Some("Incorrect operand type for operator or function; operator or function: >, operand type: BOOL".to_string()),
        ),
    ] {
        let outcome = with_parameters(&db, statement, parameters);
        match expected {
            Some(expected) => assert_eq!(message(outcome.expect_err(statement)), expected, "{statement}"),
            None => {
                outcome.unwrap_or_else(|e| panic!("{statement}: {e:?}"));
            }
        }
    }
    // NOT BETWEEN still matches what it did.
    let resp = run(
        &db,
        "SELECT * FROM \"Nums\" WHERE NOT pk BETWEEN 2 AND 1000",
    )
    .unwrap();
    let mut keys = pks(resp);
    keys.sort();
    assert_eq!(
        keys,
        vec!["1".to_string(), "1.5".to_string(), "100000".to_string()]
    );
}

#[test]
fn a_statement_that_is_not_a_verb_breaks_off_where_dynamodb_says() {
    // Captured eu-west-2 and us-east-1, 2026-10-09: where an expression read
    // in place of a statement breaks off, and what DynamoDB says it was.
    let db = seeded();
    let malformed = |detail: &str| format!("{NOT_WELL_FORMED}{detail}");
    let unsupported = |kind: &str| format!("Unsupported operation: {kind}");
    let exists = "EXISTS can only be used in ExecuteTransaction write requests.".to_string();
    let tableless = "Only select from a single table or index is supported.".to_string();
    for (statement, expected) in [
        ("FOO BETWEEN 1", malformed("Expected AND keyword")),
        (
            "FOO BETWEEN 1 AND",
            malformed("Expected expression after AND"),
        ),
        (
            "FOO BETWEEN",
            malformed("Missing right-hand side expression of infix operator"),
        ),
        ("FOO IS", malformed("Expected type name")),
        ("FOO IS BAR", malformed("Expected type name")),
        ("FOO IS NOT", malformed("Expected type name")),
        ("FOO IS NOT BAR", malformed("Expected type name")),
        ("[1, 2", malformed("Expected RIGHT_BRACKET")),
        ("[1 2]", malformed("Expected RIGHT_BRACKET")),
        ("FOO IN [1", malformed("Expected RIGHT_BRACKET")),
        ("FOO[0", malformed("Expected RIGHT_BRACKET")),
        ("{'a': 1", malformed("Expected RIGHT_CURLY")),
        ("{'a': 1 'b': 2}", malformed("Expected RIGHT_CURLY")),
        ("{'a' 1}", malformed("Expected COLON")),
        ("(FOO", malformed("Expected RIGHT_PAREN")),
        ("(FOO BAR)", malformed("Expected RIGHT_PAREN")),
        ("FOO(1", malformed("Expected RIGHT_PAREN")),
        ("FOO(1 2)", malformed("Expected RIGHT_PAREN")),
        ("<<1", malformed("Expected RIGHT_DOUBLE_ANGLE_BRACKET")),
        ("<<1 2>>", malformed("Expected RIGHT_DOUBLE_ANGLE_BRACKET")),
        ("CASE WHEN 1 THEN 2", malformed("Expected END keyword")),
        ("NOT", malformed("Unexpected term")),
        ("-", malformed("Unexpected term")),
        ("+", malformed("Unexpected term")),
        ("FOO[", malformed("Unexpected term")),
        ("CASE", malformed("Unexpected term")),
        ("EXISTS (", malformed("Unexpected term")),
        ("FOO = SELECT", malformed("Unexpected term")),
        ("[1,]", malformed("Unexpected term")),
        ("FOO(1,)", malformed("Unexpected term")),
        ("* FROM \"Nums\"", malformed("Unexpected term")),
        ("SELECT", malformed("Unexpected term")),
        ("EXISTS", malformed("Unexpected keyword")),
        ("WHERE", malformed("Unexpected keyword")),
        ("VALUE {'a': 1}", malformed("Unexpected keyword")),
        ("ORDER BY a", malformed("Unexpected keyword")),
        ("LIMIT 1", malformed("Unexpected keyword")),
        ("FOO + WHERE", malformed("Unexpected keyword")),
        ("INSERT", malformed("Unexpected keyword")),
        ("AND", malformed("Unexpected operator")),
        ("FOO = AND", malformed("Unexpected operator")),
        ("DELETE", malformed("Expected FROM after DELETE")),
        ("UPDATE", malformed("Expected identifier for simple path")),
        ("SET", malformed("Expected identifier for simple path")),
        ("REMOVE", malformed("Expected identifier for simple path")),
        ("SET a", malformed("Expected '='")),
        (
            "SET a = 1 WHERE pk = 1",
            malformed("Unexpected token after expression"),
        ),
        (
            "REMOVE a, b",
            malformed("Unexpected token after expression"),
        ),
        ("FOO,", malformed("Unexpected token after expression")),
        ("FOO.BAR(1)", malformed("Unexpected token after expression")),
        ("FOO NOT", malformed("Unexpected token after expression")),
        ("FROM \"Nums\"", malformed("Expected data manipulation")),
        ("SET a = 1", tableless.clone()),
        ("SET a = 1, b = 2", tableless.clone()),
        ("REMOVE a", tableless.clone()),
        ("CASE WHEN 1 THEN 2 END", unsupported("SearchedCase")),
        ("CASE 1 WHEN 1 THEN 2 END", unsupported("SimpleCase")),
        ("FOO[*]", unsupported("Path")),
        ("{}.a", unsupported("Path")),
        ("[1][0]", unsupported("Path")),
        ("FOO[1] [2]", unsupported("Path")),
        ("- -1", unsupported("Lit")),
        ("NULL IS NULL", unsupported("IsType")),
        ("{}", unsupported("Struct")),
        ("[]", unsupported("List")),
        ("<<1, 2>>", unsupported("Bag")),
        ("\"FOO\"(1)", exists.clone()),
        ("FOO = (SELECT * FROM \"Nums\")", exists.clone()),
        ("-'a'", exists.clone()),
        ("- 1 + 1", exists.clone()),
        ("EXISTS(1)", exists.clone()),
        ("TRUE AND FALSE", exists.clone()),
        ("-FOO", exists.clone()),
    ] {
        let err = run(&db, statement).expect_err(statement);
        assert_eq!(message(err), expected, "{statement}");
    }

    // A semicolon ends an UPDATE, a DELETE and a SELECT with no WHERE alike,
    // and the checks after parsing still run.
    for statement in [
        "UPDATE \"Nums\" SET a = 1 WHERE pk = 1;",
        "DELETE FROM \"Nums\" WHERE pk = 99;",
        "SELECT * FROM \"Nums\";",
    ] {
        run(&db, statement).unwrap_or_else(|e| panic!("{statement}: {e:?}"));
    }
    for statement in [
        "SELECT * FROM \"Nums\" WHERE pk = 1 ;;",
        "SELECT * FROM \"Nums\" WHERE pk = 1; ;",
    ] {
        let err = run(&db, statement).expect_err(statement);
        assert_eq!(
            message(err),
            malformed("Unexpected token after semicolon. (Only one query is allowed.)")
        );
    }
    let err = run(&db, "SELECT * FROM \"Nums\" ORDER BY pk;").expect_err("ORDER BY with no WHERE");
    assert_eq!(
        message(err),
        "Must have WHERE clause in the statement when using ORDER BY clause."
    );
    let resp = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            return_consumed_capacity: None,
            statements: vec![batch_statement(
                "SELECT * FROM \"Nums\" WHERE pk = 1;",
                None,
            )],
        })
        .unwrap();
    assert!(
        resp.responses[0].error.is_none(),
        "{:?}",
        resp.responses[0].error
    );
}

#[test]
fn a_branch_that_is_not_a_key_read_ends_the_key_checks() {
    // Captured eu-west-2 and us-east-1, 2026-10-09: a branch that doesn't name
    // the partition key ends the key value checks as one naming it twice
    // does, and a branch with `NOT ... BETWEEN` on the sort key ends the
    // overlap check.
    let nums = seeded();
    let ordered = ordered_table();
    for (db, statement, parameters) in [
        (
            &nums,
            "SELECT * FROM \"Nums\" WHERE a = 1 OR pk = ?",
            vec![AttributeValue::BOOL(true)],
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE a = 1 OR pk = 'a' AND sk = ?",
            vec![AttributeValue::BOOL(true)],
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND sk = 1 AND sk = 2 OR pk = ?",
            vec![AttributeValue::BOOL(true)],
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND NOT sk BETWEEN 1 AND 2",
            vec![],
        ),
        (
            &ordered,
            "SELECT * FROM \"Ordered\" WHERE pk = 'a' AND NOT sk BETWEEN 1 AND 2 OR pk = 'a' AND sk = 3",
            vec![],
        ),
    ] {
        let resp = with_parameters(db, statement, parameters)
            .unwrap_or_else(|e| panic!("{statement}: {e:?}"));
        if statement.contains("NOT sk BETWEEN") {
            // Neither a1 nor a2; a3 by both branches, once.
            assert_eq!(resp.items.map(|items| items.len()), Some(1), "{statement}");
        } else {
            assert!(pks(resp).is_empty(), "{statement}");
        }
    }
}

#[test]
fn more_statements_that_are_not_a_verb_read_as_dynamodb_reads_them() {
    // Captured eu-west-2 and us-east-1, 2026-10-09.
    let db = seeded();
    let malformed = |detail: &str| format!("{NOT_WELL_FORMED}{detail}");
    let unsupported = |kind: &str| format!("Unsupported operation: {kind}");
    let exists = "EXISTS can only be used in ExecuteTransaction write requests.".to_string();
    let tableless = "Only select from a single table or index is supported.".to_string();
    for (statement, expected) in [
        ("CASE 1 END", malformed("Expected a WHEN clause in CASE")),
        ("CASE WHEN 1 2 END", malformed("Expected THEN keyword")),
        (
            "CASE WHEN 1 THEN 2 ELSE END",
            malformed("Unexpected keyword"),
        ),
        ("CASE 1 WHEN 1 THEN 2 ELSE 3 END", unsupported("SimpleCase")),
        ("SET a =", malformed("Unexpected term")),
        (
            "SET 1 = 1",
            malformed("Expected identifier for simple path"),
        ),
        ("SET a.b = 1", tableless.clone()),
        ("SET a[0] = 1", tableless.clone()),
        ("SET \"a\" = 1", tableless.clone()),
        ("REMOVE a.b", tableless.clone()),
        ("FOO = NOT BAR", exists.clone()),
        ("NOT NOT FOO", exists.clone()),
        ("FOO IS NOT NULL", exists.clone()),
        ("FOO LIKE 'a' ESCAPE 'b'", exists.clone()),
        ("- FOO.BAR", exists.clone()),
        ("'a'.b", unsupported("Path")),
        ("FOO(1).a", unsupported("Path")),
        ("(1).a", unsupported("Path")),
        ("<<>>", unsupported("Bag")),
        ("FOO IS STRING", unsupported("IsType")),
        ("FOO IS INT", unsupported("IsType")),
        ("= 1", malformed("Unexpected operator")),
        ("IN [1]", malformed("Unexpected operator")),
        ("FOO = [1, 2", malformed("Expected RIGHT_BRACKET")),
        ("FOO BETWEEN 1 OR 2", malformed("Expected AND keyword")),
        (
            "INSERT \"Nums\" VALUE {'pk': 1}",
            malformed("Unexpected keyword"),
        ),
        (
            "INSERT INTO",
            malformed("Expected identifier for simple path"),
        ),
        (
            "DELETE \"Nums\" WHERE pk = 1",
            malformed("Expected FROM after DELETE"),
        ),
        (
            "UPDATE 1 SET a = 1",
            malformed("Expected identifier for simple path"),
        ),
        (
            "SELECT FROM \"Nums\"",
            malformed(
                "Expected identifier or an expression but found unexpected keyword 'from' in a select list.",
            ),
        ),
    ] {
        let err = run(&db, statement).expect_err(statement);
        assert_eq!(message(err), expected, "{statement}");
    }
}
