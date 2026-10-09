//! CreateTable and UpdateTable messages about AttributeDefinitions that index
//! key schemas do not match: an index key attribute with no definition, and a
//! definition no key uses.
//!
//! DynamoDB lists the attribute names in the iteration order of the Java hash
//! tables it collects them in, which is deterministic for a given set of names,
//! so the expected strings below are exact. Captured in eu-west-2 and us-east-1
//! on 2026-10-09; the two regions agree.

use dynoxide::Database;
use dynoxide::errors::DynoxideError;

fn json<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
    serde_json::from_value(value).unwrap()
}

fn defs(names: &[&str]) -> serde_json::Value {
    serde_json::Value::Array(
        names
            .iter()
            .map(|n| serde_json::json!({"AttributeName": n, "AttributeType": "S"}))
            .collect(),
    )
}

fn ks(elements: &[(&str, &str)]) -> serde_json::Value {
    serde_json::Value::Array(
        elements
            .iter()
            .map(|(n, t)| serde_json::json!({"AttributeName": n, "KeyType": t}))
            .collect(),
    )
}

fn gsi(name: &str, key_schema: &[(&str, &str)]) -> serde_json::Value {
    serde_json::json!({
        "IndexName": name,
        "KeySchema": ks(key_schema),
        "Projection": {"ProjectionType": "KEYS_ONLY"}
    })
}

/// CreateTable through the JSON path, returning the error text either way the
/// request is refused (as it is read, or by the action).
fn create(request: serde_json::Value) -> String {
    let db = Database::memory().unwrap();
    let mut request = request;
    request["TableName"] = serde_json::json!("Messages");
    request["BillingMode"] = serde_json::json!("PAY_PER_REQUEST");
    let parsed: Result<dynoxide::actions::create_table::CreateTableRequest, _> =
        serde_json::from_value(request);
    match parsed {
        Err(e) => e.to_string(),
        Ok(request) => match db.create_table(request) {
            Err(e) => e.to_string(),
            Ok(_) => "created".to_string(),
        },
    }
}

const UNUSED: &str =
    "One or more parameter values were invalid: Some AttributeDefinitions are not used.";
const UNDEFINED: &str = "One or more parameter values were invalid: Some index key attributes are not defined in AttributeDefinitions.";
const NO_DEFINITION: &str = "Invalid KeySchema: Some index key attribute have no definition";

#[test]
fn an_unused_definition_on_a_table_with_an_index_lists_both_sets() {
    let cases: [(&str, serde_json::Value, &str); 5] = [
        (
            "one GSI",
            serde_json::json!({
                "AttributeDefinitions": defs(&["PK", "h", "a", "unused"]),
                "KeySchema": ks(&[("PK", "HASH")]),
                "GlobalSecondaryIndexes": [gsi("gsi", &[("h", "HASH"), ("a", "RANGE")])]
            }),
            "AttributeDefinitions: [h, a, unused, PK], keys used: [a, h, PK]",
        ),
        (
            "two GSIs",
            serde_json::json!({
                "AttributeDefinitions": defs(&["orderId", "customerId", "status", "createdAt", "notUsed"]),
                "KeySchema": ks(&[("orderId", "HASH")]),
                "GlobalSecondaryIndexes": [
                    gsi("byCustomer", &[("customerId", "HASH"), ("createdAt", "RANGE")]),
                    gsi("byStatus", &[("status", "HASH")])
                ]
            }),
            "AttributeDefinitions: [customerId, createdAt, notUsed, orderId, status], keys used: [createdAt, orderId, customerId, status]",
        ),
        (
            "seven definitions, which resizes the definitions table",
            serde_json::json!({
                "AttributeDefinitions": defs(&["PK", "a", "b", "c", "d", "u1", "u2"]),
                "KeySchema": ks(&[("PK", "HASH")]),
                "GlobalSecondaryIndexes": [
                    gsi("gsi0", &[("a", "HASH"), ("b", "RANGE")]),
                    gsi("gsi2", &[("c", "HASH"), ("d", "RANGE")])
                ]
            }),
            "AttributeDefinitions: [a, b, c, d, PK, u1, u2], keys used: [a, b, c, d, PK]",
        ),
        (
            "six definitions",
            serde_json::json!({
                "AttributeDefinitions": defs(&["PK", "a", "b", "c", "d", "u1"]),
                "KeySchema": ks(&[("PK", "HASH")]),
                "GlobalSecondaryIndexes": [
                    gsi("gsi0", &[("a", "HASH"), ("b", "RANGE")]),
                    gsi("gsi2", &[("c", "HASH"), ("d", "RANGE")])
                ]
            }),
            "AttributeDefinitions: [a, b, PK, c, d, u1], keys used: [a, b, c, d, PK]",
        ),
        (
            "an LSI",
            serde_json::json!({
                "AttributeDefinitions": defs(&["pk", "sk", "l1", "spare"]),
                "KeySchema": ks(&[("pk", "HASH"), ("sk", "RANGE")]),
                "LocalSecondaryIndexes": [{
                    "IndexName": "lsi",
                    "KeySchema": ks(&[("pk", "HASH"), ("l1", "RANGE")]),
                    "Projection": {"ProjectionType": "KEYS_ONLY"}
                }]
            }),
            "AttributeDefinitions: [sk, pk, l1, spare], keys used: [l1, sk, pk]",
        ),
    ];
    for (label, request, lists) in cases {
        let err = create(request);
        assert!(
            err.contains(&format!("{UNUSED} {lists}")),
            "{label}: got {err}"
        );
    }
}

