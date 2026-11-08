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

; Decorated class methods: NestJS, Angular, TypeORM.
; The decorator is a sibling of the method rather than a child, so the pattern
; anchors on adjacency. JavaScript's grammar shapes this differently, which is
; why TypeScript has its own registration file.
(class_body
  (decorator
    (call_expression
      function: (identifier) @reg.method
      arguments: (arguments) @reg.args))
  .
  (method_definition
    name: (property_identifier) @reg.handler) @reg.site)

; A decorator with no arguments.
(class_body
  (decorator
    (identifier) @reg.method)
  .
  (method_definition
    name: (property_identifier) @reg.handler) @reg.site)
