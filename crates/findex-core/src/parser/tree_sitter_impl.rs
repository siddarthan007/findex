use crate::parser::registry::LanguageConfig;
use crate::parser::ParserError;
use crate::storage::{Edge, EdgeType, Symbol};
use crate::token_budget::count_tokens;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor};

thread_local! {
    /// Per-thread cache of tree-sitter parsers and compiled queries.
    ///
    /// Creating a `Query` is expensive (it compiles the query against the
    /// grammar), and creating a `Parser` per file is also unnecessary. This
    /// cache keeps one `(Parser, Query)` pair per language per thread, which
    /// significantly speeds up bulk ingestion.
    static PARSER_CACHE: RefCell<HashMap<&'static str, (Parser, Query)>> = RefCell::new(HashMap::new());
}

/// Parse source code using a registered tree-sitter language config.
pub fn parse_with_config(
    path: &Path,
    content: &str,
    config: &LanguageConfig,
) -> Result<(Vec<Symbol>, Vec<Edge>), ParserError> {
    parse_with_query(
        path,
        content,
        &config.language,
        config.query,
        config.name,
        config.kind_map,
    )
}

/// Convenience wrappers for the existing test suite and direct callers.
pub fn parse_rust(path: &Path, content: &str) -> Result<(Vec<Symbol>, Vec<Edge>), ParserError> {
    parse_with_config(
        path,
        content,
        crate::parser::registry::config_for_extension("rs").expect("rust is always registered"),
    )
}

pub fn parse_python(path: &Path, content: &str) -> Result<(Vec<Symbol>, Vec<Edge>), ParserError> {
    parse_with_config(
        path,
        content,
        crate::parser::registry::config_for_extension("py").expect("python is always registered"),
    )
}

pub fn parse_html(path: &Path, content: &str) -> Result<(Vec<Symbol>, Vec<Edge>), ParserError> {
    parse_with_config(
        path,
        content,
        crate::parser::registry::config_for_extension("html").expect("html is always registered"),
    )
}

pub fn parse_css(path: &Path, content: &str) -> Result<(Vec<Symbol>, Vec<Edge>), ParserError> {
    parse_with_config(
        path,
        content,
        crate::parser::registry::config_for_extension("css").expect("css is always registered"),
    )
}

pub fn parse_dart(path: &Path, content: &str) -> Result<(Vec<Symbol>, Vec<Edge>), ParserError> {
    parse_with_config(
        path,
        content,
        crate::parser::registry::config_for_extension("dart").expect("dart is always registered"),
    )
}

struct RawRef {
    name: String,
    line: usize,
    edge_type: EdgeType,
}

