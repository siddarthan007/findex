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
  stack_graphs?: { resolved_edges: number; timed_out: boolean; message: string };
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
  gpu_devices: Array<{
    name: string;
    total_memory_mib: number;
    used_memory_mib: number;
    utilization_percent: number;
    temperature_celsius?: number;
  }>;
}

export interface DesktopUpdateInfo {
  version: string;
  currentVersion: string;
  notes: string;
  date?: string;
  target: string;
}
