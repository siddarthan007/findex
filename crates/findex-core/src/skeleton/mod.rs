pub mod pagerank;

use crate::storage::Symbol;
use crate::token_budget::count_tokens;
use std::collections::HashMap;

/// Generates an Aider-style elided code skeleton from symbols, sorted by PageRank, fitting within a token budget.
pub fn generate_skeleton(
    symbols: &[Symbol],
    pageranks: &HashMap<String, f32>,
    token_budget: usize,
) -> String {
    render_skeleton(symbols, token_budget, |a: &Symbol, b: &Symbol| {
        let pr_a = pageranks.get(&a.id).copied().unwrap_or(0.0);
        let pr_b = pageranks.get(&b.id).copied().unwrap_or(0.0);
        pr_b.partial_cmp(&pr_a).unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// Generate a file-local skeleton in source order.
pub fn get_file_skeleton(symbols: &[Symbol], token_budget: usize) -> String {
    render_skeleton(symbols, token_budget, |a, b| {
        a.start_line.cmp(&b.start_line)
    })
}

fn render_skeleton<F>(symbols: &[Symbol], token_budget: usize, mut sort: F) -> String
where
    F: FnMut(&Symbol, &Symbol) -> std::cmp::Ordering,
{
    let mut selected_symbols = Vec::new();

    // 1. Sort symbols according to the caller's ordering
    let mut sorted_symbols = symbols.to_vec();
    sorted_symbols.sort_by(|a, b| sort(a, b));

    // 2. Select symbols within budget
    for sym in sorted_symbols {
        let mut trial = selected_symbols.clone();
        trial.push(sym.clone());
        if count_tokens(&render_selected(&trial)) <= token_budget {
            selected_symbols.push(sym);
        }
    }

    let mut skeleton = render_selected(&selected_symbols);

    // Show a summary only if it also fits the requested budget.
    if selected_symbols.len() < symbols.len() {
        let summary = format!(
            "# ... (elided {} minor symbols to fit budget of {} tokens)\n",
            symbols.len() - selected_symbols.len(),
            token_budget
        );
        if count_tokens(&format!("{}{}", skeleton, summary)) <= token_budget {
            skeleton.push_str(&summary);
        }
    }

    skeleton
}

fn render_selected(selected_symbols: &[Symbol]) -> String {
    let mut file_groups: HashMap<String, Vec<&Symbol>> = HashMap::new();
    for sym in selected_symbols {
        file_groups
            .entry(sym.file_path.clone())
            .or_default()
            .push(sym);
    }

    let mut skeleton = String::new();

    // Sort files by name for deterministic ordering
    let mut files: Vec<&String> = file_groups.keys().collect();
    files.sort();

    for file_path in files {
        skeleton.push_str(&format!("# file: {}\n", file_path));

        let file_syms = &file_groups[file_path];
        // Sort symbols within the file by their start line to match original source layout
        let mut sorted_file_syms = file_syms.clone();
        sorted_file_syms.sort_by_key(|s| s.start_line);

        for sym in sorted_file_syms {
            let elision_suffix = match sym.kind.as_str() {
                "Function" | "Method" | "Impl" | "Class" | "Struct" | "Enum" | "Interface" => {
                    " { ... }"
                }
                _ => "",
            };
            let sig = if !elision_suffix.is_empty() {
                sym.signature.trim_end_matches('{').trim().to_string()
            } else {
                sym.signature.clone()
            };
            skeleton.push_str(&format!("  {}{}\n", sig, elision_suffix));
        }
        skeleton.push('\n');
    }

    skeleton
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skeleton_generation() {
        let symbols = vec![
            Symbol {
                id: "src/main.rs#main".to_string(),
                name: "main".to_string(),
                kind: "Function".to_string(),
                signature: "fn main()".to_string(),
                file_path: "src/main.rs".to_string(),
                start_line: 10,
                start_col: 1,
                end_line: 12,
                end_col: 1,
                docstring: None,
                ..Default::default()
            },
            Symbol {
                id: "src/main.rs#Config".to_string(),
                name: "Config".to_string(),
                kind: "Struct".to_string(),
                signature: "struct Config".to_string(),
                file_path: "src/main.rs".to_string(),
                start_line: 1,
                start_col: 1,
                end_line: 5,
                end_col: 1,
                docstring: None,
                ..Default::default()
            },
        ];

        let mut prs = HashMap::new();
        prs.insert("src/main.rs#main".to_string(), 0.8);
        prs.insert("src/main.rs#Config".to_string(), 0.2);

        let skeleton = generate_skeleton(&symbols, &prs, 100);

        let expected = "# file: src/main.rs\n  struct Config { ... }\n  fn main() { ... }\n\n";
        assert_eq!(skeleton, expected);
    }
}
