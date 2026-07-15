use oxc_allocator::{Allocator, Box as ArenaBox};
use oxc_ast::ast::*;
use oxc_ast::visit::walk;
use oxc_ast::Visit;
use oxc_parser::Parser;
use oxc_span::SourceType;
use oxc_syntax::scope::ScopeFlags;
use std::path::Path;

use crate::parser::ParserError;
use crate::storage::{Edge, EdgeType, Symbol};
use crate::token_budget::count_tokens;

struct LineIndex {
    line_starts: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, c) in text.char_indices() {
            if c == '\n' {
                line_starts.push(i + 1);
            }
        }
        Self { line_starts }
    }

    fn line_col(&self, offset: usize) -> (usize, usize) {
        let line = match self.line_starts.binary_search(&offset) {
            Ok(idx) => idx + 1,
            Err(idx) => idx,
        };
        let line_start = self.line_starts[line - 1];
        let col = offset - line_start + 1;
        (line, col)
    }
}

pub fn parse_js_ts(path: &Path, content: &str) -> Result<(Vec<Symbol>, Vec<Edge>), ParserError> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path)
        .map_err(|_| ParserError::Unsupported(path.to_string_lossy().to_string()))?;

    let parser_ret = Parser::new(&allocator, content, source_type).parse();
    if parser_ret.panicked {
        return Err(ParserError::Oxc("Oxc parser panicked".to_string()));
    }

    let line_index = LineIndex::new(content);
    let file_path_str = path.to_string_lossy().to_string();

    let language = if path
        .extension()
        .map(|e| e == "ts" || e == "tsx")
        .unwrap_or(false)
    {
        "typescript"
    } else {
        "javascript"
    }
    .to_string();

    let mut extractor = JsTsExtractor {
        file_path: file_path_str,
        source: content,
        line_index: &line_index,
        language,
        symbols: Vec::new(),
        edges: Vec::new(),
        scope_stack: Vec::new(),
    };

    extractor.visit_program(&parser_ret.program);

    Ok((extractor.symbols, extractor.edges))
}

struct JsTsExtractor<'a> {
    file_path: String,
    source: &'a str,
    line_index: &'a LineIndex,
    language: String,
    symbols: Vec<Symbol>,
    edges: Vec<Edge>,
    scope_stack: Vec<String>,
}

impl<'a> JsTsExtractor<'a> {
    fn add_symbol(
        &mut self,
        name: &str,
        kind: &str,
        signature: String,
        span: &oxc_span::Span,
    ) -> String {
        let (s_line, s_col) = self.line_index.line_col(span.start as usize);
        let (e_line, e_col) = self.line_index.line_col(span.end as usize);
        let id = format!("{}#{}:L{}C{}", self.file_path, name, s_line, s_col);
        let parent_id = self.scope_stack.last().cloned();
        let docstring = extract_leading_js_comment(self.source, span.start as usize);
        self.symbols.push(Symbol {
            id: id.clone(),
            name: name.to_string(),
            kind: kind.to_string(),
            signature: signature.clone(),
            file_path: self.file_path.clone(),
            start_line: s_line,
            start_col: s_col,
            end_line: e_line,
            end_col: e_col,
            docstring,
            parent_id: parent_id.clone(),
            qualified_name: parent_id
                .as_ref()
                .map(|parent| format!("{}::{}", parent, name)),
            language: self.language.clone(),
            token_count: count_tokens(&signature),
            ..Default::default()
        });

        if let Some(parent) = parent_id {
            if let Some(parent_symbol) = self.symbols.iter_mut().find(|symbol| symbol.id == parent)
            {
                parent_symbol.children.push(id.clone());
            }
            self.add_edge(&parent, &id, EdgeType::Contains);
        }

        id
    }

    fn add_edge(&mut self, src: &str, dst: &str, edge_type: EdgeType) {
        self.edges.push(Edge {
            src: src.to_string(),
            dst: dst.to_string(),
            edge_type,
            ..Default::default()
        });
    }

    fn current_scope_symbol(&self) -> String {
        self.scope_stack
            .last()
            .cloned()
            .unwrap_or_else(|| self.file_path.clone())
    }
}

