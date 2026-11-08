; Attribute-macro routes: #[get("/users")] fn list_users() {}   (actix, rocket)
(
  (attribute_item
    (attribute
      (identifier) @reg.method
      arguments: (token_tree) @reg.args))
  .
  (function_item
    name: (identifier) @reg.handler) @reg.site
)

; Builder routes: .route("/users", get(list_users))   (axum)
(call_expression
  function: (field_expression
    value: (_) @reg.receiver
    field: (field_identifier) @reg.method)
  arguments: (arguments) @reg.args) @reg.site

; Associated-function registration: Router::new().route(...) and friends
(call_expression
  function: (scoped_identifier
    path: (_) @reg.receiver
    name: (identifier) @reg.method)
  arguments: (arguments) @reg.args) @reg.site
