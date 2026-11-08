(function_definition name: (name) @name) @definition.function
(method_declaration name: (name) @name) @definition.method

(class_declaration name: (name) @scope.name) @scope.class
(interface_declaration name: (name) @scope.name) @scope.class
(trait_declaration name: (name) @scope.name) @scope.class
(namespace_definition name: (namespace_name) @scope.name) @scope.class
