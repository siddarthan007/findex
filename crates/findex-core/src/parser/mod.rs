pub mod js_ts;
pub mod registry;
pub mod tree_sitter_impl;
pub mod vue;

use crate::storage::{Edge, Symbol};
use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ParserError {
    #[error("Tree-sitter parser error: {0}")]
    TreeSitter(String),
    #[error("Oxc parser error: {0}")]
    Oxc(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Unsupported language for path: {0}")]
    Unsupported(String),
}

/// Return whether a path has a parser in this build. Watchers and discovery use
/// the same registry so newly enabled languages cannot silently go stale.
pub fn is_supported_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension,
                "vue" | "js" | "mjs" | "cjs" | "ts" | "mts" | "cts" | "jsx" | "tsx"
            ) || registry::config_for_extension(extension).is_some()
        })
        .unwrap_or(false)
}

/// Dynamic dispatch parser based on file extension.
/// JS/TS is handled by the oxc-based parser; all other supported extensions are
/// looked up in the static language registry.
pub fn parse_code(path: &Path, content: &str) -> Result<(Vec<Symbol>, Vec<Edge>), ParserError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| ParserError::Unsupported(path.to_string_lossy().to_string()))?;

    match ext {
        "vue" => vue::parse_vue(path, content),
        "js" | "mjs" | "cjs" | "ts" | "mts" | "cts" | "jsx" | "tsx" => {
            js_ts::parse_js_ts(path, content)
        }
        _ => {
            if let Some(config) = registry::config_for_extension(ext) {
                tree_sitter_impl::parse_with_config(path, content, config)
            } else {
                Err(ParserError::Unsupported(path.to_string_lossy().to_string()))
            }
        }
    }
}

#[cfg(test)]
mod major_language_tests {
    use super::*;
    use crate::storage::EdgeType;

    fn assert_symbol(symbols: &[Symbol], name: &str, kind: &str) {
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == name && symbol.kind == kind),
            "missing {kind} {name}: {symbols:#?}"
        );
    }

    #[cfg(feature = "lang-csharp")]
    #[test]
    fn csharp_oop_contracts_and_inheritance() {
        let source = r#"
namespace Billing.Core {
    public interface IRepository { Task Load(); }
    public record Invoice(int Id);
    public class SqlRepository : IRepository {
        public string Connection { get; init; }
        public SqlRepository(string connection) { Connection = connection; }
        public Task Load() => Task.CompletedTask;
    }
}
"#;
        let (symbols, edges) = parse_code(Path::new("Billing.cs"), source).unwrap();
        assert_symbol(&symbols, "Billing.Core", "Namespace");
        assert_symbol(&symbols, "IRepository", "Interface");
        assert_symbol(&symbols, "Invoice", "Record");
        assert_symbol(&symbols, "SqlRepository", "Class");
        assert_symbol(&symbols, "Connection", "Property");
        assert_symbol(&symbols, "SqlRepository", "Constructor");
        assert!(edges
            .iter()
            .any(|edge| edge.edge_type == EdgeType::Inherits && edge.dst == "IRepository"));
    }

    #[cfg(feature = "lang-ruby")]
    #[test]
    fn ruby_modules_classes_and_calls() {
        let source = r#"
module Billing
  class Repository < BaseRepository
    def load(id)
      fetch(id)
    end
  end
end
"#;
        let (symbols, edges) = parse_code(Path::new("billing.rb"), source).unwrap();
        assert_symbol(&symbols, "Billing", "Module");
        assert_symbol(&symbols, "Repository", "Class");
        assert_symbol(&symbols, "load", "Method");
        assert!(edges
            .iter()
            .any(|edge| { edge.edge_type == EdgeType::Inherits && edge.dst == "BaseRepository" }));
        assert!(edges
            .iter()
            .any(|edge| edge.edge_type == EdgeType::Calls && edge.dst == "fetch"));
    }

    #[cfg(feature = "lang-php")]
    #[test]
    fn php_traits_interfaces_and_inheritance() {
        let source = r#"<?php
namespace Billing;
interface Repository { public function load(int $id); }
trait LogsQueries { public function logQuery() {} }
class SqlRepository extends BaseRepository implements Repository {
    public string $connection;
    public function load(int $id) { return $this->fetch($id); }
}
"#;
        let (symbols, edges) = parse_code(Path::new("Billing.php"), source).unwrap();
        assert_symbol(&symbols, "Billing", "Namespace");
        assert_symbol(&symbols, "Repository", "Interface");
        assert_symbol(&symbols, "LogsQueries", "Trait");
        assert_symbol(&symbols, "SqlRepository", "Class");
        assert_symbol(&symbols, "connection", "Property");
        assert!(edges
            .iter()
            .any(|edge| edge.edge_type == EdgeType::Inherits));
    }

    #[cfg(feature = "lang-swift")]
    #[test]
    fn swift_protocols_value_types_and_extensions() {
        let source = r#"
protocol Repository { func load(id: Int) async throws -> String }
class BaseRepository {}
final class SqlRepository: BaseRepository, Repository {
    init() {}
    func load(id: Int) async throws -> String { "ok" }
}
struct Invoice { let id: Int }
enum State { case ready, busy }
extension Invoice { func validate() -> Bool { true } }
"#;
        let (symbols, edges) = parse_code(Path::new("Billing.swift"), source).unwrap();
        assert_symbol(&symbols, "Repository", "Protocol");
        assert_symbol(&symbols, "SqlRepository", "Class");
        assert_symbol(&symbols, "Invoice", "Struct");
        assert_symbol(&symbols, "State", "Enum");
        assert_symbol(&symbols, "Invoice", "Extension");
        assert_symbol(&symbols, "init", "Constructor");
        assert!(edges
            .iter()
            .any(|edge| edge.edge_type == EdgeType::Inherits));
    }
}
