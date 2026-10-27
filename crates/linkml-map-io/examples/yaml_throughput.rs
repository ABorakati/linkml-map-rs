//! Throughput check for the YAML stream writer: `cargo run --release -p linkml-map-io
//! --example yaml_throughput -- [records]`.

use std::time::Instant;

use linkml_map_core::value::Value;
use linkml_map_io::{Format, make_stream_writer};

fn record(i: usize) -> Value {
    let postcode_area = i % 20;
    let code = i % 7;
    let json = serde_json::json!({
        "id": format!("P:{i:08}"),
        "name": format!("Participant number {i} with a reasonably long descriptive name"),
        "age": 20 + (i % 60),
        "weight_kg": 50.0 + (i % 400) as f64 / 10.0,
        "active": i.is_multiple_of(2),
        "note": null,
        "status": if i.is_multiple_of(3) { "yes" } else { "enrolled" },
        "address": {"city": "London", "postcode": format!("SW{postcode_area} 1AA"), "lines": ["1 High Street", "Flat 2"]},
        "codes": [format!("C{code}"), "D: 9", "x #y"],
        "comment": "Free text: with colon, and a longer sentence that will certainly need to be folded by the emitter because it is long",
    });
    serde_json::from_value(json).unwrap()
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200_000);
    let rows: Vec<Value> = (0..n).map(record).collect();
    let mut writer = make_stream_writer(Format::Yaml, None).unwrap();
    let start = Instant::now();
    let mut bytes = 0usize;
    for chunk in rows.chunks(1000) {
        bytes += writer.write_chunk(chunk).unwrap().len();
    }
    let dt = start.elapsed();
    println!(
        "{n} records, {bytes} bytes, {:.3}s, {:.0} rec/s, {:.1} MB/s",
        dt.as_secs_f64(),
        n as f64 / dt.as_secs_f64(),
        bytes as f64 / 1e6 / dt.as_secs_f64()
    );
}
