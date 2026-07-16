# Implementation plan status

Updated: 2026-07-16

This file records what is implemented from `implementation_plan.md` and keeps production behavior separate from benchmark-gated research.

## Waves 1 and 2 — implemented

- Long-lived model sessions; location-qualified collision-free symbol IDs; stale symbol/chunk/edge cleanup; bounded embedding windows.
- Hybrid Tantivy/USearch retrieval, RRF, optional reranking, bidirectional graph expansion, MMR diversity, AST-aware chunks, and accurate token budgets.
- Docstrings/JSDoc, personalized PageRank, semantic diff, taint trace, structural locality, JSON/compact CLI output, installers, MCP resources/prompts, and a standards-compliant Agent Skill.
- Batched vector generation, optional CUDA with CPU fallback, and scalar USearch quantization without mislabeling it TurboQuant.

## Wave 3 — implemented

### 3.1 Merkle tree diff

- `merkle.rs` persists a deterministic directory tree whose leaves are BLAKE3 content hashes.
- Recursive comparison stops at identical hashes, records visited node counts, and returns changed/deleted relative paths.
- Ingestion format version 3 persists the snapshot only after successful primary/retrieval updates.
- Scope boundary: filesystem discovery still reads and hashes supported files. The stored-tree comparison is subtree-bounded; the entire ingestion process is not falsely described as O(log N).
- Discovery hard-prunes common generated dependency/build/index trees even when no Git ignore file exists; `FINDEX_INCLUDE_GENERATED=1` is the explicit override.

### 3.2 Stack Graphs

- The feature is enabled by default.
- Published TSG configurations are loaded only for languages present in the index: Python, JavaScript, TypeScript, TSX, and Java.
- Per-file graph builds and global path stitching have configurable time/file bounds.
- Exact definition paths become tagged `EdgeType::References` records and are refreshed independently.
- Cross-file Python resolution has a regression test. Unsupported languages retain the existing heuristic resolver.

### 3.3 MCP Tasks

- MCP `2025-11-25` task-augmented `tools/call` is implemented; the outdated draft `tasks/create` method is not.
- Persistent Sled task records include secure UUIDs, RFC 3339 timestamps, status, TTL, poll interval, result, and tool identity.
- `tasks/get`, `tasks/result`, `tasks/list`, and `tasks/cancel` are implemented with bounded TTL and concurrent-task limits.
- Cancellation is terminal, discards late results, and cooperatively interrupts discovery, parsing, index stages, retrieval, graph queries, semantic diff, and Stack Graph work. Foreign ONNX/filesystem/library calls checkpoint before and after rather than being interrupted mid-call.

### 3.4 Streamable HTTP

- Axum serves MCP POST/GET/DELETE at `/mcp` plus `/health`, with expiring UUID sessions and bounded session-specific SSE replay.
- It validates MCP protocol headers and browser Origins, binds to loopback by default, uses constant-time bearer comparison, and rejects non-loopback startup without a bearer token.
- Replay event IDs cannot cross sessions; malformed/foreign IDs are rejected, explicit DELETE terminates a session, and session/replay count and byte budgets are hard-capped.

### 3.5 Vue SFC parsing

- A source-preserving splitter routes script blocks to Oxc and CSS style blocks to tree-sitter.
- Virtual IDs, containment, and ranges are remapped to the original `.vue` file.
- Template component tags create `vue-template` reference edges.
- Tests cover TypeScript script setup, source lines, template components, and multiple style blocks.

## Additional agent and human surfaces

- New MCP tools: `get_context_bundle`, `impact_analysis`, `get_ast_outline`, `get_graph_snapshot`, `get_runtime_profile`, `get_architecture_overview`, `prune_context`, `vfs_update`, `micro_compile`, and `pin_execution_trace`.
- New CLI commands: `context`, `impact`, `ast`, `graph-export`, `doctor`, `models`, `update`, and `mcp-http`.
- TUI: six views, editable `ratatui-textarea` inputs, structured tabs, tree/scroll inspectors, source highlighting, overlay help, toasts, logger diagnostics, optional terminal images, Nord palette, Nerd Font/ASCII modes, debounced search, graph canvas/query, memory/GPU panels, and the supplied eight-frame ingestion sprite tied to a real non-blocking reindex. Reduced-motion mode disables transitions and holds the first sprite frame.
- Tauri: React/Vite UI, lazy WebGL 3D graph, GitHub-style tokens, search/AST/query/architecture/runtime views, and a per-process-token-protected local Axum API.
- Production delivery: three immutable model profiles, shared cache/offline policies, fingerprinted vector-index migrations, automatic model acquisition, dynamic-length batched reranking, idle ONNX session release, signed consent-gated CLI/TUI/Tauri updaters, locked GitHub CI/release jobs, and validated Windows NSIS/MSI bundles.

