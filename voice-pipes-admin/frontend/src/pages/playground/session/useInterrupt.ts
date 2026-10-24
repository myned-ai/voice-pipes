// useInterrupt — React hook wrapping the pure `interruptReducer`.
//
// Replaces `useBargeIn` (three loose `useRef`s) with a single state machine.
// State transitions are deterministic and unit-tested in
// `interruptReducer.test.ts`; this hook only owns the React glue.
//
// The hook also keeps a `stateRef` so event handlers reading the current
// interrupt state from inside async callbacks (WebSocket message handlers,
// RAF callbacks) always see the latest value without stale-closure hazards.

import { useCallback, useReducer, useRef } from "react";
import {
  initialInterruptState,
  interruptReducer,
  shouldDropAudio as selShouldDropAudio,
  shouldDropFinalText as selShouldDropFinalText,
  type InterruptAction,
  type InterruptState,
} from "./interruptReducer";

export interface UseInterruptApi {
  /** Latest reducer state.  Updated synchronously via `stateRef` so async
   *  callbacks can read it without stale closures; React re-renders use
   *  the `state` value returned by `useReducer`. */
  state: InterruptState;
  /** Synchronous accessor for the latest state (safe inside async callbacks). */
  getState: () => InterruptState;
  /** Dispatch an action.  Identity is stable across renders. */
  dispatch: (action: InterruptAction) => void;
  /** True when an `audio` frame for `utteranceId` should be ignored.
   *  `null` means "no active utterance known" — never flushed → returns false. */
  shouldDropAudio: (utteranceId: number | null) => boolean;
  /** True when a final `text` frame with `messageId` should be ignored. */
  shouldDropFinalText: (messageId: string) => boolean;
  /** Read and clear `pendingHeardMs` in a single step. */
  consumePendingHeardMs: () => number | null;
  /** Hard reset (e.g. on reconnect). */
  reset: () => void;
}

export function useInterrupt(): UseInterruptApi {
  const [state, baseDispatch] = useReducer(interruptReducer, initialInterruptState);

  // Mirror state into a ref so callbacks created via `useCallback` (and the
  // async WebSocket handler) always read the latest value — `useReducer`
  // values close over the render in which they were captured.
  //
  // The ref is owned exclusively by `dispatch` below (never touched during
  // render): every state transition goes through `dispatch`, which applies the
  // same pure reducer React will apply, so the ref is always exactly the state
  // React is about to render — with no window where a synchronous read could
  // observe a value older than the last dispatched action.
  const stateRef = useRef(initialInterruptState);

  const dispatch = useCallback((action: InterruptAction) => {
    // Optimistically advance the ref so synchronous reads later in the same
    // tick (e.g. dispatch then immediately read for a downstream side
    // effect) see the new state without waiting for React to re-render.
    stateRef.current = interruptReducer(stateRef.current, action);
    baseDispatch(action);
  }, []);

  const getState = useCallback(() => stateRef.current, []);

  const shouldDropAudio = useCallback(
    (utteranceId: number | null) => selShouldDropAudio(stateRef.current, utteranceId),
    [],
  );

  const shouldDropFinalText = useCallback(
    (messageId: string) => selShouldDropFinalText(stateRef.current, messageId),
    [],
  );

  const consumePendingHeardMs = useCallback((): number | null => {
    const value = stateRef.current.pendingHeardMs;
    if (value !== null) {
      dispatch({ type: "CONSUME_PENDING_HEARD_MS" });
    }
    return value;
  }, [dispatch]);

  const reset = useCallback(() => {
    dispatch({ type: "RESET" });
  }, [dispatch]);

  return {
    state,
    getState,
    dispatch,
    shouldDropAudio,
    shouldDropFinalText,
    consumePendingHeardMs,
    reset,
  };
}
