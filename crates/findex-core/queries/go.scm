(function_declaration name: (identifier) @func.name) @func.def

(method_declaration name: (field_identifier) @func.name) @func.def

(type_declaration
  (type_spec name: (type_identifier) @type.name
    type: (struct_type))) @struct.def

(type_declaration
  (type_spec name: (type_identifier) @type.name
    type: (interface_type))) @interface.def

(type_declaration
  (type_alias name: (type_identifier) @alias.name)) @alias.def

(call_expression function: (identifier) @call.name)

(call_expression function: (selector_expression field: (field_identifier) @call.name))
