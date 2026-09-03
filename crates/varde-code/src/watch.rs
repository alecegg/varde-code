//! `varde-code watch`: a long-lived, foreground background watcher that
//! proactively keeps configured repos' indexes warm.
//!
//! This is a **latency optimization, not a correctness source** (see the
//! warm-index plan). Every caller of `ensure_fresh` — including this
//! watcher — reads and writes the same freshness truth in SQLite
//! (`slice_state.built_through_rev`, `files.rev`); the watcher holds no
//! freshness state of its own in memory, so a killed-and-restarted watcher
//! resumes purely by re-reading DB state, with no burst-fire or
//! double-work risk on restart. A dead watcher just means the next real
//! query's own `ensure_fresh()` call pays the full incremental-rebuild cost
//! it always could — never staleness, only latency.

use anyhow::{Context, Result, bail};
use notify::{Event, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use crate::slice::{Scope, Slice, ensure_fresh};

/// The five slices a "keep everything warm" watcher freshens per repo —
/// mirrors what a repo-wide query already reads; the watcher does not
/// introduce a new rebuild target, it drives the existing full slice set
/// proactively.
const ALL_SLICES: [Slice; 5] = [
    Slice::Raw,
    Slice::Churn,
    Slice::Imports,
    Slice::Edges,
    Slice::Global,
];

/// Watch-list config: `~/.config/varde-code/watch.toml` by convention.
/// Discovery (`parent_dirs` → `.git` dirs) happens once at watcher startup,
/// not live — changing the watch list means restarting the process.
#[derive(Debug, Default, serde::Deserialize)]
pub struct WatchConfig {
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub parent_dirs: Vec<String>,
}

impl WatchConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("reading watch config {}", path.display()))?;
        toml::from_str(&contents)
            .with_context(|| format!("parsing watch config {} as TOML", path.display()))
    }

    pub fn default_path() -> PathBuf {
        crate::db::path::config_dir().join("watch.toml")
    }
}

/// Resolve `explicit` repo paths plus a config's `repos`/`parent_dirs` into
/// a deduplicated, canonicalized list of repo roots (a "repo root" is any
/// directory containing a `.git` entry). `parent_dirs` are scanned
/// one level for immediate `.git` children — not recursively walked — so
/// discovery stays cheap and predictable at startup.
pub fn resolve_repos(explicit: &[String], config: Option<&WatchConfig>) -> Result<Vec<PathBuf>> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut repos: Vec<PathBuf> = Vec::new();

    let mut add = |path: &Path| -> Result<()> {
        let canon = std::fs::canonicalize(path)
            .with_context(|| format!("resolving watch path {}", path.display()))?;
        if !canon.join(".git").exists() {
            bail!("{} is not a git repo root (no .git entry)", canon.display());
        }
        if seen.insert(canon.clone()) {
            repos.push(canon);
        }
        Ok(())
    };

    for path in explicit {
        add(Path::new(path))?;
    }
    if let Some(config) = config {
        for path in &config.repos {
            add(Path::new(path))?;
        }
        for parent in &config.parent_dirs {
            let entries = std::fs::read_dir(parent)
                .with_context(|| format!("reading parent dir {parent}"))?;
            for entry in entries {
                let entry = entry?;
                if entry.path().join(".git").exists() {
                    add(&entry.path())?;
                }
            }
        }
    }

    if repos.is_empty() {
        bail!("no repos to watch: pass --repo or configure repos/parent_dirs");
    }
    Ok(repos)
}

