//! DynamoDB's request member errors on the PartiQL operations: every
//! violation in a request is listed, numbered, in member order, and one on a
//! batch or transaction member refuses the whole request, writing nothing.
//! Captured in eu-west-2 and us-east-1, 2026-10-10; the regions agree on
//! every case here.

use dynoxide::Database;
use dynoxide::actions::batch_execute_statement::BatchExecuteStatementRequest;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::ExecuteStatementRequest;
use dynoxide::actions::execute_transaction::ExecuteTransactionRequest;
use dynoxide::actions::get_item::GetItemRequest;
use serde_json::{Value, json};

fn setup() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value::<CreateTableRequest>(json!({
            "TableName": "Members",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    db
}

fn single(db: &Database, request: Value) -> Result<(), String> {
    let request: ExecuteStatementRequest = serde_json::from_value(request).unwrap();
    db.execute_statement(request)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn batch(db: &Database, statements: Value) -> Result<(), String> {
    let request: BatchExecuteStatementRequest =
        serde_json::from_value(json!({"Statements": statements})).unwrap();
    db.batch_execute_statement(request)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn transaction(db: &Database, statements: Value) -> Result<(), String> {
    let request: ExecuteTransactionRequest =
        serde_json::from_value(json!({"TransactStatements": statements})).unwrap();
    db.execute_transaction(request)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn stored(db: &Database, pk: &str) -> bool {
    let request: GetItemRequest = serde_json::from_value(json!({
        "TableName": "Members",
        "Key": {"pk": {"S": pk}}
    }))
    .unwrap();
    db.get_item(request).unwrap().item.is_some()
}

/// A SELECT of exactly `length` code points.
fn select_of_length(length: usize) -> String {
    let head = r#"SELECT * FROM "Members" WHERE pk = '"#;
    format!("{head}{}'", "x".repeat(length - head.chars().count() - 1))
}

fn too_long(statement: &str, path: &str) -> String {
    format!(
        "Value '{statement}' at '{path}' failed to satisfy constraint: Member must have length \
         less than or equal to 8192"
    )
}

fn empty_parameters(path: &str) -> String {
    format!(
        "Value '[]' at '{path}' failed to satisfy constraint: Member must have length greater \
         than or equal to 1"
    )
}

fn empty_statement(path: &str) -> String {
    format!(
        "Value '' at '{path}' failed to satisfy constraint: Member must have length greater \
         than or equal to 1"
    )
}

#[test]
fn execute_statement_lists_every_member_violation() {
    let db = setup();
    let long = select_of_length(9_000);
    assert_eq!(
        single(&db, json!({"Statement": long, "Parameters": []})),
        Err(format!(
            "2 validation errors detected: {}; {}",
            too_long(&long, "statement"),
            empty_parameters("parameters")
        ))
    );
    assert_eq!(
        single(
            &db,
            json!({"Statement": long, "ReturnConsumedCapacity": "BOGUS"})
        ),
        Err(format!(
            "2 validation errors detected: {}; Value 'BOGUS' at 'returnConsumedCapacity' failed \
             to satisfy constraint: Member must satisfy enum value set: [INDEXES, TOTAL, NONE]",
            too_long(&long, "statement")
        ))
    );
}

#[test]
fn a_batch_lists_every_member_violation() {
    let db = setup();
    let long = select_of_length(9_000);
    assert_eq!(
        batch(
            &db,
            json!([{"Statement": long}, {"Statement": long}, {"Statement": long}])
        ),
        Err(format!(
            "3 validation errors detected: {}; {}; {}",
            too_long(&long, "statements.1.member.statement"),
            too_long(&long, "statements.2.member.statement"),
            too_long(&long, "statements.3.member.statement")
        ))
    );
    let short = select_of_length(60);
    assert_eq!(
        batch(
            &db,
            json!([{"Statement": long}, {"Statement": short, "Parameters": []}])
        ),
        Err(format!(
            "2 validation errors detected: {}; {}",
            too_long(&long, "statements.1.member.statement"),
            empty_parameters("statements.2.member.parameters")
        ))
    );
    assert_eq!(
        batch(&db, json!([{"Statement": ""}, {"Statement": ""}])),
        Err(format!(
            "2 validation errors detected: {}; {}",
            empty_statement("statements.1.member.statement"),
            empty_statement("statements.2.member.statement")
        ))
    );
}

#[test]
fn a_transaction_lists_every_member_violation() {
    let db = setup();
    let long = select_of_length(9_000);
    assert_eq!(
        transaction(&db, json!([{"Statement": long}, {"Statement": long}])),
        Err(format!(
            "2 validation errors detected: {}; {}",
            too_long(&long, "transactStatements.1.member.statement"),
            too_long(&long, "transactStatements.2.member.statement")
        ))
    );
}

/// An empty `Parameters` list on any member refuses the whole request, with
/// or without a placeholder, and the writes beside it don't happen.
#[test]
fn empty_parameters_on_a_member_refuse_the_whole_request() {
    let db = setup();
    let write = |pk: &str| format!(r#"INSERT INTO "Members" VALUE {{'pk': '{pk}'}}"#);
    for (index, statements) in [
        (1, json!([{"Statement": write("w1"), "Parameters": []}])),
        (
            2,
            json!([
                {"Statement": write("w2")},
                {"Statement": r#"INSERT INTO "Members" VALUE {'pk': ?}"#, "Parameters": []}
            ]),
        ),
    ] {
        assert_eq!(
            batch(&db, statements.clone()),
            Err(format!(
                "1 validation error detected: {}",
                empty_parameters(&format!("statements.{index}.member.parameters"))
            ))
        );
        assert_eq!(
            transaction(&db, statements),
            Err(format!(
                "1 validation error detected: {}",
                empty_parameters(&format!("transactStatements.{index}.member.parameters"))
            ))
        );
    }
    assert!(!stored(&db, "w1"));
    assert!(!stored(&db, "w2"));
}

/// An empty statement on a member refuses the whole request.
#[test]
fn an_empty_statement_on_a_member_refuses_the_whole_request() {
    let db = setup();
    let statements = json!([
        {"Statement": r#"INSERT INTO "Members" VALUE {'pk': 'w3'}"#},
        {"Statement": ""}
    ]);
    assert_eq!(
        batch(&db, statements.clone()),
        Err(format!(
            "1 validation error detected: {}",
            empty_statement("statements.2.member.statement")
        ))
    );
    assert_eq!(
        transaction(&db, statements),
        Err(format!(
            "1 validation error detected: {}",
            empty_statement("transactStatements.2.member.statement")
        ))
    );
    assert!(!stored(&db, "w3"));
}

const BAD_CAPACITY_MODE: &str = "Value 'BOGUS' at 'returnConsumedCapacity' failed to satisfy \
     constraint: Member must satisfy enum value set: [INDEXES, TOTAL, NONE]";

/// More than 25 statements is a violation on the `statements` member, listed
/// with the request's others and after ReturnConsumedCapacity (captured
/// eu-west-2 and us-east-1, 2026-10-10). The value echoes every statement as
/// us-east-1 prints it, the request object's Java `toString`; eu-west-2
/// printed an object identity per statement, which can't be reproduced.
#[test]
fn a_batch_over_25_statements_is_listed_with_its_other_members() {
    let db = setup();
    let statement = r#"SELECT * FROM "Members" WHERE pk = 'a'"#;
    let echo = vec![
        format!(
            "BatchStatementRequest(statement={statement}, parameters=null, \
             consistentRead=null, returnValuesOnConditionCheckFailure=null)"
        );
        26
    ]
    .join(", ");
    let too_many = format!(
        "Value '[{echo}]' at 'statements' failed to satisfy constraint: Member must have \
         length less than or equal to 25"
    );
    let request = |mode: Option<&str>| {
        let mut request = json!({"Statements": vec![json!({"Statement": statement}); 26]});
        if let Some(mode) = mode {
            request["ReturnConsumedCapacity"] = json!(mode);
        }
        serde_json::from_value::<BatchExecuteStatementRequest>(request).unwrap()
    };
    let outcome = |request| {
        db.batch_execute_statement(request)
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    assert_eq!(
        outcome(request(Some("BOGUS"))),
        Err(format!(
            "2 validation errors detected: {BAD_CAPACITY_MODE}; {too_many}"
        ))
    );
    assert_eq!(
        outcome(request(None)),
        Err(format!("1 validation error detected: {too_many}"))
    );
}

/// A transaction's token, statements and ReturnConsumedCapacity are listed
/// together, in that order, rather than the token or the mode answering on
/// its own (captured eu-west-2 and us-east-1, 2026-10-10, identical).
#[test]
fn a_transaction_lists_its_token_and_capacity_mode_with_its_statements() {
    let db = setup();
    let token = "t".repeat(40);
    let long = select_of_length(9_000);
    let request: ExecuteTransactionRequest = serde_json::from_value(json!({
        "TransactStatements": [{"Statement": long}],
        "ReturnConsumedCapacity": "BOGUS",
        "ClientRequestToken": token
    }))
    .unwrap();
    assert_eq!(
        db.execute_transaction(request)
            .map(|_| ())
            .map_err(|e| e.to_string()),
        Err(format!(
            "3 validation errors detected: Value '{token}' at 'clientRequestToken' failed to \
             satisfy constraint: Member must have length less than or equal to 36; {}; \
             {BAD_CAPACITY_MODE}",
            too_long(&long, "transactStatements.1.member.statement")
        ))
    );
}
