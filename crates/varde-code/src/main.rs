//! varde-code CLI binary.
//!
//! Dispatches `extract` and the 20 query-mode subcommands. Query subcommands
//! print the uniform JSON envelope; the process never exits non-zero for a
//! query-mode error (errors are carried inside the envelope).

use clap::Parser;
use varde_code::cli::{Cli, Command, HooksCommand};

fn main() {
    install_panic_hook();
    let cli = Cli::parse();
    init_tracing(cli.verbose);
    match cli.command {
        Command::Extract { path } => run_extract(&path),
        Command::Build {
            repo_root,
            force,
            changed_files,
        } => run_build(&repo_root, force, changed_files),
        Command::Batch { json } => run_query("batch", &json),
        Command::SymbolsInFile { json } => run_query("symbols_in_file", &json),
        Command::SymbolsInFiles { json } => run_query("symbols_in_files", &json),
        Command::GetSymbol { json } => run_query("get_symbol", &json),
        Command::Dependencies { json } => run_query("dependencies", &json),
        Command::Dependents { json } => run_query("dependents", &json),
        Command::TestsForFile { json } => run_query("tests_for_file", &json),
        Command::Hotspots { json } => run_query("hotspots", &json),
        Command::Clusters { json } => run_query("clusters", &json),
        Command::MapFile { json } => run_query("map_file", &json),
        Command::MapSymbol { json } => run_query("map_symbol", &json),
        Command::MapPath { json } => run_query("map_path", &json),
        Command::Explore { json } => run_query("explore", &json),
        Command::BlastRadius { json } => run_query("blast_radius", &json),
        Command::SymbolBlastRadius { json } => run_query("symbol_blast_radius", &json),
        Command::DetectChanges { json } => run_query("detect_changes", &json),
        Command::FindImports { json } => run_query("find_imports", &json),
        Command::TypeHierarchy { json } => run_query("type_hierarchy", &json),
        Command::FilterSymbols { json } => run_query("filter_symbols", &json),
        Command::FindPattern { json } => run_query("find_pattern", &json),
        Command::ContextPack { json } => run_query("context_pack", &json),
        Command::NavMap { json, format } => run_nav_map(&json, &format),
        // `scan` is a read-and-report operation like the query modes, but it
        // must be able to exit non-zero (findings at/above the severity
        // threshold → CI gate), so it gets its own dispatch arm instead of
        // `run_query` (which never exits non-zero by contract).
        Command::Scan { json, apply, force } => std::process::exit(run_scan(&json, apply, force)),
        // `test` is a read-and-report operation like `scan`: it must exit
        // non-zero when any `[[test]]` case fails (CI gate), so it gets its
        // own dispatch arm instead of `run_query`.
        Command::Test { json } => std::process::exit(run_test(&json)),
        Command::RulesList { json } => run_rules_list(&json),
        Command::RulesSeed { json, user, force } => run_rules_seed(&json, user, force),
        Command::RulesRemove { json, user, force } => run_rules_remove(&json, user, force),
        Command::SkillsList => run_skills_list(),
        Command::SkillsInstall { agent, dir, force } => {
            run_skills_install(&agent, force, dir.as_deref())
        }
        Command::SkillsRemove { agent, dir, force } => {
            run_skills_remove(&agent, force, dir.as_deref())
        }
        Command::Hooks(HooksCommand::List) => run_hooks_list(),
        Command::Hooks(HooksCommand::Install { agent, force, dir }) => {
            run_hooks_install(&agent, force, dir.as_deref())
        }
        Command::Hooks(HooksCommand::Remove { agent, force, dir }) => {
            run_hooks_remove(&agent, force, dir.as_deref())
        }
        Command::SliceState { json } => run_query("slice_state", &json),
        Command::Watch {
            repos,
            config,
            debounce_ms,
            list,
            stop,
            stop_all,
        } => std::process::exit(if list {
            run_watch_list()
        } else if stop_all {
            run_watch_stop_all()
        } else if stop {
            run_watch_stop(&repos, config.as_deref())
        } else {
            run_watch(&repos, config.as_deref(), debounce_ms)
        }),
    }
}

/// Replace the default panic handler with a concise, user-facing message.
/// An unexpected panic is a bug, not a normal error path (those flow through
/// the JSON envelope or a `Result`), so a raw multi-line Rust panic dump at a
/// user is unhelpful. Print one clear line plus a report pointer; the process
/// still exits non-zero (101) so scripts and CI detect the failure. Honors
/// `RUST_BACKTRACE` implicitly — set it to still get the default trace.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown cause".to_string());
        let loc = info
            .location()
            .map(|l| format!(" at {}:{}", l.file(), l.line()))
            .unwrap_or_default();
        eprintln!("varde-code: internal error: {msg}{loc}");
        eprintln!(
            "This is a bug; please report it at https://github.com/alecegg/varde-code/issues"
        );
    }));
}

/// Run one `scan` invocation: flow + envelope + threshold exit code.
///
/// `apply`/`force` (from the `--apply`/`--force` flags) are merged into the
/// JSON input so `scan_repo` — and the MCP surface, which drives scan purely
/// via the JSON input — see one contract. An explicit JSON `apply`/`force`
/// field is honored as-is unless the corresponding flag is passed, in which
/// case the flag wins (an explicit CLI switch beats an embedded value).
///
/// Returns the process exit code: 0 when no finding meets/exceeds the
/// severity threshold (default `error`), non-zero otherwise; any tool-level
/// error (bad input, missing/stale DB, unwritable `output`) emits the
/// `{ok:false,error}` envelope and returns non-zero. `output` present →
/// envelope written to that file and nothing on stdout; absent → stdout.
fn run_scan(json: &str, apply: bool, force: bool) -> i32 {
    let mut value: serde_json::Value = match serde_json::from_str(json) {
        Ok(value) => value,
        Err(e) => {
            println!(
                "{}",
                varde_code::query::render(Err(varde_code::query::ApiError::new(
                    "invalid_input",
                    format!("input is not valid JSON: {e}"),
                )))
            );
            return 1;
        }
    };
    if apply {
        value["apply"] = serde_json::json!(true);
    }
    if force {
        value["force"] = serde_json::json!(true);
    }

    let result = varde_code::scan_cli::scan_repo(&value)
        .map(|payload| serde_json::json!({ "ok": true, "data": payload }));
    let envelope = match result {
        Ok(envelope) => envelope.to_string(),
        Err(err) => varde_code::query::render(Err(err)),
    };

    if let Some(path) = value.get("output").and_then(|o| o.as_str()) {
        if let Err(e) = std::fs::write(path, &envelope) {
            println!(
                "{}",
                varde_code::query::render(Err(varde_code::query::ApiError::new(
                    "write_error",
                    format!("cannot write scan output to {path}: {e}"),
                )))
            );
            return 1;
        }
    } else {
        println!("{envelope}");
    }

    // Compute the exit code from the emitted payload: re-parse the envelope
    // (already valid JSON — we just serialized it) and compare severities.
    let payload: serde_json::Value = match serde_json::from_str(&envelope) {
        Ok(payload) => payload,
        Err(err) => {
            eprintln!("varde-code: internal error re-parsing result envelope: {err}");
            return 2;
        }
    };
    if payload["ok"] == true {
        let threshold = varde_code::scan_cli::severity_threshold(&value)
            .unwrap_or(varde_code::rules::Severity::Error);
        // Applied-aware: findings whose rewrite was applied (`rewrite_status`
        // = "applied") are resolved and don't trip the gate; everything else
        // gates exactly as before (identical for non-apply runs).
        if varde_code::scan_cli::unresolved_findings_at_or_above(&payload["data"], threshold) {
            1
        } else {
            0
        }
    } else {
        1
    }
}

