# Competitive edge roadmap

Updated: 2026-07-16

These are benchmark-gated product opportunities, not shipped claims. The goal is to reduce time-to-correct-edit, context tokens, repeated tool calls, and unnecessary compute for both strong and small-context agents.

## Promotion metrics

Every retrieval feature must be evaluated on real repository tasks with:

- Recall@5/10 and nDCG@10 for the source ranges needed to solve the task.
- Time and tool calls until the first correct edit plan.
- Returned tokens, candidate tokens avoided, and duplicate-context ratio.
- p50/p95 cold and warm latency, peak RSS/VRAM, index size, and incremental write amplification.
- Build/test success and regression rate after the agent uses the context.

A feature is promoted only if it improves task success or retrieval quality without violating the declared latency, memory, and token budgets.

## Highest-value next work

| Priority | Capability | Why it can matter | Implementation direction | Promotion gate |
|---|---|---|---|---|
| P0 | Repository retrieval evaluation harness | Model/profile choices are currently impossible to justify from model cards alone. | Curate issue-to-evidence judgments, replay lexical/dense/graph/rerank ablations, publish local HTML/JSON reports. | Statistically meaningful quality lift and measured p95/resource cost. |
| P0 | Intent router with confidence/stop policy | Exact identifiers, behavior questions, architecture questions, and runtime failures need different retrieval paths. | Deterministic query features first; use scores, index availability, duplicate rate, and graph evidence to select legs and stop. | Fewer calls/tokens with no Recall@10 loss. |
| P0 | VFS change-impact simulator | Agents need to know what an edit would break before touching disk. | Reparse shadows, calculate before/after symbols/edges/TED, map removed contracts to callers and tests, return a bounded risk delta. | Higher correct-test selection and fewer post-edit regressions. |
| P0 | Test-to-code execution graph | Static calls miss dispatch, reflection, generated routes, and actual hot paths. | Ingest coverage/test traces as expiring provenance-weighted edges; select the minimum relevant tests for a change. | Same defect detection with materially less test runtime. |
| P1 | SCIP/LSP/compiler evidence merge | Language-specific indexers resolve types and dispatch more accurately than universal syntax alone. | Import SCIP as an interchange layer, namespace provenance, and prefer compiler edges without discarding parser fallback. | Resolution precision lift per language with bounded index cost. |
| P1 | Hierarchical code communities | Large-repository architecture questions need subsystem-level evidence, not thousands of symbol neighbors. | Incremental Leiden/label-propagation communities, deterministic summaries from signatures/docs, drill-down to exact ranges. | Better global-question recall under a fixed token budget. |
| P1 | Cross-repository contract graph | Real systems span services, schemas, packages, queues, and infrastructure. | Link OpenAPI/GraphQL/protobuf/SQL migrations/package manifests/config keys to code symbols with repository-scoped trust. | Faster cross-service impact analysis without leaking source across trust boundaries. |
| P1 | Content-addressed branch/worktree cache | Agents repeatedly index identical blobs across worktrees and branches. | Reuse immutable parse/chunk/vector artifacts keyed by content, grammar, model fingerprint, and extraction schema. | Lower cold-index time and SSD writes with exact invalidation. |
| P1 | Negative and missing-evidence retrieval | A small model benefits from knowing why an attractive result is wrong and what evidence is absent. | Return bounded exclusions, ambiguity sets, unresolved references, and freshness/provenance warnings beside positive context. | Fewer wrong-owner edits and unnecessary follow-up searches. |
| P2 | Local retrieval feedback learning | Static weights do not fit every repository. | Privacy-preserving contextual bandit from explicit local signals such as opened, retained, edited, reverted, and test-passing ranges; never train on source by default. | Sustained task lift with reversible weights and no hidden network use. |
| P2 | Language-specific SSA/PDG slices | Security and behavior questions need data/control dependencies beyond adjacency taint. | Add opt-in compiler IR providers for high-value languages; store bounded summary edges and compute slices on demand. | Precision gain over adjacency taint with acceptable indexing cost. |
| P2 | Agent-neutral context IR | Different model windows and tool protocols waste tokens on repeated formatting. | Serve a compact typed context manifest with stable IDs, evidence, hashes, and on-demand range/resource links; render per client. | Lower serialized tokens without reducing usable evidence. |

## Research patterns worth adapting

- [Graphify concepts](https://graphify.com/concepts) emphasizes a typed directed graph built from parser structure; Findex should keep typed, inspectable edges and provenance rather than collapsing code into opaque embeddings.
- [Graphify documentation](https://graphify.com/docs) demonstrates path/explain workflows and an agent skill. Findex extends this direction with local token budgets, impact, VFS validation, runtime gates, and trace evidence.
- [Microsoft GraphRAG](https://github.com/microsoft/graphrag) motivates hierarchical communities for global questions. Code graphs should use deterministic symbol/contract summaries first because LLM-generated indexing summaries add cost and staleness risk.
- [GraphCoder](https://arxiv.org/abs/2406.07003), [CodexGraph](https://aclanthology.org/2025.naacl-long.7/), and [CodeRAG](https://aclanthology.org/2025.emnlp-main.1187/) provide research evidence that repository structure and code-oriented retrieval can improve repository-level reasoning. Their benchmark results are hypotheses for Findex evaluation, not transferable product guarantees.

## Explicitly avoid

- Unbounded autonomous repository crawling presented as intelligence.
- LLM summaries as the only stored representation of code.
- Depth-heavy graph expansion without typed-edge, degree, hop, or work-set bounds.
- Shipping a larger embedding/reranking model because its model card is stronger on a different dataset.
- Persistent feedback collection or source upload without explicit user control.
- Calling approximate TED, adjacency taint, heuristic resolution, or community summaries compiler-grade truth.

