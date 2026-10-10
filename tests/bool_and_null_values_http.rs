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

/// Send `body` exactly as written, for numbers serde_json would rewrite.
async fn send_text(url: &str, operation: &str, body: &str) -> (u16, Value) {
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

const NUMBER_VALUE: &str = "NUMBER_VALUE cannot be converted to Boolean";
const DECIMAL_VALUE: &str = "DECIMAL_VALUE cannot be converted to Boolean";

/// A request's `ConsistentRead` reads a string as a `BOOL` member does: a
/// string that reads as true is a strongly consistent read (1.0 unit for a
/// small item), one that reads as false or `null` an eventually consistent one
/// (0.5), and anything else is a malformed request. Captured on GetItem,
/// Query, Scan and BatchGetItem against eu-west-2 and us-east-1, 2026-10-10.
#[tokio::test]
async fn consistent_read_reads_the_strings_a_bool_reads() {
    let url = start().await;
    let (status, _) = send(
        &url,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "c"}}}),
    )
    .await;
    assert_eq!(status, 200);

    let get = |flag: Value| {
        json!({
            "TableName": TABLE, "Key": {"pk": {"S": "c"}},
            "ConsistentRead": flag, "ReturnConsumedCapacity": "TOTAL"
        })
    };
    let truthy = ["true", "TRUE", "yes", "1", "t", "y", "T", "Y"];
    let falsy = ["false", "no", "0", "f", "n", "F", "N", "No", "FALSE"];
    for (texts, units) in [(&truthy[..], 1.0), (&falsy[..], 0.5)] {
        for text in texts {
            let (status, body) = send(&url, "GetItem", get(json!(text))).await;
            assert_eq!(status, 200, "{text:?}: {body}");
            assert_eq!(
                body["ConsumedCapacity"]["CapacityUnits"],
                json!(units),
                "{text:?}"
            );
        }
    }
    let (status, body) = send(&url, "GetItem", get(Value::Null)).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["ConsumedCapacity"]["CapacityUnits"], json!(0.5));
    for text in ["abc", "off", "on", "OFF", "On", "", " true", "true "] {
        assert_eq!(
            send(&url, "GetItem", get(json!(text))).await,
            (400, serialization(Some(UNEXPECTED))),
            "{text:?}"
        );
    }
    for number in [json!(1), json!(0), json!(2)] {
        assert_eq!(
            send(&url, "GetItem", get(number.clone())).await,
            (400, serialization(Some(NUMBER_VALUE))),
            "{number}"
        );
    }

    let query = |flag: Value| {
        json!({
            "TableName": TABLE, "KeyConditionExpression": "pk = :p",
            "ExpressionAttributeValues": {":p": {"S": "c"}},
            "ConsistentRead": flag, "ReturnConsumedCapacity": "TOTAL"
        })
    };
    let scan = |flag: Value| json!({"TableName": TABLE, "ConsistentRead": flag, "ReturnConsumedCapacity": "TOTAL"});
    let batch = |flag: Value| {
        json!({
            "RequestItems": {TABLE: {"Keys": [{"pk": {"S": "c"}}], "ConsistentRead": flag}},
            "ReturnConsumedCapacity": "TOTAL"
        })
    };
    let reads: [(&str, &dyn Fn(Value) -> Value); 3] =
        [("Query", &query), ("Scan", &scan), ("BatchGetItem", &batch)];
    for (operation, request) in reads {
        for (flag, units) in [
            (json!("true"), 1.0),
            (json!("yes"), 1.0),
            (json!("1"), 1.0),
            (json!("TRUE"), 1.0),
            (json!("false"), 0.5),
            (json!("no"), 0.5),
            (json!("0"), 0.5),
            (Value::Null, 0.5),
        ] {
            let (status, body) = send(&url, operation, request(flag.clone())).await;
            assert_eq!(status, 200, "{operation} {flag}: {body}");
            let consumed = if operation == "BatchGetItem" {
                &body["ConsumedCapacity"][0]
            } else {
                &body["ConsumedCapacity"]
            };
            assert_eq!(
                consumed["CapacityUnits"],
                json!(units),
                "{operation} {flag}"
            );
        }
        for number in [json!(1), json!(0)] {
            assert_eq!(
                send(&url, operation, request(number.clone())).await,
                (400, serialization(Some(NUMBER_VALUE))),
                "{operation} {number}"
            );
        }
    }
}

