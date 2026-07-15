use crate::storage::{EdgeType, Storage, StorageError};
use std::collections::{HashMap, VecDeque};

/// A symbol predicted to be relevant to the current context, along with a
/// structural-locality score.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PredictedSymbol {
    pub symbol_id: String,
    pub score: f32,
    pub source_hops: u32,
}

/// Options for the structural-locality context predictor.
#[derive(Debug, Clone)]
pub struct PredictContextOptions {
    /// Maximum number of graph hops to expand from the seeds.
    pub max_hops: u32,
    /// Score multiplier per hop. A value of 0.7 means third-hop neighbours
    /// contribute ~0.34 of their base edge weight.
    pub decay: f32,
    /// Maximum number of predicted symbols to return (excluding seeds).
    pub max_results: usize,
    /// Weight per edge type when traversing the graph.
    pub edge_weights: HashMap<EdgeType, f32>,
    /// Bonus score for symbols that live in the same file as a seed.
    pub same_file_bonus: f32,
    /// Incoming edges are useful but generally weaker than the direction in
    /// which the program relation was recorded.
    pub reverse_edge_discount: f32,
    /// Hard bound on the graph working set for one prediction request.
    pub max_nodes_visited: usize,
    /// Per-node fan-out bound protecting against generated hubs and God nodes.
    pub max_neighbors_per_node: usize,
}

impl Default for PredictContextOptions {
    fn default() -> Self {
        let mut edge_weights = HashMap::new();
        edge_weights.insert(EdgeType::Calls, 1.0);
        edge_weights.insert(EdgeType::References, 0.6);
        edge_weights.insert(EdgeType::Imports, 0.7);
        edge_weights.insert(EdgeType::Inherits, 0.9);
        edge_weights.insert(EdgeType::Contains, 0.5);
        edge_weights.insert(EdgeType::Defines, 0.8);
        Self {
            max_hops: 2,
            decay: 0.7,
            max_results: 50,
            edge_weights,
            same_file_bonus: 0.15,
            reverse_edge_discount: 0.85,
            max_nodes_visited: 2_000,
            max_neighbors_per_node: 256,
        }
    }
}

