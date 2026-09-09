---
type: task
parent: 2026-09-09-isp-auto-property-accessors
status: done
verified: passed
depends_on: []
modifies:
  - crates/varde-code/src/extract/langs/cs.rs
  - crates/varde-code/tests/scan_cli.rs
creates: []
---

Exclude generated C# property accessors from functions

Emit function entities only for accessor declarations with bodies.
Keep explicit block and arrow accessor scopes intact.
Add a valid C# end-to-end ISP regression.

#### Out of scope

- SOLID rule SQL changes.
- Other language extractors.

#### Verification

- assert: cargo test -p varde-code --test scan_cli → valid C# auto-properties do not trigger solid-isp.
- assert: cargo test -p varde-code → existing explicit C# accessor coverage remains green.
- retrieve: crates/varde-code/src/extract/langs/cs.rs → bodyless accessors do not create Function entities or function scopes.

#### Progress

- Excluded bodyless accessors; added C# ISP and explicit-accessor coverage.
