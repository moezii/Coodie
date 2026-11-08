; Attribute routes: [HttpGet("/users")] public IActionResult List() {}
(method_declaration
  (attribute_list
    (attribute
      name: (identifier) @reg.method
      (attribute_argument_list) @reg.args))
  name: (identifier) @reg.handler) @reg.site

; Attribute without arguments: [HttpGet] public IActionResult List() {}
(method_declaration
  (attribute_list
    (attribute
      name: (identifier) @reg.method))
  name: (identifier) @reg.handler) @reg.site

; Minimal API: app.MapGet("/users", Handler)
(invocation_expression
  function: (member_access_expression
    expression: (_) @reg.receiver
    name: (identifier) @reg.method)
  arguments: (argument_list) @reg.args) @reg.site
