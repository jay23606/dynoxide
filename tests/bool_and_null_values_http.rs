//! How a `BOOL` or `NULL` member is read, on the wire.
//!
//! DynamoDB reads either member's value as a boolean leniently: besides JSON
//! `true` and `false` it takes the strings `true`, `t`, `yes`, `y` and `1` as
//! true and `false`, `f`, `no`, `n` and `0` as false, in any case, and refuses
//! any other string, a number or a list as a malformed request. A member whose
//! value is JSON `null` counts as absent. A `NULL` that reads as false is
//! refused as it is parsed when it was a string, or sits beside another member,
//! and so without the envelope a lone `{"NULL": false}` gets on PutItem,
//! UpdateItem and DeleteItem.
//!
//! Every expectation below was captured against real DynamoDB in eu-west-2 and
//! us-east-1 on 2026-10-10, and the two regions agree on each one.

#![cfg(feature = "http-server")]

use dynoxide::Database;
use serde_json::{Value, json};

const TABLE: &str = "Values";
const UNEXPECTED: &str = "Unexpected token received from parser";
const EMPTY: &str =
    "Supplied AttributeValue is empty, must contain exactly one of the supported datatypes";
const NULL_FALSE: &str = "One or more parameter values were invalid: Null attribute value types must have the value of true";

async fn start() -> String {
    let db = Database::memory().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        dynoxide::server::serve_on(listener, db).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let url = format!("http://{addr}");
    let (status, _) = send(
        &url,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "BillingMode": "PAY_PER_REQUEST",
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}]
        }),
    )
    .await;
    assert_eq!(status, 200);
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
    let text = resp.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn serialization(message: Option<&str>) -> Value {
    let mut body = json!({"__type": "com.amazon.coral.service#SerializationException"});
    if let Some(message) = message {
        body["Message"] = json!(message);
    }
    body
}

fn validation(message: &str) -> Value {
    json!({"__type": "com.amazon.coral.validate#ValidationException", "message": message})
}

fn enveloped(message: &str) -> Value {
    validation(&format!("1 validation error detected: {message}"))
}

fn for_key(message: &str, key: &str) -> Value {
    validation(&format!(
        "ExpressionAttributeValues contains invalid value: {message} for key {key}"
    ))
}

/// Put `value` as attribute `v` of a new item and read back what was stored,
/// or return the refusal.
async fn put_value(url: &str, pk: &str, value: Value) -> Result<Value, (u16, Value)> {
    let (status, body) = send(
        url,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": pk}, "v": value}}),
    )
    .await;
    if status != 200 {
        return Err((status, body));
    }
    let (_, item) = send(
        url,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": pk}}, "ConsistentRead": true}),
    )
    .await;
    Ok(item["Item"]["v"].clone())
}

#[tokio::test]
async fn a_bool_reads_the_strings_dynamodb_reads_as_booleans() {
    let url = start().await;
    let truthy = [
        "t", "T", "y", "Y", "YES", "yEs", "TrUe", "true", "TRUE", "yes", "1",
    ];
    let falsy = [
        "false", "FALSE", "False", "fAlSe", "0", "n", "N", "No", "NO", "f", "no",
    ];
    for (i, text) in truthy.iter().enumerate() {
        assert_eq!(
            put_value(&url, &format!("t{i}"), json!({"BOOL": text})).await,
            Ok(json!({"BOOL": true})),
            "{text:?}"
        );
    }
    for (i, text) in falsy.iter().enumerate() {
        assert_eq!(
            put_value(&url, &format!("f{i}"), json!({"BOOL": text})).await,
            Ok(json!({"BOOL": false})),
            "{text:?}"
        );
    }
}

