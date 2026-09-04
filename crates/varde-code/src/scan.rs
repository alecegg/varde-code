//! Directory walking with skip-and-report semantics.
//!
//! Contract (plan): unparseable files (syntax error, unsupported/binary,
//! tree-sitter failure) are skipped and reported as per-file diagnostics; the
//! run continues and exits 0 unless a fatal (non-per-file) error occurs — a
//! missing/unreadable root path is fatal.

use crate::extract;
use crate::model::{Diagnostic, ExtractOutput, FileMeta};
use crate::parse::{language_for_path, parse_source};
use anyhow::Result;
use rayon::prelude::*;
use std::path::Path;

/// Sentinel used when filesystem metadata cannot be trusted.
pub const UNKNOWN_METADATA: i64 = -1;

/// Metadata collected for a file in the current source listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub mtime: i64,
    pub size: i64,
    pub content_hash: Option<String>,
}

/// List files and collect metadata without reading file contents.
///
/// The directory walk runs in parallel (`WalkBuilder::build_parallel`) and each
/// entry's cached `file_type()` (from `readdir`, no syscall) decides file-ness,
/// with `entry.metadata()` supplying mtime/size — one stat per file instead of
/// the previous `path().is_file()` + `fs::metadata` double-stat.
pub fn list_source_files(path: &str) -> Result<Vec<SourceFile>> {
    let root = Path::new(path);
    if !root.exists() {
        return Err(anyhow::anyhow!("path does not exist: {path}"));
    }

    if root.is_file() {
        return Ok(vec![source_file(root)]);
    }

    let collected = std::sync::Mutex::new(Vec::<SourceFile>::new());
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|entry| !is_vcs_internal(entry))
        .build_parallel()
        .run(|| {
            Box::new(|result| {
                if let Ok(entry) = result
                    && entry.file_type().is_some_and(|ft| ft.is_file())
                {
                    collected
                        .lock()
                        .expect("walk collector lock poisoned")
                        .push(source_file_from_entry(&entry));
                }
                ignore::WalkState::Continue
            })
        });

    let mut files = collected
        .into_inner()
        .expect("walk collector lock poisoned");
    files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Prune VCS-internal directories from the walk. `.hidden(false)` (set on the
/// walkers so legitimate dotfiles like `.github/` and `.eslintrc` are indexed)
/// otherwise descends into `.git/`, which holds machine internals — refs,
/// hooks, logs, and potentially thousands of loose objects plus multi-megabyte
/// packfiles. Indexing those pollutes the `files` table (observed: ~8% of rows
/// on a real repo, >90% on a freshly-committed one), inflates file counts that
/// feed nav_map/clusters, and wastes the walk. A submodule's `.git` is a
/// gitlink *file*, also named `.git`, so matching the name (not just dirs)
/// prunes both. This is deliberately narrow — only `.git`, the one dotdir that
/// is never source — to preserve the dotfile-including policy above.
pub(crate) fn is_vcs_internal(entry: &ignore::DirEntry) -> bool {
    entry.file_name() == std::ffi::OsStr::new(".git")
}

/// Extract `(mtime, size)` from a `Metadata`, with `UNKNOWN_METADATA` sentinels
/// for any field that can't be trusted (nanos/len overflow, bad timestamp).
fn metadata_mtime_size(metadata: &std::fs::Metadata) -> (i64, i64) {
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(UNKNOWN_METADATA);
    let size = i64::try_from(metadata.len()).unwrap_or(UNKNOWN_METADATA);
    (mtime, size)
}

/// Build a `SourceFile` from a single path (single-file input case).
fn source_file(path: &Path) -> SourceFile {
    let (mtime, size) = std::fs::metadata(path)
        .ok()
        .map(|metadata| metadata_mtime_size(&metadata))
        .unwrap_or((UNKNOWN_METADATA, UNKNOWN_METADATA));

    source_file_with_metadata(path, mtime, size)
}

/// Build a `SourceFile` from a walker entry, reusing the entry's cached
/// metadata (one `stat` total). `content_hash` stays `None` — it is computed at
/// scan time from the bytes actually read, never here.
fn source_file_from_entry(entry: &ignore::DirEntry) -> SourceFile {
    let path = entry.path().display().to_string();
    let (mtime, size) = entry
        .metadata()
        .ok()
        .map(|metadata| metadata_mtime_size(&metadata))
        .unwrap_or((UNKNOWN_METADATA, UNKNOWN_METADATA));
    SourceFile {
        path,
        mtime,
        size,
        content_hash: None,
    }
}

fn source_file_with_metadata(path: &Path, mtime: i64, size: i64) -> SourceFile {
    let content_hash = if mtime == UNKNOWN_METADATA && size == UNKNOWN_METADATA {
        std::fs::read(path)
            .ok()
            .map(|contents| content_hash(&contents))
    } else {
        None
    };

    SourceFile {
        path: path.display().to_string(),
        mtime,
        size,
        content_hash,
    }
}

fn content_hash(contents: &[u8]) -> String {
    const FNV_OFFSET_BASIS: u64 = 14_695_981_039_346_656_037;
    const FNV_PRIME: u64 = 1_099_511_628_211;
    let hash = contents.iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    });
    format!("{hash:016x}")
}

