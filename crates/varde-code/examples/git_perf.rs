//! Query-path freshness cost check: warm no-op `ensure_fresh` on a git repo,
//! measuring both changed_files paths — the default walk and the opt-in
//! git-status fast path (`VARDE_GIT_FAST_PATH=1`).
//!
//! Release benchmark (2026-09-03, Apple Git 2.50.1): the git path is
//! **2-5x SLOWER than the walk** at every repo size tested, so it is opt-in
//! and the walk is the default:
//!
//! - 201-file fixture: default walk ~35-70 ms/call vs git path ~60-160 ms/call
//! - varde (4,223 files): `git status` alone ~344 ms (scales with repo size)
//!   vs `list_source_files` ~83 ms; git spawn cost ~40-110 ms each
//! - the git path only wins where spawns are cheap (Linux) and the walk
//!   dominates (very large repos)
//!
//! Run: `cargo run --release -p varde-code --example git_perf`
fn main() {
    use varde_code::build::run_with_force;
    use varde_code::slice::{Scope, Slice, ensure_fresh};
    let root = std::env::temp_dir().join("varde-git-perf");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for i in 0..200 {
        std::fs::write(
            root.join(format!("a{i}.ts")),
            format!("import {{ foo{i} }} from \"./x\";\nfoo{i}();\n"),
        )
        .unwrap();
    }
    let mut exports = String::new();
    for i in 0..200 {
        exports.push_str(&format!("export const foo{i} = () => {i};\n"));
    }
    std::fs::write(root.join("x.ts"), exports).unwrap();
    // git init + commit so the fast path applies
    let g = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&root)
            .status()
            .unwrap();
    };
    g(&["init", "-q"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    g(&["add", "-A"]);
    g(&["commit", "-qm", "init"]);
    run_with_force(root.to_str().unwrap(), true).unwrap();

    let bench = |label: &str, n: u32| {
        for _ in 0..3 {
            ensure_fresh(&[Slice::Edges], root.to_str().unwrap(), &Scope::Repo).unwrap();
        }
        let t = std::time::Instant::now();
        for _ in 0..n {
            ensure_fresh(&[Slice::Edges], root.to_str().unwrap(), &Scope::Repo).unwrap();
        }
        let per_call = t.elapsed() / n;
        println!("{label}: {per_call:?} per call ({n} calls)");
    };

    // 1) Default: the walk.
    bench("warm no-op ensure_fresh (default walk)      ", 20);
    // 2) Opt-in git-status fast path. `set_var` is `unsafe` under edition 2024.
    unsafe { std::env::set_var("VARDE_GIT_FAST_PATH", "1") };
    bench("warm no-op ensure_fresh (git-status fast path)", 20);
    let _ = std::fs::remove_dir_all(&root);
}
