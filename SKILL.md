---
name: findex
description: "Local, token-bounded codebase intelligence through MCP and CLI: architecture mapping, hybrid search, exact symbol navigation, structural context prediction/pruning, impact analysis, speculative VFS compilation, semantic diffs, trace/taint pinning, and runtime diagnostics. Use before reading many files, when locating or changing code, tracing behavior, reviewing a refactor, investigating unfamiliar architecture, or reducing agent retrieval calls and context tokens."
---

# Findex

Use Findex as the retrieval and relationship layer before opening source. Its job is to return the smallest evidence set that can answer or unblock the task, with exact symbol IDs and source ranges.

## Operating contract

1. Retrieve before reading. Do not scan whole directories or concatenate files when Findex can narrow the scope.
2. Set a budget before calling. Start at 1024-2048 tokens; grow only after identifying a specific missing dependency.
3. Preserve exact symbol IDs, file paths, line ranges, versions, and content hashes from results.
4. Separate evidence from inference. Treat indexed relationships as leads until exact source ranges confirm the behavior.
5. Prefer one bounded call over repeated broad calls. Stop retrieving when the evidence answers the question.
6. Reindex after committed disk changes. Use VFS tools for unsaved or speculative content.
7. Measure the result: note returned tokens, avoided candidate tokens, latency, truncation flags, and unresolved ambiguity.
8. Read effective settings before relying on an optional stage. A compiled capability and an enabled runtime gate are different facts.

## Start every repository task

Check index health with `get_stats`, then call `get_settings` when the task depends on lexical/semantic retrieval, reranking, graph expansion, Stack Graphs, VFS, traces, or GPU policy. If the index is absent, stale, points at another root, or the requested file is missing, call `reindex` with task mode when available.

For an unfamiliar repository, call `get_architecture_overview` first. It is source-free and cheaper than a repo-wide search. Fetch `repo_map` only when you need named entrypoints, contracts, or a compact signature map.

Do not call `list_files`, `repo_map`, architecture overview, and graph snapshot together by default. Choose the one that answers the orientation question.

## Choose the smallest workflow

| Intent | First call | Follow only if needed |
|---|---|---|
| Broad implementation or bug task | `fetch_context` or `get_context_bundle` | exact source ranges, then `impact_analysis` |
| Exact identifier or error token | `search_code` lexical | `get_definition`, callers/references |
| Concept without known names | `search_code` hybrid | semantic only if dense index is ready |
| Repository architecture | `get_architecture_overview` | focal `repo_map` or bounded graph snapshot |
| One file's structure | `get_ast_outline` | `get_file_skeleton` for compact source shape |
| Change a shared symbol | `impact_analysis` | callers/callees, depth-1 expansion |
| Multiple known seed symbols | `predict_context` | `prune_context` with the task budget |
| Speculative edit | `vfs_update` | `micro_compile`, then disk edit after approval |
| Runtime path evidence | `pin_execution_trace` | `predict_context` or pruned trace neighborhood |
| Security/data-flow lead | `taint_trace` | confirm each reported hop in source/tests |
| Review structural change | `semantic_diff` | inspect changed ranges; run language checks |
| Resource issue | `get_runtime_profile` | tune one documented environment control |
| Optional feature behaves differently | `get_settings` | ask before using `set_setting` |

## End-to-end agent protocol

Use this complete sequence for unfamiliar or high-risk work:

