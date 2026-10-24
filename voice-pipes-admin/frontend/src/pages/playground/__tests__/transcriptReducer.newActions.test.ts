// Tests for the live action surface dispatched by usePlaygroundSession via
// the dispatch shim in useTranscriptManager.  The reveal-pipeline-driven
// actions (PLAYBACK_START / TEXT_RECEIVED / CLEAR_AUDIO / CAPTION_RECEIVED)
// were removed in cleanup pass 3 — production now uses appendCaption,
// processFinalText, and TRANSCRIPT_TRIM directly against React state, so
// those reducer cases became dead code.

import { describe, expect, it } from "vitest";
import type { PipelineMessage } from "../../../lib/transport/types";
import {
  initialTranscriptState,
  transcriptReducer,
  type TranscriptState,
} from "../transcriptReducer";

const T = 1_000_000;

function reduce(...actions: Parameters<typeof transcriptReducer>[1][]): TranscriptState {
  return actions.reduce(
    (s, a) => transcriptReducer(s, a),
    initialTranscriptState,
  );
}

/** Stateful helper for tests that need to dispatch actions one at a time
 *  and inspect the result after each one. */
function makeReducer() {
  let state = initialTranscriptState;
  return (action: Parameters<typeof transcriptReducer>[1]) => {
    state = transcriptReducer(state, action);
    return state;
  };
}

describe("USER_TEXT_SENT", () => {
  it("appends a user chat entry", () => {
    const s = reduce({ type: "USER_TEXT_SENT", text: "hi", now: T });
    expect(s.entries).toEqual([
      { role: "user", text: "hi", timestamp: T, type: "chat" },
    ]);
  });

  it("appends to existing entries without disturbing them", () => {
    const before = reduce({ type: "USER_TEXT_SENT", text: "first", now: T });
    const after = transcriptReducer(before, {
      type: "USER_TEXT_SENT",
      text: "second",
      now: T + 1,
    });
    expect(after.entries).toHaveLength(before.entries.length + 1);
    expect(after.entries[0]).toEqual(before.entries[0]);
  });
});

describe("ERROR_RECEIVED", () => {
  it("appends a system error entry with severity/code/component metadata", () => {
    const s = reduce({
      type: "ERROR_RECEIVED",
      message: "TTS failed",
      now: T,
      severity: "error",
      errorCode: "tts_timeout",
      component: "tts",
    });
    expect(s.entries).toEqual([
      {
        role: "system",
        text: "TTS failed",
        timestamp: T,
        type: "error",
        severity: "error",
        errorCode: "tts_timeout",
        component: "tts",
      },
    ]);
  });

  it("works with optional fields omitted", () => {
    const s = reduce({
      type: "ERROR_RECEIVED",
      message: "something",
      now: T,
    });
    expect(s.entries[0].type).toBe("error");
    expect(s.entries[0].severity).toBeUndefined();
  });
});

describe("PIPELINE_MESSAGE", () => {
  it("maps assistant role to agent and stores the original message", () => {
    const pm: PipelineMessage = {
      kind: "chat",
      text: "hello world",
      role: "assistant",
      message_id: "m1",
      turn_id: "abc",
    };
    const s = reduce({ type: "PIPELINE_MESSAGE", pipelineMessage: pm, now: T });
    expect(s.entries[0]).toEqual({
      role: "agent",
      text: "hello world",
      timestamp: T,
      type: "message",
      pipelineMessage: pm,
    });
  });

  it("preserves user and system roles unchanged", () => {
    const pmUser: PipelineMessage = { kind: "chat", text: "hi", role: "user", message_id: "u1" };
    const pmSys: PipelineMessage = { kind: "info", text: "boot", role: "system", message_id: "s1" };
    const s = reduce(
      { type: "PIPELINE_MESSAGE", pipelineMessage: pmUser, now: T },
      { type: "PIPELINE_MESSAGE", pipelineMessage: pmSys, now: T + 1 },
    );
    expect(s.entries[0].role).toBe("user");
    expect(s.entries[1].role).toBe("system");
  });

  it("handles messages without text by storing an empty string", () => {
    const pm: PipelineMessage = { kind: "data", role: "assistant", message_id: "d1", payload: { x: 1 } };
    const s = reduce({ type: "PIPELINE_MESSAGE", pipelineMessage: pm, now: T });
    expect(s.entries[0].text).toBe("");
    expect(s.entries[0].pipelineMessage).toBe(pm);
  });
});

