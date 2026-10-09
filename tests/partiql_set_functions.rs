//! PartiQL's `set_add` and `set_delete`.
//!
//! `UPDATE ... SET a = set_add(a, <<...>>)` adds members to a set and
//! `set_delete` removes them, in any case. Adding to a missing attribute
//! creates the set; removing from one leaves it missing, and removing every
//! member removes the attribute. The call has to be the whole of the value: its
//! first argument the path it assigns to and its second a set. DynamoDB refuses
//! a malformed call before it looks the table up, naming where it stands, and
//! refuses a member of the wrong type, or a target that isn't a set, as an
//! operand of the wrong type. `<<>>` is refused wherever it is written, and a
//! call in an INSERT value is an unsupported data type (captured eu-west-2,
//! us-east-1, eu-west-1 and us-west-2, 2026-10-09).

use dynoxide::Database;
use dynoxide::actions::batch_execute_statement::{
    BatchExecuteStatementRequest, BatchStatementRequest,
};
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::{ExecuteStatementRequest, ExecuteStatementResponse};
use dynoxide::actions::execute_transaction::{ExecuteTransactionRequest, ParameterizedStatement};
use dynoxide::actions::get_item::GetItemRequest;
use dynoxide::actions::put_item::PutItemRequest;
use dynoxide::errors::DynoxideError;
use dynoxide::types::{
    AttributeDefinition, AttributeValue, Item, KeySchemaElement, KeyType, ScalarAttributeType,
};
use std::collections::HashMap;

fn s(v: &str) -> AttributeValue {
    AttributeValue::S(v.to_string())
}

fn ss(members: &[&str]) -> AttributeValue {
    AttributeValue::SS(members.iter().map(|m| m.to_string()).collect())
}

