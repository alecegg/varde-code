//! Coverage-parity checklist harness for the resolution phase.
//!
//! Custom harness (`harness = false`, like `coverage_parity.rs`): one
//! assertion per resolution/graph capability over the shared fixture project
//! under `resolve_fixtures/`. Each capability mirrors a unit-test AC in
//! `src/resolve.rs`. Exits non-zero on any failure.

use std::path::PathBuf;

use ast_grep_language::SupportLang;
use varde_code::extract;
use varde_code::model::{Entity, EntityKind, Symbol};
use varde_code::parse::parse_source;
use varde_code::resolve::{EdgeKind, EdgeTarget, ResolvedGraph};

const FIXTURES: &str = "resolve_fixtures";

fn fixture_dir(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURES)
        .join(rel)
}

/// Parse + extract every fixture file under `<FIXTURES>/<rel>` (sorted).
fn load_project(rel: &str) -> (Vec<Entity>, Vec<Symbol>, Vec<String>) {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(fixture_dir(rel))
        .expect("resolve fixture dir exists")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    paths.sort();
    let files: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let mut entities = Vec::new();
    let mut symbols = Vec::new();
    for (file_id, path) in paths.iter().enumerate() {
        let source = std::fs::read_to_string(path).expect("read fixture");
        let lang = match path.extension().and_then(|e| e.to_str()) {
            Some("rs") => SupportLang::Rust,
            Some("ts") => SupportLang::TypeScript,
            Some("js") => SupportLang::JavaScript,
            Some("rb") => SupportLang::Ruby,
            Some("php") => SupportLang::Php,
            Some("c") | Some("h") => SupportLang::C,
            Some("cpp") => SupportLang::Cpp,
            Some("scala") | Some("sc") | Some("sbt") => SupportLang::Scala,
            Some("dart") => SupportLang::Dart,
            Some("lua") => SupportLang::Lua,
            Some("ex") | Some("exs") => SupportLang::Elixir,
            Some("sol") => SupportLang::Solidity,
            Some("hs") => SupportLang::Haskell,
            Some("sh") | Some("bash") => SupportLang::Bash,
            other => panic!("unsupported fixture extension {other:?}"),
        };
        let parsed = parse_source(&lang, &source);
        let result = extract::extract(&parsed, file_id as u32);
        entities.extend(result.entities);
        symbols.extend(result.symbols);
    }
    (entities, symbols, files)
}

fn file_id(graph: &ResolvedGraph, suffix: &str) -> u32 {
    graph
        .nodes
        .iter()
        .position(|n| n.path.ends_with(suffix))
        .expect("node exists") as u32
}

fn entity_id(entities: &[Entity], kind: EntityKind, name: &str) -> u32 {
    entities
        .iter()
        .position(|e| e.kind == kind && e.name == name)
        .expect("entity exists") as u32
}

type Check = Result<(), String>;
type CheckFn = fn() -> Check;

fn check_import_resolution() -> Check {
    let (entities, symbols, files) = load_project("rust/imports");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let a = file_id(&graph, "/a.rs");
    let b = file_id(&graph, "/b.rs");
    let edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import && e.resolved)
        .collect();
    if edges.len() != 1 || edges[0].from != a || edges[0].to != EdgeTarget::File(b) {
        return Err(format!("import resolution mismatch: {edges:?}"));
    }
    Ok(())
}

/// Ruby cross-file import: `require_relative "b"` in a.rb resolves to b.rb via
/// resolve.rs's shared file-stem matching, exercising the generic resolution
/// path for a non-Rust extractor (the Ruby-support pilot).
fn check_ruby_import_resolution() -> Check {
    let (entities, symbols, files) = load_project("ruby/imports");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let a = file_id(&graph, "/a.rb");
    let b = file_id(&graph, "/b.rb");
    let edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import && e.resolved)
        .collect();
    if edges.len() != 1 || edges[0].from != a || edges[0].to != EdgeTarget::File(b) {
        return Err(format!("ruby import resolution mismatch: {edges:?}"));
    }
    Ok(())
}

