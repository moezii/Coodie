; tree-sitter-r 1.x replaced the six assignment node kinds with a single
; `binary_operator` carrying lhs/operator/rhs, so a function definition is
; recognised by an identifier bound to a `function_definition`.
(binary_operator
  lhs: (identifier) @name
  rhs: (function_definition)) @definition.function

(binary_operator
  lhs: (function_definition)
  rhs: (identifier) @name) @definition.function