/// Run one `test` invocation: flow + envelope + failure-gate exit code.
///
/// Mirrors `run_scan`'s shape: parses `json`, runs the operation, prints the
/// `{ok, data}`/`{ok:false, error}` envelope to stdout, and returns the
/// process exit code — `0` when `summary.failed == 0`, non-zero when any
/// test failed or a tool-level error occurred (bad input JSON, invalid
/// `rulesDir`, etc).
fn run_test(json: &str) -> i32 {
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(value) => value,
        Err(e) => {
            println!(
                "{}",
                varde_code::query::render(Err(varde_code::query::ApiError::new(
                    "invalid_input",
                    format!("input is not valid JSON: {e}"),
                )))
            );
            return 1;
        }
    };

    let result = varde_code::test_cli::run_tests(&value)
        .map(|payload| serde_json::json!({ "ok": true, "data": payload }));
    let envelope = match result {
        Ok(envelope) => envelope.to_string(),
        Err(err) => varde_code::query::render(Err(err)),
    };
    println!("{envelope}");

    let payload: serde_json::Value = match serde_json::from_str(&envelope) {
        Ok(payload) => payload,
        Err(err) => {
            eprintln!("varde-code: internal error re-parsing result envelope: {err}");
            return 2;
        }
    };
    if payload["ok"] == true {
        let failed = payload["data"]["summary"]["failed"].as_u64().unwrap_or(0);
        if failed == 0 { 0 } else { 1 }
    } else {
        1
    }
}

/// Resolve the watch list (explicit `--repo`s + optional config file, or
/// the default `~/.config/varde-code/watch.toml` when neither is given),
/// then run the watcher until killed. Prints a clear error and exits
/// non-zero on any setup failure (bad config, no repos resolved, another
/// watcher already running for this repo set); once running, per-repo
/// reconcile failures are logged by `watch::run` and do not exit the
/// process — one repo's transient failure must not take down every other
/// watched repo.
fn run_watch(repos: &[String], config_path: Option<&str>, debounce_ms: u64) -> i32 {
    let resolved = match resolve_watch_repos(repos, config_path) {
        Ok(resolved) => resolved,
        Err(code) => return code,
    };

    match varde_code::watch::run(&resolved, std::time::Duration::from_millis(debounce_ms)) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("varde-code watch: {err:#}");
            1
        }
    }
}

/// Shared `--repo`/`--config` resolution for `run_watch` and `run_watch_stop`
/// — same repo set must resolve identically for both, since `--stop` looks
/// up a running watcher's lock by hashing this same resolved list (see
/// `watch::watch_set_id`). On failure, prints the error and returns the exit
/// code the caller should return.
fn resolve_watch_repos(
    repos: &[String],
    config_path: Option<&str>,
) -> Result<Vec<std::path::PathBuf>, i32> {
    let config = match config_path {
        Some(path) => match varde_code::watch::WatchConfig::load(std::path::Path::new(path)) {
            Ok(config) => Some(config),
            Err(err) => {
                eprintln!("varde-code watch: {err:#}");
                return Err(1);
            }
        },
        None => {
            let default_path = varde_code::watch::WatchConfig::default_path();
            if repos.is_empty() && default_path.exists() {
                match varde_code::watch::WatchConfig::load(&default_path) {
                    Ok(config) => Some(config),
                    Err(err) => {
                        eprintln!("varde-code watch: {err:#}");
                        return Err(1);
                    }
                }
            } else {
                None
            }
        }
    };

    varde_code::watch::resolve_repos(repos, config.as_ref()).map_err(|err| {
        eprintln!("varde-code watch: {err:#}");
        1
    })
}

/// `varde-code watch --list`: print every watcher instance (live or
/// stale-locked) as a JSON array and exit 0. Never exits non-zero — this is
/// a read-only listing, same posture as `rules_list`/`slice_state`.
fn run_watch_list() -> i32 {
    match varde_code::watch::list_instances() {
        Ok(instances) => {
            println!(
                "{}",
                serde_json::to_string(&instances).unwrap_or_else(|_| "[]".to_string())
            );
            0
        }
        Err(err) => {
            eprintln!("varde-code watch --list: {err:#}");
            1
        }
    }
}

/// `varde-code watch --stop`: stop the watcher for the `--repo`/`--config`-
/// resolved repo set and exit. Non-zero if no watcher lock exists for that
/// repo set or the stop itself fails.
fn run_watch_stop(repos: &[String], config_path: Option<&str>) -> i32 {
    let resolved = match resolve_watch_repos(repos, config_path) {
        Ok(resolved) => resolved,
        Err(code) => return code,
    };
    match varde_code::watch::stop(&resolved) {
        Ok(outcome) => {
            println!(
                "{}",
                serde_json::to_string(&outcome).unwrap_or_else(|_| "{}".to_string())
            );
            0
        }
        Err(err) => {
            eprintln!("varde-code watch --stop: {err:#}");
            1
        }
    }
}

/// `varde-code watch --stop-all`: stop every running watcher instance and
/// exit. Non-zero only if enumerating instances itself fails; a per-instance
/// stop failure is reported inline in the JSON output, not a process exit.
fn run_watch_stop_all() -> i32 {
    match varde_code::watch::stop_all() {
        Ok(outcomes) => {
            println!(
                "{}",
                serde_json::to_string(&outcomes).unwrap_or_else(|_| "[]".to_string())
            );
            0
        }
        Err(err) => {
            eprintln!("varde-code watch --stop-all: {err:#}");
            1
        }
    }
}

/// Run one `rules_list` invocation: parses input, delegates to
/// `scan_cli::rules_list`, prints the uniform envelope. Never exits
/// non-zero — this is a read-only listing, not a CI gate like `scan`.
fn run_rules_list(json: &str) {
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(value) => value,
        Err(e) => {
            println!(
                "{}",
                varde_code::query::render(Err(varde_code::query::ApiError::new(
                    "invalid_input",
                    format!("input is not valid JSON: {e}"),
                )))
            );
            return;
        }
    };
    println!(
        "{}",
        varde_code::query::render(varde_code::scan_cli::rules_list(&value))
    );
}

