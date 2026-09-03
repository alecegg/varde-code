//! Automated 2x-vs-`ast-grep run` budget check for the pattern-rule pipeline
//! (AGENTS.md performance constraint; task pattern-rule-perf-benchmark).
//!
//! The full rule pipeline (find_pattern matching + constraints + correlation
//! + Finding conversion) must run in no more than 2x the wall-clock time of
//!   the equivalent `ast-grep run` invocation on the same target and pattern.
//!
//! The test is `#[ignore]`d by default because wall-clock comparisons flake
//! on loaded machines; run it explicitly, warm, exactly as the find_pattern
//! budget is verified today:
//!
//! ```text
//! cargo test --release -p varde-code --test pattern_rule_perf -- --ignored --nocapture
//! ```
//!
//! Methodology mirrors BENCHMARK.md's find_pattern runs: same target, same
//! pattern, 5 runs each side, median of the two, ratio must be <= 2.0.
//! The correlation DB is built once up front, outside the timed region —
//! only the rule pipeline itself is timed (ast-grep has no equivalent for
//! the build step).

use std::path::Path;
use std::process::Command;
use std::time::Instant;
use varde_code::model::ExtractOutput;
use varde_code::rules::{Rule, RuleKind};

/// The benchmark target: varde-code's own source tree (Rust), same class of
/// target BENCHMARK.md's scale runs used.
const TARGET: &str = "."; // whole crate: src + tests + fixtures

/// A representative Rust pattern with a metavariable — `Ok($VALUE)` matches
/// `Result`-unwrap sites throughout the codebase.
const PATTERN: &str = "Ok($VALUE)";

/// Rule pipelined against the target (explicit rust language, no constraints).
fn bench_rule() -> Rule {
    Rule {
        id: "bench-ok-pattern".to_string(),
        kind: RuleKind::Pattern,
        severity: varde_code::rules::Severity::Warning,
        message: "benchmark rule".to_string(),
        name: None,
        description: None,
        remediation: None,
        pattern: Some(PATTERN.to_string()),
        query: None,
        thresholds: None,
        strings: None,
        constraints: None,
        fix: None,
        rewrite: None,
        languages: Some(vec!["rust".to_string()]),
        exclude_test_paths: None,
        exclude_tooling_paths: None,
        test: None,
    }
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).expect("comparable"));
    values[values.len() / 2]
}

/// Time `f` n times, returning the median wall-clock duration in seconds.
#[allow(dead_code)]
fn time_n<F: FnMut()>(n: usize, mut f: F) -> f64 {
    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        let start = Instant::now();
        f();
        samples.push(start.elapsed().as_secs_f64());
    }
    median(&mut samples)
}

/// Build the correlation DB for the target once (outside the timed region).
fn build_correlation_db(repo: &Path, target: &Path) -> std::path::PathBuf {
    let output: ExtractOutput =
        varde_code::scan::run(target.to_str().expect("target is utf8")).expect("scan succeeds");
    let graph = varde_code::resolve::resolve(&output.entities, &output.symbols, &output.files)
        .expect("resolve succeeds");
    let db_path = std::env::temp_dir().join(format!(
        "varde-pattern-perf-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    varde_code::persist::persist(&db_path, std::slice::from_ref(&output), &graph, repo)
        .expect("persist succeeds");
    db_path
}

#[test]
#[ignore = "wall-clock budget check; run explicitly (see module docs)"]
fn pattern_rule_pipeline_stays_within_2x_of_ast_grep_run() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.to_path_buf();
    let target = manifest.join(TARGET);
    assert!(target.is_dir(), "benchmark target missing: {target:?}");

    // Correlate against a fresh DB (built once, not timed).
    let db_path = build_correlation_db(&repo, &target);
    let conn = varde_code::db::open(&db_path).expect("db opens");
    let rule = bench_rule();

    // Warm-up both sides once (grammar loading, disk cache, ...).
    let _ = varde_code::query::find_pattern::find_pattern(&serde_json::json!({
        "pattern": PATTERN,
        "path": target.display().to_string(),
        "language": "rust",
    }));
    let _ = Command::new("ast-grep")
        .args(["run", "-p", PATTERN, "--lang", "rust", "--json=compact"])
        .arg(&target)
        .output()
        .expect("ast-grep runs");

    // Interleaved runs: each pair does one ast-grep run + one pipeline run
    // back to back, so machine drift (thermal, background load) hits both
    // sides of a pair equally. Medians per side.
    let mut ast_samples = Vec::with_capacity(5);
    let mut pipeline_samples = Vec::with_capacity(5);
    for _ in 0..5 {
        let t = Instant::now();
        let out = Command::new("ast-grep")
            .args(["run", "-p", PATTERN, "--lang", "rust", "--json=compact"])
            .arg(&target)
            .output()
            .expect("ast-grep runs");
        assert!(out.status.success(), "ast-grep run failed");
        ast_samples.push(t.elapsed().as_secs_f64());

        let t = Instant::now();
        let (findings, diagnostics) = varde_code::rules::pattern::run_pattern_rules(
            std::slice::from_ref(&rule),
            &target,
            &conn,
        )
        .expect("pipeline succeeds");
        assert!(!findings.is_empty(), "benchmark rule must match something");
        assert!(diagnostics.is_empty(), "no diagnostics expected");
        pipeline_samples.push(t.elapsed().as_secs_f64());
    }
    let ast_seconds = median(&mut ast_samples);
    let pipeline_seconds = median(&mut pipeline_samples);

    let ratio = pipeline_seconds / ast_seconds;
    println!(
        "pattern-rule perf: pipeline={pipeline_seconds:.3}s ast-grep={ast_seconds:.3}s \
         ratio={ratio:.2}x (budget 2.0x)"
    );
    assert!(
        ratio <= 2.0,
        "pattern-rule pipeline {ratio:.2}x slower than ast-grep run — exceeds the 2x budget \
         (pipeline={pipeline_seconds:.3}s, ast-grep={ast_seconds:.3}s)"
    );

    let _ = std::fs::remove_file(&db_path);
}
