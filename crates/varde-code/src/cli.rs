//! CLI surface: one subcommand per query mode + `extract`.
//!
//! Query subcommands share the `--json '<object>'` convention: a single JSON
//! object argument carrying `repoRoot` (or `dbPath`) plus mode-specific
//! fields. Every query subcommand prints the uniform envelope
//! (`{"ok":true,"data":...}` / `{"ok":false,"error":{...}}`) to stdout.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "varde-code",
    version,
    about,
    long_about = "varde-code: native code-intelligence CLI (parse, index, query, scan).\n\
\n\
Typical flow:\n\
  1. varde-code build --repo-root <path>        # index the repo (required before query/scan)\n\
  2. varde-code <query-mode> --json '{...}'      # e.g. symbols_in_file, dependencies, hotspots\n\
  3. varde-code scan --json '{\"repoRoot\":\"<path>\"}'  # run rule packs, exit non-zero above threshold\n\
\n\
Scan rules (built-in + user + repo scope):\n\
  varde-code rules_list --json '{\"repoRoot\":\"<path>\"}'   # see what would run, with override provenance\n\
  varde-code rules_seed --json '{\"repoRoot\":\"<path>\"}'   # copy built-ins to .varde-code/rules/ for editing\n\
  varde-code rules_remove --json '{\"repoRoot\":\"<path>\"}' # undo a seed (skips locally-edited files)\n\
  Add --user to rules_seed/rules_remove to target ~/.config/varde-code/rules/ instead.\n\
\n\
Agent skills (rule authoring, rule-scan triage, codebase navigation):\n\
  varde-code skills_list                                    # list packs and harness targets\n\
  varde-code skills_install [--agent <harness>]             # install for Claude, Codex, OpenCode, or Pi\n\
  varde-code skills_remove [--agent <harness>]              # undo an install (skips locally-edited files)\n\
  --agent is repeatable or comma-separated; omitting it targets all harnesses.\n\
\n\
Run `varde-code <subcommand> --help` for a subcommand's full input shape."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Enable verbose logging (only applies when RUST_LOG is unset).
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Extract entities and symbols from a file or directory tree.
    Extract {
        /// Path to a file or directory to extract from.
        path: String,
    },
    /// Extract, resolve, and persist a repo's index so the query subcommands
    /// (symbols_in_file, find_pattern's DB-backed peers, etc.) can read it.
    /// Full rebuild every run — writes to
    /// `~/.config/varde-code/repos/<name>-<hash>/index.db`.
    Build {
        /// Path to the repo root to index.
        #[arg(long, alias = "repo-root")]
        repo_root: String,
        /// Skip incremental detection and force a full rebuild.
        #[arg(long)]
        force: bool,
        /// Include the full list of reparsed file paths in the JSON result.
        /// Off by default: the result carries `changedFilesCount` and a small
        /// `changedFilesSample` instead, so a large rebuild doesn't dump
        /// hundreds of paths an agent rarely needs inline.
        #[arg(long)]
        changed_files: bool,
    },
    /// Query: run several query modes in one call.
    /// (see `batch` help for inputs)
    #[command(name = "batch")]
    Batch {
        /// JSON object: { repoRoot|dbPath, calls: [{ mode, ... }, ...] }
        #[arg(long)]
        json: String,
    },
    /// Query: list symbols declared in a file.
    /// (see `symbols_in_file` help for inputs)
    #[command(name = "symbols_in_file")]
    SymbolsInFile {
        /// JSON object: { repoRoot|dbPath, filePath, includeBody? }
        #[arg(long)]
        json: String,
    },
    /// Query: batch form of `symbols_in_file` over many files in one call.
    /// (see `symbols_in_files` help for inputs)
    #[command(name = "symbols_in_files")]
    SymbolsInFiles {
        /// JSON object: { repoRoot|dbPath, filePaths, includeBody? }
        #[arg(long)]
        json: String,
    },
    /// Query: get one symbol by name (+ optional file/kind).
    /// (see `get_symbol` help for inputs)
    #[command(name = "get_symbol")]
    GetSymbol {
        /// JSON object: { repoRoot|dbPath, name, filePath?, kind?, includeBody? }
        #[arg(long)]
        json: String,
    },
    /// Query: files that import or otherwise depend on a file.
    /// (see `dependencies` help for inputs)
    #[command(name = "dependencies")]
    Dependencies {
        /// JSON object: { repoRoot|dbPath, filePath, direction?, maxDepth? }
        #[arg(long)]
        json: String,
    },
    /// Query: files that depend on the given file.
    /// (see `dependents` help for inputs)
    #[command(name = "dependents")]
    Dependents {
        /// JSON object: { repoRoot|dbPath, filePath, maxDepth? }
        #[arg(long)]
        json: String,
    },
    /// Query: test files covering a given file.
    /// (see `tests_for_file` help for inputs)
    #[command(name = "tests_for_file")]
    TestsForFile {
        /// JSON object: { repoRoot|dbPath, filePath }
        #[arg(long)]
        json: String,
    },
    /// Query: risk hotspots ranked by complexity + churn.
    /// (see `hotspots` help for inputs)
    #[command(name = "hotspots")]
    Hotspots {
        /// JSON object: { repoRoot|dbPath }
        #[arg(long)]
        json: String,
    },
    /// Query: community-detection partition of the resolution graph into
    /// densely-interconnected file clusters. (see `clusters` help for inputs)
    #[command(name = "clusters")]
    Clusters {
        /// JSON object: { repoRoot|dbPath, minSize?, maxClusters?, seedPath? }
        #[arg(long)]
        json: String,
    },
    /// Query: map a file to its persisted node info.
    /// (see `map_file` help for inputs)
    #[command(name = "map_file")]
    MapFile {
        /// JSON object: { repoRoot|dbPath, filePath }
        #[arg(long)]
        json: String,
    },
    /// Query: map a symbol to its persisted entity.
    /// (see `map_symbol` help for inputs)
    #[command(name = "map_symbol")]
    MapSymbol {
        /// JSON object: { repoRoot|dbPath, name, sourceFile? }
        #[arg(long)]
        json: String,
    },
    /// Query: map a dependency path between two files.
    /// (see `map_path` help for inputs)
    #[command(name = "map_path")]
    MapPath {
        /// JSON object: { repoRoot|dbPath, sourceFile, targetFile, maxDepth? }
        #[arg(long)]
        json: String,
    },
    /// Query: explore the dependency graph from a seed file or symbol name.
    /// `query.params.input` resolves as a file path first, falling back to a
    /// symbol-name match (exact, then substring) when no file matches;
    /// `query.params.direction` is `outgoing` (default), `incoming`, or
    /// `both`. (see `explore` help for inputs)
    #[command(name = "explore")]
    Explore {
        /// JSON object: { repoRoot|dbPath, query: { params: { input, direction?, maxItems? } } }
        #[arg(long)]
        json: String,
    },
    /// Query: all files reachable from a file (transitive).
    /// (see `blast_radius` help for inputs)
    #[command(name = "blast_radius")]
    BlastRadius {
        /// JSON object: { repoRoot|dbPath, filePath }
        #[arg(long)]
        json: String,
    },
    /// Query: all files reachable from a symbol.
    /// (see `symbol_blast_radius` help for inputs)
    #[command(name = "symbol_blast_radius")]
    SymbolBlastRadius {
        /// JSON object: { repoRoot|dbPath, name, kind? }
        #[arg(long)]
        json: String,
    },
    /// Query: symbols changed between git states.
    /// (see `detect_changes` help for inputs)
    #[command(name = "detect_changes")]
    DetectChanges {
        /// JSON object: { repoRoot|dbPath, diffMode, range? }
        #[arg(long)]
        json: String,
    },
    /// Query: resolved import edges of a file.
    /// (see `find_imports` help for inputs)
    #[command(name = "find_imports")]
    FindImports {
        /// JSON object: { repoRoot|dbPath, filePath }
        #[arg(long)]
        json: String,
    },
    /// Query: type hierarchy for a symbol.
    /// (see `type_hierarchy` help for inputs)
    #[command(name = "type_hierarchy")]
    TypeHierarchy {
        /// JSON object: { repoRoot|dbPath, name?, filePath? }
        #[arg(long)]
        json: String,
    },
    /// Query: filter symbols by kind/tags/language/etc.
    /// (see `filter_symbols` help for inputs)
    #[command(name = "filter_symbols")]
    FilterSymbols {
        /// JSON object: { repoRoot|dbPath, kind?, tags?, language?, file?, ... }
        #[arg(long)]
        json: String,
    },
    /// Query: find AST nodes matching a $VAR / $$$VAR pattern.
    /// (see `find_pattern` help for inputs)
    #[command(name = "find_pattern")]
    FindPattern {
        /// JSON object: { repoRoot|dbPath, pattern, file?, language? }
        #[arg(long)]
        json: String,
    },
    /// Query: keyword-driven context bundle — files/symbols matching `query`
    /// (substring/exact against paths, directory names, and symbol names)
    /// plus their one-hop graph neighborhood, ranked and with covering
    /// tests. Structural only: no doc corpus, no semantic search.
    /// (see `context_pack` help for inputs)
    #[command(name = "context_pack")]
    ContextPack {
        /// JSON object: { repoRoot|dbPath, query }
        #[arg(long)]
        json: String,
    },
    /// Query: session-start repo orientation map — entrypoints,
    /// foundational files, module layers, subsystems, symbols, flows, and
    /// hotspots assembled from the persisted index. JSON is the canonical
    /// output; `--format text` derives a plain-text rendering from the same
    /// JSON. (see `nav_map` help for inputs)
    #[command(name = "nav_map")]
    NavMap {
        /// JSON object: { repoRoot|dbPath }
        #[arg(long)]
        json: String,
        /// Output format: `json` (default) or `text`.
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// Scan: run the repo's rule packs (user + repo scope) against the
    /// persisted index and emit findings. Requires a fresh prior `build`;
    /// exits non-zero when findings exist at/above the severity threshold.
    /// (see `scan` help for inputs)
    #[command(name = "scan")]
    Scan {
        /// JSON object: { repoRoot, output?, severityThreshold? }
        #[arg(long)]
        json: String,
        /// Apply `rewrite` templates to matched files. Default (no flag) is
        /// read-only: matches reported, nothing written.
        #[arg(long)]
        apply: bool,
        /// With `--apply`: write to files with uncommitted git changes.
        /// Without it, such files are skipped as `skipped-dirty`.
        #[arg(long)]
        force: bool,
    },
    /// Test: run every rule's `[[test]]` entries and report pass/fail.
    /// Self-contained — no DB, no prior `build`; exits non-zero when any
    /// test fails. (see `test` help for inputs)
    #[command(name = "test")]
    Test {
        /// JSON object: { rulesDir? }
        #[arg(long)]
        json: String,
    },
    /// List the rules that would run for a repo (built-in + user + repo
    /// scope, with override provenance) — no DB required, never scans.
    /// Run this first to see what `scan` would use before seeding or
    /// customizing anything. (see `rules_list` help for inputs)
    #[command(name = "rules_list")]
    RulesList {
        /// JSON object: { repoRoot }
        #[arg(long)]
        json: String,
    },
    /// Seed: materialize the built-in rule packs as editable TOML files in
    /// the repo (`.varde-code/rules/`, default) or user (`--user`,
    /// `~/.config/varde-code/rules/`) scope, so they can be customized
    /// directly instead of only overridden by id. Existing files are left
    /// untouched unless `--force`. Undo with `rules_remove` (same
    /// `--user`/`--force` flags). (see `rules_seed` help)
    #[command(name = "rules_seed")]
    RulesSeed {
        /// JSON object: { repoRoot }
        #[arg(long)]
        json: String,
        /// Seed into the user-scoped rules dir instead of the repo's.
        #[arg(long)]
        user: bool,
        /// Overwrite files that already exist (default: leave them alone).
        #[arg(long)]
        force: bool,
    },
    /// Undo `rules_seed`: delete previously seeded built-in rule pack files
    /// from the repo (default) or user (`--user`) rules dir. Only removes
    /// files matching a shipped built-in's name; custom rule files are left
    /// alone. A seeded file edited since seeding is skipped unless `--force`
    /// (which discards those local edits). (see `rules_remove` help)
    #[command(name = "rules_remove")]
    RulesRemove {
        /// JSON object: { repoRoot }
        #[arg(long)]
        json: String,
        /// Remove from the user-scoped rules dir instead of the repo's.
        #[arg(long)]
        user: bool,
        /// Also remove seeded files that were locally modified.
        #[arg(long)]
        force: bool,
    },
    /// List the agent skills that ship with this binary and their supported
    /// harness targets (Claude, Codex, OpenCode, Pi) — no filesystem writes.
    /// (see `skills_list` help for inputs)
    #[command(name = "skills_list")]
    SkillsList,
    /// Install the bundled agent skills for Claude, Codex, OpenCode, and Pi.
    /// Omitting `--agent` targets all four harnesses. Each pack is written as
    /// its own `varde-code-<name>/` subdirectory. Existing files are left
    /// untouched unless `--force`. `--dir` overrides each selected harness's
    /// default directory, primarily for project-local installs and testing.
    /// (see `skills_install` help)
    #[command(name = "skills_install")]
    SkillsInstall {
        /// Harnesses to target: `claude`, `codex`, `opencode`, `pi`.
        /// Repeatable or comma-separated. Defaults to all four.
        #[arg(long = "agent", value_delimiter = ',')]
        agent: Vec<String>,
        /// Target directory to install skill packs into.
        #[arg(long)]
        dir: Option<String>,
        /// Overwrite files that already exist (default: leave them alone).
        #[arg(long)]
        force: bool,
    },
    /// Undo `skills_install` for the selected harnesses (all four by default).
    /// `--dir` overrides each selected harness's default directory. Only files
    /// matching a shipped skill pack are touched. A file edited since install
    /// is skipped unless `--force`. Empty pack directories are removed once
    /// cleared.
    /// (see `skills_remove` help)
    #[command(name = "skills_remove")]
    SkillsRemove {
        /// Harnesses to target: `claude`, `codex`, `opencode`, `pi`.
        /// Repeatable or comma-separated. Defaults to all four.
        #[arg(long = "agent", value_delimiter = ',')]
        agent: Vec<String>,
        /// Directory skill packs were installed into.
        #[arg(long)]
        dir: Option<String>,
        /// Also remove installed files that were locally modified.
        #[arg(long)]
        force: bool,
    },
    /// Session-start hooks for supported agent harnesses (claude, codex,
    /// opencode, pi) that shell out to `nav_map` at session start — same
    /// idea as `skills`, but installs a hook instead of skill files.
    /// (see `hooks install`/`hooks remove`/`hooks list` help)
    #[command(subcommand)]
    Hooks(HooksCommand),
    /// Dump the slice freshness ledger for a repo (the `--why` view: what
    /// each derived slice was last built through and whether it's stale).
    /// Read-only — never builds or freshens. (see `slice_state` help for inputs)
    #[command(name = "slice_state")]
    SliceState {
        /// JSON object: { repoRoot }
        #[arg(long)]
        json: String,
    },
    /// Run a long-lived background watcher that proactively keeps one or
    /// more repos' indexes warm via the existing incremental `ensure_fresh`
    /// path (fs events, debounced). Latency optimization only: every query
    /// still self-verifies freshness independently, so a dead or lagging
    /// watcher never produces a stale answer, only a slower one. Foreground
    /// process — backgrounding is the caller's job (launchd/systemd/nohup).
    Watch {
        /// Explicit repo paths to watch (combined with `--config`'s
        /// `repos`/`parent_dirs`, if given). Also identifies which running
        /// watcher `--stop` targets.
        #[arg(long = "repo")]
        repos: Vec<String>,
        /// Path to a watch.toml config (defaults to
        /// `~/.config/varde-code/watch.toml` if present and no `--repo` was
        /// given).
        #[arg(long)]
        config: Option<String>,
        /// Debounce window in milliseconds before an fs-event burst triggers
        /// a rebuild.
        #[arg(long, default_value_t = 750)]
        debounce_ms: u64,
        /// List every running (or stale-locked) watcher instance as JSON and
        /// exit — does not start a watcher. Ignores `--repo`/`--config`.
        #[arg(long, conflicts_with_all = ["stop", "stop_all"])]
        list: bool,
        /// Stop the watcher for the repo set given via `--repo`/`--config`
        /// (SIGTERM if live; always clears its lock) and exit — does not
        /// start a watcher.
        #[arg(long, conflicts_with = "stop_all")]
        stop: bool,
        /// Stop every running watcher instance and exit — does not start a
        /// watcher. Ignores `--repo`/`--config`.
        #[arg(long)]
        stop_all: bool,
    },
}

