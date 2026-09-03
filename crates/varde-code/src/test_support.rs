//! Shared test-only support for process-global environment overrides.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;

/// Holds the shared HOME lock while an override is active.
pub(crate) struct HomeOverride {
    previous: Option<OsString>,
    _lock: Option<MutexGuard<'static, ()>>,
}

impl HomeOverride {
    /// Redirect HOME to `path` until this guard is dropped.
    pub(crate) fn new(path: &Path) -> Self {
        let lock = crate::HOME_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let _lock = crate::HOME_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
}
