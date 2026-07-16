use crate::storage::{Edge, EdgeType, Storage, StorageError, Symbol};
use std::collections::HashMap;
use thiserror::Error;

const MAX_QUERY_CHARS: usize = 16_384;
const MAX_QUERY_TOKENS: usize = 2_048;
const DEFAULT_RESULT_LIMIT: usize = 1_000;
const MAX_RESULT_LIMIT: usize = 10_000;

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
    Contains,
    Starts,
    Ends,
    With,
    Limit,
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
    Integer(usize),
    Ident(String),
    String(String),
}

fn tokenize(input: &str) -> Result<Vec<Token>, GraphQueryError> {
    if input.chars().count() > MAX_QUERY_CHARS {
        return Err(GraphQueryError::Parse(format!(
            "query exceeds the {MAX_QUERY_CHARS}-character limit"
        )));
    }
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
                "CONTAINS" => Token::Contains,
                "STARTS" => Token::Starts,
                "ENDS" => Token::Ends,
                "WITH" => Token::With,
                "LIMIT" => Token::Limit,
                _ => Token::Ident(ident),
            };
            tokens.push(tok);
            continue;
        }
        if ch == '\'' || ch == '"' {
            let quote = ch;
            let mut s = String::new();
            let mut closed = false;
            let mut escaped = false;
            for c in chars.by_ref() {
                if escaped {
                    s.push(c);
                    escaped = false;
                    continue;
                }
                if c == '\\' {
                    escaped = true;
                    continue;
                }
                if c == quote {
                    closed = true;
                    break;
                }
                s.push(c);
            }
            if !closed {
                return Err(GraphQueryError::Parse(
                    "unterminated string literal".to_string(),
                ));
            }
            tokens.push(Token::String(s));
            continue;
        }
        if ch.is_ascii_digit() {
            let mut digits = ch.to_string();
            while let Some(next) = chars.peek().copied().filter(char::is_ascii_digit) {
                digits.push(next);
                chars.next();
            }
            let value = digits
                .parse()
                .map_err(|_| GraphQueryError::Parse("invalid integer".to_string()))?;
            tokens.push(Token::Integer(value));
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
        if tokens.len() > MAX_QUERY_TOKENS {
            return Err(GraphQueryError::Parse(format!(
                "query exceeds the {MAX_QUERY_TOKENS}-token limit"
            )));
        }
    }
    if tokens.len() > MAX_QUERY_TOKENS {
        return Err(GraphQueryError::Parse(format!(
            "query exceeds the {MAX_QUERY_TOKENS}-token limit"
        )));
    }
    Ok(tokens)
}

#[derive(Debug, Clone)]
pub enum Field {
    Name,
    Kind,
    FilePath,
    Language,
    Signature,
    QualifiedName,
}

#[derive(Debug, Clone, Copy)]
pub enum Comparison {
    Eq,
    Contains,
    StartsWith,
    EndsWith,
}

#[derive(Debug, Clone)]
pub struct Condition {
    pub alias: String,
    pub field: Field,
    pub comparison: Comparison,
    pub value: String,
}

#[derive(Debug, Clone)]
struct NodePattern {
    alias: String,
}

#[derive(Debug, Clone)]
struct EdgePattern {
    alias: Option<String>,
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
    limit: usize,
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

    let limit = if matches!(tokens.get(pos), Some(Token::Limit)) {
        pos += 1;
        match tokens.get(pos) {
            Some(Token::Integer(value)) => {
                pos += 1;
                if *value == 0 || *value > MAX_RESULT_LIMIT {
                    return Err(GraphQueryError::Parse(format!(
                        "LIMIT must be between 1 and {MAX_RESULT_LIMIT}"
                    )));
                }
                *value
            }
            other => {
                return Err(GraphQueryError::Parse(format!(
                    "expected integer after LIMIT, got {other:?}"
                )))
            }
        }
    } else {
        DEFAULT_RESULT_LIMIT
    };
    if pos != tokens.len() {
        return Err(GraphQueryError::Parse(format!(
            "unexpected trailing token {:?}",
            tokens[pos]
        )));
    }
    if left.alias == right.alias {
        return Err(GraphQueryError::Parse(
            "node aliases must be distinct".to_string(),
        ));
    }
    let valid_alias = |alias: &str| {
        alias == left.alias || alias == right.alias || edge.alias.as_deref() == Some(alias)
    };
    if let Some(alias) = conditions
        .iter()
        .map(|condition| condition.alias.as_str())
        .chain(returns.iter().map(String::as_str))
        .find(|alias| !valid_alias(alias))
    {
        return Err(GraphQueryError::Parse(format!("unknown alias '{alias}'")));
    }

