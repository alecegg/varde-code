//! Shared test helper for query submodule tests: isolate `HOME` (which
//! drives the conventional DB path) for the duration of a test closure,
//! serialized against every other test that mutates `HOME` via the shared
//! [`crate::HOME_TEST_LOCK`].

// The inner module name intentionally mirrors the file so the helper is
// reached as `test_support::with_home_redirect` from sibling test modules.
#[cfg(test)]
#[allow(clippy::module_inception)]
pub(crate) mod test_support {
    /// Redirect `HOME` to an isolated temp dir for the duration of `f`.
    ///
    /// `prefix` namespaces the temp dir per call site (mirrors each module's
    /// prior local `varde-<prefix>-home-...` naming) so concurrent test runs
    /// across modules never collide; `label` further disambiguates within a
    /// module's own tests.
    pub(crate) fn with_isolated_home<F: FnOnce()>(prefix: &str, label: &str, f: F) {
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");
        let home = std::env::temp_dir().join(format!(
            "varde-{prefix}-home-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("home creates");
        let original = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", &home) };
        f();
        match original {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let _ = std::fs::remove_dir_all(&home);
    }
}
