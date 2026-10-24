// interruptReducer — unified barge-in state machine.
//
// Replaces the three loose `useRef`s in `useBargeIn` (flushedUtteranceRef,
// blockedLlmMessageIdRef, preFlushPlayedMsRef) with a single pure reducer
// whose state transitions are deterministic, exhaustively testable, and
// independent of React.
//
// Three initiator paths converge here:
//
//   A. Server-detected barge-in    → SERVER_CLEAR_AUDIO
//   B. User typed text mid-speech  → USER_INITIATED_FLUSH
//   C. Explicit cancel button      → USER_INITIATED_FLUSH (with reason "client_cancel")
//
// All three cause subsequent `audio` frames and the eventual
// `AGENT_TEXT_FINALIZED` for the same utterance to be ignored.
//
// Ordering invariant (enforced here, not at the call site):
//   `TURN_FINALIZED` arriving after either flush action MUST NOT clear the
//   interrupt state — late `audio_meta` or `text` for the flushed utterance
//   would otherwise leak through.

/** Why a flush happened. */
export type InterruptReason =
  | "user_speech"   // server-detected barge-in
  | "user_text"     // user sent text mid-speech
  | "client_cancel" // explicit cancel button
  | "disconnect";   // transport dropped

/** State held by the reducer. */
export interface InterruptState {
  /** The server-assigned utterance ID whose audio has been flushed.  Any
   *  later `audio` frame stamped with this ID must be dropped.  `null`
   *  when no interrupt is in flight. */
  flushedUtteranceId: number | null;

  /** The `messageId` of the canonical LLM text whose late arrival must be
   *  ignored (so a late `text` frame cannot overwrite already-trimmed text).
   *  Format `"llm-{utteranceId}"`.  `null` when nothing is blocked. */
  blockedFinalMessageId: string | null;

  /** Milliseconds of audio that had already played at the moment the user
   *  initiated a flush via the text path (path B).  Captured here so the
   *  later-arriving server `clear_audio` does not double-flush and can
   *  reuse the accurate offset for `playback_report`.  `null` when not in
   *  the user-text path. */
  pendingHeardMs: number | null;

  /** Why the most recent flush happened.  `null` when no flush is in flight. */
  reason: InterruptReason | null;
}

/** The empty initial state. */
export const initialInterruptState: InterruptState = {
  flushedUtteranceId: null,
  blockedFinalMessageId: null,
  pendingHeardMs: null,
  reason: null,
};

// ── Actions ────────────────────────────────────────────────────────────────

export type InterruptAction =
  /** User typed or clicked cancel while the agent was speaking.  The caller
   *  has already called `playback.interrupt()` and captured `heardMs`.
   *  `utteranceId` may be null when no utterance is active — the reducer
   *  no-ops in that case (nothing to flush). */
  | {
      type: "USER_INITIATED_FLUSH";
      utteranceId: number | null;
      heardMs: number;
      reason: "user_text" | "client_cancel";
    }
  /** Server sent `clear_audio`.  May arrive in either order relative to
   *  `USER_INITIATED_FLUSH` for the same utterance — the reducer coalesces. */
  | {
      type: "SERVER_CLEAR_AUDIO";
      utteranceId: number;
      reason: "user_speech";
    }
  /** Transport dropped mid-utterance. */
  | {
      type: "TRANSPORT_DISCONNECTED";
      utteranceId: number | null;
    }
  /** A turn ended cleanly — fired on `state→listening`.  Only clears
   *  interrupt state when no flush is in flight for the current utterance. */
  | { type: "TURN_FINALIZED"; utteranceId: number | null }
  /** One-shot read of `pendingHeardMs` — zeroes it so a later `clear_audio`
   *  cannot double-count the value already used by the user-text path. */
  | { type: "CONSUME_PENDING_HEARD_MS" }
  /** Hard reset (e.g. on reconnect). */
  | { type: "RESET" };

// ── Selectors ──────────────────────────────────────────────────────────────

/** Whether incoming `audio` frames for the given utterance should be dropped.
 *  Accepts `null` (semantically "no active utterance known") and returns
 *  false in that case — a null id cannot have been flushed. */
export function shouldDropAudio(
  state: InterruptState,
  utteranceId: number | null,
): boolean {
  if (utteranceId == null) return false;
  return state.flushedUtteranceId === utteranceId;
}

/** Whether an incoming `AGENT_TEXT_FINALIZED` with the given `messageId`
 *  should be ignored. */
export function shouldDropFinalText(
  state: InterruptState,
  messageId: string,
): boolean {
  return state.blockedFinalMessageId === messageId;
}

/** Build the canonical `messageId` that gets blocked when an utterance is
 *  flushed.  Centralised here so server and client agree on the format. */
export function blockedMessageIdFor(utteranceId: number): string {
  return `llm-${utteranceId}`;
}

// ── Reducer ────────────────────────────────────────────────────────────────

export function interruptReducer(
  state: InterruptState,
  action: InterruptAction,
): InterruptState {
  switch (action.type) {
    case "USER_INITIATED_FLUSH": {
      // No-op when there's no active utterance to flush.  This happens if
      // the user hits Stop / sends text after the agent has already finished
      // and the speaking ref was nulled out.
      if (action.utteranceId == null) return state;
      // Idempotent: if we already flushed this utterance, keep the earliest
      // captured `heardMs` (which is the most accurate one — later flushes
      // happen after the audio has been stopped).
      if (state.flushedUtteranceId === action.utteranceId) {
        return state;
      }
      return {
        flushedUtteranceId: action.utteranceId,
        blockedFinalMessageId: blockedMessageIdFor(action.utteranceId),
        pendingHeardMs: action.heardMs,
        reason: action.reason,
      };
    }

    case "SERVER_CLEAR_AUDIO": {
      // If we've already flushed this utterance via the user-text path,
      // the user-initiated state wins — don't overwrite `pendingHeardMs`,
      // don't change the reason.  This is the ordering coalescence.
      if (state.flushedUtteranceId === action.utteranceId) {
        return state;
      }
      return {
        flushedUtteranceId: action.utteranceId,
        blockedFinalMessageId: blockedMessageIdFor(action.utteranceId),
        // `pendingHeardMs` stays null — the caller will measure it from the
        // playback service's own `interrupt()` call.
        pendingHeardMs: null,
        reason: action.reason,
      };
    }

    case "TRANSPORT_DISCONNECTED": {
      if (action.utteranceId == null) return state;
      return {
        flushedUtteranceId: action.utteranceId,
        blockedFinalMessageId: blockedMessageIdFor(action.utteranceId),
        pendingHeardMs: null,
        reason: "disconnect",
      };
    }

    case "TURN_FINALIZED": {
      // Ordering invariant: do NOT clear interrupt state if the finalized
      // turn is the one we just flushed.  Late `audio` or `text` for the
      // flushed utterance would otherwise leak through.
      if (action.utteranceId != null && state.flushedUtteranceId === action.utteranceId) {
        return state;
      }
      // A different utterance finalized cleanly — nothing to do; we keep
      // any in-flight interrupt state targeting some other utterance.
      return state;
    }

    case "CONSUME_PENDING_HEARD_MS": {
      if (state.pendingHeardMs === null) return state;
      return { ...state, pendingHeardMs: null };
    }

    case "RESET":
      return initialInterruptState;

    default:
      // Exhaustiveness check; widening to `never` would require strict
      // discriminated-union narrowing — this is a runtime fallback.
      return state;
  }
}