## Wave 3 production hardening (2026-07-16)

- Fixed packaged Windows startup at its second root cause: the desktop previously opened relative `.findex_db` from the launch working directory and failed under `C:\Windows\System32`. The default is now `<app-local-data>/dev.findex.desktop/index`; `FINDEX_DB_PATH` remains an explicit override. Debug and optimized NSIS payloads both survive System32 launches.
- Added validated `findex://search`, `open`, `symbol`, `graph`, and `settings` routes, current/startup URL handling, runtime Windows/Linux registration, and single-instance forwarding. Tray Show/Settings/Quit and runtime-gated close-to-tray keep the local MCP/index host available without duplicate writers.
- Fixed Tauri updater initialization by replacing the invalid `plugins.updater: null` state with a deserializable configuration and avoiding an empty runtime public-key override. A real `tauri dev` process now reaches the event loop without the startup panic.
- Added versioned index-local settings shared by CLI, TUI, Tauri/Axum, MCP, ingestion, retrieval, and model runtime. Optional indexing/retrieval/VFS/trace/watcher/GPU/UI stages are switchable without rebuilds and validated/clamped before persistence.
- Search reports requested versus effective retrieval mode and rejects the invalid state where both lexical and semantic retrieval are disabled. Rerank pool, graph hops, graph expansion, structural prefetch, MMR, compute provider, RAM/VRAM policy, and idle release now obey runtime settings.
- Fixed repository graph snapshots/architecture metrics dropping parser edges whose destination was a symbol name rather than an ID. Batch resolution now uses exact IDs, unique/qualified names, and file/path locality with explicit confidence/evidence metadata.
- Graph-augmented retrieval now ranks typed bidirectional neighbors with hop decay, execution-trace/Stack-Graph evidence boosts, fan-out bounds, and logarithmic degree penalty so God nodes do not flood context.
- Natural-language query analysis splits camel/snake/code tokens, expands bounded code-domain concepts, recognizes typed relation intent, and fuses raw lexical, expanded lexical, semantic, reranker, and direct graph evidence independently. A real fixture proves that “code where authentication service calls api” ranks the caller implementation first.
- A revision/model/settings-aware bounded query cache reuses exact and deterministic paraphrase-equivalent results, expires entries by TTL, and cannot mix different Merkle roots or embedding fingerprints. Runtime settings expose enablement, entry cap, and TTL across CLI, desktop, and MCP; hard 64 MiB process and 4 MiB entry ceilings prevent retention spikes.
- Architecture orientation now includes directory/module hierarchy, weakly connected structural communities, deterministic source-free summaries, hubs, and internal/boundary edge counts. This is the low-cost GraphRAG-style P0 layer; incremental Leiden communities and LLM community reports remain benchmark-gated rather than implied.
- Production model acquisition is cache-first and asynchronous. Missing embedding/reranking models use dimension-compatible deterministic fallbacks while background workers download pinned artifacts and hot-swap verified ONNX sessions; fingerprint mismatches rebuild vectors before mixing representations.
- The desktop graph now supports search/focus, fit, pin, pause, category/edge/confidence filters, 1-3 hop neighborhoods, selected-edge direction/particles, dragging, keyboard controls, and light/dark-aware WebGL rendering. Settings and runtime views expose effective compute/memory policy.
- The TUI now provides GitHub-light/Nord-dark themes plus typed-edge filters, 0-4 hop focus, selection, pan, zoom, fit, pin/inspection, and motion controls while preserving the supplied smooth ingestion animation.
- Unified Tauri packages include the release `findex` sidecar (CLI plus TUI). NSIS and WiX add/remove the user PATH safely; DEB/RPM/AppImage mappings expose `/usr/bin/findex`. An actual unsigned NSIS bundle completed successfully after validating the uninstall hook.
- MCP adds structured search output, `get_settings`, `set_setting`, `findex://architecture`, and `findex://settings`. Runtime gates reject disabled VFS, micro-compile, trace/taint pinning, and structural-prefetch calls explicitly.
- MCP now exposes drop-in `fetch_context`, exact indexed-only `fetch_file`, `find_files`, and local-only `list_models`. `structured`, `compact`, and `text` response modes avoid duplicating the same payload into both MCP content channels.
- `findex setup-agent` safely installs MCP plus the portable skill for Codex, Claude, Cursor, and Antigravity, supports `--dry-run`, preserves unrelated servers, backs up JSON configs, and requires `--force` before replacing a different Findex entry.
- Google sign-in is shared by CLI, TUI, and desktop through Firebase Auth and the OS credential vault. Diagnostics remain master-off by default, collect no automatic source/query/path/repository identity, and retain failed zstd-compressed uploads in a bounded queue. Firestore has been successfully provisioned and its rules/indexes deployed to findexcodeintelligence.
- Desktop/TUI presentation now uses a shared eight-frame Nord activity glyph, soft GitHub light/dark tokens, reduced-motion behavior, human-readable finding/evidence/next-step reports, and source syntax highlighting. WebGL graph resolution, particles, and motion adapt to graph size and pause while hidden.
- MCP stdio and HTTP requests are capped at 1 MiB, file listings use deterministic pagination, and the file-tree resource has a 4 MiB ceiling.