/// Walk a file or directory tree and extract everything parseable.
///
/// Directory walking uses `ignore::WalkBuilder` (same crate + `.hidden(false)`
/// policy as `find_pattern`'s directory walk, for the same reason: match
/// `ast-grep`'s observed traversal rather than a hand-rolled recursive
/// `read_dir` that blanket-skips dot-prefixed entries) and per-file
/// parse+extract runs in parallel via `rayon`, since it was previously fully
/// sequential and was the dominant cost in `build`'s scan phase — see
/// `BENCHMARK.md`.
pub fn run(path: &str) -> Result<ExtractOutput> {
    tracing::info!(file = path, "extracting");
    let profile = std::env::var_os("VARDE_PROFILE").is_some();
    let t = std::time::Instant::now();
    let files = list_source_files(path)?;
    if profile {
        eprintln!(
            "VARDE_PROFILE scan: list_source_files done at {:?} ({} files)",
            t.elapsed(),
            files.len()
        );
    }

    let mut output = ExtractOutput {
        entities: Vec::new(),
        symbols: Vec::new(),
        diagnostics: Vec::new(),
        files: Vec::with_capacity(files.len()),
        file_meta: Vec::with_capacity(files.len()),
    };

    // Parse + extract in parallel; each file also yields its scan-time
    // content hash so persistence never re-reads the bytes.
    let processed: Vec<ProcessedFile> = files
        .par_iter()
        .enumerate()
        .map(|(i, file)| process_file(Path::new(&file.path), i as u32))
        .collect();
    if profile {
        eprintln!(
            "VARDE_PROFILE scan: parallel parse+extract done at {:?}",
            t.elapsed()
        );
    }

    // Reserve exact capacity up front from counts already computed by the
    // parallel step above: without this, `extend` below regrows `entities`/
    // `symbols` by doubling as files merge in, and at repo scale (millions
    // of entities) the last few doublings each copy a multi-million-element
    // Vec of non-trivial structs — real, measured cost (see BENCHMARK.md).
    let (entity_total, symbol_total) = processed.iter().fold((0, 0), |(e, s), p| match &p.result {
        FileResult::Extracted {
            entities, symbols, ..
        } => (e + entities.len(), s + symbols.len()),
        FileResult::Diagnostic(_) => (e, s),
    });
    output.entities.reserve_exact(entity_total);
    output.symbols.reserve_exact(symbol_total);

    for (file, processed) in files.into_iter().zip(processed) {
        let ProcessedFile {
            content_hash,
            result,
        } = processed;
        output.file_meta.push(FileMeta {
            mtime: file.mtime,
            size: file.size,
            content_hash,
        });
        output.files.push(file.path);
        merge(&mut output, result);
    }
    if profile {
        eprintln!(
            "VARDE_PROFILE scan: sequential merge done at {:?}",
            t.elapsed()
        );
    }

    tracing::info!(
        entities = output.entities.len(),
        symbols = output.symbols.len(),
        diagnostics = output.diagnostics.len(),
        "extract complete"
    );
    Ok(output)
}

