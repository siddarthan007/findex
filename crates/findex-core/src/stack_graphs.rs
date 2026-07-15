//! Precise cross-file name resolution using GitHub Stack Graphs.
//!
//! Python, JavaScript, TypeScript/TSX and Java use their published TSG rule
//! packages. Other languages retain Findex's fast heuristic resolver.

#[cfg(feature = "stack-graphs")]
mod enabled {
    use crate::storage::{Edge, EdgeType, Storage, Symbol};
    use serde::{Deserialize, Serialize};
    use stack_graphs::graph::StackGraph;
    use stack_graphs::partial::PartialPaths;
    use stack_graphs::stitching::{
        ForwardPartialPathStitcher, GraphEdgeCandidates, StitcherConfig,
    };
    use stack_graphs::CancelAfterDuration as StitchCancellation;
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tree_sitter_stack_graphs::loader::LanguageConfiguration;
    use tree_sitter_stack_graphs::{
        CancelAfterDuration, NoCancellation, Variables, FILE_PATH_VAR, ROOT_PATH_VAR,
    };

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    pub struct StackGraphStats {
        pub enabled: bool,
        pub supported_files: usize,
        pub graph_nodes: usize,
        pub resolved_edges: usize,
        pub skipped_files: usize,
        pub timed_out: bool,
        pub message: String,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct ResolvedEdge {
        source_file: String,
        source_line: usize,
        source_symbol: String,
        target_file: String,
        target_line: usize,
        target_symbol: String,
    }

    fn configurations(files: &[PathBuf]) -> Result<Vec<LanguageConfiguration>, String> {
        let cancellation = NoCancellation;
        let extensions: HashSet<_> = files
            .iter()
            .filter_map(|path| path.extension().and_then(|extension| extension.to_str()))
            .collect();
        let mut configurations = Vec::new();
        if extensions.contains("py") {
            configurations.push(
                tree_sitter_stack_graphs_python::try_language_configuration(&cancellation)
                    .map_err(|error| error.to_string())?,
            );
        }
        if extensions
            .iter()
            .any(|extension| matches!(*extension, "js" | "mjs" | "cjs"))
        {
            configurations.push(
                tree_sitter_stack_graphs_javascript::try_language_configuration(&cancellation)
                    .map_err(|error| error.to_string())?,
            );
        }
        if extensions
            .iter()
            .any(|extension| matches!(*extension, "ts" | "mts" | "cts"))
        {
            configurations.push(
                tree_sitter_stack_graphs_typescript::try_language_configuration_typescript(
                    &cancellation,
                )
                .map_err(|error| error.to_string())?,
            );
        }
        if extensions.contains("tsx") {
            configurations.push(
                tree_sitter_stack_graphs_typescript::try_language_configuration_tsx(&cancellation)
                    .map_err(|error| error.to_string())?,
            );
        }
        if extensions.contains("java") {
            configurations.push(
                tree_sitter_stack_graphs_java::try_language_configuration(&cancellation)
                    .map_err(|error| error.to_string())?,
            );
        }
        Ok(configurations)
    }

    fn supported(path: &Path) -> bool {
        matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("py" | "js" | "mjs" | "cjs" | "ts" | "mts" | "cts" | "tsx" | "java")
        )
    }

