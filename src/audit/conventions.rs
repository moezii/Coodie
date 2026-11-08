//! Framework conventions, expressed as data.
//!
//! A convention says: "when this receiver calls this method (or this decorator
//! sits above a function), that is an entrypoint of this kind." Detection alone
//! never creates an entrypoint — the call site still has to supply a real source
//! range, and HTTP routes still have to look like routes.
//!
//! Adding a framework should mean adding rows here, not writing traversal code.

use super::EntryPointKind;

/// A method-call registration such as `app.get("/users", handler)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallConvention {
    /// Method name on the receiver (`get`, `HandleFunc`, `add_job`).
    pub method: &'static str,
    pub kind: EntryPointKind,
    pub http_method: Option<&'static str>,
    /// When true the first string argument must look like a route (`/...`).
    /// This is what stops Express's `app.get("trust proxy")` config getter from
    /// being mistaken for a GET route.
    pub requires_route: bool,
    pub framework: &'static str,
}

/// A decorator/annotation registration such as `@app.get("/x")` or `@GetMapping`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoratorConvention {
    /// Decorator name, matched against the trailing segment (`app.get` -> `get`)
    /// or the whole dotted path when `qualified` is set.
    pub name: &'static str,
    pub kind: EntryPointKind,
    pub http_method: Option<&'static str>,
    pub framework: &'static str,
}

/// Receiver names that may register HTTP-ish routes. An allowlist keeps
/// `someMap.get(key)` from becoming an endpoint.
pub const HTTP_RECEIVERS: &[&str] = &[
    "app", "api", "router", "server", "fastify", "koa", "hono", "elysia", "srv",
    "mux", "http", "r", "e", "engine", "group", "subrouter", "bp", "blueprint",
    "routes", "route", "web", "handler", "svc",
];

/// Receivers that may register event subscriptions/publications.
pub const EVENT_RECEIVERS: &[&str] = &[
    "emitter", "events", "bus", "eventBus", "event_bus", "queue", "socket", "io",
    "ws", "channel", "consumer", "subscriber", "pubsub", "broker", "stream",
];

/// Receivers that may register CLI commands.
pub const CLI_RECEIVERS: &[&str] = &["program", "cli", "yargs", "cmd", "command", "app"];

/// Receivers that may register scheduled work.
pub const SCHEDULE_RECEIVERS: &[&str] =
    &["scheduler", "cron", "schedule", "agenda", "celery", "beat", "jobs"];

/// Receivers whose `listen`/`run`/`serve` call starts the application.
pub const STARTUP_RECEIVERS: &[&str] = &[
    "app", "server", "fastify", "http", "srv", "uvicorn", "hypercorn", "engine",
    "api", "cli", "program",
    // Single-page application roots.
    "root", "createroot", "reactdom", "hydrateroot", "vue", "createapp",
];

pub const CALL_CONVENTIONS: &[CallConvention] = &[
    // ---- HTTP verbs shared by Express/Fastify/Koa/Hono/Gin/Flask-style APIs ----
    http("get", Some("GET")),
    http("post", Some("POST")),
    http("put", Some("PUT")),
    http("patch", Some("PATCH")),
    http("delete", Some("DELETE")),
    http("head", Some("HEAD")),
    http("options", Some("OPTIONS")),
    http("all", None),
    // Go's net/http and gorilla/mux use capitalised names.
    http("GET", Some("GET")),
    http("POST", Some("POST")),
    http("PUT", Some("PUT")),
    http("PATCH", Some("PATCH")),
    http("DELETE", Some("DELETE")),
    http("HEAD", Some("HEAD")),
    http("OPTIONS", Some("OPTIONS")),
    http("HandleFunc", None),
    http("Handle", None),
    // ASP.NET minimal APIs.
    http("MapGet", Some("GET")),
    http("MapPost", Some("POST")),
    http("MapPut", Some("PUT")),
    http("MapPatch", Some("PATCH")),
    http("MapDelete", Some("DELETE")),
    http("MapControllers", None),
    // Explicit route registration helpers.
    http("route", None),
    http("add_route", None),
    http("add_api_route", None),
    http("add_url_rule", None),
    http("addRoute", None),
    // ---- WebSocket ----
    CallConvention {
        method: "ws",
        kind: EntryPointKind::WebSocket,
        http_method: None,
        requires_route: true,
        framework: "websocket",
    },
    CallConvention {
        method: "websocket",
        kind: EntryPointKind::WebSocket,
        http_method: None,
        requires_route: true,
        framework: "websocket",
    },
    // ---- Events ----
    event("on"),
    event("subscribe"),
    event("addListener"),
    event("add_listener"),
    event("consume"),
    event("listen_to"),
    // ---- Scheduled work ----
    scheduled("schedule"),
    scheduled("add_job"),
    scheduled("addJob"),
    scheduled("every"),
    scheduled("cron"),
    scheduled("scheduleJob"),
    // ---- CLI ----
    cli("command"),
    cli("addCommand"),
    cli("add_command"),
    // ---- Startup ----
    startup("listen"),
    startup("run"),
    startup("serve"),
    startup("start"),
    startup("ListenAndServe"),
    startup("Run"),
    // A single-page app boots through `createRoot(...).render(<App />)`. Without
    // this the root component has no caller and the whole component tree looks
    // unreachable.
    startup("render"),
    startup("mount"),
    startup("hydrate"),
    startup("hydrateRoot"),
];

