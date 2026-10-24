import { useEffect, useRef, useState } from 'react';
import axios from 'axios';
import { Link, useNavigate } from 'react-router-dom';
import { HardDrive, Search, SlidersHorizontal, Copy, Check, X, Activity, Layers } from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';
import { API_BASE } from '../../config/api';
import { formatStarted } from './detail/utils';
import type { ApiEnvelope, PaginatedResponse, PipelineGroup, SessionListItem } from '../../lib/orchestrator';

const API = API_BASE;

type StatusFilter = 'all' | 'completed' | 'in_progress' | 'error';

type TimeRange = 'today' | '24h' | '3d' | '7d' | '30d' | 'all';

const TIME_RANGES: { value: TimeRange; label: string }[] = [
  { value: 'today', label: 'Today' },
  { value: '24h', label: '24h' },
  { value: '3d', label: '3d' },
  { value: '7d', label: '7d' },
  { value: '30d', label: '30d' },
  { value: 'all', label: 'All' },
];

const STATUS_OPTIONS: { value: StatusFilter; label: string }[] = [
  { value: 'all', label: 'All' },
  { value: 'completed', label: 'Completed' },
  { value: 'in_progress', label: 'Live' },
  { value: 'error', label: 'Errors' },
];

/** RFC3339 lower bound for a preset, or null for 'all'. */
function timeRangeSince(range: TimeRange): string | null {
  const DAY_MS = 24 * 60 * 60 * 1000;
  switch (range) {
    case 'today': {
      const midnight = new Date();
      midnight.setHours(0, 0, 0, 0);
      return midnight.toISOString();
    }
    case '24h': return new Date(Date.now() - DAY_MS).toISOString();
    case '3d': return new Date(Date.now() - 3 * DAY_MS).toISOString();
    case '7d': return new Date(Date.now() - 7 * DAY_MS).toISOString();
    case '30d': return new Date(Date.now() - 30 * DAY_MS).toISOString();
    case 'all': return null;
  }
}

/** Session length as m:ss (or h:mm:ss), '—' while still running. */
function formatLength(startedAt: string, endedAt: string | null): string {
  if (!endedAt) return '—';
  const s = Math.max(0, Math.round((new Date(endedAt).getTime() - new Date(startedAt).getTime()) / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, '0');
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`;
}

function statusMeta(status: string): { label: string; dot: string; text: string; pulse: boolean } {
  switch (status) {
    case 'completed': return { label: 'Done', dot: 'bg-emerald-400', text: 'text-emerald-400', pulse: false };
    case 'in_progress': return { label: 'Live', dot: 'bg-sky-400', text: 'text-sky-400', pulse: true };
    case 'error': return { label: 'Error', dot: 'bg-red-400', text: 'text-red-400', pulse: false };
    case 'terminated': return { label: 'Ended', dot: 'bg-orange-400', text: 'text-orange-400', pulse: false };
    default: return { label: status, dot: 'bg-slate-400', text: 'text-slate-400', pulse: false };
  }
}

function Segmented({ options, value, onChange, ariaLabel, isDark }: {
  options: { value: string; label: string }[];
  value: string;
  onChange: (v: string) => void;
  ariaLabel: string;
  isDark: boolean;
}) {
  return (
    <div
      role="group"
      aria-label={ariaLabel}
      className={`inline-flex items-stretch h-8 rounded-lg border p-0.5 ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-300'}`}
    >
      {options.map(opt => (
        <button
          key={opt.value}
          aria-pressed={value === opt.value}
          onClick={() => onChange(opt.value)}
          className={`px-2.5 text-sm rounded-md whitespace-nowrap transition-colors ${
            value === opt.value
              ? isDark ? 'bg-indigo-500/20 text-indigo-300 font-semibold' : 'bg-indigo-50 text-indigo-600 font-semibold'
              : isDark ? 'text-slate-400 hover:text-slate-200' : 'text-gray-500 hover:text-gray-800'
          }`}
        >
          {opt.label}
        </button>
      ))}
    </div>
  );
}

