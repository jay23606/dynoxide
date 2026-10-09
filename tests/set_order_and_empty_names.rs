//! The order set members come back in, and where an empty attribute name is
//! refused. Every expected answer here was captured against DynamoDB in
//! eu-west-2 and us-east-1 on 9 October 2026, and in ten regions where those
//! two disagreed.

#![cfg(feature = "http-server")]

use dynoxide::Database;
use serde_json::{Value, json};

async fn start(db: Database) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { dynoxide::server::serve_on(listener, db).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    url
}

async fn call(url: &str, target: &str, body: Value) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(url)
        .header("x-amz-target", target)
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=fakekey/20260101/us-east-1/dynamodb/aws4_request, SignedHeaders=host;x-amz-date;x-amz-target, Signature=fakesig",
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

async fn ddb(url: &str, op: &str, body: Value) -> (u16, Value) {
    call(url, &format!("DynamoDB_20120810.{op}"), body).await
}

async fn ok(url: &str, op: &str, body: Value) -> Value {
    let (status, out) = ddb(url, op, body).await;
    assert_eq!(status, 200, "{op} failed: {out}");
    out
}

/// Assert a ValidationException with exactly this message.
async fn refused(url: &str, op: &str, body: Value, message: &str) {
    let (status, out) = ddb(url, op, body).await;
    assert_eq!(status, 400, "{op} should be refused, got {out}");
    assert!(
        out["__type"]
            .as_str()
            .unwrap_or_default()
            .ends_with("ValidationException"),
        "{op}: unexpected error {out}"
    );
    assert_eq!(out["message"].as_str().unwrap_or_default(), message, "{op}");
}

async fn create_table(url: &str, name: &str, streams: bool) {
    let mut body = json!({
        "TableName": name,
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        "BillingMode": "PAY_PER_REQUEST"
    });
    if streams {
        body["StreamSpecification"] =
            json!({"StreamEnabled": true, "StreamViewType": "NEW_AND_OLD_IMAGES"});
    }
    ok(url, "CreateTable", body).await;
}

fn tiny() -> String {
    format!("0.{}1", "0".repeat(129))
}

/// The item the captures wrote: every set out of order, and sets nested in a
/// map and a list.
fn unsorted_item(pk: &str) -> Value {
    json!({
        "pk": {"S": pk},
        "ss": {"SS": ["b", "a", "B", "A", "aa", "_", "é", "ｚ", "😀", "Z", "0", "10", "9", " ", "~"]},
        "ns": {"NS": ["100", "-1", "10", "0.5", "9", "5", "-10", "-0.5", "0", "1e3", "25", "2.50",
                       "-1e-5", "1E10", "9".repeat(38), "-1E-130", "1E-130"]},
        "bs": {"BS": ["Ag==", "AQA=", "/w==", "AQ==", "gA==", "fw==", "AA==", "AQAA"]},
        "m": {"M": {"inner_ss": {"SS": ["z", "y", "x"]}, "inner_ns": {"NS": ["3", "1", "2"]}}},
        "l": {"L": [{"SS": ["c", "b", "a"]}, {"M": {"deep": {"NS": ["20", "3", "100"]}}}]}
    })
}

const SORTED_SS: [&str; 15] = [
    " ", "0", "10", "9", "A", "B", "Z", "_", "a", "aa", "b", "~", "é", "ｚ", "😀",
];
const SORTED_BS: [&str; 8] = [
    "AA==", "AQ==", "AQA=", "AQAA", "Ag==", "fw==", "gA==", "/w==",
];

