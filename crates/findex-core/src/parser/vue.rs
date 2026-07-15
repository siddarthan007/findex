//! Vue Single-File Component parsing with source-accurate block remapping.

use crate::parser::{js_ts, registry, tree_sitter_impl, ParserError};
use crate::storage::{Edge, EdgeType, Symbol};
use crate::token_budget::count_tokens;
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug)]
struct SfcBlock<'a> {
    kind: &'static str,
    attributes: &'a str,
    content: &'a str,
    line_offset: usize,
}

pub fn parse_vue(path: &Path, content: &str) -> Result<(Vec<Symbol>, Vec<Edge>), ParserError> {
    let file_path = path.to_string_lossy().to_string();
    let component_name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("VueComponent");
    let root_id = format!("{}#{}:L1C1", file_path, component_name);
    let mut root = Symbol {
        id: root_id.clone(),
        name: component_name.to_string(),
        kind: "Component".to_string(),
        signature: format!("component <{}>", component_name),
        file_path: file_path.clone(),
        start_line: 1,
        start_col: 1,
        end_line: content.lines().count().max(1),
        end_col: content.lines().last().map_or(1, |line| line.len() + 1),
        language: "vue".to_string(),
        token_count: count_tokens(component_name),
        qualified_name: Some(component_name.to_string()),
        ..Default::default()
    };

    let mut symbols = Vec::new();
    let mut edges = Vec::new();
    for block in extract_blocks(content) {
        match block.kind {
            "script" => {
                let ext = script_extension(block.attributes);
                let virtual_path = path.with_extension(ext);
                let (block_symbols, block_edges) =
                    js_ts::parse_js_ts(&virtual_path, block.content)?;
                merge_block(
                    &root_id,
                    &file_path,
                    block.line_offset,
                    block_symbols,
                    block_edges,
                    &mut MergeTarget {
                        root: &mut root,
                        symbols: &mut symbols,
                        edges: &mut edges,
                    },
                );
            }
            "style" => {
                // CSS is indexed only when it is actually CSS. Preprocessor
                // blocks remain represented by the SFC root instead of being
                // fed to a grammar that would emit misleading symbols.
                let language = attribute_value(block.attributes, "lang").unwrap_or("css");
                if language.eq_ignore_ascii_case("css") {
                    let virtual_path = path.with_extension("css");
                    if let Some(config) = registry::config_for_extension("css") {
                        let (block_symbols, block_edges) = tree_sitter_impl::parse_with_config(
                            &virtual_path,
                            block.content,
                            config,
                        )?;
                        merge_block(
                            &root_id,
                            &file_path,
                            block.line_offset,
                            block_symbols,
                            block_edges,
                            &mut MergeTarget {
                                root: &mut root,
                                symbols: &mut symbols,
                                edges: &mut edges,
                            },
                        );
                    }
                }
            }
            "template" => {
                let mut seen = HashSet::new();
                for component in template_component_tags(block.content) {
                    if seen.insert(component.clone()) {
                        edges.push(Edge {
                            src: root_id.clone(),
                            dst: component,
                            edge_type: EdgeType::References,
                            tags: vec!["vue-template".to_string()],
                            ..Default::default()
                        });
                    }
                }
            }
            _ => {}
        }
    }

    symbols.insert(0, root);
    Ok((symbols, edges))
}

struct MergeTarget<'a> {
    root: &'a mut Symbol,
    symbols: &'a mut Vec<Symbol>,
    edges: &'a mut Vec<Edge>,
}

fn merge_block(
    root_id: &str,
    original_path: &str,
    line_offset: usize,
    mut block_symbols: Vec<Symbol>,
    mut block_edges: Vec<Edge>,
    target: &mut MergeTarget<'_>,
) {
    let mut ids = HashMap::new();
    for symbol in &block_symbols {
        ids.insert(
            symbol.id.clone(),
            format!(
                "{}#{}:L{}C{}",
                original_path,
                symbol.name,
                symbol.start_line + line_offset,
                symbol.start_col
            ),
        );
    }

    for symbol in &mut block_symbols {
        let old_id = symbol.id.clone();
        symbol.id = ids[&old_id].clone();
        symbol.file_path = original_path.to_string();
        symbol.start_line += line_offset;
        symbol.end_line += line_offset;
        symbol.parent_id = symbol
            .parent_id
            .as_ref()
            .and_then(|id| ids.get(id).cloned())
            .or_else(|| Some(root_id.to_string()));
        symbol.children = symbol
            .children
            .iter()
            .filter_map(|id| ids.get(id).cloned())
            .collect();
        symbol.qualified_name = Some(format!("{}::{}", root_id, symbol.name));
        if symbol.parent_id.as_deref() == Some(root_id) {
            target.root.children.push(symbol.id.clone());
            target.edges.push(Edge {
                src: root_id.to_string(),
                dst: symbol.id.clone(),
                edge_type: EdgeType::Contains,
                tags: vec!["vue-sfc".to_string()],
                ..Default::default()
            });
        }
    }

    for edge in &mut block_edges {
        edge.src = ids
            .get(&edge.src)
            .cloned()
            .unwrap_or_else(|| root_id.to_string());
        if let Some(remapped) = ids.get(&edge.dst) {
            edge.dst = remapped.clone();
        }
        edge.tags.push("vue-sfc".to_string());
    }
    target.symbols.extend(block_symbols);
    target.edges.extend(block_edges);
}

