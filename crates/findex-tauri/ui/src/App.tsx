import { FormEvent, lazy, Suspense, useEffect, useRef, useState } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  Activity, ArrowDownToLine, Braces, Boxes, Check, Cpu, Database, Download, FileCode2, GitBranch,
  Moon, Network, RotateCcw, Search, Settings2, ShieldCheck, SquareTerminal, Sun, Workflow, X
} from 'lucide-react';
import { api } from './api';
import type {
  ArchitectureOverview, AstNode, AstOutline, DeepLinkPayload, DesktopUpdateInfo, FindexSettings, GraphNode, GraphSnapshot, ImpactReport,
  ModelStatus, RuntimeProfile, SearchResult, SourcePreview, Stats, ThemePreference
} from './types';

type View = 'graph' | 'architecture' | 'search' | 'ast' | 'query' | 'runtime' | 'settings';

const EMPTY_GRAPH: GraphSnapshot = { nodes: [], links: [], truncated: false };
const COLORS = { god: '#f85149', ui: '#58a6ff', api: '#3fb950', code: '#a371f7' } as const;
const GraphCanvas = lazy(() => import('./GraphCanvas'));

function App() {
  const [view, setView] = useState<View>('graph');
  const [graph, setGraph] = useState<GraphSnapshot>(EMPTY_GRAPH);
  const [stats, setStats] = useState<Stats | null>(null);
  const [runtime, setRuntime] = useState<RuntimeProfile | null>(null);
  const [architecture, setArchitecture] = useState<ArchitectureOverview | null>(null);
  const [settings, setSettings] = useState<FindexSettings | null>(null);
  const [systemDark, setSystemDark] = useState(() => window.matchMedia('(prefers-color-scheme: dark)').matches);
  const [selected, setSelected] = useState<GraphNode | null>(null);
  const [impact, setImpact] = useState<ImpactReport | null>(null);
  const [source, setSource] = useState<SourcePreview | null>(null);
  const [searchInput, setSearchInput] = useState('');
  const [searchMode, setSearchMode] = useState('hybrid');
  const [searchResults, setSearchResults] = useState<SearchResult[]>([]);
  const [query, setQuery] = useState("MATCH (a)-[:Calls]->(b) WHERE a.name = 'main' RETURN a, b LIMIT 50");
  const [queryResult, setQueryResult] = useState('');
  const [ast, setAst] = useState<AstOutline | null>(null);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState('');
  const [availableUpdate, setAvailableUpdate] = useState<DesktopUpdateInfo | null>(null);
  const [showUpdate, setShowUpdate] = useState(false);
  const [installingUpdate, setInstallingUpdate] = useState(false);

  useEffect(() => {
    Promise.all([api.graph(), api.stats(), api.settings()])
      .then(([nextGraph, nextStats, nextSettings]) => {
        setGraph(nextGraph);
        setStats(nextStats);
        setSettings(nextSettings);
        setSelected(nextGraph.nodes[0] ?? null);
      })
      .catch(cause => setError(String(cause)))
      .finally(() => setBusy(false));
  }, []);

  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    let active = true;
    let unlisten: UnlistenFn | undefined;
    const apply = async (payload: DeepLinkPayload) => {
      if (!active) return;
      const parsed = new URL(payload.url);
      if (payload.route === 'settings' || payload.route === 'graph') {
        setView(payload.route);
        return;
      }
      const rawQuery = payload.route === 'symbol'
        ? parsed.searchParams.get('id')
        : payload.route === 'open'
          ? parsed.searchParams.get('path') ?? parsed.searchParams.get('root')
          : parsed.searchParams.get('q') ?? parsed.searchParams.get('query');
      const queryText = rawQuery?.trim().slice(0, 512);
      if (!queryText) return;
      const requestedMode = parsed.searchParams.get('mode') ?? (payload.route === 'search' ? 'hybrid' : 'lexical');
      const mode = ['hybrid', 'lexical', 'semantic'].includes(requestedMode) ? requestedMode : 'hybrid';
      setSearchInput(queryText);
      setSearchMode(mode);
      setView('search');
      setBusy(true);
      try {
        const results = await api.search(queryText, mode);
        if (!active) return;
        setSearchResults(results);
        if (payload.route === 'symbol' && results[0]) chooseSearchResult(results[0]);
      } catch (cause) {
        if (active) setError(`Deep-link search failed: ${String(cause)}`);
      } finally {
        if (active) setBusy(false);
      }
    };
    void (async () => {
      const pending = await api.pendingDeepLink();
      if (pending) await apply(pending);
      unlisten = await listen<DeepLinkPayload>('findex-deep-link', event => { void apply(event.payload); });
    })().catch(cause => setError(`Deep-link listener failed: ${String(cause)}`));
    return () => { active = false; unlisten?.(); };
  }, []);

  const resolvedTheme = settings?.ui.theme === 'light' || settings?.ui.theme === 'dark'
    ? settings.ui.theme
    : systemDark ? 'dark' : 'light';

  useEffect(() => {
    document.documentElement.dataset.theme = resolvedTheme;
    document.documentElement.style.colorScheme = resolvedTheme;
  }, [resolvedTheme]);

  useEffect(() => {
    const media = window.matchMedia('(prefers-color-scheme: dark)');
    const update = () => setSystemDark(media.matches);
    media.addEventListener('change', update);
    return () => media.removeEventListener('change', update);
  }, []);

  async function saveSettings(next: FindexSettings) {
    setBusy(true);
    try {
      setSettings(await api.saveSettings(next));
    } catch (cause) {
      setError(`Settings were not saved: ${String(cause)}`);
    } finally {
      setBusy(false);
    }
  }

  function cycleTheme() {
    if (!settings) return;
    const order: ThemePreference[] = ['system', 'light', 'dark'];
    const theme = order[(order.indexOf(settings.ui.theme) + 1) % order.length];
    void saveSettings({ ...settings, ui: { ...settings.ui, theme } });
  }

  useEffect(() => {
    const timer = window.setTimeout(() => {
      api.updateCheck().then(update => {
        if (update) setAvailableUpdate(update);
      }).catch(() => { /* Update checks never interrupt local code work. */ });
    }, 5000);
    return () => window.clearTimeout(timer);
  }, []);

  useEffect(() => {
    if (!selected) return;
    setImpact(null);
    setSource(null);
    api.impact(selected.id).then(report => {
      setImpact(report);
      if (report) void api.source(report.symbol).then(setSource).catch(() => setSource(null));
    }).catch(() => setImpact(null));
  }, [selected?.id]);

  useEffect(() => {
    if (view !== 'runtime') return;
    let active = true;
    const refresh = () => api.runtime().then(value => active && setRuntime(value)).catch(cause => setError(String(cause)));
    refresh();
    const timer = window.setInterval(refresh, 3000);
    return () => { active = false; window.clearInterval(timer); };
  }, [view]);

  useEffect(() => {
    if (view !== 'architecture' || architecture) return;
    api.architecture().then(setArchitecture).catch(cause => setError(String(cause)));
  }, [view, architecture]);

  useEffect(() => {
    if (view !== 'ast' || !selected) return;
    api.ast(selected.file_path).then(setAst).catch(cause => setError(String(cause)));
  }, [view, selected?.file_path]);

  async function runSearch(event?: FormEvent) {
    event?.preventDefault();
    if (!searchInput.trim()) return;
    setBusy(true);
    setView('search');
    try {
      setSearchResults(await api.search(searchInput.trim(), searchMode));
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  }

  async function runQuery() {
    setBusy(true);
    try { setQueryResult(await api.query(query)); }
    catch (cause) { setError(String(cause)); }
    finally { setBusy(false); }
  }

  function chooseSearchResult(result: SearchResult) {
    const existing = graph.nodes.find(node => node.id === result.symbol.id);
    setSelected(existing ?? {
      id: result.symbol.id, name: result.symbol.name, kind: result.symbol.kind,
      file_path: result.symbol.file_path, degree: 0, category: 'code'
    });
  }

  async function installUpdate() {
    setInstallingUpdate(true);
    try {
      await api.installUpdate();
    } catch (cause) {
      setError(`Update failed: ${String(cause)}`);
      setInstallingUpdate(false);
    }
  }

  return (
    <div className="app-shell">
      <header className="topbar">
        <div className="brand"><Boxes size={16} strokeWidth={2.2} /><span>findex</span><b>local</b></div>
        <form className="command-search" onSubmit={runSearch}>
          <Search size={15} />
          <input value={searchInput} onChange={event => setSearchInput(event.target.value)} placeholder="Search behavior, symbol, path…" aria-label="Search the codebase" />
          <button type="submit" className="command-submit" aria-label="Run search">Enter</button>
        </form>
        {availableUpdate && <button className="update-chip" onClick={() => setShowUpdate(true)}><ArrowDownToLine size={13} />Update {availableUpdate.version}</button>}
        <button className="icon-button" onClick={cycleTheme} title={`Theme: ${settings?.ui.theme ?? 'system'}`} aria-label="Cycle color theme">
          {resolvedTheme === 'dark' ? <Moon size={14} /> : <Sun size={14} />}
        </button>
        <div className="top-metrics">
          <span><Database size={13} />{stats?.files.toLocaleString() ?? '—'} files</span>
          <span><GitBranch size={13} />{stats?.edges.toLocaleString() ?? '—'} edges</span>
          <i className={busy ? 'status-dot busy' : 'status-dot'} />
        </div>
      </header>

      <nav className="rail" aria-label="Primary">
        <NavButton active={view === 'graph'} label="Graph" onClick={() => setView('graph')}><Network /></NavButton>
        <NavButton active={view === 'architecture'} label="Architecture" onClick={() => setView('architecture')}><Workflow /></NavButton>
        <NavButton active={view === 'search'} label="Search" onClick={() => setView('search')}><Search /></NavButton>
        <NavButton active={view === 'ast'} label="AST" onClick={() => setView('ast')}><Braces /></NavButton>
        <NavButton active={view === 'query'} label="Query" onClick={() => setView('query')}><SquareTerminal /></NavButton>
        <div className="rail-spacer" />
        <NavButton active={view === 'runtime'} label="Runtime" onClick={() => setView('runtime')}><Cpu /></NavButton>
        <NavButton active={view === 'settings'} label="Settings" onClick={() => setView('settings')}><Settings2 /></NavButton>
      </nav>

      <main className="workspace">
        <section className="content-panel">
          {view === 'graph' && (
            <Suspense fallback={<Empty title="Loading WebGL topology" detail="The 3D engine is code-split so non-graph views start without this cost." />}>
              <GraphCanvas graph={graph} selected={selected} onSelect={setSelected} theme={resolvedTheme} settings={settings?.ui} />
            </Suspense>
          )}

          {view === 'search' && (
            <div className="scroll-view">
              <div className="view-heading"><div><h1>Index search</h1><p>Hybrid retrieval with structural expansion and MMR deduplication.</p></div><select value={searchMode} onChange={event => setSearchMode(event.target.value)}><option>hybrid</option><option>lexical</option><option>semantic</option></select></div>
              <div className="result-list">
                {searchResults.length === 0 && <Empty title="No result set" detail="Run a search from the command bar. Behavioral queries work better than filenames." />}
                {searchResults.map(result => <button key={result.symbol.id} className={selected?.id === result.symbol.id ? 'result-row selected' : 'result-row'} onClick={() => chooseSearchResult(result)}>
                  <span className="score">{result.score.toFixed(3)}</span><span className="kind">{result.symbol.kind}</span><span className="result-main"><b>{result.symbol.name}</b><code>{result.symbol.signature}</code></span><span className="location">{compactPath(result.symbol.file_path)}:{result.symbol.start_line}</span>
                </button>)}
              </div>
            </div>
          )}

          {view === 'architecture' && <ArchitectureView overview={architecture} />}

          {view === 'ast' && (
            <div className="scroll-view">
              <div className="view-heading"><div><h1>AST outline</h1><p>{selected?.file_path ?? 'Select a graph or search node first.'}</p></div><FileCode2 size={18} /></div>
              <div className="ast-tree">{ast?.roots.map(node => <AstRow node={node} depth={0} key={node.id} />) ?? <Empty title="No file selected" detail="Select a node to inspect its source-accurate symbol hierarchy." />}</div>
            </div>
          )}

          {view === 'query' && (
            <div className="query-view">
              <div className="view-heading"><div><h1>Graph query</h1><p>Inspect the exact context an agent can retrieve.</p></div><button className="primary" onClick={runQuery}>Run query</button></div>
              <textarea value={query} onChange={event => setQuery(event.target.value)} spellCheck={false} />
              <pre>{queryResult || 'Query results will appear here.'}</pre>
            </div>
          )}

          {view === 'runtime' && <RuntimeView runtime={runtime} />}

          {view === 'settings' && settings && <SettingsView settings={settings} onSave={saveSettings} busy={busy} />}
        </section>

        <aside className="inspector-panel">
          <div className="inspector-title"><Activity size={14} /><span>Inspector</span></div>
          {selected ? <>
            <div className="symbol-head"><i style={{ background: COLORS[selected.category] }} /><div><b>{selected.name}</b><span>{selected.kind}</span></div></div>
            <code className="symbol-id">{selected.id}</code>
            <dl className="facts"><div><dt>Degree</dt><dd>{selected.degree}</dd></div><div><dt>Risk</dt><dd className={impact?.god_node ? 'danger' : ''}>{impact ? `${impact.risk_score.toFixed(1)}/100` : '—'}</dd></div><div><dt>Incoming</dt><dd>{impact?.incoming_edges ?? '—'}</dd></div><div><dt>Outgoing</dt><dd>{impact?.outgoing_edges ?? '—'}</dd></div></dl>
            <section className="inspector-section"><h2>Location</h2><p>{selected.file_path}</p></section>
            {source && <section className="inspector-section source-section"><h2>Exact indexed range · L{source.start_line}–{source.end_line}</h2><CodePreview source={source} /></section>}
            <section className="inspector-section"><h2>Affected files</h2>{impact?.affected_files.slice(0, 8).map(path => <p className="path" key={path}>{compactPath(path)}</p>) ?? <p className="muted">Select an indexed node for impact data.</p>}</section>
            <section className="inspector-section"><h2>Retrieval guidance</h2><p className="muted">Inspect impact before editing. Prefer exact AST ranges or a bounded context bundle over whole-file reads.</p></section>
          </> : <Empty title="Nothing selected" detail="Choose a node or search result." />}
        </aside>
      </main>

      {showUpdate && availableUpdate && <div className="modal-backdrop" role="presentation" onMouseDown={event => event.target === event.currentTarget && !installingUpdate && setShowUpdate(false)}>
        <section className="update-dialog" role="dialog" aria-modal="true" aria-labelledby="update-title">
          <header><span><ShieldCheck size={17} /><b id="update-title">Signed update available</b></span><button aria-label="Close update dialog" disabled={installingUpdate} onClick={() => setShowUpdate(false)}><X size={16} /></button></header>
          <div className="update-version"><b>{availableUpdate.version}</b><span>Current {availableUpdate.currentVersion} · {availableUpdate.target}</span></div>
          <p>{availableUpdate.notes || 'Maintenance and performance improvements.'}</p>
          <div className="trust-note"><ShieldCheck size={14} /><span>The installer is downloaded over HTTPS and its mandatory Tauri signature is verified before installation.</span></div>
          <footer><button disabled={installingUpdate} onClick={() => setShowUpdate(false)}>Not now</button><button className="primary" disabled={installingUpdate} onClick={installUpdate}>{installingUpdate ? 'Downloading and verifying…' : 'Install update'}</button></footer>
        </section>
      </div>}

      <footer className="statusbar">
        <span>Merkle {stats?.merkle_root?.slice(0, 10) ?? 'not indexed'}</span>
        <span>Stack refs {stats?.stack_graphs?.resolved_edges?.toLocaleString() ?? '—'}</span>
        <span>WebGL · Axum loopback · MCP 2025-11-25</span>
        {error && <span className="footer-error">{error}</span>}
      </footer>
      <PointerCompanion enabled={settings?.ui.cursor_companion !== false && settings?.ui.motion !== false} />
    </div>
  );
}