fn numeric_ns() -> Vec<String> {
    let mut out: Vec<String> = ["-10", "-1", "-0.5", "-0.00001"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    out.push(format!("-{}", tiny()));
    out.push("0".into());
    out.push(tiny());
    for n in [
        "0.5",
        "2.5",
        "5",
        "9",
        "10",
        "25",
        "100",
        "1000",
        "10000000000",
    ] {
        out.push(n.into());
    }
    out.push("9".repeat(38));
    out
}

fn hash_ns() -> Vec<String> {
    let nines = "9".repeat(38);
    let tiny = tiny();
    let negative = format!("-{tiny}");
    [
        "0",
        &tiny,
        &negative,
        "25",
        &nines,
        "2.5",
        "10000000000",
        "9",
        "-0.5",
        "-0.00001",
        "5",
        "0.5",
        "1000",
        "100",
        "-1",
        "10",
        "-10",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("expected an array, got {v}"))
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

/// Every set in the item, in the order a numeric surface returns it.
fn assert_numeric_order(item: &Value, surface: &str) {
    assert_eq!(strings(&item["ss"]["SS"]), SORTED_SS, "{surface}: SS");
    assert_eq!(strings(&item["ns"]["NS"]), numeric_ns(), "{surface}: NS");
    assert_eq!(strings(&item["bs"]["BS"]), SORTED_BS, "{surface}: BS");
    assert_eq!(
        strings(&item["m"]["M"]["inner_ss"]["SS"]),
        ["x", "y", "z"],
        "{surface}"
    );
    assert_eq!(
        strings(&item["m"]["M"]["inner_ns"]["NS"]),
        ["1", "2", "3"],
        "{surface}"
    );
    assert_eq!(
        strings(&item["l"]["L"][0]["SS"]),
        ["a", "b", "c"],
        "{surface}"
    );
    assert_eq!(
        strings(&item["l"]["L"][1]["M"]["deep"]["NS"]),
        ["3", "20", "100"],
        "{surface}"
    );
}

/// The same item from a surface that returns number sets in hash order.
fn assert_hash_order(item: &Value, surface: &str) {
    assert_eq!(strings(&item["ss"]["SS"]), SORTED_SS, "{surface}: SS");
    assert_eq!(strings(&item["ns"]["NS"]), hash_ns(), "{surface}: NS");
    assert_eq!(strings(&item["bs"]["BS"]), SORTED_BS, "{surface}: BS");
    assert_eq!(
        strings(&item["m"]["M"]["inner_ns"]["NS"]),
        ["3", "2", "1"],
        "{surface}"
    );
    assert_eq!(
        strings(&item["l"]["L"][1]["M"]["deep"]["NS"]),
        ["3", "20", "100"],
        "{surface}"
    );
}

#[tokio::test]
async fn reads_and_return_values_give_sets_sorted_with_numbers_by_value() {
    let url = start(Database::memory().unwrap()).await;
    create_table(&url, "Tbl", false).await;
    ok(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item": unsorted_item("a")}),
    )
    .await;
    let key = json!({"pk": {"S": "a"}});

    let got = ok(&url, "GetItem", json!({"TableName": "Tbl", "Key": key})).await;
    assert_numeric_order(&got["Item"], "GetItem");

    let got = ok(
        &url,
        "BatchGetItem",
        json!({"RequestItems": {"Tbl": {"Keys": [key]}}}),
    )
    .await;
    assert_numeric_order(&got["Responses"]["Tbl"][0], "BatchGetItem");

    let got = ok(
        &url,
        "Query",
        json!({"TableName": "Tbl", "KeyConditionExpression": "pk = :p",
               "ExpressionAttributeValues": {":p": {"S": "a"}}, "ScanIndexForward": false}),
    )
    .await;
    assert_numeric_order(&got["Items"][0], "Query");

    let got = ok(
        &url,
        "UpdateItem",
        json!({"TableName": "Tbl", "Key": key, "UpdateExpression": "SET u = :one",
               "ExpressionAttributeValues": {":one": {"N": "1"}}, "ReturnValues": "ALL_NEW"}),
    )
    .await;
    assert_numeric_order(&got["Attributes"], "UpdateItem ALL_NEW");

    let got = ok(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item": unsorted_item("a"), "ReturnValues": "ALL_OLD"}),
    )
    .await;
    assert_numeric_order(&got["Attributes"], "PutItem ALL_OLD");
}

#[tokio::test]
async fn a_failed_condition_returns_the_stored_item_with_numbers_by_value() {
    let url = start(Database::memory().unwrap()).await;
    create_table(&url, "Tbl", false).await;
    ok(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item": unsorted_item("a")}),
    )
    .await;
    let (status, out) = ddb(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item": {"pk": {"S": "a"}},
               "ConditionExpression": "attribute_not_exists(pk)",
               "ReturnValuesOnConditionCheckFailure": "ALL_OLD"}),
    )
    .await;
    assert_eq!(status, 400, "{out}");
    assert_numeric_order(&out["Item"], "ConditionalCheckFailed Item");

    let got = ok(
        &url,
        "DeleteItem",
        json!({"TableName": "Tbl", "Key": {"pk": {"S": "a"}}, "ReturnValues": "ALL_OLD"}),
    )
    .await;
    assert_numeric_order(&got["Attributes"], "DeleteItem ALL_OLD");
}