fn extract_blocks(content: &str) -> Vec<SfcBlock<'_>> {
    let lower = content.to_ascii_lowercase();
    let mut blocks = Vec::new();
    for kind in ["template", "script", "style"] {
        let mut offset = 0;
        let open_pattern = format!("<{}", kind);
        let close_pattern = format!("</{}>", kind);
        while let Some(relative_open) = lower[offset..].find(&open_pattern) {
            let open = offset + relative_open;
            let Some(relative_gt) = lower[open..].find('>') else {
                break;
            };
            let content_start = open + relative_gt + 1;
            let Some(relative_close) = lower[content_start..].find(&close_pattern) else {
                break;
            };
            let content_end = content_start + relative_close;
            let attributes_start = open + open_pattern.len();
            blocks.push(SfcBlock {
                kind,
                attributes: &content[attributes_start..content_start - 1],
                content: &content[content_start..content_end],
                line_offset: content[..content_start]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count(),
            });
            offset = content_end + close_pattern.len();
        }
    }
    blocks.sort_by_key(|block| block.line_offset);
    blocks
}

fn script_extension(attributes: &str) -> &'static str {
    match attribute_value(attributes, "lang").map(str::to_ascii_lowercase) {
        Some(lang) if lang == "ts" || lang == "typescript" => "ts",
        Some(lang) if lang == "tsx" => "tsx",
        Some(lang) if lang == "jsx" => "jsx",
        _ => "js",
    }
}

fn attribute_value<'a>(attributes: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = attributes;
    while let Some(position) = rest.find(key) {
        let candidate = &rest[position + key.len()..];
        let candidate = candidate.trim_start();
        let Some(candidate) = candidate.strip_prefix('=') else {
            rest = &candidate[key.len().min(candidate.len())..];
            continue;
        };
        let candidate = candidate.trim_start();
        let quote = candidate.chars().next()?;
        if quote != '\'' && quote != '"' {
            return candidate.split_whitespace().next();
        }
        let value = &candidate[1..];
        return value.find(quote).map(|end| &value[..end]);
    }
    None
}

fn template_component_tags(template: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let bytes = template.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'<' {
            index += 1;
            continue;
        }
        index += 1;
        if index < bytes.len() && matches!(bytes[index], b'/' | b'!' | b'?') {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len()
            && (bytes[index].is_ascii_alphanumeric()
                || matches!(bytes[index], b'-' | b'_' | b'.' | b':'))
        {
            index += 1;
        }
        if start == index {
            continue;
        }
        let tag = &template[start..index];
        let is_component = tag.contains('-')
            || tag
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_uppercase());
        if is_component {
            tags.push(tag.to_string());
        }
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_script_template_and_source_lines() {
        let source = r#"<template>
  <UserCard :user="user" />
  <app-shell />
</template>
<script setup lang="ts">
interface User { id: number }
function loadUser(): User { return { id: 1 } }
</script>
<style>
.card { color: red; }
</style>"#;
        let (symbols, edges) = parse_vue(Path::new("UserView.vue"), source).unwrap();
        let load = symbols
            .iter()
            .find(|symbol| symbol.name == "loadUser")
            .unwrap();
        assert_eq!(load.file_path, "UserView.vue");
        assert_eq!(load.start_line, 7);
        assert!(symbols.iter().any(|symbol| symbol.kind == "Component"));
        assert!(edges
            .iter()
            .any(|edge| { edge.edge_type == EdgeType::References && edge.dst == "UserCard" }));
        assert!(edges
            .iter()
            .any(|edge| { edge.edge_type == EdgeType::References && edge.dst == "app-shell" }));
    }

    #[test]
    fn block_extraction_handles_multiple_styles() {
        let source = "<style>.a{}</style>\n<style lang='scss'>$x: 1;</style>";
        assert_eq!(extract_blocks(source).len(), 2);
    }
}
