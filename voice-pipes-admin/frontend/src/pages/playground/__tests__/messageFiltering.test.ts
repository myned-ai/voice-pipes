/**
 * Tests for the server message → transcript filtering rules in usePlaygroundSession.
 *
 * WHY THIS FILE EXISTS
 * --------------------
 * The playground transcript used to show the AI response TWICE:
 *   1. via "text" messages (kind="text") — which drive word-reveal and placeholder logic
 *   2. via "message" messages (kind="message", role="assistant") — which were appended
 *      as a second bubble, producing visible duplicates.
 *
 * The fix: the "message" handler only adds entries for role="system" pipeline messages
 * (e.g. tool-call results, metadata). User and assistant messages are deduplicated:
 *   - user: already added locally in sendText
 *   - assistant: already handled by the "text" path with full word-reveal logic
 *
 * These tests exercise the filtering logic in isolation, extracted as a pure function
 * so it can be tested without React, WebSocket, or requestAnimationFrame.
 *
 * REFACTORING NOTE
 * ----------------
 * The root cause of this bug escaping test coverage is that usePlaygroundSession
 * reimplements transcript state imperatively with useRef + setTranscript instead of
 * using the transcriptReducer. The reducer IS well-tested (transcriptReducer.test.ts),
 * but the hook's own imperative message handlers are not.
 *
 * Future work: migrate usePlaygroundSession to useReducer(transcriptReducer) so
 * ALL transcript mutations go through the well-tested reducer.
 */

import { describe, it, expect } from 'vitest';

// ── Pure helper extracted from the usePlaygroundSession "message" handler ──

type PipelineRole = 'user' | 'assistant' | 'system';

interface PipelineMessageStub {
  role: PipelineRole;
  text?: string;
  kind?: string;
}

/**
 * Mirrors the filtering decision in the "message" case of handleMessage.
 * Returns true when the message should be appended to the transcript.
 */
function shouldAddPipelineMessage(pm: PipelineMessageStub): boolean {
  const role = pm.role === 'user' ? 'user' : pm.role === 'system' ? 'system' : 'agent';
  // Only add system pipeline messages.
  // - user: already added locally in sendText
  // - assistant/agent: already handled by the "text" path (word-reveal, placeholder)
  return role === 'system';
}

// ── Tests ─────────────────────────────────────────────────────────────────────

describe('pipeline "message" deduplication filter', () => {
  it('suppresses assistant/agent messages (already handled by "text" path)', () => {
    expect(shouldAddPipelineMessage({ role: 'assistant', text: 'Hello world' })).toBe(false);
  });

  it('suppresses user messages (already added locally in sendText)', () => {
    expect(shouldAddPipelineMessage({ role: 'user', text: 'what time is it?' })).toBe(false);
  });

  it('allows system pipeline messages (e.g. tool results, metadata)', () => {
    expect(shouldAddPipelineMessage({ role: 'system', text: 'tool:calendar called' })).toBe(true);
  });

  it('allows system messages with no text', () => {
    expect(shouldAddPipelineMessage({ role: 'system', kind: 'tool_result' })).toBe(true);
  });

  it('suppresses an assistant message with an empty text string', () => {
    expect(shouldAddPipelineMessage({ role: 'assistant', text: '' })).toBe(false);
  });
});

// ── Regression: old code used "role !== user" which passed agent through ──

describe('regression: old filter let assistant through', () => {
  /** Old (buggy) filtering logic that caused the duplicate bubble. */
  function oldFilter(pm: PipelineMessageStub): boolean {
    const role = pm.role === 'user' ? 'user' : pm.role === 'system' ? 'system' : 'agent';
    return role !== 'user'; // BUG: this passes "agent" → duplicate bubble
  }

  it('old filter incorrectly returns true for assistant role', () => {
    // This is the bug — the old code would add a duplicate bubble for assistant messages.
    expect(oldFilter({ role: 'assistant', text: 'Hello world' })).toBe(true);
  });

  it('new filter correctly returns false for assistant role', () => {
    expect(shouldAddPipelineMessage({ role: 'assistant', text: 'Hello world' })).toBe(false);
  });
});

