//! Deterministic code-query understanding used before lexical/vector retrieval.
//!
//! This deliberately avoids an LLM on the hot path. It separates identifier,
//! concept, and relation evidence so behavioral questions can be matched to the
//! code graph without spending network tokens or introducing nondeterminism.

use crate::storage::{EdgeType, Storage, Symbol};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryIntent {
    pub raw: String,
    pub terms: Vec<String>,
    pub expanded_terms: Vec<String>,
    pub subject_terms: Vec<String>,
    pub object_terms: Vec<String>,
    pub relation: Option<EdgeType>,
    pub canonical: String,
    pub lexical_query: String,
}

pub fn analyze_query(query: &str) -> QueryIntent {
    let raw = query.trim().chars().take(2_048).collect::<String>();
    let tokens = code_tokens(&raw);
    let relation_index = tokens
        .iter()
        .position(|token| relation_for(token).is_some());
    let relation = relation_index.and_then(|index| relation_for(&tokens[index]));
    let terms = useful_terms(&tokens);
    let subject_terms = relation_index
        .map(|index| useful_terms(&tokens[..index]))
        .unwrap_or_default();
    let object_terms = relation_index
        .map(|index| useful_terms(&tokens[index + 1..]))
        .unwrap_or_default();

    let mut expanded = BTreeSet::new();
    for term in &terms {
        expanded.insert(term.clone());
        for synonym in synonyms(term) {
            expanded.insert((*synonym).to_string());
        }
    }
    let expanded_terms: Vec<_> = expanded.into_iter().take(32).collect();
    let lexical_query = expanded_terms
        .iter()
        .map(|term| escape_tantivy_term(term))
        .collect::<Vec<_>>()
        .join(" ");
    let mut canonical_terms = expanded_terms.clone();
    canonical_terms.sort();
    let canonical_subject = canonicalize_terms(&subject_terms);
    let canonical_object = canonicalize_terms(&object_terms);
    let canonical = format!(
        "{}|{}|{}|{}",
        relation
            .map(|value| format!("{value:?}").to_ascii_lowercase())
            .unwrap_or_else(|| "any".to_string()),
        canonical_subject.join("+"),
        canonical_object.join("+"),
        canonical_terms.join("+")
    );
    QueryIntent {
        raw,
        terms,
        expanded_terms,
        subject_terms,
        object_terms,
        relation,
        canonical,
        lexical_query,
    }
}

/// Score graph evidence that directly satisfies a relationship-shaped query.
/// Sources receive the largest boost because "A calls B" normally asks for A.
pub fn relation_evidence_boost(
    storage: &Storage,
    symbol: &Symbol,
    intent: &QueryIntent,
) -> Result<f32, crate::storage::StorageError> {
    let Some(relation) = intent.relation else {
        return Ok(text_match_score(symbol, &intent.expanded_terms) * 0.08);
    };
    let subject_match = text_match_score(symbol, &intent.subject_terms)
        .max(text_match_score(symbol, &expand_side(&intent.subject_terms)) * 0.8);
    let mut best: f32 = 0.0;
    for edge in storage.get_edges_by_src(&symbol.id)? {
        if edge.edge_type != relation {
            continue;
        }
        let target = match storage.get_symbol(&edge.dst)? {
            Some(target) => Some(target),
            None => crate::resolver::resolve_definition(&edge.dst, &symbol.id, storage)?,
        };
        let Some(target) = target else { continue };
        let object_match = text_match_score(&target, &intent.object_terms)
            .max(text_match_score(&target, &expand_side(&intent.object_terms)) * 0.8);
        let evidence = if edge.trace_id.is_some() {
            0.14
        } else if edge.tags.iter().any(|tag| tag == "stack-graphs") {
            0.1
        } else {
            0.04
        };
        best = best.max(subject_match * 0.24 + object_match * 0.34 + evidence);
    }
    // A target may be the strongest lexical/semantic anchor. Keep it, but rank
    // a matching caller above it when both are available.
    for edge in storage.get_edges_by_dst(&symbol.id)? {
        if edge.edge_type == relation {
            best = best.max(
                text_match_score(symbol, &intent.object_terms)
                    .max(text_match_score(symbol, &expand_side(&intent.object_terms)) * 0.8)
                    * 0.18,
            );
        }
    }
    Ok(best.min(0.72))
}

