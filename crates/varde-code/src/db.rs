//! SQLite persistence: schema DDL and database open/rebuild scaffold.
//!
//! Own schema, own DB path — independent of varde's 28-table
//! `intelligence.db` shape. Known-dead tables from varde
//! (`health_findings`, `health_markers`, `health_run_fingerprint`) are
//! deliberately not part of this schema.
//!
//! Write model: full rebuild per run. [`open_or_rebuild`] drops every table
//! and index (if present) and recreates the schema, so a second call against
//! the same path succeeds with no duplicate-table errors.

use std::path::Path;

use anyhow::Result;

pub mod path;

/// Bumped whenever `schema_ddl` changes shape (a new/removed/retyped
/// column, table, or index) — or when the encoding of a persisted blob
/// column changes, since the on-disk bytes then become undecodable by the
/// current code just as surely as a shape change would. Stored in SQLite's
/// built-in `user_version` pragma (part of the file header — readable
/// without touching any table, so it works even against a schema from
/// before this column existed).
/// [`run_with_force`](crate::build::run_with_force)'s incremental path
/// checks the on-disk value against this constant before trusting the
/// existing database; a mismatch forces a full rebuild instead of writing
/// delta rows against a shape the current code doesn't expect. A
/// never-versioned database (any db written before this pragma was
/// introduced) reads back `user_version = 0`, which mismatches the first
/// nonzero version unconditionally — so the first `build` after this ships
/// self-heals every existing index once, with no manual `rm` required.
///
/// - v12: `graph_cache.blob` re-encoded with `postcard` (was `bincode`);
///   forces old caches to rebuild rather than be misread by the new decoder.
/// - v13: `is_test_path` recognizes C#/.NET conventions (`*Test.cs`/
///   `*Tests.cs` files and `*.Test`/`*.Tests`/`*.UnitTests`/… project dirs);
///   the generated column is STORED, so old DBs must rebuild to recompute it.
/// - v14: `is_test_path` extends flat-filename test conventions to C++
///   (`*_test.cc`/`.cpp`/`.cxx`), Solidity (`*.t.sol`), Bash (`*.bats`), and
///   Ruby (`*_spec.rb`/`*_test.rb`); same STORED-column rebuild requirement.
/// - v15: extraction emits `EntityKind::TypeRef` rows (a field/param/local's
///   declared type) that type-directed call resolution reads; force a rebuild
///   so existing indexes gain them instead of resolving with partial data.
pub const SCHEMA_VERSION: i64 = 15;

/// All tables in the schema, in a deterministic drop order (junction tables
/// before the tables they reference, so `DROP TABLE IF EXISTS` never trips a
/// foreign-key constraint even if FK enforcement were enabled).
pub const TABLES: [&str; 12] = [
    "graph_cache",
    "slice_state",
    "slice_meta",
    "community_members",
    "clone_band_members",
    "resolved_edges",
    "diagnostics",
    "symbols",
    "entities",
    "communities",
    "clone_bands",
    "files",
];

/// DDL executed on a fresh database.
///
/// Column layout follows the plan's drafted schema (one table per
/// struct/collection from `ExtractOutput`/`ResolvedGraph`, junction tables
/// for many-to-many membership, fan-in/fan-out denormalized onto `files`).
///
/// `pub(crate)` so query-layer unit tests can stand up the real schema —
/// including generated columns like `is_test_path` — in an in-memory
/// connection instead of hand-rolling a partial `files` table that drifts
/// from production.
pub(crate) fn schema_ddl() -> &'static str {
    r#"
