//! Several request-member validation messages, and the order DynamoDB reports
//! them in when more than one thing is wrong.
//!
//! Every expectation was captured against DynamoDB in eu-west-2 and us-east-1
//! on 9 and 10 October 2026, and the two regions agree on each one. DynamoDB
//! joins request-member errors into one `N validation errors detected:`
//! message and checks some members ahead of others.

#![cfg(feature = "http-server")]

use dynoxide::Database;
use serde_json::{Value, json};

const TABLE_NULL: &str =
    "Value null at 'tableName' failed to satisfy constraint: Member must not be null";
const KEY_NULL: &str = "Value null at 'key' failed to satisfy constraint: Member must not be null";
const RETURN_VALUES_BOGUS: &str = "Value 'BOGUS' at 'returnValues' failed to satisfy constraint: Member must satisfy enum value set: [ALL_NEW, UPDATED_OLD, ALL_OLD, NONE, UPDATED_NEW]";
const RCC_BOGUS: &str = "Value 'BOGUS' at 'returnConsumedCapacity' failed to satisfy constraint: Member must satisfy enum value set: [INDEXES, TOTAL, NONE]";
const RICM_BOGUS: &str = "Value 'BOGUS' at 'returnItemCollectionMetrics' failed to satisfy constraint: Member must satisfy enum value set: [SIZE, NONE]";
const SELECT_BOGUS: &str = "Value 'BOGUS' at 'select' failed to satisfy constraint: Member must satisfy enum value set: [SPECIFIC_ATTRIBUTES, COUNT, ALL_ATTRIBUTES, ALL_PROJECTED_ATTRIBUTES]";
const LIMIT_ZERO: &str = "Value '0' at 'limit' failed to satisfy constraint: Member must have value greater than or equal to 1";
const RETURN_VALUES_NOT_HONOURED: &str = "ReturnValues can only be ALL_OLD or NONE";

fn joined(errors: &[&str]) -> String {
    match errors {
        [one] => format!("1 validation error detected: {one}"),
        many => format!(
            "{} validation errors detected: {}",
            many.len(),
            many.join("; ")
        ),
    }
}

