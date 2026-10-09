use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

const P: usize = ttl::SWEEP_PAGE_SIZE;
const PAST: &str = "1000";
const FUTURE: &str = "9999999999";

fn request<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
    serde_json::from_value(value).unwrap()
}

/// `swept` has TTL on `ttl` and a GSI on `g`; `other` has neither.
fn fixture() -> Database {
    let db = Database::memory().unwrap();
    db.create_table(request(json!({
        "TableName": "swept",
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [
            {"AttributeName": "pk", "AttributeType": "S"},
            {"AttributeName": "g", "AttributeType": "S"}
        ],
        "GlobalSecondaryIndexes": [{
            "IndexName": "byG",
            "KeySchema": [{"AttributeName": "g", "KeyType": "HASH"}],
            "Projection": {"ProjectionType": "ALL"}
        }],
        "BillingMode": "PAY_PER_REQUEST"
    })))
    .unwrap();
    db.create_table(request(json!({
        "TableName": "other",
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        "BillingMode": "PAY_PER_REQUEST"
    })))
    .unwrap();
    db.update_time_to_live(request(json!({
        "TableName": "swept",
        "TimeToLiveSpecification": {"Enabled": true, "AttributeName": "ttl"}
    })))
    .unwrap();
    db.put_item(request(
        json!({"TableName": "other", "Item": {"pk": {"S": "kept"}}}),
    ))
    .unwrap();
    db
}

fn put(db: &Database, pk: &str, ttl: &str) {
    db.put_item(request(json!({
        "TableName": "swept",
        "Item": {"pk": {"S": pk}, "g": {"S": format!("g-{pk}")}, "ttl": {"N": ttl}}
    })))
    .unwrap();
}

fn put_expired(db: &Database, count: usize) {
    for i in 0..count {
        put(db, &format!("item{i:04}"), PAST);
    }
}

/// Every item in `swept`, or in its GSI, as JSON sorted by key.
fn contents(db: &Database, index: Option<&str>) -> Vec<String> {
    let mut scan = json!({"TableName": "swept"});
    if let Some(index) = index {
        scan["IndexName"] = json!(index);
    }
    let resp: actions::scan::ScanResponse = db.scan(request(scan)).unwrap();
    assert!(
        resp.last_evaluated_key.is_none(),
        "fixture outgrew one scan"
    );
    let mut items: Vec<String> = resp
        .items
        .unwrap_or_default()
        .iter()
        .map(|item| {
            let sorted: std::collections::BTreeMap<_, _> = item.iter().collect();
            serde_json::to_string(&sorted).unwrap()
        })
        .collect();
    items.sort();
    items
}

#[test]
fn a_request_waiting_when_a_page_ends_runs_before_the_next_page() {
    let db = fixture();
    put_expired(&db, 6 * P);
    let ran = Arc::new(AtomicUsize::new(0));
    let mut queued = 0;
    let mut waiters = Vec::new();
    let deleted = db
        .sweep_ttl_by_page(
            |page| {
                assert_eq!(
                    ran.load(Ordering::SeqCst),
                    queued,
                    "page {page} started before a request waiting for the lock had run"
                );
                // Queue a request behind this page, which holds the lock.
                let other = db.clone();
                let ran = Arc::clone(&ran);
                waiters.push(std::thread::spawn(move || {
                    other
                        .with_storage(|_| {
                            ran.fetch_add(1, Ordering::SeqCst);
                            Ok(())
                        })
                        .unwrap();
                }));
                while db.inner.waiting() == 0 {
                    std::thread::yield_now();
                }
                queued += 1;
            },
            |_| {},
        )
        .unwrap();
    assert_eq!(deleted, 6 * P);
    for waiter in waiters {
        waiter.join().unwrap();
    }
    assert_eq!(ran.load(Ordering::SeqCst), queued);
}

#[test]
fn sweeping_by_page_leaves_what_a_single_call_sweep_leaves() {
    // Live and expired items interleaved by key, across several pages.
    let build = || {
        let db = fixture();
        for i in 0..4 * P + 7 {
            let ttl = if i % 3 == 0 { FUTURE } else { PAST };
            put(&db, &format!("item{i:04}"), ttl);
        }
        db
    };
    let paged = build();
    let single = build();

    let paged_deleted = paged.sweep_ttl().unwrap();
    let single_deleted = single
        .with_storage(|s| pollster::block_on(ttl::sweep_expired_items(s)))
        .unwrap();

    assert_eq!(paged_deleted, single_deleted);
    assert_eq!(contents(&paged, None), contents(&single, None));
    assert_eq!(
        contents(&paged, Some("byG")),
        contents(&single, Some("byG"))
    );
    // Only the live third remain, in the table and in its index.
    assert_eq!(contents(&paged, None).len(), (4 * P + 7).div_ceil(3));
    assert_eq!(contents(&paged, Some("byG")).len(), (4 * P + 7).div_ceil(3));
}

#[test]
fn a_table_that_keeps_growing_does_not_keep_the_sweep_running() {
    let db = fixture();
    put_expired(&db, 3 * P);
    let mut gaps = 0;
    let deleted = db
        .sweep_ttl_by_page(
            |_| {},
            |_| {
                gaps += 1;
                // Bounds the failure: an unbounded sweep would go on forever.
                assert!(
                    gaps < 20,
                    "the sweep kept reading rows written after it began"
                );
                // A page of expired rows, all ahead of the sweep.
                for i in 0..P {
                    put(&db, &format!("zz{gaps:02}-{i:04}"), PAST);
                }
            },
        )
        .unwrap();

    assert_eq!(deleted, 3 * P);
    assert_eq!(contents(&db, None).len(), gaps * P);
    // The next sweep takes what this one left.
    assert_eq!(db.sweep_ttl().unwrap(), gaps * P);
}

#[test]
fn two_sweeps_at_once_delete_each_expired_item_once() {
    // An embedder's timer and the MCP tool can sweep the same database at the
    // same time, each handing the lock to the other between its pages.
    let db = fixture();
    put_expired(&db, 8 * P);
    for i in 0..10 {
        put(&db, &format!("live{i:02}"), FUTURE);
    }

    let other = db.clone();
    let background = std::thread::spawn(move || other.sweep_ttl());
    let here = db.sweep_ttl().unwrap();
    let there = background.join().unwrap().unwrap();

    assert_eq!(here + there, 8 * P);
    assert_eq!(contents(&db, None).len(), 10);
    assert_eq!(contents(&db, Some("byG")).len(), 10);
}

#[test]
fn a_sweep_that_fits_in_one_page_takes_the_lock_once() {
    let db = fixture();
    put_expired(&db, P - 1);
    let mut pages = 0;
    let mut gaps = 0;
    let deleted = db.sweep_ttl_by_page(|_| pages += 1, |_| gaps += 1).unwrap();
    assert_eq!(deleted, P - 1);
    assert_eq!((pages, gaps), (1, 0));
}
