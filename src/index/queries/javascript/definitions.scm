(function_declaration name: (identifier) @name) @definition.function
(generator_function_declaration name: (identifier) @name) @definition.function
(method_definition name: (_) @name) @definition.method
(variable_declarator
  name: (identifier) @name
  value: [
    (arrow_function)
    (function_expression)
    (generator_function)
  ]) @definition.function
(assignment_expression
  left: (_) @name
  right: [
    (arrow_function)
    (function_expression)
    (generator_function)
  ]) @definition.function
(pair
  key: (_) @name
  value: [
    (arrow_function)
    (function_expression)
    (generator_function)
  ]) @definition.function
(class_declaration name: (identifier) @scope.name) @scope.class
(class name: (identifier) @scope.name) @scope.class
