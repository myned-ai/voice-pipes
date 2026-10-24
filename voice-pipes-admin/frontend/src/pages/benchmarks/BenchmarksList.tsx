import { useEffect, useRef, useState } from 'react';
import { Link, useNavigate } from 'react-router-dom';
import axios from 'axios';
import { FlaskConical, Play, Trash2, Eye, GitCompare, Loader2, BarChart2, Server, ChevronDown, Plus, WifiOff } from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';
import { API_BASE } from '../../config/api';
import type { Experiment, ExperimentStatus } from '../../components/benchmark/types';

interface WorkerInfo {
  id: string;
  status?: string;
  pipeline_name?: string | null;
  tags?: string[];
  capacity?: { max_concurrent?: number; active_sessions?: number };
}

/**
 * Multi-select + "Compare Selected" is switched off in the list UI for now
 * (the /benchmarks/compare page itself still works when reached by URL).
 * Flip this to re-enable the row checkboxes and the compare button.
 */
const COMPARE_ENABLED = false;

const STATUS_BADGE: Record<ExperimentStatus, { bg: string; text: string; label: string }> = {
  draft:    { bg: 'bg-slate-500/20', text: 'text-slate-400', label: 'Draft' },
  running:  { bg: 'bg-amber-500/20', text: 'text-amber-400', label: 'Running' },
  complete: { bg: 'bg-emerald-500/20', text: 'text-emerald-400', label: 'Complete' },
  failed:   { bg: 'bg-red-500/20', text: 'text-red-400', label: 'Failed' },
};

