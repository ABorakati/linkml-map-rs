#![no_main]
//! The jsonl loader over arbitrary bytes (file-based, so each input is written to a scratch file).

use fuzz_support::load_bytes;
use libfuzzer_sys::fuzz_target;
use linkml_map_io::Format;

fuzz_target!(|data: &[u8]| {
    let _ = load_bytes(Format::Jsonl, data);
});