async fn start() -> String {
    let db = Database::memory().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        dynoxide::server::serve_on(listener, db).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let url = format!("http://{addr}");
    let (status, created) = send(
        &url,
        "CreateTable",
        json!({
            "TableName": "Env",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, 200, "{created}");
    url
}

async fn send(url: &str, operation: &str, body: String) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(url)
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=fakekey/20260101/us-east-1/dynamodb/aws4_request, SignedHeaders=host;x-amz-date;x-amz-target, Signature=fakesig",
        )
        .header("x-amz-date", "20260101T000000Z")
        .body(body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// The message of the ValidationException `body` is refused with.
async fn refusal(url: &str, operation: &str, body: Value) -> String {
    let (status, resp) = send(url, operation, body.to_string()).await;
    assert_eq!(status, 400, "{operation} {body}: {resp}");
    assert!(
        resp["__type"]
            .as_str()
            .unwrap()
            .ends_with("ValidationException"),
        "{operation} {body}: {resp}"
    );
    resp["message"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------------------
// UpdateItem with no TableName keeps that error beside the others
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_item_without_a_table_name_joins_it_with_the_other_member_errors() {
    let url = start().await;
    let cases: [(Value, Vec<&str>); 5] = [
        (
            json!({"ReturnValues": "BOGUS"}),
            vec![TABLE_NULL, RETURN_VALUES_BOGUS, KEY_NULL],
        ),
        (
            json!({"ReturnConsumedCapacity": "BOGUS"}),
            vec![RCC_BOGUS, TABLE_NULL, KEY_NULL],
        ),
        (
            json!({"ReturnItemCollectionMetrics": "BOGUS"}),
            vec![TABLE_NULL, RICM_BOGUS, KEY_NULL],
        ),
        (json!({}), vec![TABLE_NULL, KEY_NULL]),
        (
            json!({"Key": {"pk": {"S": "a"}}, "ReturnValues": "BOGUS"}),
            vec![TABLE_NULL, RETURN_VALUES_BOGUS],
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(
            refusal(&url, "UpdateItem", body.clone()).await,
            joined(&expected),
            "{body}"
        );
    }
}

#[tokio::test]
async fn update_item_without_a_table_name_or_key_keeps_the_update_expression_out_of_it() {
    // The captured two-error answer is the same with or without an
    // UpdateExpression beside it.
    let url = start().await;
    let message = refusal(
        &url,
        "UpdateItem",
        json!({"UpdateExpression": "SET a = :v"}),
    )
    .await;
    assert_eq!(message, joined(&[TABLE_NULL, KEY_NULL]));
}

// ---------------------------------------------------------------------------
// Query with no TableName and a Limit of 0
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_without_a_table_name_joins_it_with_a_zero_limit() {
    let url = start().await;
    let message = refusal(&url, "Query", json!({"Limit": 0})).await;
    assert_eq!(message, joined(&[TABLE_NULL, LIMIT_ZERO]));

    // A Select outside the enum leads.
    let message = refusal(&url, "Query", json!({"Limit": 0, "Select": "BOGUS"})).await;
    assert_eq!(message, joined(&[SELECT_BOGUS, TABLE_NULL, LIMIT_ZERO]));
}

#[tokio::test]
async fn query_with_a_table_name_still_reports_a_zero_limit_alone() {
    let url = start().await;
    let message = refusal(&url, "Query", json!({"TableName": "Env", "Limit": 0})).await;
    assert_eq!(
        message,
        joined(&[
            "Value at 'Limit' failed to satisfy constraint: Member must have value greater than or equal to 1"
        ])
    );
}

// ---------------------------------------------------------------------------
// BatchGetItem: a malformed table name beats the key count
// ---------------------------------------------------------------------------

fn keys(n: usize) -> Value {
    Value::Array(
        (0..n)
            .map(|i| json!({"pk": {"S": format!("k{i}")}}))
            .collect(),
    )
}

#[tokio::test]
async fn batch_get_item_reports_a_malformed_table_name_ahead_of_the_total_key_count() {
    let url = start().await;
    // 1 + 100 keys in all, one entry under a name that breaks the constraints.
    let message = refusal(
        &url,
        "BatchGetItem",
        json!({"RequestItems": {
            "a": {"Keys": keys(1)},
            "Env": {"Keys": keys(100)}
        }}),
    )
    .await;
    assert!(
        message.contains(
            "at 'requestItems' failed to satisfy constraint: Map keys must satisfy constraint"
        ),
        "the map-key error leads: {message}"
    );
    assert!(
        !message.contains("member.Keys"),
        "the key count is not reported beside it: {message}"
    );
}

#[tokio::test]
async fn batch_get_item_still_reports_over_100_keys_in_one_valid_entry() {
    let url = start().await;
    let message = refusal(
        &url,
        "BatchGetItem",
        json!({"RequestItems": {"Env": {"Keys": keys(101)}}}),
    )
    .await;
    assert_eq!(
        message,
        "1 validation error detected: Value at 'RequestItems.Env.member.Keys' failed to satisfy constraint: Member must have length less than or equal to 100"
    );
}

// ---------------------------------------------------------------------------
// DeleteItem: a ReturnValues it does not honour is a request member
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_item_joins_an_unsupported_return_values_after_the_other_member_errors() {
    let url = start().await;
    for rv in ["ALL_NEW", "UPDATED_NEW"] {
        // Short table name.
        let message = refusal(
            &url,
            "DeleteItem",
            json!({"TableName": "ab", "Key": {"pk": {"S": "a"}}, "ReturnValues": rv}),
        )
        .await;
        assert_eq!(
            message,
            joined(&[
                "Value 'ab' at 'tableName' failed to satisfy constraint: Member must have length greater than or equal to 3",
                RETURN_VALUES_NOT_HONOURED
            ]),
            "{rv}"
        );

        // An unknown ReturnConsumedCapacity leads.
        let message = refusal(
            &url,
            "DeleteItem",
            json!({"TableName": "Env", "Key": {"pk": {"S": "a"}}, "ReturnValues": rv, "ReturnConsumedCapacity": "BOGUS"}),
        )
        .await;
        assert_eq!(
            message,
            joined(&[RCC_BOGUS, RETURN_VALUES_NOT_HONOURED]),
            "{rv}"
        );
    }
}

#[tokio::test]
async fn delete_item_reports_an_unsupported_return_values_ahead_of_the_expressions() {
    let url = start().await;
    let alone = joined(&[RETURN_VALUES_NOT_HONOURED]);
    let bad_expressions = [
        json!({"ConditionExpression": "a = = b"}),
        json!({"ConditionExpression": "a = :unused", "ExpressionAttributeValues": {":unused": {"S": "x"}}}),
        json!({"ExpressionAttributeValues": {":v": {"S": "x"}}}),
        json!({
            "ConditionExpression": "attribute_exists(a)",
            "Expected": {"a": {"Exists": true}}
        }),
    ];
    for extra in bad_expressions {
        for rv in ["ALL_NEW", "UPDATED_NEW"] {
            let mut body =
                json!({"TableName": "Env", "Key": {"pk": {"S": "a"}}, "ReturnValues": rv});
            for (k, v) in extra.as_object().unwrap() {
                body[k] = v.clone();
            }
            assert_eq!(
                refusal(&url, "DeleteItem", body.clone()).await,
                alone,
                "{body}"
            );
        }
    }
}

#[tokio::test]
async fn delete_item_with_no_key_reports_the_key_alone() {
    let url = start().await;
    let message = refusal(
        &url,
        "DeleteItem",
        json!({"TableName": "Env", "ReturnValues": "ALL_NEW"}),
    )
    .await;
    assert_eq!(message, joined(&[KEY_NULL]));
}

#[tokio::test]
async fn delete_item_on_a_missing_table_still_reports_an_unsupported_return_values() {
    let url = start().await;
    let message = refusal(
        &url,
        "DeleteItem",
        json!({"TableName": "Missing", "Key": {"pk": {"S": "a"}}, "ReturnValues": "ALL_NEW"}),
    )
    .await;
    assert_eq!(message, joined(&[RETURN_VALUES_NOT_HONOURED]));
}

#[tokio::test]
async fn delete_item_reports_an_unsupported_return_values_bare_ahead_of_a_deep_key() {
    let url = start().await;
    let depth = 100;
    let key = format!(
        "{}{{\"S\":\"leaf\"}}{}",
        "{\"M\":{\"n\":".repeat(depth),
        "}}".repeat(depth)
    );
    let body =
        format!("{{\"TableName\":\"Env\",\"Key\":{{\"pk\":{key}}},\"ReturnValues\":\"ALL_NEW\"}}");
    let (status, resp) = send(&url, "DeleteItem", body).await;
    assert_eq!(status, 400, "{resp}");
    assert_eq!(resp["message"], RETURN_VALUES_NOT_HONOURED, "{resp}");
}

// ---------------------------------------------------------------------------
// UpdateItem: an empty UpdateExpression
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_item_envelopes_an_empty_update_expression_and_reports_an_empty_condition_first() {
    let url = start().await;
    let base = json!({"TableName": "Env", "Key": {"pk": {"S": "a"}}});

    let mut body = base.clone();
    body["UpdateExpression"] = json!("");
    assert_eq!(
        refusal(&url, "UpdateItem", body).await,
        joined(&["Invalid UpdateExpression: The expression can not be empty;"])
    );

    let mut body = base.clone();
    body["UpdateExpression"] = json!("");
    body["ConditionExpression"] = json!("");
    assert_eq!(
        refusal(&url, "UpdateItem", body).await,
        joined(&["Invalid ConditionExpression: The expression can not be empty;"])
    );

    // The same with the table missing.
    let body = json!({
        "TableName": "Missing", "Key": {"pk": {"S": "a"}}, "UpdateExpression": ""
    });
    assert_eq!(
        refusal(&url, "UpdateItem", body).await,
        joined(&["Invalid UpdateExpression: The expression can not be empty;"])
    );
}
