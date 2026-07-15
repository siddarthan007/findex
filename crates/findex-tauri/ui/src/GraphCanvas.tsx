import { useEffect, useMemo, useRef, useState } from 'react';
import ForceGraph3D from 'react-force-graph-3d';
import { LocateFixed, Pause, Play } from 'lucide-react';
import type { GraphNode, GraphSnapshot } from './types';

const COLORS = { god: '#f85149', ui: '#58a6ff', api: '#3fb950', code: '#a371f7' } as const;
const nodeId = (value: string | GraphNode) => typeof value === 'string' ? value : value.id;

export default function GraphCanvas({ graph, selected, onSelect }: {
  graph: GraphSnapshot;
  selected: GraphNode | null;
  onSelect: (node: GraphNode) => void;
}) {
  const graphRef = useRef<any>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ width: 900, height: 700 });
  const [paused, setPaused] = useState(false);
  const categories = useMemo(() => graph.nodes.reduce((counts, node) => {
    counts[node.category] += 1;
    return counts;
  }, { god: 0, ui: 0, api: 0, code: 0 }), [graph.nodes]);

  useEffect(() => {
    if (!stageRef.current) return;
    const observer = new ResizeObserver(([entry]) => setSize({
      width: Math.floor(entry.contentRect.width),
      height: Math.floor(entry.contentRect.height)
    }));
    observer.observe(stageRef.current);
    return () => observer.disconnect();
  }, []);

  function toggleAnimation() {
    if (paused) graphRef.current?.resumeAnimation();
    else graphRef.current?.pauseAnimation();
    setPaused(value => !value);
  }

  return <div className="graph-stage" ref={stageRef}>
    <div className="view-toolbar">
      <div><h1>Code graph</h1><span>{graph.nodes.length.toLocaleString()} visible nodes {graph.truncated && '· bounded view'}</span></div>
      <div className="toolbar-actions">
        <button onClick={() => graphRef.current?.zoomToFit(500, 70)}><LocateFixed size={14} /> Fit</button>
        <button onClick={toggleAnimation}>{paused ? <Play size={14} /> : <Pause size={14} />}{paused ? 'Resume' : 'Pause'}</button>
      </div>
    </div>
    <ForceGraph3D
      ref={graphRef}
      width={size.width}
      height={Math.max(300, size.height)}
      graphData={graph as any}
      backgroundColor="#0d1117"
      nodeColor={(node: any) => COLORS[node.category as keyof typeof COLORS] ?? COLORS.code}
      nodeVal={(node: any) => Math.min(12, 2.3 + Math.log2(1 + node.degree))}
      nodeOpacity={0.9}
      nodeResolution={10}
      nodeLabel={(node: any) => `${String(node.name).replace(/[<>]/g, '')} · ${node.kind}\n${node.file_path}`}
      linkColor={() => '#30363d'}
      linkOpacity={0.42}
      linkWidth={(link: any) => nodeId(link.source) === selected?.id || nodeId(link.target) === selected?.id ? 1.2 : 0.28}
      linkDirectionalParticles={(link: any) => !paused && (nodeId(link.source) === selected?.id || nodeId(link.target) === selected?.id) ? 1 : 0}
      linkDirectionalParticleColor={() => '#58a6ff'}
      linkDirectionalParticleWidth={1.6}
      linkDirectionalParticleSpeed={0.003}
      warmupTicks={40}
      cooldownTicks={120}
      d3AlphaDecay={0.035}
      d3VelocityDecay={0.35}
      onNodeClick={(node: any) => onSelect(node as GraphNode)}
    />
    <div className="legend">
      {(['god', 'ui', 'api', 'code'] as const).map(category => <span key={category}><i style={{ background: COLORS[category] }} />{category === 'god' ? 'God node' : category.toUpperCase()} <b>{categories[category]}</b></span>)}
    </div>
  </div>;
}
