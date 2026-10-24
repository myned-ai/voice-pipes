import { useTheme } from '../../../contexts/ThemeContext';
import type { SessionEvent, EventTimestamp } from './types';

// ── Timestamp formatting helpers ────────────────────────────

function formatTimestamp(ts: EventTimestamp | undefined): string {
  if (!ts) return '';
  const d = new Date(ts.secs_since_epoch * 1000 + ts.nanos_since_epoch / 1_000_000);
  return d.toISOString().split('T')[1].replace('Z', '');
}

function formatClientMs(ms: number | undefined): string {
  if (ms == null) return '—';
  const d = new Date(ms);
  return d.toISOString().split('T')[1].replace('Z', '');
}

// ── Event card renderers ────────────────────────────────────

export const GenericJsonCard = ({ event }: { event: SessionEvent }) => {
  const { isDark } = useTheme();
  // Pipeline events (pipeline_events.jsonl, serde tag = "kind") carry their
  // data flat on the event rather than under `payload`/`content`. When neither
  // wrapper is present, show the remaining top-level fields (minus the envelope
  // keys) so the card isn't an empty bar.
  const envelopeKeys = new Set(['event_type', 'type', 'kind', 'component', 'timestamp', 'payload', 'content']);
  const flatDetails = Object.fromEntries(
    Object.entries(event as unknown as Record<string, unknown>).filter(([k]) => !envelopeKeys.has(k)),
  );
  const details = event.payload ?? event.content ?? (Object.keys(flatDetails).length > 0 ? flatDetails : null);
  return (
  <div className={`text-sm p-4 rounded border ${isDark ? 'bg-slate-800 border-slate-700 text-slate-300' : 'bg-white border-gray-200 text-gray-700'}`}>
    <div className="flex justify-between items-center mb-2">
       <span className={`text-xs uppercase tracking-widest font-bold ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>{event.event_type || event.type || "Event"}</span>
       <div className="flex items-center gap-3">
         <span className={`text-[10px] font-mono ${isDark ? 'text-slate-600' : 'text-gray-500'}`}>{formatTimestamp(event.timestamp)}</span>
         <span className={`text-xs font-mono tracking-tighter ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>{event.component}</span>
       </div>
    </div>
    {details != null && (
      <pre className={`text-xs break-all whitespace-pre-wrap p-2 rounded ${isDark ? 'bg-slate-900/50' : 'bg-gray-50'}`}>
        {JSON.stringify(details, null, 2)}
      </pre>
    )}
  </div>
);
};

export const LlmPromptCard = ({ event }: { event: SessionEvent }) => {
  const { isDark } = useTheme();
  const messages = (event.payload?.messages as { role: string; content: string }[] | undefined) ?? [];
  return (
    <div className={`text-sm p-4 rounded ${isDark ? 'bg-indigo-950/30 border border-indigo-900/50 text-indigo-200' : 'bg-indigo-50 border border-indigo-200 text-indigo-800'}`}>
      <div className={`flex justify-between mb-3 pb-2 ${isDark ? 'border-b border-indigo-900/30' : 'border-b border-indigo-200'}`}>
         <span className={`text-xs font-bold block ${isDark ? 'text-indigo-400' : 'text-indigo-600'}`}>LLM Context & Prompt</span>
         <div className="flex items-center gap-3">
           <span className={`text-[10px] font-mono ${isDark ? 'text-indigo-800' : 'text-indigo-300'}`}>{formatTimestamp(event.timestamp)}</span>
           <span className={`text-xs font-mono tracking-tighter ${isDark ? 'text-indigo-600/60' : 'text-indigo-400'}`}>{event.component}</span>
         </div>
      </div>
      <div className="space-y-3">
        {messages.map((msg: { role: string; content: string }, i: number) => (
          <div key={i} className="flex flex-col gap-1">
            <span className={`text-[10px] uppercase tracking-widest font-bold ${isDark ? 'text-indigo-500/80' : 'text-indigo-500'}`}>{msg.role}</span>
            <div className={`whitespace-pre-wrap p-2 rounded font-mono text-[11px] leading-relaxed ${isDark ? 'bg-slate-950/40 text-slate-300 border border-indigo-900/30' : 'bg-white text-gray-700 border border-indigo-200'}`}>{msg.content}</div>
          </div>
        ))}
      </div>
    </div>
  );
};

export const LlmResponseCard = ({ event }: { event: SessionEvent }) => {
  const { isDark } = useTheme();
  const text = (event.payload?.text as string | undefined);
  return (
  <div className={`text-sm p-4 rounded ${isDark ? 'bg-emerald-950/30 border border-emerald-900/50 text-emerald-200' : 'bg-emerald-50 border border-emerald-200 text-emerald-800'}`}>
    <div className="flex justify-between mb-2">
       <span className={`text-xs font-bold block ${isDark ? 'text-emerald-400' : 'text-emerald-600'}`}>AI Assistant Response</span>
       <div className="flex items-center gap-3">
         <span className={`text-[10px] font-mono ${isDark ? 'text-emerald-800' : 'text-emerald-300'}`}>{formatTimestamp(event.timestamp)}</span>
         <span className={`text-xs font-mono tracking-tighter ${isDark ? 'text-emerald-600/60' : 'text-emerald-400'}`}>{event.component}</span>
       </div>
    </div>
    <div className={`whitespace-pre-wrap mt-2 p-2 rounded text-sm italic shadow-inner ${isDark ? 'bg-emerald-900/10 text-emerald-100/90 border border-emerald-900/20' : 'bg-white text-emerald-700 border border-emerald-200'}`}>
       "{text || String(event.payload ?? '')}"
    </div>
  </div>
);
};

export const LlmTimingCard = ({ event }: { event: SessionEvent }) => {
  const { isDark } = useTheme();
  type LlmPayload = {
    total_ms?: number; elapsed_ms?: number; generate_ms?: number;
    cache_reset_ms?: number; prompt_ingest_ms?: number; first_token_ms?: number;
    prompt_tokens?: number; output_tokens?: number; tokens?: number; tokens_per_sec?: number;
    device?: string; prefill_cache?: boolean;
    usage?: { prompt_tokens?: number; completion_tokens?: number; output_tokens?: number };
    first_sentence_chars?: number;
  };
  const raw = (event.payload ?? {}) as LlmPayload;
  const usage = raw.usage ?? {};
  // Normalise: Rust worker emits elapsed_ms + usage.{prompt,completion}_tokens
  const promptTok = raw.prompt_tokens ?? usage.prompt_tokens ?? null;
  const outputTok = raw.output_tokens ?? usage.completion_tokens ?? usage.output_tokens ?? raw.tokens ?? null;
  const p = {
    ...raw,
    total_ms: raw.total_ms ?? raw.elapsed_ms ?? null,
    generate_ms: raw.generate_ms ?? raw.elapsed_ms ?? null,
    prompt_tokens: promptTok,
    output_tokens: outputTok,
    tokens_per_sec: raw.tokens_per_sec ?? (outputTok && (raw.generate_ms ?? raw.elapsed_ms) ? Math.round((outputTok / ((raw.generate_ms ?? raw.elapsed_ms)! / 1000)) * 10) / 10 : null),
  };
  const totalMs = p.total_ms ?? 0;
  const cacheMs = p.cache_reset_ms ?? 0;
  const ingestMs = p.prompt_ingest_ms ?? 0;
  const genMs = p.generate_ms ?? 0;
  const hasCache = cacheMs > 0;
  const hasIngest = ingestMs > 0;
  // Proportional bar widths
  const barTotal = cacheMs + ingestMs + genMs || 1;

  return (
    <div className={`text-sm p-4 rounded ${isDark ? 'bg-amber-950/30 border border-amber-900/50 text-amber-200' : 'bg-amber-50 border border-amber-200 text-amber-800'}`}>
      <div className="flex justify-between mb-3">
        <span className={`text-xs font-bold ${isDark ? 'text-amber-400' : 'text-amber-600'}`}>LLM Timing Breakdown</span>
        <div className="flex items-center gap-3">
          <span className={`text-[10px] font-mono ${isDark ? 'text-amber-900' : 'text-amber-300'}`}>{formatTimestamp(event.timestamp)}</span>
          <span className={`text-xs font-mono tracking-tighter ${isDark ? 'text-amber-600/60' : 'text-amber-500'}`}>{p.device || event.component}{p.prefill_cache ? ' (prefill)' : ''}</span>
        </div>
      </div>

      {/* Stacked bar */}
      <div className={`flex h-5 rounded overflow-hidden mb-3 ${isDark ? 'bg-slate-900/50' : 'bg-gray-200'}`}>
        {hasCache && (
          <div className="bg-rose-500/60 flex items-center justify-center text-[9px] text-white font-mono" style={{ width: `${(cacheMs / barTotal) * 100}%` }} title={`Cache reset: ${cacheMs}ms`}>
            {cacheMs > barTotal * 0.08 ? `${Math.round(cacheMs)}ms` : ''}
          </div>
        )}
        {hasIngest && (
          <div className="bg-sky-500/60 flex items-center justify-center text-[9px] text-white font-mono" style={{ width: `${(ingestMs / barTotal) * 100}%` }} title={`Prompt ingest: ${ingestMs}ms`}>
            {ingestMs > barTotal * 0.08 ? `${Math.round(ingestMs)}ms` : ''}
          </div>
        )}
        <div className="bg-emerald-500/60 flex items-center justify-center text-[9px] text-white font-mono" style={{ width: `${(genMs / barTotal) * 100}%` }} title={`Generation: ${genMs}ms`}>
          {genMs > barTotal * 0.08 ? `${Math.round(genMs)}ms` : ''}
        </div>
      </div>

      {/* Legend */}
      <div className="flex flex-wrap gap-x-4 gap-y-1 text-[10px] mb-3">
        {hasCache && <span className="flex items-center gap-1"><span className="w-2 h-2 rounded-sm bg-rose-500/60 inline-block" /> Cache reset</span>}
        {hasIngest && <span className="flex items-center gap-1"><span className="w-2 h-2 rounded-sm bg-sky-500/60 inline-block" /> Prompt ingest</span>}
        <span className="flex items-center gap-1"><span className="w-2 h-2 rounded-sm bg-emerald-500/60 inline-block" /> Generation</span>
      </div>

      {/* Stats grid */}
      <div className="grid grid-cols-2 sm:grid-cols-4 gap-2">
        <div className={`rounded p-2 text-center ${isDark ? 'bg-slate-900/50' : 'bg-amber-100/50'}`}>
          <div className={`font-mono font-bold text-base ${isDark ? 'text-amber-400' : 'text-amber-600'}`}>{Math.round(totalMs)}<span className={`text-[10px] font-normal ${isDark ? 'text-amber-600' : 'text-amber-500'}`}>ms</span></div>
          <div className={`text-[10px] uppercase ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>Total</div>
        </div>
        <div className={`rounded p-2 text-center ${isDark ? 'bg-slate-900/50' : 'bg-sky-50'}`}>
          <div className={`font-mono font-bold text-base ${isDark ? 'text-sky-400' : 'text-sky-600'}`}>{p.prompt_tokens ?? '—'}</div>
          <div className={`text-[10px] uppercase ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>Prompt Tokens</div>
        </div>
        <div className={`rounded p-2 text-center ${isDark ? 'bg-slate-900/50' : 'bg-emerald-50'}`}>
          <div className={`font-mono font-bold text-base ${isDark ? 'text-emerald-400' : 'text-emerald-600'}`}>{p.output_tokens ?? '—'}</div>
          <div className={`text-[10px] uppercase ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>Output Tokens</div>
        </div>
        <div className={`rounded p-2 text-center ${isDark ? 'bg-slate-900/50' : 'bg-amber-50'}`}>
          <div className={`font-mono font-bold text-base ${isDark ? 'text-amber-300' : 'text-amber-600'}`}>{p.tokens_per_sec != null ? p.tokens_per_sec.toFixed(1) : '—'}</div>
          <div className={`text-[10px] uppercase ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>tok/s</div>
        </div>
      </div>
      <div className={`mt-2 text-[10px] ${isDark ? 'text-slate-500' : 'text-gray-400'} flex flex-wrap gap-x-4 gap-y-1`}>
        {p.first_token_ms != null && (
          <span>
            First token (TTFT): <span className={`font-mono ${isDark ? 'text-amber-300' : 'text-amber-600'}`}>{Math.round(p.first_token_ms)}ms</span>
          </span>
        )}
        {p.first_sentence_chars != null && p.first_sentence_chars > 0 && (
          <span>
            · First sentence: <span className={`font-mono ${isDark ? 'text-amber-300' : 'text-amber-600'}`}>{p.first_sentence_chars} chars</span>
          </span>
        )}
      </div>
    </div>
  );
};

export const TtsTimingCard = ({ event }: { event: SessionEvent }) => {
  const { isDark } = useTheme();
  type TtsPayload = { elapsed_ms?: number; chars?: number; audio_duration_s?: number; rtf?: number; sample_rate?: number; voice?: string };
  const p = (event.payload ?? {}) as TtsPayload;
  const elapsedMs = p.elapsed_ms ?? 0;
  const chars = p.chars ?? 0;
  const audioDur = p.audio_duration_s ?? 0;
  const rtf = p.rtf ?? (audioDur > 0 ? ((elapsedMs / 1000) / audioDur) : null);
  const sampleRate = p.sample_rate ?? null;
  const voice = p.voice ?? null;

  return (
    <div className={`text-sm p-4 rounded ${isDark ? 'bg-violet-950/30 border border-violet-900/50 text-violet-200' : 'bg-violet-50 border border-violet-200 text-violet-800'}`}>
      <div className="flex justify-between mb-3">
        <span className={`text-xs font-bold ${isDark ? 'text-violet-400' : 'text-violet-600'}`}>TTS Timing</span>
        <div className="flex items-center gap-3">
          <span className={`text-[10px] font-mono ${isDark ? 'text-violet-900' : 'text-violet-300'}`}>{formatTimestamp(event.timestamp)}</span>
          <span className={`text-xs font-mono tracking-tighter ${isDark ? 'text-violet-600/60' : 'text-violet-500'}`}>{event.component}{voice ? ` (${voice})` : ''}</span>
        </div>
      </div>

      {/* Single bar showing elapsed time */}
      <div className={`flex h-5 rounded overflow-hidden mb-3 ${isDark ? 'bg-slate-900/50' : 'bg-gray-200'}`}>
        <div className="bg-violet-500/60 flex items-center justify-center text-[9px] text-white font-mono w-full" title={`Synthesis: ${Math.round(elapsedMs)}ms`}>
          {Math.round(elapsedMs)}ms
        </div>
      </div>

      {/* Stats grid */}
      <div className="grid grid-cols-2 sm:grid-cols-4 gap-2">
        <div className={`rounded p-2 text-center ${isDark ? 'bg-slate-900/50' : 'bg-violet-100/50'}`}>
          <div className={`font-mono font-bold text-base ${isDark ? 'text-violet-400' : 'text-violet-600'}`}>{Math.round(elapsedMs)}<span className={`text-[10px] font-normal ${isDark ? 'text-violet-600' : 'text-violet-500'}`}>ms</span></div>
          <div className={`text-[10px] uppercase ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>Latency</div>
        </div>
        <div className={`rounded p-2 text-center ${isDark ? 'bg-slate-900/50' : 'bg-violet-50'}`}>
          <div className={`font-mono font-bold text-base ${isDark ? 'text-violet-400' : 'text-violet-600'}`}>{chars}</div>
          <div className={`text-[10px] uppercase ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>Chars</div>
        </div>
        <div className={`rounded p-2 text-center ${isDark ? 'bg-slate-900/50' : 'bg-violet-50'}`}>
          <div className={`font-mono font-bold text-base ${isDark ? 'text-violet-400' : 'text-violet-600'}`}>{audioDur > 0 ? audioDur.toFixed(2) : '—'}<span className={`text-[10px] font-normal ${isDark ? 'text-violet-600' : 'text-violet-500'}`}>s</span></div>
          <div className={`text-[10px] uppercase ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>Audio Duration</div>
        </div>
        <div className={`rounded p-2 text-center ${isDark ? 'bg-slate-900/50' : 'bg-violet-50'}`}>
          <div className={`font-mono font-bold text-base ${isDark ? 'text-violet-300' : 'text-violet-600'}`}>{rtf != null ? rtf.toFixed(2) : '—'}<span className={`text-[10px] font-normal ${isDark ? 'text-violet-600' : 'text-violet-500'}`}>x</span></div>
          <div className={`text-[10px] uppercase ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>RTF</div>
        </div>
      </div>
      {sampleRate && (
        <div className={`mt-2 text-[10px] ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
          Sample rate: <span className={`font-mono ${isDark ? 'text-violet-300' : 'text-violet-600'}`}>{sampleRate} Hz</span>
        </div>
      )}
    </div>
  );
};

export const EventCardRenderer = ({ event }: { event: SessionEvent }) => {
  const type = event.event_type || event.type;
  if (type === "llm_prompt") return <LlmPromptCard event={event} />;
  if (type === "llm_response") return <LlmResponseCard event={event} />;
  if (type === "llm_detail") return <LlmTimingCard event={event} />;
  if (type === "llm_timing") return <GenericJsonCard event={event} />;
  if (type === "tts_timing") return <TtsTimingCard event={event} />;
  if (type === "tts_detail") return <TtsTimingCard event={event} />;
  if (type === "stt_timing") return <GenericJsonCard event={event} />;
  if (type === "client_timing") return <ClientTimingCard event={event} />;
  if (type === "barge_in_rtt") return <BargeInRttCard event={event} />;
  return <GenericJsonCard event={event} />;
};

// ── Status badge ────────────────────────────────────────────

export const ClientTimingCard = ({ event }: { event: SessionEvent }) => {
  const { isDark } = useTheme();
  type ClientPayload = { event?: string; timing_event?: string; client_timestamp_ms?: number; utterance_id?: number };
  // `client_timing` arrives in two shapes:
  //  • events.jsonl (BFF / client capture): fields nested under `payload`.
  //  • pipeline_events.jsonl (worker PipelineEvent, serde tag = "kind"): the
  //    fields are flat on the event itself (`timing_event`,
  //    `client_timestamp_ms`, `utterance_id`) with no `payload` wrapper.
  // Read the nested payload first, then fall back to the flat event so both
  // sources render instead of showing "unknown / — / (empty)".
  const nested = (event.payload ?? {}) as ClientPayload;
  const flat = event as unknown as ClientPayload;
  const p: ClientPayload = {
    event: nested.event ?? flat.event,
    timing_event: nested.timing_event ?? flat.timing_event,
    client_timestamp_ms: nested.client_timestamp_ms ?? flat.client_timestamp_ms,
    utterance_id: nested.utterance_id ?? flat.utterance_id,
  };
  const evtName: string = p.event ?? p.timing_event ?? 'unknown';
  const badgeColor: Record<string, string> = {
    playback_start: 'text-emerald-400 bg-emerald-500/10 border-emerald-500/30',
    playback_end:   'text-sky-400 bg-sky-500/10 border-sky-500/30',
    underrun:       'text-amber-400 bg-amber-500/10 border-amber-500/30',
  };
  const badge = badgeColor[evtName] ?? (isDark ? 'text-slate-400 bg-slate-700/30 border-slate-600' : 'text-gray-600 bg-gray-100 border-gray-300');
  const serverRx = formatTimestamp(event.timestamp);
  return (
    <div className={`text-sm p-3 rounded border flex items-center gap-3 ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <span className={`text-xs font-bold px-2 py-0.5 rounded border ${badge}`}>{evtName}</span>
      <span className={`text-xs ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
        client ts: <span className="font-mono">{formatClientMs(p.client_timestamp_ms)}</span>
      </span>
      {serverRx && (
        <span className={`text-[10px] font-mono ${isDark ? 'text-slate-600' : 'text-gray-400'} ml-1`}>
          server rx: {serverRx}
        </span>
      )}
      {p.utterance_id != null && (
        <span className={`text-xs ml-auto font-mono ${isDark ? 'text-slate-600' : 'text-gray-400'}`}>turn #{p.utterance_id}</span>
      )}
    </div>
  );
};

export const BargeInRttCard = ({ event }: { event: SessionEvent }) => {
  const { isDark } = useTheme();
  type BargeInPayload = { rtt_ms?: number; last_played_offset_ms?: number; utterance_id?: number };
  const p = (event.payload ?? {}) as BargeInPayload;
  return (
    <div className={`text-sm p-3 rounded border flex items-center gap-3 ${isDark ? 'bg-rose-950/30 border-rose-900/40' : 'bg-rose-50 border-rose-200'}`}>
      <span className={`text-xs font-bold ${isDark ? 'text-rose-400' : 'text-rose-600'}`}>Barge-in RTT</span>
      <span className={`font-mono font-bold text-base ${isDark ? 'text-rose-300' : 'text-rose-700'}`}>
        {p.rtt_ms != null ? `${Math.round(p.rtt_ms)} ms` : '—'}
      </span>
      {p.last_played_offset_ms != null && (
        <span className={`text-xs ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
          playback offset: <span className="font-mono">{Math.round(p.last_played_offset_ms)} ms</span>
        </span>
      )}
      <span className={`text-[10px] font-mono ${isDark ? 'text-rose-900' : 'text-rose-300'} ml-1`}>
        {formatTimestamp(event.timestamp)}
      </span>
      {p.utterance_id != null && (
        <span className={`text-xs ml-auto font-mono ${isDark ? 'text-slate-600' : 'text-gray-400'}`}>turn #{p.utterance_id}</span>
      )}
    </div>
  );
};
