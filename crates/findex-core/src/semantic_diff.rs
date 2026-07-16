use crate::parser::ParserError;
use std::collections::{HashMap, VecDeque};
use tree_sitter::{Language, Node, Parser};

const MAX_ALIGNMENT_CELLS: usize = 250_000;
const MAX_REPORTED_CHANGES: usize = 2_048;
const MAX_CHANGE_TEXT_CHARS: usize = 512;

/// A single semantic change between two parse trees.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum SemanticChange {
    Insert {
        node_id: usize,
        label: String,
        text: String,
    },
    Delete {
        node_id: usize,
        label: String,
        text: String,
    },
    Update {
        old_id: usize,
        new_id: usize,
        label: String,
        old_text: String,
        new_text: String,
    },
}

/// The result of a tree-edit-distance semantic diff.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SemanticDiff {
    /// Approximate tree-edit distance. Smaller is more similar.
    pub distance: f32,
    /// Human-readable list of changes detected in the trees.
    pub changes: Vec<SemanticChange>,
    /// True when a linear-memory keyed alignment replaced the quadratic
    /// sequence alignment for a wide sibling list.
    pub bounded_alignment: bool,
    /// True when the change list reached its safety cap.
    pub changes_truncated: bool,
}

/// Diff two source strings for the same language.
///
/// This uses a recursive, ordered tree-edit-distance approximation:
/// nodes are aligned with a sequence-edit-distance over their children,
/// and node-text differences are recorded as `Update` changes.
pub fn diff_code(
    old_code: &str,
    new_code: &str,
    language: &Language,
) -> Result<SemanticDiff, ParserError> {
    if old_code == new_code {
        return Ok(SemanticDiff {
            distance: 0.0,
            changes: Vec::new(),
            bounded_alignment: false,
            changes_truncated: false,
        });
    }
    let old_tree = parse(old_code, language)?;
    let new_tree = parse(new_code, language)?;
    Ok(diff_nodes(
        old_tree.root_node(),
        new_tree.root_node(),
        old_code.as_bytes(),
        new_code.as_bytes(),
    ))
}

fn parse(code: &str, language: &Language) -> Result<tree_sitter::Tree, ParserError> {
    let mut parser = Parser::new();
    parser
        .set_language(language)
        .map_err(|e| ParserError::TreeSitter(format!("failed to set language: {:?}", e)))?;
    parser
        .parse(code, None)
        .ok_or_else(|| ParserError::TreeSitter("failed to parse".to_string()))
}

fn diff_nodes(a: Node, b: Node, old: &[u8], new: &[u8]) -> SemanticDiff {
    if crate::cancellation::is_cancelled() {
        return cancelled_diff();
    }
    let label_a = a.kind();
    let label_b = b.kind();

    if label_a != label_b {
        let changes = vec![delete_change(a, old), insert_change(b, new)];
        return SemanticDiff {
            distance: 1.0,
            changes,
            bounded_alignment: false,
            changes_truncated: false,
        };
    }

    let a_text = a.utf8_text(old).unwrap_or("");
    let b_text = b.utf8_text(new).unwrap_or("");
    if a_text == b_text {
        return SemanticDiff {
            distance: 0.0,
            changes: Vec::new(),
            bounded_alignment: false,
            changes_truncated: false,
        };
    }

    let mut changes = Vec::new();
    let mut distance = 0.0;

    // Report the smallest changed nodes. Recording an update for every parent
    // whose aggregate text changed duplicates payloads (including entire
    // files) and obscures the actionable leaf edit.
    if a.child_count() == 0 && b.child_count() == 0 {
        distance += 0.5;
        changes.push(SemanticChange::Update {
            old_id: a.id(),
            new_id: b.id(),
            label: label_a.to_string(),
            old_text: bounded_node_text(a, old),
            new_text: bounded_node_text(b, new),
        });
    }

    let child_diff = diff_children(a, b, old, new);
    distance += child_diff.distance;
    let mut changes_truncated = child_diff.changes_truncated;
    append_changes(&mut changes, child_diff.changes, &mut changes_truncated);

    SemanticDiff {
        distance,
        changes,
        bounded_alignment: child_diff.bounded_alignment,
        changes_truncated,
    }
}

#[derive(Clone, Copy)]
enum Op {
    Match(usize, usize),
    Delete,
    Insert,
    None,
}

