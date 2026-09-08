---
name: varde-code-rule-scan-triage
description: >
  TRIGGER: Run a varde-code scan, triage each finding as a real issue or a
  false positive, and fix the real ones. Use when the user asks to "run a
  scan and fix what's real", "check for false positives", "triage scan
  findings", or "clean up lint findings" for this repo or another one indexed
  by varde-code. SKIP: Skip when the user wants to author or edit a rule
  (`rule-authoring`), or wants raw scan output without triage/fixing.
  Example phrases: "run a rule scan and fix the real findings", "scan for
  issues and weed out false positives", "triage the last scan run".
allowed-tools: Bash Read Edit Grep
---

## What a scan finding is

A `varde-code scan` finding is a **candidate**, not a verdict — the rule
engine flags patterns that are usually a problem, but it cannot see the
context a human/agent pass can (test fixtures, intentional cycles, public
API re-exports, trait-mandated boilerplate). Do not treat findings as
"advisory" or optional to act on — that framing lets an agent skim past
them without a decision. Every finding gets an explicit classification:
**needs a fix** or **not a real issue (with a stated reason)**. Silence or
skipping is not a valid outcome for any finding — if you haven't looked at
it, it isn't triaged.

## Workflow

1. **Ensure a fresh index.** Run `varde-code build --json '{"repoRoot":"<repo>"}'` if the repo hasn't been built/indexed recently (or the user says code changed since the last scan). Skip if they confirm the index is current.

2. **Run the scan.**
   ```bash
   varde-code scan --json '{"repoRoot":"<repo>"}'
   ```
   Do not pass `--apply` at this stage — triage before touching any files.

3. **Group findings** by `rule_id`. Handle one rule at a time rather than one finding at a time — findings from the same rule usually share the same false-positive pattern, so the first triage decision often resolves the rest of that group at once.

4. **Triage each finding/group.** For each, read the actual code at `location.file`:`location.span` (or the line indicated) with the Read tool — never trust `message`/`evidence` alone. Every finding must land in one of two buckets — **needs a fix** or **not a real issue** — before you move on; there is no third "skip/advisory" bucket. Decide using:
   - **`certainty`** (pattern rules only, `High`/`Medium`/`Low` if present) — a hint, not a verdict. Low-certainty findings deserve closer reading; high-certainty ones can still be false positives if the rule's pattern is too broad for this call site.
   - **The rule's own intent** — read the rule definition (`varde-code rules list --json '{"repoRoot":"<repo>"}'` to locate it, then read the `.toml`) to understand what it's actually guarding against, so you're judging against intent rather than guessing from the message string.
   - **Context the rule can't see** — e.g. a "hardcoded credential" match that's actually a test fixture, an "unused export" that's a public API re-export, a "circular import" that's a documented intentional cycle. This is exactly what a human/agent pass adds over the static rule.
   - When genuinely unsure, ask the user rather than guessing — false-positive triage is a judgment call, and a wrong call in either direction (suppressing a real bug, or "fixing" correct code) is costly.

5. **Record every classification as you go** (a short running list: rule id, file:line, verdict, one-line reason) — for "not a real issue" verdicts especially, do not silently drop them; they're part of the deliverable, not noise to discard.

6. **Fix the real findings.**
   - If the rule sets `rewrite` and the finding is a straightforward pattern match, prefer `varde-code scan --apply --json '{"repoRoot":"<repo>"}'` for that rule's findings rather than hand-editing — it's the same transform, applied consistently, and gated on a clean git tree (add `--force` only if the user explicitly wants to override the dirty-tree check, and say so).
   - Otherwise fix by hand with Edit, following `agent_instructions`/`remediation` from the finding as a starting point, not a script to paste verbatim — verify the fix actually addresses the flagged code.
   - Group fixes for the same rule together; re-read surrounding code before editing so the fix fits the file's existing style.

7. **Verify.** Re-run `varde-code scan --json '{"repoRoot":"<repo>"}'` after fixes and confirm the addressed findings are gone and nothing new was introduced. If the project has a test/build command (check `AGENTS.md`/`CLAUDE.md`), run it to confirm the fixes didn't break anything.

8. **Report a summary covering every finding from the scan**, not just the ones fixed: findings fixed (rule id + file:line), findings judged not a real issue (with the one-line reason from step 5), and anything left for the user to decide. A finding that appears in neither list is a sign triage was incomplete — go back and classify it.

## Gotchas

- `scan --apply` only touches findings whose rule has a `rewrite` template; it silently skips (with `rewrite_status`) everything else — don't assume `--apply` alone resolves a rule group.
- `scan --apply` refuses to touch files with uncommitted changes unless `--force` is passed (dirty-tree safety gate) — surface this to the user rather than reaching for `--force` by default.
- A finding's `evidence`/`message` is generated from the rule template, not a proof of correctness — always read the real code before deciding real vs. false positive, especially for `sql` rules where the message may summarize an aggregate across multiple locations.
- If the same not-a-real-issue pattern recurs across many findings from one rule, that's a signal the rule itself may need tightening (see the `rule-authoring` skill) rather than something to triage finding-by-finding forever — flag this to the user instead of repeatedly making the same judgment call.
- Never mark a finding as not a real issue just because fixing it is inconvenient — the bar is "the flagged code is not actually the problem the rule describes," not "the fix is annoying."
- A large finding count is not a reason to sample or triage "the important-looking ones" — group by rule to make the volume tractable (step 3), but every finding still needs a verdict. Treating scan output as advisory-level noise to skim is exactly the failure mode this skill exists to prevent.