function NavButton({ active, label, onClick, children }: { active: boolean; label: string; onClick: () => void; children: React.ReactElement }) {
  return <button className={active ? 'rail-button active' : 'rail-button'} onClick={onClick} aria-label={label} title={label}>{children}</button>;
}

function AstRow({ node, depth }: { node: AstNode; depth: number }) {
  return <div className="ast-branch"><div className="ast-row" style={{ paddingLeft: 14 + depth * 20 }}><Braces size={13} /><span className="kind">{node.kind}</span><b>{node.name}</b><code>{node.signature}</code><em>L{node.start_line}–{node.end_line}</em></div>{node.children.map(child => <AstRow key={child.id} node={child} depth={depth + 1} />)}</div>;
}

function ArchitectureView({ overview }: { overview: ArchitectureOverview | null }) {
  if (!overview) return <Empty title="Reading architecture index" detail="This view uses graph metadata and signatures, not full source files." />;
  const ranked = (values: Record<string, number>) => Object.entries(values).sort((a, b) => b[1] - a[1]);
  return <div className="scroll-view architecture-view">
    <div className="view-heading"><div><h1>Architecture</h1><p>Source-free map of language boundaries, layers, contracts, entrypoints, and coupling hubs.</p></div><Workflow size={18} /></div>
    <div className="architecture-summary">
      <span><b>{overview.files.toLocaleString()}</b> files</span>
      <span><b>{overview.symbols.toLocaleString()}</b> symbols</span>
      <span><b>{overview.cross_file_edges.toLocaleString()}</b> cross-file edges</span>
    </div>
    <div className="architecture-grid">
      <section><h2>Languages</h2>{ranked(overview.languages).map(([name, count]) => <p key={name}><span>{name}</span><b>{count.toLocaleString()}</b></p>)}</section>
      <section><h2>Layers</h2>{ranked(overview.layers).map(([name, count]) => <p key={name}><span>{name}</span><b>{count.toLocaleString()}</b></p>)}</section>
      <section className="wide"><h2>Highest-coupling symbols</h2>{overview.hubs.slice(0, 20).map(hub => <p key={hub.symbol.id}><span><code>{hub.symbol.kind}</code> {hub.symbol.name}<small>{compactPath(hub.symbol.file_path)}:{hub.symbol.line}</small></span><b>{hub.incoming} in · {hub.outgoing} out</b></p>)}</section>
      <section className="wide"><h2>Module hierarchy</h2>{overview.modules.slice(0, 24).map(module => <p key={module.path} title={module.summary}><span>{module.path}<small>{module.dominant_language} · {module.dominant_layer}</small></span><b>{module.symbols.toLocaleString()} symbols · {module.files} files</b></p>)}</section>
      <section className="wide"><h2>Graph communities</h2>{overview.communities.slice(0, 16).map(community => <p key={community.id} title={community.summary}><span>{community.id}<small>{community.hubs.slice(0, 3).map(hub => hub.name).join(' · ') || community.summary}</small></span><b>{community.symbols} nodes · {community.internal_edges} internal · {community.boundary_edges} boundary</b></p>)}</section>
      <section><h2>Contracts</h2>{overview.contracts.slice(0, 20).map(symbol => <p key={symbol.id}><span>{symbol.name}<small>{symbol.kind}</small></span><b>L{symbol.line}</b></p>)}</section>
      <section><h2>Entrypoints</h2>{overview.entrypoints.slice(0, 20).map(symbol => <p key={symbol.id}><span>{symbol.name}<small>{compactPath(symbol.file_path)}</small></span><b>L{symbol.line}</b></p>)}</section>
    </div>
  </div>;
}