fn diff_children(a: Node, b: Node, old: &[u8], new: &[u8]) -> SemanticDiff {
    let mut a_walker = a.walk();
    let a_children: Vec<Node> = a.children(&mut a_walker).collect();
    let mut b_walker = b.walk();
    let b_children: Vec<Node> = b.children(&mut b_walker).collect();

    let m = a_children.len();
    let n = b_children.len();

    if m == 0 && n == 0 {
        return SemanticDiff {
            distance: 0.0,
            changes: Vec::new(),
            bounded_alignment: false,
            changes_truncated: false,
        };
    }

    if m.saturating_mul(n) > MAX_ALIGNMENT_CELLS {
        return diff_children_keyed(&a_children, &b_children, old, new);
    }

    // Compute cheap pair costs for sequence alignment. Recursively computing
    // every subtree pair here makes a near-identical file diff combinatorial;
    // recurse only into the pairs selected during backtracking below.
    // Sequence-edit distance over children.
    let columns = n + 1;
    let mut dp = vec![0.0f32; (m + 1) * columns];
    let mut ops = vec![Op::None; (m + 1) * columns];

    for i in 1..=m {
        if i % 32 == 0 && crate::cancellation::is_cancelled() {
            return cancelled_diff();
        }
        dp[i * columns] = i as f32;
        ops[i * columns] = Op::Delete;
    }
    for j in 1..=n {
        dp[j] = j as f32;
        ops[j] = Op::Insert;
    }

    for i in 1..=m {
        for j in 1..=n {
            let index = i * columns + j;
            let pair_distance = if a_children[i - 1].kind() != b_children[j - 1].kind() {
                2.1
            } else if a_children[i - 1].utf8_text(old).unwrap_or("")
                == b_children[j - 1].utf8_text(new).unwrap_or("")
            {
                0.0
            } else {
                0.5
            };
            let sub = dp[(i - 1) * columns + j - 1] + pair_distance;
            let del = dp[(i - 1) * columns + j] + 1.0;
            let ins = dp[i * columns + j - 1] + 1.0;

            let best = sub.min(del).min(ins);
            dp[index] = best;
            ops[index] = if best == sub {
                Op::Match(i - 1, j - 1)
            } else if best == del {
                Op::Delete
            } else {
                Op::Insert
            };
        }
    }

    // Backtrack to collect changes.
    let mut changes = Vec::new();
    let mut bounded_alignment = false;
    let mut changes_truncated = false;
    let mut i = m;
    let mut j = n;
    while i > 0 || j > 0 {
        if (i + j).is_multiple_of(32) && crate::cancellation::is_cancelled() {
            return cancelled_diff();
        }
        match ops[i * columns + j] {
            Op::Match(ai, bj) => {
                let sub = diff_nodes(a_children[ai], b_children[bj], old, new);
                bounded_alignment |= sub.bounded_alignment;
                changes_truncated |= sub.changes_truncated;
                append_changes(&mut changes, sub.changes, &mut changes_truncated);
                i -= 1;
                j -= 1;
            }
            Op::Delete => {
                let node = a_children[i - 1];
                push_change(
                    &mut changes,
                    delete_change(node, old),
                    &mut changes_truncated,
                );
                i -= 1;
            }
            Op::Insert => {
                let node = b_children[j - 1];
                push_change(
                    &mut changes,
                    insert_change(node, new),
                    &mut changes_truncated,
                );
                j -= 1;
            }
            Op::None => break,
        }
    }

    SemanticDiff {
        distance: dp[m * columns + n],
        changes,
        bounded_alignment,
        changes_truncated,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct NodeKey {
    kind: String,
    identity: [u8; 16],
}

fn node_key(node: Node, source: &[u8]) -> NodeKey {
    let identity_bytes = node
        .child_by_field_name("name")
        .and_then(|name| name.utf8_text(source).ok())
        .map(str::as_bytes)
        .unwrap_or_else(|| node.utf8_text(source).unwrap_or("").as_bytes());
    let hash = blake3::hash(identity_bytes);
    let mut identity = [0; 16];
    identity.copy_from_slice(&hash.as_bytes()[..16]);
    NodeKey {
        kind: node.kind().to_string(),
        identity,
    }
}

/// Linear-memory alignment for exceptionally wide sibling lists. Named nodes
/// are anchored by `(kind, name)`; anonymous nodes use `(kind, content hash)`.
/// This avoids allocating three O(m*n) matrices on generated or minified code.
fn diff_children_keyed(
    old_children: &[Node],
    new_children: &[Node],
    old: &[u8],
    new: &[u8],
) -> SemanticDiff {
    let mut new_by_key: HashMap<NodeKey, VecDeque<usize>> = HashMap::new();
    for (index, node) in new_children.iter().copied().enumerate() {
        new_by_key
            .entry(node_key(node, new))
            .or_default()
            .push_back(index);
    }

    let mut matched_new = vec![false; new_children.len()];
    let mut distance = 0.0;
    let mut changes = Vec::new();
    let mut changes_truncated = false;
    let mut bounded_alignment = true;

    for old_node in old_children.iter().copied() {
        if crate::cancellation::is_cancelled() {
            return cancelled_diff();
        }
        let matched = new_by_key
            .get_mut(&node_key(old_node, old))
            .and_then(VecDeque::pop_front);
        if let Some(new_index) = matched {
            matched_new[new_index] = true;
            let sub = diff_nodes(old_node, new_children[new_index], old, new);
            distance += sub.distance;
            bounded_alignment |= sub.bounded_alignment;
            changes_truncated |= sub.changes_truncated;
            append_changes(&mut changes, sub.changes, &mut changes_truncated);
        } else {
            distance += 1.0;
            push_change(
                &mut changes,
                delete_change(old_node, old),
                &mut changes_truncated,
            );
        }
    }

    for (index, new_node) in new_children.iter().copied().enumerate() {
        if !matched_new[index] {
            distance += 1.0;
            push_change(
                &mut changes,
                insert_change(new_node, new),
                &mut changes_truncated,
            );
        }
    }

    SemanticDiff {
        distance,
        changes,
        bounded_alignment,
        changes_truncated,
    }
}

fn cancelled_diff() -> SemanticDiff {
    SemanticDiff {
        distance: 0.0,
        changes: Vec::new(),
        bounded_alignment: true,
        changes_truncated: true,
    }
}

fn bounded_node_text(node: Node, source: &[u8]) -> String {
    let text = node.utf8_text(source).unwrap_or("");
    let mut chars = text.chars();
    let mut bounded: String = chars.by_ref().take(MAX_CHANGE_TEXT_CHARS).collect();
    if chars.next().is_some() {
        bounded.push('…');
    }
    bounded
}

fn delete_change(node: Node, source: &[u8]) -> SemanticChange {
    SemanticChange::Delete {
        node_id: node.id(),
        label: node.kind().to_string(),
        text: bounded_node_text(node, source),
    }
}

fn insert_change(node: Node, source: &[u8]) -> SemanticChange {
    SemanticChange::Insert {
        node_id: node.id(),
        label: node.kind().to_string(),
        text: bounded_node_text(node, source),
    }
}

fn push_change(changes: &mut Vec<SemanticChange>, change: SemanticChange, truncated: &mut bool) {
    if changes.len() < MAX_REPORTED_CHANGES {
        changes.push(change);
    } else {
        *truncated = true;
    }
}

fn append_changes(
    changes: &mut Vec<SemanticChange>,
    incoming: Vec<SemanticChange>,
    truncated: &mut bool,
) {
    let remaining = MAX_REPORTED_CHANGES.saturating_sub(changes.len());
    if incoming.len() > remaining {
        *truncated = true;
    }
    changes.extend(incoming.into_iter().take(remaining));
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Language;

    fn rust_language() -> Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    #[test]
    fn test_identical_code_has_zero_distance() {
        let code = "fn main() { let x = 1; }";
        let diff = diff_code(code, code, &rust_language()).unwrap();
        assert_eq!(diff.distance, 0.0);
        assert!(diff.changes.is_empty());
    }

    #[test]
    fn test_simple_change_detected() {
        let old = "fn main() { let x = 1; }";
        let new = "fn main() { let x = 2; }";
        let diff = diff_code(old, new, &rust_language()).unwrap();
        assert!(diff.distance > 0.0);
        assert!(diff
            .changes
            .iter()
            .any(|c| matches!(c, SemanticChange::Update { .. })));
    }

    #[test]
    fn test_inserted_statement() {
        let old = "fn main() { let x = 1; }";
        let new = "fn main() { let x = 1; let y = 2; }";
        let diff = diff_code(old, new, &rust_language()).unwrap();
        assert!(diff.distance > 0.0);
        assert!(diff
            .changes
            .iter()
            .any(|c| matches!(c, SemanticChange::Insert { .. })));
    }

    #[test]
    fn test_large_near_identical_input_is_bounded() {
        let old = (0..250)
            .map(|index| format!("fn value_{index}() -> usize {{ {index} }}"))
            .collect::<Vec<_>>()
            .join("\n");
        let new = old.replace(
            "fn value_125() -> usize { 125 }",
            "fn value_125() -> usize { 999 }",
        );
        let diff = diff_code(&old, &new, &rust_language()).unwrap();
        assert!(diff.distance > 0.0);
        assert!(diff.changes.len() < 10);
    }

    #[test]
    fn wide_generated_code_uses_linear_memory_alignment() {
        let old = (0..1_000)
            .map(|index| format!("fn value_{index}() -> usize {{ {index} }}"))
            .collect::<Vec<_>>()
            .join("\n");
        let new = old.replace(
            "fn value_500() -> usize { 500 }",
            "fn value_500() -> usize { 999 }",
        );

        let diff = diff_code(&old, &new, &rust_language()).unwrap();

        assert!(diff.bounded_alignment);
        assert!(diff.distance > 0.0);
        assert!(!diff.changes.is_empty());
        assert!(diff.changes.len() < 10);
    }

    #[test]
    fn change_payloads_are_bounded() {
        let old = format!("fn removed() {{ {} }}", "a();".repeat(1_000));
        let diff = diff_code(&old, "", &rust_language()).unwrap();
        let longest = diff
            .changes
            .iter()
            .map(|change| match change {
                SemanticChange::Insert { text, .. } | SemanticChange::Delete { text, .. } => {
                    text.chars().count()
                }
                SemanticChange::Update {
                    old_text, new_text, ..
                } => old_text.chars().count().max(new_text.chars().count()),
            })
            .max()
            .unwrap_or(0);
        assert!(longest <= MAX_CHANGE_TEXT_CHARS + 1);
    }
}
