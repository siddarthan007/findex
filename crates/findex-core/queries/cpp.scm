((function_definition
  declarator: (_ declarator: (identifier) @func.name)) @func.def)

((function_definition
  declarator: (_ declarator: (field_identifier) @func.name)) @func.def)

(class_specifier name: (type_identifier) @class.name) @class.def

(struct_specifier name: (type_identifier) @struct.name) @struct.def

(enum_specifier name: (type_identifier) @enum.name) @enum.def

(union_specifier name: (type_identifier) @union.name) @union.def

(alias_declaration name: (type_identifier) @alias.name) @alias.def

(namespace_definition name: (namespace_identifier) @namespace.name) @namespace.def

[(class_specifier) (struct_specifier) (union_specifier)]
  (base_class_clause [(type_identifier) (qualified_identifier) (template_type)] @inherit.name)

(call_expression function: (identifier) @call.name)

(call_expression function: (field_expression field: (field_identifier) @call.name))

(call_expression function: (qualified_identifier name: (identifier) @call.name))
