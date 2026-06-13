//! Benchmark: rows/second at workers = 1, 2, 4, 8.
//!
//! Synthesizes N rows (default 100_000) of the measurements fixture
//! (PersonQuantityValue with a nested QuantityValue), runs the pipeline
//! at each worker count, and prints a throughput table.
//!
//! Usage:
//!   cargo run -p linkml-map-pipeline --bin pipeline-bench [N_ROWS]
//!
//! Example:
//!   cargo run -p linkml-map-pipeline --bin pipeline-bench 100000
//!   cargo run -p linkml-map-pipeline --release --bin pipeline-bench

use std::path::PathBuf;

use anyhow::Result;
use linkml_map_io::{write_vec, Format};
use linkml_map_pipeline::PipelineConfig;

#[tokio::main]
async fn main() -> Result<()> {
    let n_rows: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100_000);

    // Locate fixture directory relative to this binary's manifest dir.
    // CARGO_MANIFEST_DIR is set at compile time.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let fixture = PathBuf::from(manifest_dir)
        .parent() // crates/
        .unwrap()
        .parent() // workspace root
        .unwrap()
        .join("tests/examples/measurements");

    let tmp = tempfile::tempdir()?;

    // ── Synthesize N source rows ──────────────────────────────────────────────
    use linkml_map_core::value::Value;

    let make_val = |pairs: &[(&str, Value)]| -> Value {
        let obj = serde_json::Value::Object(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), serde_json::to_value(v).unwrap()))
                .collect(),
        );
        serde_json::from_value(obj).unwrap()
    };

    println!("Synthesizing {n_rows} rows …");
    let rows: Vec<Value> = (0..n_rows)
        .map(|i| {
            let height = make_val(&[
                ("value", Value::Float(150.0 + (i as f64 % 100.0))),
                ("unit", Value::Str("cm".into())),
            ]);
            make_val(&[
                ("id", Value::Str(format!("P:{i}"))),
                ("height", height),
            ])
        })
        .collect();

    let src_path = tmp.path().join("bench_source.jsonl");
    write_vec(&src_path, Format::Jsonl, rows).await?;
    println!("Source rows written to {}", src_path.display());

    // ── Run at each worker count ──────────────────────────────────────────────
    let worker_counts = [1usize, 2, 4, 8];

    println!();
    println!(
        "{:<10} {:>12} {:>14} {:>12}",
        "workers", "rows_in", "rows/sec", "elapsed_ms"
    );
    println!("{}", "-".repeat(52));

    for &workers in &worker_counts {
        let out_path = tmp.path().join(format!("bench_out_{workers}.jsonl"));

        let cfg = PipelineConfig {
            source_path: src_path.to_str().unwrap().to_owned(),
            source_format: Some(Format::Jsonl),
            schema_path: fixture
                .join("source/quantity_value.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
            target_schema_path: fixture
                .join("target/quantity_value_flat.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
            spec_path: fixture
                .join("transform/qv-to-scalar.transform.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
            out_path: out_path.to_str().unwrap().to_owned(),
            out_format: Some(Format::Jsonl),
            source_class: Some("Person".to_owned()),
            workers,
            ordered: false, // unordered for maximum throughput in bench
            channel_depth: 512,
        };

        let stats = linkml_map_pipeline::run_pipeline(cfg).await?;

        println!(
            "{:<10} {:>12} {:>14.0} {:>12}",
            workers,
            stats.rows_in,
            stats.rows_per_sec,
            stats.elapsed.as_millis()
        );
    }

    println!();
    println!("Note: run with --release for production-representative numbers.");
    println!("Note: expr AST re-parsing is per-row in current engine (follow-up: plan.rs).");

    Ok(())
}
