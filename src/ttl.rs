//! TTL (Time to Live) support.
//!
//! Provides background expiry of items with expired TTL attributes.
//!
//! A sweep reads each TTL-enabled table a page at a time, carrying its
//! position between pages, so a caller that owns a lock around the storage
//! can release it between pages and let other requests run.
//! [`sweep_expired_items`] runs every page in one call under whatever lock its
//! caller holds.

use std::collections::VecDeque;

use crate::actions::helpers::KeySchema;
use crate::actions::{gsi, lsi, vector_index};
use crate::errors::Result;
use crate::storage::{ScanParams, TableMetadata};
use crate::storage_backend::StorageBackend;
use crate::streams;
use crate::types::{AttributeDefinition, AttributeValue, Item, VectorIndex};

/// JSON representation of the TTL service identity for stream records.
const TTL_USER_IDENTITY: &str = r#"{"type":"Service","principalId":"dynamodb.amazonaws.com"}"#;

/// Rows read per page of a sweep.
///
/// A page is the unit of work done under one hold of the caller's lock, and
/// every expired row in it costs a write transaction of its own, so the size
/// bounds how long other requests can wait behind a sweep.
pub(crate) const SWEEP_PAGE_SIZE: usize = 25;

/// Sweep all TTL-enabled tables and delete expired items.
///
/// Runs the whole sweep in one call, so it holds whatever lock the caller has
/// taken around `storage` until it returns. `Database::sweep_ttl` drives the
/// same sweep a page at a time instead.
///
/// Returns the total number of items deleted across all tables.
pub async fn sweep_expired_items<S: StorageBackend>(storage: &S) -> Result<usize> {
    let mut sweep = Sweep::begin(storage).await?;
    while sweep.next_page(storage).await? {}
    Ok(sweep.deleted())
}

/// A table the sweep set out to cover, as it stood when the sweep began.
struct SweptTable {
    name: String,
    /// Tells the table the sweep began on from one deleted and recreated
    /// under the same name since.
    table_id: Option<String>,
}

/// A table's key schema and index definitions, parsed once and kept with the
/// JSON they came from.
///
/// A sweep is the case where many items expire at once, because a shared TTL
/// is how they got there, so the definitions are parsed once per table rather
/// than once per item or per page. Keeping the source lets a page see that an
/// `UpdateTable` changed them since the last page, and reparse.
struct TableDefs {
    key_schema_json: String,
    attribute_definitions_json: String,
    gsi_json: Option<String>,
    lsi_json: Option<String>,
    vector_json: Option<String>,
    key_schema: KeySchema,
    gsi_defs: Vec<gsi::GsiDef>,
    lsi_defs: Vec<lsi::LsiDef>,
    vector_defs: Vec<VectorIndex>,
    attr_defs: Vec<AttributeDefinition>,
}

impl TableDefs {
    fn parse(meta: &TableMetadata) -> Result<Self> {
        Ok(Self {
            key_schema_json: meta.key_schema.clone(),
            attribute_definitions_json: meta.attribute_definitions.clone(),
            gsi_json: meta.gsi_definitions.clone(),
            lsi_json: meta.lsi_definitions.clone(),
            vector_json: meta.vector_index_definitions.clone(),
            key_schema: crate::actions::helpers::parse_key_schema(meta)?,
            gsi_defs: gsi::parse_gsi_defs(meta)?,
            lsi_defs: lsi::parse_lsi_defs(meta)?,
            vector_defs: vector_index::parse_vector_defs(meta)?,
            attr_defs: vector_index::parse_attr_defs(meta)?,
        })
    }

    fn parsed_from(&self, meta: &TableMetadata) -> bool {
        self.key_schema_json == meta.key_schema
            && self.attribute_definitions_json == meta.attribute_definitions
            && self.gsi_json == meta.gsi_definitions
            && self.lsi_json == meta.lsi_definitions
            && self.vector_json == meta.vector_index_definitions
    }
}