describe("CLEAR_ALL", () => {
  it("returns the initial state from any prior state", () => {
    const dirty = reduce(
      { type: "USER_TEXT_SENT", text: "hi", now: T },
      { type: "ERROR_RECEIVED", message: "boom", now: T + 3 },
    );
    expect(dirty.entries.length).toBeGreaterThan(0);

    const cleared = transcriptReducer(dirty, { type: "CLEAR_ALL" });
    expect(cleared).toEqual(initialTranscriptState);
  });
});

describe("Action ordering: realistic transcript turn", () => {
  it("user-typed followed by error appends in order", () => {
    const s = reduce(
      { type: "USER_TEXT_SENT", text: "tell me a joke", now: T },
      {
        type: "ERROR_RECEIVED",
        message: "STT timeout",
        now: T + 2,
        severity: "warning",
        component: "stt",
      },
    );
    expect(s.entries).toHaveLength(2);
    expect(s.entries[0].role).toBe("user");
    expect(s.entries[0].text).toBe("tell me a joke");
    expect(s.entries[1].type).toBe("error");
  });
});

describe("TEXT_TRUNCATED", () => {
  function withAgentEntry(text: string, utteranceId: number): TranscriptState {
    return {
      ...initialTranscriptState,
      entries: [{ role: "agent", text, timestamp: T, debugLog: [], utteranceId }],
    };
  }

  it("replaces agent text with finalText based on utteranceId", () => {
    const state = withAgentEntry("one two three four five", 42);
    const next = transcriptReducer(state, {
      type: "TEXT_TRUNCATED",
      utteranceId: 42,
      finalText: "one two three",
    });
    expect(next.entries[0].text).toBe("one two three");
    expect(next.entries[0].debugLog?.[0]).toContain("TRIMMED (server)");
  });

  it("is a no-op if utteranceId doesn't match", () => {
    const state = withAgentEntry("one two three", 42);
    const next = transcriptReducer(state, {
      type: "TEXT_TRUNCATED",
      utteranceId: 99,
      finalText: "one two",
    });
    expect(next).toBe(state);
  });

  it("is a no-op if utteranceId matches but role is not agent", () => {
    const state: TranscriptState = {
      ...initialTranscriptState,
      entries: [{ role: "user", text: "hi", timestamp: T, utteranceId: 42 }],
    };
    const next = transcriptReducer(state, {
      type: "TEXT_TRUNCATED",
      utteranceId: 42,
      finalText: "one two",
    });
    expect(next).toBe(state);
  });

  it("applies server text even when server has MORE words than client (non-trimmed entry)", () => {
    // Normal non-interrupted turn: server text replaces placeholder text freely.
    const state = withAgentEntry("one two", 42);
    const next = transcriptReducer(state, {
      type: "TEXT_TRUNCATED",
      utteranceId: 42,
      finalText: "one two three four",
    });
    expect(next.entries[0].text).toBe("one two three four");
  });

  it("TEXT_TRUNCATED updates entry.text but leaves heardText (client snapshot) unchanged", () => {
    // After an interrupt, AGENT_FREEZE_AT_WORD sets entry.heardText to the
    // heard prefix while leaving entry.text as the full LLM text.
    // TEXT_TRUNCATED from the server updates entry.text for history purposes
    // but must NOT change heardText — ChatBubble uses heardText ?? text, so
    // the display stays frozen at the heard prefix regardless.
    const state: TranscriptState = {
      ...initialTranscriptState,
      entries: [{
        role: "agent",
        text: "Once upon a time there was a curious young girl named Lila.",
        heardText: "Once upon a time there was a curious",
        timestamp: T,
        utteranceId: 5,
        debugLog: ["FREEZE_AT_WORD: wordIndex=7"],
      }],
    };
    const next = transcriptReducer(state, {
      type: "TEXT_TRUNCATED",
      utteranceId: 5,
      finalText: "Once upon a time there was a curious young girl",
    });
    // Server updated entry.text (authoritative LLM history value).
    expect(next.entries[0].text).toBe("Once upon a time there was a curious young girl");
    // Display snapshot (heardText) is unchanged.
    expect(next.entries[0].heardText).toBe("Once upon a time there was a curious");
  });

  it("TEXT_TRUNCATED updates entry.text even when server sends more words than currently stored", () => {
    // With the heardText design, TEXT_TRUNCATED only manages the authoritative
    // LLM text (entry.text).  There is no expansion guard; the display is
    // governed by entry.heardText which TEXT_TRUNCATED never touches.
    const state: TranscriptState = {
      ...initialTranscriptState,
      entries: [{
        role: "agent",
        text: "Once upon a time there was a curious",
        heardText: "Once upon a time there was a curious",
        timestamp: T,
        utteranceId: 5,
      }],
    };
    const next = transcriptReducer(state, {
      type: "TEXT_TRUNCATED",
      utteranceId: 5,
      finalText: "Once upon a time there was a curious young girl named Lila. She loved exploring",
    });
    // entry.text now has the server value.
    expect(next.entries[0].text).toBe("Once upon a time there was a curious young girl named Lila. She loved exploring");
    // Display remains frozen at the client snapshot.
    expect(next.entries[0].heardText).toBe("Once upon a time there was a curious");
  });
});

