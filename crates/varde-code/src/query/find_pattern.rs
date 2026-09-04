//! find_pattern mode: hand-rolled AST meta-variable pattern matcher.
//!
//! Not ast-grep-core's matcher — a standalone structural matcher over the
//! parsed tree (parsing reuses the project's existing `parse_source`).
//!
//! Syntax (this phase's scope):
//! - `$VAR`  — single-node capture: matches exactly one node of any kind.
//! - `$VAR:kind` — single-node capture constrained to a given tree-sitter
//!   node kind (e.g. `$FN:function_item`); fails to match otherwise.
//! - `$$$VAR` — variadic capture: matches zero or more sibling nodes.
//! - `$$$VAR:kind` — variadic capture where every captured node must have
//!   the given kind.
//!
//! Relational operators — top-level `inside`/`has`/`precedes`/`follows`
//! input fields, each `{"kind": "..."}` — filter matches by ancestor,
//! descendant, or sibling kind:
//! - `inside`: some ancestor of the match has the given kind.
//! - `has`: some descendant of the match has the given kind.
//! - `precedes`/`follows`: some later/earlier *sibling* (same parent) has
//!   the given kind — this is ast-grep's default "neighbor" `stopBy`, not a
//!   full-document-order search.
//!
//! Scoped to kind-only constraints; a nested sub-pattern for these fields
//! (ast-grep's full relational rules) is out of scope for this phase.
//!
//! The pattern is parsed in the target language after rewriting `$VAR`/`$$$VAR`
//! tokens to marker identifiers, then matched structurally: same node kind,
//! leaf text equality, children matched pairwise with variadic backtracking.
//! Trivia — unnamed/anonymous tokens (`;`, `,`, brackets) — is stripped from
//! both sides before comparison (`strip_trivia`), matching ast-grep's
//! default "Smart" match strictness.

use anyhow::Result;
use std::collections::HashMap;

use super::{ApiError, req_str};