#[tokio::test]
async fn a_bool_refuses_anything_else_as_a_malformed_request() {
    let url = start().await;
    let refused_strings = [
        "off", "OFF", "on", "abc", " ", "nope", "falsey", "2", "-1", "null", "", "1.0", "01", "00",
        " no", "no ",
    ];
    for text in refused_strings {
        assert_eq!(
            put_value(&url, "x", json!({"BOOL": text})).await,
            Err((400, serialization(Some(UNEXPECTED)))),
            "{text:?}"
        );
    }
    let cases = [
        (
            json!(1),
            serialization(Some("NUMBER_VALUE cannot be converted to Boolean")),
        ),
        (
            json!(0),
            serialization(Some("NUMBER_VALUE cannot be converted to Boolean")),
        ),
        (
            json!(1.5),
            serialization(Some("DECIMAL_VALUE cannot be converted to Boolean")),
        ),
        (json!([]), serialization(None)),
        (
            json!({}),
            serialization(Some("Start of structure or map found where not expected")),
        ),
        (Value::Null, validation(EMPTY)),
    ];
    for (value, refusal) in cases {
        assert_eq!(
            put_value(&url, "x", json!({"BOOL": value})).await,
            Err((400, refusal)),
            "{value}"
        );
    }
}

#[tokio::test]
async fn a_null_reads_the_same_strings_and_must_read_as_true() {
    let url = start().await;
    for (i, text) in ["true", "yes", "1", "t", "TRUE"].iter().enumerate() {
        assert_eq!(
            put_value(&url, &format!("n{i}"), json!({"NULL": text})).await,
            Ok(json!({"NULL": true})),
            "{text:?}"
        );
    }
    let cases = [
        (json!("abc"), serialization(Some(UNEXPECTED))),
        (json!(""), serialization(Some(UNEXPECTED))),
        (
            json!(0),
            serialization(Some("NUMBER_VALUE cannot be converted to Boolean")),
        ),
        (
            json!(1.5),
            serialization(Some("DECIMAL_VALUE cannot be converted to Boolean")),
        ),
        (json!([]), serialization(None)),
        // A string that reads as false is refused as the value is parsed, bare.
        (json!("false"), validation(NULL_FALSE)),
        (json!("no"), validation(NULL_FALSE)),
        (json!("0"), validation(NULL_FALSE)),
        (json!("n"), validation(NULL_FALSE)),
        // JSON false is refused by the request's validation, in the envelope.
        (json!(false), enveloped(NULL_FALSE)),
    ];
    for (value, refusal) in cases {
        assert_eq!(
            put_value(&url, "x", json!({"NULL": value})).await,
            Err((400, refusal)),
            "{value}"
        );
    }
    // Nested, the same split holds.
    let nested = [
        (json!({"L": [{"NULL": "false"}]}), validation(NULL_FALSE)),
        (json!({"M": {"k": {"NULL": "no"}}}), validation(NULL_FALSE)),
        (json!({"L": [{"NULL": false}]}), enveloped(NULL_FALSE)),
    ];
    for (value, refusal) in nested {
        assert_eq!(
            put_value(&url, "x", value.clone()).await,
            Err((400, refusal)),
            "{value}"
        );
    }
}

#[tokio::test]
async fn a_member_set_to_null_counts_as_absent() {
    let url = start().await;
    for member in ["S", "N", "L", "M", "SS"] {
        assert_eq!(
            put_value(&url, "x", json!({member: null})).await,
            Err((400, validation(EMPTY))),
            "{member}"
        );
    }
    assert_eq!(
        put_value(&url, "x", json!({"S": null, "N": null})).await,
        Err((400, validation(EMPTY)))
    );
    assert_eq!(
        put_value(&url, "a", json!({"S": "a", "N": null})).await,
        Ok(json!({"S": "a"}))
    );
    assert_eq!(
        put_value(&url, "b", json!({"NULL": null, "BOOL": true})).await,
        Ok(json!({"BOOL": true}))
    );
}