describe("AGENT_PLACEHOLDER_CREATED", () => {
  it("appends an empty agent entry with debug log", () => {
    const s = reduce({ type: "AGENT_PLACEHOLDER_CREATED", now: T, utteranceId: 42 });
    expect(s.entries).toHaveLength(1);
    expect(s.entries[0]).toMatchObject({
      role: "agent",
      text: "",
      timestamp: T,
      utteranceId: 42,
    });
    expect(s.entries[0].debugLog).toContain("CREATED PLACEHOLDER");
  });
});

describe("AGENT_CAPTION_APPEND", () => {
  function withPlaceholder(): TranscriptState {
    return reduce({ type: "AGENT_PLACEHOLDER_CREATED", now: T, utteranceId: 42 });
  }

  it("sets text on first append (no leading space)", () => {
    const before = withPlaceholder();
    const after = transcriptReducer(before, {
      type: "AGENT_CAPTION_APPEND",
      utteranceId: 42,
      captionText: "hello",
    });
    expect(after.entries[0].text).toBe("hello");
  });

  it("concatenates with single space on subsequent appends", () => {
    const after = reduce(
      { type: "AGENT_PLACEHOLDER_CREATED", now: T, utteranceId: 42 },
      { type: "AGENT_CAPTION_APPEND", utteranceId: 42, captionText: "hello" },
      { type: "AGENT_CAPTION_APPEND", utteranceId: 42, captionText: "world" },
    );
    expect(after.entries[0].text).toBe("hello world");
  });

  it("is a no-op for out-of-range entryIndex", () => {
    const before = withPlaceholder();
    const after = transcriptReducer(before, {
      type: "AGENT_CAPTION_APPEND",
      utteranceId: 99,
      captionText: "x",
    });
    expect(after).toBe(before);
  });

  it("is a no-op when target entry is not an agent entry", () => {
    const before = reduce({ type: "USER_TEXT_SENT", text: "hi", now: T });
    before.entries[0].utteranceId = 100;
    const after = transcriptReducer(before, {
      type: "AGENT_CAPTION_APPEND",
      utteranceId: 100,
      captionText: "x",
    });
    expect(after).toBe(before);
  });
});

