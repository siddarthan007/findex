use crate::skeleton::pagerank::compute_pagerank;
use crate::storage::{EdgeType, Storage, StorageError, Symbol};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

/// Resolves a reference edge to its most likely definition symbol using proximity and PageRank.
pub fn resolve_definition(
    ref_name: &str,
    src_symbol_id: &str,
    storage: &Storage,
) -> Result<Option<Symbol>, StorageError> {
    let mut prefix = None;
    let mut leaf = ref_name;

    if ref_name.contains('.') {
        if let Some(idx) = ref_name.rfind('.') {
            prefix = Some(&ref_name[..idx]);
            leaf = &ref_name[idx + 1..];
        }
    } else if ref_name.contains("::") {
        if let Some(idx) = ref_name.rfind("::") {
            prefix = Some(&ref_name[..idx]);
            leaf = &ref_name[idx + 2..];
        }
    }

    let candidates = storage.get_symbols_by_name(leaf)?;
    if candidates.is_empty() {
        return Ok(None);
    }
    if candidates.len() == 1 {
        return Ok(Some(candidates[0].clone()));
    }

    // Determine the source file for proximity scoring.
    let src_file = storage
        .get_symbol(src_symbol_id)
        .ok()
        .flatten()
        .map(|s| s.file_path)
        .unwrap_or_else(|| {
            src_symbol_id
                .split('#')
                .next()
                .unwrap_or(src_symbol_id)
                .to_string()
        });
    let src_path = Path::new(&src_file);
    let src_dir = src_path.parent();

    // PageRank is only needed when there are multiple candidates.
    let all_symbols = storage.list_symbols()?;
    let pageranks = compute_pagerank(&all_symbols, &storage.list_edges()?);

    let mut ranked = Vec::new();
    for cand in &candidates {
        let mut score = 0;
        let cand_path = Path::new(&cand.file_path);

        if cand_path == src_path {
            score += 100;
        } else if let (Some(sd), Some(cd)) = (src_dir, cand_path.parent()) {
            if sd == cd {
                score += 50;
            }
        }

        // Boost score if the parent symbol name matches the reference prefix.
        if let Some(pref) = prefix {
            let last_pref_part = pref
                .rsplit('.')
                .next()
                .unwrap_or(pref)
                .rsplit("::")
                .next()
                .unwrap_or(pref);
            if let Some(ref p_id) = cand.parent_id {
                let parent_name = storage
                    .get_symbol(p_id)?
                    .map(|parent| parent.name)
                    .unwrap_or_else(|| {
                        p_id.rsplit('#')
                            .next()
                            .unwrap_or(p_id)
                            .split(":L")
                            .next()
                            .unwrap_or(p_id)
                            .to_string()
                    });
                if parent_name == last_pref_part {
                    score += 200;
                }
            }
        }

        let pr = pageranks.get(&cand.id).copied().unwrap_or(0.0);
        let final_score = score as f32 + pr * 10.0;
        ranked.push((cand, final_score));
    }

    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(Some(ranked[0].0.clone()))
}

/// Finds all symbols referencing a definition symbol.
pub fn resolve_references(
    def_symbol_id: &str,
    storage: &Storage,
) -> Result<Vec<Symbol>, StorageError> {
    let def_sym = match storage.get_symbol(def_symbol_id)? {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };

    let candidate_edges = storage.get_edges_by_dst(&def_sym.name)?;
    let mut refs = HashSet::new();

    for edge in candidate_edges {
        if let EdgeType::Calls | EdgeType::References = edge.edge_type {
            if let Some(resolved) = resolve_definition(&edge.dst, &edge.src, storage)? {
                if resolved.id == def_symbol_id {
                    if let Some(src_sym) = storage.get_symbol(&edge.src)? {
                        refs.insert(src_sym);
                    }
                }
            }
        }
    }

    Ok(refs.into_iter().collect())
}