/// `hooks install`/`hooks remove`/`hooks list` subcommands. Flat-flag
/// convention (no `--json` envelope), parallel to `skills_install`/
/// `skills_remove`/`skills_list`.
#[derive(Subcommand)]
pub enum HooksCommand {
    /// List the 4 supported agent hook targets (claude, codex, opencode,
    /// pi) and the directory/file each installs to — no filesystem writes.
    /// (see `hooks list` help for inputs)
    #[command(name = "list")]
    List,
    /// Install session-start hooks for the given agents (default: all 4)
    /// that shell out to `varde-code nav_map` at session start.
    /// Existing entries are left untouched unless `--force`. Undo with
    /// `hooks remove` (same `--agent`/`--dir`/`--force`).
    /// (see `hooks install` help)
    #[command(name = "install")]
    Install {
        /// Agent(s) to install for: `claude`, `codex`, `opencode`, `pi`.
        /// Repeatable (`--agent claude --agent codex`) or comma-separated
        /// (`--agent claude,codex`). Defaults to all 4.
        #[arg(long = "agent", value_delimiter = ',')]
        agent: Vec<String>,
        /// Overwrite entries that already exist (default: leave them alone).
        #[arg(long)]
        force: bool,
        /// Override the install target directory (primarily for testing).
        /// Without it, each agent resolves its real per-OS default
        /// (`~/.claude/`, `~/.codex/`, `~/.config/opencode/`,
        /// `~/.pi/agent/extensions/`).
        #[arg(long)]
        dir: Option<String>,
    },
    /// Undo `hooks install`: remove previously installed session-start
    /// hooks for the given agents (default: all 4). Whole-file targets
    /// (opencode, pi) are deleted; merge targets (claude, codex) have only
    /// this tool's injected entry removed, never the rest of the shared
    /// config file. A target edited since install is skipped unless
    /// `--force` (which discards those local edits).
    /// (see `hooks remove` help)
    #[command(name = "remove")]
    Remove {
        /// Agent(s) to remove for: `claude`, `codex`, `opencode`, `pi`.
        /// Repeatable or comma-separated. Defaults to all 4.
        #[arg(long = "agent", value_delimiter = ',')]
        agent: Vec<String>,
        /// Also remove entries that were locally modified.
        #[arg(long)]
        force: bool,
        /// Override the install target directory (primarily for testing).
        /// Without it, each agent resolves its real per-OS default.
        #[arg(long)]
        dir: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, HooksCommand};
    use clap::Parser;