## Language and bounded-context hardening

- Default parser coverage includes JavaScript/TypeScript/JSX/TSX, Vue, Rust, Python, HTML/CSS, Dart, C/C++, Go, Java, C#, Ruby, PHP, and Swift. Shared symbol roles cover classes, structs, interfaces, traits, protocols, mixins, records, extensions, constructors, methods, modules/namespaces, properties, aliases, and inheritance.
- Structural locality uses on-demand adjacency reads with hop, working-set, and per-node fan-out caps. Token pruning ranks relevance against source-chunk cost, preserves every explicit seed, and continues after oversized candidates.
- The VFS is bounded by bytes and file count, LRU-evicted, backed by shared immutable strings, version/hash aware, and path-normalized. It is process-local by default; `FINDEX_VFS_PERSIST=1` restores/stores a bounded snapshot in project metadata when that source-retention tradeoff is acceptable. Micro-compilation never mutates the indexed on-disk files.
- Semantic diff uses quadratic ordered alignment only below a fixed cell threshold, then falls back to keyed linear-memory alignment. Change text and result counts are capped and truncation is reported.
- Taint propagation has node/edge/fan-out/label limits and persists only edges that carried labels. Execution traces reject unknown symbols, do not bridge gaps, and retain accumulated trace tags.

## Correctness and verification

- 116 workspace tests pass in the current hardening tree (112 core and 4 CLI/TUI), including updater compatibility/safety, relationship-query ranking, cancellation, MCP replay/bounds and stale-event rejection, TSG rule compilation, consent telemetry and queue eviction, query-cache bounds, deterministic architecture summaries, model/vector migration, wide-AST memory bounds, VFS persistence/eviction, graph pruning, trace validation, and complex language fixtures.
- `cargo test --workspace --locked` and `cargo clippy --workspace --all-targets --locked -- -D warnings` pass.
- Vue, Merkle, Stack Graph, generated-tree exclusion, normalized AST lookup, connected graph sampling, HTTP header policy, and MCP task lifecycle have direct tests.
- The production React bundle type-checks/builds; rendered browser QA covered light/dark graph settling, graph interaction, account/privacy settings, compact layout, behavioral search, highlighted exact ranges, and console warnings/errors. Optimized Tauri produces the desktop binary plus a 30.97 MB unified NSIS installer containing the CLI/TUI sidecar. The release executable launched from a System32 working directory, reached its native window, accepted a dynamic settings deep link through the single-instance handoff, and exited cleanly after the smoke test.

## Deliberately future work

- True TurboQuant (rotation/calibration plus recall and latency evaluation).
- Versioned migration from sled to a maintained transactional backend.
- Compiler-grade and framework-specific name/module semantics beyond the current published and bundled lexical TSG rules.
- Cross-device account preference sync and Firestore reporting after provisioning and deploying the database rules/indexes.
