use clap::{Parser, Subcommand, ValueEnum};
use findex_core::graph_query::query_graph;
use findex_core::intelligence::{
    ast_outline, build_context_bundle, graph_snapshot, impact_analysis,
};
use findex_core::mcp::McpServer;
use findex_core::mcp_http::{serve as serve_mcp_http, McpHttpConfig};
use findex_core::watch::watch_codebase;
use findex_core::{
    build_vector_index, get_codebase_skeleton, get_file_skeleton, ingest_codebase,
    ingest_codebase_full, ingest_codebase_phase0, search_codebase_with_components,
    semantic_diff_files,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

mod agent_setup;
mod ingest_sprite;
mod tui;
use agent_setup::AgentTarget;
use findex_core::resolver::{
    expand_context, get_callees, get_callers, resolve_definition, resolve_references,
};
use findex_core::search::local_embedder::create_embedder;
use findex_core::search::rerank::create_reranker;
use findex_core::storage::Storage;
use findex_core::taint::{propagate_taint, TaintConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
    Compact,
}

#[derive(Parser)]
#[command(name = "findex")]
#[command(about = "Findex Codebase Intelligence Engine CLI", long_about = None)]
struct Cli {
    #[arg(short, long, default_value = ".findex_db", global = true)]
    db_path: PathBuf,

    /// Output format for agent and human consumers (`--output` is an alias).
    #[arg(
        long,
        alias = "output",
        value_enum,
        default_value = "text",
        global = true
    )]
    format: OutputFormat,

    /// Shorthand for `--format json`.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Ingest and index a codebase directory
    Index {
        /// The root directory of the codebase to index
        path: PathBuf,
    },
    /// List all ingested symbols in the database
    ListSymbols,
    /// Run benchmark to verify ingestion speed and incremental re-indexing
    Benchmark {
        /// The root directory of the codebase to benchmark
        path: PathBuf,
    },
    /// Search the codebase using lexical, semantic, or hybrid ranking
    Search {
        /// The search query
        query: String,
        /// Search mode: hybrid, lexical, semantic
        #[arg(short, long, default_value = "hybrid")]
        mode: String,
        /// Limit the number of search results
        #[arg(short, long, default_value = "10")]
        limit: usize,
    },
    /// Generate an Aider-style elided codebase skeleton
    Skeleton {
        /// Budget in tokens for the generated skeleton
        #[arg(short, long, default_value = "1024")]
        tokens: usize,
    },
    /// Resolve a symbol reference name to its definition site (Go-to-definition)
    ResolveDef {
        /// Reference name of the symbol (e.g. "new", "run")
        symbol: String,
        /// Source symbol context ID (e.g. "src/main.rs#main")
        #[arg(short, long)]
        context: Option<String>,
    },
    /// Find all references to a definition symbol ID
    ResolveRefs {
        /// Fully-qualified definition symbol ID (e.g. "src/lib.rs#run")
        symbol_id: String,
    },
    /// Locate all direct callers of a function symbol ID
    Callers {
        /// Fully-qualified symbol ID
        symbol_id: String,
    },
    /// Locate all direct callees of a function symbol ID
    Callees {
        /// Fully-qualified symbol ID
        symbol_id: String,
    },
    /// Perform a BFS graph expansion to assemble context around a symbol ID
    Expand {
        /// Fully-qualified symbol ID
        symbol_id: String,
        /// Maximum traversal depth
        #[arg(short = 'D', long, default_value = "1")]
        depth: u32,
    },
    /// Run a Cypher-like graph query on the indexed code graph
    GraphQuery {
        /// Query string, e.g. "MATCH (a)-[:Calls]->(b) WHERE a.name = 'main' RETURN a, b"
        query: String,
    },
    /// Return the signature skeleton of a single file
    FileSkeleton {
        /// Path to the file
        path: PathBuf,
        /// Token budget for the skeleton
        #[arg(short, long, default_value = "1024")]
        tokens: usize,
    },
    /// Watch a codebase directory and re-index incrementally on file changes
    Watch {
        /// The root directory of the codebase to watch
        path: PathBuf,
        /// Debounce window in milliseconds
        #[arg(short = 'w', long, default_value = "500")]
        debounce_ms: u64,
    },
    /// Start the Model Context Protocol (MCP) server on stdio
    Mcp,
    /// Start the MCP Streamable HTTP endpoint (POST /mcp, GET /health)
    McpHttp {
        #[arg(long, default_value = "127.0.0.1:37420")]
        bind: String,
        /// Environment variable containing the bearer token
        #[arg(long, default_value = "FINDEX_MCP_TOKEN")]
        token_env: String,
    },
    /// Launch the interactive terminal UI
    Tui,
    /// Show index statistics
    Status,
    /// Build a token-bounded context package for an agent task
    Context {
        query: String,
        #[arg(short, long, default_value = "hybrid")]
        mode: String,
        #[arg(short, long, default_value = "2048")]
        tokens: usize,
    },
    /// Assess blast radius and God-node risk before changing a symbol
    Impact { symbol_id: String },
    /// Print the nested AST/symbol outline of a file
    Ast { path: PathBuf },
    /// Export a bounded graph snapshot for visualization
    GraphExport {
        #[arg(short, long, default_value = "1000")]
        limit: usize,
    },
    /// Inspect CPU, memory, GPU, batching and quantization policy
    Doctor {
        #[arg(long)]
        no_gpu: bool,
    },
    /// Download or verify the pinned embedding and reranking artifacts
    Models {
        /// Accuracy/latency profile: fast, balanced, or quality
        #[arg(long, default_value = "fast")]
        profile: findex_core::models::ModelProfile,
        /// Resolve strictly from the local Hugging Face cache
        #[arg(long)]
        offline: bool,
        /// Acquire only the embedding model
        #[arg(long, conflicts_with = "reranker_only")]
        embedding_only: bool,
        /// Acquire only the cross-encoder reranker
        #[arg(long, conflicts_with = "embedding_only")]
        reranker_only: bool,
    },
    /// Check for or install a signed Findex release
    Update {
        #[command(subcommand)]
        command: UpdateCommand,
    },
    /// Inspect or change persisted production feature and resource controls
    Settings {
        #[command(subcommand)]
        command: SettingsCommand,
    },
    /// Build or rebuild the vector index from stored symbols
    BuildVectors,
    /// Compare two source files structurally
    SemanticDiff { file_a: PathBuf, file_b: PathBuf },
    /// Trace a taint label through the symbol graph
    Taint {
        /// Exact source symbol ID
        source: String,
        #[arg(short, long, default_value = "user-input")]
        label: String,
        #[arg(short = 'D', long, default_value = "3")]
        depth: u32,
    },
    /// Install Findex MCP and its token-saving skill for a coding agent
    SetupAgent {
        /// Agent configuration to install, or all supported agents
        #[arg(value_enum, default_value = "all")]
        agent: AgentTarget,
        /// Replace only an existing Findex entry or skill; unrelated config is preserved
        #[arg(long)]
        force: bool,
        /// Print intended paths and commands without changing anything
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum UpdateCommand {
    /// Check GitHub Releases without changing the installed binary
    Check {
        /// Use a check cached within the last 24 hours when available
        #[arg(long)]
        cached: bool,
    },
    /// Download, verify, and install the newest release
    Install {
        /// Confirm installation non-interactively
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum SettingsCommand {
    /// Print the effective settings for this index
    Show,
    /// Change only the supplied values; omitted settings are preserved
    Set {
        #[arg(long)]
        lexical: Option<bool>,
        #[arg(long)]
        semantic: Option<bool>,
        #[arg(long)]
        reranking: Option<bool>,
        #[arg(long)]
        graph_expansion: Option<bool>,
        #[arg(long)]
        structural_prefetch: Option<bool>,
        #[arg(long)]
        stack_graphs: Option<bool>,
        #[arg(long)]
        watcher: Option<bool>,
        #[arg(long)]
        vfs_shadowing: Option<bool>,
        #[arg(long)]
        trace_pinning: Option<bool>,
        #[arg(long)]
        graph_hops: Option<u32>,
        #[arg(long)]
        candidates: Option<usize>,
        #[arg(long)]
        token_budget: Option<usize>,
        #[arg(long)]
        mmr_lambda: Option<f32>,
        #[arg(long)]
        compute: Option<findex_core::settings::ComputeDevice>,
        #[arg(long)]
        model_profile: Option<findex_core::models::ModelProfile>,
        #[arg(long)]
        memory_mib: Option<u64>,
        #[arg(long)]
        gpu_memory_mib: Option<u64>,
        #[arg(long)]
        idle_seconds: Option<u64>,
        #[arg(long)]
        theme: Option<findex_core::settings::ThemePreference>,
        #[arg(long)]
        motion: Option<bool>,
        #[arg(long)]
        graph_particles: Option<bool>,
        #[arg(long)]
        graph_labels: Option<bool>,
        #[arg(long)]
        predictive_query_cache: Option<bool>,
        #[arg(long)]
        query_cache_entries: Option<usize>,
        #[arg(long)]
        query_cache_ttl_seconds: Option<u64>,
        #[arg(long)]
        minimize_to_tray: Option<bool>,
        #[arg(long)]
        cursor_companion: Option<bool>,
        #[arg(long)]
        terminal_pointer_input: Option<bool>,
    },
    /// Restore production defaults
    Reset,
}

impl Cli {
    fn output_format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            self.format
        }
    }
}

fn print_json(value: serde_json::Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    findex_core::runtime::configure_runtime();
    let cli = Cli::parse();
    let persisted_settings = findex_core::settings::load_or_default(&cli.db_path);
    findex_core::runtime::apply_runtime_settings(&persisted_settings);
    let format = cli.output_format();

    match &cli.command {
        Commands::Index { path } => {
            if format == OutputFormat::Text {
                println!("Opening database at: {:?}", cli.db_path);
            }
            let storage = Storage::open(&cli.db_path)?;

            if format == OutputFormat::Text {
                println!("Ingesting codebase at: {:?}", path);
            }
            let stats = if std::env::var("FINDEX_EMBEDDING_MODEL_DIR").is_ok() {
                ingest_codebase_full(path, &cli.db_path, &storage)?
            } else {
                ingest_codebase(path, &cli.db_path, &storage)?
            };

            let loc_per_sec = if stats.duration_ms > 0 {
                (stats.total_lines as f64) / (stats.duration_ms as f64 / 1000.0)
            } else {
                0.0
            };
            match format {
                OutputFormat::Json => print_json(serde_json::json!({
                    "command": "index",
                    "root": path,
                    "db_path": cli.db_path,
                    "stats": stats,
                    "loc_per_second": loc_per_sec
                }))?,
                OutputFormat::Compact => println!(
                    "files={} changed={} deleted={} lines={} duration_ms={} loc_per_sec={:.2}",
                    stats.total_files,
                    stats.parsed_files,
                    stats.deleted_files,
                    stats.total_lines,
                    stats.duration_ms,
                    loc_per_sec
                ),
                OutputFormat::Text => {
                    println!("\n=== Ingestion Complete ===");
                    println!("Total files found:   {}", stats.total_files);
                    println!("Parsed files:        {}", stats.parsed_files);
                    println!("Deleted files:       {}", stats.deleted_files);
                    println!("Total bytes parsed:  {}", stats.total_bytes);
                    println!("Total lines parsed:  {}", stats.total_lines);
                    println!("Time taken:          {} ms", stats.duration_ms);
                    println!("Throughput:          {:.2} LOC/sec", loc_per_sec);
                }
            }
        }
        Commands::ListSymbols => {
            let storage = Storage::open(&cli.db_path)?;
            let symbols = storage.list_symbols()?;
            match format {
                OutputFormat::Json => print_json(serde_json::json!({ "symbols": symbols }))?,
                OutputFormat::Compact => {
                    for sym in symbols {
                        println!(
                            "{}\t{}\t{}:{}-{}",
                            sym.kind, sym.name, sym.file_path, sym.start_line, sym.end_line
                        );
                    }
                }
                OutputFormat::Text => {
                    println!("Found {} symbols in index:", symbols.len());
                    for sym in symbols {
                        println!(
                            "[{}] {} -> {}:{}-{}",
                            sym.kind, sym.name, sym.file_path, sym.start_line, sym.end_line
                        );
                    }
                }
            }
        }
        Commands::Benchmark { path } => {
            println!("=== FINDEX BENCHMARK ===");
            println!("Target Path: {:?}", path);

            // 0. Phase-0 cold run (symbol graph + lexical index, no vector index)
            let phase0_db_dir = tempfile::tempdir()?;
            let phase0_storage = Storage::open(phase0_db_dir.path().join("bench_db"))?;

            println!("\nExecuting Phase-0 Cold Run (no vector index)...");
            let phase0_cold_stats =
                ingest_codebase_phase0(path, phase0_db_dir.path(), &phase0_storage)?;
            println!("Phase-0 Cold Run complete:");
            println!("  Total Files:      {}", phase0_cold_stats.total_files);
            println!("  Parsed Files:     {}", phase0_cold_stats.parsed_files);
            println!("  Total Lines:      {}", phase0_cold_stats.total_lines);
            println!("  Duration:         {} ms", phase0_cold_stats.duration_ms);

            let phase0_cold_loc_per_sec = if phase0_cold_stats.duration_ms > 0 {
                (phase0_cold_stats.total_lines as f64)
                    / (phase0_cold_stats.duration_ms as f64 / 1000.0)
            } else {
                0.0
            };
            println!("  Throughput:       {:.2} LOC/sec", phase0_cold_loc_per_sec);

            // 1. Full cold run (clear database first)
            let temp_db_dir = tempfile::tempdir()?;
            let storage = Storage::open(temp_db_dir.path().join("bench_db"))?;

            println!("\nExecuting Full Cold Run...");
            let cold_stats = ingest_codebase_full(path, temp_db_dir.path(), &storage)?;
            println!("Full Cold Run complete:");
            println!("  Total Files:      {}", cold_stats.total_files);
            println!("  Parsed Files:     {}", cold_stats.parsed_files);
            println!("  Total Lines:      {}", cold_stats.total_lines);
            println!("  Duration:         {} ms", cold_stats.duration_ms);

            let cold_loc_per_sec = if cold_stats.duration_ms > 0 {
                (cold_stats.total_lines as f64) / (cold_stats.duration_ms as f64 / 1000.0)
            } else {
                0.0
            };
            println!("  Throughput:       {:.2} LOC/sec", cold_loc_per_sec);

            // 2. Incremental run (nothing changed) - Phase 0 scope
            println!("\nExecuting Hot Run (No Changes)...");
            let hot_stats_no_change = ingest_codebase_phase0(path, temp_db_dir.path(), &storage)?;
            println!("Hot Run (No Changes) complete:");
            println!("  Parsed Files:     {}", hot_stats_no_change.parsed_files);
            println!("  Duration:         {} ms", hot_stats_no_change.duration_ms);

            // 3. Incremental run (one file modified) - Phase 0 scope
            // Let's find a supported file to modify for benchmarking
            let files = storage.list_files()?;
            if let Some(file_to_modify) = files.first() {
                println!(
                    "\nSimulating file modification on: {:?}",
                    file_to_modify.path
                );

                // Read original contents
                let original_content = std::fs::read_to_string(&file_to_modify.path)?;

                // Append a small comment
                let mut modified_content = original_content.clone();
                modified_content.push_str("\n// Findex benchmark modification comment\n");

                // Write modified contents
                std::fs::write(&file_to_modify.path, &modified_content)?;

                let start_hot = Instant::now();
                let hot_stats_changed = ingest_codebase_phase0(path, temp_db_dir.path(), &storage)?;
                let hot_duration = start_hot.elapsed().as_millis();

                // Restore file
                std::fs::write(&file_to_modify.path, original_content)?;

                println!("Hot Run (1 File Changed) complete:");
                println!("  Parsed Files:     {}", hot_stats_changed.parsed_files);
                println!("  Duration:         {} ms", hot_duration);

                // Verify Phase 0 Performance Gates
                println!("\n=== Phase 0 Gate Validation ===");
                let gate_throughput_passed = phase0_cold_loc_per_sec >= 100_000.0;
                let gate_incremental_passed = hot_duration < 1000;

                println!(
                    "Throughput Gate (>= 100,000 LOC/sec): {}",
                    if gate_throughput_passed {
                        "PASSED"
                    } else {
                        "FAILED (Need more optimization or larger codebase for multithreading scaling)"
                    }
                );
                println!(
                    "Incremental Gate (< 1.0s): {}",
                    if gate_incremental_passed {
                        "PASSED"
                    } else {
                        "FAILED"
                    }
                );
            } else {
                println!("\nNo files found in the codebase to run the modification benchmark.");
            }
        }
        Commands::Search { query, mode, limit } => {
            let storage = Storage::open(&cli.db_path)?;

            let reranker = create_reranker();
            let embedder = create_embedder(128);

            let start = Instant::now();
            let results = search_codebase_with_components(
                &cli.db_path,
                &storage,
                query,
                mode,
                Some(reranker.as_ref()),
                embedder.as_ref(),
                *limit,
            )?;
            let duration = start.elapsed().as_millis();

            match format {
                OutputFormat::Json => {
                    let values: Vec<_> = results
                        .into_iter()
                        .map(|(symbol, score)| serde_json::json!({ "score": score, "symbol": symbol }))
                        .collect();
                    print_json(serde_json::json!({
                        "query": query,
                        "mode": mode,
                        "elapsed_ms": duration,
                        "results": values
                    }))?;
                }
                OutputFormat::Compact => {
                    for (sym, score) in results {
                        println!(
                            "{:.4}\t{}\t{}\t{}:{}-{}",
                            score, sym.kind, sym.name, sym.file_path, sym.start_line, sym.end_line
                        );
                    }
                }
                OutputFormat::Text => {
                    println!(
                        "=== Search Results (Mode: {}, Latency: {} ms) ===",
                        mode, duration
                    );
                    for (idx, (sym, score)) in results.into_iter().enumerate() {
                        println!(
                            "{}. [Score: {:.4}] [{}] {} -> {}:{}-{}",
                            idx + 1,
                            score,
                            sym.kind,
                            sym.name,
                            sym.file_path,
                            sym.start_line,
                            sym.end_line
                        );
                        println!("   Signature: {}", sym.signature);
                    }
                }
            }
        }
        Commands::Skeleton { tokens } => {
            let storage = Storage::open(&cli.db_path)?;
            let skeleton = get_codebase_skeleton(&storage, *tokens)?;
            if format == OutputFormat::Json {
                print_json(serde_json::json!({ "token_budget": tokens, "skeleton": skeleton }))?;
            } else {
                if format == OutputFormat::Text {
                    println!("=== Codebase Skeleton (Budget: {} tokens) ===", tokens);
                }
                println!("{}", skeleton);
            }
        }
        Commands::ResolveDef { symbol, context } => {
            let storage = Storage::open(&cli.db_path)?;
            let context_id = context.clone().unwrap_or_else(|| "".to_string());

            let start = Instant::now();
            let result = resolve_definition(symbol, &context_id, &storage)?;
            let duration = start.elapsed().as_millis();

            if format == OutputFormat::Json {
                print_json(serde_json::json!({
                    "query": symbol,
                    "elapsed_ms": duration,
                    "definition": result
                }))?;
            } else if let Some(sym) = result {
                if format == OutputFormat::Text {
                    println!("=== Name Resolution Result (Latency: {} ms) ===", duration);
                    println!("Resolved definition site to:");
                }
                println!(
                    "[{}] {} -> {}:{}-{}\n{}",
                    sym.kind, sym.name, sym.file_path, sym.start_line, sym.end_line, sym.signature
                );
            } else {
                println!("No definition found for: {}", symbol);
            }
        }
        Commands::ResolveRefs { symbol_id } => {
            let storage = Storage::open(&cli.db_path)?;
            let refs = resolve_references(symbol_id, &storage)?;
            if format == OutputFormat::Json {
                print_json(serde_json::json!({ "symbol_id": symbol_id, "references": refs }))?;
            } else {
                if format == OutputFormat::Text {
                    println!("=== Found {} references to: {} ===", refs.len(), symbol_id);
                }
                for (idx, sym) in refs.into_iter().enumerate() {
                    println!(
                        "{}. [{}] {} in {}:{}-{}",
                        idx + 1,
                        sym.kind,
                        sym.name,
                        sym.file_path,
                        sym.start_line,
                        sym.end_line
                    );
                }
            }
        }
        Commands::Callers { symbol_id } => {
            let storage = Storage::open(&cli.db_path)?;
            let callers = get_callers(symbol_id, &storage)?;
            if format == OutputFormat::Json {
                print_json(serde_json::json!({ "symbol_id": symbol_id, "callers": callers }))?;
            } else {
                if format == OutputFormat::Text {
                    println!("=== Callers of: {} ===", symbol_id);
                }
                for sym in callers {
                    println!(
                        "[{}] {} in {}:{}-{}",
                        sym.kind, sym.name, sym.file_path, sym.start_line, sym.end_line
                    );
                }
            }
        }
        Commands::Callees { symbol_id } => {
            let storage = Storage::open(&cli.db_path)?;
            let callees = get_callees(symbol_id, &storage)?;
            if format == OutputFormat::Json {
                print_json(serde_json::json!({ "symbol_id": symbol_id, "callees": callees }))?;
            } else {
                if format == OutputFormat::Text {
                    println!("=== Callees of: {} ===", symbol_id);
                }
                for sym in callees {
                    println!(
                        "[{}] {} in {}:{}-{}",
                        sym.kind, sym.name, sym.file_path, sym.start_line, sym.end_line
                    );
                }
            }
        }
        Commands::Expand { symbol_id, depth } => {
            let storage = Storage::open(&cli.db_path)?;
            let expanded = expand_context(symbol_id, *depth, &storage)?;
            if format == OutputFormat::Json {
                print_json(
                    serde_json::json!({ "symbol_id": symbol_id, "depth": depth, "symbols": expanded }),
                )?;
            } else {
                if format == OutputFormat::Text {
                    println!("=== BFS Graph Expansion (Depth: {}) ===", depth);
                }
                for sym in expanded {
                    println!(
                        "[{}] {} in {}:{}-{}",
                        sym.kind, sym.name, sym.file_path, sym.start_line, sym.end_line
                    );
                }
            }
        }
        Commands::GraphQuery { query } => {
            let storage = Storage::open(&cli.db_path)?;
            let result = query_graph(&storage, query)?;
            if format == OutputFormat::Json {
                print_json(serde_json::json!({ "query": query, "result": result.to_text() }))?;
            } else {
                if format == OutputFormat::Text {
                    println!("=== Graph Query Result ===");
                }
                println!("{}", result.to_text());
            }
        }
        Commands::FileSkeleton { path, tokens } => {
            let storage = Storage::open(&cli.db_path)?;
            let skeleton = get_file_skeleton(&storage, path, *tokens)?;
            if format == OutputFormat::Json {
                print_json(
                    serde_json::json!({ "path": path, "token_budget": tokens, "skeleton": skeleton }),
                )?;
            } else {
                if format == OutputFormat::Text {
                    println!("=== File Skeleton: {} ===", path.display());
                }
                println!("{}", skeleton);
            }
        }
        Commands::Watch { path, debounce_ms } => {
            start_background_update_check();
            let storage = Arc::new(Storage::open(&cli.db_path)?);
            watch_codebase(path, &cli.db_path, storage, *debounce_ms, |_stats| {})?;
        }
        Commands::Mcp => {
            start_background_update_check();
            let server = McpServer::open(&cli.db_path)?;
            server.run()?;
        }
        Commands::McpHttp { bind, token_env } => {
            start_background_update_check();
            let bind = bind.parse()?;
            let bearer_token = std::env::var(token_env)
                .ok()
                .filter(|token| !token.is_empty());
            let config = McpHttpConfig {
                bind,
                bearer_token,
                allowed_origins: std::env::var("FINDEX_MCP_ALLOWED_ORIGINS")
                    .ok()
                    .map(|origins| {
                        origins
                            .split(',')
                            .map(|origin| origin.trim().to_string())
                            .collect()
                    })
                    .unwrap_or_default(),
            };
            if format != OutputFormat::Json {
                eprintln!("Findex MCP HTTP listening on http://{bind}/mcp");
            }
            serve_mcp_http(McpServer::open(&cli.db_path)?, config).await?;
        }
        Commands::Tui => {
            let mut app = tui::App::new(cli.db_path)?;
            app.run()?;
        }
        Commands::Status => {
            let storage = Storage::open(&cli.db_path)?;
            let stats = serde_json::json!({
                "db_path": cli.db_path,
                "files": storage.list_files()?.len(),
                "symbols": storage.list_symbols()?.len(),
                "edges": storage.list_edges()?.len(),
                "vector_index_present": cli.db_path.join("vector").exists(),
                "lexical_index_present": cli.db_path.join("lexical").exists()
                ,"merkle_root": storage
                    .get_metadata::<findex_core::merkle::MerkleSnapshot>("merkle:v1")?
                    .map(|snapshot| snapshot.root_hash_hex()),
                "stack_graphs": storage
                    .get_metadata::<findex_core::stack_graphs::StackGraphStats>("stack-graphs:last")?
            });
            match format {
                OutputFormat::Json => print_json(stats)?,
                OutputFormat::Compact => println!(
                    "files={} symbols={} edges={} vectors={} lexical={}",
                    stats["files"], stats["symbols"], stats["edges"],
                    stats["vector_index_present"], stats["lexical_index_present"]
                ),
                OutputFormat::Text => println!(
                    "Index status\n  files: {}\n  symbols: {}\n  edges: {}\n  vector index: {}\n  lexical index: {}",
                    stats["files"], stats["symbols"], stats["edges"],
                    stats["vector_index_present"], stats["lexical_index_present"]
                ),
            }
        }
        Commands::Context {
            query,
            mode,
            tokens,
        } => {
            let storage = Storage::open(&cli.db_path)?;
            let reranker = create_reranker();
            let embedder = create_embedder(128);
            let bundle = build_context_bundle(
                &cli.db_path,
                &storage,
                query,
                mode,
                *tokens,
                Some(reranker.as_ref()),
                embedder.as_ref(),
            )?;
            if format == OutputFormat::Json {
                print_json(serde_json::to_value(bundle)?)?;
            } else {
                println!("{}", bundle.repo_map);
                for item in &bundle.items {
                    println!(
                        "\n# {} — {}:{}-{} ({:.3}, {} tok)\n{}",
                        item.symbol.signature,
                        item.symbol.file_path,
                        item.symbol.start_line,
                        item.symbol.end_line,
                        item.score,
                        item.tokens,
                        item.source
                    );
                }
                eprintln!(
                    "used {} / {} tokens; avoided approximately {} candidate tokens",
                    bundle.tokens_used, bundle.token_budget, bundle.candidate_tokens_avoided
                );
            }
        }
        Commands::Impact { symbol_id } => {
            let storage = Storage::open(&cli.db_path)?;
            let report = impact_analysis(&storage, symbol_id)?;
            if format == OutputFormat::Json {
                print_json(serde_json::to_value(report)?)?;
            } else {
                println!(
                    "{}\nrisk={:.1}/100 god_node={} in={} out={} affected_files={}",
                    report.symbol.signature,
                    report.risk_score,
                    report.god_node,
                    report.incoming_edges,
                    report.outgoing_edges,
                    report.affected_files.len()
                );
                for path in report.affected_files {
                    println!("  {path}");
                }
            }
        }
        Commands::Ast { path } => {
            let storage = Storage::open(&cli.db_path)?;
            let outline = ast_outline(&storage, path)?;
            print_json(serde_json::to_value(outline)?)?;
        }
        Commands::GraphExport { limit } => {
            let storage = Storage::open(&cli.db_path)?;
            print_json(serde_json::to_value(graph_snapshot(&storage, *limit)?)?)?;
        }
        Commands::Doctor { no_gpu } => {
            let profile = findex_core::runtime::profile(!no_gpu);
            if format == OutputFormat::Json {
                print_json(serde_json::to_value(profile)?)?;
            } else {
                println!("Findex runtime profile");
                println!(
                    "  CPU threads: {} logical / {} Rayon",
                    profile.logical_cpus, profile.rayon_threads
                );
                println!(
                    "  RAM: {:.1} GiB available / {:.1} GiB total",
                    profile.available_memory_bytes as f64 / 1_073_741_824.0,
                    profile.total_memory_bytes as f64 / 1_073_741_824.0
                );
                println!(
                    "  Process RSS: {:.1} MiB",
                    profile.process_memory_bytes as f64 / 1_048_576.0
                );
                println!(
                    "  Memory budget: {:.1} MiB",
                    profile.memory_budget_bytes as f64 / 1_048_576.0
                );
                println!("  Vector quantization: {}", profile.vector_quantization);
                println!("  Embedding batch: {}", profile.recommended_embedding_batch);
                println!("  CUDA compiled: {}", profile.cuda_compiled);
                for gpu in profile.gpu_devices {
                    println!(
                        "  GPU {}: {} / {} MiB, {}%",
                        gpu.name,
                        gpu.used_memory_mib,
                        gpu.total_memory_mib,
                        gpu.utilization_percent
                    );
                }
            }
        }
        Commands::Models {
            profile,
            offline,
            embedding_only,
            reranker_only,
        } => {
            use findex_core::models::{ensure_model_for_profile, ModelKind};
            let kinds: Vec<_> = if *embedding_only {
                vec![ModelKind::Embedding]
            } else if *reranker_only {
                vec![ModelKind::Reranker]
            } else {
                vec![ModelKind::Embedding, ModelKind::Reranker]
            };
            let mut resolved = Vec::with_capacity(kinds.len());
            for kind in kinds {
                if format == OutputFormat::Text {
                    eprintln!("Resolving {kind:?} model...");
                }
                resolved.push(ensure_model_for_profile(kind, *profile, *offline)?);
            }
            match format {
                OutputFormat::Json => print_json(serde_json::to_value(&resolved)?)?,
                OutputFormat::Compact => {
                    for model in resolved {
                        println!(
                            "{}\t{:?}\t{}\t{}\t{}\t{}\t{}",
                            model.profile,
                            model.kind,
                            model.repository,
                            model.revision,
                            model.artifact,
                            model.model_path.display(),
                            model.tokenizer_path.display()
                        );
                    }
                }
                OutputFormat::Text => {
                    for model in resolved {
                        println!(
                            "{:?} ready ({})\n  repo: {}\n  revision: {}\n  artifact: {}\n  model: {}\n  tokenizer: {}",
                            model.kind,
                            model.profile,
                            model.repository,
                            model.revision,
                            model.artifact,
                            model.model_path.display(),
                            model.tokenizer_path.display()
                        );
                    }
                }
            }
        }
        Commands::Update { command } => match command {
            UpdateCommand::Check { cached } => {
                let check = findex_core::updater::check_for_update(!cached)?;
                match format {
                    OutputFormat::Json => print_json(serde_json::to_value(check)?)?,
                    OutputFormat::Compact => {
                        if !check.enabled {
                            println!("disabled");
                        } else if let Some(update) = check.available {
                            println!("available\t{}\t{}", update.version, update.target);
                        } else {
                            println!("current\t{}", check.current_version);
                        }
                    }
                    OutputFormat::Text => print_update_check(&check),
                }
            }
            UpdateCommand::Install { yes } => {
                let check = findex_core::updater::check_for_update(true)?;
                let Some(update) = check.available else {
                    if check.enabled {
                        println!("Findex {} is current.", check.current_version);
                    } else {
                        println!(
                            "Updates are disabled in this local build; packaged releases contain the signing key."
                        );
                    }
                    return Ok(());
                };
                if !yes && !confirm_update(&update)? {
                    println!("Update cancelled; no files were changed.");
                    return Ok(());
                }
                eprintln!(
                    "Downloading and verifying Findex {} for {}...",
                    update.version, update.target
                );
                findex_core::updater::install_update(&update)?;
                println!(
                    "Findex {} installed. Restart this process to use it.",
                    update.version
                );
            }
        },
        Commands::Settings { command } => {
            let settings = match command {
                SettingsCommand::Show => findex_core::settings::load(&cli.db_path)?,
                SettingsCommand::Reset => findex_core::settings::reset(&cli.db_path)?,
                SettingsCommand::Set {
                    lexical,
                    semantic,
                    reranking,
                    graph_expansion,
                    structural_prefetch,
                    stack_graphs,
                    watcher,
                    vfs_shadowing,
                    trace_pinning,
                    graph_hops,
                    candidates,
                    token_budget,
                    mmr_lambda,
                    compute,
                    model_profile,
                    memory_mib,
                    gpu_memory_mib,
                    idle_seconds,
                    theme,
                    motion,
                    graph_particles,
                    graph_labels,
                    predictive_query_cache,
                    query_cache_entries,
                    query_cache_ttl_seconds,
                    minimize_to_tray,
                    cursor_companion,
                    terminal_pointer_input,
                } => {
                    let mut settings = findex_core::settings::load(&cli.db_path)?;
                    if let Some(value) = lexical {
                        settings.indexing.lexical_index = *value;
                    }
                    if let Some(value) = semantic {
                        settings.indexing.semantic_index = *value;
                        settings.retrieval.semantic_search = *value;
                    }
                    if let Some(value) = reranking {
                        settings.retrieval.reranking = *value;
                    }
                    if let Some(value) = graph_expansion {
                        settings.retrieval.graph_expansion = *value;
                    }
                    if let Some(value) = structural_prefetch {
                        settings.retrieval.structural_prefetch = *value;
                    }
                    if let Some(value) = stack_graphs {
                        settings.indexing.stack_graphs = *value;
                    }
                    if let Some(value) = watcher {
                        settings.indexing.watcher = *value;
                    }
                    if let Some(value) = vfs_shadowing {
                        settings.indexing.vfs_shadowing = *value;
                    }
                    if let Some(value) = trace_pinning {
                        settings.indexing.execution_trace_pinning = *value;
                    }
                    if let Some(value) = graph_hops {
                        settings.retrieval.graph_hops = *value;
                    }
                    if let Some(value) = candidates {
                        settings.retrieval.candidate_limit = *value;
                    }
                    if let Some(value) = token_budget {
                        settings.retrieval.default_token_budget = *value;
                    }
                    if let Some(value) = mmr_lambda {
                        settings.retrieval.mmr_lambda = *value;
                    }
                    if let Some(value) = compute {
                        settings.runtime.compute_device = *value;
                    }
                    if let Some(value) = model_profile {
                        settings.runtime.model_profile = value.to_string();
                    }
                    if let Some(value) = memory_mib {
                        settings.runtime.memory_budget_mib = *value;
                    }
                    if let Some(value) = gpu_memory_mib {
                        settings.runtime.gpu_memory_limit_mib = *value;
                    }
                    if let Some(value) = idle_seconds {
                        settings.runtime.model_idle_seconds = *value;
                    }
                    if let Some(value) = theme {
                        settings.ui.theme = *value;
                    }
                    if let Some(value) = motion {
                        settings.ui.motion = *value;
                    }
                    if let Some(value) = graph_particles {
                        settings.ui.graph_particles = *value;
                    }
                    if let Some(value) = graph_labels {
                        settings.ui.graph_labels = *value;
                    }
                    if let Some(value) = predictive_query_cache {
                        settings.retrieval.predictive_query_cache = *value;
                    }
                    if let Some(value) = query_cache_entries {
                        settings.retrieval.query_cache_entries = *value;
                    }
                    if let Some(value) = query_cache_ttl_seconds {
                        settings.retrieval.query_cache_ttl_seconds = *value;
                    }
                    if let Some(value) = minimize_to_tray {
                        settings.ui.minimize_to_tray = *value;
                    }
                    if let Some(value) = cursor_companion {
                        settings.ui.cursor_companion = *value;
                    }
                    if let Some(value) = terminal_pointer_input {
                        settings.ui.terminal_pointer_input = *value;
                    }
                    findex_core::settings::save(&cli.db_path, settings)?
                }
            };
            findex_core::runtime::apply_runtime_settings(&settings);
            print_settings(&settings, format)?;
        }
        Commands::BuildVectors => {
            let storage = Storage::open(&cli.db_path)?;
            let start = Instant::now();
            build_vector_index(&cli.db_path, &storage)?;
            let elapsed_ms = start.elapsed().as_millis();
            if format == OutputFormat::Json {
                print_json(serde_json::json!({ "built": true, "elapsed_ms": elapsed_ms }))?;
            } else {
                println!("Vector index rebuilt in {} ms", elapsed_ms);
            }
        }
        Commands::SemanticDiff { file_a, file_b } => {
            let diff = semantic_diff_files(file_a, file_b)?;
            if format == OutputFormat::Json {
                print_json(serde_json::to_value(diff)?)?;
            } else if format == OutputFormat::Compact {
                println!("distance={} changes={}", diff.distance, diff.changes.len());
            } else {
                println!("Semantic distance: {}", diff.distance);
                println!("{} structural changes", diff.changes.len());
                println!("{}", serde_json::to_string_pretty(&diff.changes)?);
            }
        }
        Commands::Taint {
            source,
            label,
            depth,
        } => {
            let storage = Storage::open(&cli.db_path)?;
            let config = TaintConfig {
                max_hops: (*depth).min(16),
                ..TaintConfig::default()
            };
            let result = propagate_taint(&storage, &[(source.clone(), label.clone())], &config)?;
            if format == OutputFormat::Json {
                print_json(serde_json::to_value(result)?)?;
            } else if format == OutputFormat::Compact {
                println!(
                    "symbols={} edges={}",
                    result.tainted_symbols.len(),
                    result.tainted_edges.len()
                );
            } else {
                println!(
                    "Taint `{}` reached {} symbols across {} edges",
                    label,
                    result.tainted_symbols.len(),
                    result.tainted_edges.len()
                );
                for id in result.tainted_symbols.keys() {
                    println!("  {}", id);
                }
            }
        }
        Commands::SetupAgent {
            agent,
            force,
            dry_run,
        } => {
            let reports = agent_setup::setup(*agent, *force, *dry_run)?;
            match format {
                OutputFormat::Json => print_json(serde_json::to_value(reports)?)?,
                OutputFormat::Compact => {
                    for report in reports {
                        println!(
                            "{}\tchanged={}\tmcp={}\tskill={}",
                            report.agent, report.changed, report.mcp, report.skill
                        );
                    }
                }
                OutputFormat::Text => {
                    for report in reports {
                        println!("{}", report.agent);
                        println!("  MCP: {}", report.mcp);
                        println!("  skill: {}", report.skill);
                    }
                }
            }
        }
    }

    Ok(())
}

fn print_settings(
    settings: &findex_core::settings::FindexSettings,
    format: OutputFormat,
) -> anyhow::Result<()> {
    match format {
        OutputFormat::Json => print_json(serde_json::to_value(settings)?),
        OutputFormat::Compact => {
            println!("{}", serde_json::to_string(settings)?);
            Ok(())
        }
        OutputFormat::Text => {
            println!("Findex production settings");
            println!("{}", serde_json::to_string_pretty(settings)?);
            Ok(())
        }
    }
}

fn print_update_check(check: &findex_core::updater::UpdateCheck) {
    if !check.enabled {
        println!("Updates are disabled in this local build (no compiled signing public key).");
    } else if let Some(update) = &check.available {
        println!(
            "Findex {} is available (current {}).\nTarget: {}\n{}",
            update.version,
            check.current_version,
            update.target,
            update.notes.trim()
        );
        println!("Run `findex update install` to review and install it.");
    } else {
        println!("Findex {} is current.", check.current_version);
    }
}

fn confirm_update(update: &findex_core::updater::AvailableUpdate) -> anyhow::Result<bool> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("interactive confirmation requires a terminal; pass --yes to consent");
    }
    println!(
        "Install signed Findex {} for {}?\n{}",
        update.version,
        update.target,
        update.notes.trim()
    );
    print!("Continue [y/N]: ");
    std::io::stdout().flush()?;
    let mut response = String::new();
    std::io::stdin().read_line(&mut response)?;
    Ok(matches!(
        response.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn start_background_update_check() {
    if !findex_core::updater::updater_enabled() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("findex-update-check".to_string())
        .spawn(|| {
            let _ = findex_core::updater::check_for_update(false);
        });
}
