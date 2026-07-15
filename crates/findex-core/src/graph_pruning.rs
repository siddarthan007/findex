use crate::storage::{Edge, Storage, StorageError, Symbol};
use crate::structural_locality::{predict_context, PredictContextOptions};
use std::collections::{HashMap, HashSet};

/// A subgraph kept within a token budget.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PrunedContext {
    pub symbols: Vec<Symbol>,
    pub edges: Vec<Edge>,
    pub total_tokens: usize,
    pub token_budget: usize,
    pub omitted_symbols: usize,
    pub budget_exhausted: bool,
}

/// Build a token-budget-limited context around `seeds`.
///
/// The algorithm:
/// 1. Ask the structural-locality predictor for related symbols.
/// 2. Sort candidate symbols by descending locality score.
/// 3. Greedily add symbols until `max_tokens` would be exceeded.
/// 4. Return the kept symbols and the edges whose endpoints both survived.
///
/// The seed symbols are always included first; if a seed alone exceeds the
/// budget, the result still contains that seed so callers can decide how to
/// truncate further.
pub fn prune_context(
    storage: &Storage,
    seeds: &[String],
    max_tokens: usize,
) -> Result<PrunedContext, StorageError> {
    let candidate_limit = (max_tokens / 8).clamp(64, 2_000);
    let options = PredictContextOptions {
        max_results: candidate_limit,
        max_nodes_visited: candidate_limit.saturating_mul(2),
        ..PredictContextOptions::default()
    };
    let predictions = predict_context(storage, seeds, &options)?;

    let mut kept_ids = HashSet::new();
    let mut total_tokens = 0usize;
    let mut symbols = Vec::new();

    // Seeds are mandatory and preserve caller order, even when they alone
    // exceed the budget. This keeps pruning from silently dropping the task anchor.
    for id in seeds {
        if !kept_ids.insert(id.clone()) {
            continue;
        }
        if let Some(sym) = storage.get_symbol(id)? {
            total_tokens = total_tokens.saturating_add(symbol_cost(storage, &sym)?);
            symbols.push(sym);
        }
    }
    let mandatory_count = symbols.len();

    let mut candidates = Vec::new();
    for prediction in predictions {
        if kept_ids.contains(&prediction.symbol_id) {
            continue;
        }
        if let Some(symbol) = storage.get_symbol(&prediction.symbol_id)? {
            let cost = symbol_cost(storage, &symbol)?;
            let density = prediction.score / (cost.max(1) as f32).sqrt();
            candidates.push((symbol, cost, prediction.score, density));
        }
    }
    candidates.sort_by(|a, b| {
        b.3.partial_cmp(&a.3)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
    });

    let candidate_count = candidates.len();
    for (symbol, cost, _score, _density) in candidates {
        if total_tokens.saturating_add(cost) <= max_tokens {
            total_tokens += cost;
            kept_ids.insert(symbol.id.clone());
            symbols.push(symbol);
        }
        // Continue after an oversized candidate: a smaller, still-relevant
        // symbol may fit the remaining budget.
    }

    let mut edge_map = HashMap::new();
    for id in &kept_ids {
        for edge in storage.get_edges_by_src(id)? {
            if kept_ids.contains(&edge.dst) {
                edge_map.insert(edge_key(&edge), edge);
            }
        }
    }
    let mut edges: Vec<_> = edge_map.into_values().collect();
    edges.sort_by_key(edge_key);
    let kept_predicted = symbols.len().saturating_sub(mandatory_count);

    Ok(PrunedContext {
        symbols,
        edges,
        total_tokens,
        token_budget: max_tokens,
        omitted_symbols: candidate_count.saturating_sub(kept_predicted),
        budget_exhausted: total_tokens >= max_tokens || candidate_count > kept_predicted,
    })
}