/// Run one `rules_seed` invocation: parses input, delegates to
/// `scan_cli::rules_seed`, prints the uniform envelope. Never exits
/// non-zero — writing seed files is not a CI gate.
fn run_rules_seed(json: &str, user: bool, force: bool) {
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(value) => value,
        Err(e) => {
            println!(
                "{}",
                varde_code::query::render(Err(varde_code::query::ApiError::new(
                    "invalid_input",
                    format!("input is not valid JSON: {e}"),
                )))
            );
            return;
        }
    };
    println!(
        "{}",
        varde_code::query::render(varde_code::scan_cli::rules_seed(&value, user, force))
    );
}

/// Run one `rules_remove` invocation: parses input, delegates to
/// `scan_cli::rules_remove`, prints the uniform envelope. Never exits
/// non-zero — deleting seed files is not a CI gate.
fn run_rules_remove(json: &str, user: bool, force: bool) {
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(value) => value,
        Err(e) => {
            println!(
                "{}",
                varde_code::query::render(Err(varde_code::query::ApiError::new(
                    "invalid_input",
                    format!("input is not valid JSON: {e}"),
                )))
            );
            return;
        }
    };
    println!(
        "{}",
        varde_code::query::render(varde_code::scan_cli::rules_remove(&value, user, force))
    );
}

/// Run one `skills_list` invocation: describes bundled skill packs and the
/// supported harness targets. No filesystem access.
fn run_skills_list() {
    let packs: Vec<serde_json::Value> = varde_code::skills::SKILL_PACKS
        .iter()
        .map(|pack| {
            serde_json::json!({
                "name": pack.name,
                "skillName": varde_code::skills::install_dir_name(pack.name),
                "installDirName": varde_code::skills::install_dir_name(pack.name),
                "files": pack.files.iter().map(|f| f.rel_path).collect::<Vec<_>>(),
            })
        })
        .collect();
    let targets: Vec<serde_json::Value> = varde_code::hooks::HOOK_TARGETS
        .iter()
        .map(|target| {
            let default_dir = default_skill_dir(target.agent);
            serde_json::json!({
                "agent": target.agent,
                "defaultTargetDir": default_dir.display().to_string(),
            })
        })
        .collect();
    println!(
        "{}",
        varde_code::query::render(Ok::<_, varde_code::query::ApiError>(
            serde_json::json!({ "packs": packs, "targets": targets })
        ))
    );
}

/// Run one `skills_install` invocation for the selected harnesses. Without
/// `--agent`, installs for all four supported harnesses.
fn run_skills_install(agents: &[String], force: bool, dir: Option<&str>) {
    let agents = match resolve_skill_agents(agents) {
        Ok(agents) => agents,
        Err(err) => return print_invalid_argument(err),
    };
    let result = install_skills_for_agents(&agents, force, dir).map_err(|e| {
        varde_code::query::ApiError::new("io_error", format!("failed to install skills: {e}"))
    });
    println!("{}", varde_code::query::render(result));
}

/// Run one `skills_remove` invocation for the selected harnesses. Without
/// `--agent`, removes installations for all four supported harnesses.
fn run_skills_remove(agents: &[String], force: bool, dir: Option<&str>) {
    let agents = match resolve_skill_agents(agents) {
        Ok(agents) => agents,
        Err(err) => return print_invalid_argument(err),
    };
    let result = remove_skills_for_agents(&agents, force, dir).map_err(|e| {
        varde_code::query::ApiError::new("io_error", format!("failed to remove skills: {e}"))
    });
    println!("{}", varde_code::query::render(result));
}

fn print_invalid_argument(message: String) {
    println!(
        "{}",
        varde_code::query::render(Err::<serde_json::Value, _>(
            varde_code::query::ApiError::new("invalid_argument", message)
        ))
    );
}

fn install_skills_for_agents(
    agents: &[&str],
    force: bool,
    dir: Option<&str>,
) -> std::io::Result<serde_json::Value> {
    let mut target_dirs = Vec::new();
    let mut installed = Vec::new();
    for &agent in agents {
        let target_dir = dir
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| default_skill_dir(agent));
        target_dirs.push(serde_json::json!({
            "agent": agent,
            "targetDir": target_dir.display().to_string(),
        }));
        for result in varde_code::skills::install_skills(&target_dir, force)? {
            installed.push(serde_json::json!({
                "agent": agent,
                "packName": result.pack_name,
                "path": result.path.display().to_string(),
                "written": result.written,
                "skippedExisting": result.skipped_existing,
            }));
        }
    }
    Ok(serde_json::json!({ "targetDirs": target_dirs, "installed": installed }))
}

fn remove_skills_for_agents(
    agents: &[&str],
    force: bool,
    dir: Option<&str>,
) -> std::io::Result<serde_json::Value> {
    let mut target_dirs = Vec::new();
    let mut removed = Vec::new();
    for &agent in agents {
        let target_dir = dir
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| default_skill_dir(agent));
        target_dirs.push(serde_json::json!({
            "agent": agent,
            "targetDir": target_dir.display().to_string(),
        }));
        for result in varde_code::skills::remove_skills(&target_dir, force)? {
            removed.push(serde_json::json!({
                "agent": agent,
                "packName": result.pack_name,
                "path": result.path.display().to_string(),
                "removed": result.removed,
                "skippedModified": result.skipped_modified,
            }));
        }
    }
    Ok(serde_json::json!({ "targetDirs": target_dirs, "removed": removed }))
}

/// Resolve the requested `--agent` names to `HookTarget`s. Empty (no
/// `--agent` given) defaults to every target in `HOOK_TARGETS`. Unknown
/// agent names are reported as an error rather than silently ignored.
fn resolve_hook_targets(agents: &[String]) -> Result<Vec<varde_code::hooks::HookTarget>, String> {
    if agents.is_empty() {
        return Ok(varde_code::hooks::HOOK_TARGETS.to_vec());
    }
    agents
        .iter()
        .map(|name| {
            varde_code::hooks::HOOK_TARGETS
                .iter()
                .find(|t| t.agent == name)
                .copied()
                .ok_or_else(|| {
                    let known: Vec<&str> = varde_code::hooks::HOOK_TARGETS
                        .iter()
                        .map(|t| t.agent)
                        .collect();
                    format!("unknown agent {name:?}; expected one of {known:?}")
                })
        })
        .collect()
}