export default function BenchmarksList() {
  const { isDark } = useTheme();
  const navigate = useNavigate();
  const [experiments, setExperiments] = useState<Experiment[]>([]);
  const [loading, setLoading] = useState(true);
  const [fetchError, setFetchError] = useState<string | null>(null);
  const [statusFilter, setStatusFilter] = useState<ExperimentStatus | 'all'>('all');
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [workers, setWorkers] = useState<WorkerInfo[]>([]);
  const [runMenuFor, setRunMenuFor] = useState<string | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);

  const fetchExperiments = () => {
    axios.get(`${API_BASE}/api/v1/benchmark/experiments`)
      .then(res => {
        const data = res.data;
        setExperiments(Array.isArray(data) ? data : data?.data ?? []);
        setFetchError(null);
        setLoading(false);
      })
      .catch(err => {
        setFetchError(err?.message ?? 'Failed to reach orchestrator');
        setLoading(false);
      });
  };

  useEffect(() => {
    fetchExperiments();
  }, []);

  // Fetch worker fleet (poll every 5s so capacity/status stay fresh).
  useEffect(() => {
    const fetchWorkers = () => {
      axios.get(`${API_BASE}/api/v1/workers`)
        .then(res => {
          const payload = res.data?.data ?? res.data;
          const items: WorkerInfo[] = payload?.items ?? payload?.workers ?? (Array.isArray(payload) ? payload : []);
          setWorkers(items);
        })
        .catch(() => {});
    };
    fetchWorkers();
    const id = setInterval(fetchWorkers, 5000);
    return () => clearInterval(id);
  }, []);

  // Close the run-menu popover on outside click / Escape.
  useEffect(() => {
    if (!runMenuFor) return;
    const onDocClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setRunMenuFor(null);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setRunMenuFor(null);
    };
    document.addEventListener('mousedown', onDocClick);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDocClick);
      document.removeEventListener('keydown', onKey);
    };
  }, [runMenuFor]);

  // Auto-poll while any experiment is running
  useEffect(() => {
    const hasRunning = experiments.some(e => e.status === 'running');
    if (!hasRunning) return;
    const interval = setInterval(fetchExperiments, 5000);
    return () => clearInterval(interval);
  }, [experiments]);

  const filtered = statusFilter === 'all'
    ? experiments
    : experiments.filter(e => e.status === statusFilter);

  const toggleSelect = (id: string) => {
    setSelected(prev => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  };

  const handleDelete = async (id: string) => {
    if (!confirm('Delete this experiment?')) return;
    try {
      await axios.delete(`${API_BASE}/api/v1/benchmark/experiments/${encodeURIComponent(id)}`);
      setExperiments(prev => prev.filter(e => e.id !== id));
      setSelected(prev => { const n = new Set(prev); n.delete(id); return n; });
    } catch (err) {
      console.error('Delete failed', err);
    }
  };

  const handleRun = async (id: string, workerId?: string) => {
    // A re-run of a finished experiment overwrites results.json; warn first.
    const exp = experiments.find(e => e.id === id);
    const isRerun = exp?.status === 'complete' || exp?.status === 'failed';
    if (isRerun && !confirm(
      `This experiment is already ${exp?.status}. Re-running will overwrite the existing results. Continue?`,
    )) {
      return;
    }
    try {
      const params = new URLSearchParams();
      if (workerId) params.set('worker_id', workerId);
      if (isRerun) params.set('force', 'true');
      const qs = params.toString() ? `?${params.toString()}` : '';
      await axios.post(`${API_BASE}/api/v1/benchmark/experiments/${encodeURIComponent(id)}/run${qs}`);
      setRunMenuFor(null);
      fetchExperiments();
    } catch (err) {
      console.error('Run failed', err);
    }
  };

  const handleCompare = () => {
    if (selected.size < 2) return;
    navigate(`/benchmarks/compare?ids=${[...selected].join(',')}`);
  };

  const fmtDate = (iso?: string) => {
    if (!iso) return '—';
    return new Date(iso).toLocaleString(undefined, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
  };

  return (
    <div className="p-8 overflow-y-auto h-full">
      {/* Header */}
      <div className="flex items-center justify-between mb-6">
        <div className="flex items-center gap-3">
          <FlaskConical size={24} className="text-indigo-400" />
          <h1 className={`text-2xl font-bold ${isDark ? 'text-white' : 'text-gray-900'}`}>Benchmarks</h1>
        </div>
        <Link
          to="/benchmarks/new"
          className="inline-flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium bg-indigo-500 hover:bg-indigo-600 text-white transition-colors"
        >
          <Plus size={16} /> New experiment
        </Link>
      </div>

      {/* Filters */}
      <div className="flex items-center gap-2 mb-6">
        {(['all', 'draft', 'running', 'complete', 'failed'] as const).map(s => (
          <button
            key={s}
            onClick={() => setStatusFilter(s)}
            className={`px-3 py-1.5 rounded-lg text-xs font-medium transition-colors ${
              statusFilter === s
                ? 'bg-indigo-500 text-white'
                : isDark ? 'bg-slate-800 text-slate-400 hover:bg-slate-700' : 'bg-gray-100 text-gray-600 hover:bg-gray-200'
            }`}
          >
            {s === 'all' ? 'All' : s.charAt(0).toUpperCase() + s.slice(1)}
          </button>
        ))}

        {COMPARE_ENABLED && selected.size >= 2 && (
          <button
            onClick={handleCompare}
            className="ml-auto flex items-center gap-2 px-4 py-1.5 rounded-lg text-xs font-medium bg-indigo-500 text-white hover:bg-indigo-600 transition-colors"
          >
            <GitCompare size={14} />
            Compare Selected ({selected.size})
          </button>
        )}
      </div>

      {/* List */}
      {loading ? (
        <div className="flex items-center justify-center py-20">
          <Loader2 size={24} className="animate-spin text-indigo-400" />
        </div>
      ) : fetchError && experiments.length === 0 ? (
        <div className={`flex flex-col items-center justify-center py-24 rounded-xl border gap-3 ${isDark ? 'bg-slate-800/50 border-slate-700' : 'bg-white border-gray-200'}`}>
          <div className={`w-14 h-14 rounded-2xl flex items-center justify-center ${isDark ? 'bg-slate-700/60' : 'bg-gray-100'}`}>
            <WifiOff size={24} className={isDark ? 'text-slate-500' : 'text-gray-400'} />
          </div>
          <span className={`text-base font-semibold ${isDark ? 'text-slate-300' : 'text-gray-600'}`}>Orchestrator Offline</span>
          <span className={`text-sm text-center max-w-md leading-relaxed ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
            Benchmark experiments are served by the orchestrator. Could not reach{' '}
            <code className={`px-1.5 py-0.5 rounded text-xs ${isDark ? 'bg-slate-700 text-slate-300' : 'bg-gray-100 text-gray-600'}`}>{API_BASE}</code>.
            Start the orchestrator and this list will refresh automatically.
          </span>
        </div>
      ) : filtered.length === 0 ? (
        <div className={`flex flex-col items-center justify-center py-24 rounded-xl border gap-3 ${isDark ? 'bg-slate-800/50 border-slate-700' : 'bg-white border-gray-200'}`}>
          <div className={`w-14 h-14 rounded-2xl flex items-center justify-center ${isDark ? 'bg-slate-700/60' : 'bg-gray-100'}`}>
            <BarChart2 size={24} className={isDark ? 'text-slate-500' : 'text-gray-400'} />
          </div>
          <span className={`text-base font-semibold ${isDark ? 'text-slate-300' : 'text-gray-600'}`}>No experiments yet</span>
          <span className={`text-sm text-center max-w-xs leading-relaxed ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
            Create an experiment to compare pipeline variants side-by-side
          </span>
        </div>
      ) : (
        <div className="space-y-3">
          {filtered.map(exp => {
            const badge = STATUS_BADGE[exp.status];
            return (
              <div
                key={exp.id}
                className={`rounded-xl border p-5 transition-colors ${
                  isDark ? 'bg-slate-800 border-slate-700 hover:border-slate-600' : 'bg-white border-gray-200 hover:border-gray-300'
                } ${selected.has(exp.id) ? (isDark ? 'ring-2 ring-indigo-500/40' : 'ring-2 ring-indigo-400/50') : ''}`}
              >
                <div className="flex items-start gap-4">
                  {/* Checkbox (hidden while COMPARE_ENABLED is off) */}
                  <input
                    type="checkbox"
                    checked={selected.has(exp.id)}
                    onChange={() => toggleSelect(exp.id)}
                    className={`mt-1 rounded border-slate-600 text-indigo-500 focus:ring-indigo-500 ${COMPARE_ENABLED ? '' : 'hidden'}`}
                  />

                  {/* Content */}
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-3 mb-1">
                      <Link
                        to={`/benchmarks/${exp.id}`}
                        className={`text-lg font-semibold hover:underline ${isDark ? 'text-white' : 'text-gray-900'}`}
                      >
                        {exp.name}
                      </Link>
                      <span className={`px-2 py-0.5 rounded-full text-xs font-medium ${badge.bg} ${badge.text}`}>
                        {badge.label}
                        {exp.status === 'running' && <Loader2 size={10} className="inline ml-1 animate-spin" />}
                      </span>
                    </div>

                    {exp.description && (
                      <p className={`text-sm mb-2 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>{exp.description}</p>
                    )}

                    <div className={`flex items-center gap-4 text-xs ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                      <span>{exp.variants.length} variant{exp.variants.length !== 1 ? 's' : ''}</span>
                      <span>{exp.repetitions} rep{exp.repetitions !== 1 ? 's' : ''}</span>
                      <span>Created {fmtDate(exp.created_at)}</span>
                      {exp.completed_at && <span>Completed {fmtDate(exp.completed_at)}</span>}
                    </div>
                  </div>

                  {/* Actions */}
                  <div className="flex items-center gap-2 shrink-0">
                    {(exp.status === 'draft' || exp.status === 'complete' || exp.status === 'failed') && (
                      <div className="relative" ref={runMenuFor === exp.id ? menuRef : undefined}>
                        <button
                          onClick={() => setRunMenuFor(runMenuFor === exp.id ? null : exp.id)}
                          className="flex items-center gap-1 px-2 py-1.5 rounded-lg text-emerald-400 hover:bg-emerald-500/10 transition-colors"
                          title="Run experiment on a worker"
                        >
                          <Play size={16} />
                          <ChevronDown size={12} />
                        </button>
                        {runMenuFor === exp.id && (
                          <div
                            className={`absolute right-0 mt-2 w-72 rounded-lg border shadow-xl z-20 ${
                              isDark ? 'bg-slate-900 border-slate-700' : 'bg-white border-gray-200'
                            }`}
                          >
                            <div className={`px-3 py-2 text-[11px] uppercase tracking-wide font-semibold ${
                              isDark ? 'text-slate-400 border-b border-slate-700' : 'text-gray-500 border-b border-gray-200'
                            }`}>
                              Run on worker
                            </div>
                            <button
                              onClick={() => handleRun(exp.id)}
                              className={`w-full text-left px-3 py-2 text-sm flex items-center gap-2 transition-colors ${
                                isDark ? 'hover:bg-slate-800 text-slate-200' : 'hover:bg-gray-50 text-gray-800'
                              }`}
                            >
                              <Server size={14} className="text-indigo-400" />
                              <div className="flex-1">
                                <div className="font-medium">Auto (match variant tags)</div>
                                <div className={`text-xs ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                                  Least-loaded worker whose tags include all variant tags
                                </div>
                              </div>
                            </button>
                            <div className={`max-h-72 overflow-y-auto ${
                              isDark ? 'border-t border-slate-700' : 'border-t border-gray-200'
                            }`}>
                              {workers.length === 0 ? (
                                <div className={`px-3 py-3 text-xs ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                                  No workers registered. Start one with <code>cargo run … serve</code>.
                                </div>
                              ) : (
                                workers.map(w => {
                                  const active = (w.status ?? '').toLowerCase() === 'active';
                                  const cap = w.capacity ?? {};
                                  const busy = (cap.active_sessions ?? 0) >= (cap.max_concurrent ?? 1);
                                  const disabled = !active || busy;
                                  return (
                                    <button
                                      key={w.id}
                                      disabled={disabled}
                                      onClick={() => handleRun(exp.id, w.id)}
                                      className={`w-full text-left px-3 py-2 text-sm flex items-center gap-2 transition-colors ${
                                        disabled
                                          ? (isDark ? 'opacity-50 cursor-not-allowed' : 'opacity-50 cursor-not-allowed')
                                          : (isDark ? 'hover:bg-slate-800 text-slate-200' : 'hover:bg-gray-50 text-gray-800')
                                      }`}
                                      title={disabled ? (active ? 'No spare capacity' : `Status: ${w.status}`) : `Run on ${w.id}`}
                                    >
                                      <Server size={14} className={active ? 'text-emerald-400' : 'text-slate-500'} />
                                      <div className="flex-1 min-w-0">
                                        <div className="font-medium truncate">{w.id}</div>
                                        <div className={`text-xs truncate ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                                          {(w.pipeline_name ?? 'pipeline?')} · {(cap.active_sessions ?? 0)}/{(cap.max_concurrent ?? '?')} busy
                                          {w.tags && w.tags.length > 0 ? ` · [${w.tags.join(', ')}]` : ''}
                                        </div>
                                      </div>
                                    </button>
                                  );
                                })
                              )}
                            </div>
                          </div>
                        )}
                      </div>
                    )}
                    <Link
                      to={`/benchmarks/${exp.id}`}
                      className={`p-2 rounded-lg transition-colors ${isDark ? 'text-slate-400 hover:bg-slate-700' : 'text-gray-400 hover:bg-gray-100'}`}
                      title="View details"
                    >
                      <Eye size={16} />
                    </Link>
                    <button
                      onClick={() => handleDelete(exp.id)}
                      className="p-2 rounded-lg text-red-400 hover:bg-red-500/10 transition-colors"
                      title="Delete experiment"
                    >
                      <Trash2 size={16} />
                    </button>
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