/// Locate all direct callers of a function symbol ID.
pub fn get_callers(symbol_id: &str, storage: &Storage) -> Result<Vec<Symbol>, StorageError> {
    let sym = match storage.get_symbol(symbol_id)? {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };

    let incoming = storage.get_edges_by_dst(&sym.name)?;
    let mut callers = HashSet::new();
    for edge in incoming {
        if edge.edge_type == EdgeType::Calls {
            if let Some(resolved) = resolve_definition(&edge.dst, &edge.src, storage)? {
                if resolved.id == symbol_id {
                    if let Some(src_sym) = storage.get_symbol(&edge.src)? {
                        callers.insert(src_sym);
                    }
                }
            }
        }
    }
    Ok(callers.into_iter().collect())
}

/// Locate all direct callees of a function symbol ID.
pub fn get_callees(symbol_id: &str, storage: &Storage) -> Result<Vec<Symbol>, StorageError> {
    if storage.get_symbol(symbol_id)?.is_none() {
        return Ok(Vec::new());
    }

    let outgoing = storage.get_edges_by_src(symbol_id)?;
    let mut callees = Vec::new();
    let mut seen = HashSet::new();
    for edge in outgoing {
        if edge.edge_type == EdgeType::Calls {
            if let Some(resolved) = resolve_definition(&edge.dst, symbol_id, storage)? {
                if seen.insert(resolved.id.clone()) {
                    callees.push(resolved);
                }
            }
        }
    }
    Ok(callees)
}