fn parse_with_query(
    path: &Path,
    content: &str,
    language: &Language,
    query_str: &str,
    lang_name: &'static str,
    kind_map: &[(&str, &str)],
) -> Result<(Vec<Symbol>, Vec<Edge>), ParserError> {
    PARSER_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let (parser, query) = cache.entry(lang_name).or_insert_with(|| {
            let mut parser = Parser::new();
            parser
                .set_language(language)
                .expect("language should be valid");
            let query = Query::new(language, query_str).expect("query should be valid");
            (parser, query)
        });

        let tree = parser
            .parse(content, None)
            .ok_or_else(|| ParserError::TreeSitter(format!("Failed to parse {}", lang_name)))?;

        let mut query_cursor = QueryCursor::new();
        let root_node = tree.root_node();
        let source_bytes = content.as_bytes();
        let mut matches = query_cursor.matches(query, root_node, source_bytes);

        let mut symbols = Vec::new();
        let mut raw_refs = Vec::new();
        let file_path_str = path.to_string_lossy().to_string();

        let capture_names = query.capture_names();

        while let Some(m) = matches.next() {
            let mut def_node: Option<Node> = None;
            let mut name: Option<String> = None;
            let mut kind: Option<String> = None;

            let mut ref_name: Option<String> = None;
            let mut ref_node: Option<Node> = None;
            let mut reference_type = EdgeType::References;

            for capture in m.captures {
                let cap_name = &capture_names[capture.index as usize];
                if cap_name.ends_with(".def") {
                    def_node = Some(capture.node);
                    let prefix = cap_name.split('.').next().unwrap_or("unknown");
                    kind = Some(map_kind(prefix, kind_map).to_string());
                } else if cap_name.ends_with(".name") {
                    if let Ok(text) = capture.node.utf8_text(source_bytes) {
                        if cap_name.starts_with("call") {
                            ref_name = Some(text.to_string());
                            ref_node = Some(capture.node);
                            reference_type = EdgeType::Calls;
                        } else if cap_name.starts_with("ref") {
                            ref_name = Some(text.to_string());
                            ref_node = Some(capture.node);
                            reference_type = EdgeType::References;
                        } else if cap_name.starts_with("inherit") {
                            ref_name = Some(text.to_string());
                            ref_node = Some(capture.node);
                            reference_type = EdgeType::Inherits;
                        } else if cap_name.starts_with("import") {
                            ref_name = Some(text.to_string());
                            ref_node = Some(capture.node);
                            reference_type = EdgeType::Imports;
                        } else {
                            name = Some(text.to_string());
                        }
                    }
                }
            }

            // Process Definition
            if let (Some(def), Some(n), Some(k)) = (def_node, name, kind) {
                let start = def.start_position();
                let end = def.end_position();

                let signature = if let Ok(sig_text) = def.utf8_text(source_bytes) {
                    sig_text.lines().next().unwrap_or("").to_string()
                } else {
                    n.clone()
                };

                let docstring = extract_docstring(def, source_bytes, lang_name);
                symbols.push(Symbol {
                    // Line and column make same-named nested/overloaded symbols
                    // collision-free while retaining a readable SCIP-style ID.
                    id: format!(
                        "{}#{}:L{}C{}",
                        file_path_str,
                        n,
                        start.row + 1,
                        start.column + 1
                    ),
                    name: n,
                    kind: k,
                    signature: signature.clone(),
                    file_path: file_path_str.clone(),
                    start_line: start.row + 1,
                    start_col: start.column + 1,
                    end_line: end.row + 1,
                    end_col: end.column + 1,
                    docstring,
                    language: lang_name.to_lowercase(),
                    token_count: count_tokens(&signature),
                    ..Default::default()
                });
            }

            // Process Reference/Call capture
            if let (Some(r_name), Some(r_node)) = (ref_name, ref_node) {
                raw_refs.push(RawRef {
                    name: r_name,
                    line: r_node.start_position().row + 1,
                    edge_type: reference_type,
                });
            }
        }

        // Resolve containing symbols for each reference to construct Edges
        let mut edges = Vec::new();
        for r in raw_refs {
            let containing_sym = symbols
                .iter()
                .filter(|sym| sym.start_line <= r.line && r.line <= sym.end_line)
                .min_by_key(|sym| sym.end_line.saturating_sub(sym.start_line));

            let src = match containing_sym {
                Some(sym) => sym.id.clone(),
                None => file_path_str.clone(),
            };

            edges.push(Edge {
                src,
                dst: r.name, // Will be resolved to exact symbol ID at query time
                edge_type: r.edge_type,
                ..Default::default()
            });
        }

        assign_containment(&mut symbols, &mut edges);
        normalize_symbol_roles(&mut symbols);

        Ok((symbols, edges))
    })
}

fn normalize_symbol_roles(symbols: &mut [Symbol]) {
    let parents: HashMap<_, _> = symbols
        .iter()
        .map(|symbol| {
            (
                symbol.id.clone(),
                (symbol.name.clone(), symbol.kind.clone()),
            )
        })
        .collect();

    for symbol in symbols {
        let Some(parent_id) = symbol.parent_id.as_deref() else {
            symbol.qualified_name = Some(symbol.name.clone());
            continue;
        };
        let Some((parent_name, parent_kind)) = parents.get(parent_id) else {
            continue;
        };
        let object_scope = matches!(
            parent_kind.as_str(),
            "Class"
                | "Struct"
                | "Interface"
                | "Trait"
                | "Impl"
                | "Mixin"
                | "Extension"
                | "Protocol"
                | "Record"
                | "Enum"
        );
        if object_scope && symbol.kind == "Function" {
            symbol.kind = if symbol.name == "__init__" || symbol.name == parent_name.as_str() {
                "Constructor".to_string()
            } else {
                "Method".to_string()
            };
        }
        symbol.qualified_name = Some(format!("{parent_name}::{}", symbol.name));
    }
}