/// A Query's `ScanIndexForward` reads a string the same way: one that reads as
/// false reads the sort key descending, one that reads as true ascending, and
/// `null` is the default, ascending. Captured against eu-west-2 and us-east-1,
/// 2026-10-10.
#[tokio::test]
async fn scan_index_forward_reads_the_strings_a_bool_reads() {
    let url = start().await;
    let (status, _) = send(
        &url,
        "CreateTable",
        json!({
            "TableName": "Sorted",
            "BillingMode": "PAY_PER_REQUEST",
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "N"}
            ],
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"}
            ]
        }),
    )
    .await;
    assert_eq!(status, 200);
    for sk in ["1", "2", "3"] {
        let (status, _) = send(
            &url,
            "PutItem",
            json!({"TableName": "Sorted", "Item": {"pk": {"S": "p"}, "sk": {"N": sk}}}),
        )
        .await;
        assert_eq!(status, 200);
    }
    let query = |flag: Value| {
        json!({
            "TableName": "Sorted", "KeyConditionExpression": "pk = :p",
            "ExpressionAttributeValues": {":p": {"S": "p"}}, "ScanIndexForward": flag
        })
    };
    let order = |body: &Value| -> Vec<String> {
        body["Items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["sk"]["N"].as_str().unwrap().to_string())
            .collect()
    };
    for (flag, expected) in [
        (json!("false"), ["3", "2", "1"]),
        (json!("no"), ["3", "2", "1"]),
        (json!("0"), ["3", "2", "1"]),
        (json!("f"), ["3", "2", "1"]),
        (json!("N"), ["3", "2", "1"]),
        (json!("FALSE"), ["3", "2", "1"]),
        (json!("true"), ["1", "2", "3"]),
        (json!("T"), ["1", "2", "3"]),
        (json!("Y"), ["1", "2", "3"]),
        (Value::Null, ["1", "2", "3"]),
        (json!(false), ["3", "2", "1"]),
    ] {
        let (status, body) = send(&url, "Query", query(flag.clone())).await;
        assert_eq!(status, 200, "{flag}: {body}");
        assert_eq!(order(&body), expected, "{flag}");
    }
    for (flag, refusal) in [
        (json!(0), serialization(Some(NUMBER_VALUE))),
        (json!("abc"), serialization(Some(UNEXPECTED))),
        (json!("off"), serialization(Some(UNEXPECTED))),
        (json!(""), serialization(Some(UNEXPECTED))),
    ] {
        assert_eq!(
            send(&url, "Query", query(flag.clone())).await,
            (400, refusal),
            "{flag}"
        );
    }
    let mut both = query(json!("abc"));
    both["ConsistentRead"] = json!("abc");
    assert_eq!(
        send(&url, "Query", both).await,
        (400, serialization(Some(UNEXPECTED)))
    );
}

/// A number given for a boolean is named by how it is written: a whole number
/// of any size, `-0` included, is `NUMBER_VALUE`, and one with a point or an
/// exponent `DECIMAL_VALUE`, whether it is a `BOOL`, a `NULL` or
/// `ConsistentRead`. Captured against eu-west-2 and us-east-1, 2026-10-10.
#[tokio::test]
async fn a_number_for_a_boolean_is_named_by_how_it_is_written() {
    let url = start().await;
    let whole = [
        "-0",
        "0",
        "1",
        "2",
        "-1",
        "18446744073709551616",
        "9223372036854775808",
        "9223372036854775807",
        "-9223372036854775809",
        "4294967296",
    ];
    let decimal = ["1.0", "0.0", "-0.0", "1e30", "1E0"];
    for (numbers, message) in [(&whole[..], NUMBER_VALUE), (&decimal[..], DECIMAL_VALUE)] {
        for number in numbers {
            for member in ["BOOL", "NULL"] {
                let body = format!(
                    r#"{{"TableName": "{TABLE}", "Item": {{"pk": {{"S": "x"}}, "v": {{"{member}": {number}}}}}}}"#
                );
                assert_eq!(
                    send_text(&url, "PutItem", &body).await,
                    (400, serialization(Some(message))),
                    "{member} {number}"
                );
            }
            let body = format!(
                r#"{{"TableName": "{TABLE}", "Key": {{"pk": {{"S": "x"}}}}, "ConsistentRead": {number}}}"#
            );
            assert_eq!(
                send_text(&url, "GetItem", &body).await,
                (400, serialization(Some(message))),
                "ConsistentRead {number}"
            );
        }
    }
}

