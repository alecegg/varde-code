---
type: task
parent: 2026-09-07-nav-map-followups/local-release
status: done
verified: passed
depends_on: []
modifies: []
creates: []
---

Install the verified local CLI

Install `crates/varde-code` into the PATH-resolved local binary location.

#### Out of scope

- Publishing a versioned release.

#### Verification

- assert: `varde-code --version` → installed local CLI reports its package version.
- assert: `cargo test -p varde-code nav_map_tests --lib` → nav-map source tests pass.

#### Progress

- Installed with `cargo install --path crates/varde-code`; `/Users/alec/.local/bin/varde-code --version` reports `varde-code 0.1.0`.
