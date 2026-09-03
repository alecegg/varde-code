//! Per-repo advisory lock guarding the "detect + incremental rebuild"
//! critical section inside [`crate::slice::ensure_fresh`].
//!
//! Concurrent callers (multiple agents' CLI invocations, plus a future
//! background watcher) racing `ensure_fresh` on the same repo today only
//! coordinate through SQLite's own `SQLITE_BUSY` retry — every racer detects
//! the same staleness and does the same rebuild work, retrying against an
//! `EXCLUSIVE`-locked connection instead of skipping redundant work. This
//! lock makes the loser wait, then observe the winner's rebuild already
//! caught up (`built_through_rev`) and no-op instead of duplicating it.
//!
//! [`acquire`] uses a **kernel advisory lock (`flock`)** on the lock file
//! rather than a PID-file-plus-staleness-check scheme. `flock` is the only
//! approach that is actually mutually exclusive under contention: the kernel
//! serializes `LOCK_EX` across independent open file descriptions (different
//! threads and processes alike) and — crucially — releases the lock
//! automatically when the holder's fd closes *or the process dies*, so a
//! crashed holder (`kill -9`, panic, power loss) never wedges the lock.
//!
//! A read-then-`remove_file` (or rename-aside) "reclaim the stale lock"
//! scheme *cannot* be made correct here: removing or moving the lock file to
//! break it opens a window in which the file is absent, an `O_EXCL` creator
//! slips in, and two callers end up believing they hold the lock. `flock`
//! sidesteps stale detection entirely — there is no lock file content to
//! reclaim, only a kernel lock the OS owns.
//!
//! The raw `flock`/`kill` externs (no `libc`/`fs2` dependency) match this
//! crate's existing minimal-FFI style. On non-unix targets `acquire` falls
//! back to a best-effort exclusive-create lock file with no crash recovery.
//!
//! [`reclaim_if_stale`], [`pid_is_alive`], and [`terminate`] remain for
//! [`crate::watch`]'s separate single-instance watcher lock, whose PID-file +
//! liveness-check model is appropriate for that low-contention use.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A held per-repo lock.
pub struct RepoLock {
    /// On unix the held `File` *is* the lock: `flock` is released when this fd
    /// closes on drop (or when the process dies — the kernel cleans up). The
    /// lock file is intentionally left on disk; its mere presence means
    /// nothing without a live `flock` behind it.
    #[cfg(unix)]
    _file: std::fs::File,
    /// On non-unix we fall back to an exclusive-create lock file removed on
    /// drop: best-effort mutual exclusion among live processes, with no
    /// crash recovery (a crashed holder's file must be cleared out of band).
    #[cfg(not(unix))]
    path: PathBuf,
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        // unix: closing `_file` releases the flock — nothing to do here.
        #[cfg(not(unix))]
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

const BASE_BACKOFF_MS: u64 = 10;
const MAX_BACKOFF_MS: u64 = 250;

fn backoff_ms(attempt: u32) -> u64 {
    (BASE_BACKOFF_MS.saturating_mul(1 << attempt.min(6))).min(MAX_BACKOFF_MS)
}

fn lock_path_for(db_path: &Path) -> Result<PathBuf> {
    let lock_path = db_path.with_extension("db.lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating lock parent dir {}", parent.display()))?;
    }
    Ok(lock_path)
}

/// Acquire the advisory lock for `db_path`'s repo (a `.lock` sibling of the
/// index DB file). Blocks with exponential backoff until acquired or
/// `timeout` elapses.
///
/// On unix this holds an exclusive `flock`; a crashed holder's lock is
/// released by the kernel, so callers never wait out the timeout on a dead
/// process. See the module docs for why a PID-file reclaim scheme is unsafe.
#[cfg(unix)]
pub fn acquire(db_path: &Path, timeout: Duration) -> Result<RepoLock> {
    use std::io::Write;
    use std::os::unix::io::AsRawFd;

    let lock_path = lock_path_for(db_path)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("opening lock file {}", lock_path.display()))?;

    let deadline = Instant::now() + timeout;
    let mut attempt = 0u32;
    loop {
        // SAFETY: `as_raw_fd` is valid for the lifetime of `file`; `flock`
        // only manipulates kernel lock state for that fd.
        let ret = unsafe { libc_flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
        if ret == 0 {
            // Record the holder PID for observability only — the `flock`, not
            // this content, enforces exclusion. Best-effort; ignore errors.
            let _ = file.set_len(0);
            let _ = write!(&file, "{}", std::process::id());
            return Ok(RepoLock { _file: file });
        }

        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::WouldBlock {
            return Err(
                anyhow::Error::from(err).context(format!("flock on {}", lock_path.display()))
            );
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for repo lock at {} (held by a live process)",
                lock_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(backoff_ms(attempt)));
        attempt += 1;
    }
}