enum FileResult {
    Diagnostic(Diagnostic),
    Extracted {
        entities: Vec<crate::model::Entity>,
        symbols: Vec<crate::model::Symbol>,
        /// Set when the parse hit a syntax error (or the walk-depth guard)
        /// but tree-sitter's error recovery still yielded usable entities:
        /// we keep the partial extract *and* record the diagnostic, rather
        /// than discarding every entity in the file. Dropping the whole file
        /// on one localized error erased thousands of valid entities from
        /// real .NET code (Newtonsoft's `#if`/`#endif`-in-initializer files),
        /// blanking them from the nav map, edges, and every SQL rule.
        diagnostic: Option<Diagnostic>,
    },
}

/// The per-file output of `process_file`: the extracted entities/symbols (or
/// skip diagnostic) plus the scan-time content hash of the file bytes.
struct ProcessedFile {
    content_hash: String,
    result: FileResult,
}

fn merge(out: &mut ExtractOutput, result: FileResult) {
    match result {
        FileResult::Diagnostic(d) => out.diagnostics.push(d),
        FileResult::Extracted {
            entities,
            symbols,
            diagnostic,
        } => {
            out.entities.extend(entities);
            out.symbols.extend(symbols);
            if let Some(d) = diagnostic {
                out.diagnostics.push(d);
            }
        }
    }
}

/// Parsed fragment for a contiguous slice of the file list, produced by
/// [`parse_chunk`]. Entities/symbols/diagnostics carry their *global*
/// `file_id` (`start_index + position_in_slice`), so the streaming full build
/// ([`crate::persist::persist_full_streaming`]) can insert them against one
/// repo-wide file-id table exactly as a whole-repo [`run`] would.
pub(crate) struct ChunkParsed {
    pub entities: Vec<crate::model::Entity>,
    pub symbols: Vec<crate::model::Symbol>,
    pub diagnostics: Vec<Diagnostic>,
    /// One content hash per file in the slice, in slice order (index-aligned
    /// with the slice passed to [`parse_chunk`]).
    pub content_hashes: Vec<String>,
}

/// Parse+extract a contiguous slice of the file list in parallel, assigning
/// each file the global id `start_index + position_in_slice`.
///
/// Order-preserving: the returned entities/symbols follow the slice's file
/// order (the parallel map is collected in order, then merged sequentially),
/// so a streaming writer assigns row ids in the same order a single whole-repo
/// [`run`] would — the property the streaming full build relies on to produce
/// a byte-identical index.
pub(crate) fn parse_chunk(files: &[SourceFile], start_index: usize) -> ChunkParsed {
    let processed: Vec<ProcessedFile> = files
        .par_iter()
        .enumerate()
        .map(|(i, file)| process_file(Path::new(&file.path), (start_index + i) as u32))
        .collect();

    let mut parsed = ChunkParsed {
        entities: Vec::new(),
        symbols: Vec::new(),
        diagnostics: Vec::new(),
        content_hashes: Vec::with_capacity(files.len()),
    };
    for p in processed {
        parsed.content_hashes.push(p.content_hash);
        match p.result {
            FileResult::Diagnostic(d) => parsed.diagnostics.push(d),
            FileResult::Extracted {
                entities,
                symbols,
                diagnostic,
            } => {
                parsed.entities.extend(entities);
                parsed.symbols.extend(symbols);
                if let Some(d) = diagnostic {
                    parsed.diagnostics.push(d);
                }
            }
        }
    }
    parsed
}

