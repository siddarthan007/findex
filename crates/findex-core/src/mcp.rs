use crate::graph_pruning::prune_context;
use crate::graph_query::query_graph;
use crate::intelligence::{
    architecture_overview, ast_outline, build_context_bundle, graph_snapshot, impact_analysis,
};
use crate::mcp_tasks::{TaskManager, TaskStatus};
use crate::resolver::{
    expand_context, get_callees, get_callers, resolve_definition, resolve_references,
};
use crate::search::local_embedder::create_embedder;
use crate::search::rerank::{create_reranker, Reranker};
use crate::search::vector::Embedder;
use crate::skeleton::pagerank::PersonalizationConfig;
use crate::storage::Storage;
use crate::structural_locality::{predict_context, PredictContextOptions};
use crate::taint::{pin_execution_trace, pin_taint, propagate_taint, TaintConfig};
use crate::vfs::{micro_compile, Vfs, VfsSnapshot};
use crate::{
    get_codebase_skeleton, get_codebase_skeleton_with_personalization, get_file_skeleton,
    ingest_codebase, search_codebase_with_components, semantic_diff_files,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Ingestion error: {0}")]
    Ingestion(#[from] crate::IngestionError),
    #[error("Storage error: {0}")]
    Storage(#[from] crate::storage::StorageError),
    #[error("Graph query error: {0}")]
    GraphQuery(#[from] crate::graph_query::GraphQueryError),
    #[error("Parser error: {0}")]
    Parser(#[from] crate::parser::ParserError),
    #[error("VFS error: {0}")]
    Vfs(#[from] crate::vfs::VfsError),
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
    #[error("Unknown tool: {0}")]
    UnknownTool(String),
    #[error("Missing parameter: {0}")]
    MissingParameter(String),
}

/// Serves Findex code-intelligence tools over the Model Context Protocol (MCP)
/// via stdio JSON-RPC 2.0.
#[derive(Clone)]
pub struct McpServer {
    db_path: std::path::PathBuf,
    storage: Arc<Storage>,
    reranker: Arc<dyn Reranker>,
    embedder: Arc<dyn Embedder>,
    tasks: Arc<TaskManager>,
    vfs: Arc<Mutex<Vfs>>,
    vfs_persist: bool,
}

impl McpServer {
    pub fn open<P: AsRef<Path>>(db_path: P) -> Result<Self, McpError> {
        let db_path = db_path.as_ref().to_path_buf();
        let storage = Arc::new(Storage::open(&db_path)?);
        let reranker = create_reranker();
        let embedder = create_embedder(128);
        crate::runtime::start_model_idle_janitor(&embedder, &reranker);
        let tasks = Arc::new(TaskManager::new(storage.clone()));
        let vfs_persist = std::env::var("FINDEX_VFS_PERSIST").as_deref() == Ok("1");
        let mut vfs = Vfs::new();
        if vfs_persist {
            if let Some(snapshot) = storage.get_metadata::<VfsSnapshot>("vfs:shadow:v1")? {
                let report = vfs.restore_snapshot(snapshot);
                eprintln!(
                    "Restored {} VFS shadow file(s); {} skipped",
                    report.loaded, report.skipped
                );
            }
        }
        Ok(Self {
            db_path,
            storage,
            reranker,
            embedder,
            tasks,
            vfs: Arc::new(Mutex::new(vfs)),
            vfs_persist,
        })
    }

    pub fn run(&self) -> Result<(), McpError> {
        let stdin = io::stdin();
        let mut stdout = io::stdout().lock();
        let reader = stdin.lock();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let request: JsonRpcRequest = match serde_json::from_str(&line) {
                Ok(req) => req,
                Err(e) => {
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": { "code": -32700, "message": format!("Parse error: {}", e) }
                    });
                    Self::send(&mut stdout, &response)?;
                    continue;
                }
            };

            // JSON-RPC notifications are one-way messages and MUST NOT receive
            // a response. The previous implementation emitted a literal null.
            if request.id.is_none() {
                self.handle_notification(&request);
                continue;
            }

            let response = self.handle_request(request);
            Self::send(&mut stdout, &response)?;
        }

        Ok(())
    }

    /// Transport-neutral JSON-RPC entry point used by Streamable HTTP and
    /// embedded desktop hosts. Notifications intentionally return `None`.
    pub fn handle_json(&self, value: Value) -> Option<Value> {
        let request: JsonRpcRequest = match serde_json::from_value(value) {
            Ok(request) => request,
            Err(error) => {
                return Some(Self::error_response(
                    None,
                    -32600,
                    format!("Invalid request: {error}"),
                ))
            }
        };
        if request.id.is_none() {
            self.handle_notification(&request);
            None
        } else {
            Some(self.handle_request(request))
        }
    }

    fn handle_notification(&self, request: &JsonRpcRequest) {
        match request.method.as_str() {
            "notifications/initialized" | "notifications/cancelled" => {}
            _ => {
                // Unknown notifications are intentionally ignored per JSON-RPC.
            }
        }
    }

    fn send(stdout: &mut io::StdoutLock, value: &Value) -> Result<(), McpError> {
        let text = serde_json::to_string(value)?;
        stdout.write_all(text.as_bytes())?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
        Ok(())
    }

    fn handle_request(&self, request: JsonRpcRequest) -> Value {
        match request.method.as_str() {
            "initialize" => self.handle_initialize(&request),
            "tools/list" => self.handle_tools_list(&request),
            "tools/call" => self.handle_tool_call(&request),
            "tasks/get" => self.handle_task_get(&request),
            "tasks/result" => self.handle_task_result(&request),
            "tasks/list" => self.handle_task_list(&request),
            "tasks/cancel" => self.handle_task_cancel(&request),
            "resources/list" => self.handle_resources_list(&request),
            "resources/read" => self.handle_resources_read(&request),
            "prompts/list" => self.handle_prompts_list(&request),
            "prompts/get" => self.handle_prompts_get(&request),
            "ping" => json!({ "jsonrpc": "2.0", "id": request.id, "result": {} }),
            _ => Self::error_response(
                request.id,
                -32601,
                format!("Method not found: {}", request.method),
            ),
        }
    }

    fn handle_initialize(&self, request: &JsonRpcRequest) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": request.id,
            "result": {
                "protocolVersion": "2025-11-25",
                "serverInfo": {
                    "name": "findex-mcp",
                    "version": env!("CARGO_PKG_VERSION"),
                    "description": "Local hybrid codebase intelligence with exact source locations"
                },
                "capabilities": {
                    "tools": { "listChanged": false },
                    "resources": { "listChanged": false, "subscribe": false },
                    "prompts": { "listChanged": false },
                    "logging": {}
                    ,"tasks": {
                        "list": {},
                        "cancel": {},
                        "requests": { "tools": { "call": {} } }
                    }
                },
                "instructions": "For broad tasks, call get_context_bundle once with a strict token budget and read only its exact source ranges. For known identifiers, use lexical search and exact symbol navigation. Run impact_analysis before changing shared symbols; expand depth only when direct evidence is insufficient."
            }
        })
    }

    /// MCP resources: read-only, URI-addressed data that agents can fetch on demand
    /// rather than having it inlined into every tool result.
    fn handle_resources_list(&self, request: &JsonRpcRequest) -> Value {
        let files = match self.storage.list_files() {
            Ok(files) => files,
            Err(_) => {
                return Self::error_response(
                    request.id.clone(),
                    -32603,
                    "failed to list files".to_string(),
                )
            }
        };

        // Always-present synthetic resources.
        let mut resources = vec![
            json!({
                "uri": "findex://repo/map",
                "name": "Repo Map (1k token skeleton)",
                "description": "Personalized-PageRank elided codebase skeleton. Fetch for high-level orientation.",
                "mimeType": "text/plain"
            }),
            json!({
                "uri": "findex://tree",
                "name": "File Tree",
                "description": "All indexed files with sizes.",
                "mimeType": "text/plain"
            }),
            json!({
                "uri": "findex://stats",
                "name": "Index Statistics",
                "description": "Symbol/edge/vector counts.",
                "mimeType": "application/json"
            }),
        ];

        // One resource per indexed file so an agent can pull a skeleton on demand.
        for f in files.iter().take(2000) {
            let uri = format!("findex://file/{}", url_encode(&f.path.to_string_lossy()));
            resources.push(json!({
                "uri": uri,
                "name": f.path.to_string_lossy(),
                "description": "Signature skeleton for this file.",
                "mimeType": "text/plain"
            }));
        }

        json!({
            "jsonrpc": "2.0",
            "id": request.id,
            "result": { "resources": resources }
        })
    }

    fn handle_resources_read(&self, request: &JsonRpcRequest) -> Value {
        let uri = request
            .params
            .as_ref()
            .and_then(|p| p.get("uri"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let (mime, text) = match uri {
            "findex://repo/map" => match get_codebase_skeleton(&self.storage, 1024) {
                Ok(t) => ("text/plain", t),
                Err(e) => return Self::error_response(request.id.clone(), -32603, e.to_string()),
            },
            "findex://tree" => {
                let mut out = String::new();
                if let Ok(files) = self.storage.list_files() {
                    for f in files {
                        out.push_str(&format!("{}\t{}B\n", f.path.to_string_lossy(), f.size));
                    }
                }
                ("text/plain", out)
            }
            "findex://stats" => {
                let symbols = self.storage.list_symbols().map(|s| s.len()).unwrap_or(0);
                let edges = self.storage.list_edges().map(|e| e.len()).unwrap_or(0);
                let files = self.storage.list_files().map(|f| f.len()).unwrap_or(0);
                (
                    "application/json",
                    serde_json::to_string_pretty(&json!({
                        "symbols": symbols, "edges": edges, "files": files
                    }))
                    .unwrap_or_else(|_| "{}".to_string()),
                )
            }
            other if other.starts_with("findex://file/") => {
                let encoded = &other["findex://file/".len()..];
                let path = url_decode(encoded);
                match get_file_skeleton(&self.storage, Path::new(&path), 1024) {
                    Ok(t) => ("text/plain", t),
                    Err(e) => {
                        return Self::error_response(request.id.clone(), -32603, e.to_string())
                    }
                }
            }
            _ => {
                return Self::error_response(
                    request.id.clone(),
                    -32602,
                    format!("unknown resource uri: {}", uri),
                )
            }
        };

        json!({
            "jsonrpc": "2.0",
            "id": request.id,
            "result": {
                "contents": [{
                    "uri": uri,
                    "mimeType": mime,
                    "text": text
                }]
            }
        })
    }

    /// MCP prompts: pre-parameterized recipes that teach agents how to combine
    /// Findex tools for common tasks.
    fn handle_prompts_list(&self, request: &JsonRpcRequest) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": request.id,
            "result": {
                "prompts": [
                    {
                        "name": "understand_symbol",
                        "description": "Build context around a symbol before editing. Calls get_definition then expand_context depth=1, then reads only the returned line ranges.",
                        "arguments": [{
                            "name": "symbol",
                            "description": "Symbol name or reference to understand",
                            "required": true
                        }]
                    },
                    {
                        "name": "plan_refactor",
                        "description": "Orient before a refactor. Fetch repo_map, then locate affected symbols via search_code.",
                        "arguments": []
                    },
                    {
                        "name": "trace_call",
                        "description": "Trace who calls a function and what it calls, to scope a change.",
                        "arguments": [{
                            "name": "symbol_id",
                            "description": "Fully-qualified symbol ID",
                            "required": true
                        }]
                    }
                ]
            }
        })
    }

    fn handle_prompts_get(&self, request: &JsonRpcRequest) -> Value {
        let params = request
            .params
            .as_ref()
            .cloned()
            .unwrap_or_else(|| json!({}));
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let text = match name {
            "understand_symbol" => {
                let symbol = arguments.get("symbol").and_then(Value::as_str).unwrap_or("<symbol>");
                format!("Use Findex to understand `{}`: call get_definition, then expand_context with depth=1. Read only the returned source ranges, and expand further only if necessary.", symbol)
            }
            "plan_refactor" => "Fetch repo_map with a 1024-token budget, search for the affected behavior, then inspect callers and references before proposing the refactor.".to_string(),
            "trace_call" => {
                let symbol = arguments.get("symbol_id").and_then(Value::as_str).unwrap_or("<symbol_id>");
                format!("For `{}`, call get_callers and get_callees, then expand_context at depth=1. Summarize the execution boundary using exact file and line ranges.", symbol)
            }
            _ => return Self::error_response(request.id.clone(), -32602, format!("Unknown prompt: {}", name)),
        };

        json!({
            "jsonrpc": "2.0",
            "id": request.id,
            "result": {
                "description": format!("Findex workflow: {}", name),
                "messages": [{
                    "role": "user",
                    "content": { "type": "text", "text": text }
                }]
            }
        })
    }

    fn task_id(request: &JsonRpcRequest) -> Option<&str> {
        request
            .params
            .as_ref()
            .and_then(|params| params.get("taskId"))
            .and_then(Value::as_str)
    }

    fn handle_task_get(&self, request: &JsonRpcRequest) -> Value {
        let Some(task_id) = Self::task_id(request) else {
            return Self::error_response(request.id.clone(), -32602, "Missing taskId".to_string());
        };
        match self.tasks.get(task_id) {
            Some(task) => {
                json!({ "jsonrpc": "2.0", "id": request.id, "result": task.protocol_value() })
            }
            None => Self::error_response(request.id.clone(), -32602, "Task not found".to_string()),
        }
    }

    fn handle_task_result(&self, request: &JsonRpcRequest) -> Value {
        let Some(task_id) = Self::task_id(request) else {
            return Self::error_response(request.id.clone(), -32602, "Missing taskId".to_string());
        };
        match self.tasks.wait_terminal(task_id) {
            Some(task) if task.status == TaskStatus::Cancelled => {
                Self::error_response(request.id.clone(), -32603, "Task was cancelled".to_string())
            }
            Some(task) => json!({
                "jsonrpc": "2.0",
                "id": request.id,
                "result": task.result.unwrap_or_else(|| json!({
                    "content": [{ "type": "text", "text": task.status_message }],
                    "isError": true
                }))
            }),
            None => Self::error_response(request.id.clone(), -32602, "Task not found".to_string()),
        }
    }

    fn handle_task_list(&self, request: &JsonRpcRequest) -> Value {
        let tasks: Vec<_> = self
            .tasks
            .list()
            .into_iter()
            .take(100)
            .map(|task| task.protocol_value())
            .collect();
        json!({ "jsonrpc": "2.0", "id": request.id, "result": { "tasks": tasks } })
    }

    fn handle_task_cancel(&self, request: &JsonRpcRequest) -> Value {
        let Some(task_id) = Self::task_id(request) else {
            return Self::error_response(request.id.clone(), -32602, "Missing taskId".to_string());
        };
        match self.tasks.cancel(task_id) {
            Ok(task) => {
                json!({ "jsonrpc": "2.0", "id": request.id, "result": task.protocol_value() })
            }
            Err(error) => Self::error_response(request.id.clone(), -32602, error),
        }
    }

    fn handle_tools_list(&self, request: &JsonRpcRequest) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": request.id,
            "result": {
                "tools": [
                    {
                        "name": "search_code",
                        "description": "Search the codebase using lexical, semantic, or hybrid ranking. Returns symbols with file paths and line ranges.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "query": { "type": "string" },
                                "mode": { "type": "string", "enum": ["hybrid", "lexical", "semantic"], "default": "hybrid" },
                                "limit": { "type": "integer", "default": 10 }
                            },
                            "required": ["query"]
                        },
                        "execution": { "taskSupport": "optional" },
                        "outputSchema": { "type": "object" }
                    },
                    {
                        "name": "get_definition",
                        "description": "Resolve a symbol reference to its definition site.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "symbol": { "type": "string" },
                                "context": { "type": "string", "description": "Source symbol context ID (e.g. file.rs#symbol)" }
                            },
                            "required": ["symbol"]
                        }
                    },
                    {
                        "name": "get_references",
                        "description": "Find all references to a definition symbol ID.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "symbol_id": { "type": "string" }
                            },
                            "required": ["symbol_id"]
                        }
                    },
                    {
                        "name": "get_callers",
                        "description": "Locate all direct callers of a function symbol ID.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "symbol_id": { "type": "string" }
                            },
                            "required": ["symbol_id"]
                        }
                    },
                    {
                        "name": "get_callees",
                        "description": "Locate all direct callees of a function symbol ID.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "symbol_id": { "type": "string" }
                            },
                            "required": ["symbol_id"]
                        }
                    },
                    {
                        "name": "expand_context",
                        "description": "Perform a BFS graph expansion to assemble context around a symbol ID.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "symbol_id": { "type": "string" },
                                "depth": { "type": "integer", "default": 1 }
                            },
                            "required": ["symbol_id"]
                        }
                    },
                    {
                        "name": "graph_query",
                        "description": "Run a Cypher-like query on the code graph (e.g. MATCH (a)-[:Calls]->(b) WHERE a.name = 'main' RETURN a, b).",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "query": { "type": "string" }
                            },
                            "required": ["query"]
                        }
                    },
                    {
                        "name": "get_file_skeleton",
                        "description": "Return the signature skeleton of a single file, omitting bodies.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string" },
                                "token_budget": { "type": "integer", "default": 1024 }
                            },
                            "required": ["path"]
                        }
                    },
                    {
                        "name": "repo_map",
                        "description": "Generate an Aider-style elided codebase skeleton ranked by PageRank.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "token_budget": { "type": "integer", "default": 1024 },
                                "focal_symbols": { "type": "array", "items": { "type": "string" }, "default": [] },
                                "focal_files": { "type": "array", "items": { "type": "string" }, "default": [] }
                            }
                        },
                        "execution": { "taskSupport": "optional" },
                        "outputSchema": { "type": "object" }
                    },
                    {
                        "name": "reindex",
                        "description": "Re-ingest the codebase from a given root directory. Use when files have changed.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "root": { "type": "string" }
                            },
                            "required": ["root"]
                        },
                        "execution": { "taskSupport": "optional" },
                        "outputSchema": { "type": "object" }
                    },
                    {
                        "name": "semantic_diff",
                        "description": "Compare two files of the same tree-sitter-backed language and return structural AST changes.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "file_a": { "type": "string" },
                                "file_b": { "type": "string" }
                            },
                            "required": ["file_a", "file_b"]
                        },
                        "execution": { "taskSupport": "optional" },
                        "outputSchema": { "type": "object" }
                    },
                    {
                        "name": "taint_trace",
                        "description": "Trace a labeled source through call/reference/containment graph edges without modifying the index.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "source": { "type": "string", "description": "Exact symbol ID" },
                                "label": { "type": "string", "default": "user-input" },
                                "depth": { "type": "integer", "default": 3, "minimum": 0, "maximum": 16 },
                                "pin": { "type": "boolean", "default": false, "description": "Persist carried taint tags on traversed adjacency edges" }
                            },
                            "required": ["source"]
                        }
                    },
                    {
                        "name": "predict_context",
                        "description": "Rank symbols structurally likely to be relevant to one or more focal symbol IDs.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "symbol_ids": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                                "depth": { "type": "integer", "default": 2, "minimum": 1, "maximum": 8 },
                                "limit": { "type": "integer", "default": 20, "minimum": 1, "maximum": 100 }
                            },
                            "required": ["symbol_ids"]
                        }
                    },
                    {
                        "name": "prune_context",
                        "description": "Return the highest-value structural subgraph around explicit symbols within a strict token budget. Explicit seeds are never silently dropped.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "symbol_ids": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                                "token_budget": { "type": "integer", "default": 2048, "minimum": 64, "maximum": 32768 }
                            },
                            "required": ["symbol_ids"]
                        },
                        "outputSchema": { "type": "object" }
                    },
                    {
                        "name": "vfs_update",
                        "description": "Write or delete a bounded in-memory shadow file for speculative edits. Returns version/hash, eviction, and memory-budget state.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string" },
                                "content": { "type": "string" },
                                "delete": { "type": "boolean", "default": false }
                            },
                            "required": ["path"]
                        },
                        "outputSchema": { "type": "object" }
                    },
                    {
                        "name": "micro_compile",
                        "description": "Parse one VFS-shadowed file without disk I/O or persisted-index mutation; returns versioned symbols and relationships for edit validation.",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "path": { "type": "string" } },
                            "required": ["path"]
                        },
                        "outputSchema": { "type": "object" }
                    },
                    {
                        "name": "pin_execution_trace",
                        "description": "Attach a bounded runtime symbol path to adjacency edges. Unknown symbols never create phantom relationships and trace identities accumulate.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "trace_id": { "type": "string" },
                                "symbol_ids": { "type": "array", "items": { "type": "string" }, "minItems": 2 }
                            },
                            "required": ["trace_id", "symbol_ids"]
                        },
                        "outputSchema": { "type": "object" }
                    },
                    {
                        "name": "list_files",
                        "description": "List indexed files and their byte sizes.",
                        "inputSchema": { "type": "object", "properties": {} }
                    },
                    {
                        "name": "get_stats",
                        "description": "Return index file, symbol, edge, and vector counts.",
                        "inputSchema": { "type": "object", "properties": {} }
                    },
                    {
                        "name": "get_context_bundle",
                        "description": "Return a single token-bounded repo map plus exact source ranges ranked for a task. Designed to replace repeated search-and-read loops.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "query": { "type": "string" },
                                "mode": { "type": "string", "enum": ["hybrid", "lexical", "semantic"], "default": "hybrid" },
                                "token_budget": { "type": "integer", "default": 2048, "minimum": 128, "maximum": 32768 }
                            },
                            "required": ["query"]
                        },
                        "execution": { "taskSupport": "optional" },
                        "outputSchema": { "type": "object" }
                    },
                    {
                        "name": "impact_analysis",
                        "description": "Calculate fan-in, fan-out, callers, callees, references, affected files and God-node risk before editing a symbol.",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "symbol_id": { "type": "string" } },
                            "required": ["symbol_id"]
                        },
                        "outputSchema": { "type": "object" }
                    },
                    {
                        "name": "get_ast_outline",
                        "description": "Return the nested indexed AST/symbol outline for a file, including multi-language Vue SFC children.",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "path": { "type": "string" } },
                            "required": ["path"]
                        },
                        "outputSchema": { "type": "object" }
                    },
                    {
                        "name": "get_graph_snapshot",
                        "description": "Return a bounded, degree-ranked code graph for visualization or structural planning. Nodes are classified as God/UI/API/code.",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "limit": { "type": "integer", "default": 1000, "minimum": 1, "maximum": 10000 } }
                        },
                        "outputSchema": { "type": "object" }
                    },
                    {
                        "name": "get_architecture_overview",
                        "description": "Return a compact source-free architecture digest: languages, layers, contracts, entrypoints, cross-file coupling, and high-degree hubs.",
                        "inputSchema": { "type": "object", "properties": {} },
                        "outputSchema": { "type": "object" }
                    },
                    {
                        "name": "get_runtime_profile",
                        "description": "Inspect CPU, RAM, process memory, configured budgets, vector quantization and NVIDIA GPU state.",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "include_gpu": { "type": "boolean", "default": true } }
                        },
                        "outputSchema": { "type": "object" }
                    }
                ]
            }
        })
    }

    fn handle_tool_call(&self, request: &JsonRpcRequest) -> Value {
        let params = match request.params.as_ref() {
            Some(p) => p,
            None => {
                return Self::error_response(
                    request.id.clone(),
                    -32602,
                    "Missing params".to_string(),
                )
            }
        };

        let name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => {
                return Self::error_response(
                    request.id.clone(),
                    -32602,
                    "Missing tool name".to_string(),
                )
            }
        };

        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        if let Some(task_options) = params.get("task") {
            return self.handle_task_tool_call(request, name, args, task_options);
        }

        let start = std::time::Instant::now();
        let result = self.execute_tool(name, &args);
        let elapsed_ms = start.elapsed().as_millis();

        match result {
            Ok(content) => json!({
                "jsonrpc": "2.0",
                "id": request.id,
                "result": Self::tool_success_result(name, content, elapsed_ms)
            }),
            Err(McpError::UnknownTool(name)) => Self::error_response(
                request.id.clone(),
                -32602,
                format!("Unknown tool: {}", name),
            ),
            Err(error) => json!({
                "jsonrpc": "2.0",
                "id": request.id,
                "result": Self::tool_error_result(name, &error, elapsed_ms)
            }),
        }
    }

    fn execute_tool(&self, name: &str, args: &Value) -> Result<String, McpError> {
        match name {
            "search_code" => self.tool_search_code(args),
            "get_definition" => self.tool_get_definition(args),
            "get_references" => self.tool_get_references(args),
            "get_callers" => self.tool_get_callers(args),
            "get_callees" => self.tool_get_callees(args),
            "expand_context" => self.tool_expand_context(args),
            "graph_query" => self.tool_graph_query(args),
            "get_file_skeleton" => self.tool_get_file_skeleton(args),
            "repo_map" => self.tool_repo_map(args),
            "reindex" => self.tool_reindex(args),
            "semantic_diff" => self.tool_semantic_diff(args),
            "taint_trace" => self.tool_taint_trace(args),
            "predict_context" => self.tool_predict_context(args),
            "prune_context" => self.tool_prune_context(args),
            "vfs_update" => self.tool_vfs_update(args),
            "micro_compile" => self.tool_micro_compile(args),
            "pin_execution_trace" => self.tool_pin_execution_trace(args),
            "list_files" => self.tool_list_files(),
            "get_stats" => self.tool_get_stats(),
            "get_context_bundle" => self.tool_get_context_bundle(args),
            "impact_analysis" => self.tool_impact_analysis(args),
            "get_ast_outline" => self.tool_get_ast_outline(args),
            "get_graph_snapshot" => self.tool_get_graph_snapshot(args),
            "get_architecture_overview" => self.tool_get_architecture_overview(),
            "get_runtime_profile" => self.tool_get_runtime_profile(args),
            other => Err(McpError::UnknownTool(other.to_string())),
        }
    }

    fn tool_success_result(name: &str, content: String, elapsed_ms: u128) -> Value {
        let data = serde_json::from_str::<Value>(&content).unwrap_or_else(|_| {
            json!({
                "text": content.clone()
            })
        });
        json!({
            "content": [{
                "type": "text",
                "text": format!("[findex:{} {}ms]\n{}", name, elapsed_ms, content)
            }],
            "structuredContent": {
                "tool": name,
                "elapsed_ms": elapsed_ms,
                "data": data
            }
        })
    }

    fn tool_error_result(name: &str, error: &McpError, elapsed_ms: u128) -> Value {
        json!({
            "content": [{ "type": "text", "text": error.to_string() }],
            "structuredContent": {
                "tool": name,
                "elapsed_ms": elapsed_ms,
                "error": error.to_string()
            },
            "isError": true
        })
    }

    fn handle_task_tool_call(
        &self,
        request: &JsonRpcRequest,
        name: &str,
        args: Value,
        task_options: &Value,
    ) -> Value {
        if !matches!(
            name,
            "search_code" | "repo_map" | "reindex" | "semantic_diff" | "get_context_bundle"
        ) {
            return Self::error_response(
                request.id.clone(),
                -32601,
                format!("tool {name} does not support task execution"),
            );
        }
        let ttl = task_options.get("ttl").and_then(Value::as_u64);
        let task = match self.tasks.create(name, ttl) {
            Ok(task) => task,
            Err(error) => return Self::error_response(request.id.clone(), -32603, error),
        };
        let task_id = task.task_id.clone();
        let server = self.clone();
        let tool_name = name.to_string();
        std::thread::Builder::new()
            .name(format!("findex-task-{}", &task_id[..8]))
            .spawn(move || {
                let started = std::time::Instant::now();
                let execution = server.execute_tool(&tool_name, &args);
                let elapsed = started.elapsed().as_millis();
                let (mut result, failed) = match execution {
                    Ok(content) => (
                        Self::tool_success_result(&tool_name, content, elapsed),
                        false,
                    ),
                    Err(error) => (Self::tool_error_result(&tool_name, &error, elapsed), true),
                };
                if let Some(object) = result.as_object_mut() {
                    object.insert(
                        "_meta".to_string(),
                        json!({ "io.modelcontextprotocol/related-task": { "taskId": task_id } }),
                    );
                }
                server.tasks.complete(&task_id, result, failed);
            })
            .ok();
        json!({
            "jsonrpc": "2.0",
            "id": request.id,
            "result": { "task": task.protocol_value() }
        })
    }

    fn error_response(id: Option<Value>, code: i32, message: String) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id.unwrap_or(Value::Null),
            "error": { "code": code, "message": message }
        })
    }

    // --- Tool implementations ---

    fn tool_search_code(&self, args: &Value) -> Result<String, McpError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::MissingParameter("query".to_string()))?;
        let mode = args
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("hybrid");
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

        let results = search_codebase_with_components(
            &self.db_path,
            &self.storage,
            query,
            mode,
            Some(self.reranker.as_ref()),
            self.embedder.as_ref(),
            limit.clamp(1, 100),
        )?;
        let mut lines = Vec::new();
        for (idx, (sym, score)) in results.iter().enumerate() {
            lines.push(format!(
                "{}. [Score: {:.4}] [{}] {} -> {}:{}-{}",
                idx + 1,
                score,
                sym.kind,
                sym.name,
                sym.file_path,
                sym.start_line,
                sym.end_line
            ));
            lines.push(format!("   Signature: {}", sym.signature));
        }
        Ok(lines.join("\n"))
    }

    fn tool_get_definition(&self, args: &Value) -> Result<String, McpError> {
        let symbol = args
            .get("symbol")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::MissingParameter("symbol".to_string()))?;
        let context = args.get("context").and_then(|v| v.as_str()).unwrap_or("");

        match resolve_definition(symbol, context, &self.storage)? {
            Some(sym) => Ok(format!(
                "[{}] {} -> {}:{}-{}\nSignature: {}",
                sym.kind, sym.name, sym.file_path, sym.start_line, sym.end_line, sym.signature
            )),
            None => Ok(format!("No definition found for: {}", symbol)),
        }
    }

    fn tool_get_references(&self, args: &Value) -> Result<String, McpError> {
        let symbol_id = args
            .get("symbol_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::MissingParameter("symbol_id".to_string()))?;
        let refs = resolve_references(symbol_id, &self.storage)?;
        let mut lines = vec![format!("Found {} references to {}", refs.len(), symbol_id)];
        for (idx, sym) in refs.iter().enumerate() {
            lines.push(format!(
                "{}. [{}] {} in {}:{}-{}",
                idx + 1,
                sym.kind,
                sym.name,
                sym.file_path,
                sym.start_line,
                sym.end_line
            ));
        }
        Ok(lines.join("\n"))
    }

    fn tool_get_callers(&self, args: &Value) -> Result<String, McpError> {
        let symbol_id = args
            .get("symbol_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::MissingParameter("symbol_id".to_string()))?;
        let callers = get_callers(symbol_id, &self.storage)?;
        let mut lines = vec![format!("Callers of {}:", symbol_id)];
        for sym in callers {
            lines.push(format!(
                "  [{}] {} in {}:{}-{}",
                sym.kind, sym.name, sym.file_path, sym.start_line, sym.end_line
            ));
        }
        Ok(lines.join("\n"))
    }

    fn tool_get_callees(&self, args: &Value) -> Result<String, McpError> {
        let symbol_id = args
            .get("symbol_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::MissingParameter("symbol_id".to_string()))?;
        let callees = get_callees(symbol_id, &self.storage)?;
        let mut lines = vec![format!("Callees of {}:", symbol_id)];
        for sym in callees {
            lines.push(format!(
                "  [{}] {} in {}:{}-{}",
                sym.kind, sym.name, sym.file_path, sym.start_line, sym.end_line
            ));
        }
        Ok(lines.join("\n"))
    }

    fn tool_expand_context(&self, args: &Value) -> Result<String, McpError> {
        let symbol_id = args
            .get("symbol_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::MissingParameter("symbol_id".to_string()))?;
        let depth = args
            .get("depth")
            .and_then(|v| v.as_u64())
            .unwrap_or(1)
            .min(8) as u32;

        let expanded = expand_context(symbol_id, depth, &self.storage)?;
        let mut lines = vec![format!("Context expansion (depth={}):", depth)];
        for sym in expanded {
            lines.push(format!(
                "  [{}] {} in {}:{}-{}",
                sym.kind, sym.name, sym.file_path, sym.start_line, sym.end_line
            ));
        }
        Ok(lines.join("\n"))
    }

    fn tool_graph_query(&self, args: &Value) -> Result<String, McpError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::MissingParameter("query".to_string()))?;
        let result = query_graph(&self.storage, query)?;
        Ok(result.to_text())
    }

    fn tool_get_file_skeleton(&self, args: &Value) -> Result<String, McpError> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::MissingParameter("path".to_string()))?;
        let budget = args
            .get("token_budget")
            .and_then(|v| v.as_u64())
            .unwrap_or(1024)
            .clamp(64, 32_768) as usize;
        let skeleton = get_file_skeleton(&self.storage, Path::new(path), budget)?;
        Ok(skeleton)
    }

    fn tool_repo_map(&self, args: &Value) -> Result<String, McpError> {
        let budget = args
            .get("token_budget")
            .and_then(Value::as_u64)
            .unwrap_or(1024)
            .clamp(64, 32_768) as usize;
        let string_array = |name: &str| {
            args.get(name)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let config = PersonalizationConfig {
            mentioned_symbols: string_array("focal_symbols"),
            focal_files: string_array("focal_files"),
            boost_well_named: true,
        };
        let skeleton = get_codebase_skeleton_with_personalization(&self.storage, budget, &config)?;
        Ok(skeleton)
    }

    fn tool_reindex(&self, args: &Value) -> Result<String, McpError> {
        let root = args
            .get("root")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::MissingParameter("root".to_string()))?;
        let stats = ingest_codebase(root, &self.db_path, &self.storage)?;
        Ok(format!(
            "Re-index complete: {} files, {} changed, {} deleted, {} ms",
            stats.total_files, stats.parsed_files, stats.deleted_files, stats.duration_ms
        ))
    }

    fn tool_semantic_diff(&self, args: &Value) -> Result<String, McpError> {
        let file_a = args
            .get("file_a")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::MissingParameter("file_a".to_string()))?;
        let file_b = args
            .get("file_b")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::MissingParameter("file_b".to_string()))?;
        Ok(serde_json::to_string_pretty(&semantic_diff_files(
            file_a, file_b,
        )?)?)
    }

    fn tool_taint_trace(&self, args: &Value) -> Result<String, McpError> {
        let source = args
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::MissingParameter("source".to_string()))?;
        if self.storage.get_symbol(source)?.is_none() {
            return Err(McpError::InvalidRequest(format!(
                "unknown symbol id: {}",
                source
            )));
        }
        let label = args
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("user-input")
            .to_string();
        let config = TaintConfig {
            max_hops: args
                .get("depth")
                .and_then(Value::as_u64)
                .unwrap_or(3)
                .min(16) as u32,
            ..TaintConfig::default()
        };
        let result = propagate_taint(&self.storage, &[(source.to_string(), label)], &config)?;
        if args.get("pin").and_then(Value::as_bool).unwrap_or(false) {
            pin_taint(&self.storage, &result)?;
        }
        Ok(serde_json::to_string_pretty(&result)?)
    }

    fn tool_predict_context(&self, args: &Value) -> Result<String, McpError> {
        let symbol_ids: Vec<String> = args
            .get("symbol_ids")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if symbol_ids.is_empty() {
            return Err(McpError::MissingParameter("symbol_ids".to_string()));
        }
        let options = PredictContextOptions {
            max_hops: args
                .get("depth")
                .and_then(Value::as_u64)
                .unwrap_or(2)
                .clamp(1, 8) as u32,
            max_results: args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .clamp(1, 100) as usize,
            ..PredictContextOptions::default()
        };
        Ok(serde_json::to_string_pretty(&predict_context(
            &self.storage,
            &symbol_ids,
            &options,
        )?)?)
    }

    fn tool_prune_context(&self, args: &Value) -> Result<String, McpError> {
        let symbol_ids: Vec<String> = args
            .get("symbol_ids")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if symbol_ids.is_empty() {
            return Err(McpError::MissingParameter("symbol_ids".to_string()));
        }
        let budget = args
            .get("token_budget")
            .and_then(Value::as_u64)
            .unwrap_or(2_048)
            .clamp(64, 32_768) as usize;
        Ok(serde_json::to_string_pretty(&prune_context(
            &self.storage,
            &symbol_ids,
            budget,
        )?)?)
    }

    fn tool_vfs_update(&self, args: &Value) -> Result<String, McpError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::MissingParameter("path".to_string()))?;
        let mut vfs = self
            .vfs
            .lock()
            .map_err(|_| McpError::InvalidRequest("VFS lock was poisoned".to_string()))?;
        let deleted = args.get("delete").and_then(Value::as_bool).unwrap_or(false);
        let result = if deleted {
            json!({ "path": path, "deleted": vfs.remove(path).is_some() })
        } else {
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| McpError::MissingParameter("content".to_string()))?;
            serde_json::to_value(vfs.put(path, content.to_string())?)?
        };
        if self.vfs_persist {
            self.storage
                .set_metadata("vfs:shadow:v1", &vfs.export_snapshot())?;
        }
        Ok(serde_json::to_string_pretty(&json!({
            "result": result,
            "vfs": vfs.stats()
        }))?)
    }

    fn tool_micro_compile(&self, args: &Value) -> Result<String, McpError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::MissingParameter("path".to_string()))?;
        let vfs = self
            .vfs
            .lock()
            .map_err(|_| McpError::InvalidRequest("VFS lock was poisoned".to_string()))?;
        Ok(serde_json::to_string_pretty(&micro_compile(path, &vfs)?)?)
    }

    fn tool_pin_execution_trace(&self, args: &Value) -> Result<String, McpError> {
        let trace_id = args
            .get("trace_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| McpError::MissingParameter("trace_id".to_string()))?;
        let symbol_ids: Vec<String> = args
            .get("symbol_ids")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if symbol_ids.len() < 2 {
            return Err(McpError::InvalidRequest(
                "symbol_ids must contain at least two symbols".to_string(),
            ));
        }
        Ok(serde_json::to_string_pretty(&pin_execution_trace(
            &self.storage,
            trace_id,
            &symbol_ids,
        )?)?)
    }

    fn tool_list_files(&self) -> Result<String, McpError> {
        Ok(serde_json::to_string_pretty(&self.storage.list_files()?)?)
    }

    fn tool_get_stats(&self) -> Result<String, McpError> {
        let files = self.storage.list_files()?.len();
        let symbols = self.storage.list_symbols()?.len();
        let edges = self.storage.list_edges()?.len();
        let vector_dir = self.db_path.join("vector");
        let vectors = if vector_dir.exists() {
            crate::search::vector::VectorIndex::open_or_create_with_quantization(
                vector_dir,
                self.embedder.dimension(),
                std::env::var("FINDEX_VECTOR_QUANTIZATION")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or_default(),
            )
            .map(|index| index.size())
            .unwrap_or(0)
        } else {
            0
        };
        Ok(serde_json::to_string_pretty(&json!({
            "files": files,
            "symbols": symbols,
            "edges": edges,
            "vectors": vectors,
            "merkle": self.storage.get_metadata::<crate::merkle::MerkleSnapshot>("merkle:v1")?
                .map(|snapshot| snapshot.root_hash_hex()),
            "stack_graphs": self.storage.get_metadata::<crate::stack_graphs::StackGraphStats>("stack-graphs:last")?,
            "runtime": crate::runtime::profile(false)
        }))?)
    }

    fn tool_get_context_bundle(&self, args: &Value) -> Result<String, McpError> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::MissingParameter("query".to_string()))?;
        let mode = args.get("mode").and_then(Value::as_str).unwrap_or("hybrid");
        let budget = args
            .get("token_budget")
            .and_then(Value::as_u64)
            .unwrap_or(2048)
            .clamp(128, 32_768) as usize;
        let bundle = build_context_bundle(
            &self.db_path,
            &self.storage,
            query,
            mode,
            budget,
            Some(self.reranker.as_ref()),
            self.embedder.as_ref(),
        )?;
        Ok(serde_json::to_string_pretty(&bundle)?)
    }

    fn tool_impact_analysis(&self, args: &Value) -> Result<String, McpError> {
        let symbol_id = args
            .get("symbol_id")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::MissingParameter("symbol_id".to_string()))?;
        Ok(serde_json::to_string_pretty(&impact_analysis(
            &self.storage,
            symbol_id,
        )?)?)
    }

    fn tool_get_ast_outline(&self, args: &Value) -> Result<String, McpError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::MissingParameter("path".to_string()))?;
        Ok(serde_json::to_string_pretty(&ast_outline(
            &self.storage,
            Path::new(path),
        )?)?)
    }

    fn tool_get_graph_snapshot(&self, args: &Value) -> Result<String, McpError> {
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(1000)
            .clamp(1, 10_000) as usize;
        Ok(serde_json::to_string(&graph_snapshot(
            &self.storage,
            limit,
        )?)?)
    }

    fn tool_get_architecture_overview(&self) -> Result<String, McpError> {
        Ok(serde_json::to_string_pretty(&architecture_overview(
            &self.storage,
        )?)?)
    }

    fn tool_get_runtime_profile(&self, args: &Value) -> Result<String, McpError> {
        let include_gpu = args
            .get("include_gpu")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        Ok(serde_json::to_string_pretty(&crate::runtime::profile(
            include_gpu,
        ))?)
    }
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

