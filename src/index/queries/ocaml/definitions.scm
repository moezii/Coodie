(let_binding pattern: (value_name) @name) @definition.function

(class_definition
  (class_binding
    name: (class_name) @scope.name)) @scope.class

; `module_binding` lost its `name` field in tree-sitter-ocaml 0.24; the module
; name is a plain child now.
(module_definition
  (module_binding
    (module_name) @scope.name)) @scope.class