/// Run the watcher: one process-lifetime single-instance lock, one `notify`
/// watcher per repo, one shared debounce loop that reconciles whichever
/// repos have pending events once `debounce` has elapsed with no further
/// events for that repo.
pub fn run(repos: &[PathBuf], debounce: Duration) -> Result<()> {
    let _instance_lock = acquire_instance_lock(repos)?;

    let (tx, rx) = channel::<(PathBuf, PathBuf)>();
    // Keep each repo's `notify::Watcher` alive for the process lifetime —
    // dropping it stops that repo's events.
    let mut watchers = Vec::with_capacity(repos.len());
    for repo in repos {
        let repo_for_events = repo.clone();
        let ignore_matcher = build_ignore_matcher(repo)?;
        let tx = tx.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            let Ok(event) = res else { return };
            // Same exclusions as the existing fallback walk
            // (`ignore::WalkBuilder` — `.gitignore`, `.git/` itself) so
            // watching a repo doesn't generate a reconcile for every commit
            // (`.git/index`, `.git/index.lock`, etc.) or for `target/`,
            // `node_modules/`, and the like.
            for path in event.paths.iter().filter(|path| !is_ignored(&ignore_matcher, path)) {
                if let Err(err) = tx.send((repo_for_events.clone(), path.clone())) {
                    eprintln!(
                        "varde-code watch: dropped change event for {} (reconcile loop gone): {err}",
                        path.display()
                    );
                }
            }
        })
        .context("creating fs watcher")?;
        watcher
            .watch(repo, RecursiveMode::Recursive)
            .with_context(|| format!("watching {}", repo.display()))?;
        watchers.push(watcher);
        eprintln!("varde-code watch: watching {}", repo.display());
    }

    // Per-repo "last event seen" timestamp plus the paths that changed since
    // the last reconcile — a repo is due for reconcile once `debounce` has
    // elapsed since its last event with no newer one arriving in between
    // (collapses a burst — e.g. `git checkout` touching hundreds of files —
    // into a single rebuild). The accumulated paths are the drill-down
    // handle `watch --list` surfaces so an agent doesn't have to guess what
    // the watcher last reconciled.
    let mut pending: HashMap<PathBuf, (Instant, HashSet<PathBuf>)> = HashMap::new();

    loop {
        let wait = next_wait(&pending, debounce);
        match rx.recv_timeout(wait) {
            Ok((repo, path)) => {
                let entry = pending
                    .entry(repo)
                    .or_insert_with(|| (Instant::now(), HashSet::new()));
                entry.0 = Instant::now();
                entry.1.insert(path);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        let now = Instant::now();
        let due: Vec<PathBuf> = pending
            .iter()
            .filter(|&(_, &(last, _))| now.duration_since(last) >= debounce)
            .map(|(repo, _)| repo.clone())
            .collect();
        for repo in due {
            if let Some((_, changed_paths)) = pending.remove(&repo) {
                run_guarded(&repo, || reconcile(&repo, &changed_paths));
            }
        }
    }
    Ok(())
}

/// How long to block on the next event before re-checking which pending
/// repos have crossed the debounce window. `None` pending → block
/// indefinitely (any duration works since `recv_timeout` only needs a
/// value); otherwise wake up right when the earliest pending repo becomes
/// due.
fn next_wait(
    pending: &HashMap<PathBuf, (Instant, HashSet<PathBuf>)>,
    debounce: Duration,
) -> Duration {
    pending
        .values()
        .map(|&(last, _)| debounce.saturating_sub(last.elapsed()))
        .min()
        .unwrap_or(Duration::from_secs(3600))
        .max(Duration::from_millis(1))
}

/// Build a `.gitignore`-aware matcher for `repo` (also honoring `.ignore`
/// files, same as `ignore::WalkBuilder`'s default config used by the
/// fallback walk elsewhere in this crate).
fn build_ignore_matcher(repo: &Path) -> Result<ignore::gitignore::Gitignore> {
    let mut builder = ignore::gitignore::GitignoreBuilder::new(repo);
    for candidate in [".gitignore", ".ignore"] {
        let path = repo.join(candidate);
        if path.exists()
            && let Some(err) = builder.add(&path)
        {
            eprintln!("varde-code watch: warning: failed to read {candidate}: {err}");
        }
    }
    builder
        .build()
        .with_context(|| format!("building ignore matcher for {}", repo.display()))
}

/// Is `path` excluded from consideration — either inside a `.git/`
/// directory anywhere in its ancestry (never a meaningful source change;
/// checked by component rather than by stripping `repo`'s prefix, since
/// `notify` and `std::fs::canonicalize` can disagree on `/var` vs
/// `/private/var`-style symlink resolution on macOS) or matched by the
/// repo's own ignore rules?
fn is_ignored(matcher: &ignore::gitignore::Gitignore, path: &Path) -> bool {
    if path.components().any(|c| c.as_os_str() == ".git") {
        return true;
    }
    let is_dir = path.is_dir();
    matcher.matched(path, is_dir).is_ignore()
}

/// Reconcile one repo: drive the existing incremental `ensure_fresh` path
/// over every slice, going through the same per-repo advisory lock any
/// other caller (a concurrent CLI query) would — see `repo_lock`. Errors
/// are logged, not fatal: one repo's transient failure (e.g. a mid-rebase
/// working tree) must not take down the watcher for every other repo.
/// Run one repo's reconcile under a panic guard. A panic in a single repo's
/// reconcile (a corrupt index, a tree-sitter edge case) must not unwind the
/// whole watch loop and silently stop watching every *other* repo — catch it,
/// log, and carry on. The repo lock (released on unwind via RAII) and the
/// MEMORY-journal rollback leave that repo's index consistent.
fn run_guarded(repo: &Path, task: impl FnOnce()) {
    if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task)) {
        let msg = panic
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown cause".to_string());
        eprintln!(
            "varde-code watch: {}: reconcile panicked ({msg}); continuing",
            repo.display()
        );
    }
}

