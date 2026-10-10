//! A transaction's action or statement list longer than DynamoDB allows is
//! echoed in its constraint message as the Java model's `toString`: every
//! field of every action named, every attribute value with all sixteen of its
//! fields, and a map's entries in the order of a Java hash map. An empty list
//! is echoed as `[]`. Each expected head and tail below is DynamoDB's own text
//! for the same request (captured us-east-1, eu-west-1 and us-west-2,
//! 2026-10-10; eu-west-2 echoes no list for TransactWriteItems and
//! TransactGetItems, and some hosts print a value as an object hash, so the
//! form most regions give is the one followed). The requests use the capture's
//! own table name, so the whole message can be compared by length too.

use dynoxide::Database;
use dynoxide::actions::execute_transaction::ExecuteTransactionRequest;
use dynoxide::actions::transact_get_items::TransactGetItemsRequest;
use dynoxide::actions::transact_write_items::TransactWriteItemsRequest;
use serde_json::{Value, json};

const TABLE: &str = "_capture_20261010_mcii_none";

fn assert_echo(message: &str, head: &str, tail: &str, len: usize) {
    assert!(
        message.starts_with(head),
        "head differs:\n{}\n---\n{}",
        &message[..message.len().min(head.len() + 200)],
        head
    );
    assert!(
        message.ends_with(tail),
        "tail differs: {}",
        &message[message.len().saturating_sub(400)..]
    );
    assert_eq!(message.len(), len);
}

#[test]
fn an_empty_statement_list_is_echoed_as_brackets() {
    let db = Database::memory().unwrap();
    let err = db
        .execute_transaction(
            serde_json::from_value::<ExecuteTransactionRequest>(json!({"TransactStatements": []}))
                .unwrap(),
        )
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "1 validation error detected: Value '[]' at 'transactStatements' failed to satisfy \
         constraint: Member must have length greater than or equal to 1"
    );
}

/// The capture's first statement also carried `ReturnValuesOnConditionCheckFailure:
/// ALL_OLD`, which DynamoDB echoes; Dynoxide doesn't read that member of a
/// transaction's statement, so this request leaves it out and expects `null`.
#[test]
fn a_statement_list_over_100_is_echoed_in_the_java_form() {
    let db = Database::memory().unwrap();
    let mut statements = vec![json!({
        "Statement": format!("UPDATE \"{TABLE}\" SET a = ? WHERE pk = ?"),
        "Parameters": [{"N": "1"}, {"S": "k"}]
    })];
    statements.extend(
        (0..100).map(|_| json!({"Statement": format!("SELECT * FROM \"{TABLE}\" WHERE pk = 'a'")})),
    );
    let err = db
        .execute_transaction(
            serde_json::from_value::<ExecuteTransactionRequest>(
                json!({"TransactStatements": statements}),
            )
            .unwrap(),
        )
        .unwrap_err();
    assert_echo(
        &err.to_string(),
        concat!(
            "1 validation error detected: Value '[ParameterizedStatement(statement=UPDATE \"_capture_20261010_mcii_none\" SET a = ? WHERE pk = ?",
            ", parameters=[AttributeValue(s=null, n=1, b=null, iNT=null, dECIMAL=null",
            ", dOUBLE=null, sS=null, nS=null, bS=null, iNTSET=null, dECIMALSET=null",
            ", dOUBLESET=null, m=null, l=null, nULL=null, bOOL=null), AttributeValue(s=k",
            ", n=null, b=null, iNT=null, dECIMAL=null, dOUBLE=null, sS=null, nS=null",
            ", bS=null, iNTSET=null, dECIMALSET=null, dOUBLESET=null, m=null, l=null",
            ", nULL=null, bOOL=null)], returnValuesOnConditionCheckFailure=null)",
            ", ParameterizedStatement(statement=SELECT * FROM \"_capture_20261010_mcii_none\" WHERE pk = 'a'",
            ", parameters=null, returnValuesOnConditionCheckFailure=null), ",
        ),
        concat!(
            "returnValuesOnConditionCheckFailure=null)",
            ", ParameterizedStatement(statement=SELECT * FROM \"_capture_20261010_mcii_none\" WHERE pk = 'a'",
            ", parameters=null",
            ", returnValuesOnConditionCheckFailure=null)]' at 'transactStatements' failed to satisfy constraint: Member must have length less than or equal to 100",
        ),
        15961,
    );
}

