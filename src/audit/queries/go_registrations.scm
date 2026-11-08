; Method-call registrations: http.HandleFunc("/x", handler), router.GET("/x", handler)
(call_expression
  function: (selector_expression
    operand: (_) @reg.receiver
    field: (field_identifier) @reg.method)
  arguments: (argument_list) @reg.args) @reg.site
