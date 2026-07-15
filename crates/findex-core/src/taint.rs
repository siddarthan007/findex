use crate::storage::{Edge, EdgeType, Storage, StorageError};
use std::collections::{HashMap, HashSet, VecDeque};

/// A taint label attached to a symbol or edge.
pub type TaintLabel = String;

/// Configuration controlling how taint propagates through the graph.
#[derive(Debug, Clone)]
pub struct TaintConfig {
    pub max_hops: u32,
    /// Hard cap on distinct symbols retained by a propagation pass.
    pub max_nodes: usize,
    /// Hard cap on distinct adjacency edges inspected by a pass.
    pub max_edges: usize,
    /// Per-symbol fan-out cap protecting against generated hub nodes.
    pub max_neighbors_per_node: usize,
    /// Bound adversarial seed sets from multiplying labels without limit.
    pub max_labels_per_symbol: usize,
    /// Which edge types allow taint to flow. Omitted edge types block flow.
    pub traversable_edges: HashSet<EdgeType>,
}

impl Default for TaintConfig {
    fn default() -> Self {
        let mut traversable_edges = HashSet::new();
        traversable_edges.insert(EdgeType::Calls);
        traversable_edges.insert(EdgeType::References);
        traversable_edges.insert(EdgeType::Contains);
        traversable_edges.insert(EdgeType::Imports);
        Self {
            max_hops: 4,
            max_nodes: 10_000,
            max_edges: 50_000,
            max_neighbors_per_node: 512,
            max_labels_per_symbol: 64,
            traversable_edges,
        }
    }
}

/// Result of a taint-propagation pass.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TaintResult {
    /// symbol_id -> set of taint labels.
    pub tainted_symbols: HashMap<String, HashSet<TaintLabel>>,
    /// Edges that carried taint during propagation.
    pub tainted_edges: Vec<Edge>,
    pub unknown_seeds: Vec<String>,
    pub truncated: bool,
    pub inspected_edges: usize,
}

/// Propagate taint labels from the given seed symbols through the graph.
///
/// The propagation is bidirectional: taint can flow from a source to its
/// callers, callees, parents, and children.
pub fn propagate_taint(
    storage: &Storage,
    seeds: &[(String, TaintLabel)],
    config: &TaintConfig,
) -> Result<TaintResult, StorageError> {
    let mut labels: HashMap<String, HashSet<TaintLabel>> = HashMap::new();
    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    let mut processed_labels: HashMap<String, HashSet<TaintLabel>> = HashMap::new();
    let mut unknown_seeds = Vec::new();
    let mut truncated = false;

    for (symbol_id, label) in seeds {
        if storage.get_symbol(symbol_id)?.is_none() {
            unknown_seeds.push(symbol_id.clone());
            continue;
        }
        if labels.len() >= config.max_nodes && !labels.contains_key(symbol_id) {
            truncated = true;
            continue;
        }
        let symbol_labels = labels.entry(symbol_id.clone()).or_default();
        if symbol_labels.len() >= config.max_labels_per_symbol {
            truncated = true;
        } else if symbol_labels.insert(label.clone()) {
            queue.push_back((symbol_id.clone(), 0));
        }
    }

    let mut carried_edges: HashMap<String, Edge> = HashMap::new();
    let mut inspected_edge_keys = HashSet::new();
    while let Some((id, hops)) = queue.pop_front() {
        if hops >= config.max_hops {
            continue;
        }
        let already_processed = processed_labels.entry(id.clone()).or_default();
        let pending_labels: Vec<_> = labels
            .get(&id)
            .into_iter()
            .flatten()
            .filter(|label| !already_processed.contains(*label))
            .cloned()
            .collect();
        already_processed.extend(pending_labels.iter().cloned());
        if pending_labels.is_empty() {
            continue;
        }

        let mut adjacent = storage.get_edges_by_src(&id)?;
        adjacent.extend(storage.get_edges_by_dst(&id)?);
        adjacent.sort_by_key(edge_key);
        adjacent.dedup_by(|left, right| edge_key(left) == edge_key(right));
        if adjacent.len() > config.max_neighbors_per_node {
            adjacent.truncate(config.max_neighbors_per_node);
            truncated = true;
        }

        for edge in adjacent {
            if !config.traversable_edges.contains(&edge.edge_type) {
                continue;
            }
            let key = edge_key(&edge);
            if inspected_edge_keys.insert(key.clone())
                && inspected_edge_keys.len() > config.max_edges
            {
                inspected_edge_keys.remove(&key);
                truncated = true;
                break;
            }
            let neighbor = if edge.src == id {
                edge.dst.clone()
            } else {
                edge.src.clone()
            };
            if !labels.contains_key(&neighbor) && labels.len() >= config.max_nodes {
                truncated = true;
                continue;
            }

            let neighbor_labels = labels.entry(neighbor.clone()).or_default();
            let available = config
                .max_labels_per_symbol
                .saturating_sub(neighbor_labels.len());
            let mut changed = false;
            for label in pending_labels.iter().take(available) {
                changed |= neighbor_labels.insert(label.clone());
            }
            if available < pending_labels.len() {
                truncated = true;
            }
            if changed {
                carried_edges.entry(key).or_insert(edge);
                queue.push_back((neighbor, hops + 1));
            }
        }
    }

    let mut tainted_edges: Vec<Edge> = carried_edges.into_values().collect();
    tainted_edges.sort_by_key(edge_key);

    Ok(TaintResult {
        tainted_symbols: labels,
        tainted_edges,
        unknown_seeds,
        truncated,
        inspected_edges: inspected_edge_keys.len(),
    })
}

