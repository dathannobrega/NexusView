//! Minimal CLI harness over the engine — also the seed of the future headless
//! CLI promised by RNF-04, and a benchmark tool for open/index/search timing.
//!
//!   cargo run --release --example nexus_query -- <file> [query]
//!
//! Prints structure, timings, and a preview of the first matching rows.

use nexus_core::Dataset;
use std::process::ExitCode;
use std::time::Instant;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: nexus_query <file> [query]");
        return ExitCode::FAILURE;
    };
    let query = args.next().unwrap_or_default();

    let t0 = Instant::now();
    let dataset = match Dataset::open(&path, None) {
        Ok(ds) => ds,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let open_elapsed = t0.elapsed();

    // Two runs: the first builds any lazy index (Bloom, RF-04); the second
    // reuses it. For a selective single substring, warm ≫ cold faster.
    let t1 = Instant::now();
    let matches = match dataset.search(&query) {
        Ok(ids) => ids,
        Err(e) => {
            eprintln!("query error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let cold = t1.elapsed();
    let t2 = Instant::now();
    let _ = dataset.search(&query);
    let warm = t2.elapsed();

    println!("file        : {path}");
    println!(
        "columns     : {} ({})",
        dataset.column_count(),
        dataset.columns().join(", ")
    );
    println!("rows        : {}", dataset.row_count());
    println!("open+index  : {open_elapsed:.2?}");
    println!(
        "query       : {:?}",
        if query.is_empty() { "<all>" } else { &query }
    );
    println!("matches     : {}", matches.len());
    println!("search 1st  : {cold:.2?}  (first run; builds the lazy Bloom for a substring)");
    println!("search 2nd  : {warm:.2?}  (repeat run; Bloom reused for a substring)");

    let preview = 5.min(matches.len());
    if preview > 0 {
        println!("--- first {preview} matches ---");
        for &row in matches.iter().take(preview) {
            println!("{}", dataset.raw_line(row as usize));
        }
    }
    ExitCode::SUCCESS
}
