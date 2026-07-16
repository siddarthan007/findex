use std::collections::HashMap;

/// Combines the results of lexical and vector search using Reciprocal Rank Fusion (RRF).
/// RRF score formula: score(doc) = sum_{m in models} (1 / (60 + rank_m(doc)))
pub fn rrf_merge(lexical: &[String], vector: &[String], limit: usize) -> Vec<(String, f32)> {
    rrf_merge_rankings(&[lexical, vector], limit)
}

/// Fuse any number of independent rankings. Keeping raw lexical, normalized
/// code terms, and semantic similarity as separate legs prevents one noisy
/// natural-language query from dominating every retrieval signal.
pub fn rrf_merge_rankings(rankings: &[&[String]], limit: usize) -> Vec<(String, f32)> {
    let mut scores = HashMap::new();
    for ranking in rankings {
        for (index, id) in ranking.iter().enumerate() {
            let rank = index + 1;
            let rrf_score = 1.0 / (60.0 + rank as f32);
            *scores.entry(id.clone()).or_insert(0.0) += rrf_score;
        }
    }

    // Convert to vector and sort descending by combined RRF score
    let mut fused: Vec<(String, f32)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    fused.truncate(limit);
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rrf_merge() {
        let lexical = vec![
            "doc_a".to_string(),
            "doc_b".to_string(),
            "doc_c".to_string(),
        ];

        let vector = vec![
            "doc_b".to_string(),
            "doc_d".to_string(),
            "doc_a".to_string(),
        ];

        let results = rrf_merge(&lexical, &vector, 2);

        assert_eq!(results.len(), 2);
        // doc_b ranks 2nd in lexical (1/62) and 1st in vector (1/61) -> highest total score
        assert_eq!(results[0].0, "doc_b");
        // doc_a ranks 1st in lexical (1/61) and 3rd in vector (1/63) -> next highest
        assert_eq!(results[1].0, "doc_a");
    }
}
