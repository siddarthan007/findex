# Production operations

## Install and package

Windows source install:

```powershell
.\scripts\install.ps1
.\scripts\install.ps1 -Cuda
```

Linux/macOS source install:

```sh
./scripts/install.sh
FINDEX_CUDA=1 ./scripts/install.sh
```

Installers build a release CLI, place `findex` under `~/.findex/bin`, acquire both pinned models through the application, switch runtime to cache-only policy, and write `~/.findex/mcp-config.json`.

Build the desktop bundle from `crates/findex-tauri`:

```sh
npm ci
npm run tauri:build
```

Tagged GitHub releases build locked CLI artifacts and Tauri installers on Windows/Linux. Production signing credentials must be configured in the release environment; do not embed them in the repository.

## Model lifecycle

```sh
findex models --profile fast
findex models --profile balanced
findex models --profile quality
findex models --profile balanced --offline
findex --format json models --profile fast
```

`fast` uses `sentence-transformers/all-MiniLM-L6-v2` plus `cross-encoder/ms-marco-MiniLM-L6-v2` at immutable commits. It is the default because it minimizes CPU latency, vector storage, and cold-build cost.

`balanced` uses the official quantized ONNX artifacts from `jinaai/jina-embeddings-v2-base-code` and `jinaai/jina-reranker-v1-turbo-en`. `quality` uses the same immutable repositories' full-precision ONNX artifacts. Set `FINDEX_MODEL_PROFILE=balanced|quality` for runtime use after acquisition.

Files use the standard Hugging Face content-addressed cache. Concurrent processes reuse completed blobs/snapshots. Production release binaries default to automatic acquisition; debug builds stay network-silent unless explicitly enabled.

`FINDEX_MODEL_POLICY=auto|offline|disabled` controls runtime resolution. Explicit `FINDEX_EMBEDDING_MODEL_DIR` and `FINDEX_RERANKER_MODEL_DIR` override bundled models and expect `model.onnx` plus `tokenizer.json`.

Long-running hosts unload ONNX sessions after `FINDEX_MODEL_IDLE_SECS` (default 300; `0` disables). The next inference lazily reloads from cache. This releases inference arenas but not the on-disk cache.

Vector mappings persist dimension, scalar format, and an embedding fingerprint that includes model artifact and maximum sequence length. Profile/window changes therefore rebuild the vector graph once instead of mixing incompatible embeddings. The rebuild can be expensive for large repositories; use `findex build-vectors` during warm-up rather than paying that cost on the first semantic query.

## Signed updates

```sh
findex update check
findex update install
```

Packaged long-running CLI/TUI/desktop processes check at most once per 24 hours in the background. They never install automatically. CLI requires an interactive answer or explicit `--yes`; TUI uses F8 and Enter; Tauri shows release details and an Install button. CLI archives are streamed to a bounded staging directory and verified with the compiled Minisign public key before extraction. Tauri uses its mandatory updater signature verification. Developer builds without a compiled key do not check the network.

Release CI requires `FINDEX_UPDATER_PUBLIC_KEY`, `TAURI_SIGNING_PRIVATE_KEY`, and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. Only the public key is compiled into binaries. Never commit or log the private key.

## CPU and RAM

- `FINDEX_RAYON_THREADS`: parsing/indexing pool. Default leaves two logical CPUs for the OS and other agents.
- `FINDEX_ONNX_THREADS`: ONNX CPU intra-op pool, default half the Rayon pool capped at 8.
- `FINDEX_MEMORY_BUDGET_MB`: process policy target reported by diagnostics.
- `FINDEX_EMBEDDING_BATCH`: explicit embedding batch, otherwise selected from RAM/GPU headroom.
- `FINDEX_VECTOR_QUANTIZATION=bf16|i8|b1`: accuracy/storage tradeoff. Benchmark retrieval quality before changing an existing index.
- `FINDEX_VFS_MAX_MB` and `FINDEX_VFS_MAX_FILES`: hard shadow-store bounds with LRU eviction.

Do not maximize Rayon and ONNX pools independently; that oversubscribes CPUs and increases tail latency. Run `findex doctor --format json` after tuning.

## CUDA

Compile with the `cuda` feature and install a compatible ONNX Runtime/CUDA/cuDNN stack. Runtime probes CUDA and falls back to CPU when registration fails.

- `FINDEX_ONNX_DEVICE=auto|cpu`: force CPU or allow CUDA.
- `FINDEX_CUDA_DEVICE_ID`: CUDA ordinal, default 0.
- `FINDEX_GPU_MEMORY_LIMIT_MB`: CUDA arena cap. Default uses at most 60% of currently free memory after headroom and never more than 4 GiB.

The CUDA arena uses same-as-requested growth and heuristic cuDNN algorithm selection to reduce peak workspace. The cap controls the arena, not every internal CUDA allocation. CUDA does not accelerate parsing, Merkle hashing, Stack Graphs, Tantivy, or USearch.

## Watchers and partial indexing

```sh
findex watch . --debounce-ms 500
```

The watcher filters through the same parser registry as ingestion, coalesces and deduplicates changed paths, then runs a Merkle-aware update. Unchanged content is not reparsed. Repository discovery may still run to detect deletes, ignore changes, and renames correctly.

Keep the database outside generated directories. Generated dependency/build/index paths are excluded unless `FINDEX_INCLUDE_GENERATED=1`. Enabling generated content increases CPU, SSD writes, RAM, and retrieval noise.

Do not run two writers against one database. Use MCP tasks or one watcher as the indexing owner; other clients should query it.

## Storage and cache care

- Keep `.findex_db` on local SSD, not a high-latency network filesystem.
- Do not delete the Hugging Face cache during active model acquisition.
- Back up only persistent indexes if rebuild cost matters; source remains authoritative.
- VFS stays process-local unless `FINDEX_VFS_PERSIST=1`. Persistence stores unsaved source in `vfs:shadow:v1` inside the project database and is intentionally opt-in.
- Execution/taint pins are metadata leads. Use stable IDs and clear/rebuild the index when trace provenance is no longer trustworthy.

## TUI controls

The TUI uses Ratatui/Crossterm with editable text areas, structured tabs, overlays, toasts, and bounded TachyonFX transitions. Theme tokens come from Opaline's Nord palette with Coolor ANSI-256 fallback. Set:

- `FINDEX_TUI_MOTION=0` for reduced motion.
- `FINDEX_TUI_ICONS=ascii` without Nerd Font glyph support.

The dependency set also provides tree, scroll, syntax, image, large-text, and logger widgets for bounded inspector/diagnostic views. Image rendering must use terminal protocol detection or half-block fallback and must not resize synchronously on every frame.

## Troubleshooting

Model download fails: retry `findex models`; use `HF_TOKEN` for gated/private overrides; verify proxy/CA settings; then test `--offline` to distinguish network from cache problems.

CUDA fails: set `FINDEX_ONNX_DEVICE=cpu` to restore service, verify compatible CUDA/cuDNN/ONNX Runtime versions, inspect `findex doctor`, then re-enable with a conservative memory cap.

High idle RSS: confirm the host has been idle longer than `FINDEX_MODEL_IDLE_SECS`; GPU drivers may retain process-level allocations even after an ONNX session drops.

Stale results: verify root/files/stats, stop duplicate writers, run one task-mode reindex, and check Stack Graph timeout diagnostics.

Low retrieval quality: confirm model/vector dimension consistency, reindex after changing models or quantization, compare lexical vs hybrid, reduce broad query language, and benchmark reranking lift rather than assuming it helps.
