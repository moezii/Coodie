; Method-call registrations: app.get("/x", handler), router.post(...), emitter.on(...)
(call_expression
  function: (member_expression
    object: (_) @reg.receiver
    property: (property_identifier) @reg.method)
  arguments: (arguments) @reg.args) @reg.site

; Bare-call registrations: get("/x", handler) after a destructured import
(call_expression
  function: (identifier) @reg.method
  arguments: (arguments) @reg.args) @reg.site
