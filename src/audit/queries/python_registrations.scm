; Decorated definitions: @app.get("/x") over def handler(): ...
(decorated_definition
  (decorator) @reg.decorator
  definition: (function_definition
    name: (identifier) @reg.handler)) @reg.site

; Method-call registrations: app.add_route("/x", handler), scheduler.add_job(...)
(call
  function: (attribute
    object: (_) @reg.receiver
    attribute: (identifier) @reg.method)
  arguments: (argument_list) @reg.args) @reg.site
