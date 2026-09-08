//! End-to-end `scan` CLI tests: spawn the built binary against a fixture
//! repo and assert process exit codes, stdout, and output-file behavior.
//!
//! The DB lives under the conventional `$HOME/.config/varde-code/repos/...`
//! path, so each child process receives a throwaway tempdir as `HOME`.

use std::path::{Path, PathBuf};
use std::process::Command;

const TS_FIXTURE: &str = r#"
async function fetchData(): Promise<void> {
  console.trace("async log");
}
"#;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_varde-code"))
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("varde-scan-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir creates");
    dir
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent dir creates");
    }
    std::fs::write(path, contents).expect("fixture writes");
}

/// Build the fixture repo and return (home, repo).
fn build_fixture(tag: &str, severity: &str) -> (PathBuf, PathBuf) {
    let home = tempdir(&format!("{tag}-home"));
    let repo = tempdir(&format!("{tag}-repo"));
    write(&repo.join("main.ts"), TS_FIXTURE);
    write(
        &repo.join(".varde-code/rules/pack.toml"),
        &format!(
            r#"
[[rule]]
id = "no-console"
kind = "pattern"
severity = "{severity}"
message = "console call detected"
pattern = "console.trace($MSG)"
"#
        ),
    );
    let build = Command::new(bin())
        .args(["build", "--repo-root"])
        .arg(&repo)
        .env("HOME", &home)
        .output()
        .expect("build runs");
    assert!(build.status.success(), "build fails: {:?}", build.status);
    (home, repo)
}

fn scan(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .args(["scan"])
        .arg("--json")
        .args(args)
        .env("HOME", home)
        .output()
        .expect("scan runs")
}

#[test]
fn error_finding_exits_nonzero_with_default_threshold() {
    let (home, repo) = build_fixture("err", "error");
    let out = scan(&home, &[&format!(r#"{{"repoRoot":"{}"}}"#, repo.display())]);
    assert!(!out.status.success(), "error finding → non-zero exit");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("envelope JSON");
    assert_eq!(payload["ok"], true);
    assert_eq!(
        payload["data"]["findings"].as_array().map(|a| a.len()),
        Some(1)
    );
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn info_only_findings_exit_zero_with_default_threshold() {
    let (home, repo) = build_fixture("info", "info");
    let out = scan(&home, &[&format!(r#"{{"repoRoot":"{}"}}"#, repo.display())]);
    assert!(out.status.success(), "info findings → exit 0");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn output_path_writes_envelope_and_stdout_stays_empty() {
    let (home, repo) = build_fixture("out", "warning");
    let out_path = home.join("scan-out.json");
    let out = scan(
        &home,
        &[&format!(
            r#"{{"repoRoot":"{}","output":"{}"}}"#,
            repo.display(),
            out_path.display()
        )],
    );
    assert!(
        out.stdout.is_empty(),
        "nothing on stdout when output is set"
    );
    let written = std::fs::read_to_string(&out_path).expect("output file written");
    let payload: serde_json::Value = serde_json::from_str(&written).expect("envelope JSON");
    assert_eq!(payload["ok"], true);
    assert!(payload["data"]["findings"].as_array().is_some());
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn omitted_output_prints_envelope_to_stdout() {
    let (home, repo) = build_fixture("stdout", "warning");
    let out = scan(&home, &[&format!(r#"{{"repoRoot":"{}"}}"#, repo.display())]);
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("envelope JSON");
    assert_eq!(payload["ok"], true);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&repo);
}

/// Ruby `module`/`include` is mixin composition, not interface
/// implementation, so the interface-contract SOLID rules (solid-lsp,
/// too-many-interfaces) and fat-interface-on-a-module must NOT fire on Ruby —
/// while a genuine Ruby *class* with too many methods still can.
#[test]
fn ruby_mixins_do_not_trigger_interface_solid_rules() {
    let home = tempdir("ruby-solid-home");
    let repo = tempdir("ruby-solid-repo");
    // A fat mixin module (16 methods) + a class including 4 mixins: under the
    // old rules this produced fat-interface, too-many-interfaces, and 4×
    // solid-lsp findings. All are Ruby-idiom false positives.
    let mut big = String::from("module BigHelpers\n");
    for i in 0..16 {
        big.push_str(&format!("  def m{i}; end\n"));
    }
    big.push_str("end\n");
    write(&repo.join("big.rb"), &big);
    write(
        &repo.join("widget.rb"),
        "module A; def a; end; end\nmodule B; def b; end; end\nmodule C; def c; end; end\nmodule D; def d; end; end\nclass Widget\n  include A\n  include B\n  include C\n  include D\n  def own; end\nend\n",
    );
    let build = Command::new(bin())
        .args(["build", "--repo-root"])
        .arg(&repo)
        .env("HOME", &home)
        .output()
        .expect("build runs");
    assert!(build.status.success(), "build fails: {:?}", build.status);

    let out = scan(&home, &[&format!(r#"{{"repoRoot":"{}"}}"#, repo.display())]);
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("envelope JSON");
    let findings = payload["data"]["findings"]
        .as_array()
        .expect("findings array");
    let interface_rules = [
        "solid-lsp",
        "solid-isp",
        "fat-interface",
        "too-many-interfaces",
    ];
    let offenders: Vec<&str> = findings
        .iter()
        .filter_map(|f| f["rule_id"].as_str())
        .filter(|id| interface_rules.contains(id))
        .collect();
    assert!(
        offenders.is_empty(),
        "Ruby mixins must not trigger interface SOLID rules, got: {offenders:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn unwritable_output_emits_error_envelope_and_exits_nonzero() {
    let (home, repo) = build_fixture("unwritable", "warning");
    let missing_dir = home.join("no-such-dir/out.json");
    let out = scan(
        &home,
        &[&format!(
            r#"{{"repoRoot":"{}","output":"{}"}}"#,
            repo.display(),
            missing_dir.display()
        )],
    );
    assert!(!out.status.success(), "unwritable output → non-zero exit");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("envelope JSON");
    assert_eq!(payload["ok"], false);
    assert!(payload["error"]["code"].as_str().is_some());
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&repo);
}
