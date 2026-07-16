import { invoke } from '@tauri-apps/api/core';
import { mockGraph, mockRuntime, mockSettings, mockStats } from './mock';
import type { ArchitectureOverview, AstOutline, DeepLinkPayload, DesktopUpdateInfo, FindexSettings, GraphSnapshot, ImpactReport, ModelStatus, RuntimeProfile, SearchResult, SourcePreview, Stats, SymbolRecord } from './types';

const isTauri = () => '__TAURI_INTERNALS__' in window;
let connection: Promise<{ baseUrl: string; token: string }> | null = null;

async function config() {
  connection ??= invoke<{ baseUrl: string; token: string }>('get_api_config');
  return connection;
}

async function request<T>(path: string, body?: unknown): Promise<T> {
  const { baseUrl, token } = await config();
  const response = await fetch(`${baseUrl}${path}`, {
    method: body === undefined ? 'GET' : 'POST',
    headers: { 'content-type': 'application/json', 'x-findex-token': token },
    body: body === undefined ? undefined : JSON.stringify(body)
  });
  if (!response.ok) throw new Error(`Findex API ${response.status}`);
  return response.json() as Promise<T>;
}

export const api = {
  async pendingDeepLink(): Promise<DeepLinkPayload | null> {
    if (!isTauri()) return null;
    return invoke<DeepLinkPayload | null>('take_pending_deep_link');
  },
  async models(): Promise<ModelStatus[]> {
    if (!isTauri()) return [];
    return invoke<ModelStatus[]>('list_models');
  },
  async downloadModels(profile: FindexSettings['runtime']['model_profile']): Promise<void> {
    if (!isTauri()) return;
    await invoke('download_model_profile', { profile });
  },
  async graph(): Promise<GraphSnapshot> {
    return isTauri() ? request('/api/graph') : mockGraph;
  },
  async stats(): Promise<Stats> {
    return isTauri() ? request('/api/stats') : mockStats;
  },
  async runtime(): Promise<RuntimeProfile> {
    return isTauri() ? request('/api/runtime') : mockRuntime;
  },
  async settings(): Promise<FindexSettings> {
    if (!isTauri()) return mockSettings;
    return request('/api/settings');
  },
  async saveSettings(settings: FindexSettings): Promise<FindexSettings> {
    if (!isTauri()) return settings;
    return request('/api/settings', settings);
  },
  async architecture(): Promise<ArchitectureOverview> {
    if (!isTauri()) return {
      files: mockStats.files, symbols: mockStats.symbols, edges: mockStats.edges,
      languages: { Rust: 212, TypeScript: 144, Python: 72 },
      layers: { core: 268, ui: 82, api: 44, tests: 34 },
      symbol_kinds: { Function: 2840, Method: 1900, Struct: 640, Interface: 180 },
      entrypoints: [], contracts: [], cross_file_edges: 4821,
      modules: [
        { path: 'crates/findex-core', files: 220, symbols: 4210, dominant_layer: 'core', dominant_language: 'Rust', summary: 'Rust core module crates/findex-core: 4,210 symbols across 220 files' },
        { path: 'findex-tauri/ui', files: 45, symbols: 680, dominant_layer: 'ui', dominant_language: 'TypeScript', summary: 'TypeScript UI module findex-tauri/ui: 680 symbols across 45 files' }
      ],
      communities: [{ id: 'community-1', symbols: 442, files: 38, internal_edges: 1230, boundary_edges: 84, hubs: [], summary: '442-symbol service community across 38 files' }],
      hubs: mockGraph.nodes.slice(0, 12).map(node => ({
        symbol: { id: node.id, name: node.name, kind: node.kind, file_path: node.file_path, line: 1 },
        incoming: Math.floor(node.degree / 2), outgoing: Math.ceil(node.degree / 2)
      }))
    };
    return request('/api/architecture');
  },
  async search(query: string, mode = 'hybrid'): Promise<SearchResult[]> {
    if (!isTauri()) {
      return mockGraph.nodes
        .filter(node => `${node.name} ${node.kind} ${node.file_path}`.toLowerCase().includes(query.toLowerCase()))
        .slice(0, 25)
        .map((node, index) => ({
          score: 1 - index / 30,
          symbol: { ...node, signature: `${node.kind.toLowerCase()} ${node.name}`, start_line: 12 + index, end_line: 28 + index, language: 'rust', token_count: 96 }
        }));
    }
    return request('/api/search', { query, mode, limit: 50 });
  },
  async source(symbol: Pick<SymbolRecord, 'file_path' | 'start_line' | 'end_line'>): Promise<SourcePreview | null> {
    if (!isTauri()) return null;
    return request('/api/source', { path: symbol.file_path, start_line: symbol.start_line, end_line: symbol.end_line });
  },
  async query(query: string): Promise<string> {
    if (!isTauri()) return `Matched 25 paths\n${query}\n\nPreview mode uses a deterministic graph fixture.`;
    return (await request<{ text: string }>('/api/query', { query })).text;
  },
  async ast(path: string): Promise<AstOutline> {
    if (!isTauri()) return { file_path: path, roots: [{ id: 'root', name: 'App', kind: 'Component', signature: 'component App', start_line: 1, end_line: 180, children: [{ id: 'child', name: 'runSearch', kind: 'Function', signature: 'function runSearch(query)', start_line: 42, end_line: 61, children: [] }] }] };
    return request('/api/ast', { path });
  },
  async impact(symbolId: string): Promise<ImpactReport | null> {
    if (!isTauri()) return null;
    return request('/api/impact', { symbol_id: symbolId });
  },
  async updateCheck(): Promise<DesktopUpdateInfo | null> {
    if (!isTauri()) return null;
    return invoke<DesktopUpdateInfo | null>('check_for_update');
  },
  async installUpdate(): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>('install_update');
  }
};
