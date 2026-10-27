//! JSONL oracle for the expression differential fuzzer.
//!
//! `expr_oracle <in.jsonl> <out.jsonl>` serves a batch file (or, with no
//! arguments, stdin to stdout). It runs on a 2 MiB thread, the stack size of a
//! rayon worker, so an expression that overflows the stack here would also
//! overflow it in the pipeline.

use std::fs::File;
use std::io::{BufReader, BufWriter, stdin, stdout};

use linkml_map_conformance::exprfuzz;

fn main() -> std::io::Result<()> {
    exprfuzz::install_quiet_panic_hook();
    let limit = std::time::Duration::from_secs(5);
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || -> std::io::Result<()> {
            if let [inp, out] = args.as_slice() {
                exprfuzz::serve(
                    BufReader::new(File::open(inp)?),
                    BufWriter::new(File::create(out)?),
                    limit,
                )
            } else {
                exprfuzz::serve(stdin().lock(), stdout(), limit)
            }
        })?
        .join()
        .expect("oracle thread panicked")
}
