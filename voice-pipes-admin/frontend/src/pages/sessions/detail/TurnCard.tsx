import { useState } from 'react';
import { Mic, Keyboard, MessageSquare, ChevronDown, AlertCircle } from 'lucide-react';
import { useTheme } from '../../../contexts/ThemeContext';
import type { Utterance, LlmTiming, TtsTiming, SttTiming, OrchTurn, ReportAnomaly } from './types';
import { TurnCardHeader } from './TurnCardHeader';
import { AgentTurnText } from './AgentTurnText';
import { PipelineTimeline } from './PipelineTimeline';
import { FragmentPlayer } from './AudioPlayers';
import { fragmentTextsFor } from './fragments';
import { deriveTurnDisplayState, firstSentenceStartupMs } from './utils';
import { API_BASE } from '../../../config/api';

export const TurnCard = ({ utt, sessionId, llmTiming, ttsTiming, sttTiming, orchTurn, anomalies, isColdStart, isRealtime, sessionSttProvider, sessionTtsProvider }: { utt: Utterance; sessionId: string; llmTiming?: LlmTiming; ttsTiming?: TtsTiming; sttTiming?: SttTiming; orchTurn?: OrchTurn; anomalies?: ReportAnomaly[]; isColdStart?: boolean; isRealtime?: boolean; sessionSttProvider?: string; sessionTtsProvider?: string }) => {
  const { isDark } = useTheme();
  const stt = utt.stt;
  const summary = utt.summary;
  const audioFiles = utt.audio_files || {};
  // Single derived UI state with explicit precedence:
  //   terminated > interrupted > playback_barge_in > completed.
  // `isTerminated` here means *failure* only — playback barge-ins are still
  // successful turns (server completed) and must not get the failure styling.
  const displayState = deriveTurnDisplayState(summary);
  const isTerminated = displayState === 'terminated';
  const turnStatus =
    displayState === 'completed'
      ? (orchTurn?.status ?? (summary ? 'completed' : undefined))
      : displayState;

  // Compute timeline segment durations
  const sttMs = sttTiming?.elapsed_ms ?? summary?.processing_ms ?? stt?.latency_ms ?? 0;
  const llmMs = llmTiming?.total_ms ?? llmTiming?.elapsed_ms ?? 0;
  const ttsMs = ttsTiming?.elapsed_ms ?? 0;
  const endpointingMs: number | undefined = sttTiming?.endpointing_ms ?? summary?.endpointing_ms;

  const [promptOpen, setPromptOpen] = useState(false);

  // Determine user-side text: speech transcript or typed text input
  const userText: string | null = utt.stt?.text ?? utt.user_text ?? null;
  const isTextMode = !utt.stt && !!utt.user_text;

  // Classify audio files: user-side vs AI-side by key naming heuristic
  const isUserAudio = (k: string) => /user|mic|input|^in[_.]/.test(k.toLowerCase());
  const userAudioEntries = Object.entries(audioFiles).filter(([k]) => isUserAudio(k));
  const aiAudioEntries = Object.entries(audioFiles).filter(([k]) => !isUserAudio(k));

  // Is the LLM prompt trivially the same as the STT transcript?
  const llmInputIsJustTranscript = !!(utt.llm_input && userText &&
    utt.llm_input.replace(/\W/g, '').toLowerCase() === userText.replace(/\W/g, '').toLowerCase());

  // S2S latency decomposition: provider
  // endpointing + first-text diagnostic, shown in the stats footer.
  const s2sEndpointingMs = isRealtime ? summary?.provider_endpointing_ms : undefined;
  const s2sFirstTextMs = isRealtime ? summary?.first_text_ms : undefined;
  const hasStats = !!(llmTiming || ttsTiming || summary?.stt_provider || summary?.tts_provider || s2sEndpointingMs != null || s2sFirstTextMs != null);

  // For interrupted turns the agent audio below is the FULL synthesized
  // response, but the user only heard up to the barge-in point. Surface that
  // honestly so a mostly/never-heard reply isn't presented as delivered speech.
  const isInterruptedTurn = displayState === 'interrupted' || displayState === 'playback_barge_in';
  const playedOffsetMs = summary?.last_played_offset_ms;
  const synthesizedS = ttsTiming?.audio_duration_s
    ?? (summary?.synthesis_ms != null ? summary.synthesis_ms / 1000 : undefined);
  const heardS = playedOffsetMs != null ? playedOffsetMs / 1000 : undefined;

  // The as-heard reconstruction differs from the server track when the
  // client reported underruns *or* the turn was cut by
  // a barge-in — the backend clamps it to `heard_ms` in that case. Without a
  // client playback report there is nothing to reconstruct from.
  const asHeardAvailable = !!utt.playback_health && ((summary?.audio_underruns ?? 0) > 0 || isInterruptedTurn);
  // Where the client stopped receiving audio, in ideal-audio ms. Mirrors the
  // backend's clamp source (`playback_health.heard_ms`), falling back to the
  // barge-in offset for captures without a playback report.
  const cutMs: number | null = isInterruptedTurn ? (utt.playback_health?.heard_ms ?? playedOffsetMs ?? null) : null;
  const heard = { gaps: summary?.playback_gaps ?? [], cutMs };

  const asHeardHint = asHeardAvailable ? ' Switch to “as heard” for the delivered portion.' : '';
  const heardNote: string | null = !isInterruptedTurn
    ? null
    : heardS != null && heardS < 0.15
      ? 'Barge-in before playback — this response was not heard. Audio below is the full synthesized output (never delivered).'
      : heardS != null
        ? `Interrupted — ~${heardS.toFixed(1)}s heard before barge-in${synthesizedS != null ? ` of ${synthesizedS.toFixed(1)}s synthesized` : ''}. Audio below is the full synthesized output.${asHeardHint}`
        : `Interrupted — audio below is the full synthesized output; only part was heard before the barge-in.${asHeardHint}`;

  return (
    <div className={`rounded-xl p-5 space-y-4 border ${isTerminated ? (isDark ? 'bg-slate-800/60 border-orange-500/30' : 'bg-orange-50/30 border-orange-200') : (isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200')}`}>
      <TurnCardHeader
        utt={utt}
        displayState={displayState}
        turnStatus={turnStatus}
        summary={summary}
        orchTurn={orchTurn}
        anomalies={anomalies}
        isColdStart={isColdStart}
        isRealtime={isRealtime}
      />

      {/* Pipeline Timeline */}
      {(sttMs > 0 || llmMs > 0 || ttsMs > 0) && (
        <PipelineTimeline
          sttMs={sttMs} llmMs={llmMs} ttsMs={ttsMs}
          ttftMs={summary?.ttft_ms}
          ttftEstimated={summary?.ttft_estimated}
          ttfbMs={summary?.ttfb_ms}
          totalMs={summary?.total_ms ?? orchTurn?.total_ms}
          llmGenerationMs={summary?.generation_ms ?? llmTiming?.generate_ms}
          ttsTtfbMs={summary?.tts_internal_ttfb_ms}
          endpointingMs={endpointingMs}
          llmOutputTokens={llmTiming?.output_tokens}
          llmCacheResetMs={llmTiming?.cache_reset_ms}
          llmPromptIngestMs={llmTiming?.prompt_ingest_ms}
          llmFirstTokenWithinGenMs={llmTiming?.first_token_ms}
          llmConnectMs={llmTiming?.connect_ms}
          llmFirstTokenAfterConnectMs={
            llmTiming?.first_token_ms != null && llmTiming?.connect_ms != null
              ? Math.max(0, llmTiming.first_token_ms - llmTiming.connect_ms)
              : undefined
          }
          llmFirstSentenceChars={llmTiming?.first_sentence_chars}
          playedOffsetMs={playedOffsetMs}
          interrupted={isInterruptedTurn}
        />
      )}

      {/* ── User speech row: transcript + per-turn mic audio ── */}
      {userText && (
        <div className="flex flex-col gap-1.5">
          <div className="flex items-start gap-3">
            {isTextMode
              ? <Keyboard size={14} className={`mt-0.5 shrink-0 ${isDark ? 'text-sky-300' : 'text-sky-600'}`} />
              : <Mic size={14} className={`mt-0.5 shrink-0 ${isDark ? 'text-sky-300' : 'text-sky-600'}`} />
            }
            <span className={`text-sm leading-relaxed ${isDark ? 'text-sky-300' : 'text-sky-800'}`}>
              {userText}
            </span>
          </div>
          {userAudioEntries.length > 0 && (
            <div className="pl-7">
              <FragmentPlayer entries={userAudioEntries} sessionId={sessionId} />
            </div>
          )}
        </div>
      )}

      {/* ── Agent response row ── */}
      {utt.llm_output?.text && (
        <div className="flex flex-col gap-1.5">
          <div className="flex items-start gap-3">
            <MessageSquare size={14} className={`mt-0.5 shrink-0 ${isDark ? 'text-emerald-300' : 'text-emerald-600'}`} />
            <AgentTurnText
              fullText={utt.llm_output.text}
              retainedText={utt.retained_text}
              summary={summary}
            />
          </div>
          {aiAudioEntries.length > 0 && (
            <div className="pl-7 flex flex-col gap-1">
              {heardNote && (
                <div className={`flex items-start gap-1.5 text-[10px] leading-snug ${isDark ? 'text-amber-400/80' : 'text-amber-600'}`}>
                  <AlertCircle size={11} className="mt-px shrink-0" />
                  <span>{heardNote}</span>
                </div>
              )}
              <FragmentPlayer
                entries={aiAudioEntries}
                sessionId={sessionId}
                fragmentTexts={fragmentTextsFor(utt, aiAudioEntries)}
                fragments={utt.fragments}
                heard={heard}
                concatUrl={`${API_BASE}/api/v1/sessions/${encodeURIComponent(sessionId)}/audio/turn/${utt.utterance_id}/tts_concat.wav`}
                asHeardUrl={
                  asHeardAvailable
                    ? `${API_BASE}/api/v1/sessions/${encodeURIComponent(sessionId)}/audio/turn/${utt.utterance_id}/as-heard.wav`
                    : undefined
                }
              />
            </div>
          )}
        </div>
      )}

      {/* ── Collapsible LLM prompt ── */}
      {utt.llm_input && (
        <div className="mt-1">
          <button
            onClick={() => setPromptOpen(o => !o)}
            className={`flex items-center gap-1.5 text-[10px] font-mono transition-colors ${isDark ? 'text-indigo-500/60 hover:text-indigo-400' : 'text-indigo-600/60 hover:text-indigo-500'}`}
          >
            <ChevronDown size={11} className={`transition-transform duration-150 ${promptOpen ? '' : '-rotate-90'}`} />
            LLM prompt · {utt.llm_input.length} chars
            {llmInputIsJustTranscript && (
              <span className={`ml-1 ${isDark ? 'text-slate-400' : 'text-gray-400'}`}>· same as transcript</span>
            )}
          </button>
          {promptOpen && (
            <div className={`mt-1.5 rounded p-3 text-xs font-mono whitespace-pre-wrap max-h-52 overflow-y-auto leading-relaxed ${isDark ? 'bg-indigo-950/25 border border-indigo-900/30 text-indigo-200/80' : 'bg-indigo-50 border border-indigo-100 text-indigo-900'}`}>
              {utt.llm_input}
            </div>
          )}
        </div>
      )}

      {/* ── Stats footer: perf numbers + providers in one compact row ── */}
      {hasStats && (
        <div className={`flex flex-wrap gap-x-5 gap-y-1 text-[10px] pt-3 border-t ${isDark ? 'text-slate-500 border-slate-700/50' : 'text-gray-400 border-gray-200'}`}>
          {llmTiming?.tokens_per_sec != null && (
            <span className={`font-mono font-semibold ${isDark ? 'text-amber-400/80' : 'text-amber-600'}`}>{llmTiming.tokens_per_sec.toFixed(1)} tok/s</span>
          )}
          {llmTiming?.prompt_tokens != null && (
            <span>{llmTiming.prompt_tokens}↑ {llmTiming.output_tokens ?? 0}↓ tok</span>
          )}
          {llmTiming?.first_sentence_chars != null && (
            <span>1st sent: {llmTiming.first_sentence_chars} ch</span>
          )}
          {llmTiming?.first_sentence_chars != null && llmTiming.first_sentence_chars > 0 && (() => {
            // Shared numerator with the pipeline-timeline header so the
            // two ms/ch readings never disagree for the same turn.
            const ttsStart = firstSentenceStartupMs({
              ttfbMs: summary?.ttfb_ms,
              ttsInternalTtfbMs: summary?.tts_internal_ttfb_ms,
              ttftMs: summary?.ttft_ms,
              llmFirstTokenMs: llmTiming?.first_token_ms,
            });
            if (ttsStart != null && ttsStart > 0) {
              const rate = ttsStart / llmTiming.first_sentence_chars;
              return (
                <span
                  className={`font-mono font-semibold ${isDark ? 'text-amber-400/80' : 'text-amber-600'}`}
                  title={`LLM first sentence startup rate: time to emit first sentence (${Math.round(ttsStart)}ms) divided by its characters (${llmTiming.first_sentence_chars} ch).`}
                >
                  {rate.toFixed(1)} ms/ch
                </span>
              );
            }
            return null;
          })()}
          {ttsTiming?.rtf != null && (
            <span className={`font-mono font-semibold ${isDark ? 'text-violet-400/80' : 'text-violet-600'}`}>{ttsTiming.rtf.toFixed(2)}x RTF</span>
          )}
          {summary?.synthesis_ms != null && (
            <span title="Total TTS synthesis: first text fragment → last audio chunk (pre-pacer)">
              TTS {Math.round(summary.synthesis_ms)}ms
            </span>
          )}
          {s2sEndpointingMs != null && (
            <span
              className={`font-mono font-semibold ${s2sEndpointingMs > 500 ? (isDark ? 'text-rose-400/80' : 'text-rose-600') : (isDark ? 'text-violet-400/80' : 'text-violet-600')}`}
              title="Provider endpointing (S2S): provider end-of-speech anchor − the worker's own VAD stamp. The provider's VAD / transcription lag; budget 500 ms."
            >
              Provider endpointing {Math.round(s2sEndpointingMs)}ms
            </span>
          )}
          {s2sFirstTextMs != null && (
            <span title="Time to first transcript delta from the provider (diagnostic — the audio may follow much later; TTFB is the user-facing number).">
              first text {Math.round(s2sFirstTextMs)}ms
            </span>
          )}
          {ttsTiming?.chars != null && <span>{ttsTiming.chars} chars</span>}
          {ttsTiming?.audio_duration_s != null && <span>{ttsTiming.audio_duration_s.toFixed(2)}s audio</span>}
          {ttsTiming?.voice && <span>voice: {ttsTiming.voice}</span>}
          {/* Only surface the per-turn provider when it differs from the
              session-level provider (shown in the header strip) — otherwise it's
              the same label repeated on every turn. */}
          {summary?.stt_provider && summary.stt_provider !== sessionSttProvider && (
            <span>
              STT: {summary.stt_provider}
              {summary.stt_mode === 'streaming' && (
                <span
                  className={`ml-1 ${isDark ? 'text-sky-400/70' : 'text-sky-600'}`}
                  title="Streaming STT — transcript arrives incrementally during user speech; processing_ms not directly comparable to batch providers"
                >
                  (streaming)
                </span>
              )}
            </span>
          )}
          {summary?.tts_provider && summary.tts_provider !== sessionTtsProvider && <span>TTS: {summary.tts_provider}</span>}
          {summary?.audio_underruns != null && summary.audio_underruns > 0 && (
            <span className={isDark ? 'text-amber-400' : 'text-amber-600'}>
              {summary.audio_underruns} underrun{summary.audio_underruns !== 1 ? 's' : ''}
            </span>
          )}
        </div>
      )}
    </div>
  );
};
