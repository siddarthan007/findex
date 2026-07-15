(class_declaration name: (identifier) @class.name) @class.def
(interface_declaration name: (identifier) @interface.name) @interface.def
(struct_declaration name: (identifier) @struct.name) @struct.def
(record_declaration name: (identifier) @record.name) @record.def
(enum_declaration name: (identifier) @enum.name) @enum.def
(namespace_declaration name: (_) @namespace.name) @namespace.def
(method_declaration name: (identifier) @method.name) @method.def
(constructor_declaration name: (identifier) @constructor.name) @constructor.def
(property_declaration name: (identifier) @property.name) @property.def
(class_declaration (base_list (_) @inherit.name))
(interface_declaration (base_list (_) @inherit.name))
(struct_declaration (base_list (_) @inherit.name))
(record_declaration (base_list (_) @inherit.name))
(invocation_expression function: (identifier) @call.name)
(invocation_expression function: (member_access_expression name: (identifier) @call.name))
