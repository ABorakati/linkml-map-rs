//! End-to-end Rust bench: full pipeline (read JSONL file -> transform -> write
//! JSONL file) over a pre-generated source file, wall-clocked at 1 thread and
//! all cores. Same workload + same input file as `bench_python.py e2e`, so the
//! comparison is end-to-end vs end-to-end.
//!
//! Usage: cargo run --release -p linkml-map-pipeline --bin bench-e2e <SRC.jsonl>
//!
//! Emits two JSON lines (`rust-e2e-1thread`, `rust-e2e-Ncore`) + stderr summary.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use linkml_map_io::Format;
use linkml_map_pipeline::{run_pipeline, PipelineConfig};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/examples/measurements")
}

async fn run_once(src: &str, out: &str, workers: usize, label: &str) -> Result<()> {
    let f = fixture();
    let cfg = PipelineConfig {
        source_path: src.to_owned(),
        source_format: Some(Format::Jsonl),
        schema_path: f
            .join("source/quantity_value.yaml")
            .to_str()
            .unwrap()
            .to_owned(),
        target_schema_path: f
            .join("target/quantity_value_flat.yaml")
            .to_str()
            .unwrap()
            .to_owned(),
        spec_path: f
            .join("transform/qv-to-scalar.transform.yaml")
            .to_str()
            .unwrap()
            .to_owned(),
        out_path: out.to_owned(),
        out_format: Some(Format::Jsonl),
        source_class: Some("Person".to_owned()),
        workers,
        ordered: false,
        channel_depth: 512,
    };
    // Wall-clock the whole call: plan load + stream read + transform + write.
    let start = Instant::now();
    let stats = run_pipeline(cfg).await?;
    let elapsed = start.elapsed().as_secs_f64();
    let rps = if elapsed > 0.0 {
        stats.rows_in as f64 / elapsed
    } else {
        f64::INFINITY
    };
    println!(
        "{}",
        serde_json::json!({
            "impl": label, "mode": "e2e", "workers": workers,
            "n_rows": stats.rows_in, "elapsed_s": elapsed, "rows_per_sec": rps,
        })
    );
    eprintln!(
        "[{label}] {} rows (read+transform+write) in {elapsed:.3}s = {rps:.0} rows/sec ({workers} thread(s))",
        stats.rows_in
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let src = std::env::args()
        .nth(1)
        .expect("usage: bench-e2e <SRC.jsonl>");
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);

    let out1 = format!("{src}.rust_out_1.jsonl");
    let outn = format!("{src}.rust_out_n.jsonl");
    run_once(&src, &out1, 1, "rust-e2e-1thread").await?;
    run_once(&src, &outn, cores, "rust-e2e-Ncore").await?;
    Ok(())
}