/// Persist taint labels on the edges that carried them.
///
/// Each tainted edge is rewritten with tags derived from the union of labels
/// on its endpoints, prefixed with `taint:`. This is the "pinning" step that
/// attaches the analysis result to the adjacency list.
pub fn pin_taint(storage: &Storage, result: &TaintResult) -> Result<(), StorageError> {
    for edge in &result.tainted_edges {
        let mut updated = edge.clone();
        let mut new_tags: HashSet<String> = HashSet::new();
        if let Some(src_labels) = result.tainted_symbols.get(&edge.src) {
            for label in src_labels {
                new_tags.insert(format!("taint:{}", label));
            }
        }
        if let Some(dst_labels) = result.tainted_symbols.get(&edge.dst) {
            for label in dst_labels {
                new_tags.insert(format!("taint:{}", label));
            }
        }
        for tag in new_tags {
            if !updated.tags.contains(&tag) {
                updated.tags.push(tag);
            }
        }
        storage.save_edge(&updated)?;
    }
    Ok(())
}

/// Pin an execution trace to the adjacency list.
///
/// The trace is a sequence of symbol ids observed at runtime. Consecutive
/// pairs are stored as `References` edges tagged with `execution-trace` and
/// the provided `trace_id`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExecutionTraceReport {
    pub trace_id: String,
    pub edges_pinned: usize,
    pub unknown_symbols: Vec<String>,
    pub truncated: bool,
}

pub fn pin_execution_trace(
    storage: &Storage,
    trace_id: &str,
    path: &[String],
) -> Result<ExecutionTraceReport, StorageError> {
    const MAX_TRACE_STEPS: usize = 10_000;

    let mut unknown_symbols = Vec::new();
    let mut known = Vec::with_capacity(path.len().min(MAX_TRACE_STEPS + 1));
    for symbol_id in path.iter().take(MAX_TRACE_STEPS + 1) {
        if storage.get_symbol(symbol_id)?.is_some() {
            known.push(Some(symbol_id.clone()));
        } else {
            unknown_symbols.push(symbol_id.clone());
            known.push(None);
        }
    }

    let mut edges_pinned = 0;
    for window in known.windows(2).take(MAX_TRACE_STEPS) {
        let (Some(src), Some(dst)) = (&window[0], &window[1]) else {
            continue;
        };
        let src = src.clone();
        let dst = dst.clone();
        let key = format!("{}:{}:{:?}", src, dst, EdgeType::References);

        let mut edge = if let Some(existing) = storage.get_edge_by_key(&key)? {
            existing
        } else {
            Edge {
                src,
                dst,
                edge_type: EdgeType::References,
                tags: vec!["execution-trace".to_string()],
                trace_id: None,
            }
        };

        edge.trace_id = Some(trace_id.to_string());
        if !edge.tags.contains(&"execution-trace".to_string()) {
            edge.tags.push("execution-trace".to_string());
        }
        let trace_tag = format!("trace:{trace_id}");
        if !edge.tags.contains(&trace_tag) {
            edge.tags.push(trace_tag);
        }
        storage.save_edge(&edge)?;
        edges_pinned += 1;
    }
    Ok(ExecutionTraceReport {
        trace_id: trace_id.to_string(),
        edges_pinned,
        unknown_symbols,
        truncated: path.len() > MAX_TRACE_STEPS + 1,
    })
}