impl<'a> Visit<'_> for JsTsExtractor<'a> {
    fn visit_function(&mut self, func: &Function<'_>, flags: Option<ScopeFlags>) {
        if let Some(id) = &func.id {
            let name = id.name.as_str();
            let is_component = is_likely_component(name) || function_body_contains_jsx(&func.body);
            let kind = if is_component {
                "Component"
            } else {
                "Function"
            };
            let symbol_id = self.add_symbol(name, kind, format!("function {}", name), &func.span);
            self.scope_stack.push(symbol_id);
            walk::walk_function(self, func, flags);
            self.scope_stack.pop();
            return;
        }
        walk::walk_function(self, func, flags);
    }

    fn visit_class(&mut self, class: &Class<'_>) {
        if let Some(id) = &class.id {
            let name = id.name.as_str();
            let symbol_id = self.add_symbol(name, "Class", format!("class {}", name), &class.span);
            if let Some(base) = class.super_class.as_ref().and_then(expression_name) {
                self.add_edge(&symbol_id, &base, EdgeType::Inherits);
            }
            if let Some(interfaces) = &class.implements {
                for interface in interfaces {
                    self.add_edge(
                        &symbol_id,
                        &type_name(&interface.expression),
                        EdgeType::Inherits,
                    );
                }
            }
            self.scope_stack.push(symbol_id);
            walk::walk_class(self, class);
            self.scope_stack.pop();
            return;
        }
        walk::walk_class(self, class);
    }

    fn visit_method_definition(&mut self, method: &MethodDefinition<'_>) {
        if let PropertyKey::StaticIdentifier(ident) = &method.key {
            let name = ident.name.as_str();
            let kind = match method.kind {
                MethodDefinitionKind::Constructor => "Constructor",
                MethodDefinitionKind::Method => "Method",
                MethodDefinitionKind::Get => "Getter",
                MethodDefinitionKind::Set => "Setter",
            };
            let symbol_id = self.add_symbol(
                name,
                kind,
                format!("{} {}", kind.to_lowercase(), name),
                &method.span,
            );
            self.scope_stack.push(symbol_id);
            walk::walk_method_definition(self, method);
            self.scope_stack.pop();
            return;
        }
        walk::walk_method_definition(self, method);
    }

    fn visit_ts_interface_declaration(&mut self, interface: &TSInterfaceDeclaration<'_>) {
        let name = interface.id.name.as_str();
        let symbol_id = self.add_symbol(
            name,
            "Interface",
            format!("interface {}", name),
            &interface.span,
        );
        if let Some(heritage) = &interface.extends {
            for base in heritage {
                if let Some(base_name) = expression_name(&base.expression) {
                    self.add_edge(&symbol_id, &base_name, EdgeType::Inherits);
                }
            }
        }
        self.scope_stack.push(symbol_id);
        walk::walk_ts_interface_declaration(self, interface);
        self.scope_stack.pop();
    }

    fn visit_ts_type_alias_declaration(&mut self, alias: &TSTypeAliasDeclaration<'_>) {
        let name = alias.id.name.as_str();
        self.add_symbol(name, "TypeAlias", format!("type {}", name), &alias.span);
        walk::walk_ts_type_alias_declaration(self, alias);
    }

    fn visit_variable_declaration(&mut self, decl: &VariableDeclaration<'_>) {
        for declarator in &decl.declarations {
            if let BindingPatternKind::BindingIdentifier(ident) = &declarator.id.kind {
                let name = ident.name.as_str();
                let is_component = is_likely_component(name)
                    && declarator
                        .init
                        .as_ref()
                        .is_some_and(|init| expr_contains_jsx(init));
                let function_value = declarator.init.as_ref().is_some_and(|init| {
                    matches!(
                        init,
                        Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_)
                    )
                });
                let kind = if is_component {
                    "Component"
                } else if function_value {
                    "Function"
                } else {
                    "Variable"
                };
                // Local variables are deliberately omitted: treating every `let`
                // as an architectural entity bloats the graph and lowers retrieval precision.
                if self.scope_stack.is_empty() || function_value || is_component {
                    self.add_symbol(name, kind, format!("const/let {}", name), &declarator.span);
                }
            }
        }
        walk::walk_variable_declaration(self, decl);
    }

    fn visit_property_definition(&mut self, property: &PropertyDefinition<'_>) {
        if let Some(name) = property.key.static_name() {
            self.add_symbol(
                name.as_str(),
                "Property",
                format!("property {name}"),
                &property.span,
            );
        }
        walk::walk_property_definition(self, property);
    }

    fn visit_ts_method_signature(&mut self, method: &TSMethodSignature<'_>) {
        if let Some(name) = method.key.static_name() {
            let kind = match method.kind {
                TSMethodSignatureKind::Method => "Method",
                TSMethodSignatureKind::Get => "Getter",
                TSMethodSignatureKind::Set => "Setter",
            };
            self.add_symbol(name.as_str(), kind, format!("{kind} {name}"), &method.span);
        }
        walk::walk_ts_method_signature(self, method);
    }

    fn visit_ts_property_signature(&mut self, property: &TSPropertySignature<'_>) {
        if let Some(name) = property.key.static_name() {
            self.add_symbol(
                name.as_str(),
                "Property",
                format!("property {name}"),
                &property.span,
            );
        }
        walk::walk_ts_property_signature(self, property);
    }

    fn visit_enum(&mut self, declaration: &TSEnumDeclaration<'_>) {
        let name = declaration.id.name.as_str();
        let symbol_id = self.add_symbol(name, "Enum", format!("enum {name}"), &declaration.span);
        self.scope_stack.push(symbol_id);
        walk::walk_enum(self, declaration);
        self.scope_stack.pop();
    }

    fn visit_ts_module_declaration(&mut self, declaration: &TSModuleDeclaration<'_>) {
        let name = declaration.id.name().as_str();
        let kind = match declaration.kind {
            TSModuleDeclarationKind::Namespace => "Namespace",
            _ => "Module",
        };
        let symbol_id = self.add_symbol(name, kind, format!("{kind} {name}"), &declaration.span);
        self.scope_stack.push(symbol_id);
        walk::walk_ts_module_declaration(self, declaration);
        self.scope_stack.pop();
    }

    fn visit_import_declaration(&mut self, import: &ImportDeclaration<'_>) {
        let source = import.source.value.as_str();
        let src_scope = self.current_scope_symbol();
        self.add_edge(&src_scope, source, EdgeType::Imports);
        walk::walk_import_declaration(self, import);
    }

    fn visit_jsx_element(&mut self, elem: &JSXElement<'_>) {
        if let JSXElementName::Identifier(ident) = &elem.opening_element.name {
            let name = ident.name.as_str();
            if is_component_name(name) {
                let src_scope = self.current_scope_symbol();
                self.add_edge(&src_scope, name, EdgeType::References);
            }
        }
        walk::walk_jsx_element(self, elem);
    }

    fn visit_call_expression(&mut self, expr: &CallExpression<'_>) {
        let callee_name = match &expr.callee {
            Expression::Identifier(ident) => Some(ident.name.as_str().to_string()),
            Expression::StaticMemberExpression(member) => {
                if let Expression::Identifier(obj) = &member.object {
                    Some(format!("{}.{}", obj.name, member.property.name))
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(name) = callee_name {
            let src_scope = self.current_scope_symbol();
            self.add_edge(&src_scope, &name, EdgeType::Calls);
        }
        walk::walk_call_expression(self, expr);
    }
}

fn expression_name(expression: &Expression<'_>) -> Option<String> {
    match expression {
        Expression::Identifier(identifier) => Some(identifier.name.to_string()),
        Expression::StaticMemberExpression(member) => expression_name(&member.object)
            .map(|object| format!("{object}.{}", member.property.name)),
        _ => None,
    }
}

fn type_name(name: &TSTypeName<'_>) -> String {
    match name {
        TSTypeName::IdentifierReference(identifier) => identifier.name.to_string(),
        TSTypeName::QualifiedName(qualified) => {
            format!("{}.{}", type_name(&qualified.left), qualified.right.name)
        }
    }
}

fn is_likely_component(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_uppercase())
}

fn is_component_name(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_uppercase()) && !name.contains('.')
}