pub fn code_tokens(value: &str) -> Vec<String> {
    let mut normalized = String::with_capacity(value.len() + 16);
    let mut previous_lower_or_digit = false;
    for character in value.chars() {
        if character.is_ascii_uppercase() && previous_lower_or_digit {
            normalized.push(' ');
        }
        if character.is_alphanumeric() {
            normalized.extend(character.to_lowercase());
            previous_lower_or_digit = character.is_ascii_lowercase() || character.is_ascii_digit();
        } else {
            normalized.push(' ');
            previous_lower_or_digit = false;
        }
    }
    normalized
        .split_whitespace()
        .filter(|term| term.len() > 1)
        .map(str::to_string)
        .collect()
}

fn useful_terms(tokens: &[String]) -> Vec<String> {
    tokens
        .iter()
        .filter(|term| !is_stopword(term) && relation_for(term).is_none())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(24)
        .collect()
}

fn expand_side(terms: &[String]) -> Vec<String> {
    let mut expanded = BTreeSet::new();
    for term in terms {
        expanded.insert(term.clone());
        for synonym in synonyms(term) {
            expanded.insert((*synonym).to_string());
        }
    }
    expanded.into_iter().collect()
}

fn canonicalize_terms(terms: &[String]) -> Vec<String> {
    terms
        .iter()
        .map(|term| match term.as_str() {
            "auth" | "authentication" | "authenticate" | "authorization" | "authorize"
            | "identity" | "login" | "oauth" | "jwt" => "auth",
            "api" | "endpoint" | "route" | "handler" | "http" => "api",
            "database" | "db" | "repository" | "storage" | "sql" => "database",
            "cache" | "cached" | "memoize" | "lru" | "redis" => "cache",
            "ui" | "component" | "view" | "widget" | "frontend" => "ui",
            "service" | "manager" | "provider" => "service",
            "array" | "list" | "slice" | "vec" | "vector" => "sequence",
            "map" | "hashmap" | "dictionary" | "dict" | "table" => "map",
            "queue" | "deque" | "channel" => "queue",
            "tree" | "trie" | "ast" | "syntax" => "tree",
            "graph" | "dag" | "adjacency" | "edge" | "node" => "graph",
            "lock" | "mutex" | "semaphore" | "atomic" | "synchronization" => "sync",
            "error" | "exception" | "failure" | "result" => "error",
            _ => term,
        })
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn text_match_score(symbol: &Symbol, terms: &[String]) -> f32 {
    if terms.is_empty() {
        return 0.35;
    }
    let haystack = code_tokens(&format!(
        "{} {} {} {} {} {}",
        symbol.name,
        symbol.qualified_name.as_deref().unwrap_or_default(),
        symbol.kind,
        symbol.signature,
        symbol.file_path,
        symbol.docstring.as_deref().unwrap_or_default()
    ));
    let matches = terms
        .iter()
        .filter(|term| haystack.iter().any(|token| token == *term))
        .count();
    matches as f32 / terms.len().max(1) as f32
}

fn relation_for(term: &str) -> Option<EdgeType> {
    match term {
        "call" | "calls" | "called" | "calling" | "invoke" | "invokes" | "request" | "requests"
        | "send" | "sends" | "dispatch" | "dispatches" | "forward" | "forwards" | "uses" => {
            Some(EdgeType::Calls)
        }
        "import" | "imports" | "include" | "includes" | "require" | "requires" => {
            Some(EdgeType::Imports)
        }
        "depend" | "depends" | "dependency" | "dependencies" => Some(EdgeType::Imports),
        "inherit" | "inherits" | "extend" | "extends" | "implement" | "implements" => {
            Some(EdgeType::Inherits)
        }
        "contain" | "contains" | "inside" | "within" => Some(EdgeType::Contains),
        "define" | "defines" | "declares" => Some(EdgeType::Defines),
        "reference" | "references" | "reads" | "writes" | "publishes" | "subscribes" | "emits"
        | "consumes" => Some(EdgeType::References),
        _ => None,
    }
}

fn synonyms(term: &str) -> &'static [&'static str] {
    match term {
        "auth" | "authentication" | "authenticate" | "authorization" | "authorize" => &[
            "auth",
            "authentication",
            "login",
            "identity",
            "session",
            "token",
            "oauth",
            "jwt",
        ],
        "api" | "endpoint" | "route" => &[
            "api", "endpoint", "route", "handler", "http", "request", "client", "fetch",
        ],
        "database" | "db" | "repository" => {
            &["database", "db", "repository", "storage", "sql", "query"]
        }
        "cache" | "cached" | "memoize" => &["cache", "cached", "memoize", "lru", "redis"],
        "ui" | "component" | "view" | "widget" => {
            &["ui", "component", "view", "widget", "frontend"]
        }
        "service" => &["service", "manager", "provider", "client"],
        "array" | "list" | "slice" | "vec" | "vector" | "sequence" => {
            &["array", "list", "slice", "vec", "vector", "sequence"]
        }
        "map" | "hashmap" | "dictionary" | "dict" | "table" => {
            &["map", "hashmap", "dictionary", "dict", "table", "lookup"]
        }
        "queue" | "deque" | "channel" => &["queue", "deque", "channel", "buffer"],
        "tree" | "trie" | "ast" | "syntax" => &["tree", "trie", "ast", "syntax", "node"],
        "graph" | "dag" | "adjacency" | "edge" | "node" => {
            &["graph", "dag", "adjacency", "edge", "node", "topology"]
        }
        "lock" | "mutex" | "semaphore" | "atomic" | "synchronization" => {
            &["lock", "mutex", "semaphore", "atomic", "synchronization"]
        }
        "error" | "exception" | "failure" | "result" => {
            &["error", "exception", "failure", "result", "retry"]
        }
        _ => &[],
    }
}

