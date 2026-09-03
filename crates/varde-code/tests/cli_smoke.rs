//! CLI smoke tests for the scaffolded varde-code binary.

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_varde-code"))
}

#[test]
fn help_prints_nonempty_stub_and_exits_zero() {
    let out = bin().arg("--help").output().expect("run --help");
    assert!(out.status.success(), "exit code must be 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.trim().is_empty(), "help must not be empty");
}