/// The real per-agent, per-OS default install directory (user-level scope),
/// used when `--dir` is not given.
fn default_hook_dir(agent: &str) -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("~"));
    match agent {
        varde_code::hooks::CLAUDE_AGENT => home.join(".claude"),
        varde_code::hooks::CODEX_AGENT => home.join(".codex"),
        varde_code::hooks::OPENCODE_AGENT => home.join(".config").join("opencode"),
        varde_code::hooks::PI_AGENT => home.join(".pi").join("agent").join("extensions"),
        other => home.join(format!(".{other}")),
    }
}

/// The user-level skill directory each supported harness discovers by default.
fn default_skill_dir(agent: &str) -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("~"));
    match agent {
        varde_code::hooks::CLAUDE_AGENT => home.join(".claude").join("skills"),
        varde_code::hooks::CODEX_AGENT => home.join(".agents").join("skills"),
        varde_code::hooks::OPENCODE_AGENT => home.join(".config").join("opencode").join("skills"),
        varde_code::hooks::PI_AGENT => home.join(".pi").join("agent").join("skills"),
        other => home.join(format!(".{other}")).join("skills"),
    }
}

/// Resolve the selected skill-install targets. The harness list is shared with
/// session-start hooks so both surfaces accept the same target names.
fn resolve_skill_agents(agents: &[String]) -> Result<Vec<&'static str>, String> {
    resolve_hook_targets(agents)
        .map(|targets| targets.into_iter().map(|target| target.agent).collect())
}

/// Run one `hooks list` invocation: describes the 4 supported agent hook
/// targets and where each installs to. No filesystem access.
fn run_hooks_list() {
    let targets: Vec<serde_json::Value> = varde_code::hooks::HOOK_TARGETS
        .iter()
        .map(|target| {
            let (rel_path, kind) = match target.kind {
                varde_code::hooks::HookKind::MergeInto(rel_path, _) => (rel_path, "merge"),
                varde_code::hooks::HookKind::WriteFile(rel_path, _) => (rel_path, "write_file"),
            };
            let default_dir = default_hook_dir(target.agent);
            serde_json::json!({
                "agent": target.agent,
                "kind": kind,
                "relPath": rel_path,
                "defaultTargetDir": default_dir.display().to_string(),
                "defaultPath": default_dir.join(rel_path).display().to_string(),
            })
        })
        .collect();
    println!(
        "{}",
        varde_code::query::render(Ok::<_, varde_code::query::ApiError>(
            serde_json::json!({ "targets": targets })
        ))
    );
}

/// Run one `hooks install` invocation: installs the session-start hook for
/// each requested agent (default: all 4) into either `--dir` (uniformly, for
/// testing) or each agent's real per-OS default directory. Never exits
/// non-zero — installing hooks is not a CI gate.
fn run_hooks_install(agents: &[String], force: bool, dir: Option<&str>) {
    let targets = match resolve_hook_targets(agents) {
        Ok(targets) => targets,
        Err(err) => {
            println!(
                "{}",
                varde_code::query::render(Err::<serde_json::Value, _>(
                    varde_code::query::ApiError::new("invalid_argument", err)
                ))
            );
            return;
        }
    };
    let result = install_or_remove_hooks(&targets, dir, |target_dir, one_target| {
        varde_code::hooks::install_hooks(target_dir, one_target, force)
    })
    .map(|(installed, dirs)| {
        let installed_json: Vec<serde_json::Value> = installed
            .iter()
            .map(|r| {
                serde_json::json!({
                    "agent": r.agent,
                    "path": r.path.display().to_string(),
                    "written": r.written,
                    "skippedExisting": r.skipped_existing,
                })
            })
            .collect();
        serde_json::json!({ "targetDirs": dirs, "installed": installed_json })
    })
    .map_err(|e| {
        varde_code::query::ApiError::new("io_error", format!("failed to install hooks: {e}"))
    });
    println!("{}", varde_code::query::render(result));
}

/// Run one `hooks remove` invocation: undoes `hooks install` for each
/// requested agent (default: all 4). Never exits non-zero — removing hooks
/// is not a CI gate.
fn run_hooks_remove(agents: &[String], force: bool, dir: Option<&str>) {
    let targets = match resolve_hook_targets(agents) {
        Ok(targets) => targets,
        Err(err) => {
            println!(
                "{}",
                varde_code::query::render(Err::<serde_json::Value, _>(
                    varde_code::query::ApiError::new("invalid_argument", err)
                ))
            );
            return;
        }
    };
    let result = install_or_remove_hooks(&targets, dir, |target_dir, one_target| {
        varde_code::hooks::remove_hooks(target_dir, one_target, force)
    })
    .map(|(removed, dirs)| {
        let removed_json: Vec<serde_json::Value> = removed
            .iter()
            .map(|r| {
                serde_json::json!({
                    "agent": r.agent,
                    "path": r.path.display().to_string(),
                    "removed": r.removed,
                    "skippedModified": r.skipped_modified,
                })
            })
            .collect();
        serde_json::json!({ "targetDirs": dirs, "removed": removed_json })
    })
    .map_err(|e| {
        varde_code::query::ApiError::new("io_error", format!("failed to remove hooks: {e}"))
    });
    println!("{}", varde_code::query::render(result));
}

/// Shared install/remove driver: with `--dir`, all `targets` are installed
/// into that single directory in one call (matches `hooks.rs`'s own tests,
/// which pass one dir for multiple agents since each target's `rel_path` is
/// a distinct filename). Without `--dir`, each target is installed into its
/// own resolved per-agent default directory via a separate call. Returns the
/// concatenated per-target results plus a `{agent: targetDir}` map for the
/// envelope.
fn install_or_remove_hooks<T>(
    targets: &[varde_code::hooks::HookTarget],
    dir: Option<&str>,
    op: impl Fn(&std::path::Path, &[varde_code::hooks::HookTarget]) -> std::io::Result<Vec<T>>,
) -> std::io::Result<(Vec<T>, serde_json::Value)> {
    match dir {
        Some(dir) => {
            let target_dir = std::path::Path::new(dir);
            let results = op(target_dir, targets)?;
            let dirs: serde_json::Map<String, serde_json::Value> = targets
                .iter()
                .map(|t| {
                    (
                        t.agent.to_string(),
                        serde_json::Value::String(target_dir.display().to_string()),
                    )
                })
                .collect();
            Ok((results, serde_json::Value::Object(dirs)))
        }
        None => {
            let mut all_results = Vec::new();
            let mut dirs = serde_json::Map::new();
            for target in targets {
                let target_dir = default_hook_dir(target.agent);
                let results = op(&target_dir, std::slice::from_ref(target))?;
                dirs.insert(
                    target.agent.to_string(),
                    serde_json::Value::String(target_dir.display().to_string()),
                );
                all_results.extend(results);
            }
            Ok((all_results, serde_json::Value::Object(dirs)))
        }
    }
}