#[test]
fn an_action_list_over_100_is_echoed_in_the_java_form() {
    let db = Database::memory().unwrap();
    let key = json!({"pk": {"S": "k"}});
    let full = |mut action: Value| {
        action["TableName"] = json!(TABLE);
        action["ConditionExpression"] = json!("#n = :v");
        action["ExpressionAttributeNames"] = json!({"#n": "a"});
        action["ExpressionAttributeValues"] = json!({":v": {"N": "1"}});
        action["ReturnValuesOnConditionCheckFailure"] = json!("ALL_OLD");
        action
    };
    let mut items = vec![
        json!({"ConditionCheck": full(json!({"Key": key}))}),
        json!({"Delete": full(json!({"Key": key}))}),
        json!({"Update": full(json!({"Key": key, "UpdateExpression": "SET #n = :v"}))}),
        json!({"Put": full(json!({"Item": {
            "pk": {"S": "p"},
            "b": {"BOOL": true},
            "l": {"L": [{"N": "1"}]},
            "m": {"M": {"x": {"S": "y"}}},
            "ss": {"SS": ["a"]},
            "nul": {"NULL": true},
            "bin": {"B": "YWJj"}
        }}))}),
    ];
    items.extend(
        (0..97)
            .map(|i| json!({"Put": {"TableName": TABLE, "Item": {"pk": {"S": format!("t{i}")}}}})),
    );
    let err = db
        .transact_write_items(
            serde_json::from_value::<TransactWriteItemsRequest>(json!({"TransactItems": items}))
                .unwrap(),
        )
        .unwrap_err();
    assert_echo(
        &err.to_string(),
        concat!(
            "1 validation error detected: Value '[TransactWriteItem(conditionCheck=ConditionCheck(key={pk=AttributeValue(s=k",
            ", n=null, b=null, iNT=null, dECIMAL=null, dOUBLE=null, sS=null, nS=null",
            ", bS=null, iNTSET=null, dECIMALSET=null, dOUBLESET=null, m=null, l=null",
            ", nULL=null, bOOL=null)}, tableName=_capture_20261010_mcii_none",
            ", conditionExpression=#n = :v, expressionAttributeNames={#n=a}",
            ", expressionAttributeValues={:v=AttributeValue(s=null, n=1, b=null, iNT=null",
            ", dECIMAL=null, dOUBLE=null, sS=null, nS=null, bS=null, iNTSET=null",
            ", dECIMALSET=null, dOUBLESET=null, m=null, l=null, nULL=null, bOOL=null)}",
            ", returnValuesOnConditionCheckFailure=ALL_OLD, workloadProfileName=null)",
            ", put=null, delete=null, update=null), TransactWriteItem(conditionCheck=null",
            ", put=null, delete=Delete(key={pk=AttributeValue(s=k, n=null, b=null",
            ", iNT=null, dECIMAL=null, dOUBLE=null, sS=null, nS=null, bS=null",
            ", iNTSET=null, dECIMALSET=null, dOUBLESET=null, m=null, l=null, nULL=null",
            ", bOOL=null)}, tableName=_capture_20261010_mcii_none",
            ", conditionExpression=#n = :v, expressionAttributeNames={#n=a}",
            ", expressionAttributeValues={:v=AttributeValue(s=null, n=1, b=null, iNT=null",
            ", dECIMAL=null, dOUBLE=null, sS=null, nS=null, bS=null, iNTSET=null",
            ", dECIMALSET=null, dOUBLESET=null, m=null, l=null, nULL=null, bOOL=null)}",
            ", returnValuesOnConditionCheckFailure=ALL_OLD, workloadProfileName=null)",
            ", update=null), TransactWriteItem(conditionCheck=null, put=null, delete=null",
            ", update=Update(key={pk=AttributeValue(s=k, n=null, b=null, iNT=null",
            ", dECIMAL=null, dOUBLE=null, sS=null, nS=null, bS=null, iNTSET=null",
            ", dECIMALSET=null, dOUBLESET=null, m=null, l=null, nULL=null, bOOL=null)}",
            ", updateExpression=SET #n = :v, tableName=_capture_20261010_mcii_none",
            ", conditionExpression=#n = :v, expressionAttributeNames={#n=a}",
            ", expressionAttributeValues={:v=AttributeValue(s=null, n=1, b=null, iNT=null",
            ", dECIMAL=null, dOUBLE=null, sS=null, nS=null, bS=null, iNTSET=null",
            ", dECIMALSET=null, dOUBLESET=null, m=null, l=null, nULL=null, bOOL=null)}",
            ", returnValuesOnConditionCheckFailure=ALL_OLD, workloadProfileName=null))",
            ", TransactWriteItem(conditionCheck=null",
            ", put=Put(item={ss=AttributeValue(s=null, n=null, b=null, iNT=null",
            ", dECIMAL=null, dOUBLE=null, sS=[a], nS=null, bS=null, iNTSET=null",
            ", dECIMALSET=null, dOUBLESET=null, m=null, l=null, nULL=null, bOOL=null)",
            ", b=AttributeValue(s=null, n=null, b=null, iNT=null, dECIMAL=null",
            ", dOUBLE=null, sS=null, nS=null, bS=null, iNTSET=null, dECIMALSET=null",
            ", dOUBLESET=null, m=null, l=null, nULL=null, bOOL=true)",
            ", nul=AttributeValue(s=null, n=null, b=null, iNT=null, dECIMAL=null",
            ", dOUBLE=null, sS=null, nS=null, bS=null, iNTSET=null, dECIMALSET=null",
            ", dOUBLESET=null, m=null, l=null, nULL=true, bOOL=null)",
            ", bin=AttributeValue(s=null, n=null",
            ", b=java.nio.HeapByteBuffer[pos=0 lim=3 cap=3], iNT=null, dECIMAL=null",
            ", dOUBLE=null, sS=null, nS=null, bS=null, iNTSET=null, dECIMALSET=null",
            ", dOUBLESET=null, m=null, l=null, nULL=null, bOOL=null)",
            ", pk=AttributeValue(s=p, n=null, b=null, iNT=null, dECIMAL=null, dOUBLE=null",
            ", sS=null, nS=null, bS=null, iNTSET=null, dECIMALSET=null, dOUBLESET=null",
            ", m=null, l=null, nULL=null, bOOL=null), l=AttributeValue(s=null, n=null",
            ", b=null, iNT=null, dECIMAL=null, dOUBLE=null, sS=null, nS=null, bS=null",
            ", iNTSET=null, dECIMALSET=null, dOUBLESET=null, m=null",
            ", l=[AttributeValue(s=null, n=1, b=null, iNT=null, dECIMAL=null, dOUBLE=null",
            ", sS=null, nS=null, bS=null, iNTSET=null, dECIMALSET=null, dOUBLESET=null",
            ", m=null, l=null, nULL=null, bOOL=null)], nULL=null, bOOL=null)",
            ", m=AttributeValue(s=null, n=null, b=null, iNT=null, dECIMAL=null",
            ", dOUBLE=null, sS=null, nS=null, bS=null, iNTSET=null, dECIMALSET=null",
            ", dOUBLESET=null, m={x=AttributeValue(s=y, n=null, b=null, iNT=null",
            ", dECIMAL=null, dOUBLE=null, sS=null, nS=null, bS=null, iNTSET=null",
            ", dECIMALSET=null, dOUBLESET=null, m=null, l=null, nULL=null, bOOL=null)}",
            ", l=null, nULL=null, bOOL=null)}, tableName=_capture_20261010_mcii_none",
            ", conditionExpression=#n = :v, expressionAttributeNames={#n=a}",
            ", expressionAttributeValues={:v=AttributeValue(s=null, n=1, b=null, iNT=null",
            ", dECIMAL=null, dOUBLE=null, sS=null, nS=null, bS=null, iNTSET=null",
            ", dECIMALSET=null, dOUBLESET=null, m=null, l=null, nULL=null, bOOL=null)}",
            ", returnValuesOnConditionCheckFailure=ALL_OLD, workloadProfileName=null)",
            ", delete=null, update=null), TransactWriteItem(conditionCheck=null",
            ", put=Put(item={pk=AttributeValue(s=t0, n=null, b=null, iNT=null",
            ", dECIMAL=null, dOUBLE=null, sS=null, nS=null, bS=null, iNTSET=null",
            ", dECIMALSET=null, dOUBLESET=null, m=null, l=null, nULL=null, bOOL=null)}",
            ", tableName=_capture_20261010_mcii_none, conditionExpression=null",
            ", expressionAttributeNames=null, expressionAttributeValues=null",
            ", returnValuesOnConditionCheckFailure=null, workloadProfileName=null)",
            ", delete=null, update=null), ",
        ),
        concat!(
            "1010_mcii_none, conditionExpression=null, expressionAttributeNames=null",
            ", expressionAttributeValues=null, returnValuesOnConditionCheckFailure=null",
            ", workloadProfileName=null), delete=null",
            ", update=null)]' at 'transactItems' failed to satisfy constraint: Member must have length less than or equal to 100",
        ),
        49744,
    );
}