fn ns(members: &[&str]) -> AttributeValue {
    AttributeValue::NS(members.iter().map(|m| m.to_string()).collect())
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
    let item: Item = HashMap::from([
        ("pk".to_string(), s("s0")),
        ("ss".to_string(), ss(&["a", "b", "c"])),
        ("ns".to_string(), ns(&["1", "2", "3"])),
        (
            "bs".to_string(),
            AttributeValue::BS(vec![vec![1u8], vec![2u8]]),
        ),
        ("l".to_string(), AttributeValue::L(vec![s("x")])),
        ("s".to_string(), s("str")),
        (
            "m".to_string(),
            AttributeValue::M(HashMap::from([("sub".to_string(), ss(&["x"]))])),
        ),
    ]);
    db.put_item(PutItemRequest {
        table_name: "Things".to_string(),
        item,
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

fn refusal(db: &Database, statement: &str, parameters: Option<Vec<AttributeValue>>) -> String {
    match run(db, statement, parameters) {
        Err(DynoxideError::ValidationException(m)) => m,
        other => panic!("{statement}: expected a ValidationException, got {other:?}"),
    }
}

fn item(db: &Database) -> Item {
    db.get_item(GetItemRequest {
        table_name: "Things".to_string(),
        key: HashMap::from([("pk".to_string(), s("s0"))]),
        ..Default::default()
    })
    .unwrap()
    .item
    .unwrap()
}

/// A set attribute's members, sorted, or `missing`.
fn members(db: &Database, path: &[&str]) -> String {
    let mut value = item(db).get(path[0]).cloned();
    for step in &path[1..] {
        value = match value {
            Some(AttributeValue::M(map)) => map.get(*step).cloned(),
            _ => None,
        };
    }
    let mut out: Vec<String> = match value {
        Some(AttributeValue::SS(m)) | Some(AttributeValue::NS(m)) => m,
        Some(AttributeValue::BS(m)) => m.iter().map(|b| format!("{b:?}")).collect(),
        None => return "missing".to_string(),
        Some(other) => return format!("{other:?}"),
    };
    out.sort();
    out.join(",")
}

#[test]
fn set_add_and_set_delete_change_a_set() {
    for (statement, parameters, path, expected) in [
        (
            "UPDATE \"Things\" SET ss = set_add(ss, <<'0', 'z'>>) WHERE pk = 's0'",
            None,
            vec!["ss"],
            "0,a,b,c,z",
        ),
        (
            "UPDATE \"Things\" SET ns = set_add(ns, <<4, 1>>) WHERE pk = 's0'",
            None,
            vec!["ns"],
            "1,2,3,4",
        ),
        (
            "UPDATE \"Things\" SET ns = set_add(ns, <<4.0, 5>>) WHERE pk = 's0'",
            None,
            vec!["ns"],
            "1,2,3,4,5",
        ),
        (
            "UPDATE \"Things\" SET ss = set_add(ss, ?) WHERE pk = 's0'",
            Some(vec![ss(&["q"])]),
            vec!["ss"],
            "a,b,c,q",
        ),
        (
            "UPDATE \"Things\" SET bs = set_add(bs, ?) WHERE pk = 's0'",
            Some(vec![AttributeValue::BS(vec![vec![1u8], vec![3u8]])]),
            vec!["bs"],
            "[1],[2],[3]",
        ),
        (
            "UPDATE \"Things\" SET ss = set_add(ss, <<'a'>>) WHERE pk = 's0'",
            None,
            vec!["ss"],
            "a,b,c",
        ),
        (
            "UPDATE \"Things\" SET nss = set_add(nss, <<'x'>>) WHERE pk = 's0'",
            None,
            vec!["nss"],
            "x",
        ),
        (
            "UPDATE \"Things\" SET ss = SET_ADD(ss, <<'x'>>) WHERE pk = 's0'",
            None,
            vec!["ss"],
            "a,b,c,x",
        ),
        (
            "UPDATE \"Things\" SET ss = set_add (ss, <<'z'>>) WHERE pk = 's0'",
            None,
            vec!["ss"],
            "a,b,c,z",
        ),
        (
            "UPDATE \"Things\" SET m.sub = set_add(m.sub, <<'y'>>) WHERE pk = 's0'",
            None,
            vec!["m", "sub"],
            "x,y",
        ),
        (
            "UPDATE \"Things\" SET ss = set_delete(ss, <<'a'>>) WHERE pk = 's0'",
            None,
            vec!["ss"],
            "b,c",
        ),
        (
            "UPDATE \"Things\" SET ss = set_delete(ss, <<'a', 'b', 'c'>>) WHERE pk = 's0'",
            None,
            vec!["ss"],
            "missing",
        ),
        (
            "UPDATE \"Things\" SET ss = set_delete(ss, <<'zz'>>) WHERE pk = 's0'",
            None,
            vec!["ss"],
            "a,b,c",
        ),
        (
            "UPDATE \"Things\" SET nss = set_delete(nss, <<'a'>>) WHERE pk = 's0'",
            None,
            vec!["nss"],
            "missing",
        ),
        (
            "UPDATE \"Things\" SET ns = set_delete(ns, <<2>>) WHERE pk = 's0'",
            None,
            vec!["ns"],
            "1,3",
        ),
        (
            "UPDATE \"Things\" SET ns = set_delete(ns, <<2.0>>) WHERE pk = 's0'",
            None,
            vec!["ns"],
            "1,3",
        ),
    ] {
        let db = db();
        let response =
            run(&db, statement, parameters).unwrap_or_else(|e| panic!("{statement}: {e}"));
        assert_eq!(response.items, Some(vec![]), "{statement}");
        assert_eq!(members(&db, &path), expected, "{statement}");
    }
}

#[test]
fn two_set_clauses_each_take_a_call() {
    let db = db();
    run(
        &db,
        "UPDATE \"Things\" SET ss = set_add(ss, <<'x'>>) SET ns = set_add(ns, <<9>>) \
         WHERE pk = 's0'",
        None,
    )
    .unwrap();
    assert_eq!(members(&db, &["ss"]), "a,b,c,x");
    assert_eq!(members(&db, &["ns"]), "1,2,3,9");
}

#[test]
fn returning_shows_the_set_after_the_call() {
    let db = db();
    let response = run(
        &db,
        "UPDATE \"Things\" SET ss = set_add(ss, <<'r'>>) WHERE pk = 's0' RETURNING ALL NEW *",
        None,
    )
    .unwrap();
    let items = response.items.unwrap();
    let mut returned = match items[0].get("ss") {
        Some(AttributeValue::SS(m)) => m.clone(),
        other => panic!("{other:?}"),
    };
    returned.sort();
    assert_eq!(returned, vec!["a", "b", "c", "r"]);
}

#[test]
fn a_malformed_call_is_refused_where_it_stands() {
    let db = db();
    for (statement, parameters, expected) in [
        (
            "UPDATE \"Things\" SET t = set_add(ss, <<'x'>>) WHERE pk = 's0'",
            None,
            "The first argument to SET_ADD must equal the assignment value at 1:25:7",
        ),
        (
            "UPDATE \"Things\" SET ss = set_add(<<'a'>>, <<'b'>>) WHERE pk = 's0'",
            None,
            "The first argument to SET_ADD must equal the assignment value at 1:26:7",
        ),
        (
            "UPDATE \"Things\" SET t = set_add(ss, 'x') WHERE pk = 's0'",
            None,
            "The first argument to SET_ADD must equal the assignment value at 1:25:7",
        ),
        (
            "UPDATE \"Things\" SET ss = set_add(ss, 'x') WHERE pk = 's0'",
            None,
            "The second argument to SET_ADD must be a value with type SET at 1:34:2",
        ),
        (
            "UPDATE \"Things\" SET ss = set_add(ss, ['x']) WHERE pk = 's0'",
            None,
            "The second argument to SET_ADD must be a value with type SET at 1:34:2",
        ),
        (
            "UPDATE \"Things\" SET ss = SET_ADD(ss, 'x') WHERE pk = 's0'",
            None,
            "The second argument to SET_ADD must be a value with type SET at 1:34:2",
        ),
        (
            "UPDATE \"Things\" SET ss = set_add(ss, ?) WHERE pk = 's0'",
            Some(vec![s("x")]),
            "The second argument to SET_ADD must be a value with type SET at 1:34:2",
        ),
        (
            "UPDATE \"Things\" SET ss = set_add(ss, ?) WHERE pk = 's0'",
            Some(vec![AttributeValue::L(vec![s("x")])]),
            "The second argument to SET_ADD must be a value with type SET at 1:34:2",
        ),
        (
            "UPDATE \"Things\" SET ss = set_add(ss) WHERE pk = 's0'",
            None,
            "SET_ADD must have exactly two arguments at 1:26:7",
        ),
        (
            "UPDATE \"Things\" SET ss = set_add(ss, <<'x'>>, 'y') WHERE pk = 's0'",
            None,
            "SET_ADD must have exactly two arguments at 1:26:7",
        ),
        (
            "UPDATE \"Things\" SET ss = set_add('x') WHERE pk = 's0'",
            None,
            "SET_ADD must have exactly two arguments at 1:26:7",
        ),
        (
            "UPDATE \"Things\" SET ss = set_delete(ss) WHERE pk = 's0'",
            None,
            "SET_DELETE must have exactly two arguments at 1:26:10",
        ),
        (
            "UPDATE \"Things\" SET ss = set_delete('a', <<'a'>>) WHERE pk = 's0'",
            None,
            "The first argument to SET_DELETE must equal the assignment value at 1:26:10",
        ),
        (
            "UPDATE \"Things\" SET ss = set_delete(ss, 'a') WHERE pk = 's0'",
            None,
            "The second argument to SET_DELETE must be a value with type SET at 1:37:2",
        ),
        (
            "UPDATE \"Things\" SET ss = set_add(ss, <<'z'>>) + 1 WHERE pk = 's0'",
            None,
            "Unrecognized function: set_add at 1:26:7",
        ),
        // The table is looked up after the call is judged.
        (
            "UPDATE \"Missing\" SET t = set_add(ss, <<'x'>>) WHERE pk = 'a'",
            None,
            "The first argument to SET_ADD must equal the assignment value at 1:26:7",
        ),
    ] {
        assert_eq!(refusal(&db, statement, parameters), expected, "{statement}");
    }
    assert_eq!(members(&db, &["ss"]), "a,b,c");
}

#[test]
fn a_member_or_target_of_the_wrong_type_is_an_operand_of_the_wrong_type() {
    let db = db();
    for (statement, parameters) in [
        (
            "UPDATE \"Things\" SET ss = set_add(ss, <<1>>) WHERE pk = 's0'",
            None,
        ),
        (
            "UPDATE \"Things\" SET ss = set_delete(ss, <<1>>) WHERE pk = 's0'",
            None,
        ),
        (
            "UPDATE \"Things\" SET ss = set_add(ss, ?) WHERE pk = 's0'",
            Some(vec![ns(&["1"])]),
        ),
        (
            "UPDATE \"Things\" SET s = set_add(s, <<'x'>>) WHERE pk = 's0'",
            None,
        ),
        (
            "UPDATE \"Things\" SET l = set_add(l, <<'x'>>) WHERE pk = 's0'",
            None,
        ),
    ] {
        assert_eq!(
            refusal(&db, statement, parameters),
            "An operand in the update expression has an incorrect data type",
            "{statement}"
        );
    }
    assert_eq!(members(&db, &["ss"]), "a,b,c");
}

#[test]
fn duplicate_and_empty_members_are_refused() {
    let db = db();
    for (statement, expected) in [
        (
            "UPDATE \"Things\" SET ss = set_add(ss, <<'x', 'x'>>) WHERE pk = 's0'",
            "One or more parameter values were invalid: Input collection [x, x] contains \
             duplicates.",
        ),
        (
            "UPDATE \"Things\" SET ss = set_delete(ss, <<'a', 'a'>>) WHERE pk = 's0'",
            "One or more parameter values were invalid: Input collection [a, a] contains \
             duplicates.",
        ),
        (
            "UPDATE \"Things\" SET ss = set_add(ss, <<>>) WHERE pk = 's0'",
            "Empty bags are not supported",
        ),
        (
            "UPDATE \"Things\" SET ss = set_delete(ss, <<>>) WHERE pk = 's0'",
            "Empty bags are not supported",
        ),
        (
            "UPDATE \"Things\" SET ss = <<>> WHERE pk = 's0'",
            "Empty bags are not supported",
        ),
        (
            "INSERT INTO \"Things\" VALUE {'pk': 'e1', 's': <<>>}",
            "Empty bags are not supported",
        ),
    ] {
        assert_eq!(refusal(&db, statement, None), expected, "{statement}");
    }
    assert_eq!(members(&db, &["ss"]), "a,b,c");
}

#[test]
fn a_call_outside_an_update_assignment_is_refused() {
    let db = db();
    assert_eq!(
        refusal(
            &db,
            "SELECT * FROM \"Things\" WHERE pk = 's0' AND set_delete(ss, <<'a'>>) = ss",
            None
        ),
        "Unrecognized function: set_delete at 1:44:10"
    );
    assert_eq!(
        refusal(
            &db,
            "INSERT INTO \"Things\" VALUE {'pk': 'e2', 'ss': set_delete(<<'a'>>, <<'b'>>)}",
            None
        ),
        "Unsupported data type: set_delete under key root at 1:47:10"
    );
    assert_eq!(
        refusal(
            &db,
            "INSERT INTO \"Things\" VALUE {'pk': 'e3', 'm': {'x': set_add(<<'a'>>, <<'b'>>)}}",
            None
        ),
        "Unsupported data type: Call under key root.x at 1:52:7"
    );
}

#[test]
fn a_missing_item_fails_the_update() {
    let db = db();
    match run(
        &db,
        "UPDATE \"Things\" SET ss = set_add(ss, <<'x'>>) WHERE pk = 'nope'",
        None,
    ) {
        Err(DynoxideError::ConditionalCheckFailedException(m, _)) => {
            assert_eq!(m, "The conditional request failed")
        }
        other => panic!("expected a failed condition, got {other:?}"),
    }
}

#[test]
fn transactions_and_batches_run_a_call() {
    let db = db();
    db.execute_transaction(ExecuteTransactionRequest {
        transact_statements: vec![ParameterizedStatement {
            statement: "UPDATE \"Things\" SET ss = set_add(ss, <<'t'>>) WHERE pk = 's0'"
                .to_string(),
            parameters: None,
        }],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(members(&db, &["ss"]), "a,b,c,t");

    let mut member = BatchStatementRequest::default();
    member.statement =
        "UPDATE \"Things\" SET ss = set_delete(ss, <<'t'>>) WHERE pk = 's0'".to_string();
    let response = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            statements: vec![member],
            ..Default::default()
        })
        .unwrap();
    assert!(
        response.responses[0].error.is_none(),
        "{:?}",
        response.responses[0]
    );
    assert_eq!(members(&db, &["ss"]), "a,b,c");
}
