//! Shared git subprocess helper.
//!
//! Centralizes the "shell out to `git`, any failure -> `None`" contract used
//! by `churn::commit_count` and `query::mapping::git_changed_files`.

use std::path::Path;

/// Run `git <args>` from `cwd`. Returns `Some(stdout)` on a successful
/// (exit-0) invocation, `None` on any failure: missing binary, non-repo,
/// or non-zero exit.
pub fn run_git(args: &[&str], cwd: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}