/// Marker prefixes injected by preprocessing (kept unlikely to collide with
/// real identifiers).
const SINGLE_PREFIX: &str = "__varde_meta_s_";
const VARIADIC_PREFIX: &str = "__varde_meta_v_";
const SUFFIX: &str = "__";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetaKind {
    /// `$NAME` — matches any single node.
    Single(&'static str),
    /// `$$$NAME` — matches zero or more sibling nodes.
    Variadic(&'static str),
}

/// Rewrite `$VAR` / `$$$VAR` tokens in a pattern into parseable marker
/// identifiers.
fn preprocess(pattern: &str) -> String {
    let mut out = pattern.to_string();
    // Variadic first ($$$ before $).
    out = replace_meta(&out, "$$$", VARIADIC_PREFIX);
    out = replace_meta(&out, "$", SINGLE_PREFIX);
    out
}

/// Strip `:kind` suffixes off `$VAR:kind` / `$$$VAR:kind` tokens before
/// they reach [`preprocess`], recording each name's required kind in a side
/// map. The identifier that ends up parsed (and later matched via
/// [`meta_of`]) never contains the kind — `:` isn't valid inside most
/// languages' identifier tokens, so the constraint has to live out-of-band
/// rather than embedded in the marker text.
fn split_kind_constraints(pattern: &str) -> (String, HashMap<String, String>) {
    let mut out = String::with_capacity(pattern.len());
    let mut constraints = HashMap::new();
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let sigil = if pattern[i..].starts_with("$$$") {
            Some("$$$")
        } else if pattern[i..].starts_with('$') {
            Some("$")
        } else {
            None
        };
        if let Some(sigil) = sigil {
            let rest = &pattern[i + sigil.len()..];
            let name_end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            let name = &rest[..name_end];
            if !name.is_empty() {
                out.push_str(sigil);
                out.push_str(name);
                let mut consumed = sigil.len() + name_end;
                let after_name = &pattern[i + consumed..];
                if let Some(rest2) = after_name.strip_prefix(':') {
                    let kind_end = rest2
                        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                        .unwrap_or(rest2.len());
                    let kind = &rest2[..kind_end];
                    if !kind.is_empty() {
                        constraints.insert(name.to_string(), kind.to_string());
                        consumed += 1 + kind_end;
                    }
                }
                i += consumed;
                continue;
            }
        }
        let ch_len = pattern[i..].chars().next().map(char::len_utf8).unwrap_or(1);
        out.push_str(&pattern[i..i + ch_len]);
        i += ch_len;
    }
    (out, constraints)
}

fn replace_meta(pattern: &str, sigil: &str, prefix: &str) -> String {
    // Token syntax contract shared with rules/rewrite.rs::template_tokens
    // and rules/pattern.rs::capture_names (see rewrite.rs's TemplateToken
    // doc): `$$$` before `$`, `[A-Za-z0-9_]` name run, empty name → literal.
    // This byte loop deliberately stays here rather than consolidating into
    // the shared tokenizer: it rewrites tokens into marker identifiers for
    // parsing (not name extraction), and it is the matcher's hot path —
    // consolidating would change find_pattern's behavior and its perf
    // budget. Keep the boundary rules above in sync with template_tokens.
    let mut result = String::with_capacity(pattern.len());
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if pattern[i..].starts_with(sigil) {
            // Parse the following identifier.
            let rest = &pattern[i + sigil.len()..];
            let name_end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            let name = &rest[..name_end];
            if !name.is_empty() {
                result.push_str(prefix);
                result.push_str(name);
                result.push_str(SUFFIX);
                i += sigil.len() + name_end;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Soft cap on backtracking steps for the variadic (`$$$VAR`) matcher, shared
/// by `match_sequence`/`bind_sequence`. Pathological patterns (e.g. many
/// `$$$VAR` markers against a long sibling list) can otherwise blow up
/// combinatorially; this bounds worst-case latency with a generous ceiling
/// that no real-world pattern should approach.
const MAX_BACKTRACK_STEPS: usize = 100_000;

thread_local! {
    /// Remaining backtracking steps for the in-flight `find_pattern` call.
    /// Reset at the start of each call; decremented by `tick_budget`.
    static BACKTRACK_BUDGET: std::cell::Cell<usize> =
        const { std::cell::Cell::new(MAX_BACKTRACK_STEPS) };
}

/// Consume one unit of backtracking budget. Returns `false` once the budget
/// is exhausted, signalling callers to stop recursing.
fn tick_budget() -> bool {
    BACKTRACK_BUDGET.with(|budget| {
        let remaining = budget.get();
        if remaining == 0 {
            return false;
        }
        budget.set(remaining - 1);
        true
    })
}

/// Reset the backtracking budget before a fresh `find_pattern` run.
fn reset_budget() {
    BACKTRACK_BUDGET.with(|budget| budget.set(MAX_BACKTRACK_STEPS));
}

/// Whether the backtracking budget was exhausted during the last run.
fn budget_exhausted() -> bool {
    BACKTRACK_BUDGET.with(|budget| budget.get() == 0)
}

type PNode<'a> =
    ast_grep_core::Node<'a, ast_grep_core::tree_sitter::StrDoc<ast_grep_language::SupportLang>>;

/// Cache of a pattern node's (trivia-stripped) children, keyed by
/// `Node::node_id()`. The pattern tree is small, static and shared across
/// every candidate node `find_matches`/`match_node` visit in a file — most
/// candidates fail to match, but each failed attempt still re-collects the
/// *same* pattern children over and over via `pattern.children().collect()`.
/// This cache turns that into one `TreeCursor` walk per distinct pattern
/// node per file, plus cheap `Vec<Node>` clones (`Node` is a small
/// ref+id handle, not a subtree copy) for every subsequent visit.
///
/// A full `TreeCursor`-based rewrite of `match_node`/`match_sequence` was
/// attempted first but abandoned: `match_sequence`'s memoized backtracking
/// (`rec`, keyed by `(pi, si)`) needs random-access indexing into *both*
/// sides' child sequences (`p[pi]`, arbitrary `s[si + consume]` jumps for
/// `$$$VAR`), which a `TreeCursor` — a single-position walk with only
/// next-sibling/parent/first-child moves — cannot provide without
/// rebuilding an index on first use anyway. So this fallback (pattern-side
/// caching only) is what's implemented; see the task's Progress notes.
type PatternChildCache<'p> = std::cell::RefCell<HashMap<usize, Vec<PNode<'p>>>>;

fn cached_pattern_children<'p>(cache: &PatternChildCache<'p>, node: &PNode<'p>) -> Vec<PNode<'p>> {
    let id = node.node_id();
    if let Some(children) = cache.borrow().get(&id) {
        return children.clone();
    }
    let children = strip_trivia(node.children().collect());
    cache.borrow_mut().insert(id, children.clone());
    children
}

/// If `node` is a single-level grammar wrapper whose sole named child is a
/// *variadic* marker, return that marker. Some grammars wrap each element of a
/// list in an extra node — e.g. C# wraps every call/constructor argument in an
/// `argument` node, so the pattern `foo($$$A)` parses its argument list as
/// `argument_list → argument → identifier("__varde_meta_v_A__")`. Buried under
/// that lone `argument`, the variadic is never seen at the `argument_list`
/// sequence level, so it fails to match empty or multi-element argument lists.
///
/// This only *detects* a liftable wrapper; the caller
/// ([`match_node_capture`]) decides whether to apply it, using the source
/// side to tell a per-element wrapper (lift) from a singleton list container
/// (keep). Only variadic markers are considered: single markers already match
/// through ordinary recursive descent, and lifting them would change which node
/// their capture binds.
fn lift_variadic_marker<'p>(node: &PNode<'p>) -> Option<PNode<'p>> {
    // A node that is itself a bare marker has no wrapper to lift.
    if meta_of(node).is_some() {
        return None;
    }
    let named: Vec<PNode<'p>> = node.children().filter(|c| c.is_named()).collect();
    let [only] = named.as_slice() else {
        return None;
    };
    matches!(meta_of(only), Some(MetaKind::Variadic(_))).then(|| only.clone())
}

/// Detect whether a node is a meta-variable marker.
///
/// Markers are single identifier tokens, so only leaf nodes (no children)
/// can be markers. Without the leaf guard, a compound node whose text
/// happens to start *and* end with the marker delimiters — e.g. the root
/// of `$KEY = $VAL`, whose text is `__varde_meta_s_KEY__ =
/// __varde_meta_s_VAL__` — is misdetected as one giant meta variable named
/// `KEY__ = __varde_meta_s_VAL`, which then matches any node and binds the
/// whole program as a single capture.
fn meta_of(node: &PNode<'_>) -> Option<MetaKind> {
    if node.children().count() != 0 {
        return None;
    }
    let text = node.text();
    if let Some(rest) = text.strip_prefix(SINGLE_PREFIX)
        && let Some(name) = rest.strip_suffix(SUFFIX)
    {
        return Some(MetaKind::Single(leak(name)));
    }
    if let Some(rest) = text.strip_prefix(VARIADIC_PREFIX)
        && let Some(name) = rest.strip_suffix(SUFFIX)
    {
        return Some(MetaKind::Variadic(leak(name)));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_pattern(pattern: &str, source: &str) -> serde_json::Value {
        run_pattern_ex(pattern, source, serde_json::json!({}))
    }

    fn run_pattern_ex(pattern: &str, source: &str, extra: serde_json::Value) -> serde_json::Value {
        let dir = std::env::temp_dir().join(format!(
            "fp-meta-test-{}-{}",
            std::process::id(),
            pattern.len()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        let file = dir.join("sample.ts");
        std::fs::write(&file, source).expect("fixture writes");
        let mut input = serde_json::json!({
            "filePath": file.display().to_string(),
            "pattern": pattern,
        });
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                input[k] = v.clone();
            }
        }
        let out = find_pattern(&input).expect("pattern runs");
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn two_meta_variables_in_one_statement_bind_separately() {
        // Regression for the credential rule shape: `$KEY = $VAL`. The
        // assignment root's text is `__varde_meta_s_KEY__ =
        // __varde_meta_s_VAL__`, which starts and ends with the marker
        // delimiters — meta_of used to misdetect the whole root as one
        // giant meta variable (matching every node and binding the entire
        // program under the bogus name `KEY__ = __varde_meta_s_VAL`).
        let out = run_pattern(
            "$KEY = $VAL",
            "const x = 1;\napi_key = \"sk-abc\";\nlet z;\n",
        );
        let matches = out.as_array().expect("matches array");
        assert_eq!(matches.len(), 1, "only the real assignment matches: {out}");
        assert_eq!(matches[0]["text"], "api_key = \"sk-abc\"");
        let caps = matches[0]["captures"].as_object().expect("captures object");
        assert_eq!(caps["KEY"]["text"], "api_key", "{out}");
        assert_eq!(caps["VAL"]["text"], "\"sk-abc\"", "{out}");
        assert!(
            caps.keys().all(|k| k == "KEY" || k == "VAL"),
            "no bogus capture names: {out}"
        );
    }

    #[test]
    fn kind_constraint_filters_single_capture() {
        let out = run_pattern("$KEY = $VAL:number", "x = 1;\ny = \"s\";\n");
        let matches = out.as_array().expect("matches array");
        assert_eq!(matches.len(), 1, "only the numeric RHS matches: {out}");
        assert_eq!(matches[0]["text"], "x = 1");
    }

    #[test]
    fn kind_constraint_on_variadic_requires_every_element() {
        // $$$ARGS:number should only bind when every captured argument is a
        // number literal — mixed-kind argument lists must not match.
        let out = run_pattern("foo($$$ARGS:number)", "foo(1, 2, 3);\nfoo(1, \"x\");\n");
        let matches = out.as_array().expect("matches array");
        assert_eq!(matches.len(), 1, "only the all-numeric call matches: {out}");
        assert_eq!(matches[0]["text"], "foo(1, 2, 3)");
    }

    /// Run a pattern over a C# source fixture (`sample.cs`), returning the match
    /// array directly. C# wraps each argument in an `argument` node, so this
    /// exercises the buried-variadic lift that the TS fixtures cannot.
    fn run_pattern_cs(pattern: &str, source: &str) -> Vec<serde_json::Value> {
        let dir = std::env::temp_dir().join(format!(
            "fp-cs-test-{}-{}",
            std::process::id(),
            pattern.len()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        let file = dir.join("sample.cs");
        std::fs::write(&file, source).expect("fixture writes");
        let out = find_pattern(&serde_json::json!({
            "filePath": file.display().to_string(),
            "pattern": pattern,
            "language": "csharp",
        }))
        .expect("pattern runs");
        let _ = std::fs::remove_dir_all(&dir);
        out.as_array().cloned().expect("matches array")
    }

    #[test]
    fn variadic_matches_empty_and_multi_arg_through_csharp_wrapper() {
        // Regression: in C# `$$$A` sits under an `argument` wrapper inside
        // `argument_list`. Before the lift, the variadic could only match a
        // single-argument call (varde 6 vs ast-grep 7 on real code); empty and
        // multi-argument calls were missed. `$$$A` must match 0, 1, and N args.
        let src = "class C { void M() {\n\
            Foo();\n\
            Foo(1);\n\
            Foo(1, 2, 3);\n\
            var a = new Thing();\n\
            var b = new Thing(x, y);\n\
        } }\n";

        let calls = run_pattern_cs("Foo($$$A)", src);
        let texts: Vec<&str> = calls.iter().map(|m| m["text"].as_str().unwrap()).collect();
        assert!(texts.contains(&"Foo()"), "empty-arg call matched: {texts:?}");
        assert!(texts.contains(&"Foo(1)"), "single-arg call matched: {texts:?}");
        assert!(
            texts.contains(&"Foo(1, 2, 3)"),
            "multi-arg call matched: {texts:?}"
        );
        assert_eq!(calls.len(), 3, "exactly the three Foo calls: {texts:?}");

        let news = run_pattern_cs("new Thing($$$A)", src);
        assert_eq!(
            news.len(),
            2,
            "both empty and multi-arg constructor calls match: {news:?}"
        );
    }

    #[test]
    fn expression_form_throw_pattern_locates_statement_root() {
        // Regression: `throw new $E($$$A)` with no trailing `;` used to fail
        // with "cannot locate pattern root in wrapper". C# has no throw
        // *expression*, so the fragment only parses inside the wrapper, which
        // appends a `;` — the located node is the `throw_statement`. The pattern
        // must resolve and match statement-form throws (parity with ast-grep,
        // which matches the no-`;` form).
        let src = "class C { void M() {\n\
            throw new System.Exception();\n\
            throw new System.Exception(\"msg\");\n\
        } }\n";
        let matches = run_pattern_cs("throw new $E($$$A)", src);
        assert_eq!(
            matches.len(),
            2,
            "no-semicolon throw pattern matches both throw statements: {matches:?}"
        );
        assert!(
            matches
                .iter()
                .all(|m| m["kind"] == "throw_statement"),
            "located root is the throw statement: {matches:?}"
        );
    }

    #[test]
    fn inside_relation_filters_by_ancestor_kind() {
        let out = run_pattern_ex(
            "helper()",
            "function outer() { helper(); }\nhelper();\n",
            serde_json::json!({"inside": {"kind": "function_declaration"}}),
        );
        let matches = out.as_array().expect("matches array");
        assert_eq!(
            matches.len(),
            1,
            "only the call inside outer() matches: {out}"
        );
    }

    #[test]
    fn has_relation_filters_by_descendant_kind() {
        let out = run_pattern_ex(
            "function $NAME() { $$$BODY }",
            "function withCall() { helper(); }\nfunction empty() {}\n",
            serde_json::json!({"has": {"kind": "call_expression"}}),
        );
        let matches = out.as_array().expect("matches array");
        assert_eq!(
            matches.len(),
            1,
            "only the function containing a call matches: {out}"
        );
        let caps = matches[0]["captures"].as_object().expect("captures object");
        assert_eq!(caps["NAME"]["text"], "withCall", "{out}");
    }

    #[test]
    fn follows_relation_requires_a_preceding_sibling_kind() {
        let out = run_pattern_ex(
            "let $A = $B;",
            "let a = 1;\nlet b = 2;\n",
            serde_json::json!({"follows": {"kind": "lexical_declaration"}}),
        );
        let matches = out.as_array().expect("matches array");
        assert_eq!(
            matches.len(),
            1,
            "only the second declaration follows one: {out}"
        );
        assert_eq!(matches[0]["text"], "let b = 2;");
    }

    #[test]
    fn precedes_relation_requires_a_following_sibling_kind() {
        let out = run_pattern_ex(
            "let $A = $B;",
            "let a = 1;\nlet b = 2;\n",
            serde_json::json!({"precedes": {"kind": "lexical_declaration"}}),
        );
        let matches = out.as_array().expect("matches array");
        assert_eq!(
            matches.len(),
            1,
            "only the first declaration precedes one: {out}"
        );
        assert_eq!(matches[0]["text"], "let a = 1;");
    }

    // --- literal-atom prefilter ---

    #[test]
    fn mandatory_atoms_extracts_literal_identifiers() {
        // The two literal leaves of `console.log($MSG)` must appear verbatim
        // in any matching file; `$MSG` is a meta-var and contributes nothing.
        assert_eq!(mandatory_atoms("console.log($MSG)"), vec!["console", "log"]);
    }

    #[test]
    fn mandatory_atoms_skips_meta_vars_and_kind_constraints() {
        // Pure meta-vars + punctuation ⇒ no atoms ⇒ prefilter disabled.
        assert!(mandatory_atoms("$KEY = $VAL").is_empty());
        // A `:kind` suffix is a node-kind constraint, not source text.
        assert!(mandatory_atoms("$VAL:number").is_empty());
        // Variadic markers (`$$$`) are likewise not literals.
        assert_eq!(mandatory_atoms("foo($$$ARGS:number)"), vec!["foo"]);
    }

    #[test]
    fn mandatory_atoms_includes_string_and_number_literals() {
        // A string literal's inner identifier and a numeric literal both must
        // appear verbatim (returned sorted+deduped).
        assert_eq!(
            mandatory_atoms("require(\"react\")"),
            vec!["react", "require"]
        );
        assert_eq!(mandatory_atoms("foo(42)"), vec!["42", "foo"]);
        // Dedup: a repeated atom appears once.
        assert_eq!(mandatory_atoms("log($X); log($Y)"), vec!["log"]);
    }

    /// Run a directory search over an in-memory set of `(filename, source)`
    /// fixtures — the code path the prefilter actually gates.
    fn run_pattern_dir(tag: &str, pattern: &str, files: &[(&str, &str)]) -> serde_json::Value {
        let dir = std::env::temp_dir().join(format!("fp-dir-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        for (name, src) in files {
            std::fs::write(dir.join(name), src).expect("fixture writes");
        }
        let input = serde_json::json!({
            "path": dir.display().to_string(),
            "pattern": pattern,
            "language": "typescript",
        });
        let out = find_pattern(&input).expect("pattern runs");
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn prefilter_changes_timing_not_results() {
        // Three files: one real match; one that contains both atoms
        // (`console`, `log`) but does not structurally match (prefilter lets it
        // through, the matcher rejects it); one that lacks an atom entirely
        // (prefilter skips it without parsing). Only the real match survives —
        // proving the prefilter never drops a match and defers the final
        // decision to the matcher.
        let out = run_pattern_dir(
            "prefilter",
            "console.log($MSG)",
            &[
                ("match.ts", "console.log(\"hi\");\n"),
                ("atom_no_match.ts", "const log = console;\n"),
                ("no_atom.ts", "let x = 1;\n"),
            ],
        );
        let matches = out.as_array().expect("matches array");
        assert_eq!(matches.len(), 1, "only the real console.log matches: {out}");
        assert_eq!(matches[0]["captures"]["MSG"]["text"], "\"hi\"", "{out}");
    }

    #[test]
    fn atomless_pattern_still_searches_whole_tree() {
        // `$A = $B` has no literal atom, so the prefilter is disabled and every
        // file is parsed — the assignment is still found.
        // Bare assignments (`assignment_expression`), which `$A = $B` matches —
        // a `let`-bound `variable_declarator` is a different shape.
        let out = run_pattern_dir(
            "atomless",
            "$A = $B",
            &[("a.ts", "x = 1;\n"), ("b.ts", "y = 2;\n")],
        );
        let matches = out.as_array().expect("matches array");
        assert_eq!(matches.len(), 2, "both assignments found: {out}");
    }
}

/// Intern the meta-variable name to a &'static str, leaking only the first
/// time a given name is seen. Meta-variable names come from a small,
/// naturally bounded vocabulary per pattern (`$FOO`, `$BAR`, ...), but this
/// process runs as a resident daemon serving many pattern-match requests
/// over its lifetime — leaking unconditionally on every call would grow the
/// leaked set without bound as new pattern text is seen. The interner caps
/// it at the number of *distinct* names ever seen, not the number of calls.
fn leak(s: &str) -> &'static str {
    static INTERNED: std::sync::OnceLock<std::sync::Mutex<HashMap<String, &'static str>>> =
        std::sync::OnceLock::new();
    let cache = INTERNED.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&interned) = cache.get(s) {
        return interned;
    }
    let interned: &'static str = Box::leak(s.to_string().into_boxed_str());
    cache.insert(s.to_string(), interned);
    interned
}

/// Drop trivia — unnamed (anonymous) tokens, e.g. `;`, `,`, `(`, `)` — from a
/// child sequence before structural comparison. Mirrors ast-grep's default
/// "Smart" match strictness: named nodes carry the pattern's meaning, while
/// punctuation-only tokens are formatting/optional-syntax noise (a trailing
/// comma before `)`, an ASI-omitted `;`) that shouldn't defeat matching an
/// unformatted pattern against differently-formatted source. Stripped from
/// both sides symmetrically, so a pattern that happens to spell out a token
/// the source omits (or vice versa) still aligns.
fn strip_trivia<'a>(children: Vec<PNode<'a>>) -> Vec<PNode<'a>> {
    // Most node kinds are all-named children (e.g. argument lists without
    // punctuation nodes at this level); skip the scan/rebuild entirely for
    // them rather than allocating a same-length copy on every match_node
    // call.
    if children.iter().all(|c| c.is_named()) {
        return children;
    }
    children.into_iter().filter(|c| c.is_named()).collect()
}

/// Does the pattern node match the source node, and if so, what captures
/// does that alignment bind? Merges what used to be two separate
/// structural walks — `match_node` (locate) followed by `collect_captures`
/// (bind, re-deriving the same pattern/source alignment from scratch) — into
/// one: alignment is computed once and captures are collected along the way,
/// discarded automatically on any backtracked-away branch (an `Option`
/// return, not a side-effecting `&mut` accumulator, so a failed branch's
/// partial captures never leak into the result).
fn match_node_capture<'p>(
    pattern: &PNode<'p>,
    source: &PNode<'_>,
    cache: &PatternChildCache<'p>,
    constraints: &HashMap<String, String>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    if let Some(meta) = meta_of(pattern) {
        return match meta {
            MetaKind::Single(name) => {
                if let Some(k) = constraints.get(name)
                    && source.kind().as_ref() != k.as_str()
                {
                    return None;
                }
                let mut caps = serde_json::Map::new();
                caps.insert(name.to_string(), node_json(source));
                Some(caps)
            }
            // A variadic marker only makes sense at the sequence level;
            // treat it as matching a single node here (harmless for
            // root-position checks, mirrors match_node's prior behavior).
            MetaKind::Variadic(name) => {
                if let Some(k) = constraints.get(name)
                    && source.kind().as_ref() != k.as_str()
                {
                    return None;
                }
                let mut caps = serde_json::Map::new();
                caps.insert(name.to_string(), serde_json::json!([node_json(source)]));
                Some(caps)
            }
        };
    }
    if pattern.kind() != source.kind() {
        return None;
    }
    let pchildren = cached_pattern_children(cache, pattern);
    if pchildren.is_empty() {
        // Leaf: text must match exactly, no captures at this level.
        return if pattern.text() == source.text() {
            Some(serde_json::Map::new())
        } else {
            None
        };
    }
    let schildren: Vec<PNode<'_>> = strip_trivia(source.children().collect());
    // Lift per-element wrappers that bury a variadic marker (e.g. C#'s
    // `argument` node) up to this sequence level, but only when the wrapper's
    // kind is a *per-element* wrapper here — i.e. the source has some number
    // other than exactly one child of that kind. A singleton list container
    // (`arguments` in TS/JS holds the marker directly and appears once) has
    // exactly one same-kind source child and is left intact for ordinary
    // recursive descent, which handles the variadic one level down. Without the
    // count gate, that container would be collapsed and the variadic would
    // wrongly swallow sibling slots. Most patterns have no liftable child, so
    // the common path returns the cached `pchildren` untouched.
    let plifted: Vec<PNode<'_>> = if pchildren.iter().any(|c| lift_variadic_marker(c).is_some()) {
        pchildren
            .iter()
            .map(|c| match lift_variadic_marker(c) {
                Some(marker)
                    if schildren.iter().filter(|s| s.kind() == c.kind()).count() != 1 =>
                {
                    marker
                }
                _ => c.clone(),
            })
            .collect()
    } else {
        pchildren
    };
    match_sequence_capture(&plifted, &schildren, cache, constraints)
}

/// Match a pattern-child sequence against a source-child sequence, allowing
/// variadic markers to consume zero or more source children (backtracking),
/// binding captures along the accepted path in the same recursion that
/// decides the match. Memoized like the old bool-only `match_sequence`
/// (`(pi, si) -> Option<Map>`): a `None`/`Some` outcome — and, for `Some`,
/// its bound captures — is deterministic per `(pi, si)` suffix, since
/// backtracking always resolves to the same first-successful alignment.
fn match_sequence_capture<'p>(
    p: &[PNode<'p>],
    s: &[PNode<'_>],
    cache: &PatternChildCache<'p>,
    constraints: &HashMap<String, String>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    type Memo = HashMap<(usize, usize), Option<serde_json::Map<String, serde_json::Value>>>;
    fn rec<'p>(
        p: &[PNode<'p>],
        s: &[PNode<'_>],
        pi: usize,
        si: usize,
        memo: &mut Memo,
        cache: &PatternChildCache<'p>,
        constraints: &HashMap<String, String>,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        if let Some(result) = memo.get(&(pi, si)) {
            return result.clone();
        }
        if !tick_budget() {
            // Backtracking budget exhausted: bail out of this branch rather
            // than continuing to recurse on a pathological pattern.
            return None;
        }
        let result = if pi == p.len() {
            if si == s.len() {
                Some(serde_json::Map::new())
            } else {
                None
            }
        } else {
            let pnode = &p[pi];
            if let Some(MetaKind::Variadic(name)) = meta_of(pnode) {
                // Widest/first accepted alignment: the smallest `consume`
                // for which the remaining pattern still matches the
                // remaining source, tried in ascending order (mirrors the
                // old `match_sequence`'s `.any` over ascending `consume` and
                // `bind_sequence`'s ascending loop with early return).
                (0..=(s.len() - si)).find_map(|consume| {
                    if let Some(k) = constraints.get(name)
                        && !s[si..si + consume]
                            .iter()
                            .all(|n| n.kind().as_ref() == k.as_str())
                    {
                        return None;
                    }
                    rec(p, s, pi + 1, si + consume, memo, cache, constraints).map(|rest_caps| {
                        let mut caps = serde_json::Map::new();
                        let bound: Vec<serde_json::Value> =
                            s[si..si + consume].iter().map(node_json).collect();
                        caps.insert(name.to_string(), serde_json::json!(bound));
                        caps.extend(rest_caps);
                        caps
                    })
                })
            } else if si >= s.len() {
                None
            } else {
                match_node_capture(pnode, &s[si], cache, constraints).and_then(|node_caps| {
                    rec(p, s, pi + 1, si + 1, memo, cache, constraints).map(|rest_caps| {
                        let mut caps = node_caps;
                        caps.extend(rest_caps);
                        caps
                    })
                })
            }
        };
        memo.insert((pi, si), result.clone());
        result
    }
    rec(p, s, 0, 0, &mut HashMap::new(), cache, constraints)
}

/// Find every node in `source` (root included) matching the pattern root,
/// binding its captures inline as part of the same structural walk instead
/// of a second re-walk over each matched subtree afterwards (see
/// `match_node_capture`/`match_sequence_capture`, which check-and-bind in
/// one recursive pass).
///
/// Kind-filtered before attempting the expensive structural
/// `match_node_capture` call: a plain (non-meta-var) pattern root can only
/// ever match nodes of its own kind, so skip straight to recursing into
/// children otherwise instead of walking the pattern/source child sequences
/// just to fail on the kind check inside `match_node_capture`.
fn find_matches<'p, 's>(
    pattern: &PNode<'p>,
    node: &PNode<'s>,
    out: &mut Vec<(PNode<'s>, serde_json::Map<String, serde_json::Value>)>,
    cache: &PatternChildCache<'p>,
    constraints: &HashMap<String, String>,
) {
    let plain_kind_mismatch = meta_of(pattern).is_none() && pattern.kind() != node.kind();
    if !plain_kind_mismatch
        && let Some(captures) = match_node_capture(pattern, node, cache, constraints)
    {
        out.push((node.clone(), captures));
    }
    // Still recurse into every child regardless of whether `node` matched —
    // a matched node's descendants (or its siblings) can independently
    // contain further matches elsewhere in the tree. This recursion locates
    // further matches only; it does not re-derive the alignment of a
    // subtree that already matched above (that alignment, and its
    // captures, were already computed by `match_node_capture` in this same
    // call).
    for child in node.children() {
        find_matches(pattern, &child, out, cache, constraints);
    }
}

/// Top-level relational filters — `inside`/`has`/`precedes`/`follows` —
/// applied to a match's node after structural matching, kind-only (see
/// module docs for scope). All present fields are ANDed together.
#[derive(Default)]
struct Relations {
    inside: Option<String>,
    has: Option<String>,
    precedes: Option<String>,
    follows: Option<String>,
}

impl Relations {
    fn is_empty(&self) -> bool {
        self.inside.is_none()
            && self.has.is_none()
            && self.precedes.is_none()
            && self.follows.is_none()
    }

    /// Does `node` satisfy every relational constraint set on this
    /// `Relations`? `ancestors()`/`dfs()`/`next_all()`/`prev_all()` are
    /// ast-grep-core's own traversal primitives — `dfs()` includes `node`
    /// itself first, so `has` skips one to only consider descendants.
    fn matches(&self, node: &PNode<'_>) -> bool {
        if let Some(k) = &self.inside
            && !node.ancestors().any(|a| a.kind().as_ref() == k.as_str())
        {
            return false;
        }
        if let Some(k) = &self.has
            && !node.dfs().skip(1).any(|d| d.kind().as_ref() == k.as_str())
        {
            return false;
        }
        if let Some(k) = &self.precedes
            && !node.next_all().any(|s| s.kind().as_ref() == k.as_str())
        {
            return false;
        }
        if let Some(k) = &self.follows
            && !node.prev_all().any(|s| s.kind().as_ref() == k.as_str())
        {
            return false;
        }
        true
    }
}

/// Parse a `{"kind": "..."}` relational constraint from an optional
/// top-level input field.
fn parse_relation_kind(input: &serde_json::Value, key: &str) -> Result<Option<String>, ApiError> {
    match input.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => {
            let kind = v.get("kind").and_then(|k| k.as_str()).ok_or_else(|| {
                ApiError::new(
                    "invalid_input",
                    format!("{key:?} requires a string \"kind\" field"),
                )
            })?;
            Ok(Some(kind.to_string()))
        }
    }
}

fn node_json(node: &PNode<'_>) -> serde_json::Value {
    let pos = node.start_pos();
    let end = node.end_pos();
    serde_json::json!({
        "kind": node.kind().as_ref(),
        "text": node.text().as_ref(),
        "span": {
            "start_byte": node.range().start,
            "end_byte": node.range().end,
            "start_line": pos.line() + 1,
            "start_col": pos.column(node),
            "end_line": end.line() + 1,
            "end_col": end.column(node),
        },
    })
}

/// Depth-first search for the first node whose text matches `target`.
/// Post-order: children are checked before `node` itself, so when a wrapper
/// node's text happens to equal its single child's text exactly (e.g. a
/// whole-file pattern with one top-level statement), the more specific
/// (deepest) matching node wins instead of the outer wrapper.
fn find_text_node<'a>(node: &PNode<'a>, target: &str) -> Option<PNode<'a>> {
    for child in node.children() {
        if let Some(found) = find_text_node(&child, target) {
            return Some(found);
        }
    }
    if node.text().trim() == target {
        return Some(node.clone());
    }
    None
}

/// Resolve the language for a pattern-matching call: explicit `language`
/// input wins, otherwise infer from a single target file's extension.
/// Directory targets require an explicit `language` since the tree walk
/// covers mixed extensions.
///
/// Both paths span the full `ast-grep` grammar set (every language
/// `ast-grep-language` links), not just the extraction-supported subset:
/// structural search needs only a grammar to parse against. Explicit names
/// accept `ast-grep`'s own aliases (e.g. `ts`, `py`, `c++`).
fn resolve_lang(
    input: &serde_json::Value,
    file_hint: Option<&std::path::Path>,
) -> Result<ast_grep_language::SupportLang, ApiError> {
    use ast_grep_language::SupportLang;
    use std::str::FromStr;
    match input.get("language").and_then(|v| v.as_str()) {
        Some(l) => SupportLang::from_str(l)
            .map_err(|_| ApiError::new("invalid_input", format!("unsupported language {l:?}"))),
        None => {
            let path = file_hint.ok_or_else(|| {
                ApiError::new(
                    "invalid_input",
                    "language is required when searching a directory",
                )
            })?;
            crate::parse::any_language_for_path(path).ok_or_else(|| {
                ApiError::new(
                    "invalid_input",
                    format!("unsupported file type {}", path.display()),
                )
            })
        }
    }
}

/// Match a parsed pattern against one file's source. Returns match JSON
/// objects (without a `file` field — the caller attaches it).
/// Wrap a pattern fragment in a minimal function body so statement/expression
/// fragments that aren't valid top-level syntax (e.g. a bare method call)
/// still parse. Wrapper syntax is language-specific — a Rust-only `fn ... {}`
/// wrapper doesn't parse in TS/JS/Go/Python.
fn wrap_for_lang(lang: &ast_grep_language::SupportLang, rewritten: &str) -> String {
    use ast_grep_language::SupportLang;
    match lang {
        SupportLang::Rust => format!("fn __varde_wrapper__() {{ {rewritten} }}"),
        SupportLang::TypeScript | SupportLang::Tsx | SupportLang::JavaScript => {
            format!("function __varde_wrapper__() {{ {rewritten} }}")
        }
        SupportLang::Go | SupportLang::Swift => {
            format!("func __varde_wrapper__() {{ {rewritten} }}")
        }
        SupportLang::Python => format!("def __varde_wrapper__():\n    {rewritten}\n"),
        // C-family: a plain function body carries statement/expression
        // fragments. These languages need a statement terminator, so append a
        // `;` — a fragment that already ends in one just yields a harmless
        // trailing empty statement.
        SupportLang::C | SupportLang::Cpp | SupportLang::Dart => {
            format!("void __varde_wrapper__() {{ {rewritten}; }}")
        }
        // Java/C# require the method to live inside a type declaration.
        SupportLang::Java | SupportLang::CSharp => {
            format!("class __VardeWrapper__ {{ void __varde_wrapper__() {{ {rewritten}; }} }}")
        }
        SupportLang::Kotlin => format!("fun __varde_wrapper__() {{ {rewritten} }}"),
        SupportLang::Php => format!("<?php function __varde_wrapper__() {{ {rewritten}; }}"),
        SupportLang::Scala => {
            format!("object __VardeWrapper__ {{ def __varde_wrapper__() = {{ {rewritten} }} }}")
        }
        SupportLang::Solidity => format!(
            "contract __VardeWrapper__ {{ function __varde_wrapper__() public {{ {rewritten}; }} }}"
        ),
        SupportLang::Ruby => format!("def __varde_wrapper__\n{rewritten}\nend"),
        SupportLang::Lua => format!("function __varde_wrapper__() {rewritten} end"),
        // Shell statements are already valid at top level; other languages
        // (Bash, config/markup grammars, etc.) have no universal single-function
        // wrapper, so fall back to the bare fragment — `locate_pattern_root`
        // still prefers the raw parse whenever the fragment parses on its own.
        _ => rewritten.to_string(),
    }
}

/// Locate the pattern's root node for matching. Always prefers an exact
/// text-match lookup over blindly trusting the parse root: a pattern that
/// happens to be valid top-level syntax on its own (e.g. `throw new
/// Error($MSG)` is a complete program) would otherwise resolve to the whole
/// `program`/`source_file` node, which never matches anything since that
/// kind never appears as an inner node.
fn locate_pattern_root<'a>(
    raw_parsed: &'a crate::parse::ParsedFile,
    wrapped_parsed: &'a crate::parse::ParsedFile,
    rewritten: &str,
) -> Result<PNode<'a>, ApiError> {
    let trimmed = rewritten.trim();
    if !raw_parsed.has_error() {
        let root = raw_parsed.root.root();
        if let Some(found) = find_text_node(&root, trimmed) {
            return Ok(found);
        }
        return Ok(root);
    }
    if wrapped_parsed.has_error() {
        return Err(ApiError::new(
            "invalid_pattern",
            "pattern does not parse in the target language",
        ));
    }
    let root = wrapped_parsed.root.root();
    // Try the pattern text verbatim first. If that fails, retry with a trailing
    // `;`: several wrappers (C/C++/Dart/Java/C#/PHP/Solidity — see
    // `wrap_for_lang`) append a statement terminator, so an *expression*-form
    // fragment like `throw new $E($$$A)` (no `;`) becomes a `throw_statement`
    // whose text carries the appended `;` and never equals the semicolon-less
    // pattern. Retrying with the terminator locates that statement node — giving
    // parity with `ast-grep run`, which matches the no-`;` form fine. A fragment
    // that already ended in `;` matched on the first attempt.
    find_text_node(&root, trimmed)
        .or_else(|| find_text_node(&root, &format!("{trimmed};")))
        .ok_or_else(|| ApiError::new("invalid_pattern", "cannot locate pattern root in wrapper"))
}