#[tokio::test]
async fn each_member_is_read_before_the_value_is_checked_for_one_type() {
    let url = start().await;
    let cases = [
        (
            json!({"S": "a", "BOOL": "yes"}),
            validation(
                "Supplied AttributeValue has more than one datatypes set, \
                 must contain exactly one of the supported datatypes",
            ),
        ),
        // A malformed member is reported ahead of the second member.
        (
            json!({"BOOL": "abc", "S": null}),
            serialization(Some(UNEXPECTED)),
        ),
        (
            json!({"BOOL": "abc", "S": "a"}),
            serialization(Some(UNEXPECTED)),
        ),
        (
            json!({"NULL": 1, "S": "a"}),
            serialization(Some("NUMBER_VALUE cannot be converted to Boolean")),
        ),
        // So is a NULL that reads as false, bare even when it was JSON false.
        (json!({"BOOL": "yes", "NULL": "no"}), validation(NULL_FALSE)),
        (json!({"NULL": false, "S": "a"}), validation(NULL_FALSE)),
        (json!({"N": "1", "NULL": false}), validation(NULL_FALSE)),
        (
            json!({"BOOL": "yes", "NULL": false}),
            validation(NULL_FALSE),
        ),
    ];
    for (value, refusal) in cases {
        assert_eq!(
            put_value(&url, "x", value.clone()).await,
            Err((400, refusal)),
            "{value}"
        );
    }
}

#[tokio::test]
async fn the_same_reading_applies_to_keys_and_expression_values() {
    let url = start().await;
    let key = json!({"pk": {"S": "a"}});
    assert_eq!(
        send(&url, "PutItem", json!({"TableName": TABLE, "Item": key}))
            .await
            .0,
        200
    );
    let update = |name: &str, value: Value| {
        json!({
            "TableName": TABLE,
            "Key": {"pk": {"S": "a"}},
            "UpdateExpression": format!("SET {name} = :v"),
            "ExpressionAttributeValues": {":v": value}
        })
    };
    assert_eq!(
        send(&url, "UpdateItem", update("u", json!({"BOOL": "yes"})))
            .await
            .0,
        200
    );
    assert_eq!(
        send(&url, "UpdateItem", update("u2", json!({"NULL": "TRUE"})))
            .await
            .0,
        200
    );
    let (_, item) = send(
        &url,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "a"}}, "ConsistentRead": true}),
    )
    .await;
    assert_eq!(item["Item"]["u"], json!({"BOOL": true}));
    assert_eq!(item["Item"]["u2"], json!({"NULL": true}));

    let cases = [
        (
            "UpdateItem",
            update("u3", json!({"BOOL": "abc"})),
            serialization(Some(UNEXPECTED)),
        ),
        (
            "UpdateItem",
            update("u4", json!({"NULL": false})),
            enveloped(NULL_FALSE),
        ),
        (
            "UpdateItem",
            update("u5", json!({"NULL": "false"})),
            for_key(NULL_FALSE, ":v"),
        ),
        (
            "DeleteItem",
            json!({
                "TableName": TABLE, "Key": {"pk": {"S": "zz"}}, "ConditionExpression": "v = :v",
                "ExpressionAttributeValues": {":v": {"NULL": "no"}}
            }),
            for_key(NULL_FALSE, ":v"),
        ),
        (
            "DeleteItem",
            json!({
                "TableName": TABLE, "Key": {"pk": {"S": "zz"}}, "ConditionExpression": "v = :v",
                "ExpressionAttributeValues": {":v": {"NULL": false}}
            }),
            enveloped(NULL_FALSE),
        ),
        (
            "Query",
            json!({
                "TableName": TABLE, "KeyConditionExpression": "pk = :p", "FilterExpression": "v = :v",
                "ExpressionAttributeValues": {":p": {"S": "a"}, ":v": {"NULL": "no"}}
            }),
            for_key(NULL_FALSE, ":v"),
        ),
        (
            "Query",
            json!({
                "TableName": TABLE, "KeyConditionExpression": "pk = :v",
                "ExpressionAttributeValues": {":v": {"S": null}}
            }),
            for_key(EMPTY, ":v"),
        ),
        (
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": null}}}),
            validation(EMPTY),
        ),
        (
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"NULL": "no"}}}),
            validation(NULL_FALSE),
        ),
    ];
    for (operation, request, refusal) in cases {
        assert_eq!(
            send(&url, operation, request.clone()).await,
            (400, refusal),
            "{operation} {request}"
        );
    }
}

