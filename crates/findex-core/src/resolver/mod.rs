use crate::skeleton::pagerank::compute_pagerank;
use crate::storage::{EdgeType, Storage, StorageError, Symbol};
use std::collections::{HashSet, VecDeque};
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
}