pub const DECORATOR_CONVENTIONS: &[DecoratorConvention] = &[
    // FastAPI / Flask / Starlette / Litestar / Quart
    decorator_http("get", Some("GET")),
    decorator_http("post", Some("POST")),
    decorator_http("put", Some("PUT")),
    decorator_http("patch", Some("PATCH")),
    decorator_http("delete", Some("DELETE")),
    decorator_http("head", Some("HEAD")),
    decorator_http("options", Some("OPTIONS")),
    decorator_http("route", None),
    // Spring / JAX-RS annotations
    decorator_http("GetMapping", Some("GET")),
    decorator_http("PostMapping", Some("POST")),
    decorator_http("PutMapping", Some("PUT")),
    decorator_http("PatchMapping", Some("PATCH")),
    decorator_http("DeleteMapping", Some("DELETE")),
    decorator_http("RequestMapping", None),
    decorator_http("GET", Some("GET")),
    decorator_http("POST", Some("POST")),
    decorator_http("PUT", Some("PUT")),
    decorator_http("DELETE", Some("DELETE")),
    // ASP.NET attributes
    decorator_http("HttpGet", Some("GET")),
    decorator_http("HttpPost", Some("POST")),
    decorator_http("HttpPut", Some("PUT")),
    decorator_http("HttpPatch", Some("PATCH")),
    decorator_http("HttpDelete", Some("DELETE")),
    decorator_http("Route", None),
    // NestJS
    decorator_http("Get", Some("GET")),
    decorator_http("Post", Some("POST")),
    decorator_http("Put", Some("PUT")),
    decorator_http("Patch", Some("PATCH")),
    decorator_http("Delete", Some("DELETE")),
    // WebSocket
    DecoratorConvention {
        name: "websocket",
        kind: EntryPointKind::WebSocket,
        http_method: None,
        framework: "websocket",
    },
    // Background work: Celery, APScheduler, Spring, Prefect, Airflow
    decorator_kind("task", EntryPointKind::Scheduled, "celery"),
    decorator_kind("shared_task", EntryPointKind::Scheduled, "celery"),
    decorator_kind("scheduled_job", EntryPointKind::Scheduled, "apscheduler"),
    decorator_kind("Scheduled", EntryPointKind::Scheduled, "spring"),
    decorator_kind("flow", EntryPointKind::Scheduled, "prefect"),
    decorator_kind("dag", EntryPointKind::Scheduled, "airflow"),
    // Event listeners
    decorator_kind("EventListener", EntryPointKind::Event, "spring"),
    decorator_kind("KafkaListener", EntryPointKind::Event, "spring"),
    decorator_kind("RabbitListener", EntryPointKind::Event, "spring"),
    decorator_kind("OnEvent", EntryPointKind::Event, "nestjs"),
    decorator_kind("subscriber", EntryPointKind::Event, "generic"),
    // CLI: Click / Typer
    decorator_kind("command", EntryPointKind::Cli, "click"),
    decorator_kind("group", EntryPointKind::Cli, "click"),
    // GraphQL
    decorator_kind("Query", EntryPointKind::GraphQl, "graphql"),
    decorator_kind("Mutation", EntryPointKind::GraphQl, "graphql"),
    decorator_kind("Subscription", EntryPointKind::GraphQl, "graphql"),
    decorator_kind("strawberry_field", EntryPointKind::GraphQl, "strawberry"),
];

const fn http(method: &'static str, http_method: Option<&'static str>) -> CallConvention {
    CallConvention {
        method,
        kind: EntryPointKind::Http,
        http_method,
        requires_route: true,
        framework: "http",
    }
}