/// A sweep in progress: where it has got to, and what it has deleted.
///
/// Each [`next_page`](Self::next_page) re-reads the current table's metadata,
/// so a table deleted, recreated or with TTL disabled since the last page ends
/// that table's part of the sweep, and a changed index set is picked up before
/// the next delete. A page reads its rows and deletes the expired ones in the
/// same call, so an item is only ever deleted as it stands at the delete.
pub(crate) struct Sweep {
    /// Expiry is judged against the time the sweep began. An item that
    /// expires mid-sweep belongs to the next one.
    now: u64,
    /// Tables still to finish, the current one first. Fixed when the sweep
    /// begins: a table that gains TTL mid-sweep belongs to the next one.
    tables: VecDeque<SweptTable>,
    /// The key of the last row the current table's previous page read.
    after: Option<(String, String)>,
    /// Rows the current table's part of the sweep may still read: the
    /// table's row count when the sweep reached it, less what it has read
    /// since. Without it, rows written ahead of the sweep between pages
    /// could keep it running indefinitely; they are left for the next sweep.
    remaining: Option<usize>,
    defs: Option<TableDefs>,
    page_size: usize,
    deleted: usize,
}

impl Sweep {
    /// Start a sweep: read the clock and list the TTL-enabled tables.
    pub(crate) async fn begin<S: StorageBackend>(storage: &S) -> Result<Self> {
        let now = storage.clock().now_unix_secs();
        let tables = storage
            .list_ttl_enabled_tables()
            .await?
            .into_iter()
            .map(|meta| SweptTable {
                name: meta.table_name,
                table_id: meta.table_id,
            })
            .collect();
        Ok(Self {
            now,
            tables,
            after: None,
            remaining: None,
            defs: None,
            page_size: SWEEP_PAGE_SIZE,
            deleted: 0,
        })
    }

    /// Items deleted so far.
    pub(crate) fn deleted(&self) -> usize {
        self.deleted
    }

    /// Read the next page of the current table and delete its expired items.
    ///
    /// Returns whether the sweep has more to do.
    pub(crate) async fn next_page<S: StorageBackend>(&mut self, storage: &S) -> Result<bool> {
        while let Some(table) = self.tables.front() {
            let meta = match storage.get_table_metadata(&table.name).await? {
                Some(meta) if meta.ttl_enabled && meta.table_id == table.table_id => meta,
                _ => {
                    self.finish_table();
                    continue;
                }
            };
            let Some(ttl_attr) = meta.ttl_attribute.as_deref() else {
                self.finish_table();
                continue;
            };

            let remaining = match self.remaining {
                Some(remaining) => remaining,
                None => usize::try_from(storage.count_items(&meta.table_name).await?).unwrap_or(0),
            };
            let limit = self.page_size.min(remaining);
            if limit == 0 {
                self.finish_table();
                continue;
            }

            let defs = match self.defs.take() {
                Some(defs) if defs.parsed_from(&meta) => defs,
                _ => TableDefs::parse(&meta)?,
            };

            let rows = storage
                .scan_items(
                    &meta.table_name,
                    &ScanParams {
                        limit: Some(limit),
                        exclusive_start_pk: self.after.as_ref().map(|(pk, _)| pk.as_str()),
                        exclusive_start_sk: self.after.as_ref().map(|(_, sk)| sk.as_str()),
                        ..Default::default()
                    },
                )
                .await?;

            for (pk, sk, item_json) in &rows {
                let item: Item = match serde_json::from_str(item_json) {
                    Ok(i) => i,
                    Err(_) => continue,
                };
                if is_expired(&item, ttl_attr, self.now) {
                    delete_expired_item(storage, &meta, &defs, pk, sk, &item).await?;
                    self.deleted += 1;
                }
            }

            self.defs = Some(defs);
            self.remaining = Some(remaining - rows.len());
            if rows.len() < limit || rows.len() == remaining {
                self.finish_table();
            } else if let Some((pk, sk, _)) = rows.into_iter().last() {
                self.after = Some((pk, sk));
            }
            return Ok(!self.tables.is_empty());
        }
        Ok(false)
    }

