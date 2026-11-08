(import_statement name: (dotted_name) @import.module)
(aliased_import name: (dotted_name) @import.module alias: (identifier) @import.alias)
(import_from_statement
  module_name: (dotted_name) @import.module
  name: (dotted_name) @import.symbol)
(import_from_statement
  module_name: (dotted_name) @import.module
  name: (aliased_import
    name: (dotted_name) @import.symbol
    alias: (identifier) @import.alias))