fn reconcile(repo: &Path, changed_paths: &HashSet<PathBuf>) {
    let repo_root = repo.to_string_lossy().into_owned();
    if let Err(err) = ensure_fresh(&ALL_SLICES, &repo_root, &Scope::Repo) {
        eprintln!("varde-code watch: {repo_root}: reconcile failed: {err:#}");
    }
    write_last_changed(repo, changed_paths);
}

/// Persist the paths that triggered the most recent reconcile for `repo`,
/// so `watch --list` can surface them as a drill-down handle instead of
/// only the repo root — an agent can feed these straight into
/// `symbols_in_file`/`map_file` without having to guess what changed.
fn write_last_changed(repo: &Path, changed_paths: &HashSet<PathBuf>) {
    let dir = crate::db::path::config_dir().join("watch-locks");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "varde-code watch: cannot create {} to record changed paths for {}: {err}",
            dir.display(),
            repo.display()
        );
        return;
    }
    let path = last_changed_path_for(repo, &dir);
    let mut paths: Vec<String> = changed_paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    paths.sort();
    let payload = serde_json::json!({ "changed_paths": paths });
    if let Err(err) = std::fs::write(&path, payload.to_string()) {
        eprintln!(
            "varde-code watch: failed to write changed-paths file {} for {}: {err}",
            path.display(),
            repo.display()
        );
    }
}

fn last_changed_path_for(repo: &Path, dir: &Path) -> PathBuf {
    dir.join(format!(
        "{}.changed",
        watch_set_id(std::slice::from_ref(&repo.to_path_buf()))
    ))
}

/// Single-instance guard for the watcher process itself — distinct from
/// `repo_lock`'s per-repo rebuild lock. This one answers "is a watcher
/// already running for this repo set", not "is a rebuild in progress", and
/// is held for the process's entire lifetime.
struct InstanceLock {
    path: PathBuf,
    meta_path: PathBuf,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // A `NotFound` here is expected (e.g. the lock was reclaimed as
        // stale), so only surface real cleanup failures — a leftover lock
        // makes the next watcher start reclaim it or wait unnecessarily.
        for path in [&self.path, &self.meta_path] {
            if let Err(err) = std::fs::remove_file(path)
                && err.kind() != std::io::ErrorKind::NotFound
            {
                eprintln!(
                    "varde-code watch: failed to remove {}: {err}",
                    path.display()
                );
            }
        }
    }
}

/// Sidecar of `<hash>.lock`: which repos that watcher instance covers, for
/// `watch --list`'s display. Kept separate from the `.lock` file itself
/// (rather than appended to it) because `repo_lock::reclaim_if_stale` — reused
/// here for stale-lock detection — expects the lock file's entire contents to
/// be exactly the PID, nothing else.
fn meta_path_for(lock_path: &Path) -> PathBuf {
    lock_path.with_extension("meta")
}

fn write_instance_meta(meta_path: &Path, repos: &[PathBuf]) {
    let repos_json: Vec<String> = repos
        .iter()
        .map(|r| r.to_string_lossy().into_owned())
        .collect();
    let meta = serde_json::json!({ "repos": repos_json });
    let _ = std::fs::write(meta_path, meta.to_string());
}

fn acquire_instance_lock(repos: &[PathBuf]) -> Result<InstanceLock> {
    let dir = crate::db::path::config_dir().join("watch-locks");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating watch lock dir {}", dir.display()))?;
    let path = dir.join(format!("{}.lock", watch_set_id(repos)));

    let lock = try_create_lock(&path).or_else(|err| match err {
        CreateError::AlreadyExists => {
            // The previous holder may have been killed (`kill -9`, crash)
            // rather than exited cleanly — `InstanceLock`'s `Drop` never ran,
            // so the file alone doesn't mean a watcher is still running.
            // Reclaim it the same way `repo_lock` reclaims a crashed rebuild
            // lock: check the recorded PID, and only bail if it's alive.
            if crate::repo_lock::reclaim_if_stale(&path).unwrap_or(false) {
                return try_create_lock(&path).map_err(|err| match err {
                    CreateError::AlreadyExists => anyhow::anyhow!(
                        "a watcher for this repo set was started by another process \
                         just now (lock {})",
                        path.display()
                    ),
                    CreateError::Other(err) => err,
                });
            }
            let existing_pid = std::fs::read_to_string(&path).unwrap_or_default();
            bail!(
                "a watcher for this repo set already appears to be running \
                 (lock {} held, pid={existing_pid}); `varde-code watch --list` \
                 shows every running watcher, `--stop`/`--stop-all` shuts them down",
                path.display()
            );
        }
        CreateError::Other(err) => Err(err),
    })?;
    write_instance_meta(&lock.meta_path, repos);
    Ok(lock)
}

