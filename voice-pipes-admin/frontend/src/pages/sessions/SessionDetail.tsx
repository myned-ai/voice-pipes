import { Fragment, useEffect, useState, useCallback, useSyncExternalStore } from 'react';
import axios from 'axios';
import { useParams, Link } from 'react-router-dom';
import { ArrowLeft, PlayCircle, Database, PanelRightClose, PanelRightOpen, ClipboardCopy, Check, AlertTriangle, Activity } from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';
import type { Utterance, LlmTiming, OrchTurn, SessionEvent, SessionData, ReportAnomaly, DeviceInfo } from './detail/types';
import { normalizeLlmTiming, formatDuration, formatStarted, isRealtimeSession, formatDeviceBadge } from './detail/utils';
import type { DeviceTone } from './detail/utils';
import { StatusBadge } from '../../components/StatusBadge';
import { API_BASE } from '../../config/api';

// Extracted sub-components
import { TurnCard } from './detail/TurnCard';
import { DroppedUtteranceCard } from './detail/DroppedUtteranceCard';
import { OrchestratorTurnCard } from './detail/OrchestratorTurnCard';
import { EventCardRenderer } from './detail/EventCards';
import { ConversationPlayer } from './detail/AudioPlayers';
import { TelemetryPanel } from './detail/TelemetryPanel';
import { ClientToolCallCard, ClientToolsPanel } from './detail/ClientToolsPanel';
import { SessionDescriptorsContext } from '../playground/SessionDescriptorsContext';
import { SessionSummaryDialog } from './detail/SessionSummaryDialog';

const API = API_BASE;

// Telemetry panel is w-1/3 of the viewport — below this width that third is
// too cramped to be readable, so the panel defaults to closed. The manual
// toggle still works either way; it just changes the default.
const TELEMETRY_MIN_VIEWPORT = '(min-width: 1280px)';

/** Whether the viewport currently matches a media query, kept live.
 *  `matchMedia` is an external store: subscribe to its `change` events and
 *  read the current match on every render, so a query change or a resize
 *  between render and subscription is never missed. */