/// Perform a BFS graph expansion around a symbol using structural edges.
pub fn expand_context(
    symbol_id: &str,
    depth: u32,
    storage: &Storage,
) -> Result<Vec<Symbol>, StorageError> {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut result = Vec::new();

    if let Some(sym) = storage.get_symbol(symbol_id)? {
        let id = sym.id.clone();
        visited.insert(id.clone());
        queue.push_back((id, 0u32));
        result.push(sym);
    }

    while let Some((current_id, current_depth)) = queue.pop_front() {
        if current_depth >= depth {
            continue;
        }

        let edges = storage.get_edges_by_src(&current_id)?;
        for edge in edges {
            let neighbor_id = if edge.edge_type == EdgeType::Contains {
                &edge.dst
            } else {
                // For call/reference edges the dst is a name; resolve it to a symbol id.
                match resolve_definition(&edge.dst, &current_id, storage)? {
                    Some(sym) => {
                        if visited.insert(sym.id.clone()) {
                            result.push(sym.clone());
                            queue.push_back((sym.id, current_depth + 1));
                        }
                        continue;
                    }
                    None => continue,
                }
            };

            if visited.insert(neighbor_id.clone()) {
                if let Some(sym) = storage.get_symbol(neighbor_id)? {
                    result.push(sym.clone());
                    queue.push_back((neighbor_id.clone(), current_depth + 1));
                }
            }
        }

        // Expand incoming edges as well, so context includes direct callers
        // and containers in addition to callees and children.
        let current = match storage.get_symbol(&current_id)? {
            Some(symbol) => symbol,
            None => continue,
        };
        let mut incoming = storage.get_edges_by_dst(&current_id)?;
        incoming.extend(storage.get_edges_by_dst(&current.name)?);

        for edge in incoming {
            let targets_current = if edge.dst == current_id {
                true
            } else {
                resolve_definition(&edge.dst, &edge.src, storage)?
                    .map(|resolved| resolved.id == current_id)
                    .unwrap_or(false)
            };
            if !targets_current || !visited.insert(edge.src.clone()) {
                continue;
            }
            if let Some(source) = storage.get_symbol(&edge.src)? {
                let source_id = source.id.clone();
                result.push(source);
                queue.push_back((source_id, current_depth + 1));
            }
        }
    }

    Ok(result)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RankedContextNeighbor {
    pub symbol: Symbol,
    pub score: f32,
    pub hops: u32,
    pub relation: EdgeType,
    pub direction: String,
    pub evidence: String,
}

/// Bounded, provenance-aware graph expansion for retrieval. Typed edges,
/// exact Stack Graph evidence, and execution traces outrank ambiguous parser
/// references; a logarithmic degree penalty prevents God nodes from flooding
/// every result set.
pub fn expand_context_ranked(
    symbol_id: &str,
    depth: u32,
    max_nodes: usize,
    storage: &Storage,
) -> Result<Vec<RankedContextNeighbor>, StorageError> {
    let depth = depth.min(4);
    let max_nodes = max_nodes.clamp(1, 512);
    let mut best: HashMap<String, RankedContextNeighbor> = HashMap::new();
    let mut queue = VecDeque::from([(symbol_id.to_string(), 0_u32, 1.0_f32)]);
    let mut expanded = HashSet::new();

    while let Some((current_id, hops, current_score)) = queue.pop_front() {
        if hops >= depth || !expanded.insert((current_id.clone(), hops)) {
            continue;
        }
        let Some(current) = storage.get_symbol(&current_id)? else {
            continue;
        };
        let mut neighbors = Vec::new();

        for edge in storage.get_edges_by_src(&current_id)? {
            let target = if edge.edge_type == EdgeType::Contains {
                storage.get_symbol(&edge.dst)?
            } else {
                match storage.get_symbol(&edge.dst)? {
                    Some(symbol) => Some(symbol),
                    None => resolve_definition(&edge.dst, &current_id, storage)?,
                }
            };
            if let Some(target) = target {
                neighbors.push((target, edge, "outgoing"));
            }
        }

        let mut incoming = storage.get_edges_by_dst(&current_id)?;
        incoming.extend(storage.get_edges_by_dst(&current.name)?);
        for edge in incoming {
            let targets_current = edge.dst == current_id
                || resolve_definition(&edge.dst, &edge.src, storage)?
                    .is_some_and(|resolved| resolved.id == current_id);
            if targets_current {
                if let Some(source) = storage.get_symbol(&edge.src)? {
                    neighbors.push((source, edge, "incoming"));
                }
            }
        }

        neighbors.sort_by(|left, right| {
            edge_priority(&right.1)
                .partial_cmp(&edge_priority(&left.1))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        neighbors.truncate(96);
        for (neighbor, edge, direction) in neighbors {
            if neighbor.id == symbol_id {
                continue;
            }
            let degree = storage.get_edges_by_src(&neighbor.id)?.len()
                + storage.get_edges_by_dst(&neighbor.id)?.len()
                + storage.get_edges_by_dst(&neighbor.name)?.len();
            let degree_penalty = 1.0 / (1.0 + (degree as f32).ln_1p() * 0.14);
            let score = current_score * 0.72 * edge_priority(&edge) * degree_penalty;
            let next_hops = hops + 1;
            let candidate = RankedContextNeighbor {
                symbol: neighbor.clone(),
                score,
                hops: next_hops,
                relation: edge.edge_type,
                direction: direction.to_string(),
                evidence: edge_evidence(&edge),
            };
            let should_replace = best
                .get(&neighbor.id)
                .map(|existing| candidate.score > existing.score)
                .unwrap_or(true);
            if should_replace {
                best.insert(neighbor.id.clone(), candidate);
            }
            if next_hops < depth && best.len() < max_nodes.saturating_mul(2) {
                queue.push_back((neighbor.id, next_hops, score));
            }
        }
    }

    let mut ranked: Vec<_> = best.into_values().collect();
    ranked.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.symbol.id.cmp(&right.symbol.id))
    });
    ranked.truncate(max_nodes);
    Ok(ranked)
}

