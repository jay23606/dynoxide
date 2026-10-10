//! PartiQL literals and forms DynamoDB refuses, and the order it reports them.
//!
//! - A bag holds only strings or only numbers. One that starts with a string is
//!   refused at the first element that isn't one; one that starts with a
//!   number is refused without a position, unless the element is a parameter,
//!   an Ion literal or `null`. A parameter or an Ion literal after the first
//!   element gets its own message; as the first element it gets the bag's.
//! - Ion literals, written between backticks, are refused by type, and
//!   `X'...'` is not a binary literal.
//! - A PartiQL keyword can't be a path's dot component unless it is quoted.
//! - A call in a SELECT list is an unexpected path component at its name.
//! - `EXISTS(SELECT ...)` in a WHERE clause is an unsupported token at the
//!   SELECT.
//! - After a syntax error, the refusals DynamoDB makes while it reads values
//!   come in the order they are written, WHERE clause first; operand refusals
//!   come after them, and the table is looked up last.
//!
//! Captured eu-west-2 and us-east-1, 2026-10-09; both regions answered every
//! case identically.

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::{ExecuteStatementRequest, ExecuteStatementResponse};
use dynoxide::actions::execute_transaction::{ExecuteTransactionRequest, ParameterizedStatement};
use dynoxide::actions::put_item::PutItemRequest;
use dynoxide::errors::DynoxideError;
use dynoxide::types::{
    AttributeDefinition, AttributeValue, KeySchemaElement, KeyType, ScalarAttributeType,
};
use std::collections::HashMap;

const NOT_WELL_FORMED: &str = "Statement wasn't well formed, can't be processed: ";
const BAG: &str =
    "Unsupported data type in Bag. DynamoDB only supports either numbers or strings in bags";
const IS_INT: &str = "Unsupported data type for IS operator: IntegerType. Supported data types are [MISSING, NULL, BOOLEAN, BLOB, STRING, NUMERIC, STRUCT, TUPLE, LIST]";

fn s(v: &str) -> AttributeValue {
    AttributeValue::S(v.to_string())
}

fn n(v: &str) -> AttributeValue {
    AttributeValue::N(v.to_string())
}

fn db() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(CreateTableRequest {
        table_name: "Things".to_string(),
        key_schema: vec![KeySchemaElement {
            attribute_name: "pk".to_string(),
            key_type: KeyType::HASH,
        }],
        attribute_definitions: vec![AttributeDefinition {
            attribute_name: "pk".to_string(),
            attribute_type: ScalarAttributeType::S,
        }],
        ..Default::default()
    })
    .unwrap();
    db.put_item(PutItemRequest {
        table_name: "Things".to_string(),
        item: HashMap::from([
            ("pk".to_string(), s("it1")),
            ("s".to_string(), s("str")),
            ("ss".to_string(), AttributeValue::SS(vec!["a".to_string()])),
            ("l".to_string(), AttributeValue::L(vec![s("x")])),
            (
                "m".to_string(),
                AttributeValue::M(HashMap::from([
                    ("name".to_string(), n("1")),
                    ("inner".to_string(), n("2")),
                ])),
            ),
        ]),
        ..Default::default()
    })
    .unwrap();
    db
}

fn run(
    db: &Database,
    statement: &str,
    parameters: Option<Vec<AttributeValue>>,
) -> Result<ExecuteStatementResponse, DynoxideError> {
    db.execute_statement(ExecuteStatementRequest {
        statement: statement.to_string(),
        parameters,
        ..Default::default()
    })
}

fn refusal(db: &Database, statement: &str) -> String {
    refusal_with(db, statement, None)
}

fn refusal_with(db: &Database, statement: &str, parameters: Option<Vec<AttributeValue>>) -> String {
    match run(db, statement, parameters) {
        Err(DynoxideError::ValidationException(m)) => m,
        other => panic!("{statement}: expected a ValidationException, got {other:?}"),
    }
}

/// `line:column:length` of the first `token` at or after `from`, as DynamoDB
/// reports a position: counted in characters from 1.
fn at(statement: &str, token: &str, len: usize, from: usize) -> String {
    let index = statement[from..]
        .find(token)
        .unwrap_or_else(|| panic!("{token:?} in {statement}"))
        + from;
    let before = &statement[..index];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap().chars().count() + 1;
    format!("{line}:{column}:{len}")
}

