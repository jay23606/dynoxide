//! A legacy condition's `ComparisonOperator` that DynamoDB doesn't have, or
//! that is missing, refuses the request. It is never dropped: a dropped
//! condition ran the Query or Scan without it, and a request whose every
//! condition dropped returned every item.
//!
//! Captured on a ScanFilter and a QueryFilter, 10 October 2026: an unknown
//! operator (`FOO`, or `eq` in lower case) in eu-west-2, ap-southeast-2 and
//! eu-central-1, and a missing one in those three and us-east-1. us-east-1
//! answers an unknown operator with the enum constraint instead; the majority
//! is followed. The QueryFilter's missing operator and every KeyConditions
//! case are inferred from the ScanFilter's.

use dynoxide::Database;
use dynoxide::DynoxideError;
use dynoxide::actions::query::QueryRequest;
use dynoxide::actions::scan::ScanRequest;
use serde_json::{Value, json};

const INVALID_OPERATOR: &str =
    "One or more parameter values were invalid: Invalid comparison operator";

fn missing_operator(member: &str, attribute: &str) -> String {
    format!(
        "1 validation error detected: Value null at '{member}.{attribute}.member.comparisonOperator' \
         failed to satisfy constraint: Member must not be null"
    )
}

fn request<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

/// The member error a request's reader refuses it with, as the server sends
/// it: without the reader's internal `VALIDATION:` marker.
fn member_error<T: serde::de::DeserializeOwned + std::fmt::Debug>(value: Value) -> String {
    let err = serde_json::from_value::<T>(value).unwrap_err().to_string();
    err.strip_prefix("VALIDATION:")
        .expect("a member error carries the VALIDATION: marker")
        .to_string()
}

/// Table `Tbl` keyed on `pk` and `sk`, holding two items in partition `e`
/// whose `a` is 5 and 6, so a filter on `a` that runs keeps one of them.
fn db() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(request(json!({
        "TableName": "Tbl",
        "KeySchema": [
            {"AttributeName": "pk", "KeyType": "HASH"},
            {"AttributeName": "sk", "KeyType": "RANGE"}
        ],
        "AttributeDefinitions": [
            {"AttributeName": "pk", "AttributeType": "S"},
            {"AttributeName": "sk", "AttributeType": "S"}
        ],
        "BillingMode": "PAY_PER_REQUEST"
    })))
    .unwrap();
    for (sk, a) in [("1", "5"), ("2", "6")] {
        db.put_item(request(json!({
            "TableName": "Tbl",
            "Item": {"pk": {"S": "e"}, "sk": {"S": sk}, "a": {"N": a}}
        })))
        .unwrap();
    }
    db
}

fn refusal<T: std::fmt::Debug>(result: Result<T, DynoxideError>, case: &str) -> String {
    match result {
        Ok(response) => panic!("{case} was accepted: {response:?}"),
        Err(err) => {
            assert_eq!(
                err.error_type(),
                "com.amazon.coral.validate#ValidationException",
                "{case}: {err:?}"
            );
            err.to_string()
        }
    }
}

fn condition(operator: Option<&str>) -> Value {
    let mut condition = json!({"AttributeValueList": [{"N": "5"}]});
    if let Some(operator) = operator {
        condition["ComparisonOperator"] = json!(operator);
    }
    condition
}

#[test]
fn an_unknown_scan_filter_operator_is_refused() {
    let db = db();
    for operator in ["FOO", "eq"] {
        let scan = db.scan(request(json!({
            "TableName": "Tbl", "ScanFilter": {"a": condition(Some(operator))}
        })));
        assert_eq!(refusal(scan, operator), INVALID_OPERATOR, "{operator:?}");
    }
    // Beside a condition that runs, the unknown one still refuses the Scan.
    let scan = db.scan(request(json!({
        "TableName": "Tbl",
        "ScanFilter": {
            "a": condition(Some("EQ")),
            "b": {"AttributeValueList": [{"N": "1"}], "ComparisonOperator": "FOO"}
        }
    })));
    assert_eq!(refusal(scan, "EQ and FOO"), INVALID_OPERATOR);
}

