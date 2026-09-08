---
type: task
parent: 2026-09-07-nav-map-section-budget
status: done
verified: passed
depends_on: []
modifies:
  - crates/varde-code/src/query/nav_map.rs
  - crates/varde-code/src/main.rs
creates: []
---

Preserve nav-map orientation sections under the global budget

Change the nav-map budgeter so every populated orientation section receives a
bounded opportunity to retain an item before remaining tokens are spent by the
existing orientation priority. Keep the entire rendered map within
`maxTokensEstimate`, including module-layer handling. Preserve the JSON
`guide.truncated` contract and record accurate `shown`, `total`, and `more`
details whenever a section cannot retain all its items.

Update the text renderer to consult `guide.truncated` before rendering an empty
array as `(none)`. An empty array caused by budget trimming must state that no
items were shown, include the total, and provide the existing follow-up hint.
Truly empty sections must still render `(none)`.

Add focused regressions for section-level retention under a constrained budget
and for the distinct text output of naturally empty versus budget-truncated
sections. Keep existing item caps, ranked order, and module-layer edge
disclosure intact unless the reserve design requires a narrow adjustment.

#### Out of scope

- Changing section builders, ranking heuristics, or their hard item caps.
- Adding new query modes or changing the JSON envelope shape.
- Re-auditing language-specific entrypoint detection.

#### Verification

- assert: `cargo test nav_map` → nav-map query and text-rendering tests pass, including new section-reserve and truncation-disclosure regressions.
- retrieve: `crates/varde-code/src/query/nav_map.rs` and `crates/varde-code/src/main.rs` → confirm retained sections remain budgeted and text distinguishes truncation from a true empty result.

#### Progress

- Reserved one fitting item per populated section before priority overflow.
- Rendered empty truncated arrays with their total and follow-up hint.
- Verified with `cargo test -p varde-code nav_map` and clippy.
