# Agent playbook

These recipes turn broad prompts into bounded evidence requests. Keep the task statement concrete and make the stop condition explicit.

## Prompt shape

Use four fields in your own working request:

```text
Objective: the behavior or decision to produce.
Boundary: layer, component, symbol, or failure path in scope.
Evidence: exact source ranges, definitions, callers, tests, and runtime trace if available.
Budget/stop: token limit and what makes retrieval sufficient.
```

Example:

```text
Objective: add cancellation to background indexing.
Boundary: watcher -> ingestion handoff; do not redesign parsers.
Evidence: entrypoint, state owner, direct callers, tests, and shutdown/error path.
Budget/stop: 2048 retrieval tokens; stop once ownership and all call sites are confirmed.
```

This prevents vague semantic queries and gives ranking a usable target.

## Proven working patterns

### Retrieval-grounded generation

Call `get_context_bundle`, cite exact ranges in the plan, and make every proposed code change traceable to one returned owner or call site. If a claim lacks a range, label it an inference and verify it before editing.

### Least-to-most decomposition

Resolve in this order: architecture boundary -> exact anchors -> direct relationships -> implementation -> validation. Do not expand the graph before confirming anchors.

### Plan, execute, verify

Before editing, state the invariants and expected affected files. After editing, micro-compile speculative files, run native checks, reindex, then repeat exact navigation to confirm IDs and relationships still resolve.

### Contrastive retrieval

When names are ambiguous, include both desired and excluded behavior in the query: `token refresh path, not initial login`. Use returned reasons/scores to see whether the index honored the distinction.

### Budget escalation

Use 1024 tokens to locate, 2048 to implement a local change, and 4096 for a genuinely cross-layer change. Escalate once and name the missing evidence. Avoid repeatedly calling the same 2048-token bundle with paraphrases.

### Tool-result compression

Carry forward IDs, signatures, path:line ranges, and a one-sentence verified role. Drop full tool payloads once those facts are captured. Never paste graph snapshots, file inventories, or model diagnostics into the implementation prompt unless directly relevant.

## Task recipes

### Fix a bug

1. Search the exact error or failing test lexically.
2. Resolve the owner definition and direct callers.
3. Fetch a 2048-token context bundle centered on observed behavior.
4. Run impact analysis on the intended edit symbol.
5. Edit the smallest owner; validate the failing test plus adjacent contract tests.
6. Reindex and confirm navigation.

### Add a feature

1. Get architecture overview.
2. Search for the nearest existing behavior, not a guessed filename.
3. Inspect the interface/trait/protocol and one implementation.
4. Use callers/references to locate integration and tests.
5. Prune the known seeds into the implementation budget.
6. Preserve established dependency direction; verify impact after edits.

### Refactor

1. Seed with exact IDs.
2. Run impact analysis and callers/references.
3. Use `predict_context` for structurally adjacent symbols.
4. Use `prune_context` so all seeds fit the planning budget.
5. Compare before/after with semantic diff, but use compiler/tests for correctness.
6. Reindex and ensure no expected references disappeared.

### Understand execution

Use direct callers/callees first. If a real trace exists, pin its ordered exact IDs, then compare the trace with static edges. Explain disagreements as dynamic dispatch, missing coverage, stale index, or heuristic resolution—not as certainty.

### Security review

Start from a confirmed input/source symbol. Run taint trace without pinning, inspect each hop, identify sanitizers/validators and sinks in source, then run tests or analyzers appropriate to the language. Adjacency taint is triage, not a vulnerability verdict.

### Review an unsaved patch

Shadow the complete files using VFS, micro-compile each, compare versions/hashes, inspect structural diff against disk copies, and run impact analysis on changed public symbols. Do not persist VFS unless the user accepts unsaved-source storage.

## Quality checks before answering

- Are all important names exact IDs rather than guesses?
- Are claims backed by current source ranges?
- Did retrieval stay within the stated token budget?
- Were truncated/bounded/heuristic results disclosed?
- Did you inspect shared-symbol impact before editing?
- Did native format/build/test checks run?
- Was the index refreshed after disk changes?
- Did you stop instead of gathering unrelated context?

## Anti-patterns

- Whole-repository reads "for context."
- Semantic search for an exact error string.
- Depth-3 expansion before direct navigation.
- Treating score as correctness probability.
- Treating AST parse success as type/build success.
- Treating a taint path as proof of exploitability.
- Treating Stack Graph coverage as universal.
- Launching duplicate reindex/model downloads.
- Enabling CUDA without measuring latency, memory headroom, and fallback behavior.

## Further prompting references

Use current primary guidance when adapting these patterns: [Agent Skills specification](https://agentskills.io/specification), [OpenAI prompt engineering guide](https://platform.openai.com/docs/guides/prompt-engineering), and [Anthropic prompt engineering overview](https://docs.anthropic.com/en/docs/build-with-claude/prompt-engineering/overview).