enum CreateError {
    AlreadyExists,
    Other(anyhow::Error),
}

fn try_create_lock(path: &Path) -> Result<InstanceLock, CreateError> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            use std::io::Write;
            write!(file, "{}", std::process::id()).map_err(|err| {
                CreateError::Other(anyhow::Error::from(err).context("writing pid to lock file"))
            })?;
            Ok(InstanceLock {
                path: path.to_path_buf(),
                meta_path: meta_path_for(path),
            })
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(CreateError::AlreadyExists)
        }
        Err(err) => Err(CreateError::Other(
            anyhow::Error::from(err).context("creating watcher instance lock"),
        )),
    }
}

/// One running (or recently-running) watcher instance, as reported by
/// `watch --list` — derived from a `<hash>.lock` file plus its `.meta`
/// sidecar (see `write_instance_meta`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct WatchInstance {
    pub pid: u32,
    pub alive: bool,
    pub repos: Vec<String>,
    pub lock_path: String,
    /// Paths that triggered each repo's most recent reconcile — the
    /// drill-down handle for an agent to see what actually changed rather
    /// than only which repo root is being watched. Empty until the first
    /// reconcile after the watcher starts.
    pub changed_paths: Vec<String>,
}

/// List every watcher instance with a lock file under `watch-locks/`,
/// live or stale. Read-only — never reclaims or removes anything; that's
/// `--stop`/`--stop-all`'s job, so `--list` is always safe to run alongside
/// a live watcher.
pub fn list_instances() -> Result<Vec<WatchInstance>> {
    let dir = crate::db::path::config_dir().join("watch-locks");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut instances = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("lock") {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(pid) = contents.trim().parse::<u32>() else {
            continue;
        };
        let repos = std::fs::read_to_string(meta_path_for(&path))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("repos").cloned())
            .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok())
            .unwrap_or_default();
        let changed_paths = repos
            .iter()
            .flat_map(|repo| {
                std::fs::read_to_string(last_changed_path_for(Path::new(repo), &dir))
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .and_then(|v| v.get("changed_paths").cloned())
                    .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok())
                    .unwrap_or_default()
            })
            .collect();
        instances.push(WatchInstance {
            pid,
            alive: crate::repo_lock::pid_is_alive(pid),
            repos,
            lock_path: path.to_string_lossy().into_owned(),
            changed_paths,
        });
    }
    Ok(instances)
}

/// Outcome of stopping one watcher instance, as reported by `--stop`/`--stop-all`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StopOutcome {
    pub pid: u32,
    pub repos: Vec<String>,
    /// `true` if a live process was signaled; `false` if the lock was
    /// already stale (no signal needed — the lock/meta files are removed
    /// either way).
    pub stopped: bool,
}

/// Stop the watcher for exactly the given repo set (same `--repo` values a
/// `watch` invocation for it would use) — sends `SIGTERM` if its lock's PID
/// is alive, then removes the lock/meta files regardless (covers the stale
/// case too, so `--stop` also doubles as manual lock cleanup).
pub fn stop(repos: &[PathBuf]) -> Result<StopOutcome> {
    let dir = crate::db::path::config_dir().join("watch-locks");
    let path = dir.join(format!("{}.lock", watch_set_id(repos)));
    let contents = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "no watcher lock found for this repo set ({})",
            path.display()
        )
    })?;
    let pid: u32 = contents
        .trim()
        .parse()
        .with_context(|| format!("lock file {} does not contain a valid pid", path.display()))?;
    stop_instance(&path, pid)
}

/// Stop every watcher instance with a lock under `watch-locks/`, regardless
/// of repo set.
pub fn stop_all() -> Result<Vec<StopOutcome>> {
    list_instances()?
        .into_iter()
        .map(|instance| stop_instance(Path::new(&instance.lock_path), instance.pid))
        .collect()
}