function CopyIdButton({ id, isDark }: { id: string; isDark: boolean }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      aria-label="Copy session ID"
      title="Copy session ID"
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        navigator.clipboard.writeText(id).catch(() => {});
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      }}
      className={`p-1 rounded flex-none opacity-0 group-hover:opacity-100 transition-opacity ${
        copied
          ? 'text-emerald-400 opacity-100'
          : isDark ? 'text-slate-500 hover:text-slate-300 hover:bg-slate-700' : 'text-gray-400 hover:text-gray-600 hover:bg-gray-100'
      }`}
    >
      {copied ? <Check size={12} /> : <Copy size={12} />}
    </button>
  );
}

const GRID_COLS = 'grid grid-cols-[92px_minmax(190px,1.35fr)_minmax(110px,0.9fr)_minmax(150px,1fr)_56px_150px_72px] gap-3 items-center px-4';

/** Page numbers to render, with '…' gaps for long ranges. */
function pageWindow(current: number, count: number): (number | '…')[] {
  if (count <= 7) return Array.from({ length: count }, (_, i) => i + 1);
  const wanted = [1, count, current - 1, current, current + 1]
    .filter(p => p >= 1 && p <= count)
    .sort((a, b) => a - b);
  const out: (number | '…')[] = [];
  let prev = 0;
  for (const p of [...new Set(wanted)]) {
    if (p - prev === 2) out.push(prev + 1);
    else if (p - prev > 2) out.push('…');
    out.push(p);
    prev = p;
  }
  return out;
}