describe("AGENT_TEXT_FILL_PLACEHOLDER", () => {
  it("sets text when placeholder is empty", () => {
    const before = reduce({ type: "AGENT_PLACEHOLDER_CREATED", now: T, utteranceId: 42 });
    const after = transcriptReducer(before, {
      type: "AGENT_TEXT_FILL_PLACEHOLDER",
      utteranceId: 42,
      text: "final answer",
    });
    expect(after.entries[0].text).toBe("final answer");
    expect(after.entries[0].debugLog?.some((m) => m.includes("hasCaption"))).toBe(true);
  });

  it("ignores text when caption already populated the bubble", () => {
    const before = reduce(
      { type: "AGENT_PLACEHOLDER_CREATED", now: T, utteranceId: 42 },
      { type: "AGENT_CAPTION_APPEND", utteranceId: 42, captionText: "caption" },
    );
    const after = transcriptReducer(before, {
      type: "AGENT_TEXT_FILL_PLACEHOLDER",
      utteranceId: 42,
      text: "should be ignored",
    });
    expect(after.entries[0].text).toBe("caption");
    expect(after.entries[0].debugLog?.some((m) => m.includes("hasCaption=true"))).toBe(true);
  });

  it("is a no-op for out-of-range entryIndex", () => {
    const before = reduce({ type: "AGENT_PLACEHOLDER_CREATED", now: T, utteranceId: 42 });
    const after = transcriptReducer(before, {
      type: "AGENT_TEXT_FILL_PLACEHOLDER",
      utteranceId: 99,
      text: "x",
    });
    expect(after).toBe(before);
  });
});

describe("AGENT_FINAL_NEW", () => {
  it("appends a new agent entry without debug log", () => {
    const s = reduce({ type: "AGENT_FINAL_NEW", text: "hello", now: T, utteranceId: 42 });
    expect(s.entries).toEqual([
      { role: "agent", text: "hello", timestamp: T, utteranceId: 42, isFullTextFilled: true },
    ]);
  });
});

describe("USER_TRANSCRIBED", () => {
  it("appends a user entry without `type` field (distinct from USER_TEXT_SENT)", () => {
    const s = reduce({ type: "USER_TRANSCRIBED", text: "spoken", now: T });
    expect(s.entries).toEqual([
      { role: "user", text: "spoken", timestamp: T, utteranceId: undefined },
    ]);
  });

  it("inserts the user entry before the corresponding agent turn when a newer turn already exists", () => {
    const r = makeReducer();
    r({ type: "AGENT_PLACEHOLDER_CREATED", now: T, utteranceId: 0 });
    r({ type: "AGENT_TEXT_FILL_PLACEHOLDER", utteranceId: 0, text: "agent text 0" });
    r({ type: "AGENT_PLACEHOLDER_CREATED", now: T + 100, utteranceId: 1 });
    
    // User STT for turn 1 arrives late, after agent placeholder for turn 1 was already created
    const s = r({ type: "USER_TRANSCRIBED", text: "user text 1", utteranceId: 1, now: T + 200 });
    
    // User entry should be spliced before agent turn 1, and speakingEntryIndex shifted from 1 to 2
    expect(s.entries).toHaveLength(3);
    expect(s.entries[0].role).toBe("agent"); // agent turn 0
    expect(s.entries[1].role).toBe("user");  // user turn 1 spliced
    expect(s.entries[1].text).toBe("user text 1");
    expect(s.entries[2].role).toBe("agent"); // agent turn 1 placeholder
    expect(s.speakingEntryIndex).toBe(2);
  });

  it("appends after earlier agent-initiated turns instead of hoisting above them", () => {
    // Agent-initiated turns (idle prompts) use the 10001+ utteranceId range.
    // A later user turn has a *smaller* id, and must still render below them.
    const r = makeReducer();
    r({ type: "AGENT_PLACEHOLDER_CREATED", now: T, utteranceId: 10001 });
    r({ type: "AGENT_TEXT_FILL_PLACEHOLDER", utteranceId: 10001, text: "Are you still there?" });
    r({ type: "AGENT_PLACEHOLDER_CREATED", now: T + 100, utteranceId: 10002 });
    r({ type: "AGENT_TEXT_FILL_PLACEHOLDER", utteranceId: 10002, text: "Are you still there?" });

    // The user finally answers; their turn gets a normal (small) id.
    const s = r({ type: "USER_TRANSCRIBED", text: "Yeah, I'm here.", utteranceId: 2, now: T + 200 });

    expect(s.entries).toHaveLength(3);
    expect(s.entries[0]).toMatchObject({ role: "agent", utteranceId: 10001 });
    expect(s.entries[1]).toMatchObject({ role: "agent", utteranceId: 10002 });
    expect(s.entries[2]).toMatchObject({ role: "user", text: "Yeah, I'm here." });
  });

  it("never splices above an earlier USER bubble sharing the same id (client-tool dispatch)", () => {
    // A client-tool dispatch flushes the user's transcription under a
    // provisional utterance id; the model's reply turn later reuses the
    // same id for the user's follow-up question. The follow-up must
    // render BELOW the earlier user bubble (speaking order), splicing
    // only above the agent reply.
    const r = makeReducer();
    r({ type: "USER_TRANSCRIBED", text: "Open the settings.", utteranceId: 4, now: T });
    r({ type: "AGENT_PLACEHOLDER_CREATED", now: T + 100, utteranceId: 4 });

    const s = r({
      type: "USER_TRANSCRIBED",
      text: "Which screen did you open?",
      utteranceId: 4,
      now: T + 200,
    });

    expect(s.entries).toHaveLength(3);
    expect(s.entries[0]).toMatchObject({ role: "user", text: "Open the settings." });
    expect(s.entries[1]).toMatchObject({ role: "user", text: "Which screen did you open?" });
    expect(s.entries[2]).toMatchObject({ role: "agent", utteranceId: 4 });
  });
});

