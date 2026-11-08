(invocation_expression function: (_) @call.callee) @call

; `new BookingService()` is a use of BookingService.
(object_creation_expression type: (_) @call.callee) @call
