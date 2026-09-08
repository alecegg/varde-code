---
type: task
parent: 2026-09-07-nav-map-followups/nest-and-diversity
status: done
verified: passed
depends_on: []
modifies:
  - crates/varde-code/src/extract/langs/role_tags.rs
  - crates/varde-code/src/query/entrypoints.rs
creates: []
---

Classify Nest middleware precisely

Replace the broad `@Injectable()` middleware signal. A Nest class needs both
`@Injectable()` and `implements NestMiddleware` before it is an entrypoint.

#### Out of scope

- New Nest role types.
- Changes to controller route detection.

#### Verification

- assert: `cargo test -p varde-code semantic_entrypoint_nestjs` → Nest services are excluded and true middleware remains listed.
- assert: `cargo test -p varde-code role_tags` → composite TypeScript role matching passes.
- assert: `cargo clippy -p varde-code --lib -- -D warnings` → no lint warnings.

#### Progress

- Required `@Injectable()` plus `NestMiddleware`; services are excluded while middleware remains listed.