#[tokio::test]
async fn scan_transactions_partiql_and_streams_give_number_sets_in_hash_order() {
    let url = start(Database::memory().unwrap()).await;
    create_table(&url, "Tbl", true).await;
    ok(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item": unsorted_item("a")}),
    )
    .await;
    let key = json!({"pk": {"S": "a"}});

    let got = ok(&url, "Scan", json!({"TableName": "Tbl"})).await;
    assert_hash_order(&got["Items"][0], "Scan");

    let got = ok(
        &url,
        "TransactGetItems",
        json!({"TransactItems": [{"Get": {"TableName": "Tbl", "Key": key}}]}),
    )
    .await;
    assert_hash_order(&got["Responses"][0]["Item"], "TransactGetItems");

    let select = "SELECT * FROM \"Tbl\" WHERE pk = 'a'";
    let got = ok(&url, "ExecuteStatement", json!({"Statement": select})).await;
    assert_hash_order(&got["Items"][0], "ExecuteStatement");

    let got = ok(
        &url,
        "BatchExecuteStatement",
        json!({"Statements": [{"Statement": select}]}),
    )
    .await;
    assert_hash_order(&got["Responses"][0]["Item"], "BatchExecuteStatement");

    let got = ok(
        &url,
        "ExecuteTransaction",
        json!({"TransactStatements": [{"Statement": select}]}),
    )
    .await;
    assert_hash_order(&got["Responses"][0]["Item"], "ExecuteTransaction");

    let (status, out) = ddb(
        &url,
        "TransactWriteItems",
        json!({"TransactItems": [{"ConditionCheck": {"TableName": "Tbl", "Key": key,
            "ConditionExpression": "attribute_not_exists(pk)",
            "ReturnValuesOnConditionCheckFailure": "ALL_OLD"}}]}),
    )
    .await;
    assert_eq!(status, 400, "{out}");
    assert_hash_order(
        &out["CancellationReasons"][0]["Item"],
        "cancellation reason",
    );

    let table = ok(&url, "DescribeTable", json!({"TableName": "Tbl"})).await;
    let arn = table["Table"]["LatestStreamArn"]
        .as_str()
        .unwrap()
        .to_string();
    let stream = call(
        &url,
        "DynamoDBStreams_20120810.DescribeStream",
        json!({"StreamArn": arn}),
    )
    .await
    .1;
    let shard = stream["StreamDescription"]["Shards"][0]["ShardId"]
        .as_str()
        .unwrap()
        .to_string();
    let iterator = call(
        &url,
        "DynamoDBStreams_20120810.GetShardIterator",
        json!({"StreamArn": arn, "ShardId": shard, "ShardIteratorType": "TRIM_HORIZON"}),
    )
    .await
    .1["ShardIterator"]
        .as_str()
        .unwrap()
        .to_string();
    let records = call(
        &url,
        "DynamoDBStreams_20120810.GetRecords",
        json!({"ShardIterator": iterator}),
    )
    .await
    .1;
    assert_hash_order(
        &records["Records"][0]["dynamodb"]["NewImage"],
        "stream NewImage",
    );
}

