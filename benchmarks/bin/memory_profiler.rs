use dynoxide::Database;
use dynoxide::actions::batch_write_item::{BatchWriteItemRequest, PutRequest, WriteRequest};
use dynoxide::actions::delete_item::DeleteItemRequest;
use dynoxide::actions::query::QueryRequest;
use dynoxide::actions::scan::ScanRequest;
use dynoxide::actions::update_item::UpdateItemRequest;
use dynoxide::types::AttributeValue;
use dynoxide_benchmarks::containers::{
    self, ContainerState, ContainerTarget, container_state, docker_available, image_available,
    remove_container, report_not_ready, report_stopped, wait_for_list_tables,
};
use dynoxide_benchmarks::memory_measurement::{current_rss_bytes, disk_usage_bytes, format_bytes};
use dynoxide_benchmarks::stats;
use dynoxide_benchmarks::{BENCH_TABLE, ItemSize, create_table_request, generate_items, make_key};
use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// How long a container gets to answer its first ListTables.
const CONTAINER_READY_TIMEOUT: Duration = Duration::from_secs(300);

struct CsvWriter {
    rows: Vec<String>,
    mode: &'static str,
}

impl CsvWriter {
    fn new(mode: &'static str) -> Self {
        Self {
            rows: Vec::new(),
            mode,
        }
    }

    fn record(
        &mut self,
        step: &str,
        items_in_table: usize,
        start: Instant,
        disk_bytes: Option<u64>,
    ) {
        let rss = current_rss_bytes();
        let elapsed_ms = start.elapsed().as_millis();
        let disk = disk_bytes.unwrap_or(0);
        self.rows.push(format!(
            "{},{},{},{},{},{}",
            self.mode, step, rss, items_in_table, elapsed_ms, disk
        ));

        eprintln!(
            "  [{:>12}] {:<30} RSS: {:>10}  items: {:>6}  elapsed: {:>6}ms{}",
            self.mode,
            step,
            format_bytes(rss),
            items_in_table,
            elapsed_ms,
            if let Some(d) = disk_bytes {
                format!("  disk: {}", format_bytes(d))
            } else {
                String::new()
            }
        );
    }
}

fn main() {
    eprintln!("=== Dynoxide Memory Profiler ===\n");

    // Run in-memory mode
    let mut csv = CsvWriter::new("memory");
    run_workload_memory(&mut csv);

    eprintln!();

    // Run file-backed mode
    let mut csv_file = CsvWriter::new("file");
    run_workload_file(&mut csv_file);

    // Docker container idle RSS
    // Methodology: 30s settle after the first successful ListTables,
    // then 3 samples taken 10s apart. Reports mean and range.
    let mut docker_rows: Vec<String> = Vec::new();
    let mut failures: Vec<(&str, String)> = Vec::new();
    if docker_available() {
        eprintln!("\n--- Docker Container Idle RSS (30s settle, 3 samples @ 10s intervals) ---");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        for target in containers::ALL {
            if !image_available(target.image) {
                eprintln!(
                    "  {}: image {} not pulled -- skipping (docker pull it to include it)",
                    target.label, target.image
                );
                continue;
            }
            eprintln!("  {}:", target.label);
            match measure_container_idle_rss(&runtime, target) {
                Ok(samples) => {
                    let mean = stats::mean(&samples) as u64;
                    let min = *samples.iter().min_by(|a, b| a.total_cmp(b)).unwrap() as u64;
                    let max = *samples.iter().max_by(|a, b| a.total_cmp(b)).unwrap() as u64;
                    eprintln!(
                        "  {} idle RSS: {} (range: {} to {})",
                        target.label,
                        format_bytes(mean),
                        format_bytes(min),
                        format_bytes(max)
                    );
                    docker_rows.push(format!("docker_idle,{},{mean},0,0,0", target.key));
                }
                Err(reason) => {
                    eprintln!("  FAILED: {reason}");
                    failures.push((target.key, reason));
                }
            }
        }
    } else {
        eprintln!("\nDocker not available -- skipping container idle RSS");
    }

    // Write CSV to stdout (human-readable progress goes to stderr)
    println!("mode,step,rss_bytes,items_in_table,elapsed_ms,disk_bytes");
    for row in csv
        .rows
        .iter()
        .chain(csv_file.rows.iter())
        .chain(docker_rows.iter())
    {
        println!("{row}");
    }

    // A target that was attempted and never answered is a failed run, not a
    // shorter CSV: a README refresh would otherwise keep that target's old
    // number beside a new commit link.
    if !failures.is_empty() {
        eprintln!("\nFAILED: no idle RSS for:");
        for (target, reason) in &failures {
            eprintln!("  {target}: {reason}");
        }
        std::process::exit(1);
    }
}

