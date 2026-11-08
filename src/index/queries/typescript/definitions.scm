(function_declaration name: (_) @name) @definition.function
(generator_function_declaration name: (_) @name) @definition.function
(method_definition name: (_) @name) @definition.method
(abstract_method_signature name: (_) @name) @definition.method
(method_signature name: (_) @name) @definition.method
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
(class_declaration name: (_) @scope.name) @scope.class
(abstract_class_declaration name: (_) @scope.name) @scope.class
(class name: (_) @scope.name) @scope.class
(internal_module name: (_) @scope.name) @scope.class
(interface_declaration name: (type_identifier) @scope.name) @scope.class
(type_alias_declaration name: (type_identifier) @scope.name) @scope.class
(enum_declaration name: (identifier) @scope.name) @scope.class
