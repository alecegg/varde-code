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
    /// A language process entrypoint: the `main`/`Main` function a compiled or
    /// interpreted program starts at (Rust/Go/C/C++/Java/Kotlin `main`, C#
    /// `Main`). Not decorator/base-class inferred — detected structurally by
    /// name + language (see `query::entrypoints::detect_process_mains`).
    ProcessMain,
}

/// One declarative rule: a symbol matches when every `Some(_)` field matches
/// the corresponding signal (decorator/annotation text, base class name, or
/// a glob over the symbol's file path). `None` fields are wildcards.
///
/// The `decorator` value is normally matched exactly, but a leading `*.`
/// makes it a *suffix* rule: it matches any decorator whose final
/// dotted segment equals the text after `*.`. This is how framework
/// decorators bound to an arbitrarily-named receiver are matched — e.g.
/// FastAPI's `@app.get` / `@router.get` / `@v1.get` and Flask blueprints'
/// `@admin_bp.route`, which the extractor stores as the full callee
/// (`app.get`, `admin_bp.route`). `*` never appears in a real identifier,
/// so the two forms can't collide. A bare decorator (no dot) still matches a
/// `*.name` rule, since its only segment is also its last.
#[derive(Debug, Clone, Copy)]
pub struct RoleTagRule {
    pub decorator: Option<&'static str>,
    pub base_class: Option<&'static str>,
    pub path_glob: Option<&'static str>,
    pub role: RoleTag,
}

