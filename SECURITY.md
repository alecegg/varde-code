# Security Policy

## Supported versions

varde-code is pre-1.0. Security fixes land on the latest `0.1.x` release only;
there is no back-porting to older tags. Always run the newest tagged release.

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |
| < 0.1   | :x:                |

## Reporting a vulnerability

Please report security issues **privately** — do not open a public issue for a
suspected vulnerability.

Use GitHub's private vulnerability reporting: go to the repository's
**Security** tab → **Report a vulnerability** (this opens a private advisory
visible only to the maintainers). See GitHub's
[private reporting docs](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
if you haven't used it before.

When reporting, please include:

- affected version (`varde-code --version`) and platform,
- a description of the issue and its impact,
- steps to reproduce (a minimal repo or input is ideal),
- any known workaround.

## What to expect

- We aim to acknowledge a report within **7 days**.
- If confirmed, we will work on a fix, keep you updated on progress, and
  credit you in the advisory/release notes unless you prefer to remain
  anonymous.
- Once a fix ships, the advisory is published.

## Scope notes

varde-code is a local CLI that reads source trees and maintains a local
SQLite index; it opens no network listeners and runs no server. The most
relevant threat surface is untrusted **input** — a malicious repository,
source file, or index database. Reports about parsing, extraction, or index
handling of hostile input are in scope. Denial-of-service via a
pathologically large repository (resource exhaustion on a repo you chose to
index) is generally out of scope.
