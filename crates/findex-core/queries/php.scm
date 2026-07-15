(namespace_definition name: (namespace_name) @namespace.name) @namespace.def
(interface_declaration name: (name) @interface.name) @interface.def
(trait_declaration name: (name) @trait.name) @trait.def
(class_declaration name: (name) @class.name) @class.def
(enum_declaration name: (name) @enum.name) @enum.def
(property_declaration (property_element (variable_name (name) @property.name))) @property.def
(function_definition name: (name) @func.name) @func.def
(method_declaration name: (name) @method.name) @method.def
(class_declaration (base_clause (_) @inherit.name))
(class_interface_clause [(name) (qualified_name)] @inherit.name)
(function_call_expression function: [(qualified_name (name) @call.name) (variable_name (name) @call.name)])
(scoped_call_expression name: (name) @call.name)
(member_call_expression name: (name) @call.name)
