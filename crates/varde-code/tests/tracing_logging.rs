//! Tracing/logging wiring tests (task: tracing-logging-wiring).
//! Human-readable logs on stderr; stdout stays a clean JSON document.

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_varde-code"))
}

const MIXED_DIR: &str = "tests/fixtures/ts/mixed";

#[test]
fn stdout_stays_valid_json_with_verbose_and_rust_log_trace() {
    let out = bin()
        .args(["extract", MIXED_DIR, "--verbose"])
        .env("RUST_LOG", "trace")
        .output()
        .expect("run extract");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must remain well-formed JSON");
    assert!(value.is_object());
}

#[test]
fn human_readable_logs_on_stderr_not_stdout() {
    let out = bin()
        .args(["extract", MIXED_DIR, "--verbose"])
        .env("RUST_LOG", "trace")
        .output()
        .expect("run extract");
    assert!(out.status.success());

    let stderr = String::from_utf8_lossy(&out.stderr);
    // Human-readable tracing lines: level tag + message on stderr.
    assert!(
        stderr.contains("INFO") || stderr.contains("WARN"),
        "stderr must carry human-readable log lines, got: {stderr:?}"
    );
    assert!(
        stderr.contains("extract complete") || stderr.contains("skipped"),
        "stderr should mention extraction activity, got: {stderr:?}"
    );

    // The same log text must be absent from stdout.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("INFO") && !stdout.contains("WARN"),
        "log lines must not leak onto stdout"
    );
}
