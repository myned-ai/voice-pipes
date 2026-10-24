import { useTheme } from '../../../contexts/ThemeContext';
import { firstSentenceStartupMs } from './utils';
// ── Pipeline Timeline (Gantt-style with overlap) ────────────────────

interface PipelineTimelineProps {
  sttMs: number;
  llmMs: number;
  ttsMs: number;
  /** summary.ttft_ms — time from utterance commit to first LLM token */
  ttftMs?: number;
  /** summary.ttfb_ms — time from utterance commit to first audio byte */
  ttfbMs?: number;
  /** summary.total_ms — wall clock from utterance commit to end */
  totalMs?: number;
  /** summary.generation_ms — LLM first token → last token */
  llmGenerationMs?: number;
  /** Component-internal (pre-pacer) TTS TTFB: first LLM fragment → first audio
   *  sample. Distinct from the perceived `ttfbMs`; used to draw the TTS-startup
   *  sub-segment. Optional — when absent the TTS bar renders as a single block. */
  ttsTtfbMs?: number;
  /** VAD silence window after last speech — used to split STT bar */
  endpointingMs?: number;
  /** llmTiming.output_tokens — for token tick marks */
  llmOutputTokens?: number;
  /** llmTiming.cache_reset_ms — KV-cache reset duration */
  llmCacheResetMs?: number;
  /** llmTiming.prompt_ingest_ms — prompt prefill duration */
  llmPromptIngestMs?: number;
  /** llmTiming.first_token_ms — time within generate() to first token */
  llmFirstTokenWithinGenMs?: number;
  /** Item 5: cloud-LLM HTTP connect / first-byte latency (request → response
   *  headers received). When present, the TTFT breakdown surfaces this as
   *  the "network/connect" portion vs the model-side decode portion. */
  llmConnectMs?: number;
  /** Item 5: model-side first-token latency (response headers → first content
   *  byte). For cloud LLMs this equals `ttftMs - llmConnectMs`. */
  llmFirstTokenAfterConnectMs?: number;
  /** True when `ttftMs` was filled from `llm_detail.first_token_ms` because the
   *  live metric was not captured (cold-start turn). Surfaces "(est.)" hint. */
  ttftEstimated?: boolean;
  llmFirstSentenceChars?: number;
  /** For interrupted turns, ms of audio actually heard before the
   *  barge-in (`summary.last_played_offset_ms`). When present the timeline draws
   *  a barge-in marker at first-audio + this offset and ghosts the unheard TTS
   *  tail, so synthesized-but-undelivered audio is visually distinct. */
  playedOffsetMs?: number;
  /** True when this turn was interrupted (server barge-in or playback barge-in). */
  interrupted?: boolean;
}

/** Grid lines rendered inside each Gantt bar track. Hoisted to module scope to satisfy the React Compiler's static-components rule. */
export const GridLines = ({ gridLines, isDark, pct }: { gridLines: number[]; isDark: boolean; pct: (ms: number) => number }) => (
  <>
    {gridLines.map((t) => (
      <div
        key={t}
        className={`absolute top-0 bottom-0 w-px pointer-events-none ${
          t === 0
            ? (isDark ? 'bg-slate-400/30' : 'bg-gray-400/40')
            : (isDark ? 'border-l border-dashed border-slate-700/80' : 'border-l border-dashed border-gray-300/80')
        }`}
        style={{ left: `${pct(t)}%` }}
      />
    ))}
  </>
);

