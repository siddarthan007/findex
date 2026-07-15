(function_definition
  name: (identifier) @func.name) @func.def

(class_definition
  name: (identifier) @class.name) @class.def

(type_alias_statement
  . (type) @alias.name) @alias.def

(class_definition
  superclasses: (argument_list (identifier) @inherit.name))

(class_definition
  superclasses: (argument_list (attribute attribute: (identifier) @inherit.name)))

(call
  function: (identifier) @call.name)

(call
  function: (attribute attribute: (identifier) @call.name))