#[test]
fn a_get_list_over_100_is_echoed_in_the_java_form() {
    let db = Database::memory().unwrap();
    let mut items = vec![json!({"Get": {
        "TableName": TABLE,
        "Key": {"pk": {"S": "k"}},
        "ProjectionExpression": "#n",
        "ExpressionAttributeNames": {"#n": "a"}
    }})];
    items.extend(
        (0..100)
            .map(|i| json!({"Get": {"TableName": TABLE, "Key": {"pk": {"S": format!("g{i}")}}}})),
    );
    let err = db
        .transact_get_items(
            serde_json::from_value::<TransactGetItemsRequest>(json!({"TransactItems": items}))
                .unwrap(),
        )
        .unwrap_err();
    assert_echo(
        &err.to_string(),
        concat!(
            "1 validation error detected: Value '[TransactGetItem(get=Get(key={pk=AttributeValue(s=k",
            ", n=null, b=null, iNT=null, dECIMAL=null, dOUBLE=null, sS=null, nS=null",
            ", bS=null, iNTSET=null, dECIMALSET=null, dOUBLESET=null, m=null, l=null",
            ", nULL=null, bOOL=null)}, tableName=_capture_20261010_mcii_none",
            ", projectionExpression=#n, expressionAttributeNames={#n=a}",
            ", workloadProfileName=null))",
            ", TransactGetItem(get=Get(key={pk=AttributeValue(s=g0, n=null, b=null",
            ", iNT=null, dECIMAL=null, dOUBLE=null, sS=null, nS=null, bS=null",
            ", iNTSET=null, dECIMALSET=null, dOUBLESET=null, m=null, l=null, nULL=null",
            ", bOOL=null)}, tableName=_capture_20261010_mcii_none",
            ", projectionExpression=null, expressionAttributeNames=null",
            ", workloadProfileName=null)), ",
        ),
        concat!(
            "l, dECIMALSET=null, dOUBLESET=null, m=null, l=null, nULL=null, bOOL=null)}",
            ", tableName=_capture_20261010_mcii_none, projectionExpression=null",
            ", expressionAttributeNames=null",
            ", workloadProfileName=null))]' at 'transactItems' failed to satisfy constraint: Member must have length less than or equal to 100",
        ),
        34969,
    );
}
