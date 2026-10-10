//! `ReturnValuesOnConditionCheckFailure` on the three PartiQL operations, and
//! the reasons `ExecuteTransaction` gives when it cancels.
//!
//! With `ALL_OLD`, an UPDATE or DELETE whose WHERE clause is false on an item
//! that exists hands back the whole item: on that statement's cancellation
//! reason, on a batch member's `Error`, or on `ExecuteStatement`'s
//! `ConditionalCheckFailedException`. Nothing comes back for `NONE`, for a
//! missing item, for a duplicate INSERT, or for a statement without a
//! condition. A transaction lists every statement that failed, and a DELETE
//! with a condition on an item that isn't there fails only when something
//! else cancels the transaction.
//!
//! Every expected answer was captured against DynamoDB in eu-west-2 and
//! us-east-1 on 10 October 2026, the two regions identical; the case labels
//! (R1, B1, S1, X5, Y2, ...) are the capture's.

#![cfg(feature = "http-server")]

use dynoxide::Database;
use serde_json::{Value, json};

const T: &str = "Tbl";

async fn start() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let db = Database::memory().unwrap();
    tokio::spawn(async move { dynoxide::server::serve_on(listener, db).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    url
}

async fn ddb(url: &str, op: &str, body: Value) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(url)
        .header("x-amz-target", format!("DynamoDB_20120810.{op}"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=fakekey/20260101/us-east-1/dynamodb/aws4_request, \
             SignedHeaders=host;x-amz-date;x-amz-target, Signature=fakesig",
        )
        .header("x-amz-date", "20260101T000000Z")
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn ok(url: &str, op: &str, body: Value) -> Value {
    let (status, out) = ddb(url, op, body).await;
    assert_eq!(status, 200, "{op} failed: {out}");
    out
}

/// The capture's table: `t1`, `t2` (every attribute type, its sets out of
/// order) and `t3`, `t4`.
async fn seeded() -> String {
    let url = start().await;
    ok(
        &url,
        "CreateTable",
        json!({
            "TableName": T,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        }),
    )
    .await;
    let rich = json!({
        "pk": {"S": "t2"}, "v": {"N": "2"}, "s": {"S": "two"}, "ss": {"SS": ["b", "a"]},
        "ns": {"NS": ["3", "1", "2"]}, "bin": {"B": "AQID"},
        "m": {"M": {"k": {"S": "x"}, "n": {"N": "1"}}}, "l": {"L": [{"N": "1"}, {"S": "y"}]},
        "nul": {"NULL": true}, "bo": {"BOOL": true}
    });
    for item in [
        json!({"pk": {"S": "t1"}, "v": {"N": "1"}}),
        rich,
        json!({"pk": {"S": "t3"}, "v": {"N": "3"}}),
        json!({"pk": {"S": "t4"}, "v": {"N": "4"}}),
    ] {
        ok(&url, "PutItem", json!({"TableName": T, "Item": item})).await;
    }
    url
}

/// `t2` as DynamoDB returns it from a PartiQL operation: number sets in hash
/// order, string sets by their bytes.
fn t2() -> Value {
    json!({
        "ss": {"SS": ["a", "b"]}, "s": {"S": "two"}, "v": {"N": "2"},
        "ns": {"NS": ["3", "2", "1"]}, "nul": {"NULL": true}, "bin": {"B": "AQID"},
        "l": {"L": [{"N": "1"}, {"S": "y"}]}, "m": {"M": {"k": {"S": "x"}, "n": {"N": "1"}}},
        "pk": {"S": "t2"}, "bo": {"BOOL": true}
    })
}

fn t1() -> Value {
    json!({"pk": {"S": "t1"}, "v": {"N": "1"}})
}

const ALL_OLD: &str = "ALL_OLD";

fn stmt(text: &str, mode: Option<&str>) -> Value {
    // ` T ` in the text stands for the quoted table name.
    let mut s = json!({"Statement": text.replace(" T ", &format!(" \"{T}\" "))});
    if let Some(mode) = mode {
        s["ReturnValuesOnConditionCheckFailure"] = json!(mode);
    }
    s
}

fn update_fails(pk: &str, mode: Option<&str>) -> Value {
    stmt(
        &format!("UPDATE T SET w = 1 WHERE pk = '{pk}' AND v = 99"),
        mode,
    )
}

fn update_ok(pk: &str) -> Value {
    stmt(&format!("UPDATE T SET w = 1 WHERE pk = '{pk}'"), None)
}

const CCF: &str = "The conditional request failed";

/// One cancellation reason: its code, and its item when it has one.
#[derive(Debug, PartialEq)]
struct Reason {
    code: String,
    item: Option<Value>,
}

fn none() -> Reason {
    Reason {
        code: "None".into(),
        item: None,
    }
}

fn failed(item: Option<Value>) -> Reason {
    Reason {
        code: "ConditionalCheckFailed".into(),
        item,
    }
}

/// Run a transaction that must cancel, and return its reasons, checking the
/// message each kind carries and the summary that lists them.
async fn cancelled(url: &str, statements: Vec<Value>) -> Vec<Reason> {
    let (status, out) = ddb(
        url,
        "ExecuteTransaction",
        json!({"TransactStatements": statements}),
    )
    .await;
    assert_eq!(status, 400, "expected a cancellation, got {out}");
    assert!(
        out["__type"]
            .as_str()
            .unwrap()
            .ends_with("#TransactionCanceledException"),
        "{out}"
    );
    let reasons: Vec<Reason> = out["CancellationReasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let code = r["Code"].as_str().unwrap().to_string();
            match code.as_str() {
                "None" => assert!(r.get("Message").is_none(), "{r}"),
                "ConditionalCheckFailed" => assert_eq!(r["Message"], CCF, "{r}"),
                _ => {}
            }
            Reason {
                code,
                item: r.get("Item").cloned(),
            }
        })
        .collect();
    let codes: Vec<&str> = reasons.iter().map(|r| r.code.as_str()).collect();
    assert_eq!(
        out["Message"],
        format!(
            "Transaction cancelled, please refer cancellation reasons for specific reasons [{}]",
            codes.join(", ")
        )
    );
    reasons
}

async fn committed(url: &str, statements: Vec<Value>) {
    ok(
        url,
        "ExecuteTransaction",
        json!({"TransactStatements": statements}),
    )
    .await;
}

async fn get(url: &str, pk: &str) -> Option<Value> {
    ok(
        url,
        "GetItem",
        json!({"TableName": T, "Key": {"pk": {"S": pk}}}),
    )
    .await
    .get("Item")
    .cloned()
}

// --- ExecuteTransaction: the item -----------------------------------------

#[tokio::test]
async fn a_failed_update_with_all_old_carries_the_item() {
    // R1, R5, R9.
    let url = seeded().await;
    assert_eq!(
        cancelled(
            &url,
            vec![update_ok("t1"), update_fails("t2", Some(ALL_OLD))]
        )
        .await,
        vec![none(), failed(Some(t2()))]
    );
    assert_eq!(
        cancelled(
            &url,
            vec![update_fails("t2", Some(ALL_OLD)), {
                let mut s = update_ok("t1");
                s["ReturnValuesOnConditionCheckFailure"] = json!(ALL_OLD);
                s
            }]
        )
        .await,
        vec![failed(Some(t2())), none()]
    );
    let with_parameters = json!({
        "Statement": format!("UPDATE \"{T}\" SET w = ? WHERE pk = ? AND v = ?"),
        "Parameters": [{"N": "1"}, {"S": "t2"}, {"N": "99"}],
        "ReturnValuesOnConditionCheckFailure": ALL_OLD
    });
    assert_eq!(
        cancelled(&url, vec![with_parameters]).await,
        vec![failed(Some(t2()))]
    );
}

#[tokio::test]
async fn none_absent_or_another_statements_mode_carries_no_item() {
    // R2, R3, R4.
    let url = seeded().await;
    for mode in [Some("NONE"), None] {
        assert_eq!(
            cancelled(&url, vec![update_ok("t1"), update_fails("t2", mode)]).await,
            vec![none(), failed(None)],
            "{mode:?}"
        );
    }
    let mut first = update_ok("t1");
    first["ReturnValuesOnConditionCheckFailure"] = json!(ALL_OLD);
    assert_eq!(
        cancelled(&url, vec![first, update_fails("t2", None)]).await,
        vec![none(), failed(None)]
    );
}

#[tokio::test]
async fn a_failed_delete_with_all_old_carries_the_item() {
    // R6.
    let url = seeded().await;
    assert_eq!(
        cancelled(
            &url,
            vec![
                update_ok("t1"),
                stmt("DELETE FROM T WHERE pk = 't2' AND v = 99", Some(ALL_OLD))
            ]
        )
        .await,
        vec![none(), failed(Some(t2()))]
    );
}

#[tokio::test]
async fn a_duplicate_insert_or_a_missing_item_carries_no_item() {
    // R7, R7b, R8.
    let url = seeded().await;
    for mode in [ALL_OLD, "NONE"] {
        let reasons = cancelled(
            &url,
            vec![
                update_ok("t1"),
                stmt("INSERT INTO T VALUE {'pk': 't2', 'v': 5}", Some(mode)),
            ],
        )
        .await;
        assert_eq!(
            reasons,
            vec![
                none(),
                Reason {
                    code: "DuplicateItem".into(),
                    item: None
                }
            ]
        );
    }
    assert_eq!(
        cancelled(
            &url,
            vec![
                update_ok("t1"),
                stmt("UPDATE T SET w = 1 WHERE pk = 'zz'", Some(ALL_OLD))
            ]
        )
        .await,
        vec![none(), failed(None)]
    );
}

#[tokio::test]
async fn reads_and_statements_without_a_condition_accept_the_mode() {
    // R15, R17, R18.
    let url = seeded().await;
    let out = ok(
        &url,
        "ExecuteTransaction",
        json!({"TransactStatements": [stmt("SELECT * FROM T WHERE pk = 't1'", Some(ALL_OLD))]}),
    )
    .await;
    assert_eq!(out, json!({"Responses": [{"Item": t1()}]}));
    let mut plain = stmt("UPDATE T SET w = 18 WHERE pk = 't3'", Some(ALL_OLD));
    committed(&url, vec![plain.clone()]).await;
    plain["Statement"] = json!(format!("UPDATE \"{T}\" SET w = 1 WHERE pk = 't3'"));
    assert_eq!(
        cancelled(&url, vec![plain, update_fails("t2", None)]).await,
        vec![none(), failed(None)]
    );
}

// --- ExecuteTransaction: the reasons ---------------------------------------

#[tokio::test]
async fn every_failing_statement_is_listed() {
    // R20, X5, X6.
    let url = seeded().await;
    assert_eq!(
        cancelled(
            &url,
            vec![
                update_fails("t2", Some(ALL_OLD)),
                stmt("DELETE FROM T WHERE pk = 't1' AND v = 99", Some(ALL_OLD)),
            ]
        )
        .await,
        vec![failed(Some(t2())), failed(Some(t1()))]
    );
    assert_eq!(
        cancelled(
            &url,
            vec![
                update_fails("t1", Some(ALL_OLD)),
                stmt("INSERT INTO T VALUE {'pk': 't2'}", None),
            ]
        )
        .await,
        vec![
            failed(Some(t1())),
            Reason {
                code: "DuplicateItem".into(),
                item: None
            }
        ]
    );
    assert_eq!(
        cancelled(
            &url,
            vec![
                update_fails("t1", Some(ALL_OLD)),
                stmt("UPDATE T SET w = 6 WHERE pk = 't3'", None),
                stmt("DELETE FROM T WHERE pk = 't2' AND v = 99", Some(ALL_OLD)),
            ]
        )
        .await,
        vec![failed(Some(t1())), none(), failed(Some(t2()))]
    );
    // Nothing the cancelled transactions ran was kept.
    assert!(get(&url, "t3").await.unwrap().get("w").is_none());
    assert!(get(&url, "t2").await.is_some());
}

#[tokio::test]
async fn a_key_of_the_wrong_type_hides_the_condition_failures() {
    // X7, X8.
    let url = seeded().await;
    let wrong_key = stmt("UPDATE T SET w = 1 WHERE pk = 1", None);
    let key_error = || Reason {
        code: "ValidationError".into(),
        item: None,
    };
    assert_eq!(
        cancelled(
            &url,
            vec![update_fails("t1", Some(ALL_OLD)), wrong_key.clone()]
        )
        .await,
        vec![none(), key_error()]
    );
    assert_eq!(
        cancelled(&url, vec![wrong_key, update_fails("t1", Some(ALL_OLD))]).await,
        vec![key_error(), none()]
    );
}

#[tokio::test]
async fn a_conditional_delete_of_a_missing_item_fails_only_when_the_transaction_does() {
    // Y1, Y3, X3, X4b: alone, or beside statements that succeed, it does nothing.
    let url = seeded().await;
    let delete_missing = |mode| stmt("DELETE FROM T WHERE pk = 'zz' AND v = 1", mode);
    committed(&url, vec![delete_missing(Some(ALL_OLD))]).await;
    committed(&url, vec![delete_missing(Some(ALL_OLD)), update_ok("t1")]).await;
    committed(
        &url,
        vec![stmt(
            "DELETE FROM T WHERE pk = 'zz' AND attribute_not_exists(v)",
            None,
        )],
    )
    .await;
    // Y2, Y4, Y5: when another statement cancels the transaction, it is
    // reported as a failed condition, with no item.
    assert_eq!(
        cancelled(&url, vec![delete_missing(None), update_fails("t2", None)]).await,
        vec![failed(None), failed(None)]
    );
    assert_eq!(
        cancelled(
            &url,
            vec![
                update_ok("t1"),
                delete_missing(Some(ALL_OLD)),
                update_fails("t2", Some(ALL_OLD)),
            ]
        )
        .await,
        vec![none(), failed(None), failed(Some(t2()))]
    );
    // Y6: a DELETE naming only the key is not; Y7: an UPDATE of a missing item
    // fails either way.
    assert_eq!(
        cancelled(
            &url,
            vec![
                stmt("DELETE FROM T WHERE pk = 'zz'", None),
                update_fails("t2", None)
            ]
        )
        .await,
        vec![none(), failed(None)]
    );
    assert_eq!(
        cancelled(
            &url,
            vec![
                stmt("UPDATE T SET w = 1 WHERE pk = 'zz'", None),
                update_fails("t2", None)
            ]
        )
        .await,
        vec![failed(None), failed(None)]
    );
    // Y9, Y10: a DELETE whose condition holds and a new INSERT report None,
    // and neither is kept.
    assert_eq!(
        cancelled(
            &url,
            vec![
                stmt("DELETE FROM T WHERE pk = 't3' AND v = 3", None),
                update_fails("t2", None)
            ]
        )
        .await,
        vec![none(), failed(None)]
    );
    assert_eq!(
        cancelled(
            &url,
            vec![
                stmt("INSERT INTO T VALUE {'pk': 'n1'}", None),
                update_fails("t2", None)
            ]
        )
        .await,
        vec![none(), failed(None)]
    );
    assert!(get(&url, "t3").await.is_some());
    assert!(get(&url, "n1").await.is_none());
}

// --- ExecuteTransaction: the member ----------------------------------------

fn enum_violation(value: &str, path: &str) -> String {
    format!(
        "Value '{value}' at '{path}' failed to satisfy constraint: Member must satisfy enum \
         value set: [ALL_OLD, NONE]"
    )
}

async fn refused(url: &str, op: &str, body: Value) -> String {
    let (status, out) = ddb(url, op, body.clone()).await;
    assert_eq!(status, 400, "{op} should be refused: {body}");
    assert!(
        out["__type"]
            .as_str()
            .unwrap()
            .ends_with("#ValidationException"),
        "{out}"
    );
    out["message"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn a_transaction_refuses_a_mode_outside_all_old_and_none() {
    // R10, R12, R13, V6.
    let url = seeded().await;
    for bad in ["BOGUS", "all_old", "ALL_NEW", ""] {
        assert_eq!(
            refused(
                &url,
                "ExecuteTransaction",
                json!({"TransactStatements": [update_ok("t1"), update_fails("t2", Some(bad))]})
            )
            .await,
            format!(
                "1 validation error detected: {}",
                enum_violation(
                    bad,
                    "transactStatements.2.member.returnValuesOnConditionCheckFailure"
                )
            )
        );
    }
    // V8: null is the member left out.
    let mut null_mode = update_fails("t2", None);
    null_mode["ReturnValuesOnConditionCheckFailure"] = Value::Null;
    assert_eq!(cancelled(&url, vec![null_mode]).await, vec![failed(None)]);
    // V7: a number is not a string.
    let mut number = update_fails("t2", None);
    number["ReturnValuesOnConditionCheckFailure"] = json!(1);
    let (status, out) = ddb(
        &url,
        "ExecuteTransaction",
        json!({"TransactStatements": [number]}),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        out,
        json!({
            "__type": "com.amazon.coral.service#SerializationException",
            "Message": "NUMBER_VALUE cannot be converted to String"
        })
    );
}

#[tokio::test]
async fn a_transactions_mode_is_listed_before_its_statement_and_parameters() {
    // V1, R11.
    let url = seeded().await;
    let big = format!("SELECT * FROM \"{T}\" WHERE pk = '{}'", "x".repeat(9000));
    let statement_too_long = |path: &str| {
        format!(
            "Value '{big}' at '{path}' failed to satisfy constraint: Member must have length \
             less than or equal to 8192"
        )
    };
    let token = "t".repeat(40);
    assert_eq!(
        refused(
            &url,
            "ExecuteTransaction",
            json!({
                "TransactStatements": [
                    {"Statement": big, "Parameters": [], "ReturnValuesOnConditionCheckFailure": "BOGUS"}
                ],
                "ClientRequestToken": token
            })
        )
        .await,
        [
            "4 validation errors detected: ".to_string(),
            [
                format!(
                    "Value '{token}' at 'clientRequestToken' failed to satisfy constraint: \
                     Member must have length less than or equal to 36"
                ),
                enum_violation(
                    "BOGUS",
                    "transactStatements.1.member.returnValuesOnConditionCheckFailure"
                ),
                statement_too_long("transactStatements.1.member.statement"),
                "Value '[]' at 'transactStatements.1.member.parameters' failed to satisfy \
                 constraint: Member must have length greater than or equal to 1"
                    .to_string(),
            ]
            .join("; ")
        ]
        .concat()
    );
    assert_eq!(
        refused(
            &url,
            "ExecuteTransaction",
            json!({
                "TransactStatements": [
                    update_fails("t2", Some("BOGUS")),
                    update_fails("t1", Some("ALSO_BOGUS")),
                    {"Statement": big}
                ],
                "ReturnConsumedCapacity": "BOGUS"
            })
        )
        .await,
        [
            "4 validation errors detected: ".to_string(),
            [
                enum_violation(
                    "BOGUS",
                    "transactStatements.1.member.returnValuesOnConditionCheckFailure"
                ),
                enum_violation(
                    "ALSO_BOGUS",
                    "transactStatements.2.member.returnValuesOnConditionCheckFailure"
                ),
                statement_too_long("transactStatements.3.member.statement"),
                "Value 'BOGUS' at 'returnConsumedCapacity' failed to satisfy constraint: \
                 Member must satisfy enum value set: [INDEXES, TOTAL, NONE]"
                    .to_string(),
            ]
            .join("; ")
        ]
        .concat()
    );
}

#[tokio::test]
async fn the_mode_takes_part_in_the_token_match() {
    // I1 to I4: absent counts as NONE.
    let url = seeded().await;
    let request = |mode: Option<&str>| {
        json!({
            "TransactStatements": [stmt("UPDATE T SET w = 30 WHERE pk = 't4'", mode)],
            "ClientRequestToken": "rv6-token"
        })
    };
    ok(&url, "ExecuteTransaction", request(Some("NONE"))).await;
    let (status, out) = ddb(&url, "ExecuteTransaction", request(Some(ALL_OLD))).await;
    assert_eq!(status, 400);
    assert!(
        out["__type"]
            .as_str()
            .unwrap()
            .ends_with("#IdempotentParameterMismatchException"),
        "{out}"
    );
    assert_eq!(
        ok(&url, "ExecuteTransaction", request(None)).await,
        json!({"Responses": []})
    );
    assert_eq!(
        ok(&url, "ExecuteTransaction", request(Some("NONE"))).await,
        json!({"Responses": []})
    );
}

// --- BatchExecuteStatement --------------------------------------------------

async fn batch(url: &str, statements: Vec<Value>) -> Vec<Value> {
    ok(
        url,
        "BatchExecuteStatement",
        json!({"Statements": statements}),
    )
    .await["Responses"]
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::test]
async fn a_batch_member_with_all_old_carries_the_item_on_its_error() {
    // B1, B2.
    let url = seeded().await;
    assert_eq!(
        batch(
            &url,
            vec![
                update_fails("t2", Some(ALL_OLD)),
                update_fails("t1", Some("NONE")),
                update_fails("t3", None),
            ]
        )
        .await,
        vec![
            json!({"Error": {"Code": "ConditionalCheckFailed", "Message": CCF, "Item": t2()}, "TableName": T}),
            json!({"Error": {"Code": "ConditionalCheckFailed", "Message": CCF}, "TableName": T}),
            json!({"Error": {"Code": "ConditionalCheckFailed", "Message": CCF}, "TableName": T}),
        ]
    );
    assert_eq!(
        batch(
            &url,
            vec![stmt(
                "DELETE FROM T WHERE pk = 't2' AND v = 99",
                Some(ALL_OLD)
            )]
        )
        .await,
        vec![
            json!({"Error": {"Code": "ConditionalCheckFailed", "Message": CCF, "Item": t2()}, "TableName": T})
        ]
    );
}

#[tokio::test]
async fn a_batch_member_carries_no_item_where_there_is_none_to_give() {
    // B3, B5, X9, X10.
    let url = seeded().await;
    assert_eq!(
        batch(
            &url,
            vec![stmt("INSERT INTO T VALUE {'pk': 't2'}", Some(ALL_OLD))]
        )
        .await,
        vec![
            json!({"Error": {"Code": "DuplicateItem", "Message": "Duplicate primary key exists in table"}, "TableName": T})
        ]
    );
    assert_eq!(
        batch(
            &url,
            vec![stmt("SELECT * FROM T WHERE pk = 't1'", Some(ALL_OLD))]
        )
        .await,
        vec![json!({"Item": t1(), "TableName": T})]
    );
    assert_eq!(
        batch(
            &url,
            vec![stmt(
                "DELETE FROM T WHERE pk = 'zz' AND v = 1",
                Some(ALL_OLD)
            )]
        )
        .await,
        vec![json!({"TableName": T})]
    );
    assert_eq!(
        batch(
            &url,
            vec![stmt("UPDATE T SET w = 1 WHERE pk = 'zz'", Some(ALL_OLD))]
        )
        .await,
        vec![json!({"Error": {"Code": "ConditionalCheckFailed", "Message": CCF}, "TableName": T})]
    );
}

#[tokio::test]
async fn a_batch_refuses_a_mode_outside_all_old_and_none() {
    // B4, B6, V4, V2.
    let url = seeded().await;
    for (bad, member) in [("BOGUS", 2), ("all_old", 1), ("", 1)] {
        let mut statements = vec![update_fails("t2", Some(bad))];
        if member == 2 {
            statements.insert(0, update_ok("t1"));
        }
        assert_eq!(
            refused(
                &url,
                "BatchExecuteStatement",
                json!({"Statements": statements})
            )
            .await,
            format!(
                "1 validation error detected: {}",
                enum_violation(
                    bad,
                    &format!("statements.{member}.member.returnValuesOnConditionCheckFailure")
                )
            )
        );
    }
    let big = format!("SELECT * FROM \"{T}\" WHERE pk = '{}'", "x".repeat(9000));
    assert_eq!(
        refused(
            &url,
            "BatchExecuteStatement",
            json!({
                "Statements": [
                    {"Statement": big, "Parameters": [], "ReturnValuesOnConditionCheckFailure": "BOGUS"}
                ],
                "ReturnConsumedCapacity": "BOGUS"
            })
        )
        .await,
        [
            "4 validation errors detected: ".to_string(),
            [
                "Value 'BOGUS' at 'returnConsumedCapacity' failed to satisfy constraint: \
                 Member must satisfy enum value set: [INDEXES, TOTAL, NONE]"
                    .to_string(),
                enum_violation(
                    "BOGUS",
                    "statements.1.member.returnValuesOnConditionCheckFailure"
                ),
                format!(
                    "Value '{big}' at 'statements.1.member.statement' failed to satisfy \
                     constraint: Member must have length less than or equal to 8192"
                ),
                "Value '[]' at 'statements.1.member.parameters' failed to satisfy constraint: \
                 Member must have length greater than or equal to 1"
                    .to_string(),
            ]
            .join("; ")
        ]
        .concat()
    );
}

// --- ExecuteStatement -------------------------------------------------------

async fn statement_fails(url: &str, body: Value) -> Value {
    let (status, out) = ddb(url, "ExecuteStatement", body.clone()).await;
    assert_eq!(status, 400, "{body} should fail: {out}");
    out
}

#[tokio::test]
async fn a_failed_statement_with_all_old_carries_the_item() {
    // S1, S2, and RETURNING ALL OLD * beside the mode. DynamoDB writes this
    // exception's message as `Message`, and its number sets in hash order, as
    // it does the PartiQL operations' other items.
    let url = seeded().await;
    for text in [
        "UPDATE T SET w = 1 WHERE pk = 't2' AND v = 99",
        "DELETE FROM T WHERE pk = 't2' AND v = 99",
        "UPDATE T SET w = 1 WHERE pk = 't2' AND v = 99 RETURNING ALL OLD *",
        "DELETE FROM T WHERE pk = 't2' AND v = 99 RETURNING ALL OLD *",
    ] {
        assert_eq!(
            statement_fails(&url, stmt(text, Some(ALL_OLD))).await,
            json!({
                "__type": "com.amazonaws.dynamodb.v20120810#ConditionalCheckFailedException",
                "Item": t2(),
                "Message": CCF
            }),
            "{text}"
        );
    }
}

#[tokio::test]
async fn a_failed_statement_carries_no_item_where_there_is_none_to_give() {
    // S3, C6, C9, C11; C7 and C8 succeed.
    let url = seeded().await;
    let plain = json!({
        "__type": "com.amazonaws.dynamodb.v20120810#ConditionalCheckFailedException",
        "message": CCF
    });
    assert_eq!(
        statement_fails(
            &url,
            stmt(
                "UPDATE T SET w = 1 WHERE pk = 't2' AND v = 99",
                Some("NONE")
            )
        )
        .await,
        plain
    );
    let mut null_mode = stmt("UPDATE T SET w = 1 WHERE pk = 't2' AND v = 99", None);
    null_mode["ReturnValuesOnConditionCheckFailure"] = Value::Null;
    assert_eq!(statement_fails(&url, null_mode).await, plain);
    assert_eq!(
        statement_fails(
            &url,
            stmt("UPDATE T SET w = 1 WHERE pk = 'zz'", Some(ALL_OLD))
        )
        .await,
        plain
    );
    assert_eq!(
        statement_fails(
            &url,
            stmt("INSERT INTO T VALUE {'pk': 't2'}", Some(ALL_OLD))
        )
        .await,
        json!({
            "__type": "com.amazonaws.dynamodb.v20120810#DuplicateItemException",
            "message": "Duplicate primary key exists in table"
        })
    );
    assert_eq!(
        ok(
            &url,
            "ExecuteStatement",
            stmt("SELECT * FROM T WHERE pk = 't1'", Some(ALL_OLD))
        )
        .await,
        json!({"Items": [t1()]})
    );
    assert_eq!(
        ok(
            &url,
            "ExecuteStatement",
            stmt("DELETE FROM T WHERE pk = 'zz' AND v = 1", Some(ALL_OLD))
        )
        .await,
        json!({"Items": []})
    );
}

#[tokio::test]
async fn a_statement_refuses_a_mode_outside_all_old_and_none_listed_first() {
    // S4, V5, V3.
    let url = seeded().await;
    for bad in ["BOGUS", ""] {
        assert_eq!(
            refused(
                &url,
                "ExecuteStatement",
                stmt("UPDATE T SET w = 1 WHERE pk = 't2' AND v = 99", Some(bad))
            )
            .await,
            format!(
                "1 validation error detected: {}",
                enum_violation(bad, "returnValuesOnConditionCheckFailure")
            )
        );
    }
    let big = format!("SELECT * FROM \"{T}\" WHERE pk = '{}'", "x".repeat(9000));
    assert_eq!(
        refused(
            &url,
            "ExecuteStatement",
            json!({
                "Statement": big, "Parameters": [], "ReturnValuesOnConditionCheckFailure": "BOGUS",
                "ReturnConsumedCapacity": "BOGUS", "Limit": 0
            })
        )
        .await,
        [
            "5 validation errors detected: ".to_string(),
            [
                enum_violation("BOGUS", "returnValuesOnConditionCheckFailure"),
                "Value '0' at 'limit' failed to satisfy constraint: Member must have value \
                 greater than or equal to 1"
                    .to_string(),
                format!(
                    "Value '{big}' at 'statement' failed to satisfy constraint: Member must \
                     have length less than or equal to 8192"
                ),
                "Value 'BOGUS' at 'returnConsumedCapacity' failed to satisfy constraint: \
                 Member must satisfy enum value set: [INDEXES, TOTAL, NONE]"
                    .to_string(),
                "Value '[]' at 'parameters' failed to satisfy constraint: Member must have \
                 length greater than or equal to 1"
                    .to_string(),
            ]
            .join("; ")
        ]
        .concat()
    );
}