describe("INTERRUPTED_DIVIDER", () => {
  it("appends a system divider with empty text when no reason given", () => {
    const s = reduce({ type: "INTERRUPTED_DIVIDER", now: T });
    expect(s.entries).toEqual([
      { role: "system", text: "", timestamp: T, type: "interrupted" },
    ]);
  });

  it("formats reason as `interrupted (<reason>)` when provided", () => {
    const s = reduce({
      type: "INTERRUPTED_DIVIDER",
      reason: "user_speech",
      now: T,
    });
    expect(s.entries[0]).toMatchObject({
      role: "system",
      text: "interrupted (user_speech)",
      type: "interrupted",
    });
  });

  it("coalesces a second divider dispatched within 2s into the previous one", () => {
    // Simulates the barge-in race: client dispatches a divider with no
    // reason synchronously, then the server's clear_audio round-trips a
    // few ms later with the authoritative reason.
    const reduce2 = makeReducer();
    reduce2({ type: "INTERRUPTED_DIVIDER", now: T });
    const s = reduce2({
      type: "INTERRUPTED_DIVIDER",
      reason: "user_speech",
      now: T + 50,
    });
    expect(s.entries).toHaveLength(1);
    expect(s.entries[0]).toMatchObject({
      type: "interrupted",
      text: "interrupted (user_speech)",
      timestamp: T + 50,
    });
  });

  it("keeps the existing reason when the second dispatch has none", () => {
    const reduce2 = makeReducer();
    reduce2({ type: "INTERRUPTED_DIVIDER", reason: "client_cancel", now: T });
    const s = reduce2({ type: "INTERRUPTED_DIVIDER", now: T + 100 });
    expect(s.entries).toHaveLength(1);
    expect(s.entries[0].text).toBe("interrupted (client_cancel)");
  });

  it("appends a fresh divider when more than 2s have passed", () => {
    const reduce2 = makeReducer();
    reduce2({ type: "INTERRUPTED_DIVIDER", reason: "client_cancel", now: T });
    const s = reduce2({
      type: "INTERRUPTED_DIVIDER",
      reason: "user_speech",
      now: T + 2500,
    });
    expect(s.entries).toHaveLength(2);
  });
});

