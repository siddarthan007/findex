import type { FindexSettings, GraphSnapshot, RuntimeProfile, Stats } from './types';

const kinds = ['Function', 'Struct', 'Component', 'Handler', 'Interface', 'Method'];
const names = ['ingest', 'resolve', 'SearchPanel', 'apiContext', 'SymbolGraph', 'rank', 'parseVue', 'query'];

export const mockGraph: GraphSnapshot = (() => {
  const nodes = Array.from({ length: 144 }, (_, index) => {
    const category = index % 37 === 0 ? 'god' : index % 9 === 0 ? 'ui' : index % 13 === 0 ? 'api' : 'code';
    return {
      id: `src/module_${index % 18}.rs#${names[index % names.length]}_${index}`,
      name: `${names[index % names.length]}${index}`,
      kind: kinds[index % kinds.length],
      file_path: index % 9 === 0 ? `ui/components/Panel${index}.tsx` : `src/module_${index % 18}.rs`,
      degree: 2 + ((index * 7) % 29),
      category
    } as const;
  });
  const links = nodes.flatMap((node, index) => [
    { source: node.id, target: nodes[(index * 7 + 11) % nodes.length].id, kind: 'Calls', confidence: .96, evidence: 'stack_graph', tags: ['stack-graphs'] },
    ...(index % 3 === 0 ? [{ source: node.id, target: nodes[(index + 1) % nodes.length].id, kind: 'References', confidence: .82, evidence: 'file_locality', tags: [] }] : [])
  ]);
  return { nodes, links, truncated: true };
})();

export const mockStats: Stats = {
  files: 428,
  symbols: 6812,
  edges: 18740,
  vectors: 6812,
  merkle_root: '8f2ab91d71c936b0',
  stack_graphs: { resolved_edges: 3241, timed_out: false, message: 'exact edges ready' }
};

export const mockRuntime: RuntimeProfile = {
  logical_cpus: 24,
  rayon_threads: 22,
  total_memory_bytes: 32 * 1024 ** 3,
  available_memory_bytes: 19 * 1024 ** 3,
  process_memory_bytes: 486 * 1024 ** 2,
  memory_budget_bytes: 2048 * 1024 ** 2,
  cuda_compiled: true,
  vector_quantization: 'bf16',
  recommended_embedding_batch: 32,
  onnx_intra_threads: 6,
  gpu_memory_limit_bytes: 2147483648,
  model_policy: 'disabled',
  model_profile: 'fast',
  compute_device: 'auto',
  gpu_devices: [{ name: 'NVIDIA RTX', total_memory_mib: 8192, used_memory_mib: 1840, utilization_percent: 12, temperature_celsius: 47 }]
};

export const mockSettings: FindexSettings = {
  version: 2,
  indexing: { lexical_index: true, semantic_index: true, stack_graphs: true, watcher: true, vfs_shadowing: true, execution_trace_pinning: true },
  retrieval: {
    semantic_search: true, reranking: true, graph_expansion: true, structural_prefetch: true,
    graph_hops: 1, candidate_limit: 32, default_token_budget: 2048, mmr_lambda: .75,
    predictive_query_cache: true, query_cache_entries: 128, query_cache_ttl_seconds: 300
  },
  runtime: { compute_device: 'auto', model_profile: 'fast', memory_budget_mib: 2048, gpu_memory_limit_mib: 4096, model_idle_seconds: 300 },
  telemetry: { enabled: false, crash_reports: false, include_hardware: false, include_project_metrics: false, include_source_samples: false },
  ui: {
    theme: 'system', motion: true, graph_particles: true, graph_labels: true,
    minimize_to_tray: true, cursor_companion: true, terminal_pointer_input: true
  }
};
