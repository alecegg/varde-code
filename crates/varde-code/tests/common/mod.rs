//! Shared helpers for integration tests: resolve fixture files relative to
//! `tests/fixtures/`.

use std::path::PathBuf;

/// Absolute path to a fixture file under `tests/fixtures/<rel>`.
pub fn fixture_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

/// Contents of a fixture file under `tests/fixtures/<rel>`.
#[allow(dead_code)] // not every test binary uses every helper
pub fn fixture(rel: &str) -> String {
    std::fs::read_to_string(fixture_path(rel)).expect("fixture exists")
}
