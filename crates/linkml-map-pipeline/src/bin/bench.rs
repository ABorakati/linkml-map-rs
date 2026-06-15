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
            make_val(&[("id", Value::Str(format!("P:{i}"))), ("height", height)])
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

    // ── FK parallel microbenchmark ────────────────────────────────────────────
    // Shows that FK transforms still parallelize: index is built once, each
    // row transform only reads from it (Arc<ObjectIndex>, no lock contention).
    fk_parallel_bench(n_rows)?;

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

/// FK parallel microbenchmark.
///
/// Builds ONE `ObjectIndex` over a synthetic container (N entities), then fans
/// out M mapping-row transforms across 1/2/4/8 rayon threads each sharing
/// `Arc<ObjectIndex>`.  Reports rows/sec to show parallelization holds.
///
/// The critical property: `ObjectIndex` is `Send + Sync` (no locking), so
/// all threads read the same index simultaneously without contention.
fn fk_parallel_bench(n_rows: usize) -> Result<()> {
    use indexmap::IndexMap;
    use linkml_map_core::{
        datamodel::{ClassDerivation, SlotDerivation, TransformationSpecification},
        engine::{CompiledExprs, ObjectIndex, ObjectTransformer},
        schema::{
            ClassDef, InMemorySchema, InMemorySchemaBuilder, RangeKind, SchemaProvider, SlotDef,
        },
        value::Value,
    };
    use rayon::prelude::*;
    use std::sync::Arc;
    use std::time::Instant;

    // ── Schema: Container { entities: {Entity}, mappings: [Mapping] }
    //           Mapping  { subject: Entity, predicate: string }
    //           Entity   { id: string (identifier), name: string }
    let schema: InMemorySchema = InMemorySchemaBuilder::new()
        .add_type("string")
        .add_class(ClassDef {
            name: "Container".into(),
            tree_root: true,
            is_a: None,
            mixins: vec![],
        })
        .add_slot(
            "Container",
            SlotDef {
                name: "entities".into(),
                range: RangeKind::Class("Entity".into()),
                multivalued: true,
                required: false,
                identifier: false,
                key: false,
                unit: None,
                any_of_enums: vec![],
                inlined: false,
                inlined_as_list: false,
            },
        )
        .add_slot(
            "Container",
            SlotDef {
                name: "mappings".into(),
                range: RangeKind::Class("Mapping".into()),
                multivalued: true,
                required: false,
                identifier: false,
                key: false,
                unit: None,
                any_of_enums: vec![],
                inlined: false,
                inlined_as_list: false,
            },
        )
        .add_class(ClassDef {
            name: "Mapping".into(),
            tree_root: false,
            is_a: None,
            mixins: vec![],
        })
        .add_slot(
            "Mapping",
            SlotDef {
                name: "subject".into(),
                range: RangeKind::Class("Entity".into()),
                multivalued: false,
                required: false,
                identifier: false,
                key: false,
                unit: None,
                any_of_enums: vec![],
                inlined: false,
                inlined_as_list: false,
            },
        )
        .add_slot(
            "Mapping",
            SlotDef {
                name: "predicate".into(),
                range: RangeKind::Type("string".into()),
                multivalued: false,
                required: false,
                identifier: false,
                key: false,
                unit: None,
                any_of_enums: vec![],
                inlined: false,
                inlined_as_list: false,
            },
        )
        .add_class(ClassDef {
            name: "Entity".into(),
            tree_root: false,
            is_a: None,
            mixins: vec![],
        })
        .add_slot(
            "Entity",
            SlotDef {
                name: "id".into(),
                range: RangeKind::Type("string".into()),
                multivalued: false,
                required: true,
                identifier: true,
                key: false,
                unit: None,
                any_of_enums: vec![],
                inlined: false,
                inlined_as_list: false,
            },
        )
        .add_slot(
            "Entity",
            SlotDef {
                name: "name".into(),
                range: RangeKind::Type("string".into()),
                multivalued: false,
                required: false,
                identifier: false,
                key: false,
                unit: None,
                any_of_enums: vec![],
                inlined: false,
                inlined_as_list: false,
            },
        )
        .build();

    // ── Spec: Mapping → FlatMapping (subject.id, subject.name) via FK exprs
    let mut mapping_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
    mapping_slots.insert(
        "subject_id".into(),
        SlotDerivation {
            name: "subject_id".into(),
            expr: Some("subject.id".into()),
            ..Default::default()
        },
    );
    mapping_slots.insert(
        "subject_name".into(),
        SlotDerivation {
            name: "subject_name".into(),
            expr: Some("subject.name".into()),
            ..Default::default()
        },
    );
    mapping_slots.insert(
        "predicate_id".into(),
        SlotDerivation {
            name: "predicate_id".into(),
            populated_from: Some("predicate".into()),
            ..Default::default()
        },
    );

    let spec = TransformationSpecification {
        class_derivations: Some(vec![ClassDerivation {
            name: "Mapping".into(),
            populated_from: Some("Mapping".into()),
            slot_derivations: Some(mapping_slots),
            ..Default::default()
        }]),
        ..Default::default()
    };

    let compiled_exprs =
        CompiledExprs::build(&spec).map_err(|e| anyhow::anyhow!("compile exprs: {e}"))?;

    // ── Synthesize N_ENTITIES entities + n_rows mapping rows
    let n_entities: usize = (n_rows / 100).max(20).min(1000);

    let entities_json: serde_json::Value = {
        let mut obj = serde_json::Map::new();
        for i in 0..n_entities {
            obj.insert(
                format!("E:{i}"),
                serde_json::json!({ "name": format!("entity_{i}") }),
            );
        }
        serde_json::Value::Object(obj)
    };
    let mappings_json: serde_json::Value = serde_json::Value::Array(
        (0..n_rows)
            .map(|j| {
                serde_json::json!({
                    "subject": format!("E:{}", j % n_entities),
                    "predicate": format!("P:{j}"),
                })
            })
            .collect(),
    );
    let container_json = serde_json::json!({
        "entities": entities_json,
        "mappings": mappings_json,
    });
    let container: Value = serde_json::from_value(container_json).unwrap();

    // Build index ONCE — this is the key invariant: one build, N parallel reads.
    let t_idx = Instant::now();
    let index = Arc::new(ObjectIndex::build(
        &container,
        Some("Container"),
        Some(&schema as &dyn SchemaProvider),
    ));
    let idx_ms = t_idx.elapsed().as_millis();

    let mappings: Vec<Value> = match &container {
        Value::Map(m) => match m.get("mappings") {
            Some(Value::List(l)) => l.clone(),
            _ => panic!("expected mappings list"),
        },
        _ => panic!("expected container map"),
    };

    let spec = Arc::new(spec);
    let compiled_exprs = Arc::new(compiled_exprs);
    let schema = Arc::new(schema);

    println!();
    println!(
        "=== FK parallel microbenchmark ({n_rows} rows, {n_entities} entities, index built in {idx_ms}ms) ==="
    );
    println!("{:<10} {:>14} {:>12}", "workers", "rows/sec", "elapsed_ms");
    println!("{}", "-".repeat(40));

    for &workers in &[1usize, 2, 4, 8] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .map_err(|e| anyhow::anyhow!("rayon pool: {e}"))?;

        let spec2 = Arc::clone(&spec);
        let compiled2 = Arc::clone(&compiled_exprs);
        let schema2 = Arc::clone(&schema);
        let index2 = Arc::clone(&index);
        let rows2 = mappings.clone();

        let (rows_per_sec, elapsed_ms) = pool.install(move || {
            let start = Instant::now();
            let _: Vec<Value> = rows2
                .par_iter()
                .map(|row| {
                    let engine = ObjectTransformer::new_borrowed(
                        &spec2,
                        Some(schema2.as_ref() as &dyn SchemaProvider),
                        None,
                    )
                    .with_compiled_exprs(&compiled2);
                    engine
                        .map_object_with_index(row, Some("Mapping"), &index2)
                        .expect("FK transform failed")
                })
                .collect();
            let elapsed = start.elapsed();
            let rps = if elapsed.as_secs_f64() > 0.0 {
                rows2.len() as f64 / elapsed.as_secs_f64()
            } else {
                f64::INFINITY
            };
            (rps, elapsed.as_millis())
        });

        println!("{:<10} {:>14.0} {:>12}", workers, rows_per_sec, elapsed_ms);
    }
    println!();
    println!("Note: all workers share one Arc<ObjectIndex> built once before the loop.");
    println!("      Index reads are lock-free (Send+Sync HashMap, no interior mutability).");

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