1. **Access:** call `get_stats`; record repository identity, freshness, and available indexes.
2. **Configure:** read `get_settings`; do not change it unless the user asked for a policy change.
3. **Orient:** use architecture overview for layers/contracts or a focused repo map for signatures.
4. **Question:** express one behavior, boundary, evidence need, budget, and stop condition.
5. **Anchor:** use lexical search for exact strings or hybrid search for behavior; keep 5-10 results.
6. **Resolve:** replace display names with exact IDs through definition/reference tools.
7. **Relate:** inspect direct typed edges before any multi-hop expansion.
8. **Budget:** request a 1024-2048 token bundle or prune confirmed seeds into the available window.
9. **Verify:** read only returned ranges; corroborate important claims with source, tests, or trace evidence.
10. **Impact:** run impact analysis before changing public, shared, or high-degree symbols.
11. **Change:** shadow uncertain edits in VFS, micro-compile, then apply the approved disk edit and native checks.
12. **Refresh:** reindex changed disk state and re-run the exact navigation that established the original hypothesis.
13. **Report:** distinguish observed evidence, inference, bounded approximations, and any unresolved coverage gap.
14. **Stop:** do not gather more context once the stated evidence and validation condition are satisfied.

## Retrieval loop

Use this loop instead of open-ended search:

1. State a concrete retrieval question: behavior + boundary + expected artifact.
2. Call the narrowest tool with a limit or token budget.
3. Inspect reasons, scores, exact IDs, graph hops, provenance, `retrieval_trace.effective_mode`, and truncation metadata.
4. Read only returned source ranges plus minimal local context.
5. Verify the hypothesis with definitions, callers, tests, or a micro-compile.
6. Stop when evidence is sufficient. If not, expand exactly one dimension: query specificity, one graph hop, or budget.

This is a least-to-most workflow: establish anchors first, resolve relationships second, then inspect implementation. Do not ask a semantic model to rediscover an exact symbol the lexical or graph index already knows.

## Search and context rules

- Use lexical mode for identifiers, paths, error strings, API names, and regex patterns.
- Use hybrid mode for behavioral questions. It combines lexical and dense evidence when vectors exist.
- Use semantic mode only for vocabulary mismatch and only after `get_stats` confirms vectors.
- If a requested retrieval leg is disabled, inspect `effective_mode`; Findex may use the enabled leg. It returns an error when both lexical and semantic retrieval are disabled.
- Start `search_code` at 5-10 results. Reranking 100 weak candidates wastes compute and attention.
- Start `get_context_bundle` at 2048 tokens. Use 1024 for localization and 4096 only for cross-layer planning.
- Use `response_mode: structured` when the client reads `structuredContent`; use `compact` only when it does not. Never carry both channels into context.
- Use `find_files` -> `fetch_file` or `fetch_context` as drop-in replacements for generic repository fetch tools. Exact indexed paths and hard budgets prevent accidental whole-file/repository context.
- Use `regex:` only with a bounded literal pattern. Do not reproduce a repository-wide grep inside a regex.
- Never infer identity from a display name when duplicate symbols exist; retain the returned symbol ID.

## Relationship and architecture rules

- Prefer direct callers, callees, references, or definitions over `expand_context`.
- Keep expansion at depth 1. Move to depth 2 only after naming the unresolved edge.
- Seed `predict_context` with exact IDs from confirmed anchors, not guessed names.
- Use `prune_context` when several seeds compete for a strict context window. Explicit seeds are retained.
- Use `graph_query` for a precise relationship predicate. Use `get_graph_snapshot` for visualization or multi-branch planning, never as raw agent context.
- Run `impact_analysis` before modifying high fan-in/fan-out symbols. Treat God-node status as review risk, not proof that a refactor is necessary.

## Edit safely with VFS

Use `vfs_update` for the complete proposed file content, then `micro_compile` the same path. Compare returned version and hash with the update result. Micro-compilation parses and extracts relationships; it does not type-check, link, execute tests, or mutate the persistent index.

Delete a shadow with `{ "path": "...", "delete": true }`. VFS is memory-bounded and LRU-evicted. It is process-local by default; set `FINDEX_VFS_PERSIST=1` only when persisting unsaved source inside the project index is acceptable.

After writing verified edits to disk, run the language's formatter/compiler/tests and reindex. Do not treat a successful micro-compile as a build pass.

