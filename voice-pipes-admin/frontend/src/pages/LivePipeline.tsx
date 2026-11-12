import { useEffect, useState, useCallback, useMemo, useRef } from 'react'
import axios from 'axios'
import ReactFlow, { Background, Controls, applyNodeChanges, applyEdgeChanges } from 'reactflow'
import type { Node, Edge, NodeChange, EdgeChange, ReactFlowInstance } from 'reactflow'
import { Activity, AudioLines, FileCode, Gauge, WifiOff, Users, Tag, Globe, Hash, Cpu } from 'lucide-react'
import 'reactflow/dist/style.css'
import { useTheme } from '../contexts/ThemeContext';
import { API_BASE } from '../config/api';
import { groupFleetByPipeline } from '../lib/fleet';
import type { ApiEnvelope, PipelineGraph } from '../lib/orchestrator';
import { FLEET_COLORS, healthBadgeStyle } from './livePipeline/styles';
import type { GraphLayout, HealthSnapshot, LiveMetrics, LiveWorker, OverallMetrics, PatchParamsResp, PipelineFlowNode } from './livePipeline/types';
import { CustomPipelineNode } from './livePipeline/PipelineNode';
import { emptyGraphMessage, workerOptionLabel } from './livePipeline/labels';
import { RawConfigDrawer } from './livePipeline/RawConfigDrawer';

/** Best-effort error text for a failed PATCH: the BFF/orchestrator error body
 *  (`error` / `message`), then the axios/Error message, then a fallback. */
function describeRequestError(e: unknown): string {
  const body: unknown = axios.isAxiosError(e) ? e.response?.data : undefined;
  const fields = body && typeof body === 'object' ? (body as Record<string, unknown>) : undefined;
  const msg = fields?.error || fields?.message || (e instanceof Error ? e.message : undefined) || 'request failed';
  return String(msg);
}