/// Predict a set of symbols structurally related to the given seed symbols.
///
/// The graph is treated as undirected for this pre-fetcher: callers and
/// callees are equally likely to be needed next. The score decays with the
/// number of hops so that immediate neighbours dominate the working set.
pub fn predict_context(
    storage: &Storage,
    seeds: &[String],
    options: &PredictContextOptions,
) -> Result<Vec<PredictedSymbol>, StorageError> {
    let mut scores: HashMap<String, f32> = HashMap::new();
    let mut best_hops: HashMap<String, u32> = HashMap::new();
    let mut queue: VecDeque<(String, u32)> = VecDeque::new();

    for seed in seeds {
        let Some(symbol) = storage.get_symbol(seed)? else {
            continue;
        };
        scores.insert(seed.clone(), 1.0);
        best_hops.insert(seed.clone(), 0);
        queue.push_back((seed.clone(), 0));

        // Same-file context is useful for local contracts but should not become
        // a new expansion frontier; that would turn large files into graph floods.
        for related in storage.get_symbols_by_file(std::path::Path::new(&symbol.file_path))? {
            if related.id != *seed {
                merge_score(
                    scores.entry(related.id.clone()).or_insert(0.0),
                    options.same_file_bonus,
                );
                best_hops.entry(related.id).or_insert(1);
            }
        }
    }

    let mut visited = 0usize;
    while let Some((id, hops)) = queue.pop_front() {
        if visited >= options.max_nodes_visited || hops >= options.max_hops {
            continue;
        }
        visited += 1;
        let current_score = *scores.get(&id).unwrap_or(&0.0);
        let mut neighbors = storage
            .get_edges_by_src(&id)?
            .into_iter()
            .map(|edge| (edge.dst, edge.edge_type, 1.0f32))
            .chain(
                storage
                    .get_edges_by_dst(&id)?
                    .into_iter()
                    .map(|edge| (edge.src, edge.edge_type, options.reverse_edge_discount)),
            )
            .filter(|(neighbor, _, _)| neighbor != &id)
            .collect::<Vec<_>>();
        neighbors.sort_by(|a, b| {
            let a_weight = options.edge_weights.get(&a.1).copied().unwrap_or(0.5) * a.2;
            let b_weight = options.edge_weights.get(&b.1).copied().unwrap_or(0.5) * b.2;
            b_weight
                .partial_cmp(&a_weight)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        neighbors.truncate(options.max_neighbors_per_node);

        for (neighbor, edge_type, direction) in neighbors {
            let weight = options.edge_weights.get(&edge_type).copied().unwrap_or(0.5);
            let contribution = current_score * weight * direction * options.decay;
            merge_score(scores.entry(neighbor.clone()).or_insert(0.0), contribution);

            let next_hops = hops + 1;
            if next_hops <= options.max_hops
                && best_hops
                    .get(&neighbor)
                    .is_none_or(|&best| best > next_hops)
                && best_hops.len() < options.max_nodes_visited
            {
                best_hops.insert(neighbor.clone(), next_hops);
                queue.push_back((neighbor, next_hops));
            }
        }
    }

    // Convert to sorted results, dropping the seeds themselves from the ranked
    // list because they are already in the working set.
    let mut results: Vec<PredictedSymbol> = scores
        .into_iter()
        .filter(|(id, _)| !seeds.contains(id))
        .map(|(symbol_id, score)| {
            let source_hops = best_hops.get(&symbol_id).copied().unwrap_or(0);
            PredictedSymbol {
                symbol_id,
                score,
                source_hops,
            }
        })
        .collect();
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(options.max_results);
    Ok(results)
}

fn merge_score(existing: &mut f32, contribution: f32) {
    let contribution = contribution.clamp(0.0, 0.99);
    *existing = 1.0 - (1.0 - existing.clamp(0.0, 1.0)) * (1.0 - contribution);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Edge, EdgeType, Storage, Symbol};
    use tempfile::tempdir;

    fn make_sym(id: &str, name: &str, file_path: &str) -> Symbol {
        Symbol {
            id: id.to_string(),
            name: name.to_string(),
            kind: "Function".to_string(),
            signature: name.to_string(),
            file_path: file_path.to_string(),
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 1,
            docstring: None,
            ..Default::default()
        }
    }

    #[test]
    fn test_predict_context_traverses_edges() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path().join("db")).unwrap();

        let a = make_sym("f#a", "a", "f");
        let b = make_sym("f#b", "b", "f");
        let c = make_sym("f#c", "c", "f");
        storage.save_symbols_batch(&[a, b, c]).unwrap();

        storage
            .save_edges_batch(&[
                Edge {
                    src: "f#a".to_string(),
                    dst: "f#b".to_string(),
                    edge_type: EdgeType::Calls,
                    ..Default::default()
                },
                Edge {
                    src: "f#b".to_string(),
                    dst: "f#c".to_string(),
                    edge_type: EdgeType::Calls,
                    ..Default::default()
                },
            ])
            .unwrap();

        let opts = PredictContextOptions::default();
        let predictions = predict_context(&storage, &["f#a".to_string()], &opts).unwrap();

        // Direct callee b should rank highest, and c should be reachable in 2 hops.
        let ids: Vec<_> = predictions.iter().map(|p| p.symbol_id.clone()).collect();
        assert!(ids.contains(&"f#b".to_string()));
        assert!(ids.contains(&"f#c".to_string()));

        let b_score = predictions
            .iter()
            .find(|p| p.symbol_id == "f#b")
            .unwrap()
            .score;
        let c_score = predictions
            .iter()
            .find(|p| p.symbol_id == "f#c")
            .unwrap()
            .score;
        assert!(b_score > c_score);
    }

    #[test]
    fn test_predict_context_returns_empty_for_unknown_seed() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path().join("db")).unwrap();
        let opts = PredictContextOptions::default();
        let predictions = predict_context(&storage, &["missing#x".to_string()], &opts).unwrap();
        assert!(predictions.is_empty());
    }

    #[test]
    fn predictor_never_leaks_beyond_the_hop_limit() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path().join("db")).unwrap();
        storage
            .save_symbols_batch(&[
                make_sym("a", "a", "a.rs"),
                make_sym("b", "b", "b.rs"),
                make_sym("c", "c", "c.rs"),
                make_sym("d", "d", "d.rs"),
            ])
            .unwrap();
        storage
            .save_edges_batch(&[
                Edge {
                    src: "a".into(),
                    dst: "b".into(),
                    edge_type: EdgeType::Calls,
                    ..Default::default()
                },
                Edge {
                    src: "b".into(),
                    dst: "c".into(),
                    edge_type: EdgeType::Calls,
                    ..Default::default()
                },
                Edge {
                    src: "c".into(),
                    dst: "d".into(),
                    edge_type: EdgeType::Calls,
                    ..Default::default()
                },
            ])
            .unwrap();
        let options = PredictContextOptions {
            max_hops: 2,
            ..Default::default()
        };

        let predictions = predict_context(&storage, &["a".to_string()], &options).unwrap();

        assert!(predictions
            .iter()
            .any(|prediction| prediction.symbol_id == "c"));
        assert!(!predictions
            .iter()
            .any(|prediction| prediction.symbol_id == "d"));
        assert!(predictions
            .iter()
            .all(|prediction| prediction.source_hops <= 2));
    }
}
