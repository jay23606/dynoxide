//! PutItem, UpdateItem and DeleteItem report their request-validation errors
//! inside the `1 validation error detected: ` envelope.
//!
//! Captured against DynamoDB in eu-west-2 and us-east-1 on 2026-10-09; both
//! regions agree on every answer here. `ExpressionAttributeValues` without an
//! expression carries no `: ConditionExpression is null` suffix on any of the
//! three, and DeleteItem refuses a `ReturnValues` it can't honour before it
//! looks the table up.

#![cfg(feature = "http-server")]

use dynoxide::Database;
use serde_json::{Value, json};

const INVALID: &str = "One or more parameter values were invalid: ";

fn one(message: &str) -> String {
    format!("1 validation error detected: {message}")
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
        }),
    )
    .await;
    assert_eq!(status, 200, "{created}");
    url
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
    (status, resp.json().await.unwrap())
}

/// Send `extra` merged into a request for `operation` on the table `table`,
/// and return the ValidationException's message.
async fn refusal(url: &str, operation: &str, table: &str, extra: Value) -> String {
    let mut body = match operation {
        "PutItem" => json!({"TableName": table, "Item": {"pk": {"S": "a"}}}),
        _ => json!({"TableName": table, "Key": {"pk": {"S": "a"}}}),
    };
    for (k, v) in extra.as_object().unwrap() {
        body[k] = v.clone();
    }
    let (status, resp) = send(url, operation, body).await;
    assert_eq!(status, 400, "{operation} {extra}: {resp}");
    assert!(
        resp["__type"]
            .as_str()
            .unwrap()
            .ends_with("ValidationException"),
        "{operation} {extra}: {resp}"
    );
    resp["message"].as_str().unwrap().to_string()
}

/// The families every one of the three writes envelopes the same way.
fn shared_cases() -> Vec<(Value, String)> {
    vec![
        (
            json!({"ExpressionAttributeValues": {":v": {"S": "x"}}}),
            one("ExpressionAttributeValues can only be specified when using expressions"),
        ),
        (
            json!({"ExpressionAttributeNames": {"#a": "a"}}),
            one("ExpressionAttributeNames can only be specified when using expressions"),
        ),
        (
            json!({"ConditionExpression": "attribute_exists(a)", "ExpressionAttributeValues": {}}),
            one("ExpressionAttributeValues must not be empty"),
        ),
        (
            json!({"ConditionExpression": "attribute_exists(a)", "ExpressionAttributeNames": {}}),
            one("ExpressionAttributeNames must not be empty"),
        ),
        (
            json!({"ConditionExpression": ""}),
            one("Invalid ConditionExpression: The expression can not be empty;"),
        ),
        (
            json!({"Expected": {"a": {"Exists": true}}}),
            one(&format!(
                "{INVALID}Value must be provided when Exists is true for Attribute: a"
            )),
        ),
    ]
}

#[tokio::test]
async fn put_update_and_delete_envelope_their_request_validation_errors() {
    let url = start().await;
    for operation in ["PutItem", "UpdateItem", "DeleteItem"] {
        for (extra, expected) in shared_cases() {
            let got = refusal(&url, operation, "Env", extra.clone()).await;
            assert_eq!(got, expected, "{operation} {extra}");
        }
    }
}

#[tokio::test]
async fn every_malformed_expected_condition_is_enveloped_on_delete_and_put() {
    let url = start().await;
    let cases = [
        (
            json!({"a": {"Exists": false, "Value": {"S": "x"}}}),
            "Value cannot be used when Exists is false for Attribute: a",
        ),
        (
            json!({"a": {"Value": {"S": "x"}, "ComparisonOperator": "EQ",
                "AttributeValueList": [{"S": "x"}]}}),
            "Value and AttributeValueList cannot be used together for Attribute: a",
        ),
        (
            json!({"a": {"ComparisonOperator": "EQ"}}),
            "Value or AttributeValueList must be used with ComparisonOperator: EQ for Attribute: a",
        ),
        (
            json!({"a": {"ComparisonOperator": "BETWEEN", "AttributeValueList": [{"S": "x"}]}}),
            "Invalid number of argument(s) for the BETWEEN ComparisonOperator",
        ),
    ];
    for operation in ["DeleteItem", "PutItem"] {
        for (expected_param, message) in &cases {
            let got = refusal(&url, operation, "Env", json!({"Expected": expected_param})).await;
            assert_eq!(got, one(&format!("{INVALID}{message}")), "{operation}");
        }
    }
}

#[tokio::test]
async fn delete_item_refuses_return_values_it_cannot_honour_before_the_table() {
    let url = start().await;
    for rv in ["ALL_NEW", "UPDATED_NEW", "UPDATED_OLD"] {
        let got = refusal(&url, "DeleteItem", "Env", json!({"ReturnValues": rv})).await;
        assert_eq!(got, one("ReturnValues can only be ALL_OLD or NONE"), "{rv}");
    }
    // The table doesn't exist; the request is still refused for ReturnValues.
    let got = refusal(
        &url,
        "DeleteItem",
        "Missing",
        json!({"ReturnValues": "ALL_NEW"}),
    )
    .await;
    assert_eq!(got, one("ReturnValues can only be ALL_OLD or NONE"));
}

