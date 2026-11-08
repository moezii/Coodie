(import_statement source: (string) @import.module)
(import_statement
  (import_clause (identifier) @import.symbol)
  source: (string) @import.module)
(import_statement
  (import_clause
    (named_imports
      (import_specifier name: (identifier) @import.symbol)))
  source: (string) @import.module)
(import_statement
  (import_clause
    (named_imports
      (import_specifier
        name: (identifier) @import.symbol
        alias: (identifier) @import.alias)))
  source: (string) @import.module)