/// Literal identifier/keyword/number atoms a pattern requires verbatim in any
/// source it can match — the prefilter's key. Every non-meta-var leaf in the
/// pattern must match a source leaf by exact text (`match_node_capture`), so
/// each maximal `[A-Za-z0-9_]` run in the pattern (excluding meta-var names
/// and `:kind` constraints) must appear as a byte substring of any matching
/// file. AND-ing them is therefore *conservative*: a file missing any atom
/// cannot match and can be skipped without parsing, but the filter never
/// skips a file that could match (it only ever over-selects). Punctuation
/// (`.`/`(`/`=`) is intentionally not an atom — it is trivia the "Smart"
/// matcher strips and appears in nearly every file, so it would not narrow.
/// A pattern with no literal atoms (e.g. `$A = $B`) yields an empty set and
/// the prefilter is skipped (parse everything), which is correct.
fn mandatory_atoms(pattern: &str) -> Vec<String> {
    let bytes = pattern.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut atoms = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            // Skip a `$`/`$$$` sigil, its meta-var name, and any `:kind`
            // suffix — none of that is literal source text.
            while i < bytes.len() && bytes[i] == b'$' {
                i += 1;
            }
            while i < bytes.len() && ident(bytes[i]) {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b':' {
                i += 1;
                while i < bytes.len() && ident(bytes[i]) {
                    i += 1;
                }
            }
            continue;
        }
        if ident(bytes[i]) {
            let start = i;
            while i < bytes.len() && ident(bytes[i]) {
                i += 1;
            }
            atoms.push(pattern[start..i].to_string());
            continue;
        }
        i += 1;
    }
    atoms.sort();
    atoms.dedup();
    atoms
}

