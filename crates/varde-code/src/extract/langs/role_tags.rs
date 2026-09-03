//! Shared role-tag rule type + per-language declarative tables.
//!
//! A "role tag" is a coarse semantic label (route handler, page component,
//! CLI command, ...) inferred from a decorator/annotation/attribute name, a
//! base class, or a file path convention — the same three signals every
//! extractor already surfaces via `EntityKind::Decorator` entities,
//! `extends` relationships, and file paths. Rather than scattering
//! per-language if/else string comparisons wherever role tags are consumed,
//! every language contributes a `&[RoleTagRule]` table here and callers
//! match against it through one shared function.
//!
//! This module only defines the types, the per-language tables, and the
//! matching function — it is not wired into any query yet (see the
//! `semantic-entrypoint-query` plan task for that).

/// A coarse semantic role inferred for a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoleTag {
    RouteHandler,
    PageComponent,
    CliCommand,
    BackgroundJob,
    EventListener,
    Middleware,
}

/// One declarative rule: a symbol matches when every `Some(_)` field matches
/// the corresponding signal (decorator/annotation text, base class name, or
/// a glob over the symbol's file path). `None` fields are wildcards.
#[derive(Debug, Clone, Copy)]
pub struct RoleTagRule {
    pub decorator: Option<&'static str>,
    pub base_class: Option<&'static str>,
    pub path_glob: Option<&'static str>,
    pub role: RoleTag,
}