    #[test]
    fn build_accepts_force_flag() {
        let cli =
            Cli::try_parse_from(["varde-code", "build", "--repo-root", "/tmp/repo", "--force"])
                .expect("build arguments parse");
        assert!(matches!(cli.command, Command::Build { force: true, .. }));
    }
    #[test]
    fn scan_accepts_json_argument() {
        let json = r#"{"repoRoot":"/tmp/repo"}"#;
        let cli = Cli::try_parse_from(["varde-code", "scan", "--json", json])
            .expect("scan arguments parse");
        match cli.command {
            Command::Scan {
                json: parsed,
                apply,
                force,
            } => {
                assert_eq!(parsed, json);
                assert!(!apply, "apply defaults off");
                assert!(!force, "force defaults off");
            }
            _ => panic!("expected Command::Scan"),
        }
    }

    #[test]
    fn test_accepts_json_argument() {
        let json = r#"{"rulesDir":"/tmp/rules"}"#;
        let cli = Cli::try_parse_from(["varde-code", "test", "--json", json])
            .expect("test arguments parse");
        match cli.command {
            Command::Test { json: parsed } => assert_eq!(parsed, json),
            _ => panic!("expected Command::Test"),
        }
    }

    #[test]
    fn scan_accepts_apply_and_force_flags() {
        let json = r#"{"repoRoot":"/tmp/repo"}"#;
        let cli = Cli::try_parse_from(["varde-code", "scan", "--json", json, "--apply", "--force"])
            .expect("scan flags parse");
        match cli.command {
            Command::Scan {
                json: parsed,
                apply,
                force,
            } => {
                assert_eq!(parsed, json);
                assert!(apply, "--apply parsed");
                assert!(force, "--force parsed");
            }
            _ => panic!("expected Command::Scan"),
        }
    }

