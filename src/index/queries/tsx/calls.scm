(call_expression function: (_) @call.callee) @call

; `new ApiClient()` is a use of ApiClient. Without this every class
; instantiated with `new` reads as unreferenced.
(new_expression constructor: (_) @call.callee) @call
