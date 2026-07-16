//! Precise cross-file name resolution using GitHub Stack Graphs.
//!
//! Python, JavaScript, TypeScript/TSX and Java use published TSG packages.
//! Rust, C/C++, Dart, Go, HTML and CSS use bundled, validated lexical TSG
//! rules alongside Findex's typed heuristic edges; their narrower guarantees
//! are reported separately instead of being described as full resolution.

#[cfg(feature = "stack-graphs")]
mod enabled {
    use crate::storage::{Edge, EdgeType, Storage, Symbol};
    use serde::{Deserialize, Serialize};
    use stack_graphs::graph::StackGraph;
    use stack_graphs::partial::PartialPaths;
    use stack_graphs::stitching::{
        ForwardPartialPathStitcher, GraphEdgeCandidates, StitcherConfig,
    };
    use stack_graphs::CancellationFlag as StitchCancellationFlag;
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};
    use tree_sitter_stack_graphs::loader::LanguageConfiguration;
    use tree_sitter_stack_graphs::{
        CancellationFlag as BuildCancellationFlag, NoCancellation, Variables, FILE_PATH_VAR,
        ROOT_PATH_VAR,
    };

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    pub struct StackGraphStats {
        pub enabled: bool,
        pub supported_files: usize,
        pub graph_nodes: usize,
        pub resolved_edges: usize,
        pub skipped_files: usize,
        #[serde(default)]
        pub published_rule_files: usize,
        #[serde(default)]
        pub bundled_rule_files: usize,
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

    struct CooperativeCancellation {
        started: Instant,
        timeout: Duration,
        task: Option<crate::cancellation::CancellationToken>,
    }

    impl CooperativeCancellation {
        fn new(timeout: Duration) -> Self {
            Self {
                started: Instant::now(),
                timeout,
                task: crate::cancellation::inherited_token(),
            }
        }

        fn cancelled(&self) -> bool {
            self.started.elapsed() > self.timeout
                || self
                    .task
                    .as_ref()
                    .is_some_and(crate::cancellation::CancellationToken::is_cancelled)
        }
    }

    impl BuildCancellationFlag for CooperativeCancellation {
        fn check(
            &self,
            at: &'static str,
        ) -> Result<(), tree_sitter_stack_graphs::CancellationError> {
            if self.cancelled() {
                Err(tree_sitter_stack_graphs::CancellationError(at))
            } else {
                Ok(())
            }
        }
    }

    impl StitchCancellationFlag for CooperativeCancellation {
        fn check(&self, at: &'static str) -> Result<(), stack_graphs::CancellationError> {
            if self.cancelled() {
                Err(stack_graphs::CancellationError(at))
            } else {
                Ok(())
            }
        }
    }

    fn bundled_configuration(
        extension: &str,
        scope: &str,
        file_types: &[&str],
        source: &'static str,
    ) -> Result<LanguageConfiguration, String> {
        let parser = crate::parser::registry::config_for_extension(extension)
            .ok_or_else(|| format!("missing parser for bundled {extension} TSG rules"))?;
        LanguageConfiguration::from_sources(
            parser.language.clone(),
            Some(scope.to_string()),
            None,
            file_types
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            PathBuf::from(format!("findex://tsg/{extension}")),
            source,
            None,
            None,
            &NoCancellation,
        )
        .map_err(|error| error.to_string())
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
        if extensions.contains("rs") {
            configurations.push(bundled_configuration(
                "rs",
                "source.rust",
                &["rs"],
                include_str!("../tsg/rust.tsg"),
            )?);
        }
        if extensions
            .iter()
            .any(|extension| matches!(*extension, "c" | "h"))
        {
            configurations.push(bundled_configuration(
                "c",
                "source.c",
                &["c", "h"],
                include_str!("../tsg/c.tsg"),
            )?);
        }
        if extensions
            .iter()
            .any(|extension| matches!(*extension, "cc" | "cpp" | "cxx" | "hpp" | "hxx"))
        {
            configurations.push(bundled_configuration(
                "cpp",
                "source.cpp",
                &["cc", "cpp", "cxx", "hpp", "hxx"],
                include_str!("../tsg/cpp.tsg"),
            )?);
        }
        if extensions.contains("dart") {
            configurations.push(bundled_configuration(
                "dart",
                "source.dart",
                &["dart"],
                include_str!("../tsg/dart.tsg"),
            )?);
        }
        if extensions.contains("go") {
            configurations.push(bundled_configuration(
                "go",
                "source.go",
                &["go"],
                include_str!("../tsg/go.tsg"),
            )?);
        }
        if extensions
            .iter()
            .any(|extension| matches!(*extension, "html" | "htm"))
        {
            configurations.push(bundled_configuration(
                "html",
                "text.html.basic",
                &["html", "htm"],
                include_str!("../tsg/html.tsg"),
            )?);
        }
        if extensions.contains("css") {
            configurations.push(bundled_configuration(
                "css",
                "source.css",
                &["css"],
                include_str!("../tsg/css.tsg"),
            )?);
        }
        Ok(configurations)
    }

    fn supported(path: &Path) -> bool {
        matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some(
                "py" | "js"
                    | "mjs"
                    | "cjs"
                    | "ts"
                    | "mts"
                    | "cts"
                    | "tsx"
                    | "java"
                    | "rs"
                    | "c"
                    | "h"
                    | "cc"
                    | "cpp"
                    | "cxx"
                    | "hpp"
                    | "hxx"
                    | "dart"
                    | "go"
                    | "html"
                    | "htm"
                    | "css"
            )
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
            crate::cancellation::checkpoint().map_err(|error| error.to_string())?;
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
            let cancellation = CooperativeCancellation::new(Duration::from_millis(per_file_ms));
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
        let cancellation = CooperativeCancellation::new(Duration::from_millis(timeout_ms));
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
        let is_published = |path: &Path| {
            matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("py" | "js" | "mjs" | "cjs" | "ts" | "mts" | "cts" | "tsx" | "java")
            )
        };
        let published_rule_files = supported_files
            .iter()
            .filter(|path| is_published(path))
            .count();
        let bundled_rule_files = supported_files.len().saturating_sub(published_rule_files);
        if supported_files.len() > max_files {
            return Ok(StackGraphStats {
                enabled: true,
                supported_files: supported_files.len(),
                skipped_files: supported_files.len(),
                published_rule_files,
                bundled_rule_files,
                message: format!(
                    "skipped: supported file count exceeds FINDEX_STACK_GRAPH_MAX_FILES={max_files}"
                ),
                ..Default::default()
            });
        }

        let (graph, skipped_files) = build_graph(root, &supported_files)?;
        let graph_nodes = graph.iter_nodes().count();
        let (resolved, timed_out) = resolve_graph(&graph);
        crate::cancellation::checkpoint().map_err(|error| error.to_string())?;
        let symbols = storage.list_symbols().map_err(|error| error.to_string())?;
        let mut by_file: HashMap<String, Vec<&Symbol>> = HashMap::new();
        for symbol in &symbols {
            crate::cancellation::checkpoint().map_err(|error| error.to_string())?;
            by_file
                .entry(symbol.file_path.clone())
                .or_default()
                .push(symbol);
        }
        let mut edges = Vec::new();
        for resolution in resolved {
            crate::cancellation::checkpoint().map_err(|error| error.to_string())?;
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
                    let mut tags = vec!["stack-graphs".to_string()];
                    tags.push(
                        if is_published(Path::new(&resolution.source_file)) {
                            "stack-graphs-published"
                        } else {
                            "stack-graphs-bundled"
                        }
                        .to_string(),
                    );
                    edges.push(Edge {
                        src: source.id.clone(),
                        dst: target.id.clone(),
                        edge_type: EdgeType::References,
                        tags,
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
            published_rule_files,
            bundled_rule_files,
            timed_out,
            message: if timed_out {
                format!("resolution hit the bounded query timeout; partial edges saved ({published_rule_files} published-rule files, {bundled_rule_files} bundled lexical-rule files)")
            } else {
                format!("stack-graph edges saved ({published_rule_files} published-rule files, {bundled_rule_files} bundled lexical-rule files)")
            },
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn bundled_language_rules_compile_against_registered_grammars() {
            let files = ["rs", "c", "cpp", "dart", "go", "html", "css"]
                .into_iter()
                .map(|extension| PathBuf::from(format!("sample.{extension}")))
                .collect::<Vec<_>>();
            let configurations = configurations(&files).unwrap();
            for extension in ["rs", "c", "cpp", "dart", "go", "html", "css"] {
                assert!(configurations.iter().any(|configuration| configuration
                    .file_types
                    .iter()
                    .any(|file_type| file_type == extension)));
            }
        }
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