function RuntimeView({ runtime }: { runtime: RuntimeProfile | null }) {
  if (!runtime) return <Empty title="Reading runtime probes" detail="GPU probing only runs while this view is open." />;
  const ramUsed = 1 - runtime.available_memory_bytes / runtime.total_memory_bytes;
  const budgetUsed = runtime.process_memory_bytes / Math.max(1, runtime.memory_budget_bytes);
  return <div className="scroll-view runtime-view">
    <div className="view-heading"><div><h1>Runtime</h1><p>Explicit compute and memory policy; telemetry is not sampled on the search hot path.</p></div><Cpu size={18} /></div>
    <div className="runtime-grid">
      <Meter label="System RAM" value={ramUsed} detail={`${gib(runtime.available_memory_bytes)} GiB available`} />
      <Meter label="Findex budget" value={budgetUsed} detail={`${mib(runtime.process_memory_bytes)} / ${mib(runtime.memory_budget_bytes)} MiB`} warning={budgetUsed > .85} />
      <div className="runtime-card"><h2>Compute policy</h2><dl><div><dt>Logical CPUs</dt><dd>{runtime.logical_cpus}</dd></div><div><dt>Rayon workers</dt><dd>{runtime.rayon_threads}</dd></div><div><dt>ONNX workers</dt><dd>{runtime.onnx_intra_threads}</dd></div><div><dt>Device policy</dt><dd>{runtime.compute_device}</dd></div><div><dt>Model profile</dt><dd>{runtime.model_profile}</dd></div><div><dt>Embedding batch</dt><dd>{runtime.recommended_embedding_batch}</dd></div><div><dt>Vectors</dt><dd>{runtime.vector_quantization}</dd></div><div><dt>CUDA build</dt><dd>{runtime.cuda_compiled ? 'enabled' : 'CPU'}</dd></div></dl></div>
      <div className="runtime-card"><h2>GPU</h2>{runtime.gpu_devices.length ? runtime.gpu_devices.map(gpu => <div key={gpu.name} className="gpu"><b>{gpu.name}</b><span>{gpu.used_memory_mib} / {gpu.total_memory_mib} MiB</span><span>{gpu.utilization_percent}% · {gpu.temperature_celsius ?? '—'}°C</span></div>) : <p className="muted">No NVIDIA telemetry. CPU fallback remains available.</p>}</div>
    </div>
  </div>;
}

