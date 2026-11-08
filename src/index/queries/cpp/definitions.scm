(function_definition
  declarator: (function_declarator
    declarator: (_) @name)) @definition.function
(function_definition
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (_) @name))) @definition.function
(function_definition
  declarator: (reference_declarator
    (function_declarator
      declarator: (_) @name))) @definition.function
(class_specifier name: (type_identifier) @scope.name) @scope.class
(namespace_definition name: (namespace_identifier) @scope.name) @scope.class
(struct_specifier name: (type_identifier) @scope.name) @scope.class
(union_specifier name: (type_identifier) @scope.name) @scope.class