/// PHP cross-file import: `require_once "b.php"` in a.php resolves to b.php via
/// resolve.rs's shared file-stem matching, exercising the generic resolution
/// path for a non-Rust extractor.
fn check_php_import_resolution() -> Check {
    let (entities, symbols, files) = load_project("php/imports");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let a = file_id(&graph, "/a.php");
    let b = file_id(&graph, "/b.php");
    let edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import && e.resolved)
        .collect();
    if edges.len() != 1 || edges[0].from != a || edges[0].to != EdgeTarget::File(b) {
        return Err(format!("php import resolution mismatch: {edges:?}"));
    }
    Ok(())
}

/// C cross-file import: `#include "b.h"` in a.c resolves to b.h via
/// resolve.rs's shared file-stem matching, exercising the generic resolution
/// path for a non-Rust extractor. C++ shares the same `#include` path, so one
/// check covers both.
fn check_c_import_resolution() -> Check {
    let (entities, symbols, files) = load_project("c/imports");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let a = file_id(&graph, "/a.c");
    let b = file_id(&graph, "/b.h");
    let edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import && e.resolved)
        .collect();
    if edges.len() != 1 || edges[0].from != a || edges[0].to != EdgeTarget::File(b) {
        return Err(format!("c import resolution mismatch: {edges:?}"));
    }
    Ok(())
}

/// Scala cross-file import: `import com.example.Helper` in Main.scala resolves
/// to Helper.scala via resolve.rs's shared trailing-segment stem matching (the
/// import spec's last segment `Helper` matches the sibling file stem). Scala
/// imports are package-based and in general do NOT stem-match files, so this
/// fixture is deliberately crafted (import tail == sibling file stem) to
/// exercise a genuinely RESOLVED import edge through the generic path — the
/// common case (a package import spanning unrelated files) stays unresolved,
/// which is the accepted approximation documented in scala.rs.
fn check_scala_import_resolution() -> Check {
    let (entities, symbols, files) = load_project("scala/imports");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let main = file_id(&graph, "/Main.scala");
    let helper = file_id(&graph, "/Helper.scala");
    let edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import && e.resolved)
        .collect();
    if edges.len() != 1 || edges[0].from != main || edges[0].to != EdgeTarget::File(helper) {
        return Err(format!("scala import resolution mismatch: {edges:?}"));
    }
    Ok(())
}

/// Dart cross-file import: `import 'b.dart';` in a.dart resolves to sibling
/// b.dart via resolve.rs's shared file-stem matching (the relative URI's stem
/// `b` matches the sibling file stem), exercising the generic resolution path
/// for the Dart extractor.
fn check_dart_import_resolution() -> Check {
    let (entities, symbols, files) = load_project("dart/imports");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let a = file_id(&graph, "/a.dart");
    let b = file_id(&graph, "/b.dart");
    let edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import && e.resolved)
        .collect();
    if edges.len() != 1 || edges[0].from != a || edges[0].to != EdgeTarget::File(b) {
        return Err(format!("dart import resolution mismatch: {edges:?}"));
    }
    Ok(())
}

/// Lua cross-file import: `require("b")` in a.lua resolves to sibling b.lua via
/// resolve.rs's shared file-stem matching (the require tail `b` matches the
/// sibling file stem), exercising the generic resolution path for a call-based,
/// dynamic-language extractor.
fn check_lua_import_resolution() -> Check {
    let (entities, symbols, files) = load_project("lua/imports");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let a = file_id(&graph, "/a.lua");
    let b = file_id(&graph, "/b.lua");
    let edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import && e.resolved)
        .collect();
    if edges.len() != 1 || edges[0].from != a || edges[0].to != EdgeTarget::File(b) {
        return Err(format!("lua import resolution mismatch: {edges:?}"));
    }
    Ok(())
}