CREATE TABLE files (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    path TEXT NOT NULL UNIQUE,
    -- Derived from `path` alone (test/tests/__tests__/spec/specs/fixtures
    -- directory segments, or a .test./.spec. filename infix), so it's a
    -- SQLite generated column rather than a value threaded through
    -- extract/persist: no extraction-time work, and it can't drift out of
    -- sync with path. Shared by every scan rule that wants to exclude
    -- test/fixture code from a structural-smell signal (see
    -- low_fan_in_high_fan_out_file.toml, circular_import.toml,
    -- debug_statement_strict.toml) instead of each rule reimplementing its
    -- own path heuristic.
    is_test_path BOOLEAN GENERATED ALWAYS AS (
        path LIKE '%/test/%' OR path LIKE '%/tests/%'
        OR path LIKE '%/__tests__/%' OR path LIKE '%/spec/%' OR path LIKE '%/specs/%'
        OR path LIKE '%/fixture/%' OR path LIKE '%/fixtures/%'
        OR path LIKE '%.test.%' OR path LIKE '%.spec.%'
        OR path LIKE 'test/%' OR path LIKE 'tests/%'
        -- Go convention: `foo_test.go` files live alongside the code they
        -- test, not under a `test/`-segment directory.
        OR path LIKE '%\_test.go' ESCAPE '\'
        -- Python convention: `test_foo.py` / `foo_test.py` at any depth.
        OR path LIKE '%/test\_%.py' ESCAPE '\' OR path LIKE 'test\_%.py' ESCAPE '\'
        OR path LIKE '%\_test.py' ESCAPE '\'
        -- Java/Kotlin convention: `FooTest.java` / `FooTests.java` outside a
        -- `test/`-segment directory (e.g. flat single-module layouts).
        OR path LIKE '%Test.java' OR path LIKE '%Tests.java'
        OR path LIKE '%Test.kt' OR path LIKE '%Tests.kt'
        -- C#/.NET convention: `FooTests.cs` files, plus the dominant .NET
        -- solution layout of a separate `<Project>.Tests` test project. The
        -- `.Tests/` directory segment isn't caught by `%/tests/%` above
        -- (the separator before `Tests` is `.`, not `/`), so match the
        -- `.<suffix>/` project-dir forms explicitly. Bounding with the `.`
        -- avoids false hits like `Contests/` that a bare `%Tests/%` would take.
        OR path LIKE '%Test.cs' OR path LIKE '%Tests.cs'
        OR path LIKE '%.Test/%' OR path LIKE '%.Tests/%'
        OR path LIKE '%.UnitTests/%' OR path LIKE '%.IntegrationTests/%'
        OR path LIKE '%.FunctionalTests/%' OR path LIKE '%.AcceptanceTests/%'
        -- C++ convention: GoogleTest `foo_test.cc`/`.cpp`/`.cxx` files live
        -- alongside the code they test (like Go's `_test.go`), not under a
        -- `test/`-segment directory. The escaped `_` requires a literal
        -- underscore, so `latest.cpp` is not swept up.
        OR path LIKE '%\_test.cc' ESCAPE '\' OR path LIKE '%\_test.cpp' ESCAPE '\'
        OR path LIKE '%\_test.cxx' ESCAPE '\'
        -- Solidity convention: Foundry test contracts use the `.t.sol` double
        -- extension (`Counter.t.sol`); this is the runner's discovery signal
        -- and files may sit outside a `test/` dir. Scripts use `.s.sol` and
        -- are intentionally left unmatched.
        OR path LIKE '%.t.sol'
        -- Bash convention: Bats test files carry the `.bats` extension.
        OR path LIKE '%.bats'
        -- Ruby convention: RSpec `foo_spec.rb` / Minitest `foo_test.rb`
        -- outside the usual `spec/`/`test/` dirs (already caught above). The
        -- escaped `_` keeps `latest.rb` out.
        OR path LIKE '%\_spec.rb' ESCAPE '\' OR path LIKE '%\_test.rb' ESCAPE '\'
    ) STORED,
    -- Non-test tooling that legitimately behaves differently from shipped
    -- production code: benchmark harnesses, build/dev scripts, and docs
    -- generators. `console-log-strict`/`duplicate-code-clone` treat this the
    -- same as `is_test_path` — see debug_statement_strict.toml and
    -- duplicate_code_clone.toml — kept as a separate column instead of
    -- folding into `is_test_path` so "is this a test" and "is this shipped"
    -- stay independently queryable.
    is_tooling_path BOOLEAN GENERATED ALWAYS AS (
        path LIKE '%/bench/%' OR path LIKE '%/benches/%' OR path LIKE '%/benchmark/%' OR path LIKE '%/benchmarks/%'
        OR path LIKE 'bench/%' OR path LIKE 'benches/%'
        OR path LIKE '%/scripts/%' OR path LIKE '%/script/%' OR path LIKE 'scripts/%' OR path LIKE 'script/%'
        OR path LIKE '%/docs/%' OR path LIKE 'docs/%'
        OR path LIKE '%/.claude/%' OR path LIKE '.claude/%'
    ) STORED,
    mtime INTEGER NOT NULL DEFAULT 0,
    size INTEGER NOT NULL DEFAULT 0,
    content_hash TEXT NOT NULL DEFAULT '',
    complexity INTEGER,
    churn INTEGER,
    fan_in INTEGER NOT NULL DEFAULT 0,
    fan_out INTEGER NOT NULL DEFAULT 0,
    community_id INTEGER,
    rev INTEGER NOT NULL DEFAULT 0,
    edges_built_rev INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE entities (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind INTEGER NOT NULL,
    name TEXT NOT NULL,
    file_id INTEGER NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    start_line INTEGER NOT NULL,
    start_col INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    end_col INTEGER NOT NULL,
    enclosing_function TEXT,
    method TEXT,
    path TEXT,
    status TEXT,
    body_shape TEXT,
    body_minhash BLOB,
    is_async BOOLEAN,
    is_test BOOLEAN NOT NULL DEFAULT 0,
    -- Function entities only: name of the immediately enclosing
    -- class/interface/impl-target type, when the function is a method.
    -- Drives SOLID class-membership rules (interface method-coverage,
    -- fat-interface member counts) that need "which methods belong to
    -- this type" without span-containment math.
    owner_type TEXT
);

CREATE TABLE symbols (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind INTEGER NOT NULL,
    name TEXT NOT NULL,
    file_id INTEGER NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    start_line INTEGER NOT NULL,
    start_col INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    end_col INTEGER NOT NULL
);

CREATE TABLE diagnostics (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    file_id INTEGER NOT NULL,
    message TEXT NOT NULL,
    severity TEXT NOT NULL
);

CREATE TABLE resolved_edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    from_file_id INTEGER NOT NULL,
    to_file_id INTEGER,
    kind INTEGER NOT NULL,
    resolved INTEGER NOT NULL,
    from_entity_id INTEGER,
    to_entity_id INTEGER
);

CREATE TABLE communities (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    label TEXT NOT NULL
);

CREATE TABLE community_members (
    community_id INTEGER NOT NULL,
    file_id INTEGER NOT NULL
);

CREATE TABLE clone_bands (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    label TEXT NOT NULL
);

CREATE TABLE clone_band_members (
    band_id INTEGER NOT NULL,
    entity_id INTEGER NOT NULL
);

CREATE TABLE slice_state (
    slice TEXT PRIMARY KEY,
    built_through_rev INTEGER NOT NULL,
    schema_version INTEGER NOT NULL
);

CREATE TABLE slice_meta (
    key TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);

CREATE TABLE graph_cache (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    blob BLOB NOT NULL,
    rev INTEGER NOT NULL
);
    "#
}

