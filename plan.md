# Findex Constitution: Architectural Blueprint & Coding Standards

This document serves as the absolute technical constitution for the development of **Findex**, a blazingly-fast, model-agnostic codebase intelligence engine. All AI coding agents, developers, and compilers working on this repository must adhere strictly to the rules, designs, and standards defined here.

---

## 1. System Environment & Hardware Configuration

Findex is designed to run locally on a developer workstation. The target environment specs must dictate the parallelization, memory mapping, and GPU compute limits:

| Hardware Component | Spec | Constraints & Mapping |
| :--- | :--- | :--- |
| **Operating System** | Windows 11 | - Native Windows file paths (`\` and `\\`) handled via `std::path::Path`<br>- Support for Windows asynchronous file operations<br>- Proper handling of locked files and file watcher constraints |
| **CPU** | Intel Core i7 14700HX<br>(20 cores / 28 threads:<br>8 P-cores, 12 E-cores) | - Rayon threads pool configured to prevent starvation of the UI thread<br>- CPU-intensive tasks (parsing, index building) isolated to 24-26 threads<br>- Non-blocking tokio runtime for I/O and MCP transport |
| **System RAM** | 16 GB DDR5 | - Strict prevention of memory leaks (no unbounded caches)<br>- Disk-backed indexes with `memmap2` zero-copy I/O to stay within RAM limits<br>- Heap memory reuse via arenas (`bumpalo` in `oxc`, recycling in parsers) |
| **GPU** | NVIDIA RTX 5060 (8 GB VRAM) | - Batch size limit on embeddings generation to prevent VRAM overflow<br>- Candle / ONNX Runtime (ORT) configured for CUDA execution provider<br>- Fallback to CPU-bound inference if VRAM usage exceeds 90% |

---

## 2. Coding Standards & Rust Norms

### 2.1 Code Safety
- **No Unsafe Code by Default**: All crates must declare `#![deny(unsafe_code)]` unless interacting with OS memory mappings (`memmap2`) or low-level FFI bindings (usearch, C++ wrappers).
- **Documented Unsafe Invariants**: Any `unsafe` block must be accompanied by a `// SAFETY:` comment explaining why the operation is guaranteed to be safe and how invariants are preserved.

### 2.2 Lifetime and Allocation Optimization
- **Zero-Copy Deserialization**: Use `serde`'s lifetime features (`#[serde(borrow)]` and `&'a str` / `&'a [u8]`) to parse indices directly from memory-mapped files without heap allocations.
- **Arena Allocations**: For AST modifications and compiler-like parsing passes, use arena allocators (`bumpalo`) to allocate nodes in contiguous memory chunks and drop them instantly.

### 2.3 Concurrency and Parallelism
- **Rayon for Data Parallelism**: Use Rayon work-stealing for CPU-bound computations (file traversal, Blake3 hashing, AST parse jobs).
- **Tokio for Async I/O**: Use Tokio only for async I/O tasks (file watching, MCP stdio/HTTP transports, network database access). Do not mix CPU-intensive blocking calls in Tokio threads without `tokio::task::spawn_blocking`.

### 2.4 Error Handling
- Use the `thiserror` crate for writing library crates to expose precise, typed, action-guiding error enums.
- Use `anyhow` or `miette` only in the main application binary, CLI, and MCP entry point for clean diagnostic rendering.

---

## 3. SCIP-Based Code Intelligence Schema

Findex utilizes a SCIP (Sourcegraph Code Intelligence Protocol) inspired data layout. This protocol replaces LSIF, reducing payload size by 8× and boosting processing speeds.

### SCIP Entity Schema (RocksDB / sled Tables)

```mermaid
erDiagram
    Files ||--o{ Symbols : contains
    Symbols ||--o{ Edges : references
    Symbols ||--o{ Chunks : partitioned_into
    Chunks ||--|| Vectors : represents

    Files {
        string path PK
        string folder
        string blake3_hash
        uint64 last_modified
    }

    Symbols {
        string symbol_id PK
        string kind
        string signature
        string file_path FK
        int start_line
        int start_col
        int end_line
        int end_col
        string docstring
    }

    Edges {
        string src_symbol_id FK
        string dst_symbol_id FK
        string edge_type "calls | imports | defines | inherits | references"
    }

    Chunks {
        string chunk_id PK
        string symbol_id FK
        int start_line
        int end_line
        string text_content
    }

    Vectors {
        string chunk_id PK FK
        float_array values "USearch f32, BF16, I8, or B1 scalar storage"
    }
```

---

## 4. Deep-Dive Ingestion Pipeline

Ingestion is modeled on Salsa's incremental query system. The input database registers file modifications using a Merkle tree diffing engine.

```
Source Files ──► Merkle Tree Diff ──► Changed Files Only ──► Rayon Parallel Parse ──► per-file CST & Symbol Extraction ──► stack-graph Subgraphs ──► sled / Tantivy / USearch
```

### 4.1 Parser Layering by Language
We support target languages using a hybrid parsing approach:

1. **JavaScript, TypeScript, TSX, React, Vue**:
   Parsed using **oxc** (the fastest TypeScript parser in Rust) with direct arena allocation.

   *Oxc Parser Recycling Code Standard*:
   ```rust
   use oxc::allocator::Allocator;
   use oxc::parser::{Parser, ParserReturn};
   use oxc::span::SourceType;
   use std::path::Path;

   pub fn parse_js_ts_files(files: &[(&Path, &str)]) {
       // Create a single allocator instance and recycle it to stabilize capacity
       let mut allocator = Allocator::default();

       for (path, source_text) in files {
           let source_type = SourceType::from_path(path)
               .unwrap_or_else(|_| SourceType::mjs());

           let parser_return = Parser::new(&allocator, source_text, source_type).parse();

           if !parser_return.diagnostics.is_empty() {
               // Log parser diagnostics, handle recoverable errors
           }

           let program = parser_return.program;
           // Process program AST nodes...

           // Reset the allocator to clear memory for the next file iteration
           allocator.reset();
       }
   }
   ```

2. **Python, Rust, Dart, Flutter, C, C++, HTML, CSS**:
   Parsed using **tree-sitter** grammars. Extraction of definitions and references utilizes tree-sitter Query files (`.scm`).

### 4.2 Incremental Recomputation (Salsa)
The ingestion state must be modeled using `salsa` to cache AST evaluations and graph steps.

*Salsa Ingestion Query Setup*:
```rust
#[salsa::input(debug)]
pub struct SourceFile {
    #[returns(ref)]
    pub path: String,
    #[returns(ref)]
    pub text: String,
}

#[salsa::tracked]
pub fn parse_ast(db: &dyn IngestionDb, file: SourceFile) -> ParsedAST {
    let source_text = file.text(db);
    let path = file.path(db);
    // Execute tree-sitter or oxc parsing
    // Return structured AST definitions and references
    ParsedAST::new(db, ...)
}

#[salsa::db]
#[derive(Clone, Default)]
pub struct IngestionDatabase {
    storage: salsa::Storage<Self>,
}
```

### 4.3 Name Resolution via Stack-Graphs
To maintain file-incremental correctness:
- Construct local symbol resolution subgraphs for each file.
- Perform path-finding on the combined stack-graph at query time.
- Avoid building a monolithic, static graph database, as it destroys incremental performance.

---

## 5. Storage & Vector Compression Layout

Findex combines three distinct indexes to optimize search:

```
                  ┌──────────────────────┐
                  │    Search Query      │
                  └──────────┬───────────┘
                             │
            ┌────────────────┼────────────────┐
            ▼                ▼                ▼
     ┌─────────────┐  ┌─────────────┐  ┌──────────────┐
     │  Tantivy    │  │  USearch    │  │    sled      │
     │  (Lexical)  │  │  (Vector)   │  │ (Graph / KV) │
     └──────┬──────┘  └──────┬──────┘  └──────┬───────┘
            │                │                │
            └────────────────┼────────────────┘
                             │
                             ▼
                 Reciprocal Rank Fusion
```

### 5.1 Lexical Index (Tantivy)
- Define a Tantivy schema containing:
  - `path`: Key string (untokenized, stored).
  - `symbol_name`: Text field (tokenized, with trigrams enabled for fuzzy matching).
  - `body`: Text field (tokenized, BM25 scored).
- Commit strategy: Write in batches using `IndexWriter::commit()` at the end of the ingestion phase.

### 5.2 Vector Index (USearch)
- For high-speed, local similarity search, use **usearch** (compact in-memory HNSW with mmap capabilities).
- Use USearch scalar storage (`BF16`, `I8`, or `B1`) only after recall and latency measurement. These formats are **not TurboQuant**. True TurboQuant requires a rotation/calibration implementation plus corpus-specific recall benchmarks and remains a separately gated backend.
- Keep the vector store behind the existing abstraction so another backend can be evaluated without changing retrieval or MCP contracts.

*USearch Initialization Code Standard*:
```rust
use usearch::{new_index, Index, IndexOptions, MetricKind, ScalarKind};

pub fn init_vector_index(dimensions: usize) -> Index {
    let options = IndexOptions {
        dimensions,
        metric: MetricKind::Cos,
        quantization: ScalarKind::BF16, // Fits 8GB VRAM / 16GB RAM constraints
        connectivity: 16,
        expansion_add: 128,
        expansion_search: 64,
    };
    new_index(&options).expect("Failed to initialize vector index")
}
```

---

## 6. Hybrid Query & Retrieval Engine

The search pipeline uses a two-stage process to extract relevant code blocks:

### 6.1 Two-Stage Pipeline
1. **Stage 1 (Retrieval)**:
   - Run parallel Tantivy BM25 + USearch vector queries (top 50-100 matches).
   - Merge results using Reciprocal Rank Fusion (RRF):
     $$RRF\_Score(d) = \sum_{m \in M} \frac{1}{60 + Rank_m(d)}$$
2. **Stage 2 (Reranking)**:
   - Run the merged results through a local cross-encoder (`jina-reranker-v2-base-multilingual` or `BGE-reranker-v2-m3`) using ONNX Runtime (`ort` with CUDA execution provider on the RTX 5060).
   - Prune results to the top 10 matches.

### 6.2 Graph Expansion
- Using the matched symbols from Stage 2, traverse the sled/petgraph adjacency list.
- Fetch direct callers, callees, definition sites, and types.
- Append these related code structures to the context.

### 6.3 Aider-Style PageRank Repo-Map Skeletons
To save thousands of tokens, Findex must not return full code files. It must construct a code skeleton:
- Calculate personalized **PageRank** on the dependency graph.
- Using tree-sitter, extract all signature definitions (classes, functions, interface signatures).
- Keep signatures for the top PageRank nodes, eliding function bodies (using tree-sitter `TreeContext`).
- Binary-search the rendered map to fit the model's token budget (default: 1024 tokens).

---

## 7. Model-Agnostic MCP & UI Surface

Findex exposes code intelligence tools to any IDE and AI model using the **Model Context Protocol (MCP)**.

```
┌──────────────────────────────────────┐
│             AI Agent / IDE           │
└──────────────────┬───────────────────┘
                   │ MCP (JSON-RPC)
                   ▼
┌──────────────────────────────────────┐
│             Findex Server            │
└──────┬───────────┬───────────┬───────┘
       │           │           │
       ▼           ▼           ▼
    sled Graph    USearch   Tantivy
```

### 7.1 MCP Tools Interface (2025-11-25 Specs)
The engine must register the following tools:

- `search_code(query: String, mode: String)`: Executes lexical, semantic, or hybrid search.
- `get_definition(symbol: String)`: Resolves a symbol ID to its definition source lines.
- `get_references(symbol: String)`: Locates all reference sites of a symbol.
- `get_callers(symbol: String)` / `get_callees(symbol: String)`: Traverses execution graph edges.
- `expand_context(symbol: String, depth: u32)`: Traverses dependencies to build structural context.
- `repo_map(token_budget: u32)`: Emits the personalized PageRank codebase skeleton.
- `get_context_bundle(query, mode, token_budget)`: Returns one ranked, token-bounded repo map and exact source package to replace repeated search/read loops.
- `impact_analysis(symbol_id)`: Reports callers, callees, references, affected files, fan-in/out, and God-node risk before edits.
- `get_ast_outline(path)`: Returns a nested source outline, including source-mapped Vue SFC blocks.
- `get_graph_snapshot(limit)`: Returns a bounded, degree-ranked God/UI/API/code graph for visualization and planning.
- `get_runtime_profile(include_gpu)`: Reports CPU, RAM, process RSS, memory policy, vector format, batching guidance, and optional NVIDIA telemetry.

Long-running tools may opt into MCP Tasks by adding `task` to the original `tools/call`. Implement `tasks/get`, `tasks/result`, `tasks/list`, and `tasks/cancel`; do not invent a `tasks/create` method. Tasks are experimental in MCP `2025-11-25`, so task support must be advertised accurately and bounded by TTL and concurrency limits.

Support stdio plus Streamable HTTP POST/GET/DELETE. HTTP must bind to loopback by default, validate `Origin` and the MCP protocol-version header, require a bearer token for any non-loopback bind, and cap request bodies. Session IDs must be random and expiring; SSE replay must be count/byte bounded and scoped so an event ID can never replay another session.

### 7.2 Progressive Disclosure (SKILL.md)
Findex ships with a `SKILL.md` file that teaches coding agents how to interact with the engine. Only the tool descriptions are loaded at agent startup (~30-50 tokens). The complete instructions are loaded on demand.

### 7.3 Tauri WebGL UI
- A desktop interface built on **Tauri** (v2) with a web frontend.
- Embed the Rust core and an Axum loopback API inside the Tauri process. The WebView receives a random per-process API token through a Tauri command; the token must not be compiled into frontend assets.
- Use a compact React interface with GitHub-style design tokens and a lazy-loaded WebGL/Three.js graph. Red denotes God nodes, blue UI, green API, and purple general code.
- Provide human workflows, not a graph demo only: hybrid search, exact context inspection, AST hierarchy, manual bounded graph queries, impact analysis, and CPU/RAM/GPU policy views.
- Keep the default graph bounded and degree-ranked; large graphs must be filtered or sampled instead of freezing the WebView.

---

## 8. Implementation Roadmap & Performance Gates

Development is split into 4 phases. Work on a phase cannot start until the gating benchmarks of the previous phase are passed.

### Phase 0: Ingestion Core (Weeks 1-4)
- **Scope**: Memmap2 read, Blake3 hashing, parallel tree-sitter/oxc parsing, and sled storage behind the `Storage` abstraction.
- **Languages**: JavaScript, TypeScript, Python, Rust.
- **Performance Gate**: Ingestion speed must exceed **100k LOC/sec/core**, and incremental re-indexing on a changed file must complete in **<1.0 seconds**.

### Phase 1: Retrieval MVP (Weeks 5-10)
- **Scope**: Tantivy BM25 + USearch vector index (with BF16 quantization), hybrid RRF merge, and the PageRank skeleton map.
- **Performance Gate**: Combined retrieval latency must stay under **200 ms** (on a 500k LOC codebase). Skeletons must achieve a **10× token reduction** compared to full-file contexts.

### Phase 2: Structure & Full Resolution (Weeks 11-18)
- **Scope**: stack-graphs name-resolution logic, graph expansion engine, and local cross-encoder reranking (via `ort` on RTX 5060). Additional languages (Dart, Flutter, C, C++).
- **Performance Gate**: Cross-file go-to-definition queries must resolve in **<100 ms**. Reranking accuracy (NDCG@10) must show a **>15% improvement** over baseline hybrid retrieval.

### Phase 3: Accelerators & UI (Weeks 19+)
- **Implemented Wave 3 scope**: persisted Merkle comparison; published Stack Graph packages plus validated bounded lexical TSG rules; Vue SFC parsing; task-augmented MCP calls with cooperative cancellation; session-bound Streamable HTTP POST/GET/DELETE and bounded SSE replay; advanced Ratatui; React/Tauri/Axum graph exploration; unified packaging; shared OAuth profile; and consent-gated diagnostics.
- **Remaining accelerator scope**: true TurboQuant, benchmark-gated CodeScout/Ornith fallback wrappers, framework/compiler-grade semantics beyond current TSG coverage, and a separately versioned storage-backend migration.
- **Performance Gate**: benchmark graph frame time at multiple bounded node counts on target hardware; benchmark vector recall and storage before changing scalar formats. Do not treat an unmeasured 10,000-node/60 FPS target as a shipped guarantee.
