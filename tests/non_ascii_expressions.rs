//! Non-ASCII characters in every expression surface: UpdateExpression,
//! ConditionExpression, FilterExpression, KeyConditionExpression,
//! ProjectionExpression, PartiQL statements, and the transaction forms.
//!
//! Each case puts a 2-, 3- or 4-byte character (é, €, an emoji) or a
//! non-breaking space at the start of an expression, between two tokens, at
//! the end, against a placeholder, inside a name, or in a placeholder's own
//! name, and is replayed against the answer DynamoDB gave in eu-west-2 on
//! 9 October 2026. DynamoDB reads none of these characters outside a string
//! literal, the non-breaking space included: each is a token of its own in the
//! `near` window.
//!
//! us-east-1 gave the same answers apart from the emoji on Scan and in
//! transactions, which its older validation layer reports as two UTF-16 halves
//! (`token: "?"`); the fixture keeps both regions' answers.
//!
//! All 140 captured cases are replayed. In a placeholder's own name the
//! character is refused as an invalid key of its map, before the expression is
//! read. In a PartiQL statement it is refused as not well formed with nothing
//! after the colon, and inside a string literal the statement runs.

#![cfg(feature = "http-server")]

use dynoxide::Database;
use serde_json::{Value, json};

const FIXTURE: &str = include_str!("fixtures/non_ascii_expressions.json");
const TABLE: &str = "_capture_20261009_u8_t";

async fn start() -> String {
    let db = Database::memory().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        dynoxide::server::serve_on(listener, db).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    format!("http://{addr}")
}

/// Send `body` as `operation`; `None` when the server didn't answer at all.
async fn send(url: &str, operation: &str, body: &Value) -> Option<(u16, Value)> {
    let resp = reqwest::Client::new()
        .post(url)
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=fakekey/20260101/us-east-1/dynamodb/aws4_request, SignedHeaders=host;x-amz-date;x-amz-target, Signature=fakesig",
        )
        .header("x-amz-date", "20260101T000000Z")
        .body(body.to_string())
        .send()
        .await
        .ok()?;
    let status = resp.status().as_u16();
    let text = resp.text().await.ok()?;
    Some((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

async fn seed(url: &str) {
    let create = json!({
        "TableName": TABLE,
        "BillingMode": "PAY_PER_REQUEST",
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}]
    });
    let put = json!({
        "TableName": TABLE,
        "Item": {"pk": {"S": "x"}, "a": {"N": "1"}, "b": {"N": "2"}}
    });
    for (operation, body) in [("CreateTable", create), ("PutItem", put)] {
        let (status, answer) = send(url, operation, &body).await.expect("server answers");
        assert_eq!(status, 200, "{operation} failed: {answer}");
    }
}

/// Replay every fixture case `wanted` selects against a fresh server, and
/// return each one that answers differently from DynamoDB in eu-west-2.
async fn replay(wanted: impl Fn(&str, &str) -> bool) -> (usize, Vec<String>) {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 140, "the fixture holds {} cases", cases.len());

    let url = start().await;
    seed(&url).await;

    let mut ran = 0;
    let mut failures = Vec::new();
    for case in cases {
        let id = case["id"].as_str().unwrap();
        let mut parts = id.split('/').skip(1);
        let (surface, position) = (parts.next().unwrap(), parts.next().unwrap());
        if !wanted(surface, position) {
            continue;
        }
        ran += 1;
        let operation = case["operation"].as_str().unwrap();
        let expected = &case["eu_west_2"];
        let Some((status, answer)) = send(&url, operation, &case["body"]).await else {
            failures.push(format!("{id}: no answer from the server"));
            continue;
        };
        if expected["ok"] == true {
            if status != 200 {
                failures.push(format!("{id}: refused where DynamoDB runs it: {answer}"));
            }
            continue;
        }
        let type_ok = answer["__type"]
            .as_str()
            .is_some_and(|t| t.ends_with("#ValidationException"));
        if status != 400 || !type_ok || answer["message"] != expected["message"] {
            failures.push(format!(
                "{id}: got {status} {answer}, DynamoDB answered {}",
                expected["message"]
            ));
        }
    }

    // The server still answers after every case.
    let get = json!({"TableName": TABLE, "Key": {"pk": {"S": "x"}}});
    match send(&url, "GetItem", &get).await {
        Some((200, _)) => {}
        other => failures.push(format!("the server stopped answering: {other:?}")),
    }
    (ran, failures)
}

/// The character anywhere in an expression: refused with DynamoDB's syntax
/// error naming it, and the server goes on answering.
#[tokio::test]
async fn non_ascii_characters_in_expressions_answer_as_dynamodb_does() {
    let (ran, failures) = replay(|surface, position| {
        surface != "partiql" && !matches!(position, "value-key" | "name-key")
    })
    .await;
    assert_eq!(ran, 112);
    assert!(
        failures.is_empty(),
        "{} of {ran} cases differ from DynamoDB:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The character in a PartiQL statement: outside a string DynamoDB refuses the
/// statement as not well formed, with nothing after the colon; inside a string
/// it runs.
#[tokio::test]
async fn non_ascii_characters_in_partiql_statements_answer_as_dynamodb_does() {
    let (ran, failures) = replay(|surface, _| surface == "partiql").await;
    assert_eq!(ran, 20);
    assert!(
        failures.is_empty(),
        "{} of {ran} cases differ from DynamoDB:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The character in a placeholder's own name: the name map's key is refused
/// as a syntax error before the expression is read.
#[tokio::test]
async fn non_ascii_characters_in_placeholder_names_answer_as_dynamodb_does() {
    let (ran, failures) = replay(|_, position| matches!(position, "value-key" | "name-key")).await;
    assert_eq!(ran, 8);
    assert!(
        failures.is_empty(),
        "{} of {ran} cases differ from DynamoDB:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
