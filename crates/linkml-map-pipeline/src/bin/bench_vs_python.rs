//! Rust side of the base-Python comparison. Same workload as `bench_python.py`:
//! N synthetic PersonQuantityValue rows through the `qv-to-scalar` transform,
//! transform-only (no I/O), reusing the cached-AST `CompiledPlan`.
//!
//! Emits machine-readable JSON lines (one per configuration) plus a human
//! summary on stderr. Two configurations:
//!   - `rust-1thread`  : single worker — apples-to-apples vs Python's GIL.
//!   - `rust-Ncore`    : all logical cores — the multicore advantage.
//!
//! Usage: cargo run --release -p linkml-map-pipeline --bin bench-vs-python [N_ROWS]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use linkml_map_core::value::Value;
use linkml_map_pipeline::CompiledPlan;
use rayon::prelude::*;

fn make_rows(n: usize) -> Vec<Value> {
    let make_val = |pairs: &[(&str, Value)]| -> Value {
        let obj = serde_json::Value::Object(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), serde_json::to_value(v).unwrap()))
                .collect(),
        );
        serde_json::from_value(obj).unwrap()
    };
    (0..n)
        .map(|i| {
            let height = make_val(&[
                ("value", Value::Float(150.0 + (i as f64 % 100.0))),
                ("unit", Value::Str("cm".into())),
            ]);
            make_val(&[("id", Value::Str(format!("P:{i}"))), ("height", height)])
        })
        .collect()
}

fn time_transform(plan: &Arc<CompiledPlan>, rows: &[Value]) -> f64 {
    let start = Instant::now();
    let processed: usize = rows
        .par_iter()
        .map(|row| usize::from(plan.transform(row.clone()).is_ok()))
        .sum();
    assert_eq!(processed, rows.len(), "all rows must transform");
    let elapsed = start.elapsed().as_secs_f64();
    if elapsed > 0.0 {
        rows.len() as f64 / elapsed
    } else {
        f64::INFINITY
    }
}

fn run(plan: &Arc<CompiledPlan>, rows: &[Value], workers: usize, label: &str) -> Result<()> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()?;
    // Warm up.
    pool.install(|| {
        for r in rows.iter().take(rows.len().min(100)) {
            let _ = plan.transform(r.clone());
        }
    });
    let start = Instant::now();
    let rps = pool.install(|| time_transform(plan, rows));
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "{}",
        serde_json::json!({
            "impl": label, "workers": workers, "n_rows": rows.len(),
            "elapsed_s": elapsed, "rows_per_sec": rps,
        })
    );
    eprintln!(
        "[{label}] {} rows in {elapsed:.3}s = {rps:.0} rows/sec ({workers} thread(s))",
        rows.len()
    );
    Ok(())
}

fn main() -> Result<()> {
    let n_rows: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100_000);

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let fixture = PathBuf::from(manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/examples/measurements");

    let plan = Arc::new(CompiledPlan::load(
        &fixture.join("transform/qv-to-scalar.transform.yaml"),
        &fixture.join("source/quantity_value.yaml"),
        &fixture.join("target/quantity_value_flat.yaml"),
        Some("Person"),
    )?);

    let rows = make_rows(n_rows);
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);

    run(&plan, &rows, 1, "rust-1thread")?;
    run(&plan, &rows, cores, "rust-Ncore")?;
    Ok(())
}