/// Put the four items the legacy-condition captures read: `e1` with `a` 1 and
/// `b` true, `e2` with `b` false, `e3` with `b` the string `x`, `e4` with `a` 2.
async fn put_legacy_items(url: &str) {
    for item in [
        json!({"pk": {"S": "e1"}, "a": {"N": "1"}, "b": {"BOOL": true}}),
        json!({"pk": {"S": "e2"}, "b": {"BOOL": false}}),
        json!({"pk": {"S": "e3"}, "b": {"S": "x"}}),
        json!({"pk": {"S": "e4"}, "a": {"N": "2"}}),
    ] {
        let (status, body) = send(url, "PutItem", json!({"TableName": TABLE, "Item": item})).await;
        assert_eq!(status, 200, "{body}");
    }
}

/// The four places a legacy condition is written, each holding `condition`
/// on attribute `b` (or the partition key, for KeyConditions).
fn legacy_requests(condition: &Value) -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "QueryFilter",
            "Query",
            json!({
                "TableName": TABLE,
                "KeyConditions": {"pk": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "e1"}]}},
                "QueryFilter": {"b": condition}
            }),
        ),
        (
            "ScanFilter",
            "Scan",
            json!({"TableName": TABLE, "ScanFilter": {"b": condition}}),
        ),
        (
            "KeyConditions",
            "Query",
            json!({"TableName": TABLE, "KeyConditions": {"pk": condition}}),
        ),
        (
            "Expected",
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": {"S": "e9"}}, "Expected": {"b": condition}}),
        ),
    ]
}

/// A legacy condition whose value is a NULL that reads as false is refused,
/// whatever its operator and wherever it is written, ahead of the operator's
/// own checks. It used to be dropped from a QueryFilter or ScanFilter, so the
/// read ran unfiltered, and to leave a Query with no key condition. The
/// refusal is bare, except JSON `false` in `Expected`, which keeps the
/// envelope. Captured against eu-west-2 and us-east-1, 2026-10-10.
#[tokio::test]
async fn a_legacy_condition_refuses_a_null_that_reads_as_false() {
    let url = start().await;
    put_legacy_items(&url).await;
    for flag in [json!(false), json!("no"), json!("false")] {
        for operator in ["EQ", "NE", "IN", "CONTAINS", "NULL", "NOT_NULL"] {
            let condition = json!({
                "ComparisonOperator": operator,
                "AttributeValueList": [{"NULL": flag}]
            });
            for (place, operation, request) in legacy_requests(&condition) {
                let refusal = if place == "Expected" && flag == json!(false) {
                    enveloped(NULL_FALSE)
                } else {
                    validation(NULL_FALSE)
                };
                assert_eq!(
                    send(&url, operation, request).await,
                    (400, refusal),
                    "{place} {operator} {flag}"
                );
            }
        }
    }
    // The Value form of Expected too.
    for (flag, refusal) in [
        (json!(false), enveloped(NULL_FALSE)),
        (json!("no"), validation(NULL_FALSE)),
    ] {
        assert_eq!(
            send(
                &url,
                "PutItem",
                json!({"TableName": TABLE, "Item": {"pk": {"S": "e9"}},
                    "Expected": {"b": {"Value": {"NULL": flag}}}})
            )
            .await,
            (400, refusal),
            "Value {flag}"
        );
    }
}

/// A BOOL or NULL in a legacy condition that doesn't read as a boolean makes
/// the whole request malformed, whatever its operator and wherever it is
/// written. Captured against eu-west-2 and us-east-1, 2026-10-10. That it is
/// refused ahead of the request's other checks, a `Limit` of 0 here, is
/// inferred: DynamoDB fails the request as it is read.
#[tokio::test]
async fn a_legacy_condition_with_a_malformed_boolean_fails_the_request() {
    let url = start().await;
    put_legacy_items(&url).await;
    for operator in ["EQ", "NE", "IN", "CONTAINS", "NULL", "NOT_NULL"] {
        let condition = json!({
            "ComparisonOperator": operator,
            "AttributeValueList": [{"BOOL": "maybe"}]
        });
        for (place, operation, request) in legacy_requests(&condition) {
            assert_eq!(
                send(&url, operation, request).await,
                (400, serialization(Some(UNEXPECTED))),
                "{place} {operator}"
            );
        }
    }
    assert_eq!(
        send(
            &url,
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": {"S": "e9"}},
                "Expected": {"b": {"Value": {"BOOL": "maybe"}}}})
        )
        .await,
        (400, serialization(Some(UNEXPECTED)))
    );
    for (operation, request) in [
        (
            "Query",
            json!({
                "TableName": TABLE, "Limit": 0,
                "KeyConditions": {"pk": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "e1"}]}},
                "QueryFilter": {"b": {"ComparisonOperator": "EQ", "AttributeValueList": [{"BOOL": "maybe"}]}}
            }),
        ),
        (
            "Scan",
            json!({
                "TableName": TABLE, "Limit": 0,
                "ScanFilter": {"b": {"ComparisonOperator": "EQ", "AttributeValueList": [{"NULL": "maybe"}]}}
            }),
        ),
    ] {
        assert_eq!(
            send(&url, operation, request).await,
            (400, serialization(Some(UNEXPECTED))),
            "{operation}"
        );
    }
    // A BOOL that does read filters as the boolean it spells.
    let (status, body) = send(
        &url,
        "Scan",
        json!({"TableName": TABLE, "ScanFilter": {"b": {
            "ComparisonOperator": "EQ", "AttributeValueList": [{"BOOL": "no"}]
        }}}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["Items"],
        json!([{"pk": {"S": "e2"}, "b": {"BOOL": false}}])
    );
}