fn function_body_contains_jsx(body: &Option<ArenaBox<'_, FunctionBody<'_>>>) -> bool {
    body.as_ref().is_some_and(|b| {
        let mut visitor = HasJsxVisitor(false);
        visitor.visit_function_body(b);
        visitor.0
    })
}

fn expr_contains_jsx(expr: &Expression<'_>) -> bool {
    let mut visitor = HasJsxVisitor(false);
    visitor.visit_expression(expr);
    visitor.0
}

fn extract_leading_js_comment(source: &str, start: usize) -> Option<String> {
    let prefix = source.get(..start)?.trim_end();

    if prefix.ends_with("*/") {
        if let Some(open) = prefix.rfind("/**") {
            let between = &prefix[open + 3..prefix.len().saturating_sub(2)];
            let cleaned = between
                .lines()
                .map(|line| line.trim().trim_start_matches('*').trim())
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }

    let mut lines = Vec::new();
    for line in prefix.lines().rev() {
        let line = line.trim();
        if let Some(comment) = line.strip_prefix("//") {
            lines.push(comment.trim().to_string());
        } else {
            break;
        }
    }
    lines.reverse();
    let joined = lines.join("\n");
    (!joined.is_empty()).then_some(joined)
}

struct HasJsxVisitor(bool);

impl<'a> Visit<'a> for HasJsxVisitor {
    fn visit_jsx_element(&mut self, _elem: &JSXElement<'a>) {
        self.0 = true;
    }
    fn visit_jsx_fragment(&mut self, _frag: &JSXFragment<'a>) {
        self.0 = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_js_ts_parsing() {
        let code = "
            import { useState } from 'react';
            export interface User {
                id: number;
                name: string;
            }
            export class UserService {
                getUser(id: number): User {
                    return { id, name: 'Alice' };
                }
            }
            function helper() {
                console.log('done');
            }
        ";

        let (syms, edges) = parse_js_ts(Path::new("test.ts"), code).unwrap();

        assert!(syms
            .iter()
            .any(|s| s.name == "User" && s.kind == "Interface"));
        assert!(syms
            .iter()
            .any(|s| s.name == "UserService" && s.kind == "Class"));
        assert!(syms
            .iter()
            .any(|s| s.name == "getUser" && s.kind == "Method"));
        assert!(syms
            .iter()
            .any(|s| s.name == "helper" && s.kind == "Function"));

        assert!(edges
            .iter()
            .any(|e| e.dst == "react" && matches!(e.edge_type, EdgeType::Imports)));
        assert!(edges
            .iter()
            .any(|e| e.dst == "console.log" && matches!(e.edge_type, EdgeType::Calls)));
    }

    #[test]
    fn test_react_component_parsing() {
        let code = r#"
            import React from 'react';
            export function Button({ label }) {
                return <button>{label}</button>;
            }
            export function App() {
                return <Button label="Click" />;
            }
        "#;

        let (syms, edges) = parse_js_ts(Path::new("app.jsx"), code).unwrap();

        assert!(syms
            .iter()
            .any(|s| s.name == "Button" && s.kind == "Component"));
        assert!(syms
            .iter()
            .any(|s| s.name == "App" && s.kind == "Component"));
        assert!(edges
            .iter()
            .any(|e| e.dst == "Button" && matches!(e.edge_type, EdgeType::References)));
    }

    #[test]
    fn test_jsdoc_scope_edges_and_unique_ids() {
        let code = r#"
            /** Fetch a user by id. */
            function load(id) { return fetch(id); }
            function load(name) { return fetch(name); }
        "#;
        let (syms, edges) = parse_js_ts(Path::new("api.js"), code).unwrap();
        let loads: Vec<_> = syms.iter().filter(|sym| sym.name == "load").collect();
        assert_eq!(loads.len(), 2);
        assert_ne!(loads[0].id, loads[1].id);
        assert_eq!(loads[0].docstring.as_deref(), Some("Fetch a user by id."));
        assert!(edges
            .iter()
            .any(|edge| edge.src == loads[0].id && edge.dst == "fetch"));
    }

    #[test]
    fn models_typescript_oop_and_module_contracts() {
        let code = r#"
            interface Store extends Disposable {
                readonly id: string;
                load(key: string): Promise<string>;
            }
            abstract class BaseStore implements Store {
                id = "base";
                abstract load(key: string): Promise<string>;
            }
            class CacheStore extends BaseStore {
                constructor() { super(); }
                get size() { return 1; }
                load = async (key: string) => key;
            }
            enum State { Ready, Busy }
            namespace Runtime { export type Mode = "fast" | "safe"; }
        "#;

        let (symbols, edges) = parse_js_ts(Path::new("contracts.ts"), code).unwrap();

        for (name, kind) in [
            ("Store", "Interface"),
            ("id", "Property"),
            ("load", "Method"),
            ("CacheStore", "Class"),
            ("size", "Getter"),
            ("State", "Enum"),
            ("Runtime", "Namespace"),
            ("Mode", "TypeAlias"),
        ] {
            assert!(
                symbols
                    .iter()
                    .any(|symbol| symbol.name == name && symbol.kind == kind),
                "missing {kind} {name}"
            );
        }
        assert!(edges
            .iter()
            .any(|edge| { edge.edge_type == EdgeType::Inherits && edge.dst == "BaseStore" }));
        assert!(edges
            .iter()
            .any(|edge| { edge.edge_type == EdgeType::Inherits && edge.dst == "Disposable" }));
    }
}
