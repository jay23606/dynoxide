use crate::actions::vector_lifecycle::VectorIndexLifecycle;
use crate::actions::{TableDescription, build_table_description};
use crate::errors::{DynoxideError, Result};
use crate::storage_backend::{BackendError, StorageBackend};
use crate::types::ProjectionType;
use serde::{Deserialize, Serialize};

/// Internal deserialization struct for detecting missing TableName.
#[derive(Debug, Default, Deserialize)]
struct DescribeTableRequestRaw {
    #[serde(rename = "TableName", default)]
    table_name: Option<String>,
}

#[derive(Debug, Default)]
pub struct DescribeTableRequest {
    pub table_name: String,
}

impl<'de> serde::Deserialize<'de> for DescribeTableRequest {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let raw = DescribeTableRequestRaw::deserialize(deserializer)?;

        if raw.table_name.is_none() {
            return Err(serde::de::Error::custom(
                "VALIDATION:The parameter 'TableName' is required but was not present in the request",
            ));
        }
        let table_name = raw.table_name.unwrap();

        // Length check (before pattern, matching DynamoDB ordering)
        if table_name.len() < 3 || table_name.len() > 255 {
            return Err(serde::de::Error::custom(
                "VALIDATION:TableName must be at least 3 characters long and at most 255 characters long",
            ));
        }

        // Pattern check
        if !table_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        {
            return Err(serde::de::Error::custom(format!(
                "VALIDATION:1 validation error detected: \
                 Value '{}' at 'tableName' failed to satisfy constraint: \
                 Member must satisfy regular expression pattern: [a-zA-Z0-9_.-]+",
                table_name
            )));
        }

        Ok(DescribeTableRequest { table_name })
    }
}

/// Bytes DynamoDB adds to `TableSizeBytes` for each item on top of the item's
/// own size. There are none: four items written to an eu-west-2 table on
/// 8 October 2026 read back after DynamoDB's refresh as exactly the sum of
/// their item sizes (10,056 bytes), and the table's secondary indexes reported
/// the plain sum of their projected items too.
const TABLE_SIZE_OVERHEAD_PER_ITEM: i64 = 0;

/// The `ItemCount` and `TableSizeBytes` a table description reports, from one
/// query so the two cannot disagree. DynamoDB refreshes both about every six
/// hours; they are reported live here. `None` for both if the query fails.
pub(crate) async fn item_count_and_size<S: StorageBackend>(
    storage: &S,
    table_name: &str,
) -> (Option<i64>, Option<i64>) {
    match storage.item_count_and_size(table_name).await {
        Ok((count, bytes)) => (
            Some(count),
            Some(bytes + count * TABLE_SIZE_OVERHEAD_PER_ITEM),
        ),
        Err(_) => (None, None),
    }
}

/// Fill in each GSI's and LSI's `ItemCount` and `IndexSizeBytes` from what the
/// index holds, live, as `TableSizeBytes` is. Each is the number of entries and
/// the plain sum of their projected item sizes, with no per-entry overhead: the
/// figures DynamoDB reported in eu-west-2 on 9 October 2026 for an ALL GSI, a
/// KEYS_ONLY GSI and a KEYS_ONLY LSI over the same four items. An index whose
/// figures can't be read reports neither.
///
/// A vector index reports its entries and the billable size each write stored
/// with its entry: the vector at 4 bytes per dimension, its attribute's name,
/// and the rest of the projected item. Read back from DynamoDB in eu-west-2 on
/// 9 October 2026: 358 bytes for two entries under an ALL projection and 50
/// under KEYS_ONLY, not the projected items' own sizes.
pub(crate) async fn fill_index_figures<S: StorageBackend>(
    storage: &S,
    desc: &mut TableDescription,
) {
    fn read(result: std::result::Result<(i64, i64), BackendError>) -> (Option<i64>, Option<i64>) {
        match result {
            Ok((count, bytes)) => (Some(count), Some(bytes)),
            Err(_) => (None, None),
        }
    }
    let table_name = desc.table_name.clone();
    for gsi in desc.global_secondary_indexes.iter_mut().flatten() {
        let whole_items =
            gsi.projection.projection_type.clone().unwrap_or_default() == ProjectionType::ALL;
        (gsi.item_count, gsi.index_size_bytes) = read(
            storage
                .gsi_item_count_and_size(&table_name, &gsi.index_name, whole_items)
                .await,
        );
    }
    for lsi in desc.local_secondary_indexes.iter_mut().flatten() {
        let whole_items =
            lsi.projection.projection_type.clone().unwrap_or_default() == ProjectionType::ALL;
        (lsi.item_count, lsi.index_size_bytes) = read(
            storage
                .lsi_item_count_and_size(&table_name, &lsi.index_name, whole_items)
                .await,
        );
    }
    for vix in desc.vector_indexes.iter_mut().flatten() {
        (vix.item_count, vix.index_size_bytes) = read(
            storage
                .vector_item_count_and_size(&table_name, &vix.index_name)
                .await,
        );
    }
}

