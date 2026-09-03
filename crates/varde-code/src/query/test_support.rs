//! Query-facing wrapper around the crate-wide HOME test helper.

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
        crate::test_support::with_isolated_home(&format!("{prefix}-{label}"), f);
    }
}