/// Parse + extract one file, or return a skip diagnostic.
///
/// The raw file bytes are read exactly once: `content_hash` is derived from
/// them, and source files parse from the same read (via `from_utf8`) rather
/// than issuing a second `read_to_string`. Non-source files are detected by
/// extension *before* any read, so they contribute no I/O and only a stable
/// empty-content hash.
fn process_file(path: &Path, file_id: u32) -> ProcessedFile {
    let Some(lang) = language_for_path(path) else {
        let msg = "unsupported file type — skipped";
        // debug, not warn: fires once per non-source file walked (docs,
        // assets, configs) — often the majority of files in a repo. The
        // info-severity `Diagnostic` below is the persisted record; this is
        // just a log echo, and at warn-level it dominated `build`'s log
        // output on large repos.
        tracing::debug!(file = %path.display(), "{msg}");
        return ProcessedFile {
            content_hash: content_hash(&[]),
            result: FileResult::Diagnostic(Diagnostic {
                file_id,
                message: msg.to_string(),
                severity: "info".to_string(),
            }),
        };
    };

    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => {
            // Unreadable source file (e.g. permission denied) — same skip
            // diagnostic as a binary body, with a stable empty-content hash.
            let msg = "binary or non-UTF-8 — skipped";
            tracing::warn!(file = %path.display(), "{msg}");
            return ProcessedFile {
                content_hash: content_hash(&[]),
                result: FileResult::Diagnostic(Diagnostic {
                    file_id,
                    message: msg.to_string(),
                    severity: "error".to_string(),
                }),
            };
        }
    };
    let content_hash = content_hash(&bytes);

    let source = match std::str::from_utf8(&bytes) {
        Ok(source) => source,
        Err(_) => {
            let msg = "binary or non-UTF-8 — skipped";
            tracing::warn!(file = %path.display(), "{msg}");
            return ProcessedFile {
                content_hash,
                result: FileResult::Diagnostic(Diagnostic {
                    file_id,
                    message: msg.to_string(),
                    severity: "error".to_string(),
                }),
            };
        }
    };
    let parsed = parse_source(&lang, source);
    let result = extract::extract(&parsed, file_id);
    // A syntax error is localized: tree-sitter's error recovery still parses
    // the rest of the file, so `result.entities`/`result.symbols` hold the
    // valid constructs outside the error region. Keep them and record a
    // diagnostic, rather than discarding the whole file — dropping it erased
    // thousands of real entities from `#if`/`#endif`-heavy .NET code.
    let diagnostic = result.has_error.then(|| {
        let msg = "syntax error — partial extract kept";
        tracing::warn!(
            file = %path.display(),
            entities = result.entities.len(),
            symbols = result.symbols.len(),
            "{msg}"
        );
        Diagnostic {
            file_id,
            message: msg.to_string(),
            severity: "warning".to_string(),
        }
    });
    if diagnostic.is_none() {
        tracing::debug!(
            file = %path.display(),
            entities = result.entities.len(),
            symbols = result.symbols.len(),
            "parsed file"
        );
    }
    ProcessedFile {
        content_hash,
        result: FileResult::Extracted {
            entities: result.entities,
            symbols: result.symbols,
            diagnostic,
        },
    }
}
#[cfg(test)]
mod tests {
    use super::{
        FileResult, UNKNOWN_METADATA, list_source_files, process_file, source_file_with_metadata,
    };
    use std::path::Path;