/// A legacy `IN` names the type of every value in its list, and a value whose
/// other member is JSON `null` is the type of the member it has. Captured
/// against eu-west-2 and us-east-1, 2026-10-10.
#[tokio::test]
async fn a_legacy_in_checks_the_type_of_every_value() {
    let url = start().await;
    put_legacy_items(&url).await;
    let in_bool = "One or more parameter values were invalid: ComparisonOperator IN is not valid for BOOL AttributeValue type";
    let scan = |operator: &str, values: Value| {
        json!({"TableName": TABLE, "ScanFilter": {"b": {
            "ComparisonOperator": operator, "AttributeValueList": values
        }}})
    };
    for values in [
        json!([{"BOOL": true, "S": null}]),
        json!([{"S": null, "BOOL": true}]),
        json!([{"S": "x"}, {"BOOL": true, "S": null}]),
        json!([{"S": "x"}, {"S": null, "BOOL": true}]),
        json!([{"BOOL": true}]),
    ] {
        assert_eq!(
            send(&url, "Scan", scan("IN", values.clone())).await,
            (400, validation(in_bool)),
            "{values}"
        );
    }
    let (status, body) = send(&url, "Scan", scan("EQ", json!([{"BOOL": true, "S": null}]))).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["Count"], json!(1));
    assert_eq!(body["Items"][0]["pk"], json!({"S": "e1"}));

    // Expected checks the same, in the envelope unless the request reads a
    // boolean from a string.
    for operation in ["PutItem", "DeleteItem"] {
        let request = |expected: Value| {
            let mut request = if operation == "PutItem" {
                json!({"TableName": TABLE, "Item": {"pk": {"S": "e1"}, "a": {"N": "1"}, "b": {"BOOL": true}}})
            } else {
                json!({"TableName": TABLE, "Key": {"pk": {"S": "e5"}}})
            };
            request["Expected"] = expected;
            request
        };
        let in_list = |values: Value| json!({"b": {"ComparisonOperator": "IN", "AttributeValueList": values}});
        for (expected, refusal) in [
            (in_list(json!([{"BOOL": true}])), enveloped(in_bool)),
            (in_list(json!([{"BOOL": "true"}])), validation(in_bool)),
            (
                in_list(json!([{"S": "x"}, {"BOOL": "true"}])),
                validation(in_bool),
            ),
            (
                in_list(json!([{"NULL": true}])),
                enveloped(
                    "One or more parameter values were invalid: ComparisonOperator IN is not valid for NULL AttributeValue type",
                ),
            ),
        ] {
            assert_eq!(
                send(&url, operation, request(expected.clone())).await,
                (400, refusal),
                "{operation} {expected}"
            );
        }
    }
}

/// Every refusal of a legacy `Expected` comes back bare once the request reads
/// a boolean from a string, even one in another condition, and in the
/// envelope otherwise. Captured on PutItem and DeleteItem against eu-west-2
/// and us-east-1, 2026-10-10.
#[tokio::test]
async fn an_expected_reading_a_string_boolean_refuses_bare() {
    let url = start().await;
    put_legacy_items(&url).await;
    let null_count = "One or more parameter values were invalid: Invalid number of argument(s) for the NULL ComparisonOperator";
    let exists_false = "One or more parameter values were invalid: Value cannot be used when Exists is false for Attribute: b";
    for operation in ["PutItem", "DeleteItem"] {
        let request = |expected: Value| {
            let mut request = if operation == "PutItem" {
                json!({"TableName": TABLE, "Item": {"pk": {"S": "e1"}, "a": {"N": "1"}, "b": {"BOOL": true}}})
            } else {
                json!({"TableName": TABLE, "Key": {"pk": {"S": "e5"}}})
            };
            request["Expected"] = expected;
            request
        };
        for (expected, refusal) in [
            (
                json!({"b": {"ComparisonOperator": "NULL", "AttributeValueList": [{"S": "x"}]}}),
                enveloped(null_count),
            ),
            (
                json!({
                    "b": {"ComparisonOperator": "EQ", "AttributeValueList": [{"BOOL": "no"}]},
                    "nl": {"ComparisonOperator": "NULL", "AttributeValueList": [{"NULL": true}]}
                }),
                validation(null_count),
            ),
            (
                json!({"b": {"Exists": false, "Value": {"BOOL": "no"}}}),
                validation(exists_false),
            ),
            (
                json!({"b": {"Exists": false, "Value": {"BOOL": true}}}),
                enveloped(exists_false),
            ),
        ] {
            assert_eq!(
                send(&url, operation, request(expected.clone())).await,
                (400, refusal),
                "{operation} {expected}"
            );
        }
    }
}