function SettingsView({ settings, onSave, busy }: {
  settings: FindexSettings;
  onSave: (settings: FindexSettings) => Promise<void>;
  busy: boolean;
}) {
  const [draft, setDraft] = useState(settings);
  const [models, setModels] = useState<ModelStatus[]>([]);
  const [downloading, setDownloading] = useState<string | null>(null);
  const [modelError, setModelError] = useState('');
  useEffect(() => setDraft(settings), [settings]);
  useEffect(() => { void api.models().then(setModels); }, []);
  const dirty = JSON.stringify(draft) !== JSON.stringify(settings);
  const setIndexing = (key: keyof FindexSettings['indexing'], value: boolean) =>
    setDraft(current => ({ ...current, indexing: { ...current.indexing, [key]: value } }));
  const setRetrieval = <K extends keyof FindexSettings['retrieval']>(key: K, value: FindexSettings['retrieval'][K]) =>
    setDraft(current => ({ ...current, retrieval: { ...current.retrieval, [key]: value } }));
  const setRuntime = <K extends keyof FindexSettings['runtime']>(key: K, value: FindexSettings['runtime'][K]) =>
    setDraft(current => ({ ...current, runtime: { ...current.runtime, [key]: value } }));
  const setUi = <K extends keyof FindexSettings['ui']>(key: K, value: FindexSettings['ui'][K]) =>
    setDraft(current => ({ ...current, ui: { ...current.ui, [key]: value } }));

  return <div className="scroll-view settings-view">
    <div className="view-heading">
      <div><h1>Settings</h1><p>Production controls are persisted beside the index and shared by CLI, TUI, desktop, and MCP.</p></div>
      <div className="heading-actions">
        <button disabled={!dirty || busy} onClick={() => setDraft(settings)}><RotateCcw size={14} />Discard</button>
        <button className="primary" disabled={!dirty || busy} onClick={() => void onSave(draft)}>{busy ? 'Saving…' : 'Save changes'}</button>
      </div>
    </div>
    <div className="settings-grid">
      <SettingsSection title="Indexing" detail="Disable expensive index stages without installing a different build.">
        <Toggle label="Lexical index" detail="Tantivy BM25 and trigram-compatible symbol lookup." checked={draft.indexing.lexical_index} onChange={value => setIndexing('lexical_index', value)} />
        <Toggle label="Semantic index" detail="USearch vectors and ONNX query embedding." checked={draft.indexing.semantic_index} onChange={value => { setIndexing('semantic_index', value); setRetrieval('semantic_search', value); }} />
        <Toggle label="Exact Stack Graphs" detail="Precise name resolution for supported language packages." checked={draft.indexing.stack_graphs} onChange={value => setIndexing('stack_graphs', value)} />
        <Toggle label="Incremental watcher" detail="Debounced partial re-indexing after file changes." checked={draft.indexing.watcher} onChange={value => setIndexing('watcher', value)} />
        <Toggle label="VFS shadowing" detail="Unsaved-buffer overlays and bounded micro-compilation." checked={draft.indexing.vfs_shadowing} onChange={value => setIndexing('vfs_shadowing', value)} />
        <Toggle label="Trace pinning" detail="Persist execution evidence and taint tags on graph edges." checked={draft.indexing.execution_trace_pinning} onChange={value => setIndexing('execution_trace_pinning', value)} />
      </SettingsSection>

      <SettingsSection title="Retrieval" detail="Bound candidate work before allocating tokens or model compute.">
        <Toggle label="Cross-encoder reranking" detail="Rerank only the bounded first-stage pool." checked={draft.retrieval.reranking} onChange={value => setRetrieval('reranking', value)} />
        <Toggle label="Graph expansion" detail="Pull typed neighbors around top retrieval anchors." checked={draft.retrieval.graph_expansion} onChange={value => setRetrieval('graph_expansion', value)} />
        <Toggle label="Structural prefetch" detail="Predict likely next context from graph locality." checked={draft.retrieval.structural_prefetch} onChange={value => setRetrieval('structural_prefetch', value)} />
        <Toggle label="Predictive query cache" detail="Reuse revision-safe results and deterministic paraphrase keys without model calls." checked={draft.retrieval.predictive_query_cache} onChange={value => setRetrieval('predictive_query_cache', value)} />
        <NumberSetting label="Graph hops" value={draft.retrieval.graph_hops} min={0} max={4} onChange={value => setRetrieval('graph_hops', value)} />
        <NumberSetting label="Candidate pool" value={draft.retrieval.candidate_limit} min={4} max={200} onChange={value => setRetrieval('candidate_limit', value)} />
        <NumberSetting label="Query cache entries" value={draft.retrieval.query_cache_entries} min={1} max={2048} onChange={value => setRetrieval('query_cache_entries', value)} />
        <NumberSetting label="Query cache TTL (seconds)" value={draft.retrieval.query_cache_ttl_seconds} min={5} max={86400} step={5} onChange={value => setRetrieval('query_cache_ttl_seconds', value)} />
        <NumberSetting label="Default token budget" value={draft.retrieval.default_token_budget} min={128} max={32768} step={128} onChange={value => setRetrieval('default_token_budget', value)} />
      </SettingsSection>

      <SettingsSection title="Compute and memory" detail="CUDA is used only by compatible ONNX builds; CPU fallback remains mandatory.">
        <SelectSetting label="Compute device" value={draft.runtime.compute_device} options={['auto', 'cpu', 'cuda']} onChange={value => setRuntime('compute_device', value as FindexSettings['runtime']['compute_device'])} />
        <SelectSetting label="Model profile" value={draft.runtime.model_profile} options={['fast', 'balanced', 'quality']} onChange={value => setRuntime('model_profile', value as FindexSettings['runtime']['model_profile'])} />
        <NumberSetting label="RAM budget (MiB)" value={draft.runtime.memory_budget_mib} min={256} max={1048576} step={256} onChange={value => setRuntime('memory_budget_mib', value)} />
        <NumberSetting label="GPU arena limit (MiB)" value={draft.runtime.gpu_memory_limit_mib} min={256} max={1048576} step={256} onChange={value => setRuntime('gpu_memory_limit_mib', value)} />
        <NumberSetting label="Release models after (seconds)" value={draft.runtime.model_idle_seconds} min={30} max={86400} step={30} onChange={value => setRuntime('model_idle_seconds', value)} />
        <p className="settings-note">Device changes release active ONNX sessions immediately. A model-profile change takes effect when Findex next creates its model components.</p>
      </SettingsSection>

      <SettingsSection title="Local model catalog" detail="Pinned revisions, background downloads, and explicit accuracy/latency profiles.">
        {(['fast', 'balanced', 'quality'] as const).map(profile => {
          const profileModels = models.filter(model => model.profile === profile);
          const ready = profileModels.length === 2 && profileModels.every(model => model.installed);
          return <div className="model-row" key={profile}>
            <span><b>{profile}</b><small>{profile === 'fast' ? '384d MiniLM · lowest latency' : profile === 'balanced' ? 'Quantized code embedding + reranker' : 'Full-precision code models'}</small></span>
            <span className={ready ? 'model-state ready' : 'model-state'}>{ready ? <><Check size={12} />Ready</> : 'Not cached'}</span>
            <button disabled={downloading !== null || ready} onClick={() => {
              setDownloading(profile);
              setModelError('');
              void api.downloadModels(profile).then(() => api.models()).then(setModels).catch(cause => setModelError(`Model download failed: ${String(cause)}`)).finally(() => setDownloading(null));
            }}><Download size={13} />{downloading === profile ? 'Downloading…' : ready ? 'Installed' : 'Download'}</button>
          </div>;
        })}
        {modelError && <p className="settings-error">{modelError}</p>}
        <p className="settings-note">Downloads use the shared Hugging Face cache and immutable commit revisions. Findex never deletes shared model files from this screen.</p>
      </SettingsSection>

      <SettingsSection title="Appearance" detail="GitHub-derived tokens with restrained motion and native system preference.">
        <SelectSetting label="Theme" value={draft.ui.theme} options={['system', 'light', 'dark']} onChange={value => setUi('theme', value as ThemePreference)} />
        <Toggle label="Motion" detail="Short state transitions and status animation." checked={draft.ui.motion} onChange={value => setUi('motion', value)} />
        <Toggle label="Graph particles" detail="Animate only edges adjacent to the selected node." checked={draft.ui.graph_particles} onChange={value => setUi('graph_particles', value)} />
        <Toggle label="Graph labels" detail="Show detailed hover labels for graph nodes." checked={draft.ui.graph_labels} onChange={value => setUi('graph_labels', value)} />
        <Toggle label="Minimize to tray" detail="Keep indexing and local agent access available after closing the window." checked={draft.ui.minimize_to_tray} onChange={value => setUi('minimize_to_tray', value)} />
        <Toggle label="Pointer companion" detail="Show the restrained cursor-following ASCII marker; reduced-motion always disables it." checked={draft.ui.cursor_companion} onChange={value => setUi('cursor_companion', value)} />
        <Toggle label="Terminal pointer input" detail="Enable TUI mouse capture; touch terminals may translate gestures into mouse events." checked={draft.ui.terminal_pointer_input} onChange={value => setUi('terminal_pointer_input', value)} />
      </SettingsSection>
    </div>
  </div>;
}