    #[test]
    fn rules_list_accepts_json_argument() {
        let json = r#"{"repoRoot":"/tmp/repo"}"#;
        let cli = Cli::try_parse_from(["varde-code", "rules_list", "--json", json])
            .expect("rules_list arguments parse");
        match cli.command {
            Command::RulesList { json: parsed } => assert_eq!(parsed, json),
            _ => panic!("expected Command::RulesList"),
        }
    }

    #[test]
    fn rules_seed_accepts_json_and_flags() {
        let json = r#"{"repoRoot":"/tmp/repo"}"#;
        let cli = Cli::try_parse_from([
            "varde-code",
            "rules_seed",
            "--json",
            json,
            "--user",
            "--force",
        ])
        .expect("rules_seed arguments parse");
        match cli.command {
            Command::RulesSeed {
                json: parsed,
                user,
                force,
            } => {
                assert_eq!(parsed, json);
                assert!(user, "--user parsed");
                assert!(force, "--force parsed");
            }
            _ => panic!("expected Command::RulesSeed"),
        }
    }

    #[test]
    fn nav_map_defaults_to_json_format() {
        let json = r#"{"repoRoot":"/tmp/repo"}"#;
        let cli = Cli::try_parse_from(["varde-code", "nav_map", "--json", json])
            .expect("nav_map arguments parse");
        match cli.command {
            Command::NavMap {
                json: parsed,
                format,
            } => {
                assert_eq!(parsed, json);
                assert_eq!(format, "json", "format defaults to json");
            }
            _ => panic!("expected Command::NavMap"),
        }
    }