/// A legacy condition's `Exists` reads a string as a `BOOL` member does, and
/// one read from a string makes the request's refusals bare. Captured on
/// PutItem against eu-west-2 and us-east-1, 2026-10-10.
#[tokio::test]
async fn exists_reads_the_strings_a_bool_reads() {
    let url = start().await;
    put_legacy_items(&url).await;
    let ccf = json!({
        "__type": "com.amazonaws.dynamodb.v20120810#ConditionalCheckFailedException",
        "message": "The conditional request failed"
    });
    let must = |when: &str| {
        format!(
            "One or more parameter values were invalid: Value must be provided when Exists is {when} for Attribute: a"
        )
    };
    let cannot = "One or more parameter values were invalid: Value cannot be used when Exists is false for Attribute: a";
    // e1 has `a`, e2 doesn't. Each write puts the item back as it was.
    let write = |pk: &str, exists: Value, value: bool| {
        let item = if pk == "e1" {
            json!({"pk": {"S": "e1"}, "a": {"N": "1"}, "b": {"BOOL": true}})
        } else {
            json!({"pk": {"S": "e2"}, "b": {"BOOL": false}})
        };
        let mut condition = json!({"Exists": exists});
        if value {
            condition["Value"] = json!({"N": "1"});
        }
        json!({"TableName": TABLE, "Item": item, "Expected": {"a": condition}})
    };
    let ok = (200, json!({}));
    for text in ["false", "no", "0", "f"] {
        let exists = json!(text);
        assert_eq!(
            send(&url, "PutItem", write("e1", exists.clone(), false)).await,
            (400, ccf.clone()),
            "{text}"
        );
        assert_eq!(
            send(&url, "PutItem", write("e2", exists.clone(), false)).await,
            ok,
            "{text}"
        );
        for pk in ["e1", "e2"] {
            assert_eq!(
                send(&url, "PutItem", write(pk, exists.clone(), true)).await,
                (400, validation(cannot)),
                "{text} {pk}"
            );
        }
    }
    for text in ["true", "TRUE", "yes"] {
        let exists = json!(text);
        for pk in ["e1", "e2"] {
            assert_eq!(
                send(&url, "PutItem", write(pk, exists.clone(), false)).await,
                (400, validation(&must("true"))),
                "{text} {pk}"
            );
        }
        assert_eq!(
            send(&url, "PutItem", write("e1", exists.clone(), true)).await,
            ok,
            "{text}"
        );
        assert_eq!(
            send(&url, "PutItem", write("e2", exists.clone(), true)).await,
            (400, ccf.clone()),
            "{text}"
        );
    }
    // JSON booleans and null keep the envelope.
    assert_eq!(
        send(&url, "PutItem", write("e1", json!(false), true)).await,
        (400, enveloped(cannot))
    );
    assert_eq!(
        send(&url, "PutItem", write("e2", json!(true), false)).await,
        (400, enveloped(&must("true")))
    );
    assert_eq!(
        send(&url, "PutItem", write("e2", Value::Null, false)).await,
        (400, enveloped(&must("null")))
    );
    assert_eq!(
        send(&url, "PutItem", write("e1", Value::Null, true)).await,
        ok
    );
    // Anything else is a malformed request.
    for pk in ["e1", "e2"] {
        for value in [false, true] {
            assert_eq!(
                send(&url, "PutItem", write(pk, json!("maybe"), value)).await,
                (400, serialization(Some(UNEXPECTED))),
                "maybe {pk} {value}"
            );
            assert_eq!(
                send(&url, "PutItem", write(pk, json!(1), value)).await,
                (400, serialization(Some(NUMBER_VALUE))),
                "1 {pk} {value}"
            );
        }
    }
}