#[tokio::test]
async fn writes_store_set_members_sorted() {
    let url = start(Database::memory().unwrap()).await;
    create_table(&url, "Tbl", false).await;
    let key = json!({"pk": {"S": "a"}});
    ok(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item": unsorted_item("a")}),
    )
    .await;
    let get = |pk: &'static str| {
        let url = url.clone();
        async move {
            ok(
                &url,
                "GetItem",
                json!({"TableName": "Tbl", "Key": {"pk": {"S": pk}}}),
            )
            .await["Item"]
                .clone()
        }
    };

    ok(
        &url,
        "UpdateItem",
        json!({"TableName": "Tbl", "Key": key, "UpdateExpression": "ADD ns :v",
        "ExpressionAttributeValues": {":v": {"NS": ["7", "-3", "1000"]}}}),
    )
    .await;
    let mut expected = numeric_ns();
    expected.insert(1, "-3".to_string());
    let seven = expected.iter().position(|n| n == "9").unwrap();
    expected.insert(seven, "7".to_string());
    assert_eq!(strings(&get("a").await["ns"]["NS"]), expected);

    ok(
        &url,
        "UpdateItem",
        json!({"TableName": "Tbl", "Key": key, "UpdateExpression": "ADD bs :v",
        "ExpressionAttributeValues": {":v": {"BS": ["QA==", "Aw=="]}}}),
    )
    .await;
    assert_eq!(
        strings(&get("a").await["bs"]["BS"]),
        [
            "AA==", "AQ==", "AQA=", "AQAA", "Ag==", "Aw==", "QA==", "fw==", "gA==", "/w=="
        ]
    );

    let got = ok(&url, "UpdateItem", json!({"TableName": "Tbl", "Key": key, "UpdateExpression": "SET s2 = :v",
        "ExpressionAttributeValues": {":v": {"SS": ["q", "p", "o"]}}, "ReturnValues": "UPDATED_NEW"})).await;
    assert_eq!(strings(&got["Attributes"]["s2"]["SS"]), ["o", "p", "q"]);
    let got = ok(
        &url,
        "UpdateItem",
        json!({"TableName": "Tbl", "Key": key, "UpdateExpression": "SET s2 = :v",
        "ExpressionAttributeValues": {":v": {"SS": ["k", "j"]}}, "ReturnValues": "UPDATED_OLD"}),
    )
    .await;
    assert_eq!(strings(&got["Attributes"]["s2"]["SS"]), ["o", "p", "q"]);

    ok(
        &url,
        "UpdateItem",
        json!({"TableName": "Tbl", "Key": key, "UpdateExpression": "ADD n :v",
        "ExpressionAttributeValues": {":v": {"NS": ["3", "1", "2"]}}}),
    )
    .await;
    ok(
        &url,
        "UpdateItem",
        json!({"TableName": "Tbl", "Key": key,
        "AttributeUpdates": {"n": {"Action": "ADD", "Value": {"NS": ["-5", "50"]}}}}),
    )
    .await;
    assert_eq!(
        strings(&get("a").await["n"]["NS"]),
        ["-5", "1", "2", "3", "50"]
    );

    ok(
        &url,
        "BatchWriteItem",
        json!({"RequestItems": {"Tbl": [{"PutRequest": {"Item":
        {"pk": {"S": "bw"}, "ss": {"SS": ["c", "a", "b"]}, "ns": {"NS": ["3", "1", "2"]}}}}]}}),
    )
    .await;
    let bw = get("bw").await;
    assert_eq!(strings(&bw["ss"]["SS"]), ["a", "b", "c"]);
    assert_eq!(strings(&bw["ns"]["NS"]), ["1", "2", "3"]);

    ok(
        &url,
        "TransactWriteItems",
        json!({"TransactItems": [{"Put": {"TableName": "Tbl", "Item":
        {"pk": {"S": "tw"}, "ss": {"SS": ["c", "a", "b"]}, "ns": {"NS": ["3", "1", "2"]}}}}]}),
    )
    .await;
    ok(
        &url,
        "TransactWriteItems",
        json!({"TransactItems": [{"Update": {"TableName": "Tbl",
        "Key": {"pk": {"S": "tw"}}, "UpdateExpression": "ADD ns :v",
        "ExpressionAttributeValues": {":v": {"NS": ["0", "10"]}}}}]}),
    )
    .await;
    let tw = get("tw").await;
    assert_eq!(strings(&tw["ss"]["SS"]), ["a", "b", "c"]);
    assert_eq!(strings(&tw["ns"]["NS"]), ["0", "1", "2", "3", "10"]);

    ok(
        &url,
        "ExecuteStatement",
        json!({"Statement":
        "INSERT INTO \"Tbl\" VALUE {'pk': 'p1', 'ss': <<'b', 'a', 'c'>>, 'ns': <<3, 1, 2, -4>>}"}),
    )
    .await;
    let p1 = get("p1").await;
    assert_eq!(strings(&p1["ss"]["SS"]), ["a", "b", "c"]);
    assert_eq!(strings(&p1["ns"]["NS"]), ["-4", "1", "2", "3"]);
}

#[tokio::test]
async fn number_set_members_are_normalised_then_sorted() {
    let url = start(Database::memory().unwrap()).await;
    create_table(&url, "Tbl", false).await;
    ok(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item":
        {"pk": {"S": "r1"}, "ns": {"NS": ["1e2", "5", "1.0e1", "01", "-0", "0.10"]}}}),
    )
    .await;
    let got = ok(
        &url,
        "GetItem",
        json!({"TableName": "Tbl", "Key": {"pk": {"S": "r1"}}}),
    )
    .await;
    assert_eq!(
        strings(&got["Item"]["ns"]["NS"]),
        ["0", "0.1", "1", "5", "10", "100"]
    );
}

/// A row stored before sets were sorted keeps its stored order on disk; it
/// still reads back sorted.
#[tokio::test]
async fn a_row_stored_unsorted_reads_back_sorted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    let path = path.to_str().unwrap().to_string();
    {
        let url = start(Database::new(&path).unwrap()).await;
        create_table(&url, "Tbl", false).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    {
        let storage = dynoxide::storage::Storage::new(&path).unwrap();
        let item = json!({"pk": {"S": "old"}, "ss": {"SS": ["c", "a", "b"]},
                          "ns": {"NS": ["3", "1", "2"]}, "bs": {"BS": ["Ag==", "AQ=="]}});
        let text = item.to_string();
        // Keys are stored as their type tag and value.
        storage.put_item("Tbl", "S:old", "", &text, 40).unwrap();
    }
    let url = start(Database::new(&path).unwrap()).await;
    let got = ok(
        &url,
        "GetItem",
        json!({"TableName": "Tbl", "Key": {"pk": {"S": "old"}}}),
    )
    .await;
    assert_eq!(strings(&got["Item"]["ss"]["SS"]), ["a", "b", "c"]);
    assert_eq!(strings(&got["Item"]["ns"]["NS"]), ["1", "2", "3"]);
    assert_eq!(strings(&got["Item"]["bs"]["BS"]), ["AQ==", "Ag=="]);
}

