(class_declaration declaration_kind: "class" name: (type_identifier) @class.name) @class.def
(class_declaration declaration_kind: "actor" name: (type_identifier) @class.name) @class.def
(protocol_declaration name: (type_identifier) @protocol.name) @protocol.def
(class_declaration declaration_kind: "struct" name: (type_identifier) @struct.name) @struct.def
(class_declaration declaration_kind: "enum" name: (type_identifier) @enum.name) @enum.def
(class_declaration declaration_kind: "extension" name: (_) @extension.name) @extension.def
(typealias_declaration name: (type_identifier) @alias.name) @alias.def
(function_declaration name: (simple_identifier) @func.name) @func.def
(init_declaration "init" @constructor.name) @constructor.def
(deinit_declaration "deinit" @method.name) @method.def
(class_declaration (inheritance_specifier inherits_from: (_) @inherit.name))
(protocol_declaration (inheritance_specifier inherits_from: (_) @inherit.name))
(call_expression (simple_identifier) @call.name)