    fn finish_table(&mut self) {
        self.tables.pop_front();
        self.after = None;
        self.remaining = None;
        self.defs = None;
    }
}

/// Delete one expired item with its index fan-out and stream record.
///
/// Each TTL deletion is atomic with its own index fan-out: a mid-fan-out
/// failure rolls that item's delete back rather than leaving a torn index.
/// Items are independent, so this is one transaction per deleted item.
async fn delete_expired_item<S: StorageBackend>(
    storage: &S,
    meta: &TableMetadata,
    defs: &TableDefs,
    pk: &str,
    sk: &str,
    item: &Item,
) -> Result<()> {
    crate::actions::helpers::with_write_transaction(storage, async {
        storage.delete_item(&meta.table_name, pk, sk).await?;
        let target = gsi::IndexWrite {
            table_name: &meta.table_name,
            pk,
            sk,
            pk_attr: &defs.key_schema.partition_key,
            sk_attr: defs.key_schema.sort_key.as_deref(),
            old_item: Some(item),
            capacity_mode: None,
        };
        // A TTL deletion has no caller to report capacity to, so it asks for
        // none and the fan-out skips sizing the indexes rather than sizing
        // them for a discarded map.
        let _ = gsi::maintain_gsis_after_delete_with_defs(storage, &defs.gsi_defs, &target).await?;
        let _ = lsi::maintain_lsis_after_delete_with_defs(storage, &defs.lsi_defs, &target).await?;
        let _ = vector_index::maintain_vector_indexes_after_delete_with_defs(
            storage,
            &defs.vector_defs,
            &defs.attr_defs,
            &target,
        )
        .await?;
        // Generate stream REMOVE record with TTL service identity
        if meta.stream_enabled {
            record_ttl_stream_event(storage, meta, &defs.key_schema, item).await?;
        }
        Ok(())
    })
    .await
}

/// Check if an item's TTL attribute indicates it has expired.
///
/// Returns false if:
/// - The TTL attribute doesn't exist on the item
/// - The TTL attribute is not a Number type
/// - The TTL value is >= current epoch seconds (not yet expired)
fn is_expired(item: &Item, ttl_attr: &str, now_epoch_secs: u64) -> bool {
    match item.get(ttl_attr) {
        Some(AttributeValue::N(n)) => {
            // Parse as i64 first to handle potential negative values, then compare
            match n.parse::<i64>() {
                Ok(ttl_val) if ttl_val >= 0 => (ttl_val as u64) < now_epoch_secs,
                _ => false,
            }
        }
        _ => false,
    }
}

