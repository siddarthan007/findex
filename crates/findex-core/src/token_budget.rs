//! Token-budget utilities.
//!
//! Findex avoids a heavy tokenizer dependency on the hot path. The heuristic
//! used here is fast and close enough for context budgeting: split on
//! whitespace, then scale by 1.3 to approximate the token count of typical
//! code-LLM tokenizers.

/// Returns a fast, approximate token count for the given text.
pub fn count_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    // Count whitespace-separated runs of non-whitespace characters.
    let words = text.split_whitespace().count();
    // Approximate token count: words * 1.3, rounded up.
    (words * 13).div_ceil(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_tokens() {
        assert_eq!(count_tokens(""), 0);
        assert_eq!(count_tokens("fn main() {}"), 4);
        assert_eq!(count_tokens("pub fn main() -> Result<()>"), 7);
    }
}