fn run_workload_memory(csv: &mut CsvWriter) {
    eprintln!("--- In-Memory Mode ---");
    let start = Instant::now();

    let db = Database::memory().unwrap();
    csv.record("db_created", 0, start, None);

    db.create_table(create_table_request(BENCH_TABLE)).unwrap();
    csv.record("table_created", 0, start, None);

    // Load 1,000 items
    load_items(&db, 1_000, ItemSize::Medium);
    csv.record("loaded_1k", 1_000, start, None);

    // Load to 10,000 items
    load_items_offset(&db, 1_000, 10_000, ItemSize::Medium);
    csv.record("loaded_10k", 10_000, start, None);

    // Run queries
    run_queries(&db, 100);
    csv.record("after_queries", 10_000, start, None);

    // Run scans
    run_scan(&db);
    csv.record("after_scan", 10_000, start, None);

    // Run updates
    run_updates(&db, 500);
    csv.record("after_updates", 10_000, start, None);

    // Delete 5,000 items
    delete_items(&db, 5_000);
    csv.record("after_deletes", 5_000, start, None);

    csv.record("final", 5_000, start, None);
}

fn run_workload_file(csv: &mut CsvWriter) {
    eprintln!("--- File-Backed Mode ---");
    let start = Instant::now();

    let tmpdir = TempDir::new().unwrap();
    let db_path = tmpdir.path().join("bench.db");
    let db = Database::new(db_path.to_str().unwrap()).unwrap();
    csv.record(
        "db_created",
        0,
        start,
        Some(disk_usage_bytes(tmpdir.path())),
    );

    db.create_table(create_table_request(BENCH_TABLE)).unwrap();
    csv.record(
        "table_created",
        0,
        start,
        Some(disk_usage_bytes(tmpdir.path())),
    );

    // Load 1,000 items
    load_items(&db, 1_000, ItemSize::Medium);
    csv.record(
        "loaded_1k",
        1_000,
        start,
        Some(disk_usage_bytes(tmpdir.path())),
    );

    // Load to 10,000 items
    load_items_offset(&db, 1_000, 10_000, ItemSize::Medium);
    csv.record(
        "loaded_10k",
        10_000,
        start,
        Some(disk_usage_bytes(tmpdir.path())),
    );

    // Run queries
    run_queries(&db, 100);
    csv.record(
        "after_queries",
        10_000,
        start,
        Some(disk_usage_bytes(tmpdir.path())),
    );

    // Run scans
    run_scan(&db);
    csv.record(
        "after_scan",
        10_000,
        start,
        Some(disk_usage_bytes(tmpdir.path())),
    );

    // Run updates
    run_updates(&db, 500);
    csv.record(
        "after_updates",
        10_000,
        start,
        Some(disk_usage_bytes(tmpdir.path())),
    );

    // Delete 5,000 items
    delete_items(&db, 5_000);
    csv.record(
        "after_deletes",
        5_000,
        start,
        Some(disk_usage_bytes(tmpdir.path())),
    );

    csv.record("final", 5_000, start, Some(disk_usage_bytes(tmpdir.path())));
}

fn load_items(db: &Database, count: usize, size: ItemSize) {
    let items = generate_items(count, size);
    for chunk in items.chunks(25) {
        let write_requests: Vec<WriteRequest> = chunk
            .iter()
            .map(|item| WriteRequest {
                put_request: Some(PutRequest { item: item.clone() }),
                delete_request: None,
            })
            .collect();

        let mut request_items = HashMap::new();
        request_items.insert(BENCH_TABLE.to_string(), write_requests);

        db.batch_write_item(BatchWriteItemRequest {
            request_items,
            return_consumed_capacity: None,
            return_item_collection_metrics: None,
        })
        .unwrap();
    }
}

fn load_items_offset(db: &Database, from: usize, to: usize, size: ItemSize) {
    let items: Vec<_> = (from..to)
        .map(|i| dynoxide_benchmarks::generate_item(i, size))
        .collect();

    for chunk in items.chunks(25) {
        let write_requests: Vec<WriteRequest> = chunk
            .iter()
            .map(|item| WriteRequest {
                put_request: Some(PutRequest { item: item.clone() }),
                delete_request: None,
            })
            .collect();

        let mut request_items = HashMap::new();
        request_items.insert(BENCH_TABLE.to_string(), write_requests);

        db.batch_write_item(BatchWriteItemRequest {
            request_items,
            return_consumed_capacity: None,
            return_item_collection_metrics: None,
        })
        .unwrap();
    }
}

fn run_queries(db: &Database, count: usize) {
    for i in 0..count {
        let pk = format!("user#{:06}", i % 100);
        let mut eav = HashMap::new();
        eav.insert(":pk".to_string(), AttributeValue::S(pk));
        eav.insert(":lo".to_string(), AttributeValue::N("0".to_string()));
        eav.insert(":hi".to_string(), AttributeValue::N("1000".to_string()));
        eav.insert(":age".to_string(), AttributeValue::N("25".to_string()));

        db.query(QueryRequest {
            table_name: BENCH_TABLE.to_string(),
            key_condition_expression: Some("pk = :pk AND sk BETWEEN :lo AND :hi".to_string()),
            filter_expression: Some("age > :age".to_string()),
            expression_attribute_values: Some(eav),
            scan_index_forward: true,
            ..Default::default()
        })
        .unwrap();
    }
}

