//! List indexes when one clause of an update grows or shrinks a list. Every
//! index means what it meant in the item as it was: a SET or ADD past the end
//! appends, and the appends land in the order of their indexes; an ADD,
//! DELETE or REMOVE at an index the list did not reach never sees an element
//! another clause appended; and a DELETE that empties a set held in a list
//! element takes the element out with the REMOVEs, at its original index.
//!
//! Each expectation is the answer eu-west-2 gave on 20 October 2026, with the
//! item read back afterwards; us-east-1 gave the same on every one. The tables
//! are data captured from DynamoDB; capture the requests afresh to change them.

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::execute_statement::ExecuteStatementRequest;
use dynoxide::actions::get_item::GetItemRequest;
use dynoxide::actions::update_item::UpdateItemRequest;
use serde_json::{Value, json};

const ITEM: &str = r##"{"pk":{"S":"k"},"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##;

/// What DynamoDB did: the attributes other than the key afterwards, or the
/// refusal's message (the item is then left as it was).
enum Outcome {
    Ok(&'static str),
    Err(&'static str),
}
use Outcome::{Err, Ok};

/// (case, UpdateExpression, ExpressionAttributeValues, outcome)
const UPDATE_ITEM: &[(&str, &str, &str, Outcome)] = &[
    (
        r##"ADD l[3] alone (index past the end)"##,
        r##"ADD l[3] :n"##,
        r##"{":n":{"N":"5"}}"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"},{"N":"5"}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"REMOVE l[3] alone (past the end, control)"##,
        r##"REMOVE l[3]"##,
        r##"{}"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"SET l[10] then ADD l[3]"##,
        r##"SET l[10] = :s ADD l[3] :n"##,
        r##"{":s":{"N":"7"},":n":{"N":"5"}}"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"},{"N":"5"},{"N":"7"}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"SET l[10] (a string) then ADD l[3]"##,
        r##"SET l[10] = :s ADD l[3] :n"##,
        r##"{":s":{"S":"x"},":n":{"N":"5"}}"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"},{"N":"5"},{"S":"x"}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"SET l[10] then REMOVE l[3]"##,
        r##"SET l[10] = :s REMOVE l[3]"##,
        r##"{":s":{"N":"7"}}"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"},{"N":"7"}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"SET l[10] then REMOVE l[2] (an original element)"##,
        r##"SET l[10] = :s REMOVE l[2]"##,
        r##"{":s":{"N":"7"}}"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"7"}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"SET l[10] then DELETE l[3]"##,
        r##"SET l[10] = :s DELETE l[3] :a"##,
        r##"{":s":{"SS":["a","b"]},":a":{"SS":["a"]}}"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"},{"SS":["a","b"]}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"SET l[3] then REMOVE l[3] (same path)"##,
        r##"SET l[3] = :s REMOVE l[3]"##,
        r##"{":s":{"N":"7"}}"##,
        Err(
            r##"1 validation error detected: Invalid UpdateExpression: Two document paths overlap with each other; must remove or rewrite one of these paths; path one: [l, [3]], path two: [l, [3]]"##,
        ),
    ),
    (
        r##"SET l[10], l[11] (two appends)"##,
        r##"SET l[10] = :a, l[11] = :b"##,
        r##"{":a":{"N":"7"},":b":{"N":"8"}}"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"},{"N":"7"},{"N":"8"}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"SET l[11], l[10] (two appends, written the other way)"##,
        r##"SET l[11] = :b, l[10] = :a"##,
        r##"{":a":{"N":"7"},":b":{"N":"8"}}"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"},{"N":"7"},{"N":"8"}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"DELETE ls[0] :a alone (empties the set)"##,
        r##"DELETE ls[0] :a"##,
        r##"{":a":{"SS":["a"]}}"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"}]},"ls":{"L":[{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"DELETE ls[0] :a then REMOVE ls[1]"##,
        r##"DELETE ls[0] :a REMOVE ls[1]"##,
        r##"{":a":{"SS":["a"]}}"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"}]},"ls":{"L":[{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"DELETE ls[0] :a then SET ls[1]"##,
        r##"DELETE ls[0] :a SET ls[1] = :q"##,
        r##"{":a":{"SS":["a"]},":q":{"S":"q"}}"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"}]},"ls":{"L":[{"S":"q"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"DELETE lss[0] :a, lss[1] :b"##,
        r##"DELETE lss[0] :a, lss[1] :b"##,
        r##"{":a":{"SS":["a"]},":b":{"SS":["b"]}}"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"DELETE ls2[0] :a, ls2[1] :b (both empty)"##,
        r##"DELETE ls2[0] :a, ls2[1] :b"##,
        r##"{":a":{"SS":["a"]},":b":{"SS":["b"]}}"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"REMOVE ls[1] then DELETE ls[0] :a"##,
        r##"REMOVE ls[1] DELETE ls[0] :a"##,
        r##"{":a":{"SS":["a"]}}"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"}]},"ls":{"L":[{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
];

/// (case, PartiQL SET or REMOVE clauses, outcome)
const PARTIQL: &[(&str, &str, Outcome)] = &[
    (
        r##"REMOVE l[3] alone (past the end, control)"##,
        r##"REMOVE l[3]"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"SET l[10] then REMOVE l[3]"##,
        r##"SET l[10] = 7 REMOVE l[3]"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"},{"N":"7"}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"SET l[10] then REMOVE l[2] (an original element)"##,
        r##"SET l[10] = 7 REMOVE l[2]"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"7"}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"SET l[3] then REMOVE l[3] (same path)"##,
        r##"SET l[3] = 7 REMOVE l[3]"##,
        Err(
            r##"Two document paths overlap with each other; must remove or rewrite one of these paths; path one: [l, [3]], path two: [l, [3]]"##,
        ),
    ),
    (
        r##"SET l[10], l[11] (two appends)"##,
        r##"SET l[10] = 7, l[11] = 8"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"},{"N":"7"},{"N":"8"}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
    (
        r##"SET l[11], l[10] (two appends, written the other way)"##,
        r##"SET l[11] = 8, l[10] = 7"##,
        Ok(
            r##"{"l":{"L":[{"N":"1"},{"N":"2"},{"N":"3"},{"N":"7"},{"N":"8"}]},"ls":{"L":[{"SS":["a"]},{"S":"x"},{"S":"y"}]},"ls2":{"L":[{"SS":["a"]},{"SS":["b"]}]},"lss":{"L":[{"SS":["a"]},{"SS":["b","c"]},{"S":"z"}]}}"##,
        ),
    ),
];

fn setup() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(
        serde_json::from_value::<CreateTableRequest>(json!({
            "TableName": "Lists",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap(),
    )
    .unwrap();
    let item: Value = serde_json::from_str(ITEM).unwrap();
    db.put_item(serde_json::from_value(json!({"TableName": "Lists", "Item": item})).unwrap())
        .unwrap();
    db
}

/// The attributes other than the key, as stored.
fn stored(db: &Database) -> Value {
    let item = db
        .get_item(
            serde_json::from_value::<GetItemRequest>(json!({
                "TableName": "Lists", "Key": {"pk": {"S": "k"}}, "ConsistentRead": true
            }))
            .unwrap(),
        )
        .unwrap()
        .item
        .unwrap();
    let mut value = serde_json::to_value(item).unwrap();
    value.as_object_mut().unwrap().remove("pk");
    value
}

/// Compare `got` (the call's result and the item afterwards) with `want`.
fn check(case: &str, result: Result<(), String>, after: Value, want: &Outcome) -> Option<String> {
    let original = {
        let mut item: Value = serde_json::from_str(ITEM).unwrap();
        item.as_object_mut().unwrap().remove("pk");
        item
    };
    match (want, result) {
        (Ok(item), std::result::Result::Ok(())) => {
            let item: Value = serde_json::from_str(item).unwrap();
            (after != item).then(|| format!("{case}\n  want: {item}\n  got:  {after}"))
        }
        (Err(message), std::result::Result::Err(got)) => {
            if !got.ends_with(message) {
                Some(format!(
                    "{case}\n  want error: {message}\n  got error:  {got}"
                ))
            } else {
                (after != original)
                    .then(|| format!("{case}\n  a refusal changed the item: {after}"))
            }
        }
        (Ok(_), std::result::Result::Err(got)) => Some(format!("{case}\n  refused: {got}")),
        (Err(message), std::result::Result::Ok(())) => {
            Some(format!("{case}\n  accepted, want: {message}"))
        }
    }
}

#[test]
fn update_item_reads_every_index_as_the_item_had_it() {
    assert_eq!(UPDATE_ITEM.len(), 16, "a case was added or lost");
    let mut wrong = Vec::new();
    for (case, expression, values, want) in UPDATE_ITEM {
        let db = setup();
        let values: Value = serde_json::from_str(values).unwrap();
        let mut request = json!({
            "TableName": "Lists",
            "Key": {"pk": {"S": "k"}},
            "UpdateExpression": expression
        });
        if values.as_object().is_some_and(|v| !v.is_empty()) {
            request["ExpressionAttributeValues"] = values;
        }
        let result = db
            .update_item(serde_json::from_value::<UpdateItemRequest>(request).unwrap())
            .map(|_| ())
            .map_err(|e| e.to_string());
        wrong.extend(check(case, result, stored(&db), want));
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn partiql_reads_every_index_as_the_item_had_it() {
    assert_eq!(PARTIQL.len(), 6, "a case was added or lost");
    let mut wrong = Vec::new();
    for (case, clauses, want) in PARTIQL {
        let db = setup();
        let statement = format!("UPDATE \"Lists\" {clauses} WHERE pk = 'k'");
        let result = db
            .execute_statement(
                serde_json::from_value::<ExecuteStatementRequest>(json!({"Statement": statement}))
                    .unwrap(),
            )
            .map(|_| ())
            .map_err(|e| e.to_string());
        wrong.extend(check(case, result, stored(&db), want));
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
