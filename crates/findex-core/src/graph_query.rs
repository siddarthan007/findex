use crate::storage::{Edge, EdgeType, Storage, StorageError, Symbol};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum GraphQueryError {
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),
}

/// Result of a graph query.
#[derive(Debug, Clone)]
pub enum QueryResult {
    Nodes(Vec<Symbol>),
    Edges(Vec<Edge>),
}

impl QueryResult {
    pub fn to_text(&self) -> String {
        match self {
            QueryResult::Nodes(syms) => {
                if syms.is_empty() {
                    return "(no symbols matched)".to_string();
                }
                syms.iter()
                    .map(|s| {
                        format!(
                            "[{}] {} -> {}:{}-{}",
                            s.kind, s.name, s.file_path, s.start_line, s.end_line
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            QueryResult::Edges(edges) => {
                if edges.is_empty() {
                    return "(no edges matched)".to_string();
                }
                edges
                    .iter()
                    .map(|e| format!("{} -{:?}-> {}", e.src, e.edge_type, e.dst))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
    }
}

#[derive(Debug, Clone)]
enum Token {
    Match,
    Return,
    Where,
    And,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Colon,
    Arrow,
    Dash,
    Comma,
    Dot,
    Eq,
    Ident(String),
    String(String),
}

fn tokenize(input: &str) -> Result<Vec<Token>, GraphQueryError> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        if ch.is_alphabetic() || ch == '_' {
            let mut ident = String::new();
            ident.push(ch);
            while let Some(&c) = chars.peek() {
                if c.is_alphanumeric() || c == '_' {
                    ident.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            let tok = match ident.to_uppercase().as_str() {
                "MATCH" => Token::Match,
                "RETURN" => Token::Return,
                "WHERE" => Token::Where,
                "AND" => Token::And,
                _ => Token::Ident(ident),
            };
            tokens.push(tok);
            continue;
        }
        if ch == '\'' || ch == '"' {
            let quote = ch;
            let mut s = String::new();
            for c in chars.by_ref() {
                if c == quote {
                    break;
                }
                s.push(c);
            }
            tokens.push(Token::String(s));
            continue;
        }
        let tok = match ch {
            '(' => Token::LParen,
            ')' => Token::RParen,
            '[' => Token::LBracket,
            ']' => Token::RBracket,
            ':' => Token::Colon,
            ',' => Token::Comma,
            '.' => Token::Dot,
            '=' => Token::Eq,
            '-' => {
                if chars.peek() == Some(&'>') {
                    chars.next();
                    Token::Arrow
                } else {
                    Token::Dash
                }
            }
            _ => {
                return Err(GraphQueryError::Parse(format!(
                    "unexpected character '{}'",
                    ch
                )))
            }
        };
        tokens.push(tok);
    }
    Ok(tokens)
}

#[derive(Debug, Clone)]
pub enum Field {
    Name,
    Kind,
    FilePath,
}

#[derive(Debug, Clone)]
pub struct Condition {
    pub alias: String,
    pub field: Field,
    pub value: String,
}

#[derive(Debug, Clone)]
struct NodePattern {
    alias: String,
}

#[derive(Debug, Clone)]
struct EdgePattern {
    edge_type: Option<EdgeType>,
}

#[derive(Debug, Clone)]
struct ParsedQuery {
    left: NodePattern,
    edge: EdgePattern,
    right: NodePattern,
    conditions: Vec<Condition>,
    #[allow(dead_code)]
    returns: Vec<String>,
}

fn parse(input: &str) -> Result<ParsedQuery, GraphQueryError> {
    let tokens = tokenize(input)?;
    let mut pos = 0usize;

    macro_rules! expect {
        ($pat:pat) => {{
            let tok = tokens
                .get(pos)
                .ok_or_else(|| GraphQueryError::Parse("unexpected end of query".to_string()))?;
            match tok {
                $pat => {
                    pos += 1;
                }
                _ => {
                    return Err(GraphQueryError::Parse(format!(
                        "unexpected token {:?}",
                        tok
                    )))
                }
            }
        }};
    }

    expect!(Token::Match);
    let left = parse_node(&tokens, &mut pos)?;
    let edge = parse_edge(&tokens, &mut pos)?;
    let right = parse_node(&tokens, &mut pos)?;

    let mut conditions = Vec::new();
    if let Some(Token::Where) = tokens.get(pos) {
        pos += 1;
        conditions.push(parse_condition(&tokens, &mut pos)?);
        while let Some(Token::And) = tokens.get(pos) {
            pos += 1;
            conditions.push(parse_condition(&tokens, &mut pos)?);
        }
    }

    expect!(Token::Return);
    let mut returns = Vec::new();
    returns.push(expect_ident(&tokens, &mut pos)?);
    while let Some(Token::Comma) = tokens.get(pos) {
        pos += 1;
        returns.push(expect_ident(&tokens, &mut pos)?);
    }

    Ok(ParsedQuery {
        left,
        edge,
        right,
        conditions,
        returns,
    })
}

fn parse_node(tokens: &[Token], pos: &mut usize) -> Result<NodePattern, GraphQueryError> {
    expect_token(tokens, pos, &Token::LParen)?;
    let alias = expect_ident(tokens, pos)?;
    expect_token(tokens, pos, &Token::RParen)?;
    Ok(NodePattern { alias })
}

fn parse_edge(tokens: &[Token], pos: &mut usize) -> Result<EdgePattern, GraphQueryError> {
    expect_token(tokens, pos, &Token::Dash)?;
    expect_token(tokens, pos, &Token::LBracket)?;

    // Optional edge alias
    if let Some(Token::Ident(_)) = tokens.get(*pos) {
        *pos += 1;
    }

    let edge_type = if let Some(Token::Colon) = tokens.get(*pos) {
        *pos += 1;
        let name = expect_ident(tokens, pos)?;
        Some(parse_edge_type(&name)?)
    } else {
        None
    };

    expect_token(tokens, pos, &Token::RBracket)?;
    expect_token(tokens, pos, &Token::Arrow)?;
    Ok(EdgePattern { edge_type })
}

fn parse_condition(tokens: &[Token], pos: &mut usize) -> Result<Condition, GraphQueryError> {
    let alias = expect_ident(tokens, pos)?;
    expect_token(tokens, pos, &Token::Dot)?;
    let field_name = expect_ident(tokens, pos)?;
    expect_token(tokens, pos, &Token::Eq)?;
    let value = expect_string(tokens, pos)?;
    let field = match field_name.to_lowercase().as_str() {
        "name" => Field::Name,
        "kind" => Field::Kind,
        "file_path" | "filepath" | "path" => Field::FilePath,
        other => return Err(GraphQueryError::Parse(format!("unknown field '{}'", other))),
    };
    Ok(Condition {
        alias,
        field,
        value,
    })
}

fn expect_ident(tokens: &[Token], pos: &mut usize) -> Result<String, GraphQueryError> {
    match tokens.get(*pos) {
        Some(Token::Ident(s)) => {
            *pos += 1;
            Ok(s.clone())
        }
        other => Err(GraphQueryError::Parse(format!(
            "expected identifier, got {:?}",
            other
        ))),
    }
}

fn expect_string(tokens: &[Token], pos: &mut usize) -> Result<String, GraphQueryError> {
    match tokens.get(*pos) {
        Some(Token::String(s)) => {
            *pos += 1;
            Ok(s.clone())
        }
        other => Err(GraphQueryError::Parse(format!(
            "expected string, got {:?}",
            other
        ))),
    }
}

fn expect_token(
    tokens: &[Token],
    pos: &mut usize,
    expected: &Token,
) -> Result<(), GraphQueryError> {
    match tokens.get(*pos) {
        Some(tok) if std::mem::discriminant(tok) == std::mem::discriminant(expected) => {
            *pos += 1;
            Ok(())
        }
        other => Err(GraphQueryError::Parse(format!(
            "expected {:?}, got {:?}",
            expected, other
        ))),
    }
}

fn parse_edge_type(name: &str) -> Result<EdgeType, GraphQueryError> {
    match name.to_lowercase().as_str() {
        "calls" => Ok(EdgeType::Calls),
        "imports" => Ok(EdgeType::Imports),
        "defines" => Ok(EdgeType::Defines),
        "references" => Ok(EdgeType::References),
        "inherits" => Ok(EdgeType::Inherits),
        "contains" => Ok(EdgeType::Contains),
        other => Err(GraphQueryError::Parse(format!(
            "unknown edge type '{}'",
            other
        ))),
    }
}

fn matches_symbol(sym: &Symbol, conditions: &[Condition], alias: &str) -> bool {
    conditions
        .iter()
        .filter(|c| c.alias == alias)
        .all(|c| match &c.field {
            Field::Name => sym.name == c.value,
            Field::Kind => sym.kind == c.value,
            Field::FilePath => sym.file_path == c.value,
        })
}

/// Execute a Cypher-like graph query against the index.
///
/// Supported shape:
/// `MATCH (a)-[:EdgeType]->(b) WHERE a.name = '...' RETURN a, b`
pub fn query_graph(storage: &Storage, input: &str) -> Result<QueryResult, GraphQueryError> {
    let q = parse(input)?;

    let want_edge = q.edge.edge_type.is_some();
    let edges = storage.list_edges()?;
    let mut matched_edges: Vec<Edge> = Vec::new();
    let mut left_matches: HashMap<String, Symbol> = HashMap::new();
    let mut right_matches: HashMap<String, Symbol> = HashMap::new();

    for edge in edges {
        if let Some(expected) = q.edge.edge_type {
            if edge.edge_type != expected {
                continue;
            }
        }

        let Some(src) = storage.get_symbol(&edge.src)? else {
            continue;
        };
        let Some(dst) = storage.get_symbol(&edge.dst)? else {
            continue;
        };

        if !matches_symbol(&src, &q.conditions, &q.left.alias) {
            continue;
        }
        if !matches_symbol(&dst, &q.conditions, &q.right.alias) {
            continue;
        }

        if want_edge {
            matched_edges.push(edge);
        }
        left_matches.insert(src.id.clone(), src);
        right_matches.insert(dst.id.clone(), dst);
    }

    if want_edge {
        Ok(QueryResult::Edges(matched_edges))
    } else {
        let mut symbols: Vec<Symbol> = left_matches
            .into_values()
            .chain(right_matches.into_values())
            .collect();
        symbols.sort_by(|a, b| a.id.cmp(&b.id));
        symbols.dedup_by(|a, b| a.id == b.id);
        Ok(QueryResult::Nodes(symbols))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Edge, EdgeType, Storage, Symbol};
    use tempfile::tempdir;

    fn make_sym(id: &str, name: &str, kind: &str, file: &str) -> Symbol {
        Symbol {
            id: id.to_string(),
            name: name.to_string(),
            kind: kind.to_string(),
            signature: name.to_string(),
            file_path: file.to_string(),
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 1,
            docstring: None,
            ..Default::default()
        }
    }

    #[test]
    fn test_query_call_edges() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path().join("db")).unwrap();

        let a = make_sym("f#a", "a", "Function", "f");
        let b = make_sym("f#b", "b", "Function", "f");
        storage.save_symbols_batch(&[a, b]).unwrap();
        storage
            .save_edges_batch(&[Edge {
                src: "f#a".to_string(),
                dst: "f#b".to_string(),
                edge_type: EdgeType::Calls,
                ..Default::default()
            }])
            .unwrap();

        let res = query_graph(&storage, "MATCH (a)-[:Calls]->(b) RETURN a, b").unwrap();
        match res {
            QueryResult::Edges(edges) => assert_eq!(edges.len(), 1),
            _ => panic!("expected edges"),
        }
    }

    #[test]
    fn test_query_with_where() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path().join("db")).unwrap();

        let a = make_sym("f#a", "a", "Function", "f");
        let b = make_sym("f#b", "b", "Function", "f");
        storage.save_symbols_batch(&[a, b]).unwrap();
        storage
            .save_edges_batch(&[Edge {
                src: "f#a".to_string(),
                dst: "f#b".to_string(),
                edge_type: EdgeType::Calls,
                ..Default::default()
            }])
            .unwrap();

        let res = query_graph(
            &storage,
            "MATCH (a)-[:Calls]->(b) WHERE a.name = 'a' RETURN a, b",
        )
        .unwrap();
        match res {
            QueryResult::Edges(edges) => assert_eq!(edges.len(), 1),
            _ => panic!("expected edges"),
        }
    }
}