    #[test]
    fn nav_map_accepts_text_format_flag() {
        let json = r#"{"repoRoot":"/tmp/repo"}"#;
        let cli =
            Cli::try_parse_from(["varde-code", "nav_map", "--json", json, "--format", "text"])
                .expect("nav_map --format text parses");
        match cli.command {
            Command::NavMap { format, .. } => assert_eq!(format, "text"),
            _ => panic!("expected Command::NavMap"),
        }
    }

    /// Regression: the command injected into every session-start hook must
    /// name a subcommand this CLI actually recognizes. It shipped as
    /// `varde-code report nav_map ...` since 0.1.0 — but there is no `report`
    /// subcommand, so injection failed with "unrecognized subcommand" and
    /// never produced output for any agent. Guard the subcommand name by
    /// parsing it with clap.
    #[test]
    fn injected_session_start_command_names_a_valid_subcommand() {
        let cmd = crate::hooks::CLAUDE_SESSION_START_COMMAND;
        assert!(
            !cmd.contains("report"),
            "injected command must not reference the non-existent `report` subcommand: {cmd}"
        );
        let subcommand = cmd
            .strip_prefix("varde-code ")
            .and_then(|rest| rest.split_whitespace().next())
            .expect("injected command starts with `varde-code <subcommand>`");
        assert_eq!(subcommand, "nav_map", "injected command invokes nav_map");
        Cli::try_parse_from(["varde-code", subcommand, "--json", "{}"]).unwrap_or_else(|e| {
            panic!("injected subcommand {subcommand:?} must parse as a valid CLI subcommand: {e}")
        });
    }