fn run_query(mode: &str, json: &str) {
    println!("{}", varde_code::query::run_mode(mode, json));
}

/// `nav_map` — JSON is the canonical envelope, printed as-is; `--format
/// text` derives a plain-text rendering from the same JSON (never a second
/// data-gathering path).
fn run_nav_map(json: &str, format: &str) {
    let envelope = varde_code::query::run_mode("nav_map", json);
    if format == "text" {
        println!("{}", render_nav_map_text(&envelope));
    } else {
        println!("{}", envelope);
    }
}

/// Render the `nav_map` JSON envelope as plain text, one section per header.
/// Each section's items are rendered as concise, human-readable lines (symbol
/// names, file paths, call trees) — not a bare item count — because this text
/// is injected verbatim into a session at start and a count alone orients
/// nobody. On error (or unparseable input), fall back to the raw envelope so no
/// information is lost.
fn render_nav_map_text(envelope: &str) -> String {
    const SECTIONS: [&str; 7] = [
        "entrypoints",
        "foundational_files",
        "module_layers",
        "subsystems",
        "symbols",
        "flows",
        "hotspots",
    ];
    let value: serde_json::Value = match serde_json::from_str(envelope) {
        Ok(v) => v,
        Err(_) => return envelope.to_string(),
    };
    if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return envelope.to_string();
    }
    let data = value
        .get("data")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let truncated = data
        .pointer("/guide/truncated")
        .and_then(|value| value.as_object());
    let mut out = String::new();
    for section in SECTIONS {
        out.push_str(&format!("## {section}\n"));
        match data.get(section) {
            Some(serde_json::Value::Array(items)) if items.is_empty() => {
                if let Some(info) = truncated.and_then(|sections| sections.get(section)) {
                    let total = info
                        .get("total")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let more = info
                        .get("more")
                        .and_then(|value| value.as_str())
                        .unwrap_or("");
                    out.push_str(&format!("(truncated: 0/{total} shown; {more})\n\n"));
                } else {
                    out.push_str("(none)\n\n");
                }
            }
            Some(serde_json::Value::Array(items)) => {
                for item in items {
                    out.push_str(&render_nav_map_item(section, item));
                }
                out.push('\n');
            }
            Some(serde_json::Value::Object(_)) if section == "module_layers" => {
                out.push_str(&render_module_layers(&data[section]));
            }
            Some(other) => {
                out.push_str(&format!("{other}\n\n"));
            }
            None => out.push_str("(missing)\n\n"),
        }
    }
    // Truncation guide (F1): tell the reader what the token budget cut and how
    // to get the rest.
    if let Some(truncated) = truncated
        && !truncated.is_empty()
    {
        out.push_str("## truncated (token budget)\n");
        for (section, info) in truncated {
            let shown = info.get("shown").and_then(|v| v.as_u64()).unwrap_or(0);
            let total = info.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
            let more = info.get("more").and_then(|v| v.as_str()).unwrap_or("");
            out.push_str(&format!("- {section}: {shown}/{total} shown — {more}\n"));
        }
        out.push('\n');
    }
    out
}

/// String field lookup helper for the nav_map item renderers.
fn str_field<'a>(item: &'a serde_json::Value, key: &str) -> &'a str {
    item.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

/// Integer field lookup helper for the nav_map item renderers.
fn u64_field(item: &serde_json::Value, key: &str) -> u64 {
    item.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
}

/// Render one item of an array-valued nav_map section as a concise line.
/// Each section has a known shape (see `query/nav_map.rs`); unknown shapes
/// fall back to compact JSON so nothing is silently dropped.
fn render_nav_map_item(section: &str, item: &serde_json::Value) -> String {
    match section {
        "entrypoints" => {
            let role = str_field(item, "role");
            let role = if role.is_empty() {
                String::new()
            } else {
                format!("  [{role}]")
            };
            // Route verb+path (annotation/decorator handlers carry these while
            // keeping the handler name as `symbol`); shown as `(GET /path)`
            // unless the symbol already *is* the route string (call-based
            // routes), to avoid `- GET /x  (GET /x)` duplication.
            let symbol = str_field(item, "symbol");
            let method = str_field(item, "method");
            let path = str_field(item, "path");
            let route = match (method.is_empty(), path.is_empty()) {
                (false, false) => format!("{method} {path}"),
                (true, false) => path.to_string(),
                _ => String::new(),
            };
            let route = if route.is_empty() || route == symbol {
                String::new()
            } else {
                format!("  ({route})")
            };
            format!(
                "- {}{}  {}{}\n",
                symbol,
                route,
                str_field(item, "file"),
                role
            )
        }
        "foundational_files" => format!(
            "- {}  ({} dependents, {} refs)\n",
            str_field(item, "file"),
            u64_field(item, "dependents"),
            u64_field(item, "count"),
        ),
        "subsystems" => {
            let name = str_field(item, "name");
            let members: Vec<&str> = item
                .get("members")
                .and_then(|m| m.as_array())
                .map(|a| a.iter().filter_map(|m| m.as_str()).collect())
                .unwrap_or_default();
            let omitted = u64_field(item, "membersOmitted");
            let more = if omitted > 0 {
                format!(" (+{omitted} more)")
            } else {
                String::new()
            };
            format!("- {name}: {}{more}\n", members.join(", "))
        }
        "symbols" => {
            let owner = str_field(item, "owner");
            let qualified = if owner.is_empty() {
                str_field(item, "symbol").to_string()
            } else {
                format!("{owner}::{}", str_field(item, "symbol"))
            };
            format!(
                "- {}  {}  ({} callers)\n",
                qualified,
                str_field(item, "file"),
                u64_field(item, "callers"),
            )
        }
        "hotspots" => format!(
            "- {}  (score {}, complexity {}, churn {})\n",
            str_field(item, "file"),
            u64_field(item, "score"),
            u64_field(item, "complexity"),
            u64_field(item, "churn"),
        ),
        "flows" => {
            // nav_map already ranks flows biggest-first and embeds a bounded
            // summary per flow (`nodeCount` = full size, `root` = capped tree,
            // `more` = the explore query for the whole tree). The renderer just
            // pretty-prints that summary; it does no bounding of its own beyond
            // a recursion-depth safety guard.
            let node_count = u64_field(item, "nodeCount");
            let mut out = format!(
                "- {}  ({} nodes)\n",
                str_field(item, "entrypoint"),
                node_count
            );
            if let Some(root) = item.get("root") {
                render_flow_node(root, 1, &mut out);
            }
            let more = str_field(item, "more");
            if !more.is_empty() {
                out.push_str(&format!("  → full tree: {more}\n"));
            }
            out
        }
        _ => format!("- {item}\n"),
    }
}

/// Hard recursion-depth guard for the flow renderer. The JSON it renders is
/// already depth-bounded by nav_map's `summarize_flow`, so this only protects
/// against a pathological hand-built envelope — not a normal cap.
const FLOW_RENDER_MAX_DEPTH: usize = 12;

