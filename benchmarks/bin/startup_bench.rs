use aws_sdk_dynamodb::types::{
    AttributeDefinition, AttributeValue, KeySchemaElement, KeyType, ScalarAttributeType,
};
use dynoxide_benchmarks::containers::{
    ContainerTarget, DYNAMODB_LOCAL, MULTI_SERVICE_EMULATORS, docker_available, image_available,
    remove_container, report_not_ready, wait_for_container_list_tables, wait_for_list_tables,
};
use dynoxide_benchmarks::http::make_sdk_client;
use dynoxide_benchmarks::stats::{mean, stddev};
use serde::Serialize;
use std::time::{Duration, Instant};

/// How long a container gets to answer its first ListTables.
const CONTAINER_READY_TIMEOUT: Duration = Duration::from_secs(300);

/// LocalStack has always been timed over at most five starts, because each
/// one is slow. The other multi-service emulators are timed the same way.
const MULTI_SERVICE_MAX_REPS: usize = 5;

#[derive(Debug, Serialize)]
struct StartupResult {
    target: String,
    mode: String, // "cold" or "warm"
    samples: Vec<f64>,
    mean_ms: f64,
    stddev_ms: f64,
}

/// A target that was attempted and produced no measurement.
#[derive(Debug, Serialize)]
struct Failure {
    target: String,
    reason: String,
}

#[derive(Debug, Serialize)]
struct AllResults {
    results: Vec<StartupResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    failures: Vec<Failure>,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let repetitions = args
        .iter()
        .position(|a| a == "--reps")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(10);

    let output_path = args
        .iter()
        .position(|a| a == "--output")
        .and_then(|i| args.get(i + 1).cloned());

    eprintln!("=== Startup Benchmark ({repetitions} repetitions) ===\n");

    let mut all_results = Vec::new();
    let mut failures = Vec::new();

    // --- Dynoxide Embedded (cold start) ---
    {
        eprintln!("Dynoxide Embedded (cold start):");
        let mut samples = Vec::new();
        for _ in 0..repetitions {
            let t = Instant::now();
            let db = dynoxide::Database::memory().unwrap();
            // Verify it's ready by listing tables
            db.list_tables(dynoxide::actions::list_tables::ListTablesRequest {
                exclusive_start_table_name: None,
                limit: None,
            })
            .unwrap();
            let us = t.elapsed().as_nanos() as f64 / 1000.0;
            samples.push(us / 1000.0); // Store as ms
        }
        let m = mean(&samples);
        let s = stddev(&samples);
        eprintln!(
            "  mean: {:.3}ms  stddev: {:.3}ms  ({:.1}us)",
            m,
            s,
            m * 1000.0
        );
        all_results.push(cold("dynoxide_embedded", samples));
    }

    // --- Dynoxide HTTP (cold start) ---
    {
        eprintln!("\nDynoxide HTTP (cold start):");
        match measure_dynoxide_http_startups(repetitions).await {
            Ok(samples) => {
                eprintln!(
                    "  mean: {:.1}ms  stddev: {:.1}ms",
                    mean(&samples),
                    stddev(&samples)
                );
                all_results.push(cold("dynoxide_http", samples));
            }
            Err(reason) => fail(&mut failures, "dynoxide_http", reason),
        }
    }

    if docker_available() {
        // --- DynamoDB Local (cold start, then warm start) ---
        if image_available(DYNAMODB_LOCAL.image) {
            eprintln!("\nDynamoDB Local (cold start):");
            match measure_container_startups(&DYNAMODB_LOCAL, repetitions).await {
                Ok(samples) => {
                    print_container_summary(&samples);
                    all_results.push(cold(DYNAMODB_LOCAL.key, samples));
                }
                Err(reason) => fail(&mut failures, DYNAMODB_LOCAL.key, reason),
            }

            eprintln!("\nDynamoDB Local (warm start):");
            match measure_warm_start().await {
                Ok(ms) => {
                    // One measurement: it is the same container throughout.
                    eprintln!("  time after warmup: {ms:.1}ms");
                    all_results.push(StartupResult {
                        target: DYNAMODB_LOCAL.key.to_string(),
                        mode: "warm".to_string(),
                        samples: vec![ms],
                        mean_ms: ms,
                        stddev_ms: 0.0,
                    });
                }
                Err(reason) => fail(&mut failures, DYNAMODB_LOCAL.key, reason),
            }
        } else {
            skip_missing_image(&DYNAMODB_LOCAL);
        }

        // --- LocalStack, Floci, MiniStack (cold start) ---
        for target in MULTI_SERVICE_EMULATORS {
            if !image_available(target.image) {
                skip_missing_image(target);
                continue;
            }
            eprintln!("\n{} (cold start):", target.label);
            let reps = repetitions.min(MULTI_SERVICE_MAX_REPS);
            match measure_container_startups(target, reps).await {
                Ok(samples) => {
                    print_container_summary(&samples);
                    all_results.push(cold(target.key, samples));
                }
                Err(reason) => fail(&mut failures, target.key, reason),
            }
        }
    } else {
        eprintln!("\nDocker not available -- skipping the container targets");
    }

    // Output
    let failed = !failures.is_empty();
    let output = AllResults {
        results: all_results,
        failures,
    };
    let json = serde_json::to_string_pretty(&output).unwrap();