    #[test]
    fn hooks_install_defaults_to_empty_agent_list() {
        let cli =
            Cli::try_parse_from(["varde-code", "hooks", "install"]).expect("hooks install parses");
        match cli.command {
            Command::Hooks(HooksCommand::Install { agent, force, dir }) => {
                assert!(
                    agent.is_empty(),
                    "no --agent means empty vec (caller defaults to all 4)"
                );
                assert!(!force);
                assert!(dir.is_none());
            }
            _ => panic!("expected Command::Hooks(HooksCommand::Install)"),
        }
    }

    #[test]
    fn hooks_install_accepts_repeated_agent_flags() {
        let cli = Cli::try_parse_from([
            "varde-code",
            "hooks",
            "install",
            "--agent",
            "claude",
            "--agent",
            "codex",
        ])
        .expect("repeated --agent parses");
        match cli.command {
            Command::Hooks(HooksCommand::Install { agent, .. }) => {
                assert_eq!(agent, vec!["claude".to_string(), "codex".to_string()]);
            }
            _ => panic!("expected Command::Hooks(HooksCommand::Install)"),
        }
    }

    #[test]
    fn hooks_install_accepts_comma_separated_agents() {
        let cli =
            Cli::try_parse_from(["varde-code", "hooks", "install", "--agent", "claude,codex"])
                .expect("comma-separated --agent parses");
        match cli.command {
            Command::Hooks(HooksCommand::Install { agent, .. }) => {
                assert_eq!(agent, vec!["claude".to_string(), "codex".to_string()]);
            }
            _ => panic!("expected Command::Hooks(HooksCommand::Install)"),
        }
    }