fn edge_priority(edge: &crate::storage::Edge) -> f32 {
    let typed: f32 = match edge.edge_type {
        EdgeType::Inherits => 1.0,
        EdgeType::Calls => 0.96,
        EdgeType::Defines => 0.94,
        EdgeType::Imports => 0.86,
        EdgeType::References => 0.78,
        EdgeType::Contains => 0.74,
    };
    let evidence: f32 =
        if edge.trace_id.is_some() || edge.tags.iter().any(|tag| tag == "execution-trace") {
            1.12
        } else if edge.tags.iter().any(|tag| tag == "stack-graphs") {
            1.08
        } else {
            1.0
        };
    (typed * evidence).min(1.1_f32)
}

fn edge_evidence(edge: &crate::storage::Edge) -> String {
    if let Some(trace_id) = edge.trace_id.as_deref() {
        format!("execution_trace:{trace_id}")
    } else if edge.tags.iter().any(|tag| tag == "stack-graphs") {
        "stack_graph".to_string()
    } else if edge.tags.is_empty() {
        "ast_extracted".to_string()
    } else {
        edge.tags.join(",")
    }
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
            signature: format!("fn {}", name),
            file_path: file_path.to_string(),
            start_line: 1,
            start_col: 1,
            end_line: 10,
            end_col: 1,
            docstring: None,
            ..Default::default()
        }
    }

    #[test]
    fn test_name_resolution_and_expansion() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path().join("db")).unwrap();

        let sym_a = make_sym("src/main.rs#main", "main", "src/main.rs");
        let sym_b = make_sym("src/lib.rs#run", "run", "src/lib.rs");
        storage.save_symbol(&sym_a).unwrap();
        storage.save_symbol(&sym_b).unwrap();

        // Edge representing call from main -> run
        let edge = Edge {
            src: "src/main.rs#main".to_string(),
            dst: "run".to_string(),
            edge_type: EdgeType::Calls,
            ..Default::default()
        };
        storage.save_edge(&edge).unwrap();

        // Verify resolution
        let resolved = resolve_definition("run", "src/main.rs#main", &storage)
            .unwrap()
            .unwrap();
        assert_eq!(resolved.id, "src/lib.rs#run");

        // Verify callers and callees
        let callers = get_callers("src/lib.rs#run", &storage).unwrap();
        assert!(callers.iter().any(|s| s.id == "src/main.rs#main"));

        let callees = get_callees("src/main.rs#main", &storage).unwrap();
        assert!(callees.iter().any(|s| s.id == "src/lib.rs#run"));

        // Verify expansion
        let expanded = expand_context("src/main.rs#main", 1, &storage).unwrap();
        assert!(expanded.iter().any(|s| s.id == "src/lib.rs#run"));
    }

    #[test]
    fn ranked_expansion_preserves_relation_and_trace_evidence() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path().join("db")).unwrap();
        let root = make_sym("src/main.rs#main", "main", "src/main.rs");
        let traced = make_sym("src/trace.rs#hot", "hot", "src/trace.rs");
        let plain = make_sym("src/plain.rs#cold", "cold", "src/plain.rs");
        for symbol in [&root, &traced, &plain] {
            storage.save_symbol(symbol).unwrap();
        }
        storage
            .save_edges_batch(&[
                Edge {
                    src: root.id.clone(),
                    dst: traced.id.clone(),
                    edge_type: EdgeType::Calls,
                    trace_id: Some("request-42".to_string()),
                    tags: vec!["execution-trace".to_string()],
                },
                Edge {
                    src: root.id.clone(),
                    dst: plain.id.clone(),
                    edge_type: EdgeType::Calls,
                    ..Edge::default()
                },
            ])
            .unwrap();

        let ranked = expand_context_ranked(&root.id, 1, 8, &storage).unwrap();

        assert_eq!(ranked[0].symbol.id, traced.id);
        assert_eq!(ranked[0].relation, EdgeType::Calls);
        assert_eq!(ranked[0].evidence, "execution_trace:request-42");
        assert_eq!(ranked[0].hops, 1);
    }
}
