//! Shared test-only support for process-global environment overrides.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Acquire a poisoned HOME lock after its prior holder unwound.
pub(crate) fn home_lock() -> MutexGuard<'static, ()> {
    recover_lock(&crate::HOME_TEST_LOCK)
}

fn recover_lock(lock: &Mutex<()>) -> MutexGuard<'_, ()> {
    lock.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Holds the shared HOME lock while an override is active.
pub(crate) struct HomeOverride {
    previous: Option<OsString>,
    _lock: Option<MutexGuard<'static, ()>>,
}

impl HomeOverride {
    /// Redirect HOME to `path` until this guard is dropped.
    pub(crate) fn new(path: &Path) -> Self {
        let lock = home_lock();
        let previous = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", path) };
        Self {
            previous,
            _lock: Some(lock),
        }
    }

    /// Redirect HOME while the caller holds [`crate::HOME_TEST_LOCK`].
    ///
    /// This supports existing tests that need one lock around several setup
    /// and assertion phases. The caller's lock must outlive this guard.
    pub(crate) fn while_locked(path: &Path) -> Self {
        let previous = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", path) };
        Self {
            previous,
            _lock: None,
        }
    }
}

impl Drop for HomeOverride {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(home) => unsafe { std::env::set_var("HOME", home) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

/// Run a test with a unique, temporary HOME directory.
pub(crate) fn with_isolated_home<F: FnOnce()>(label: &str, f: F) {
    let home = unique_home(label);
    std::fs::create_dir_all(&home).expect("home creates");
    let _override = HomeOverride::new(&home);
    f();
    let _ = std::fs::remove_dir_all(&home);
}

fn unique_home(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "varde-test-home-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_override_restores_the_prior_value_after_a_panic() {
        let _lock = home_lock();
        let original = std::env::var_os("HOME");
        let replacement = std::env::temp_dir().join("varde-test-home-restore");

        let outcome = std::panic::catch_unwind(|| {
            let _override = HomeOverride::while_locked(&replacement);
            assert_eq!(std::env::var_os("HOME"), Some(replacement.into_os_string()));
            panic!("expected test panic");
        });

        assert!(outcome.is_err());
        assert_eq!(std::env::var_os("HOME"), original);
    }

    #[test]
    fn recover_lock_acquires_a_poisoned_mutex() {
        let lock = Mutex::new(());
        let _ = std::panic::catch_unwind(|| {
            let _guard = lock.lock().expect("fresh lock");
            panic!("poison the lock");
        });

        let _guard = recover_lock(&lock);
    }
}