fn is_stopword(term: &str) -> bool {
    matches!(
        term,
        "a" | "an"
            | "and"
            | "are"
            | "code"
            | "does"
            | "for"
            | "from"
            | "in"
            | "is"
            | "me"
            | "of"
            | "show"
            | "that"
            | "the"
            | "to"
            | "where"
            | "which"
            | "with"
    )
}

fn escape_tantivy_term(term: &str) -> String {
    term.chars()
        .filter(|character| character.is_alphanumeric() || *character == '_')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Edge;

    fn symbol(id: &str, name: &str, path: &str) -> Symbol {
        Symbol {
            id: id.into(),
            name: name.into(),
            file_path: path.into(),
            kind: "Function".into(),
            ..Default::default()
        }
    }

    #[test]
    fn understands_behavioral_auth_call_query() {
        let intent = analyze_query("code where authentication service calls api");
        assert_eq!(intent.relation, Some(EdgeType::Calls));
        assert!(intent.subject_terms.contains(&"authentication".to_string()));
        assert!(intent.object_terms.contains(&"api".to_string()));
        assert!(intent.expanded_terms.contains(&"oauth".to_string()));
        assert!(intent.expanded_terms.contains(&"endpoint".to_string()));
        assert_eq!(
            intent.canonical,
            analyze_query("auth service invokes endpoint").canonical
        );
    }

    #[test]
    fn direct_call_relationship_boosts_caller() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(directory.path()).unwrap();
        let caller = symbol("auth", "AuthenticationService", "src/auth/service.rs");
        let target = symbol("api", "requestApi", "src/api/client.rs");
        storage.save_symbol(&caller).unwrap();
        storage.save_symbol(&target).unwrap();
        storage
            .save_edge(&Edge {
                src: caller.id.clone(),
                dst: target.id.clone(),
                edge_type: EdgeType::Calls,
                tags: vec!["stack-graphs".into()],
                ..Default::default()
            })
            .unwrap();
        let intent = analyze_query("authentication service calls api");
        assert!(relation_evidence_boost(&storage, &caller, &intent).unwrap() > 0.5);
    }
}