/// Record a stream REMOVE event for a TTL deletion, with the DynamoDB service
/// user identity to distinguish from manual deletes.
async fn record_ttl_stream_event<S: StorageBackend>(
    storage: &S,
    meta: &TableMetadata,
    key_schema: &KeySchema,
    old_item: &Item,
) -> Result<()> {
    let view_type = meta
        .stream_view_type
        .as_deref()
        .unwrap_or("NEW_AND_OLD_IMAGES");

    let keys = streams::extract_keys_with_schema(old_item, key_schema);
    let keys_json = serde_json::to_string(&keys).unwrap_or_default();

    let old_image_json = match view_type {
        "OLD_IMAGE" | "NEW_AND_OLD_IMAGES" => {
            Some(serde_json::to_string(old_item).unwrap_or_default())
        }
        _ => None,
    };

    let seq_num = storage
        .next_stream_sequence_number(&meta.table_name)
        .await?;
    let sid = streams::shard_id(&meta.table_name);
    let now = storage.clock().now_unix_secs() as i64;

    storage
        .insert_stream_record_with_identity(
            &meta.table_name,
            "REMOVE",
            &keys_json,
            None,
            old_image_json.as_deref(),
            &seq_num.to_string(),
            &sid,
            now,
            Some(TTL_USER_IDENTITY),
        )
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_expired_with_past_timestamp() {
        let mut item = Item::new();
        item.insert("ttl".to_string(), AttributeValue::N("1000".to_string()));
        assert!(is_expired(&item, "ttl", 2000));
    }

    #[test]
    fn test_is_expired_with_future_timestamp() {
        let mut item = Item::new();
        item.insert("ttl".to_string(), AttributeValue::N("3000".to_string()));
        assert!(!is_expired(&item, "ttl", 2000));
    }

    #[test]
    fn test_is_expired_with_equal_timestamp() {
        let mut item = Item::new();
        item.insert("ttl".to_string(), AttributeValue::N("2000".to_string()));
        // Equal means NOT expired (must be strictly less than)
        assert!(!is_expired(&item, "ttl", 2000));
    }

    #[test]
    fn test_is_expired_missing_attribute() {
        let item = Item::new();
        assert!(!is_expired(&item, "ttl", 2000));
    }

    #[test]
    fn test_is_expired_non_numeric_attribute() {
        let mut item = Item::new();
        item.insert(
            "ttl".to_string(),
            AttributeValue::S("not-a-number".to_string()),
        );
        assert!(!is_expired(&item, "ttl", 2000));
    }

    #[test]
    fn test_is_expired_negative_value() {
        let mut item = Item::new();
        item.insert("ttl".to_string(), AttributeValue::N("-100".to_string()));
        assert!(!is_expired(&item, "ttl", 2000));
    }
}

/// A sweep driven a page at a time, with other requests landing between
/// pages, as `Database::sweep_ttl` lets them.
#[cfg(all(test, any(feature = "native-sqlite", feature = "_has-encryption")))]
mod sweep_tests {
    use super::*;
    use crate::Database;
    use serde_json::json;

    const P: usize = SWEEP_PAGE_SIZE;
    const PAST: i64 = 1000;
    const FUTURE: i64 = 9_999_999_999;