function useMediaQuery(query: string): boolean {
  const subscribe = useCallback((onChange: () => void) => {
    const mq = window.matchMedia(query);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, [query]);
  return useSyncExternalStore(subscribe, () => window.matchMedia(query).matches);
}

// ── Main component ──────────────────────────────────────────

export default function SessionDetail() {
  const { isDark } = useTheme();
  const { id } = useParams<{ id: string }>();
  // One path segment, whatever characters the id carries.
  const sessionPath = encodeURIComponent(id ?? "");
  const [data, setData] = useState<SessionData | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Open by default only when the viewport is wide enough for a useful
  // third; an explicit user toggle overrides the responsive default until
  // they toggle again (or the page reloads).
  const telemetryFits = useMediaQuery(TELEMETRY_MIN_VIEWPORT);
  const [telemetryPref, setTelemetryPref] = useState<boolean | null>(null);
  const showTelemetry = telemetryPref ?? telemetryFits;
  const [viewMode, setViewMode] = useState<'timeline' | 'turns'>('turns');
  const [eventsTotal, setEventsTotal] = useState(0);
  const [eventsLoaded, setEventsLoaded] = useState(0);
  const [reportCopyState, setReportCopyState] = useState<'idle' | 'loading' | 'copied' | 'error'>('idle');
  const [showSummary, setShowSummary] = useState(false);
  const [idCopied, setIdCopied] = useState(false);
  // Server-detected anomalies grouped by utterance id.  Sourced from the
  // same `/report-for-llm?format=json` bundle so we don't re-implement the
  // detection logic client-side. Chips render on the relevant TurnCards.
  const [anomaliesByUtt, setAnomaliesByUtt] = useState<Record<string, ReportAnomaly[]>>({});
  const EVENTS_PAGE = 200;

  useEffect(() => {
    axios.get(`${API}/api/v1/sessions/${sessionPath}`)
      .then(res => {
        const d = res.data.data;
        setData(d);
        setEventsTotal(d.events_total ?? d.events?.length ?? 0);
        setEventsLoaded(d.events?.length ?? 0);
        setLoading(false);
      })
      .catch(err => {
        setError(err.response?.data?.detail || err.message);
        setLoading(false);
      });
  }, [id, sessionPath]);

  // Side-fetch the diagnostic bundle once per session so TurnCards can
  // render anomaly chips without re-deriving the rules in the client.
  // Fails silently — the cards still render without chips if this 404s.
  useEffect(() => {
    if (!id) return;
    let cancelled = false;
    axios.get(`${API}/api/v1/sessions/${sessionPath}/report-for-llm`, { params: { format: 'json' } })
      .then(res => {
        if (cancelled) return;
        // Endpoint shape: `{ data: { anomalies: [...], ... } }` (matches the
        // wrapping used by the regular session detail endpoint).
        const anoms: ReportAnomaly[] = res.data?.data?.anomalies ?? res.data?.anomalies ?? [];
        const grouped: Record<string, ReportAnomaly[]> = {};
        for (const a of anoms) {
          const key = a.utt == null ? '_session' : String(a.utt);
          (grouped[key] ??= []).push(a);
        }
        setAnomaliesByUtt(grouped);
      })
      .catch(() => { /* non-fatal: chips simply won't render */ });
    return () => { cancelled = true; };
  }, [id, sessionPath]);

  const copyId = useCallback(async () => {
    if (!id) return;
    try {
      await navigator.clipboard.writeText(id);
      setIdCopied(true);
      setTimeout(() => setIdCopied(false), 1500);
    } catch (err) {
      console.error('Failed to copy session id:', err);
    }
  }, [id]);

  const loadMoreEvents = useCallback(() => {
    axios.get(`${API}/api/v1/sessions/${sessionPath}`, { params: { events_offset: eventsLoaded, events_limit: EVENTS_PAGE } })
      .then(res => {
        const d = res.data.data;
        const more = d.events ?? [];
        setData((prev: SessionData | null) => ({
          ...(prev ?? {}),
          events: [...(prev?.events ?? []), ...(more as SessionEvent[])],
        } as SessionData));
        setEventsLoaded(prev => prev + more.length);
      });
  }, [sessionPath, eventsLoaded]);

  // Fetches the LLM-friendly diagnostic report and copies it to the clipboard.
  // Backend renders the markdown wrapper; we just transport it verbatim.
  const copyReportForLlm = useCallback(async () => {
    if (!id) return;
    setReportCopyState('loading');
    try {
      const res = await axios.get(`${API}/api/v1/sessions/${sessionPath}/report-for-llm`, {
        // Include the compact event_timeline so the LLM can see ordering /
        // propagation context (especially useful when something went wrong
        // mid-pipeline that the aggregates don't expose).
        params: { include_events: true },
        responseType: 'text',
        // axios coerces JSON-looking strings by default; force text so the
        // fenced ```json block in the markdown is preserved as-is.
        transformResponse: [(data) => data],
      });
      await navigator.clipboard.writeText(typeof res.data === 'string' ? res.data : String(res.data));
      setReportCopyState('copied');
      setTimeout(() => setReportCopyState('idle'), 2000);
    } catch (err) {
      console.error('Failed to copy report-for-llm:', err);
      setReportCopyState('error');
      setTimeout(() => setReportCopyState('idle'), 2500);
    }
  }, [id, sessionPath]);

  if (loading) return <div className={`p-8 ${isDark ? 'text-white' : 'text-gray-900'}`}>Loading session {id}...</div>;
  if (error) return <div className="p-8 text-red-400">Error: {error}</div>;
  if (!data) return <div className={`p-8 ${isDark ? 'text-white' : 'text-gray-900'}`}>No data available.</div>;

  const hasUtterances = data.utterances && data.utterances.length > 0;
  const hasEvents = data.events && data.events.length > 0;
  const hasOrchTurns = data.turns && data.turns.length > 0;

  // Session-level provider identity (shown in the header strip). Per-turn
  // cards compare against these so a constant provider isn't repeated on every
  // card — only a genuine mid-session provider switch surfaces.
  const sessionSttProvider = data.providers?.stt?.provider ?? data.metadata?.stt_provider;
  const sessionTtsProvider = data.providers?.tts?.provider ?? data.metadata?.tts_provider;

  // Per-utterance telemetry is normalized server-side by the BFF.
  // Each `utterance.{stt_timing,llm_timing,llm_detail,tts_timing,tts_detail}`
  // is matched via `pipeline_events.jsonl` lifecycle markers, so we no longer
  // pair flat event arrays by index here.
  //
  // Helper: pick the richest LLM payload available for a turn.
  const pickLlmTiming = (utt: Utterance): LlmTiming | undefined => {
    if (utt.llm_detail) {
      // llm_detail uses the same payload shape as llm_timing; normalise so the
      // UI sees the consistent {total_ms, generate_ms, prompt_tokens, ...} shape.
      return normalizeLlmTiming({ event_type: 'llm_detail', payload: utt.llm_detail as unknown as Record<string, unknown> });
    }
    if (utt.llm_timing) {
      return normalizeLlmTiming({ event_type: 'llm_timing', payload: utt.llm_timing as unknown as Record<string, unknown> });
    }
    return undefined;
  };

  return (
    <div className={`flex h-full w-full overflow-hidden transition-colors ${isDark ? 'bg-[#0f172a] text-slate-300' : 'bg-gray-50 text-gray-700'}`}>
      
      {/* LEFT: Transcript and Details */}
      <div className={`overflow-y-auto min-w-0 ${showTelemetry ? 'flex-1' : 'w-full'} ${showTelemetry ? (isDark ? 'border-r border-slate-800' : 'border-r border-gray-200') : ''}`}>
        {/* Sticky compact header: verdict-first identity chips + one aligned
            action row, with the UUID and provider strip demoted to a quiet
            mono meta line. Rows flex-wrap so narrow windows stack the action
            group under the chips instead of truncating labels. */}
        <div className={`sticky top-0 z-20 px-4 xl:px-6 py-3 border-b backdrop-blur-sm ${isDark ? 'bg-[#0f172a]/90 border-slate-800' : 'bg-gray-50/90 border-gray-200'}`}>
          <div className="flex items-center gap-x-3 gap-y-2 flex-wrap">
            <Link to="/sessions" className={`p-1.5 rounded-lg transition-colors shrink-0 ${isDark ? 'text-indigo-400 hover:text-indigo-300 hover:bg-slate-800' : 'text-indigo-600 hover:text-indigo-500 hover:bg-gray-100'}`}>
              <ArrowLeft size={18} />
            </Link>

            {/* Verdict chip — pre-computed by BFF (`_compute_verdict`). The
                underlying logic counts warn-level anomalies by component
                and folds in the `unstable: true` flag from
                `_distribution_stats`.
                Hover for the full headline. Leads the header: it is
                the one-glance answer to "was this session OK?". */}
            {data.verdict && (() => {
              const v = data.verdict;
              const palette: Record<string, { bg: string; text: string; ring: string; icon: string }> = {
                healthy:  { bg: isDark ? 'bg-emerald-500/15' : 'bg-emerald-50',
                            text: isDark ? 'text-emerald-300' : 'text-emerald-700',
                            ring: isDark ? 'ring-emerald-500/30' : 'ring-emerald-300',
                            icon: '✓' },
                // Graded (v2) intermediate level: metrics over target but the
                // experience held up. Teal sits between healthy-green and
                // degraded-amber.
                fair:     { bg: isDark ? 'bg-teal-500/15' : 'bg-teal-50',
                            text: isDark ? 'text-teal-300' : 'text-teal-700',
                            ring: isDark ? 'ring-teal-500/30' : 'ring-teal-300',
                            icon: '≈' },
                degraded: { bg: isDark ? 'bg-amber-500/15' : 'bg-amber-50',
                            text: isDark ? 'text-amber-300' : 'text-amber-700',
                            ring: isDark ? 'ring-amber-500/30' : 'ring-amber-300',
                            icon: '⚠' },
                broken:   { bg: isDark ? 'bg-red-500/15' : 'bg-red-50',
                            text: isDark ? 'text-red-300' : 'text-red-700',
                            ring: isDark ? 'ring-red-500/30' : 'ring-red-300',
                            icon: '✕' },
              };
              const p = palette[v.level] ?? palette.degraded;
              const culprit = v.primary_culprit && v.primary_culprit !== 'none'
                ? ` · ${v.primary_culprit}`
                : '';
              return (
                <span
                  className={`inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-[11px] font-medium ring-1 ${p.bg} ${p.text} ${p.ring}`}
                  title={v.headline}
                >
                  <span>{p.icon}</span>
                  <span className="uppercase tracking-wide">{v.level}</span>
                  {culprit && <span className="opacity-70">{culprit}</span>}
                  {/* A grade reached by the operator's own rules says so on
                      the badge itself, not only in the tooltip: the badge is
                      what gets glanced at and screenshotted, and a lenient
                      "healthy" must not look the same as a default one. */}
                  {(v.custom_grading?.length ?? 0) > 0 && (
                    <span className="opacity-70 normal-case">· custom rules</span>
                  )}
                </span>
              );
            })()}

            {data.metadata?.status && <StatusBadge status={data.metadata.status} />}

            <div className="flex items-center gap-x-2 gap-y-1 flex-wrap text-sm min-w-0">
              {data.metadata?.pipeline_name && <span className="text-indigo-400 font-semibold">{data.metadata.pipeline_name}</span>}
              {isRealtimeSession(data.metadata) && (
                <span
                  className={`text-[10px] font-medium px-1.5 py-0.5 rounded ring-1 ${isDark ? 'bg-violet-500/15 text-violet-300 ring-violet-500/30' : 'bg-violet-50 text-violet-700 ring-violet-300'}`}
                  title="Realtime speech-to-speech session (OpenAI Realtime / Gemini Live). STT, LLM and TTS are fused inside the provider; per-turn metrics use a single realtime component instead of the classic stt/llm/tts triple."
                >
                  Realtime S2S
                </span>
              )}
              {data.metadata?.turn_count != null && (
                <>
                  <span className={isDark ? 'text-slate-600' : 'text-gray-300'}>·</span>
                  <span className={`tabular-nums ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>{data.metadata.turn_count} turns</span>
                </>
              )}
              {(data.metadata?.started_at || data.metadata?.start_time) && (() => {
                const when = formatStarted((data.metadata.started_at || data.metadata.start_time)!);
                return (
                  <>
                    <span className={isDark ? 'text-slate-600' : 'text-gray-300'}>·</span>
                    <span className={isDark ? 'text-slate-400' : 'text-gray-500'} title={when.abs}>{when.rel}</span>
                  </>
                );
              })()}
              {data.metadata?.started_at && data.metadata?.ended_at && (
                <span className={`text-xs tabular-nums ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                  ({formatDuration(data.metadata.started_at, data.metadata.ended_at)})
                </span>
              )}
            </div>

            {/* Action row: report copy + view controls, equal 32px heights.
                ml-auto right-aligns the group; as a single flex child it wraps
                below the identity chips as one unit on narrow windows. */}
            <div className="ml-auto flex items-center gap-2 flex-wrap">
              <button
                onClick={() => setShowSummary(true)}
                className={`flex items-center gap-1.5 h-8 px-3 text-sm font-medium rounded-lg border transition-colors whitespace-nowrap ${
                  isDark
                    ? 'bg-slate-800 border-slate-700 text-indigo-300 hover:bg-slate-700 hover:text-white'
                    : 'bg-white border-gray-300 text-indigo-600 hover:bg-gray-100 hover:text-indigo-700'
                }`}
                title="View high-level summary and key performance facts for this session"
              >
                <Activity size={14} className="shrink-0" />
                <span>Quick Summary</span>
              </button>
              <button
                onClick={copyReportForLlm}
                disabled={reportCopyState === 'loading'}
                className={`flex items-center gap-1.5 h-8 px-3 text-sm font-medium rounded-lg border transition-colors disabled:opacity-50 whitespace-nowrap ${
                  reportCopyState === 'copied'
                    ? 'bg-emerald-600 border-emerald-600 text-white'
                    : reportCopyState === 'error'
                      ? 'bg-red-600 border-red-600 text-white'
                      : (isDark ? 'bg-slate-800 border-slate-700 text-slate-300 hover:bg-slate-700 hover:text-white' : 'bg-white border-gray-300 text-gray-600 hover:bg-gray-100 hover:text-gray-900')
                }`}
                title="Copy a compact diagnostic report (markdown + JSON) suitable for pasting into an LLM"
              >
                {reportCopyState === 'copied'
                  ? <><Check size={14} /> Copied</>
                  : reportCopyState === 'error'
                    ? <><ClipboardCopy size={14} /> Failed</>
                    : reportCopyState === 'loading'
                      ? <><ClipboardCopy size={14} /> Copying…</>
                      : <><ClipboardCopy size={14} /> Copy report for LLM</>}
              </button>
              <div className={`flex items-stretch h-8 p-0.5 rounded-lg border ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-300'}`} role="group" aria-label="View">
                <button
                  onClick={() => setViewMode('turns')}
                  className={`px-3 text-sm font-medium rounded-md transition-colors whitespace-nowrap ${viewMode === 'turns' ? (isDark ? 'bg-indigo-500/20 text-indigo-300' : 'bg-indigo-50 text-indigo-600') : (isDark ? 'text-slate-400 hover:text-slate-200' : 'text-gray-500 hover:text-gray-700')}`}
                >
                  Turns
                </button>
                <button
                  onClick={() => setViewMode('timeline')}
                  className={`px-3 text-sm font-medium rounded-md transition-colors whitespace-nowrap ${viewMode === 'timeline' ? (isDark ? 'bg-indigo-500/20 text-indigo-300' : 'bg-indigo-50 text-indigo-600') : (isDark ? 'text-slate-400 hover:text-slate-200' : 'text-gray-500 hover:text-gray-700')}`}
                >
                  Event Timeline
                </button>
              </div>
              <button
                onClick={() => setTelemetryPref(!showTelemetry)}
                className={`flex items-center justify-center h-8 w-8 rounded-lg border transition-colors ${isDark ? 'bg-slate-800 border-slate-700 text-slate-400 hover:text-white hover:bg-slate-700' : 'bg-white border-gray-300 text-gray-500 hover:text-gray-900 hover:bg-gray-100'}`}
                title={showTelemetry ? "Hide Telemetry Panel" : "Show Telemetry Panel"}
              >
                {showTelemetry ? <PanelRightClose size={16} /> : <PanelRightOpen size={16} />}
              </button>
            </div>
          </div>

          {/* Meta line: full UUID (copyable) + resolved provider identity, so
              the reader doesn't have to drill into per-turn telemetry. */}
          <div className={`flex items-center gap-x-4 gap-y-1 flex-wrap mt-1.5 sm:pl-9 font-mono text-[11px] ${isDark ? 'text-slate-500' : 'text-gray-500'}`}>
            <span className="flex items-center gap-1 min-w-0">
              <span className={`truncate ${isDark ? 'text-slate-400' : 'text-gray-600'}`} title={id}>{id}</span>
              <button
                onClick={copyId}
                title={idCopied ? 'Copied' : 'Copy session id'}
                className={`shrink-0 p-1 rounded transition-colors ${idCopied ? (isDark ? 'text-emerald-400' : 'text-emerald-600') : (isDark ? 'text-slate-500 hover:text-slate-300 hover:bg-slate-800' : 'text-gray-400 hover:text-gray-600 hover:bg-gray-100')}`}
              >
                {idCopied ? <Check size={12} /> : <ClipboardCopy size={12} />}
              </button>
            </span>
            {data.providers && (() => {
              const p = data.providers;
              const llm = p.llm
                ? `${p.llm.provider ?? '?'}`
                  + (p.llm.model ? `/${p.llm.model}` : '')
                  + (p.llm.model_version ? ` (v:${p.llm.model_version})` : '')
                : null;
              const stt = p.stt
                ? `${p.stt.provider ?? data.metadata?.stt_provider ?? '?'}`
                  + (p.stt.model ? `/${p.stt.model}` : '')
                  + (p.stt.model_version ? ` (v:${p.stt.model_version})` : '')
                  + (p.stt.mode && p.stt.mode !== 'unknown' ? ` (${p.stt.mode})` : '')
                : null;
              const tts = p.tts
                ? `${p.tts.provider ?? data.metadata?.tts_provider ?? '?'}`
                  + (p.tts.model ? `/${p.tts.model}` : '')
                  + (p.tts.model_version ? ` (v:${p.tts.model_version})` : '')
                  + (p.tts.voice ? ` voice=${p.tts.voice}` : '')
                : null;
              if (!llm && !stt && !tts) return null;
              // Map a device badge's semantic tone to theme classes. GPU is
              // highlighted (emerald), CPU muted, an unconfirmed/requested GPU
              // amber, and an unknown device greyed — so the strip never
              // presents a guess as a confirmed fact.
              const deviceCls = (tone: DeviceTone) => {
                switch (tone) {
                  case 'gpu': return isDark ? 'text-emerald-400/80' : 'text-emerald-600';
                  case 'unconfirmed': return isDark ? 'text-amber-400/70' : 'text-amber-600';
                  case 'unknown': return isDark ? 'text-slate-600' : 'text-gray-400';
                  default: return isDark ? 'text-slate-500' : 'text-gray-400'; // cpu
                }
              };
              const deviceEl = (device?: DeviceInfo) => {
                const badge = formatDeviceBadge(device);
                if (!badge) return null;
                return <span className={deviceCls(badge.tone)} title={badge.title}> · {badge.label}</span>;
              };
              return (
                <>
                  {llm && <span><span className={isDark ? 'text-amber-400/70' : 'text-amber-600'}>llm</span>: {llm}{deviceEl(p.llm?.device)}</span>}
                  {stt && <span><span className={isDark ? 'text-sky-400/70' : 'text-sky-600'}>stt</span>: {stt}{deviceEl(p.stt?.device)}</span>}
                  {tts && <span><span className={isDark ? 'text-violet-400/70' : 'text-violet-600'}>tts</span>: {tts}{deviceEl(p.tts?.device)}</span>}
                </>
              );
            })()}
          </div>
        </div>

        <div className="p-6 xl:p-8 space-y-6">
          
          {data.captures_missing && (
            <div className={`p-4 rounded-lg flex items-start gap-3 border ${isDark ? 'bg-amber-900/20 border-amber-500/30 text-amber-200' : 'bg-amber-50 border-amber-200 text-amber-800'}`}>
              <AlertTriangle className="shrink-0 mt-0.5" size={18} />
              <div className="text-sm">
                <p className="font-semibold mb-1">Local Captures Missing</p>
                <p className="opacity-90">The local captures directory for this session could not be found. Audio playback, event timeline, and per-turn metrics are unavailable. The orchestrator may be running from a different directory than the worker, or you might have forgotten to add the <code>[pipeline.audit]</code> block to your pipeline configuration file.</p>
              </div>
            </div>
          )}
          {(data.unreadable_event_lines ?? 0) > 0 && (
            <div className={`p-4 rounded-lg flex items-start gap-3 border ${isDark ? 'bg-amber-900/20 border-amber-500/30 text-amber-200' : 'bg-amber-50 border-amber-200 text-amber-800'}`}>
              <AlertTriangle className="shrink-0 mt-0.5" size={18} />
              <div className="text-sm">
                <p className="font-semibold mb-1">Event record incomplete</p>
                <p className="opacity-90">
                  {data.unreadable_event_lines} line{data.unreadable_event_lines === 1 ? "" : "s"} of this session&apos;s event files could not be read and {data.unreadable_event_lines === 1 ? "was" : "were"} skipped, so an event or two may be missing from a turn. Everything that could be read is shown.
                </p>
              </div>
            </div>
          )}
          {/* Conversation Player */}
          {hasUtterances && (
            <ConversationPlayer utterances={data.utterances!} sessionId={id!} />
          )}

          {/* Client-side tool activity fallback: when there are no local
              per-utterance captures to interleave with, show the calls as
              one aggregate panel. With utterances present, the cards
              render inline at their chronological slot in the turns view. */}
          {!hasUtterances && (data.client_tool_calls?.length ?? 0) > 0 && (
            <ClientToolsPanel calls={data.client_tool_calls!} />
          )}

          {/* Fallback: individual audio streams when no per-turn files */}
          {!hasUtterances && (
            <div className={`rounded-xl p-6 shadow-xl border ${isDark ? 'bg-slate-800 border-slate-700/50' : 'bg-white border-gray-200'}`}>
               <h3 className="font-bold text-emerald-400 mb-4 flex items-center gap-2"><PlayCircle size={18}/> Audio Capture</h3>
               <div className="flex flex-col gap-4">
                 <div>
                   <span className={`text-xs uppercase tracking-widest mb-2 block font-bold ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>Input (Mic)</span>
                   <audio controls className="w-full h-8" src={`${API}/api/v1/sessions/${sessionPath}/audio/in`} />
                 </div>
                 <div>
                    <span className={`text-xs uppercase tracking-widest mb-2 block font-bold ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>Output (TTS)</span>
                    <audio controls className="w-full h-8" src={`${API}/api/v1/sessions/${sessionPath}/audio/out`} />
                 </div>
               </div>
            </div>
          )}

          {/* Turns view */}
          {viewMode === 'turns' && (
            <div className="space-y-4">
              <h3 className="font-bold text-indigo-400 flex items-center gap-2">
                <Database size={18}/> Turns ({(() => {
                  if (!hasUtterances) return hasOrchTurns ? data.turns!.length : 0;
                  const total = data.utterances!.length;
                  // Only count true server-side terminations as "terminated".
                  // Playback barge-ins (client_interrupted only) are completed turns
                  // with client-side interruption context, not failures.
                  const terminated = (data.utterances ?? []).filter((u: Utterance) => u.summary?.terminated).length;
                  const completed = total - terminated;
                  return terminated > 0 ? `${completed} + ${terminated} terminated` : `${total}`;
                })()})
              </h3>
              {hasUtterances ? (
                <div className="space-y-4">
                  {(() => {
                    // U-2: identify cold-start turn (smallest utterance_id) so
                    // TurnCard can render a muted "Cold start" badge and the
                    // user doesn't compare its latencies against warmed turns.
                    const ids = (data.utterances ?? [])
                      .map((u: Utterance) => u.utterance_id)
                      .filter((x): x is number => typeof x === 'number');
                    const coldId = ids.length ? Math.min(...ids) : null;
                    const sessionIsRealtime = isRealtimeSession(data.metadata);
                    // Client tool calls interleave at their chronological
                    // slot: the BFF stamps each with `after_utterance_id`
                    // (the last turn whose output started before the call);
                    // null/unplaceable calls render before the first turn.
                    const callsByAfter = new Map<number | null, typeof data.client_tool_calls>();
                    for (const c of data.client_tool_calls ?? []) {
                      const key = typeof c.after_utterance_id === 'number' ? c.after_utterance_id : null;
                      const bucket = callsByAfter.get(key) ?? [];
                      bucket.push(c);
                      callsByAfter.set(key, bucket);
                    }
                    const callCards = (key: number | null) =>
                      (callsByAfter.get(key) ?? []).map((c) => (
                        <ClientToolCallCard key={c.call_id} call={c} />
                      ));
                    return (
                      <>
                        {callCards(null)}
                        {data.utterances!.map((utt: Utterance, i: number) => (
                          <Fragment key={utt.utterance_id ?? i}>
                            {(utt.dropped_before ?? []).map((drop, j) => (
                              <DroppedUtteranceCard key={`drop-${j}`} drop={drop} sessionId={id!} utteranceId={utt.utterance_id} />
                            ))}
                            {/* An id that only lost speech never became a turn. */}
                            {!(utt.dropped_before?.length && !utt.stt && !utt.summary && !utt.llm_output && !utt.audio_files) && (
                            <TurnCard
                              utt={utt}
                              sessionId={id!}
                              llmTiming={pickLlmTiming(utt)}
                              ttsTiming={utt.tts_detail ?? utt.tts_timing}
                              sttTiming={utt.stt_timing}
                              orchTurn={hasOrchTurns ? data.turns![i] : undefined}
                              anomalies={anomaliesByUtt[String(utt.utterance_id)] ?? []}
                              isColdStart={coldId != null && utt.utterance_id === coldId}
                              isRealtime={sessionIsRealtime}
                              sessionSttProvider={sessionSttProvider}
                              sessionTtsProvider={sessionTtsProvider}
                            />
                            )}
                            {typeof utt.utterance_id === 'number' && callCards(utt.utterance_id)}
                          </Fragment>
                        ))}
                      </>
                    );
                  })()}
                </div>
              ) : hasOrchTurns ? (
                <SessionDescriptorsContext.Provider value={data.metadata?.metric_descriptors ?? []}>
                  <div className="space-y-4">
                    {[...data.turns!]
                      // The orchestrator's in-memory store pushes new
                      // turns to the front of its ring buffer, so the
                      // API returns them newest-first. Render them in
                      // chronological order (oldest first → #1) so the
                      // displayed turn numbers match the conversation
                      // flow.
                      .sort(
                        (a, b) =>
                          new Date(a.started_at || 0).getTime() -
                          new Date(b.started_at || 0).getTime(),
                      )
                      .map((turn: OrchTurn, i: number) => (
                        <OrchestratorTurnCard key={turn.id || i} turn={turn} index={i} sessionSttProvider={sessionSttProvider} sessionTtsProvider={sessionTtsProvider} />
                      ))}
                  </div>
                </SessionDescriptorsContext.Provider>
              ) : hasEvents ? (
                // Fallback: show events.jsonl data if no per-utterance files
                <div className={`space-y-6 ml-4 pl-4 py-2 border-l-2 ${isDark ? 'border-slate-700' : 'border-gray-300'}`}>
                  {data.events!.map((evt: SessionEvent, i: number) => (
                    <div key={i} className="flex flex-col gap-2 relative">
                      <div className={`absolute w-3 h-3 bg-indigo-500 rounded-full -left-[23px] top-4 border-2 ${isDark ? 'border-[#0f172a]' : 'border-gray-50'}`} />
                      <EventCardRenderer event={evt} />
                    </div>
                  ))}
                </div>
              ) : (
                <div className={`p-4 rounded-lg text-center text-sm border ${isDark ? 'bg-slate-900 border-slate-800 text-slate-500' : 'bg-gray-50 border-gray-200 text-gray-400'}`}>
                   No turn data captured for this session.
                </div>
              )}
            </div>
          )}

          {/* Event timeline view */}
          {viewMode === 'timeline' && (
            <div className={`rounded-xl p-6 border ${isDark ? 'bg-slate-800/80 border-slate-700/50' : 'bg-white border-gray-200'}`}>
              <h3 className="font-bold text-indigo-400 mb-4 flex items-center gap-2"><Database size={18}/> Event Timeline</h3>
              {hasEvents ? (
                <>
                  <div className={`space-y-6 ml-4 pl-4 py-2 border-l-2 ${isDark ? 'border-slate-700' : 'border-gray-300'}`}>
                    {data.events!.map((evt: SessionEvent, i: number) => (
                      <div key={i} className="flex flex-col gap-2 relative">
                        <div className={`absolute w-3 h-3 bg-indigo-500 rounded-full -left-[23px] top-4 border-2 ${isDark ? 'border-[#0f172a]' : 'border-gray-50'}`} />
                        <EventCardRenderer event={evt} />
                      </div>
                    ))}
                  </div>
                  {eventsLoaded < eventsTotal && (
                    <div className="text-center mt-4">
                      <button
                        onClick={loadMoreEvents}
                        className={`px-4 py-2 text-sm rounded-lg border ${isDark ? 'bg-slate-700 border-slate-600 text-slate-300 hover:bg-slate-600' : 'bg-white border-gray-300 text-gray-600 hover:bg-gray-100'}`}
                      >
                        Load more events ({eventsLoaded} / {eventsTotal})
                      </button>
                    </div>
                  )}
                </>
              ) : (
                <div className={`p-4 rounded-lg text-center text-sm border ${isDark ? 'bg-slate-900 border-slate-800 text-slate-500' : 'bg-gray-50 border-gray-200 text-gray-400'}`}>
                   No events found in events.jsonl.
                </div>
              )}
            </div>
          )}
          
        </div>
      </div>

      {/* RIGHT: Telemetry Panel */}
      {showTelemetry && <TelemetryPanel data={data} />}

      {/* Quick Session Summary Modal */}
      {data && (
        <SessionSummaryDialog
          isOpen={showSummary}
          onClose={() => setShowSummary(false)}
          data={data}
          sessionId={id ?? ''}
          anomaliesByUtt={anomaliesByUtt}
          onCopyReportForLlm={copyReportForLlm}
          reportCopyState={reportCopyState}
        />
      )}
    </div>
  );
}
