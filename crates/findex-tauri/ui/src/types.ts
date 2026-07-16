export type NodeCategory = 'god' | 'ui' | 'api' | 'code';

export interface GraphNode {
  id: string;
  name: string;
  kind: string;
  file_path: string;
  degree: number;
  category: NodeCategory;
  x?: number;
  y?: number;
  z?: number;
}

export interface GraphLink {
  source: string | GraphNode;
  target: string | GraphNode;
  kind: string;
  confidence: number;
  evidence: string;
  tags: string[];
}

export interface GraphSnapshot {
  nodes: GraphNode[];
  links: GraphLink[];
  truncated: boolean;
}

export interface Stats {
  files: number;
  symbols: number;
  edges: number;
  vectors: number;
  merkle_root?: string;
  stack_graphs?: { resolved_edges: number; timed_out: boolean; message: string; published_rule_files?: number; bundled_rule_files?: number };
  index_root?: string;
}

export interface SymbolRecord {
  id: string;
  name: string;
  kind: string;
  signature: string;
  file_path: string;
  start_line: number;
  end_line: number;
  language: string;
  token_count: number;
}

export interface SearchResult { score: number; symbol: SymbolRecord }
export interface SourcePreview { path: string; start_line: number; end_line: number; text: string; truncated: boolean }

export interface AstNode {
  id: string;
  name: string;
  kind: string;
  signature: string;
  start_line: number;
  end_line: number;
  children: AstNode[];
}

export interface AstOutline { file_path: string; roots: AstNode[] }

export interface ImpactReport {
  symbol: SymbolRecord;
  incoming_edges: number;
  outgoing_edges: number;
  risk_score: number;
  god_node: boolean;
  affected_files: string[];
  callers: SymbolRecord[];
  callees: SymbolRecord[];
  references: SymbolRecord[];
}

export interface ArchitectureSymbol {
  id: string; name: string; kind: string; file_path: string; line: number;
}

export interface ArchitectureOverview {
  files: number;
  symbols: number;
  edges: number;
  languages: Record<string, number>;
  layers: Record<string, number>;
  symbol_kinds: Record<string, number>;
  entrypoints: ArchitectureSymbol[];
  contracts: ArchitectureSymbol[];
  hubs: Array<{ symbol: ArchitectureSymbol; incoming: number; outgoing: number }>;
  cross_file_edges: number;
  modules: Array<{ path: string; files: number; symbols: number; dominant_layer: string; dominant_language: string; summary: string }>;
  communities: Array<{ id: string; symbols: number; files: number; internal_edges: number; boundary_edges: number; hubs: ArchitectureSymbol[]; summary: string }>;
}

export interface RuntimeProfile {
  logical_cpus: number;
  rayon_threads: number;
  total_memory_bytes: number;
  available_memory_bytes: number;
  process_memory_bytes: number;
  memory_budget_bytes: number;
  cuda_compiled: boolean;
  vector_quantization: string;
  recommended_embedding_batch: number;
  onnx_intra_threads: number;
  gpu_memory_limit_bytes: number;
  model_policy: string;
  model_profile: string;
  compute_device: 'auto' | 'cpu' | 'cuda' | string;
  gpu_devices: Array<{
    name: string;
    total_memory_mib: number;
    used_memory_mib: number;
    utilization_percent: number;
    temperature_celsius?: number;
  }>;
}

export type ThemePreference = 'system' | 'light' | 'dark';
export type ComputeDevice = 'auto' | 'cpu' | 'cuda';

export interface FindexSettings {
  version: number;
  indexing: {
    lexical_index: boolean;
    semantic_index: boolean;
    stack_graphs: boolean;
    watcher: boolean;
    vfs_shadowing: boolean;
    execution_trace_pinning: boolean;
  };
  retrieval: {
    semantic_search: boolean;
    reranking: boolean;
    graph_expansion: boolean;
    structural_prefetch: boolean;
    graph_hops: number;
    candidate_limit: number;
    default_token_budget: number;
    mmr_lambda: number;
    predictive_query_cache: boolean;
    query_cache_entries: number;
    query_cache_ttl_seconds: number;
  };
  runtime: {
    compute_device: ComputeDevice;
    model_profile: 'fast' | 'balanced' | 'quality';
    memory_budget_mib: number;
    gpu_memory_limit_mib: number;
    model_idle_seconds: number;
  };
  telemetry: {
    enabled: boolean;
    crash_reports: boolean;
    include_hardware: boolean;
    include_project_metrics: boolean;
    include_source_samples: boolean;
  };
  ui: {
    theme: ThemePreference;
    motion: boolean;
    graph_particles: boolean;
    graph_labels: boolean;
    minimize_to_tray: boolean;
    cursor_companion: boolean;
    terminal_pointer_input: boolean;
  };
}

export interface DeepLinkPayload {
  url: string;
  route: 'search' | 'open' | 'symbol' | 'graph' | 'settings' | 'auth';
}

export interface UserProfile {
  uid: string;
  email: string;
  display_name: string;
  photo_url?: string;
  signed_in_at: string;
}

export interface TelemetryStatus {
  enabled: boolean;
  queued_events: number;
  queued_bytes: number;
  queue_limit_bytes: number;
  source_collection_active: boolean;
}

export interface ModelStatus {
  kind: 'embedding' | 'reranker';
  profile: 'fast' | 'balanced' | 'quality';
  repository: string;
  revision: string;
  artifact: string;
  installed: boolean;
  model_path?: string;
  tokenizer_path?: string;
  error?: string;
}

export interface DesktopUpdateInfo {
  version: string;
  currentVersion: string;
  notes: string;
  date?: string;
  target: string;
}
