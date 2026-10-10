//! A request DynamoDB refuses with a constraint message echoes the member it
//! refused, in the Java model's `toString` form, which names all sixteen of a
//! value's fields and so runs to about twenty times the request's own size.
//! Dynoxide bounds the echo: a rejected request near the body limit must not
//! build an error message hundreds of megabytes long. The bound is Dynoxide's
//! own, well above any echo of an ordinary request; DynamoDB's behaviour for
//! echoes this size has not been captured.

use dynoxide::Database;
use dynoxide::actions::batch_execute_statement::{
    BatchExecuteStatementRequest, BatchStatementRequest,
};
use dynoxide::actions::batch_get_item::{BatchGetItemRequest, KeysAndAttributes};
use dynoxide::actions::batch_write_item::{BatchWriteItemRequest, PutRequest, WriteRequest};
use dynoxide::errors::DynoxideError;
use dynoxide::types::AttributeValue;
use std::collections::HashMap;

/// A little over the bound, so a test can tell a capped echo from an uncapped
/// one: the uncapped echoes below run to tens of megabytes.
const BOUNDED: usize = 1_200_000;

fn message(err: DynoxideError) -> String {
    match err {
        DynoxideError::ValidationException(m) => m,
        other => panic!("expected a ValidationException, got {other:?}"),
    }
}

/// BatchWriteItem echoes the whole RequestItems map when a table name breaks
/// its constraints. One item of 200,000 small attributes is about 3 MB of
/// request and about 40 MB of echo.
#[test]
fn a_batch_write_echo_is_bounded() {
    let db = Database::memory().unwrap();
    let item: HashMap<String, AttributeValue> = (0..200_000)
        .map(|i| (format!("a{i}"), AttributeValue::N("1".to_string())))
        .collect();
    let mut request_items = HashMap::new();
    request_items.insert(
        "a!".to_string(),
        vec![WriteRequest {
            put_request: Some(PutRequest { item }),
            delete_request: None,
        }],
    );
    let err = db
        .batch_write_item(BatchWriteItemRequest {
            request_items,
            ..Default::default()
        })
        .unwrap_err();
    let message = message(err);
    assert!(
        message.starts_with("1 validation error detected: Value '{a!=[WriteRequest(putRequest="),
        "unexpected message start: {}",
        &message[..message.len().min(200)]
    );
    assert!(
        message.len() < BOUNDED,
        "the echo ran to {} bytes",
        message.len()
    );
}

/// BatchExecuteStatement echoes the statement list when it holds more than 25.
/// Twenty-six statements of 2,000 parameters each are about 1.5 MB of request
/// and about 10 MB of echo.
#[test]
fn a_batch_statement_echo_is_bounded() {
    let db = Database::memory().unwrap();
    let statements: Vec<BatchStatementRequest> = (0..26)
        .map(|_| {
            let mut statement = BatchStatementRequest::default();
            statement.statement = "SELECT * FROM \"T\" WHERE pk = ?".to_string();
            statement.parameters = Some(
                (0..2_000)
                    .map(|_| AttributeValue::N("1".to_string()))
                    .collect(),
            );
            statement
        })
        .collect();
    let err = db
        .batch_execute_statement(BatchExecuteStatementRequest {
            statements,
            ..Default::default()
        })
        .unwrap_err();
    let message = message(err);
    assert!(
        message.starts_with("1 validation error detected: Value '[BatchStatementRequest("),
        "unexpected message start: {}",
        &message[..message.len().min(200)]
    );
    assert!(
        message.len() < BOUNDED,
        "the echo ran to {} bytes",
        message.len()
    );
}

/// BatchGetItem echoes its RequestItems map as JSON when a table name breaks
/// its constraints. That echo is no larger than the request, but it is cut at
/// the same bound: 100 keys of 12,000-character strings are about 1.2 MB.
#[test]
fn a_batch_get_echo_is_bounded() {
    let db = Database::memory().unwrap();
    let keys: Vec<HashMap<String, AttributeValue>> = (0..100)
        .map(|i| {
            HashMap::from([(
                "pk".to_string(),
                AttributeValue::S(format!("{i}{}", "x".repeat(12_000))),
            )])
        })
        .collect();
    let mut request_items = HashMap::new();
    request_items.insert(
        "a!".to_string(),
        KeysAndAttributes {
            keys,
            ..Default::default()
        },
    );
    let err = db
        .batch_get_item(BatchGetItemRequest {
            request_items,
            ..Default::default()
        })
        .unwrap_err();
    let message = message(err);
    assert!(
        message.starts_with("1 validation error detected: Value '{\"a!\":"),
        "unexpected message start: {}",
        &message[..message.len().min(200)]
    );
    assert!(
        message.len() < BOUNDED,
        "the echo ran to {} bytes",
        message.len()
    );
}