#[test]
fn sixteen_names_resize_both_tables_and_keep_dynamodbs_order() {
    let g: Vec<String> = (1..=14).map(|i| format!("g{i}")).collect();
    let mut names: Vec<&str> = vec!["PK"];
    names.extend(g.iter().map(String::as_str));
    names.push("u1");
    let gsis: Vec<serde_json::Value> = g
        .chunks(2)
        .enumerate()
        .map(|(i, pair)| {
            gsi(
                &format!("gsi{}", i * 2),
                &[(&pair[0], "HASH"), (&pair[1], "RANGE")],
            )
        })
        .collect();
    let err = create(serde_json::json!({
        "AttributeDefinitions": defs(&names),
        "KeySchema": ks(&[("PK", "HASH")]),
        "GlobalSecondaryIndexes": gsis
    }));
    assert!(
        err.contains(&format!(
            "{UNUSED} AttributeDefinitions: [g11, g10, g13, g12, g1, g2, g14, g3, g4, g5, g6, g7, \
             g8, g9, PK, u1], keys used: [g11, g10, g13, g12, g1, g2, g14, g3, g4, g5, g6, g7, g8, \
             g9, PK]"
        )),
        "got {err}"
    );
}

#[test]
fn an_unused_definition_on_a_table_without_indexes_keeps_the_count_message() {
    let err = create(serde_json::json!({
        "AttributeDefinitions": defs(&["pk", "extra"]),
        "KeySchema": ks(&[("pk", "HASH")])
    }));
    assert!(
        err.contains(
            "One or more parameter values were invalid: Number of attributes in KeySchema does \
             not exactly match number of attributes defined in AttributeDefinitions"
        ),
        "got {err}"
    );
}

#[test]
fn an_undefined_index_key_attribute_names_the_first_index_missing_one() {
    let cases: [(&str, serde_json::Value, String); 7] = [
        (
            "one of three",
            serde_json::json!({
                "AttributeDefinitions": defs(&["PK", "h", "a"]),
                "KeySchema": ks(&[("PK", "HASH")]),
                "GlobalSecondaryIndexes": [gsi("gsi", &[("h", "HASH"), ("a", "RANGE"), ("zz", "RANGE")])]
            }),
            format!("{UNDEFINED} Keys: [h, zz, a], AttributeDefinitions: [h, a, PK]"),
        ),
        (
            "two indexes, both missing one: the first is named",
            serde_json::json!({
                "AttributeDefinitions": defs(&["PK", "a", "c"]),
                "KeySchema": ks(&[("PK", "HASH")]),
                "GlobalSecondaryIndexes": [
                    gsi("gsi1", &[("a", "HASH"), ("b", "RANGE")]),
                    gsi("gsi2", &[("c", "HASH"), ("z", "RANGE")])
                ]
            }),
            format!("{UNDEFINED} Keys: [a, b], AttributeDefinitions: [a, PK, c]"),
        ),
        (
            "only the second index missing one",
            serde_json::json!({
                "AttributeDefinitions": defs(&["PK", "a", "b", "c"]),
                "KeySchema": ks(&[("PK", "HASH")]),
                "GlobalSecondaryIndexes": [
                    gsi("gsi1", &[("a", "HASH"), ("b", "RANGE")]),
                    gsi("gsi2", &[("c", "HASH"), ("z", "RANGE")])
                ]
            }),
            format!("{UNDEFINED} Keys: [z, c], AttributeDefinitions: [a, b, PK, c]"),
        ),
        (
            "one of four, which resizes the keys table",
            serde_json::json!({
                "AttributeDefinitions": defs(&["PK", "w", "x", "y"]),
                "KeySchema": ks(&[("PK", "HASH")]),
                "GlobalSecondaryIndexes": [gsi("gsi", &[("w", "HASH"), ("x", "RANGE"), ("y", "RANGE"), ("q", "RANGE")])]
            }),
            format!("{UNDEFINED} Keys: [x, y, q, w], AttributeDefinitions: [x, y, PK, w]"),
        ),
        (
            "an LSI",
            serde_json::json!({
                "AttributeDefinitions": defs(&["pk", "sk"]),
                "KeySchema": ks(&[("pk", "HASH"), ("sk", "RANGE")]),
                "LocalSecondaryIndexes": [{
                    "IndexName": "lsi",
                    "KeySchema": ks(&[("pk", "HASH"), ("l", "RANGE")]),
                    "Projection": {"ProjectionType": "KEYS_ONLY"}
                }]
            }),
            format!("{UNDEFINED} Keys: [l, pk], AttributeDefinitions: [sk, pk]"),
        ),
        (
            "fewer definitions than the index has key attributes",
            serde_json::json!({
                "AttributeDefinitions": defs(&["PK", "a"]),
                "KeySchema": ks(&[("PK", "HASH")]),
                "GlobalSecondaryIndexes": [gsi("gsi", &[("a", "HASH"), ("m", "RANGE"), ("n", "RANGE")])]
            }),
            NO_DEFINITION.to_string(),
        ),
        (
            "an undefined key wins over an unused definition",
            serde_json::json!({
                "AttributeDefinitions": defs(&["PK", "a", "spare"]),
                "KeySchema": ks(&[("PK", "HASH")]),
                "GlobalSecondaryIndexes": [gsi("gsi1", &[("a", "HASH"), ("b", "RANGE")])]
            }),
            format!("{UNDEFINED} Keys: [a, b], AttributeDefinitions: [a, PK, spare]"),
        ),
    ];
    for (label, request, message) in cases {
        let err = create(request);
        assert!(err.contains(&message), "{label}: got {err}");
    }
}

