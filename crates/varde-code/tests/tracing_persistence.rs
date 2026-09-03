//! Integration test for persist tracing (own process, so the global capture
//! subscriber is deterministic — no other test has installed a subscriber in
//! this binary).

use std::sync::{Arc, Mutex};

use varde_code::model::{
    Diagnostic, Entity, EntityKind, ExtractOutput, FileMeta, Span, Symbol, SymbolKind,
};
use varde_code::persist as persist_crate;
use varde_code::resolve::{CloneBand, Community, FileNode, ResolvedGraph};

fn span() -> Span {
    Span {
        start_byte: 0,
        end_byte: 4,
        start_line: 1,
        start_col: 0,
        end_line: 1,
        end_col: 4,
    }
}

fn entity(file_id: u32) -> Entity {
    Entity {
        kind: EntityKind::Function,
        name: "fn".to_string(),
        file_id,
        span: span(),
        enclosing_function: None,
        method: None,
        path: None,
        status: None,
        body_shape: None,
        body_minhash: None,
        is_async: None,
        is_test: false,
        owner_type: None,
    }
}

fn symbol(file_id: u32) -> Symbol {
    Symbol {
        kind: SymbolKind::Binding,
        name: "fn".to_string(),
        file_id,
        span: span(),
    }
}

fn diagnostic(file_id: u32) -> Diagnostic {
    Diagnostic {
        file_id,
        message: "skipped".to_string(),
        severity: "error".to_string(),
    }
}

fn temp_db(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "varde-persist-tracing-{}-{}",
        tag,
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir creates");
    let db = dir.join("index.db");
    let _ = std::fs::remove_file(&db);
    db
}

#[derive(Clone)]
struct CaptureSubscriber {
    events: Arc<Mutex<Vec<String>>>,
}

impl tracing::Subscriber for CaptureSubscriber {
    fn enabled(&self, _m: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn register_callsite(
        &self,
        _m: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::always()
    }
    fn new_span(&self, attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        self.events
            .lock()
            .unwrap()
            .push(format!("span:{}", attrs.metadata().name()));
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _s: &tracing::span::Id, _v: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _s: &tracing::span::Id, _f: &tracing::span::Id) {}
    fn event(&self, _event: &tracing::Event<'_>) {}
    fn enter(&self, _s: &tracing::span::Id) {}
    fn exit(&self, _s: &tracing::span::Id) {}
}

mod persist {
    pub mod tracing_captures_span_per_stage {
        use super::super::*;

        #[test]
        fn captures_a_span_per_major_stage() {
            static INSTALL: std::sync::Once = std::sync::Once::new();
            static BUFFER: std::sync::LazyLock<Arc<Mutex<Vec<String>>>> =
                std::sync::LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));
            INSTALL.call_once(|| {
                let subscriber = CaptureSubscriber {
                    events: BUFFER.clone(),
                };
                let _ = tracing::subscriber::set_global_default(subscriber);
                tracing::callsite::rebuild_interest_cache();
            });
            let events = BUFFER.clone();

            let db_path = temp_db("tracing");
            let output = vec![ExtractOutput {
                entities: vec![entity(0)],
                symbols: vec![symbol(0)],
                diagnostics: vec![diagnostic(1)],
                files: vec!["a.rs".to_string(), "c.rs".to_string()],
                file_meta: vec![
                    FileMeta {
                        mtime: 0,
                        size: 0,
                        content_hash: "0000000000000000".to_string()
                    };
                    2
                ],
            }];
            let graph = ResolvedGraph {
                nodes: vec![FileNode {
                    path: "a.rs".to_string(),
                    community_id: Some(0),
                    fan_in: 0,
                    fan_out: 0,
                }],
                edges: vec![],
                communities: vec![Community {
                    id: 0,
                    members: vec!["a.rs".to_string()],
                }],
                clone_bands: vec![CloneBand {
                    id: 0,
                    members: vec![0],
                }],
            };

            persist_crate::persist(
                &db_path,
                &output,
                &graph,
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
            )
            .expect("persist succeeds");

            let captured = events.lock().unwrap();
            for stage in [
                "persist",
                "schema",
                "files",
                "entities",
                "symbols",
                "diagnostics",
                "edges",
                "communities",
                "clone_bands",
            ] {
                assert!(
                    captured.iter().any(|e| e == &format!("span:{stage}")),
                    "expected a span for stage {stage:?}; captured: {captured:?}"
                );
            }
        }
    }
}