    #[test]
    fn hooks_install_accepts_force_and_dir() {
        let cli = Cli::try_parse_from([
            "varde-code",
            "hooks",
            "install",
            "--force",
            "--dir",
            "/tmp/hooks-test",
        ])
        .expect("--force/--dir parse");
        match cli.command {
            Command::Hooks(HooksCommand::Install { force, dir, .. }) => {
                assert!(force);
                assert_eq!(dir.as_deref(), Some("/tmp/hooks-test"));
            }
            _ => panic!("expected Command::Hooks(HooksCommand::Install)"),
        }
    }

    #[test]
    fn hooks_remove_accepts_agent_force_dir() {
        let cli = Cli::try_parse_from([
            "varde-code",
            "hooks",
            "remove",
            "--agent",
            "pi",
            "--force",
            "--dir",
            "/tmp/hooks-test",
        ])
        .expect("hooks remove parses");
        match cli.command {
            Command::Hooks(HooksCommand::Remove { agent, force, dir }) => {
                assert_eq!(agent, vec!["pi".to_string()]);
                assert!(force);
                assert_eq!(dir.as_deref(), Some("/tmp/hooks-test"));
            }
            _ => panic!("expected Command::Hooks(HooksCommand::Remove)"),
        }
    }

    #[test]
    fn hooks_list_parses() {
        let cli = Cli::try_parse_from(["varde-code", "hooks", "list"]).expect("hooks list parses");
        assert!(matches!(cli.command, Command::Hooks(HooksCommand::List)));
    }

    #[test]
    fn scan_requires_json_argument() {
        match Cli::try_parse_from(["varde-code", "scan"]) {
            Err(err) => {
                let message = err.to_string();
                assert!(
                    message.contains("json"),
                    "clap names the missing argument: {message}"
                );
            }
            Ok(_) => panic!("missing required --json must fail"),
        }
    }
}