function SettingsSection({ title, detail, children }: { title: string; detail: string; children: React.ReactNode }) {
  return <section className="settings-section"><header><h2>{title}</h2><p>{detail}</p></header>{children}</section>;
}

function Toggle({ label, detail, checked, onChange }: { label: string; detail: string; checked: boolean; onChange: (checked: boolean) => void }) {
  return <label className="setting-row"><span><b>{label}</b><small>{detail}</small></span><input type="checkbox" checked={checked} onChange={event => onChange(event.target.checked)} /></label>;
}

function NumberSetting({ label, value, min, max, step = 1, onChange }: { label: string; value: number; min: number; max: number; step?: number; onChange: (value: number) => void }) {
  return <label className="setting-row compact"><span><b>{label}</b></span><input type="number" value={value} min={min} max={max} step={step} onChange={event => onChange(Number(event.target.value))} /></label>;
}

function SelectSetting({ label, value, options, onChange }: { label: string; value: string; options: string[]; onChange: (value: string) => void }) {
  return <label className="setting-row compact"><span><b>{label}</b></span><select value={value} onChange={event => onChange(event.target.value)}>{options.map(option => <option value={option} key={option}>{option}</option>)}</select></label>;
}

function Meter({ label, value, detail, warning = false }: { label: string; value: number; detail: string; warning?: boolean }) {
  return <div className="runtime-card"><h2>{label}</h2><div className="meter"><i style={{ width: `${Math.min(100, Math.max(0, value * 100))}%` }} className={warning ? 'warning' : ''} /></div><b>{(value * 100).toFixed(1)}%</b><span>{detail}</span></div>;
}

