//! End-to-end `scan` CLI tests: spawn the built binary against a fixture
//! repo and assert process exit codes, stdout, and output-file behavior.
//!
//! The DB lives under the conventional `$HOME/.config/varde-code/repos/...`
//! path, so each test redirects `HOME` to a throwaway tempdir (serialized —
//! env is process-global).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

static HOME_LOCK: Mutex<()> = Mutex::new(());

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
    unsafe { std::env::set_var("HOME", &home) };
    let build = Command::new(bin())
        .args(["build", "--repo-root"])
        .arg(&repo)
        .output()
        .expect("build runs");
    assert!(build.status.success(), "build fails: {:?}", build.status);
    (home, repo)
}

fn scan(home: &Path, args: &[&str]) -> std::process::Output {
    unsafe { std::env::set_var("HOME", home) };
    Command::new(bin())
        .args(["scan"])
        .arg("--json")
        .args(args)
        .output()
        .expect("scan runs")
}

#[test]
fn error_finding_exits_nonzero_with_default_threshold() {
    let _guard = HOME_LOCK.lock().expect("home lock");
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
    let _guard = HOME_LOCK.lock().expect("home lock");
    let (home, repo) = build_fixture("info", "info");
    let out = scan(&home, &[&format!(r#"{{"repoRoot":"{}"}}"#, repo.display())]);
    assert!(out.status.success(), "info findings → exit 0");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn output_path_writes_envelope_and_stdout_stays_empty() {
    let _guard = HOME_LOCK.lock().expect("home lock");
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
    let _guard = HOME_LOCK.lock().expect("home lock");
    let (home, repo) = build_fixture("stdout", "warning");
    let out = scan(&home, &[&format!(r#"{{"repoRoot":"{}"}}"#, repo.display())]);
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("envelope JSON");
    assert_eq!(payload["ok"], true);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn unwritable_output_emits_error_envelope_and_exits_nonzero() {
    let _guard = HOME_LOCK.lock().expect("home lock");
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
