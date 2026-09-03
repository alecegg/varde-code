<!-- docs:v1 {"specs":{"cli":"crates/varde-code/src/cli.rs","lib":"crates/varde-code/src/lib.rs"}} -->

# varde-code

A native Rust code-intelligence engine: tree-sitter parsing, entity/symbol extraction, and a
SQLite-backed query/scan engine, exposed as a library plus a single `varde-code` CLI binary.

## Scope

- Multi-language parsing (tree-sitter) for JS/TS/TSX, Go, C, C++, C#, Java, Swift, Kotlin, Rust,
  Python, Ruby, PHP, Scala, Dart, Lua, Elixir, Solidity, Haskell, and Bash (Tier B / shell) (`crates/varde-code/src/extract/langs/`). These
  twenty-one are the *extraction/indexing* languages; structural `find_pattern` search additionally covers every
  grammar `ast-grep-language` links (HCL/Terraform, …)
- Entity/symbol extraction, resolution (imports/dependencies/type hierarchy), and complexity/churn
  metrics, persisted to a local SQLite index (`crates/varde-code/src/db`, `persist.rs`)
- A query engine (`query/`) exposed via the `varde-code` CLI (`cli.rs`) for lookups like
  `symbols_in_file`, `get_symbol`, `dependencies`/`dependents`, `hotspots`, `type_hierarchy`,
  `blast_radius`, and dependency-graph maps (`map_file`/`map_symbol`/`map_path`, `explore`)
- A pattern-matching scan/rules engine (`rules/`, `scan.rs`) for structural pattern search
  (`find_pattern`) and rule-based scanning (`scan`, `rules_list`)
- An optional background watcher (`watch`) that keeps a repo's index incrementally fresh
- Explicitly out of scope: no MCP server or long-running daemon beyond the optional watcher —
  this repo is a library plus a CLI binary

## Install

Prebuilt binaries (macOS arm64/x86_64, Linux x86_64/arm64) are attached to each
[tagged release](https://github.com/alecegg/varde-code/releases). Download the tarball for your
platform, extract it, and put `varde-code` on your `PATH`. Each tarball also contains
`THIRD-PARTY-LICENSES.md` — the attribution notices for the statically-linked dependencies.

Or build from source with Cargo (requires Rust 1.88+):

```sh
cargo install --path crates/varde-code
```

## Usage

Build an index for a repo, then query it:

```sh
varde-code build --repo-root .
varde-code hotspots --json '{"repoRoot": "."}'
varde-code symbols_in_file --json '{"repoRoot": ".", "filePath": "src/main.rs"}'
```

Every query subcommand takes a single `--json '<object>'` argument and prints a uniform
`{"ok": true, "data": ...}` / `{"ok": false, "error": {...}}` envelope to stdout. The index is
stored at `~/.config/varde-code/repos/<name>-<hash>/index.db`.

## Docs

- [docs/CLI.md](docs/CLI.md) — every subcommand, its JSON input, and what it does
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — module map, write/read data flow, watch and scan loops
- [BUILT_IN_RULES.md](BUILT_IN_RULES.md) — scope and schema for the built-in scan rule pack
- [BENCHMARK.md](BENCHMARK.md) — speed/quality tracking against `ast-grep` for overlapping modes
- [CHANGELOG.md](CHANGELOG.md) — notable changes per release

## Status

Beta. Core parsing/extraction/query/scan surfaces are covered by an extensive test suite and
gated by CI.