    if let Some(path) = output_path {
        std::fs::write(&path, &json).unwrap();
        eprintln!("\nResults written to {path}");
    } else {
        println!("{json}");
    }

    // A target that was attempted and never answered is a failed run, not a
    // shorter results list: a README refresh would otherwise keep that
    // target's old number beside a new commit link.
    if failed {
        eprintln!("\nFAILED: no startup time for:");
        for failure in &output.failures {
            eprintln!("  {}: {}", failure.target, failure.reason);
        }
        std::process::exit(1);
    }
}

fn cold(target: &str, samples: Vec<f64>) -> StartupResult {
    StartupResult {
        target: target.to_string(),
        mode: "cold".to_string(),
        mean_ms: mean(&samples),
        stddev_ms: stddev(&samples),
        samples,
    }
}

fn fail(failures: &mut Vec<Failure>, target: &str, reason: String) {
    eprintln!("  FAILED: {reason}");
    failures.push(Failure {
        target: target.to_string(),
        reason,
    });
}

fn skip_missing_image(target: &ContainerTarget) {
    eprintln!(
        "\n{}: image {} not pulled -- skipping (docker pull it to include it)",
        target.label, target.image
    );
}

fn print_container_summary(samples: &[f64]) {
    eprintln!(
        "  mean: {:.0}ms  stddev: {:.0}ms",
        mean(samples),
        stddev(samples)
    );
}

async fn measure_dynoxide_http_startups(reps: usize) -> Result<Vec<f64>, String> {
    let mut samples = Vec::with_capacity(reps);
    for _ in 0..reps {
        samples.push(measure_dynoxide_http_startup().await?);
    }
    Ok(samples)
}

async fn measure_dynoxide_http_startup() -> Result<f64, String> {
    let t = Instant::now();

    let db = dynoxide::Database::memory().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        dynoxide::server::serve_on(listener, db).await;
    });

    let ready = wait_for_list_tables(&format!("http://{addr}"), Duration::from_secs(10)).await;
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    server.abort();

    if ready {
        Ok(ms)
    } else {
        Err(format!(
            "in-process server at {addr} did not answer ListTables within 10s"
        ))
    }
}

/// Time `reps` starts of `target`, from `docker run` to the first successful
/// ListTables. Any start that fails fails the target: an average over the
/// starts that happened to work would read as a full measurement.
async fn measure_container_startups(
    target: &ContainerTarget,
    reps: usize,
) -> Result<Vec<f64>, String> {
    let mut samples = Vec::with_capacity(reps);
    for i in 0..reps {
        eprintln!("  rep {}/{reps}...", i + 1);
        let ms = measure_container_startup(target)
            .await
            .map_err(|e| format!("rep {}/{reps}: {e}", i + 1))?;
        samples.push(ms);
    }
    Ok(samples)
}

async fn measure_container_startup(target: &ContainerTarget) -> Result<f64, String> {
    let container_name = target.container_name("dynoxide-bench-");
    remove_container(&container_name);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let t = Instant::now();
    target.run(&container_name)?;
    let ready = wait_for_container_list_tables(
        &target.endpoint(),
        &container_name,
        CONTAINER_READY_TIMEOUT,
    )
    .await;
    let ms = t.elapsed().as_secs_f64() * 1000.0;

    let result = if ready {
        Ok(ms)
    } else {
        Err(report_not_ready(
            target,
            &container_name,
            CONTAINER_READY_TIMEOUT,
        ))
    };
    remove_container(&container_name);
    tokio::time::sleep(Duration::from_millis(500)).await;
    result
}

/// Start DynamoDB Local, warm the JVM up with 500 PutItem and 100 GetItem
/// calls, then time one ListTables.
async fn measure_warm_start() -> Result<f64, String> {
    let target = &DYNAMODB_LOCAL;
    let container_name = "dynoxide-bench-warm";
    let timeout = Duration::from_secs(60);

    remove_container(container_name);
    tokio::time::sleep(Duration::from_millis(500)).await;

    target.run(container_name)?;
    if !wait_for_container_list_tables(&target.endpoint(), container_name, timeout).await {
        let reason = report_not_ready(target, container_name, timeout);
        remove_container(container_name);
        return Err(reason);
    }

    let client = make_sdk_client(&target.endpoint());

    let _ = client
        .create_table()
        .table_name("_warmup")
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("pk")
                .key_type(KeyType::Hash)
                .build()
                .unwrap(),
        )
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("pk")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .send()
        .await;

    for i in 0..500 {
        let _ = client
            .put_item()
            .table_name("_warmup")
            .item("pk", AttributeValue::S(format!("w{i}")))
            .item("data", AttributeValue::S(format!("data-{i}")))
            .send()
            .await;
    }
    for i in 0..100 {
        let _ = client
            .get_item()
            .table_name("_warmup")
            .key("pk", AttributeValue::S(format!("w{i}")))
            .send()
            .await;
    }

    // Now measure first operation after warmup
    let t = Instant::now();
    let listed = client.list_tables().send().await;
    let ms = t.elapsed().as_secs_f64() * 1000.0;

    remove_container(container_name);

    match listed {
        Ok(_) => Ok(ms),
        Err(e) => Err(format!("ListTables after the warm-up failed: {e}")),
    }
}