#[test]
fn a_missing_scan_filter_operator_is_refused() {
    let got = member_error::<ScanRequest>(json!({
        "TableName": "Tbl", "ScanFilter": {"a": condition(None)}
    }));
    assert_eq!(got, missing_operator("scanFilter", "a"));
    // A JSON null reads as missing (inferred).
    let got = member_error::<ScanRequest>(json!({
        "TableName": "Tbl",
        "ScanFilter": {"a": {"AttributeValueList": [{"N": "5"}], "ComparisonOperator": null}}
    }));
    assert_eq!(got, missing_operator("scanFilter", "a"));
}

/// A request a Rust caller builds without going through serde is held to the
/// same rule.
#[test]
fn a_scan_built_in_code_refuses_a_missing_operator_too() {
    let db = db();
    let scan = db.scan(ScanRequest {
        table_name: "Tbl".to_string(),
        scan_filter: Some(json!({"a": condition(None)})),
        ..Default::default()
    });
    assert_eq!(
        refusal(scan, "built in code"),
        missing_operator("scanFilter", "a")
    );
}

fn query_request(key_operator: Option<&str>, filter: Option<Value>) -> Value {
    let mut key = json!({"AttributeValueList": [{"S": "e"}]});
    if let Some(operator) = key_operator {
        key["ComparisonOperator"] = json!(operator);
    }
    let mut req = json!({"TableName": "Tbl", "KeyConditions": {"pk": key}});
    if let Some(filter) = filter {
        req["QueryFilter"] = json!({"a": filter});
    }
    req
}

fn query(
    db: &Database,
    key_operator: Option<&str>,
    filter: Option<Value>,
) -> Result<(), DynoxideError> {
    db.query(request(query_request(key_operator, filter)))
        .map(|_| ())
}

#[test]
fn an_unknown_or_missing_query_filter_operator_is_refused() {
    let db = db();
    for operator in ["FOO", "eq"] {
        let got = refusal(
            query(&db, Some("EQ"), Some(condition(Some(operator)))),
            operator,
        );
        assert_eq!(got, INVALID_OPERATOR, "{operator:?}");
    }
    let got = member_error::<QueryRequest>(query_request(Some("EQ"), Some(condition(None))));
    assert_eq!(got, missing_operator("queryFilter", "a"));
    // Built in code, the request is refused when it runs.
    let built = QueryRequest {
        query_filter: Some(json!({"a": condition(None)})),
        ..request(query_request(Some("EQ"), None))
    };
    assert_eq!(
        refusal(db.query(built), "built in code"),
        missing_operator("queryFilter", "a")
    );
}

/// Inferred: KeyConditions takes the filters' rule. An unknown operator
/// used to be refused in Dynoxide's own words, a lower-case `eq` ran as
/// `EQ`, and a missing one was refused as an unsupported empty operator.
#[test]
fn an_unknown_or_missing_key_condition_operator_is_refused() {
    let db = db();
    for operator in ["FOO", "eq"] {
        let got = refusal(query(&db, Some(operator), None), operator);
        assert_eq!(got, INVALID_OPERATOR, "{operator:?}");
    }
    let got = member_error::<QueryRequest>(query_request(None, None));
    assert_eq!(got, missing_operator("keyConditions", "pk"));
}

#[test]
fn a_known_operator_still_filters() {
    let db = db();
    let scan = db
        .scan(request(json!({
            "TableName": "Tbl", "ScanFilter": {"a": condition(Some("EQ"))}
        })))
        .unwrap();
    assert_eq!((scan.count, scan.scanned_count), (1, 2));
    let query = db
        .query(request(json!({
            "TableName": "Tbl",
            "KeyConditions": {"pk": {"AttributeValueList": [{"S": "e"}], "ComparisonOperator": "EQ"}},
            "QueryFilter": {"a": condition(Some("EQ"))}
        })))
        .unwrap();
    assert_eq!((query.count, query.scanned_count), (1, 2));
}