// ─── Interruption shows typing dots then full/buffered-ahead text ───────────
// Regression tests for AGENT_FREEZE_AT_WORD, the client-side snapshot action
// dispatched by handleClearAudio to freeze the agent bubble at the last heard
// word.  Unlike the old AGENT_TEXT_TRIM_TO_WORD, this sets entry.heardText
// without mutating entry.text, so the full LLM text is preserved for history.
describe("AGENT_FREEZE_AT_WORD", () => {
  const FULL_TEXT = "Sure! Why did the scarecrow win an award? Because he was outstanding in his field!";
  const UTT = 2;

  /** Build the common entry state: placeholder created → full LLM text fills
   *  it before any captions arrive (isFullTextFilled = true). */
  function stateWithFullText() {
    return reduce(
      { type: "AGENT_PLACEHOLDER_CREATED", now: T, utteranceId: UTT },
      { type: "AGENT_TEXT_FILL_PLACEHOLDER", utteranceId: UTT, text: FULL_TEXT },
    );
  }

  it("sets heardText to the heard prefix, entry.text stays unchanged", () => {
    const before = stateWithFullText();
    expect(before.entries[0].isFullTextFilled).toBe(true);

    const after = transcriptReducer(before, {
      type: "AGENT_FREEZE_AT_WORD",
      utteranceId: UTT,
      wordIndex: 4, // words 0-4 = "Sure! Why did the scarecrow"
    });
    expect(after.entries[0].heardText).toBe("Sure! Why did the scarecrow");
    // Full LLM text is preserved in entry.text.
    expect(after.entries[0].text).toBe(FULL_TEXT);
  });

  it("heardText covers all words when wordIndex is the last word", () => {
    const before = stateWithFullText();
    const words = FULL_TEXT.split(/\s+/).filter(Boolean);
    const after = transcriptReducer(before, {
      type: "AGENT_FREEZE_AT_WORD",
      utteranceId: UTT,
      wordIndex: words.length - 1,
    });
    expect(after.entries[0].heardText).toBe(FULL_TEXT);
    expect(after.entries[0].text).toBe(FULL_TEXT);
  });

  it("freezes caption-built entries (isFullTextFilled falsy) — captions arrive ahead of playback", () => {
    // Caption path: placeholder → one caption appended — isFullTextFilled NOT set.
    // Captions arrive with buffered-ahead audio so the entry contains future text.
    // AGENT_FREEZE_AT_WORD must snapshot at the heard word regardless of
    // isFullTextFilled, unlike the old AGENT_TEXT_TRIM_TO_WORD.
    const capState = reduce(
      { type: "AGENT_PLACEHOLDER_CREATED", now: T, utteranceId: UTT },
      { type: "AGENT_CAPTION_APPEND", utteranceId: UTT, captionText: "Sure! Why did the scarecrow win an award" },
    );
    expect(capState.entries[0].isFullTextFilled).toBeFalsy();

    const after = transcriptReducer(capState, {
      type: "AGENT_FREEZE_AT_WORD",
      utteranceId: UTT,
      wordIndex: 2, // keep words 0-2: "Sure! Why did"
    });
    // heardText is frozen at the heard words.
    expect(after.entries[0].heardText).toBe("Sure! Why did");
    // entry.text still has all buffered-ahead caption text.
    expect(after.entries[0].text).toBe("Sure! Why did the scarecrow win an award");
  });

  it("is a no-op when the utteranceId does not match any entry", () => {
    const before = stateWithFullText();
    const after = transcriptReducer(before, {
      type: "AGENT_FREEZE_AT_WORD",
      utteranceId: 999,
      wordIndex: 2,
    });
    expect(after).toBe(before); // referential equality — no allocation
  });

  it("is a no-op when utteranceId is -1 (sentinel)", () => {
    const before = stateWithFullText();
    const after = transcriptReducer(before, {
      type: "AGENT_FREEZE_AT_WORD",
      utteranceId: -1,
      wordIndex: 2,
    });
    expect(after).toBe(before);
  });

  it("TEXT_TRUNCATED updates entry.text but heardText (display value) stays frozen", () => {
    // After freeze, the server's TEXT_TRUNCATED updates entry.text for LLM
    // history purposes.  heardText is never modified — ChatBubble renders
    // heardText ?? text, so the display stays frozen at the heard prefix.
    const frozen = transcriptReducer(stateWithFullText(), {
      type: "AGENT_FREEZE_AT_WORD",
      utteranceId: UTT,
      wordIndex: 4, // heardText = "Sure! Why did the scarecrow" (5 words)
    });
    const final = transcriptReducer(frozen, {
      type: "TEXT_TRUNCATED",
      utteranceId: UTT,
      finalText: "Sure! Why did the scarecrow win an award?", // server value (longer)
    });
    // Server updated entry.text.
    expect(final.entries[0].text).toBe("Sure! Why did the scarecrow win an award?");
    // Display snapshot unchanged.
    expect(final.entries[0].heardText).toBe("Sure! Why did the scarecrow");
  });

  it("TEXT_TRUNCATED with shorter server text: entry.text is updated, heardText is unaffected", () => {
    const frozen = transcriptReducer(stateWithFullText(), {
      type: "AGENT_FREEZE_AT_WORD",
      utteranceId: UTT,
      wordIndex: 4, // client heard up to word 4
    });
    const final = transcriptReducer(frozen, {
      type: "TEXT_TRUNCATED",
      utteranceId: UTT,
      finalText: "Sure! Why did", // server says only 3 words
    });
    expect(final.entries[0].text).toBe("Sure! Why did");
    expect(final.entries[0].heardText).toBe("Sure! Why did the scarecrow");
  });

  it("full interrupt sequence: placeholder → full text fill → freeze → divider", () => {
    const r = makeReducer();
    r({ type: "AGENT_PLACEHOLDER_CREATED", now: T, utteranceId: UTT });
    r({ type: "AGENT_TEXT_FILL_PLACEHOLDER", utteranceId: UTT, text: FULL_TEXT });
    // User interrupts after 4 words heard
    r({ type: "AGENT_FREEZE_AT_WORD", utteranceId: UTT, wordIndex: 4 });
    const s = r({ type: "INTERRUPTED_DIVIDER", reason: "user_speech", now: T + 100 });
    expect(s.entries).toHaveLength(2);
    expect(s.entries[0].heardText).toBe("Sure! Why did the scarecrow");
    expect(s.entries[0].text).toBe(FULL_TEXT); // full text preserved
    expect(s.entries[1].type).toBe("interrupted");
  });
});

