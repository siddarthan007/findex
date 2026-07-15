use crate::storage::{Edge, Symbol};
use petgraph::graphmap::DiGraphMap;
use std::collections::HashMap;
use std::path::Path;

/// Aider-style personalization inputs for repository maps.
#[derive(Debug, Clone, Default)]
pub struct PersonalizationConfig {
    /// Symbol IDs or names mentioned in the current task (10x boost).
    pub mentioned_symbols: Vec<String>,
    /// Files currently being edited or discussed (50x boost).
    pub focal_files: Vec<String>,
    /// Boost descriptive identifiers over generic names such as `run`/`main`.
    pub boost_well_named: bool,
}

/// Computes PageRank for the given symbols and edges using the power iteration method.
/// Uses a default damping factor of 0.85 and 20 iterations.
pub fn compute_pagerank(symbols: &[Symbol], edges: &[Edge]) -> HashMap<String, f32> {
    compute_personalized_pagerank(symbols, edges, &PersonalizationConfig::default())
}

pub fn compute_personalized_pagerank(
    symbols: &[Symbol],
    edges: &[Edge],
    personalization: &PersonalizationConfig,
) -> HashMap<String, f32> {
    let mut graph = DiGraphMap::new();

    // 1. Add all symbols as nodes
    for sym in symbols {
        graph.add_node(sym.id.as_str());
    }

    // Map symbol names to their qualified IDs for name-resolution
    let mut name_to_ids: HashMap<&str, Vec<&str>> = HashMap::new();
    for sym in symbols {
        name_to_ids
            .entry(sym.name.as_str())
            .or_default()
            .push(sym.id.as_str());
    }

    // 2. Add edges, resolving bare names to fully qualified symbol IDs where needed.
    for edge in edges {
        if !graph.contains_node(edge.src.as_str()) {
            continue;
        }

        if graph.contains_node(edge.dst.as_str()) {
            // Already a valid symbol ID in the graph (e.g. containment edge or resolved test edge)
            graph.add_edge(edge.src.as_str(), edge.dst.as_str(), ());
        } else {
            // Bare name; resolve it to candidate symbol IDs using heuristic locality rules
            if let Some(candidates) = name_to_ids.get(edge.dst.as_str()) {
                let src_file = edge.src.split('#').next().unwrap_or(edge.src.as_str());
                let src_path = Path::new(src_file);
                let src_dir = src_path.parent();

                // Group candidates by distance/locality
                let mut same_file = Vec::new();
                let mut same_dir = Vec::new();

                for &cand_id in candidates {
                    if !graph.contains_node(cand_id) {
                        continue;
                    }
                    let cand_file = cand_id.split('#').next().unwrap_or(cand_id);
                    let cand_path = Path::new(cand_file);

                    if cand_path == src_path {
                        same_file.push(cand_id);
                    } else if let (Some(sd), Some(cd)) = (src_dir, cand_path.parent()) {
                        if sd == cd {
                            same_dir.push(cand_id);
                        }
                    }
                }

                // Add edges to the most local/relevant candidates
                if !same_file.is_empty() {
                    for cand_id in same_file {
                        graph.add_edge(edge.src.as_str(), cand_id, ());
                    }
                } else if !same_dir.is_empty() {
                    for cand_id in same_dir {
                        graph.add_edge(edge.src.as_str(), cand_id, ());
                    }
                } else {
                    // Fall back to all matching candidates in the repository
                    for &cand_id in candidates {
                        if graph.contains_node(cand_id) {
                            graph.add_edge(edge.src.as_str(), cand_id, ());
                        }
                    }
                }
            }
        }
    }

    let nodes: Vec<&str> = graph.nodes().collect();
    let n = nodes.len();
    if n == 0 {
        return HashMap::new();
    }

    // 3. Build and normalize the personalization/teleport vector.
    let symbols_by_id: HashMap<&str, &Symbol> = symbols
        .iter()
        .map(|symbol| (symbol.id.as_str(), symbol))
        .collect();
    let mentioned: Vec<String> = personalization
        .mentioned_symbols
        .iter()
        .map(|value| value.to_lowercase())
        .collect();
    let mut weights: HashMap<&str, f32> = HashMap::new();
    for &node in &nodes {
        let mut weight = 1.0f32;
        if let Some(symbol) = symbols_by_id.get(node) {
            let id = symbol.id.to_lowercase();
            let name = symbol.name.to_lowercase();
            if mentioned.iter().any(|value| value == &id || value == &name) {
                weight *= 10.0;
            }
            if personalization
                .focal_files
                .iter()
                .any(|file| symbol.file_path == *file || symbol.file_path.ends_with(file))
            {
                weight *= 50.0;
            }
            if personalization.boost_well_named && is_well_named(&symbol.name) {
                weight *= 10.0;
            }
        }
        weights.insert(node, weight);
    }
    let total_weight: f32 = weights.values().sum::<f32>().max(f32::EPSILON);
    let teleport: HashMap<&str, f32> = weights
        .into_iter()
        .map(|(node, weight)| (node, weight / total_weight))
        .collect();
    let mut pagerank = teleport.clone();
    let damping_factor = 0.85;
    let iterations = 20;

    // 4. Power iteration
    for _ in 0..iterations {
        let mut next_pagerank = HashMap::new();
        // Personalized teleport replaces the uniform `(1-d)/N` term.
        for &node in &nodes {
            next_pagerank.insert(node, (1.0 - damping_factor) * teleport[&node]);
        }

        // Account for dangling nodes (out-degree = 0)
        let mut dangling_sum = 0.0;
        for &node in &nodes {
            let out_degree = graph.neighbors(node).count();
            if out_degree == 0 {
                dangling_sum += pagerank[&node];
            }
        }
        let dangling_share = damping_factor * dangling_sum;
        for &node in &nodes {
            *next_pagerank.get_mut(node).unwrap() += dangling_share * teleport[&node];
        }

        // Distribute scores along edges
        for &node in &nodes {
            let neighbors: Vec<&str> = graph.neighbors(node).collect();
            let out_degree = neighbors.len();
            if out_degree > 0 {
                let share = (damping_factor * pagerank[&node]) / out_degree as f32;
                for target in neighbors {
                    *next_pagerank.get_mut(target).unwrap() += share;
                }
            }
        }

        pagerank = next_pagerank;
    }

    // Convert to String keys
    pagerank
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

fn is_well_named(name: &str) -> bool {
    const GENERIC: &[&str] = &[
        "main", "run", "get", "set", "new", "init", "data", "item", "value", "handler",
    ];
    let lower = name.to_lowercase();
    if name.len() < 8 || GENERIC.contains(&lower.as_str()) {
        return false;
    }
    name.contains('_')
        || name
            .chars()
            .zip(name.chars().skip(1))
            .any(|(left, right)| left.is_lowercase() && right.is_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::EdgeType;

    #[test]
    fn test_pagerank_computation() {
        let symbols = vec![
            Symbol {
                id: "sym_a".to_string(),
                name: "A".to_string(),
                kind: "Function".to_string(),
                signature: "fn A()".to_string(),
                file_path: "main.rs".to_string(),
                start_line: 1,
                start_col: 1,
                end_line: 1,
                end_col: 1,
                docstring: None,
                ..Default::default()
            },
            Symbol {
                id: "sym_b".to_string(),
                name: "B".to_string(),
                kind: "Function".to_string(),
                signature: "fn B()".to_string(),
                file_path: "main.rs".to_string(),
                start_line: 2,
                start_col: 1,
                end_line: 2,
                end_col: 1,
                docstring: None,
                ..Default::default()
            },
            Symbol {
                id: "sym_c".to_string(),
                name: "C".to_string(),
                kind: "Function".to_string(),
                signature: "fn C()".to_string(),
                file_path: "main.rs".to_string(),
                start_line: 3,
                start_col: 1,
                end_line: 3,
                end_col: 1,
                docstring: None,
                ..Default::default()
            },
        ];

        // A calls B, B calls C (linear dependency)
        let edges = vec![
            Edge {
                src: "sym_a".to_string(),
                dst: "sym_b".to_string(),
                edge_type: EdgeType::Calls,
                ..Default::default()
            },
            Edge {
                src: "sym_b".to_string(),
                dst: "sym_c".to_string(),
                edge_type: EdgeType::Calls,
                ..Default::default()
            },
        ];

        let pr = compute_pagerank(&symbols, &edges);

        assert_eq!(pr.len(), 3);
        // C has incoming edge from B, B has incoming from A, A has none -> C should have highest PageRank
        assert!(pr["sym_c"] > pr["sym_b"]);
        assert!(pr["sym_b"] > pr["sym_a"]);
    }

    #[test]
    fn test_personalization_boosts_focal_symbol() {
        let symbols = vec![
            Symbol {
                id: "a".into(),
                name: "alpha".into(),
                file_path: "a.rs".into(),
                ..Default::default()
            },
            Symbol {
                id: "b".into(),
                name: "beta".into(),
                file_path: "b.rs".into(),
                ..Default::default()
            },
        ];
        let config = PersonalizationConfig {
            mentioned_symbols: vec!["beta".into()],
            ..Default::default()
        };
        let scores = compute_personalized_pagerank(&symbols, &[], &config);
        assert!(scores["b"] > scores["a"]);
    }
}