/// Indexes covering `query-surface`'s O(1)/O(log N)-per-traversal
/// acceptance criteria: `dependencies`/`dependents`/`blast_radius` traverse
/// `resolved_edges` from either end; `symbols_in_file`-style lookups filter
/// `entities`/`symbols` by file; `entities`/`symbols` are also looked up by
/// `name` (get_symbol, type_hierarchy, map_symbol) — without an index that
/// degrades to a full table scan on every call.
const INDEX_DDL: &str = r#"
CREATE INDEX idx_resolved_edges_from ON resolved_edges(from_file_id);
CREATE INDEX idx_resolved_edges_to ON resolved_edges(to_file_id);
CREATE INDEX idx_resolved_edges_from_entity ON resolved_edges(from_entity_id);
CREATE INDEX idx_resolved_edges_to_entity ON resolved_edges(to_entity_id);
CREATE INDEX idx_entities_file ON entities(file_id);
CREATE INDEX idx_symbols_file ON symbols(file_id);
CREATE INDEX idx_entities_name ON entities(name);
CREATE INDEX idx_symbols_name ON symbols(name);
"#;

/// Open the database at `path` without touching the schema.
///
/// Pragmas trade durability for write throughput: `synchronous=OFF` skips
/// the fsyncs SQLite normally does per-transaction, and `journal_mode=OFF`
/// disables the rollback journal entirely (not `MEMORY` — that still holds
/// the full journal in RAM for the transaction's lifetime, which on a
/// multi-million-row single-transaction write competes with the
/// already-large in-memory `entities`/`symbols` vectors for RAM and can
/// drive the system into swap; `OFF` needs no journal memory at all). Safe
/// here because `persist` always writes a full rebuild in one transaction —
/// a crash mid-write leaves the prior file either untouched or truncated,
/// and either way the fix is just to re-run `build`, never a manual repair.
/// The incremental/freshen path writes *in place* (no temp+rename) and so
/// needs safe rollback on error; it uses [`open_incremental`] (`MEMORY`
/// journal) instead, where the small delta makes the RAM cost negligible.
///
/// Two further RAM-neutral tuning pragmas (see `PERFORMANCE_AUDIT.md` §4.8):
/// `page_size=16384` (fewer page splits on the bulk load; it only takes
/// effect while the DB is still empty, so it is set before every other
/// pragma that could touch the header) and `locking_mode=EXCLUSIVE`
/// (single-writer build drops the shared-lock overhead). Both are no-ops on
/// an already-populated database (page size is fixed once page 1 exists, and
/// EXCLUSIVE only ever applies to this connection's lifetime).
/// See `BENCHMARK.md`.
pub fn open(path: &Path) -> Result<rusqlite::Connection> {
    open_tuned(path, "OFF")
}

