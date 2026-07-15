use crate::storage::{Chunk, Symbol};
use crate::token_budget::count_tokens;

/// Split a symbol body at code-shaped boundaries while preserving exact
/// parent and line-range metadata. Small symbols remain a single document.
pub fn chunk_symbol(symbol: &Symbol, body: &str, max_tokens: usize) -> Vec<Chunk> {
    let max_tokens = max_tokens.max(32);
    if count_tokens(body) <= max_tokens {
        return vec![make_chunk(symbol, body, 0, 0, body.lines().count())];
    }

    let lines: Vec<&str> = body.lines().collect();
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;

    while start < lines.len() {
        let mut end = start;
        let mut last_boundary = None;
        while end < lines.len() {
            end += 1;
            let text = lines[start..end].join("\n");
            let tokens = count_tokens(&text);
            if is_boundary(lines[end - 1]) && tokens >= max_tokens / 2 {
                last_boundary = Some(end);
            }
            if tokens >= max_tokens {
                end = last_boundary
                    .filter(|boundary| *boundary > start)
                    .unwrap_or(end);
                break;
            }
        }

        let text = lines[start..end].join("\n");
        chunks.push(make_chunk(symbol, &text, index, start, end));
        index += 1;
        start = end;
    }

    chunks
}

fn make_chunk(
    symbol: &Symbol,
    text: &str,
    chunk_index: usize,
    line_offset: usize,
    line_end_offset: usize,
) -> Chunk {
    let id = if chunk_index == 0
        && line_offset == 0
        && line_end_offset >= symbol.end_line.saturating_sub(symbol.start_line)
    {
        symbol.id.clone()
    } else {
        format!("{}::chunk:{}", symbol.id, chunk_index)
    };
    Chunk {
        id,
        parent_symbol_id: symbol.id.clone(),
        file_path: symbol.file_path.clone(),
        chunk_index,
        start_line: symbol.start_line + line_offset,
        end_line: (symbol.start_line + line_end_offset.saturating_sub(1)).min(symbol.end_line),
        text: text.to_string(),
        token_count: count_tokens(text),
    }
}

fn is_boundary(line: &str) -> bool {
    let line = line.trim();
    line.is_empty()
        || line.ends_with('}')
        || line.ends_with(';')
        || line.starts_with("match ")
        || line.starts_with("if ")
        || line.starts_with("for ")
        || line.starts_with("while ")
        || line.starts_with("try:")
        || line.starts_with("except ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_large_symbols_and_preserves_ranges() {
        let symbol = Symbol {
            id: "src/lib.rs#large:L10C1".into(),
            file_path: "src/lib.rs".into(),
            start_line: 10,
            end_line: 209,
            ..Default::default()
        };
        let body = (0..200)
            .map(|line| format!("let value_{} = compute({});", line, line))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunk_symbol(&symbol, &body, 64);
        assert!(chunks.len() > 1);
        assert_eq!(chunks[0].parent_symbol_id, symbol.id);
        assert_eq!(chunks[0].start_line, 10);
        assert!(chunks
            .windows(2)
            .all(|pair| pair[0].end_line + 1 == pair[1].start_line));
    }
}
