import { FormEvent, lazy, Suspense, useEffect, useState } from 'react';
import {
  Activity, ArrowDownToLine, Braces, Boxes, Cpu, Database, FileCode2, GitBranch,
  Network, Search, ShieldCheck, SquareTerminal, Workflow, X
} from 'lucide-react';
import { api } from './api';
import type {
  ArchitectureOverview, AstNode, AstOutline, DesktopUpdateInfo, GraphNode, GraphSnapshot, ImpactReport,
  RuntimeProfile, SearchResult, Stats
} from './types';

type View = 'graph' | 'architecture' | 'search' | 'ast' | 'query' | 'runtime';

const EMPTY_GRAPH: GraphSnapshot = { nodes: [], links: [], truncated: false };
const COLORS = { god: '#f85149', ui: '#58a6ff', api: '#3fb950', code: '#a371f7' } as const;
const GraphCanvas = lazy(() => import('./GraphCanvas'));

function App() {
  const [view, setView] = useState<View>('graph');
  const [graph, setGraph] = useState<GraphSnapshot>(EMPTY_GRAPH);
  const [stats, setStats] = useState<Stats | null>(null);
  const [runtime, setRuntime] = useState<RuntimeProfile | null>(null);
  const [architecture, setArchitecture] = useState<ArchitectureOverview | null>(null);
  const [selected, setSelected] = useState<GraphNode | null>(null);
  const [impact, setImpact] = useState<ImpactReport | null>(null);
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
    Promise.all([api.graph(), api.stats()])
      .then(([nextGraph, nextStats]) => {
        setGraph(nextGraph);
        setStats(nextStats);
        setSelected(nextGraph.nodes[0] ?? null);
      })
      .catch(cause => setError(String(cause)))
      .finally(() => setBusy(false));
  }, []);

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
    api.impact(selected.id).then(setImpact).catch(() => setImpact(null));
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
      </nav>

      <main className="workspace">
        <section className="content-panel">
          {view === 'graph' && (
            <Suspense fallback={<Empty title="Loading WebGL topology" detail="The 3D engine is code-split so non-graph views start without this cost." />}>
              <GraphCanvas graph={graph} selected={selected} onSelect={setSelected} />
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
        </section>

        <aside className="inspector-panel">
          <div className="inspector-title"><Activity size={14} /><span>Inspector</span></div>
          {selected ? <>
            <div className="symbol-head"><i style={{ background: COLORS[selected.category] }} /><div><b>{selected.name}</b><span>{selected.kind}</span></div></div>
            <code className="symbol-id">{selected.id}</code>
            <dl className="facts"><div><dt>Degree</dt><dd>{selected.degree}</dd></div><div><dt>Risk</dt><dd className={impact?.god_node ? 'danger' : ''}>{impact ? `${impact.risk_score.toFixed(1)}/100` : '—'}</dd></div><div><dt>Incoming</dt><dd>{impact?.incoming_edges ?? '—'}</dd></div><div><dt>Outgoing</dt><dd>{impact?.outgoing_edges ?? '—'}</dd></div></dl>
            <section className="inspector-section"><h2>Location</h2><p>{selected.file_path}</p></section>
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
      <div className="runtime-card"><h2>Compute policy</h2><dl><div><dt>Logical CPUs</dt><dd>{runtime.logical_cpus}</dd></div><div><dt>Rayon workers</dt><dd>{runtime.rayon_threads}</dd></div><div><dt>ONNX workers</dt><dd>{runtime.onnx_intra_threads}</dd></div><div><dt>Model profile</dt><dd>{runtime.model_profile}</dd></div><div><dt>Embedding batch</dt><dd>{runtime.recommended_embedding_batch}</dd></div><div><dt>Vectors</dt><dd>{runtime.vector_quantization}</dd></div><div><dt>CUDA build</dt><dd>{runtime.cuda_compiled ? 'enabled' : 'CPU'}</dd></div></dl></div>
      <div className="runtime-card"><h2>GPU</h2>{runtime.gpu_devices.length ? runtime.gpu_devices.map(gpu => <div key={gpu.name} className="gpu"><b>{gpu.name}</b><span>{gpu.used_memory_mib} / {gpu.total_memory_mib} MiB</span><span>{gpu.utilization_percent}% · {gpu.temperature_celsius ?? '—'}°C</span></div>) : <p className="muted">No NVIDIA telemetry. CPU fallback remains available.</p>}</div>
    </div>
  </div>;
}

function Meter({ label, value, detail, warning = false }: { label: string; value: number; detail: string; warning?: boolean }) {
  return <div className="runtime-card"><h2>{label}</h2><div className="meter"><i style={{ width: `${Math.min(100, Math.max(0, value * 100))}%` }} className={warning ? 'warning' : ''} /></div><b>{(value * 100).toFixed(1)}%</b><span>{detail}</span></div>;
}

function Empty({ title, detail }: { title: string; detail: string }) {
  return <div className="empty"><Boxes size={22} /><b>{title}</b><p>{detail}</p></div>;
}

function compactPath(path: string) { return path.split(/[\\/]/).slice(-3).join('/'); }
function mib(value: number) { return (value / 1024 ** 2).toFixed(0); }
function gib(value: number) { return (value / 1024 ** 3).toFixed(1); }

export default App;