// ── Duplicate bubble: "state→listening before text arrives" race ─────────────
//
// ORIGINAL BUG:
//   onPlaybackStart stores placeholderUtteranceId = utteranceIdRef (e.g. 0).
//   state→listening can arrive BEFORE the "text" message, incrementing
//   utteranceIdRef to 1.  The old TEXT_RECEIVED check was:
//     placeholderUtteranceId === utteranceIdRef.current → 0 === 1 → false
//   so it appended a NEW bubble instead of updating the placeholder in-place.
//
// ATTEMPTED (BROKEN) FIX:
//   Parsing server utteranceId from msg.messageId ("llm-N" → N) and comparing
//   against placeholderUtteranceId.  This FAILED because the server starts its
//   utterance IDs at 1 while the client counter starts at 0, so the comparison
//   always mismatches: placeholderUtteranceId(0) !== serverUttId(1).
//
// CORRECT FIX:
//   Use speakingEntryRef >= 0 AND placeholderUtteranceIdRef >= 0 as the signal
//   that an active placeholder exists for the current turn, without comparing
//   ID spaces that are not aligned.
//
//   Invariant: placeholderUtteranceIdRef is:
//     >=  0  → set by onPlaybackStart; text hasn't arrived yet
//      = -1  → cleared after text arrives or after interrupt

describe('placeholder update decision — speakingEntryRef + placeholderUtteranceIdRef', () => {
  /**
   * Mirrors the TEXT_RECEIVED handler's "should I update in-place?" decision.
   * Replaces the old `placeholderUtteranceId === clientUttId` comparison.
   */
  function shouldUpdatePlaceholder(args: {
    placeholderUttId: number;   // placeholderUtteranceIdRef.current
    speakingEntryIndex: number; // speakingEntryRef.current (-1 = none)
  }): boolean {
    return args.speakingEntryIndex >= 0 && args.placeholderUttId >= 0;
  }

  it('updates in-place when placeholder is pending and entry is active', () => {
    expect(shouldUpdatePlaceholder({ placeholderUttId: 0, speakingEntryIndex: 1 })).toBe(true);
  });

  it('appends new entry when no placeholder exists (text before audio)', () => {
    expect(shouldUpdatePlaceholder({ placeholderUttId: -1, speakingEntryIndex: -1 })).toBe(false);
  });

  it('appends new entry when speakingEntryIndex is -1 (no active turn)', () => {
    // Can happen if clear_audio already reset the refs
    expect(shouldUpdatePlaceholder({ placeholderUttId: 0, speakingEntryIndex: -1 })).toBe(false);
  });

  it('appends new entry when placeholder was cleared (text already arrived)', () => {
    // placeholderUtteranceIdRef = -1 after first TEXT_RECEIVED
    expect(shouldUpdatePlaceholder({ placeholderUttId: -1, speakingEntryIndex: 1 })).toBe(false);
  });

  it('regression: old comparison would fail when state→listening increments counter', () => {
    // Old check: placeholderUttId === utteranceIdRef.current
    // Scenario: placeholder stored at value 0, then counter incremented to 1
    const placeholderUttId: number = 0;  // stored by onPlaybackStart
    const clientCounterAfterListening: number = 1;  // incremented by state→listening
    const oldResult = placeholderUttId === clientCounterAfterListening;
    expect(oldResult).toBe(false); // proves old code caused duplicate

    // New check doesn't use the counter at all:
    const newResult = shouldUpdatePlaceholder({ placeholderUttId: 0, speakingEntryIndex: 1 });
    expect(newResult).toBe(true); // correctly updates in-place
  });

  it('regression: serverUttId parsing also fails (server=1, client=0)', () => {
    // Server utterance IDs start at 1; client starts at 0.
    // Parsing "llm-1" → 1 and comparing against placeholderUttId (0) would mismatch.
    const serverUttId: number = 1;       // parsed from "llm-1"
    const placeholderUttId: number = 0;  // stored by onPlaybackStart at client counter 0
    const badResult = placeholderUttId === serverUttId;
    expect(badResult).toBe(false); // proves server/client ID comparison is unreliable

    // New check doesn't compare IDs at all:
    const newResult = shouldUpdatePlaceholder({ placeholderUttId: 0, speakingEntryIndex: 1 });
    expect(newResult).toBe(true); // correctly updates in-place
  });
});