#[tokio::test]
async fn delete_item_checks_its_condition_and_expected_before_the_table() {
    let url = start().await;
    let got = refusal(
        &url,
        "DeleteItem",
        "Missing",
        json!({"ConditionExpression": ""}),
    )
    .await;
    assert_eq!(
        got,
        one("Invalid ConditionExpression: The expression can not be empty;")
    );
    let got = refusal(
        &url,
        "DeleteItem",
        "Missing",
        json!({"Expected": {"a": {"Exists": true}}}),
    )
    .await;
    assert_eq!(
        got,
        one(&format!(
            "{INVALID}Value must be provided when Exists is true for Attribute: a"
        ))
    );
}

#[tokio::test]
async fn delete_item_envelopes_a_null_false_value_wherever_it_sits() {
    let url = start().await;
    let message = one(&format!(
        "{INVALID}Null attribute value types must have the value of true"
    ));
    let got = refusal(
        &url,
        "DeleteItem",
        "Env",
        json!({"ConditionExpression": "a = :v",
            "ExpressionAttributeValues": {":v": {"NULL": false}}}),
    )
    .await;
    assert_eq!(got, message, "expression value");
    let got = refusal(
        &url,
        "DeleteItem",
        "Env",
        json!({"Key": {"pk": {"NULL": false}}}),
    )
    .await;
    assert_eq!(got, message, "key");
    let got = refusal(
        &url,
        "DeleteItem",
        "Env",
        json!({"Expected": {"a": {"Value": {"NULL": false}}}}),
    )
    .await;
    assert_eq!(got, message, "Expected value");
}

#[tokio::test]
async fn delete_item_without_an_expression_names_nothing_after_the_message() {
    let url = start().await;
    let got = refusal(
        &url,
        "DeleteItem",
        "Env",
        json!({"Expected": {"a": {"Exists": false}}, "ExpressionAttributeNames": {"#a": "a"}}),
    )
    .await;
    assert_eq!(
        got,
        one("ExpressionAttributeNames can only be specified when using expressions")
    );
}

/// The in-process API answers as the wire does.
#[test]
fn the_library_delete_item_envelopes_too() {
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value(json!({
            "TableName": "Env",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    let request = serde_json::from_value(json!({
        "TableName": "Env",
        "Key": {"pk": {"S": "a"}},
        "ConditionExpression": ""
    }))
    .unwrap();
    let err = db.delete_item(request).unwrap_err();
    assert_eq!(
        err.to_string(),
        one("Invalid ConditionExpression: The expression can not be empty;")
    );
}

/// A malformed legacy `Expected` is refused before anything is written. It
/// used to be dropped as unreadable, so the write ran with no condition at
/// all, and a DeleteItem guarded by it deleted the item (captured eu-west-2
/// and us-east-1, 2026-10-10: DynamoDB refuses each shape and writes nothing).
#[tokio::test]
async fn a_malformed_expected_is_refused_and_nothing_is_written() {
    let url = start().await;
    let cases = [
        (
            json!({"a": {"Value": {}}}),
            "Supplied AttributeValue is empty, must contain exactly one of the supported \
             datatypes"
                .to_string(),
        ),
        (
            json!({"a": {"ComparisonOperator": "BOGUS", "AttributeValueList": [{"S": "x"}]}}),
            one(&format!("{INVALID}Invalid comparison operator")),
        ),
        (
            json!({"a": {"ComparisonOperator": "EQ", "AttributeValueList": []}}),
            one(&format!(
                "{INVALID}Invalid number of argument(s) for the EQ ComparisonOperator"
            )),
        ),
    ];
    let original = json!({"pk": {"S": "a"}, "v": {"S": "original"}});
    for (expected, message) in cases {
        for operation in ["PutItem", "UpdateItem", "DeleteItem"] {
            let (status, _) = send(
                &url,
                "PutItem",
                json!({"TableName": "Env", "Item": original}),
            )
            .await;
            assert_eq!(status, 200);
            let body = match operation {
                "PutItem" => json!({
                    "TableName": "Env",
                    "Item": {"pk": {"S": "a"}, "v": {"S": "overwritten"}},
                    "Expected": expected
                }),
                "UpdateItem" => json!({
                    "TableName": "Env",
                    "Key": {"pk": {"S": "a"}},
                    "AttributeUpdates": {"v": {"Action": "PUT", "Value": {"S": "overwritten"}}},
                    "Expected": expected
                }),
                _ => json!({
                    "TableName": "Env",
                    "Key": {"pk": {"S": "a"}},
                    "Expected": expected
                }),
            };
            let (status, resp) = send(&url, operation, body).await;
            assert_eq!(status, 400, "{operation} {expected}: {resp}");
            assert_eq!(
                resp["message"].as_str().or(resp["Message"].as_str()),
                Some(message.as_str()),
                "{operation} {expected}"
            );
            let (_, got) = send(
                &url,
                "GetItem",
                json!({"TableName": "Env", "Key": {"pk": {"S": "a"}}, "ConsistentRead": true}),
            )
            .await;
            assert_eq!(
                got["Item"], original,
                "{operation} {expected} left the item alone"
            );
        }
    }
}