export const PipelineTimeline = ({
  sttMs, llmMs, ttsMs,
  ttftMs, ttfbMs, totalMs,
  llmGenerationMs, ttsTtfbMs,
  endpointingMs,
  llmOutputTokens,
  llmCacheResetMs, llmPromptIngestMs, llmFirstTokenWithinGenMs,
  llmConnectMs, llmFirstTokenAfterConnectMs,
  ttftEstimated,
  llmFirstSentenceChars,
  playedOffsetMs,
  interrupted,
}: PipelineTimelineProps) => {
  const { isDark } = useTheme();

  // Can we show the Gantt (overlap) view?
  // For interrupted turns / older runs, ttft_ms / ttfb_ms may be absent.
  // We try several derivations in order of quality:
  //   1. summary.ttft_ms (preferred — measured wall-clock from utterance commit)
  //   2. ttfb_ms - tts_ttfb_ms (LLM portion of overall response latency)
  //   3. ttfb_ms (conservative upper bound on TTFT)
  //   4. llmTiming.connect_ms + first_token_ms (cloud worker — TLS + decode)
  //   5. llmTiming.first_token_ms (local worker — within-generate first-token)
  //   6. llmMs - llmGenerationMs (anything is better than nothing)
  // Only when all six fail do we fall back to the 3-row sequential layout.
  const derivedFromLlmTiming =
    llmConnectMs != null && llmFirstTokenWithinGenMs != null
      ? llmConnectMs + llmFirstTokenWithinGenMs
      : llmFirstTokenWithinGenMs != null
        ? llmFirstTokenWithinGenMs
        : llmGenerationMs != null && llmMs > llmGenerationMs
          ? llmMs - llmGenerationMs
          : undefined;
  const effectiveTtftMs: number | undefined =
    ttftMs ??
    (ttfbMs != null && ttsTtfbMs != null ? ttfbMs - ttsTtfbMs : undefined) ??
    ttfbMs ??
    derivedFromLlmTiming;

  // totalMs may be 0 for interrupted turns — derive from component durations instead.
  const derivedTotalMs = effectiveTtftMs != null
    ? effectiveTtftMs + Math.max(llmMs, ttsMs > 0 ? ttsMs : 0)
    : null;
  const effectiveTotalMs = (totalMs != null && totalMs > 0) ? totalMs : derivedTotalMs;
  const canGantt = effectiveTtftMs != null && effectiveTotalMs != null && effectiveTotalMs > 0;

  if (!canGantt) {
    // Fallback: 3-row sequential Gantt (no overlap modelling).  We reach
    // this path only when *no* TTFT signal could be derived — e.g. a
    // turn terminated before the LLM produced any tokens.  We still
    // render one row per component on a shared time axis so the user
    // sees the pipeline structure (3 distinct components) rather than
    // a single monolithic stacked bar that hides the per-component
    // boundaries.
    const total = sttMs + llmMs + ttsMs;
    if (total === 0) return null;
    type Row = { label: 'STT' | 'LLM' | 'TTS'; ms: number; bar: string; text: string };
    const rows: Row[] = [
      { label: 'STT', ms: sttMs, bar: 'bg-sky-500/80',    text: isDark ? 'text-sky-400'    : 'text-sky-600' },
      { label: 'LLM', ms: llmMs, bar: 'bg-amber-500/80',  text: isDark ? 'text-amber-400'  : 'text-amber-600' },
      { label: 'TTS', ms: ttsMs, bar: 'bg-violet-500/80', text: isDark ? 'text-violet-400' : 'text-violet-600' },
    ];
    // Sequential layout: STT runs from 0..sttMs, LLM from sttMs..sttMs+llmMs, TTS after that.
    // Bars are placed on a shared axis (0..total) so widths are comparable across rows.
    let cursor = 0;
    const placed = rows.map(r => {
      const start = cursor;
      cursor += r.ms;
      return { ...r, start, end: cursor };
    });
    const LABEL_W = '2rem';
    return (
      <div className={`rounded-lg p-3 border ${isDark ? 'bg-slate-900/40 border-slate-700/50' : 'bg-gray-50 border-gray-200'}`}>
        <div className="flex items-center justify-between mb-2">
          <span className={`text-[10px] font-bold uppercase tracking-widest ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
            Pipeline Timeline{' '}
            <span className={`normal-case font-normal ${isDark ? 'text-slate-600' : 'text-gray-400'}`}>(sequential — no streaming overlap data)</span>
          </span>
          <span className={`text-xs font-mono ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>{Math.round(total)}ms total</span>
        </div>
        <div className="space-y-1">
          {placed.map(r => r.ms > 0 && (
            <div key={r.label} className="flex items-center gap-2">
              <span className={`text-[10px] font-bold text-right flex-shrink-0 ${r.text}`} style={{ width: LABEL_W }}>{r.label}</span>
              <div className={`flex-1 h-5 rounded relative overflow-hidden ${isDark ? 'bg-slate-900/60' : 'bg-gray-200'}`}>
                <div
                  className={`h-full absolute top-0 flex items-center justify-center text-[9px] text-white font-mono ${r.bar}`}
                  style={{ left: `${(r.start / total) * 100}%`, width: `${(r.ms / total) * 100}%` }}
                  title={`${r.label}: ${Math.round(r.ms)}ms (sequential — overlap unknown)`}
                >
                  {(r.ms / total) * 100 > 10 ? `${Math.round(r.ms)}ms` : ''}
                </div>
              </div>
            </div>
          ))}
        </div>
      </div>
    );
  }

  // ── Gantt view: overlapping bars on shared time axis ──

  // t=0 = utterance commit (STT done). STT runs before t=0.
  const sttStart = -sttMs;
  const sttEnd = 0;

  // LLM: starts at t=0, first token at ttftMs, ends when last useful token was sent (≤ effectiveTotalMs).
  // llm_generation_ms includes post-EOS cleanup inside generate() which is irrelevant to
  // streaming performance — cap at effectiveTotalMs so the bar never extends past the last audio byte.
  const llmStart = 0;
  const llmEndRaw = llmGenerationMs != null ? effectiveTtftMs! + llmGenerationMs : llmMs;
  const llmEnd = Math.min(llmEndRaw, effectiveTotalMs!);

  // TTS: starts when first text fragment arrives (~ttftMs), ends when audio streaming ends (effectiveTotalMs).
  const ttsStart = ttsTtfbMs != null && ttfbMs != null ? (ttfbMs - ttsTtfbMs) : effectiveTtftMs!;
  const ttsEnd = effectiveTotalMs!;

  // First-sentence startup time for the ms/ch label. Shared with the
  // TurnCard footer via `firstSentenceStartupMs` so the two never disagree.
  // (Kept separate from `ttsStart`, which also drives the TTS bar geometry.)
  const msPerCharStartupMs = firstSentenceStartupMs({
    ttfbMs,
    ttsInternalTtfbMs: ttsTtfbMs,
    ttftMs,
    llmFirstTokenMs: llmFirstTokenWithinGenMs,
  });

  // Axis bounds — clamp the left edge so the response portion (t=0 → axisMax) always
  // gets at least MIN_RESPONSE_PCT of the total visual width. This prevents a very long
  // STT bar from squashing the LLM/TTS bars into an unreadable sliver.
  const axisMax = Math.max(ttsEnd, llmEnd, effectiveTotalMs!);
  const MIN_RESPONSE_PCT = 0.30; // response region always ≥ 30% of width
  const maxAxisSpan = axisMax / MIN_RESPONSE_PCT;
  const naturalAxisMin = Math.min(sttStart, 0);
  const axisMin = Math.max(naturalAxisMin, axisMax - maxAxisSpan);
  const isSttTruncated = axisMin > naturalAxisMin; // STT bar overflows the left boundary
  const axisSpan = axisMax - axisMin;
  if (axisSpan <= 0) return null;

  const pct = (ms: number) => ((ms - axisMin) / axisSpan) * 100;

  // Barge-in boundary (turn-relative ms) = first audio byte + ms heard.
  // Marks where audio delivery to the user actually stopped; everything to the
  // right of it in the TTS bar was synthesized but never heard.
  const bargeInMs =
    interrupted && playedOffsetMs != null && ttfbMs != null
      ? Math.min(ttfbMs + Math.max(0, playedOffsetMs), effectiveTotalMs!)
      : undefined;

  // ── Grid: pick a round step based on the *visible* span ──
  const gridStep = axisSpan <= 1000 ? 100 : axisSpan <= 3000 ? 250 : axisSpan <= 8000 ? 500 : 1000;
  const gridLines: number[] = [];
  for (let t = Math.ceil(axisMin / gridStep) * gridStep; t <= axisMax; t += gridStep) {
    gridLines.push(t);
  }

  // ── STT bar split: speaking portion (hollow) vs endpointing/finalization (solid) ──
  const sttSpeechMs = endpointingMs != null ? Math.max(0, sttMs - endpointingMs) : sttMs;
  const sttFinMs = endpointingMs ?? 0;

  // ── LLM TTFT sub-segments: orchestration → cache reset → prompt ingest → first token ──
  const hasLlmDetail = llmCacheResetMs != null || llmPromptIngestMs != null || llmFirstTokenWithinGenMs != null;
  const cacheResetMs = llmCacheResetMs ?? 0;
  const promptIngestMs = llmPromptIngestMs ?? 0;
  const firstTokInGenMs = llmFirstTokenWithinGenMs ?? 0;
  const orchMs = hasLlmDetail
    ? Math.max(0, (ttftMs ?? effectiveTtftMs ?? 0) - cacheResetMs - promptIngestMs - firstTokInGenMs)
    : 0;
  const showOrchGap = orchMs > 5;

  const LABEL_W = '2rem';

  return (
    <div className={`rounded-lg p-3 border ${isDark ? 'bg-slate-900/40 border-slate-700/50' : 'bg-gray-50 border-gray-200'}`}>

      {/* ── Header ── */}
      <div className="flex items-center justify-between mb-1">
        <span className={`text-[10px] font-bold uppercase tracking-widest ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
          Pipeline Timeline{' '}
          <span className={`normal-case font-normal ${isDark ? 'text-slate-600' : 'text-gray-400'}`}>(streaming overlap)</span>
        </span>
        <div className={`flex items-center gap-3 text-xs font-mono ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
          {isSttTruncated && (
            <span className={`text-[10px] ${isDark ? 'text-sky-500/60' : 'text-sky-600/60'}`} title="STT bar is cropped — t=0 kept at 70%+ of width so LLM/TTS bars remain readable">
              ◄ STT {Math.round(sttMs)}ms (cropped)
            </span>
          )}
          {/* "Response" (TTFB) is shown by the ← Response → bracket under
              the bars — don't repeat it in the header. */}
          {llmFirstSentenceChars != null && llmFirstSentenceChars > 0 && msPerCharStartupMs != null && msPerCharStartupMs > 0 && (
            <span
              className={isDark ? 'text-amber-400/90' : 'text-amber-600'}
              title={`LLM first sentence: ${llmFirstSentenceChars} characters. Generation startup rate: ${Math.round(msPerCharStartupMs)}ms / ${llmFirstSentenceChars} characters.`}
            >
              1st sent: {llmFirstSentenceChars}ch @ {(msPerCharStartupMs / llmFirstSentenceChars).toFixed(1)}ms/ch
            </span>
          )}
          {/* Item 5: cloud-LLM connect vs decode split.  Shown only when the
              worker emitted connect_ms (currently openai-compat).  Helps
              distinguish "slow to start" (TLS/regional/cold-route) from
              "slow to stream" (model decode). */}
          {llmConnectMs != null && (
            <span
              className={`text-[10px] ${isDark ? 'text-amber-300/80' : 'text-amber-600/80'}`}
              title={`LLM TTFT split — connect/headers: ${Math.round(llmConnectMs)}ms (TLS + cloud route + provider model load); decode: ${llmFirstTokenAfterConnectMs != null ? Math.round(llmFirstTokenAfterConnectMs) : '?'}ms (model first-token).`}
            >
              TTFT {Math.round(llmConnectMs)}ms conn
              {llmFirstTokenAfterConnectMs != null ? ` + ${Math.round(llmFirstTokenAfterConnectMs)}ms decode` : ''}
            </span>
          )}
          {/* Wall-clock lives on the card header clock chip for measured
              turns (single source). Show it here only as the estimated fallback
              when the real total wasn't captured (e.g. interrupted turns), so it
              is never both duplicated and never missing. */}
          {(totalMs == null || totalMs === 0) && (
            <span title="Wall-clock (estimated): total turn time from utterance commit to the last audio byte, derived from component durations because the measured total wasn't captured (e.g. an interrupted turn).">
              {Math.round(effectiveTotalMs!)}ms wall-clock (est.)
            </span>
          )}
        </div>
      </div>

      {/* ── Axis tick labels ── */}
      <div className="flex mb-1" style={{ paddingLeft: LABEL_W }}>
        <div className="relative flex-1 h-3">
          {gridLines.map((t) => (
            <span
              key={t}
              className={`absolute text-[8px] font-mono select-none -translate-x-1/2 ${isDark ? 'text-slate-400' : 'text-gray-400'}`}
              style={{ left: `${pct(t)}%` }}
            >
              {t === 0 ? 't=0' : `${t > 0 ? '+' : ''}${t}ms`}
            </span>
          ))}
        </div>
      </div>

      {/* ── EOU wait bracket (above STT bar) ── */}
      {sttMs > 0 && endpointingMs != null && endpointingMs > 0 && (() => {
        const eouLeft = Math.max(0, pct(sttStart + sttSpeechMs));
        const eouRight = pct(sttEnd);
        const eouW = eouRight - eouLeft;
        const eouMid = eouLeft + eouW / 2;
        return (
          <div className="flex" style={{ paddingLeft: LABEL_W }}>
            <div className="relative flex-1" style={{ height: '0.9rem' }}>
              <div
                className={`absolute top-1.5 h-px ${isDark ? 'bg-sky-400/35' : 'bg-sky-500/35'}`}
                style={{ left: `${eouLeft}%`, width: `${eouW}%` }}
              />
              <div className={`absolute top-0.5 h-2 w-px ${isDark ? 'bg-sky-400/35' : 'bg-sky-500/35'}`} style={{ left: `${eouLeft}%` }} />
              <div className={`absolute top-0.5 h-2 w-px ${isDark ? 'bg-sky-400/35' : 'bg-sky-500/35'}`} style={{ left: `${eouRight}%` }} />
              {eouW > 3 && (
                <span
                  className={`absolute text-[7px] font-mono -translate-x-1/2 top-0 whitespace-nowrap ${isDark ? 'text-sky-500/60' : 'text-sky-600/60'}`}
                  style={{ left: `${eouMid}%` }}
                >
                  silence {Math.round(endpointingMs)}ms
                </span>
              )}
            </div>
          </div>
        );
      })()}

      {/* ── Gantt bars ── */}
      <div className="space-y-1">

        {/* STT bar: hollow = speaking, solid = endpointing/finalization */}
        {sttMs > 0 && (
          <div className="flex items-center gap-2">
            <span className={`text-[10px] font-bold text-right flex-shrink-0 ${isDark ? 'text-sky-400' : 'text-sky-600'}`} style={{ width: LABEL_W }}>STT</span>
            <div className={`flex-1 h-5 rounded relative overflow-hidden ${isDark ? 'bg-slate-900/60' : 'bg-gray-200'}`}>
              <GridLines gridLines={gridLines} isDark={isDark} pct={pct} />
              {/* Truncation indicator: when STT bar extends beyond the visible axis */}
              {isSttTruncated && (
                <div
                  className={`absolute top-0 left-0 h-full w-5 flex items-center justify-start pl-0.5 z-10 pointer-events-none
                    ${isDark ? 'bg-gradient-to-r from-slate-900/90 via-slate-900/60 to-transparent text-sky-400/70' : 'bg-gradient-to-r from-gray-200/90 via-gray-200/60 to-transparent text-sky-600/70'}`}
                  title={`STT bar truncated — full duration: ${Math.round(sttMs)}ms`}
                >
                  <span className="text-[9px] font-mono">◄</span>
                </div>
              )}
              {sttFinMs > 0 ? (
                <>
                  {/* Hollow: speaking portion (clamped to visible axis) */}
                  <div
                    className={`h-full absolute top-0 flex items-center justify-center text-[9px] font-mono border-y border-r ${isDark ? 'bg-sky-900/25 border-sky-600/40 text-sky-300' : 'bg-sky-100/80 border-sky-300 text-sky-700'}`}
                    style={{ left: `${Math.max(0, pct(sttStart))}%`, width: `${Math.max(0, pct(sttStart + sttSpeechMs) - Math.max(0, pct(sttStart)))}%` }}
                    title={`Speaking: ~${Math.round(sttSpeechMs)}ms (user was talking)${isSttTruncated ? ` — bar truncated, full STT: ${Math.round(sttMs)}ms` : ''}`}
                  >
                    {(pct(sttStart + sttSpeechMs) - Math.max(0, pct(sttStart))) > 14 ? `~${Math.round(sttSpeechMs)}ms` : ''}
                  </div>
                  {/* Silence / EOU detection */}
                  <div
                    className={`h-full absolute top-0 flex items-center justify-center text-[9px] font-mono border-y border-r border-dashed ${isDark ? 'border-sky-500/50 text-sky-400/70' : 'border-sky-400/60 text-sky-600/70'}`}
                    style={{
                      left: `${pct(sttStart + sttSpeechMs)}%`,
                      width: `${pct(sttEnd) - pct(sttStart + sttSpeechMs)}%`,
                      backgroundImage: isDark
                        ? 'repeating-linear-gradient(45deg, transparent, transparent 3px, rgba(56,189,248,0.18) 3px, rgba(56,189,248,0.18) 6px)'
                        : 'repeating-linear-gradient(45deg, transparent, transparent 3px, rgba(14,165,233,0.12) 3px, rgba(14,165,233,0.12) 6px)',
                    }}
                    title={`Silence / EOU detection: ${Math.round(sttFinMs)}ms — system waiting for end of speech before dispatching to LLM`}
                  >
                    {(pct(sttEnd) - pct(sttStart + sttSpeechMs)) > 10 ? `~${Math.round(sttFinMs)}ms` : ''}
                  </div>
                </>
              ) : (
                <div
                  className="bg-sky-500/80 h-full absolute top-0 flex items-center justify-center text-[9px] text-white font-mono"
                  style={{ left: `${Math.max(0, pct(sttStart))}%`, width: `${pct(sttEnd) - Math.max(0, pct(sttStart))}%` }}
                  title={`STT: ${Math.round(sttMs)}ms${isSttTruncated ? ' (bar truncated to fit)' : ''}`}
                >
                  {(pct(sttEnd) - Math.max(0, pct(sttStart))) > 12 ? `${Math.round(sttMs)}ms` : ''}
                </div>
              )}
            </div>
          </div>
        )}

        {/* LLM bar: TTFT optionally sub-divided, streaming with token ticks */}
        {(llmEnd - llmStart) > 0 && (
          <div className="flex items-center gap-2">
            <span className={`text-[10px] font-bold text-right flex-shrink-0 ${isDark ? 'text-amber-400' : 'text-amber-600'}`} style={{ width: LABEL_W }}>LLM</span>
            <div className={`flex-1 h-5 rounded relative overflow-hidden ${isDark ? 'bg-slate-900/60' : 'bg-gray-200'}`}>
              <GridLines gridLines={gridLines} isDark={isDark} pct={pct} />
              {effectiveTtftMs != null ? (
                <>
                  {/* TTFT portion: sub-divided when llm_detail fields available */}
                  {effectiveTtftMs > 0 && (() => {
                    if (hasLlmDetail) {
                      // Build ordered segments within [0, ttftMs]
                      type Seg = { ms: number; cls: string; title: string; label: string };
                      const segs: Seg[] = [];
                      if (showOrchGap) segs.push({ ms: orchMs, cls: 'bg-rose-600/80', label: 'orch', title: `Orchestration/queue overhead: ${Math.round(orchMs)}ms — time from utterance commit before LLM received the prompt` });
                      if (cacheResetMs > 0) segs.push({ ms: cacheResetMs, cls: isDark ? 'bg-amber-800' : 'bg-amber-700', label: 'cache', title: `KV-cache reset: ${Math.round(cacheResetMs)}ms — clearing previous context from the KV cache` });
                      if (promptIngestMs > 0) segs.push({ ms: promptIngestMs, cls: isDark ? 'bg-amber-700' : 'bg-amber-600', label: 'ingest', title: `Prompt ingest (prefill): ${Math.round(promptIngestMs)}ms — encoding the prompt tokens through all transformer layers` });
                      const remaining = Math.max(0, (ttftMs ?? effectiveTtftMs!) - orchMs - cacheResetMs - promptIngestMs);
                      if (remaining > 0) segs.push({ ms: remaining, cls: isDark ? 'bg-amber-500' : 'bg-amber-400', label: 'gen₁', title: `Time to first generated token: ${Math.round(remaining)}ms — autoregressive decode of first output token` });

                      let cursor = 0;
                      return segs.map((seg, idx) => {
                        const left = pct(cursor);
                        const w = pct(cursor + seg.ms) - left;
                        cursor += seg.ms;
                        return (
                          <div key={idx}
                            className={`h-full absolute top-0 flex items-center justify-center text-[9px] text-white font-mono ${seg.cls}`}
                            style={{ left: `${left}%`, width: `${Math.max(w, 0.5)}%` }}
                            title={seg.title}
                          >
                            {w > 8 ? seg.label : ''}
                          </div>
                        );
                      });
                    }
                    // Fallback: monolithic dark amber TTFT block
                    return (
                      <div
                        className={`h-full absolute top-0 flex items-center justify-center text-[9px] text-white font-mono ${isDark ? 'bg-amber-600' : 'bg-amber-500'}`}
                        style={{ left: `${pct(0)}%`, width: `${pct(effectiveTtftMs!) - pct(0)}%` }}
                        title={`Thinking / TTFT: ${Math.round(effectiveTtftMs!)}ms${ttftEstimated ? ' (estimated from llm_detail.first_token_ms)' : ''} (prompt encoding + KV-cache)`}
                      >
                        {(pct(effectiveTtftMs!) - pct(0)) > 14 ? `${Math.round(effectiveTtftMs!)}ms${ttftEstimated ? ' (est.)' : ''}` : ''}
                      </div>
                    );
                  })()}

                  {/* Streaming / generation portion with token tick marks */}
                  {(llmEnd - effectiveTtftMs!) > 0 && (() => {
                    const streamLeft = pct(effectiveTtftMs!);
                    const streamW = pct(llmEnd) - streamLeft;
                    const genMs = llmGenerationMs ?? (llmEnd - effectiveTtftMs!);
                    const nTokens = llmOutputTokens ?? 0;
                    // Cap at 80 visual ticks; if > 80 tokens, bin into 80 marks
                    const nTicks = Math.min(nTokens, 80);
                    return (
                      <div
                        className="h-full absolute top-0 bg-amber-500/55 flex items-center justify-center text-[9px] text-white font-mono"
                        style={{ left: `${streamLeft}%`, width: `${streamW}%` }}
                        title={`Streaming generation: ${Math.round(genMs)}ms${nTokens > 0 ? ` · ~${nTokens} tokens` : ''}`}
                      >
                        {/* Token ticks (uniform approximation — tick spacing = throughput signal) */}
                        {nTicks > 0 && Array.from({ length: nTicks }, (_, i) => (
                          <div
                            key={i}
                            className="absolute top-1 bottom-1 w-px bg-white/30 pointer-events-none"
                            style={{ left: `${((i + 0.5) / nTicks) * 100}%` }}
                          />
                        ))}
                        <span className="relative z-10">
                          {streamW > 10 ? `${Math.round(genMs)}ms${nTokens > 0 ? ` ~${nTokens}t` : ''}` : ''}
                        </span>
                      </div>
                    );
                  })()}
                </>
              ) : (
                <div
                  className="bg-amber-500/80 h-full absolute top-0 flex items-center justify-center text-[9px] text-white font-mono"
                  style={{ left: `${pct(llmStart)}%`, width: `${pct(llmEnd) - pct(llmStart)}%` }}
                  title={`LLM: ${Math.round(llmEnd - llmStart)}ms`}
                >
                  {(pct(llmEnd) - pct(llmStart)) > 12 ? `${Math.round(llmEnd - llmStart)}ms` : ''}
                </div>
              )}
            </div>
          </div>
        )}

        {/* TTS bar: dark violet = startup (TTS TTFB), lighter = streaming synthesis */}
        {(ttsEnd - ttsStart) > 0 && (
          <div className="flex items-center gap-2">
            <span className={`text-[10px] font-bold text-right flex-shrink-0 ${isDark ? 'text-violet-400' : 'text-violet-600'}`} style={{ width: LABEL_W }}>TTS</span>
            <div className={`flex-1 h-5 rounded relative overflow-hidden ${isDark ? 'bg-slate-900/60' : 'bg-gray-200'}`}>
              <GridLines gridLines={gridLines} isDark={isDark} pct={pct} />
              {ttsTtfbMs != null ? (
                <>
                  {ttsTtfbMs > 0 && (
                    <div
                      className={`h-full absolute top-0 flex items-center justify-center text-[9px] text-white font-mono ${isDark ? 'bg-violet-600' : 'bg-violet-500'}`}
                      style={{ left: `${pct(ttsStart)}%`, width: `${pct(ttsStart + ttsTtfbMs) - pct(ttsStart)}%` }}
                      title={`TTS startup (first audio): ${Math.round(ttsTtfbMs)}ms`}
                    >
                      {(pct(ttsStart + ttsTtfbMs) - pct(ttsStart)) > 10 ? `${Math.round(ttsTtfbMs)}ms` : ''}
                    </div>
                  )}
                  {(ttsEnd - ttsStart - ttsTtfbMs) > 0 && (
                    <div
                      className="bg-violet-500/50 h-full absolute top-0 flex items-center justify-center text-[9px] text-white font-mono"
                      style={{ left: `${pct(ttsStart + ttsTtfbMs)}%`, width: `${pct(ttsEnd) - pct(ttsStart + ttsTtfbMs)}%` }}
                      title={`TTS streaming: ${Math.round(ttsEnd - ttsStart - ttsTtfbMs)}ms`}
                    >
                      {(pct(ttsEnd) - pct(ttsStart + ttsTtfbMs)) > 10 ? `${Math.round(ttsEnd - ttsStart - ttsTtfbMs)}ms` : ''}
                    </div>
                  )}
                </>
              ) : (
                <div
                  className="bg-violet-500/80 h-full absolute top-0 flex items-center justify-center text-[9px] text-white font-mono"
                  style={{ left: `${pct(ttsStart)}%`, width: `${pct(ttsEnd) - pct(ttsStart)}%` }}
                  title={`TTS: ${Math.round(ttsEnd - ttsStart)}ms`}
                >
                  {(pct(ttsEnd) - pct(ttsStart)) > 12 ? `${Math.round(ttsEnd - ttsStart)}ms` : ''}
                </div>
              )}
              {/* Ghost the synthesized-but-unheard TTS tail (right of the
                  barge-in point) so undelivered audio is visually distinct. */}
              {bargeInMs != null && pct(ttsEnd) - pct(bargeInMs) > 0.5 && (
                <div
                  className="absolute top-0 h-full pointer-events-none"
                  style={{
                    left: `${pct(bargeInMs)}%`,
                    width: `${pct(ttsEnd) - pct(bargeInMs)}%`,
                    backgroundImage: isDark
                      ? 'repeating-linear-gradient(45deg, rgba(15,23,42,0.6) 0, rgba(15,23,42,0.6) 3px, rgba(244,63,94,0.18) 3px, rgba(244,63,94,0.18) 6px)'
                      : 'repeating-linear-gradient(45deg, rgba(255,255,255,0.6) 0, rgba(255,255,255,0.6) 3px, rgba(244,63,94,0.16) 3px, rgba(244,63,94,0.16) 6px)',
                  }}
                  title={`Synthesized but not heard — barge-in after ${Math.round(playedOffsetMs!)}ms of audio played`}
                />
              )}
              {/* Barge-in line: where audio delivery to the user stopped. */}
              {bargeInMs != null && (
                <div
                  className={`absolute top-0 h-full w-px z-10 pointer-events-none ${isDark ? 'bg-rose-400' : 'bg-rose-500'}`}
                  style={{ left: `${pct(bargeInMs)}%` }}
                  title={`Barge-in — audio delivery stopped here (${Math.round(playedOffsetMs!)}ms heard)`}
                />
              )}
            </div>
          </div>
        )}
      </div>

      {/* ── Marker row: TTFT ("1st token") upper lane, TTFB ("1st audio") lower lane ── */}
      <div className="flex mt-0.5" style={{ paddingLeft: LABEL_W }}>
        <div className="relative flex-1" style={{ height: '2.75rem' }}>
          {/* TTFT: shorter tick, label in upper lane. effectiveTtftMs may be derived for interrupted turns. */}
          {effectiveTtftMs != null && (
            <>
              <div
                className={`absolute top-0 w-px h-3 border-l border-dashed ${isDark ? 'border-amber-400' : 'border-amber-500'}`}
                style={{ left: `${pct(effectiveTtftMs)}%` }}
              />
              <span
                className={`absolute text-[8px] font-mono -translate-x-1/2 whitespace-nowrap ${isDark ? 'text-amber-300' : 'text-amber-600'}`}
                style={{ left: `${pct(effectiveTtftMs)}%`, top: '0.8rem' }}
              >
                {ttftMs != null ? '1st token' : '~1st token'}
              </span>
            </>
          )}
          {/* TTFB: longer tick, label in lower lane */}
          {ttfbMs != null && (
            <>
              <div
                className={`absolute top-0 w-px h-5 border-l border-dashed ${isDark ? 'border-emerald-300' : 'border-emerald-500'}`}
                style={{ left: `${pct(ttfbMs)}%` }}
              />
              <span
                className={`absolute text-[8px] font-mono -translate-x-1/2 whitespace-nowrap ${isDark ? 'text-emerald-300' : 'text-emerald-600'}`}
                style={{ left: `${pct(ttfbMs)}%`, top: '1.5rem' }}
              >
                1st audio
              </span>
            </>
          )}
        </div>
      </div>

      {/* ── Latency bracket: t=0 → TTFB ── */}
      {ttfbMs != null && ttfbMs > 0 && (() => {
        const bL = pct(0);
        const bR = pct(ttfbMs);
        const bW = bR - bL;
        const mid = bL + bW / 2;
        return (
          <div className="flex mt-0.5" style={{ paddingLeft: LABEL_W }}>
            <div className="relative flex-1" style={{ height: '1.75rem' }}>
              <div
                className={`absolute top-1.5 h-px ${isDark ? 'bg-emerald-400/70' : 'bg-emerald-500/60'}`}
                style={{ left: `${bL}%`, width: `${bW}%` }}
              />
              <div className={`absolute top-0.5 h-2 w-px ${isDark ? 'bg-emerald-400/70' : 'bg-emerald-500/60'}`} style={{ left: `${bL}%` }} />
              <div className={`absolute top-0.5 h-2 w-px ${isDark ? 'bg-emerald-400/70' : 'bg-emerald-500/60'}`} style={{ left: `${bR}%` }} />
              <span
                className={`absolute text-[8px] font-mono font-semibold -translate-x-1/2 top-3 whitespace-nowrap ${isDark ? 'text-emerald-300' : 'text-emerald-600'}`}
                style={{ left: `${mid}%` }}
              >
                ← Response {Math.round(ttfbMs)}ms →
              </span>
            </div>
          </div>
        );
      })()}

      {/* ── TTS tail bracket: TTFB → wall-clock ── only shown when totalMs is the real measured value */}
      {ttfbMs != null && totalMs != null && totalMs > 0 && (totalMs - ttfbMs) > 10 && (() => {
        const tL = pct(ttfbMs);
        const tR = pct(totalMs);
        const tW = tR - tL;
        const mid = tL + tW / 2;
        return (
          <div className="flex" style={{ paddingLeft: LABEL_W }}>
            <div className="relative flex-1" style={{ height: '1.25rem' }}>
              <div
                className={`absolute top-1.5 h-px ${isDark ? 'bg-violet-300/60' : 'bg-violet-500/45'}`}
                style={{ left: `${tL}%`, width: `${tW}%` }}
              />
              <div className={`absolute top-0.5 h-2 w-px ${isDark ? 'bg-violet-300/60' : 'bg-violet-500/45'}`} style={{ left: `${tL}%` }} />
              <div className={`absolute top-0.5 h-2 w-px ${isDark ? 'bg-violet-300/60' : 'bg-violet-500/45'}`} style={{ left: `${tR}%` }} />
              {tW > 4 && (
                <span
                  className={`absolute text-[8px] font-mono -translate-x-1/2 top-3 whitespace-nowrap ${isDark ? 'text-violet-300' : 'text-violet-600/60'}`}
                  style={{ left: `${mid}%` }}
                  title="TTS synthesis tail: continued audio streaming after first byte was sent"
                >
                  +{Math.round(totalMs - ttfbMs)}ms TTS tail
                </span>
              )}
            </div>
          </div>
        );
      })()}

      {/* ── Legend ── */}
      <div className={`flex flex-wrap gap-x-4 gap-y-1 mt-2 pt-2 border-t ${isDark ? 'border-slate-700/40' : 'border-gray-200'}`}>
        {sttMs > 0 && (
          endpointingMs != null ? (
            <>
              <span className={`flex items-center gap-1 text-[10px] ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                <span className={`w-2 h-2 rounded-sm border inline-block ${isDark ? 'bg-sky-900/25 border-sky-600/40' : 'bg-sky-100/80 border-sky-300'}`} />
                <span className={isDark ? 'text-sky-400' : 'text-sky-600'}>Speaking</span> ~{Math.round(sttSpeechMs)}ms
              </span>
              <span className={`flex items-center gap-1 text-[10px] ${isDark ? 'text-slate-400' : 'text-gray-500'}`} title="Silence (EOU detection): time waiting for end-of-speech before LLM dispatch. In production this is typically 300–700ms.">
                <span
                  className={`w-2 h-2 rounded-sm inline-block border border-dashed ${isDark ? 'border-sky-500/50' : 'border-sky-400/60'}`}
                  style={{ backgroundImage: isDark ? 'repeating-linear-gradient(45deg, transparent, transparent 2px, rgba(56,189,248,0.2) 2px, rgba(56,189,248,0.2) 4px)' : 'repeating-linear-gradient(45deg, transparent, transparent 2px, rgba(14,165,233,0.15) 2px, rgba(14,165,233,0.15) 4px)' }}
                />
                <span className={isDark ? 'text-sky-400' : 'text-sky-600'}>Silence (EOU)</span> {Math.round(endpointingMs)}ms
              </span>
            </>
          ) : (
            <span className={`flex items-center gap-1 text-[10px] ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
              <span className="w-2 h-2 rounded-sm bg-sky-500 inline-block" />
              <span className={isDark ? 'text-sky-400' : 'text-sky-600'}>STT</span> {Math.round(sttMs)}ms
            </span>
          )
        )}
        {showOrchGap && (
          <span className={`flex items-center gap-1 text-[10px] ${isDark ? 'text-rose-400' : 'text-rose-600'}`} title="Gap between STT commit and LLM start — internal queue/dispatch overhead">
            <span className="w-2 h-2 rounded-sm bg-rose-600/80 inline-block" />
            Orch overhead {Math.round(orchMs)}ms
          </span>
        )}
        {hasLlmDetail && cacheResetMs > 0 && (
          <span className={`flex items-center gap-1 text-[10px] ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
            <span className={`w-2 h-2 rounded-sm inline-block ${isDark ? 'bg-amber-800' : 'bg-amber-700'}`} />
            <span className={isDark ? 'text-amber-500' : 'text-amber-700'}>Cache reset</span> {Math.round(cacheResetMs)}ms
          </span>
        )}
        {hasLlmDetail && promptIngestMs > 0 && (
          <span className={`flex items-center gap-1 text-[10px] ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
            <span className={`w-2 h-2 rounded-sm inline-block ${isDark ? 'bg-amber-700' : 'bg-amber-600'}`} />
            <span className={isDark ? 'text-amber-400' : 'text-amber-600'}>Prompt ingest</span> {Math.round(promptIngestMs)}ms
          </span>
        )}
        {ttftMs != null && !hasLlmDetail && (
          <span className={`flex items-center gap-1 text-[10px] ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
            <span className={`w-2 h-2 rounded-sm inline-block ${isDark ? 'bg-amber-600' : 'bg-amber-500'}`} />
            <span className={isDark ? 'text-amber-400' : 'text-amber-600'}>TTFT</span> {Math.round(ttftMs)}ms
          </span>
        )}
        {(llmGenerationMs ?? (llmEnd - (ttftMs ?? 0))) > 0 && ttftMs != null && (
          <span className={`flex items-center gap-1 text-[10px] ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
            <span className="w-2 h-2 rounded-sm bg-amber-500/55 inline-block" />
            <span className={isDark ? 'text-amber-400' : 'text-amber-600'}>LLM gen</span>{' '}
            {Math.round(llmGenerationMs ?? (llmEnd - ttftMs))}ms
            {llmOutputTokens != null && llmOutputTokens > 0 && (
              <span className={`ml-1 ${isDark ? 'text-slate-500' : 'text-gray-400'}`} title="Token ticks shown as uniform approximation; spacing encodes tokens/sec">
                · ~{llmOutputTokens}t{llmOutputTokens <= 80 ? '' : ' (80 shown)'}
              </span>
            )}
          </span>
        )}
        {ttsTtfbMs != null && (
          <span className={`flex items-center gap-1 text-[10px] ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
            <span className={`w-2 h-2 rounded-sm inline-block ${isDark ? 'bg-violet-600' : 'bg-violet-500'}`} />
            <span className={isDark ? 'text-violet-400' : 'text-violet-600'}>TTS startup</span> {Math.round(ttsTtfbMs)}ms
          </span>
        )}
        {ttfbMs != null && (
          <span className={`flex items-center gap-1 text-[10px] ${isDark ? 'text-emerald-400/80' : 'text-emerald-600'}`}>
            <span className="w-2 h-2 rounded-sm bg-emerald-500 inline-block" />
            Response {Math.round(ttfbMs)}ms
          </span>
        )}
      </div>
    </div>
  );
};
