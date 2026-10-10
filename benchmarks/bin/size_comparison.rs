use dynoxide_benchmarks::containers;
use dynoxide_benchmarks::memory_measurement::format_bytes;
use std::process::Command;

fn main() {
    println!("=== Dynoxide vs container emulators: Size Comparison ===\n");

    let dynoxide_size = measure_dynoxide_binary();
    let images: Vec<_> = containers::ALL
        .iter()
        .map(|target| (target, measure_docker_image(target.image)))
        .collect();

    println!("{:<25} {:>15}", "Target", "Install Size");
    println!("{:-<25} {:-<15}", "", "");

    if let Some(size) = dynoxide_size {
        println!("{:<25} {:>15}", "Dynoxide (binary)", format_bytes(size));
    } else {
        println!(
            "{:<25} {:>15}",
            "Dynoxide (binary)", "(build with --release first)"
        );
    }

    for (target, size) in &images {
        let name = format!("{} (Docker)", target.label);
        match size {
            Some(size) => println!("{:<25} {:>15}", name, format_bytes(*size)),
            None => println!("{:<25} {:>15}", name, "(docker pull first)"),
        }
    }

    // Print ratios
    if let Some(dyn_size) = dynoxide_size {
        println!();
        for (target, size) in &images {
            if let Some(size) = size {
                println!(
                    "{} is {:.0}x larger than Dynoxide",
                    target.label,
                    *size as f64 / dyn_size as f64
                );
            }
        }
    }

    // Output JSON for report generation
    let mut json = serde_json::Map::new();
    json.insert(
        "dynoxide_binary_bytes".to_string(),
        serde_json::json!(dynoxide_size),
    );
    for (target, size) in &images {
        json.insert(
            format!("{}_docker_bytes", target.key),
            serde_json::json!(size),
        );
    }
    println!("\n--- JSON Output ---");
    println!("{}", serde_json::Value::Object(json));
}

fn measure_dynoxide_binary() -> Option<u64> {
    // Look for the release binary in the parent crate's target directory
    let paths = ["../target/release/dynoxide", "target/release/dynoxide"];

    for path in &paths {
        if let Ok(meta) = std::fs::metadata(path) {
            return Some(meta.len());
        }
    }

    // Try building and measuring
    eprintln!(
        "Note: Release binary not found. Run `cargo build --release` in the root crate first."
    );
    None
}

fn measure_docker_image(image: &str) -> Option<u64> {
    let output = Command::new("docker")
        .args(["image", "inspect", image, "--format", "{{.Size}}"])
        .output()
        .ok()?;

    if !output.status.success() {
        eprintln!("Note: Docker image {image} not found. Run `docker pull {image}` first.");
        return None;
    }

    let size_str = String::from_utf8_lossy(&output.stdout);
    size_str.trim().parse::<u64>().ok()
}
