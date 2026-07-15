//! Compact, token-aware intelligence products for agents and human UIs.

use crate::search::rerank::Reranker;
use crate::search::vector::Embedder;
use crate::storage::{EdgeType, Storage, Symbol};
use crate::structural_locality::{predict_context, PredictContextOptions};
use crate::token_budget::count_tokens;
use crate::{get_codebase_skeleton, search_codebase_with_components, IngestionError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextItem {
    pub score: f32,
    pub symbol: Symbol,
    pub source: String,
    pub tokens: usize,
    /// Why this range was selected (lexical/semantic anchor or structural
    /// neighbor), so callers can audit retrieval decisions.
    pub reason: String,
    pub graph_hops: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureOverview {
    pub files: usize,
    pub symbols: usize,
    pub edges: usize,
    pub languages: BTreeMap<String, usize>,
    pub layers: BTreeMap<String, usize>,
    pub symbol_kinds: BTreeMap<String, usize>,
    pub entrypoints: Vec<ArchitectureSymbol>,
    pub contracts: Vec<ArchitectureSymbol>,
    pub hubs: Vec<ArchitectureHub>,
    pub cross_file_edges: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureSymbol {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureHub {
    pub symbol: ArchitectureSymbol,
    pub incoming: usize,
    pub outgoing: usize,
}

/// Produce a source-free architectural digest. It is intentionally based on
/// indexed roles and relationships rather than file contents, making it cheap
/// enough to use as an agent's first orientation call.
pub fn architecture_overview(storage: &Storage) -> Result<ArchitectureOverview, IngestionError> {
    let files = storage.list_files()?;
    let symbols = storage.list_symbols()?;
    let edges = storage.list_edges()?;
    let mut languages = BTreeMap::new();
    let mut layers = BTreeMap::new();
    let mut symbol_kinds = BTreeMap::new();
    for file in &files {
        *languages
            .entry(language_for_path(&file.path.to_string_lossy()).to_string())
            .or_default() += 1;
        *layers
            .entry(layer_for_path(&file.path.to_string_lossy()).to_string())
            .or_default() += 1;
    }
    for symbol in &symbols {
        *symbol_kinds.entry(symbol.kind.clone()).or_default() += 1;
    }

    let by_id: HashMap<_, _> = symbols
        .iter()
        .map(|symbol| (symbol.id.as_str(), symbol))
        .collect();
    let mut incoming: HashMap<&str, usize> = HashMap::new();
    let mut outgoing: HashMap<&str, usize> = HashMap::new();
    let mut cross_file_edges = 0;
    for edge in &edges {
        *incoming.entry(edge.dst.as_str()).or_default() += 1;
        *outgoing.entry(edge.src.as_str()).or_default() += 1;
        if let (Some(src), Some(dst)) = (by_id.get(edge.src.as_str()), by_id.get(edge.dst.as_str()))
        {
            if normalized_path_key(&src.file_path) != normalized_path_key(&dst.file_path) {
                cross_file_edges += 1;
            }
        }
    }

    let to_summary = |symbol: &Symbol| ArchitectureSymbol {
        id: symbol.id.clone(),
        name: symbol.name.clone(),
        kind: symbol.kind.clone(),
        file_path: symbol.file_path.clone(),
        line: symbol.start_line,
    };
    let mut entrypoints: Vec<_> = symbols
        .iter()
        .filter(|symbol| is_entrypoint(symbol))
        .map(to_summary)
        .collect();
    entrypoints.sort_by(|left, right| left.file_path.cmp(&right.file_path));
    entrypoints.truncate(32);

    let contract_kinds = ["Interface", "Trait", "Protocol", "Mixin", "Annotation"];
    let mut contracts: Vec<_> = symbols
        .iter()
        .filter(|symbol| contract_kinds.contains(&symbol.kind.as_str()))
        .map(to_summary)
        .collect();
    contracts.sort_by(|left, right| left.file_path.cmp(&right.file_path));
    contracts.truncate(64);

    let mut hubs: Vec<_> = symbols
        .iter()
        .map(|symbol| ArchitectureHub {
            symbol: to_summary(symbol),
            incoming: incoming.get(symbol.id.as_str()).copied().unwrap_or(0),
            outgoing: outgoing.get(symbol.id.as_str()).copied().unwrap_or(0),
        })
        .filter(|hub| hub.incoming + hub.outgoing > 0)
        .collect();
    hubs.sort_by_key(|hub| std::cmp::Reverse(hub.incoming + hub.outgoing));
    hubs.truncate(32);

    Ok(ArchitectureOverview {
        files: files.len(),
        symbols: symbols.len(),
        edges: edges.len(),
        languages,
        layers,
        symbol_kinds,
        entrypoints,
        contracts,
        hubs,
        cross_file_edges,
    })
}

fn language_for_path(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => "Rust",
        "py" | "pyi" => "Python",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "ts" | "tsx" | "mts" | "cts" => "TypeScript",
        "vue" => "Vue",
        "dart" => "Dart",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "C++",
        "go" => "Go",
        "java" => "Java",
        "cs" => "C#",
        "rb" => "Ruby",
        "php" | "phtml" => "PHP",
        "swift" => "Swift",
        "html" | "htm" => "HTML",
        "css" | "scss" | "sass" | "less" => "CSS",
        _ => "Other",
    }
}

fn layer_for_path(path: &str) -> &'static str {
    let path = normalized_path_key(path);
    if path.contains("/test") || path.contains("/spec") || path.contains("__tests__") {
        "tests"
    } else if path.contains("/ui/")
        || path.contains("/components/")
        || path.contains("/views/")
        || path.contains("/frontend/")
    {
        "ui"
    } else if path.contains("/api/")
        || path.contains("/routes/")
        || path.contains("/controllers/")
        || path.contains("/handlers/")
    {
        "api"
    } else if path.contains("/domain/") || path.contains("/models/") {
        "domain"
    } else if path.contains("/data/")
        || path.contains("/storage/")
        || path.contains("/repository/")
        || path.contains("/db/")
    {
        "data"
    } else if path.contains("/scripts/") || path.contains("/tools/") || path.contains("/build/") {
        "tooling"
    } else {
        "core"
    }
}

fn is_entrypoint(symbol: &Symbol) -> bool {
    let name = symbol.name.to_ascii_lowercase();
    matches!(name.as_str(), "main" | "app" | "application" | "server")
        || symbol.file_path.ends_with("main.rs")
        || symbol.file_path.ends_with("main.py")
        || symbol.file_path.ends_with("index.ts")
        || symbol.file_path.ends_with("index.js")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBundle {
    pub query: String,
    pub mode: String,
    pub token_budget: usize,
    pub tokens_used: usize,
    pub candidate_tokens_avoided: usize,
    pub repo_map: String,
    pub items: Vec<ContextItem>,
}

/// A single bounded retrieval product that replaces the common agent loop of
/// search -> open whole file -> search again. Source ranges are exact and the
/// returned payload never intentionally exceeds `token_budget`.
pub fn build_context_bundle<P: AsRef<Path>>(
    db_path: P,
    storage: &Storage,
    query: &str,
    mode: &str,
    token_budget: usize,
    reranker: Option<&dyn Reranker>,
    embedder: &dyn Embedder,
) -> Result<ContextBundle, IngestionError> {
    let token_budget = token_budget.clamp(128, 32_768);
    let map_budget = (token_budget / 4).clamp(64, 1024);
    let repo_map = get_codebase_skeleton(storage, map_budget)?;
    let mut used = count_tokens(&repo_map);
    let search_candidates =
        search_codebase_with_components(db_path, storage, query, mode, reranker, embedder, 40)?;

    let seed_ids: Vec<_> = search_candidates
        .iter()
        .take(5)
        .map(|(symbol, _)| symbol.id.clone())
        .collect();
    let predicted = predict_context(
        storage,
        &seed_ids,
        &PredictContextOptions {
            max_hops: 2,
            max_results: 32,
            max_nodes_visited: 512,
            max_neighbors_per_node: 96,
            ..PredictContextOptions::default()
        },
    )?;

    let mut candidates: Vec<(Symbol, f32, String, Option<u32>)> = search_candidates
        .into_iter()
        .map(|(symbol, score)| (symbol, score, format!("{mode} retrieval anchor"), None))
        .collect();
    let mut positions: HashMap<String, usize> = candidates
        .iter()
        .enumerate()
        .map(|(index, (symbol, _, _, _))| (symbol.id.clone(), index))
        .collect();
    for prediction in predicted {
        if let Some(index) = positions.get(&prediction.symbol_id).copied() {
            candidates[index].1 = candidates[index].1.max(prediction.score);
            candidates[index].2.push_str(" + structural locality");
            candidates[index].3 = Some(prediction.source_hops);
        } else if let Some(symbol) = storage.get_symbol(&prediction.symbol_id)? {
            positions.insert(symbol.id.clone(), candidates.len());
            candidates.push((
                symbol,
                prediction.score * 0.9,
                "structural locality from a top retrieval anchor".to_string(),
                Some(prediction.source_hops),
            ));
        }
    }
    candidates.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.file_path.cmp(&right.0.file_path))
    });

    let mut candidate_tokens = 0;
    let mut retained_tokens = 0;
    let mut items = Vec::new();
    for (symbol, score, reason, graph_hops) in candidates {
        let source = read_line_range(
            Path::new(&symbol.file_path),
            symbol.start_line,
            symbol.end_line,
        )
        .unwrap_or_else(|_| symbol.signature.clone());
        let tokens = count_tokens(&source);
        candidate_tokens += tokens;
        if used + tokens > token_budget {
            continue;
        }
        used += tokens;
        retained_tokens += tokens;
        items.push(ContextItem {
            score,
            symbol,
            source,
            tokens,
            reason,
            graph_hops,
        });
    }

    Ok(ContextBundle {
        query: query.to_string(),
        mode: mode.to_string(),
        token_budget,
        tokens_used: used,
        candidate_tokens_avoided: candidate_tokens.saturating_sub(retained_tokens),
        repo_map,
        items,
    })
}

