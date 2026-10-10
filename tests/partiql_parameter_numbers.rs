//! A PartiQL number parameter is read the way Java's `BigDecimal` reads it.
//!
//! An exponent outside an `int`, or a scale (digits after the point less the
//! exponent) outside one, can't be converted at all, and the message names
//! the text. Otherwise the precision less the scale is range-checked as for a
//! PartiQL literal, so `1e2147483647` is an underflow where PutItem calls it an
//! overflow. The rule is the same in every position a parameter can stand, in
//! a list, a map or a number set, and in transactions and batches (captured
//! eu-west-2 and us-east-1, 2026-10-09).

use dynoxide::Database;
use dynoxide::actions::batch_execute_statement::{
    BatchExecuteStatementRequest, BatchStatementRequest,
};
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::ExecuteStatementRequest;
use dynoxide::actions::execute_transaction::{ExecuteTransactionRequest, ParameterizedStatement};
use dynoxide::errors::DynoxideError;
use dynoxide::types::ScalarAttributeType;
use dynoxide::types::{AttributeDefinition, AttributeValue, KeySchemaElement, KeyType};
use std::collections::HashMap;

const UNDERFLOW: &str =
    "Number underflow. Attempting to store a number with magnitude smaller than supported range";
const OVERFLOW: &str =
    "Number overflow. Attempting to store a number with magnitude larger than supported range";

/// Shapes that can't be converted: an exponent outside an `int`, or a scale
/// outside one.
const UNCONVERTIBLE: &[&str] = &[
    "1e99999999999",
    "-1e99999999999",
    "1e-99999999999",
    "1e2147483648",
    "1e-2147483648",
    "0.1e-2147483647",
    "-0e-2147483648",
    "0e-2147483648",
];

/// Shapes whose precision less scale reaches an `int`'s limit, or whose
/// magnitude is below the range.
const UNDERFLOWING: &[&str] = &[
    "1e2147483647",
    "-1e2147483647",
    "0e2147483647",
    "1e2147483646",
    "10e2147483646",
    "12e2147483647",
    "1.5e2147483647",
    "1E+2147483647",
    "00001e2147483647",
    "1e-2147483647",
];

const OVERFLOWING: &[&str] = &["1e2147483645", "1e126"];

fn expected(shape: &str) -> String {
    if UNCONVERTIBLE.contains(&shape) {
        format!("The parameter cannot be converted to a numeric value: {shape}")
    } else if UNDERFLOWING.contains(&shape) {
        UNDERFLOW.to_string()
    } else if OVERFLOWING.contains(&shape) {
        OVERFLOW.to_string()
    } else if shape.is_empty() {
        "The parameter cannot be converted to a numeric value".to_string()
    } else {
        panic!("no expectation for {shape}")
    }
}

fn shapes() -> impl Iterator<Item = &'static str> {
    UNCONVERTIBLE
        .iter()
        .chain(UNDERFLOWING)
        .chain(OVERFLOWING)
        .chain(std::iter::once(&""))
        .copied()
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
    db
}

fn refusal(db: &Database, statement: &str, parameter: AttributeValue) -> String {
    match db.execute_statement(ExecuteStatementRequest {
        statement: statement.to_string(),
        parameters: Some(vec![parameter]),
        ..Default::default()
    }) {
        Err(DynoxideError::ValidationException(m)) => m,
        other => panic!("{statement}: expected a ValidationException, got {other:?}"),
    }
}

#[test]
fn a_parameter_number_is_read_as_bigdecimal_reads_it_in_every_position() {
    let db = db();
    for shape in shapes() {
        for statement in [
            "SELECT * FROM \"Things\" WHERE pk = 'a' AND n = ?",
            "INSERT INTO \"Things\" VALUE {'pk': 'p', 'n': ?}",
            "UPDATE \"Things\" SET n = ? WHERE pk = 'a'",
            "SELECT * FROM \"Things\" WHERE pk = ?",
        ] {
            assert_eq!(
                refusal(&db, statement, n(shape)),
                expected(shape),
                "{statement} with {shape:?}"
            );
        }
        let statement = "SELECT * FROM \"Things\" WHERE pk = 'a' AND n = ?";
        if !shape.is_empty() {
            for nested in [
                AttributeValue::L(vec![n("1"), n(shape)]),
                AttributeValue::M(HashMap::from([("x".to_string(), n(shape))])),
                AttributeValue::NS(vec!["1".to_string(), shape.to_string()]),
            ] {
                assert_eq!(
                    refusal(&db, statement, nested.clone()),
                    expected(shape),
                    "{shape:?} in {nested:?}"
                );
            }
        }
    }
}

