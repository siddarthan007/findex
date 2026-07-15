(function_signature
  name: (identifier) @func.name) @func.def

(class_definition
  name: (identifier) @class.name) @class.def

(mixin_declaration
  . (identifier) @mixin.name) @mixin.def

(enum_declaration
  name: (identifier) @enum.name) @enum.def

(extension_declaration
  name: (identifier) @extension.name) @extension.def

(type_alias
  . (type_identifier) @alias.name) @alias.def

(constructor_signature
  name: (identifier) @constructor.name) @constructor.def

(class_definition superclass: (superclass (_) @inherit.name))
(class_definition interfaces: (interfaces (_) @inherit.name))
(class_definition (mixin_application_class (_) @inherit.name))
(mixin_declaration (interfaces (_) @inherit.name))

(postfix_expression
  (_) @call.name
  (selector))