fn symbol_cost(storage: &Storage, symbol: &Symbol) -> Result<usize, StorageError> {
    let chunk_tokens: usize = storage
        .get_chunks_by_symbol(&symbol.id)?
        .iter()
        .map(|chunk| chunk.token_count)
        .sum();
    Ok(chunk_tokens.max(symbol.token_count).max(1))
}

fn edge_key(edge: &Edge) -> String {
    format!("{}\u{1f}{}\u{1f}{:?}", edge.src, edge.dst, edge.edge_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Edge, EdgeType, Storage, Symbol};
    use tempfile::tempdir;

    fn make_sym(id: &str, name: &str, tokens: usize) -> Symbol {
        Symbol {
            id: id.to_string(),
            name: name.to_string(),
            kind: "Function".to_string(),
            signature: name.to_string(),
            file_path: "f".to_string(),
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 1,
            docstring: None,
            token_count: tokens,
            ..Default::default()
        }
    }

    #[test]
    fn test_prune_context_respects_budget() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path().join("db")).unwrap();

        // Seed a=5 tokens, b=4 tokens, c=3 tokens. a calls b, b calls c.
        let a = make_sym("f#a", "a", 5);
        let b = make_sym("f#b", "b", 4);
        let c = make_sym("f#c", "c", 3);
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

        let ctx = prune_context(&storage, &["f#a".to_string()], 9).unwrap();
        // a (5) + b (4) = 9 tokens; c should be pruned.
        assert_eq!(ctx.total_tokens, 9);
        assert_eq!(ctx.symbols.len(), 2);
        assert!(ctx.symbols.iter().any(|s| s.id == "f#a"));
        assert!(ctx.symbols.iter().any(|s| s.id == "f#b"));
        assert!(!ctx.symbols.iter().any(|s| s.id == "f#c"));

        // Only the edge between surviving endpoints should remain.
        assert_eq!(ctx.edges.len(), 1);
        assert_eq!(ctx.edges[0].dst, "f#b");
    }

    #[test]
    fn test_prune_context_keeps_all_when_budget_is_large() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path().join("db")).unwrap();

        let a = make_sym("f#a", "a", 1);
        let b = make_sym("f#b", "b", 1);
        let c = make_sym("f#c", "c", 1);
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

        let ctx = prune_context(&storage, &["f#a".to_string()], 100).unwrap();
        assert_eq!(ctx.symbols.len(), 3);
        assert_eq!(ctx.edges.len(), 2);
    }

    #[test]
    fn skips_oversized_candidates_and_keeps_smaller_relevant_symbols() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path().join("db")).unwrap();
        storage
            .save_symbols_batch(&[
                make_sym("f#a", "a", 5),
                make_sym("f#large", "large", 20),
                make_sym("f#small", "small", 3),
            ])
            .unwrap();
        storage
            .save_edges_batch(&[
                Edge {
                    src: "f#a".into(),
                    dst: "f#large".into(),
                    edge_type: EdgeType::Calls,
                    ..Default::default()
                },
                Edge {
                    src: "f#a".into(),
                    dst: "f#small".into(),
                    edge_type: EdgeType::References,
                    ..Default::default()
                },
            ])
            .unwrap();

        let context = prune_context(&storage, &["f#a".to_string()], 8).unwrap();

        assert!(context.symbols.iter().any(|symbol| symbol.id == "f#a"));
        assert!(context.symbols.iter().any(|symbol| symbol.id == "f#small"));
        assert!(!context.symbols.iter().any(|symbol| symbol.id == "f#large"));
        assert_eq!(context.total_tokens, 8);
    }

    #[test]
    fn always_keeps_every_seed() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path().join("db")).unwrap();
        storage
            .save_symbols_batch(&[make_sym("a", "a", 10), make_sym("b", "b", 10)])
            .unwrap();

        let context = prune_context(&storage, &["a".into(), "b".into()], 5).unwrap();

        assert_eq!(context.symbols.len(), 2);
        assert_eq!(context.total_tokens, 20);
        assert!(context.budget_exhausted);
    }
}