/// Like [`open`], but with `journal_mode=MEMORY` instead of `OFF`.
///
/// Under `journal_mode=OFF` there is no rollback journal at all, so a
/// `ROLLBACK` — including the implicit one rusqlite issues when a transaction
/// is dropped on an error path — cannot restore the prior state and leaves the
/// database corrupt. `MEMORY` keeps the rollback journal in RAM, so a
/// transaction on this connection aborts cleanly on error instead of splicing
/// a half-applied write into the live index.
///
/// This is used by the incremental build and the `slice` fresheners, whose
/// transactions touch only the small changed delta — the multi-million-row
/// journal-in-RAM concern that keeps the full build on `OFF` (see [`open`])
/// does not apply. The residual gap is a *power-loss* crash mid-commit, which
/// loses the RAM journal; that is recoverable with `build --force` (a full
/// rebuild via temp-file + atomic rename), same posture as before.
pub fn open_incremental(path: &Path) -> Result<rusqlite::Connection> {
    open_tuned(path, "MEMORY")
}

fn open_tuned(path: &Path, journal_mode: &str) -> Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(path)?;
    conn.pragma_update(None, "page_size", 16384i64)?;
    conn.pragma_update(None, "synchronous", "OFF")?;
    conn.pragma_update(None, "journal_mode", journal_mode)?;
    conn.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
    Ok(conn)
}

/// Open the database at `path` read-only — SQLite itself enforces
/// read-only-ness at the connection level (`OpenFlags`), so a rule file can
/// query but never mutate the intelligence DB. Does not touch the schema;
/// errors (with the path in the message) when the DB is absent/unopenable.
pub fn open_read_only(path: &Path) -> anyhow::Result<rusqlite::Connection> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(conn)
}

/// Open the database at `path`, dropping and recreating the full schema.
///
/// Full-rebuild semantics: any prior data is wiped. Safe to call on a fresh
/// path, an existing empty file, or a path with a previously applied schema.
pub fn open_or_rebuild(path: &Path) -> Result<rusqlite::Connection> {
    let conn = open(path)?;
    rebuild_schema(&conn)?;
    create_indexes(&conn)?;
    Ok(conn)
}

