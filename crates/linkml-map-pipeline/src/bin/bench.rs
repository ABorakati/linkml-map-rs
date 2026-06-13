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
use linkml_map_core::value::Value;
use linkml_map_io::{write_vec, Format};
use linkml_map_pipeline::{CompiledPlan, PipelineConfig};
use std::sync::Arc;
use std::time::Instant;

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
    println!("Note: the table above is end-to-end (JSONL read + transform + write).");
    println!("      The serial reader/writer caps throughput on this tiny-per-row fixture.");

    // ── Transform-only microbenchmark ─────────────────────────────────────────
    // Isolates per-row CPU (the cached-AST lever) from the serial I/O path:
    // generate rows in memory, then time ONLY plan.transform across rayon
    // threads. Compares the string-parse path (BEFORE) vs cached-AST (AFTER).
    transform_only_bench(&fixture, n_rows)?;

    Ok(())
}

/// Microbenchmark the transform step alone (no I/O), comparing the per-row
/// string-parse path against the cached-AST path at 1/2/4/8 threads.
fn transform_only_bench(fixture: &std::path::Path, n_rows: usize) -> Result<()> {
    let plan = Arc::new(CompiledPlan::load(
        &fixture.join("transform/qv-to-scalar.transform.yaml"),
        &fixture.join("source/quantity_value.yaml"),
        &fixture.join("target/quantity_value_flat.yaml"),
        Some("Person"),
    )?);

    // Build N rows in memory once.
    let make_val = |pairs: &[(&str, Value)]| -> Value {
        let obj = serde_json::Value::Object(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), serde_json::to_value(v).unwrap()))
                .collect(),
        );
        serde_json::from_value(obj).unwrap()
    };
    let rows: Vec<Value> = (0..n_rows)
        .map(|i| {
            let height = make_val(&[
                ("value", Value::Float(150.0 + (i as f64 % 100.0))),
                ("unit", Value::Str("cm".into())),
            ]);
            make_val(&[("id", Value::Str(format!("P:{i}"))), ("height", height)])
        })
        .collect();

    println!();
    println!(
        "=== Transform-only microbenchmark ({} expr: slot(s); {n_rows} rows) ===",
        plan.expr_slot_count()
    );
    println!(
        "{:<8} {:>16} {:>16} {:>10}",
        "workers", "BEFORE rows/sec", "AFTER rows/sec", "speedup"
    );
    println!("{}", "-".repeat(54));

    for &workers in &[1usize, 2, 4, 8] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()?;

        // BEFORE: per-row string parse.
        let before = pool.install(|| time_transform(&plan, &rows, /*cached=*/ false));
        // AFTER: cached AST.
        let after = pool.install(|| time_transform(&plan, &rows, /*cached=*/ true));

        println!(
            "{:<8} {:>16.0} {:>16.0} {:>9.2}x",
            workers,
            before,
            after,
            after / before
        );
    }
    println!();
    println!("BEFORE = transform_uncached (re-parses expr per row).");
    println!("AFTER  = transform (evaluates the cached AST; expr parsed once at load).");

    Ok(())
}

/// Run all rows through the (cached or uncached) transform on the current
/// rayon pool and return rows/sec.
fn time_transform(plan: &Arc<CompiledPlan>, rows: &[Value], cached: bool) -> f64 {
    use rayon::prelude::*;
    let start = Instant::now();
    let processed: usize = rows
        .par_iter()
        .map(|row| {
            let out = if cached {
                plan.transform(row.clone())
            } else {
                plan.transform_uncached(row.clone())
            };
            // Touch the result so the optimizer cannot elide the work.
            usize::from(out.is_ok())
        })
        .sum();
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(processed, rows.len(), "all rows must transform");
    if elapsed > 0.0 {
        rows.len() as f64 / elapsed
    } else {
        f64::INFINITY
    }
}