#[tokio::test]
async fn batch_and_transaction_writes_read_the_strings_too() {
    let url = start().await;
    let (status, _) = send(
        &url,
        "BatchWriteItem",
        json!({"RequestItems": {TABLE: [
            {"PutRequest": {"Item": {"pk": {"S": "bw"}, "v": {"BOOL": "n"}}}}
        ]}}),
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = send(
        &url,
        "TransactWriteItems",
        json!({"TransactItems": [
            {"Put": {"TableName": TABLE, "Item": {"pk": {"S": "tw"}, "v": {"NULL": "yes"}}}}
        ]}),
    )
    .await;
    assert_eq!(status, 200);
    for (pk, stored) in [
        ("bw", json!({"BOOL": false})),
        ("tw", json!({"NULL": true})),
    ] {
        let (_, item) = send(
            &url,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": pk}}, "ConsistentRead": true}),
        )
        .await;
        assert_eq!(item["Item"]["v"], stored, "{pk}");
    }
}

#[tokio::test]
async fn a_partiql_parameter_reads_the_strings_too() {
    let url = start().await;
    assert_eq!(
        send(
            &url,
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": {"S": "p0"}}})
        )
        .await
        .0,
        200
    );
    let set = |name: &str, value: Value| {
        json!({
            "Statement": format!(r#"UPDATE "{TABLE}" SET {name} = ? WHERE pk = 'p0'"#),
            "Parameters": [value]
        })
    };
    for (name, value) in [
        ("w", json!({"NULL": "true"})),
        ("w2", json!({"BOOL": "t"})),
        ("w3", json!({"BOOL": "no"})),
    ] {
        assert_eq!(
            send(&url, "ExecuteStatement", set(name, value)).await,
            (200, json!({"Items": []}))
        );
    }
    let (_, item) = send(
        &url,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "p0"}}, "ConsistentRead": true}),
    )
    .await;
    assert_eq!(item["Item"]["w"], json!({"NULL": true}));
    assert_eq!(item["Item"]["w2"], json!({"BOOL": true}));
    assert_eq!(item["Item"]["w3"], json!({"BOOL": false}));

    assert_eq!(
        send(&url, "ExecuteStatement", set("w4", json!({"BOOL": 1}))).await,
        (
            400,
            serialization(Some("NUMBER_VALUE cannot be converted to Boolean"))
        )
    );
    assert_eq!(
        send(
            &url,
            "ExecuteStatement",
            json!({
                "Statement": format!(r#"SELECT * FROM "{TABLE}" WHERE v = ?"#),
                "Parameters": [{"NULL": "false"}]
            })
        )
        .await,
        (400, validation(NULL_FALSE))
    );

    // In a batch, a value refused as malformed fails the request; one refused
    // as invalid fails only its own statement.
    let good = json!({"Statement": format!(r#"UPDATE "{TABLE}" SET w5 = 1 WHERE pk = 'p0'"#)});
    let batch = |value: Value| {
        json!({"Statements": [
            {
                "Statement": format!(r#"UPDATE "{TABLE}" SET w6 = ? WHERE pk = 'p1'"#),
                "Parameters": [value]
            },
            good.clone()
        ]})
    };
    for (value, refusal) in [
        (json!({"BOOL": "abc"}), serialization(Some(UNEXPECTED))),
        (
            json!({"NULL": 1}),
            serialization(Some("NUMBER_VALUE cannot be converted to Boolean")),
        ),
    ] {
        assert_eq!(
            send(&url, "BatchExecuteStatement", batch(value.clone())).await,
            (400, refusal),
            "{value}"
        );
    }
    for (value, message) in [
        (json!({"S": null}), EMPTY),
        (json!({"NULL": null}), EMPTY),
        (json!({"NULL": "no"}), NULL_FALSE),
    ] {
        assert_eq!(
            send(&url, "BatchExecuteStatement", batch(value.clone())).await,
            (
                200,
                json!({"Responses": [
                    {"Error": {"Code": "ValidationError", "Message": message}},
                    {"TableName": TABLE}
                ]})
            ),
            "{value}"
        );
    }
}