/// Elixir cross-file import: `alias Helper` in a.ex resolves to sibling
/// Helper.ex via resolve.rs's shared file-stem matching (the module alias
/// `Helper` matches the sibling file stem `Helper`), exercising the generic
/// resolution path for a call-node-based extractor. Elixir imports are
/// module-based and in general do NOT stem-match files, so this fixture is
/// deliberately crafted (module name == sibling file stem, matching case) to
/// exercise a genuinely RESOLVED import edge — the common case (a package/app
/// module import unrelated to a sibling file name) stays unresolved, which is
/// the accepted approximation documented in elixir.rs.
fn check_elixir_import_resolution() -> Check {
    let (entities, symbols, files) = load_project("elixir/imports");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let a = file_id(&graph, "/a.ex");
    let helper = file_id(&graph, "/Helper.ex");
    let edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import && e.resolved)
        .collect();
    if edges.len() != 1 || edges[0].from != a || edges[0].to != EdgeTarget::File(helper) {
        return Err(format!("elixir import resolution mismatch: {edges:?}"));
    }
    Ok(())
}

/// Solidity cross-file import: `import "./b.sol";` in a.sol resolves to sibling
/// b.sol via resolve.rs's shared file-stem matching (the relative path's stem
/// `b` matches the sibling file stem), exercising the generic resolution path
/// for the Solidity extractor. Unlike Elixir, Solidity imports are genuinely
/// path-based, so this is the common case rather than a crafted one.
fn check_solidity_import_resolution() -> Check {
    let (entities, symbols, files) = load_project("solidity/imports");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let a = file_id(&graph, "/a.sol");
    let b = file_id(&graph, "/b.sol");
    let edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import && e.resolved)
        .collect();
    if edges.len() != 1 || edges[0].from != a || edges[0].to != EdgeTarget::File(b) {
        return Err(format!("solidity import resolution mismatch: {edges:?}"));
    }
    Ok(())
}

/// Haskell cross-file import: `import B` in a.hs resolves to sibling B.hs via
/// resolve.rs's shared file-stem matching (the import spec `B` matches the
/// sibling file stem `B`). Haskell imports are module/package-based and in
/// general do NOT stem-match files; this fixture is deliberately crafted
/// (module name == sibling file stem) so a genuinely RESOLVED edge is
/// exercised (mirrors the Elixir approximation).
fn check_haskell_import_resolution() -> Check {
    let (entities, symbols, files) = load_project("haskell/imports");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let a = file_id(&graph, "/a.hs");
    let b = file_id(&graph, "/B.hs");
    let edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import && e.resolved)
        .collect();
    if edges.len() != 1 || edges[0].from != a || edges[0].to != EdgeTarget::File(b) {
        return Err(format!("haskell import resolution mismatch: {edges:?}"));
    }
    Ok(())
}

/// Bash cross-file import: `source ./b.sh` in a.sh resolves to sibling b.sh via
/// resolve.rs's shared file-stem matching (the relative path's stem `b` matches
/// the sibling file stem). Bash `source`/`.` are genuinely path-based, so this
/// is the common case rather than a crafted one.
fn check_bash_import_resolution() -> Check {
    let (entities, symbols, files) = load_project("bash/imports");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let a = file_id(&graph, "/a.sh");
    let b = file_id(&graph, "/b.sh");
    let edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import && e.resolved)
        .collect();
    if edges.len() != 1 || edges[0].from != a || edges[0].to != EdgeTarget::File(b) {
        return Err(format!("bash import resolution mismatch: {edges:?}"));
    }
    Ok(())
}

fn check_same_file_call_resolution() -> Check {
    let (entities, symbols, files) = load_project("rust/calls");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let local_fn = entity_id(&entities, EntityKind::Function, "local_fn");
    let resolved: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Call && e.resolved)
        .collect();
    if resolved.len() != 1 || resolved[0].to != EdgeTarget::Entity(local_fn) {
        return Err(format!("same-file call resolution mismatch: {resolved:?}"));
    }
    Ok(())
}

fn check_cross_file_call_resolution() -> Check {
    let (entities, symbols, files) = load_project("rust/imports");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let helper = entity_id(&entities, EntityKind::Function, "helper");
    let resolved: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Call && e.resolved)
        .collect();
    if resolved.len() != 1 || resolved[0].to != EdgeTarget::Entity(helper) {
        return Err(format!("cross-file call resolution mismatch: {resolved:?}"));
    }
    Ok(())
}

fn check_unresolved_diagnostics() -> Check {
    let (entities, symbols, files) = load_project("rust/calls");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let unresolved: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Call && !e.resolved)
        .collect();
    if unresolved.len() != 1 || unresolved[0].to != EdgeTarget::Unknown {
        return Err(format!("unresolved-call handling mismatch: {unresolved:?}"));
    }
    Ok(())
}