    Ok(ParsedQuery {
        left,
        edge,
        right,
        conditions,
        returns,
        limit,
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
    let alias = if let Some(Token::Ident(alias)) = tokens.get(*pos) {
        *pos += 1;
        Some(alias.clone())
    } else {
        None
    };

    let edge_type = if let Some(Token::Colon) = tokens.get(*pos) {
        *pos += 1;
        let name = expect_ident(tokens, pos)?;
        Some(parse_edge_type(&name)?)
    } else {
        None
    };

    expect_token(tokens, pos, &Token::RBracket)?;
    expect_token(tokens, pos, &Token::Arrow)?;
    Ok(EdgePattern { alias, edge_type })
}

fn parse_condition(tokens: &[Token], pos: &mut usize) -> Result<Condition, GraphQueryError> {
    let alias = expect_ident(tokens, pos)?;
    expect_token(tokens, pos, &Token::Dot)?;
    let field_name = expect_ident(tokens, pos)?;
    let comparison = match tokens.get(*pos) {
        Some(Token::Eq) => {
            *pos += 1;
            Comparison::Eq
        }
        Some(Token::Contains) => {
            *pos += 1;
            Comparison::Contains
        }
        Some(Token::Starts) => {
            *pos += 1;
            expect_token(tokens, pos, &Token::With)?;
            Comparison::StartsWith
        }
        Some(Token::Ends) => {
            *pos += 1;
            expect_token(tokens, pos, &Token::With)?;
            Comparison::EndsWith
        }
        other => {
            return Err(GraphQueryError::Parse(format!(
                "expected =, CONTAINS, STARTS WITH, or ENDS WITH; got {other:?}"
            )))
        }
    };
    let value = expect_string(tokens, pos)?;
    let field = match field_name.to_lowercase().as_str() {
        "name" => Field::Name,
        "kind" => Field::Kind,
        "file_path" | "filepath" | "path" => Field::FilePath,
        "language" | "lang" => Field::Language,
        "signature" => Field::Signature,
        "qualified_name" | "qualifiedname" | "qualified" => Field::QualifiedName,
        other => return Err(GraphQueryError::Parse(format!("unknown field '{}'", other))),
    };
    Ok(Condition {
        alias,
        field,
        comparison,
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
        .all(|condition| {
            let actual = match &condition.field {
                Field::Name => sym.name.as_str(),
                Field::Kind => sym.kind.as_str(),
                Field::FilePath => sym.file_path.as_str(),
                Field::Language => sym.language.as_str(),
                Field::Signature => sym.signature.as_str(),
                Field::QualifiedName => sym.qualified_name.as_deref().unwrap_or_default(),
            };
            compare(actual, &condition.value, condition.comparison)
        })
}

fn compare(actual: &str, expected: &str, comparison: Comparison) -> bool {
    match comparison {
        Comparison::Eq => actual == expected,
        Comparison::Contains => actual.to_lowercase().contains(&expected.to_lowercase()),
        Comparison::StartsWith => actual.to_lowercase().starts_with(&expected.to_lowercase()),
        Comparison::EndsWith => actual.to_lowercase().ends_with(&expected.to_lowercase()),
    }
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
        crate::cancellation::checkpoint()
            .map_err(|error| GraphQueryError::Parse(error.to_string()))?;
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
        if matched_edges.len() >= q.limit
            || (!want_edge && left_matches.len() + right_matches.len() >= q.limit)
        {
            break;
        }
    }

    if want_edge {
        matched_edges.truncate(q.limit);
        Ok(QueryResult::Edges(matched_edges))
    } else {
        let mut symbols: Vec<Symbol> = left_matches
            .into_values()
            .chain(right_matches.into_values())
            .collect();
        symbols.sort_by(|a, b| a.id.cmp(&b.id));
        symbols.dedup_by(|a, b| a.id == b.id);
        symbols.truncate(q.limit);
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

    #[test]
    fn query_supports_safe_text_operators_and_limit() {
        let parsed = parse(
            "MATCH (a)-[edge:Calls]->(b) WHERE a.path CONTAINS 'src/' AND b.name STARTS WITH 'get' RETURN a, edge LIMIT 25",
        )
        .unwrap();
        assert_eq!(parsed.limit, 25);
        assert_eq!(parsed.conditions.len(), 2);
        assert_eq!(parsed.edge.alias.as_deref(), Some("edge"));
    }

    #[test]
    fn malformed_and_ambiguous_queries_are_rejected() {
        assert!(parse("MATCH (a)-[:Calls]->(b) RETURN a LIMIT 10 garbage").is_err());
        assert!(parse("MATCH (a)-[:Calls]->(b) RETURN a LIMIT 0").is_err());
        assert!(parse("MATCH (a)-[:Calls]->(b) RETURN a LIMIT 10001").is_err());
        assert!(parse("MATCH (a)-[:Calls]->(b) WHERE a.name = 'unterminated RETURN a").is_err());
        assert!(parse("MATCH (a)-[:Calls]->(a) RETURN a").is_err());
        assert!(parse("MATCH (a)-[:Calls]->(b) RETURN missing").is_err());
    }
}
