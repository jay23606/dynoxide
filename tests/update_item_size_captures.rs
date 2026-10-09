//! Every UpdateItem size probe captured against eu-west-2 on 9 October 2026,
//! replayed against the engine.
//!
//! Each case is an UpdateItem whose padding was bisected against DynamoDB's
//! update gate. eu-west-2 accepted the finished item at `accepted_finished`
//! bytes and refused it one byte larger, every accepted item was read back, and
//! the finished size was calibrated with PutItem, which is flat at 409,600. A
//! case accepted at 409,600 was held by the finished item rather than by the
//! request charge.
//!
//! The fixture writes the padding as `{"$pad": "<char>"}` wherever it appears
//! (in the request and in the finished item), and this file expands it to the
//! length a case needs. Each case runs against a fresh copy of its seed item.

use dynoxide::Database;
use dynoxide::actions::create_table::CreateTableRequest;
use dynoxide::actions::delete_item::DeleteItemRequest;
use dynoxide::actions::get_item::GetItemRequest;
use dynoxide::actions::put_item::PutItemRequest;
use dynoxide::actions::update_item::UpdateItemRequest;
use dynoxide::types::*;
use serde_json::{Value, json};
use std::collections::HashMap;

const FIXTURE: &str = include_str!("fixtures/update_item_size_captures.json");
const REFUSAL: &str = "Item size to update has exceeded the maximum allowed size";
const KEY: &str = "a";
const TABLE: &str = "Captures";

fn make_db() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(CreateTableRequest {
        table_name: TABLE.to_string(),
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

/// Replace every `{"$pad": "<char>"}` with that character repeated `len` times.
fn expand(value: &Value, len: usize) -> Value {
    match value {
        Value::Object(map) => {
            if let (1, Some(Value::String(ch))) = (map.len(), map.get("$pad")) {
                return Value::String(ch.repeat(len));
            }
            Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), expand(v, len)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| expand(v, len)).collect()),
        other => other.clone(),
    }
}

fn item_from(value: &Value, len: usize) -> HashMap<String, AttributeValue> {
    let mut item: HashMap<String, AttributeValue> =
        serde_json::from_value(expand(value, len)).expect("fixture item");
    item.insert("pk".to_string(), AttributeValue::S(KEY.to_string()));
    item
}

/// Sets come back in whatever order the engine keeps them, so compare sorted.
fn canonical(value: &AttributeValue) -> AttributeValue {
    match value {
        AttributeValue::SS(v) => AttributeValue::SS(sorted(v)),
        AttributeValue::NS(v) => AttributeValue::NS(sorted(v)),
        AttributeValue::BS(v) => AttributeValue::BS(sorted(v)),
        AttributeValue::L(v) => AttributeValue::L(v.iter().map(canonical).collect()),
        AttributeValue::M(m) => {
            AttributeValue::M(m.iter().map(|(k, v)| (k.clone(), canonical(v))).collect())
        }
        other => other.clone(),
    }
}

fn sorted<T: Clone + Ord>(v: &[T]) -> Vec<T> {
    let mut v = v.to_vec();
    v.sort();
    v
}

fn canonical_item(item: &HashMap<String, AttributeValue>) -> HashMap<String, AttributeValue> {
    item.iter()
        .map(|(k, v)| (k.clone(), canonical(v)))
        .collect()
}

struct Case<'a> {
    id: &'a str,
    what: &'a str,
    seed: &'a Value,
    request: &'a Value,
    finished: &'a Value,
    accepted_finished: usize,
}

impl Case<'_> {
    /// Put the seed item back (or clear the key), then send the update padded
    /// to `len`.
    fn run(&self, db: &Database, len: usize) -> Result<(), String> {
        match self.seed {
            Value::Null => {
                db.delete_item(DeleteItemRequest {
                    table_name: TABLE.to_string(),
                    key: HashMap::from([("pk".to_string(), AttributeValue::S(KEY.to_string()))]),
                    ..Default::default()
                })
                .unwrap();
            }
            seed => {
                db.put_item(PutItemRequest {
                    table_name: TABLE.to_string(),
                    item: item_from(seed, len),
                    ..Default::default()
                })
                .unwrap();
            }
        }

        let mut request = expand(self.request, len);
        let fields = request.as_object_mut().expect("fixture request");
        fields.insert("TableName".to_string(), json!(TABLE));
        fields.insert("Key".to_string(), json!({ "pk": { "S": KEY } }));
        let request: UpdateItemRequest = serde_json::from_value(request)
            .unwrap_or_else(|e| panic!("{}: fixture request does not parse: {e}", self.id));
        db.update_item(request)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn stored(&self, db: &Database) -> HashMap<String, AttributeValue> {
        db.get_item(GetItemRequest {
            table_name: TABLE.to_string(),
            key: HashMap::from([("pk".to_string(), AttributeValue::S(KEY.to_string()))]),
            consistent_read: Some(true),
            ..Default::default()
        })
        .unwrap()
        .item
        .unwrap_or_default()
    }

    /// Accepted at DynamoDB's figure, stored exactly as DynamoDB stored it, and
    /// refused one byte larger. Returns what went wrong, if anything.
    fn check(&self, db: &Database) -> Option<String> {
        let base = item_size(&item_from(self.finished, 0));
        let len = self.accepted_finished - base;
        let label = format!("{} ({})", self.id, self.what);

        if let Err(e) = self.run(db, len) {
            return Some(format!(
                "{label}: refused at {}, which eu-west-2 accepts: {e}",
                self.accepted_finished
            ));
        }
        let expected = item_from(self.finished, len);
        let stored = self.stored(db);
        if canonical_item(&stored) != canonical_item(&expected) {
            return Some(format!(
                "{label}: stored item differs from the one eu-west-2 stored"
            ));
        }
        match self.run(db, len + 1) {
            Ok(()) => Some(format!(
                "{label}: accepted at {}, which eu-west-2 refuses",
                self.accepted_finished + 1
            )),
            Err(e) if e != REFUSAL => Some(format!(
                "{label}: refused one byte over for the wrong reason: {e}"
            )),
            Err(_) => None,
        }
    }
}

#[test]
fn every_captured_update_lands_on_the_ceiling_eu_west_2_measured() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture parses");
    let cases = fixture["cases"].as_array().expect("cases");
    assert!(cases.len() >= 90, "the fixture holds {} cases", cases.len());

    let db = make_db();
    let mut failures = Vec::new();
    for case in cases {
        let case = Case {
            id: case["id"].as_str().unwrap(),
            what: case["what"].as_str().unwrap(),
            seed: &case["seed"],
            request: &case["request"],
            finished: &case["finished"],
            accepted_finished: case["accepted_finished"].as_u64().unwrap() as usize,
        };
        if let Some(failure) = case.check(&db) {
            failures.push(failure);
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} captured updates disagree with eu-west-2:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