const EMPTY: &str = "Empty attribute name";
const ENVELOPED: &str = "1 validation error detected: Empty attribute name";

fn with_empty_key() -> Value {
    json!({"M": {"": {"S": "x"}}})
}

#[tokio::test]
async fn put_update_and_delete_refuse_an_empty_name_inside_the_envelope() {
    let url = start(Database::memory().unwrap()).await;
    create_table(&url, "Tbl", false).await;
    ok(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item": {"pk": {"S": "a"}}}),
    )
    .await;
    let e = with_empty_key();

    for item in [
        json!({"pk": {"S": "e"}, "": {"S": "x"}}),
        json!({"pk": {"S": "e"}, "m": e}),
        json!({"pk": {"S": "e"}, "m": {"M": {"a": e}}}),
        json!({"pk": {"S": "e"}, "l": {"L": [e]}}),
    ] {
        refused(
            &url,
            "PutItem",
            json!({"TableName": "Tbl", "Item": item}),
            ENVELOPED,
        )
        .await;
    }
    // Ahead of a missing table, a missing or mistyped key, a failing condition
    // and the item-size limit.
    refused(
        &url,
        "PutItem",
        json!({"TableName": "Missing", "Item": {"pk": {"S": "e"}, "m": e}}),
        ENVELOPED,
    )
    .await;
    refused(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item": {"other": {"S": "e"}, "m": e}}),
        ENVELOPED,
    )
    .await;
    refused(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item": {"pk": {"N": "1"}, "m": e}}),
        ENVELOPED,
    )
    .await;
    refused(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item": {"pk": {"S": "a"}, "m": e},
        "ConditionExpression": "attribute_not_exists(pk)"}),
        ENVELOPED,
    )
    .await;
    refused(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item": {"pk": {"S": "e"}, "m": e,
        "big": {"S": "x".repeat(410_000)}}}),
        ENVELOPED,
    )
    .await;
    refused(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item": {"pk": {"S": "e"}},
        "Expected": {"": {"Exists": false}}}),
        ENVELOPED,
    )
    .await;
    refused(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item": {"pk": {"S": "e"}},
        "ConditionExpression": "attribute_not_exists(pk) OR m = :v",
        "ExpressionAttributeValues": {":v": e}}),
        ENVELOPED,
    )
    .await;
    refused(&url, "PutItem", json!({"TableName": "Tbl", "Item": {"pk": {"S": "e"}},
        "ConditionExpression": "attribute_not_exists(#e)", "ExpressionAttributeNames": {"#e": ""}}),
        "1 validation error detected: ExpressionAttributeNames contains invalid value: Empty attribute name for key #e").await;

    let key = json!({"pk": {"S": "a"}});
    refused(
        &url,
        "UpdateItem",
        json!({"TableName": "Tbl", "Key": key, "UpdateExpression": "SET m2 = :v",
        "ExpressionAttributeValues": {":v": e}}),
        ENVELOPED,
    )
    .await;
    refused(
        &url,
        "UpdateItem",
        json!({"TableName": "Tbl", "Key": key, "UpdateExpression": "SET u = :v",
        "ExpressionAttributeValues": {":v": {"L": [{"M": {"a": e}}]}}}),
        ENVELOPED,
    )
    .await;
    // An unused value, and ahead of a syntax error and an unused name.
    refused(
        &url,
        "UpdateItem",
        json!({"TableName": "Tbl", "Key": key, "UpdateExpression": "SET u = :o",
        "ExpressionAttributeValues": {":o": {"N": "1"}, ":v": e}}),
        ENVELOPED,
    )
    .await;
    refused(
        &url,
        "UpdateItem",
        json!({"TableName": "Tbl", "Key": key, "UpdateExpression": "SET u =",
        "ExpressionAttributeValues": {":v": e}}),
        ENVELOPED,
    )
    .await;
    refused(
        &url,
        "UpdateItem",
        json!({"TableName": "Tbl", "Key": key, "UpdateExpression": "SET u = :v",
        "ExpressionAttributeNames": {"#unused": "x"}, "ExpressionAttributeValues": {":v": e}}),
        ENVELOPED,
    )
    .await;
    refused(
        &url,
        "UpdateItem",
        json!({"TableName": "Tbl", "Key": {"pk": {"S": "a"}, "": {"S": "x"}},
        "UpdateExpression": "SET u = :o", "ExpressionAttributeValues": {":o": {"N": "1"}}}),
        ENVELOPED,
    )
    .await;
    refused(
        &url,
        "UpdateItem",
        json!({"TableName": "Tbl", "Key": key,
        "AttributeUpdates": {"m3": {"Action": "PUT", "Value": e}}}),
        ENVELOPED,
    )
    .await;
    refused(
        &url,
        "UpdateItem",
        json!({"TableName": "Tbl", "Key": key,
        "AttributeUpdates": {"": {"Action": "PUT", "Value": {"S": "x"}}}}),
        ENVELOPED,
    )
    .await;
    refused(&url, "UpdateItem", json!({"TableName": "Tbl", "Key": key,
        "AttributeUpdates": {"u": {"Action": "PUT", "Value": {"N": "1"}}}, "Expected": {"m": {"Value": e}}}), ENVELOPED).await;
    refused(&url, "UpdateItem", json!({"TableName": "Tbl", "Key": key, "UpdateExpression": "SET #e = :v",
        "ExpressionAttributeNames": {"#e": ""}, "ExpressionAttributeValues": {":v": {"S": "x"}}}),
        "1 validation error detected: ExpressionAttributeNames contains invalid value: Empty attribute name for key #e").await;

    refused(
        &url,
        "DeleteItem",
        json!({"TableName": "Tbl", "Key": {"pk": {"S": "zz"}, "": {"S": "x"}}}),
        ENVELOPED,
    )
    .await;
    refused(&url, "DeleteItem", json!({"TableName": "Tbl", "Key": {"pk": {"S": "zz"}},
        "ConditionExpression": "attribute_not_exists(pk) OR m = :v", "ExpressionAttributeValues": {":v": e}}), ENVELOPED).await;
    refused(
        &url,
        "DeleteItem",
        json!({"TableName": "Tbl", "Key": {"pk": {"S": "zz"}},
        "Expected": {"": {"Exists": false}}}),
        ENVELOPED,
    )
    .await;
    refused(&url, "DeleteItem", json!({"TableName": "Tbl", "Key": {"pk": {"S": "zz"}},
        "ConditionExpression": "attribute_not_exists(#e)", "ExpressionAttributeNames": {"#e": ""}}),
        "1 validation error detected: ExpressionAttributeNames contains invalid value: Empty attribute name for key #e").await;

    // Nothing was written by any of them.
    let got = ok(&url, "Scan", json!({"TableName": "Tbl"})).await;
    assert_eq!(got["Count"], 1, "{got}");
}