/// Link nested symbols to their immediate containing symbol and emit
/// `EdgeType::Contains` edges. This builds the structural hierarchy used by
/// predictive context pre-fetching and token-budget graph pruning.
///
/// Runs in O(n log n) by sorting symbols by start line and using a stack to
/// track the current parent interval.
fn assign_containment(symbols: &mut [Symbol], edges: &mut Vec<Edge>) {
    if symbols.is_empty() {
        return;
    }

    let mut order: Vec<usize> = (0..symbols.len()).collect();
    order.sort_by(|&i, &j| {
        symbols[i]
            .start_line
            .cmp(&symbols[j].start_line)
            .then_with(|| symbols[j].end_line.cmp(&symbols[i].end_line))
    });

    let mut stack: Vec<usize> = Vec::new();
    for idx in order {
        while let Some(&top) = stack.last() {
            if symbols[top].end_line < symbols[idx].start_line {
                stack.pop();
            } else {
                break;
            }
        }

        if let Some(&parent_idx) = stack.last() {
            let child_id = symbols[idx].id.clone();
            let parent_id = symbols[parent_idx].id.clone();
            symbols[idx].parent_id = Some(parent_id.clone());
            symbols[parent_idx].children.push(child_id.clone());
            edges.push(Edge {
                src: parent_id,
                dst: child_id,
                edge_type: EdgeType::Contains,
                ..Default::default()
            });
        }

        stack.push(idx);
    }
}

fn map_kind<'a>(prefix: &str, kind_map: &[(&'a str, &'a str)]) -> &'a str {
    kind_map
        .iter()
        .find(|(p, _)| *p == prefix)
        .map(|(_, kind)| *kind)
        .unwrap_or("Symbol")
}

fn extract_docstring(def: Node<'_>, source: &[u8], lang_name: &str) -> Option<String> {
    // Python docstrings live inside the definition body rather than before it.
    if lang_name == "python" {
        // Keep a text fallback in addition to the grammar walk below. Python
        // grammar releases have represented a definition body using both a
        // named `body` field and an unnamed block node, while the source form
        // of a docstring is stable.
        if let Ok(definition) = def.utf8_text(source) {
            if let Some(docstring) = extract_python_docstring(definition) {
                return Some(docstring);
            }
        }

        if let Some(body) = def.child_by_field_name("body") {
            if let Some(first) = body.named_child(0) {
                if first.kind() == "expression_statement" {
                    if let Some(value) = find_string_node(first) {
                        if let Ok(text) = value.utf8_text(source) {
                            let cleaned = clean_doc_comment(text);
                            if !cleaned.is_empty() {
                                return Some(cleaned);
                            }
                        }
                    }
                }
            }
        }
    }

    // Rust/C/C++/Go/Java and Python comments are named siblings. Collect an
    // immediately preceding contiguous comment block, allowing one blank line.
    let mut comments = Vec::new();
    let mut next_start_row = def.start_position().row;
    let mut sibling = def.prev_named_sibling();
    while let Some(node) = sibling {
        if !node.kind().contains("comment") || node.end_position().row + 2 < next_start_row {
            break;
        }
        if let Ok(text) = node.utf8_text(source) {
            comments.push(clean_doc_comment(text));
        }
        next_start_row = node.start_position().row;
        sibling = node.prev_named_sibling();
    }

    comments.reverse();
    let joined = comments
        .into_iter()
        .filter(|comment| !comment.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!joined.is_empty()).then_some(joined)
}