fn run_scan(db: &Database) {
    let mut eav = HashMap::new();
    eav.insert(":age".to_string(), AttributeValue::N("70".to_string()));

    db.scan(ScanRequest {
        table_name: BENCH_TABLE.to_string(),
        filter_expression: Some("age > :age".to_string()),
        expression_attribute_values: Some(eav),
        ..Default::default()
    })
    .unwrap();
}

fn run_updates(db: &Database, count: usize) {
    let items = generate_items(count, ItemSize::Medium);
    for (i, item) in items.iter().enumerate() {
        let key = make_key(item);

        let mut eav = HashMap::new();
        eav.insert(
            ":val".to_string(),
            AttributeValue::L(vec![AttributeValue::N(format!("{i}"))]),
        );

        db.update_item(UpdateItemRequest {
            table_name: BENCH_TABLE.to_string(),
            key,
            update_expression: Some("SET scores = list_append(scores, :val)".to_string()),
            condition_expression: Some("attribute_exists(pk)".to_string()),
            expression_attribute_values: Some(eav),
            ..Default::default()
        })
        .unwrap();
    }
}

fn delete_items(db: &Database, count: usize) {
    let items = generate_items(count, ItemSize::Medium);
    for item in &items {
        let key = make_key(item);

        db.delete_item(DeleteItemRequest {
            table_name: BENCH_TABLE.to_string(),
            key,
            ..Default::default()
        })
        .unwrap();
    }
}

// --- Docker container idle RSS helpers ---

/// Start a container, wait for its first successful ListTables, settle for
/// 30 seconds (no requests served), then take 3 RSS samples 10 seconds apart
/// via `docker stats`. Returns all samples as f64 bytes.
fn measure_container_idle_rss(
    runtime: &tokio::runtime::Runtime,
    target: &ContainerTarget,
) -> Result<Vec<f64>, String> {
    const SETTLE_SECS: u64 = 30;
    const SAMPLE_COUNT: usize = 3;
    const SAMPLE_INTERVAL_SECS: u64 = 10;

    let container_name = target.container_name("dynoxide-bench-mem-");
    remove_container(&container_name);
    std::thread::sleep(Duration::from_millis(500));

    target.run(&container_name)?;

    // The same readiness check as the startup benchmark.
    if !runtime.block_on(wait_for_list_tables(
        &target.endpoint(),
        CONTAINER_READY_TIMEOUT,
    )) {
        let reason = report_not_ready(target, &container_name, CONTAINER_READY_TIMEOUT);
        remove_container(&container_name);
        return Err(reason);
    }

    // Settle: no requests, just wait
    eprintln!("    settling {SETTLE_SECS}s after the first ListTables...");
    std::thread::sleep(Duration::from_secs(SETTLE_SECS));

    // Take multiple samples
    let mut samples = Vec::new();
    for i in 0..SAMPLE_COUNT {
        if i > 0 {
            std::thread::sleep(Duration::from_secs(SAMPLE_INTERVAL_SECS));
        }
        if let Some(rss) = read_container_rss(&container_name) {
            eprintln!("    sample {}: {}", i + 1, format_bytes(rss));
            samples.push(rss as f64);
        }
    }

    // Containers run without `--rm`, so one that died during the settle is
    // still there for `docker stats` to read, and whatever it reports is not
    // idle memory. A container running now was
    // running for every sample.
    let state = container_state(&container_name);
    let stopped =
        (state != ContainerState::Running).then(|| report_stopped(target, &container_name, &state));

    // Clean up
    remove_container(&container_name);
    std::thread::sleep(Duration::from_millis(500));

    if let Some(reason) = stopped {
        Err(reason)
    } else if samples.is_empty() {
        Err(format!(
            "{}: docker stats returned no memory reading for {container_name}",
            target.label
        ))
    } else {
        Ok(samples)
    }
}

/// Read a container's current memory usage from `docker stats`.
/// Output format: "312.5MiB / 7.656GiB" — we parse the first value.
fn read_container_rss(container_name: &str) -> Option<u64> {
    let output = Command::new("docker")
        .args([
            "stats",
            container_name,
            "--no-stream",
            "--format",
            "{{.MemUsage}}",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    // Format: "312.5MiB / 7.656GiB"
    let usage_part = text.trim().split('/').next()?.trim();
    parse_mem_usage(usage_part)
}

/// Parse a Docker memory usage string like "312.5MiB" or "1.2GiB" into bytes.
fn parse_mem_usage(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(val) = s.strip_suffix("GiB") {
        val.trim()
            .parse::<f64>()
            .ok()
            .map(|v| (v * 1_073_741_824.0) as u64)
    } else if let Some(val) = s.strip_suffix("MiB") {
        val.trim()
            .parse::<f64>()
            .ok()
            .map(|v| (v * 1_048_576.0) as u64)
    } else if let Some(val) = s.strip_suffix("KiB") {
        val.trim().parse::<f64>().ok().map(|v| (v * 1024.0) as u64)
    } else if let Some(val) = s.strip_suffix("B") {
        val.trim().parse::<u64>().ok()
    } else {
        None
    }
}