    /// Regression: a localized syntax error must NOT discard the whole file.
    /// C# conditional-compilation directives (`#if`/`#endif`) inside a
    /// collection initializer trip tree-sitter-c-sharp into an ERROR node —
    /// pervasive in cross-framework .NET code (Newtonsoft) — yet the class and
    /// its methods parse fine. We keep the recovered entities and record a
    /// `warning` diagnostic, rather than blanking the file from the index.
    #[test]
    fn syntax_error_keeps_partial_extract_and_records_diagnostic() {
        let dir = std::env::temp_dir().join(format!("varde-scan-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("Widget.cs");
        std::fs::write(
            &path,
            "using System;\n\
             using System.Collections.Generic;\n\n\
             class Widget\n\
             {\n\
             \x20\x20\x20\x20static readonly Dictionary<Type, int> Map = new Dictionary<Type, int>\n\
             \x20\x20\x20\x20{\n\
             \x20\x20\x20\x20\x20\x20\x20\x20{ typeof(int), 1 },\n\
             #if HAVE_BIG\n\
             \x20\x20\x20\x20\x20\x20\x20\x20{ typeof(long), 2 },\n\
             #endif\n\
             \x20\x20\x20\x20\x20\x20\x20\x20{ typeof(string), 3 },\n\
             \x20\x20\x20\x20};\n\n\
             \x20\x20\x20\x20public int Compute() => 42;\n\
             }\n",
        )
        .expect("write cs");

        let processed = process_file(Path::new(path.to_str().unwrap()), 0);
        match processed.result {
            FileResult::Extracted {
                entities,
                diagnostic,
                ..
            } => {
                assert!(
                    !entities.is_empty(),
                    "error-recovery must retain entities, got none"
                );
                assert!(
                    entities.iter().any(|e| e.name == "Widget"),
                    "the class outside the error region must survive: {:?}",
                    entities.iter().map(|e| &e.name).collect::<Vec<_>>()
                );
                assert!(
                    entities.iter().any(|e| e.name == "Compute"),
                    "the method outside the error region must survive"
                );
                let d = diagnostic.expect("a syntax-error file must still carry a diagnostic");
                assert_eq!(d.message, "syntax error — partial extract kept");
                assert_eq!(d.severity, "warning");
            }
            FileResult::Diagnostic(d) => {
                panic!("file wrongly discarded on syntax error: {d:?}");
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn source_file_hashes_content_when_metadata_is_unknown() {
        let path =
            std::env::temp_dir().join(format!("varde-scan-content-hash-{}.rs", std::process::id()));
        let contents = b"fn main() {}\n";
        std::fs::write(&path, contents).expect("source writes");

        let source = source_file_with_metadata(&path, UNKNOWN_METADATA, UNKNOWN_METADATA);

        assert_eq!(source.content_hash.as_deref(), Some("355463d2db8c9b7f"));

        let _ = std::fs::remove_file(path);
    }

    /// Regression: `.hidden(false)` (set so legit dotfiles like `.github/` are
    /// indexed) otherwise lets the walker descend into `.git/`, persisting refs,
    /// hooks, logs, and loose objects as bogus source rows. The walk must prune
    /// `.git/` while still returning the real source file and other dotfiles.
    #[test]
    fn walk_excludes_dot_git_but_keeps_other_dotfiles() {
        let dir = std::env::temp_dir().join(format!("varde-scan-gitwalk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // A realistic `.git/` skeleton: nested subdirs and files, like a real repo.
        std::fs::create_dir_all(dir.join(".git/hooks")).expect("git dir");
        std::fs::create_dir_all(dir.join(".git/objects/ab")).expect("obj dir");
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\n").expect("head");
        std::fs::write(dir.join(".git/hooks/pre-commit.sample"), "#!/bin/sh\n").expect("hook");
        std::fs::write(dir.join(".git/objects/ab/cdef"), b"\x00binary").expect("obj");
        // A legit dotfile that MUST still be indexed (the reason for hidden(false)).
        std::fs::create_dir_all(dir.join(".github")).expect("gh dir");
        std::fs::write(dir.join(".github/ci.yml"), "on: push\n").expect("ci");
        // The real source file.
        std::fs::write(dir.join("main.rs"), "fn main() {}\n").expect("src");

        let files = list_source_files(dir.to_str().unwrap()).expect("walks");
        let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();

        assert!(
            paths.iter().all(|p| !p.contains("/.git/")),
            ".git internals must be pruned: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.ends_with("main.rs")),
            "real source must be indexed: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.ends_with(".github/ci.yml")),
            "non-.git dotfiles must still be indexed: {paths:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
