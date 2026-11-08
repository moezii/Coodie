; The callee is the first child of the call, not any identifier inside it —
; without the anchor the arguments match too.
(call_expression . (identifier) @call.callee) @call
(call_expression . (navigation_expression (identifier) @call.callee)) @call