#[test]
fn transactions_and_batches_read_a_parameter_number_the_same_way() {
    let db = db();
    let err = db
        .execute_transaction(ExecuteTransactionRequest {
            transact_statements: vec![ParameterizedStatement {
                statement: "UPDATE \"Things\" SET n = ? WHERE pk = 'a'".to_string(),
                parameters: Some(vec![n("1e99999999999")]),
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap_err()
        .to_string();
    assert!(
        err.ends_with("The parameter cannot be converted to a numeric value: 1e99999999999"),
        "{err}"
    );

    let mut member = BatchStatementRequest::default();
    member.statement = "SELECT * FROM \"Things\" WHERE pk = 'a' AND n = ?".to_string();
    member.parameters = Some(vec![n("1e2147483647")]);
    let response = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            statements: vec![member],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        response.responses[0].error.as_ref().unwrap().message,
        UNDERFLOW
    );
}

/// The same numbers sent over the wire, where they are checked as the request
/// is decoded.
#[cfg(feature = "http-server")]
mod over_http {
    use super::*;
    use serde_json::{Value, json};

    async fn start() -> String {
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

    fn message(body: &Value) -> String {
        body.get("message")
            .or_else(|| body.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    #[tokio::test]
    async fn a_parameter_number_on_the_wire_is_read_as_bigdecimal_reads_it() {
        let url = start().await;
        for shape in shapes() {
            for parameter in [
                json!({"N": shape}),
                json!({"L": [{"N": "1"}, {"N": shape}]}),
                json!({"M": {"x": {"N": shape}}}),
            ] {
                let (status, body) = send(
                    &url,
                    "ExecuteStatement",
                    json!({
                        "Statement": "SELECT * FROM \"Things\" WHERE pk = 'a' AND n = ?",
                        "Parameters": [parameter],
                    }),
                )
                .await;
                assert_eq!(status, 400, "{shape:?} in {parameter}: {body}");
                assert_eq!(message(&body), expected(shape), "{shape:?} in {parameter}");
            }
        }

        let (status, body) = send(
            &url,
            "ExecuteTransaction",
            json!({"TransactStatements": [{
                "Statement": "UPDATE \"Things\" SET n = ? WHERE pk = 'a'",
                "Parameters": [{"N": "1e99999999999"}],
            }]}),
        )
        .await;
        assert_eq!(status, 400, "{body}");
        assert!(
            message(&body)
                .ends_with("The parameter cannot be converted to a numeric value: 1e99999999999"),
            "{body}"
        );

        let (status, body) = send(
            &url,
            "BatchExecuteStatement",
            json!({"Statements": [{
                "Statement": "SELECT * FROM \"Things\" WHERE pk = 'a' AND n = ?",
                "Parameters": [{"N": "1e2147483647"}],
            }]}),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            body["Responses"][0]["Error"]["Message"], UNDERFLOW,
            "{body}"
        );
    }

    #[tokio::test]
    async fn put_item_keeps_its_own_rule_for_the_same_number() {
        // The control: an item attribute is still read by the stored-number
        // rule, which calls this exponent an overflow.
        let url = start().await;
        let (status, body) = send(
            &url,
            "PutItem",
            json!({"TableName": "Things", "Item": {"pk": {"S": "a"}, "n": {"N": "1e2147483647"}}}),
        )
        .await;
        assert_eq!(status, 400, "{body}");
        assert!(message(&body).ends_with(OVERFLOW), "{body}");
    }
}