fn match_source(
    pattern_root: &PNode<'_>,
    lang: &ast_grep_language::SupportLang,
    source: &str,
    constraints: &HashMap<String, String>,
    relations: &Relations,
) -> Result<Vec<serde_json::Value>, ApiError> {
    // Match on tree-sitter's error-recovered tree rather than rejecting the
    // whole file on the first syntax error. A single unparseable construct
    // (often just modern-syntax the pinned grammar version doesn't know)
    // otherwise discards *every* match in the file — a large recall loss and a
    // parity gap vs. `ast-grep run`, which matches recovered trees. ERROR and
    // MISSING nodes carry their own kinds, so they never match a real pattern's
    // node kind; tolerating them only recovers the well-formed regions and
    // never invents spurious matches.
    let source_parsed = crate::parse::parse_source(lang, source);
    let source_root = source_parsed.root.root();

    reset_budget();
    let cache: PatternChildCache<'_> = std::cell::RefCell::new(HashMap::new());
    // Locate and bind captures in one structural walk per match: `find_matches`
    // now calls `match_node_capture`, which checks alignment and collects
    // captures together, instead of the old two-phase locate-then-rewalk
    // (`find_matches` followed by a per-match `collect_captures` call).
    let mut matched: Vec<(PNode<'_>, serde_json::Map<String, serde_json::Value>)> = Vec::new();
    find_matches(
        pattern_root,
        &source_root,
        &mut matched,
        &cache,
        constraints,
    );
    if budget_exhausted() {
        return Err(ApiError::new(
            "pattern_too_complex",
            "pattern matching exceeded the backtracking step budget; \
             simplify the pattern (fewer $$$VAR markers or a narrower search)",
        ));
    }
    if !relations.is_empty() {
        matched.retain(|(node, _)| relations.matches(node));
    }

    let out: Vec<serde_json::Value> = matched
        .into_iter()
        .map(|(m, captures)| {
            serde_json::json!({
                "kind": m.kind().as_ref(),
                "text": m.text().as_ref(),
                "span": node_json(&m)["span"],
                "captures": serde_json::Value::Object(captures),
            })
        })
        .collect();
    Ok(out)
}

/// find_pattern — find AST nodes matching a `$VAR`/`$$$VAR` pattern.
///
/// Inputs: `pattern` (required); either `filePath`/`file` (search one file)
/// or `path` (search a directory tree — requires `language` since a tree may
/// mix extensions). Files with syntax errors are matched on tree-sitter's
/// error-recovered tree (matching `ast-grep run`), not skipped. A
/// `pattern_too_complex` error still fails a single-file call; in a directory
/// search it drops that one file rather than failing the whole call.
///
/// Output: array of matches `{kind, text, span, captures, file}` where
/// `captures` maps each meta-variable name to its matched node(s) — a single
/// node object for `$VAR`, an array for `$$$VAR`. Empty array when nothing
/// matches.
pub fn find_pattern(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    let pattern_text = req_str(input, "pattern")?;

    let single_file = input
        .get("filePath")
        .and_then(|v| v.as_str())
        .or_else(|| input.get("file").and_then(|v| v.as_str()));
    let dir_path = input.get("path").and_then(|v| v.as_str());

    let target = single_file.or(dir_path).ok_or_else(|| {
        ApiError::new(
            "invalid_input",
            "missing filePath (or path for a directory search)",
        )
    })?;
    let target_path = std::path::Path::new(target);
    let is_dir = target_path.is_dir();

    let lang = resolve_lang(input, (!is_dir).then_some(target_path))?;
    let (stripped, constraints) = split_kind_constraints(pattern_text);
    let rewritten = preprocess(&stripped);

    let relations = Relations {
        inside: parse_relation_kind(input, "inside")?,
        has: parse_relation_kind(input, "has")?,
        precedes: parse_relation_kind(input, "precedes")?,
        follows: parse_relation_kind(input, "follows")?,
    };

    // Parse the pattern once, up front — shared across every file in the
    // walk rather than re-parsed per file (previously 2N+2 pattern parses
    // across N files; this was the leading cause of find_pattern's ~4x
    // slowdown vs. ast-grep, see BENCHMARK.md).
    let raw_parsed = crate::parse::parse_source(&lang, &rewritten);
    let wrapped = wrap_for_lang(&lang, &rewritten);
    let wrapped_parsed = crate::parse::parse_source(&lang, &wrapped);
    let pattern_root = locate_pattern_root(&raw_parsed, &wrapped_parsed, &rewritten)?;
    let pattern_root = &pattern_root;

    // Literal atoms every match must contain verbatim; empty ⇒ no prefilter.
    let atoms = mandatory_atoms(pattern_text);

    if !is_dir {
        let source = std::fs::read_to_string(target_path)
            .map_err(|e| ApiError::new("file_error", format!("{}: {e}", target_path.display())))?;
        let matches = match_source(pattern_root, &lang, &source, &constraints, &relations)?;
        let out: Vec<_> = matches
            .into_iter()
            .map(|mut m| {
                m["file"] = serde_json::json!(target_path.display().to_string());
                m
            })
            .collect();
        return Ok(serde_json::json!(out));
    }

    let profile = std::env::var_os("VARDE_PROFILE").is_some();
    let t_total = std::time::Instant::now();

    // Overlap the directory walk with per-file parse/match instead of the old
    // two-phase "collect every path, sort, then parallel-match" pipeline. That
    // pipeline serialized the entire tree walk (and a full path sort) as a
    // barrier before the first file could be parsed — measurably a fifth of
    // the wall-clock on a large, sparse-match repo (see BENCHMARK.md). Using
    // `ignore`'s parallel walker (the same `WalkParallel` ast-grep's CLI uses)
    // lets a file be matched the instant it is discovered, on whichever worker
    // thread found it. `.hidden(false)` preserves the gitignore-respecting,
    // dot-file-including walk policy the old `collect_files_for_lang` set (see
    // that removed helper's note on the match-count gap it fixed).
    //
    // pattern_root/constraints/relations are read-only and shared by reference
    // across worker threads (the old rayon `par_iter` relied on the same
    // `Sync`-ness); each thread pushes its matches under a short-lived `Mutex`
    // guard, contended only for the append, not for the parse/match work.
    // Literal-atom prefilter: skip parsing any file that cannot contain a
    // match because it lacks one of the pattern's mandatory literal atoms (see
    // `mandatory_atoms`). This is where a persist-and-serve tool should be able
    // to beat a live-parse tool on sparse patterns — most files never reach the
    // tree-sitter parse. This prototype reads each file to substring-check it;
    // a persisted token→files index would answer the prefilter without the read
    // (the read is the only cost left for skipped files — see the profile
    // counters below). `scanned`/`parsed` count files that reached the byte
    // check vs. files that survived it into the parser.
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let results: Mutex<Vec<serde_json::Value>> = Mutex::new(Vec::new());
    let scanned = AtomicUsize::new(0);
    let parsed = AtomicUsize::new(0);
    ignore::WalkBuilder::new(target_path)
        .hidden(false)
        .filter_entry(|entry| !crate::scan::is_vcs_internal(entry))
        .build_parallel()
        .run(|| {
            Box::new(|entry| {
                use ignore::WalkState;
                let Ok(entry) = entry else {
                    return WalkState::Continue;
                };
                let path = entry.path();
                if !path.is_file()
                    || crate::parse::any_language_for_path(path).as_ref() != Some(&lang)
                {
                    return WalkState::Continue;
                }
                scanned.fetch_add(1, Ordering::Relaxed);
                let Ok(source) = std::fs::read_to_string(path) else {
                    // Unreadable/non-UTF-8 file: skip-and-report, as before.
                    return WalkState::Continue;
                };
                // Prefilter: every mandatory atom must be present, else the
                // file provably cannot match — skip the parse entirely.
                if !atoms.iter().all(|a| source.contains(a.as_str())) {
                    return WalkState::Continue;
                }
                parsed.fetch_add(1, Ordering::Relaxed);
                // Skip-and-report: a single unparseable/pathological file in a
                // tree search doesn't fail the whole call.
                if let Ok(matches) =
                    match_source(pattern_root, &lang, &source, &constraints, &relations)
                {
                    let mut local: Vec<serde_json::Value> = matches
                        .into_iter()
                        .map(|mut m| {
                            m["file"] = serde_json::json!(path.display().to_string());
                            m
                        })
                        .collect();
                    if !local.is_empty() {
                        results.lock().unwrap().append(&mut local);
                    }
                }
                WalkState::Continue
            })
        });
    let mut out = results.into_inner().unwrap();

    // The parallel walk yields matches in nondeterministic thread order; sort
    // by (file, start byte) to restore the old file-sorted-then-tree-order
    // output. Only matched results are sorted (not every walked path), so this
    // is off the hot path unless a pattern matches nearly everything.
    out.sort_by(|a, b| {
        let fa = a["file"].as_str().unwrap_or("");
        let fb = b["file"].as_str().unwrap_or("");
        fa.cmp(fb).then_with(|| {
            let sa = a["span"]["start_byte"].as_u64().unwrap_or(0);
            let sb = b["span"]["start_byte"].as_u64().unwrap_or(0);
            sa.cmp(&sb)
        })
    });

    if profile {
        let scanned = scanned.load(Ordering::Relaxed);
        let parsed = parsed.load(Ordering::Relaxed);
        eprintln!(
            "VARDE_PROFILE find_pattern: walk+match={:?} matches={} atoms={:?} \
             scanned={} parsed={} skipped_by_prefilter={} ({:.0}%)",
            t_total.elapsed(),
            out.len(),
            atoms,
            scanned,
            parsed,
            scanned - parsed,
            if scanned > 0 {
                100.0 * (scanned - parsed) as f64 / scanned as f64
            } else {
                0.0
            },
        );
    }
    Ok(serde_json::json!(out))
}
