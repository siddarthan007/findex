(class_declaration name: (identifier) @class.name) @class.def

(interface_declaration name: (identifier) @interface.name) @interface.def

(method_declaration name: (identifier) @method.name) @method.def

(constructor_declaration name: (identifier) @constructor.name) @constructor.def

(compact_constructor_declaration name: (identifier) @constructor.name) @constructor.def

(enum_declaration name: (identifier) @enum.name) @enum.def

(record_declaration name: (identifier) @record.name) @record.def

(annotation_type_declaration name: (identifier) @annotation.name) @annotation.def

(module_declaration name: [(identifier) (scoped_identifier)] @module.name) @module.def

(class_declaration (superclass (type_identifier) @inherit.name))
(class_declaration (super_interfaces (type_list (type_identifier) @inherit.name)))
(interface_declaration (extends_interfaces (type_list (type_identifier) @inherit.name)))
(record_declaration (super_interfaces (type_list (type_identifier) @inherit.name)))
(enum_declaration (super_interfaces (type_list (type_identifier) @inherit.name)))

(method_invocation name: (identifier) @call.name)
