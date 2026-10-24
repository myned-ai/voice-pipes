import { useState, useRef, useCallback, useEffect, useMemo } from 'react';
import { PlayCircle, Pause, Square, SkipForward, Mic, Volume2, ChevronLeft, ChevronRight, ChevronDown } from 'lucide-react';
import { useTheme } from '../../../contexts/ThemeContext';
import type { FragmentInfo, Utterance } from './types';
import { API_BASE } from '../../../config/api';
import { fragmentSnippet } from './fragments';
import { FragmentTimeline } from './FragmentTimeline';
import type { HeardModel } from './heardTimeline';
import { buildPlaylist, type PlaylistEntry } from './conversationPlaylist';

const API = API_BASE;
// ── Conversation player ─────────────────────────────────────

// Playlist construction lives in `conversationPlaylist.ts` (trimmed user
// speech, skipped silent turns, real response-latency lead-ins).

export const ConversationPlayer = ({ utterances, sessionId }: { utterances: Utterance[]; sessionId: string }) => {
  const { isDark } = useTheme();
  const audioRef = useRef<HTMLAudioElement>(null);
  const [playlist] = useState(() => buildPlaylist(utterances, sessionId, API));
  const [currentIdx, setCurrentIdx] = useState(-1);
  const [playing, setPlaying] = useState(false);
  // Set while holding the lead-in silence before `currentIdx` starts.
  const [waiting, setWaiting] = useState<PlaylistEntry | null>(null);
  const leadInTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearLeadIn = useCallback(() => {
    if (leadInTimer.current !== null) {
      clearTimeout(leadInTimer.current);
      leadInTimer.current = null;
    }
    setWaiting(null);
  }, []);
  useEffect(() => clearLeadIn, [clearLeadIn]);

  const startEntry = useCallback((idx: number) => {
    const el = audioRef.current;
    if (!el) return;
    el.src = playlist[idx].url;
    el.play().catch(() => {});
    setPlaying(true);
  }, [playlist]);

  /** Move to `idx`: hold its lead-in (the real response latency before an
   *  agent reply, a short gap otherwise), then play. */
  const playIndex = useCallback((idx: number, opts: { skipLeadIn?: boolean } = {}) => {
    clearLeadIn();
    const el = audioRef.current;
    if (!el || idx < 0 || idx >= playlist.length) {
      setPlaying(false);
      setCurrentIdx(-1);
      return;
    }
    setCurrentIdx(idx);
    const entry = playlist[idx];
    if (entry.leadInMs > 0 && !opts.skipLeadIn) {
      el.pause();
      setPlaying(true);
      setWaiting(entry);
      leadInTimer.current = setTimeout(() => {
        leadInTimer.current = null;
        setWaiting(null);
        startEntry(idx);
      }, entry.leadInMs);
      return;
    }
    startEntry(idx);
  }, [playlist, clearLeadIn, startEntry]);

  const handlePlay = () => {
    if (playlist.length === 0) return;
    const el = audioRef.current;
    // Resume a paused clip in place; anything else starts the entry (a
    // lead-in interrupted by pause is not re-held — the wait was already felt).
    if (currentIdx >= 0 && el && el.src === playlist[currentIdx].url && el.currentTime > 0 && el.paused && !el.ended) {
      el.play().catch(() => {});
      setPlaying(true);
      return;
    }
    playIndex(currentIdx < 0 ? 0 : currentIdx, { skipLeadIn: currentIdx >= 0 });
  };
  const handlePause = () => { clearLeadIn(); audioRef.current?.pause(); setPlaying(false); };
  const handleStop = () => { clearLeadIn(); audioRef.current?.pause(); setPlaying(false); setCurrentIdx(-1); };
  const handleSkip = () => { if (currentIdx >= 0) playIndex(currentIdx + 1, { skipLeadIn: true }); };
  const handleEnded = () => { playIndex(currentIdx + 1); };

  const current = currentIdx >= 0 ? playlist[currentIdx] : null;

  if (playlist.length === 0) return null;

  return (
    <div className={`rounded-xl p-5 shadow-xl border ${isDark ? 'bg-gradient-to-r from-slate-800 to-slate-800/80 border-slate-700/50' : 'bg-white border-gray-200'}`}>
      <h3 className="font-bold text-emerald-400 mb-4 flex items-center gap-2">
        <PlayCircle size={18}/> Conversation Playback
      </h3>
      <audio ref={audioRef} onEnded={handleEnded} className="hidden" />

      <div className="flex items-center gap-3 mb-4">
        {playing ? (
          <button onClick={handlePause} className="p-2.5 rounded-full bg-amber-500/20 text-amber-400 hover:bg-amber-500/30 transition-colors" title="Pause">
            <Pause size={20} />
          </button>
        ) : (
          <button onClick={handlePlay} className="p-2.5 rounded-full bg-emerald-500/20 text-emerald-400 hover:bg-emerald-500/30 transition-colors" title="Play conversation">
            <PlayCircle size={20} />
          </button>
        )}
        <button onClick={handleStop} className={`p-2 rounded-full transition-colors ${isDark ? 'bg-slate-700/50 text-slate-400 hover:bg-slate-700' : 'bg-gray-200 text-gray-500 hover:bg-gray-300'}`} title="Stop">
          <Square size={16} />
        </button>
        <button onClick={handleSkip} className={`p-2 rounded-full transition-colors ${isDark ? 'bg-slate-700/50 text-slate-400 hover:bg-slate-700' : 'bg-gray-200 text-gray-500 hover:bg-gray-300'}`} title="Skip to next">
          <SkipForward size={16} />
        </button>

        <div className="flex-1 ml-2">
          {current ? (
            <div className="flex items-center gap-2">
              <span className={`text-xs font-bold uppercase tracking-widest ${current.side === 'user' ? 'text-sky-400' : 'text-emerald-400'}`}>
                {current.side === 'user' ? <Mic size={12} className="inline mr-1" /> : <Volume2 size={12} className="inline mr-1" />}
                {current.label}
              </span>
              <span className={`text-xs ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>{currentIdx + 1} / {playlist.length}</span>
              {waiting && waiting.leadInReason === 'response latency' && (
                <span
                  className={`text-xs font-mono ${isDark ? 'text-amber-400/80' : 'text-amber-600'}`}
                  title="Held for as long as the user actually waited for this reply (e2e_perceived_ms)"
                >
                  ⏱ {(waiting.leadInMs / 1000).toFixed(1)} s response latency…
                </span>
              )}
            </div>
          ) : (
            <span className={`text-xs ${isDark ? 'text-slate-300' : 'text-gray-400'}`}>Press play to hear the full conversation turn-by-turn</span>
          )}
        </div>
      </div>

      {/* Mini timeline showing all segments */}
      <div className="flex gap-0.5 h-3 rounded overflow-hidden">
        {playlist.map((entry, i) => (
          <button
            key={i}
            onClick={() => playIndex(i)}
            className={`flex-1 transition-all cursor-pointer ${
              i === currentIdx
                ? (entry.side === 'user' ? 'bg-sky-400' : 'bg-emerald-400')
                : (entry.side === 'user' ? 'bg-sky-900/60 hover:bg-sky-800/80' : 'bg-emerald-900/60 hover:bg-emerald-800/80')
            }`}
            title={entry.label}
          />
        ))}
      </div>
      <div className={`flex justify-between mt-1 text-[10px] ${isDark ? 'text-slate-600' : 'text-gray-400'}`}>
        <span>User = blue</span>
        <span>AI = green</span>
      </div>
    </div>
  );
};

// ── Per-turn card ──────────────────────────────────────────────────────────────

// ── Compact fragment audio player ──────────────────────────
// One shared <audio> plus a way to pick which fragment it plays.
// Single-fragment case: just the audio element, no chrome.
//
// When `concatUrl` is provided AND there are multiple fragments, the
// player defaults to a server-stitched "all" track that plays the
// whole turn gaplessly — realtime providers (OpenAI Realtime / Gemini
// Live) emit one tiny `tts_frag_*.wav` per `response.audio.delta`,
// and chaining them via successive `<audio>.src` swaps produces
// audible clicks.
//
// The per-fragment picker adapts to how many fragments there are,
// because "fragment" means two very different things:
//
//   • Classic TTS: one fragment per *sentence*, with its text alongside.
//     A handful of chips, each a meaningful unit — shown inline, labelled
//     with the sentence's first words rather than a bare number.
//   • Realtime S2S: one fragment per audio delta, ~40 ms each. Hundreds
//     per turn, none individually meaningful. Rendering a chip per
//     fragment produced a 373-button wall inside the turn card. Above
//     `CHIP_THRESHOLD` the chips collapse to a one-line summary and an
//     opt-in stepper (prev / #n / next) for anyone who really needs to
//     step through deltas.

/** Above this many fragments, chips become a summary + stepper. Chips
 *  are a selection control and stop being scannable well before this;
 *  24 is roughly two rows at the card's width. */
export const CHIP_THRESHOLD = 24;

const chipClass = (isDark: boolean, active: boolean, tone: 'emerald' | 'amber' | 'violet') => {
  const base = 'text-[10px] font-mono px-2 py-0.5 rounded transition-colors';
  if (!active) {
    return `${base} ${isDark ? 'bg-slate-700/60 text-slate-400 hover:bg-slate-700 hover:text-slate-200' : 'bg-gray-100 text-gray-500 hover:bg-gray-200 hover:text-gray-700'}`;
  }
  const tones = {
    emerald: isDark ? 'bg-emerald-600/40 text-emerald-200 ring-1 ring-emerald-500/60' : 'bg-emerald-100 text-emerald-700 ring-1 ring-emerald-400/40',
    amber: isDark ? 'bg-amber-600/40 text-amber-200 ring-1 ring-amber-500/60' : 'bg-amber-100 text-amber-700 ring-1 ring-amber-400/40',
    violet: isDark ? 'bg-violet-600/50 text-violet-200 ring-1 ring-violet-500/60' : 'bg-violet-100 text-violet-700 ring-1 ring-violet-400/40',
  };
  return `${base} ${tones[tone]}`;
};

export const FragmentPlayer = ({ entries, sessionId, concatUrl, asHeardUrl, fragmentTexts, fragments, heard }: {
  entries: [string, string][];
  sessionId: string;
  concatUrl?: string;
  asHeardUrl?: string;
  /** Sentence text per fragment kind, when the capture has it. Labels the
   *  chips; absent for realtime deltas. See `fragmentTextsFor`. */
  fragmentTexts?: Record<string, string>;
  /** Per-fragment timing (`utt.fragments`). Enables the timeline bar. */
  fragments?: FragmentInfo[];
  /** Client-reported underruns and barge-in cut, drawn on the timeline. */
  heard?: HeardModel;
}) => {
  const { isDark } = useTheme();
  // activeIdx === -1 → play the joined "all" track (only valid when
  // concatUrl is set); -2 → the "as heard" reconstruction (client-reported
  // underrun silences spliced in). Otherwise points at an entries[] index.
  const hasConcat = !!concatUrl && entries.length > 1;
  const hasAsHeard = !!asHeardUrl && hasConcat;
  const [activeIdx, setActiveIdx] = useState(hasConcat ? -1 : 0);
  const audioRef = useRef<HTMLAudioElement>(null);
  const shouldAutoPlay = useRef(false);

  const dense = entries.length > CHIP_THRESHOLD;
  // Dense mode only: the stepper is opt-in, and the stitched track's
  // duration (once its metadata loads) gives the "~40 ms avg" figure the
  // summary line shows — the API exposes fragment filenames, not durations.
  const [inspecting, setInspecting] = useState(false);
  const [concatDurationS, setConcatDurationS] = useState<number | null>(null);
  const [stepperInput, setStepperInput] = useState('');

  // The timeline needs one timing record per entry, in entry order. Bail
  // (no bar) when the capture predates `utt.fragments` or the sets disagree.
  const timelineFragments = useMemo(() => {
    if (!fragments || fragments.length === 0) return null;
    const byKind = new Map(fragments.map(f => [f.kind, f]));
    const ordered = entries.map(([kind]) => byKind.get(kind));
    return ordered.every((f): f is FragmentInfo => !!f) ? ordered : null;
  }, [fragments, entries]);
  const heardModel = useMemo<HeardModel>(() => heard ?? { gaps: [], cutMs: null }, [heard]);

  const url = (filename: string) => `${API}/api/v1/sessions/${encodeURIComponent(sessionId)}/audio/${filename}`;
  const currentSrc = activeIdx === -2 && asHeardUrl
    ? asHeardUrl
    : activeIdx === -1 && concatUrl
      ? concatUrl
      : url(entries[Math.max(0, activeIdx)][1]);

  // When active index changes, reload; if flagged, also play
  useEffect(() => {
    const el = audioRef.current;
    if (!el) return;
    if (shouldAutoPlay.current) {
      shouldAutoPlay.current = false;
      el.load();
      el.play().catch(() => {/* autoplay blocked */});
    } else {
      el.load();
    }
  }, [activeIdx]);

  const selectFragment = (i: number) => {
    shouldAutoPlay.current = true;
    setActiveIdx(i);
  };

  // Stepper navigation wraps, and stepping "next" from a whole-turn track
  // lands on the first fragment rather than the second.
  const stepFragment = (delta: 1 | -1) => {
    const n = entries.length;
    const from = activeIdx < 0 ? (delta > 0 ? -1 : 0) : activeIdx;
    selectFragment(((from + delta) % n + n) % n);
  };

  const commitStepperInput = () => {
    const parsed = Number.parseInt(stepperInput, 10);
    setStepperInput('');
    if (!Number.isFinite(parsed)) return;
    selectFragment(Math.min(entries.length, Math.max(1, parsed)) - 1);
  };

  // Auto-advance only matters in per-fragment mode. The joined track
  // is a single continuous WAV — `onEnded` fires once at the very end.
  const handleEnded = () => {
    if (activeIdx >= 0 && activeIdx < entries.length - 1) {
      shouldAutoPlay.current = true;
      setActiveIdx(i => i + 1);
    }
  };

  const handleLoadedMetadata = () => {
    const el = audioRef.current;
    if (!el || activeIdx !== -1 || !Number.isFinite(el.duration)) return;
    setConcatDurationS(el.duration);
  };

  if (entries.length === 0) return null;

  // Single fragment — plain audio player, no chrome
  if (entries.length === 1) {
    return (
      <audio
        controls
        className="w-full h-7 mt-1.5"
        src={url(entries[0][1])}
      />
    );
  }

  const muted = isDark ? 'text-slate-500' : 'text-gray-500';
  const avgMs = concatDurationS !== null ? Math.round((concatDurationS * 1000) / entries.length) : null;
  const playingFragment = activeIdx >= 0 ? activeIdx + 1 : null;

  return (
    <div className="mt-1.5 space-y-1.5">
      {/* Track selector + fragment picker */}
      <div className="flex flex-wrap items-center gap-1">
        {hasConcat && (
          <button
            onClick={() => selectFragment(-1)}
            className={chipClass(isDark, activeIdx === -1, 'emerald')}
            title={hasAsHeard ? "Server track: the ideal TTS audio as synthesized (no client playback breaks)" : "Play the whole turn as one gapless stream (server-stitched)"}
          >
            {hasAsHeard ? 'server' : 'all'}
          </button>
        )}
        {hasAsHeard && (
          <button
            onClick={() => selectFragment(-2)}
            className={chipClass(isDark, activeIdx === -2, 'amber')}
            title="What the user actually heard — reconstructed from client playback timing (underrun silences spliced into the server audio); not a recording"
          >
            as heard
          </button>
        )}

        {dense ? (
          <>
            <span className={`text-[10px] font-mono ml-1 ${muted}`} title="Realtime providers emit one fragment per audio delta; individually these are too short to be meaningful. Use the stitched track, or step through them.">
              {entries.length} fragments{avgMs !== null ? ` · ~${avgMs} ms avg` : ''}
            </span>
            <button
              onClick={() => setInspecting(v => !v)}
              className={`text-[10px] font-mono px-1.5 py-0.5 rounded inline-flex items-center gap-0.5 transition-colors ${isDark ? 'text-slate-400 hover:text-slate-200 hover:bg-slate-700/60' : 'text-gray-500 hover:text-gray-700 hover:bg-gray-100'}`}
              aria-expanded={inspecting}
              title={inspecting ? 'Hide the fragment stepper' : 'Step through individual fragments'}
            >
              {inspecting ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
              inspect
            </button>
          </>
        ) : (
          entries.map(([kind], i) => {
            const text = fragmentTexts?.[kind];
            const label = text ? fragmentSnippet(text) : String(i + 1);
            return (
              <button
                key={kind}
                onClick={() => selectFragment(i)}
                className={`${chipClass(isDark, i === activeIdx, 'violet')} max-w-[160px] truncate`}
                title={text ? `Fragment ${i + 1}: ${text}` : `Play fragment ${i + 1} (${kind})`}
              >
                {label}
              </button>
            );
          })
        )}
      </div>

      {/* Dense mode: opt-in stepper */}
      {dense && inspecting && (
        <div className={`flex items-center gap-1 text-[10px] font-mono ${muted}`}>
          <button
            onClick={() => stepFragment(-1)}
            className={`p-0.5 rounded ${isDark ? 'hover:bg-slate-700/60 hover:text-slate-200' : 'hover:bg-gray-100 hover:text-gray-700'}`}
            aria-label="Previous fragment"
            title="Previous fragment"
          >
            <ChevronLeft size={12} />
          </button>
          <span>fragment</span>
          <input
            type="number"
            min={1}
            max={entries.length}
            value={stepperInput}
            placeholder={playingFragment !== null ? String(playingFragment) : '–'}
            onChange={e => setStepperInput(e.target.value)}
            onBlur={commitStepperInput}
            onKeyDown={e => { if (e.key === 'Enter') commitStepperInput(); }}
            aria-label="Fragment number"
            className={`w-14 px-1 py-0.5 rounded text-center font-mono text-[10px] outline-none ring-1 ${
              isDark ? 'bg-slate-800 text-slate-200 ring-slate-700 focus:ring-violet-500/60' : 'bg-white text-gray-800 ring-gray-200 focus:ring-violet-400/60'
            }`}
          />
          <span>/ {entries.length}</span>
          <button
            onClick={() => stepFragment(1)}
            className={`p-0.5 rounded ${isDark ? 'hover:bg-slate-700/60 hover:text-slate-200' : 'hover:bg-gray-100 hover:text-gray-700'}`}
            aria-label="Next fragment"
            title="Next fragment"
          >
            <ChevronRight size={12} />
          </button>
          {playingFragment !== null && (
            <span className={`ml-1 ${isDark ? 'text-violet-300' : 'text-violet-600'}`}>
              playing #{playingFragment}
            </span>
          )}
        </div>
      )}

      {/* Fragment timeline: where each fragment sits on the track being
          played, with the client's underruns and barge-in drawn in. */}
      {timelineFragments && (
        <FragmentTimeline
          fragments={timelineFragments}
          fragmentTexts={fragmentTexts}
          heard={heardModel}
          mode={activeIdx === -2 ? 'heard' : 'server'}
          activeFragment={activeIdx >= 0 ? activeIdx : null}
          audioRef={audioRef}
          onSelectFragment={selectFragment}
          isDark={isDark}
        />
      )}

      {/* Shared player */}
      <audio
        ref={audioRef}
        controls
        className="w-full h-7"
        src={currentSrc}
        onEnded={handleEnded}
        onLoadedMetadata={handleLoadedMetadata}
      />
    </div>
  );
};