fn stop_instance(lock_path: &Path, pid: u32) -> Result<StopOutcome> {
    let meta_path = meta_path_for(lock_path);
    let repos = std::fs::read_to_string(&meta_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("repos").cloned())
        .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok())
        .unwrap_or_default();

    let alive = crate::repo_lock::pid_is_alive(pid);
    if alive {
        crate::repo_lock::terminate(pid)
            .with_context(|| format!("sending SIGTERM to pid {pid}"))?;
    }
    let _ = std::fs::remove_file(lock_path);
    let _ = std::fs::remove_file(&meta_path);
    Ok(StopOutcome {
        pid,
        repos,
        stopped: alive,
    })
}

/// Deterministic id for a watch set: hash of the sorted, canonicalized repo
/// paths, so the same set of repos always maps to the same lock file
/// regardless of the order they were passed in.
fn watch_set_id(repos: &[PathBuf]) -> String {
    let mut paths: Vec<String> = repos
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    paths.sort();
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for byte in paths.join("\n").as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_repos_dedupes_explicit_and_config() {
        let dir = std::env::temp_dir().join(format!("varde-watch-test-{}", std::process::id()));
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        let config = WatchConfig {
            repos: vec![repo.to_string_lossy().into_owned()],
            parent_dirs: vec![],
        };
        let repos = resolve_repos(&[repo.to_string_lossy().into_owned()], Some(&config)).unwrap();
        assert_eq!(repos.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_repos_discovers_under_parent_dir() {
        let dir = std::env::temp_dir().join(format!("varde-watch-test2-{}", std::process::id()));
        let repo_a = dir.join("a");
        let repo_b = dir.join("b");
        let not_a_repo = dir.join("c");
        std::fs::create_dir_all(repo_a.join(".git")).unwrap();
        std::fs::create_dir_all(repo_b.join(".git")).unwrap();
        std::fs::create_dir_all(&not_a_repo).unwrap();

        let config = WatchConfig {
            repos: vec![],
            parent_dirs: vec![dir.to_string_lossy().into_owned()],
        };
        let mut repos = resolve_repos(&[], Some(&config)).unwrap();
        repos.sort();
        assert_eq!(repos.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_repos_rejects_non_git_explicit_path() {
        let dir = std::env::temp_dir().join(format!("varde-watch-test3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let result = resolve_repos(&[dir.to_string_lossy().into_owned()], None);
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_repos_errors_when_nothing_configured() {
        let result = resolve_repos(&[], None);
        assert!(result.is_err());
    }

    #[test]
    fn run_guarded_contains_a_panicking_reconcile() {
        // A panic in one repo's reconcile must be caught so the watch loop
        // survives to serve every other repo. (The default panic hook still
        // prints the panic to stderr — expected; what matters is that control
        // returns here instead of unwinding out of the loop.)
        run_guarded(Path::new("/nonexistent-repo"), || panic!("boom"));

        // A non-panicking task still runs to completion through the guard.
        let ran = std::cell::Cell::new(false);
        run_guarded(Path::new("/nonexistent-repo"), || ran.set(true));
        assert!(ran.get(), "non-panicking task runs under the guard");
    }

    #[test]
    fn reconcile_refreshes_index_after_file_change() {
        // End-to-end smoke test for the watch action: a file change followed by
        // a reconcile must land in the on-disk index.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let home =
            std::env::temp_dir().join(format!("varde-watch-home-{}-{stamp}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("home creates");
        let _home_override = crate::test_support::HomeOverride::new(&home);

        let root =
            std::env::temp_dir().join(format!("varde-watch-repo-{}-{stamp}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("repo creates");
        std::fs::write(root.join("a.rs"), "fn alpha() {}\n").expect("a.rs writes");
        let root_s = root.to_str().expect("root is utf-8");

        // Initial build establishes the index.
        crate::build::run_with_force(root_s, false).expect("initial build succeeds");

        // Add a function, then reconcile the repo exactly as the watch loop does.
        std::fs::write(root.join("a.rs"), "fn alpha() {}\nfn beta() {}\n")
            .expect("a.rs edit writes");
        reconcile(&root, &HashSet::new());

        // The freshly-added entity must now be present in the index.
        let db_path = crate::db::path::repo_db_path(&root);
        let conn = crate::db::open(&db_path).expect("db opens");
        let beta: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entities WHERE name = 'beta'",
                [],
                |r| r.get(0),
            )
            .expect("count query runs");
        assert!(beta >= 1, "reconcile picked up the newly added function");
        drop(conn);

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&root);
    }
}
