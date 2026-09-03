//! Throwaway profiler: isolate tree-sitter parse cost from the extract walk.
//!
//! Usage: cargo run --release --example parse_vs_walk -- <repo_root>
//!
//! Reads every supported source file once, then times three things across the
//! whole repo (single-threaded, so the numbers are comparable per-phase):
//!   1. read+utf8 (I/O baseline)
//!   2. parse_source only (tree-sitter)
//!   3. extract::extract (parse + the merged entity/symbol walk)
//!
//! walk cost = (3) - (2). Prints totals and the parse:walk ratio.

use std::time::Instant;
use varde_code::extract;
use varde_code::parse::{language_for_path, parse_source};
use varde_code::scan::list_source_files;

fn main() {
    let root = std::env::args()
        .nth(1)
        .expect("usage: parse_vs_walk <repo_root>");
    let files = list_source_files(&root).expect("list files");

    // Preload bytes so I/O is out of the timed sections.
    let mut sources: Vec<(ast_grep_language::SupportLang, String)> = Vec::new();
    let mut bytes_total = 0usize;
    for f in &files {
        let path = std::path::Path::new(&f.path);
        let Some(lang) = language_for_path(path) else {
            continue;
        };
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        bytes_total += src.len();
        sources.push((lang, src));
    }
    eprintln!("supported files: {}  bytes: {}", sources.len(), bytes_total);

    // Warm once (grammar init, allocator).
    for (lang, src) in &sources {
        let p = parse_source(lang, src);
        std::hint::black_box(extract::extract(&p, 0));
    }

    let reps = 5;

    let t = Instant::now();
    for _ in 0..reps {
        for (lang, src) in &sources {
            std::hint::black_box(parse_source(lang, src));
        }
    }
    let parse_only = t.elapsed() / reps;

    let t = Instant::now();
    let mut ent = 0usize;
    let mut sym = 0usize;
    for _ in 0..reps {
        for (lang, src) in &sources {
            let p = parse_source(lang, src);
            let r = extract::extract(&p, 0);
            ent = r.entities.len();
            sym = r.symbols.len();
            std::hint::black_box((&r.entities, &r.symbols));
        }
    }
    let parse_plus_walk = t.elapsed() / reps;

    let walk = parse_plus_walk.saturating_sub(parse_only);
    eprintln!("parse_only        = {parse_only:?}");
    eprintln!("parse+walk        = {parse_plus_walk:?}");
    eprintln!("walk (difference) = {walk:?}");
    eprintln!(
        "ratio parse:walk  = {:.2} : {:.2}",
        parse_only.as_secs_f64() / parse_plus_walk.as_secs_f64(),
        walk.as_secs_f64() / parse_plus_walk.as_secs_f64(),
    );
    eprintln!("(last-file entities={ent} symbols={sym})");
}