#[derive(Debug, Default, Serialize)]
pub struct DescribeTableResponse {
    #[serde(rename = "Table")]
    pub table: TableDescription,
}

pub async fn execute<S: StorageBackend>(
    storage: &S,
    request: DescribeTableRequest,
    lifecycle: &VectorIndexLifecycle,
) -> Result<DescribeTableResponse> {
    // Validate table name format before checking existence (DynamoDB validates input first)
    crate::validation::validate_table_name(&request.table_name)?;

    let meta = storage
        .get_table_metadata(&request.table_name)
        .await?
        .ok_or_else(|| {
            DynoxideError::ResourceNotFoundException(format!(
                "Requested resource not found: Table: {} not found",
                request.table_name
            ))
        })?;

    let (item_count, table_size_bytes) = item_count_and_size(storage, &request.table_name).await;

    let vector_phases = lifecycle.phases_armed_on(storage, &request.table_name);
    let mut desc = build_table_description(&meta, item_count, table_size_bytes, &vector_phases);
    fill_index_figures(storage, &mut desc).await;

    Ok(DescribeTableResponse { table: desc })
}

#[cfg(test)]
mod tests {
    use super::TABLE_SIZE_OVERHEAD_PER_ITEM;
    use crate::actions::vector_lifecycle::VectorIndexLifecycle;
    use crate::actions::{
        batch_write_item, create_table, delete_item, describe_table, get_item, put_item,
        transact_write_items, update_item,
    };
    use crate::storage::Storage;
    use crate::types::{Item, item_size};

    fn item(value: serde_json::Value) -> Item {
        serde_json::from_value(value).unwrap()
    }

    fn put(storage: &Storage, value: serde_json::Value) -> Item {
        let stored = item(value.clone());
        let request = serde_json::from_value(serde_json::json!({
            "TableName": "Sized",
            "Item": value
        }))
        .unwrap();
        pollster::block_on(put_item::execute(storage, request)).unwrap();
        stored
    }

    fn described(storage: &Storage) -> (Option<i64>, Option<i64>) {
        let lifecycle = VectorIndexLifecycle::default();
        let request = describe_table::DescribeTableRequest {
            table_name: "Sized".to_string(),
        };
        let table = pollster::block_on(describe_table::execute(storage, request, &lifecycle))
            .unwrap()
            .table;
        (table.item_count, table.table_size_bytes)
    }

    fn expected(items: &[&Item]) -> Option<i64> {
        let bytes: usize = items.iter().map(|i| item_size(i)).sum();
        Some(bytes as i64 + items.len() as i64 * TABLE_SIZE_OVERHEAD_PER_ITEM)
    }