    fn build_graph(root: &Path, files: &[PathBuf]) -> Result<(StackGraph, usize), String> {
        let configurations = configurations(files)?;
        let mut graph = StackGraph::new();
        let per_file_ms = std::env::var("FINDEX_STACK_GRAPH_FILE_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(400)
            .clamp(25, 10_000);
        let mut skipped = 0;

        for path in files {
            let Some(configuration) = configurations.iter().find(|configuration| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        configuration
                            .file_types
                            .iter()
                            .any(|file_type| file_type == extension)
                    })
            }) else {
                skipped += 1;
                continue;
            };
            let Ok(source) = std::fs::read_to_string(path) else {
                skipped += 1;
                continue;
            };
            let path_string = path.to_string_lossy().replace('\\', "/");
            let root_string = root.to_string_lossy().replace('\\', "/");
            let Ok(file) = graph.add_file(&path_string) else {
                skipped += 1;
                continue;
            };
            let mut globals = Variables::new();
            globals
                .add(FILE_PATH_VAR.into(), path_string.as_str().into())
                .map_err(|_| "duplicate FILE_PATH stack-graph variable".to_string())?;
            globals
                .add(ROOT_PATH_VAR.into(), root_string.as_str().into())
                .map_err(|_| "duplicate ROOT_PATH stack-graph variable".to_string())?;
            let cancellation = CancelAfterDuration::new(Duration::from_millis(per_file_ms));
            if configuration
                .sgl
                .build_stack_graph_into(&mut graph, file, &source, &globals, &cancellation)
                .is_err()
            {
                skipped += 1;
            }
        }
        Ok((graph, skipped))
    }

    fn resolve_graph(graph: &StackGraph) -> (Vec<ResolvedEdge>, bool) {
        let mut partials = PartialPaths::new();
        let mut resolved = HashSet::new();
        let references: Vec<_> = graph
            .iter_nodes()
            .filter(|handle| graph[*handle].is_reference())
            .collect();
        let timeout_ms = std::env::var("FINDEX_STACK_GRAPH_QUERY_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(2_000)
            .clamp(50, 60_000);
        let cancellation = StitchCancellation::new(Duration::from_millis(timeout_ms));
        let result = ForwardPartialPathStitcher::find_all_complete_partial_paths(
            &mut GraphEdgeCandidates::new(graph, &mut partials, None),
            references,
            StitcherConfig::default(),
            &cancellation,
            |graph, _partials, path| {
                let source = path.start_node;
                let target = path.end_node;
                let Some(source_file) = graph[source].file() else {
                    return;
                };
                let Some(target_file) = graph[target].file() else {
                    return;
                };
                let Some(source_symbol) = graph[source].symbol() else {
                    return;
                };
                let Some(target_symbol) = graph[target].symbol() else {
                    return;
                };
                let source_line = graph
                    .source_info(source)
                    .map(|info| info.span.start.line + 1)
                    .unwrap_or(1);
                let target_line = graph
                    .source_info(target)
                    .map(|info| info.span.start.line + 1)
                    .unwrap_or(1);
                resolved.insert(ResolvedEdge {
                    source_file: graph[source_file]
                        .name()
                        .replace('/', std::path::MAIN_SEPARATOR_STR),
                    source_line,
                    source_symbol: graph[source_symbol].to_string(),
                    target_file: graph[target_file]
                        .name()
                        .replace('/', std::path::MAIN_SEPARATOR_STR),
                    target_line,
                    target_symbol: graph[target_symbol].to_string(),
                });
            },
        );
        (resolved.into_iter().collect(), result.is_err())
    }

    fn nearest_symbol<'a>(
        by_file: &'a HashMap<String, Vec<&'a Symbol>>,
        file: &str,
        line: usize,
        name: Option<&str>,
    ) -> Option<&'a Symbol> {
        by_file.get(file).and_then(|symbols| {
            symbols
                .iter()
                .filter(|symbol| name.is_none_or(|name| symbol.name == name))
                .min_by_key(|symbol| {
                    let contains = symbol.start_line <= line && line <= symbol.end_line;
                    (
                        !contains,
                        symbol.start_line.abs_diff(line),
                        symbol.end_line.saturating_sub(symbol.start_line),
                    )
                })
                .copied()
        })
    }

    pub fn resolve_into_storage(root: &Path, storage: &Storage) -> Result<StackGraphStats, String> {
        if std::env::var("FINDEX_STACK_GRAPHS").as_deref() == Ok("0") {
            return Ok(StackGraphStats {
                message: "disabled by FINDEX_STACK_GRAPHS=0".to_string(),
                ..Default::default()
            });
        }
        let max_files = std::env::var("FINDEX_STACK_GRAPH_MAX_FILES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(2_000)
            .clamp(1, 50_000);
        let all_files = storage.list_files().map_err(|error| error.to_string())?;
        let supported_files: Vec<_> = all_files
            .iter()
            .map(|file| file.path.clone())
            .filter(|path| supported(path))
            .take(max_files + 1)
            .collect();
        if supported_files.len() > max_files {
            return Ok(StackGraphStats {
                enabled: true,
                supported_files: supported_files.len(),
                skipped_files: supported_files.len(),
                message: format!(
                    "skipped: supported file count exceeds FINDEX_STACK_GRAPH_MAX_FILES={max_files}"
                ),
                ..Default::default()
            });
        }

        let (graph, skipped_files) = build_graph(root, &supported_files)?;
        let graph_nodes = graph.iter_nodes().count();
        let (resolved, timed_out) = resolve_graph(&graph);
        let symbols = storage.list_symbols().map_err(|error| error.to_string())?;
        let mut by_file: HashMap<String, Vec<&Symbol>> = HashMap::new();
        for symbol in &symbols {
            by_file
                .entry(symbol.file_path.clone())
                .or_default()
                .push(symbol);
        }
        let mut edges = Vec::new();
        for resolution in resolved {
            let source = nearest_symbol(
                &by_file,
                &resolution.source_file,
                resolution.source_line,
                None,
            );
            let target = nearest_symbol(
                &by_file,
                &resolution.target_file,
                resolution.target_line,
                Some(&resolution.target_symbol),
            );
            if let (Some(source), Some(target)) = (source, target) {
                if source.id != target.id {
                    edges.push(Edge {
                        src: source.id.clone(),
                        dst: target.id.clone(),
                        edge_type: EdgeType::References,
                        tags: vec!["stack-graphs".to_string()],
                        ..Default::default()
                    });
                }
            }
        }
        edges.sort_by(|a, b| (&a.src, &a.dst).cmp(&(&b.src, &b.dst)));
        edges.dedup_by(|a, b| a.src == b.src && a.dst == b.dst);
        storage
            .delete_edges_with_tag("stack-graphs")
            .map_err(|error| error.to_string())?;
        storage
            .save_edges_batch(&edges)
            .map_err(|error| error.to_string())?;
        Ok(StackGraphStats {
            enabled: true,
            supported_files: supported_files.len(),
            graph_nodes,
            resolved_edges: edges.len(),
            skipped_files,
            timed_out,
            message: if timed_out {
                "resolution hit the bounded query timeout; partial exact edges were saved"
                    .to_string()
            } else {
                "exact stack-graph reference edges saved".to_string()
            },
        })
    }
}

#[cfg(feature = "stack-graphs")]
pub use enabled::*;

#[cfg(not(feature = "stack-graphs"))]
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct StackGraphStats {
    pub enabled: bool,
    pub supported_files: usize,
    pub graph_nodes: usize,
    pub resolved_edges: usize,
    pub skipped_files: usize,
    pub timed_out: bool,
    pub message: String,
}