fn add_index(attribute_definitions: &[&str], key_schema: &[(&str, &str)]) -> String {
    let db = Database::memory().unwrap();
    db.create_table(json(serde_json::json!({
        "TableName": "Messages",
        "BillingMode": "PAY_PER_REQUEST",
        "AttributeDefinitions": defs(&["pk"]),
        "KeySchema": ks(&[("pk", "HASH")])
    })))
    .unwrap();
    let result: Result<_, DynoxideError> = db.update_table(json(serde_json::json!({
        "TableName": "Messages",
        "AttributeDefinitions": defs(attribute_definitions),
        "GlobalSecondaryIndexUpdates": [{"Create": {
            "IndexName": "added",
            "KeySchema": ks(key_schema),
            "Projection": {"ProjectionType": "KEYS_ONLY"}
        }}]
    })));
    match result {
        Err(e) => e.to_string(),
        Ok(_) => "updated".to_string(),
    }
}

#[test]
fn update_table_applies_the_same_rule_to_a_new_index() {
    let err = add_index(&["pk"], &[("m1", "HASH")]);
    assert!(
        err.contains(&format!(
            "{UNDEFINED} Keys: [m1], AttributeDefinitions: [pk]"
        )),
        "got {err}"
    );
    // Fewer definitions than the new index has key attributes: m3 missing of
    // three with m1 and m2 declared, and m1 missing of two with m2 declared.
    for (declared, key_schema) in [
        (
            &["m1", "m2"][..],
            &[("m1", "HASH"), ("m2", "RANGE"), ("m3", "RANGE")][..],
        ),
        (&["m2"][..], &[("m1", "HASH"), ("m2", "RANGE")][..]),
    ] {
        let err = add_index(declared, key_schema);
        assert!(err.contains(NO_DEFINITION), "{key_schema:?}: got {err}");
    }
}

#[test]
fn a_gsi_key_schema_longer_than_eight_fails_the_request_model() {
    let names = ["h1", "h2", "h3", "h4", "r1", "r2", "r3", "r4", "r5"];
    let mut all = vec!["PK"];
    all.extend(names);
    let key_schema: Vec<(&str, &str)> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (*n, if i < 4 { "HASH" } else { "RANGE" }))
        .collect();
    let err = create(serde_json::json!({
        "AttributeDefinitions": defs(&all),
        "KeySchema": ks(&[("PK", "HASH")]),
        "GlobalSecondaryIndexes": [gsi("gsi", &key_schema)]
    }));
    let rendered: Vec<String> = key_schema
        .iter()
        .map(|(n, t)| format!("KeySchemaElement(attributeName={n}, keyType={t})"))
        .collect();
    assert!(
        err.contains(&format!(
            "1 validation error detected: Value '[{}]' at 'keySchema' failed to satisfy \
             constraint: Member must have length less than or equal to 8",
            rendered.join(", ")
        )),
        "got {err}"
    );
}
