#![no_main]
//! The stream writers (TSV/CSV above all) and `dump_output` over arbitrary values.

use fuzz_support::FuzzValue;
use libfuzzer_sys::{arbitrary::Arbitrary, fuzz_target};
use linkml_map_io::{Format, dump_output, make_stream_writer};

#[derive(Debug, Arbitrary)]
struct Input {
    chunks: Vec<Vec<FuzzValue>>,
    single: FuzzValue,
}

fuzz_target!(|input: Input| {
    let chunks: Vec<Vec<_>> = input
        .chunks
        .into_iter()
        .map(|c| c.into_iter().map(FuzzValue::into_value).collect())
        .collect();
    for format in [Format::Tsv, Format::Csv, Format::Json, Format::Jsonl, Format::Yaml] {
        let mut writer = make_stream_writer(format, None).expect("text writer");
        for chunk in &chunks {
            let _ = writer.write_chunk(chunk);
        }
        let _ = writer.finalize();
    }
    let single = input.single.into_value();
    for format in [Format::Tsv, Format::Csv, Format::Json, Format::Jsonl, Format::Yaml] {
        let _ = dump_output(&single, format);
    }
});
