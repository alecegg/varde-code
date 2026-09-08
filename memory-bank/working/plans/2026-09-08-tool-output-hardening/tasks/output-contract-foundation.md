---
type: task
parent: 2026-09-08-tool-output-hardening
status: done
verified: passed
depends_on: []
modifies:
  - crates/varde-code/src/query/output.rs
  - crates/varde-code/src/query/mod.rs
  - crates/varde-code/tests/query_envelope.rs
creates: []
---

# Define the shared machine-output contract

Create the shared `{ ok, data, meta }` renderer. Keep stable error codes.
Relativize only known repository path fields. Preserve external file paths.
Expose explicit compactness and truncation metadata for adapters to use.

Test approach: contract tests first. The shared query renderer already exists.

#### Out of scope

- Migrating non-query command handlers.
- Compacting command-specific result payloads.

#### Verification

- assert: cargo test -p varde-code --test query_envelope → shared envelope tests pass
- assert: cargo test -p varde-code query::output → output policy tests pass

#### Progress

- Added the shared `{ok,data,meta}` renderer with compact and truncation metadata, structured error data, and key-scoped repository path rewriting. Verified query envelope and output-policy tests.