/// Minimal percent-encoding for resource URIs (file paths may contain spaces,
/// backslashes, or unicode). Only the unreserved set and `/` are left alone.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn url_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::rerank::MockReranker;
    use crate::search::vector::MockEmbedder;
    use tempfile::TempDir;

    fn test_server() -> (TempDir, McpServer) {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("db");
        let storage = Arc::new(Storage::open(&db_path).unwrap());
        let server = McpServer {
            db_path: db_path.clone(),
            storage: storage.clone(),
            reranker: Arc::new(MockReranker),
            embedder: Arc::new(MockEmbedder::new(128)),
            tasks: Arc::new(TaskManager::new(storage)),
            vfs: Arc::new(Mutex::new(Vfs::new())),
            vfs_persist: false,
        };
        (directory, server)
    }

    fn request(method: &str, params: Option<Value>) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: method.to_string(),
            params,
        }
    }

    #[test]
    fn test_url_roundtrip() {
        for s in &[
            "src/main.rs",
            "a b/c.dart",
            "C:\\Users\\x y\\f.rs",
            "ünïcode/Path",
        ] {
            let enc = url_encode(s);
            assert!(!enc.contains(' '), "encoded must have no spaces: {}", enc);
            let dec = url_decode(&enc);
            assert_eq!(&dec, s, "roundtrip failed for {}", s);
        }
    }

    #[test]
    fn initialize_advertises_current_protocol_and_capabilities() {
        let (_directory, server) = test_server();
        let response = server.handle_request(request("initialize", Some(json!({}))));
        assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(response["result"]["serverInfo"]["version"], "0.3.0");
        assert!(response["result"]["capabilities"]["tools"].is_object());
        assert!(response["result"]["capabilities"]["resources"].is_object());
        assert!(response["result"]["capabilities"]["prompts"].is_object());
    }

    #[test]
    fn tool_list_includes_extended_code_intelligence_surface() {
        let (_directory, server) = test_server();
        let response = server.handle_request(request("tools/list", None));
        let names: Vec<&str> = response["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        for expected in [
            "search_code",
            "repo_map",
            "semantic_diff",
            "taint_trace",
            "predict_context",
            "prune_context",
            "vfs_update",
            "micro_compile",
            "pin_execution_trace",
            "list_files",
            "get_stats",
            "get_context_bundle",
            "impact_analysis",
            "get_ast_outline",
            "get_graph_snapshot",
            "get_architecture_overview",
            "get_runtime_profile",
        ] {
            assert!(names.contains(&expected), "missing MCP tool {expected}");
        }
    }

    #[test]
    fn prompts_get_returns_recipe_and_unknown_prompt_is_protocol_error() {
        let (_directory, server) = test_server();
        let response = server.handle_request(request(
            "prompts/get",
            Some(json!({
                "name": "understand_symbol",
                "arguments": { "symbol": "ingest_codebase" }
            })),
        ));
        assert!(response["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("ingest_codebase"));

        let unknown = server.handle_request(request(
            "prompts/get",
            Some(json!({ "name": "not-a-prompt" })),
        ));
        assert_eq!(unknown["error"]["code"], -32602);
    }

    #[test]
    fn tool_execution_failures_use_is_error_but_unknown_tools_use_protocol_error() {
        let (_directory, server) = test_server();
        let missing_query = server.handle_request(request(
            "tools/call",
            Some(json!({ "name": "search_code", "arguments": {} })),
        ));
        assert_eq!(missing_query["result"]["isError"], true);
        assert!(missing_query["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("query"));

        let unknown = server.handle_request(request(
            "tools/call",
            Some(json!({ "name": "not-a-tool", "arguments": {} })),
        ));
        assert_eq!(unknown["error"]["code"], -32602);
    }

    #[test]
    fn task_augmented_tool_call_completes_and_returns_original_tool_result() {
        let (_directory, server) = test_server();
        let created = server.handle_request(request(
            "tools/call",
            Some(json!({
                "name": "repo_map",
                "arguments": { "token_budget": 128 },
                "task": { "ttl": 30_000 }
            })),
        ));
        let task_id = created["result"]["task"]["taskId"]
            .as_str()
            .expect("task id");
        assert_eq!(created["result"]["task"]["status"], "working");

        let result =
            server.handle_request(request("tasks/result", Some(json!({ "taskId": task_id }))));
        assert!(result["result"]["content"].is_array());
        assert_eq!(
            result["result"]["_meta"]["io.modelcontextprotocol/related-task"]["taskId"],
            task_id
        );
        let completed =
            server.handle_request(request("tasks/get", Some(json!({ "taskId": task_id }))));
        assert_eq!(completed["result"]["status"], "completed");
    }

    #[test]
    fn unsupported_task_execution_is_a_protocol_error() {
        let (_directory, server) = test_server();
        let response = server.handle_request(request(
            "tools/call",
            Some(json!({
                "name": "get_stats",
                "arguments": {},
                "task": { "ttl": 30_000 }
            })),
        ));
        assert_eq!(response["error"]["code"], -32601);
    }
}