fn edge_key(edge: &Edge) -> String {
    format!("{}:{}:{:?}", edge.src, edge.dst, edge.edge_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Storage, Symbol};
    use tempfile::tempdir;

    fn make_sym(id: &str, name: &str) -> Symbol {
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
            ..Default::default()
        }
    }

    #[test]
    fn test_taint_propagates_through_call_edge() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path().join("db")).unwrap();

        let a = make_sym("f#a", "a");
        let b = make_sym("f#b", "b");
        storage.save_symbols_batch(&[a, b]).unwrap();
        storage
            .save_edges_batch(&[Edge {
                src: "f#a".to_string(),
                dst: "f#b".to_string(),
                edge_type: EdgeType::Calls,
                tags: vec![],
                trace_id: None,
            }])
            .unwrap();

        let config = TaintConfig::default();
        let result = propagate_taint(
            &storage,
            &[("f#a".to_string(), "user-input".to_string())],
            &config,
        )
        .unwrap();

        assert!(result.tainted_symbols.contains_key("f#a"));
        assert!(result.tainted_symbols.contains_key("f#b"));
        assert_eq!(result.tainted_edges.len(), 1);

        pin_taint(&storage, &result).unwrap();
        let edges = storage.list_edges().unwrap();
        assert!(edges[0].tags.iter().any(|t| t == "taint:user-input"));
    }

    #[test]
    fn test_execution_trace_pinning() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path().join("db")).unwrap();
        storage
            .save_symbols_batch(&[
                make_sym("f#entry", "entry"),
                make_sym("f#mid", "mid"),
                make_sym("f#sink", "sink"),
            ])
            .unwrap();

        let first = pin_execution_trace(
            &storage,
            "req-42",
            &[
                "f#entry".to_string(),
                "f#mid".to_string(),
                "f#sink".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(first.edges_pinned, 2);
        assert!(first.unknown_symbols.is_empty());

        let second = pin_execution_trace(
            &storage,
            "req-43",
            &["f#entry".to_string(), "f#mid".to_string()],
        )
        .unwrap();
        assert_eq!(second.edges_pinned, 1);

        let edges = storage.list_edges().unwrap();
        assert_eq!(edges.len(), 2);
        for edge in &edges {
            assert!(edge.tags.contains(&"execution-trace".to_string()));
            assert!(edge.tags.contains(&"trace:req-42".to_string()));
        }
        let shared = edges.iter().find(|edge| edge.src == "f#entry").unwrap();
        assert_eq!(shared.trace_id.as_deref(), Some("req-43"));
        assert!(shared.tags.contains(&"trace:req-43".to_string()));
    }

    #[test]
    fn unknown_trace_symbols_do_not_create_phantom_edges() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path().join("db")).unwrap();
        storage
            .save_symbols_batch(&[make_sym("f#entry", "entry"), make_sym("f#sink", "sink")])
            .unwrap();

        let report = pin_execution_trace(
            &storage,
            "req-unknown",
            &[
                "f#entry".to_string(),
                "missing".to_string(),
                "f#sink".to_string(),
            ],
        )
        .unwrap();

        assert_eq!(report.unknown_symbols, vec!["missing"]);
        assert_eq!(report.edges_pinned, 0);
        assert!(storage.list_edges().unwrap().is_empty());
    }

    #[test]
    fn propagation_honors_node_and_fanout_caps() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path().join("db")).unwrap();
        let mut symbols = vec![make_sym("root", "root")];
        let mut edges = Vec::new();
        for index in 0..20 {
            let id = format!("node-{index}");
            symbols.push(make_sym(&id, &id));
            edges.push(Edge {
                src: "root".into(),
                dst: id,
                edge_type: EdgeType::Calls,
                ..Default::default()
            });
        }
        storage.save_symbols_batch(&symbols).unwrap();
        storage.save_edges_batch(&edges).unwrap();
        let config = TaintConfig {
            max_nodes: 5,
            max_neighbors_per_node: 8,
            ..Default::default()
        };

        let result = propagate_taint(
            &storage,
            &[("root".to_string(), "input".to_string())],
            &config,
        )
        .unwrap();

        assert!(result.truncated);
        assert!(result.tainted_symbols.len() <= 5);
        assert!(result.tainted_edges.len() <= 4);
    }
}
