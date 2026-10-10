//! A quoted name in a PartiQL statement is one attribute, whatever it holds.
//! `"a.b"` and `"a[0]"` name the top-level attributes `a.b` and `a[0]` in a
//! WHERE clause, in a condition function, in `IS NOT MISSING`, in a
//! projection and as a SET target, while `a.b`, `"a"."b"`, `a[0]` and
//! `"a"[0]` step into `a`. Captured eu-west-2 and us-east-1, 2026-10-10
//! (cases 7); the regions agree on every row.

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::ExecuteStatementRequest;
use dynoxide::actions::get_item::GetItemRequest;
use dynoxide::actions::put_item::PutItemRequest;
use serde_json::{Value, json};

fn setup() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value::<CreateTableRequest>(json!({
            "TableName": "Names",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    for item in [
        json!({"pk": {"S": "q1"}, "a.b": {"N": "1"}}),
        json!({"pk": {"S": "q2"}, "a": {"M": {"b": {"N": "1"}}}}),
        json!({"pk": {"S": "q3"}, "a[0]": {"N": "1"}}),
        json!({"pk": {"S": "q4"}, "a": {"L": [{"N": "1"}]}}),
        json!({"pk": {"S": "q5"}}),
    ] {
        put(&db, item);
    }
    db
}

fn put(db: &Database, item: Value) {
    db.put_item(
        serde_json::from_value::<PutItemRequest>(json!({"TableName": "Names", "Item": item}))
            .unwrap(),
    )
    .unwrap();
}

/// The items a statement returns, as wire JSON, or the error's message.
fn run(db: &Database, statement: &str) -> Result<Vec<Value>, String> {
    let request: ExecuteStatementRequest =
        serde_json::from_value(json!({"Statement": statement})).unwrap();
    db.execute_statement(request)
        .map(|response| {
            response
                .items
                .unwrap_or_default()
                .into_iter()
                .map(|item| serde_json::to_value(item).unwrap())
                .collect()
        })
        .map_err(|e| e.to_string())
}

/// The `pk` of each item a SELECT returns, sorted.
fn keys(db: &Database, condition: &str) -> Vec<String> {
    let statement = format!(r#"SELECT pk FROM "Names" WHERE {condition}"#);
    let mut keys: Vec<String> = run(db, &statement)
        .unwrap_or_else(|e| panic!("{statement}: {e}"))
        .iter()
        .map(|item| item["pk"]["S"].as_str().unwrap().to_string())
        .collect();
    keys.sort();
    keys
}

fn stored(db: &Database, pk: &str) -> Value {
    let request: GetItemRequest = serde_json::from_value(json!({
        "TableName": "Names", "Key": {"pk": {"S": pk}}
    }))
    .unwrap();
    serde_json::to_value(db.get_item(request).unwrap().item.unwrap()).unwrap()
}

#[test]
fn a_quoted_name_with_a_dot_or_a_bracket_is_one_attribute_in_a_where_clause() {
    let db = setup();
    for (condition, expected) in [
        (r#""a.b" = 1"#, "q1"),
        (r#""a[0]" = 1"#, "q3"),
        ("a.b = 1", "q2"),
        ("a[0] = 1", "q4"),
        (r#""a"."b" = 1"#, "q2"),
        (r#""a"[0] = 1"#, "q4"),
        (r#"attribute_exists("a.b")"#, "q1"),
        (r#""a.b" IS NOT MISSING"#, "q1"),
    ] {
        assert_eq!(keys(&db, condition), vec![expected], "{condition}");
    }
}

#[test]
fn a_quoted_name_with_a_dot_or_a_bracket_is_one_attribute_in_a_projection() {
    let db = setup();
    for (projection, pk, expected) in [
        (r#""a.b""#, "q1", json!([{"a.b": {"N": "1"}}])),
        (r#""a.b""#, "q2", json!([{}])),
        (r#""a[0]""#, "q3", json!([{"a[0]": {"N": "1"}}])),
        (r#""a[0]""#, "q4", json!([{}])),
    ] {
        let statement = format!(r#"SELECT {projection} FROM "Names" WHERE pk = '{pk}'"#);
        assert_eq!(
            run(&db, &statement),
            Ok(expected.as_array().unwrap().clone()),
            "{statement}"
        );
    }
}

#[test]
fn a_quoted_name_with_a_dot_or_a_bracket_is_one_attribute_in_an_update() {
    let db = setup();
    for statement in [
        r#"UPDATE "Names" SET "x.y" = 5 WHERE pk = 'q5'"#,
        r#"UPDATE "Names" SET "x[0]" = 6 WHERE pk = 'q5'"#,
        r#"UPDATE "Names" REMOVE "zz.y" WHERE pk = 'q5'"#,
    ] {
        assert_eq!(run(&db, statement), Ok(vec![]), "{statement}");
    }
    assert_eq!(
        stored(&db, "q5"),
        json!({"pk": {"S": "q5"}, "x.y": {"N": "5"}, "x[0]": {"N": "6"}})
    );
    assert_eq!(
        run(
            &db,
            r#"UPDATE "Names" SET w = 1 WHERE pk = 'q1' AND "a.b" = 1"#
        ),
        Ok(vec![])
    );
    assert_eq!(
        run(
            &db,
            r#"UPDATE "Names" SET w = 1 WHERE pk = 'q2' AND "a.b" = 1"#
        ),
        Err("The conditional request failed".to_string())
    );
}

/// An item holding both a top-level `a.b` and an `a` with a `b` inside it
/// keeps the two apart, in a WHERE clause, a projection and one SET writing
/// both (captured eu-west-2 and us-east-1, 2026-10-10).
#[test]
fn a_top_level_name_with_a_dot_and_a_nested_path_are_told_apart() {
    let db = setup();
    put(
        &db,
        json!({"pk": {"S": "both"}, "a.b": {"N": "1"}, "a": {"M": {"b": {"N": "2"}}}}),
    );
    assert_eq!(keys(&db, r#"pk = 'both' AND "a.b" = 1"#), vec!["both"]);
    assert_eq!(keys(&db, "pk = 'both' AND a.b = 2"), vec!["both"]);
    assert_eq!(
        keys(&db, r#"pk = 'both' AND "a.b" = 2"#),
        Vec::<String>::new()
    );
    assert_eq!(
        run(&db, r#"SELECT "a.b", a.b FROM "Names" WHERE pk = 'both'"#),
        Ok(vec![json!({"a.b": {"N": "1"}, "b": {"N": "2"}})])
    );
    run(
        &db,
        r#"UPDATE "Names" SET "a.b" = 3, a.b = 4 WHERE pk = 'both'"#,
    )
    .unwrap();
    assert_eq!(
        stored(&db, "both"),
        json!({"pk": {"S": "both"}, "a.b": {"N": "3"}, "a": {"M": {"b": {"N": "4"}}}})
    );
}

/// A key attribute whose name holds a dot or a bracket is named by quoting
/// it, and the statement is then a key read, ordered by its sort key, or
/// names the item to write, as any other key is (captured eu-west-2 and
/// us-east-1, 2026-10-10).
#[test]
fn a_key_attribute_whose_name_holds_a_dot_is_named_by_quoting_it() {
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value::<CreateTableRequest>(json!({
            "TableName": "Dotted",
            "KeySchema": [
                {"AttributeName": "k.1", "KeyType": "HASH"},
                {"AttributeName": "s[0]", "KeyType": "RANGE"}
            ],
            "AttributeDefinitions": [
                {"AttributeName": "k.1", "AttributeType": "S"},
                {"AttributeName": "s[0]", "AttributeType": "N"}
            ],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    for sk in ["1", "2"] {
        db.put_item(
            serde_json::from_value::<PutItemRequest>(json!({
                "TableName": "Dotted",
                "Item": {"k.1": {"S": "x"}, "s[0]": {"N": sk}, "v": {"N": "0"}}
            }))
            .unwrap(),
        )
        .unwrap();
    }
    let sort_keys = |statement: &str| -> Vec<String> {
        run(&db, statement)
            .unwrap_or_else(|e| panic!("{statement}: {e}"))
            .iter()
            .map(|item| item["s[0]"]["N"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        sort_keys(r#"SELECT * FROM "Dotted" WHERE "k.1" = 'x' ORDER BY "s[0]" DESC"#),
        vec!["2", "1"]
    );
    assert_eq!(
        sort_keys(r#"SELECT * FROM "Dotted" WHERE "k.1" = 'x' AND "s[0]" > 1"#),
        vec!["2"]
    );
    assert_eq!(
        run(
            &db,
            r#"UPDATE "Dotted" SET v = 5 WHERE "k.1" = 'x' AND "s[0]" = 1"#
        ),
        Ok(vec![])
    );
    assert_eq!(
        run(
            &db,
            r#"SELECT v FROM "Dotted" WHERE "k.1" = 'x' AND "s[0]" = 1"#
        ),
        Ok(vec![json!({"v": {"N": "5"}})])
    );
    assert_eq!(
        run(
            &db,
            r#"DELETE FROM "Dotted" WHERE "k.1" = 'x' AND "s[0]" = 2"#
        ),
        Ok(vec![])
    );
    assert_eq!(
        sort_keys(r#"SELECT * FROM "Dotted" WHERE "k.1" = 'x'"#),
        vec!["1"]
    );
}