export default function LivePipeline() {
  const nodeTypes = useMemo(() => ({ custom: CustomPipelineNode }), []);
  const [rawNodes, setRawNodes] = useState<PipelineFlowNode[]>([]);
  const [rawEdges, setRawEdges] = useState<Edge[]>([]);
  const [health, setHealth] = useState<HealthSnapshot | null>(null);
  // Why the graph is empty when the backend could not fetch it.
  const [pipelineError, setPipelineError] = useState<string | null>(null);
  const [rfInstance, setRfInstance] = useState<ReactFlowInstance | null>(null);
  const [showMetrics, setShowMetrics] = useState(false);
  const [showResample, setShowResample] = useState(false);
  const [showRawConfig, setShowRawConfig] = useState(false);
  const [rawToml, setRawToml] = useState<string | null>(null);
  const [pipelineName, setPipelineName] = useState<string>('');
  const [showWorkers, setShowWorkers] = useState(false);
  const [workers, setWorkers] = useState<LiveWorker[]>([]);
  const [overallMetrics, setOverallMetrics] = useState<OverallMetrics | null>(null);
  const [selectedWorkerId, setSelectedWorkerId] = useState<string>('');
  const pipelineLoaded = useRef(false);
  const workersInitialized = useRef(false);

  const { isDark } = useTheme();
  const isOffline = !health || health.status === 'offline';

  // Optimistically apply a runtime param edit and PATCH the worker.
  //
  // Returns null on success, error message string on failure (used by the
  // inline editor to surface a tooltip + red border without tearing down the
  // input). On any non-2xx the local metadata is reverted so the displayed
  // value matches what the worker actually accepted.
  const applyParamUpdate = useCallback(
    async (nodeId: string, key: string, value: unknown): Promise<string | null> => {
      if (!selectedWorkerId) return 'no worker selected';

      // Snapshot the previous value so we can revert if the PATCH fails.
      let previous: unknown = undefined;
      setRawNodes(prev =>
        prev.map(n => {
          if (n.id !== nodeId) return n;
          const meta = (n.data?.metadata ?? {}) as Record<string, unknown>;
          previous = meta[key];
          return { ...n, data: { ...n.data, metadata: { ...meta, [key]: value } } };
        }),
      );

      try {
        const res = await axios.patch<ApiEnvelope<PatchParamsResp>>(
          `${API_BASE}/api/v1/workers/${encodeURIComponent(selectedWorkerId)}/params`,
          { component: nodeId, key, value },
        );
        // Server is the source of truth — re-apply whatever it stored
        // (covers e.g. NaN-sentinel "disabled" semantics).
        const applied = res.data?.data?.value;
        if (applied !== undefined) {
          setRawNodes(prev =>
            prev.map(n => {
              if (n.id !== nodeId) return n;
              const meta = (n.data?.metadata ?? {}) as Record<string, unknown>;
              return { ...n, data: { ...n.data, metadata: { ...meta, [key]: applied } } };
            }),
          );
        }
        return null;
      } catch (e) {
        // Revert local optimistic update.
        setRawNodes(prev =>
          prev.map(n => {
            if (n.id !== nodeId) return n;
            const meta = (n.data?.metadata ?? {}) as Record<string, unknown>;
            const reverted = { ...meta };
            if (previous === undefined) delete reverted[key];
            else reverted[key] = previous;
            return { ...n, data: { ...n.data, metadata: reverted } };
          }),
        );
        return describeRequestError(e);
      }
    },
    [selectedWorkerId],
  );

  // Filter out resample nodes when toggle is off, rewiring edges around them.
  // Also injects the live `workerId` + PATCH callback into each node's data so
  // `EditableParam` can submit changes (kept here, not in `fetchPipeline`, so
  // switching the selected worker re-binds the callback without a re-fetch).
  const layout = useMemo<GraphLayout>(() => {
    const decorate = (n: PipelineFlowNode): PipelineFlowNode => ({
      ...n,
      data: {
        ...n.data,
        workerId: selectedWorkerId,
        onParamUpdate: applyParamUpdate,
      },
    });
    if (showResample) {
      const repositioned = rawNodes.map((n, i) => decorate({ ...n, position: { x: 100 + i * 320, y: 250 + (i % 2 === 0 ? -120 : 120) } }));
      return { nodes: repositioned, edges: rawEdges };
    }
    const resampleIds = new Set(rawNodes.filter(n => n.data.componentType === 'resample').map(n => n.id));
    const visible = rawNodes.filter(n => !resampleIds.has(n.id));
    const repositioned = visible.map((n, i) => decorate({ ...n, position: { x: 100 + i * 320, y: 250 + (i % 2 === 0 ? -120 : 120) } }));
    // Rewire edges: for each resample node, connect its incoming source to its outgoing target
    let rewired = [...rawEdges];
    for (const rid of resampleIds) {
      const incoming = rewired.filter(e => e.target === rid);
      const outgoing = rewired.filter(e => e.source === rid);
      // Remove edges touching this resample node
      rewired = rewired.filter(e => e.source !== rid && e.target !== rid);
      // Bridge: connect each incoming source to each outgoing target
      for (const inc of incoming) {
        for (const out of outgoing) {
          rewired.push({
            ...out,
            id: `e-${inc.source}-${out.target}`,
            source: inc.source,
            target: out.target,
          });
        }
      }
    }
    return { nodes: repositioned, edges: rewired };
  }, [showResample, rawNodes, rawEdges, selectedWorkerId, applyParamUpdate]);

  // Interactive edits (drag, select, measure) applied on top of the current
  // layout. Keyed on the `layout` they were made against, so a new layout
  // (worker switch, resample toggle, metrics tick) discards them and the graph
  // resets to the fresh positions — exactly what the old effect did by
  // overwriting `nodes`/`edges`.
  const [edited, setEdited] = useState<{ base: GraphLayout } & GraphLayout | null>(null);
  const nodes = edited?.base === layout ? edited.nodes : layout.nodes;
  const edges = edited?.base === layout ? edited.edges : layout.edges;

  // Allow nodes & edges to be dragged and moved interactively
  const onNodesChange = useCallback(
    (changes: NodeChange[]) => setEdited(prev => {
      const cur = prev?.base === layout ? prev : { base: layout, nodes: layout.nodes, edges: layout.edges };
      return { ...cur, nodes: applyNodeChanges(changes, cur.nodes) };
    }),
    [layout]
  );
  const onEdgesChange = useCallback(
    (changes: EdgeChange[]) => setEdited(prev => {
      const cur = prev?.base === layout ? prev : { base: layout, nodes: layout.nodes, edges: layout.edges };
      return { ...cur, edges: applyEdgeChanges(changes, cur.edges) };
    }),
    [layout]
  );

  const onNodeDoubleClick = useCallback((_: React.MouseEvent, node: Node) => {
    if (rfInstance) {
      const currentZoom = rfInstance.getZoom();
      // If we are already zoomed in roughly around this level, zoom back out
      if (currentZoom >= 1.2) {
        rfInstance.fitView({ padding: 0.1, maxZoom: 1, duration: 800 });
      } else {
        // Zoom in on the node
        rfInstance.setCenter(node.position.x + 130, node.position.y + 100, { zoom: 1.5, duration: 800 });
      }
    }
  }, [rfInstance]);

  const fetchPipeline = useCallback((workerId?: string) => {
    const params = workerId ? `?worker_id=${encodeURIComponent(workerId)}` : '';
    axios.get<ApiEnvelope<PipelineGraph>>(`${API_BASE}/api/v1/pipeline${params}`)
      .then((res) => {
        const failure = res.data.error;
        setPipelineError(failure ? (failure.message || failure.code) : null);
        if (failure) {
          setRawNodes([]);
          setRawEdges([]);
          setRawToml(null);
          return;
        }
        const pipeline = res.data.data;
        if (pipeline) {
          setRawToml(pipeline.raw_toml ?? null);
          setPipelineName(pipeline.name || 'voice-pipeline');
        }
        if (pipeline && pipeline.nodes) {
           const mappedNodes: PipelineFlowNode[] = pipeline.nodes.map((n, i) => {
             const metaProvider = n.metadata?.provider;
             return {
               id: String(n.id),
               type: 'custom',
               data: {
                   label: n.label,
                   componentType: n.type,
                   provider: n.provider || (typeof metaProvider === 'string' ? metaProvider : undefined),
                   input_sample_rate: n.input_sample_rate ?? null,
                   output_sample_rate: n.output_sample_rate ?? null,
                   metadata: n.metadata || {},
                   param_schema: n.param_schema || [],
                   nodeId: String(n.id),
                   showMetrics: false,
                   metrics: {}
               },
               position: { x: 100 + (i * 320), y: 250 + (i % 2 === 0 ? -120 : 120) },
             };
           });
           setRawNodes(mappedNodes);

           if (pipeline.edges) {
              const mappedEdges: Edge[] = pipeline.edges.map((e) => ({
                id: `e-${e.from}-${e.to}`,
                source: String(e.from),
                target: String(e.to),
                type: 'smoothstep',
                animated: true,
                label: e.sample_rate ? `${(e.sample_rate / 1000).toFixed(0)} kHz` : (e.data_type === 'text' ? 'text' : undefined),
                labelStyle: { fontSize: '10px', fontWeight: 600, fontFamily: 'monospace', fill: '#94a3b8' },
                labelBgStyle: { fill: '#0f172a', fillOpacity: 0.8 },
                labelBgPadding: [6, 3] as [number, number],
                labelBgBorderRadius: 4,
                style: { stroke: e.data_type === 'text' ? '#a78bfa' : '#3b82f6', strokeWidth: 3 }
              }));
              setRawEdges(mappedEdges);
           }
        }
      })
      .catch((err: unknown) => {
        console.error(err);
        setPipelineError(err instanceof Error ? err.message : 'the pipeline could not be fetched');
      });
  }, []);

  useEffect(() => {
    const fetchHealth = () => {
      axios.get<ApiEnvelope<HealthSnapshot>>(`${API_BASE}/api/v1/health`)
        .then(res => {
          setHealth(res.data.data);
          // Fetch pipeline once on first successful connection (before workers are loaded)
          if (!pipelineLoaded.current) {
            pipelineLoaded.current = true;
            fetchPipeline();
          }
        })
        .catch(() => {
          setHealth({ status: 'offline' });
          pipelineLoaded.current = false;
          workersInitialized.current = false;
          setRawNodes([]);
          setRawEdges([]);
        });
    };

    fetchHealth();
    const intervalId = setInterval(fetchHealth, 5000);
    return () => clearInterval(intervalId);
  }, [fetchPipeline]);

  // Fetch real-time metrics while the overlay is on.
  useEffect(() => {
    if (!showMetrics) return;

    const fetchAndSetMetrics = async () => {
      try {
        // Ask the orchestrator for a 60-second rolling window so the
        // figures match the "last 60s" label rendered on each node.
        const params = new URLSearchParams({ window: '60s' });
        if (selectedWorkerId) params.set('worker_id', selectedWorkerId);
        const res = await axios.get<ApiEnvelope<LiveMetrics>>(`${API_BASE}/api/v1/metrics?${params.toString()}`);
        const m = res.data.data;
        if (!m) return;

        // Format a percentile: when no completed turns landed in the
        // window the histogram is empty and the orchestrator returns 0.
        // Show an em-dash instead of "0 ms" so users don't think the
        // pipeline is reporting genuinely zero-latency turns.
        const fmt = (v: unknown): string => {
          const n = Number(v);
          return Number.isFinite(n) && n > 0 ? `${n} ms` : '—';
        };

        const totalRequests = Number(m.pipeline?.requests_total) || 0;
        setOverallMetrics({
          active_workers: m.workers_active,
          active_requests: m.pipeline?.requests_active,
          total_requests: totalRequests,
          p50_latency: fmt(m.pipeline?.latency_ms?.p50),
          p90_latency: fmt(m.pipeline?.latency_ms?.p90),
          errors: m.pipeline?.errors_total
        });

        const componentMetrics: Record<string, Record<string, string>> = {
          stt: { p50: fmt(m.stt?.latency_ms?.p50), p90: fmt(m.stt?.latency_ms?.p90) },
          // LLM / text-processor TTFT (end-of-speech → first token).
          llm: { p50: fmt(m.text?.latency_ms?.p50), p90: fmt(m.text?.latency_ms?.p90) },
          tts: { p50: fmt(m.tts?.latency_ms?.p50), p90: fmt(m.tts?.latency_ms?.p90) }
        };

        setRawNodes(nds => nds.map(n => {
          const cType = n.data.componentType;
          return {
            ...n,
            data: {
              ...n.data,
              showMetrics: true,
              metrics: componentMetrics[cType] || {}
            }
          };
        }));
      } catch (e) {
        console.error("Failed to fetch metrics", e);
      }
    };

    fetchAndSetMetrics(); // initial call
    const intervalId = setInterval(fetchAndSetMetrics, 5000);
    return () => clearInterval(intervalId);
  }, [showMetrics, selectedWorkerId]);

  // Turning the overlay off clears the last figures right in the click
  // handler (not in the effect above) so the next "on" starts blank.
  const toggleMetrics = () => {
    const next = !showMetrics;
    setShowMetrics(next);
    if (!next) {
      setOverallMetrics(null);
      setRawNodes(nds => nds.map(n => ({
         ...n,
         data: { ...n.data, showMetrics: false }
      })));
    }
  };

  // Worker fleet (for the overview bar + selector + panel).  P14 — consume the
  // BFF's shared fleet SSE stream instead of polling /workers every 5s, so this
  // page and the dashboard share a single orchestrator poll.
  useEffect(() => {
    // EventSource cannot send headers; the login cookie carries the token.
    const es = new EventSource(`${API_BASE}/api/v1/stream/fleet`, { withCredentials: true });
    es.onmessage = (ev) => {
      try {
        const snap = JSON.parse(ev.data) as { workers?: unknown };
        const items = Array.isArray(snap.workers) ? (snap.workers as LiveWorker[]) : [];
        setWorkers(items);
        // Auto-select first worker on initial load
        if (!workersInitialized.current && items.length > 0) {
          workersInitialized.current = true;
          setSelectedWorkerId(items[0].id);
        }
      } catch {
        /* ignore a malformed frame */
      }
    };
    return () => es.close();
  }, []);

  // Re-fetch pipeline graph when selected worker changes
  useEffect(() => {
    if (selectedWorkerId) {
      fetchPipeline(selectedWorkerId);
    }
  }, [selectedWorkerId, fetchPipeline]);

  // P15 — fleet overview: group the worker fleet by pipeline with a
  // fraction-aware health status so every pipeline is visible at once and the
  // operator can drill into one rather than picking a worker blind.
  const fleetGroups = useMemo(() => groupFleetByPipeline(workers), [workers]);
  const selectedPipeline = useMemo(
    () => workers.find(x => x.id === selectedWorkerId)?.pipeline_name || null,
    [workers, selectedWorkerId],
  );

  return (
      <div style={{ width: '100%', height: '100%', display: 'flex', flexDirection: 'column', backgroundColor: isDark ? '#0f172a' : '#f8fafc', transition: 'background-color 0.3s ease' }}>
        <header style={{ padding: '1rem 2rem', background: isDark ? '#1e293b' : '#ffffff', color: isDark ? '#f8fafc' : '#0f172a', borderBottom: isDark ? '1px solid #334155' : '1px solid #e2e8f0', display: 'flex', alignItems: 'center', justifyContent: 'space-between', fontFamily: 'system-ui, sans-serif', transition: 'all 0.3s ease' }}>
          <div />
          <div style={{ display: 'flex', alignItems: 'center', gap: '16px', fontSize: '14px', fontWeight: 500, color: isDark ? '#f8fafc' : '#334155' }}>

            <button 
              onClick={() => setShowWorkers(prev => !prev)}
              style={{
                background: showWorkers ? (isDark ? '#1a332e' : '#d1fae5') : 'transparent',
                border: showWorkers ? (isDark ? '1px solid #10b981' : '1px solid #34d399') : '1px solid transparent',
                color: showWorkers ? (isDark ? '#34d399' : '#059669') : (isDark ? '#94a3b8' : '#64748b'),
                cursor: 'pointer',
                display: 'flex',
                alignItems: 'center',
                gap: '6px',
                padding: '6px 12px',
                borderRadius: '8px',
                fontWeight: 600,
                fontSize: '12px',
                transition: 'all 0.2s',
              }}
              title="Toggle Worker Fleet Panel"
            >
              <Users size={16} />
              {showWorkers ? 'Hide Workers' : 'Show Workers'}
            </button>

            <button 
              onClick={toggleMetrics}
              style={{
                background: showMetrics ? (isDark ? '#312e81' : '#e0e7ff') : 'transparent',
                border: showMetrics ? (isDark ? '1px solid #4f46e5' : '1px solid #818cf8') : '1px solid transparent',
                color: showMetrics ? (isDark ? '#a5b4fc' : '#4f46e5') : (isDark ? '#94a3b8' : '#64748b'),
                cursor: 'pointer',
                display: 'none',
                alignItems: 'center',
                gap: '6px',
                padding: '6px 12px',
                borderRadius: '8px',
                fontWeight: 600,
                fontSize: '12px',
                transition: 'all 0.2s',
              }}
              title="Toggle Live Realtime Metrics"
            >
              <Gauge size={16} />
              {showMetrics ? 'Hide Metrics' : 'Show Metrics'}
            </button>

            {/* Pipeline / worker selector */}
            {workers.length > 1 && (
              <select
                value={selectedWorkerId}
                onChange={e => setSelectedWorkerId(e.target.value)}
                style={{
                  background: isDark ? '#1e293b' : '#ffffff',
                  border: isDark ? '1px solid #334155' : '1px solid #e2e8f0',
                  color: isDark ? '#f8fafc' : '#0f172a',
                  padding: '6px 10px',
                  borderRadius: '8px',
                  fontSize: '12px',
                  fontWeight: 600,
                  cursor: 'pointer',
                  outline: 'none',
                }}
                title="Switch between worker pipelines"
              >
                {workers.map((w) => (
                  <option key={w.id} value={w.id}>
                    {workerOptionLabel(w)}
                  </option>
                ))}
              </select>
            )}

            <button 
              onClick={() => setShowResample(prev => !prev)}
              style={{
                background: showResample ? (isDark ? '#1e3a5f' : '#dbeafe') : 'transparent',
                border: showResample ? (isDark ? '1px solid #3b82f6' : '1px solid #60a5fa') : '1px solid transparent',
                color: showResample ? (isDark ? '#60a5fa' : '#2563eb') : (isDark ? '#94a3b8' : '#64748b'),
                cursor: 'pointer',
                display: 'flex',
                alignItems: 'center',
                gap: '6px',
                padding: '6px 12px',
                borderRadius: '8px',
                fontWeight: 600,
                fontSize: '12px',
                transition: 'all 0.2s',
              }}
              title="Toggle Resample Nodes"
            >
              <AudioLines size={16} />
              {showResample ? 'Hide Resamplers' : 'Show Resamplers'}
            </button>

            <button 
              onClick={() => setShowRawConfig(true)}
              style={{
                background: showRawConfig ? (isDark ? '#1e3a5f' : '#dbeafe') : 'transparent',
                border: showRawConfig ? (isDark ? '1px solid #3b82f6' : '1px solid #60a5fa') : '1px solid transparent',
                color: showRawConfig ? (isDark ? '#60a5fa' : '#2563eb') : (isDark ? '#94a3b8' : '#64748b'),
                cursor: 'pointer',
                display: 'flex',
                alignItems: 'center',
                gap: '6px',
                padding: '6px 12px',
                borderRadius: '8px',
                fontWeight: 600,
                fontSize: '12px',
                transition: 'all 0.2s',
              }}
              title="View Raw Pipeline Configuration (TOML)"
            >
              <FileCode size={16} />
              Raw Config
            </button>

            <div style={{ width: '1px', height: '24px', background: isDark ? '#334155' : '#e2e8f0' }} />

            <div style={{ display: 'flex', alignItems: 'center', gap: '12px', background: isDark ? '#334155' : '#f1f5f9', padding: '6px 12px', borderRadius: '8px' }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                <Users size={14} style={{ color: isDark ? '#94a3b8' : '#64748b' }} />
                <span style={{ fontSize: '12px', fontFamily: 'monospace', fontWeight: 600 }}>{health?.workers?.active ?? 0}/{health?.workers?.total ?? 0}</span>
                <span style={{ fontSize: '11px', color: isDark ? '#64748b' : '#94a3b8' }}>workers</span>
              </div>
              <div style={{ width: '1px', height: '16px', background: isDark ? '#475569' : '#cbd5e1' }} />
              <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                <div style={{ width: 8, height: 8, borderRadius: '50%', background: health && health.status !== 'offline' ? '#22c55e' : '#ef4444', boxShadow: health && health.status !== 'offline' ? '0 0 8px rgba(34, 197, 94, 0.5)' : 'none' }} />
                <span style={{ fontSize: '12px', fontWeight: 600 }}>{health && health.status !== 'offline' ? 'Online' : 'Offline'}</span>
              </div>
            </div>
          </div>
        </header>

        {/* P15 — fleet overview: every pipeline at a glance (fraction-healthy),
            click a pipeline to drill its first worker into the graph below. */}
        {fleetGroups.length > 1 && (
          <div style={{
            display: 'flex', alignItems: 'center', gap: '8px',
            padding: '8px 2rem', overflowX: 'auto',
            background: isDark ? '#0b1220' : '#f1f5f9',
            borderBottom: isDark ? '1px solid #1e293b' : '1px solid #e2e8f0',
          }}>
            <span style={{ fontSize: '11px', fontWeight: 700, textTransform: 'uppercase', letterSpacing: '0.05em', color: isDark ? '#64748b' : '#94a3b8', marginRight: '4px', whiteSpace: 'nowrap' }}>
              Pipelines
            </span>
            {fleetGroups.map(g => {
              const c = FLEET_COLORS[g.status][isDark ? 'dark' : 'light'];
              const active = g.name === selectedPipeline;
              return (
                <button
                  key={g.name}
                  onClick={() => { const w0 = g.workers[0]; if (w0) setSelectedWorkerId(w0.id); }}
                  title={`${g.name}: ${g.healthy}/${g.total} healthy${g.dead ? ` · ${g.dead} dead` : ''} — click to inspect`}
                  style={{
                    display: 'inline-flex', alignItems: 'center', gap: '6px',
                    padding: '5px 10px', borderRadius: '8px', cursor: 'pointer',
                    fontSize: '12px', fontWeight: 600, whiteSpace: 'nowrap',
                    background: c.bg, color: c.text,
                    border: `1px solid ${active ? c.activeBorder : c.border}`,
                    boxShadow: active ? `0 0 0 1px ${c.activeBorder}` : 'none',
                  }}
                >
                  <span style={{ width: 8, height: 8, borderRadius: '50%', background: c.dot, flexShrink: 0 }} />
                  {g.name}
                  <span style={{ opacity: 0.85, fontVariantNumeric: 'tabular-nums' }}>{g.healthy}/{g.total}</span>
                  {g.dead > 0 && <span style={{ color: c.dot }}>· {g.dead} dead</span>}
                </button>
              );
            })}
          </div>
        )}

        <main style={{ flex: 1, position: 'relative' }}>
          {isOffline ? (
            <div style={{
              display: 'flex',
              flexDirection: 'column',
              alignItems: 'center',
              justifyContent: 'center',
              height: '100%',
              gap: '16px',
              fontFamily: 'system-ui, sans-serif',
              color: isDark ? '#94a3b8' : '#64748b',
            }}>
              <WifiOff size={48} style={{ color: isDark ? '#475569' : '#94a3b8' }} />
              <span style={{ fontSize: '18px', fontWeight: 600, color: isDark ? '#cbd5e1' : '#334155' }}>
                Orchestrator Offline
              </span>
              <span style={{ fontSize: '13px' }}>
                Waiting for connection to <code style={{ background: isDark ? '#334155' : '#e2e8f0', padding: '2px 6px', borderRadius: '4px', fontSize: '12px' }}>{API_BASE}</code> ...
              </span>
            </div>
          ) : nodes.length === 0 ? (
            <div style={{
              display: 'flex',
              flexDirection: 'column',
              alignItems: 'center',
              justifyContent: 'center',
              height: '100%',
              gap: '16px',
              fontFamily: 'system-ui, sans-serif',
              color: isDark ? '#94a3b8' : '#64748b',
            }}>
              <Users size={48} style={{ color: isDark ? '#475569' : '#94a3b8' }} />
              <span style={{ fontSize: '18px', fontWeight: 600, color: isDark ? '#cbd5e1' : '#334155' }}>
                {emptyGraphMessage(pipelineError, workers.length).title}
              </span>
              <span style={{ fontSize: '13px', textAlign: 'center', maxWidth: '520px', lineHeight: '1.5' }}>
                {emptyGraphMessage(pipelineError, workers.length).detail}
              </span>
            </div>
          ) : (
          <>
          <ReactFlow 
              nodes={nodes} 
              edges={edges} 
              nodeTypes={nodeTypes}
              onNodesChange={onNodesChange}
              onEdgesChange={onEdgesChange}
              onNodeDoubleClick={onNodeDoubleClick}
              onInit={setRfInstance}
              fitView
              fitViewOptions={{ padding: 0.1, maxZoom: 1 }}
          >
            <Background color={isDark ? '#334155' : '#cbd5e1'} gap={16} />
            <Controls style={{ 
                backgroundColor: isDark ? '#1e293b' : 'white', 
                color: isDark ? '#f8fafc' : '#111827',
                fill: isDark ? '#f8fafc' : '#111827',
                border: isDark ? '1px solid #334155' : '1px solid #e5e7eb'
            }} />
          </ReactFlow>

          {/* End-to-end overall metrics overlay */}
          {showMetrics && overallMetrics && (
             <div style={{
                position: 'absolute',
                top: 20,
                right: 20,
                background: isDark ? 'rgba(30, 41, 59, 0.85)' : 'rgba(255, 255, 255, 0.85)',
                backdropFilter: 'blur(8px)',
                border: isDark ? '1px solid #334155' : '1px solid #e2e8f0',
                boxShadow: isDark ? '0 10px 15px -3px rgba(0,0,0,0.5)' : '0 10px 15px -3px rgba(0,0,0,0.1)',
                padding: '16px',
                borderRadius: '12px',
                zIndex: 10,
                minWidth: '220px',
                display: 'flex',
                flexDirection: 'column',
                gap: '12px',
                fontFamily: 'system-ui, sans-serif'
             }}>
                <div style={{ fontSize: '11px', textTransform: 'uppercase', letterSpacing: '0.05em', color: isDark ? '#818cf8' : '#4f46e5', fontWeight: 700, display: 'flex', alignItems: 'center', gap: '6px' }}>
                  <Activity size={14} />
                  {(() => {
                    const w = workers.find((w) => w.id === selectedWorkerId);
                    return w?.pipeline_name ? `${w.pipeline_name} (60s avg)` : 'Pipeline Metrics (60s avg)';
                  })()}
                </div>
                {Object.entries(overallMetrics).map(([key, value]) => (
                  <div key={key} style={{ display: 'flex', justifyContent: 'space-between', fontSize: '13px' }}>
                    <span style={{ color: isDark ? '#94a3b8' : '#64748b' }}>{key.replace(/_/g, ' ')}</span>
                    <span style={{ color: isDark ? '#f8fafc' : '#0f172a', fontWeight: 600, fontFamily: 'monospace' }}>{value}</span>
                  </div>
                ))}
             </div>
          )}

          {/* Worker fleet panel */}
          {showWorkers && (
             <div style={{
                position: 'absolute',
                top: 20,
                left: 20,
                background: isDark ? 'rgba(30, 41, 59, 0.92)' : 'rgba(255, 255, 255, 0.92)',
                backdropFilter: 'blur(8px)',
                border: isDark ? '1px solid #334155' : '1px solid #e2e8f0',
                boxShadow: isDark ? '0 10px 15px -3px rgba(0,0,0,0.5)' : '0 10px 15px -3px rgba(0,0,0,0.1)',
                padding: '16px',
                borderRadius: '12px',
                zIndex: 10,
                minWidth: '280px',
                maxWidth: '340px',
                maxHeight: 'calc(100% - 40px)',
                overflowY: 'auto',
                display: 'flex',
                flexDirection: 'column',
                gap: '12px',
                fontFamily: 'system-ui, sans-serif'
             }}>
                <div style={{ fontSize: '11px', textTransform: 'uppercase', letterSpacing: '0.05em', color: isDark ? '#34d399' : '#059669', fontWeight: 700, display: 'flex', alignItems: 'center', gap: '6px' }}>
                  <Users size={14} />
                  Worker Fleet ({workers.length})
                </div>
                {workers.length === 0 ? (
                  <div style={{ fontSize: '13px', color: isDark ? '#64748b' : '#94a3b8' }}>No workers registered</div>
                ) : workers.map((w) => (
                  <div key={w.id} style={{
                    background: isDark ? 'rgba(15, 23, 42, 0.6)' : 'rgba(248, 250, 252, 0.8)',
                    border: isDark ? '1px solid #1e293b' : '1px solid #e2e8f0',
                    borderRadius: '8px',
                    padding: '10px 12px',
                    display: 'flex',
                    flexDirection: 'column',
                    gap: '6px',
                  }}>
                    <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
                      <span style={{ fontSize: '13px', fontWeight: 600, color: isDark ? '#f8fafc' : '#0f172a', fontFamily: 'monospace', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', maxWidth: '220px' }} title={w.id}>
                        {w.id}
                      </span>
                      <span style={{
                        fontSize: '10px', fontWeight: 700, padding: '2px 6px', borderRadius: '6px',
                        background: w.status === 'active' ? (isDark ? '#065f46' : '#d1fae5') : (isDark ? '#7f1d1d' : '#fee2e2'),
                        color: w.status === 'active' ? (isDark ? '#34d399' : '#059669') : (isDark ? '#fca5a5' : '#dc2626'),
                      }}>
                        {w.status ?? 'unknown'}
                      </span>
                    </div>
                    {w.pipeline_name && (
                      <div style={{ display: 'flex', alignItems: 'center', gap: '6px', fontSize: '12px' }}>
                        <Cpu size={12} style={{ color: '#10b981', flexShrink: 0 }} />
                        <span style={{ color: isDark ? '#94a3b8' : '#64748b' }}>{w.pipeline_name}</span>
                      </div>
                    )}
                    {w.pipeline_config_hash && (
                      <div style={{ display: 'flex', alignItems: 'center', gap: '6px', fontSize: '11px' }}>
                        <Hash size={12} style={{ color: isDark ? '#64748b' : '#94a3b8', flexShrink: 0 }} />
                        <span style={{ fontFamily: 'monospace', color: isDark ? '#64748b' : '#94a3b8' }} title={w.pipeline_config_hash}>
                          {w.pipeline_config_hash?.slice(0, 12)}…
                        </span>
                      </div>
                    )}
                    {w.transport_address && (
                      <div style={{ display: 'flex', alignItems: 'center', gap: '6px', fontSize: '11px' }}>
                        <Globe size={12} style={{ color: isDark ? '#64748b' : '#94a3b8', flexShrink: 0 }} />
                        <span style={{ fontFamily: 'monospace', color: isDark ? '#64748b' : '#94a3b8' }}>{w.transport_address}</span>
                      </div>
                    )}
                    {w.tags && w.tags.length > 0 && (
                      <div style={{ display: 'flex', alignItems: 'center', gap: '4px', flexWrap: 'wrap' }}>
                        <Tag size={11} style={{ color: isDark ? '#64748b' : '#94a3b8', flexShrink: 0 }} />
                        {w.tags.map((tag: string) => (
                          <span key={tag} style={{
                            fontSize: '10px', padding: '1px 6px', borderRadius: '4px',
                            background: isDark ? '#1e293b' : '#f1f5f9',
                            color: isDark ? '#94a3b8' : '#64748b',
                            border: isDark ? '1px solid #334155' : '1px solid #e2e8f0',
                          }}>{tag}</span>
                        ))}
                      </div>
                    )}
                    <div style={{ display: 'flex', gap: '8px', fontSize: '11px', color: isDark ? '#64748b' : '#94a3b8' }} title={`${w.capacity?.active_sessions ?? w.active_sessions ?? 0} active sessions out of ${w.capacity?.max_concurrent ?? w.max_concurrent ?? '?'} maximum concurrent allowed`}>
                      <span>{w.capacity?.active_sessions ?? w.active_sessions ?? 0}/{w.capacity?.max_concurrent ?? w.max_concurrent ?? '?'} sessions</span>
                      {w.aggregate_health && (
                        <span style={{ marginLeft: 'auto', ...healthBadgeStyle(w.aggregate_health, isDark) }} title={`Worker aggregate health: ${w.aggregate_health}`}>
                          {w.aggregate_health}
                        </span>
                      )}
                    </div>
                    {Array.isArray(w.components) && w.components.length > 0 && (
                      <div style={{ display: 'flex', flexWrap: 'wrap', gap: '4px' }}>
                        {w.components.map((c) => {
                          const state = c.health?.state ?? 'not_configured';
                          const reason = c.health?.reason || '';
                          return (
                            <span
                              key={c.id}
                              title={`${c.kind} · ${c.id} · ${state}${reason ? ` — ${reason}` : ''}`}
                              style={{
                                fontSize: '10px',
                                fontFamily: 'monospace',
                                padding: '2px 6px',
                                borderRadius: '4px',
                                ...healthBadgeStyle(state, isDark),
                              }}
                            >
                              {c.kind}
                            </span>
                          );
                        })}
                      </div>
                    )}
                  </div>
                ))}
             </div>
          )}
          </>
          )}

        </main>

        <RawConfigDrawer
          open={showRawConfig}
          onClose={() => setShowRawConfig(false)}
          pipelineName={pipelineName}
          workerId={selectedWorkerId}
          tomlContent={rawToml}
        />
      </div>
  )
}

