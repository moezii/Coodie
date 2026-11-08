//! Edge cases, per language.
//!
//! Two failures matter and they are not symmetric. A **missed entrypoint** is
//! silent and poisons everything downstream, because reachability is measured
//! from the ways in. A **false entrypoint** is visible and merely noisy.
//!
//! So each language gets both: shapes that must be found, and lookalikes that
//! must not be.

use codereader::audit::entrypoints::extract_entrypoints;
use codereader::audit::{EntryPointKind, Exposure};
use codereader::index::SourceLanguage;
use std::path::PathBuf;

fn routes(language_id: &str, file: &str, source: &str) -> Vec<String> {
    let language = SourceLanguage::new(language_id).expect("known language");
    extract_entrypoints(&language, &PathBuf::from(file), source.as_bytes())
        .into_iter()
        .filter_map(|entrypoint| entrypoint.route)
        .collect()
}

fn entrypoint_kinds(language_id: &str, file: &str, source: &str) -> Vec<EntryPointKind> {
    let language = SourceLanguage::new(language_id).expect("known language");
    extract_entrypoints(&language, &PathBuf::from(file), source.as_bytes())
        .into_iter()
        .map(|entrypoint| entrypoint.kind)
        .collect()
}

// ---------------------------------------------------------------------------
// Shapes that must be found
// ---------------------------------------------------------------------------

#[test]
fn express_router_chained_off_a_call() {
    let found = routes(
        "javascript",
        "s.js",
        "express.Router().get('/health', check);\n",
    );
    assert!(found.contains(&"/health".to_owned()), "{found:?}");
}

#[test]
fn a_route_with_path_parameters_keeps_them() {
    let found = routes(
        "python",
        "api.py",
        "@app.get(\"/users/{user_id}/orders/{order_id}\")\ndef fetch(user_id, order_id):\n    return []\n",
    );
    assert!(
        found.contains(&"/users/{user_id}/orders/{order_id}".to_owned()),
        "path parameters must survive verbatim: {found:?}"
    );
}

#[test]
fn a_root_route_is_still_a_route() {
    let found = routes("javascript", "s.js", "app.get('/', home);\n");
    assert!(found.contains(&"/".to_owned()), "{found:?}");
}

#[test]
fn single_quotes_double_quotes_and_backticks_all_parse() {
    for quoted in ["'/a'", "\"/b\"", "`/c`"] {
        let source = format!("app.get({quoted}, handler);\n");
        let found = routes("javascript", "s.js", &source);
        assert_eq!(found.len(), 1, "{quoted} did not parse: {found:?}");
    }
}

#[test]
fn an_async_arrow_handler_is_still_a_route() {
    let found = routes(
        "javascript",
        "s.js",
        "app.post('/pay', async (req, res) => { await charge(); });\n",
    );
    assert!(found.contains(&"/pay".to_owned()), "{found:?}");
}

#[test]
fn every_language_with_a_binary_reports_its_main() {
    let cases = [
        ("rust", "m.rs", "fn main() {\n    run();\n}\n"),
        ("go", "m.go", "package main\n\nfunc main() {\n}\n"),
        ("c", "m.c", "int main(void) {\n  return 0;\n}\n"),
        ("cpp", "m.cpp", "int main() {\n  return 0;\n}\n"),
        ("python", "m.py", "if __name__ == \"__main__\":\n    run()\n"),
    ];
    for (language, file, source) in cases {
        let kinds = entrypoint_kinds(language, file, source);
        assert!(
            kinds.contains(&EntryPointKind::Cli),
            "{language} has no way in: {kinds:?}"
        );
    }
}

#[test]
fn a_language_without_a_registration_file_still_finds_main() {
    // C and C++ have no framework conventions here. Returning nothing left
    // them with no roots at all, so every symbol read as unreachable.
    let language = SourceLanguage::new("cpp").expect("cpp");
    let found = extract_entrypoints(
        &language,
        &PathBuf::from("m.cpp"),
        b"int main() { return work(); }\n",
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].exposure, Exposure::External);
}

// ---------------------------------------------------------------------------
// Lookalikes that must not be found
// ---------------------------------------------------------------------------

#[test]
fn a_map_lookup_that_happens_to_hold_a_path_is_not_a_route() {
    // The discriminator is the handler: registering a route passes one, reading
    // a map does not.
    let found = routes(
        "javascript",
        "s.js",
        "const cache = new Map();\ncache.get('/users');\nsessions.delete('/tokens/abc');\n",
    );
    assert!(found.is_empty(), "map access read as routes: {found:?}");
}

