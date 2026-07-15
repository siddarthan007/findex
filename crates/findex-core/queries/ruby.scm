(method name: (_) @method.name) @method.def
(singleton_method name: (_) @method.name) @method.def
(alias name: (_) @method.name) @method.def
(class name: [(constant) @class.name (scope_resolution name: (_) @class.name)]) @class.def
(singleton_class value: [(constant) @class.name (scope_resolution name: (_) @class.name)]) @class.def
(module name: [(constant) @module.name (scope_resolution name: (_) @module.name)]) @module.def
(class superclass: (superclass [(constant) @inherit.name (scope_resolution name: (_) @inherit.name)]))
(call method: (identifier) @call.name)
