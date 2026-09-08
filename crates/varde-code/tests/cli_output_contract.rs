//! Process-level output contract tests for non-query CLI commands.

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_varde-code");

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "varde-output-contract-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir creates");
    dir
}

fn run(home: &PathBuf, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("HOME", home)
        .output()
        .expect("binary runs")
}

fn assert_envelope(output: &Output, args: &[&str]) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|err| {
        panic!("{args:?} must write JSON to stdout: {err}; stdout: {stdout:?}")
    });
    assert!(
        value
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .is_some()
    );
    assert!(value.get("data").is_some(), "{args:?}: {stdout}");
    assert!(value.get("meta").is_some(), "{args:?}: {stdout}");
}

fn output_value(output: &Output, args: &[&str]) -> serde_json::Value {
    assert_envelope(output, args);
    serde_json::from_slice(&output.stdout).expect("stdout parsed after envelope assertion")
}

#[test]
fn non_query_machine_commands_emit_the_complete_envelope() {
    let home = tempdir("home");
    let missing = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("missing-output-contract-path");
    let missing = missing.to_string_lossy().to_string();
    let commands = [
        vec!["build", "--repo-root", &missing],
        vec!["extract", &missing],
        vec!["scan", "--json", "not-json"],
        vec!["test", "--json", "not-json"],
        vec!["rules_list", "--json", "not-json"],
        vec!["rules_seed", "--json", "not-json"],
        vec!["rules_remove", "--json", "not-json"],
        vec!["skills_list"],
        vec!["skills_install", "--agent", "unknown"],
        vec!["skills_remove", "--agent", "unknown"],
        vec!["hooks", "list"],
        vec!["hooks", "install", "--agent", "unknown"],
        vec!["hooks", "remove", "--agent", "unknown"],
        vec!["watch", "--list"],
        vec!["watch", "--stop-all"],
        vec!["watch", "--stop", "--repo", &missing],
    ];

    for args in commands {
        let output = run(&home, &args);
        assert_envelope(&output, &args);
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn path_aware_commands_apply_compact_output_policy() {
    let home = tempdir("policy-home");
    let repo = tempdir("policy-repo");
    let rules_dir = repo.join("rules");
    std::fs::write(repo.join("lib.rs"), "pub fn target() {}\n").expect("source fixture writes");
    std::fs::create_dir_all(&rules_dir).expect("rules dir creates");
    std::fs::write(
        rules_dir.join("pack.toml"),
        r#"
[[rule]]
id = "no-console-log"
kind = "pattern"
severity = "warning"
message = "console.log detected"
pattern = "console.log($MSG)"

[[rule.test]]
name = "flags console.log"
invalid = ["console.log(\"bad\");"]
"#,
    )
    .expect("rule fixture writes");
    let repo_root = repo.to_str().expect("utf-8 path");
    let repo_json = format!(r#"{{"repoRoot":"{repo_root}"}}"#);
    let rules_json = format!(r#"{{"rulesDir":"{}"}}"#, rules_dir.display());
    let missing = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("missing-policy-path");
    let missing = missing.to_string_lossy().to_string();

    let build = run(&home, &["build", "--repo-root", repo_root]);
    assert!(build.status.success(), "build failed: {:?}", build.stderr);
    let build_output = output_value(&build, &["build"]);
    assert_eq!(build_output["meta"]["compact"], true);
    let build_data = &build_output["data"];
    assert!(
        build_data["changedFilesSample"]
            .as_array()
            .is_some_and(|paths| {
                paths.iter().all(|path| {
                    !path
                        .as_str()
                        .is_some_and(|path| path.starts_with(repo_root))
                })
            })
    );

    let build_changed_files = run(
        &home,
        &[
            "build",
            "--repo-root",
            repo_root,
            "--force",
            "--changed-files",
        ],
    );
    assert!(
        build_changed_files.status.success(),
        "build failed: {:?}",
        build_changed_files.stderr
    );
    let build_data = &output_value(&build_changed_files, &["build"])["data"];
    assert!(build_data["changedFiles"].as_array().is_some_and(|paths| {
        paths.iter().all(|path| {
            !path
                .as_str()
                .is_some_and(|path| path.starts_with(repo_root))
        })
    }));

    let extract = run(
        &home,
        &["extract", repo.join("lib.rs").to_str().expect("utf-8 path")],
    );
    assert!(
        extract.status.success(),
        "extract failed: {:?}",
        extract.stderr
    );
    assert_eq!(
        output_value(&extract, &["extract"])["meta"]["compact"],
        true
    );

    let scan = run(&home, &["scan", "--json", &repo_json]);
    assert_eq!(output_value(&scan, &["scan"])["meta"]["compact"], true);

    let test = run(&home, &["test", "--json", &rules_json]);
    assert_eq!(output_value(&test, &["test"])["meta"]["compact"], true);
    assert_eq!(
        output_value(&test, &["test"])["data"]["results"][0]["file"],
        "pack.toml"
    );

    for args in [
        vec!["rules_list", "--json", repo_json.as_str()],
        vec!["rules_seed", "--json", repo_json.as_str()],
        vec!["rules_remove", "--json", repo_json.as_str()],
    ] {
        let output = run(&home, &args);
        assert_eq!(
            output_value(&output, &args)["meta"]["compact"],
            true,
            "{args:?}"
        );
    }

    let skills = run(&home, &["skills_list"]);
    assert_eq!(
        output_value(&skills, &["skills_list"])["meta"]["compact"],
        false
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&missing);
}

#[test]
fn nav_map_text_stays_human_readable() {
    let home = tempdir("nav-home");
    let repo = tempdir("nav-repo");
    std::fs::write(repo.join("lib.rs"), "pub fn target() {}\n").expect("fixture writes");
    let build = run(
        &home,
        &["build", "--repo-root", repo.to_str().expect("utf-8 path")],
    );
    assert!(
        build.status.success(),
        "build stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let json = format!(r#"{{"repoRoot":"{}"}}"#, repo.display());
    let output = run(&home, &["nav_map", "--format", "text", "--json", &json]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !stdout.trim_start().starts_with('{'),
        "text output: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&repo);
}