#[tokio::test]
async fn reads_refuse_an_empty_name_bare() {
    let url = start(Database::memory().unwrap()).await;
    create_table(&url, "Tbl", false).await;
    let e = with_empty_key();
    let key = json!({"pk": {"S": "a"}});
    let ean = "ExpressionAttributeNames contains invalid value: Empty attribute name for key #e";

    refused(
        &url,
        "GetItem",
        json!({"TableName": "Tbl", "Key": {"pk": {"S": "a"}, "": {"S": "y"}}}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "GetItem",
        json!({"TableName": "Missing", "Key": {"pk": {"S": "a"}, "": {"S": "y"}}}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "GetItem",
        json!({"TableName": "Tbl", "Key": {"pk": e}}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "GetItem",
        json!({"TableName": "Tbl", "Key": key, "AttributesToGet": [""]}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "GetItem",
        json!({"TableName": "Tbl", "Key": key, "ProjectionExpression": "#e",
        "ExpressionAttributeNames": {"#e": ""}}),
        ean,
    )
    .await;
    refused(
        &url,
        "GetItem",
        json!({"TableName": "Tbl", "Key": key, "ProjectionExpression": "pk",
        "ExpressionAttributeNames": {"#e": ""}}),
        ean,
    )
    .await;

    let kce = |extra: Value| {
        let mut body = json!({"TableName": "Tbl", "KeyConditionExpression": "pk = :p",
            "ExpressionAttributeValues": {":p": {"S": "a"}}});
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        body
    };
    refused(
        &url,
        "Query",
        kce(json!({"FilterExpression": "m = :v",
        "ExpressionAttributeValues": {":p": {"S": "a"}, ":v": e}})),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "Query",
        json!({"TableName": "Tbl", "KeyConditionExpression": "pk = :v",
        "ExpressionAttributeValues": {":v": e}}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "Query",
        json!({"TableName": "Missing", "KeyConditionExpression": "pk = :p",
        "FilterExpression": "m = :v", "ExpressionAttributeValues": {":p": {"S": "a"}, ":v": e}}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "Query",
        kce(json!({"ExclusiveStartKey": {"pk": {"S": "a"}, "": {"S": "x"}}})),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "Query",
        kce(json!({"ProjectionExpression": "#e", "ExpressionAttributeNames": {"#e": ""}})),
        ean,
    )
    .await;
    let legacy = json!({"pk": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "a"}]}});
    refused(
        &url,
        "Query",
        json!({"TableName": "Tbl", "KeyConditions": legacy, "AttributesToGet": [""]}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "Query",
        json!({"TableName": "Tbl", "KeyConditions": legacy,
        "QueryFilter": {"m": {"ComparisonOperator": "EQ", "AttributeValueList": [e]}}}),
        EMPTY,
    )
    .await;

    refused(
        &url,
        "Scan",
        json!({"TableName": "Tbl", "AttributesToGet": [""]}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "Scan",
        json!({"TableName": "Tbl",
        "ScanFilter": {"m": {"ComparisonOperator": "EQ", "AttributeValueList": [e]}}}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "Scan",
        json!({"TableName": "Tbl", "ExclusiveStartKey": {"pk": {"S": "a"}, "": {"S": "x"}}}),
        "The provided starting key is invalid: Empty attribute name",
    )
    .await;
    refused(
        &url,
        "Scan",
        json!({"TableName": "Tbl", "FilterExpression": "attribute_exists(#e)",
        "ExpressionAttributeNames": {"#e": ""}}),
        ean,
    )
    .await;
    refused(
        &url,
        "Scan",
        json!({"TableName": "Tbl", "FilterExpression": "m = :v",
        "ExpressionAttributeValues": {":v": e}}),
        "ExpressionAttributeValues contains invalid value: Empty attribute name for key :v",
    )
    .await;

    refused(
        &url,
        "BatchGetItem",
        json!({"RequestItems": {"Tbl": {"Keys": [{"pk": {"S": "a"}, "": {"S": "x"}}]}}}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "BatchGetItem",
        json!({"RequestItems": {"Tbl": {"Keys": [key], "AttributesToGet": [""]}}}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "BatchGetItem",
        json!({"RequestItems": {"Tbl": {"Keys": [key], "ProjectionExpression": "#e",
        "ExpressionAttributeNames": {"#e": ""}}}}),
        ean,
    )
    .await;

    refused(
        &url,
        "TransactGetItems",
        json!({"TransactItems": [{"Get": {"TableName": "Tbl",
        "Key": {"pk": {"S": "a"}, "": {"S": "x"}}}}]}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "TransactGetItems",
        json!({"TransactItems": [{"Get": {"TableName": "Tbl", "Key": key,
        "ProjectionExpression": "#e", "ExpressionAttributeNames": {"#e": ""}}}]}),
        ean,
    )
    .await;
}

#[tokio::test]
async fn an_empty_name_in_a_legacy_filter_itself_is_accepted() {
    let url = start(Database::memory().unwrap()).await;
    create_table(&url, "Tbl", false).await;
    ok(
        &url,
        "Scan",
        json!({"TableName": "Tbl", "ScanFilter": {"": {"ComparisonOperator": "NOT_NULL"}}}),
    )
    .await;
    ok(
        &url,
        "Query",
        json!({"TableName": "Tbl",
        "KeyConditions": {"pk": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "a"}]}},
        "QueryFilter": {"": {"ComparisonOperator": "NOT_NULL"}}}),
    )
    .await;
    // A name made of a space is a name.
    ok(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item": {"pk": {"S": "s"}, "m": {"M": {" ": {"S": "space"}}}}}),
    )
    .await;
}

#[tokio::test]
async fn batch_and_transaction_writes_refuse_an_empty_name_up_front() {
    let url = start(Database::memory().unwrap()).await;
    create_table(&url, "Tbl", false).await;
    ok(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item": {"pk": {"S": "a"}}}),
    )
    .await;
    let e = with_empty_key();
    let values_message =
        "ExpressionAttributeValues contains invalid value: Empty attribute name for key :v";

    refused(
        &url,
        "BatchWriteItem",
        json!({"RequestItems": {"Tbl": [{"PutRequest": {"Item": {"pk": {"S": "e"}, "m": e}}}]}}),
        EMPTY,
    )
    .await;
    refused(&url, "BatchWriteItem", json!({"RequestItems": {"Tbl": [{"PutRequest": {"Item": {"pk": {"S": "e"}, "": {"S": "x"}}}}]}}), EMPTY).await;
    refused(&url, "BatchWriteItem", json!({"RequestItems": {"Tbl": [{"DeleteRequest": {"Key": {"pk": {"S": "a"}, "": {"S": "x"}}}}]}}), EMPTY).await;
    refused(&url, "BatchWriteItem", json!({"RequestItems": {"Missing": [{"PutRequest": {"Item": {"pk": {"S": "e"}, "m": e}}}]}}), EMPTY).await;

    let tw = |action: Value| json!({"TransactItems": [action]});
    refused(
        &url,
        "TransactWriteItems",
        tw(json!({"Put": {"TableName": "Tbl", "Item": {"pk": {"S": "e"}, "m": e}}})),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "TransactWriteItems",
        tw(json!({"Put": {"TableName": "Missing", "Item": {"pk": {"S": "e"}, "m": e}}})),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "TransactWriteItems",
        json!({"TransactItems": [
        {"Put": {"TableName": "Tbl", "Item": {"pk": {"S": "e1"}}}},
        {"Put": {"TableName": "Tbl", "Item": {"pk": {"S": "e2"}, "m": e}}}]}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "TransactWriteItems",
        tw(json!({"Delete": {"TableName": "Tbl", "Key": {"pk": {"S": "zz"}, "": {"S": "x"}}}})),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "TransactWriteItems",
        tw(
            json!({"Update": {"TableName": "Tbl", "Key": {"pk": {"S": "a"}},
        "UpdateExpression": "SET m2 = :v", "ExpressionAttributeValues": {":v": e}}}),
        ),
        values_message,
    )
    .await;
    refused(&url, "TransactWriteItems", tw(json!({"ConditionCheck": {"TableName": "Tbl", "Key": {"pk": {"S": "a"}},
        "ConditionExpression": "attribute_exists(pk) OR m = :v", "ExpressionAttributeValues": {":v": e}}})), values_message).await;
    refused(&url, "TransactWriteItems", tw(json!({"Delete": {"TableName": "Tbl", "Key": {"pk": {"S": "zz"}},
        "ConditionExpression": "attribute_not_exists(pk) OR m = :v", "ExpressionAttributeValues": {":v": e}}})), values_message).await;
    refused(&url, "TransactWriteItems", tw(json!({"Put": {"TableName": "Tbl", "Item": {"pk": {"S": "b"}},
        "ConditionExpression": "attribute_not_exists(pk) OR m = :v", "ExpressionAttributeValues": {":v": e}}})), values_message).await;
    refused(
        &url,
        "TransactWriteItems",
        tw(
            json!({"Update": {"TableName": "Tbl", "Key": {"pk": {"S": "a"}},
        "UpdateExpression": "SET #e = :v", "ExpressionAttributeNames": {"#e": ""},
        "ExpressionAttributeValues": {":v": {"S": "x"}}}}),
        ),
        "ExpressionAttributeNames contains invalid value: Empty attribute name for key #e",
    )
    .await;

    let got = ok(&url, "Scan", json!({"TableName": "Tbl"})).await;
    assert_eq!(got["Count"], 1, "nothing written: {got}");
}

#[tokio::test]
async fn partiql_refuses_a_parameter_holding_an_empty_name() {
    let url = start(Database::memory().unwrap()).await;
    create_table(&url, "Tbl", false).await;
    ok(
        &url,
        "PutItem",
        json!({"TableName": "Tbl", "Item": {"pk": {"S": "a"}}}),
    )
    .await;
    let e = json!({"M": {"": {"N": "1"}}});

    refused(
        &url,
        "ExecuteStatement",
        json!({"Statement": "INSERT INTO \"Tbl\" VALUE {'pk': 'e', 'm': ?}",
        "Parameters": [e]}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "ExecuteStatement",
        json!({"Statement": "UPDATE \"Tbl\" SET m2 = ? WHERE pk = 'a'",
        "Parameters": [e]}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "ExecuteStatement",
        json!({"Statement": "SELECT * FROM \"Tbl\" WHERE pk = 'a' AND m = ?",
        "Parameters": [e]}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "ExecuteStatement",
        json!({"Statement": "INSERT INTO \"Missing\" VALUE {'pk': 'e', 'm': ?}",
        "Parameters": [e]}),
        EMPTY,
    )
    .await;
    refused(
        &url,
        "ExecuteTransaction",
        json!({"TransactStatements": [
        {"Statement": "INSERT INTO \"Tbl\" VALUE {'pk': 'e1'}"},
        {"Statement": "INSERT INTO \"Tbl\" VALUE {'pk': 'e2', 'm': ?}", "Parameters": [e]}]}),
        EMPTY,
    )
    .await;

    let got = ok(
        &url,
        "BatchExecuteStatement",
        json!({"Statements": [
        {"Statement": "INSERT INTO \"Tbl\" VALUE {'pk': 'b1', 'm': ?}", "Parameters": [e]},
        {"Statement": "INSERT INTO \"Tbl\" VALUE {'pk': 'b2'}"}]}),
    )
    .await;
    assert_eq!(
        got["Responses"][0]["Error"]["Code"], "ValidationError",
        "{got}"
    );
    assert_eq!(got["Responses"][0]["Error"]["Message"], EMPTY, "{got}");
    assert!(got["Responses"][1].get("Error").is_none(), "{got}");

    let got = ok(&url, "Scan", json!({"TableName": "Tbl"})).await;
    let pks: Vec<&str> = got["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["pk"]["S"].as_str().unwrap())
        .collect();
    assert_eq!(pks.len(), 2, "only a and b2 exist: {pks:?}");
}
