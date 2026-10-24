// useSynchronizedSpeech — clock-driven karaoke highlight hook.
//
// Replaces the RAF loop in useWordReveal with a thin React adapter over the
// pure `PlaybackClock` + `WordTimingTable` data layer.  The hook subscribes
// to the clock, asks the table how many words have been heard at the current
// playback offset, and exposes a `HighlightInfo` for the UI.
//
// Design properties:
//
// • Single source of time:  every word index comes from `clock.getNowMs()`.
//   No `performance.now()`, no `Date.now()`, no `setTimeout` for word timing.
//
// • Single source of word geometry:  the `WordTimingTable` owns the math.
//   This hook never tokenizes text, never estimates syllables, never picks
//   a fallback strategy.
//
// • Monotonic ratchet:  the visible word index never moves backwards within
//   an utterance.  Reset happens only when `utteranceId` changes.
//
// • Pure subscription:  the hook starts the clock by subscribing on mount
//   and stops it by unsubscribing on unmount.  Idle utterances cost zero
//   CPU because `DefaultPlaybackClock` lazily starts its driver.
//
// The hook does NOT own:  the audio service, the transcript reducer, the
// interrupt reducer, or the entry-index ↔ utterance-id mapping.  Those are
// all passed in via the `getActive` callback so the same hook can drive any
// transport (WebSocket today, WebRTC tomorrow).

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { ClockSnapshot, PlaybackClock } from "../../../lib/audio/playback-clock";
import type { WordTimingTable } from "../../../lib/audio/word-timing-table";
import type { HighlightInfo } from "./types";

/** What the hook needs to know about the *currently active* utterance to
 *  produce a `HighlightInfo`.  Returning `null` means "no active utterance"
 *  and the hook will emit `null`. */
export interface ActiveUtterance {
  /** Server-assigned utterance id whose audio is being played.  Used as the
   *  cache key for the ratchet — when this changes the ratchet resets. */
  utteranceId: number;
  /** The word-timing table accumulated for this utterance so far. */
  table: WordTimingTable;
  /** Index into the transcript entries array for the agent bubble being
   *  spoken.  Carried through to the emitted `HighlightInfo` so the UI can
   *  locate the right `<ChatBubble>` to highlight. */
  entryIndex: number;
}

export interface UseSynchronizedSpeechOptions {
  /** Returns the active utterance descriptor, or `null` when no agent
   *  utterance is being played.  Called on every clock tick.  Should be
   *  cheap — typically a synchronous read from a ref. */
  getActive: () => ActiveUtterance | null;

  /** When `false`, the hook does not subscribe to the clock and emits
   *  `null`.  Used by the migration feature flag. */
  enabled?: boolean;
}

export interface UseSynchronizedSpeechResult {
  /** Current highlight info (drives the karaoke marker). */
  highlight: HighlightInfo | null;
  /** Synchronously-updated mirror of `highlight` for stale-closure-free reads
   *  in async callbacks (e.g. the barge-in trim sites that need to know the
   *  last revealed word index at the moment of an interrupt). */
  highlightRef: React.MutableRefObject<HighlightInfo | null>;
  /** Latest clock snapshot for diagnostics / dev overlays.  Updated on every
   *  tick but referentially-equal snapshots are dropped to avoid spurious
   *  re-renders. */
  snapshot: ClockSnapshot | null;
}

/** Compute a single highlight tick.  Pure — exported for unit tests. */
export function computeHighlight(
  active: ActiveUtterance | null,
  nowMs: number,
  ratchetFloor: number,
): { highlight: HighlightInfo | null; nextRatchet: number } {
  if (!active) return { highlight: null, nextRatchet: 0 };

  const wordsHeard = active.table.wordsHeardAtMs(nowMs, ratchetFloor);
  if (wordsHeard <= 0) {
    return { highlight: null, nextRatchet: ratchetFloor };
  }

  // wordsHeard is a *count*, the highlight wants the *index* of the latest
  // word.  Clamp at table.wordCount-1 so the final word stays highlighted
  // when nowMs runs past the last timing.
  const wordIndex = Math.min(active.table.wordCount - 1, wordsHeard - 1);
  return {
    highlight: { entryIndex: active.entryIndex, wordIndex },
    nextRatchet: Math.max(ratchetFloor, wordsHeard),
  };
}

export function useSynchronizedSpeech(
  clock: PlaybackClock,
  opts: UseSynchronizedSpeechOptions,
): UseSynchronizedSpeechResult {
  const { getActive, enabled = true } = opts;

  const [highlight, setHighlight] = useState<HighlightInfo | null>(null);
  const [snapshot, setSnapshot] = useState<ClockSnapshot | null>(null);

  // Synchronous mirror of `highlight` so async callbacks can read the latest
  // word index without React closures lagging behind.
  const highlightRef = useRef<HighlightInfo | null>(null);

  // Per-utterance ratchet floor.  Stored in a ref so it survives across
  // ticks without forcing a re-render and never participates in dependency
  // arrays.
  const ratchetRef = useRef<{ utteranceId: number | null; floor: number }>({
    utteranceId: null,
    floor: 0,
  });

  // Stable handle to the latest getActive — avoids re-subscribing the clock
  // every time the caller passes a new closure.  Synced in a layout effect
  // (not during render): it runs synchronously after commit, before the clock
  // driver (RAF / message loop) can deliver the next tick, so no tick ever
  // reads a stale closure.
  const getActiveRef = useRef(getActive);
  useLayoutEffect(() => {
    getActiveRef.current = getActive;
  }, [getActive]);

  useEffect(() => {
    // Disabled: no subscription, nothing emitted.  Any highlight left over
    // from a previous enabled period was already cleared by that
    // subscription's teardown below.
    if (!enabled) return;

    const unsubscribe = clock.subscribe((snap) => {
      setSnapshot(snap);

      const active = getActiveRef.current();

      // Reset ratchet on utterance change so a new turn starts at word 0.
      const currentId = active?.utteranceId ?? null;
      if (ratchetRef.current.utteranceId !== currentId) {
        ratchetRef.current = { utteranceId: currentId, floor: 0 };
      }

      const { highlight: hi, nextRatchet } = computeHighlight(
        active,
        snap.nowMs,
        ratchetRef.current.floor,
      );

      ratchetRef.current.floor = nextRatchet;

      // Avoid spurious setState when nothing visible changed.
      setHighlight((prev) => {
        if (prev === hi) {
          highlightRef.current = prev;
          return prev;
        }
        if (
          prev &&
          hi &&
          prev.entryIndex === hi.entryIndex &&
          prev.wordIndex === hi.wordIndex
        ) {
          highlightRef.current = prev;
          return prev;
        }
        highlightRef.current = hi;
        return hi;
      });
    });

    return () => {
      unsubscribe();
      // Teardown (disable / clock swap / unmount) clears the emitted values so
      // a later re-enable never shows the previous utterance's highlight
      // before its first fresh tick.  Runs in the same commit phase the old
      // in-effect clear did, so consumers observe identical timing.
      highlightRef.current = null;
      setHighlight(null);
      setSnapshot(null);
    };
  }, [clock, enabled]);

  return { highlight, highlightRef, snapshot };
}