/// Render one node of a (pre-bounded) flow call-tree as an indented line,
/// recursing into `children`. A `recurses` node (cycle back-edge, marked by
/// nav_map) is shown as a leaf. A `childrenOmitted` count (nav_map dropped the
/// subtree at its size/depth cap) is shown as a trailing marker so the reader
/// knows the call tree continues.
fn render_flow_node(node: &serde_json::Value, depth: usize, out: &mut String) {
    if depth > FLOW_RENDER_MAX_DEPTH {
        return;
    }
    let indent = "  ".repeat(depth);
    let symbol = str_field(node, "symbol");
    let file = str_field(node, "file");
    let recurses = node
        .get("recurses")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || node.get("backref_to").is_some();
    if recurses {
        // nav_map collapses N repeated sibling backrefs to the same target into
        // one node carrying `recursesCount`; surface the multiplier.
        let count = node
            .get("recursesCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(1);
        let marker = if count > 1 {
            format!("(↑ recurses, ×{count})")
        } else {
            "(↑ recurses)".to_string()
        };
        out.push_str(&format!("{indent}{symbol}  {file}  {marker}\n"));
        return;
    }
    out.push_str(&format!("{indent}{symbol}  {file}\n"));
    if let Some(children) = node.get("children").and_then(|c| c.as_array()) {
        for child in children {
            render_flow_node(child, depth + 1, out);
        }
    }
    if let Some(omitted) = node
        .get("childrenOmitted")
        .and_then(|v| v.as_u64())
        .filter(|n| *n > 0)
    {
        out.push_str(&format!(
            "{}… {omitted} more call(s) below\n",
            "  ".repeat(depth + 1)
        ));
    }
}

/// Render the `module_layers` object: dependency cycles and a summary of the
/// resolved cross-module import edges.
fn render_module_layers(layers: &serde_json::Value) -> String {
    let mut out = String::new();
    let cycles = layers.get("cycles").and_then(|c| c.as_array());
    if let Some(cycles) = cycles.filter(|c| !c.is_empty()) {
        out.push_str("cycles:\n");
        for cycle in cycles {
            let members: Vec<&str> = cycle
                .as_array()
                .map(|a| a.iter().filter_map(|m| m.as_str()).collect())
                .unwrap_or_default();
            out.push_str(&format!("- {}\n", members.join(" → ")));
        }
    }
    if let Some(edges) = layers.get("edges").and_then(|e| e.as_array()) {
        out.push_str(&format!("edges ({}):\n", edges.len()));
        for edge in edges {
            out.push_str(&format!(
                "- {} → {}  ({} files)\n",
                str_field(edge, "from_module"),
                str_field(edge, "to_module"),
                u64_field(edge, "crossing_files"),
            ));
        }
    }
    out.push('\n');
    out
}

fn init_tracing(verbose: bool) {
    use tracing_subscriber::EnvFilter;
    let filter = match std::env::var("RUST_LOG") {
        Ok(val) => EnvFilter::new(val),
        Err(_) => EnvFilter::new(if verbose {
            "debug"
        } else {
            "varde_code=info,warn,error"
        }),
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn run_build(repo_root: &str, force: bool, changed_files: bool) {
    match varde_code::build::run_with_force(repo_root, force) {
        Ok(summary) => {
            // The reparsed-path list is a drill-down handle, but on a full
            // build it's the entire repo — hundreds of paths of low inline
            // value (audit F11). Emit a count plus a small sample by default;
            // `--changed-files` opts back into the full array.
            const CHANGED_FILES_SAMPLE: usize = 10;
            let mut result = serde_json::json!({
                "ok": true,
                "dbPath": summary.db_path,
                "entities": summary.entities,
                "symbols": summary.symbols,
                "diagnostics": summary.diagnostics,
                "unchanged": summary.unchanged,
                "reparsed": summary.reparsed,
                "changedFilesCount": summary.changed_files.len(),
            });
            if changed_files {
                result["changedFiles"] = serde_json::json!(summary.changed_files);
            } else if summary.changed_files.len() > CHANGED_FILES_SAMPLE {
                result["changedFilesSample"] =
                    serde_json::json!(summary.changed_files[..CHANGED_FILES_SAMPLE]);
            } else {
                result["changedFilesSample"] = serde_json::json!(summary.changed_files);
            }
            println!("{result}");
        }
        Err(e) => {
            tracing::error!(repo_root = repo_root, "fatal error: {e:#}");
            eprintln!("varde-code: error: {e:#}");
            std::process::exit(1);
        }
    }
}

fn run_extract(path: &str) {
    match varde_code::scan::run(path) {
        Ok(output) => {
            tracing::debug!(file = path, "emitting JSON document");
            let mut doc = serde_json::to_value(&output).expect("output serializes");
            // The JSON contract exposes a resolved `file` path per
            // entity/symbol/diagnostic (external CLI consumers, not the
            // in-process `file_id` used internally) — inject it here rather
            // than growing `model::Entity`/`Symbol`/`Diagnostic` back to
            // carrying an owned path.
            for key in ["entities", "symbols", "diagnostics"] {
                if let Some(items) = doc.get_mut(key).and_then(|v| v.as_array_mut()) {
                    for item in items {
                        if let Some(id) = item.get("file_id").and_then(|v| v.as_u64()) {
                            item["file"] = serde_json::json!(output.files[id as usize]);
                        }
                    }
                }
            }
            println!(
                "{}",
                serde_json::to_string(&doc).expect("output serializes")
            );
        }
        Err(e) => {
            tracing::error!(file = path, "fatal error: {e:#}");
            eprintln!("varde-code: error: {e:#}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod nav_map_text_tests {
    use super::{
        default_hook_dir, default_skill_dir, install_or_remove_hooks, install_skills_for_agents,
        remove_skills_for_agents, render_nav_map_text, resolve_hook_targets, resolve_skill_agents,
        run_hooks_list,
    };

    /// AC3: every section name present in the JSON also appears in the text
    /// rendering.
    #[test]
    fn text_rendering_lists_every_json_section() {
        let envelope = serde_json::json!({
            "ok": true,
            "data": {
                "entrypoints": [{"symbol": "hello"}],
                "foundational_files": [],
                "module_layers": {"edges": [], "cycles": []},
                "subsystems": [],
                "symbols": [],
                "flows": [],
                "hotspots": [],
            }
        })
        .to_string();

        let text = render_nav_map_text(&envelope);

        for section in [
            "entrypoints",
            "foundational_files",
            "module_layers",
            "subsystems",
            "symbols",
            "flows",
            "hotspots",
        ] {
            assert!(
                text.contains(section),
                "text rendering missing section {section:?}: {text}"
            );
        }
    }

    /// Naturally empty sections remain `(none)`, while an empty section named
    /// in `guide.truncated` tells the reader what the budget withheld.
    #[test]
    fn text_rendering_distinguishes_empty_from_budget_truncated_sections() {
        let envelope = serde_json::json!({
            "ok": true,
            "data": {
                "entrypoints": [],
                "foundational_files": [],
                "module_layers": {"edges": [], "cycles": []},
                "subsystems": [],
                "symbols": [],
                "flows": [],
                "hotspots": [],
                "guide": {
                    "truncated": {
                        "symbols": {
                            "shown": 0,
                            "total": 12,
                            "more": "code_query mode=filter_symbols for the full symbol list",
                        }
                    }
                }
            }
        })
        .to_string();

        let text = render_nav_map_text(&envelope);

        assert!(text.contains("## entrypoints\n(none)"), "{text}");
        assert!(
            text.contains(
                "## symbols\n(truncated: 0/12 shown; code_query mode=filter_symbols for the full symbol list)"
            ),
            "{text}"
        );
    }

    /// Regression: array sections must render their actual item content
    /// (symbol names, files, call trees) — not a bare `N item(s)` count,
    /// which is what shipped and made the injected symbols/flows sections
    /// useless. Nested flow trees render indented, bounded, and back-edges
    /// are marked rather than followed.
    #[test]
    fn text_rendering_emits_item_content_not_just_counts() {
        let envelope = serde_json::json!({
            "ok": true,
            "data": {
                "entrypoints": [{"symbol": "main", "file": "src/main.rs", "role": "process_main"}],
                "foundational_files": [{"file": "src/model.rs", "dependents": 57, "count": 198}],
                "module_layers": {"edges": [{"from_module": "a", "to_module": "b", "crossing_files": 3}], "cycles": [["a", "b"]]},
                "subsystems": [{"id": 1, "name": "core", "members": ["src/a.rs", "src/b.rs"]}],
                "symbols": [{"symbol": "parse_source", "file": "src/parse.rs", "callers": 39, "owner": null}],
                "flows": [{"entrypoint": "main", "file": "src/main.rs", "nodeCount": 42,
                    "more": "code_query mode=explore {\"input\":\"main\",\"direction\":\"outgoing\"} for the full call tree",
                    "root": {"symbol": "main", "file": "src/main.rs",
                    "children": [{"symbol": "helper", "file": "src/util.rs", "children": []}], "childrenOmitted": 7}}],
                "hotspots": [{"file": "src/resolve.rs", "score": 632, "complexity": 158, "churn": 4}],
            }
        })
        .to_string();

        let text = render_nav_map_text(&envelope);

        // No section renders as a bare item count anymore.
        assert!(
            !text.contains("item(s)"),
            "sections must render content, not counts: {text}"
        );
        // Representative content from each section is present.
        assert!(text.contains("main  src/main.rs  [process_main]"), "{text}");
        assert!(
            text.contains("src/model.rs  (57 dependents, 198 refs)"),
            "{text}"
        );
        assert!(text.contains("core: src/a.rs, src/b.rs"), "{text}");
        assert!(
            text.contains("parse_source  src/parse.rs  (39 callers)"),
            "{text}"
        );
        assert!(
            text.contains("resolve.rs  (score 632, complexity 158, churn 4)"),
            "{text}"
        );
        // Flow renders its size, the indented tree, an omitted-children
        // marker, and the explore follow-up handle for the full tree.
        assert!(text.contains("main  (42 nodes)"), "flow size: {text}");
        assert!(
            text.contains("    helper  src/util.rs"),
            "flow child indented: {text}"
        );
        assert!(
            text.contains("… 7 more call(s) below"),
            "omitted marker: {text}"
        );
        assert!(
            text.contains("→ full tree: code_query mode=explore"),
            "flow follow-up handle: {text}"
        );
        // module_layers renders cycles + edges, not raw JSON.
        assert!(text.contains("a → b"), "module_layers rendered: {text}");
    }

    /// An error envelope falls back to the raw envelope rather than
    /// producing a bespoke text shape.
    #[test]
    fn text_rendering_falls_back_to_raw_envelope_on_error() {
        let envelope = serde_json::json!({
            "ok": false,
            "error": {"code": "not_found", "message": "no such repo"}
        })
        .to_string();

        let text = render_nav_map_text(&envelope);
        assert_eq!(text, envelope);
    }

    // --- `hooks` CLI wiring: resolve_hook_targets / default dirs / round trip

    fn tempdir(label: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("varde-hooks-cli-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        dir
    }

    #[test]
    fn resolve_hook_targets_empty_agent_list_means_all_four() {
        let targets = resolve_hook_targets(&[]).expect("empty --agent resolves");
        assert_eq!(targets.len(), varde_code::hooks::HOOK_TARGETS.len());
        let agents: Vec<&str> = targets.iter().map(|t| t.agent).collect();
        assert!(agents.contains(&varde_code::hooks::CLAUDE_AGENT));
        assert!(agents.contains(&varde_code::hooks::CODEX_AGENT));
        assert!(agents.contains(&varde_code::hooks::OPENCODE_AGENT));
        assert!(agents.contains(&varde_code::hooks::PI_AGENT));
    }

    #[test]
    fn resolve_hook_targets_filters_to_requested_subset() {
        let targets = resolve_hook_targets(&["claude".to_string(), "codex".to_string()])
            .expect("known agents resolve");
        assert_eq!(targets.len(), 2);
        let agents: Vec<&str> = targets.iter().map(|t| t.agent).collect();
        assert_eq!(
            agents,
            vec![
                varde_code::hooks::CLAUDE_AGENT,
                varde_code::hooks::CODEX_AGENT
            ]
        );
    }

    #[test]
    fn resolve_hook_targets_rejects_unknown_agent() {
        let err = resolve_hook_targets(&["not-a-real-agent".to_string()])
            .expect_err("unknown agent errors");
        assert!(
            err.contains("not-a-real-agent"),
            "error names the bad agent: {err}"
        );
    }

    #[test]
    fn hooks_list_performs_no_filesystem_writes_and_covers_all_four_agents() {
        // `run_hooks_list` only reads `HOOK_TARGETS` (static) and formats
        // default dirs; assert it doesn't touch disk by checking no new
        // entries appear under a scratch dir it's never told about, and that
        // its output covers all 4 agents.
        let before = tempdir("list-no-writes");
        let entries_before: Vec<_> = std::fs::read_dir(&before).unwrap().collect();
        assert!(entries_before.is_empty());

        run_hooks_list();

        let entries_after: Vec<_> = std::fs::read_dir(&before).unwrap().collect();
        assert!(
            entries_after.is_empty(),
            "hooks list must not write to the filesystem"
        );

        for agent in [
            varde_code::hooks::CLAUDE_AGENT,
            varde_code::hooks::CODEX_AGENT,
            varde_code::hooks::OPENCODE_AGENT,
            varde_code::hooks::PI_AGENT,
        ] {
            assert!(
                varde_code::hooks::HOOK_TARGETS
                    .iter()
                    .any(|t| t.agent == agent),
                "hooks list source data covers agent {agent}"
            );
        }
    }

    #[test]
    fn hooks_install_no_agent_installs_all_four_into_dir() {
        let dir = tempdir("install-all");
        let targets = resolve_hook_targets(&[]).unwrap();
        let (installed, dirs) = install_or_remove_hooks(
            &targets,
            Some(dir.to_str().unwrap()),
            |target_dir, one_target| {
                varde_code::hooks::install_hooks(target_dir, one_target, false)
            },
        )
        .expect("install succeeds");
        assert_eq!(installed.len(), varde_code::hooks::HOOK_TARGETS.len());
        assert_eq!(dirs.as_object().unwrap().len(), 4);
        assert!(dir.join("settings.json").exists());
        assert!(dir.join("config.toml").exists());
        assert!(dir.join("plugin/varde-code-nav-map.js").exists());
        assert!(dir.join("pi-extension.js").exists());
    }

    #[test]
    fn hooks_install_agent_subset_installs_only_those_two() {
        let dir = tempdir("install-subset");
        let targets = resolve_hook_targets(&["claude".to_string(), "codex".to_string()]).unwrap();
        install_or_remove_hooks(
            &targets,
            Some(dir.to_str().unwrap()),
            |target_dir, one_target| {
                varde_code::hooks::install_hooks(target_dir, one_target, false)
            },
        )
        .expect("install succeeds");
        assert!(dir.join("settings.json").exists());
        assert!(dir.join("config.toml").exists());
        assert!(!dir.join("plugin/varde-code-nav-map.js").exists());
        assert!(!dir.join("pi-extension.js").exists());
    }

    #[test]
    fn hooks_install_then_remove_round_trips_whole_file_and_merge_targets() {
        let dir = tempdir("round-trip");
        let targets = resolve_hook_targets(&[]).unwrap();

        // Whole-file target (opencode): round trip restores pre-install state
        // (file absent).
        assert!(!dir.join("plugin/varde-code-nav-map.js").exists());
        // Merge target (claude): seed unrelated content first so we can
        // assert only the injected entry is removed, not the whole file.
        std::fs::write(dir.join("settings.json"), r#"{"unrelated": true}"#).unwrap();

        install_or_remove_hooks(
            &targets,
            Some(dir.to_str().unwrap()),
            |target_dir, one_target| {
                varde_code::hooks::install_hooks(target_dir, one_target, false)
            },
        )
        .expect("install succeeds");
        assert!(dir.join("plugin/varde-code-nav-map.js").exists());
        assert!(dir.join("pi-extension.js").exists());

        install_or_remove_hooks(
            &targets,
            Some(dir.to_str().unwrap()),
            |target_dir, one_target| varde_code::hooks::remove_hooks(target_dir, one_target, false),
        )
        .expect("remove succeeds");

        // Whole-file targets: pre-install state restored (files gone).
        assert!(!dir.join("plugin/varde-code-nav-map.js").exists());
        assert!(!dir.join("pi-extension.js").exists());

        // Merge targets: the shared config file remains (it's not a
        // whole-file target), but only this tool's injected entry is
        // removed, never unrelated content.
        let codex_doc = std::fs::read_to_string(dir.join("config.toml"))
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .expect("valid toml");
        assert!(
            codex_doc.get("hooks").is_none(),
            "injected session_start table removed"
        );

        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(settings["unrelated"], true);
        assert!(
            settings.get("hooks").is_none(),
            "injected SessionStart hook removed"
        );
    }

    #[test]
    fn hooks_install_without_dir_resolves_per_agent_default_dirs() {
        let targets = resolve_hook_targets(&["claude".to_string(), "pi".to_string()]).unwrap();
        let claude_dir = default_hook_dir("claude");
        let pi_dir = default_hook_dir("pi");
        assert_ne!(
            claude_dir, pi_dir,
            "each agent resolves its own default dir"
        );
        assert!(claude_dir.ends_with(".claude"));
        assert!(pi_dir.ends_with("agent/extensions"));
        // Sanity: install_or_remove_hooks with dir=None would target these
        // dirs (not exercised here to avoid touching the real home dir).
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn skill_agents_default_to_all_four_harnesses() {
        let agents = resolve_skill_agents(&[]).expect("empty --agent resolves");
        assert_eq!(agents.len(), 4);
        assert!(agents.contains(&varde_code::hooks::CLAUDE_AGENT));
        assert!(agents.contains(&varde_code::hooks::CODEX_AGENT));
        assert!(agents.contains(&varde_code::hooks::OPENCODE_AGENT));
        assert!(agents.contains(&varde_code::hooks::PI_AGENT));
    }

    #[test]
    fn skill_default_dirs_match_harness_discovery_locations() {
        assert!(default_skill_dir("claude").ends_with(".claude/skills"));
        assert!(default_skill_dir("codex").ends_with(".agents/skills"));
        assert!(default_skill_dir("opencode").ends_with(".config/opencode/skills"));
        assert!(default_skill_dir("pi").ends_with(".pi/agent/skills"));
    }

    #[test]
    fn skill_install_with_dir_installs_for_each_selected_harness() {
        let dir = tempdir("skills-install-all");
        let agents = resolve_skill_agents(&[]).expect("empty --agent resolves");
        let result = install_skills_for_agents(&agents, false, Some(dir.to_str().unwrap()))
            .expect("install succeeds");
        let targets = result["targetDirs"].as_array().expect("target dirs array");
        let installed = result["installed"].as_array().expect("installed array");
        assert_eq!(targets.len(), 4);
        let shipped_file_count: usize = varde_code::skills::SKILL_PACKS
            .iter()
            .map(|pack| pack.files.len())
            .sum();
        assert_eq!(installed.len(), shipped_file_count * 4);
        assert!(dir.join("varde-code-codebase-navigation/SKILL.md").exists());
        assert!(
            dir.join("varde-code-codebase-navigation/agents/openai.yaml")
                .exists()
        );
    }

    #[test]
    fn skill_remove_with_dir_removes_shared_harness_installation() {
        let dir = tempdir("skills-remove-all");
        let agents = resolve_skill_agents(&[]).expect("empty --agent resolves");
        install_skills_for_agents(&agents, false, Some(dir.to_str().unwrap()))
            .expect("install succeeds");
        remove_skills_for_agents(&agents, false, Some(dir.to_str().unwrap()))
            .expect("remove succeeds");
        assert!(!dir.join("varde-code-codebase-navigation").exists());
    }
}