/// Python: Flask route decorators + a `pages/` directory convention.
pub const PYTHON_RULES: &[RoleTagRule] = &[
    RoleTagRule {
        decorator: Some("app.route"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("route"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("celery.task"),
        base_class: None,
        path_glob: None,
        role: RoleTag::BackgroundJob,
    },
    RoleTagRule {
        decorator: Some("task"),
        base_class: None,
        path_glob: None,
        role: RoleTag::BackgroundJob,
    },
    RoleTagRule {
        decorator: Some("click.command"),
        base_class: None,
        path_glob: None,
        role: RoleTag::CliCommand,
    },
    RoleTagRule {
        decorator: Some("command"),
        base_class: None,
        path_glob: None,
        role: RoleTag::CliCommand,
    },
];

/// Java: Spring MVC route annotations (`spring_route_of` in `java.rs`
/// recognizes the same `*Mapping` family for the narrower `Route` entity).
pub const JAVA_RULES: &[RoleTagRule] = &[
    RoleTagRule {
        decorator: Some("GetMapping"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("PostMapping"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("PutMapping"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("DeleteMapping"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("PatchMapping"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("RequestMapping"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("Controller"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("RestController"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("Scheduled"),
        base_class: None,
        path_glob: None,
        role: RoleTag::BackgroundJob,
    },
    RoleTagRule {
        decorator: Some("EventListener"),
        base_class: None,
        path_glob: None,
        role: RoleTag::EventListener,
    },
];

/// C#: ASP.NET Core attributes (`cs.rs` recognizes `app.MapGet(...)`
/// minimal-API calls for the narrower `Route` entity; `[ApiController]` /
/// `[Route]` are the attribute-based MVC-style equivalent).
pub const CS_RULES: &[RoleTagRule] = &[
    RoleTagRule {
        decorator: Some("ApiController"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("Route"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("HttpGet"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("HttpPost"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("Controller"),
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("ControllerBase"),
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("IHostedService"),
        path_glob: None,
        role: RoleTag::BackgroundJob,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("BackgroundService"),
        path_glob: None,
        role: RoleTag::BackgroundJob,
    },
];

/// TypeScript: Nest.js-style route/middleware decorators, a `pages/`
/// directory convention (Next.js), and Angular component decorators.
pub const TS_RULES: &[RoleTagRule] = &[
    RoleTagRule {
        decorator: Some("Get"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("Post"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("Put"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("Delete"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("Controller"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("Injectable"),
        base_class: None,
        path_glob: None,
        role: RoleTag::Middleware,
    },
    RoleTagRule {
        decorator: Some("Component"),
        base_class: None,
        path_glob: None,
        role: RoleTag::PageComponent,
    },
    RoleTagRule {
        decorator: Some("Cron"),
        base_class: None,
        path_glob: None,
        role: RoleTag::BackgroundJob,
    },
    RoleTagRule {
        decorator: Some("OnEvent"),
        base_class: None,
        path_glob: None,
        role: RoleTag::EventListener,
    },
    RoleTagRule {
        decorator: None,
        base_class: None,
        path_glob: Some("**/pages/**"),
        role: RoleTag::PageComponent,
    },
];

/// TSX shares TypeScript's decorator/annotation vocabulary but leans harder
/// on the `pages/`-directory convention since TSX files are typically
/// components themselves.
pub const TSX_RULES: &[RoleTagRule] = &[
    RoleTagRule {
        decorator: Some("Component"),
        base_class: None,
        path_glob: None,
        role: RoleTag::PageComponent,
    },
    RoleTagRule {
        decorator: None,
        base_class: None,
        path_glob: Some("**/pages/**"),
        role: RoleTag::PageComponent,
    },
    RoleTagRule {
        decorator: None,
        base_class: None,
        path_glob: Some("**/app/**"),
        role: RoleTag::PageComponent,
    },
];

/// Plain JavaScript: Express/Koa-style middleware and job runners rarely use
/// decorators (no stage-2 decorator syntax assumed), so this table leans on
/// the `pages/` convention for framework-agnostic file layouts (e.g.
/// Next.js `.js` pages) — decorator/base-class signals are left empty here
/// since JS extraction does not surface `EntityKind::Decorator` in the same
/// way TS/TSX do.
pub const JS_RULES: &[RoleTagRule] = &[RoleTagRule {
    decorator: None,
    base_class: None,
    path_glob: Some("**/pages/**"),
    role: RoleTag::PageComponent,
}];

/// Minimal glob match supporting `*` (any run of non-`/` chars) and `**`
/// (any run of chars including `/`), sufficient for the directory-convention
/// globs in these tables (e.g. `"**/pages/**"`). Not a general-purpose glob
/// implementation.
fn glob_match(pattern: &str, text: &str) -> bool {
    fn helper(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') if p.get(1) == Some(&b'*') => {
                let rest = &p[2..];
                let rest = if rest.first() == Some(&b'/') {
                    &rest[1..]
                } else {
                    rest
                };
                (0..=t.len()).any(|i| helper(rest, &t[i..]))
            }
            Some(b'*') => {
                let rest = &p[1..];
                (0..=t.len())
                    .take_while(|&i| i == 0 || t[i - 1] != b'/')
                    .any(|i| helper(rest, &t[i..]))
            }
            Some(&c) => !t.is_empty() && t[0] == c && helper(&p[1..], &t[1..]),
        }
    }
    helper(pattern.as_bytes(), text.as_bytes())
}

/// Match a symbol's decorator text, base-class name, and/or file path
/// against a per-language rule table, returning the first matching rule's
/// role tag. A rule matches only when every `Some(_)` field it declares
/// matches the corresponding `Some(_)` signal passed in — `None` rule
/// fields are wildcards, but a rule field of `Some(_)` never matches a
/// `None` signal.
pub fn match_role_tag(
    rules: &[RoleTagRule],
    decorator: Option<&str>,
    base_class: Option<&str>,
    path: Option<&str>,
) -> Option<RoleTag> {
    rules
        .iter()
        .find(|rule| {
            let decorator_ok = match rule.decorator {
                None => true,
                Some(want) => decorator == Some(want),
            };
            let base_class_ok = match rule.base_class {
                None => true,
                Some(want) => base_class == Some(want),
            };
            let path_ok = match rule.path_glob {
                None => true,
                Some(glob) => path.is_some_and(|p| glob_match(glob, p)),
            };
            decorator_ok && base_class_ok && path_ok
        })
        .map(|rule| rule.role)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flask_app_route_decorator_matches_route_handler() {
        // Fixture entity set: two decorator "entities" on different
        // symbols; only the Flask `@app.route` one should match.
        struct FixtureEntity {
            decorator: Option<&'static str>,
            base_class: Option<&'static str>,
            path: Option<&'static str>,
        }
        let fixtures = [
            FixtureEntity {
                decorator: Some("app.route"),
                base_class: None,
                path: Some("app.py"),
            },
            FixtureEntity {
                decorator: Some("staticmethod"),
                base_class: None,
                path: Some("app.py"),
            },
        ];

        let matches: Vec<Option<RoleTag>> = fixtures
            .iter()
            .map(|f| match_role_tag(PYTHON_RULES, f.decorator, f.base_class, f.path))
            .collect();

        assert_eq!(matches[0], Some(RoleTag::RouteHandler));
        assert_eq!(matches[1], None);
    }

    #[test]
    fn java_rest_controller_annotation_matches_route_handler() {
        assert_eq!(
            match_role_tag(JAVA_RULES, Some("RestController"), None, None),
            Some(RoleTag::RouteHandler)
        );
        assert_eq!(
            match_role_tag(JAVA_RULES, Some("Scheduled"), None, None),
            Some(RoleTag::BackgroundJob)
        );
        assert_eq!(
            match_role_tag(JAVA_RULES, Some("Unrelated"), None, None),
            None
        );
    }

    #[test]
    fn cs_controller_base_class_matches_route_handler() {
        assert_eq!(
            match_role_tag(CS_RULES, None, Some("ControllerBase"), None),
            Some(RoleTag::RouteHandler)
        );
        assert_eq!(
            match_role_tag(CS_RULES, None, Some("BackgroundService"), None),
            Some(RoleTag::BackgroundJob)
        );
    }

    #[test]
    fn ts_nest_get_decorator_matches_route_handler() {
        assert_eq!(
            match_role_tag(TS_RULES, Some("Get"), None, None),
            Some(RoleTag::RouteHandler)
        );
        assert_eq!(
            match_role_tag(TS_RULES, None, None, Some("src/pages/index.ts")),
            Some(RoleTag::PageComponent)
        );
    }

    #[test]
    fn tsx_pages_path_matches_page_component() {
        assert_eq!(
            match_role_tag(TSX_RULES, None, None, Some("src/pages/Home.tsx")),
            Some(RoleTag::PageComponent)
        );
        assert_eq!(
            match_role_tag(TSX_RULES, None, None, Some("src/lib/util.tsx")),
            None
        );
    }

    #[test]
    fn js_pages_path_matches_page_component() {
        assert_eq!(
            match_role_tag(JS_RULES, None, None, Some("pages/about.js")),
            Some(RoleTag::PageComponent)
        );
        assert_eq!(
            match_role_tag(JS_RULES, None, None, Some("lib/util.js")),
            None
        );
    }
}