/// Python: route decorators for the three dominant web frameworks — Flask
/// (`@app.route` / blueprint `@bp.route`), FastAPI (`@app.get` /
/// `@router.post`), and Django REST Framework (`@api_view` plus class-based
/// views keyed by base class) — Celery/Click job/CLI decorators, and a
/// `pages/` directory convention. The route decorators use the `*.` suffix
/// form because the receiver (`app`, `router`, `bp`, `v1`, ...) is
/// application-chosen and the extractor stores the full callee.
pub const PYTHON_RULES: &[RoleTagRule] = &[
    // Flask/blueprint `@<x>.route` and FastAPI/router `@<x>.<verb>`.
    RoleTagRule {
        decorator: Some("*.route"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("*.get"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("*.post"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("*.put"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("*.delete"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("*.patch"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("*.websocket"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    // Django REST Framework function-based view.
    RoleTagRule {
        decorator: Some("api_view"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    // Django / DRF class-based views, keyed by base class (they carry no
    // route decorator of their own — routing lives in `urls.py`).
    RoleTagRule {
        decorator: None,
        base_class: Some("APIView"),
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("GenericAPIView"),
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("ViewSet"),
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("ModelViewSet"),
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("GenericViewSet"),
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    // FastAPI custom middleware: `@app.middleware("http")`.
    RoleTagRule {
        decorator: Some("*.middleware"),
        base_class: None,
        path_glob: None,
        role: RoleTag::Middleware,
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
    // Celery's decorator is commonly `@shared_task` or `@<app>.task`.
    RoleTagRule {
        decorator: Some("shared_task"),
        base_class: None,
        path_glob: None,
        role: RoleTag::BackgroundJob,
    },
    RoleTagRule {
        decorator: Some("*.task"),
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
    // JAX-RS (Jersey / RESTEasy / MicroProfile) — the other major Java REST
    // standard alongside Spring MVC. `@Path` marks a resource class/method and
    // the bare verb annotations (`@GET`, `@POST`, ...) mark the actions.
    RoleTagRule {
        decorator: Some("Path"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("GET"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("POST"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("PUT"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("DELETE"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("PATCH"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("HEAD"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("OPTIONS"),
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
    // Message-driven listeners — the non-HTTP entrypoints of a Spring/Jakarta
    // app (Kafka, RabbitMQ/AMQP, JMS, and STOMP/WebSocket message mappings).
    RoleTagRule {
        decorator: Some("KafkaListener"),
        base_class: None,
        path_glob: None,
        role: RoleTag::EventListener,
    },
    RoleTagRule {
        decorator: Some("RabbitListener"),
        base_class: None,
        path_glob: None,
        role: RoleTag::EventListener,
    },
    RoleTagRule {
        decorator: Some("JmsListener"),
        base_class: None,
        path_glob: None,
        role: RoleTag::EventListener,
    },
    RoleTagRule {
        decorator: Some("MessageMapping"),
        base_class: None,
        path_glob: None,
        role: RoleTag::EventListener,
    },
];

/// Kotlin: the JVM web stacks a Kotlin service uses share Java's annotation
/// vocabulary. Spring's `@RestController`/`@*Mapping` family carries over
/// verbatim; Micronaut/Ktor-style bare verb annotations (`@Get`, `@Post`,
/// ...) are added too. `kotlin.rs` now emits `Decorator` entities for these
/// (the grammar has no `name` field on `annotation`, so the type identifier
/// is recovered by descent).
pub const KOTLIN_RULES: &[RoleTagRule] = &[
    RoleTagRule {
        decorator: Some("RestController"),
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
        decorator: Some("RequestMapping"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
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
    // Micronaut / Ktor-resource bare verb annotations.
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
        decorator: Some("Patch"),
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
    RoleTagRule {
        decorator: Some("KafkaListener"),
        base_class: None,
        path_glob: None,
        role: RoleTag::EventListener,
    },
    RoleTagRule {
        decorator: Some("RabbitListener"),
        base_class: None,
        path_glob: None,
        role: RoleTag::EventListener,
    },
];

/// Scala: `scala.rs` emits `Decorator` entities carrying the annotation's
/// full (possibly dotted) type — so cask's `@cask.get`/`@cask.post` match via
/// the `*.` suffix form. Play Framework controllers carry no annotation and
/// are matched by their base class (`BaseController` / `AbstractController` /
/// `InjectedController`).
pub const SCALA_RULES: &[RoleTagRule] = &[
    RoleTagRule {
        decorator: Some("*.get"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("*.post"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("*.put"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("*.delete"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("*.patch"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("BaseController"),
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("AbstractController"),
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("InjectedController"),
        path_glob: None,
        role: RoleTag::RouteHandler,
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
    // The full ASP.NET Core HTTP-verb attribute set — parity with the Java
    // `*Mapping` family above. Without these, `[HttpPut]`/`[HttpDelete]`/
    // `[HttpPatch]` action methods (the write half of a typical REST
    // controller) were silently dropped from entrypoints and flows.
    RoleTagRule {
        decorator: Some("HttpPut"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("HttpDelete"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("HttpPatch"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("HttpHead"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("HttpOptions"),
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
    // The rest of the Nest.js HTTP-method decorator family — parity with the
    // Java `*Mapping` and C# `Http*` verb sets. Without these, `@Patch`/
    // `@Head`/`@Options`/`@All`/`@Sse` handlers (the write half and the
    // streaming/catch-all half of a typical Nest controller) were silently
    // dropped from entrypoints and flows.
    RoleTagRule {
        decorator: Some("Patch"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("Head"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("Options"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("All"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: Some("Sse"),
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
    // Nest.js microservice / websocket message handlers — the non-HTTP
    // entrypoints of a Nest app.
    RoleTagRule {
        decorator: Some("MessagePattern"),
        base_class: None,
        path_glob: None,
        role: RoleTag::EventListener,
    },
    RoleTagRule {
        decorator: Some("EventPattern"),
        base_class: None,
        path_glob: None,
        role: RoleTag::EventListener,
    },
    RoleTagRule {
        decorator: Some("SubscribeMessage"),
        base_class: None,
        path_glob: None,
        role: RoleTag::EventListener,
    },
    RoleTagRule {
        decorator: Some("Injectable"),
        base_class: Some("NestMiddleware"),
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

/// Ruby: Rails/Sidekiq base-class conventions. Ruby extraction emits no
/// `Decorator` entities, so role tags key entirely off the class hierarchy —
/// a `superclass` (`class FooController < ApplicationController`) surfaces as
/// `Extends`, and a mixin (`include Sidekiq::Job`) surfaces as `Implements`;
/// both are fed to the base-class signal by the entrypoint query. The stored
/// name is the full scoped constant text (`ActionController::Base`), so the
/// rules match on the fully-qualified form.
pub const RUBY_RULES: &[RoleTagRule] = &[
    RoleTagRule {
        decorator: None,
        base_class: Some("ApplicationController"),
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("ActionController::Base"),
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("ActionController::API"),
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    // A class extending *any* `…Controller` — Devise engine controllers
    // (`< Devise::SessionsController`), Rails engines, and app-specific base
    // controllers (`< Api::BaseController`, `< Admin::BaseController`) that the
    // exact-base rules above miss. Suffix match on the base's final segment.
    RoleTagRule {
        decorator: None,
        base_class: Some("*Controller"),
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("ApplicationJob"),
        path_glob: None,
        role: RoleTag::BackgroundJob,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("ActiveJob::Base"),
        path_glob: None,
        role: RoleTag::BackgroundJob,
    },
    // Sidekiq workers `include Sidekiq::Job` (or the legacy `Sidekiq::Worker`)
    // — surfaced as `Implements`, which the entrypoint query folds into the
    // base-class signal alongside `Extends`.
    RoleTagRule {
        decorator: None,
        base_class: Some("Sidekiq::Job"),
        path_glob: None,
        role: RoleTag::BackgroundJob,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("Sidekiq::Worker"),
        path_glob: None,
        role: RoleTag::BackgroundJob,
    },
    // Action Cable channels are the websocket/event entrypoints of a Rails app.
    RoleTagRule {
        decorator: None,
        base_class: Some("ApplicationCable::Channel"),
        path_glob: None,
        role: RoleTag::EventListener,
    },
];

/// PHP: Symfony/API-Platform attributes (`#[Route]`, `#[Get]`, ...) — now
/// emitted as `Decorator` entities by `php.rs` — plus the Laravel/Symfony
/// controller base classes (`Controller` / `AbstractController`). Laravel's
/// `Route::get(...)` route table is call-based and surfaces separately via
/// [`crate::query::entrypoints::detect_routes`]. Attribute/base-class names
/// are the bare (imported) form; a fully-qualified `#[\...\Route]` normalizes
/// differently and is not matched (same limitation as elsewhere).
pub const PHP_RULES: &[RoleTagRule] = &[
    RoleTagRule {
        decorator: Some("Route"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
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
        decorator: Some("Patch"),
        base_class: None,
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    // Symfony Messenger handler / Console command attributes.
    RoleTagRule {
        decorator: Some("AsMessageHandler"),
        base_class: None,
        path_glob: None,
        role: RoleTag::EventListener,
    },
    RoleTagRule {
        decorator: Some("AsCommand"),
        base_class: None,
        path_glob: None,
        role: RoleTag::CliCommand,
    },
    // Laravel / Symfony controller base classes.
    RoleTagRule {
        decorator: None,
        base_class: Some("AbstractController"),
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    RoleTagRule {
        decorator: None,
        base_class: Some("Controller"),
        path_glob: None,
        role: RoleTag::RouteHandler,
    },
    // Symfony/Laravel console command base class.
    RoleTagRule {
        decorator: None,
        base_class: Some("Command"),
        path_glob: None,
        role: RoleTag::CliCommand,
    },
];

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

/// The final segment of a possibly-qualified name, split on the separators
/// the extractors emit for qualified references: `.` (Java/Kotlin/Scala/JS
/// dotted names), `/` (PHP, after namespace normalization), and `::` (Ruby
/// scope resolution). A bare name is its own last segment.
fn last_segment(name: &str) -> &str {
    name.rsplit(['.', '/', ':']).next().unwrap_or(name)
}

/// True when a *bare* rule value matches a possibly-qualified signal by its
/// final segment — so `Controller` matches `App/Http/Controllers/Controller`,
/// `GetMapping` matches `org.springframework...GetMapping`, and
/// `ApplicationController` matches `Api::ApplicationController`. A qualified
/// rule value (one that itself contains a separator, e.g. `celery.task` or
/// `ActionController::Base`) is left to exact matching, since its last segment
/// alone would be ambiguous.
fn matches_qualified(want: &str, signal: Option<&str>) -> bool {
    !want.contains(['.', '/', ':']) && signal.is_some_and(|s| last_segment(s) == want)
}

/// Match a symbol's decorator text, base-class name, and/or file path
/// against a per-language rule table, returning the first matching rule's
/// role tag. A rule matches only when every `Some(_)` field it declares
/// matches the corresponding `Some(_)` signal passed in — `None` rule
/// fields are wildcards, but a rule field of `Some(_)` never matches a
/// `None` signal. Bare rule values also match a fully-qualified signal by its
/// final segment (see [`matches_qualified`]).
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
                Some(want) => match want.strip_prefix("*.") {
                    // Suffix rule: match the decorator's final dotted segment,
                    // so `@app.get` / `@router.get` / bare `@get` all match
                    // `*.get` regardless of the receiver's name.
                    Some(suffix) => decorator.is_some_and(|d| d.rsplit('.').next() == Some(suffix)),
                    None => decorator == Some(want) || matches_qualified(want, decorator),
                },
            };
            let base_class_ok = match rule.base_class {
                None => true,
                Some(want) => match want.strip_prefix('*') {
                    // Suffix rule: match when the base class's final segment
                    // ends with the literal after `*`, so `*Controller` matches
                    // `Devise::SessionsController`, `Admin::BaseController`, and
                    // any app-specific `< Api::BaseController` — the Rails
                    // convention that a class extending *something*-Controller
                    // is itself a controller.
                    Some(suffix) => base_class.is_some_and(|b| last_segment(b).ends_with(suffix)),
                    None => base_class == Some(want) || matches_qualified(want, base_class),
                },
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
    fn python_fastapi_and_flask_blueprint_route_decorators_match() {
        // Receiver-agnostic suffix matching: FastAPI `@app.get`/`@router.post`
        // and Flask blueprint `@admin_bp.route` (arbitrary receiver name) must
        // all role-tag as route handlers, and the DRF function-view `@api_view`.
        for dec in [
            "app.get",
            "router.post",
            "v1.put",
            "app.delete",
            "app.patch",
            "app.websocket",
            "admin_bp.route",
            "app.route",
            "api_view",
        ] {
            assert_eq!(
                match_role_tag(PYTHON_RULES, Some(dec), None, None),
                Some(RoleTag::RouteHandler),
                "@{dec} must be a route handler"
            );
        }
        // Django/DRF class-based views key off the base class.
        for base in ["APIView", "ModelViewSet", "GenericViewSet"] {
            assert_eq!(
                match_role_tag(PYTHON_RULES, None, Some(base), None),
                Some(RoleTag::RouteHandler),
                "class extending {base} must be a route handler"
            );
        }
        // Celery jobs (bare `@shared_task`, `@app.task`, `@celery.task`).
        for dec in ["shared_task", "app.task", "celery.task"] {
            assert_eq!(
                match_role_tag(PYTHON_RULES, Some(dec), None, None),
                Some(RoleTag::BackgroundJob),
                "@{dec} must be a background job"
            );
        }
    }

    #[test]
    fn suffix_rule_does_not_over_match_unrelated_receivers() {
        // A bare decorator whose name isn't a verb must not match, and the
        // suffix form must still respect the segment boundary (`getter` is not
        // `get`).
        assert_eq!(
            match_role_tag(PYTHON_RULES, Some("staticmethod"), None, None),
            None
        );
        assert_eq!(
            match_role_tag(PYTHON_RULES, Some("x.getter"), None, None),
            None
        );
        assert_eq!(
            match_role_tag(PYTHON_RULES, Some("click.option"), None, None),
            None
        );
    }

    #[test]
    fn ruby_controller_suffix_rule_matches_engine_and_namespaced_bases() {
        // The `*Controller` suffix rule catches Devise/engine and app-specific
        // base controllers the exact-name rules miss.
        for base in [
            "Devise::SessionsController",
            "Admin::BaseController",
            "Api::V1::BaseController",
            "ApplicationController",
        ] {
            assert_eq!(
                match_role_tag(RUBY_RULES, None, Some(base), None),
                Some(RoleTag::RouteHandler),
                "class extending {base} must be a route handler"
            );
        }
        // A non-controller base must not match the suffix rule (and jobs still
        // route to BackgroundJob, not RouteHandler).
        assert_eq!(
            match_role_tag(RUBY_RULES, None, Some("SomeService"), None),
            None
        );
        assert_eq!(
            match_role_tag(RUBY_RULES, None, Some("ApplicationJob"), None),
            Some(RoleTag::BackgroundJob)
        );
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
    fn java_jaxrs_and_messaging_annotations_match() {
        // JAX-RS resource + verb annotations are route handlers.
        for dec in [
            "Path", "GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS",
        ] {
            assert_eq!(
                match_role_tag(JAVA_RULES, Some(dec), None, None),
                Some(RoleTag::RouteHandler),
                "@{dec} must be a route handler"
            );
        }
        // Message-driven listeners are event listeners.
        for dec in [
            "KafkaListener",
            "RabbitListener",
            "JmsListener",
            "MessageMapping",
        ] {
            assert_eq!(
                match_role_tag(JAVA_RULES, Some(dec), None, None),
                Some(RoleTag::EventListener),
                "@{dec} must be an event listener"
            );
        }
    }

    #[test]
    fn kotlin_spring_and_micronaut_annotations_match() {
        // Spring (`@RestController`, `@GetMapping`) and Micronaut/Ktor bare
        // verbs (`@Get`) all role-tag as route handlers.
        for dec in [
            "RestController",
            "Controller",
            "RequestMapping",
            "GetMapping",
            "PostMapping",
            "Get",
            "Post",
            "Patch",
        ] {
            assert_eq!(
                match_role_tag(KOTLIN_RULES, Some(dec), None, None),
                Some(RoleTag::RouteHandler),
                "@{dec} must be a route handler"
            );
        }
        assert_eq!(
            match_role_tag(KOTLIN_RULES, Some("KafkaListener"), None, None),
            Some(RoleTag::EventListener)
        );
        assert_eq!(
            match_role_tag(KOTLIN_RULES, Some("Scheduled"), None, None),
            Some(RoleTag::BackgroundJob)
        );
    }

    #[test]
    fn scala_cask_annotations_and_play_base_classes_match() {
        // cask's dotted annotations match via the `*.` suffix form.
        for dec in ["cask.get", "cask.post", "cask.put"] {
            assert_eq!(
                match_role_tag(SCALA_RULES, Some(dec), None, None),
                Some(RoleTag::RouteHandler),
                "@{dec} must be a route handler"
            );
        }
        // Play controllers match by base class.
        for base in ["BaseController", "AbstractController", "InjectedController"] {
            assert_eq!(
                match_role_tag(SCALA_RULES, None, Some(base), None),
                Some(RoleTag::RouteHandler),
                "class extending {base} must be a route handler"
            );
        }
    }

    #[test]
    fn php_symfony_laravel_attributes_and_base_classes_match() {
        // Symfony/API-Platform attributes.
        for dec in ["Route", "Get", "Post", "Put", "Delete", "Patch"] {
            assert_eq!(
                match_role_tag(PHP_RULES, Some(dec), None, None),
                Some(RoleTag::RouteHandler),
                "#[{dec}] must be a route handler"
            );
        }
        // Controller base classes (Symfony `AbstractController`, Laravel
        // `Controller`).
        for base in ["AbstractController", "Controller"] {
            assert_eq!(
                match_role_tag(PHP_RULES, None, Some(base), None),
                Some(RoleTag::RouteHandler),
                "class extending {base} must be a route handler"
            );
        }
        assert_eq!(
            match_role_tag(PHP_RULES, Some("AsMessageHandler"), None, None),
            Some(RoleTag::EventListener)
        );
        assert_eq!(
            match_role_tag(PHP_RULES, Some("AsCommand"), None, None),
            Some(RoleTag::CliCommand)
        );
    }

    #[test]
    fn fully_qualified_annotations_and_base_classes_match_by_last_segment() {
        // PHP: fully-qualified attribute and base class (normalized with `/`).
        assert_eq!(
            match_role_tag(
                PHP_RULES,
                Some("Symfony/Component/Routing/Annotation/Route"),
                None,
                None,
            ),
            Some(RoleTag::RouteHandler)
        );
        assert_eq!(
            match_role_tag(
                PHP_RULES,
                None,
                Some("Symfony/Bundle/FrameworkBundle/Controller/AbstractController"),
                None,
            ),
            Some(RoleTag::RouteHandler)
        );
        // Java: fully-qualified annotation (dotted).
        assert_eq!(
            match_role_tag(
                JAVA_RULES,
                Some("org.springframework.web.bind.annotation.GetMapping"),
                None,
                None,
            ),
            Some(RoleTag::RouteHandler)
        );
        // Ruby: namespaced controller (`::`).
        assert_eq!(
            match_role_tag(RUBY_RULES, None, Some("Api::ApplicationController"), None),
            Some(RoleTag::RouteHandler)
        );
        // Negative: a same-suffix-but-different word must not match — the last
        // segment `MyController` is not `Controller`.
        assert_eq!(
            match_role_tag(PHP_RULES, None, Some("App/Http/MyController"), None),
            None,
            "MyController must not match the bare `Controller` rule"
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
    fn cs_all_http_verb_attributes_match_route_handler() {
        // The write-side verbs were previously missing; every ASP.NET Core
        // HTTP-verb attribute must role-tag as a route handler.
        for verb in [
            "HttpGet",
            "HttpPost",
            "HttpPut",
            "HttpDelete",
            "HttpPatch",
            "HttpHead",
            "HttpOptions",
        ] {
            assert_eq!(
                match_role_tag(CS_RULES, Some(verb), None, None),
                Some(RoleTag::RouteHandler),
                "[{verb}] must be a route handler"
            );
        }
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
    fn ts_all_nest_http_verb_decorators_match_route_handler() {
        // The write/streaming/catch-all verbs were previously missing; every
        // Nest.js HTTP-method decorator must role-tag as a route handler.
        for verb in [
            "Get", "Post", "Put", "Delete", "Patch", "Head", "Options", "All", "Sse",
        ] {
            assert_eq!(
                match_role_tag(TS_RULES, Some(verb), None, None),
                Some(RoleTag::RouteHandler),
                "@{verb} must be a route handler"
            );
        }
        // Nest microservice/websocket message handlers are event listeners.
        for dec in ["MessagePattern", "EventPattern", "SubscribeMessage"] {
            assert_eq!(
                match_role_tag(TS_RULES, Some(dec), None, None),
                Some(RoleTag::EventListener),
                "@{dec} must be an event listener"
            );
        }
    }

    #[test]
    fn ts_nest_middleware_requires_injectable_and_nest_middleware() {
        assert_eq!(
            match_role_tag(TS_RULES, Some("Injectable"), Some("NestMiddleware"), None),
            Some(RoleTag::Middleware)
        );
        assert_eq!(
            match_role_tag(TS_RULES, Some("Injectable"), None, None),
            None,
            "ordinary Nest services are not middleware"
        );
        assert_eq!(
            match_role_tag(TS_RULES, None, Some("NestMiddleware"), None),
            None,
            "the interface alone is not a middleware entrypoint"
        );
    }

    #[test]
    fn ruby_rails_base_classes_match_roles() {
        // Rails controllers (route handlers) and background workers key off
        // the class hierarchy, not decorators.
        assert_eq!(
            match_role_tag(RUBY_RULES, None, Some("ApplicationController"), None),
            Some(RoleTag::RouteHandler)
        );
        assert_eq!(
            match_role_tag(RUBY_RULES, None, Some("ActionController::Base"), None),
            Some(RoleTag::RouteHandler)
        );
        assert_eq!(
            match_role_tag(RUBY_RULES, None, Some("Sidekiq::Job"), None),
            Some(RoleTag::BackgroundJob)
        );
        assert_eq!(
            match_role_tag(RUBY_RULES, None, Some("ApplicationJob"), None),
            Some(RoleTag::BackgroundJob)
        );
        assert_eq!(
            match_role_tag(RUBY_RULES, None, Some("ApplicationRecord"), None),
            None,
            "an ActiveRecord model is not an entrypoint"
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
