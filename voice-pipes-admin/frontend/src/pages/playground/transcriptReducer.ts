/**
 * Pure reducer for playground transcript state.
 *
 * Owns the ordered conversation entries (`entries`) only.  All per-turn
 * bookkeeping (speaking entry index, placeholder utterance id, agent word
 * count, text-received tracking) lives in refs inside `useTranscriptManager`,
 * and the karaoke reveal pipeline (clock + `WordTimingTable` +
 * `useSynchronizedSpeech`) is layered on top and does NOT round-trip through
 * this reducer.
 */

import type {
  ErrorComponent,
  ErrorSeverity,
  PipelineMessage,
} from "../../lib/transport/types";

// Re-export the production TranscriptEntry shape so consumers (hooks, tests,
// UI components) all agree on the entry type.
export type { TranscriptEntry } from "./session/types";
import type { TranscriptEntry } from "./session/types";

export interface TranscriptState {
  /** Ordered conversation entries displayed in the transcript. */
  entries: TranscriptEntry[];
  /**
   * Index into `entries` of the currently-speaking agent bubble (which renders
   * via `StreamingAgentBubble` with karaoke highlighting). `-1` means no entry
   * is actively speaking (idle, just demoted by interrupt, or rotated to a
   * finished `ChatBubble`). Owned here so that updates ride the same React
   * commit as the `entries` change that conceptually creates them, eliminating
   * the prior nested-`setSpeakingEntryIndex(...)`-inside-`setTranscript`-updater
   * pattern that was a deferred-render hazard under StrictMode / concurrent.
   *
   * Optional in stored state for backwards compatibility with tests that
   * construct partial `TranscriptState` literals; the reducer treats absence
   * as `-1`.
   */
  speakingEntryIndex?: number;
}

export type TranscriptAction =
  /** User typed a message and pressed send. */
  | { type: "USER_TEXT_SENT"; text: string; now: number }
  /** Server reported an error from a pipeline component. */
  | {
      type: "ERROR_RECEIVED";
      message: string;
      now: number;
      severity?: ErrorSeverity;
      errorCode?: string;
      component?: ErrorComponent;
    }
  /** Structured pipeline message (chat / tool / data). */
  | {
      type: "PIPELINE_MESSAGE";
      pipelineMessage: PipelineMessage;
      now: number;
    }
  /** Clear the conversation without resetting other session state. */
  | { type: "CLEAR_ALL" }
  /**
   * Updates an agent entry's `text` to the server's authoritative value
   * (received via TextTruncated after a PlaybackReport).  `heardText` is
   * unaffected — the client's real-time snapshot governs what ChatBubble
   * displays for interrupted turns.
   */
  | {
      type: "TEXT_TRUNCATED";
      utteranceId: number;
      finalText: string;
    }
  /**
   * Append an empty agent entry as a placeholder for an in-flight TTS turn.
   * Caller reads the resulting entry index from the dispatch's `onApplied`
   * callback to update its `speakingEntryRef` bookkeeping.
   */
  | { type: "AGENT_PLACEHOLDER_CREATED"; now: number; utteranceId: number }
  /**
   * Append `captionText` onto the true agent entry matching `utteranceId`
   * (separated by a single space if the entry already has text).
   * No-op if no matching agent entry exists.
   */
  | { type: "AGENT_CAPTION_APPEND"; utteranceId: number; captionText: string; concatRaw?: boolean }
  /**
   * Replace the placeholder agent entry's text with the authoritative LLM
   * `text` ONLY if it is currently empty (i.e. no caption fragments have
   * arrived).  When captions already populated the bubble, this is a
   * traceable no-op (debugLog records the ignore).
   */
  | { type: "AGENT_TEXT_FILL_PLACEHOLDER"; utteranceId: number; text: string }
  /**
   * Append a new finalized agent entry (used when the LLM text arrives
   * before any caption / playback started — no placeholder exists).
   * Caller reads the resulting index from `onApplied`.
   */
  | { type: "AGENT_FINAL_NEW"; text: string; now: number; utteranceId: number }
  /**
   * Append the user's STT-transcribed turn (no `type` field, distinct from
   * `USER_TEXT_SENT` which carries `type:"chat"` for the typed-message path).
   */
  | { type: "USER_TRANSCRIBED"; text: string; now: number; utteranceId?: number }
  /**
   * Append a system "interrupted" divider entry into the visible transcript
   * so the user can see exactly when an interruption fired.  `reason` is
   * shown verbatim when present.
   */
  | { type: "INTERRUPTED_DIVIDER"; reason?: string; now: number }
  /**
   * Freezes the agent entry's visible text at interrupt time by computing
   * the heard prefix (first `wordIndex + 1` words of `entry.text`) and
   * storing it in `entry.heardText`.  `entry.text` is left intact so the
   * authoritative LLM text remains available for history purposes and can
   * still be updated by the server's TEXT_TRUNCATED without affecting
   * what ChatBubble displays.
   */
  | { type: "AGENT_FREEZE_AT_WORD"; utteranceId: number; wordIndex: number }
  /**
   * Remove any agent entries that have no text / heardText, ensuring we don't
   * leave orphaned typing dot/placeholder bubbles behind.
   */
  | { type: "CLEANUP_EMPTY_PLACEHOLDERS"; utteranceId?: number }
  /**
   * Explicitly set the speaking entry index. Used by cross-file callers
   * (handleClearAudio, handlePlaybackEnd) to demote the current bubble from
   * `StreamingAgentBubble` to `ChatBubble` after an interrupt or natural
   * playback end, and by the hook's reset path. `-1` means no active speaker.
   */
  | { type: "SET_SPEAKING_ENTRY"; index: number }
  /**
   * Record the client-tool result the tester sent for a `trigger_action`
   * command card, keyed by the command's `call_id`. Persisted on the
   * transcript entry so the card keeps showing what was chosen (phase,
   * result payload, speak request) across re-renders. Terminal phases
   * (completed/failed) are not overwritten by later interim ones, so a
   * duplicate-reply test doesn't visually regress the card.
   */
  | {
      type: "CLIENT_TOOL_RESULT_RECORDED";
      callId: string;
      phase: "started" | "progress" | "completed" | "failed";
      result?: unknown;
      speak: boolean;
      now: number;
    };