const fn event(method: &'static str) -> CallConvention {
    CallConvention {
        method,
        kind: EntryPointKind::Event,
        http_method: None,
        requires_route: false,
        framework: "event",
    }
}

const fn scheduled(method: &'static str) -> CallConvention {
    CallConvention {
        method,
        kind: EntryPointKind::Scheduled,
        http_method: None,
        requires_route: false,
        framework: "scheduler",
    }
}

const fn cli(method: &'static str) -> CallConvention {
    CallConvention {
        method,
        kind: EntryPointKind::Cli,
        http_method: None,
        requires_route: false,
        framework: "cli",
    }
}

const fn startup(method: &'static str) -> CallConvention {
    CallConvention {
        method,
        kind: EntryPointKind::Startup,
        http_method: None,
        requires_route: false,
        framework: "startup",
    }
}

const fn decorator_http(
    name: &'static str,
    http_method: Option<&'static str>,
) -> DecoratorConvention {
    DecoratorConvention {
        name,
        kind: EntryPointKind::Http,
        http_method,
        framework: "http",
    }
}

const fn decorator_kind(
    name: &'static str,
    kind: EntryPointKind,
    framework: &'static str,
) -> DecoratorConvention {
    DecoratorConvention {
        name,
        kind,
        http_method: None,
        framework,
    }
}

/// Receivers allowed for a given entrypoint kind. An empty slice means the kind
/// does not constrain its receiver.
pub fn receivers_for(kind: EntryPointKind) -> &'static [&'static str] {
    match kind {
        EntryPointKind::Http | EntryPointKind::WebSocket => HTTP_RECEIVERS,
        EntryPointKind::Event => EVENT_RECEIVERS,
        EntryPointKind::Cli => CLI_RECEIVERS,
        EntryPointKind::Scheduled => SCHEDULE_RECEIVERS,
        EntryPointKind::Startup => STARTUP_RECEIVERS,
        EntryPointKind::GraphQl | EntryPointKind::Application => &[],
    }
}

/// Whether a receiver expression names something in the allowlist.
///
/// Receivers are rarely bare identifiers in real code: `Router::new()`,
/// `self.app`, and `$this->router` all need to resolve to `router`/`app`. So
/// every path segment is checked, case-insensitively, with call parentheses
/// and sigils stripped.
pub fn receiver_matches(receiver: &str, allowed: &[&str]) -> bool {
    receiver
        .split(['.', ':', '-', '>', '(', ')', '[', ']'])
        .filter(|segment| !segment.is_empty())
        .any(|segment| {
            let cleaned = segment.trim().trim_start_matches('$').to_ascii_lowercase();
            allowed.contains(&cleaned.as_str())
        })
}

pub fn call_convention(method: &str) -> Option<&'static CallConvention> {
    CALL_CONVENTIONS
        .iter()
        .find(|convention| convention.method == method)
}

pub fn decorator_convention(name: &str) -> Option<&'static DecoratorConvention> {
    DECORATOR_CONVENTIONS
        .iter()
        .find(|convention| convention.name == name)
}

/// A route argument has to look like a route. This is the precision filter that
/// separates `app.get("/users")` from `app.get("trust proxy")`.
pub fn looks_like_route(value: &str) -> bool {
    value == "*" || value.starts_with('/') || value.starts_with("./") || value.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn express_config_getter_is_not_a_route() {
        assert!(!looks_like_route("trust proxy"));
        assert!(looks_like_route("/users"));
        assert!(looks_like_route("/users/:id"));
        assert!(looks_like_route("*"));
    }

    #[test]
    fn http_verbs_resolve_to_http_conventions() {
        let convention = call_convention("post").expect("post is a convention");
        assert_eq!(convention.kind, EntryPointKind::Http);
        assert_eq!(convention.http_method, Some("POST"));
        assert!(convention.requires_route);
    }

    #[test]
    fn listen_is_startup_not_http() {
        let convention = call_convention("listen").expect("listen is a convention");
        assert_eq!(convention.kind, EntryPointKind::Startup);
        assert!(!convention.requires_route);
    }

    #[test]
    fn spring_and_fastapi_decorators_both_resolve() {
        assert_eq!(
            decorator_convention("GetMapping").map(|c| c.http_method),
            Some(Some("GET"))
        );
        assert_eq!(
            decorator_convention("post").map(|c| c.kind),
            Some(EntryPointKind::Http)
        );
    }
}
