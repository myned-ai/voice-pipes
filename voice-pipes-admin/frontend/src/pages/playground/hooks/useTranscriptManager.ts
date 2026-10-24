import { useCallback, useRef, useState } from "react";
import type { MutableRefObject } from "react";
import { type TranscriptEntry } from "../session/types";
import {
  WordTimingTable,
  timingsForFragment,
} from "../../../lib/audio/word-timing-table";
import type { CaptionMeta } from "../../../lib/transport/types";
import type { ActiveUtterance } from "../session/useSynchronizedSpeech";
import type { AudioPlaybackService } from "../../../lib/audio/playback";
import {
  transcriptReducer,
  type TranscriptAction,
  type TranscriptState,
  initialTranscriptState,
} from "../transcriptReducer";

interface UseTranscriptManagerProps {
  playbackRef: MutableRefObject<AudioPlaybackService | null>;
}

/** AudioContext timing snapshot taken when `state → listening` fires while
 *  audio is still buffered.  Used by the barge-in case-B path to derive an
 *  accurate `preFlushPlayedMs` for `PlaybackReport`. */
export interface SnapshotReveal {
  startTime: number;
  endTime: number;
  totalMs: number;
}

export function useTranscriptManager({ playbackRef }: UseTranscriptManagerProps) {
  // Single source of truth for transcript state.  Holds entries AND the
  // currently-speaking entry index, so an action that creates a placeholder
  // and an action that promotes that placeholder to the active speaker ride
  // the same React commit.  Eliminates the prior pattern of nesting
  // `setSpeakingEntryIndex(...)` inside `setTranscript`'s functional updater
  // (a deferred-render hazard under StrictMode / concurrent React because
  // the updater function may be called more than once).
  const [transcriptState, setTranscriptState] = useState<TranscriptState>(initialTranscriptState);
  const transcript = transcriptState.entries;
  const speakingEntryIndex = transcriptState.speakingEntryIndex ?? -1;

  // Utterance-ID refs typed as `number | null` rather than the historical
  // `-1` sentinel so the type system enforces null-checking and equality
  // comparisons against valid uttIds (`null !== 5`, `null === null`) cannot
  // collide with any real ID.  `speakingUtteranceIdRef` flows into outgoing
  // wire messages, so each wire-send call site in `usePlaygroundSession`
  // null-guards before passing the value to the typed `number` API — sending
  // a stale/sentinel ID was always wrong; not sending at all is the correct
  // semantic when no utterance is active.
  const placeholderUtteranceIdRef = useRef<number | null>(null);
  const agentTextReceivedForUtteranceRef = useRef<number | null>(null);
  const speakingUtteranceIdRef = useRef<number | null>(null);
  const speakingEntryIsLiveRef = useRef(false);

  // Per-utterance WordTimingTable registry, keyed by server-assigned
  // utteranceId.  Populated by `recordCaptionFragment` from each audio_meta
  // and by `recordFinalTextFallback` from the full LLM text (only if no
  // captions arrived).  Read by `getActiveUtterance()` for
  // `useSynchronizedSpeech`.
  const tablesRef = useRef<Map<number, WordTimingTable>>(new Map());

  // Inlined from the (now-deleted) useWordReveal hook: a tiny bag of refs
  // that survived the migration to the clock-driven reveal pipeline.
  // `snapshotRevealRef` / `captureSnapshot` exist solely for the barge-in
  // case-B PlaybackReport calculation in usePlaygroundSession; they are
  // unrelated to the karaoke reveal which is fully clock-driven now.
  const snapshotRevealRef = useRef<SnapshotReveal | null>(null);
  const agentEntryWordCountRef = useRef(0);
  // Synchronous mirror of `transcriptState.speakingEntryIndex` for callers
  // that need to read or block on the current speaker without waiting for a
  // React commit (e.g. handleClearAudio's late-write-blocking pattern, the
  // karaoke clock tick).  Updated inside `dispatch` whenever the reducer
  // changes the index so the two stay in lockstep without a separate `setState`.
  const speakingEntryRef = useRef(-1);

  const captureSnapshot = useCallback(() => {
    const pb = playbackRef.current;
    if (pb?.audioContext) {
      snapshotRevealRef.current = {
        startTime: pb.utteranceStartTime,
        endTime: pb.lastScheduledEndTime,
        totalMs: pb.totalEnqueuedDurationMs,
      };
    }
  }, [playbackRef]);

  /**
   * Single funnel for every transcript-state mutation.  Call sites express
   * intent via typed `TranscriptAction`s; the pure `transcriptReducer` produces
   * the next `TranscriptState` (entries + speakingEntryIndex) in one step.
   *
   * The wrapper synchronously mirrors `next.speakingEntryIndex` into
   * `speakingEntryRef` so the (ref, state) pair never drifts — the root
   * cause of the interrupted-bubble-stuck-on-TypingDots bug.  Idempotent ref writes inside the updater are safe even
   * if React calls the updater more than once (StrictMode / concurrent).
   *
   * `onApplied` (optional) receives the post-reducer `entries` array for ref
   * bookkeeping that depends on the resulting layout (e.g. agent word
   * counting in `appendCaption`).  It must NOT call any React `setState` —
   * use a dispatch action instead.
   */
  const dispatch = useCallback(
    (
      action: TranscriptAction | ((entries: TranscriptEntry[]) => TranscriptAction),
      onApplied?: (nextEntries: TranscriptEntry[]) => void,
    ) => {
      setTranscriptState((prev) => {
        const resolvedAction = typeof action === "function" ? action(prev.entries) : action;
        const next = transcriptReducer(prev, resolvedAction);
        // Sync mirror: keep `speakingEntryRef` in lockstep with the reducer.
        const nextIdx = next.speakingEntryIndex ?? -1;
        if (speakingEntryRef.current !== nextIdx) {
          speakingEntryRef.current = nextIdx;
        }
        onApplied?.(next.entries);
        return next;
      });
    },
    [],
  );

  // External callers that previously called `setSpeakingEntryIndex(idx)`
  // continue to do so via this thin wrapper, which dispatches the canonical
  // SET_SPEAKING_ENTRY action so the reducer + ref sync-mirror handle the
  // update uniformly.  Used by `usePlaygroundSession.handleClearAudio`
  // and `handlePlaybackEnd` to demote the speaking bubble after interrupt or
  // natural playback end.
  const setSpeakingEntryIndex = useCallback(
    (idx: number) => dispatch({ type: "SET_SPEAKING_ENTRY", index: idx }),
    [dispatch],
  );

  const resetWordRevealRefs = useCallback(() => {
    snapshotRevealRef.current = null;
    agentEntryWordCountRef.current = 0;
    // Demote any active speaker; reducer SET_SPEAKING_ENTRY also drives the
    // ref via the dispatch sync-mirror.
    dispatch({ type: "SET_SPEAKING_ENTRY", index: -1 });
    // Clear per-utterance word timing tables so stale data from the
    // previous session cannot corrupt the karaoke reveal when the server
    // reuses utterance IDs (it always starts from 0/1 on each new session).
    tablesRef.current.clear();
  }, [dispatch]);

  const resetTranscript = useCallback(() => {
    // CLEAR_ALL resets entries AND speakingEntryIndex to initial state in a
    // single commit; the dispatch sync-mirror keeps speakingEntryRef aligned.
    dispatch({ type: "CLEAR_ALL" });
    placeholderUtteranceIdRef.current = null;
    agentTextReceivedForUtteranceRef.current = null;
    speakingUtteranceIdRef.current = null;
    speakingEntryIsLiveRef.current = false;
    resetWordRevealRefs();
    tablesRef.current.clear();
  }, [dispatch, resetWordRevealRefs]);

  const handleServerListening = useCallback(() => {
    // Server emitted state → listening, but audio might still be actively
    // downloading/playing.  We intentionally do NOT reset the speaking refs
    // here — doing so would clear the highlight while the tail of the
    // previous utterance is still audible.  The new clock/table reveal
    // pipeline handles the natural finish.
    //
    // Specifically, do NOT reset `speakingUtteranceIdRef` to -1: late paced
    // chunks for the same turn still need to pass the caption-append guard
    // in `handleAudioMessage` (which compares `msg.meta.utteranceId ===
    // speakingUtteranceIdRef.current`). The next turn's first audio_meta
    // will overwrite this ref naturally via the `!==` check.
    speakingEntryIsLiveRef.current = false;
  }, []);

  // ---- Transcript mutation actions (all routed through `dispatch`) -------

  const appendCaption = useCallback(
    (captionText: string, serverUttId: number, concatRaw: boolean = false) => {
      dispatch(
        { type: "AGENT_CAPTION_APPEND", utteranceId: serverUttId, captionText, concatRaw },
        (next) => {
          // Recompute the running word count from the joined text so it
          // stays consistent with `processFinalText`, which sets the count
          // from the authoritative LLM text.
          const currentSpeaking = speakingEntryRef.current;
          const entryIndex = next.findLastIndex(e => e.utteranceId === serverUttId);
          if (entryIndex >= 0 && entryIndex === currentSpeaking) {
            agentEntryWordCountRef.current = next[entryIndex].text
              .split(/\s+/)
              .filter(Boolean).length;
          }
        },
      );
    },
    [dispatch],
  );

  const createPlaceholderEntry = useCallback(
    (uttId: number) => {
      // Reducer's AGENT_PLACEHOLDER_CREATED case sets `speakingEntryIndex` to
      // the appended entry's index in the same commit, and `dispatch`'s
      // sync-mirror updates `speakingEntryRef` — no nested setState needed.
      dispatch({ type: "AGENT_PLACEHOLDER_CREATED", now: Date.now(), utteranceId: uttId });
      agentEntryWordCountRef.current = 0;
      placeholderUtteranceIdRef.current = uttId;
      speakingEntryIsLiveRef.current = true;
    },
    [dispatch],
  );

  const processFinalText = useCallback(
    (
      text: string,
      serverTextUttId: number,
      role: "user" | "agent",
      timeMs: number = Date.now(),
    ) => {
      if (role === "user") {
        if (!text || !text.trim()) {
          return;
        }
        dispatch({ type: "USER_TRANSCRIBED", text, now: timeMs, utteranceId: serverTextUttId });
        return;
      }

      // role === "agent"
      agentTextReceivedForUtteranceRef.current = serverTextUttId;
      const words = text.split(/\s+/).filter(Boolean);
      agentEntryWordCountRef.current = words.length;

      if (
        speakingEntryRef.current >= 0 &&
        placeholderUtteranceIdRef.current === serverTextUttId
      ) {
        // Happy path: fill the current active placeholder.
        // The clock/table reveal pipeline (useSynchronizedSpeech) drives the
        // karaoke highlight from the WordTimingTable — no manual ratchet
        // seeding is needed here.
        placeholderUtteranceIdRef.current = null;
        dispatch({ type: "AGENT_TEXT_FILL_PLACEHOLDER", utteranceId: serverTextUttId, text });
      } else {
        // Late-arrival / no-placeholder path.
        //
        // Use the functional-dispatch form so we can inspect the *current*
        // entries snapshot before deciding which action to emit.  This handles
        // the race where turn N's final text arrives after turn N+1's
        // placeholder was created, which overwrites `placeholderUtteranceIdRef`
        // and causes the old check above to fall through.  Without this guard,
        // `AGENT_FINAL_NEW` would blindly append a *second* agent entry for
        // the same utteranceId that already has a placeholder — producing two
        // consecutive agent bubbles in the transcript.
        //
        // The reducer drives `speakingEntryIndex` for both branches:
        //   AGENT_TEXT_FILL_PLACEHOLDER — leaves it unchanged (correct: the
        //     active newer-turn placeholder must not be moved backwards).
        //   AGENT_FINAL_NEW — advances it to the new appended entry.
        // No nested setState is required.
        dispatch((entries) => {
          const existingIdx = entries.findLastIndex(
            (e) => e.role === "agent" && e.utteranceId === serverTextUttId,
          );
          if (existingIdx >= 0) {
            // A placeholder (or prior entry) for this utteranceId already
            // exists — fill it in place.  Only clear placeholderUtteranceIdRef
            // when it actually refers to this utterance (it may already point
            // to a newer turn's placeholder and must not be clobbered).
            if (placeholderUtteranceIdRef.current === serverTextUttId) {
              placeholderUtteranceIdRef.current = null;
            }
            return { type: "AGENT_TEXT_FILL_PLACEHOLDER", utteranceId: serverTextUttId, text };
          }
          // No existing entry — create a new finalized one.
          return { type: "AGENT_FINAL_NEW", text, now: timeMs, utteranceId: serverTextUttId };
        });
      }
    },
    [dispatch],
  );

  // ---- Step 5c: WordTimingTable population & active-utterance lookup -----

  /** Append a caption fragment's word timings to the table for `serverUttId`.
   *  Creates the table on first call.  Uses `timingsForFragment` so server
   *  word_timings (when present) take priority over syllable / char fallback.
   *  Safe to call repeatedly per utterance — fragments accumulate.
   *
   *  `fragmentDurationMs` is the audio CHUNK duration the caption was
   *  attached to.  `WordTimingTable.appendCaption` overrides this with a
   *  better fragment span derived from `word_timings` or
   *  `estimated_duration_ms` when available — those are far more accurate
   *  than the first-chunk duration. */
  const recordCaptionFragment = useCallback((
    serverUttId: number,
    caption: CaptionMeta,
    fragmentDurationMs: number,
    anchorMs?: number,
  ) => {
    if (serverUttId < 0) return;
    let table = tablesRef.current.get(serverUttId);
    if (!table) {
      table = new WordTimingTable();
      tablesRef.current.set(serverUttId, table);
    }
    table.appendCaption(caption, fragmentDurationMs, anchorMs);
  }, []);

  /** Fallback: if no captions arrived for this utterance (table is empty),
   *  build one fragment from the full LLM text using the char-weighted
   *  estimator.  No-op when captions already populated the table. */
  const recordFinalTextFallback = useCallback((
    serverUttId: number,
    text: string,
    totalDurationMs: number,
  ) => {
    if (serverUttId < 0 || totalDurationMs <= 0 || !text) return;
    let table = tablesRef.current.get(serverUttId);
    if (table && table.wordCount > 0) return;
    if (!table) {
      table = new WordTimingTable();
      tablesRef.current.set(serverUttId, table);
    }
    const fragment = timingsForFragment({ text }, totalDurationMs);
    table.append(fragment, totalDurationMs);
  }, []);

  /** Returns the descriptor `useSynchronizedSpeech` needs to drive the
   *  karaoke highlight, or `null` when no agent utterance is active.
   *  Uses server `utteranceId` (carried on audio_meta) as authority and
   *  reads the live agent bubble index from `speakingEntryRef`. */
  const getActiveUtterance = useCallback((): ActiveUtterance | null => {
    const utteranceId = speakingUtteranceIdRef.current;
    if (utteranceId == null) return null;
    const table = tablesRef.current.get(utteranceId);
    if (!table || table.wordCount === 0) return null;
    const entryIndex = speakingEntryRef.current;
    if (entryIndex < 0) return null;
    return { utteranceId, table, entryIndex };
  }, [speakingEntryRef]);

  return {
    transcript,
    dispatch,
    placeholderUtteranceIdRef,
    agentTextReceivedForUtteranceRef,
    speakingUtteranceIdRef,
    speakingEntryIsLiveRef,
    // Transcript bookkeeping refs/state (formerly in useWordReveal, now
    // inlined here), exposed flat so callers don't have to re-destructure a
    // nested handle.
    snapshotRevealRef,
    captureSnapshot,
    speakingEntryRef,
    speakingEntryIndex,
    setSpeakingEntryIndex,
    agentEntryWordCountRef,
    resetWordReveal: resetWordRevealRefs,
    resetTranscript,
    appendCaption,
    createPlaceholderEntry,
    processFinalText,
    handleServerListening,
    // Clock/table-driven reveal pipeline inputs.
    recordCaptionFragment,
    recordFinalTextFallback,
    getActiveUtterance,
  };
}
