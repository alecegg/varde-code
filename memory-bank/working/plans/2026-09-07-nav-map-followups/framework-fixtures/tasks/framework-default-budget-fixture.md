---
type: task
parent: 2026-09-07-nav-map-followups/framework-fixtures
status: done
verified: passed
depends_on: []
modifies:
  - crates/varde-code/src/query/nav_map.rs
creates: []
---

Keep framework routes visible in the default nav map

Add one end-to-end default-budget nav-map regression. Build an isolated mixed
framework repository with NestJS, Spring, and ASP.NET route files. Use enough
Nest handlers to exceed the entrypoint cap. Assert each framework route remains
useful after truncation. Also exclude a plain Nest `@Injectable()` service and
its uniquely named method.

#### Out of scope

- Route-detection changes.
- Permanent fixture directories.
- Budget policy changes.

#### Verification

- assert: `cargo test -p varde-code nav_map_tests` → default-budget framework fixture passes.
- assert: `cargo test -p varde-code semantic_entrypoint_nestjs` → Nest classification remains precise.
- assert: `cargo clippy -p varde-code --lib -- -D warnings` → no lint warnings.

#### Progress

- Expanded the fixture beyond the entrypoint cap.
- Asserted each framework route remains after truncation.
- Asserted the service class and uniquely named method remain excluded.
- Verified with the nav-map tests and library Clippy.
