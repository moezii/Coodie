; The callee is the first child of the call, not any identifier inside it.
; A method call reaches the name through navigation_suffix.
(call_expression . (simple_identifier) @call.callee) @call
(call_expression
  . (navigation_expression
      suffix: (navigation_suffix suffix: (simple_identifier) @call.callee))) @call
