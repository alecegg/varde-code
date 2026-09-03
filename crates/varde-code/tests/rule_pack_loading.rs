//! End-to-end `load_rules` integration tests: discovery + parse + merge
//! against real tempdir rule packs (loader task `load-rules-public-api`).

use std::path::PathBuf;
use std::sync::Mutex;
use varde_code::rules::{RuleKind, load_rules};

/// `VARDE_USER_RULES_DIR` is process-global; serializing the tests that
/// touch it prevents env races between parallel test threads.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn tempdir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("varde-rules-it-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir creates");
    dir
}

fn write(path: &PathBuf, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent dir creates");
    }
    std::fs::write(path, contents).expect("fixture writes");
}

#[test]
fn load_rules_returns_repo_scoped_rule_with_empty_user_scope() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let repo = tempdir("one-repo");
    let user = tempdir("one-user");
    // Empty user scope: no rule files there.
    unsafe { std::env::set_var("VARDE_USER_RULES_DIR", &user) };
    write(
        &repo.join(".varde-code/rules/valid.toml"),
        r#"
[[rule]]
id = "no-console"
kind = "pattern"
severity = "warning"
message = "console call detected"
pattern = "console.log($MSG)"
"#,
    );

    let (rules, diagnostics) = load_rules(&repo);
    // Plus whatever ships built-in (see `load_rules_yields_only_builtins_when_scopes_are_empty`)
    // — this test only asserts the repo-scoped rule loaded correctly.
    let no_console = rules
        .iter()
        .find(|r| r.id == "no-console")
        .expect("no-console rule present");
    assert_eq!(no_console.kind, RuleKind::Pattern);
    assert!(diagnostics.is_empty());

    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&user);
}

#[test]
fn load_rules_yields_only_builtins_when_scopes_are_empty() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let repo = tempdir("empty-repo");
    let user = tempdir("empty-user");
    unsafe { std::env::set_var("VARDE_USER_RULES_DIR", &user) };
    // No `.varde-code/rules/` in the repo, no files in the user scope.

    let (rules, diagnostics) = load_rules(&repo);
    assert_eq!(
        rules.len(),
        varde_code::rules::builtin_rules().len(),
        "with empty user/repo scopes, only built-in rules load"
    );
    assert!(rules.iter().any(|r| r.id == "churn-complexity-hotspot"));
    assert!(diagnostics.is_empty());

    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&user);
}

#[test]
fn load_rules_merges_scopes_repo_wins() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let repo = tempdir("merge-repo");
    let user = tempdir("merge-user");
    unsafe { std::env::set_var("VARDE_USER_RULES_DIR", &user) };
    write(
        &user.join("shared.toml"),
        r#"
[[rule]]
id = "shared"
kind = "pattern"
severity = "info"
message = "user version"
pattern = "foo()"

[[rule]]
id = "user-only"
kind = "pattern"
severity = "warning"
message = "only in user scope"
pattern = "bar()"
"#,
    );
    write(
        &repo.join(".varde-code/rules/shared.toml"),
        r#"
[[rule]]
id = "shared"
kind = "sql"
severity = "error"
message = "repo version"
query = "SELECT file, line FROM entities"
"#,
    );

    let (rules, diagnostics) = load_rules(&repo);
    let shared = rules
        .iter()
        .find(|r| r.id == "shared")
        .expect("shared rule present");
    assert_eq!(shared.kind, RuleKind::Sql, "repo-scoped definition wins");
    assert_eq!(shared.severity, varde_code::rules::Severity::Error);
    assert!(
        rules.iter().any(|r| r.id == "user-only"),
        "user-only rule still runs"
    );
    assert!(diagnostics.is_empty());

    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&user);
}