#[test]
fn a_config_getter_is_not_a_route() {
    let found = routes("javascript", "s.js", "app.get('trust proxy');\n");
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_route_inside_a_comment_or_string_is_not_a_route() {
    // tree-sitter parses grammar, so neither is a call at all.
    let found = routes(
        "javascript",
        "s.js",
        "// app.get('/commented', h);\nconst doc = \"app.get('/quoted', h)\";\n",
    );
    assert!(found.is_empty(), "text was read as code: {found:?}");
}

#[test]
fn a_bare_call_with_no_route_is_not_an_entrypoint() {
    let kinds = entrypoint_kinds("javascript", "s.js", "run();\nlisten(3000);\nstart();\n");
    assert!(
        kinds.is_empty(),
        "bare calls carry no evidence of being a way in: {kinds:?}"
    );
}

#[test]
fn a_function_merely_named_main_inside_a_class_is_not_a_binary_entry() {
    // Only a top-level main starts a process.
    let kinds = entrypoint_kinds(
        "rust",
        "lib.rs",
        "struct App;\nimpl App {\n    fn main(&self) {}\n}\n",
    );
    assert!(kinds.is_empty(), "{kinds:?}");
}

#[test]
fn an_empty_file_and_a_broken_file_do_not_panic() {
    // Real repositories contain both. tree-sitter recovers from errors, and the
    // audit must not fall over on either.
    for language in ["javascript", "python", "rust", "go", "java"] {
        let empty = entrypoint_kinds(language, "a.x", "");
        assert!(empty.is_empty());
        let broken = entrypoint_kinds(language, "a.x", "fn ( { unclosed @@@ ###");
        let _ = broken;
    }
}

#[test]
fn unicode_and_very_long_routes_are_handled() {
    let long_route = format!("/{}", "segment/".repeat(60));
    let source = format!("app.get('{long_route}', handler);\napp.get('/café/naïve', h2);\n");
    let found = routes("javascript", "s.js", &source);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().any(|route| route.contains("café")), "{found:?}");
}

// ---------------------------------------------------------------------------
// Serving a route vs calling one
// ---------------------------------------------------------------------------
//
// A frontend calls the same paths its backend serves, with the same syntax:
// `api.get("/auth/profile")` next to `app.get("/auth/profile", handler)`. Read
// the first as an entrypoint and a React app appears to expose 73 endpoints it
// does not have, and every one of them is reported as untested.

#[test]
fn an_outbound_client_request_is_not_an_entrypoint() {
    let found = routes(
        "javascript",
        "frontend/api.js",
        "const profile = await api.get('/auth/profile');\n\
         const cars = await axios.get('/cars/public/available');\n\
         const r = await client.post('/bookings/validate', { headers });\n\
         return http.get('/analytics/fleet');\n",
    );
    assert!(
        found.is_empty(),
        "calling an endpoint was read as serving one: {found:?}"
    );
}

#[test]
fn a_promise_chain_on_a_request_is_not_an_entrypoint() {
    let found = routes(
        "javascript",
        "frontend/api.js",
        "api.get('/users').then(setUsers);\n",
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn an_unfamiliar_receiver_with_a_real_handler_still_registers() {
    // The relaxation that matters: the app object can be called anything.
    let found = routes(
        "javascript",
        "server.js",
        "myBackend.get('/users', (req, res) => res.json(users));\n",
    );
    assert!(
        found.contains(&"/users".to_owned()),
        "an unfamiliar app object must still register: {found:?}"
    );
}

#[test]
fn an_unfamiliar_receiver_with_a_named_handler_still_registers() {
    let found = routes(
        "javascript",
        "server.js",
        "myBackend.get('/users', listUsers);\n",
    );
    assert!(found.contains(&"/users".to_owned()), "{found:?}");
}

#[test]
fn a_known_receiver_is_unaffected_by_the_handler_rule() {
    let found = routes("javascript", "server.js", "app.get('/health', check);\n");
    assert!(found.contains(&"/health".to_owned()), "{found:?}");
}

#[test]
fn kotlin_and_swift_report_their_main() {
    // Android and iOS modules otherwise have no way in at all, which makes
    // every symbol in them read as unreachable.
    for (language, file, source) in [
        ("kotlin", "Main.kt", "fun main() {\n    run()\n}\n"),
        ("swift", "main.swift", "func main() {\n    run()\n}\n"),
    ] {
        let kinds = entrypoint_kinds(language, file, source);
        assert!(
            kinds.contains(&EntryPointKind::Cli),
            "{language} has no way in: {kinds:?}"
        );
    }
}