export const initialTranscriptState: TranscriptState = {
  entries: [],
  speakingEntryIndex: -1,
};

export function transcriptReducer(
  state: TranscriptState,
  action: TranscriptAction,
): TranscriptState {
  switch (action.type) {
    case "USER_TEXT_SENT": {
      const entries: TranscriptEntry[] = [
        ...state.entries,
        { role: "user", text: action.text, timestamp: action.now, type: "chat" },
      ];
      return { ...state, entries };
    }

    case "ERROR_RECEIVED": {
      const lastEntry = state.entries[state.entries.length - 1];
      if (
        lastEntry &&
        lastEntry.type === "error" &&
        lastEntry.text === action.message
      ) {
        return state;
      }

      const entries: TranscriptEntry[] = [
        ...state.entries,
        {
          role: "system",
          text: action.message,
          timestamp: action.now,
          type: "error",
          severity: action.severity,
          errorCode: action.errorCode,
          component: action.component,
        },
      ];
      return { ...state, entries };
    }

    case "PIPELINE_MESSAGE": {
      const role =
        action.pipelineMessage.role === "assistant"
          ? "agent"
          : action.pipelineMessage.role;
      const entries: TranscriptEntry[] = [
        ...state.entries,
        {
          role,
          text: action.pipelineMessage.text ?? "",
          timestamp: action.now,
          type: "message",
          pipelineMessage: action.pipelineMessage,
        },
      ];
      return { ...state, entries };
    }

    case "CLEAR_ALL":
      return initialTranscriptState;

    case "TEXT_TRUNCATED": {
      const { utteranceId, finalText } = action;
      const targetIndex = state.entries.findIndex((e) => e.role === "agent" && e.utteranceId === utteranceId);
      if (targetIndex < 0) return state;

      const entry = state.entries[targetIndex];
      // Update entry.text to the server's authoritative value.  If the entry
      // has a heardText snapshot (set by AGENT_FREEZE_AT_WORD at interrupt
      // time), ChatBubble renders heardText instead of text, so the display
      // is already frozen and unaffected by this update.
      const next = [...state.entries];
      next[targetIndex] = {
        ...entry,
        text: finalText,
        debugLog: [
          ...(entry.debugLog || []),
          "TRIMMED (server)",
        ],
      };
      return { ...state, entries: next };
    }

    case "AGENT_PLACEHOLDER_CREATED": {
      // If an AGENT entry for this utteranceId already exists (either a prior placeholder
      // or a fully fleshed out response from early text arrival), ignore this.
      // We must scope by role="agent" because the server assigns the same utteranceId 
      // to the user's STT turn and the agent's responding turn.
      if (state.entries.some(e => e.role === "agent" && e.utteranceId === action.utteranceId)) {
        return state;
      }
      const entries: TranscriptEntry[] = [
        ...state.entries,
        {
          role: "agent",
          text: "",
          timestamp: action.now,
          utteranceId: action.utteranceId,
          debugLog: ["CREATED PLACEHOLDER"],
        },
      ];
      // The newly appended placeholder is the active speaker.
      return { ...state, entries, speakingEntryIndex: entries.length - 1 };
    }

    case "AGENT_CAPTION_APPEND": {
      const { utteranceId, captionText, concatRaw } = action;
      if (utteranceId < 0) return state;
      const entryIndex = state.entries.findLastIndex(e => e.utteranceId === utteranceId);
      if (entryIndex < 0) return state;
      const entry = state.entries[entryIndex];
      if (entry.role !== "agent") return state;
      
      // If the LLM text message fully populated the entry before any incremental
      // captions naturally arrived, we must NOT indiscriminately append to it,
      // otherwise we duplicate text! We do, however, want the transcript to correctly
      // highlight, which WordTimingTable will handle as recordCaptionFragment executes.
      if (entry.isFullTextFilled) {
        return state;
      }

      // `concatRaw` = true: the caller is feeding token-level deltas whose
      // text already includes its own leading whitespace where appropriate
      // (e.g. OpenAI Realtime: " world", ".", ","). Inserting an extra space
      // shatters words ("Z" + " " + "ircon" → "Z ircon"). The classic TTS
      // caption path (sentence-fragment granularity) keeps the legacy
      // space-joined behaviour so multi-fragment captions don't get glued
      // together without separators.
      const newText = concatRaw
        ? entry.text + captionText
        : entry.text ? entry.text + " " + captionText : captionText;
      const next = [...state.entries];
      next[entryIndex] = {
        ...entry,
        text: newText,
        debugLog: [
          ...(entry.debugLog || []),
          `APPEND${concatRaw ? "(raw)" : ""}: received "${captionText}", resulting text: "${newText}"`,
        ],
      };
      return { ...state, entries: next };
    }

    case "AGENT_TEXT_FILL_PLACEHOLDER": {
      const { utteranceId, text } = action;
      if (utteranceId < 0) return state;
      const entryIndex = state.entries.findLastIndex(e => e.utteranceId === utteranceId);
      if (entryIndex < 0) return state;
      const entry = state.entries[entryIndex];
      if (entry.role !== "agent") return state;
      const hasCaption = entry.text.length > 0;
      const next = [...state.entries];
      next[entryIndex] = hasCaption
        ? {
            ...entry,
            debugLog: [
              ...(entry.debugLog || []),
              `processFinalText(hasCaption=true): ignored "${text}"`,
            ],
          }
        : {
            ...entry,
            text,
            isFullTextFilled: true,
            debugLog: [
              ...(entry.debugLog || []),
              `processFinalText(hasCaption=false): filled "${text}"`,
            ],
          };
      return { ...state, entries: next };
    }

    case "AGENT_FINAL_NEW": {
      const entries: TranscriptEntry[] = [
        ...state.entries,
        { role: "agent", text: action.text, timestamp: action.now, utteranceId: action.utteranceId, isFullTextFilled: true },
      ];
      // A genuinely new finalized agent entry was appended (late-arrival
      // path with no prior placeholder); promote it to the active speaker so
      // the karaoke layer can attach to it for tail playback.
      return { ...state, entries, speakingEntryIndex: entries.length - 1 };
    }

    case "USER_TRANSCRIBED": {
      const { text, utteranceId, now } = action;
      const newEntry: TranscriptEntry = {
        role: "user",
        text,
        timestamp: now,
        utteranceId,
      };

      if (utteranceId !== undefined && utteranceId >= 0) {
        // The server assigns the same utteranceId to the user's STT turn and
        // the agent's responding turn, and the reply's placeholder can render
        // before the final transcript arrives — so splice the user bubble in
        // front of its own turn's AGENT entries.  Match the id exactly:
        // agent-initiated turns (idle prompts, agent Say) live in the
        // 10001+ id range, so a `>=` comparison would hoist this user turn
        // above *earlier* agent-initiated bubbles.  Match agent entries
        // only: a client-tool dispatch can flush an earlier user bubble
        // under the SAME (provisional) utterance id — splicing above it
        // would invert the user's own speaking order.
        const insertIndex = state.entries.findIndex(
          (e) => e.role === "agent" && e.utteranceId === utteranceId
        );

        if (insertIndex >= 0) {
          const next = [...state.entries];
          next.splice(insertIndex, 0, newEntry);
          
          // Since we inserted an entry before insertIndex, if speakingEntryIndex
          // is at or after insertIndex, we MUST shift speakingEntryIndex by +1
          // to keep it pointing to the correct agent bubble!
          let nextSpeakingIdx = state.speakingEntryIndex ?? -1;
          if (nextSpeakingIdx >= insertIndex) {
            nextSpeakingIdx += 1;
          }
          
          return {
            ...state,
            entries: next,
            speakingEntryIndex: nextSpeakingIdx,
          };
        }
      }

      const entries: TranscriptEntry[] = [...state.entries, newEntry];
      return { ...state, entries };
    }

    case "AGENT_FREEZE_AT_WORD": {
      const { utteranceId, wordIndex } = action;
      if (utteranceId < 0) return state;
      const entryIndex = state.entries.findLastIndex(
        (e) => e.role === "agent" && e.utteranceId === utteranceId,
      );
      if (entryIndex < 0) return state;
      const entry = state.entries[entryIndex];
      if (entry.role !== "agent") return state;
      // Compute the heard prefix: first (wordIndex + 1) words, preserving
      // whitespace tokens so the text looks natural.
      const tokens = entry.text.split(/(\s+)/);
      let wordCount = 0;
      const visibleTokens: string[] = [];
      for (const token of tokens) {
        if (/\S/.test(token)) {
          if (wordCount > wordIndex) break;
          wordCount++;
        }
        visibleTokens.push(token);
      }
      const heardText = visibleTokens.join("").trimEnd();
      const next = [...state.entries];
      // Store the heard prefix in heardText.  entry.text is left intact so
      // the server's TEXT_TRUNCATED can still update it for history purposes
      // without affecting the ChatBubble display.
      next[entryIndex] = {
        ...entry,
        heardText,
        debugLog: [
          ...(entry.debugLog || []),
          `FREEZE_AT_WORD: wordIndex=${wordIndex} → "${heardText}"`,
        ],
      };
      return { ...state, entries: next };
    }

    case "INTERRUPTED_DIVIDER": {
      // Coalesce back-to-back dividers.  Barge-in produces TWO dispatches
      // in close succession: one synchronously when the client decides to
      // interrupt (voice / cancel button / sendText), and a second when
      // the server's `clear_audio` confirmation round-trips a few ms later.
      // Without coalescing the user sees a stack of empty dividers.  If
      // the most recent entry is already an interrupted divider within
      // 2 s, replace it in place — preferring whichever dispatch carried
      // a non-empty `reason` (the server one is usually more specific).
      const COALESCE_WINDOW_MS = 2000;
      // Drop a trailing empty agent placeholder before coalescing.  When
      // a turn is interrupted before any text arrives, the placeholder
      // bubble (typing dots) is left behind between the two dividers,
      // which both blocks the coalesce check below and looks broken in
      // the UI.  Removing it lets the second divider merge with the
      // first and hides the never-spoken bubble.
      let baseEntries = state.entries;
      const tail = baseEntries[baseEntries.length - 1];
      if (tail && tail.role === "agent" && !tail.text) {
        baseEntries = baseEntries.slice(0, -1);
      }
      const last = baseEntries[baseEntries.length - 1];
      const newText = action.reason ? `interrupted (${action.reason})` : "";
      if (
        last &&
        last.type === "interrupted" &&
        action.now - last.timestamp < COALESCE_WINDOW_MS
      ) {
        // Prefer a non-empty reason over an empty one; otherwise the
        // newer dispatch wins (so a later, more authoritative reason
        // can overwrite a vague earlier one).
        const mergedText = newText || last.text;
        const next = baseEntries.slice(0, -1);
        next.push({
          role: "system",
          text: mergedText,
          timestamp: action.now,
          type: "interrupted",
        });
        return { ...state, entries: next };
      }
      const entries: TranscriptEntry[] = [
        ...baseEntries,
        {
          role: "system",
          text: newText,
          timestamp: action.now,
          type: "interrupted",
        },
      ];
      return { ...state, entries };
    }

    case "CLEANUP_EMPTY_PLACEHOLDERS": {
      const targetUtteranceId = action.utteranceId;
      const originalSpeakingId = state.speakingEntryIndex !== undefined && state.speakingEntryIndex >= 0 && state.speakingEntryIndex < state.entries.length
        ? state.entries[state.speakingEntryIndex].utteranceId
        : null;

      const nextEntries = state.entries.filter((e) => {
        const isTargetEmptyPlaceholder =
          e.role === "agent" &&
          // Structured PipelineMessage entries (trigger_action client
          // commands, tool_call / tool_result activity) legitimately
          // have no text — they are panels, not speech placeholders,
          // and must survive the listening-state sweep.
          e.type !== "message" &&
          !e.text &&
          !e.heardText &&
          (targetUtteranceId === undefined || e.utteranceId === targetUtteranceId);
        return !isTargetEmptyPlaceholder;
      });

      if (nextEntries.length === state.entries.length) {
        return state;
      }

      // Re-map speakingEntryIndex:
      let nextSpeakingIdx = -1;
      if (originalSpeakingId !== null) {
        nextSpeakingIdx = nextEntries.findIndex(
          (e) => e.role === "agent" && e.utteranceId === originalSpeakingId
        );
      }

      return {
        ...state,
        entries: nextEntries,
        speakingEntryIndex: nextSpeakingIdx,
      };
    }

    case "CLIENT_TOOL_RESULT_RECORDED": {
      let changed = false;
      const entries = state.entries.map((e) => {
        if (e.type !== "message" || e.pipelineMessage?.kind !== "trigger_action") return e;
        const payload = e.pipelineMessage.payload as { call_id?: string } | null;
        if (payload?.call_id !== action.callId) return e;
        // The first terminal outcome sticks — the server ignores
        // duplicate results too, so the card mirrors what actually
        // resolved the call.
        const existing = e.clientToolOutcome;
        if (existing && (existing.phase === "completed" || existing.phase === "failed")) {
          return e;
        }
        changed = true;
        return {
          ...e,
          clientToolOutcome: {
            phase: action.phase,
            result: action.result,
            speak: action.speak,
            timestamp: action.now,
          },
        };
      });
      return changed ? { ...state, entries } : state;
    }

    case "SET_SPEAKING_ENTRY": {
      // Idempotent fast-path: if nothing changes, return the same state
      // reference so React skips the re-render.
      if ((state.speakingEntryIndex ?? -1) === action.index) return state;
      return { ...state, speakingEntryIndex: action.index };
    }

    default:
      return state;
  }
}