fn check_graph_construction() -> Check {
    let (entities, symbols, files) = load_project("rust/graph");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    if graph.nodes.len() != 3 || graph.edges.len() != 6 {
        return Err(format!(
            "graph node/edge counts mismatch: {} nodes, {} edges",
            graph.nodes.len(),
            graph.edges.len()
        ));
    }
    Ok(())
}

fn check_fan_metrics() -> Check {
    let (entities, symbols, files) = load_project("rust/fan");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let a = file_id(&graph, "/a.rs");
    let c = file_id(&graph, "/c.rs");
    if graph.nodes[a as usize].fan_out != 2 || graph.nodes[c as usize].fan_in != 2 {
        return Err(format!(
            "fan metrics mismatch: a.fan_out={} c.fan_in={}",
            graph.nodes[a as usize].fan_out, graph.nodes[c as usize].fan_in
        ));
    }
    Ok(())
}

fn check_communities() -> Check {
    let (entities, symbols, files) = load_project("rust/communities");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    if graph.communities.len() != 2 {
        return Err(format!(
            "expected 2 communities, got {}",
            graph.communities.len()
        ));
    }
    for node in &graph.nodes {
        let cid = node.community_id.ok_or("community id missing")?;
        let community = graph
            .communities
            .iter()
            .find(|c| c.id == cid)
            .ok_or("community id unknown")?;
        if !community.members.contains(&node.path) {
            return Err(format!("{} not in community {cid}", node.path));
        }
    }
    Ok(())
}

fn check_clone_bands() -> Check {
    let (entities, symbols, files) = load_project("rust/clones");
    let graph =
        varde_code::resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let one = entity_id(&entities, EntityKind::Function, "helper_one");
    let two = entity_id(&entities, EntityKind::Function, "helper_two");
    let distinct = entity_id(&entities, EntityKind::Function, "unique_thing");
    let shared: Vec<_> = graph
        .clone_bands
        .iter()
        .filter(|b| b.members.contains(&one) && b.members.contains(&two))
        .collect();
    if shared.is_empty() {
        return Err("near-duplicates share no clone band".to_string());
    }
    for band in &graph.clone_bands {
        if band.members.contains(&distinct)
            && (band.members.contains(&one) || band.members.contains(&two))
        {
            return Err(format!("distinct function shares a band: {band:?}"));
        }
    }
    Ok(())
}

fn main() {
    let checks: [(&str, CheckFn); 18] = [
        ("import resolution", check_import_resolution),
        ("ruby import resolution", check_ruby_import_resolution),
        ("php import resolution", check_php_import_resolution),
        ("c import resolution", check_c_import_resolution),
        ("scala import resolution", check_scala_import_resolution),
        ("dart import resolution", check_dart_import_resolution),
        ("lua import resolution", check_lua_import_resolution),
        ("elixir import resolution", check_elixir_import_resolution),
        (
            "solidity import resolution",
            check_solidity_import_resolution,
        ),
        ("haskell import resolution", check_haskell_import_resolution),
        ("bash import resolution", check_bash_import_resolution),
        ("same-file call resolution", check_same_file_call_resolution),
        (
            "cross-file call resolution",
            check_cross_file_call_resolution,
        ),
        ("unresolved diagnostics", check_unresolved_diagnostics),
        ("graph construction", check_graph_construction),
        ("fan-in/fan-out metrics", check_fan_metrics),
        ("louvain communities", check_communities),
        ("clone bands", check_clone_bands),
    ];

    let mut failures = Vec::new();
    for (name, check) in checks {
        match check() {
            Ok(()) => println!("coverage_parity_harness: {name}: OK"),
            Err(e) => {
                eprintln!("coverage_parity_harness: {name}: FAIL {e}");
                failures.push(name);
            }
        }
    }

    if failures.is_empty() {
        println!(
            "coverage_parity_harness: all {} resolution/graph capabilities passed",
            checks.len()
        );
    } else {
        eprintln!(
            "coverage_parity_harness: {} capability(ies) failed: {:?}",
            failures.len(),
            failures
        );
        std::process::exit(1);
    }
}