fn read_line_range(path: &Path, start: usize, end: usize) -> std::io::Result<String> {
    let reader = BufReader::new(File::open(path)?);
    let mut output = String::new();
    for (index, line) in reader.lines().enumerate() {
        let number = index + 1;
        if number > end {
            break;
        }
        if number >= start {
            output.push_str(&line?);
            output.push('\n');
        }
    }
    Ok(output)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactReport {
    pub symbol: Symbol,
    pub incoming_edges: usize,
    pub outgoing_edges: usize,
    pub risk_score: f32,
    pub god_node: bool,
    pub affected_files: Vec<String>,
    pub callers: Vec<Symbol>,
    pub callees: Vec<Symbol>,
    pub references: Vec<Symbol>,
}

pub fn impact_analysis(storage: &Storage, symbol_id: &str) -> Result<ImpactReport, IngestionError> {
    let symbol = storage.get_symbol(symbol_id)?.ok_or_else(|| {
        crate::IngestionError::InvalidRequest(format!("unknown symbol id: {symbol_id}"))
    })?;
    let incoming = storage.get_edges_by_dst(symbol_id)?;
    let outgoing = storage.get_edges_by_src(symbol_id)?;
    let callers = crate::resolver::get_callers(symbol_id, storage)?;
    let callees = crate::resolver::get_callees(symbol_id, storage)?;
    let references = crate::resolver::resolve_references(symbol_id, storage)?;
    let mut files = BTreeSet::new();
    files.insert(symbol.file_path.clone());
    for related in callers
        .iter()
        .chain(callees.iter())
        .chain(references.iter())
    {
        files.insert(related.file_path.clone());
    }
    let degree = incoming.len() + outgoing.len();
    let fan_in = incoming.len() as f32;
    let risk_score = ((degree as f32).ln_1p() * 18.0 + fan_in.sqrt() * 8.0).min(100.0);
    Ok(ImpactReport {
        symbol,
        incoming_edges: incoming.len(),
        outgoing_edges: outgoing.len(),
        risk_score,
        god_node: degree >= 20 || incoming.len() >= 12,
        affected_files: files.into_iter().collect(),
        callers,
        callees,
        references,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstOutline {
    pub file_path: String,
    pub roots: Vec<AstNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstNode {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub start_line: usize,
    pub end_line: usize,
    pub children: Vec<AstNode>,
}

pub fn ast_outline(storage: &Storage, path: &Path) -> Result<AstOutline, IngestionError> {
    let mut symbols = storage.get_symbols_by_file(path)?;
    if symbols.is_empty() {
        let requested = normalized_path_key(&path.to_string_lossy());
        symbols = storage
            .list_symbols()?
            .into_iter()
            .filter(|symbol| normalized_path_key(&symbol.file_path) == requested)
            .collect();
    }
    let indexed_path = symbols
        .first()
        .map(|symbol| symbol.file_path.clone())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    let mut children: HashMap<Option<String>, Vec<Symbol>> = HashMap::new();
    for symbol in symbols {
        children
            .entry(symbol.parent_id.clone())
            .or_default()
            .push(symbol);
    }
    for group in children.values_mut() {
        group.sort_by_key(|symbol| (symbol.start_line, symbol.start_col));
    }
    fn build(symbol: Symbol, groups: &HashMap<Option<String>, Vec<Symbol>>) -> AstNode {
        let nested = groups
            .get(&Some(symbol.id.clone()))
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|child| build(child, groups))
            .collect();
        AstNode {
            id: symbol.id,
            name: symbol.name,
            kind: symbol.kind,
            signature: symbol.signature,
            start_line: symbol.start_line,
            end_line: symbol.end_line,
            children: nested,
        }
    }
    let roots = children
        .get(&None)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|symbol| build(symbol, &children))
        .collect();
    Ok(AstOutline {
        file_path: indexed_path,
        roots,
    })
}

fn normalized_path_key(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let without_dot = normalized.trim_start_matches("./");
    if cfg!(windows) {
        without_dot.to_ascii_lowercase()
    } else {
        without_dot.to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphSnapshot {
    pub nodes: Vec<GraphNode>,
    pub links: Vec<GraphLink>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub degree: usize,
    pub category: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphLink {
    pub source: String,
    pub target: String,
    pub kind: EdgeType,
}

pub fn graph_snapshot(storage: &Storage, limit: usize) -> Result<GraphSnapshot, IngestionError> {
    let symbols = storage.list_symbols()?;
    let edges = storage.list_edges()?;
    let mut degrees: HashMap<String, usize> = HashMap::new();
    for edge in &edges {
        *degrees.entry(edge.src.clone()).or_default() += 1;
        *degrees.entry(edge.dst.clone()).or_default() += 1;
    }
    let symbol_ids: BTreeSet<_> = symbols.iter().map(|symbol| symbol.id.clone()).collect();
    let mut ranked = symbols;
    ranked.sort_by_key(|symbol| std::cmp::Reverse(*degrees.get(symbol.id.as_str()).unwrap_or(&0)));
    let node_limit = limit.clamp(1, 10_000);
    let mut ranked_edges: Vec<_> = edges
        .iter()
        .filter(|edge| symbol_ids.contains(&edge.src) && symbol_ids.contains(&edge.dst))
        .collect();
    ranked_edges.sort_by_key(|edge| {
        std::cmp::Reverse(
            degrees.get(edge.src.as_str()).copied().unwrap_or(0)
                + degrees.get(edge.dst.as_str()).copied().unwrap_or(0),
        )
    });

    // Seed the snapshot with high-value connected pairs. Filling with top-degree
    // nodes alone can produce a visually useless cloud with zero visible links.
    let mut included = BTreeSet::new();
    for edge in &ranked_edges {
        let needed =
            usize::from(!included.contains(&edge.src)) + usize::from(!included.contains(&edge.dst));
        if needed > 0 && included.len() + needed <= node_limit {
            included.insert(edge.src.clone());
            included.insert(edge.dst.clone());
        }
        if included.len() >= node_limit {
            break;
        }
    }
    for symbol in &ranked {
        if included.len() >= node_limit {
            break;
        }
        included.insert(symbol.id.clone());
    }
    ranked.retain(|symbol| included.contains(&symbol.id));
    ranked.truncate(node_limit);

    let nodes = ranked
        .into_iter()
        .map(|symbol| {
            let degree = *degrees.get(symbol.id.as_str()).unwrap_or(&0);
            let lower_path = symbol.file_path.to_ascii_lowercase();
            let lower_kind = symbol.kind.to_ascii_lowercase();
            let category = if degree >= 20 {
                "god"
            } else if lower_kind.contains("component")
                || lower_kind.contains("widget")
                || lower_path.ends_with(".vue")
                || lower_path.ends_with(".tsx")
                || lower_path.ends_with(".dart")
            {
                "ui"
            } else if lower_path.contains("/api/")
                || lower_path.contains("\\api\\")
                || lower_kind.contains("endpoint")
                || lower_kind.contains("handler")
            {
                "api"
            } else {
                "code"
            };
            GraphNode {
                id: symbol.id,
                name: symbol.name,
                kind: symbol.kind,
                file_path: symbol.file_path,
                degree,
                category: category.to_string(),
            }
        })
        .collect();
    let edge_limit = node_limit.saturating_mul(8).clamp(1, 50_000);
    let eligible_links: Vec<_> = ranked_edges
        .into_iter()
        .filter(|edge| included.contains(&edge.src) && included.contains(&edge.dst))
        .collect();
    let truncated = symbol_ids.len() > node_limit || eligible_links.len() > edge_limit;
    let links = eligible_links
        .into_iter()
        .take(edge_limit)
        .map(|edge| GraphLink {
            source: edge.src.clone(),
            target: edge.dst.clone(),
            kind: edge.edge_type,
        })
        .collect();
    Ok(GraphSnapshot {
        nodes,
        links,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Edge;

    fn symbol(id: &str, file_path: &str, parent_id: Option<&str>) -> Symbol {
        Symbol {
            id: id.to_string(),
            name: id.to_string(),
            kind: "Function".to_string(),
            signature: format!("fn {id}()"),
            file_path: file_path.to_string(),
            start_line: 1,
            end_line: 2,
            parent_id: parent_id.map(str::to_string),
            ..Symbol::default()
        }
    }

    #[test]
    fn ast_outline_accepts_equivalent_path_separators_and_dot_prefix() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(directory.path()).unwrap();
        storage
            .save_symbol(&symbol("root", r".\src\nested.rs", None))
            .unwrap();

        let outline = ast_outline(&storage, Path::new("src/nested.rs")).unwrap();

        assert_eq!(outline.roots.len(), 1);
        assert_eq!(outline.file_path, r".\src\nested.rs");
    }

    #[test]
    fn graph_snapshot_prioritizes_connected_nodes() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(directory.path()).unwrap();
        for id in ["a", "b", "c"] {
            storage
                .save_symbol(&symbol(id, "src/lib.rs", None))
                .unwrap();
        }
        storage
            .save_edges_batch(&[
                Edge {
                    src: "a".to_string(),
                    dst: "b".to_string(),
                    edge_type: EdgeType::Calls,
                    ..Edge::default()
                },
                Edge {
                    src: "a".to_string(),
                    dst: "c".to_string(),
                    edge_type: EdgeType::References,
                    ..Edge::default()
                },
            ])
            .unwrap();

        let snapshot = graph_snapshot(&storage, 2).unwrap();

        assert_eq!(snapshot.nodes.len(), 2);
        assert!(!snapshot.links.is_empty());
    }
}
