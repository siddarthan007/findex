(function_item
  name: (identifier) @func.name) @func.def

(struct_item
  name: (type_identifier) @struct.name) @struct.def

(enum_item
  name: (type_identifier) @enum.name) @enum.def

(trait_item
  name: (type_identifier) @trait.name) @trait.def

(impl_item
  type: (type_identifier) @impl.name) @impl.def

(union_item
  name: (type_identifier) @union.name) @union.def

(type_item
  name: (type_identifier) @alias.name) @alias.def

(mod_item
  name: (identifier) @module.name) @module.def

(function_signature_item
  name: (identifier) @method.name) @method.def

(associated_type
  name: (type_identifier) @alias.name) @alias.def

(enum_variant
  name: (identifier) @variant.name) @variant.def

(const_item
  name: (identifier) @constant.name) @constant.def

(static_item
  name: (identifier) @constant.name) @constant.def

(impl_item
  trait: [(type_identifier) (scoped_type_identifier) (generic_type)] @inherit.name)

(trait_item
  (trait_bounds (type_identifier) @inherit.name))

(call_expression
  function: (identifier) @call.name)

(call_expression
  function: (field_expression field: (field_identifier) @call.name))

(call_expression
  function: (scoped_identifier name: (identifier) @call.name))
