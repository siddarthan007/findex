# MCP tool catalogue

Use exact JSON property names. Limits and budgets are safety controls, not suggestions.

## Retrieval

`search_code`

```json
{ "query": "request authentication", "mode": "hybrid", "limit": 10 }
```

Returns ranked symbols with exact locations. Use lexical for identifiers/error text, hybrid for behavior, semantic for vocabulary mismatch. `regex:<pattern>` is lexical-only. Task mode is optional.

`get_context_bundle`

```json
{ "query": "add cancellation to background indexing", "mode": "hybrid", "token_budget": 2048 }
```

Returns a repo map, ranked source ranges, selection reasons, tokens used, and candidate tokens avoided. This is the default broad-task call. Task mode is optional.

`repo_map`

```json
{ "token_budget": 1024, "focal_symbols": ["search_codebase"], "focal_files": ["crates/findex-core/src/lib.rs"] }
```

Returns a PageRank-ranked signature skeleton. Focus it when anchors are known. Task mode is optional.

`get_file_skeleton`

```json
{ "path": "crates/findex-core/src/lib.rs", "token_budget": 1024 }
```

Returns signatures/nesting without bodies. Prefer this over reading a long file for orientation.

`list_files`

```json
{}
```

Returns every indexed path with its byte size. Use it to verify index scope or choose a file; do not inline the result into a prompt when a repo map or architecture overview is sufficient.

## Exact navigation

`get_definition`

```json
{ "symbol": "resolve_definition", "context": "src/caller.rs#run:L42C1" }
```

Context disambiguates duplicate/dynamic references. Preserve the returned symbol ID.

`get_references`, `get_callers`, `get_callees`

```json
{ "symbol_id": "src/resolver.rs#resolve_definition:L8C1" }
```

These require exact IDs. They are cheaper and easier to audit than generic graph expansion.

`expand_context`

```json
{ "symbol_id": "src/resolver.rs#resolve_definition:L8C1", "depth": 1 }
```

Bounded BFS. Use only when multiple relationship types are needed.

## Architecture and graphs

`get_architecture_overview`

```json
{}
```

Source-free digest of languages, layers, symbol kinds, entrypoints, contracts, cross-file coupling, and hubs. Best first call on an unfamiliar repository.

`get_ast_outline`

```json
{ "path": "src/App.vue" }
```

Nested symbol outline, including multi-language Vue SFC children.

`graph_query`

```json
{ "query": "MATCH (a)-[:Calls]->(b) WHERE a.name = 'main' RETURN a, b LIMIT 50" }
```

Use the supported Cypher-like subset and always bound the return size.

`get_graph_snapshot`

```json
{ "limit": 1000 }
```

Degree-ranked visualization graph with God/UI/API/code categories. Maximum 10000. Do not inject a large snapshot into a model prompt.

`predict_context`

```json
{ "symbol_ids": ["src/lib.rs#search:L40C1"], "depth": 2, "limit": 20 }
```

Ranks structural neighbors for prefetch. Depth 1-8, limit 1-100. Seeds must be exact.

`prune_context`

```json
{ "symbol_ids": ["src/lib.rs#search:L40C1", "src/index.rs#open:L12C1"], "token_budget": 2048 }
```

Returns a high-value subgraph within 64-32768 tokens. Explicit seeds are not silently removed.

## Change and review

`impact_analysis`

```json
{ "symbol_id": "src/lib.rs#search:L40C1" }
```

Returns fan-in/out, callers, callees, references, affected files, risk score, and God-node classification.

`semantic_diff`

```json
{ "file_a": "before.rs", "file_b": "after.rs" }
```

Same-language tree-sitter diff. Task mode is optional. Inspect bounded/truncated flags.

`taint_trace`

```json
{ "source": "src/http.rs#read:L20C1", "label": "user-input", "depth": 3, "pin": false }
```

Depth 0-16. `pin=true` persists taint tags on traversed edges; this is a mutation.

`pin_execution_trace`

```json
{ "trace_id": "test-login-2026-07-15", "symbol_ids": ["src/http.rs#route:L10C1", "src/auth.rs#verify:L22C1"] }
```

Requires at least two known IDs. Mutates adjacency metadata but does not invent missing graph edges.

## VFS and micro-compilation

`vfs_update`

```json
{ "path": "src/auth.rs", "content": "complete proposed file content" }
```

Delete with `{ "path": "src/auth.rs", "delete": true }`. Returns version, BLAKE3 content hash, bytes, evictions, and VFS budget state. This mutates the shadow VFS only.

`micro_compile`

```json
{ "path": "src/auth.rs" }
```

Parses the shadow without disk I/O or index mutation and returns versioned symbols/edges. It is not a compiler or test runner.

## Index and runtime

`get_stats`

```json
{}
```

Returns file/symbol/edge/vector counts, Merkle root, Stack Graph status, and a CPU-only runtime profile.

`list_files`

```json
{}
```

Potentially large. Use only to verify path presence or inventory.

`reindex`

```json
{ "root": "." }
```

Mutates all persistent indexes. Task mode is optional and preferred. Do not run concurrently for the same database.

`get_runtime_profile`

```json
{ "include_gpu": true }
```

Returns CPU pools, RAM/process budgets, model policy, ONNX threads, CUDA build status, GPU arena cap, batching, quantization, and optional NVIDIA telemetry.

## MCP resources and prompts

Read `findex://repo/map`, `findex://tree`, `findex://stats`, or `findex://file/<encoded-path>` when a reusable read-only artifact is more appropriate than a tool call.

Available prompts are `understand_symbol`, `plan_refactor`, and `trace_call`. They are starting recipes, not substitutes for task-specific budgets and verification.

For task-enabled calls, add a `task` object to `tools/call`; use `tasks/get`, `tasks/result`, `tasks/list`, and `tasks/cancel`. MCP protocol `2025-11-25` does not define `tasks/create` here.