/// `at` for the first `token` after the first `after`.
fn at_after(statement: &str, after: &str, token: &str, len: usize) -> String {
    at(
        statement,
        token,
        len,
        statement.find(after).unwrap() + after.len(),
    )
}

fn set(value: &str) -> String {
    format!(r#"UPDATE "Things" SET q = {value} WHERE pk = 'it1'"#)
}

fn where_(condition: &str) -> String {
    format!(r#"SELECT * FROM "Things" WHERE pk = 'it1' AND {condition}"#)
}

#[test]
fn a_string_bag_is_refused_at_its_first_element_that_isnt_a_string() {
    let db = db();
    for (value, token, len) in [
        ("<<'a', 1>>", "1", 1),
        ("<<'a', true>>", "true", 4),
        ("<<'a', 1, true>>", "1", 1),
        ("<<'a', 'a', 1>>", "1", 1),
        ("<<'a', <<'b'>>>>", "<<", 2),
    ] {
        let statement = set(value);
        assert_eq!(
            refusal(&db, &statement),
            format!("{BAG} at {}", at_after(&statement, "<<'a'", token, len)),
            "{value}"
        );
    }
    for (value, token, len) in [
        ("<<true>>", "true", 4),
        ("<<true, 'a'>>", "true", 4),
        ("<<null>>", "null", 4),
        ("<<[1]>>", "[", 1),
        ("<<{'a': 1}>>", "{", 1),
    ] {
        let statement = set(value);
        assert_eq!(
            refusal(&db, &statement),
            format!("{BAG} at {}", at_after(&statement, "<<", token, len)),
            "{value}"
        );
    }
    for statement in [
        r#"INSERT INTO "Things" VALUE {'pk': 'b1', 'q': <<'a', 1>>}"#,
        r#"INSERT INTO "Things" VALUE {'pk': 'b2', 'm': {'x': <<'a', 1>>}}"#,
        r#"SELECT * FROM "Things" WHERE pk = 'it1' AND ss = <<'a', 1>>"#,
        r#"UPDATE "Things" SET ss = set_add(ss, <<'a', 1>>) WHERE pk = 'it1'"#,
        r#"UPDATE "Missing" SET q = <<'a', 1>> WHERE pk = 'it1'"#,
        "UPDATE \"Things\" SET q = <<'a',\n 1>> WHERE pk = 'it1'",
    ] {
        assert_eq!(
            refusal(&db, statement),
            format!("{BAG} at {}", at_after(statement, "<<'a',", "1", 1)),
            "{statement}"
        );
    }
}

#[test]
fn a_number_bag_is_refused_without_a_position() {
    let db = db();
    for value in [
        "<<1, 'a'>>",
        "<<1, 2, 'x', 'y'>>",
        "<<1, true>>",
        "<<1, [1]>>",
        "<<1, <<2>>>>",
        "<<1, 'a', true>>",
        "<<1.5, 'a'>>",
        "<<-1, 'a'>>",
    ] {
        assert_eq!(refusal(&db, &set(value)), BAG, "{value}");
    }
    for statement in [
        r#"INSERT INTO "Things" VALUE {'pk': 'b3', 'q': <<1, 'a'>>}"#,
        r#"SELECT * FROM "Things" WHERE pk = 'it1' AND ns = <<1, 'a'>>"#,
        r#"UPDATE "Missing" SET q = <<1, 'a'>> WHERE pk = 'it1'"#,
    ] {
        assert_eq!(refusal(&db, statement), BAG, "{statement}");
    }
    let numeric_null =
        "One or more parameter values were invalid: Numeric sets may not contain null under root";
    assert_eq!(refusal(&db, &set("<<1, null>>")), numeric_null);
    assert_eq!(
        refusal(
            &db,
            r#"INSERT INTO "Things" VALUE {'pk': 'b4', 'm': {'x': <<1, null>>}}"#
        ),
        format!("{numeric_null}.x")
    );
    assert_eq!(
        refusal(
            &db,
            r#"INSERT INTO "Things" VALUE {'pk': 'b5', 'l': [<<1, null>>]}"#
        ),
        format!("{numeric_null}[0]")
    );
}

#[test]
fn a_parameter_in_a_bag_is_refused() {
    let db = db();
    // As the first element, or the only one: the bag's refusal, at the `?`.
    for (value, parameters) in [
        ("<<?, 'a'>>", vec![n("1")]),
        ("<<?, 1>>", vec![s("a")]),
        ("<<?>>", vec![s("c")]),
    ] {
        let statement = set(value);
        assert_eq!(
            refusal_with(&db, &statement, Some(parameters)),
            format!("{BAG} at {}", at(&statement, "?", 1, 0)),
            "{value}"
        );
    }
    let statement = r#"INSERT INTO "Things" VALUE {'pk': 'p1', 's': <<?, ?>>}"#;
    assert_eq!(
        refusal_with(&db, statement, Some(vec![s("a"), s("b")])),
        format!("{BAG} at {}", at(statement, "?", 1, 0))
    );
    // After a string or a number: a parameter is a type DynamoDB can't store.
    for (statement, path) in [
        (set("<<'a', ?>>"), "root"),
        (set("<<1, ?>>"), "root"),
        (set("<<'a', 'b', ?>>"), "root"),
        (set("<<'a', ?, true>>"), "root"),
        (
            r#"INSERT INTO "Things" VALUE {'pk': 'p2', 'q': <<'a', ?>>}"#.to_string(),
            "root",
        ),
        (
            r#"INSERT INTO "Things" VALUE {'pk': 'p3', 'm': {'x': <<'a', ?>>}}"#.to_string(),
            "root.x",
        ),
        (where_("ss = <<'a', ?>>"), "root"),
    ] {
        assert_eq!(
            refusal_with(&db, &statement, Some(vec![s("b")])),
            format!(
                "Unsupported data type: Parameter under key {path} at {}",
                at(&statement, "?", 1, 0)
            ),
            "{statement}"
        );
    }
    // A bool ahead of the parameter is the first element that isn't a string.
    let statement = set("<<'a', true, ?>>");
    assert_eq!(
        refusal_with(&db, &statement, Some(vec![s("c")])),
        format!("{BAG} at {}", at(&statement, "true", 4, 0))
    );
}

#[test]
fn ion_literals_are_refused_by_type() {
    let db = db();
    for (literal, kind) in [
        ("`{{YWJj}}`", "BLOB"),
        ("`{{\"text\"}}`", "CLOB"),
        ("`1`", "INT"),
        ("`\"x\"`", "STRING"),
        ("`abc`", "SYMBOL"),
        ("`{a: 1}`", "STRUCT"),
        ("`[1]`", "LIST"),
        ("`2020-01-01T`", "TIMESTAMP"),
        ("`null`", "NULL"),
        ("`true`", "BOOL"),
        ("`1.5`", "DECIMAL"),
    ] {
        let statement = format!(r#"INSERT INTO "Things" VALUE {{'pk': 'i1', 's': {literal}}}"#);
        assert_eq!(
            refusal(&db, &statement),
            format!(
                "Ion Literals are not supported: {kind} under key root at {}",
                at(&statement, literal, literal.chars().count(), 0)
            ),
            "{literal}"
        );
    }
    let blob = "`{{YWJj}}`";
    for (statement, path) in [
        (set(blob), "root"),
        (
            format!(r#"INSERT INTO "Things" VALUE {{'pk': 'i2', 'm': {{'x': {blob}}}}}"#),
            "root.x",
        ),
        (
            format!(r#"INSERT INTO "Things" VALUE {{'pk': 'i3', 'l': [{blob}]}}"#),
            "root[0]",
        ),
        (where_(&format!("b = {blob}")), "root"),
        (
            format!(r#"INSERT INTO "Things" VALUE {{'pk': {blob}}}"#),
            "root",
        ),
        (set(&format!("<<'a', {blob}>>")), "root"),
        (set(&format!("<<1, {blob}>>")), "root"),
    ] {
        assert_eq!(
            refusal(&db, &statement),
            format!(
                "Ion Literals are not supported: BLOB under key {path} at {}",
                at(&statement, blob, 10, 0)
            ),
            "{statement}"
        );
    }
    // As a bag's first element, the bag's refusal.
    let statement =
        format!(r#"INSERT INTO "Things" VALUE {{'pk': 'i4', 's': <<{blob}, {blob}>>}}"#);
    assert_eq!(
        refusal(&db, &statement),
        format!("{BAG} at {}", at(&statement, blob, 10, 0))
    );
    // One that doesn't close, or doesn't read as Ion, ends the statement there.
    for statement in [
        r#"INSERT INTO "Things" VALUE {'pk': 'i5', 's': `{{YWJj}}}"#,
        r#"INSERT INTO "Things" VALUE {'pk': 'i6', 's': `{{{`}"#,
    ] {
        assert_eq!(refusal(&db, statement), NOT_WELL_FORMED, "{statement}");
    }
}

#[test]
fn x_quoted_hex_is_not_a_binary_literal() {
    let db = db();
    for (statement, message) in [
        (
            r#"INSERT INTO "Things" VALUE {'pk': 'x1', 's': <<X'616263', X'616263'>>}"#,
            "Expected RIGHT_DOUBLE_ANGLE_BRACKET",
        ),
        (
            r#"INSERT INTO "Things" VALUE {'pk': 'x2', 's': X'616263'}"#,
            "Expected RIGHT_CURLY",
        ),
        (
            r#"INSERT INTO "Things" VALUE {'pk': 'x3', 'l': [X'61']}"#,
            "Expected RIGHT_BRACKET",
        ),
        (
            r#"UPDATE "Things" SET q = X'616263' WHERE pk = 'it1'"#,
            "Unexpected token after expression",
        ),
        (
            r#"SELECT * FROM "Things" WHERE pk = 'it1' AND b = X'616263'"#,
            "Unexpected token after expression",
        ),
    ] {
        assert_eq!(
            refusal(&db, statement),
            format!("{NOT_WELL_FORMED}{message}"),
            "{statement}"
        );
    }
}

#[test]
fn a_keyword_cannot_be_a_dot_component() {
    let db = db();
    let refused = format!("{NOT_WELL_FORMED}Invalid path dot component");
    for word in [
        "inner",
        "value",
        "key",
        "date",
        "count",
        "size",
        "user",
        "first",
        "select",
        "from",
        "set",
        "order",
        "missing",
        "index",
        "list",
        "string",
        "time",
        "left",
        "join",
        "level",
        "position",
        "at",
        "by",
        "returning",
        "new",
        "old",
        "nothing",
        "offset",
        "INNER",
        "Inner",
    ] {
        assert_eq!(
            refusal(&db, &where_(&format!("m.{word} = 1"))),
            refused,
            "m.{word}"
        );
    }
    for word in [
        "name",
        "year",
        "status",
        "type",
        "data",
        "day",
        "hour",
        "minute",
        "month",
        "second",
        "both",
        "leading",
        "trailing",
        "every",
        "partition",
        "row",
    ] {
        run(&db, &where_(&format!("m.{word} = 1")), None)
            .unwrap_or_else(|e| panic!("m.{word}: {e:?}"));
    }
    run(&db, &where_(r#"m."inner" = 2"#), None).unwrap();
    for statement in [
        where_("attribute_exists(m.inner)"),
        where_("l[0].inner = 1"),
        r#"SELECT m.inner FROM "Things" WHERE pk = 'it1'"#.to_string(),
        r#"UPDATE "Things" SET m.inner = 1 WHERE pk = 'it1'"#.to_string(),
        r#"UPDATE "Things" REMOVE m.inner WHERE pk = 'it1'"#.to_string(),
        r#"UPDATE "Things" SET z = m.inner WHERE pk = 'it1'"#.to_string(),
        r#"UPDATE "Things" SET m.inner = set_add(m.inner, <<'y'>>) WHERE pk = 'it1'"#.to_string(),
        r#"SELECT * FROM "Missing" WHERE pk = 'it1' AND m.inner = 1"#.to_string(),
    ] {
        assert_eq!(refusal(&db, &statement), refused, "{statement}");
    }
    run(&db, r#"SELECT m.name FROM "Things" WHERE pk = 'it1'"#, None).unwrap();
    run(
        &db,
        r#"UPDATE "Things" SET m.name = 3 WHERE pk = 'it1'"#,
        None,
    )
    .unwrap();
}

#[test]
fn a_call_in_a_select_list_is_an_unexpected_path_component() {
    let db = db();
    for (projection, name) in [
        ("attribute_exists(ss)", "attribute_exists"),
        ("attribute_not_exists(ss)", "attribute_not_exists"),
        ("size(s)", "size"),
        ("foo(s)", "foo"),
        ("upper(s)", "upper"),
        ("EXISTS(ss)", "EXISTS"),
        ("count(*)", "count"),
        ("size (s)", "size"),
        ("size(m.sub)", "size"),
        ("pk, size(s)", "size"),
    ] {
        let statement = format!(r#"SELECT {projection} FROM "Things" WHERE pk = 'it1'"#);
        assert_eq!(
            refusal(&db, &statement),
            format!(
                "Unexpected path component at {}",
                at(&statement, name, name.len(), 0)
            ),
            "{projection}"
        );
    }
    let statement = r#"SELECT size(s) FROM "Missing" WHERE pk = 'it1'"#;
    assert_eq!(
        refusal(&db, statement),
        "Unexpected path component at 1:8:4"
    );
    let statement = "SELECT pk,\n  size(s) FROM \"Things\" WHERE pk = 'it1'";
    assert_eq!(
        refusal(&db, statement),
        "Unexpected path component at 2:3:4"
    );
    let refused = db.execute_transaction(ExecuteTransactionRequest {
        transact_statements: vec![ParameterizedStatement {
            statement: r#"SELECT size(s) FROM "Things" WHERE pk = 'it1'"#.to_string(),
            parameters: None,
            ..Default::default()
        }],
        ..Default::default()
    });
    match refused {
        Err(DynoxideError::ValidationException(m)) => assert_eq!(
            m,
            "Validation failed in TransactStatements[0]: Unexpected path component at 1:8:4"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_subquery_in_a_where_clause_is_an_unsupported_token() {
    let db = db();
    let sub = r#"SELECT * FROM "Things" WHERE pk = 'it1'"#;
    for statement in [
        where_(&format!("EXISTS({sub})")),
        where_(&format!("EXISTS ({sub})")),
        where_(&format!("exists({})", sub.to_lowercase())),
        where_(&format!("NOT EXISTS({sub})")),
        where_(&format!("EXISTS(({sub}))")),
        format!(r#"SELECT * FROM "Things" WHERE EXISTS({sub}) AND pk = 'it1'"#),
        format!(r#"UPDATE "Things" SET z = 6 WHERE pk = 'it1' AND EXISTS({sub})"#),
        format!(r#"DELETE FROM "Things" WHERE pk = 'it1' AND EXISTS({sub})"#),
        format!(r#"SELECT * FROM "Missing" WHERE pk = 'it1' AND EXISTS({sub})"#),
        where_(&format!("EXISTS({sub}) AND bogus(s)")),
        where_(&format!("attribute_exists('s') AND EXISTS({sub})")),
    ] {
        let select = statement.to_lowercase().rfind("select").unwrap();
        let expected = format!(
            "Unsupported token in expression: SELECT at {}",
            at(&statement, &statement[select..select + 6], 6, select)
        );
        assert_eq!(refusal(&db, &statement), expected, "{statement}");
    }
    let statement = where_(&format!("bogus(s) AND EXISTS({sub})"));
    assert_eq!(
        refusal(&db, &statement),
        format!(
            "Unrecognized function: bogus at {}",
            at(&statement, "bogus", 5, 0)
        )
    );
    assert_eq!(
        refusal(&db, &where_(&format!("EXISTS({sub}) AND pk ="))),
        format!("{NOT_WELL_FORMED}Missing right-hand side expression of infix operator")
    );
}

/// The refusal `statement` gets, named by the token it points at or its text.
fn assert_first(db: &Database, statement: &str, expected: &str) {
    assert_eq!(refusal(db, statement), expected, "{statement}");
}

#[test]
fn refusals_read_from_values_come_in_the_order_written() {
    let db = db();
    let is_int = |statement: &str| {
        let path = statement.find(" IS INT").unwrap();
        format!("{IS_INT} at {}", at(statement, "s", 1, path - 1))
    };
    let bogus = |statement: &str| {
        format!(
            "Unrecognized function: bogus at {}",
            at(statement, "bogus", 5, 0)
        )
    };

    let statement = where_("s IS INT AND bogus(s)");
    assert_first(&db, &statement, &is_int(&statement));
    let statement = where_("bogus(s) AND s IS INT");
    assert_first(&db, &statement, &bogus(&statement));
    let statement = where_("attribute_exists('s') AND s IS INT");
    assert_first(&db, &statement, &is_int(&statement));

    let statement = where_("ss = <<>> AND s IS INT");
    assert_first(&db, &statement, "Empty bags are not supported");
    let statement = where_("s IS INT AND ss = <<>>");
    assert_first(&db, &statement, &is_int(&statement));
    let dup =
        "One or more parameter values were invalid: Input collection [x, x] contains duplicates.";
    let statement = where_("ss = <<'x', 'x'>> AND s IS INT");
    assert_first(&db, &statement, dup);
    let statement = where_("s IS INT AND ss = <<'x', 'x'>>");
    assert_first(&db, &statement, &is_int(&statement));

    // Ahead of an operand refusal, whichever is written first.
    assert_first(
        &db,
        &where_("attribute_exists('s') AND ss = <<>>"),
        "Empty bags are not supported",
    );
    assert_first(
        &db,
        &where_("ss = <<>> AND attribute_exists('s')"),
        "Empty bags are not supported",
    );
    assert_first(
        &db,
        &where_("attribute_exists('s') AND ss = <<'x', 'x'>>"),
        dup,
    );

    let empty_key = "Path component cannot be an empty string";
    assert_first(&db, &where_("m = {'': 1} AND bogus(s)"), empty_key);
    let statement = where_("bogus(s) AND m = {'': 1}");
    assert_first(&db, &statement, &bogus(&statement));
    assert_first(
        &db,
        &where_("m = {'': 1} AND attribute_exists('s')"),
        empty_key,
    );
    assert_first(
        &db,
        &where_("attribute_exists('s') AND m = {'': 1}"),
        empty_key,
    );

    let deep = format!("{}1{}", "[".repeat(33), "]".repeat(33));
    let nesting = format!(
        "Nesting Levels have exceeded supported limits under root{}",
        "[0]".repeat(32)
    );
    assert_first(&db, &where_(&format!("l = {deep} AND bogus(s)")), &nesting);
    let statement = where_(&format!("bogus(s) AND l = {deep}"));
    assert_first(&db, &statement, &bogus(&statement));
    assert_first(
        &db,
        &where_(&format!("l = {deep} AND attribute_exists('s')")),
        &nesting,
    );
    assert_first(
        &db,
        &where_(&format!("attribute_exists('s') AND l = {deep}")),
        &nesting,
    );

    let statement = where_("ss = <<'a', 1>> AND s IS INT");
    assert_first(
        &db,
        &statement,
        &format!("{BAG} at {}", at_after(&statement, "<<'a',", "1", 1)),
    );
    let statement = where_("s IS INT AND ss = <<'a', 1>>");
    assert_first(&db, &statement, &is_int(&statement));
    assert_first(&db, &where_("ns = <<1, 'a'>> AND s IS INT"), BAG);
    let statement = where_("s IS INT AND ns = <<1, 'a'>>");
    assert_first(&db, &statement, &is_int(&statement));
    assert_first(&db, &where_("ns = <<1, 'a'>> AND bogus(s)"), BAG);
    assert_first(
        &db,
        &where_("ns = <<1, 'a'>> AND attribute_exists('s')"),
        BAG,
    );
    assert_first(
        &db,
        &where_("attribute_exists('s') AND ns = <<1, 'a'>>"),
        BAG,
    );
    let statement = where_("bogus(s) AND ss = <<'a', 1>>");
    assert_first(&db, &statement, &bogus(&statement));

    // Operand refusals come after.
    let statement = where_("attribute_type(s, 'X') AND s IS INT");
    assert_first(&db, &statement, &is_int(&statement));
    let statement = where_("s IS INT AND attribute_type(s, 'X')");
    assert_first(&db, &statement, &is_int(&statement));
    let statement = where_("size() = 1 AND s IS INT");
    assert_first(&db, &statement, &is_int(&statement));

    // A syntax error comes before all of them.
    assert_first(
        &db,
        &where_("m.inner = 1 AND s IS INT"),
        &format!("{NOT_WELL_FORMED}Invalid path dot component"),
    );
    assert_first(
        &db,
        &where_("s IS INT AND m.inner = 1"),
        &format!("{NOT_WELL_FORMED}Invalid path dot component"),
    );
    assert_first(
        &db,
        &where_(r#"m.inner = 1 AND EXISTS(SELECT * FROM "Things")"#),
        &format!("{NOT_WELL_FORMED}Invalid path dot component"),
    );
}

#[test]
fn an_update_reports_its_where_clause_before_its_set_clause() {
    let db = db();
    let statement = r#"UPDATE "Things" SET a = <<>> WHERE pk = 'it1' AND bogus(s)"#;
    assert_first(
        &db,
        statement,
        &format!(
            "Unrecognized function: bogus at {}",
            at(statement, "bogus", 5, 0)
        ),
    );
    let statement = r#"UPDATE "Things" SET a = <<'a', 1>> WHERE pk = 'it1' AND s IS INT"#;
    let path = statement.find(" IS INT").unwrap();
    assert_first(
        &db,
        statement,
        &format!("{IS_INT} at {}", at(statement, "s", 1, path - 1)),
    );
    assert_first(
        &db,
        r#"UPDATE "Things" SET a = set_add(b, <<'x'>>) WHERE pk = 'it1' AND ss = <<>>"#,
        "Empty bags are not supported",
    );
    let statement = r#"UPDATE "Things" SET q = <<'a', ?>>, r = <<'a', 1>> WHERE pk = 'it1'"#;
    assert_eq!(
        refusal_with(&db, statement, Some(vec![s("b")])),
        format!(
            "Unsupported data type: Parameter under key root at {}",
            at(statement, "?", 1, 0)
        )
    );
    let statement = r#"UPDATE "Things" SET q = <<'a', 1>>, r = <<'a', ?>> WHERE pk = 'it1'"#;
    assert_eq!(
        refusal_with(&db, statement, Some(vec![s("b")])),
        format!("{BAG} at {}", at_after(statement, "<<'a',", "1", 1))
    );
}

#[test]
fn ion_and_bag_parameters_take_their_place_in_the_order() {
    let db = db();
    let blob = "`{{YWJj}}`";
    let statement = where_(&format!("b = {blob} AND bogus(s)"));
    assert_first(
        &db,
        &statement,
        &format!(
            "Ion Literals are not supported: BLOB under key root at {}",
            at(&statement, blob, 10, 0)
        ),
    );
    let statement = where_(&format!("bogus(s) AND b = {blob}"));
    assert_first(
        &db,
        &statement,
        &format!(
            "Unrecognized function: bogus at {}",
            at(&statement, "bogus", 5, 0)
        ),
    );
    let statement = where_(&format!("b = {blob} AND attribute_exists('s')"));
    assert_first(
        &db,
        &statement,
        &format!(
            "Ion Literals are not supported: BLOB under key root at {}",
            at(&statement, blob, 10, 0)
        ),
    );
    let statement = where_("ss = <<'a', ?>> AND s IS INT");
    assert_eq!(
        refusal_with(&db, &statement, Some(vec![s("b")])),
        format!(
            "Unsupported data type: Parameter under key root at {}",
            at(&statement, "?", 1, 0)
        )
    );
    let statement = where_("s IS INT AND ss = <<'a', ?>>");
    let path = statement.find(" IS INT").unwrap();
    assert_eq!(
        refusal_with(&db, &statement, Some(vec![s("b")])),
        format!("{IS_INT} at {}", at(&statement, "s", 1, path - 1))
    );
}