    fn create_sized_table(storage: &Storage) {
        let create = serde_json::from_value(serde_json::json!({
            "TableName": "Sized",
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"}
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "S"}
            ],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap();
        pollster::block_on(create_table::execute(storage, create)).unwrap();
    }

    /// The figure for two small items, worked by hand from DynamoDB's sizing
    /// rules rather than from `item_size`, so the rule itself is pinned. A
    /// string is its UTF-8 length; a number is one byte per two significant
    /// digits plus one; each attribute adds its name's length.
    #[test]
    fn table_size_bytes_for_known_items() {
        let storage = Storage::memory().unwrap();
        create_sized_table(&storage);
        // pk "p" (2 + 1) + sk "s1" (2 + 2) + data 1,000 x (4 + 1,000) = 1,011
        put(
            &storage,
            serde_json::json!({"pk": {"S": "p"}, "sk": {"S": "s1"}, "data": {"S": "x".repeat(1000)}}),
        );
        // pk "p" (2 + 1) + sk "s2" (2 + 2) + n 1 (1 + 1 + 1) = 10
        put(
            &storage,
            serde_json::json!({"pk": {"S": "p"}, "sk": {"S": "s2"}, "n": {"N": "1"}}),
        );
        assert_eq!(
            described(&storage),
            (Some(2), Some(1021 + 2 * TABLE_SIZE_OVERHEAD_PER_ITEM))
        );
    }

    /// BatchWriteItem and TransactWriteItems store their items' sizes too, so
    /// the figure follows them as it follows PutItem.
    #[test]
    fn table_size_bytes_follows_batch_and_transactional_writes() {
        let storage = Storage::memory().unwrap();
        create_sized_table(&storage);

        let a = item(
            serde_json::json!({"pk": {"S": "p"}, "sk": {"S": "a"}, "v": {"S": "x".repeat(700)}}),
        );
        let b = item(serde_json::json!({"pk": {"S": "p"}, "sk": {"S": "b"}, "n": {"N": "-12.5"}}));
        let batch = serde_json::from_value(serde_json::json!({
            "RequestItems": {"Sized": [
                {"PutRequest": {"Item": a}},
                {"PutRequest": {"Item": b}}
            ]}
        }))
        .unwrap();
        pollster::block_on(batch_write_item::execute(&storage, batch)).unwrap();
        assert_eq!(described(&storage), (Some(2), expected(&[&a, &b])));

        // One transaction puts a new item, rewrites a, and deletes b.
        let c = item(
            serde_json::json!({"pk": {"S": "q"}, "sk": {"S": "c"}, "l": {"L": [{"S": "one"}, {"N": "2"}]}}),
        );
        let a2 = item(
            serde_json::json!({"pk": {"S": "p"}, "sk": {"S": "a"}, "v": {"S": "y".repeat(90)}}),
        );
        let transact = serde_json::from_value(serde_json::json!({
            "TransactItems": [
                {"Put": {"TableName": "Sized", "Item": c}},
                {"Put": {"TableName": "Sized", "Item": a2}},
                {"Delete": {"TableName": "Sized", "Key": {"pk": {"S": "p"}, "sk": {"S": "b"}}}}
            ]
        }))
        .unwrap();
        pollster::block_on(transact_write_items::execute(&storage, transact)).unwrap();
        assert_eq!(described(&storage), (Some(2), expected(&[&a2, &c])));
    }

    /// TableSizeBytes is the total size of the items the table holds, and
    /// follows every write that changes it.
    #[test]
    fn table_size_bytes_is_the_size_of_the_stored_items() {
        let storage = Storage::memory().unwrap();
        create_sized_table(&storage);
        assert_eq!(described(&storage), (Some(0), Some(0)));

        let a = put(
            &storage,
            serde_json::json!({"pk": {"S": "p"}, "sk": {"S": "a"}, "data": {"S": "x".repeat(1000)}}),
        );
        let b = put(
            &storage,
            serde_json::json!({"pk": {"S": "p"}, "sk": {"S": "b"}, "n": {"N": "12345.678"}}),
        );
        let c = put(
            &storage,
            serde_json::json!({
                "pk": {"S": "q"}, "sk": {"S": "c"},
                "m": {"M": {"inner": {"L": [{"S": "one"}, {"BOOL": true}, {"NULL": true}]}}},
                "ss": {"SS": ["red", "green"]}
            }),
        );
        assert!(expected(&[&a, &b, &c]).unwrap() > 1000);
        assert_eq!(described(&storage), (Some(3), expected(&[&a, &b, &c])));

        // Deleting an item takes its size off the total.
        let delete = serde_json::from_value(serde_json::json!({
            "TableName": "Sized",
            "Key": {"pk": {"S": "p"}, "sk": {"S": "b"}}
        }))
        .unwrap();
        pollster::block_on(delete_item::execute(&storage, delete)).unwrap();
        assert_eq!(described(&storage), (Some(2), expected(&[&a, &c])));

        // Replacing an item counts the new version, not both.
        let a2 = put(
            &storage,
            serde_json::json!({"pk": {"S": "p"}, "sk": {"S": "a"}, "data": {"S": "y".repeat(3000)}}),
        );
        assert_eq!(described(&storage), (Some(2), expected(&[&a2, &c])));

        // An update is counted at the item's size after the update.
        let update = serde_json::from_value(serde_json::json!({
            "TableName": "Sized",
            "Key": {"pk": {"S": "q"}, "sk": {"S": "c"}},
            "UpdateExpression": "SET extra = :v REMOVE ss",
            "ExpressionAttributeValues": {":v": {"S": "z".repeat(500)}}
        }))
        .unwrap();
        pollster::block_on(update_item::execute(&storage, update)).unwrap();
        let c2 = item(serde_json::json!({
            "pk": {"S": "q"}, "sk": {"S": "c"},
            "m": {"M": {"inner": {"L": [{"S": "one"}, {"BOOL": true}, {"NULL": true}]}}},
            "extra": {"S": "z".repeat(500)}
        }));
        let get = serde_json::from_value(serde_json::json!({
            "TableName": "Sized",
            "Key": {"pk": {"S": "q"}, "sk": {"S": "c"}}
        }))
        .unwrap();
        let stored = pollster::block_on(get_item::execute(&storage, get))
            .unwrap()
            .item
            .unwrap();
        assert_eq!(stored, c2, "the update produced the item the test expects");
        assert_eq!(described(&storage), (Some(2), expected(&[&a2, &c2])));
    }

    /// `ItemCount` and `IndexSizeBytes` for every secondary index, by name.
    fn index_figures(
        storage: &Storage,
    ) -> std::collections::BTreeMap<String, (Option<i64>, Option<i64>)> {
        let lifecycle = VectorIndexLifecycle::default();
        let request = describe_table::DescribeTableRequest {
            table_name: "Sized".to_string(),
        };
        let table = pollster::block_on(describe_table::execute(storage, request, &lifecycle))
            .unwrap()
            .table;
        let mut out = std::collections::BTreeMap::new();
        for gsi in table.global_secondary_indexes.unwrap_or_default() {
            out.insert(gsi.index_name, (gsi.item_count, gsi.index_size_bytes));
        }
        for lsi in table.local_secondary_indexes.unwrap_or_default() {
            out.insert(lsi.index_name, (lsi.item_count, lsi.index_size_bytes));
        }
        out
    }

    /// The table DynamoDB was asked about: an ALL and a KEYS_ONLY GSI on `g`
    /// and a KEYS_ONLY LSI on `l`, plus (when `include` is set) an INCLUDE
    /// GSI on `g` projecting only `n`.
    fn create_indexed_table(storage: &Storage, include: bool) {
        let mut gsis = vec![
            serde_json::json!({"IndexName": "gsiAll",
                "KeySchema": [{"AttributeName": "g", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"}}),
            serde_json::json!({"IndexName": "gsiKeys",
                "KeySchema": [{"AttributeName": "g", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "KEYS_ONLY"}}),
        ];
        if include {
            gsis.push(serde_json::json!({"IndexName": "gsiInc",
                "KeySchema": [{"AttributeName": "g", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "INCLUDE", "NonKeyAttributes": ["n"]}}));
        }
        let create = serde_json::from_value(serde_json::json!({
            "TableName": "Sized",
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"}
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "S"},
                {"AttributeName": "g", "AttributeType": "S"},
                {"AttributeName": "l", "AttributeType": "S"}
            ],
            "GlobalSecondaryIndexes": gsis,
            "LocalSecondaryIndexes": [{"IndexName": "lsiKeys",
                "KeySchema": [
                    {"AttributeName": "pk", "KeyType": "HASH"},
                    {"AttributeName": "l", "KeyType": "RANGE"}
                ],
                "Projection": {"ProjectionType": "KEYS_ONLY"}}],
            "BillingMode": "PAY_PER_REQUEST"
        }))
        .unwrap();
        pollster::block_on(create_table::execute(storage, create)).unwrap();
    }

    /// The four items written to the capture table: s1 and s2 carry both index
    /// keys, s3 and s4 neither.
    fn put_captured_items(storage: &Storage) {
        for (sk, len, keyed) in [
            ("s1", 1000, true),
            ("s2", 2000, true),
            ("s3", 3000, false),
            ("s4", 4000, false),
        ] {
            let n = &sk[1..];
            let mut value = serde_json::json!({
                "pk": {"S": "p"}, "sk": {"S": sk}, "data": {"S": "x".repeat(len)}
            });
            if keyed {
                value["g"] = serde_json::json!({"S": format!("g{n}")});
                value["l"] = serde_json::json!({"S": format!("l{n}")});
            }
            put(storage, value);
        }
    }

    /// What DynamoDB reported for the same table and items, read back in
    /// eu-west-2 on 9 October 2026 after its six-hourly refresh. Each index
    /// reads as the number of items carrying its keys and the plain sum of
    /// their projected sizes: the ALL GSI holds s1 and s2 whole (1,017 +
    /// 2,017), and a KEYS_ONLY entry is pk, sk and the index key at 10 bytes.
    #[test]
    fn index_figures_match_dynamodb_for_known_items() {
        let storage = Storage::memory().unwrap();
        create_indexed_table(&storage, false);
        put_captured_items(&storage);

        assert_eq!(described(&storage), (Some(4), Some(10056)));
        let figures = index_figures(&storage);
        assert_eq!(figures["gsiAll"], (Some(2), Some(3034)));
        assert_eq!(figures["gsiKeys"], (Some(2), Some(20)));
        assert_eq!(figures["lsiKeys"], (Some(2), Some(20)));
    }

    /// The figures follow every write that moves an item into, out of or
    /// within an index, and an INCLUDE projection counts only what it keeps.
    #[test]
    fn index_figures_follow_writes_and_projections() {
        let storage = Storage::memory().unwrap();
        create_indexed_table(&storage, true);
        put_captured_items(&storage);
        // s1 and s2 carry no `n`, so the INCLUDE index holds their keys only.
        assert_eq!(index_figures(&storage)["gsiInc"], (Some(2), Some(20)));

        // Deleting s1 takes it out of every index.
        let delete = serde_json::from_value(serde_json::json!({
            "TableName": "Sized",
            "Key": {"pk": {"S": "p"}, "sk": {"S": "s1"}}
        }))
        .unwrap();
        pollster::block_on(delete_item::execute(&storage, delete)).unwrap();
        let figures = index_figures(&storage);
        assert_eq!(figures["gsiAll"], (Some(1), Some(2017)));
        assert_eq!(figures["gsiKeys"], (Some(1), Some(10)));
        assert_eq!(figures["gsiInc"], (Some(1), Some(10)));
        assert_eq!(figures["lsiKeys"], (Some(1), Some(10)));

        // Overwriting s2 without `g` drops it from the GSIs; it keeps `l`, so
        // it stays in the LSI.
        put(
            &storage,
            serde_json::json!({"pk": {"S": "p"}, "sk": {"S": "s2"}, "l": {"S": "l2"}}),
        );
        let figures = index_figures(&storage);
        assert_eq!(figures["gsiAll"], (Some(0), Some(0)));
        assert_eq!(figures["gsiKeys"], (Some(0), Some(0)));
        assert_eq!(figures["gsiInc"], (Some(0), Some(0)));
        assert_eq!(figures["lsiKeys"], (Some(1), Some(10)));

        // Overwriting s3 with `g` and `n` brings it into the GSIs. The INCLUDE
        // entry is its keys plus `n`; the ALL entry is the whole item.
        let s3 = put(
            &storage,
            serde_json::json!({
                "pk": {"S": "p"}, "sk": {"S": "s3"}, "g": {"S": "g3"},
                "n": {"N": "12345"}, "data": {"S": "x".repeat(300)}
            }),
        );
        // pk (2 + 1) + sk (2 + 2) + g (1 + 2) = 10 for the keys; n is 1 + 4.
        let figures = index_figures(&storage);
        assert_eq!(figures["gsiAll"], (Some(1), Some(item_size(&s3) as i64)));
        assert_eq!(figures["gsiKeys"], (Some(1), Some(10)));
        assert_eq!(figures["gsiInc"], (Some(1), Some(15)));

        // An update that moves s3 within the GSIs (a longer key) and drops
        // `n` is sized at the item it leaves behind.
        let update = serde_json::from_value(serde_json::json!({
            "TableName": "Sized",
            "Key": {"pk": {"S": "p"}, "sk": {"S": "s3"}},
            "UpdateExpression": "SET g = :g REMOVE n",
            "ExpressionAttributeValues": {":g": {"S": "g333"}}
        }))
        .unwrap();
        pollster::block_on(update_item::execute(&storage, update)).unwrap();
        let s3_after = item(serde_json::json!({
            "pk": {"S": "p"}, "sk": {"S": "s3"}, "g": {"S": "g333"},
            "data": {"S": "x".repeat(300)}
        }));
        let figures = index_figures(&storage);
        assert_eq!(
            figures["gsiAll"],
            (Some(1), Some(item_size(&s3_after) as i64))
        );
        assert_eq!(figures["gsiKeys"], (Some(1), Some(12)));
        assert_eq!(figures["gsiInc"], (Some(1), Some(12)));
        assert_eq!(figures["lsiKeys"], (Some(1), Some(10)));
    }
}
