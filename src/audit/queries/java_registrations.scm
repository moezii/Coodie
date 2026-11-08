; Annotated methods with arguments: @GetMapping("/x") public User get() {}
(method_declaration
  (modifiers
    (annotation
      name: (identifier) @reg.method
      arguments: (annotation_argument_list) @reg.args))
  name: (identifier) @reg.handler) @reg.site

; Marker annotations without arguments: @Scheduled void run() {}
(method_declaration
  (modifiers
    (marker_annotation
      name: (identifier) @reg.method))
  name: (identifier) @reg.handler) @reg.site
