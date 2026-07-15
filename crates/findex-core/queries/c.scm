((function_definition
  declarator: (_ declarator: (identifier) @func.name)) @func.def)

(struct_specifier name: (type_identifier) @struct.name) @struct.def

(enum_specifier name: (type_identifier) @enum.name) @enum.def

(union_specifier name: (type_identifier) @union.name) @union.def

(type_definition declarator: (type_identifier) @alias.name) @alias.def

(call_expression function: (identifier) @call.name)

(call_expression function: (field_expression field: (field_identifier) @call.name))