    fn request<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
        serde_json::from_value(value).unwrap()
    }

    fn key(i: usize) -> String {
        format!("item{i:04}")
    }

    /// A hash-keyed table with TTL on `ttl`, optionally with a GSI on `g`.
    fn ttl_table(db: &Database, name: &str, with_gsi: bool) {
        let mut create = json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST"
        });
        if with_gsi {
            create["AttributeDefinitions"] = json!([
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "g", "AttributeType": "S"}
            ]);
            create["GlobalSecondaryIndexes"] = json!([{
                "IndexName": "byG",
                "KeySchema": [{"AttributeName": "g", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"}
            }]);
        }
        db.create_table(request(create)).unwrap();
        set_ttl(db, name, true);
    }

    fn set_ttl(db: &Database, name: &str, enabled: bool) {
        db.update_time_to_live(request(json!({
            "TableName": name,
            "TimeToLiveSpecification": {"Enabled": enabled, "AttributeName": "ttl"}
        })))
        .unwrap();
    }

    fn put(db: &Database, table: &str, pk: &str, ttl: i64) {
        db.put_item(request(json!({
            "TableName": table,
            "Item": {"pk": {"S": pk}, "g": {"S": "same"}, "ttl": {"N": ttl.to_string()}}
        })))
        .unwrap();
    }

    fn fill(db: &Database, table: &str, items: std::ops::Range<usize>) {
        for i in items {
            put(db, table, &key(i), PAST);
        }
    }

    fn exists(db: &Database, table: &str, pk: &str) -> bool {
        db.get_item(request(
            json!({"TableName": table, "Key": {"pk": {"S": pk}}}),
        ))
        .unwrap()
        .item
        .is_some()
    }

    fn count(db: &Database, table: &str, index: Option<&str>) -> usize {
        let mut scan = json!({"TableName": table});
        if let Some(index) = index {
            scan["IndexName"] = json!(index);
        }
        db.scan(request(scan)).unwrap().count
    }

    fn delete_table(db: &Database, table: &str) {
        db.delete_table(request(json!({"TableName": table})))
            .unwrap();
    }

    fn begin(db: &Database) -> Sweep {
        db.with_storage(|s| pollster::block_on(Sweep::begin(s)))
            .unwrap()
    }

    fn page(db: &Database, sweep: &mut Sweep) -> bool {
        db.with_storage(|s| pollster::block_on(sweep.next_page(s)))
            .unwrap()
    }

    fn finish(db: &Database, mut sweep: Sweep) -> usize {
        while page(db, &mut sweep) {}
        sweep.deleted()
    }

    #[test]
    fn an_item_rewritten_with_a_future_ttl_between_pages_survives() {
        let db = Database::memory().unwrap();
        ttl_table(&db, "tbl", false);
        fill(&db, "tbl", 0..3 * P);

        let mut sweep = begin(&db);
        assert!(page(&db, &mut sweep));
        put(&db, "tbl", &key(2 * P + 5), FUTURE);

        assert_eq!(finish(&db, sweep), 3 * P - 1);
        assert!(exists(&db, "tbl", &key(2 * P + 5)));
        assert_eq!(count(&db, "tbl", None), 1);
    }

    #[test]
    fn an_item_deleted_between_pages_is_not_counted() {
        let db = Database::memory().unwrap();
        ttl_table(&db, "tbl", false);
        fill(&db, "tbl", 0..3 * P);

        let mut sweep = begin(&db);
        assert!(page(&db, &mut sweep));
        db.delete_item(request(
            json!({"TableName": "tbl", "Key": {"pk": {"S": key(2 * P)}}}),
        ))
        .unwrap();

        assert_eq!(finish(&db, sweep), 3 * P - 1);
        assert_eq!(count(&db, "tbl", None), 0);
    }

    #[test]
    fn items_written_between_pages_are_left_for_the_next_sweep() {
        let db = Database::memory().unwrap();
        ttl_table(&db, "tbl", false);
        fill(&db, "tbl", 0..3 * P);

        let mut sweep = begin(&db);
        assert!(page(&db, &mut sweep));
        // Behind the sweep's position: the first page has already passed it.
        put(&db, "tbl", "item0003x", PAST);
        // Ahead of it, but past the rows the table held when the sweep reached
        // it.
        put(&db, "tbl", "item9999", PAST);

        assert_eq!(finish(&db, sweep), 3 * P);
        assert!(exists(&db, "tbl", "item0003x"));
        assert!(exists(&db, "tbl", "item9999"));
        assert_eq!(db.sweep_ttl().unwrap(), 2);
    }

    #[test]
    fn a_row_written_ahead_of_the_sweep_can_displace_the_last_row_to_the_next_sweep() {
        // A table's part of the sweep reads as many rows as the table held
        // when the sweep reached it. A row written ahead of the sweep, inside
        // that range, is read in place of the last one, which waits.
        let db = Database::memory().unwrap();
        ttl_table(&db, "tbl", false);
        fill(&db, "tbl", 0..3 * P);

        let mut sweep = begin(&db);
        assert!(page(&db, &mut sweep));
        put(&db, "tbl", &format!("{}x", key(P + 5)), PAST);

        assert_eq!(finish(&db, sweep), 3 * P);
        assert!(!exists(&db, "tbl", &format!("{}x", key(P + 5))));
        assert!(exists(&db, "tbl", &key(3 * P - 1)));
        assert_eq!(db.sweep_ttl().unwrap(), 1);
    }

    #[test]
    fn a_table_deleted_mid_sweep_ends_without_error_and_the_rest_are_swept() {
        let db = Database::memory().unwrap();
        ttl_table(&db, "tbl_a", false);
        ttl_table(&db, "tbl_b", false);
        fill(&db, "tbl_a", 0..3 * P);
        fill(&db, "tbl_b", 0..5);

        let mut sweep = begin(&db);
        assert!(page(&db, &mut sweep));
        delete_table(&db, "tbl_a");

        assert_eq!(finish(&db, sweep), P + 5);
        assert_eq!(count(&db, "tbl_b", None), 0);
    }

    #[test]
    fn a_table_recreated_mid_sweep_is_left_to_the_next_sweep() {
        let db = Database::memory().unwrap();
        ttl_table(&db, "tbl_a", false);
        fill(&db, "tbl_a", 0..3 * P);

        let mut sweep = begin(&db);
        assert!(page(&db, &mut sweep));
        delete_table(&db, "tbl_a");
        ttl_table(&db, "tbl_a", false);
        // Ahead of where the sweep had got to in the old table.
        fill(&db, "tbl_a", 2 * P..2 * P + 5);

        assert_eq!(finish(&db, sweep), P);
        assert_eq!(count(&db, "tbl_a", None), 5);
    }

    #[test]
    fn a_later_table_recreated_before_the_sweep_reaches_it_is_left_alone() {
        let db = Database::memory().unwrap();
        ttl_table(&db, "tbl_a", false);
        ttl_table(&db, "tbl_b", false);
        fill(&db, "tbl_a", 0..3 * P);
        fill(&db, "tbl_b", 0..5);

        let mut sweep = begin(&db);
        assert!(page(&db, &mut sweep));
        delete_table(&db, "tbl_b");
        ttl_table(&db, "tbl_b", false);
        fill(&db, "tbl_b", 0..5);

        assert_eq!(finish(&db, sweep), 3 * P);
        assert_eq!(count(&db, "tbl_b", None), 5);
    }

    #[test]
    fn disabling_ttl_mid_sweep_stops_that_table() {
        let db = Database::memory().unwrap();
        ttl_table(&db, "tbl", false);
        fill(&db, "tbl", 0..3 * P);

        let mut sweep = begin(&db);
        assert!(page(&db, &mut sweep));
        set_ttl(&db, "tbl", false);

        assert_eq!(finish(&db, sweep), P);
        assert_eq!(count(&db, "tbl", None), 2 * P);
    }

    #[test]
    fn an_index_added_mid_sweep_is_kept_in_step_with_the_deletes() {
        let db = Database::memory().unwrap();
        ttl_table(&db, "tbl", false);
        fill(&db, "tbl", 0..3 * P);

        let mut sweep = begin(&db);
        assert!(page(&db, &mut sweep));
        db.update_table(request(json!({
            "TableName": "tbl",
            "AttributeDefinitions": [{"AttributeName": "g", "AttributeType": "S"}],
            "GlobalSecondaryIndexUpdates": [{"Create": {
                "IndexName": "byG",
                "KeySchema": [{"AttributeName": "g", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"}
            }}]
        })))
        .unwrap();
        assert_eq!(count(&db, "tbl", Some("byG")), 2 * P);

        assert_eq!(finish(&db, sweep), 3 * P);
        assert_eq!(count(&db, "tbl", None), 0);
        assert_eq!(count(&db, "tbl", Some("byG")), 0);
    }

    #[test]
    fn an_index_dropped_mid_sweep_does_not_fail_the_sweep() {
        let db = Database::memory().unwrap();
        ttl_table(&db, "tbl", true);
        fill(&db, "tbl", 0..3 * P);

        let mut sweep = begin(&db);
        assert!(page(&db, &mut sweep));
        db.update_table(request(json!({
            "TableName": "tbl",
            "GlobalSecondaryIndexUpdates": [{"Delete": {"IndexName": "byG"}}]
        })))
        .unwrap();

        assert_eq!(finish(&db, sweep), 3 * P);
        assert_eq!(count(&db, "tbl", None), 0);
    }

    #[test]
    fn a_sweep_with_nothing_to_do_reports_no_more_work() {
        let db = Database::memory().unwrap();
        let mut sweep = begin(&db);
        assert!(!page(&db, &mut sweep));
        assert_eq!(sweep.deleted(), 0);
    }
}