describe("CLEANUP_EMPTY_PLACEHOLDERS", () => {
  it("removes empty agent placeholders and manages speakingEntryIndex", () => {
    const r = makeReducer();
    r({ type: "AGENT_PLACEHOLDER_CREATED", now: T, utteranceId: 10 });
    // This speaker is active
    let state = r({ type: "AGENT_PLACEHOLDER_CREATED", now: T, utteranceId: 11 });
    expect(state.entries).toHaveLength(2);
    expect(state.speakingEntryIndex).toBe(1);

    // Filter specific empty placeholder
    state = r({ type: "CLEANUP_EMPTY_PLACEHOLDERS", utteranceId: 10 });
    expect(state.entries).toHaveLength(1);
    expect(state.entries[0].utteranceId).toBe(11);
    // Speaking entry index mapped correctly to new index 0
    expect(state.speakingEntryIndex).toBe(0);

    // Clean up all empty placeholders
    state = r({ type: "CLEANUP_EMPTY_PLACEHOLDERS" });
    expect(state.entries).toHaveLength(0);
    expect(state.speakingEntryIndex).toBe(-1);
  });

  it("does not remove populated agent turns", () => {
    const r = makeReducer();
    r({ type: "AGENT_PLACEHOLDER_CREATED", now: T, utteranceId: 10 });
    r({ type: "AGENT_TEXT_FILL_PLACEHOLDER", utteranceId: 10, text: "hello" });
    r({ type: "AGENT_PLACEHOLDER_CREATED", now: T, utteranceId: 11 }); // remains empty
    
    const state = r({ type: "CLEANUP_EMPTY_PLACEHOLDERS" });
    expect(state.entries).toHaveLength(1);
    expect(state.entries[0].utteranceId).toBe(10);
    expect(state.entries[0].text).toBe("hello");
  });
});