fn extract_python_docstring(definition: &str) -> Option<String> {
    let (_, body) = definition.split_once('\n')?;
    let body = body.trim_start();

    for delimiter in ["\"\"\"", "'''"] {
        if let Some(rest) = body.strip_prefix(delimiter) {
            let end = rest.find(delimiter)?;
            let cleaned = clean_doc_comment(&format!("{delimiter}{}{delimiter}", &rest[..end]));
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }

    None
}

fn find_string_node(node: Node<'_>) -> Option<Node<'_>> {
    if matches!(node.kind(), "string" | "concatenated_string") {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = find_string_node(child) {
            return Some(found);
        }
    }
    None
}

fn clean_doc_comment(text: &str) -> String {
    let trimmed = text.trim();
    let unwrapped = if (trimmed.starts_with("\"\"\"") && trimmed.ends_with("\"\"\""))
        || (trimmed.starts_with("'''") && trimmed.ends_with("'''"))
    {
        &trimmed[3..trimmed.len().saturating_sub(3)]
    } else if trimmed.starts_with("/*") && trimmed.ends_with("*/") {
        &trimmed[2..trimmed.len().saturating_sub(2)]
    } else {
        trimmed
    };

    unwrapped
        .lines()
        .map(|line| {
            line.trim()
                .trim_start_matches("///")
                .trim_start_matches("//!")
                .trim_start_matches("//")
                .trim_start_matches('#')
                .trim_start_matches('*')
                .trim()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::parser::registry;

    #[test]
    fn test_rust_parsing() {
        let code = r#"
            pub struct Config {
                pub port: u16,
            }

            impl Config {
                pub fn new() -> Self {
                    Config { port: 8080 }
                }
            }

            fn main() {
                let x = Config::new();
            }
        "#;

        let (syms, edges) = parse_rust(Path::new("main.rs"), code).unwrap();
        assert!(syms.len() >= 3);

        let struct_sym = syms
            .iter()
            .find(|s| s.name == "Config" && s.kind == "Struct")
            .unwrap();
        assert_eq!(struct_sym.start_line, 2);

        let main_sym = syms.iter().find(|s| s.name == "main").unwrap();
        assert_eq!(main_sym.kind, "Function");

        // Verify reference edge extracted
        assert!(edges.iter().any(|e| e.dst == "new"));
    }

    #[test]
    fn test_python_parsing() {
        let code = r#"
class Calculator:
    def __init__(self):
        pass

    def add(self, a, b):
        return a + b

def global_helper():
    print("helper")
"#;

        let (syms, _) = parse_python(Path::new("main.py"), code).unwrap();
        assert!(syms.len() >= 3);

        let class_sym = syms.iter().find(|s| s.name == "Calculator").unwrap();
        assert_eq!(class_sym.kind, "Class");

        let global_helper = syms.iter().find(|s| s.name == "global_helper").unwrap();
        assert_eq!(global_helper.kind, "Function");
    }

    #[test]
    fn test_duplicate_names_have_unique_ids_and_docstrings() {
        let code = r#"
/// First overload-like definition.
fn duplicate() {}

/// Second definition in a nested module-like scope.
fn duplicate(value: i32) -> i32 { value }
"#;

        let (syms, _) = parse_rust(Path::new("duplicate.rs"), code).unwrap();
        let duplicates: Vec<_> = syms.iter().filter(|sym| sym.name == "duplicate").collect();
        assert_eq!(duplicates.len(), 2);
        assert_ne!(duplicates[0].id, duplicates[1].id);
        assert!(duplicates.iter().all(|sym| sym.docstring.is_some()));
    }

    #[test]
    fn test_python_docstring_extraction() {
        let code = r#"
def documented(value):
    """Return the supplied value."""
    return value
"#;
        let (syms, _) = parse_python(Path::new("documented.py"), code).unwrap();
        let symbol = syms.iter().find(|sym| sym.name == "documented").unwrap();
        assert_eq!(
            symbol.docstring.as_deref(),
            Some("Return the supplied value.")
        );
    }

    #[test]
    fn test_html_parsing() {
        let code = r#"<!DOCTYPE html>
<html>
  <head><title>Test</title></head>
  <body>
    <div class="app">
      <p>Hello</p>
    </div>
  </body>
</html>
"#;

        let (syms, _) = parse_html(Path::new("index.html"), code).unwrap();
        assert!(!syms.is_empty());
        assert!(syms.iter().any(|s| s.name == "html"));
        assert!(syms.iter().any(|s| s.name == "div"));
    }

    #[test]
    fn test_css_parsing() {
        let code = r#"
.container {
    display: flex;
}

@media (min-width: 600px) {
    .container {
        flex-direction: row;
    }
}
"#;

        let (syms, _) = parse_css(Path::new("app.css"), code).unwrap();
        assert!(!syms.is_empty());
        assert!(syms.iter().any(|s| s.name == ".container"));
        assert!(syms.iter().any(|s| s.name == "@media"));
    }

    #[test]
    fn test_dart_parsing() {
        let code = r#"
class Greeter {
    String name;
    Greeter(this.name);

    void greet() {
        print('Hello, $name');
    }
}

int add(int a, int b) => a + b;
"#;

        let (syms, _) = parse_dart(Path::new("main.dart"), code).unwrap();
        assert!(!syms.is_empty());
        assert!(syms
            .iter()
            .any(|s| s.name == "Greeter" && s.kind == "Class"));
        assert!(syms.iter().any(|s| s.name == "add"));
    }

    #[cfg(feature = "lang-c")]
    #[test]
    fn test_c_parsing() {
        let code = r#"
#include <stdio.h>
struct Point { int x; int y; };

int main() {
    printf("hi");
    return 0;
}
"#;
        let config = registry::config_for_extension("c").unwrap();
        let (syms, edges) = parse_with_config(Path::new("main.c"), code, config).unwrap();
        assert!(syms
            .iter()
            .any(|s| s.name == "main" && s.kind == "Function"));
        assert!(syms.iter().any(|s| s.name == "Point" && s.kind == "Struct"));
        assert!(edges.iter().any(|e| e.dst == "printf"));
    }

    #[cfg(feature = "lang-cpp")]
    #[test]
    fn test_cpp_parsing() {
        let code = r#"
class Greeter { public: void greet() {} };

int main() {
    Greeter g;
    g.greet();
    return 0;
}
"#;
        let config = registry::config_for_extension("cpp").unwrap();
        let (syms, edges) = parse_with_config(Path::new("main.cpp"), code, config).unwrap();
        assert!(syms
            .iter()
            .any(|s| s.name == "Greeter" && s.kind == "Class"));
        assert!(syms
            .iter()
            .any(|s| s.name == "main" && s.kind == "Function"));
        assert!(edges.iter().any(|e| e.dst == "greet"));
    }

    #[cfg(feature = "lang-go")]
    #[test]
    fn test_go_parsing() {
        let code = r#"
package main
import "fmt"

type User struct { Name string }

func main() {
    fmt.Println("hi")
}
"#;
        let config = registry::config_for_extension("go").unwrap();
        let (syms, edges) = parse_with_config(Path::new("main.go"), code, config).unwrap();
        assert!(syms
            .iter()
            .any(|s| s.name == "main" && s.kind == "Function"));
        assert!(syms.iter().any(|s| s.name == "User" && s.kind == "Struct"));
        assert!(edges.iter().any(|e| e.dst == "Println"));
    }

    #[cfg(feature = "lang-java")]
    #[test]
    fn test_java_parsing() {
        let code = r#"
class App {
    public static void main(String[] args) {
        System.out.println("hi");
    }
}

class Util {
    public void run() {}
}
"#;
        let config = registry::config_for_extension("java").unwrap();
        let (syms, edges) = parse_with_config(Path::new("App.java"), code, config).unwrap();
        assert!(syms.iter().any(|s| s.name == "App" && s.kind == "Class"));
        assert!(syms.iter().any(|s| s.name == "main" && s.kind == "Method"));
        assert!(edges.iter().any(|e| e.dst == "println"));
    }
}
