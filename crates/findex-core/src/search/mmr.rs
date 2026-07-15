use std::collections::HashSet;

use crate::storage::Symbol;

/// Select a diverse result set with Maximal Marginal Relevance (MMR).
///
/// Relevance scores are normalized per candidate set. Similarity is a cheap,
/// deterministic lexical/location proxy so the diversity pass stays off the
/// embedding hot path and works in lexical-only mode too.
pub fn mmr_diversify(
    candidates: &[(Symbol, f32)],
    limit: usize,
    lambda: f32,
) -> Vec<(Symbol, f32)> {
    if candidates.is_empty() || limit == 0 {
        return Vec::new();
    }

    let lambda = lambda.clamp(0.0, 1.0);
    let max_score = candidates
        .iter()
        .map(|(_, score)| *score)
        .fold(f32::NEG_INFINITY, f32::max);
    let relevance_scale = max_score.abs().max(f32::EPSILON);

    let mut remaining: Vec<usize> = (0..candidates.len()).collect();
    let mut selected: Vec<usize> = Vec::with_capacity(limit.min(candidates.len()));

    while !remaining.is_empty() && selected.len() < limit {
        let mut best_position = 0usize;
        let mut best_mmr = f32::NEG_INFINITY;

        for (position, &candidate_idx) in remaining.iter().enumerate() {
            let (symbol, score) = &candidates[candidate_idx];
            let relevance = (*score / relevance_scale).max(0.0);
            let redundancy = selected
                .iter()
                .map(|&selected_idx| symbol_similarity(symbol, &candidates[selected_idx].0))
                .fold(0.0f32, f32::max);
            let mmr = lambda * relevance - (1.0 - lambda) * redundancy;

            if mmr > best_mmr {
                best_mmr = mmr;
                best_position = position;
            }
        }

        selected.push(remaining.remove(best_position));
    }

    selected
        .into_iter()
        .map(|idx| candidates[idx].clone())
        .collect()
}

fn symbol_similarity(a: &Symbol, b: &Symbol) -> f32 {
    if a.id == b.id {
        return 1.0;
    }

    let a_tokens = tokens(a);
    let b_tokens = tokens(b);
    let intersection = a_tokens.intersection(&b_tokens).count() as f32;
    let union = a_tokens.union(&b_tokens).count() as f32;
    let lexical = if union > 0.0 {
        intersection / union
    } else {
        0.0
    };

    let same_file = if a.file_path == b.file_path { 0.2 } else { 0.0 };
    let overlaps =
        if a.file_path == b.file_path && a.start_line <= b.end_line && b.start_line <= a.end_line {
            0.5
        } else {
            0.0
        };

    (lexical + same_file + overlaps).min(1.0)
}

fn tokens(symbol: &Symbol) -> HashSet<String> {
    format!("{} {} {}", symbol.name, symbol.kind, symbol.signature)
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|part| !part.is_empty())
        .map(|part| part.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol(id: &str, name: &str, file: &str, score_line: usize) -> Symbol {
        Symbol {
            id: id.to_string(),
            name: name.to_string(),
            kind: "Function".to_string(),
            signature: format!("fn {}()", name),
            file_path: file.to_string(),
            start_line: score_line,
            end_line: score_line + 2,
            ..Default::default()
        }
    }

    #[test]
    fn prefers_diverse_candidates() {
        let candidates = vec![
            (symbol("a", "load_user", "user.rs", 1), 1.0),
            (symbol("b", "load_user_cached", "user.rs", 2), 0.99),
            (symbol("c", "validate_token", "auth.rs", 20), 0.8),
        ];

        let selected = mmr_diversify(&candidates, 2, 0.6);
        assert_eq!(selected[0].0.id, "a");
        assert_eq!(selected[1].0.id, "c");
    }
}