function Empty({ title, detail }: { title: string; detail: string }) {
  return <div className="empty"><Boxes size={22} /><b>{title}</b><p>{detail}</p></div>;
}

function CodePreview({ source }: { source: SourcePreview }) {
  return <pre className="source-code">{source.text.split('\n').map((line, index) => <code key={index}><span className="line-number">{source.start_line + index}</span>{highlightCodeLine(line)}</code>)}</pre>;
}

function highlightCodeLine(line: string) {
  const pattern = /(\/\/.*$|#.*$|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|`(?:\\.|[^`\\])*`|\b(?:async|await|class|const|def|enum|fn|function|impl|import|interface|let|match|new|pub|return|struct|trait|type|use|var)\b|\b\d+(?:\.\d+)?\b)/g;
  const fragments: React.ReactNode[] = [];
  let cursor = 0;
  for (const match of line.matchAll(pattern)) {
    const index = match.index ?? 0;
    if (index > cursor) fragments.push(line.slice(cursor, index));
    const token = match[0];
    const className = token.startsWith('//') || token.startsWith('#') ? 'syntax-comment'
      : /^["'`]/.test(token) ? 'syntax-string'
        : /^\d/.test(token) ? 'syntax-number' : 'syntax-keyword';
    fragments.push(<span className={className} key={`${index}-${token}`}>{token}</span>);
    cursor = index + token.length;
  }
  if (cursor < line.length) fragments.push(line.slice(cursor));
  return fragments;
}

function PointerCompanion({ enabled }: { enabled: boolean }) {
  const marker = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    if (!enabled || window.matchMedia('(prefers-reduced-motion: reduce)').matches || window.matchMedia('(pointer: coarse)').matches) return;
    let frame = 0;
    const move = (event: PointerEvent) => {
      window.cancelAnimationFrame(frame);
      frame = window.requestAnimationFrame(() => {
        if (marker.current) {
          marker.current.style.opacity = '1';
          marker.current.style.transform = `translate3d(${event.clientX + 12}px, ${event.clientY + 10}px, 0)`;
        }
      });
    };
    window.addEventListener('pointermove', move, { passive: true });
    return () => { window.cancelAnimationFrame(frame); window.removeEventListener('pointermove', move); };
  }, [enabled]);
  return enabled ? <span ref={marker} className="pointer-companion" aria-hidden="true">:&gt;</span> : null;
}

function compactPath(path: string) { return path.split(/[\\/]/).slice(-3).join('/'); }
function mib(value: number) { return (value / 1024 ** 2).toFixed(0); }
function gib(value: number) { return (value / 1024 ** 3).toFixed(1); }

export default App;