/// Non-unix fallback: exclusive-create lock file, removed on drop. Provides
/// mutual exclusion among live processes but no crashed-holder recovery.
#[cfg(not(unix))]
pub fn acquire(db_path: &Path, timeout: Duration) -> Result<RepoLock> {
    use std::io::Write;

    let lock_path = lock_path_for(db_path)?;
    let deadline = Instant::now() + timeout;
    let mut attempt = 0u32;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut file) => {
                let _ = write!(file, "{}", std::process::id());
                return Ok(RepoLock { path: lock_path });
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if Instant::now() >= deadline {
                    bail!("timed out waiting for repo lock at {}", lock_path.display());
                }
                std::thread::sleep(Duration::from_millis(backoff_ms(attempt)));
                attempt += 1;
            }
            Err(err) => {
                return Err(anyhow::Error::from(err).context("creating lock file"));
            }
        }
    }
}

/// If the lock file's recorded PID is no longer a live process, remove it and
/// report `true` (reclaimed). Any I/O error or unparseable content is treated
/// as "not stale" (leave it for the caller to time out on) rather than risk
/// reclaiming a lock that's actually held.
///
/// Used by [`crate::watch`] for its single-instance watcher lock — a
/// low-contention, one-writer use where read-then-remove reclaim is adequate.
/// It is deliberately **not** used by [`acquire`], whose high-contention
/// critical section relies on `flock` instead (see module docs).
///
/// Known liveness gap (accepted tradeoff): if the OS reuses the recorded PID
/// for an unrelated process before this check runs, a crashed holder's lock
/// looks live and callers wait out the timeout instead of reclaiming
/// immediately. PID reuse within a lock's timeout window is rare on a
/// single-machine guard; the failure mode is a bounded wait, never
/// corruption.
pub(crate) fn reclaim_if_stale(lock_path: &Path) -> Result<bool> {
    let contents = match std::fs::read_to_string(lock_path) {
        Ok(contents) => contents,
        // Lock file vanished between our failed create and this read
        // (the holder released it) — nothing to reclaim, caller retries.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(_) => return Ok(false),
    };
    let Ok(pid) = contents.trim().parse::<u32>() else {
        return Ok(false);
    };
    if pid_is_alive(pid) {
        return Ok(false);
    }
    // Best-effort remove; if another racer already reclaimed it, that's fine.
    let _ = std::fs::remove_file(lock_path);
    Ok(true)
}

/// Is `pid` a currently running process? Unix-only (`kill(pid, 0)` — signal
/// 0 checks existence without sending a signal). On non-Unix targets, always
/// reports the PID as alive so a lock is never mistakenly reclaimed (falls
/// back to the timeout instead of a false-positive removal, at the cost of
/// crashed-holder recovery only on Unix).
#[cfg(unix)]
pub(crate) fn pid_is_alive(pid: u32) -> bool {
    // SAFETY: `kill` with signal 0 performs no action beyond existence/
    // permission checks; `pid` is a plain integer we parsed ourselves.
    let ret = unsafe { libc_kill(pid as i32, 0) };
    ret == 0
        || std::io::Error::last_os_error().raw_os_error()
            == Some(1 /* EPERM: alive, not ours */)
}

#[cfg(not(unix))]
pub(crate) fn pid_is_alive(_pid: u32) -> bool {
    true
}

