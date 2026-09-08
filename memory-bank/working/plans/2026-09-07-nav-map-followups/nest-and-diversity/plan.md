---
id: 2026-09-07-nav-map-followups/nest-and-diversity
status: completed
depends_on: []
---

# Nest role precision and entrypoint diversity

Correct `@Injectable()` classification and round-robin nav-map entrypoints by language.

## Acceptance criteria

- [x] Nest services are not middleware entrypoints.
- [x] Default entrypoint maps retain detected language diversity.