export default function SessionsList() {
  const { isDark } = useTheme();
  const navigate = useNavigate();
  const searchRef = useRef<HTMLInputElement>(null);
  const popRef = useRef<HTMLDivElement>(null);
  const [jumpId, setJumpId] = useState('');
  const [grandTotal, setGrandTotal] = useState<number | null>(null);
  const [page, setPage] = useState(1);
  const [statusFilter, setStatusFilter] = useState<StatusFilter>('all');
  const [pipelineFilter, setPipelineFilter] = useState<string>('all');
  const [pipelineNames, setPipelineNames] = useState<string[]>([]);
  const [hideBenchmark, setHideBenchmark] = useState(true);
  const [hideEmpty, setHideEmpty] = useState(true);
  const [timeRange, setTimeRange] = useState<TimeRange>('all');
  const [filtersOpen, setFiltersOpen] = useState(false);
  const PAGE_SIZE = 25;

  // The loaded page, keyed on the query that produced it. `loading` is derived
  // (no page for the current query yet) rather than stored, so the fetch
  // effect never has to set state synchronously; a stale in-flight response is
  // ignored so it can't overwrite a newer query's rows.
  const queryKey = `${page}|${statusFilter}|${pipelineFilter}|${hideBenchmark}|${hideEmpty}|${timeRange}`;
  const [result, setResult] = useState<{ key: string; sessions: SessionListItem[]; total: number } | null>(null);
  const loading = result === null || result.key !== queryKey;
  const sessions = result?.sessions ?? [];
  const total = result?.total ?? 0;

  useEffect(() => {
    axios.get<ApiEnvelope<{ groups: PipelineGroup[] }>>(`${API}/api/v1/pipelines`)
      .then(res => {
        const groups = res.data?.data?.groups;
        if (Array.isArray(groups)) {
          const names = [...new Set(groups.map(g => g.pipeline_name).filter((n): n is string => Boolean(n)))];
          setPipelineNames(names.sort());
        }
      })
      .catch(() => {});
  }, []);

  // Unfiltered grand total, so the footer can say "filtered from N total"
  // instead of default-on filters silently shrinking the list.
  useEffect(() => {
    axios.get<ApiEnvelope<PaginatedResponse<SessionListItem>>>(`${API}/api/v1/sessions`, { params: { page: 1, per_page: 1 } })
      .then(res => {
        const t = res.data?.data?.total;
        if (typeof t === 'number') setGrandTotal(t);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    let cancelled = false;
    const key = queryKey;
    const params: Record<string, string | number> = { page, per_page: PAGE_SIZE };
    if (statusFilter !== 'all') {
      params.status = statusFilter;
    }
    if (pipelineFilter !== 'all') {
      params.pipeline_name = pipelineFilter;
    }
    if (hideBenchmark) {
      params.exclude_tag = 'benchmark';
    }
    if (hideEmpty) {
      params.min_turns = 1;
    }
    const since = timeRangeSince(timeRange);
    if (since) {
      params.since = since;
    }
    // Keep whatever was showing when the response carries no usable rows
    // (error or unexpected shape), but mark this query as settled.
    const settleWithPrevious = () =>
      setResult(prev => ({ key, sessions: prev?.sessions ?? [], total: prev?.total ?? 0 }));
    axios.get<ApiEnvelope<PaginatedResponse<SessionListItem> | SessionListItem[]>>(`${API}/api/v1/sessions`, { params })
      .then(res => {
        if (cancelled) return;
        const data = res.data.data;
        if (Array.isArray(data)) {
          setResult({ key, sessions: data, total: data.length });
        } else if (data && data.items) {
          setResult({ key, sessions: data.items, total: data.total ?? data.items.length });
        } else {
          settleWithPrevious();
        }
      })
      .catch(err => {
        console.error(err);
        if (!cancelled) settleWithPrevious();
      });
    return () => { cancelled = true; };
  }, [queryKey, page, statusFilter, pipelineFilter, hideBenchmark, hideEmpty, timeRange]);

  // "/" focuses the session-ID search from anywhere on the page.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName;
      if (e.key === '/' && tag !== 'INPUT' && tag !== 'TEXTAREA' && tag !== 'SELECT') {
        e.preventDefault();
        searchRef.current?.focus();
      }
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, []);

  // Close the Filters popover on outside click.
  useEffect(() => {
    if (!filtersOpen) return;
    const onDown = (e: MouseEvent) => {
      if (popRef.current && !popRef.current.contains(e.target as Node)) setFiltersOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
  }, [filtersOpen]);

  const activeHideCount = (hideBenchmark ? 1 : 0) + (hideEmpty ? 1 : 0);
  const anyFilterActive = activeHideCount > 0 || statusFilter !== 'all' || pipelineFilter !== 'all' || timeRange !== 'all';
  const pageCount = Math.max(1, Math.ceil(total / PAGE_SIZE));

  const surface = isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-300';
  const mutedText = isDark ? 'text-slate-400' : 'text-gray-500';
  const faintText = isDark ? 'text-slate-500' : 'text-gray-400';

  return (
    <div className={`p-8 h-full flex flex-col overflow-hidden transition-colors ${isDark ? 'bg-[#0f172a] text-slate-200' : 'bg-gray-50 text-gray-800'}`}>
      <div className="max-w-6xl mx-auto w-full flex flex-col min-h-0 flex-1">
        <div className="flex items-baseline gap-3 mb-4">
          <h1 className={`text-2xl font-bold flex items-center gap-3 ${isDark ? 'text-white' : 'text-gray-900'}`}>
            <HardDrive className="text-indigo-400" size={22} />
            Sessions
          </h1>
          <span className={`text-sm tabular-nums ${faintText}`}>
            {total} session{total !== 1 ? 's' : ''}
          </span>
        </div>

        {/* Toolbar: search + time + status + pipeline + extra filters on one line */}
        <div className="flex items-center gap-2 flex-wrap mb-2">
          <form
            onSubmit={(e) => { e.preventDefault(); const id = jumpId.trim(); if (id) navigate(`/sessions/${encodeURIComponent(id)}`); }}
            className={`relative flex items-center h-8 rounded-lg border flex-1 min-w-[220px] max-w-xs ${surface}`}
          >
            <Search size={14} className={`absolute left-3 ${faintText}`} />
            <input
              ref={searchRef}
              type="text"
              value={jumpId}
              onChange={(e) => setJumpId(e.target.value)}
              placeholder="Go to session ID…"
              className={`w-full h-full bg-transparent rounded-lg pl-9 pr-8 text-xs font-mono outline-none focus:ring-2 focus:ring-indigo-500/60 ${isDark ? 'text-slate-200 placeholder-slate-500' : 'text-gray-800 placeholder-gray-400'}`}
            />
            <kbd className={`absolute right-2.5 px-1.5 py-0.5 text-[10px] font-mono rounded border pointer-events-none ${isDark ? 'border-slate-700 text-slate-500 bg-slate-900' : 'border-gray-200 text-gray-400 bg-gray-50'}`}>/</kbd>
          </form>

          <Segmented
            ariaLabel="Time range"
            options={TIME_RANGES}
            value={timeRange}
            onChange={(v) => { setTimeRange(v as TimeRange); setPage(1); }}
            isDark={isDark}
          />

          <Segmented
            ariaLabel="Status"
            options={STATUS_OPTIONS}
            value={statusFilter}
            onChange={(v) => { setStatusFilter(v as StatusFilter); setPage(1); }}
            isDark={isDark}
          />

          {pipelineNames.length > 0 && (
            <div className="relative flex items-center">
              <Layers size={13} className={`absolute left-2.5 pointer-events-none ${faintText}`} />
              <select
                aria-label="Pipeline"
                value={pipelineFilter}
                onChange={e => { setPipelineFilter(e.target.value); setPage(1); }}
                className={`h-8 pl-8 pr-3 text-sm rounded-lg border appearance-none cursor-pointer ${surface} ${mutedText}`}
              >
                <option value="all">All pipelines</option>
                {pipelineNames.map(name => (
                  <option key={name} value={name}>{name}</option>
                ))}
              </select>
            </div>
          )}

          <div className="relative" ref={popRef}>
            <button
              aria-expanded={filtersOpen}
              onClick={() => setFiltersOpen(o => !o)}
              className={`flex items-center gap-1.5 h-8 px-3 text-sm rounded-lg border transition-colors ${surface} ${mutedText} ${isDark ? 'hover:text-slate-200' : 'hover:text-gray-800'}`}
            >
              <SlidersHorizontal size={13} />
              Filters
              {activeHideCount > 0 && (
                <span className="ml-0.5 px-1.5 rounded-full bg-indigo-500 text-white text-[10px] font-bold leading-4 tabular-nums">
                  {activeHideCount}
                </span>
              )}
            </button>
            {filtersOpen && (
              <div className={`absolute right-0 top-9 z-20 w-64 rounded-xl border p-1.5 shadow-xl ${isDark ? 'bg-slate-800 border-slate-700 shadow-black/50' : 'bg-white border-gray-200 shadow-gray-300/50'}`}>
                <label className={`flex items-start gap-2.5 px-2.5 py-2 rounded-lg cursor-pointer text-sm ${isDark ? 'hover:bg-slate-700/60' : 'hover:bg-gray-50'}`}>
                  <input
                    type="checkbox"
                    checked={hideBenchmark}
                    onChange={() => { setHideBenchmark(prev => !prev); setPage(1); }}
                    className="mt-0.5 accent-indigo-500"
                  />
                  <span>
                    Hide benchmark sessions
                    <span className={`block text-xs ${faintText}`}>Sessions tagged “benchmark”</span>
                  </span>
                </label>
                <label className={`flex items-start gap-2.5 px-2.5 py-2 rounded-lg cursor-pointer text-sm ${isDark ? 'hover:bg-slate-700/60' : 'hover:bg-gray-50'}`}>
                  <input
                    type="checkbox"
                    checked={hideEmpty}
                    onChange={() => { setHideEmpty(prev => !prev); setPage(1); }}
                    className="mt-0.5 accent-indigo-500"
                  />
                  <span>
                    Hide empty sessions
                    <span className={`block text-xs ${faintText}`}>Sessions with 0 turns</span>
                  </span>
                </label>
              </div>
            )}
          </div>
        </div>

        {/* Active-filter chips: a chip means the list is filtered */}
        {activeHideCount > 0 && (
          <div className="flex items-center gap-1.5 flex-wrap mb-3">
            {hideBenchmark && (
              <span className={`flex items-center gap-1 pl-2.5 pr-1 py-0.5 text-xs rounded-full border ${isDark ? 'bg-indigo-500/15 border-indigo-500/40 text-indigo-300' : 'bg-indigo-50 border-indigo-200 text-indigo-600'}`}>
                Benchmarks hidden
                <button
                  aria-label="Show benchmark sessions"
                  onClick={() => { setHideBenchmark(false); setPage(1); }}
                  className="p-0.5 rounded-full hover:bg-indigo-500/25"
                >
                  <X size={10} />
                </button>
              </span>
            )}
            {hideEmpty && (
              <span className={`flex items-center gap-1 pl-2.5 pr-1 py-0.5 text-xs rounded-full border ${isDark ? 'bg-indigo-500/15 border-indigo-500/40 text-indigo-300' : 'bg-indigo-50 border-indigo-200 text-indigo-600'}`}>
                Empty hidden
                <button
                  aria-label="Show empty sessions"
                  onClick={() => { setHideEmpty(false); setPage(1); }}
                  className="p-0.5 rounded-full hover:bg-indigo-500/25"
                >
                  <X size={10} />
                </button>
              </span>
            )}
            <button
              onClick={() => { setHideBenchmark(false); setHideEmpty(false); setPage(1); }}
              className={`px-1.5 py-0.5 text-xs rounded ${faintText} ${isDark ? 'hover:text-slate-200' : 'hover:text-gray-700'}`}
            >
              Clear all
            </button>
          </div>
        )}

        {loading ? (
          <div className="flex justify-center p-12">
            <Activity className="animate-spin text-indigo-500" size={32} />
          </div>
        ) : sessions.length === 0 ? (
          <div className={`flex flex-col items-center justify-center py-24 rounded-xl border gap-3 ${isDark ? 'bg-slate-800/50 border-slate-700' : 'bg-white border-gray-200'}`}>
            <div className={`w-14 h-14 rounded-2xl flex items-center justify-center ${isDark ? 'bg-slate-700/60' : 'bg-gray-100'}`}>
              <Activity size={24} className={faintText} />
            </div>
            <span className={`text-base font-semibold ${isDark ? 'text-slate-300' : 'text-gray-600'}`}>No sessions found</span>
            <span className={`text-sm text-center max-w-xs leading-relaxed ${faintText}`}>
              {anyFilterActive
                ? 'No sessions match the current filters.'
                : 'Start a voice pipeline session from the Playground to see data here'}
            </span>
            {anyFilterActive && (
              <button
                onClick={() => {
                  setStatusFilter('all'); setPipelineFilter('all'); setTimeRange('all');
                  setHideBenchmark(false); setHideEmpty(false); setPage(1);
                }}
                className={`px-3 py-1.5 text-sm rounded-lg border ${isDark ? 'bg-indigo-500/20 border-indigo-500/50 text-indigo-300 hover:bg-indigo-500/30' : 'bg-indigo-50 border-indigo-200 text-indigo-600 hover:bg-indigo-100'}`}
              >
                Clear all filters
              </button>
            )}
          </div>
        ) : (
          <>
          {/* Session table: header sticks while rows scroll */}
          <div className={`flex-1 min-h-0 rounded-xl border overflow-hidden flex flex-col ${isDark ? 'border-slate-700' : 'border-gray-200'}`}>
            <div className="flex-1 min-h-0 overflow-auto">
              <div className="min-w-[900px]">
                <div className={`${GRID_COLS} sticky top-0 z-10 h-9 text-[10px] font-semibold uppercase tracking-wider border-b ${isDark ? 'bg-slate-900 border-slate-700/60 text-slate-500' : 'bg-gray-100 border-gray-200 text-gray-400'}`}>
                  <span>Status</span>
                  <span>Session</span>
                  <span>Pipeline</span>
                  <span>Providers</span>
                  <span className="text-right">Turns</span>
                  <span>Started</span>
                  <span className="text-right">Length</span>
                </div>

                {sessions.map((session) => {
                  const id = session.id;
                  const status = statusMeta(session.status ?? 'completed');
                  const startedAt = session.started_at;
                  const endedAt = session.ended_at;
                  const turnCount = session.turn_count ?? 0;
                  const tags: string[] = Array.isArray(session.tags) ? session.tags : [];
                  const when = startedAt ? formatStarted(startedAt) : null;

                  return (
                    <Link
                      key={id}
                      to={`/sessions/${id}`}
                      className={`${GRID_COLS} group h-12 border-b last:border-b-0 transition-colors ${
                        isDark ? 'border-slate-800 hover:bg-slate-800' : 'border-gray-100 hover:bg-gray-50'
                      }`}
                    >
                      <span className={`flex items-center gap-2 text-sm ${status.text}`}>
                        <i className={`w-[7px] h-[7px] rounded-full flex-none ${status.dot} ${status.pulse ? 'animate-pulse' : ''}`} />
                        {status.label}
                      </span>

                      <span className="flex items-center gap-2 min-w-0">
                        <span className={`font-mono text-sm truncate group-hover:text-indigo-400 transition-colors ${isDark ? 'text-slate-200' : 'text-gray-800'}`}>
                          {id}
                        </span>
                        <CopyIdButton id={id} isDark={isDark} />
                        {tags.map(tag => (
                          <span key={tag} className={`px-2 py-px rounded-full text-[10px] font-semibold border flex-none ${
                            tag === 'benchmark'
                              ? 'bg-amber-500/10 border-amber-500/30 text-amber-400'
                              : isDark ? 'bg-violet-500/10 border-violet-500/30 text-violet-400' : 'bg-violet-50 border-violet-200 text-violet-600'
                          }`}>
                            {tag}
                          </span>
                        ))}
                      </span>

                      <span className={`text-sm truncate ${mutedText}`}>
                        {session.pipeline_name || '—'}
                      </span>

                      <span className={`text-sm truncate ${mutedText}`}>
                        {session.stt_provider || session.tts_provider ? (
                          <>
                            {session.stt_provider && <><span className={`text-[11px] ${faintText}`}>STT</span> {session.stt_provider}</>}
                            {session.stt_provider && session.tts_provider && ' · '}
                            {session.tts_provider && <><span className={`text-[11px] ${faintText}`}>TTS</span> {session.tts_provider}</>}
                          </>
                        ) : '—'}
                      </span>

                      <span className={`text-sm text-right tabular-nums ${turnCount === 0 ? faintText : isDark ? 'text-slate-200' : 'text-gray-800'}`}>
                        {turnCount}
                      </span>

                      <span className={`text-sm ${mutedText}`}>
                        {when ? (
                          <>
                            {when.rel}
                            <span className={`block text-xs leading-tight ${faintText}`}>{when.abs}</span>
                          </>
                        ) : '—'}
                      </span>

                      <span className={`text-sm text-right tabular-nums ${mutedText}`}>
                        {startedAt ? formatLength(startedAt, endedAt ?? null) : '—'}
                      </span>
                    </Link>
                  );
                })}
              </div>
            </div>
          </div>

          {/* Footer: range + honest filtered-from note + numbered pager */}
          <div className="flex items-center justify-between mt-3 flex-wrap gap-2">
            <span className={`text-xs tabular-nums ${faintText}`}>
              Showing {(page - 1) * PAGE_SIZE + 1}–{Math.min(page * PAGE_SIZE, total)} of {total}
              {grandTotal !== null && grandTotal > total ? ` · filtered from ${grandTotal} total` : ''}
            </span>
            {pageCount > 1 && (
              <div className="flex gap-1">
                <button
                  disabled={page <= 1}
                  onClick={() => setPage(p => Math.max(1, p - 1))}
                  aria-label="Previous page"
                  className={`min-w-7 h-7 px-1.5 text-sm rounded-md disabled:opacity-35 disabled:cursor-default ${mutedText} ${isDark ? 'hover:bg-slate-800 hover:text-slate-200' : 'hover:bg-gray-100 hover:text-gray-800'}`}
                >
                  ‹
                </button>
                {pageWindow(page, pageCount).map((p, i) => p === '…' ? (
                  <span key={`gap-${i}`} className={`min-w-7 h-7 leading-7 text-center text-sm ${faintText}`}>…</span>
                ) : (
                  <button
                    key={p}
                    aria-current={p === page ? 'page' : undefined}
                    onClick={() => setPage(p)}
                    className={`min-w-7 h-7 px-1.5 text-sm rounded-md tabular-nums ${
                      p === page
                        ? isDark ? 'bg-indigo-500/20 text-indigo-300 font-semibold' : 'bg-indigo-50 text-indigo-600 font-semibold'
                        : `${mutedText} ${isDark ? 'hover:bg-slate-800 hover:text-slate-200' : 'hover:bg-gray-100 hover:text-gray-800'}`
                    }`}
                  >
                    {p}
                  </button>
                ))}
                <button
                  disabled={page >= pageCount}
                  onClick={() => setPage(p => p + 1)}
                  aria-label="Next page"
                  className={`min-w-7 h-7 px-1.5 text-sm rounded-md disabled:opacity-35 disabled:cursor-default ${mutedText} ${isDark ? 'hover:bg-slate-800 hover:text-slate-200' : 'hover:bg-gray-100 hover:text-gray-800'}`}
                >
                  ›
                </button>
              </div>
            )}
          </div>
          </>
        )}
      </div>
    </div>
  );
}