## Runtime evidence and diffs

`pin_execution_trace` accepts an ordered sequence of known symbol IDs. Unknown IDs are rejected rather than creating phantom edges. Pin only traces from reproducible instrumentation and use stable trace IDs.

`taint_trace` is bounded adjacency propagation, not compiler-grade interprocedural data-flow analysis. Use it to prioritize source inspection. Pinning taint mutates edge metadata; leave `pin=false` for exploratory work.

`semantic_diff` is a bounded ordered tree-edit approximation. Check `bounded_alignment` and `changes_truncated`; never describe its numeric distance as an exact Zhang-Shasha/GumTree distance. Confirm moves, type effects, and generated-code changes with language tooling.

## Long-running calls

Use MCP task mode for tools advertising `execution.taskSupport: optional`, especially `reindex`, large context bundles, repo maps, and structural diffs. Attach `task: { "ttl": 300000 }` to the original `tools/call`, then use `tasks/get` or `tasks/result`. Cancel obsolete work instead of launching duplicates.

Task cancellation marks the protocol task terminal but may not pre-empt CPU work already in progress. Avoid speculative concurrent reindexes against the same database.

## Failure ladder

If a lookup is empty:

1. Retry once with the exact leaf identifier in lexical mode.
2. Remove namespace/generic syntax or use a bounded `regex:` pattern.
3. Check the file with `list_files` only when path presence is uncertain.
4. Check `get_stats` for vector/index state and Stack Graph diagnostics.
5. Reindex if stale.
6. Fall back to local `rg` and a small range read; record that Findex coverage was incomplete.

If relationships conflict with source, trust current source, report the stale/heuristic edge, and reindex. Do not loop the same failed call more than once without changing the hypothesis.

## Capability boundaries

- Published Stack Graph rules provide exact resolution only for Python, JavaScript, TypeScript/TSX, and Java in this build. Other languages use parser-backed symbols plus heuristic resolution.
- Dynamic language resolution remains partial even with Stack Graphs.
- CUDA accelerates compatible ONNX embedding/reranking inference only. Parsing, hashing, graph building, Tantivy, and USearch remain CPU work.
- Runtime settings can select `auto`, `cpu`, or `cuda`; a CUDA request still falls back safely when the binary/provider/hardware is incompatible.
- The vector index is optional; lexical, AST, and graph tools remain useful without models.
- VFS persistence is opt-in because it stores unsaved source content.
- Watch mode uses debounced changed paths and Merkle content identity. A change may still trigger a repository discovery pass to preserve deletion and ignore-rule correctness.

## Load references only when needed

- Read [references/tools.md](references/tools.md) for complete MCP arguments, defaults, mutations, and response-use rules.
- Read [references/agent-playbook.md](references/agent-playbook.md) for task-specific prompting templates, verification patterns, and token/compute policies.
- Read [references/operations.md](references/operations.md) for installation, model cache, GPU/RAM/CPU/SSD controls, watchers, background services, packaging, and troubleshooting.

## Human interfaces

Use `findex tui` for interactive search, typed-edge graph filtering, 0-4 hop neighborhoods, pan/zoom/fit, graph queries, impact inspection, reindexing, and resource telemetry. Set `FINDEX_TUI_ICONS=ascii` without a Nerd Font and `FINDEX_TUI_MOTION=0` for reduced motion.

Use `findex settings show` to inspect the same persisted controls used by MCP, TUI, and desktop; `findex settings set --help` changes only named values. Use `findex --format json <command>` for scripts. Use `findex models --profile fast` for the CPU-first default, choose `balanced` only when code-specialized accuracy justifies a 768d rebuild, and reserve `quality` for measured offline evaluation. Verify cache-only operation with the same profile plus `--offline`, and run `findex doctor --format json` before enabling CUDA. Use `findex update check` for read-only release discovery; never call `update install --yes` for a human unless they explicitly authorized installation.