/// Send `SIGTERM` to `pid` — used by `varde-code watch --stop` to ask a
/// running watcher to exit (it holds no in-memory freshness state, so a
/// plain terminate is safe; see `watch`'s module doc). Unix-only; a no-op
/// `Ok(())` on other targets, same "never crash, only skip the optimization"
/// posture as `pid_is_alive`.
#[cfg(unix)]
pub(crate) fn terminate(pid: u32) -> std::io::Result<()> {
    // SAFETY: same as `pid_is_alive` — `pid` is a plain integer we parsed
    // ourselves, and signal 15 (SIGTERM) is a standard, non-destructive-to-
    // memory-safety request to exit.
    let ret = unsafe { libc_kill(pid as i32, 15) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
pub(crate) fn terminate(_pid: u32) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
const LOCK_EX: i32 = 2;
#[cfg(unix)]
const LOCK_NB: i32 = 4;

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
    #[link_name = "flock"]
    fn libc_flock(fd: i32, operation: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_then_release_allows_reacquire() {
        let dir = std::env::temp_dir().join(format!("varde-repo-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("index.db");

        let lock = acquire(&db_path, Duration::from_secs(1)).unwrap();
        drop(lock);

        let lock2 = acquire(&db_path, Duration::from_secs(1)).unwrap();
        drop(lock2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn second_acquire_times_out_while_first_is_held() {
        let dir =
            std::env::temp_dir().join(format!("varde-repo-lock-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("index.db");

        let _lock = acquire(&db_path, Duration::from_secs(1)).unwrap();
        let result = acquire(&db_path, Duration::from_millis(150));
        assert!(result.is_err(), "expected timeout while lock is held");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn leftover_lock_file_with_no_live_holder_is_acquirable() {
        // A lock file left on disk by a crashed holder carries no live
        // `flock`, so acquiring must succeed immediately (the kernel released
        // the lock when that process died) — no stale-PID reclaim required.
        let dir =
            std::env::temp_dir().join(format!("varde-repo-lock-test3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("index.db");
        std::fs::write(db_path.with_extension("db.lock"), "999999999").unwrap();

        let lock = acquire(&db_path, Duration::from_secs(2)).unwrap();
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reclaim_reports_stale_and_removes_dead_pid_lock() {
        // Covers the watcher-instance-lock helper (`reclaim_if_stale`).
        let dir =
            std::env::temp_dir().join(format!("varde-repo-lock-test4-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock_path = dir.join("watch.lock");
        std::fs::write(&lock_path, "999999999").unwrap();

        assert!(
            reclaim_if_stale(&lock_path).unwrap(),
            "dead-pid lock is stale"
        );
        assert!(!lock_path.exists(), "reclaimed lock file is removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reclaim_never_removes_a_live_lock() {
        // A lock whose recorded PID is alive must be left untouched. Using our
        // own (definitely-live) PID stands in for an active holder.
        let dir =
            std::env::temp_dir().join(format!("varde-repo-lock-test5-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock_path = dir.join("watch.lock");
        let live = std::process::id().to_string();
        std::fs::write(&lock_path, &live).unwrap();

        assert!(
            !reclaim_if_stale(&lock_path).unwrap(),
            "live lock is not stale"
        );
        assert!(lock_path.exists(), "live lock file survives reclaim");
        assert_eq!(
            std::fs::read_to_string(&lock_path).unwrap(),
            live,
            "live lock contents are unchanged"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contended_acquire_never_double_holds() {
        // Many threads race to acquire the same repo lock, starting from a
        // leftover lock file. A shared counter asserts mutual exclusion: at no
        // point may two threads hold the lock simultaneously. This is the
        // regression test for the double-hold the old PID-file reclaim allowed.
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir =
            std::env::temp_dir().join(format!("varde-repo-lock-test6-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("index.db");
        std::fs::write(db_path.with_extension("db.lock"), "999999999").unwrap();

        let held = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let db_path = db_path.clone();
            let held = Arc::clone(&held);
            let max_seen = Arc::clone(&max_seen);
            handles.push(std::thread::spawn(move || {
                for _ in 0..10 {
                    let lock = acquire(&db_path, Duration::from_secs(5)).unwrap();
                    let now = held.fetch_add(1, Ordering::SeqCst) + 1;
                    max_seen.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(1));
                    held.fetch_sub(1, Ordering::SeqCst);
                    drop(lock);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            max_seen.load(Ordering::SeqCst),
            1,
            "lock was held by more than one thread at once"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
