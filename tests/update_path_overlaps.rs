//! Overlapping SET paths in an UpdateExpression, on UpdateItem and in a
//! TransactWriteItems Update.
//!
//! DynamoDB refuses `SET a.b = :v, a = :m` in either clause order, whether
//! the item holds `a` or not, naming the two paths in the order they were
//! written. UpdateItem's message carries the `1 validation error detected: `
//! envelope; a transaction's is a bare top-level ValidationException, ahead of
//! any write, and nothing is written. Captured in eu-west-2 and us-east-1,
//! 2026-10-09.

#![cfg(feature = "http-server")]

use dynoxide::Database;
use serde_json::{Value, json};

const OVERLAP: &str = "Invalid UpdateExpression: Two document paths overlap with each other; must remove or rewrite one of these paths";

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

async fn send(url: &str, operation: &str, body: Value) -> (u16, Value) {
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
        .unwrap();
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// A table with two items: `bare` without `a` and `held` with it.
async fn setup() -> String {
    let url = start().await;
    let create = json!({
        "TableName": "Overlaps",
        "BillingMode": "PAY_PER_REQUEST",
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}]
    });
    assert_eq!(send(&url, "CreateTable", create).await.0, 200);
    for item in [
        json!({"pk": {"S": "bare"}}),
        json!({"pk": {"S": "held"}, "a": {"M": {"x": {"S": "old"}}}}),
    ] {
        assert_eq!(
            send(
                &url,
                "PutItem",
                json!({"TableName": "Overlaps", "Item": item})
            )
            .await
            .0,
            200
        );
    }
    url
}

async fn stored(url: &str, pk: &str) -> Value {
    let get = json!({"TableName": "Overlaps", "Key": {"pk": {"S": pk}}, "ConsistentRead": true});
    send(url, "GetItem", get).await.1["Item"].clone()
}

/// Each clause order, and the paths it names first and second.
const SHAPES: [(&str, &str, &str); 2] = [
    ("SET a.b = :v, a = :m", "[a, b]", "[a]"),
    ("SET a = :m, a.b = :v", "[a]", "[a, b]"),
];

fn values() -> Value {
    json!({":v": {"N": "1"}, ":m": {"M": {}}})
}

#[tokio::test]
async fn update_item_names_overlapping_paths_in_the_order_written() {
    let url = setup().await;
    for pk in ["bare", "held"] {
        for (expression, one, two) in SHAPES {
            let before = stored(&url, pk).await;
            let update = json!({
                "TableName": "Overlaps",
                "Key": {"pk": {"S": pk}},
                "UpdateExpression": expression,
                "ExpressionAttributeValues": values()
            });
            let (status, body) = send(&url, "UpdateItem", update).await;
            assert_eq!(status, 400, "{pk} {expression}: {body}");
            assert_eq!(
                body["message"],
                format!("1 validation error detected: {OVERLAP}; path one: {one}, path two: {two}"),
                "{pk} {expression}"
            );
            assert_eq!(stored(&url, pk).await, before, "{pk} {expression}");
        }
    }
}

#[tokio::test]
async fn a_transacted_update_with_overlapping_paths_is_refused_up_front() {
    let url = setup().await;
    for pk in ["bare", "held"] {
        for (expression, one, two) in SHAPES {
            let before = stored(&url, pk).await;
            let transaction = json!({"TransactItems": [{"Update": {
                "TableName": "Overlaps",
                "Key": {"pk": {"S": pk}},
                "UpdateExpression": expression,
                "ExpressionAttributeValues": values()
            }}]});
            let (status, body) = send(&url, "TransactWriteItems", transaction).await;
            assert_eq!(status, 400, "{pk} {expression}: {body}");
            assert!(
                body["__type"]
                    .as_str()
                    .is_some_and(|t| t.ends_with("#ValidationException")),
                "{pk} {expression}: {body}"
            );
            assert_eq!(
                body["message"],
                format!("{OVERLAP}; path one: {one}, path two: {two}"),
                "{pk} {expression}"
            );
            assert_eq!(stored(&url, pk).await, before, "{pk} {expression}");
        }
    }
}