/// Drop all tables/indexes (if present) and recreate them.
///
/// Indexes are dropped implicitly with their tables, so dropping the nine
/// tables in `TABLES` order is sufficient; the DDL then recreates both tables
/// and indexes.
///
/// Indexes are deliberately NOT created here — see [`create_indexes`], which
/// callers run after bulk data writes so SQLite bulk-builds each index once
/// instead of maintaining its B-tree incrementally across every insert.
pub(crate) fn rebuild_schema(conn: &rusqlite::Connection) -> Result<()> {
    for table in TABLES {
        conn.execute(&format!("DROP TABLE IF EXISTS {table}"), [])?;
    }
    conn.execute_batch(schema_ddl())?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

/// Read the schema version stamped on an existing database (0 if it predates
/// this pragma being set). Used by the incremental build path to detect a
/// schema drift before trusting the database for delta writes.
pub(crate) fn schema_version(conn: &rusqlite::Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// Fingerprint of the varde-code build that produced (or is about to produce)
/// an index, stored in `slice_meta` under `build_version`.
///
/// [`SCHEMA_VERSION`] only changes when the on-disk *shape* changes, and it is
/// bumped by hand — so an extractor/resolver change that alters index *content*
/// without touching the schema (e.g. Python methods gaining `owner_type`)
/// leaves `SCHEMA_VERSION` untouched and the incremental build serves stale
/// rows until some source file happens to change. This fingerprint is the
/// automatic complement: it changes whenever the binary itself changes, so the
/// incremental gate rebuilds after any upgrade or recompile with no version
/// discipline required.
///
/// Composed from the crate version plus the running executable's size + mtime
/// (a recompile or reinstall changes at least one). It is deterministic within
/// a single binary — a build then an incremental build by the *same* binary
/// hash identically, so no spurious rebuild — and differs across binaries,
/// which is exactly when a rebuild is wanted. If the executable can't be
/// resolved, it degrades to the crate version alone (still catches releases).
/// Hashed with the same FNV-1a as `slice::head_fingerprint` so it fits the
/// integer-valued `slice_meta` store.
pub fn build_version_fingerprint() -> i64 {
    let exe_fp = std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|meta| {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("{mtime}:{}", meta.len())
        })
        .unwrap_or_default();
    let fingerprint = format!("{}:{exe_fp}", env!("CARGO_PKG_VERSION"));

    // FNV-1a, matching `slice::head_fingerprint` (kept local to avoid a
    // db -> slice layering dependency).
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in fingerprint.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    h as i64
}

/// Build the `entities`/`symbols`/`resolved_edges` lookup indexes.
///
/// Run this after bulk-inserting `entities`, `symbols`, and `resolved_edges`
/// (not before, and not interleaved) — building an index over an
/// already-populated table is one bulk sort-and-load, while creating it
/// beforehand forces SQLite to update the B-tree on every single insert.
pub(crate) fn create_indexes(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(INDEX_DDL)?;
    Ok(())
}

#[cfg(test)]
mod schema_scaffold {
    use super::*;

    /// Table name -> expected columns (subset checked in tests).
    fn table_names(conn: &rusqlite::Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .expect("sqlite_master query prepares");
        stmt.query_map([], |row| row.get::<_, String>(0))
            .expect("query maps")
            .map(|r| r.expect("row ok"))
            .collect()
    }

    fn index_names(conn: &rusqlite::Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .expect("sqlite_master index query prepares");
        stmt.query_map([], |row| row.get::<_, String>(0))
            .expect("query maps")
            .map(|r| r.expect("row ok"))
            .collect()
    }

    #[test]
    fn is_test_path_recognizes_csharp_and_dotnet_conventions() {
        let dir =
            std::env::temp_dir().join(format!("varde-schema-{}-testpath", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        let db_path = dir.join("tp.db");
        let _ = std::fs::remove_file(&db_path);
        let conn = open_or_rebuild(&db_path).expect("open succeeds");

        let cases: &[(&str, bool)] = &[
            // .NET test-project layout + `*Tests.cs` suffix (the Newtonsoft
            // shape that was slipping through and getting smell-flagged).
            ("/repo/Src/Newtonsoft.Json.Tests/BsonReaderTests.cs", true),
            ("/repo/src/App.UnitTests/Helpers.cs", true),
            ("/repo/src/Foo.IntegrationTests/Bar.cs", true),
            ("/repo/src/Widget.Test/WidgetTest.cs", true),
            // Production C# must NOT be flagged, including tricky near-misses
            // a bare `%Tests/%` would wrongly catch.
            ("/repo/Src/Newtonsoft.Json/JsonReader.cs", false),
            ("/repo/src/Contests/Leaderboard.cs", false),
            ("/repo/src/GreatestHits.cs", false),
        ];
        let mut insert = conn
            .prepare("INSERT INTO files (path) VALUES (?1)")
            .expect("prepare insert");
        for (path, _) in cases {
            insert
                .execute(rusqlite::params![path])
                .unwrap_or_else(|e| panic!("insert {path}: {e}"));
        }
        for (path, expected) in cases {
            let got: bool = conn
                .query_row(
                    "SELECT is_test_path FROM files WHERE path = ?1",
                    rusqlite::params![path],
                    |r| r.get(0),
                )
                .expect("read is_test_path");
            assert_eq!(got, *expected, "is_test_path for {path}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_test_path_recognizes_additional_language_conventions() {
        let dir =
            std::env::temp_dir().join(format!("varde-schema-{}-testpath2", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        let db_path = dir.join("tp2.db");
        let _ = std::fs::remove_file(&db_path);
        let conn = open_or_rebuild(&db_path).expect("open succeeds");

        let cases: &[(&str, bool)] = &[
            // C++ GoogleTest: tests live alongside the code (not in a `test/`
            // dir), matching Go's `_test.go` shape.
            ("/repo/src/widget_test.cc", true),
            ("/repo/src/widget_test.cpp", true),
            ("/repo/src/widget_test.cxx", true),
            // The escaped `_` must not sweep up production files that merely
            // end in `test.<ext>`.
            ("/repo/src/latest.cpp", false),
            ("/repo/src/widget.cc", false),
            // Solidity Foundry `.t.sol` (scripts use `.s.sol` and stay clean).
            ("/repo/test/Counter.t.sol", true),
            ("/repo/src/Counter.t.sol", true),
            ("/repo/script/Deploy.s.sol", false),
            ("/repo/src/Counter.sol", false),
            // Bash Bats.
            ("/repo/cli/install.bats", true),
            ("/repo/cli/install.sh", false),
            // Ruby RSpec/Minitest flat files.
            ("/repo/lib/user_spec.rb", true),
            ("/repo/lib/user_test.rb", true),
            ("/repo/lib/latest.rb", false),
            ("/repo/lib/user.rb", false),
        ];
        let mut insert = conn
            .prepare("INSERT INTO files (path) VALUES (?1)")
            .expect("prepare insert");
        for (path, _) in cases {
            insert
                .execute(rusqlite::params![path])
                .unwrap_or_else(|e| panic!("insert {path}: {e}"));
        }
        for (path, expected) in cases {
            let got: bool = conn
                .query_row(
                    "SELECT is_test_path FROM files WHERE path = ?1",
                    rusqlite::params![path],
                    |r| r.get(0),
                )
                .expect("read is_test_path");
            assert_eq!(got, *expected, "is_test_path for {path}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_path_creates_all_tables_and_indexes() {
        let dir = std::env::temp_dir().join(format!("varde-schema-{}-fresh", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        let db_path = dir.join("fresh.db");
        let _ = std::fs::remove_file(&db_path);

        let conn = open_or_rebuild(&db_path).expect("open succeeds on fresh path");

        let tables = table_names(&conn);
        for expected in [
            "files",
            "entities",
            "symbols",
            "diagnostics",
            "resolved_edges",
            "communities",
            "community_members",
            "clone_bands",
            "clone_band_members",
            "slice_state",
            "slice_meta",
            "graph_cache",
        ] {
            assert!(
                tables.contains(&expected.to_string()),
                "missing table {expected}"
            );
        }

        let indexes = index_names(&conn);
        for expected in [
            "idx_resolved_edges_from",
            "idx_resolved_edges_to",
            "idx_entities_file",
            "idx_symbols_file",
            "idx_entities_name",
            "idx_symbols_name",
        ] {
            assert!(
                indexes.contains(&expected.to_string()),
                "missing index {expected}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolved_edges_has_to_entity_id_column_and_schema_version_is_current() {
        let dir =
            std::env::temp_dir().join(format!("varde-schema-{}-to-entity-id", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        let db_path = dir.join("to_entity_id.db");
        let _ = std::fs::remove_file(&db_path);

        let conn = open_or_rebuild(&db_path).expect("open succeeds on fresh path");

        let mut stmt = conn
            .prepare("PRAGMA table_info(resolved_edges)")
            .expect("table_info prepares");
        let columns: Vec<(String, i64)> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, i64>(3)?))
            })
            .expect("query maps")
            .map(|r| r.expect("row ok"))
            .collect();
        let to_entity_id = columns
            .iter()
            .find(|(name, _)| name == "to_entity_id")
            .expect("to_entity_id column present");
        assert_eq!(
            to_entity_id.1, 0,
            "to_entity_id must be nullable (notnull = 0)"
        );

        // Nullable: inserting a row without to_entity_id must succeed.
        conn.execute(
            "INSERT INTO resolved_edges (from_file_id, kind, resolved) VALUES (1, 0, 0)",
            [],
        )
        .expect("insert without to_entity_id succeeds");

        assert_eq!(
            schema_version(&conn).expect("schema_version reads"),
            SCHEMA_VERSION
        );
        assert_eq!(
            SCHEMA_VERSION, 15,
            "schema version bumped for TypeRef extraction (type-directed call resolution)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn second_open_rebuilds_without_duplicate_table_errors() {
        let dir = std::env::temp_dir().join(format!("varde-schema-{}-rebuild", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        let db_path = dir.join("rebuild.db");
        let _ = std::fs::remove_file(&db_path);

        // First open creates the schema.
        {
            let conn = open_or_rebuild(&db_path).expect("first open succeeds");
            conn.execute("INSERT INTO files (path) VALUES ('seed.txt')", [])
                .expect("seed row inserts");
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
                .expect("count query works");
            assert_eq!(count, 1);
        }

        // Second open drops and recreates: no duplicate-table errors, data wiped.
        let conn =
            open_or_rebuild(&db_path).expect("second open succeeds without duplicate-table errors");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .expect("count query works");
        assert_eq!(count, 0, "full-rebuild wipe leaves no stale rows");
        assert!(table_names(&conn).len() >= 9);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dead_health_tables_absent() {
        let dir = std::env::temp_dir().join(format!("varde-schema-{}-health", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        let db_path = dir.join("health.db");
        let _ = std::fs::remove_file(&db_path);

        let conn = open_or_rebuild(&db_path).expect("open succeeds");
        let tables = table_names(&conn);
        for dead in [
            "health_findings",
            "health_markers",
            "health_run_fingerprint",
        ] {
            assert!(
                !tables.contains(&dead.to_string()),
                "dead table {dead} must not exist"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn open_read_only_opens_valid_db_and_rejects_writes() {
        let dir = std::env::temp_dir().join(format!("varde-ro-{}-valid", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        let db_path = dir.join("ro.db");
        let _ = std::fs::remove_file(&db_path);

        let conn = open_or_rebuild(&db_path).expect("writable open succeeds");
        drop(conn);

        let ro = open_read_only(&db_path).expect("read-only open succeeds");
        let err = ro
            .execute("INSERT INTO files (path) VALUES ('x')", [])
            .expect_err("write attempt fails on a read-only connection");
        assert!(
            err.to_string().contains("readonly"),
            "SQLite reports the read-only constraint: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_read_only_missing_path_errors_with_path_in_message() {
        let dir = std::env::temp_dir().join(format!("varde-ro-{}-missing", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        let missing = dir.join("absent.db");
        let err = open_read_only(&missing).expect_err("missing db is an error, not a panic");
        assert!(
            err.to_string().contains(&missing.display().to_string()),
            "error names the path: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
