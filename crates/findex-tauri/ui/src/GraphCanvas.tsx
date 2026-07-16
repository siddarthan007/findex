import { FormEvent, useEffect, useMemo, useRef, useState } from 'react';
import ForceGraph3D from 'react-force-graph-3d';
import { Focus, LocateFixed, LockKeyhole, Pause, Play, Search, UnlockKeyhole } from 'lucide-react';
import type { FindexSettings, GraphLink, GraphNode, GraphSnapshot } from './types';

const DARK_COLORS = { god: '#f85149', ui: '#58a6ff', api: '#3fb950', code: '#a371f7' } as const;
const LIGHT_COLORS = { god: '#cf222e', ui: '#0969da', api: '#1a7f37', code: '#8250df' } as const;
const nodeId = (value: string | GraphNode) => typeof value === 'string' ? value : value.id;

type MutableNode = GraphNode & { fx?: number; fy?: number; fz?: number };

export default function GraphCanvas({ graph, selected, onSelect, theme, settings }: {
  graph: GraphSnapshot;
  selected: GraphNode | null;
  onSelect: (node: GraphNode) => void;
  theme: 'light' | 'dark';
  settings?: FindexSettings['ui'];
}) {
  const graphRef = useRef<any>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const autoFitRef = useRef(false);
  const [size, setSize] = useState({ width: 900, height: 700 });
  const [paused, setPaused] = useState(false);
  const [pinned, setPinned] = useState<Set<string>>(new Set());
  const [query, setQuery] = useState('');
  const [hops, setHops] = useState(0);
  const [edgeKind, setEdgeKind] = useState('all');
  const [minimumConfidence, setMinimumConfidence] = useState(.6);
  const [categories, setCategories] = useState<Record<GraphNode['category'], boolean>>({ god: true, ui: true, api: true, code: true });
  const colors = theme === 'dark' ? DARK_COLORS : LIGHT_COLORS;

  const categoryCounts = useMemo(() => graph.nodes.reduce((counts, node) => {
    counts[node.category] += 1;
    return counts;
  }, { god: 0, ui: 0, api: 0, code: 0 }), [graph.nodes]);
  const edgeKinds = useMemo(() => Array.from(new Set(graph.links.map(link => link.kind))).sort(), [graph.links]);

  const filteredGraph = useMemo<GraphSnapshot>(() => {
    const acceptedLinks = graph.links.filter(link =>
      (edgeKind === 'all' || link.kind === edgeKind)
      && (link.confidence ?? 0) >= minimumConfidence
    );
    let neighborhood: Set<string> | null = null;
    if (selected && hops > 0) {
      const adjacency = new Map<string, Set<string>>();
      for (const link of acceptedLinks) {
        const source = nodeId(link.source);
        const target = nodeId(link.target);
        if (!adjacency.has(source)) adjacency.set(source, new Set());
        if (!adjacency.has(target)) adjacency.set(target, new Set());
        adjacency.get(source)!.add(target);
        adjacency.get(target)!.add(source);
      }
      neighborhood = new Set([selected.id]);
      let frontier = new Set([selected.id]);
      for (let depth = 0; depth < hops; depth += 1) {
        const next = new Set<string>();
        for (const id of frontier) for (const neighbor of adjacency.get(id) ?? []) {
          if (!neighborhood.has(neighbor)) {
            neighborhood.add(neighbor);
            next.add(neighbor);
          }
        }
        frontier = next;
      }
    }
    const nodes = graph.nodes.filter(node => categories[node.category] && (!neighborhood || neighborhood.has(node.id)));
    const ids = new Set(nodes.map(node => node.id));
    const links = acceptedLinks.filter(link => ids.has(nodeId(link.source)) && ids.has(nodeId(link.target)));
    return { nodes, links, truncated: graph.truncated || nodes.length < graph.nodes.length };
  }, [graph, selected?.id, hops, edgeKind, minimumConfidence, categories]);

  useEffect(() => {
    if (!stageRef.current) return;
    const observer = new ResizeObserver(([entry]) => setSize({
      width: Math.floor(entry.contentRect.width),
      height: Math.floor(entry.contentRect.height)
    }));
    observer.observe(stageRef.current);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!selected) return;
    const timer = window.setTimeout(() => focusNode(selected), 120);
    return () => window.clearTimeout(timer);
  }, [selected?.id]);

  useEffect(() => {
    autoFitRef.current = false;
  }, [graph.nodes.length, graph.links.length]);

  function focusNode(node: GraphNode) {
    const positioned = node as MutableNode;
    const x = positioned.x ?? 0;
    const y = positioned.y ?? 0;
    const z = positioned.z ?? 0;
    const magnitude = Math.hypot(x, y, z) || 1;
    const ratio = 1 + 75 / magnitude;
    graphRef.current?.cameraPosition(
      magnitude === 1 ? { x: 0, y: 0, z: 90 } : { x: x * ratio, y: y * ratio, z: z * ratio },
      { x, y, z },
      450
    );
  }

  function toggleAnimation() {
    if (paused) graphRef.current?.resumeAnimation();
    else graphRef.current?.pauseAnimation();
    setPaused(value => !value);
  }

  function togglePin() {
    if (!selected) return;
    const node = selected as MutableNode;
    const next = new Set(pinned);
    if (next.has(node.id)) {
      delete node.fx; delete node.fy; delete node.fz;
      next.delete(node.id);
      graphRef.current?.d3ReheatSimulation();
    } else {
      node.fx = node.x ?? 0; node.fy = node.y ?? 0; node.fz = node.z ?? 0;
      next.add(node.id);
    }
    setPinned(next);
  }

  function findNode(event: FormEvent) {
    event.preventDefault();
    const needle = query.trim().toLocaleLowerCase();
    if (!needle) return;
    const node = graph.nodes.find(candidate =>
      `${candidate.name} ${candidate.kind} ${candidate.file_path}`.toLocaleLowerCase().includes(needle)
    );
    if (node) onSelect(node);
  }

  return <div className="graph-stage" ref={stageRef} tabIndex={0} onKeyDown={event => {
    if ((event.target as HTMLElement).matches('input, textarea, select, button')) return;
    if (event.key.toLowerCase() === 'f') graphRef.current?.zoomToFit(450, 70);
    if (event.key === ' ') { event.preventDefault(); toggleAnimation(); }
  }}>
    <div className="view-toolbar graph-toolbar">
      <div><h1>Code graph</h1><span>{filteredGraph.nodes.length.toLocaleString()} nodes · {filteredGraph.links.length.toLocaleString()} typed edges {filteredGraph.truncated && '· bounded'}</span></div>
      <form className="graph-find" onSubmit={findNode}><Search size={13} /><input value={query} onChange={event => setQuery(event.target.value)} placeholder="Focus symbol or path" /></form>
      <div className="toolbar-actions">
        <button onClick={() => graphRef.current?.zoomToFit(450, 70)} title="Fit graph (F)"><LocateFixed size={14} />Fit</button>
        <button disabled={!selected} onClick={() => selected && focusNode(selected)}><Focus size={14} />Focus</button>
        <button disabled={!selected} onClick={togglePin}>{selected && pinned.has(selected.id) ? <UnlockKeyhole size={14} /> : <LockKeyhole size={14} />}{selected && pinned.has(selected.id) ? 'Unpin' : 'Pin'}</button>
        <button onClick={toggleAnimation}>{paused ? <Play size={14} /> : <Pause size={14} />}{paused ? 'Resume' : 'Pause'}</button>
      </div>
    </div>
    <div className="graph-filters" aria-label="Graph filters">
      <label>Edge <select value={edgeKind} onChange={event => setEdgeKind(event.target.value)}><option value="all">All types</option>{edgeKinds.map(kind => <option key={kind}>{kind}</option>)}</select></label>
      <label>Neighborhood <select value={hops} onChange={event => setHops(Number(event.target.value))}><option value={0}>Whole view</option><option value={1}>1 hop</option><option value={2}>2 hops</option><option value={3}>3 hops</option></select></label>
      <label className="confidence-filter">Confidence <input type="range" min={0} max={1} step={.05} value={minimumConfidence} onChange={event => setMinimumConfidence(Number(event.target.value))} /><b>{Math.round(minimumConfidence * 100)}%</b></label>
    </div>
    <ForceGraph3D
      ref={graphRef}
      width={size.width}
      height={Math.max(300, size.height)}
      graphData={filteredGraph as any}
      backgroundColor={theme === 'dark' ? '#0d1117' : '#ffffff'}
      nodeColor={(node: any) => colors[node.category as keyof typeof colors] ?? colors.code}
      nodeVal={(node: any) => Math.min(12, 2.3 + Math.log2(1 + node.degree))}
      nodeOpacity={theme === 'dark' ? .9 : .82}
      nodeResolution={10}
      nodeLabel={(node: any) => settings?.graph_labels === false ? '' : `${String(node.name).replace(/[<>]/g, '')} · ${node.kind}\n${node.file_path}`}
      linkColor={(link: GraphLink) => nodeId(link.source) === selected?.id || nodeId(link.target) === selected?.id
        ? (theme === 'dark' ? '#58a6ff' : '#0969da')
        : (theme === 'dark' ? '#30363d' : '#afb8c1')}
      linkOpacity={theme === 'dark' ? .5 : .42}
      linkWidth={(link: GraphLink) => (nodeId(link.source) === selected?.id || nodeId(link.target) === selected?.id ? 1.4 : .2) * (.65 + (link.confidence ?? .6))}
      linkDirectionalArrowLength={2.2}
      linkDirectionalArrowRelPos={1}
      linkDirectionalParticles={(link: GraphLink) => !paused && settings?.graph_particles !== false && (nodeId(link.source) === selected?.id || nodeId(link.target) === selected?.id) ? 2 : 0}
      linkDirectionalParticleColor={() => theme === 'dark' ? '#58a6ff' : '#0969da'}
      linkDirectionalParticleWidth={1.5}
      linkDirectionalParticleSpeed={.003}
      warmupTicks={40}
      cooldownTicks={120}
      d3AlphaDecay={.035}
      d3VelocityDecay={.35}
      onEngineStop={() => {
        if (autoFitRef.current) return;
        autoFitRef.current = true;
        graphRef.current?.zoomToFit(settings?.motion === false ? 0 : 300, 70);
      }}
      onNodeClick={(node: any) => onSelect(node as GraphNode)}
      onNodeDragEnd={(node: any) => {
        if (pinned.has(node.id)) { node.fx = node.x; node.fy = node.y; node.fz = node.z; }
      }}
    />
    <div className="legend">
      {(['god', 'ui', 'api', 'code'] as const).map(category => <button className={categories[category] ? 'active' : ''} key={category} onClick={() => setCategories(current => ({ ...current, [category]: !current[category] }))}><i style={{ background: colors[category] }} />{category === 'god' ? 'God node' : category.toUpperCase()} <b>{categoryCounts[category]}</b></button>)}
    </div>
  </div>;
}
